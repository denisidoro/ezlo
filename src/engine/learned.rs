//! Synthesizes completion candidates from recorded `(ctx, label)` usage history.

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::line::ParsedLine;
use crate::engine::types::format::continuation_suffix;
use crate::engine::walker::WalkState;
use crate::engine::CompletionRequest;
use crate::spec::ast::OptDecl;

pub(super) fn append_opt_learned(
    out: &mut Vec<Candidate>,
    req: &CompletionRequest<'_>,
    walk: &WalkState<'_>,
    opt: &OptDecl,
    typing_flag: bool,
) {
    let tier = if out.is_empty() { 1 } else { 2 };
    let cwd = req.cwd.to_string_lossy();
    push_from_ctx(
        out,
        req,
        &walk.usage_context,
        &cwd,
        typing_flag,
        false,
        opt.desc.as_deref(),
        tier,
        &[],
    );
    let cmd_prefix = walk.cmd_path.join(" ");
    for flag in &opt.flags {
        let alt_ctx = if cmd_prefix.is_empty() {
            flag.clone()
        } else {
            format!("{cmd_prefix} {flag}")
        };
        if alt_ctx != walk.usage_context {
            push_from_ctx(
                out,
                req,
                &alt_ctx,
                &cwd,
                typing_flag,
                false,
                opt.desc.as_deref(),
                tier,
                &[],
            );
        }
    }
}

pub(super) fn append_context_learned(
    out: &mut Vec<Candidate>,
    req: &CompletionRequest<'_>,
    parsed: &ParsedLine,
    walk: &WalkState<'_>,
    typing_flag: bool,
    allow_flags: bool,
) {
    let tier = if out.is_empty() { 1 } else { 2 };
    let cwd = req.cwd.to_string_lossy();
    let cursor_limit = parsed.cursor_word_idx.unwrap_or(parsed.words.len());
    let prior_words: Vec<&str> = parsed
        .words
        .get(parsed.cmd_word_idx..cursor_limit)
        .unwrap_or_default()
        .iter()
        .map(|w| w.unquoted.as_str())
        .collect();
    let used_words = prior_words.get(1..).unwrap_or_default();

    push_from_ctx(
        out,
        req,
        &walk.usage_context,
        &cwd,
        typing_flag,
        allow_flags,
        None,
        tier,
        used_words,
    );

    let line_ctx = prior_words.join(" ");
    if !line_ctx.is_empty() && line_ctx != walk.usage_context {
        push_from_ctx(
            out,
            req,
            &line_ctx,
            &cwd,
            typing_flag,
            allow_flags,
            None,
            tier,
            used_words,
        );
    }

    if typing_flag && allow_flags {
        if let Some(root) = walk.cmd_path.first().filter(|r| **r != walk.usage_context) {
            push_from_ctx(out, req, root, &cwd, true, true, None, tier, used_words);
        }
    }
}

pub(super) fn extend_unique(out: &mut Vec<Candidate>, extra: impl IntoIterator<Item = Candidate>) {
    for cand in extra {
        if !out
            .iter()
            .any(|c| c.insert_value == cand.insert_value || c.label == cand.label)
        {
            out.push(cand);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_from_ctx(
    out: &mut Vec<Candidate>,
    req: &CompletionRequest<'_>,
    ctx: &str,
    cwd: &str,
    typing_flag: bool,
    allow_flags: bool,
    desc: Option<&str>,
    tier: u8,
    used_words: &[&str],
) {
    for val in req.usage.learned_values(ctx) {
        let is_flag = val.starts_with('-') && val.len() > 1;
        if typing_flag {
            if !allow_flags || !is_flag || used_words.contains(&val.as_str()) {
                continue;
            }
        } else if val.starts_with('-') {
            continue;
        }
        if out.iter().any(|c| c.insert_value == val || c.label == val) {
            continue;
        }
        let kind = if is_flag {
            CandidateKind::Opt
        } else {
            CandidateKind::Arg
        };
        let mut cand = Candidate::word(&val, desc, kind, tier).with_suffix(continuation_suffix(&val));
        cand.score = req.usage.lookup_usage(ctx, &val, cwd).compute_score(req.now_ts);
        cand.learned = true;
        out.push(cand);
    }
}
