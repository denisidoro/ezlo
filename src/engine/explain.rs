//! `ezlo explain` / `Alt-H`: dissects the command line under the cursor token by token: alias and
//! wrapper expansion, what each word binds to in the spec grammar (subcommands, options, values,
//! positionals, their types, parameter bindings and `when` clauses), pre-flight diagnostics, and
//! where the current completion candidates come from.

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::diagnostics::{DiagSeverity, LineDiagnostic};
use crate::engine::effect::{line_effect, LineEffect};
use crate::engine::preview::{PreviewKind, PreviewLine};
use crate::engine::line::{char_slice, find_active_segment, is_env_assignment, parse_command_line, Aliases, ParsedLine};
use crate::engine::types::{evaluate_when_status, type_param_bindings, TypeEvalContext, WhenStatus};
use crate::engine::walker::{walk_nested_command_line, CursorSlot, WalkState, WordRoleKind};
use crate::engine::{CompletionRequest, CompletionResult};
use crate::spec::ast::{ArgDecl, Composite, Effect, OptDecl, TypeExpr, WhenClause};
use crate::spec::{CmdOrigin, SpecIndex};
use serde::Serialize;

/// Provenance label of candidates synthesized from learned usage or shell history.
pub const SOURCE_LEARNED: &str = "learned history";
/// Provenance label of candidates produced by the native zsh completion worker.
pub const SOURCE_NATIVE: &str = "native zsh";
/// Provenance label of candidates ezlo builds without a spec (paths, env assignments).
pub const SOURCE_BUILTIN: &str = "built-in";
/// Provenance label of spec candidates whose tier is unknown (specs merged without tiers).
pub const SOURCE_SPEC: &str = "spec";

