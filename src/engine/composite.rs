//! Composite value slots (`list("<sep>")`, `kv("<sep>")`): a single shell word made of several
//! parts, where only the part under the cursor is completed and replaced.

use crate::engine::candidate::Candidate;
use crate::engine::line::shell_quote;
use crate::engine::types::format::continuation_suffix;
use crate::spec::ast::Composite;

/// The part of a composite word under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Part {
    /// Char range of the line replaced by a candidate (the part only, never the whole word).
    pub replace_span: (usize, usize),
    /// Typed text of the part, before the cursor.
    pub prefix: String,
    pub mode: PartMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PartMode {
    /// One item of a `list`: `used` holds the other items of the word; `last` is false when more
    /// items follow the cursor's (then no trailing space is inserted).
    ListItem { used: Vec<String>, last: bool },
    /// Left of the first `kv` separator: keys are inserted with the separator appended.
    KvKey { sep: String },
    /// Right of the first `kv` separator: values of `key`.
    KvValue { key: String, sep: String },
}

impl Part {
    /// Whether completing this part differs from completing the whole word: the cursor is past a
    /// separator, or the word goes on past the part.
    pub fn is_split(&self, word_span: (usize, usize)) -> bool {
        self.replace_span != word_span
    }
}

/// Locates the part under `cursor` of the composite word spanning `word_span` (chars of `line`)
/// whose typed text up to the cursor is `prefix`. `None` for quoted or escaped words, whose raw
/// text doesn't map 1:1 onto the unquoted one (they are completed as a whole).
pub(super) fn locate(
    composite: &Composite,
    line: &str,
    word_span: (usize, usize),
    cursor: usize,
    prefix: &str,
) -> Option<Part> {
    let chars: Vec<char> = line.chars().collect();
    let (start, end) = (word_span.0, word_span.1.min(chars.len()));
    if !(start <= cursor && cursor <= end) {
        return None;
    }
    let before: String = chars[start..cursor].iter().collect();
    let after: String = chars[cursor..end].iter().collect();
    if before != prefix || after.contains(['\'', '"', '\\']) || before.contains(['\'', '"', '\\']) {
        return None;
    }
    let sep = composite.sep();
    let char_len = |s: &str| s.chars().count();
    match composite {
        Composite::List { .. } => {
            let seg_start = before.rfind(sep).map_or(0, |i| i + sep.len());
            let seg_end = after.find(sep);
            let used = before[..seg_start]
                .split(sep)
                .chain(seg_end.map_or("", |i| &after[i + sep.len()..]).split(sep))
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            Some(Part {
                replace_span: (
                    start + char_len(&before[..seg_start]),
                    cursor + seg_end.map_or(char_len(&after), |i| char_len(&after[..i])),
                ),
                prefix: before[seg_start..].to_owned(),
                mode: PartMode::ListItem {
                    used,
                    last: seg_end.is_none(),
                },
            })
        }
        Composite::Kv { .. } => match before.split_once(sep) {
            Some((key, value)) => Some(Part {
                replace_span: (start + char_len(key) + char_len(sep), end),
                prefix: value.to_owned(),
                mode: PartMode::KvValue {
                    key: key.to_owned(),
                    sep: sep.to_owned(),
                },
            }),
            // Re-typing a key keeps its value: replace up to and including the separator.
            None => Some(Part {
                replace_span: (
                    start,
                    cursor + after.find(sep).map_or(char_len(&after), |i| char_len(&after[..i]) + char_len(sep)),
                ),
                prefix: before,
                mode: PartMode::KvKey { sep: sep.to_owned() },
            }),
        },
    }
}

/// Rewrites the candidates of a composite slot (static suggestions and type rows, each one
/// possibly a `key<sep>value` pair) for the part under the cursor. `value_rows` are the rows of a
/// `kv` value type, offered after the separator.
pub(super) fn part_candidates(part: &Part, slot_cands: Vec<Candidate>, value_rows: Vec<Candidate>) -> Vec<Candidate> {
    match &part.mode {
        PartMode::ListItem { used, last } => slot_cands
            .into_iter()
            .filter(|c| !used.contains(&c.insert_value))
            .map(|c| if *last { c } else { c.with_suffix("") })
            .collect(),
        PartMode::KvKey { sep } => key_candidates(slot_cands, sep),
        PartMode::KvValue { key, sep } => {
            let pair_prefix = format!("{key}{sep}");
            let mut out: Vec<Candidate> = slot_cands
                .into_iter()
                .filter_map(|c| {
                    let value = c.insert_value.strip_prefix(&pair_prefix)?.to_owned();
                    let label = if c.label == c.insert_value { value.clone() } else { c.label.clone() };
                    Some(Candidate {
                        label,
                        insert_text: shell_quote(&value),
                        suffix: continuation_suffix(&value).to_owned(),
                        insert_value: value,
                        ..c
                    })
                })
                .collect();
            for cand in value_rows {
                if !out.iter().any(|c| c.insert_value == cand.insert_value) {
                    out.push(cand);
                }
            }
            out
        }
    }
}

