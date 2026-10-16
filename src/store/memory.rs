//! In-memory mirror of the usage/history tables, so ranking lookups never touch SQLite.

use crate::engine::rank::{UsageRecord, HALF_LIFE_SECS};
use crate::history::{HistMatch, LastRun};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Successful executions kept in memory (ghost text and history rows).
pub(super) const MAX_HISTORY: usize = 5000;
/// Empty-prompt recall weight of a run elsewhere in the current git repo (`cwd` itself is 1).
const WORKFLOW_REPO_WEIGHT: f64 = 0.5;
/// Empty-prompt recall weight of a run outside the current directory/repo.
const WORKFLOW_ELSEWHERE_WEIGHT: f64 = 0.05;
/// Empty-prompt recall weight of a run in another project of the same kind (`rust`, `node`, …).
const WORKFLOW_SIMILAR_WEIGHT: f64 = 0.2;
/// Lines whose last run is remembered at most.
const MAX_LAST_RUNS: usize = 10_000;

/// Recall score of a line: (frecency, newest ts, ran here, shared project kind).
type WorkflowScore = (f64, i64, bool, Option<&'static str>);

/// Walks up `cwd` and its ancestors looking for a `.git` directory or file.
pub fn resolve_repo_root(cwd: &Path) -> Option<String> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(|dir| dir.to_string_lossy().into_owned())
}

pub(super) fn in_same_repo(entry_cwd: &str, repo_root: Option<&str>) -> bool {
    repo_root.is_some_and(|root| Path::new(entry_cwd).starts_with(Path::new(root)))
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub ts: i64,
    pub cwd: String,
    pub line: String,
    pub exit: i32,
}

impl HistoryEntry {
    /// Whether this entry can be offered for the typed `prefix` (strictly longer, exit 0).
    pub(super) fn extends(&self, prefix: &str, hist_ignore_space: bool) -> bool {
        self.exit == 0
            && self.line.len() > prefix.len()
            && self.line.starts_with(prefix)
            && !(hist_ignore_space && self.line.starts_with(' '))
    }
}

#[derive(Debug, Default)]
pub(super) struct UsageStats {
    pub accepts: u64,
    pub executions: u64,
    pub last_ts: i64,
    pub failures: u64,
    pub cwds: HashSet<String>,
}

#[derive(Debug, Default)]
pub(super) struct MemoryState {
    /// `ctx -> label -> stats`; nested so lookups borrow `&str` keys without allocating.
    usage: HashMap<String, HashMap<String, UsageStats>>,
    pub history: VecDeque<HistoryEntry>,
    /// Exit status and duration of each line's most recent run (failed ones too).
    last_runs: HashMap<String, LastRun>,
}

impl MemoryState {
    /// Writes are rare (one per accept/execution), so allocating owned keys here is fine.
    pub fn stats_mut(&mut self, ctx: &str, label: &str) -> &mut UsageStats {
        self.usage
            .entry(ctx.to_owned())
            .or_default()
            .entry(label.to_owned())
            .or_default()
    }

    pub fn record_cwd(&mut self, ctx: &str, label: &str, cwd: &str, repo_root: Option<&str>) {
        let stats = self.stats_mut(ctx, label);
        stats.cwds.insert(cwd.to_owned());
        if let Some(root) = repo_root {
            stats.cwds.insert(root.to_owned());
        }
    }

    pub fn lookup(&self, ctx: &str, label: &str, cwd: &str) -> UsageRecord {
        let Some(stats) = self.usage.get(ctx).and_then(|m| m.get(label)) else {
            return UsageRecord::default();
        };
        let used_in_cwd = stats.cwds.contains(cwd)
            || (!stats.cwds.is_empty()
                && resolve_repo_root(Path::new(cwd)).is_some_and(|root| {
                    stats.cwds.contains(&root)
                        || stats.cwds.iter().any(|c| in_same_repo(c, Some(&root)))
                }));
        UsageRecord {
            accepts: stats.accepts,
            executions: stats.executions,
            last_ts: stats.last_ts,
            used_in_cwd,
            failures: stats.failures,
        }
    }

    pub fn learned_values(&self, ctx: &str) -> Vec<String> {
        let Some(labels) = self.usage.get(ctx) else {
            return Vec::new();
        };
        // Tokens that only ever failed aren't worth offering.
        let mut items: Vec<(&String, &UsageStats)> =
            labels.iter().filter(|(_, s)| s.accepts + s.executions > 0).collect();
        items.sort_by(|(la, sa), (lb, sb)| {
            (3 * sb.accepts + sb.executions)
                .cmp(&(3 * sa.accepts + sa.executions))
                .then_with(|| sb.last_ts.cmp(&sa.last_ts))
                .then_with(|| la.cmp(lb))
        });
        items.into_iter().map(|(l, _)| l.clone()).collect()
    }

    /// Distinct successful lines by frecency (each run decays with `HALF_LIFE_SECS`), weighted by
    /// where they ran: `cwd` counts fully, the same git repo partly, anywhere else barely (so a
    /// fresh directory still gets global habits, after the local ones).
    pub fn workflow_matches(
        &self,
        cwd: &str,
        repo_root: Option<&str>,
        hist_ignore_space: bool,
        now_ts: i64,
        limit: usize,
        similar: &dyn Fn(&str) -> Option<&'static str>,
    ) -> Vec<HistMatch> {
        let mut scored: HashMap<&str, WorkflowScore> = HashMap::new();
        for e in &self.history {
            let line = e.line.trim_end();
            if e.exit != 0 || line.trim().is_empty() || (hist_ignore_space && line.starts_with(' ')) {
                continue;
            }
            let here = e.cwd == cwd || in_same_repo(&e.cwd, repo_root);
            let kind = if here { None } else { similar(&e.cwd) };
            let weight = if e.cwd == cwd {
                1.0
            } else if here {
                WORKFLOW_REPO_WEIGHT
            } else if kind.is_some() {
                WORKFLOW_SIMILAR_WEIGHT
            } else {
                WORKFLOW_ELSEWHERE_WEIGHT
            };
            let age_secs = (now_ts - e.ts).max(0) as f64;
            let entry = scored.entry(line).or_insert((0.0, e.ts, false, None));
            entry.0 += weight * 0.5_f64.powf(age_secs / HALF_LIFE_SECS);
            entry.1 = entry.1.max(e.ts);
            entry.2 |= here;
            entry.3 = entry.3.or(kind);
        }
        let mut items: Vec<(&str, WorkflowScore)> = scored.into_iter().collect();
        items.sort_by(|(la, a), (lb, b)| {
            b.0.total_cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| la.cmp(lb))
        });
        items
            .into_iter()
            .take(limit)
            .map(|(line, (_, ts, in_cwd, kind))| HistMatch {
                line: line.to_owned(),
                ts,
                in_cwd,
                similar: kind.filter(|_| !in_cwd),
                last_run: self.last_run(line),
            })
            .collect()
    }

    /// Remembers how `line` last ended (newest call wins).
    pub fn record_last_run(&mut self, line: &str, run: LastRun) {
        if self.last_runs.len() >= MAX_LAST_RUNS && !self.last_runs.contains_key(line) {
            self.last_runs.clear();
        }
        self.last_runs.insert(line.trim_end().to_owned(), run);
    }

    pub fn last_run(&self, line: &str) -> Option<LastRun> {
        self.last_runs.get(line.trim_end()).copied()
    }

    pub fn push_history(&mut self, entry: HistoryEntry) {
        self.history.push_back(entry);
        while self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }
    }
}
