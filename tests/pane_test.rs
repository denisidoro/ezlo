//! Candidate preview pane (#14): per-row availability on `W` lines and the async `PANE` / `V`
//! round trip through a real daemon (fake generator runner, temp files).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ezlo::config::{Config, PaneConfig, SourcesConfig};
use ezlo::daemon::protocol::{
    decode_pane, decode_wire_response, encode_client_msg, encode_pane, parse_client_line, ClientMsg, WirePane,
    WireResponse,
};
use ezlo::daemon::{run_daemon, stop_daemon};
use ezlo::engine::generator::FakeGeneratorRunner;
use ezlo::engine::preview::pane::PaneMode;
use ezlo::spec::{parse_and_validate, SpecIndex};
use ezlo::store::Store;

const SPEC: &str = r#"
type container
  $ docker ps -a --format '{{.Names}}'
  cols name
  insert name
  preview `docker logs --tail 5 "$name"`

cmd docker "Container runtime"

cmd docker logs "Fetch the logs of a container"
  arg <container> container

cmd cat "Concatenate files"
  arg <file...> path
"#;

struct Daemon {
    socket: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = stop_daemon(&self.socket);
    }
}

fn spawn_daemon(mode: PaneMode) -> (Daemon, Arc<FakeGeneratorRunner>) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ezlo.sock");
    let config = Config {
        sources: SourcesConfig {
            enabled: vec![ezlo::config::Source::Bundled],
            ..Default::default()
        },
        complete: ezlo::config::CompleteConfig {
            pane: PaneConfig {
                mode,
                lines: 2,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let mut diags = Vec::new();
    let specs = vec![("test:pane.ezlo".to_string(), parse_and_validate(SPEC).0)];
    let index = SpecIndex::merge_ordered(&specs, &mut diags);
    let store = Store::open_in_memory().unwrap();
    let runner = Arc::new(FakeGeneratorRunner::new());
    runner.set_response("docker ps", vec!["web".into(), "db".into()]);
    runner.set_response("docker logs", vec!["log line 1".into(), "log line 2".into(), "log line 3".into()]);
    let r = Arc::clone(&runner);
    let sock = socket.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let _ = rt.block_on(run_daemon(sock, config, store, index, r));
    });
    let start = Instant::now();
    // The socket file can appear before the daemon accepts: wait until a connection works.
    while UnixStream::connect(&socket).is_err() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "daemon socket was not created");
    (Daemon { socket, _dir: dir }, runner)
}

struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn connect(socket: &Path, cwd: &Path) -> Self {
        Self::connect_cols(socket, cwd, 80)
    }

    fn connect_cols(socket: &Path, cwd: &Path, cols: usize) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut c = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        };
        c.send(&ClientMsg::Hello {
            version: "1".into(),
            shell: "zsh".into(),
            session_id: "1".into(),
            cols,
        });
        c.send(&ClientMsg::Env {
            cwd: cwd.display().to_string(),
            vars: Default::default(),
        });
        c
    }

    fn send(&mut self, msg: &ClientMsg) {
        self.writer.write_all(encode_client_msg(msg).as_bytes()).unwrap();
    }

    fn line(&mut self) -> String {
        let mut l = String::new();
        self.reader.read_line(&mut l).unwrap();
        l
    }

    /// Reads one message whose header has `n` trailing lines at field `count_field`.
    fn message(&mut self, tag: &str) -> String {
        loop {
            let header = self.line();
            assert!(!header.is_empty(), "daemon closed the connection");
            let fields: Vec<&str> = header.trim_end().split('\x1f').collect();
            let n: usize = match fields[0] {
                "R" => {
                    fields[5].parse::<usize>().unwrap()
                        + fields.get(8).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0)
                        + fields.get(9).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0)
                }
                "V" => fields[3].parse().unwrap(),
                _ => 0,
            };
            let mut raw = header.clone();
            for _ in 0..n {
                raw.push_str(&self.line());
            }
            if fields[0] == tag {
                return raw;
            }
        }
    }

    fn query(&mut self, id: u64, buffer: &str) -> WireResponse {
        self.send(&ClientMsg::Query {
            id,
            cursor: buffer.chars().count(),
            buffer: buffer.into(),
        });
        // A background refresh may re-send the same id; the first answer is enough here.
        decode_wire_response(&self.message("R")).expect("R response")
    }

    fn pane(&mut self, id: u64, row: usize, insert: &str) -> WirePane {
        self.send(&ClientMsg::Pane {
            id,
            row_index: row,
            insert: insert.into(),
        });
        decode_pane(&self.message("V")).expect("V response")
    }
}