/// One `key<sep>` candidate per distinct key, in first-seen order, described by its known values
/// (or by the row's own description for key-only rows).
fn key_candidates(slot_cands: Vec<Candidate>, sep: &str) -> Vec<Candidate> {
    let mut out: Vec<(Candidate, Vec<String>)> = Vec::new();
    for cand in slot_cands {
        let (key, value) = match cand.insert_value.split_once(sep) {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (cand.insert_value.clone(), None),
        };
        if key.is_empty() {
            continue;
        }
        if let Some((_, values)) = out.iter_mut().find(|(c, _)| c.label == key) {
            values.extend(value.filter(|v| !v.is_empty()));
            continue;
        }
        let insert = format!("{key}{sep}");
        let keyed = Candidate {
            label: key,
            insert_text: shell_quote(&insert),
            insert_value: insert,
            suffix: String::new(),
            columns: if value.is_some() { Vec::new() } else { cand.columns.clone() },
            description: if value.is_some() { String::new() } else { cand.description.clone() },
            ..cand
        };
        out.push((keyed, value.into_iter().filter(|v| !v.is_empty()).collect()));
    }
    out.into_iter()
        .map(|(mut cand, values)| {
            if !values.is_empty() {
                cand.description = values.join(" | ");
            }
            cand
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::candidate::CandidateKind;

    fn list(sep: &str) -> Composite {
        Composite::List { sep: sep.to_owned() }
    }

    fn kv(sep: &str) -> Composite {
        Composite::Kv {
            sep: sep.to_owned(),
            value_type: None,
        }
    }

    /// Locates with the cursor at `|` in `line` (the word is the last one).
    fn at(composite: &Composite, line: &str) -> Option<Part> {
        let cursor = line.find('|').unwrap();
        let line = line.replace('|', "");
        let start = line[..cursor].rfind(' ').map_or(0, |i| i + 1);
        let end = line[cursor..].find(' ').map_or(line.len(), |i| cursor + i);
        locate(composite, &line, (start, end), cursor, &line[start..cursor])
    }

    #[test]
    fn list_replaces_only_the_last_segment() {
        let part = at(&list(","), "cargo --features a,b,c|").unwrap();
        assert_eq!(part.replace_span, (21, 22));
        assert_eq!(part.prefix, "c");
        assert_eq!(
            part.mode,
            PartMode::ListItem {
                used: vec!["a".into(), "b".into()],
                last: true
            }
        );
    }

    #[test]
    fn list_middle_segment_stops_at_next_separator() {
        let part = at(&list(":"), "x a:b|b:c").unwrap();
        assert_eq!(part.replace_span, (4, 6));
        assert_eq!(part.prefix, "b");
        assert_eq!(
            part.mode,
            PartMode::ListItem {
                used: vec!["a".into(), "c".into()],
                last: false
            }
        );
    }

    #[test]
    fn list_multichar_separator() {
        let part = at(&list("::"), "x a::|").unwrap();
        assert_eq!(part.replace_span, (5, 5));
        assert_eq!(part.prefix, "");
    }

    #[test]
    fn kv_value_side_keeps_key() {
        let part = at(&kv("="), "ssh -o Key=v|").unwrap();
        assert_eq!(part.replace_span, (11, 12));
        assert_eq!(part.prefix, "v");
        assert_eq!(
            part.mode,
            PartMode::KvValue {
                key: "Key".into(),
                sep: "=".into()
            }
        );
    }

    #[test]
    fn kv_key_side_replaces_through_existing_separator() {
        let part = at(&kv("="), "ssh -o K|ey=val").unwrap();
        assert_eq!(part.replace_span, (7, 11));
        assert_eq!(part.mode, PartMode::KvKey { sep: "=".into() });
    }

    #[test]
    fn quoted_words_are_not_split() {
        assert!(at(&list(","), "x 'a,|").is_none());
    }

    #[test]
    fn key_candidates_dedupe_and_describe_values() {
        let cands = ["A=1", "A=2", "B"]
            .iter()
            .map(|v| Candidate::word(v, Some("d"), CandidateKind::Arg, 0))
            .collect();
        let keys = key_candidates(cands, "=");
        let got: Vec<_> = keys
            .iter()
            .map(|c| (c.label.as_str(), c.insert_text.as_str(), c.description.as_str(), c.suffix.as_str()))
            .collect();
        assert_eq!(got, vec![("A", "A=", "1 | 2", ""), ("B", "B=", "d", "")]);
    }
}
