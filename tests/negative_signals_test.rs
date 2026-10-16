//! Failure counts and empty-prompt workflow recall.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::daemon::protocol::{encode_client_msg, parse_client_line, ClientMsg};
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::UsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::{parse_and_validate, SpecIndex};
use ezlo::store::Store;

const SPEC: &str = r#"
cmd git
cmd git log "Show commit logs"
  opt --format <fmt> "Pretty format"
  opt --oneline "One line per commit"
cmd deploy
  arg <target>
"#;

fn query(store: &Store, cwd: &Path, line: &str, now_ts: i64) -> CompletionResult {
    let (spec, diags) = parse_and_validate(SPEC);
    assert!(diags.is_empty(), "{diags:#?}");
    let index = SpecIndex::from_single(spec);
    complete_line(&CompletionRequest {
        line,
        cursor: line.chars().count(),
        cwd,
        env: &HashMap::new(),
        aliases: &HashMap::new(),
        wrappers: &[],
        index: &index,
        runner: &FakeGeneratorRunner::new(),
        cache: &GeneratorCache::new(),
        usage: store,
        timeout: Duration::from_secs(1),
        now_ts,
        allow_stale: false,
    })
}

fn values(res: &CompletionResult) -> Vec<&str> {
    res.candidates.iter().map(|c| c.insert_value.as_str()).collect()
}

#[test]
fn failures_are_counted_and_penalize_ranking() {
    let store = Store::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();

    store.record_execution_complete(&cwd, "deploy staging", 0, 100, false);
    for ts in [110, 120, 130] {
        store.record_execution_complete(&cwd, "deploy prod", 0, ts, false);
    }
    // Without failures, the more used `prod` wins.
    assert_eq!(values(&query(&store, dir.path(), "deploy ", 200))[0], "prod");

    for ts in [140, 150, 160, 170, 180, 190] {
        store.record_execution_complete(&cwd, "deploy prod", 1, ts, false);
    }
    let rec = store.lookup_usage("deploy", "prod", &cwd);
    assert_eq!((rec.executions, rec.failures), (3, 6));
    assert!(rec.failure_penalty() > 0.0);
    // Chronically failing `prod` now ranks below `staging`.
    assert_eq!(values(&query(&store, dir.path(), "deploy ", 200))[0], "staging");

    // A single failure is noise, and signal exits (Ctrl-C) aren't failures.
    store.record_execution_complete(&cwd, "deploy staging", 1, 195, false);
    store.record_execution_complete(&cwd, "deploy staging", 130, 196, false);
    let rec = store.lookup_usage("deploy", "staging", &cwd);
    assert_eq!(rec.failures, 1);
    assert_eq!(rec.failure_penalty(), 0.0);
}

#[test]
fn tokens_that_only_failed_are_not_learned_values() {
    let store = Store::open_in_memory().expect("store");
    store.record_execution_complete("/w", "mytool good", 0, 100, false);
    store.record_execution_complete("/w", "mytool bad", 1, 110, false);
    assert_eq!(store.learned_values("mytool"), vec!["good".to_owned()]);
}

#[test]
fn failures_persist_and_old_corrections_table_is_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ezlo.db");
    {
        let conn = rusqlite::Connection::open(&db).expect("db");
        conn.execute_batch("CREATE TABLE corrections (ctx TEXT, typo TEXT, fix TEXT);")
            .expect("old table");
    }
    {
        let store = Store::open(&db).expect("store");
        store.record_execution_complete("/w", "gti status", 1, 100, false);
        store.record_execution_complete("/w", "git status", 0, 110, false);
    }
    let store = Store::open(&db).expect("reopen");
    assert_eq!(store.lookup_usage("", "gti", "/w").failures, 1);
    let conn = rusqlite::Connection::open(&db).expect("db");
    let has_table = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE name = 'corrections'")
        .and_then(|mut s| s.exists([]))
        .expect("query");
    assert!(!has_table);
}

