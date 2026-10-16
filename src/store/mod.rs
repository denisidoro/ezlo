//! SQLite-backed usage and execution history, mirrored in memory for fast ranking lookups.

mod executions;
mod memory;
mod project;

pub use executions::read_executions;
pub use memory::{resolve_repo_root, HistoryEntry};

use crate::engine::rank::{UsageLookup, UsageRecord};
use crate::history::{parse_history, HistMatch, LastRun};
use anyhow::Result;
use memory::{in_same_repo, MemoryState, MAX_HISTORY};
use rusqlite::{params, Connection};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    cwd TEXT NOT NULL,
    line TEXT NOT NULL,
    exit INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_executions_ts ON executions(ts DESC);
CREATE TABLE IF NOT EXISTS usage (
    ctx TEXT NOT NULL,
    label TEXT NOT NULL,
    kind TEXT NOT NULL,
    accepts INTEGER NOT NULL DEFAULT 0,
    executions INTEGER NOT NULL DEFAULT 0,
    last_ts INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (ctx, label)
);
CREATE TABLE IF NOT EXISTS usage_cwd (
    ctx TEXT NOT NULL,
    label TEXT NOT NULL,
    cwd TEXT NOT NULL,
    PRIMARY KEY (ctx, label, cwd)
);
DROP TABLE IF EXISTS corrections;";
/// Added after the first release: databases created before have no `failures` column.
const MIGRATE_FAILURES: &str = "ALTER TABLE usage ADD COLUMN failures INTEGER NOT NULL DEFAULT 0";
/// Added with last-run columns: how long each execution took.
const MIGRATE_DURATION: &str = "ALTER TABLE executions ADD COLUMN duration_ms INTEGER NOT NULL DEFAULT 0";

const UPSERT_ACCEPT: &str = "INSERT INTO usage (ctx, label, kind, accepts, executions, last_ts)
    VALUES (?1, ?2, ?3, 1, 0, ?4)
    ON CONFLICT(ctx, label) DO UPDATE SET accepts = accepts + 1, last_ts = excluded.last_ts";
const UPSERT_EXEC: &str = "INSERT INTO usage (ctx, label, kind, accepts, executions, last_ts)
    VALUES (?1, ?2, 'exec', 0, 1, ?3)
    ON CONFLICT(ctx, label) DO UPDATE SET executions = executions + 1, last_ts = excluded.last_ts";
/// Failures don't touch `last_ts`/cwds: they shouldn't make a token look fresh or local.
const UPSERT_FAIL: &str = "INSERT INTO usage (ctx, label, kind, accepts, executions, last_ts, failures)
    VALUES (?1, ?2, 'exec', 0, 0, ?3, 1)
    ON CONFLICT(ctx, label) DO UPDATE SET failures = failures + 1";
