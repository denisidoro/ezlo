//! History search (`ezlo history search`, zsh `Ctrl-R`): merges ezlo's execution log with the
//! shell history file and filters it by space-separated terms.

use std::collections::HashMap;
use std::path::Path;

use super::HistEntry;
use crate::store::HistoryEntry;

/// Which runs a search covers; `Ctrl-R` inside the picker cycles through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    All,
    /// Lines last run inside the current git repo.
    Repo,
    /// Lines last run in the current directory.
    Dir,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "history",
            Self::Repo => "history (repo)",
            Self::Dir => "history (dir)",
        }
    }

    /// All → Repo (only inside a repo) → Dir → All.
    pub fn next(self, in_repo: bool) -> Self {
        match self {
            Self::All if in_repo => Self::Repo,
            Self::All | Self::Repo => Self::Dir,
            Self::Dir => Self::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub line: String,
    /// ASCII-lowercased `line` (same byte offsets), so matching doesn't allocate per keystroke.
    lower: String,
    pub ts: i64,
    /// Directory of the most recent run ezlo recorded (`None`: only in the history file).
    pub cwd: Option<String>,
    /// Exit status of the most recent run ezlo recorded.
    pub exit: Option<i32>,
}

impl SearchEntry {
    pub fn new(line: String, ts: i64, cwd: Option<String>, exit: Option<i32>) -> Self {
        let lower = line.to_ascii_lowercase();
        Self { line, lower, ts, cwd, exit }
    }

    fn in_scope(&self, scope: SearchScope, cwd: &str, repo_root: Option<&str>) -> bool {
        match scope {
            SearchScope::All => true,
            SearchScope::Dir => self.cwd.as_deref() == Some(cwd),
            SearchScope::Repo => match (self.cwd.as_deref(), repo_root) {
                (Some(c), Some(root)) => Path::new(c).starts_with(root),
                _ => false,
            },
        }
    }
}

/// Unique lines, newest first. `store_rows` (newest first) give each line its directory and exit
/// status; history file lines ezlo never saw are kept too, and the newest timestamp wins.
pub fn merge_entries(store_rows: Vec<HistoryEntry>, file: Vec<HistEntry>, ignore_space: bool) -> Vec<SearchEntry> {
    let store_rows = store_rows.into_iter().map(|r| HistoryEntry { line: trim_line_end(r.line), ..r });
    let file = file.into_iter().map(|e| HistEntry { line: trim_line_end(e.line), ..e });
    let skip = |line: &str| line.trim().is_empty() || (ignore_space && line.starts_with(' '));
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<SearchEntry> = Vec::new();
    for r in store_rows {
        if skip(&r.line) || index.contains_key(&r.line) {
            continue;
        }
        index.insert(r.line.clone(), out.len());
        out.push(SearchEntry::new(r.line, r.ts, Some(r.cwd), Some(r.exit)));
    }
    for e in file {
        if skip(&e.line) {
            continue;
        }
        if let Some(&i) = index.get(&e.line) {
            if let Some(entry) = out.get_mut(i) {
                entry.ts = entry.ts.max(e.ts);
            }
            continue;
        }
        index.insert(e.line.clone(), out.len());
        out.push(SearchEntry::new(e.line, e.ts, None, None));
    }
    // Stable: equal timestamps keep ezlo's rows first, then the file's newest-first order.
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out
}

/// Drops trailing whitespace so `ls ` and `ls` are one row (kept after an escaping `\`).
fn trim_line_end(mut line: String) -> String {
    let trimmed = line.trim_end();
    if trimmed.len() < line.len() && !trimmed.ends_with('\\') {
        line.truncate(trimmed.len());
    }
    line
}

/// Space-separated query terms; all must appear (in any order). Smart case: a term with an
/// uppercase letter matches case-sensitively, otherwise case is ignored (ASCII).
#[derive(Debug, Clone, Default)]
pub struct SearchTerms {
    terms: Vec<(String, bool)>,
}

impl SearchTerms {
    pub fn parse(query: &str) -> Self {
        let terms = query
            .split_whitespace()
            .map(|t| {
                let exact = t.chars().any(|c| c.is_ascii_uppercase());
                (if exact { t.to_owned() } else { t.to_ascii_lowercase() }, exact)
            })
            .collect();
        Self { terms }
    }

    pub fn matches(&self, entry: &SearchEntry) -> bool {
        self.terms
            .iter()
            .all(|(t, exact)| if *exact { entry.line.contains(t.as_str()) } else { entry.lower.contains(t.as_str()) })
    }

    /// Sorted, merged byte ranges of every term occurrence in `text`, for highlighting.
    pub fn ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let lower = text.to_ascii_lowercase();
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (t, exact) in self.terms.iter().filter(|(t, _)| !t.is_empty()) {
            let hay = if *exact { text } else { lower.as_str() };
            out.extend(hay.match_indices(t.as_str()).map(|(i, m)| (i, i + m.len())));
        }
        out.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(out.len());
        for (s, e) in out {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        merged
    }
}

/// Indices of the entries in `scope` matching `terms`, keeping the newest-first order.
pub fn filter_entries(
    entries: &[SearchEntry],
    terms: &SearchTerms,
    scope: SearchScope,
    cwd: &str,
    repo_root: Option<&str>,
) -> Vec<u32> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.in_scope(scope, cwd, repo_root) && terms.matches(e))
        .filter_map(|(i, _)| u32::try_from(i).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: i64, cwd: &str, line: &str, exit: i32) -> HistoryEntry {
        HistoryEntry { ts, cwd: cwd.to_owned(), line: line.to_owned(), exit }
    }

    fn file(ts: i64, line: &str) -> HistEntry {
        HistEntry { ts, line: line.to_owned() }
    }

    #[test]
    fn merge_dedupes_keeps_newest_and_store_metadata() {
        let store = vec![row(300, "/repo/a", "git push", 1), row(200, "/tmp", "ls", 0), row(100, "/repo", "git push", 0)];
        let hist = vec![file(400, "ls  "), file(50, "make"), file(10, " secret"), file(5, "  ")];
        let merged = merge_entries(store, hist, true);
        let lines: Vec<&str> = merged.iter().map(|e| e.line.as_str()).collect();
        assert_eq!(lines, ["ls", "git push", "make"]);
        assert_eq!((merged[0].ts, merged[0].cwd.as_deref()), (400, Some("/tmp")));
        assert_eq!((merged[1].cwd.as_deref(), merged[1].exit), (Some("/repo/a"), Some(1)));
        assert_eq!((merged[2].cwd.as_deref(), merged[2].exit), (None, None));
        assert!(merge_entries(Vec::new(), vec![file(1, " secret")], false).iter().any(|e| e.line == " secret"));
        assert_eq!(trim_line_end("echo a\\ ".into()), "echo a\\ ");
    }

    #[test]
    fn terms_match_in_any_order_with_smart_case() {
        let e = SearchEntry::new("docker compose Up -d".into(), 0, None, None);
        assert!(SearchTerms::parse("up dock").matches(&e));
        assert!(SearchTerms::parse("Up").matches(&e));
        assert!(!SearchTerms::parse("UP").matches(&e));
        assert!(!SearchTerms::parse("dock logs").matches(&e));
        assert!(SearchTerms::parse("").matches(&e));
        assert_eq!(SearchTerms::parse("o co").ranges("docker compose"), [(1, 2), (7, 9), (11, 12)]);
    }

    #[test]
    fn scopes_filter_by_recorded_directory() {
        let entries = vec![
            SearchEntry::new("a".into(), 3, Some("/repo/sub".into()), Some(0)),
            SearchEntry::new("b".into(), 2, Some("/repo".into()), Some(0)),
            SearchEntry::new("c".into(), 1, None, None),
        ];
        let all = SearchTerms::default();
        let f = |scope| filter_entries(&entries, &all, scope, "/repo", Some("/repo"));
        assert_eq!(f(SearchScope::All), [0, 1, 2]);
        assert_eq!(f(SearchScope::Repo), [0, 1]);
        assert_eq!(f(SearchScope::Dir), [1]);
        assert_eq!(SearchScope::All.next(false), SearchScope::Dir);
        assert_eq!(SearchScope::All.next(true), SearchScope::Repo);
        assert_eq!(SearchScope::Dir.next(true), SearchScope::All);
    }
}
