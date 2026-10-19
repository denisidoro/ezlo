//! Pipeline-aware completion: when the stage under the cursor is a data consumer (`jq`, `grep`,
//! `rg`, `awk`, `cut`) and its input is a file ezlo can read in-process — piped from a
//! side-effect-free upstream stage (`cat f.json | jq …`, `head f.csv | awk …`, `< f |`), redirected
//! (`jq … < f`) or named as the stage's own operand — candidates are synthesized from a bounded
//! sample of that file: `jq` paths, column references, delimiters, headers and frequent tokens.
//! Upstream commands are never executed (see `source`).

mod args;
mod columns;
mod json;
pub mod source;

pub use json::{collect_paths, JsonPath};
pub use source::{load_sample, Sample, MAX_SAMPLE_BYTES};

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::line::{shell_quote, ParsedLine, ParsedWord};
use crate::engine::{CompletionRequest, EngineCompletion};
use args::{scan, OptTable, Scan};
use std::sync::Arc;

/// A data-consuming command ezlo completes from its input sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Jq,
    Grep,
    Rg,
    Awk,
    Cut,
}

impl Tool {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "jq" | "gojq" | "jaq" => Self::Jq,
            "grep" | "egrep" | "fgrep" | "ggrep" => Self::Grep,
            "rg" => Self::Rg,
            "awk" | "gawk" | "mawk" | "nawk" => Self::Awk,
            "cut" => Self::Cut,
            _ => return None,
        })
    }

    fn options(self) -> OptTable {
        match self {
            Self::Jq => OptTable {
                short: "fL",
                long: &[
                    ("--arg", 2),
                    ("--argjson", 2),
                    ("--slurpfile", 2),
                    ("--rawfile", 2),
                    ("--indent", 1),
                    ("--from-file", 1),
                ],
            },
            Self::Grep => OptTable {
                short: "efmABCdD",
                long: &[
                    ("--regexp", 1),
                    ("--file", 1),
                    ("--max-count", 1),
                    ("--after-context", 1),
                    ("--before-context", 1),
                    ("--context", 1),
                    ("--include", 1),
                    ("--exclude", 1),
                    ("--exclude-dir", 1),
                    ("--label", 1),
                ],
            },
            Self::Rg => OptTable {
                short: "efgtTmABCMjEr",
                long: &[
                    ("--regexp", 1),
                    ("--file", 1),
                    ("--glob", 1),
                    ("--iglob", 1),
                    ("--type", 1),
                    ("--type-not", 1),
                    ("--max-count", 1),
                    ("--after-context", 1),
                    ("--before-context", 1),
                    ("--context", 1),
                    ("--max-columns", 1),
                    ("--max-depth", 1),
                    ("--threads", 1),
                    ("--encoding", 1),
                    ("--replace", 1),
                    ("--sort", 1),
                    ("--sortr", 1),
                    ("--color", 1),
                ],
            },
            Self::Awk => OptTable {
                short: "Fvf",
                long: &[],
            },
            Self::Cut => OptTable {
                short: "dfbc",
                long: &[],
            },
        }
    }
}

/// What the word under the cursor is, for a data-consuming tool.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DataSlot {
    /// The `jq` filter.
    JqFilter,
    /// A `grep` / `rg` pattern.
    Pattern,
    /// `awk -F <sep>` / `cut -d <sep>`; `attached` holds the option when the value is glued to it.
    Delimiter { attached: Option<&'static str>, tool: Tool },
    /// The `awk` program, splitting fields by `sep` (`None` = whitespace).
    AwkProgram { sep: Option<char> },
    /// `cut -f <list>`, splitting fields by `sep`.
    CutFields { attached: bool, sep: char },
}

/// Completes the word under the cursor from the sampled input of a data-consuming pipeline stage;
/// `None` when the stage, slot or input is not something ezlo can inspect without side effects.
pub(super) fn complete_piped(req: &CompletionRequest<'_>, parsed: &ParsedLine) -> Option<EngineCompletion> {
    let cmd_idx = command_after_redirects(parsed);
    let cmd = parsed.words.get(cmd_idx)?;
    if parsed.cursor_word_idx.is_some_and(|c| c <= cmd_idx) {
        return None;
    }
    let tool = Tool::from_name(cmd.unquoted.rsplit('/').next().unwrap_or_default())?;
    let (words, k) = words_with_cursor(parsed, cmd_idx);
    let s = scan(&words, &tool.options());
    let slot = data_slot(tool, &words, k, &s)?;
    let sample = input_sample(req, parsed, tool, &words, k, &s)?;
    let prefix = parsed.typed_prefix.as_str();
    let (filter_prefix, candidates) = match &slot {
        DataSlot::JqFilter => jq_candidates(&sample, prefix)?,
        DataSlot::Pattern => (prefix.to_owned(), pattern_candidates(&sample)),
        DataSlot::Delimiter { attached, tool } => (prefix.to_owned(), delimiter_candidates(&sample, *attached, *tool)),
        DataSlot::AwkProgram { sep } => (prefix.to_owned(), awk_program_candidates(&sample, *sep)),
        DataSlot::CutFields { attached, sep } => (prefix.to_owned(), cut_field_candidates(&sample, *attached, *sep)),
    };
    (!candidates.is_empty()).then_some(EngineCompletion {
        replace_span: parsed.replace_span,
        prefix: filter_prefix,
        candidates,
    })
}

