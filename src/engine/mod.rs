mod assign;
pub mod candidate;
mod composite;
pub mod diagnostics;
pub mod effect;
pub mod explain;
pub mod generator;
pub mod icons;
mod learned;
pub mod line;
mod options;
mod params;
pub mod pipeline;
pub mod preview;
pub mod rank;
pub mod recipe;
pub mod signature;
pub mod translate;
pub mod types;
pub mod walker;

pub use diagnostics::{DiagSeverity, LineDiagnostic};

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::composite::Part;
use crate::engine::generator::{GeneratorCache, GeneratorRunner};
use crate::engine::learned::{append_context_learned, append_opt_learned, extend_unique};
use crate::engine::line::{parse_command_line, Aliases, ParsedLine};
use crate::engine::options::{cluster_candidates, option_candidates};
use crate::engine::rank::{filter_and_rank_candidates, UsageLookup};
use crate::engine::types::format::continuation_suffix;
use crate::engine::types::path::complete_builtin_path;
use crate::engine::types::{evaluate_slot_types, evaluate_type_expr, PendingRefresh, TypeEvalContext};
use crate::engine::walker::{walk_nested_command_line, CursorSlot, WalkState};
use crate::spec::ast::{ArgDecl, Composite, OptDecl, TypeExpr, WhenClause};
use crate::spec::SpecIndex;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

/// Upper bound of ranked candidates returned per query.
pub const MAX_CANDIDATES: usize = 50;

pub struct CompletionRequest<'a> {
    pub line: &'a str,
    /// Cursor offset in chars.
    pub cursor: usize,
    pub cwd: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub aliases: &'a HashMap<String, String>,
    pub wrappers: &'a [String],
    pub index: &'a SpecIndex,
    pub runner: &'a dyn GeneratorRunner,
    pub cache: &'a GeneratorCache,
    pub usage: &'a dyn UsageLookup,
    pub timeout: Duration,
    pub now_ts: i64,
    pub allow_stale: bool,
}

#[derive(Debug, Clone)]
pub struct CompletionResult {
    /// Char range of the line the candidates replace.
    pub replace_start: usize,
    pub replace_end: usize,
    pub candidates: Vec<Candidate>,
    pub diagnostics: Vec<LineDiagnostic>,
    pub usage_context: String,
    pub cmd_path: Vec<String>,
    pub known_root: bool,
    /// Stale generator results served from cache; re-run them in the background.
    pub pending_refreshes: Vec<PendingRefresh>,
    /// Positional signature of the command when the cursor is on one of its positional slots.
    pub signature: Option<signature::SignatureHint>,
}

/// Candidates ezlo computes itself for the word under the cursor, bypassing the spec walk
/// (environment assignments, pipeline data-flow inspection).
pub(crate) struct EngineCompletion {
    pub replace_span: (usize, usize),
    pub prefix: String,
    pub candidates: Vec<Candidate>,
}

/// A value slot (option value or positional arg): static suggestions plus typed rows.
struct ValueSlot<'a> {
    suggestions: &'a [String],
    desc: Option<&'a str>,
    type_expr: Option<&'a TypeExpr>,
    name: &'a str,
    when: &'a [WhenClause],
    composite: Option<&'a Composite>,
}

impl<'a> ValueSlot<'a> {
    fn from_arg(arg: &'a ArgDecl) -> Self {
        Self {
            suggestions: &arg.suggestions,
            desc: arg.desc.as_deref(),
            type_expr: arg.type_expr.as_deref(),
            name: &arg.name,
            when: &arg.when,
            composite: arg.composite.as_deref(),
        }
    }
}

