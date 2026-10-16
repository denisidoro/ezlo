//! Parameter completion anywhere in a word: `echo $U<cursor>` → `USER`, `"${HO<cursor>"` →
//! `HOME}`, `cd $XDG_C<cursor>/x` → `XDG_CONFIG_HOME`, with each variable's current value as the
//! description. Works for every command, with or without a spec (the slot is engine-owned).

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::line::ParsedLine;
use crate::engine::{CompletionRequest, EngineCompletion};

/// Completes the parameter name being typed right before the cursor; `None` when the cursor isn't
/// right after `$name` / `${name` (outside single quotes).
pub(super) fn complete_param(req: &CompletionRequest<'_>, parsed: &ParsedLine) -> Option<EngineCompletion> {
    let cidx = parsed.cursor_word_idx?;
    if parsed.cursor_quote == Some('\'') {
        return None;
    }
    let word = parsed.words.get(cidx)?;
    let raw: Vec<char> = parsed.raw_prefix.chars().collect();
    let name_len = raw.iter().rev().take_while(|c| c.is_ascii_alphanumeric() || **c == '_').count();
    let before = raw.len() - name_len;
    let braced = before >= 2 && raw[before - 1] == '{' && raw[before - 2] == '$';
    let dollar = braced || (before >= 1 && raw[before - 1] == '$');
    if !dollar {
        return None;
    }
    let dollar_at = before - if braced { 2 } else { 1 };
    // An escaped `\$` is literal.
    if dollar_at > 0 && raw[dollar_at - 1] == '\\' {
        return None;
    }
    let prefix: String = raw[before..].iter().collect();
    if prefix.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let start = word.char_start + before;
    // Replace the rest of the name after the cursor too (`$US|ER` → one name).
    let rest: String = word.raw.chars().skip(raw.len()).collect();
    let tail = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').count();
    let closed = braced && rest.chars().nth(tail) == Some('}');
    let end = word.char_start + raw.len() + tail;

    let mut names: Vec<(&String, &String)> = req.env.iter().collect();
    names.sort();
    let candidates = names
        .into_iter()
        .map(|(name, val)| {
            let insert = if braced && !closed { format!("{name}}}") } else { name.clone() };
            Candidate {
                label: name.clone(),
                description: val.clone(),
                insert_value: insert.clone(),
                insert_text: insert,
                kind: CandidateKind::Arg,
                icon: Some('$'),
                priority_tier: 2,
                ..Default::default()
            }
        })
        .collect();
    Some(EngineCompletion { replace_span: (start, end), prefix, candidates })
}
