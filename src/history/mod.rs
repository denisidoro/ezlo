//! zsh history files (`$HISTFILE` or zsh's defaults) as a completion source.

mod parse;
pub mod search;
mod zsh;

pub use parse::parse_history;
pub use zsh::unmetafy;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::types::format::format_age;

/// Upper bound of unique lines kept per history file.
pub const MAX_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistEntry {
    pub ts: i64,
    pub line: String,
}

/// Newest-first, unique lines (keeping each line's most recent timestamp).
pub fn dedupe_newest_first(entries: Vec<HistEntry>) -> Vec<HistEntry> {
    let keep: Vec<bool> = {
        let mut seen = HashSet::new();
        entries.iter().rev().map(|e| seen.insert(e.line.as_str())).collect()
    };
    entries
        .into_iter()
        .rev()
        .zip(keep)
        .filter_map(|(e, keep)| keep.then_some(e))
        .take(MAX_ENTRIES)
        .collect()
}

/// Resolves the history file: explicit `HISTFILE` from the shell, else the usual defaults.
pub fn resolve_histfile(histfile: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(h) = histfile.filter(|h| !h.is_empty()) {
        return Some(expand_home(h, home));
    }
    let home = home.filter(|h| !h.is_empty())?;
    zsh::DEFAULT_FILES
        .iter()
        .map(|name| Path::new(home).join(name))
        .find(|p| p.is_file())
}

fn expand_home(path: &str, home: Option<&str>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => Path::new(h).join(rest),
        _ => PathBuf::from(path),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    mtime: Option<SystemTime>,
    len: u64,
}

struct Cached {
    stamp: Stamp,
    entries: Arc<Vec<HistEntry>>,
}

/// Per-path cache, re-parsed only when the file's mtime/size changes.
#[derive(Default)]
pub struct HistFileCache {
    inner: Mutex<HashMap<PathBuf, Cached>>,
}

impl HistFileCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self, path: &Path) -> Arc<Vec<HistEntry>> {
        let Ok(meta) = fs::metadata(path) else {
            return Arc::new(Vec::new());
        };
        let stamp = Stamp {
            mtime: meta.modified().ok(),
            len: meta.len(),
        };
        let Ok(mut guard) = self.inner.lock() else {
            return Arc::new(Vec::new());
        };
        if let Some(c) = guard.get(path).filter(|c| c.stamp == stamp) {
            return Arc::clone(&c.entries);
        }
        let entries = fs::read(path)
            // Plain (non-EXTENDED_HISTORY) lines have no timestamp: keep 0 = unknown age.
            .map(|b| dedupe_newest_first(parse_history(&b, 0)))
            .unwrap_or_default();
        let entries = Arc::new(entries);
        guard.insert(
            path.to_path_buf(),
            Cached {
                stamp,
                entries: Arc::clone(&entries),
            },
        );
        entries
    }
}

/// A history line that matched the typed buffer, with its source metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistMatch {
    pub line: String,
    pub ts: i64,
    pub in_cwd: bool,
    /// Project kind shared with the current directory (`rust`), when it ran in another project.
    pub similar: Option<&'static str>,
    /// How the line's most recent run ended (ezlo's own store only).
    pub last_run: Option<LastRun>,
}

/// Exit status and duration of a line's most recent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LastRun {
    pub exit: i32,
    pub duration_ms: u64,
}

/// Runs shorter than this don't show a duration.
const SHOW_DURATION_MS: u64 = 1000;

/// Newest-first file entries that extend `buffer` (prefix match, strictly longer).
pub fn file_matches(entries: &[HistEntry], buffer: &str, limit: usize) -> Vec<HistMatch> {
    entries
        .iter()
        .filter(|e| e.line.len() > buffer.len() && e.line.starts_with(buffer))
        .take(limit)
        .map(|e| HistMatch {
            line: e.line.clone(),
            ts: e.ts,
            ..HistMatch::default()
        })
        .collect()
}

/// Builds dropdown rows from history matches (ezlo's own store first, then the shell file).
/// `start` is the char offset the client will replace from; history rows replace the rest of the
/// line, so the inserted text is the matched line minus the already-typed head.
pub fn history_candidates(
    matches: impl IntoIterator<Item = HistMatch>,
    start: usize,
    limit: usize,
    now_ts: i64,
) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for mut m in matches {
        if out.len() >= limit {
            break;
        }
        m.line.truncate(m.line.trim_end().len());
        if m.line.is_empty() || seen.contains(&m.line) {
            continue;
        }
        seen.insert(m.line.clone());
        out.push(Candidate {
            insert_text: m.line.chars().skip(start).collect(),
            insert_value: m.line.clone(),
            description: m.line.clone(),
            columns: match_columns(&m, now_ts),
            label: m.line,
            kind: CandidateKind::History,
            priority_tier: 0,
            ..Default::default()
        });
    }
    out
}

/// `[where, last run, age]`: `here` (cwd/repo) or the shared project kind (`rust`), then how
/// the last run went (`✗ 2` when it failed, its duration when it took a while), then the age.
fn match_columns(m: &HistMatch, now_ts: i64) -> Vec<String> {
    let age = if m.ts > 0 && now_ts >= m.ts {
        format_age(Duration::from_secs(now_ts.abs_diff(m.ts)))
    } else {
        String::new()
    };
    let place = if m.in_cwd { "here" } else { m.similar.unwrap_or("") };
    let run = match m.last_run {
        Some(r) if r.exit != 0 => format!("✗ {}", r.exit),
        Some(r) if r.duration_ms >= SHOW_DURATION_MS => format_duration(r.duration_ms),
        _ => String::new(),
    };
    vec![place.to_owned(), run, age]
}

/// `1.5s`, `42s`, `3m05s`, `2h10m`.
pub fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=9 => format!("{}.{}s", s, (ms % 1000) / 100),
        10..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Splices history rows in so they land inside the first visible window
/// (right after the best `max_rows - n` spec candidates).
pub fn merge_history(cands: &mut Vec<Candidate>, hist: Vec<Candidate>, max_rows: usize) {
    if hist.is_empty() {
        return;
    }
    let split = max_rows.saturating_sub(hist.len()).min(cands.len());
    cands.splice(split..split, hist);
}