#[derive(Debug, Clone, Serialize)]
pub struct Explanation {
    /// Char range of the explained pipeline/list segment.
    pub segment: (usize, usize),
    /// One entry per command level: the outer command, then any nested `cmd`-slot command lines.
    pub commands: Vec<CommandExplain>,
    pub diagnostics: Vec<LineDiagnostic>,
    /// Declared side-effect class of the whole line (every segment, not only the explained one).
    pub effect: LineEffect,
    pub completion: CompletionExplain,
    /// Blast-radius / dry-run effect preview (filled by callers that have `[preview]` options).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub preview: Vec<PreviewLine>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandExplain {
    /// Canonical command path (aliases resolved).
    pub path: Vec<String>,
    /// Whether a spec knows the root command.
    pub known: bool,
    pub description: Option<String>,
    /// Spec files declaring this command path, highest precedence first.
    pub origins: Vec<OriginExplain>,
    /// How the line was rewritten before matching (alias expansion).
    pub notes: Vec<String>,
    pub tokens: Vec<TokenExplain>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OriginExplain {
    /// `user spec`, `bundled spec`, `generated spec` (or `spec` when untiered).
    pub tier: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenExplain {
    pub text: String,
    /// Char span in the line.
    pub start: usize,
    pub end: usize,
    /// `assignment`, `wrapper`, `command`, `subcommand`, `option`, `value`, `argument`, `--`,
    /// `nested`, `unknown`.
    pub role: String,
    pub detail: String,
    /// Value-slot details: effective type, parameter bindings, `when` clauses, static values.
    pub bindings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletionExplain {
    /// What the word under the cursor is being completed as.
    pub slot: String,
    pub prefix: String,
    /// Candidate counts per provenance, most candidates first.
    pub sources: Vec<SourceCount>,
    /// No spec knows the command: the daemon asks the native zsh completion system.
    pub native_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceCount {
    pub source: String,
    pub count: usize,
}

/// Visual role of a rendered explanation line (mapped to `region_highlight` styles by the client).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainStyle {
    Header,
    Token,
    Detail,
    Note,
    Warning,
    Error,
}

impl ExplainStyle {
    /// zsh `region_highlight` style of the line's text (empty: terminal default).
    pub fn zsh_style(self) -> &'static str {
        match self {
            Self::Header => "bold",
            Self::Token => "",
            Self::Detail => "fg=8",
            Self::Note => "fg=cyan",
            Self::Warning => "fg=yellow",
            Self::Error => "fg=red",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainLine {
    pub style: ExplainStyle,
    pub text: String,
}

/// Explains the segment of `req.line` under `req.cursor`. `comp` is the completion result for the
/// same request (its diagnostics and candidates are reported); `native_applied` tells that its
/// candidates were replaced by the native zsh fallback.
pub fn explain_line(req: &CompletionRequest<'_>, comp: &CompletionResult, native_applied: bool) -> Explanation {
    let total = req.line.chars().count();
    let cursor = req.cursor.min(total);
    let segment = find_active_segment(req.line, cursor);

    // Walk the whole segment: a trailing space puts the cursor past every word.
    let mut full = char_slice(req.line, 0, segment.1).to_owned();
    full.push(' ');
    let full_cursor = segment.1 + 1;
    let aliases = Aliases::new(req.index, req.aliases);
    let parsed = parse_command_line(&full, full_cursor, aliases, req.wrappers);

    let mut commands = Vec::new();
    let mut level_start = 0;
    let (inner_parsed, inner_walk) =
        walk_nested_command_line(req.index, parsed, aliases, req.wrappers, |p, w| {
            commands.push(explain_level(req, &full, p, w, level_start));
            level_start = w.nested_cmd_word_idx.unwrap_or(level_start);
        });
    if !inner_parsed.words.is_empty() {
        commands.push(explain_level(req, &full, &inner_parsed, &inner_walk, level_start));
    }

    Explanation {
        segment,
        commands,
        diagnostics: comp.diagnostics.clone(),
        effect: line_effect(req),
        completion: explain_completion(req, comp, native_applied),
        preview: Vec::new(),
    }
}

fn explain_level(
    req: &CompletionRequest<'_>,
    full: &str,
    parsed: &ParsedLine,
    walk: &WalkState<'_>,
    start: usize,
) -> CommandExplain {
    let index = req.index;
    let mut tokens = Vec::new();
    let mut notes = Vec::new();
    let token = |idx: usize, role: &str, detail: String, bindings: Vec<String>| {
        let w = &parsed.words[idx];
        TokenExplain {
            text: w.raw.clone(),
            start: w.char_start,
            end: w.char_end,
            role: role.to_owned(),
            detail,
            bindings,
        }
    };

    if let Some(note) = alias_note(full, parsed, start) {
        notes.push(note);
    }

    // Leading `VAR=value` assignments and wrappers (`sudo -u root`, `env`, …).
    let mut in_wrapper = false;
    for idx in start..parsed.cmd_word_idx.min(parsed.words.len()) {
        let word = &parsed.words[idx].unquoted;
        let (role, detail) = if is_env_assignment(word) {
            let name = word.split_once('=').map_or(word.as_str(), |(n, _)| n);
            ("assignment", format!("sets ${name} for the command"))
        } else if req.wrappers.iter().any(|w| w == word) {
            in_wrapper = true;
            ("wrapper", "runs the command that follows (skipped for completion)".to_owned())
        } else if in_wrapper {
            ("wrapper", "wrapper argument".to_owned())
        } else {
            ("unknown", String::new())
        };
        tokens.push(token(idx, role, detail, Vec::new()));
    }

    let Some(root_word) = parsed.words.get(parsed.cmd_word_idx) else {
        return CommandExplain {
            path: Vec::new(),
            known: false,
            description: None,
            origins: Vec::new(),
            notes,
            tokens,
        };
    };

    let root_path = walk.cmd_path.first().cloned().into_iter().collect::<Vec<_>>();
    let root_detail = if walk.known_root {
        let mut d = find_desc(index, &root_path).unwrap_or_default();
        if root_word.unquoted != root_path[0] {
            d = join_detail(d, format!("alias of {}", root_path[0]));
        }
        d
    } else {
        "no spec: options and arguments come from learned history and native zsh".to_owned()
    };
    tokens.push(token(parsed.cmd_word_idx, "command", root_detail, Vec::new()));

    let eval_ctx = TypeEvalContext {
        index,
        cwd: req.cwd,
        env: req.env,
        aliases: req.aliases,
        opt_values: &walk.opt_values,
        arg_values: &walk.arg_values,
        present_flags: &walk.present_flags,
        runner: req.runner,
        cache: req.cache,
        timeout: req.timeout,
        allow_stale: req.allow_stale,
    };

    let mut path = root_path;
    for role in &walk.word_roles {
        let word = &parsed.words[role.word_idx].unquoted;
        let tok = match &role.kind {
            WordRoleKind::Subcommand { canonical } => {
                path.push(canonical.clone());
                let mut detail = find_desc(index, &path).unwrap_or_default();
                if word != canonical {
                    detail = join_detail(detail, format!("alias of {canonical}"));
                }
                token(role.word_idx, "subcommand", detail, Vec::new())
            }
            WordRoleKind::Options(opts) if opts.is_empty() => {
                if walk.known_root {
                    token(role.word_idx, "unknown", format!("not an option of {}", path.join(" ")), Vec::new())
                } else {
                    token(role.word_idx, "option", String::new(), Vec::new())
                }
            }
            WordRoleKind::Options(opts) => {
                let detail = opts.iter().map(|o| opt_summary(o)).collect::<Vec<_>>().join("; ");
                // An inline value (`--flag=v`, `-nVALUE`) binds to the last option's slot.
                let inline = word.contains('=')
                    || opts.last().is_some_and(|o| {
                        let ends_with_flag = o.flags.iter().any(|f| {
                            f == word || (f.len() == 2 && !word.starts_with("--") && word.ends_with(&f[1..]))
                        });
                        o.value.is_some() && !ends_with_flag
                    });
                let bindings = match opts.last() {
                    Some(o) if inline => opt_value_bindings(&eval_ctx, o, 0, &walk.present_flags),
                    _ => Vec::new(),
                };
                token(role.word_idx, "option", detail, bindings)
            }
            WordRoleKind::OptValue { opt, index } => {
                let bindings = opt_value_bindings(&eval_ctx, opt, *index, &walk.present_flags);
                token(role.word_idx, "value", value_of(opt, *index), bindings)
            }
            WordRoleKind::DoubleDash => token(
                role.word_idx,
                "--",
                "end of options: the following words are positional".to_owned(),
                Vec::new(),
            ),
            WordRoleKind::Positional { index: pos, arg: Some(arg) } => {
                let mut detail = arg_placeholder(arg);
                if let Some(d) = &arg.desc {
                    detail = join_detail(detail, d.clone());
                }
                if *pos > 0 && arg.variadic {
                    detail = join_detail(detail, format!("item {}", pos + 1));
                }
                let bindings = slot_bindings(&eval_ctx, &SlotView::from_arg(arg), &walk.present_flags);
                token(role.word_idx, "argument", detail, bindings)
            }
            WordRoleKind::Positional { index: pos, arg: None } => {
                if walk.matched_cmd.is_some() {
                    token(role.word_idx, "unknown", format!("extra argument #{}: {} takes no more", pos + 1, path.join(" ")), Vec::new())
                } else {
                    token(role.word_idx, "argument", format!("argument #{}", pos + 1), Vec::new())
                }
            }
            WordRoleKind::NestedCommand { arg } => {
                let slot = arg.map(arg_placeholder).unwrap_or_else(|| "<cmd>".to_owned());
                token(
                    role.word_idx,
                    "nested",
                    format!("fills {slot}: the rest is explained as its own command line"),
                    Vec::new(),
                )
            }
        };
        tokens.push(tok);
    }

    CommandExplain {
        path: walk.cmd_path.clone(),
        known: walk.known_root,
        description: walk.matched_cmd.and_then(|c| c.desc.clone()),
        origins: index.origins_of(&walk.cmd_path).iter().map(origin_explain).collect(),
        notes,
        tokens,
    }
}

/// `alias gco → git checkout` when the level's command word came from an alias: the expanded
/// words keep the span of the alias word, so their raw text differs from the line there.
fn alias_note(full: &str, parsed: &ParsedLine, start: usize) -> Option<String> {
    let first = parsed
        .words
        .iter()
        .skip(start)
        .position(|w| char_slice(full, w.char_start, w.char_end) != w.raw)?
        + start;
    let span = (parsed.words[first].char_start, parsed.words[first].char_end);
    let expansion: Vec<&str> = parsed.words[first..]
        .iter()
        .take_while(|w| (w.char_start, w.char_end) == span)
        .map(|w| w.raw.as_str())
        .collect();
    Some(format!("alias {} → {}", char_slice(full, span.0, span.1), expansion.join(" ")))
}

fn find_desc(index: &SpecIndex, path: &[String]) -> Option<String> {
    index.cmds.iter().find(|c| c.path == path).and_then(|c| c.desc.clone())
}

fn join_detail(a: String, b: String) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b,
        (_, true) => a,
        _ => format!("{a} · {b}"),
    }
}

fn origin_explain(o: &CmdOrigin) -> OriginExplain {
    OriginExplain {
        tier: o.tier.map_or(SOURCE_SPEC, |t| t.label()).to_owned(),
        source: o.source.clone(),
    }
}

/// `value of --flag <name>`, or `value 2 of --arg <value> (<name> <value>)` for a later value of a
/// multi-value option.
fn value_of(opt: &OptDecl, index: usize) -> String {
    let flag = opt.flags.last().map_or("", String::as_str);
    let placeholder = opt.value_at(index).map(|v| format!(" {}", v.placeholder())).unwrap_or_default();
    if opt.extra_values.is_empty() {
        format!("value of {flag}{placeholder}")
    } else {
        format!("value {} of {flag}{placeholder} ({})", index + 1, opt.value_placeholders())
    }
}

/// `-b/--branch <name>: Create a new branch` (+ markers for `global`/`hidden`/`required`).
fn opt_summary(opt: &OptDecl) -> String {
    let mut s = opt.flags.join("/");
    if opt.value.is_some() {
        s.push(' ');
        s.push_str(&opt.value_placeholders());
    }
    let markers: Vec<&str> = [
        (opt.global, "global"),
        (opt.hidden, "hidden"),
        (opt.required, "required"),
        (opt.greedy, "greedy"),
    ]
        .into_iter()
        .filter_map(|(on, m)| on.then_some(m))
        .collect();
    if !markers.is_empty() {
        s.push_str(&format!(" [{}]", markers.join(", ")));
    }
    if let Some(d) = &opt.desc {
        s.push_str(": ");
        s.push_str(d);
    }
    s
}

fn arg_placeholder(arg: &ArgDecl) -> String {
    let dots = if arg.variadic { "..." } else { "" };
    if arg.required {
        format!("<{}{dots}>", arg.name)
    } else {
        format!("[{}{dots}]", arg.name)
    }
}

/// The parts of a value slot (option value or positional) that decide its candidates.
struct SlotView<'a> {
    type_expr: Option<&'a TypeExpr>,
    suggestions: &'a [String],
    when: &'a [WhenClause],
    composite: Option<&'a Composite>,
}

impl<'a> SlotView<'a> {
    fn from_arg(arg: &'a ArgDecl) -> Self {
        Self {
            type_expr: arg.type_expr.as_deref(),
            suggestions: &arg.suggestions,
            when: &arg.when,
            composite: arg.composite.as_deref(),
        }
    }
}

/// Bindings of value slot `index` of `opt` (`when` clauses refine the first value only).
fn opt_value_bindings(
    ctx: &TypeEvalContext<'_>,
    opt: &OptDecl,
    index: usize,
    present: &std::collections::HashMap<String, Option<String>>,
) -> Vec<String> {
    let Some(vs) = opt.value_at(index) else {
        return Vec::new();
    };
    let slot = SlotView {
        type_expr: vs.type_expr.as_deref(),
        suggestions: &vs.suggestions,
        when: if index == 0 { &opt.when } else { &[] },
        composite: vs.composite.as_deref(),
    };
    slot_bindings(ctx, &slot, present)
}

/// Effective type (with resolved parameters), `when` clause states, static values and composite
/// modifier of a value slot.
fn slot_bindings(
    ctx: &TypeEvalContext<'_>,
    slot: &SlotView<'_>,
    present: &std::collections::HashMap<String, Option<String>>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut active: Option<&TypeExpr> = None;
    for wc in slot.when {
        let state = match evaluate_when_status(wc, present) {
            WhenStatus::True => {
                active.get_or_insert(&wc.type_expr);
                "active".to_owned()
            }
            WhenStatus::Pending(hint) => format!("inactive, {hint}"),
            WhenStatus::Contradicted => "inactive".to_owned(),
        };
        out.push(format!("when {}: {} ({state})", when_cond(wc), type_label(&wc.type_expr)));
    }
    match active.or(slot.type_expr) {
        Some(te) => {
            let mut line = format!("type {}", type_label(te));
            let params: Vec<String> = type_param_bindings(ctx, te)
                .into_iter()
                .map(|p| match p.value {
                    Some(v) => format!("{} = {v} ← {}{}", p.name, p.source, if p.explicit { "" } else { " (default)" }),
                    None => format!("{} unset ({})", p.name, p.source),
                })
                .collect();
            if !params.is_empty() {
                line.push_str(&format!(" with {}", params.join(", ")));
            }
            out.insert(0, line);
        }
        None if slot.suggestions.is_empty() && slot.when.is_empty() => {
            out.push("untyped: learned values from history".to_owned());
        }
        None => {}
    }
    if !slot.suggestions.is_empty() {
        const MAX_SHOWN: usize = 6;
        let mut vals = slot.suggestions.iter().take(MAX_SHOWN).cloned().collect::<Vec<_>>().join(" | ");
        if slot.suggestions.len() > MAX_SHOWN {
            vals.push_str(" | …");
        }
        out.push(format!("values {vals}"));
    }
    if let Some(c) = slot.composite {
        out.push(match c {
            Composite::List { sep } => format!("list(\"{sep}\"): {sep}-separated unique items"),
            Composite::Kv { sep, value_type } => match value_type {
                Some(te) => format!("kv(\"{sep}\", {}): key{sep}value", type_label(te)),
                None => format!("kv(\"{sep}\"): key{sep}value"),
            },
        });
    }
    out
}

fn when_cond(wc: &WhenClause) -> String {
    wc.cond
        .iter()
        .map(|t| {
            let bang = if t.negated { "!" } else { "" };
            match &t.value {
                Some(v) => format!("{bang}{}={v}", t.flag),
                None => format!("{bang}{}", t.flag),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `name`, `name where …` (refinement clauses aren't spelled out).
fn type_label(te: &TypeExpr) -> String {
    let mut s = te.type_ref.name.clone();
    if te.where_cond.is_some() {
        s.push_str(" where …");
    }
    s
}

fn explain_completion(req: &CompletionRequest<'_>, comp: &CompletionResult, native_applied: bool) -> CompletionExplain {
    let aliases = Aliases::new(req.index, req.aliases);
    let parsed = parse_command_line(req.line, req.cursor, aliases, req.wrappers);
    let prefix = parsed.typed_prefix.clone();
    let (_, walk) = walk_nested_command_line(req.index, parsed, aliases, req.wrappers, |_, _| {});
    let slot = describe_slot(&walk, &prefix);

    let mut sources: Vec<SourceCount> = Vec::new();
    for cand in &comp.candidates {
        let source = candidate_source(req.index, comp, cand, native_applied);
        match sources.iter_mut().find(|s| s.source == source) {
            Some(s) => s.count += 1,
            None => sources.push(SourceCount {
                source: source.to_owned(),
                count: 1,
            }),
        }
    }
    sources.sort_by_key(|s| std::cmp::Reverse(s.count));

    CompletionExplain {
        slot,
        prefix,
        sources,
        native_fallback: !comp.known_root,
    }
}

fn describe_slot(walk: &WalkState<'_>, prefix: &str) -> String {
    match &walk.slot {
        CursorSlot::RootCommand => "command name".to_owned(),
        CursorSlot::OptValue { opt, value_idx, .. } => value_of(opt, *value_idx),
        CursorSlot::CommandContext { cluster_prefix: Some(_), .. } => "short option cluster".to_owned(),
        CursorSlot::CommandContext { .. } if prefix.starts_with('-') => "option".to_owned(),
        CursorSlot::CommandContext {
            current_arg,
            after_double_dash,
            positional_index,
            ..
        } => {
            let arg = current_arg.map_or_else(|| format!("argument #{}", positional_index + 1), |a| format!("argument {}", arg_placeholder(a)));
            if !after_double_dash && !walk.subcommands.is_empty() {
                format!("subcommand or {arg}")
            } else {
                arg
            }
        }
    }
}

/// Provenance of one completion candidate.
fn candidate_source(index: &SpecIndex, comp: &CompletionResult, cand: &Candidate, native_applied: bool) -> &'static str {
    if native_applied {
        return SOURCE_NATIVE;
    }
    if cand.learned || cand.kind == CandidateKind::History {
        return SOURCE_LEARNED;
    }
    let tier_of = |path: &[String]| {
        let origins = index.origins_of(path);
        origins.first().map(|o| o.tier.map_or(SOURCE_SPEC, |t| t.label()))
    };
    let spec_known = index.cmds.iter().any(|c| c.path == comp.cmd_path);
    match cand.kind {
        CandidateKind::File | CandidateKind::Dir if !spec_known => SOURCE_BUILTIN,
        CandidateKind::Cmd => tier_of(std::slice::from_ref(&cand.label)).unwrap_or(SOURCE_SPEC),
        CandidateKind::Sub => {
            let mut child = comp.cmd_path.clone();
            child.push(cand.label.clone());
            tier_of(&child).or_else(|| tier_of(&comp.cmd_path)).unwrap_or(SOURCE_SPEC)
        }
        _ if spec_known => tier_of(&comp.cmd_path).unwrap_or(SOURCE_SPEC),
        _ => SOURCE_BUILTIN,
    }
}

impl Explanation {
    /// Human-readable lines (CLI output and the zsh `Alt-H` overlay).
    pub fn render(&self) -> Vec<ExplainLine> {
        let mut out = Vec::new();
        let line = |style: ExplainStyle, text: String| ExplainLine { style, text };
        let width = self
            .commands
            .iter()
            .flat_map(|c| &c.tokens)
            .map(|t| t.text.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(4, 18);

        for cmd in &self.commands {
            let mut header = if cmd.path.is_empty() { "(no command)".to_owned() } else { cmd.path.join(" ") };
            if let Some(d) = &cmd.description {
                header.push_str(" — ");
                header.push_str(d);
            }
            if !cmd.origins.is_empty() {
                let tiers: Vec<&str> = cmd.origins.iter().map(|o| o.tier.as_str()).collect();
                header.push_str(&format!("  [{}]", tiers.join(" over ")));
            } else if !cmd.known && !cmd.path.is_empty() {
                header.push_str("  [no spec]");
            }
            out.push(line(ExplainStyle::Header, header));
            for note in &cmd.notes {
                out.push(line(ExplainStyle::Note, format!("  {note}")));
            }
            for tok in &cmd.tokens {
                let text = truncate(&tok.text, width);
                let pad = width.saturating_sub(text.chars().count());
                let mut s = format!("  {text}{:pad$}  {:<10}", "", tok.role);
                if !tok.detail.is_empty() {
                    s.push(' ');
                    s.push_str(&tok.detail);
                }
                let style = if tok.role == "unknown" { ExplainStyle::Warning } else { ExplainStyle::Token };
                out.push(line(style, s.trim_end().to_owned()));
                for b in &tok.bindings {
                    out.push(line(ExplainStyle::Detail, format!("  {:width$}  ↳ {b}", "")));
                }
            }
        }

        if let Some(summary) = self.effect.summary() {
            let style = match self.effect.level {
                Some(Effect::Destroy | Effect::Remote) => ExplainStyle::Warning,
                Some(Effect::Write) => ExplainStyle::Note,
                _ => ExplainStyle::Detail,
            };
            out.push(line(style, format!("  effect: {summary}")));
        }

        for d in &self.diagnostics {
            let (style, mark) = match d.severity {
                DiagSeverity::Error => (ExplainStyle::Error, "✗"),
                DiagSeverity::Warning => (ExplainStyle::Warning, "⚠"),
            };
            out.push(line(style, format!("  {mark} {}", d.message)));
        }

        for p in &self.preview {
            let (style, mark) = match p.kind {
                PreviewKind::Warning => (ExplainStyle::Warning, "⚠"),
                PreviewKind::Effect => (ExplainStyle::Note, "⇢"),
                PreviewKind::DryRun => (ExplainStyle::Detail, "⇢"),
                PreviewKind::Value => (ExplainStyle::Note, "="),
                PreviewKind::Info => (ExplainStyle::Detail, "·"),
            };
            out.push(line(style, format!("  {mark} {}", p.text)));
        }

        let c = &self.completion;
        let mut s = format!("▸ completing {}", c.slot);
        if !c.prefix.is_empty() {
            s.push_str(&format!(" from `{}`", c.prefix));
        }
        if c.sources.is_empty() {
            s.push_str(if c.native_fallback { ": no spec, falls back to native zsh" } else { ": no candidates" });
        } else {
            let parts: Vec<String> = c.sources.iter().map(|s| format!("{} {}", s.count, s.source)).collect();
            s.push_str(&format!(": {}", parts.join(", ")));
            if c.native_fallback && !c.sources.iter().any(|s| s.source == SOURCE_NATIVE) {
                s.push_str(" (no spec: the zsh widget asks native zsh)");
            }
        }
        out.push(line(ExplainStyle::Note, s));
        out
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}