#[test]
fn protocol_pane_messages_roundtrip() {
    let msg = ClientMsg::Pane {
        id: 7,
        row_index: 2,
        insert: "src/main.rs".into(),
    };
    assert_eq!(parse_client_line(&encode_client_msg(&msg)), Some(msg));
    let pane = WirePane {
        id: 7,
        row: 2,
        height: 5,
        lines: vec![
            ("fg=blue".into(), "src/  a\x1fb".into(), String::new()),
            (String::new(), "x y".into(), "0-1:fg=203 2-3:fg=#f8f8f2,bold".into()),
        ],
    };
    let decoded = decode_pane(&encode_pane(&pane)).unwrap();
    assert_eq!(decoded.lines[0], ("fg=blue".to_owned(), "src/  ab".to_owned(), String::new()), "fields are sanitized");
    assert_eq!(decoded.lines[1].1, "x y");
    assert_eq!(decoded.lines[1].2, "0-1:fg=203 2-3:fg=#f8f8f2,bold");
    assert_eq!(decoded.height, 5);
}

#[test]
fn type_preview_rows_are_auto_and_pane_runs_the_preview_command() {
    let (daemon, runner) = spawn_daemon(PaneMode::Auto);
    let cwd = tempfile::tempdir().unwrap();
    // Wide enough for the pane beside the rows: `auto` rows open it by themselves.
    let mut c = Client::connect_cols(&daemon.socket, cwd.path(), 160);

    let mut resp = c.query(1, "docker logs ");
    if resp.rows.is_empty() {
        // Cold generator cache: the refreshed rows arrive in a second `R`.
        resp = decode_wire_response(&c.message("R")).unwrap();
    }
    let row = resp.rows.iter().position(|r| r.insert_text == "web").expect("web row") + 1;
    assert_eq!(resp.rows[row - 1].pane, "auto");
    assert!(resp.pane_col > 0, "the rows make room for the pane");

    let pane = c.pane(1, row, "web");
    assert_eq!((pane.id, pane.row), (1, row));
    let texts: Vec<&str> = pane.lines.iter().map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(texts, ["log line 1", "log line 2", "log line 3"], "whole output, the shell scrolls it");
    assert_eq!(pane.height, 2, "visible height from [complete.pane] lines");
    let calls = runner.recorded_calls();
    let (_, params) = calls.iter().find(|(cmd, _)| cmd.contains("docker logs")).expect("preview ran");
    assert!(params.contains(&("name".to_owned(), "web".to_owned())), "{params:?}");

    // A row that no longer matches its insert text gets an empty pane instead of a wrong one.
    let stale = c.pane(1, row, "gone");
    assert!(stale.lines.is_empty());
}

#[test]
fn path_rows_wait_for_alt_p_on_narrow_terminals_and_preview_file_heads_and_dirs() {
    let (daemon, _runner) = spawn_daemon(PaneMode::Auto);
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("notes.txt"), "first\nsecond\nthird\n").unwrap();
    std::fs::create_dir(cwd.path().join("nested")).unwrap();
    std::fs::write(cwd.path().join("nested/inner.rs"), "").unwrap();
    let mut c = Client::connect(&daemon.socket, cwd.path());

    let resp = c.query(1, "cat ");
    let file_row = resp.rows.iter().position(|r| r.insert_text == "notes.txt").expect("file row") + 1;
    let dir_row = resp.rows.iter().position(|r| r.insert_text == "nested/").expect("dir row") + 1;
    assert_eq!(resp.rows[file_row - 1].pane, "manual", "80 columns: the pane would cost lines");
    assert_eq!(resp.pane_col, 0);

    let file = c.pane(1, file_row, "notes.txt");
    let texts: Vec<&str> = file.lines.iter().map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(texts, ["first", "second", "third"]);

    let dir = c.pane(1, dir_row, "nested/");
    assert_eq!(dir.lines, vec![("fg=blue".to_owned(), "inner.rs".to_owned(), String::new())]);

    // A newer query makes previews for the old response moot: nothing is sent for them.
    let _ = c.query(2, "cat n");
    c.send(&ClientMsg::Pane {
        id: 1,
        row_index: file_row,
        insert: "notes.txt".into(),
    });
    let fresh = c.pane(2, 1, "notes.txt");
    assert_eq!(fresh.id, 2, "the stale request was dropped, not answered");
}

#[test]
fn mode_on_marks_path_rows_auto_and_off_marks_everything_manual() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("notes.txt"), "x\n").unwrap();

    let (daemon, _runner) = spawn_daemon(PaneMode::On);
    let mut c = Client::connect(&daemon.socket, cwd.path());
    let resp = c.query(1, "cat ");
    assert!(resp.rows.iter().any(|r| r.insert_text == "notes.txt" && r.pane == "auto"));
    assert!(resp.rows.iter().all(|r| r.kind == "history" || !r.pane.is_empty()));
    drop(c);
    drop(daemon);

    // `auto` on a wide terminal: path rows open the pane like type previews do.
    let (daemon, _runner) = spawn_daemon(PaneMode::Auto);
    let mut c = Client::connect_cols(&daemon.socket, cwd.path(), 160);
    let resp = c.query(1, "cat ");
    assert!(resp.rows.iter().any(|r| r.insert_text == "notes.txt" && r.pane == "auto"));
    drop(c);
    drop(daemon);

    let (daemon, _runner) = spawn_daemon(PaneMode::Off);
    let mut c = Client::connect(&daemon.socket, cwd.path());
    let resp = c.query(1, "cat ");
    assert!(resp.rows.iter().any(|r| r.insert_text == "notes.txt" && r.pane == "manual"));
}