/// Index of the command word, past leading redirections (`< f.json jq …`).
fn command_after_redirects(parsed: &ParsedLine) -> usize {
    let mut idx = parsed.cmd_word_idx;
    while let Some(w) = parsed.words.get(idx) {
        if w.quote.is_some() || !(w.raw.starts_with('<') || w.raw.starts_with('>')) {
            break;
        }
        let bare = w.raw.trim_start_matches(['<', '>', '&', '|']).is_empty();
        idx += if bare { 2 } else { 1 };
    }
    idx
}

/// Words around the command (leading redirections, then the words after it), with an empty
/// virtual word inserted where the cursor sits on whitespace, and the index of the cursor word
/// among them.
fn words_with_cursor(parsed: &ParsedLine, cmd_idx: usize) -> (Vec<ParsedWord>, usize) {
    let leading = &parsed.words[parsed.cmd_word_idx..cmd_idx];
    let mut words: Vec<ParsedWord> = leading.iter().chain(&parsed.words[cmd_idx + 1..]).cloned().collect();
    match parsed.cursor_word_idx {
        Some(c) => (words, c - cmd_idx - 1 + leading.len()),
        None => {
            let at = parsed.replace_span.0;
            let k = words.iter().position(|w| w.char_start > at).unwrap_or(words.len()).max(leading.len());
            words.insert(
                k,
                ParsedWord {
                    raw: String::new(),
                    unquoted: String::new(),
                    char_start: at,
                    char_end: at,
                    quote: None,
                    unterminated: false,
                },
            );
            (words, k)
        }
    }
}

/// Value of the first occurrence of option `opt` (separate `-F x` or attached `-Fx`) before `k`.
fn option_value<'w>(words: &'w [ParsedWord], k: usize, s: &Scan, opt: &str) -> Option<&'w str> {
    (0..k).find_map(|i| {
        if s.values.get(&i).is_some_and(|o| o == opt) {
            return Some(words[i].unquoted.as_str());
        }
        let w = words[i].unquoted.as_str();
        w.strip_prefix(opt).filter(|v| !v.is_empty() && !s.values.contains_key(&i) && s.operands.binary_search(&i).is_err())
    })
}

/// Field separator character of an `awk -F` / `cut -d` value (`'\t'` / `$'\t'` → tab).
fn sep_char(value: &str) -> Option<char> {
    match value {
        "\\t" | "\t" => Some('\t'),
        v if v.chars().count() == 1 => v.chars().next(),
        _ => None,
    }
}

fn data_slot(tool: Tool, words: &[ParsedWord], k: usize, s: &Scan) -> Option<DataSlot> {
    let word = words[k].unquoted.as_str();
    let value_of = s.values.get(&k).map(String::as_str);
    let operand_pos = s.operands.iter().position(|&i| i == k);
    match tool {
        Tool::Jq => {
            let from_file = s.has_option(&["-f", "--from-file"]) || s.options.iter().any(|o| o.starts_with("-f"));
            (operand_pos == Some(0) && !from_file).then_some(DataSlot::JqFilter)
        }
        Tool::Grep | Tool::Rg => {
            if matches!(value_of, Some("-e" | "--regexp")) {
                return Some(DataSlot::Pattern);
            }
            let explicit = s.has_option(&["-e", "--regexp", "-f", "--file"]);
            (operand_pos == Some(0) && !explicit).then_some(DataSlot::Pattern)
        }
        Tool::Awk => {
            if value_of == Some("-F") {
                return Some(DataSlot::Delimiter { attached: None, tool });
            }
            if word.starts_with("-F") && !s.values.contains_key(&k) && s.operands.binary_search(&k).is_err() {
                return Some(DataSlot::Delimiter { attached: Some("-F"), tool });
            }
            let from_file = s.options.iter().any(|o| o.starts_with("-f"));
            if operand_pos == Some(0) && !from_file {
                let sep = option_value(words, k, s, "-F").and_then(sep_char);
                return Some(DataSlot::AwkProgram { sep });
            }
            None
        }
        Tool::Cut => {
            let sep = option_value(words, k, s, "-d").and_then(sep_char).unwrap_or('\t');
            match value_of {
                Some("-d") => return Some(DataSlot::Delimiter { attached: None, tool }),
                Some("-f") => return Some(DataSlot::CutFields { attached: false, sep }),
                _ => {}
            }
            if s.values.contains_key(&k) || s.operands.binary_search(&k).is_ok() {
                return None;
            }
            if word.starts_with("-d") {
                Some(DataSlot::Delimiter { attached: Some("-d"), tool })
            } else if word.starts_with("-f") {
                Some(DataSlot::CutFields { attached: true, sep })
            } else {
                None
            }
        }
    }
}

