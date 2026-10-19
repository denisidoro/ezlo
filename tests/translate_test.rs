//! Comment-line translation (`[complete.translate]`): the translator runner with fixture scripts,
//! and the daemon's `Q` row + `TRANSLATE` replies over a real socket.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ezlo::config::{Config, SourcesConfig, TranslateConfig};
use ezlo::daemon::protocol::{decode_translate, decode_wire_response, TranslateState};
use ezlo::daemon::{run_daemon, stop_daemon};
use ezlo::engine::generator::FakeGeneratorRunner;
use ezlo::engine::translate::{run_translator, TranslateContext, TranslateOptions};
use ezlo::spec::SpecIndex;
use ezlo::store::Store;

const SEP: char = '\x1f';

/// Writes an executable `/bin/sh` fixture script.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn opts(command: &[&str], timeout: Duration) -> TranslateOptions {
    TranslateOptions {
        command: command.iter().map(|s| (*s).to_owned()).collect(),
        timeout,
    }
}

fn run(opts: &TranslateOptions, query: &str, env: &HashMap<String, String>) -> Result<Vec<String>, String> {
    let cancel = AtomicBool::new(false);
    let ctx = TranslateContext {
        cwd: Path::new("/"),
        env,
        cancel: &cancel,
    };
    // A fixture written while another test thread forks can briefly be "text file busy" (its
    // write fd lives on in that child until exec); retry like a user would.
    for _ in 0..50 {
        match run_translator(opts, query, &ctx) {
            Err(e) if e.contains("Text file busy") => std::thread::sleep(Duration::from_millis(20)),
            other => return other,
        }
    }
    run_translator(opts, query, &ctx)
}