pub fn complete_line(req: &CompletionRequest<'_>) -> CompletionResult {
    let mut diagnostics = Vec::new();
    let aliases = Aliases::new(req.index, req.aliases);
    let (parsed, walk) = walk_nested_command_line(
        req.index,
        parse_command_line(req.line, req.cursor, aliases, req.wrappers),
        aliases,
        req.wrappers,
        |outer_parsed, outer_walk| {
            diagnostics.extend(diagnostics::validate_command_line(req, outer_parsed, outer_walk));
        },
    );
    diagnostics.extend(diagnostics::validate_command_line(req, &parsed, &walk));
    let mut refreshes = Vec::new();

    let eval_ctx = TypeEvalContext {
        index: req.index,
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

    // Parameters (`$US<cursor>`), assignments and piped data (`cat f.json | jq .<cursor>`) are
    // completed by the engine even
    // without a spec (no native fallback).
    let assignment = params::complete_param(req, &parsed)
        .or_else(|| assign::complete_assignment(req, &parsed))
        .or_else(|| pipeline::complete_piped(req, &parsed));
    let mut engine_owned = assignment.is_some();
    let part = composite_part(req, &parsed, &walk);
    let part = part.as_ref();
    let (replace_span, filter_prefix, raw_candidates) = match (&walk.slot, &assignment) {
        (_, Some(a)) => (a.replace_span, a.prefix.as_str(), a.candidates.clone()),
        (CursorSlot::RootCommand, None) => {
            let prefix = parsed.typed_prefix.as_str();
            let mut out: Vec<Candidate> = subcommand_candidates(&walk, CandidateKind::Cmd, prefix).collect();
            if assign::looks_like_env_name(prefix) {
                engine_owned = true;
                out.extend(assign::env_name_candidates(req));
            }
            (parsed.replace_span, prefix, out)
        }
        (
            CursorSlot::OptValue {
                opt,
                value_idx,
                prefix,
                replace_span,
            },
            None,
        ) => {
            let cands = opt.value_at(*value_idx).map(|vs| {
                let slot = ValueSlot {
                    suggestions: &vs.suggestions,
                    desc: opt.desc.as_deref(),
                    type_expr: vs.type_expr.as_deref(),
                    name: &vs.name,
                    when: &opt.when,
                    composite: vs.composite.as_deref(),
                };
                let mut out = value_candidates(&eval_ctx, &slot, prefix, part, &mut refreshes);
                if vs.type_expr.is_none() && vs.suggestions.is_empty() && part.is_none() {
                    append_opt_learned(&mut out, req, &walk, opt, prefix.starts_with('-'));
                }
                out
            });
            match part {
                Some(p) => (p.replace_span, p.prefix.as_str(), cands.unwrap_or_default()),
                None => (*replace_span, prefix.as_str(), cands.unwrap_or_default()),
            }
        }
        (
            CursorSlot::CommandContext {
                after_double_dash,
                current_arg,
                cluster_prefix,
                ..
            },
            None,
        ) => {
            let prefix = parsed.typed_prefix.as_str();
            let ctx = CommandContext {
                req,
                parsed: &parsed,
                walk: &walk,
                eval_ctx: &eval_ctx,
                after_double_dash: *after_double_dash,
                current_arg: *current_arg,
                part,
            };
            match (part, current_arg) {
                // Past a separator only the composite slot's values make sense.
                (Some(p), Some(arg)) if p.is_split(parsed.replace_span) => {
                    let cands =
                        value_candidates(&eval_ctx, &ValueSlot::from_arg(arg), prefix, Some(p), &mut refreshes);
                    (p.replace_span, p.prefix.as_str(), cands)
                }
                _ => {
                    let cands = match cluster_prefix {
                        Some(cp) if walk.known_root => {
                            let mut out = Vec::new();
                            cluster_candidates(&walk, cp, &mut out);
                            out
                        }
                        _ => {
                            let mut out = ctx.candidates(&mut refreshes);
                            if let Some(opt) = walk.greedy_opt {
                                greedy_value_candidates(opt, &mut out);
                            }
                            out
                        }
                    };
                    (parsed.replace_span, prefix, cands)
                }
            }
        }
    };

    let mut ranked = filter_and_rank_candidates(
        raw_candidates,
        filter_prefix,
        &walk.usage_context,
        &req.cwd.to_string_lossy(),
        req.usage,
        req.now_ts,
    );
    ranked.truncate(MAX_CANDIDATES);
    let signature = signature::signature_hint(&parsed, &walk);

    CompletionResult {
        replace_start: replace_span.0,
        replace_end: replace_span.1,
        candidates: ranked,
        diagnostics,
        usage_context: walk.usage_context,
        cmd_path: walk.cmd_path,
        known_root: walk.known_root || engine_owned,
        pending_refreshes: refreshes,
        signature,
    }
}

/// The part under the cursor when it is on a composite (`list`/`kv`) value slot.
fn composite_part(req: &CompletionRequest<'_>, parsed: &ParsedLine, walk: &WalkState<'_>) -> Option<Part> {
    let (composite, span, prefix) = match &walk.slot {
        CursorSlot::OptValue {
            opt,
            value_idx,
            prefix,
            replace_span,
        } => (opt.value_at(*value_idx)?.composite.as_deref()?, *replace_span, prefix.as_str()),
        CursorSlot::CommandContext {
            current_arg: Some(arg),
            cluster_prefix: None,
            ..
        } if walk.known_root && !parsed.typed_prefix.starts_with('-') => {
            (arg.composite.as_deref()?, parsed.replace_span, parsed.typed_prefix.as_str())
        }
        _ => return None,
    };
    composite::locate(composite, req.line, span, req.cursor, prefix)
}

/// `journalctl -b <cursor>`: the static values of a `greedy` optional value come first, since the
/// cursor word may still be that value.
fn greedy_value_candidates(opt: &OptDecl, out: &mut Vec<Candidate>) {
    let Some(vs) = &opt.value else { return };
    let values = vs.suggestions.iter().map(|s| Candidate::word(s, opt.desc.as_deref(), CandidateKind::Arg, 0));
    out.splice(0..0, values);
}

/// Subcommands (or root commands) of the walk; aliases only once `prefix` is a non-empty prefix of them.
fn subcommand_candidates<'w>(
    walk: &'w WalkState<'_>,
    kind: CandidateKind,
    prefix: &'w str,
) -> impl Iterator<Item = Candidate> + 'w {
    walk.subcommands
        .iter()
        .filter(move |sub| sub.alias_of.is_none() || (!prefix.is_empty() && sub.name.starts_with(prefix)))
        .map(move |sub| match &sub.alias_of {
            Some(canonical) => {
                let desc = match &sub.desc {
                    Some(d) => format!("{d} (alias of {canonical})"),
                    None => format!("alias of {canonical}"),
                };
                Candidate::word(&sub.name, Some(&desc), kind, 1)
            }
            None => Candidate::word(&sub.name, sub.desc.as_deref(), kind, 1),
        })
}

