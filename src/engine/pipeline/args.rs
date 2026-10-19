//! Minimal argv scanner for pipeline stages: separates options (and the words they consume) from
//! operands and shell redirections, without needing a spec for the command.

use crate::engine::line::ParsedWord;
use std::collections::HashMap;

/// Options of a command that consume following words.
pub(super) struct OptTable {
    /// Short option letters taking a value (`-n 5`, `-n5`, `-in 5`).
    pub short: &'static str,
    /// Long options taking separate values, with how many words they consume (`--arg name v`).
    pub long: &'static [(&'static str, usize)],
}

impl OptTable {
    /// Words consumed after `word` when it is an option (0 for flags, attached values, operands).
    fn consumed_after(&self, word: &str) -> usize {
        if let Some(long) = word.strip_prefix("--") {
            if long.is_empty() || long.contains('=') {
                return 0;
            }
            return self.long.iter().find(|(name, _)| *name == word).map_or(0, |(_, n)| *n);
        }
        let Some(cluster) = word.strip_prefix('-') else {
            return 0;
        };
        let last = cluster.chars().count().saturating_sub(1);
        for (i, c) in cluster.chars().enumerate() {
            if self.short.contains(c) {
                return usize::from(i == last);
            }
        }
        0
    }
}

/// Result of scanning the words after a command name.
#[derive(Debug, Default)]
pub(super) struct Scan {
    /// Indexes of operand words (not options, option values or redirections).
    pub operands: Vec<usize>,
    /// Index of a word that is the value of an option → that option word.
    pub values: HashMap<usize, String>,
    /// Option words seen (normalized unquoted text, attached values included).
    pub options: Vec<String>,
    /// Unquoted target of an input redirection (`< file`, `<file`).
    pub input: Option<usize>,
}

impl Scan {
    pub fn has_option(&self, names: &[&str]) -> bool {
        self.options.iter().any(|o| names.contains(&o.as_str()))
    }
}

/// Scans `words` (the words after the command name). `words` may hold an empty virtual word for a
/// cursor sitting on whitespace, which counts as an operand or option value like any other word.
pub(super) fn scan(words: &[ParsedWord], table: &OptTable) -> Scan {
    let mut out = Scan::default();
    let mut i = 0usize;
    let mut operands_only = false;
    while i < words.len() {
        let w = &words[i];
        if !operands_only && w.quote.is_none()
            && let Some(target) = redirect_target(&w.raw) {
                match target {
                    Redirect::InputNext => {
                        if i + 1 < words.len() {
                            out.input = Some(i + 1);
                        }
                        i += 2;
                    }
                    Redirect::OutputNext => i += 2,
                    Redirect::InputAttached => {
                        out.input = Some(i);
                        i += 1;
                    }
                    Redirect::Attached => i += 1,
                }
                continue;
            }
        let text = w.unquoted.as_str();
        if !operands_only && text == "--" {
            operands_only = true;
            i += 1;
            continue;
        }
        if !operands_only && text.starts_with('-') && text.len() > 1 {
            out.options.push(text.to_owned());
            let n = table.consumed_after(text);
            for k in 1..=n {
                out.values.insert(i + k, text.to_owned());
            }
            i += 1 + n;
            continue;
        }
        out.operands.push(i);
        i += 1;
    }
    out
}

enum Redirect {
    /// `<` alone: the next word is the input file.
    InputNext,
    /// `<file`: the word itself (after `<`) is the input file.
    InputAttached,
    /// `>`, `2>`, `>>`, `&>`: the next word is a target.
    OutputNext,
    /// `>file`, `2>&1`: self-contained.
    Attached,
}

fn redirect_target(raw: &str) -> Option<Redirect> {
    let body = raw.trim_start_matches(|c: char| c.is_ascii_digit());
    if body.starts_with("<<") || body.starts_with("<(") || body.starts_with("<&") || body.starts_with("<>") {
        return Some(if body.len() == 2 { Redirect::OutputNext } else { Redirect::Attached });
    }
    if let Some(rest) = body.strip_prefix('<') {
        return Some(if rest.is_empty() { Redirect::InputNext } else { Redirect::InputAttached });
    }
    let out = body.strip_prefix("&>").or_else(|| body.strip_prefix('>'))?;
    let out = out.trim_start_matches(['>', '|']);
    Some(if out.is_empty() { Redirect::OutputNext } else { Redirect::Attached })
}

/// Unquoted path of an input-redirection word (`<file` → `file`, otherwise the word itself).
pub(super) fn redirect_path(word: &ParsedWord) -> &str {
    if word.quote.is_none()
        && let Some(rest) = word.unquoted.strip_prefix('<') {
            return rest;
        }
    &word.unquoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::line::tokenize_segment;

    const JQ: OptTable = OptTable {
        short: "fL",
        long: &[("--arg", 2), ("--indent", 1)],
    };

    #[test]
    fn options_values_operands_and_redirects() {
        let words = tokenize_segment("-r --arg k v --indent 2 .name <in.json f.json 2>/dev/null -- -x", 0);
        let s = scan(&words, &JQ);
        let texts: Vec<&str> = s.operands.iter().map(|&i| words[i].unquoted.as_str()).collect();
        assert_eq!(texts, [".name", "f.json", "-x"]);
        assert_eq!(redirect_path(&words[s.input.unwrap()]), "in.json");
        assert_eq!(s.values.get(&2).map(String::as_str), Some("--arg"));
        assert_eq!(s.values.get(&3).map(String::as_str), Some("--arg"));
        assert!(s.has_option(&["-r"]));
    }

    #[test]
    fn short_clusters_take_value_only_when_last() {
        let words = tokenize_segment("-rf prog.jq -fprog in", 0);
        let s = scan(&words, &JQ);
        assert_eq!(s.values.get(&1).map(String::as_str), Some("-rf"));
        assert_eq!(s.operands, [3]);
    }

    #[test]
    fn separate_input_redirect() {
        let words = tokenize_segment("< data.csv x", 0);
        let s = scan(&words, &JQ);
        assert_eq!(s.input, Some(1));
        assert_eq!(s.operands, [2]);
    }
}
