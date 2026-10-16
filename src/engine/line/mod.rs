mod quote;
mod tokenize;
mod wrappers;

pub use quote::shell_quote;
pub use tokenize::tokenize_segment;
pub use wrappers::is_env_assignment;

use crate::spec::SpecIndex;
use std::collections::HashMap;
pub(crate) use tokenize::unquote_partial;

/// Command-word aliases applied before matching: top-level spec `alias` declarations, then the
/// shell aliases the session reported. A spec alias wins over a shell alias of the same name, so a
/// spec can redirect e.g. `gco` (shell: `dot git checkout`) to the richer `git checkout` grammar.
#[derive(Clone, Copy, Default)]
pub struct Aliases<'a> {
    spec: Option<&'a HashMap<String, Vec<String>>>,
    shell: Option<&'a HashMap<String, String>>,
}

enum AliasExpansion<'a> {
    Spec(&'a [String]),
    Shell(&'a str),
}

impl<'a> Aliases<'a> {
    pub fn new(index: &'a SpecIndex, shell: &'a HashMap<String, String>) -> Self {
        Self {
            spec: Some(&index.aliases),
            shell: Some(shell),
        }
    }

    /// No alias expansion at all (the line exactly as typed).
    pub fn none() -> Self {
        Self::default()
    }

    fn get(&self, word: &str) -> Option<AliasExpansion<'a>> {
        if let Some(words) = self.spec.and_then(|m| m.get(word)) {
            return Some(AliasExpansion::Spec(words));
        }
        self.shell.and_then(|m| m.get(word)).map(|v| AliasExpansion::Shell(v.as_str()))
    }
}
pub(crate) use wrappers::skip_assignments_and_wrappers;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWord {
    pub raw: String,
    pub unquoted: String,
    pub char_start: usize,
    pub char_end: usize,
    pub quote: Option<char>,
    pub unterminated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLine {
    pub segment_start: usize,
    pub words: Vec<ParsedWord>,
    /// Index into `words` of the effective command root (after env assignments & wrappers).
    pub cmd_word_idx: usize,
    /// If the cursor is on or immediately at the end of a word in `words`, its index.
    pub cursor_word_idx: Option<usize>,
    /// Unquoted prefix of the token under cursor up to `cursor`.
    pub typed_prefix: String,
    /// Raw prefix of the token under cursor up to `cursor`.
    pub raw_prefix: String,
    /// Character span `(start, end)` in the original `line` to replace when accepting a candidate.
    pub replace_span: (usize, usize),
    /// Whether the cursor is currently inside quotes (`Some('\'')` or `Some('"')`).
    pub cursor_quote: Option<char>,
    /// Whether an alias was expanded on the command word.
    pub alias_expanded: bool,
}

pub fn parse_command_line(
    line: &str,
    cursor_chars: usize,
    aliases: Aliases<'_>,
    wrappers: &[String],
) -> ParsedLine {
    let total_chars = line.chars().count();
    let cursor = cursor_chars.min(total_chars);

    let (seg_start, seg_end) = find_active_segment(line, cursor);
    let mut words = tokenize_segment(char_slice(line, seg_start, seg_end), seg_start);

    // Find initial command word index (skipping leading VAR=value assignments and wrappers)
    let initial_cmd_idx = skip_assignments_and_wrappers(&words, 0, None, wrappers);
    let alias_expanded = expand_alias(&mut words, initial_cmd_idx, cursor, aliases);

    // Determine which word the cursor is on
    let cursor_word_idx = words
        .iter()
        .position(|w| cursor >= w.char_start && cursor <= w.char_end);

    // Advance past leading VAR=value and wrapper commands (`sudo`, `env`, `nice`, etc.)
    let cmd_word_idx = skip_assignments_and_wrappers(&words, 0, cursor_word_idx, wrappers);

    let (typed_prefix, raw_prefix, replace_span, cursor_quote) = match cursor_word_idx {
        Some(cidx) => {
            let w = &words[cidx];
            let prefix_chars = cursor.saturating_sub(w.char_start);
            let raw_p: String = w.raw.chars().take(prefix_chars).collect();
            let (unq_p, q_state) = unquote_partial(&raw_p);
            (unq_p, raw_p, (w.char_start, w.char_end), q_state)
        }
        None => (String::new(), String::new(), (cursor, cursor), None),
    };

    ParsedLine {
        segment_start: seg_start,
        words,
        cmd_word_idx,
        cursor_word_idx,
        typed_prefix,
        raw_prefix,
        replace_span,
        cursor_quote,
        alias_expanded,
    }
}

impl ParsedLine {
    /// Cursor offset in chars, recovered from the cursor word and its typed prefix.
    fn cursor(&self) -> usize {
        match self.cursor_word_idx {
            Some(cidx) => self.words[cidx].char_start + self.raw_prefix.chars().count(),
            None => self.replace_span.0,
        }
    }

    /// Re-roots the line at `word_idx`, the start of a nested command line (e.g. the word filling a
    /// `cmd`-typed positional slot): expands an alias there and skips assignments and wrappers,
    /// exactly as for the top-level command word.
    pub fn reroot(&mut self, word_idx: usize, aliases: Aliases<'_>, wrappers: &[String]) {
        let cursor = self.cursor();
        let start = skip_assignments_and_wrappers(&self.words, word_idx, None, wrappers);
        if expand_alias(&mut self.words, start, cursor, aliases) {
            self.alias_expanded = true;
            self.cursor_word_idx = self
                .words
                .iter()
                .position(|w| cursor >= w.char_start && cursor <= w.char_end);
        }
        self.cmd_word_idx =
            skip_assignments_and_wrappers(&self.words, word_idx, self.cursor_word_idx, wrappers);
    }
}

/// One-level alias expansion of the command word at `cmd_idx`, only once the cursor is strictly
/// past it. Expanded words keep the original word's span. Returns whether an alias was expanded.
fn expand_alias(
    words: &mut Vec<ParsedWord>,
    cmd_idx: usize,
    cursor: usize,
    aliases: Aliases<'_>,
) -> bool {
    let Some(cmd_word) = words.get(cmd_idx) else {
        return false;
    };
    if cursor <= cmd_word.char_end || cmd_word.quote.is_some() {
        return false;
    }
    let Some(expansion) = aliases.get(&cmd_word.unquoted) else {
        return false;
    };
    let (start, end) = (cmd_word.char_start, cmd_word.char_end);
    let mut alias_tokens = match expansion {
        AliasExpansion::Shell(value) => tokenize_segment(value, start),
        AliasExpansion::Spec(expanded) => expanded.iter().map(|w| spec_alias_word(w, start, end)).collect(),
    };
    if alias_tokens.is_empty() {
        return false;
    }
    for aw in &mut alias_tokens {
        aw.char_start = start;
        aw.char_end = end;
    }
    words.splice(cmd_idx..=cmd_idx, alias_tokens);
    true
}

/// One word of a spec alias expansion, standing in for the alias word's span.
fn spec_alias_word(word: &str, start: usize, end: usize) -> ParsedWord {
    let raw = shell_quote(word);
    let quote = raw.starts_with('\'').then_some('\'');
    ParsedWord {
        raw,
        unquoted: word.to_owned(),
        char_start: start,
        char_end: end,
        quote,
        unterminated: false,
    }
}

/// Substring of `s` covering chars `[start, end)`.
pub(crate) fn char_slice(s: &str, start: usize, end: usize) -> &str {
    let byte_at = |n: usize| s.char_indices().nth(n).map_or(s.len(), |(b, _)| b);
    let from = byte_at(start);
    let to = byte_at(end.max(start));
    &s[from..to]
}

/// Char range `(start, end)` of the pipeline/list segment (`|`, `;`, `&&`, `$(`, …) holding `cursor`.
pub(crate) fn find_active_segment(line: &str, cursor: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    let mut seg_start = 0usize;
    let mut i = 0usize;
    let mut quote: Option<char> = None;

    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if q == '\'' {
                if c == '\'' {
                    quote = None;
                }
            } else if c == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if c == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            i += 1;
            continue;
        }

        // `$((…))` is arithmetic, not a command substitution: it stays inside the word.
        if c == '$' && chars.get(i + 1) == Some(&'(') && chars.get(i + 2) == Some(&'(') {
            let mut depth = 0usize;
            let mut j = i + 3;
            while j < chars.len() {
                match chars[j] {
                    '(' => depth += 1,
                    ')' if depth > 0 => depth -= 1,
                    ')' if chars.get(j + 1) == Some(&')') => break,
                    _ => {}
                }
                j += 1;
            }
            i = (j + 2).min(chars.len());
            continue;
        }

        let sep_len = if (c == '|' && chars.get(i + 1) == Some(&'|'))
            || (c == '&' && chars.get(i + 1) == Some(&'&'))
            || (c == '$' && chars.get(i + 1) == Some(&'('))
        {
            2
        } else if matches!(c, '|' | ';' | '&' | '`' | '(') {
            1
        } else {
            0
        };

        if sep_len > 0 {
            if cursor <= i {
                return (seg_start, i);
            }
            i += sep_len;
            seg_start = i;
            continue;
        }

        i += 1;
    }

    (seg_start, chars.len())
}