/// Static suggestions first (tier 0), then typed rows not already suggested (tier 1). For a
/// composite slot, `part` (the part of the word under the cursor) reshapes them into list items,
/// keys or values.
fn value_candidates(
    eval_ctx: &TypeEvalContext<'_>,
    slot: &ValueSlot<'_>,
    prefix: &str,
    part: Option<&Part>,
    refreshes: &mut Vec<PendingRefresh>,
) -> Vec<Candidate> {
    let Some(part) = part else {
        return slot_value_candidates(eval_ctx, slot, prefix, refreshes);
    };
    let (slot_prefix, value_rows) = match (&part.mode, slot.composite) {
        (composite::PartMode::ListItem { .. }, _) => (part.prefix.as_str(), Vec::new()),
        (
            composite::PartMode::KvValue { .. },
            Some(Composite::Kv {
                value_type: Some(te), ..
            }),
        ) => {
            let rows = evaluate_type_expr(eval_ctx, te, &part.prefix, refreshes)
                .into_iter()
                .map(|row| Candidate::from_row(row, 1))
                .collect();
            (prefix, rows)
        }
        _ => (prefix, Vec::new()),
    };
    let slot_cands = slot_value_candidates(eval_ctx, slot, slot_prefix, refreshes);
    composite::part_candidates(part, slot_cands, value_rows)
}

