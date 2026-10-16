use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_GEN_BYTES: usize = 1024 * 1024;
pub const MAX_GEN_ROWS: usize = 10_000;

/// Cache entries kept at most; beyond it the oldest fetches are evicted. Keys include the cwd and
/// params (pane/preview keys even include the typed line), so the key space is unbounded.
const MAX_CACHE_ENTRIES: usize = 256;
/// How long an expired entry is still kept to be served stale while it refreshes.
const STALE_KEEP: Duration = Duration::from_secs(10 * 60);

pub trait GeneratorRunner: Send + Sync {
    fn run_shell(
        &self,
        command: &str,
        cwd: &Path,
        client_env: &HashMap<String, String>,
        params: &[(String, String)],
        timeout: Duration,
    ) -> Vec<String>;
}

#[derive(Debug, Default, Clone)]
pub struct ShellGeneratorRunner;

impl GeneratorRunner for ShellGeneratorRunner {
    fn run_shell(
        &self,
        command: &str,
        cwd: &Path,
        client_env: &HashMap<String, String>,
        params: &[(String, String)],
        timeout: Duration,
    ) -> Vec<String> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);

        if cwd.is_dir() {
            cmd.current_dir(cwd);
        }
        if !client_env.is_empty() {
            cmd.env_clear();
            cmd.envs(client_env);
        }
        for (k, v) in params {
            cmd.env(k, v);
        }

        let Ok(mut child) = cmd.spawn() else {
            return Vec::new();
        };
        let pid = child.id() as i32;
        let Some(stdout) = child.stdout.take() else {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
            let _ = child.wait();
            return Vec::new();
        };

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let limited = stdout.take(MAX_GEN_BYTES as u64);
            let reader = BufReader::new(limited);
            let mut lines = Vec::new();
            // Blank lines are kept: previews need them; row parsers skip them.
            for line in reader.lines() {
                let Ok(l) = line else { break };
                lines.push(l.trim_end_matches('\r').to_string());
                if lines.len() >= MAX_GEN_ROWS {
                    break;
                }
            }
            let _ = tx.send(lines);
        });

        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Vec::new();
                    }
                    return rx
                        .recv_timeout(Duration::from_millis(100))
                        .unwrap_or_default();
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
                        let _ = child.wait();
                        return Vec::new();
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
                    let _ = child.wait();
                    return Vec::new();
                }
            }
        }
    }
}

pub type RecordedCall = (String, Vec<(String, String)>);

#[derive(Debug, Default, Clone)]
pub struct FakeGeneratorRunner {
    responses: Arc<Mutex<HashMap<String, Vec<String>>>>,
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl FakeGeneratorRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_response(&self, command_substr: impl Into<String>, lines: Vec<String>) {
        if let Ok(mut map) = self.responses.lock() {
            map.insert(command_substr.into(), lines);
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().map(|c| c.len()).unwrap_or(0)
    }

    pub fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl GeneratorRunner for FakeGeneratorRunner {
    fn run_shell(
        &self,
        command: &str,
        _cwd: &Path,
        _client_env: &HashMap<String, String>,
        params: &[(String, String)],
        _timeout: Duration,
    ) -> Vec<String> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((command.to_string(), params.to_vec()));
        }
        let Ok(map) = self.responses.lock() else {
            return Vec::new();
        };
        if let Some(exact) = map.get(command) {
            return exact.clone();
        }
        for (pat, rows) in map.iter() {
            if command.contains(pat) {
                return rows.clone();
            }
        }
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub base_type: String,
    pub params: Vec<(String, String)>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    rows: Arc<Vec<Vec<String>>>,
    fetched_at: Instant,
    ttl: Duration,
}

#[derive(Debug, Clone)]
pub enum CacheLookup {
    Fresh(Arc<Vec<Vec<String>>>),
    Stale(Arc<Vec<Vec<String>>>),
    Miss,
}

#[derive(Debug, Default, Clone)]
pub struct GeneratorCache {
    entries: Arc<Mutex<HashMap<CacheKey, CacheEntry>>>,
    /// Lower bound applied to every entry's TTL (zero for the daemon). Lets short-lived
    /// sessions like `ezlo pick` reuse generator output across keystrokes, even for
    /// types with a short or missing `cache`.
    min_ttl: Duration,
}

impl GeneratorCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_ttl(min_ttl: Duration) -> Self {
        Self {
            min_ttl,
            ..Self::default()
        }
    }

    pub fn lookup(&self, key: &CacheKey) -> CacheLookup {
        let Ok(guard) = self.entries.lock() else {
            return CacheLookup::Miss;
        };
        let Some(entry) = guard.get(key) else {
            return CacheLookup::Miss;
        };
        if entry.ttl.is_zero() {
            return CacheLookup::Miss;
        }
        if entry.fetched_at.elapsed() < entry.ttl {
            CacheLookup::Fresh(Arc::clone(&entry.rows))
        } else {
            CacheLookup::Stale(Arc::clone(&entry.rows))
        }
    }

    pub fn insert(&self, key: CacheKey, rows: Vec<Vec<String>>, ttl: Duration) -> Arc<Vec<Vec<String>>> {
        let arc_rows = Arc::new(rows);
        let ttl = ttl.max(self.min_ttl);
        if !ttl.is_zero() {
            if let Ok(mut guard) = self.entries.lock() {
                prune(&mut guard, MAX_CACHE_ENTRIES - 1);
                guard.insert(
                    key,
                    CacheEntry {
                        rows: Arc::clone(&arc_rows),
                        fetched_at: Instant::now(),
                        ttl,
                    },
                );
            }
        }
        arc_rows
    }

    pub fn force_expire_all(&self) {
        if let Ok(mut guard) = self.entries.lock() {
            let past = Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .unwrap_or_else(Instant::now);
            for entry in guard.values_mut() {
                entry.fetched_at = past;
            }
        }
    }
}

/// Drops entries past their stale window, then the oldest fetches until at most `cap` remain.
fn prune(entries: &mut HashMap<CacheKey, CacheEntry>, cap: usize) {
    entries.retain(|_, e| e.fetched_at.elapsed() < e.ttl.saturating_add(STALE_KEEP));
    if entries.len() <= cap {
        return;
    }
    let mut ages: Vec<Instant> = entries.values().map(|e| e.fetched_at).collect();
    ages.sort_unstable();
    let Some(&cutoff) = ages.get(ages.len() - cap) else {
        entries.clear();
        return;
    };
    entries.retain(|_, e| e.fetched_at >= cutoff);
}

pub fn parse_tsv_rows(lines: &[String], num_cols: usize) -> Vec<Vec<String>> {
    let cols_count = num_cols.max(1);
    lines
        .iter()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts: Vec<String> = line.split('\t').take(cols_count).map(String::from).collect();
            while parts.len() < cols_count {
                parts.push(String::new());
            }
            parts
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_runner_keeps_blank_lines_and_tsv_rows_skip_them() {
        let dir = std::env::temp_dir();
        let lines = ShellGeneratorRunner.run_shell("printf 'a\\n\\nb\\r\\n'", &dir, &HashMap::new(), &[], Duration::from_secs(5));
        assert_eq!(lines, ["a", "", "b"]);
        assert_eq!(parse_tsv_rows(&lines, 1), [vec!["a".to_string()], vec!["b".to_string()]]);
    }
}