/// The stage's input: its own input redirection, else its file operands (which take precedence
/// over stdin), else the upstream pipeline stage.
fn input_sample(
    req: &CompletionRequest<'_>,
    parsed: &ParsedLine,
    tool: Tool,
    words: &[ParsedWord],
    k: usize,
    s: &Scan,
) -> Option<Arc<Sample>> {
    if let Some(i) = s.input {
        return source::first_sample(req, [(args::redirect_path(&words[i]), &words[i])]);
    }
    // The first operand is the program / filter / pattern unless supplied by an option.
    let skip_first = match tool {
        Tool::Jq => !(s.has_option(&["--from-file"]) || s.options.iter().any(|o| o.starts_with("-f"))),
        Tool::Grep | Tool::Rg => !s.has_option(&["-e", "--regexp", "-f", "--file"]),
        Tool::Awk => !s.options.iter().any(|o| o.starts_with("-f")),
        Tool::Cut => false,
    };
    let files: Vec<usize> = s
        .operands
        .iter()
        .copied()
        .skip(usize::from(skip_first))
        .filter(|&i| i != k)
        .collect();
    if !files.is_empty() {
        // Files named but unreadable (globs, missing): stdin is not the input, don't guess.
        return source::first_sample(req, files.iter().map(|&i| (words[i].unquoted.as_str(), &words[i])));
    }
    source::upstream_sample(req, parsed.segment_start)
}

fn arg_candidate(label: &str, insert: &str, desc: String, tier: u8, suffix: &str) -> Candidate {
    Candidate {
        label: label.to_owned(),
        description: desc,
        insert_value: insert.to_owned(),
        insert_text: shell_quote(insert),
        suffix: suffix.to_owned(),
        kind: CandidateKind::Arg,
        priority_tier: tier,
        ..Default::default()
    }
}

/// Characters ending the `jq` sub-expression before the path being typed.
fn is_jq_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '|' | ',' | '(' | ';' | '{' | ':' | '=' | '<' | '>' | '!' | '+' | '*')
}

/// `jq` paths for the sub-expression under the cursor: relative to the path piped into it inside
/// the filter (`.users[] | .na` → `.name`), else absolute. The filter prefix is the sub-expression
/// alone; the inserted text keeps everything typed before it.
fn jq_candidates(sample: &Sample, typed: &str) -> Option<(String, Vec<Candidate>)> {
    let paths = sample.json_paths();
    if paths.is_empty() {
        return None;
    }
    let split = typed.rfind(is_jq_boundary).map_or(0, |i| i + typed[i..].chars().next().map_or(1, char::len_utf8));
    let (head, tail) = typed.split_at(split);
    if !(tail.is_empty() || tail.starts_with('.')) {
        return None;
    }
    let context = jq_context(head).filter(|ctx| paths.iter().any(|p| p.path == *ctx));
    let rows: Vec<(String, &str)> = match context {
        Some(ctx) => {
            let mut seen = std::collections::HashSet::new();
            paths
                .iter()
                .filter_map(|p| {
                    let rest = p.path.strip_prefix(ctx.as_str())?;
                    let rel = if rest.starts_with('.') {
                        rest.to_owned()
                    } else if rest.starts_with('[') {
                        format!(".{rest}")
                    } else {
                        return None;
                    };
                    seen.insert(rel.clone()).then_some((rel, p.desc.as_str()))
                })
                .collect()
        }
        None => paths.iter().map(|p| (p.path.clone(), p.desc.as_str())).collect(),
    };
    let name = sample.name();
    let cands = rows
        .into_iter()
        .map(|(path, desc)| arg_candidate(&path, &format!("{head}{path}"), format!("{desc} · {name}"), 0, ""))
        .collect();
    Some((tail.to_owned(), cands))
}