const INSERT_CWD: &str = "INSERT OR IGNORE INTO usage_cwd (ctx, label, cwd) VALUES (?1, ?2, ?3)";
const INSERT_EXECUTION: &str = "INSERT INTO executions (ts, cwd, line, exit, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5)";

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    mem: Arc<RwLock<MemoryState>>,
    projects: Arc<project::ProjectKinds>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let store = Self::with_connection(Connection::open(path)?)?;
        store.load_into_memory()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        let has_failures = conn
            .prepare("SELECT 1 FROM pragma_table_info('usage') WHERE name = 'failures'")?
            .exists([])?;
        if !has_failures {
            conn.execute_batch(MIGRATE_FAILURES)?;
        }
        let has_duration = conn
            .prepare("SELECT 1 FROM pragma_table_info('executions') WHERE name = 'duration_ms'")?
            .exists([])?;
        if !has_duration {
            conn.execute_batch(MIGRATE_DURATION)?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            mem: Arc::new(RwLock::new(MemoryState::default())),
            projects: Arc::new(project::ProjectKinds::default()),
        })
    }

    fn load_into_memory(&self) -> Result<()> {
        let (Ok(conn), Ok(mut mem)) = (self.conn.lock(), self.mem.write()) else {
            return Ok(());
        };

        let mut stmt = conn.prepare("SELECT ctx, label, accepts, executions, last_ts, failures FROM usage")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        for (ctx, label, accepts, executions, last_ts, failures) in rows.flatten() {
            let stats = mem.stats_mut(&ctx, &label);
            stats.accepts = accepts.unsigned_abs();
            stats.executions = executions.unsigned_abs();
            stats.last_ts = last_ts;
            stats.failures = failures.unsigned_abs();
        }


        let mut stmt = conn.prepare("SELECT ctx, label, cwd FROM usage_cwd")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for (ctx, label, cwd) in rows.flatten() {
            mem.stats_mut(&ctx, &label).cwds.insert(cwd);
        }

        let mut stmt = conn.prepare(&format!(
            "SELECT ts, cwd, line, exit FROM (
                 SELECT id, ts, cwd, line, exit FROM executions
                 WHERE exit = 0 ORDER BY id DESC LIMIT {MAX_HISTORY}
             ) ORDER BY id ASC"
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok(HistoryEntry {
                ts: r.get(0)?,
                cwd: r.get(1)?,
                line: r.get(2)?,
                exit: r.get(3)?,
            })
        })?;
        for entry in rows.flatten() {
            mem.push_history(entry);
        }

        // Oldest first, so each line ends up with its newest run.
        let mut stmt = conn.prepare(&format!(
            "SELECT line, exit, duration_ms FROM (
                 SELECT id, line, exit, duration_ms FROM executions ORDER BY id DESC LIMIT {MAX_HISTORY}
             ) ORDER BY id ASC"
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?, r.get::<_, i64>(2)?))
        })?;
        for (line, exit, duration) in rows.flatten() {
            mem.record_last_run(&line, LastRun { exit, duration_ms: duration.unsigned_abs() });
        }
        Ok(())
    }

    pub fn record_accept(&self, ctx: &str, label: &str, kind: &str, cwd: &str, ts: i64) {
        let repo_root = resolve_repo_root(Path::new(cwd));
        if let Ok(mut mem) = self.mem.write() {
            let stats = mem.stats_mut(ctx, label);
            stats.accepts += 1;
            stats.last_ts = ts;
            mem.record_cwd(ctx, label, cwd, repo_root.as_deref());
        }
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(UPSERT_ACCEPT, params![ctx, label, kind, ts]);
            let _ = conn.execute(INSERT_CWD, params![ctx, label, cwd]);
            if let Some(root) = &repo_root {
                let _ = conn.execute(INSERT_CWD, params![ctx, label, root]);
            }
        }
    }

    /// Records a finished command (see `record_execution`) that started when it finished.
    pub fn record_execution_complete(
        &self,
        cwd: &str,
        line: &str,
        exit_code: i32,
        ts: i64,
        hist_ignore_space: bool,
    ) {
        self.record_execution(cwd, line, exit_code, ts, 0, hist_ignore_space);
    }

    /// Records a command that finished at `ts` after `duration_ms`: successful lines feed history/ghost
    /// text, and every word of the first pipeline segment bumps its usage under the preceding words
    /// (`git` → `git checkout`), as an execution or, when the line failed, as a failure.
    pub fn record_execution(
        &self,
        cwd: &str,
        line: &str,
        exit_code: i32,
        ts: i64,
        duration_ms: u64,
        hist_ignore_space: bool,
    ) {
        if line.trim().is_empty() {
            return;
        }
        let keep_line = !(hist_ignore_space && line.starts_with(' '));
        let failed = is_failure(exit_code);
        let words = command_word_contexts(line);
        let repo_root = resolve_repo_root(Path::new(cwd));

        if let Ok(mut mem) = self.mem.write() {
            if keep_line {
                mem.record_last_run(line, LastRun { exit: exit_code, duration_ms });
            }
            if exit_code == 0 && keep_line {
                mem.push_history(HistoryEntry {
                    ts,
                    cwd: cwd.to_owned(),
                    line: line.to_owned(),
                    exit: exit_code,
                });
            }
            if failed {
                for (ctx, word) in &words {
                    mem.stats_mut(ctx, word).failures += 1;
                }
            } else {
                for (ctx, word) in &words {
                    let stats = mem.stats_mut(ctx, word);
                    stats.executions += 1;
                    stats.last_ts = ts;
                    mem.record_cwd(ctx, word, cwd, repo_root.as_deref());
                }
            }
        }

        let Ok(conn) = self.conn.lock() else {
            return;
        };
        if keep_line {
            let duration = i64::try_from(duration_ms).unwrap_or(i64::MAX);
            let _ = conn.execute(INSERT_EXECUTION, params![ts, cwd, line, exit_code, duration]);
        }
        for (ctx, word) in &words {
            if failed {
                let _ = conn.execute(UPSERT_FAIL, params![ctx, word, ts]);
                continue;
            }
            let _ = conn.execute(UPSERT_EXEC, params![ctx, word, ts]);
            let _ = conn.execute(INSERT_CWD, params![ctx, word, cwd]);
            if let Some(root) = &repo_root {
                let _ = conn.execute(INSERT_CWD, params![ctx, word, root]);
            }
        }
    }

    /// Empty-prompt recall: the successful lines most often and most recently run in `cwd` (then
    /// its git repo, then projects of the same kind, then anywhere), best first.
    pub fn workflow_matches(&self, cwd: &str, hist_ignore_space: bool, now_ts: i64, limit: usize) -> Vec<HistMatch> {
        let repo_root = resolve_repo_root(Path::new(cwd));
        let similar = self.similar_fn(cwd);
        self.mem
            .read()
            .map(|mem| mem.workflow_matches(cwd, repo_root.as_deref(), hist_ignore_space, now_ts, limit, &similar))
            .unwrap_or_default()
    }

    /// `dir -> shared project kind with cwd` (`None` when cwd has no known kind).
    fn similar_fn(&self, cwd: &str) -> impl Fn(&str) -> Option<&'static str> + '_ {
        let here = self.projects.of(cwd);
        move |dir: &str| if here == Default::default() { None } else { here.shared_name(self.projects.of(dir)) }
    }

    /// How `line` last ended, if ezlo saw it run.
    pub fn last_run(&self, line: &str) -> Option<LastRun> {
        self.mem.read().ok().and_then(|m| m.last_run(line))
    }

    /// Successful executed lines extending `prefix`, newest first: current `cwd`, its repo, projects
    /// of the same kind (`rust`, `node`, …), then anywhere.
    pub fn history_matches(
        &self,
        prefix: &str,
        cwd: &str,
        hist_ignore_space: bool,
        limit: usize,
    ) -> Vec<HistMatch> {
        let Ok(mem) = self.mem.read() else {
            return Vec::new();
        };
        let repo_root = resolve_repo_root(Path::new(cwd));
        let root = repo_root.as_deref();
        let eligible = |e: &&HistoryEntry| e.extends(prefix, hist_ignore_space);
        let here = mem.history.iter().rev().filter(eligible).filter(|e| e.cwd == cwd);
        let in_repo = mem
            .history
            .iter()
            .rev()
            .filter(eligible)
            .filter(|e| e.cwd != cwd && in_same_repo(&e.cwd, root));
        let similar = self.similar_fn(cwd);
        let outside = |e: &&HistoryEntry| e.cwd != cwd && !in_same_repo(&e.cwd, root);
        let like_here = mem.history.iter().rev().filter(eligible).filter(outside).filter(|e| similar(&e.cwd).is_some());
        let elsewhere = mem.history.iter().rev().filter(eligible).filter(outside).filter(|e| similar(&e.cwd).is_none());
        let mut seen = std::collections::HashSet::new();
        here.chain(in_repo)
            .chain(like_here)
            .chain(elsewhere)
            .filter(|e| seen.insert(e.line.as_str()))
            .take(limit)
            .map(|e| {
                let in_cwd = e.cwd == cwd || in_same_repo(&e.cwd, root);
                HistMatch {
                    line: e.line.clone(),
                    ts: e.ts,
                    in_cwd,
                    similar: if in_cwd { None } else { similar(&e.cwd) },
                    last_run: mem.last_run(&e.line),
                }
            })
            .collect()
    }

    /// History ghost suffix: the most recent successful line extending `line` in `cwd` (or same
    /// git repo), else globally; only when the cursor is at the end of a single-line buffer.
    pub fn suggest_ghost(
        &self,
        line: &str,
        cursor_chars: usize,
        cwd: &str,
        hist_ignore_space: bool,
    ) -> String {
        if line.is_empty()
            || line.contains('\n')
            || cursor_chars != line.chars().count()
            || (hist_ignore_space && line.starts_with(' '))
        {
            return String::new();
        }
        let Ok(mem) = self.mem.read() else {
            return String::new();
        };
        let repo_root = resolve_repo_root(Path::new(cwd));
        let root = repo_root.as_deref();
        let mut eligible = mem.history.iter().rev().filter(|e| e.extends(line, hist_ignore_space));
        let in_cwd = eligible.clone().find(|e| e.cwd == cwd);
        let in_repo = eligible.clone().find(|e| in_same_repo(&e.cwd, root));
        in_cwd
            .or(in_repo)
            .or_else(|| eligible.next())
            .and_then(|e| e.line.get(line.len()..))
            .map(str::to_owned)
            .unwrap_or_default()
    }

    /// Imports a zsh history file (see `history::parse_history`).
    pub fn import_histfile(&self, histfile: &Path, default_cwd: &str) -> Result<usize> {
        let bytes = fs::read(histfile)?;
        let entries = parse_history(&bytes, now_unix_ts());

        if let Ok(mut conn) = self.conn.lock() {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(INSERT_EXECUTION)?;
                for e in &entries {
                    stmt.execute(params![e.ts, default_cwd, e.line, 0, 0])?;
                }
            }
            tx.commit()?;
        }

        let imported = entries.len();
        if let Ok(mut mem) = self.mem.write() {
            for e in entries {
                mem.push_history(HistoryEntry {
                    ts: e.ts,
                    cwd: default_cwd.to_owned(),
                    line: e.line,
                    exit: 0,
                });
            }
        }
        Ok(imported)
    }
}