#[test]
fn migrates_databases_without_failures_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("old.db");
    {
        let conn = rusqlite::Connection::open(&db).expect("db");
        conn.execute_batch(
            "CREATE TABLE usage (ctx TEXT NOT NULL, label TEXT NOT NULL, kind TEXT NOT NULL,
                 accepts INTEGER NOT NULL DEFAULT 0, executions INTEGER NOT NULL DEFAULT 0,
                 last_ts INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (ctx, label));
             INSERT INTO usage VALUES ('git', 'status', 'exec', 0, 4, 100);",
        )
        .expect("old schema");
    }
    let store = Store::open(&db).expect("migrated");
    let rec = store.lookup_usage("git", "status", "/w");
    assert_eq!((rec.executions, rec.failures), (4, 0));
    store.record_execution_complete("/w", "git status", 1, 200, false);
    drop(store);
    let store = Store::open(&db).expect("reopen");
    assert_eq!(store.lookup_usage("git", "status", "/w").failures, 1);
}

#[test]
fn workflow_recall_prefers_local_frecent_lines() {
    let store = Store::open_in_memory().expect("store");
    let repo = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(repo.path().join(".git")).expect("git dir");
    std::fs::create_dir(repo.path().join("sub")).expect("sub dir");
    let root = repo.path().to_string_lossy().into_owned();
    let sub = repo.path().join("sub").to_string_lossy().into_owned();

    for ts in [100, 110, 120] {
        store.record_execution_complete("/elsewhere", "htop", 0, ts, false);
    }
    store.record_execution_complete(&root, "cargo test", 0, 130, false);
    store.record_execution_complete(&root, "cargo test", 0, 140, false);
    store.record_execution_complete(&sub, "make docs", 0, 150, false);
    store.record_execution_complete(&root, "cargo bench", 0, 160, false);
    store.record_execution_complete(&root, "cargo broken", 1, 170, false);
    store.record_execution_complete(&root, " secret", 0, 180, true);

    let lines = |cwd: &str| -> Vec<String> {
        store
            .workflow_matches(cwd, true, 200, 10)
            .into_iter()
            .map(|m| m.line)
            .collect()
    };
    assert_eq!(lines(&root), vec!["cargo test", "cargo bench", "make docs", "htop"]);
    assert_eq!(lines(&sub)[0], "make docs");
    assert_eq!(lines("/elsewhere")[0], "htop");
    let here = store.workflow_matches(&root, true, 200, 10);
    assert!(here[2].in_cwd && !here[3].in_cwd);
}

#[test]
fn recall_message_roundtrip() {
    let msg = ClientMsg::Recall { id: 9 };
    assert_eq!(parse_client_line(&encode_client_msg(&msg)), Some(msg));
}

#[test]
fn history_rows_show_last_run_and_rank_similar_projects() {
    use ezlo::history::{history_candidates, LastRun};
    let store = Store::open_in_memory().expect("store");
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    let c = tempfile::tempdir().expect("tempdir");
    std::fs::write(a.path().join("Cargo.toml"), "").unwrap();
    std::fs::write(b.path().join("Cargo.toml"), "").unwrap();
    let (a_s, b_s, c_s) = (
        a.path().to_string_lossy().into_owned(),
        b.path().to_string_lossy().into_owned(),
        c.path().to_string_lossy().into_owned(),
    );
    store.record_execution(&a_s, "cargo nextest run", 0, 100, 12_000, false);
    store.record_execution(&c_s, "cargo fmt --all", 0, 200, 0, false);
    // Newest run of a line failed: the row still comes from the earlier success.
    store.record_execution(&a_s, "cargo nextest run", 101, 150, 3_000, false);

    // In another Rust project, the line from the Rust crate beats a newer one from elsewhere.
    let matches = store.history_matches("cargo ", &b_s, false, 5);
    assert_eq!(matches[0].line, "cargo nextest run");
    assert_eq!(matches[0].similar, Some("rust"));
    assert_eq!(matches[0].last_run, Some(LastRun { exit: 101, duration_ms: 3_000 }));
    assert_eq!(matches[1].similar, None);

    let rows = history_candidates(matches, 0, 5, 300);
    assert_eq!(rows[0].columns[..2], ["rust".to_string(), "✗ 101".to_string()]);

    store.record_execution(&a_s, "cargo nextest run", 0, 160, 65_000, false);
    let rows = history_candidates(store.history_matches("cargo ", &b_s, false, 5), 0, 5, 300);
    assert_eq!(rows[0].columns[1], "1m05s");
}

#[test]
fn last_runs_persist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ezlo.db");
    {
        let store = Store::open(&db).expect("store");
        store.record_execution("/w", "make test", 2, 100, 4_200, false);
    }
    let store = Store::open(&db).expect("reopen");
    let run = store.last_run("make test").expect("last run");
    assert_eq!((run.exit, run.duration_ms), (2, 4_200));
}