#[test]
fn translator_gets_the_request_on_stdin_and_its_output_is_cleaned() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Echoes the request back inside a fenced block, as chatty models do.
    let echo = script(dir.path(), "echo-cmd", r#"read -r q; printf '```sh\necho "%s"\n```\n' "$q""#);
    let got = run(&opts(&[echo.to_str().unwrap()], Duration::from_secs(5)), "list files", &HashMap::new());
    assert_eq!(got, Ok(vec![r#"echo "list files""#.to_owned()]));
}

#[test]
fn translator_resolves_through_the_shell_path_and_passes_args() {
    let dir = tempfile::tempdir().expect("tempdir");
    script(dir.path(), "fake-llm", r#"cat >/dev/null; echo "ls --sort=$1""#);
    let env = HashMap::from([("PATH".to_owned(), format!("{}:/usr/bin:/bin", dir.path().display()))]);
    let got = run(&opts(&["fake-llm", "size"], Duration::from_secs(5)), "q", &env);
    assert_eq!(got, Ok(vec!["ls --sort=size".to_owned()]));
}

#[test]
fn translator_failures_are_short_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fail = script(dir.path(), "fail", "cat >/dev/null; echo 'API Error: quota' >&2; exit 3");
    let err = run(&opts(&[fail.to_str().unwrap()], Duration::from_secs(5)), "q", &HashMap::new()).unwrap_err();
    assert_eq!(err, "`fail` exited with status 3: API Error: quota");

    let stdout_err = script(dir.path(), "stdout-err", "cat >/dev/null; echo 'API Error: quota'");
    let err = run(&opts(&[stdout_err.to_str().unwrap()], Duration::from_secs(5)), "q", &HashMap::new()).unwrap_err();
    assert_eq!(err, "`stdout-err`: API Error: quota");

    let empty = script(dir.path(), "empty", "cat >/dev/null; echo '```'; echo");
    let err = run(&opts(&[empty.to_str().unwrap()], Duration::from_secs(5)), "q", &HashMap::new()).unwrap_err();
    assert_eq!(err, "`empty` printed no command");

    let missing = run(&opts(&["/nonexistent/ezlo-llm"], Duration::from_secs(5)), "q", &HashMap::new());
    assert!(missing.unwrap_err().starts_with("cannot run `/nonexistent/ezlo-llm`"));

    let unconfigured = run(&TranslateOptions::default(), "q", &HashMap::new()).unwrap_err();
    assert!(unconfigured.starts_with("not configured"), "{unconfigured}");
}

#[test]
fn translator_is_killed_on_timeout_and_cancel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let slow = script(dir.path(), "slow", "sleep 30; echo late");
    let slow = slow.to_str().unwrap();

    let started = Instant::now();
    let err = run(&opts(&[slow], Duration::from_millis(300)), "q", &HashMap::new()).unwrap_err();
    assert_eq!(err, "timed out after 300ms");
    assert!(started.elapsed() < Duration::from_secs(5), "timeout must kill the translator");

    // The cancel flag (a newer TRANSLATE, Esc, or the shell going away) kills it too.
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let env = HashMap::new();
    let ctx = TranslateContext {
        cwd: Path::new("/"),
        env: &env,
        cancel: &cancel,
    };
    let started = Instant::now();
    let err = run_translator(&opts(&[slow], Duration::from_secs(30)), "q", &ctx).unwrap_err();
    assert_eq!(err, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
}

struct Daemon {
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = stop_daemon(&self.socket);
    }
}

fn spawn_daemon(translate: TranslateConfig, dir: tempfile::TempDir) -> Daemon {
    let socket = dir.path().join("ezlo.sock");
    let mut config = Config {
        sources: SourcesConfig {
            enabled: Vec::new(),
            ..Default::default()
        },
        ..Default::default()
    };
    config.complete.translate = translate;
    let sock = socket.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        let store = Store::open_in_memory().expect("store");
        let runner = Arc::new(FakeGeneratorRunner::new());
        let _ = rt.block_on(run_daemon(sock, config, store, SpecIndex::default(), runner));
    });
    let start = Instant::now();
    while !socket.exists() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "daemon socket missing");
    Daemon { socket, _dir: dir }
}

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        // The socket file can appear a moment before the daemon accepts on it.
        let start = Instant::now();
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(s) => break s,
                Err(e) if start.elapsed() > Duration::from_secs(3) => panic!("connect: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut c = Self { stream, reader };
        c.send(&format!("HELLO{SEP}1{SEP}zsh{SEP}test{SEP}80"));
        c
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stream, "{line}").expect("send");
    }

    /// Reads one message: a header line plus the `n` lines its count field announces.
    fn read_msg(&mut self) -> String {
        let mut header = String::new();
        self.reader.read_line(&mut header).expect("read header");
        let fields: Vec<&str> = header.trim_end().split(SEP).collect();
        let extra = match fields[0] {
            "T" => fields[3].parse::<usize>().unwrap_or(0),
            // Rows, diagnostics and preview lines.
            "R" => [5, 8, 9]
                .iter()
                .map(|&i| fields.get(i).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0))
                .sum(),
            _ => 0,
        };
        let mut msg = header;
        for _ in 0..extra {
            self.reader.read_line(&mut msg).expect("read body");
        }
        msg
    }

    /// Skips other messages up to the `T` reply, which must be for `id`.
    fn translate_result(&mut self, id: u64) -> (TranslateState, Vec<String>) {
        loop {
            let msg = self.read_msg();
            let Some(t) = decode_translate(&msg) else { continue };
            assert_eq!(t.id, id);
            return (t.state, t.lines);
        }
    }
}