fn slot_value_candidates(
    eval_ctx: &TypeEvalContext<'_>,
    slot: &ValueSlot<'_>,
    prefix: &str,
    refreshes: &mut Vec<PendingRefresh>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = slot
        .suggestions
        .iter()
        .map(|sug| {
            Candidate::word(sug, slot.desc, CandidateKind::Arg, 0)
                .with_suffix(continuation_suffix(sug))
        })
        .collect();
    let mut seen: HashSet<String> = slot.suggestions.iter().cloned().collect();
    let rows = evaluate_slot_types(
        eval_ctx,
        slot.type_expr,
        slot.name,
        slot.when,
        prefix,
        refreshes,
    );
    out.extend(
        rows.into_iter()
            .filter(|row| seen.insert(row.insert_value.clone()))
            .map(|row| Candidate::from_row(row, 1)),
    );
    out
}

/// Cursor on a word of a (possibly unknown) command: subcommands, positional values, options and
/// scoped recipes.
struct CommandContext<'a> {
    req: &'a CompletionRequest<'a>,
    parsed: &'a ParsedLine,
    walk: &'a WalkState<'a>,
    eval_ctx: &'a TypeEvalContext<'a>,
    after_double_dash: bool,
    current_arg: Option<&'a ArgDecl>,
    /// Part under the cursor when `current_arg` is a composite slot.
    part: Option<&'a Part>,
}

impl CommandContext<'_> {
    fn candidates(&self, refreshes: &mut Vec<PendingRefresh>) -> Vec<Candidate> {
        let prefix = self.parsed.typed_prefix.as_str();
        let typing_flag = prefix.starts_with('-');
        let mut out = Vec::new();
        if !self.walk.known_root {
            append_context_learned(&mut out, self.req, self.parsed, self.walk, typing_flag, true);
            let path_tier = if out.is_empty() { 1 } else { 2 };
            extend_unique(&mut out, self.paths(path_tier));
            return out;
        }
        if !typing_flag {
            if !self.after_double_dash {
                out.extend(subcommand_candidates(self.walk, CandidateKind::Sub, prefix));
            }
            match self.current_arg {
                Some(arg) => {
                    out.extend(value_candidates(
                        self.eval_ctx,
                        &ValueSlot::from_arg(arg),
                        prefix,
                        self.part,
                        refreshes,
                    ));
                    if arg.type_expr.is_none() && arg.suggestions.is_empty() && self.part.is_none() {
                        append_context_learned(&mut out, self.req, self.parsed, self.walk, false, false);
                    }
                }
                None => {
                    append_context_learned(&mut out, self.req, self.parsed, self.walk, false, false);
                    if self.walk.subcommands.is_empty() {
                        extend_unique(&mut out, self.paths(2));
                    }
                }
            }
        }
        if !self.after_double_dash {
            option_candidates(self.walk, prefix, &mut out);
            if !typing_flag {
                out.extend(self.scoped_recipes());
            }
        }
        out
    }

    fn paths(&self, tier: u8) -> Vec<Candidate> {
        complete_builtin_path(
            "path",
            &self.parsed.typed_prefix,
            self.req.cwd,
            self.req.env,
        )
        .into_iter()
        .map(|row| Candidate::from_row(row, tier))
        .collect()
    }

    fn scoped_recipes(&self) -> impl Iterator<Item = Candidate> + '_ {
        self.req
            .index
            .recipes
            .iter()
            .enumerate()
            .filter(|(_, rec)| !rec.scope.is_empty() && rec.scope == self.walk.cmd_path)
            .map(|(ridx, rec)| Candidate {
                label: rec.desc.clone(),
                description: rec.template.clone(),
                insert_value: rec.template.clone(),
                insert_text: rec.template.clone(),
                kind: CandidateKind::Recipe,
                priority_tier: 3,
                recipe_index: Some(ridx),
                ..Default::default()
            })
    }
}
