//! Column and token sampling of a delimited / plain-text sample: delimiter detection, per-column
//! header or example values, and frequent word tokens for search patterns.

use std::collections::HashMap;

/// Lines inspected for delimiters, columns and tokens.
const MAX_LINES: usize = 200;
/// Delimiter candidates, in tie-break order.
const DELIMITERS: [char; 5] = [',', '\t', ';', '|', ':'];
/// Columns offered at most (`$1`..`$N`, `-f 1`..`-f N`).
pub const MAX_COLUMNS: usize = 20;
const MAX_TOKENS: usize = 40;
const MAX_FIELD_CHARS: usize = 24;

/// The first non-empty lines of a sample.
pub fn sample_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.trim().is_empty()).take(MAX_LINES).collect()
}

/// Delimiters splitting (nearly) every line into the same number (≥ 2) of fields, as
/// `(delimiter, columns)`, most columns first.
pub fn detect_delimiters(lines: &[&str]) -> Vec<(char, usize)> {
    let mut found: Vec<(char, usize)> = DELIMITERS
        .iter()
        .filter_map(|&d| {
            let counts: Vec<usize> = lines.iter().take(50).map(|l| split_fields(l, Some(d)).len()).collect();
            let first = *counts.first()?;
            let consistent = counts.iter().filter(|&&c| c == first).count();
            (first >= 2 && consistent * 5 >= counts.len() * 4).then_some((d, first))
        })
        .collect();
    found.sort_by_key(|&(_, cols)| std::cmp::Reverse(cols));
    found
}

/// Splits a line into fields: by `delim` (honoring double-quoted fields) or, with `None`, by runs
/// of whitespace like `awk`'s default.
pub fn split_fields(line: &str, delim: Option<char>) -> Vec<String> {
    let Some(d) = delim else {
        return line.split_whitespace().map(str::to_owned).collect();
    };
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' if d != '"' => quoted = !quoted,
            c if c == d && !quoted => out.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    out.push(field);
    out.into_iter().map(|f| f.trim().to_owned()).collect()
}

/// One description per column (up to `MAX_COLUMNS`): `header <name>` when the first line reads as
/// a header row, otherwise an example value.
pub fn column_descriptions(lines: &[&str], delim: Option<char>) -> Vec<String> {
    let Some(first) = lines.first() else {
        return Vec::new();
    };
    let head = split_fields(first, delim);
    let is_header = lines.len() > 1
        && delim.is_some()
        && head.iter().all(|f| !f.is_empty() && f.parse::<f64>().is_err());
    head.iter()
        .take(MAX_COLUMNS)
        .map(|f| {
            let short = shorten(f);
            if is_header {
                format!("header {short}")
            } else {
                format!("e.g. {short}")
            }
        })
        .collect()
}

/// Header names of a delimited sample (empty when the first row does not look like a header).
pub fn headers(lines: &[&str], delim: char) -> Vec<String> {
    let Some(first) = lines.first() else {
        return Vec::new();
    };
    let head = split_fields(first, Some(delim));
    if lines.len() < 2 || head.iter().any(|f| f.is_empty() || f.parse::<f64>().is_ok()) {
        return Vec::new();
    }
    head.into_iter().take(MAX_COLUMNS).collect()
}

/// Frequent word-like tokens (≥ 3 chars, not purely numeric) with their counts, most frequent
/// first (ties keep first appearance).
pub fn frequent_tokens(lines: &[&str]) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for line in lines {
        for tok in line.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))) {
            let tok = tok.trim_matches(['.', '-']);
            if tok.chars().count() < 3 || tok.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
                continue;
            }
            let order = counts.len();
            counts.entry(tok).or_insert((0, order)).0 += 1;
        }
    }
    let mut out: Vec<(&str, (usize, usize))> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.1 .1.cmp(&b.1 .1)));
    out.into_iter().take(MAX_TOKENS).map(|(t, (n, _))| (t.to_owned(), n)).collect()
}

/// Human name of a delimiter.
pub fn delimiter_name(d: char) -> &'static str {
    match d {
        ',' => "comma",
        '\t' => "tab",
        ';' => "semicolon",
        '|' => "pipe",
        ':' => "colon",
        _ => "delimiter",
    }
}

fn shorten(s: &str) -> String {
    let mut out: String = s.chars().take(MAX_FIELD_CHARS).collect();
    if s.chars().count() > MAX_FIELD_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "name,age,city\nalice,30,\"Paris, FR\"\nbob,25,Rome\n";

    #[test]
    fn detects_csv_and_headers() {
        let lines = sample_lines(CSV);
        assert_eq!(detect_delimiters(&lines), [(',', 3)]);
        assert_eq!(headers(&lines, ','), ["name", "age", "city"]);
        assert_eq!(split_fields(lines[1], Some(',')), ["alice", "30", "Paris, FR"]);
        assert_eq!(column_descriptions(&lines, Some(','))[2], "header city");
    }

    #[test]
    fn passwd_like_colon_files() {
        let lines = sample_lines("root:x:0:0:root:/root:/bin/bash\nbin:x:1:1:bin:/bin:/sbin/nologin\n");
        assert_eq!(detect_delimiters(&lines)[0], (':', 7));
        assert!(headers(&lines, ':').is_empty());
        assert_eq!(column_descriptions(&lines, Some(':'))[0], "e.g. root");
    }

    #[test]
    fn tokens_by_frequency() {
        let lines = sample_lines("ERROR disk full\nWARN disk slow\nERROR net 42\n");
        let toks = frequent_tokens(&lines);
        assert_eq!(toks[0], ("ERROR".to_owned(), 2));
        assert_eq!(toks[1], ("disk".to_owned(), 2));
        assert!(toks.iter().all(|(t, _)| t != "42"));
    }
}