#[test]
fn daemon_offers_the_translate_row_only_for_configured_comment_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = script(dir.path(), "fake-llm", "cat >/dev/null; echo 'ls -la'");
    let daemon = spawn_daemon(
        TranslateConfig {
            command: vec![fake.display().to_string(), "zsh".into()],
            ..Default::default()
        },
        dir,
    );
    let mut c = Client::connect(&daemon.socket);

    c.send(&format!("Q{SEP}1{SEP}12{SEP}# list files"));
    let resp = decode_wire_response(&c.read_msg()).expect("R");
    assert_eq!(resp.rows.len(), 1, "{resp:?}");
    assert_eq!(resp.rows[0].kind, "translate");
    assert!(resp.rows[0].display_text.contains("Ask AI"));
    assert_eq!(resp.rows[0].description, "fake-llm zsh");
    assert!(resp.diagnostics.is_empty() && resp.ghost.is_empty());

    // A bare `#` has nothing to translate: no rows (and no completions for comment lines).
    c.send(&format!("Q{SEP}2{SEP}2{SEP}# "));
    let resp = decode_wire_response(&c.read_msg()).expect("R");
    assert!(resp.rows.is_empty(), "{resp:?}");
}

#[test]
fn daemon_without_translator_shows_no_row_and_explains_on_translate() {
    let daemon = spawn_daemon(TranslateConfig::default(), tempfile::tempdir().expect("tempdir"));
    let mut c = Client::connect(&daemon.socket);
    c.send(&format!("Q{SEP}1{SEP}12{SEP}# list files"));
    let resp = decode_wire_response(&c.read_msg()).expect("R");
    assert!(resp.rows.is_empty(), "{resp:?}");

    c.send(&format!("TRANSLATE{SEP}2{SEP}# list files"));
    let (state, lines) = c.translate_result(2);
    assert_eq!(state, TranslateState::Err);
    assert!(lines[0].starts_with("not configured"), "{lines:?}");
}

#[test]
fn daemon_translates_asynchronously_and_reports_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = script(
        dir.path(),
        "fake-llm",
        r#"read -r q; case "$q" in fail*) echo 'Error: boom' >&2; exit 1 ;; esac; sleep 0.4; echo "find . -name '$q'"; echo "fd '$q'""#,
    );
    // Run through `sh` rather than exec'ing the fresh fixture (no "text file busy" races).
    let daemon = spawn_daemon(
        TranslateConfig {
            command: vec!["/bin/sh".into(), fake.display().to_string()],
            ..Default::default()
        },
        dir,
    );
    let mut c = Client::connect(&daemon.socket);

    // The session keeps answering queries while the translator thinks.
    c.send(&format!("TRANSLATE{SEP}7{SEP}#  notes"));
    c.send(&format!("Q{SEP}8{SEP}6{SEP}# note"));
    let first = c.read_msg();
    assert!(first.starts_with(&format!("R{SEP}8{SEP}")), "query answered first: {first:?}");
    // The suggestions come as rows replacing the whole line, then `T ok` ends the translation.
    let rows = decode_wire_response(&c.read_msg()).expect("R");
    assert_eq!((rows.id, rows.start, rows.end), (7, 0, "#  notes".len()));
    let got: Vec<(&str, &str)> = rows.rows.iter().map(|r| (r.kind.as_str(), r.insert_text.as_str())).collect();
    assert_eq!(got, [("translated", "find . -name 'notes'"), ("translated", "fd 'notes'")]);
    let (state, lines) = c.translate_result(7);
    assert_eq!(state, TranslateState::Ok);
    assert_eq!(lines, ["find . -name 'notes'", "fd 'notes'"]);

    c.send(&format!("TRANSLATE{SEP}8{SEP}# fail please"));
    let (state, lines) = c.translate_result(8);
    assert_eq!(state, TranslateState::Err);
    assert_eq!(lines, ["`sh` exited with status 1: Error: boom"]);

    // Cancelled translations stay silent; the next one still answers.
    c.send(&format!("TRANSLATE{SEP}9{SEP}# slow one"));
    c.send(&format!("TRANSLATE{SEP}9{SEP}"));
    c.send(&format!("TRANSLATE{SEP}10{SEP}# next"));
    let (state, lines) = c.translate_result(10);
    assert_eq!(state, TranslateState::Ok);
    assert_eq!(lines, ["find . -name 'next'", "fd 'next'"]);
}
