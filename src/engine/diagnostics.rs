//! Pre-flight command-line diagnostics against the active `.ezlo` grammar.

use crate::engine::line::ParsedLine;
use crate::engine::types::{evaluate_when_status, WhenStatus};
use crate::engine::walker::{
    collect_active_opts, find_opt, find_short_opt, push_subcommand, resolve_positional_arg,
    WalkState,
};
use crate::engine::CompletionRequest;
use crate::spec::ast::{ArgDecl, OptDecl, TypeExpr, WhenClause};
use crate::spec::SpecIndex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagSeverity {
    Error,
    Warning,
}

impl DiagSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineDiagnostic {
    pub char_start: usize,
    pub char_end: usize,
    pub severity: DiagSeverity,
    pub message: String,
}

impl LineDiagnostic {
    fn new(span: (usize, usize), severity: DiagSeverity, message: String) -> Self {
        Self { char_start: span.0, char_end: span.1, severity, message }
    }
}

struct SeenOpt<'a> {
    opt: &'a OptDecl,
    flag: String,
}

/// An option word whose detached values follow (see `walker::consume_option_word`).
struct PendingValues<'a> {
    opt: &'a OptDecl,
    /// Next value slot to fill.
    next: usize,
    flag: String,
    flag_span: (usize, usize),
}

impl<'a> PendingValues<'a> {
    /// `opt` spelled without an attached value: its values follow, unless they are optional (and
    /// not `greedy`).
    fn detached(opt: &'a OptDecl, flag: &str, flag_span: (usize, usize)) -> Option<Self> {
        let waits = opt.detached_value_count() > 0 || (opt.optional_value() && opt.greedy);
        waits.then(|| Self { opt, next: 0, flag: flag.to_owned(), flag_span })
    }

    /// `opt` spelled with an attached first value: its other values follow.
    fn after_first(opt: &'a OptDecl, flag: &str, flag_span: (usize, usize)) -> Option<Self> {
        (!opt.extra_values.is_empty()).then(|| Self { opt, next: 1, flag: flag.to_owned(), flag_span })
    }
}

