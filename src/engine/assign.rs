//! Environment assignment completion: `VAR=<cursor>`, `env VAR=<cursor>` and
//! `export VAR=<cursor>` (variable names on the left of `=`, current value and paths — per
//! colon-separated segment for `*PATH` lists — on the right).

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::line::{is_env_assignment, shell_quote, unquote_partial, ParsedLine};
use crate::engine::types::path::complete_builtin_path;
use crate::engine::{CompletionRequest, EngineCompletion};

/// Completes the word under the cursor when it is a leading `VAR=value` assignment or an argument
/// of `export`; `None` otherwise.
pub(super) fn complete_assignment(
    req: &CompletionRequest<'_>,
    parsed: &ParsedLine,
) -> Option<EngineCompletion> {
    let cmd_word = parsed.words.get(parsed.cmd_word_idx);
    let in_export = cmd_word.is_some_and(|w| w.unquoted == "export" && w.quote.is_none())
        && parsed.cursor_word_idx.is_none_or(|c| c > parsed.cmd_word_idx);
    let Some(cidx) = parsed.cursor_word_idx else {
        // `export <cursor>`: offer every variable.
        return in_export.then(|| EngineCompletion {
            replace_span: parsed.replace_span,
            prefix: String::new(),
            candidates: env_name_candidates(req),
        });
    };
    let prefix = parsed.typed_prefix.as_str();
    let leading = cidx == parsed.cmd_word_idx;
    if is_env_assignment(prefix) && (leading || in_export) {
        return Some(value_completion(req, parsed, cidx));
    }
    if in_export && !prefix.starts_with('-') && prefix.chars().all(is_env_name_char) {
        return Some(EngineCompletion {
            replace_span: parsed.replace_span,
            prefix: prefix.to_string(),
            candidates: env_name_candidates(req),
        });
    }
    None
}

/// Whether a command-position prefix reads as the start of a `VAR=` assignment: an upper-case
/// shell identifier (`PA`, `RUST_LOG`), which command names practically never are.
pub(super) fn looks_like_env_name(prefix: &str) -> bool {
    let mut chars = prefix.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && prefix.chars().any(|c| c.is_ascii_uppercase())
}

fn is_env_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `NAME=` candidates for the variables of the calling shell, described by their current value.
pub(super) fn env_name_candidates(req: &CompletionRequest<'_>) -> Vec<Candidate> {
    let mut names: Vec<(&String, &String)> = req.env.iter().collect();
    names.sort();
    names
        .into_iter()
        .map(|(name, val)| Candidate {
            label: name.clone(),
            description: val.clone(),
            insert_value: format!("{name}="),
            insert_text: format!("{name}="),
            kind: CandidateKind::Arg,
            priority_tier: 2,
            ..Default::default()
        })
        .collect()
}

/// Lists whose elements are colon-separated paths (`PATH`, `MANPATH`, `LD_LIBRARY_PATH`, ...).
fn is_colon_list(name: &str, current: Option<&str>) -> bool {
    name.ends_with("PATH")
        || name.ends_with("PATHS")
        || current.is_some_and(|v| v.contains(':') && !v.contains("://") && v.contains('/'))
}

fn value_completion(req: &CompletionRequest<'_>, parsed: &ParsedLine, cidx: usize) -> EngineCompletion {
    let word = &parsed.words[cidx];
    let raw_prefix = parsed.raw_prefix.as_str();
    let (name, value_raw) = raw_prefix.split_once('=').unwrap_or((raw_prefix, ""));
    let current = req.env.get(name).map(String::as_str);
    let split = is_colon_list(name, current) && !value_raw.contains(['\'', '"', '\\']);

    let seg_start = if split {
        value_raw.rfind(':').map_or(0, |i| i + 1)
    } else {
        0
    };
    let seg_raw = &value_raw[seg_start..];
    let (seg_prefix, _) = unquote_partial(seg_raw);
    let start = word.char_start + name.chars().count() + 1 + value_raw[..seg_start].chars().count();
    // In a list, only the segment under the cursor is replaced.
    let end = if split {
        let cursor = word.char_start + raw_prefix.chars().count();
        let rest: String = word.raw.chars().skip(raw_prefix.chars().count()).collect();
        cursor + rest.find(':').map_or(rest.chars().count(), |i| rest[..i].chars().count())
    } else {
        word.char_end
    };

    let mut candidates = Vec::new();
    match current.filter(|v| !v.is_empty()) {
        Some(_) if split && seg_start == 0 => candidates.push(Candidate {
            label: format!("${name}"),
            description: "current value".to_string(),
            insert_value: format!("${name}"),
            insert_text: format!("${name}"),
            suffix: ":".to_string(),
            kind: CandidateKind::Arg,
            ..Default::default()
        }),
        Some(val) if !split => candidates.push(Candidate {
            insert_text: shell_quote(val),
            ..Candidate::word(val, Some("current value"), CandidateKind::Arg, 0)
        }),
        _ => {}
    }
    candidates.extend(
        complete_builtin_path("path", &seg_prefix, req.cwd, req.env)
            .into_iter()
            .map(|row| Candidate::from_row(row, 1)),
    );

    EngineCompletion {
        replace_span: (start, end),
        prefix: seg_prefix,
        candidates,
    }
}