/// The path piped into the sub-expression being typed: in `.a[] | .b`, `.a[]`; through `map(`
/// / `select(` calls (`.a | map(.b` → `.a[]`).
fn jq_context(head: &str) -> Option<String> {
    let (left, right) = head.rsplit_once('|')?;
    if !right.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '(' || c.is_whitespace()) {
        return None;
    }
    let ctx = left.split_whitespace().last()?;
    let ctx = ctx.rsplit(['(', ',']).next()?;
    if !ctx.starts_with('.') {
        return None;
    }
    Some(if right.contains("map(") { format!("{ctx}[]") } else { ctx.to_owned() })
}

/// Header names (delimited samples) then frequent tokens.
fn pattern_candidates(sample: &Sample) -> Vec<Candidate> {
    let lines = columns::sample_lines(&sample.text);
    let name = sample.name();
    let mut out: Vec<Candidate> = Vec::new();
    if let Some(&(delim, _)) = columns::detect_delimiters(&lines).first() {
        for (i, h) in columns::headers(&lines, delim).iter().enumerate() {
            out.push(arg_candidate(h, h, format!("column {} header · {name}", i + 1), 0, " "));
        }
    }
    for (tok, n) in columns::frequent_tokens(&lines) {
        if out.iter().any(|c| c.insert_value == tok) {
            continue;
        }
        let mut c = arg_candidate(&tok, &tok, format!("{n}× in {name}"), 1, " ");
        c.score = n as f64 / 1000.0;
        out.push(c);
    }
    out
}

/// Detected delimiters with their column layout.
fn delimiter_candidates(sample: &Sample, attached: Option<&'static str>, tool: Tool) -> Vec<Candidate> {
    let lines = columns::sample_lines(&sample.text);
    columns::detect_delimiters(&lines)
        .into_iter()
        .map(|(d, cols)| {
            let heads = columns::headers(&lines, d);
            let desc = if heads.is_empty() {
                format!("{}: {cols} columns", columns::delimiter_name(d))
            } else {
                format!("{}: {cols} columns ({})", columns::delimiter_name(d), heads.join(", "))
            };
            let (shown, inserted) = match (d, tool) {
                ('\t', Tool::Cut) => ("\\t".to_owned(), "$'\\t'".to_owned()),
                ('\t', _) => ("\\t".to_owned(), "'\\t'".to_owned()),
                (d, _) => (d.to_string(), shell_quote(&d.to_string())),
            };
            let opt = attached.unwrap_or_default();
            Candidate {
                label: format!("{opt}{shown}"),
                description: desc,
                insert_value: format!("{opt}{shown}"),
                insert_text: format!("{opt}{inserted}"),
                suffix: " ".to_owned(),
                kind: CandidateKind::Arg,
                ..Default::default()
            }
        })
        .collect()
}

/// `{print $N}` per sampled column (described by header or example value), then `$NF`.
fn awk_program_candidates(sample: &Sample, sep: Option<char>) -> Vec<Candidate> {
    let lines = columns::sample_lines(&sample.text);
    let descs = columns::column_descriptions(&lines, sep);
    let mut out: Vec<Candidate> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let prog = format!("{{print ${}}}", i + 1);
            arg_candidate(&prog, &prog, format!("column {}: {d}", i + 1), 0, " ")
        })
        .collect();
    if descs.len() > 1 {
        let last = descs.last().map(String::as_str).unwrap_or_default();
        out.push(arg_candidate("{print $NF}", "{print $NF}", format!("last column: {last}"), 0, " "));
    }
    out
}

/// Column numbers for `cut -f`, described by header or example value.
fn cut_field_candidates(sample: &Sample, attached: bool, sep: char) -> Vec<Candidate> {
    let lines = columns::sample_lines(&sample.text);
    columns::column_descriptions(&lines, Some(sep))
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let value = if attached { format!("-f{}", i + 1) } else { (i + 1).to_string() };
            arg_candidate(&value, &value, format!("column {}: {d}", i + 1), 0, " ")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jq_context_through_pipes_and_map() {
        assert_eq!(jq_context(".users[] | ").as_deref(), Some(".users[]"));
        assert_eq!(jq_context(".users | map(").as_deref(), Some(".users[]"));
        assert_eq!(jq_context(".a | select(").as_deref(), Some(".a"));
        assert_eq!(jq_context("keys | ").as_deref(), None);
        assert_eq!(jq_context(".a "), None);
    }

    #[test]
    fn sep_chars() {
        assert_eq!(sep_char("\\t"), Some('\t'));
        assert_eq!(sep_char(","), Some(','));
        assert_eq!(sep_char(",,"), None);
    }
}