pub fn validate_command_line<'a>(
    req: &CompletionRequest<'a>,
    parsed: &ParsedLine,
    walk: &WalkState<'a>,
) -> Vec<LineDiagnostic> {
    let cursor_limit = parsed.cursor_word_idx.unwrap_or(parsed.words.len());
    if !walk.known_root || parsed.cmd_word_idx >= cursor_limit {
        return Vec::new();
    }
    // Words from a nested command line on are validated against the nested command's grammar.
    let end = walk.nested_cmd_word_idx.unwrap_or(cursor_limit);

    let mut diags = Vec::new();
    let mut seen_opts: Vec<SeenOpt<'a>> = Vec::new();
    // The walk already canonicalized a root alias.
    let mut cmd_path = walk.cmd_path[..1].to_vec();
    let mut step_opts = collect_active_opts(req.index, &cmd_path);
    let mut last_cmd_word_idx = parsed.cmd_word_idx;
    let mut after_double_dash = false;
    let mut pos_idx = 0usize;
    let mut pending_opt: Option<PendingValues<'a>> = None;

    let lookup_opt = |flag: &str, local: &[&'a OptDecl]| -> Option<&'a OptDecl> {
        find_opt(&walk.active_opts, flag).or_else(|| find_opt(local, flag))
    };
    let lookup_short = |ch: char, local: &[&'a OptDecl]| -> Option<&'a OptDecl> {
        find_short_opt(&walk.active_opts, ch).or_else(|| find_short_opt(local, ch))
    };

    for (offset, word) in parsed.words[parsed.cmd_word_idx + 1..end].iter().enumerate() {
        let word_idx = parsed.cmd_word_idx + 1 + offset;
        let text = word.unquoted.as_str();
        let word_span = (word.char_start, word.char_end);

        if let Some(mut pending) = pending_opt.take() {
            let opt = pending.opt;
            // A `greedy` optional value only takes a word that looks like one.
            if !opt.optional_value() || opt.takes_detached_value(text) {
                if let Some(kind) = opt_path_kind(opt, pending.next, req.index, &walk.present_flags) {
                    check_local_path(text, word_span, kind, req.cwd, req.env, &mut diags);
                }
                pending.next += 1;
                if pending.next < opt.detached_value_count() {
                    pending_opt = Some(pending);
                }
                continue;
            }
        }
        if !after_double_dash && text == "--" {
            after_double_dash = true;
            continue;
        }
        if !after_double_dash && text.starts_with('-') && text.len() > 1 {
            if let Some((flag_part, val_part)) = text.split_once('=') {
                let flag_chars = flag_part.chars().count();
                let flag_span = (word.char_start, word.char_start + flag_chars);
                let val_span = (word.char_start + flag_chars + 1, word.char_end);
                if let Some(opt) = lookup_opt(flag_part, &step_opts) {
                    record_seen_opt(opt, flag_part, flag_span, &mut seen_opts, &mut diags);
                    if let Some(kind) = opt_path_kind(opt, 0, req.index, &walk.present_flags) {
                        check_local_path(val_part, val_span, kind, req.cwd, req.env, &mut diags);
                    }
                    pending_opt = PendingValues::after_first(opt, flag_part, flag_span);
                } else {
                    diags.push(unknown_opt_diag(flag_part, flag_span));
                }
            } else if let Some(opt) = lookup_opt(text, &step_opts) {
                record_seen_opt(opt, text, word_span, &mut seen_opts, &mut diags);
                pending_opt = PendingValues::detached(opt, text, word_span);
            } else if text.starts_with("--") {
                diags.push(unknown_opt_diag(text, word_span));
            } else {
                let cluster = &text[1..];
                for (byte_idx, ch) in cluster.char_indices() {
                    let flag_str = format!("-{ch}");
                    let ch_pos = word.char_start + 1 + cluster[..byte_idx].chars().count();
                    let ch_span = if cluster.chars().count() == 1 {
                        word_span
                    } else {
                        (ch_pos, (ch_pos + 1).min(word.char_end))
                    };
                    let Some(opt) = lookup_short(ch, &step_opts) else {
                        diags.push(unknown_opt_diag(&flag_str, ch_span));
                        break;
                    };
                    record_seen_opt(opt, &flag_str, ch_span, &mut seen_opts, &mut diags);
                    if opt.value.is_some() {
                        let attached = &cluster[byte_idx + ch.len_utf8()..];
                        if attached.is_empty() {
                            pending_opt = PendingValues::detached(opt, &flag_str, ch_span);
                        } else {
                            if let Some(kind) = opt_path_kind(opt, 0, req.index, &walk.present_flags) {
                                let att_span = ((ch_pos + 1).min(word.char_end), word.char_end);
                                check_local_path(attached, att_span, kind, req.cwd, req.env, &mut diags);
                            }
                            pending_opt = PendingValues::after_first(opt, &flag_str, ch_span);
                        }
                        break;
                    }
                }
            }
            continue;
        }
        if !after_double_dash && pos_idx == 0 && push_subcommand(req.index, &mut cmd_path, &word.unquoted) {
            step_opts = collect_active_opts(req.index, &cmd_path);
            last_cmd_word_idx = word_idx;
            continue;
        }
        if let Some(arg_decl) = walk.matched_cmd.and_then(|c| resolve_positional_arg(c, pos_idx)) {
            if let Some(kind) = arg_path_kind(arg_decl, req.index, &walk.present_flags) {
                check_local_path(text, word_span, kind, req.cwd, req.env, &mut diags);
            }
        }
        pos_idx += 1;
    }

    if let Some(pending) = &pending_opt {
        missing_values_diag(req, parsed, end, pending, &mut diags);
    }
    if pending_opt.is_none() && !after_double_dash && walk.nested_cmd_word_idx.is_none() {
        if let Some(cidx) = parsed.cursor_word_idx.filter(|&i| i > parsed.cmd_word_idx) {
            if let Some((flag_part, _)) = parsed
                .typed_prefix
                .split_once('=')
                .filter(|(f, _)| f.starts_with('-') && f.len() > 1)
            {
                let w = &parsed.words[cidx];
                let flag_span = (w.char_start, w.char_start + flag_part.chars().count());
                if let Some(opt) = lookup_opt(flag_part, &step_opts) {
                    record_seen_opt(opt, flag_part, flag_span, &mut seen_opts, &mut diags);
                } else {
                    diags.push(unknown_opt_diag(flag_part, flag_span));
                }
            }
        }
    }

    if end > last_cmd_word_idx + 1 {
        let cmd_span = (
            parsed.words[parsed.cmd_word_idx].char_start,
            parsed.words[last_cmd_word_idx].char_end,
        );
        for opt in walk.active_opts.iter().copied().filter(|o| o.required) {
            if !seen_opts.iter().any(|s| s.opt.name == opt.name) {
                let flag = preferred_flag(opt);
                diags.push(LineDiagnostic::new(
                    cmd_span,
                    DiagSeverity::Error,
                    format!("missing required option '{flag}'"),
                ));
            }
        }
    }

    diags
}

/// `jq --arg name␣␣<cursor>…`: values still missing at the end of the line, while the cursor sits
/// before the last word (so the user isn't about to type them).
fn missing_values_diag(
    req: &CompletionRequest<'_>,
    parsed: &ParsedLine,
    end: usize,
    pending: &PendingValues<'_>,
    diags: &mut Vec<LineDiagnostic>,
) {
    let typing_at_end = parsed.words.last().is_none_or(|w| req.cursor >= w.char_start);
    if pending.opt.optional_value() || end != parsed.words.len() || typing_at_end {
        return;
    }
    let missing: Vec<String> = (pending.next..pending.opt.detached_value_count())
        .filter_map(|i| pending.opt.value_at(i).map(|v| v.placeholder()))
        .collect();
    diags.push(LineDiagnostic::new(
        pending.flag_span,
        DiagSeverity::Error,
        format!("option '{}' is missing {}", pending.flag, missing.join(" ")),
    ));
}

fn unknown_opt_diag(flag: &str, span: (usize, usize)) -> LineDiagnostic {
    LineDiagnostic::new(span, DiagSeverity::Error, format!("unknown option '{flag}'"))
}

fn record_seen_opt<'a>(
    opt: &'a OptDecl,
    flag: &str,
    span: (usize, usize),
    seen: &mut Vec<SeenOpt<'a>>,
    diags: &mut Vec<LineDiagnostic>,
) {
    if !opt.repeatable() && seen.iter().any(|s| s.opt.name == opt.name) {
        diags.push(LineDiagnostic::new(
            span,
            DiagSeverity::Error,
            format!("duplicate option '{flag}'"),
        ));
    }
    if let Some(conflict) = seen
        .iter()
        .find(|s| s.opt.name != opt.name && (excludes_target(s.opt, opt) || excludes_target(opt, s.opt)))
    {
        diags.push(LineDiagnostic::new(
            span,
            DiagSeverity::Error,
            format!("option '{flag}' cannot be used with '{}'", conflict.flag),
        ));
    }
    seen.push(SeenOpt { opt, flag: flag.to_owned() });
}

fn excludes_target(source: &OptDecl, target: &OptDecl) -> bool {
    source.excludes.iter().any(|ex| ex == &target.name || target.flags.iter().any(|f| f == ex))
}

fn preferred_flag(opt: &OptDecl) -> &str {
    opt.flags
        .iter()
        .find(|f| f.starts_with("--"))
        .or_else(|| opt.flags.first())
        .map_or(opt.name.as_str(), String::as_str)
}

/// Builtin path kind of value slot `value_idx` of `opt` (`when` clauses refine the first value).
fn opt_path_kind(
    opt: &OptDecl,
    value_idx: usize,
    index: &SpecIndex,
    present_flags: &HashMap<String, Option<String>>,
) -> Option<&'static str> {
    // A composite word (`a.txt,b.txt`) is not one path.
    let vs = opt.value_at(value_idx).filter(|vs| vs.composite.is_none())?;
    let when: &[_] = if value_idx == 0 { &opt.when } else { &[] };
    slot_builtin_path(vs.type_expr.as_deref(), &vs.name, !vs.suggestions.is_empty(), when, index, present_flags)
}