impl UsageLookup for Store {
    fn lookup_usage(&self, ctx: &str, label: &str, cwd: &str) -> UsageRecord {
        self.mem
            .read()
            .map(|mem| mem.lookup(ctx, label, cwd))
            .unwrap_or_default()
    }

    fn learned_values(&self, ctx: &str) -> Vec<String> {
        self.mem
            .read()
            .map(|mem| mem.learned_values(ctx))
            .unwrap_or_default()
    }

}

/// Whether an exit status counts as a failure. `128 + signal` statuses of interruptions (Ctrl-C,
/// Ctrl-\, SIGKILL/SIGTERM, SIGPIPE from `| head`, Ctrl-Z) aren't mistakes; other statuses above
/// 128 are (e.g. git exits 128 on fatal errors and 129 on usage errors).
fn is_failure(exit_code: i32) -> bool {
    // INT, QUIT, KILL, PIPE, TERM, TSTP (macOS 18 / Linux 20).
    const INTERRUPTIONS: [i32; 7] = [130, 131, 137, 141, 143, 146, 148];
    exit_code != 0 && !INTERRUPTIONS.contains(&exit_code)
}

/// `git checkout -b x | less` → `[("", "git"), ("git", "checkout"), ("git checkout", "-b"), ...]`,
/// stopping at the first pipeline/list operator.
fn command_word_contexts(line: &str) -> Vec<(String, &str)> {
    let mut ctx = String::new();
    let mut out = Vec::new();
    for word in line.split_whitespace() {
        if matches!(word, "|" | "||" | "&&" | ";") {
            break;
        }
        if let Some((flag, val)) = word
            .split_once('=')
            .filter(|(f, v)| !ctx.is_empty() && f.starts_with('-') && !v.is_empty())
        {
            out.push((ctx.clone(), flag));
            out.push((format!("{ctx} {flag}"), val));
        } else {
            out.push((ctx.clone(), word));
        }
        if !ctx.is_empty() {
            ctx.push(' ');
        }
        ctx.push_str(word);
    }
    out
}

pub fn now_unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