fn arg_path_kind(
    arg: &ArgDecl,
    index: &SpecIndex,
    present_flags: &HashMap<String, Option<String>>,
) -> Option<&'static str> {
    if arg.composite.is_some() {
        return None;
    }
    slot_builtin_path(arg.type_expr.as_deref(), &arg.name, !arg.suggestions.is_empty(), &arg.when, index, present_flags)
}

fn slot_builtin_path(
    declared_type: Option<&TypeExpr>,
    fallback_name: &str,
    has_suggestions: bool,
    when_clauses: &[WhenClause],
    index: &SpecIndex,
    present_flags: &HashMap<String, Option<String>>,
) -> Option<&'static str> {
    let selected_when = when_clauses
        .iter()
        .find(|wc| evaluate_when_status(wc, present_flags) == WhenStatus::True)
        .map(|wc| &wc.type_expr);
    let type_name = if let Some(te) = selected_when.or(declared_type) {
        te.type_ref.name.as_str()
    } else if !has_suggestions && index.find_type(fallback_name).is_none() {
        fallback_name
    } else {
        return None;
    };
    match type_name {
        "file" => Some("file"),
        "dir" => Some("dir"),
        _ => None,
    }
}

fn check_local_path(
    raw_val: &str,
    span: (usize, usize),
    kind: &str,
    cwd: &Path,
    env: &HashMap<String, String>,
    diags: &mut Vec<LineDiagnostic>,
) {
    if raw_val.is_empty() || raw_val == "-" || raw_val.contains(['*', '?', '[']) {
        return;
    }
    let resolved = cwd.join(expand_path_vars(raw_val, env));
    if !resolved.exists() {
        diags.push(LineDiagnostic::new(
            span,
            DiagSeverity::Warning,
            format!("{kind} '{raw_val}' does not exist"),
        ));
    } else if kind == "dir" && !resolved.is_dir() {
        diags.push(LineDiagnostic::new(
            span,
            DiagSeverity::Warning,
            format!("'{raw_val}' is not a directory"),
        ));
    }
}

fn expand_path_vars(raw: &str, env: &HashMap<String, String>) -> String {
    let lookup = |name: &str| -> Option<String> {
        env.get(name).cloned().or_else(|| (name == "HOME").then(|| std::env::var("HOME").ok()).flatten())
    };
    let tilde = if raw == "~" {
        lookup("HOME").unwrap_or_default()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{}/{rest}", lookup("HOME").unwrap_or_default())
    } else {
        raw.to_owned()
    };
    if !tilde.contains('$') {
        return tilde;
    }
    let mut out = String::with_capacity(tilde.len());
    let mut chars = tilde.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        let mut name = String::new();
        if chars.peek() == Some(&'{') {
            chars.next();
            name.extend(chars.by_ref().take_while(|&c| c != '}'));
        } else {
            while let Some(&c) = chars.peek().filter(|c| c.is_ascii_alphanumeric() || **c == '_') {
                name.push(c);
                chars.next();
            }
            if name.is_empty() {
                out.push('$');
                continue;
            }
        }
        if let Some(val) = lookup(&name) {
            out.push_str(&val);
        }
    }
    out
}
