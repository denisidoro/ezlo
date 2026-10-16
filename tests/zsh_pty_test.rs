use std::fs::File;
use std::io::{Read, Write};
use std::io::{BufRead, BufReader};
use std::os::fd::AsFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ezlo::bundled::load_bundled_specs;
use ezlo::config::Config;
use ezlo::daemon::{run_daemon, stop_daemon};
use ezlo::engine::generator::FakeGeneratorRunner;
use ezlo::spec::{parse_and_validate, SpecIndex};
use ezlo::store::Store;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::{openpty, Winsize};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

/// Self-contained docker fixture so the test doesn't depend on which specs are bundled.
const DOCKER_SPEC: &str = r#"
type container
  $ docker ps -a --format '{{.ID}}\t{{.Names}}\t{{.State}}'
  cols id name state
  insert name
  cache 2s

type container:stopped = container where state in (created, exited, dead)

cmd docker "Container runtime"

cmd docker rm "Remove one or more containers"
  opt -f --force "Force removal of running containers"
  opt -v --volumes "Remove anonymous volumes"
  arg <container...> container:stopped
    when --force: container
"#;

trait PreExecCtty {
    fn pre_exec_ctty(&mut self) -> &mut Self;
}

impl PreExecCtty for Command {
    fn pre_exec_ctty(&mut self) -> &mut Self {
        // SAFETY: only async-signal-safe calls (setsid, ioctl) run between fork and exec.
        unsafe {
            self.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
        }
    }
}

struct PtyZshHarness {
    master: File,
    child: Child,
    vt: vt100::Parser,
    socket_path: PathBuf,
    _temp_dir: tempfile::TempDir,
}

impl Drop for PtyZshHarness {
    fn drop(&mut self) {
        let _ = stop_daemon(&self.socket_path);
        let pid = self.child.id() as i32;
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PtyZshHarness {
    fn spawn(pre_source_script: &str) -> (Self, Arc<FakeGeneratorRunner>) {
        Self::spawn_with(pre_source_script, |_| {})
    }

    /// As `spawn`, with `configure` adjusting the daemon's config first.
    fn spawn_with(pre_source_script: &str, configure: impl FnOnce(&mut Config)) -> (Self, Arc<FakeGeneratorRunner>) {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let socket_path = temp_dir.path().join("ezlo.sock");

        // Disable native zsh fallback in spec-only PTY test so completion is 100% deterministic
        let mut config = Config {
            sources: ezlo::config::SourcesConfig {
                enabled: vec![ezlo::config::Source::Bundled],
                ..Default::default()
            },
            ..Default::default()
        };
        configure(&mut config);

        let mut diags = Vec::new();
        let mut specs = load_bundled_specs();
        specs.push(("test:docker.ezlo".to_string(), parse_and_validate(DOCKER_SPEC).0));
        let index = SpecIndex::merge_ordered(&specs, &mut diags);
        let store = Store::open_in_memory().expect("open in-memory store");
        let runner = Arc::new(FakeGeneratorRunner::new());
        let runner_clone = Arc::clone(&runner);

        let sock_for_daemon = socket_path.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let _ = rt.block_on(run_daemon(
                sock_for_daemon,
                config,
                store,
                index,
                runner_clone,
            ));
        });

        // Wait for daemon socket to appear
        let start = Instant::now();
        while !socket_path.exists() && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(socket_path.exists(), "daemon socket was not created");

        (Self::spawn_zsh(temp_dir, socket_path, pre_source_script), runner)
    }

    /// Starts an interactive zsh sourcing ezlo.zsh, talking to whatever listens on `socket_path`.
    fn spawn_zsh(temp_dir: tempfile::TempDir, socket_path: PathBuf, pre_source_script: &str) -> Self {

        let winsize = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).expect("openpty");
        let slave_in = pty.slave.try_clone().expect("clone slave in");
        let slave_out = pty.slave.try_clone().expect("clone slave out");
        let slave_err = pty.slave;

        let zdotdir = temp_dir.path().join("zdotdir");
        std::fs::create_dir_all(&zdotdir).expect("create zdotdir");

        let ezlo_zsh_path = std::env::var_os("EZLO_PTY_ZSH_TMP").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("shell")
            .join("ezlo.zsh"));

        let zshrc = format!(
            r#"
unsetopt beep
PROMPT='PROMPT> '
PS1='PROMPT> '
export COLUMNS=80
export LINES=24
export EZLO_SOCKET="{}"
docker() {{ print -r -- "RAN_DOCKER:$*"; }}
{}
source "{}"
"#,
            socket_path.display(),
            pre_source_script,
            ezlo_zsh_path.display()
        );
        std::fs::write(zdotdir.join(".zshrc"), zshrc).expect("write .zshrc");

        let mut cmd = Command::new("zsh");
        // `-d` (NO_GLOBAL_RCS): skip the host's /etc/zsh/zshrc & co. They are not part of the test
        // and can be slow (a gLinux one runs compinit plus zshrc.d hooks: ~2.7s before the prompt).
        cmd.arg("-d")
            .arg("-i")
            .env_clear()
            .env("HOME", temp_dir.path())
            .env("ZDOTDIR", &zdotdir)
            .env("TERM", "xterm-256color")
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .env("LANG", "C.UTF-8")
            .env("EZLO_SOCKET", &socket_path)
            .stdin(Stdio::from(slave_in))
            .stdout(Stdio::from(slave_out))
            .stderr(Stdio::from(slave_err))
            // New session with the pty as controlling terminal, so `/dev/tty` (opened by
            // `ezlo pick`) is this pty rather than whatever terminal runs the tests.
            .pre_exec_ctty();

        let child = cmd.spawn().expect("spawn zsh");
        let master = File::from(pty.master);

        let mut harness = Self {
            master,
            child,
            vt: vt100::Parser::new(24, 80, 100),
            socket_path,
            _temp_dir: temp_dir,
        };

        // Keys sent before zle draws the prompt land in a cooked-mode tty: the kernel echoes them
        // above the prompt (breaking prompt-row assertions) and slow startups eat the test's own
        // timeouts. So readiness is asserted, with a startup budget separate from the test's.
        let ready = harness.wait_until(|screen| screen.contains("PROMPT> "), Duration::from_secs(10));
        assert!(ready, "zsh prompt did not appear, screen:\n{}", harness.screen_contents());
        harness
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write to pty master");
        let _ = self.master.flush();
    }

    fn pump_output(&mut self, wait_ms: u16) {
        let mut buf = [0u8; 4096];
        let mut pfd = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
        if let Ok(n) = poll(&mut pfd, PollTimeout::from(wait_ms)) {
            if n > 0 {
                if let Ok(count) = self.master.read(&mut buf) {
                    if count > 0 {
                        self.vt.process(&buf[..count]);
                        // Answer cursor-position queries (DSR, `ESC[6n`) like a real terminal;
                        // inline TUIs such as `ezlo pick` block on them. Assumes a query isn't
                        // split across reads, which holds for these tiny writes.
                        let queries = buf[..count].windows(4).filter(|w| w == b"\x1b[6n").count();
                        for _ in 0..queries {
                            let (row, col) = self.vt.screen().cursor_position();
                            let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                            let _ = self.master.write_all(reply.as_bytes());
                        }
                    }
                }
            }
        }
    }

    fn screen_contents(&self) -> String {
        self.vt.screen().contents()
    }

    fn wait_until<F>(&mut self, predicate: F, timeout: Duration) -> bool
    where
        F: Fn(&str) -> bool,
    {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pump_output(25);
            if predicate(&self.screen_contents()) {
                return true;
            }
        }
        false
    }
}

#[test]
fn zsh_pty_menu_render_tab_accept_enter_unchanged_and_zero_fork() {
    // Point EZLO_BIN to a trap function that prints FORKED_EZLO if ezlo.zsh ever forks on a keystroke
    let (mut h, runner) = PtyZshHarness::spawn("export EZLO_BIN=/nonexistent/ezlo-no-fork-allowed");

    // 1. Type `docker rm -` -> assert menu rows appear in the rendered terminal screen
    h.send(b"docker rm -");
    let menu_shown = h.wait_until(
        |scr| scr.contains("--force") && scr.contains("Force removal"),
        Duration::from_secs(3),
    );
    assert!(
        menu_shown,
        "expected inline menu with `--force` to appear in terminal screen, got:\n{}",
        h.screen_contents()
    );

    // Record generator count once at `docker rm -`, then type `f` and `Tab` and assert 0 additional forks
    let calls_at_dash = runner.call_count();

    // 2. Press `Tab` -> assert buffer becomes `docker rm --force `
    h.send(b"\t");
    let accepted = h.wait_until(
        |scr| scr.contains("PROMPT> docker rm --force "),
        Duration::from_secs(3),
    );
    assert!(
        accepted,
        "expected Tab to accept `--force `, screen:\n{}",
        h.screen_contents()
    );
    assert_eq!(
        runner.call_count(),
        calls_at_dash,
        "expected 0 generator forks when accepting option completion"
    );

    // 3. Clear line, type `docker rm -f` (menu appears), press `Enter` WITHOUT `Tab`:
    //    - assert the typed line executes unchanged (`RAN_DOCKER:rm -f`)
    //    - assert the menu is cleared before Enter and never remains in scrollback
    h.send(b"\x15"); // Ctrl-U
    h.wait_until(
        |scr| !scr.contains("docker rm --force"),
        Duration::from_secs(2),
    );

    h.send(b"docker rm -f");
    h.wait_until(|scr| scr.contains("--force"), Duration::from_secs(2));

    h.send(b"\r");
    let ran = h.wait_until(
        |scr| scr.contains("RAN_DOCKER:rm -f"),
        Duration::from_secs(3),
    );
    let final_screen = h.screen_contents();
    assert!(
        ran,
        "expected Enter without Tab to execute `docker rm -f` unchanged, screen:\n{final_screen}"
    );
    assert!(
        !final_screen.contains("Force removal of running containers"),
        "expected menu to be cleared on line-finish and NOT remain in scrollback, screen:\n{final_screen}"
    );
}

/// The settings popup's rows (title to footer), as screen lines.
fn popup_lines(screen: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(top) = lines.iter().position(|l| l.contains("╭─ ezlo settings")) else {
        return Vec::new();
    };
    let bottom = lines[top..].iter().position(|l| l.contains('╰')).map_or(lines.len(), |i| top + i + 1);
    lines[top..bottom].iter().map(|l| l.trim_end().to_string()).collect()
}

#[test]
fn zsh_pty_alt_k_popup_toggles_suggestions_right_aligned() {
    let (mut h, _runner) = PtyZshHarness::spawn("export EZLO_BIN=/nonexistent/ezlo-no-fork-allowed");

    h.send(b"docker rm -");
    assert!(
        h.wait_until(|scr| scr.contains("Force removal"), Duration::from_secs(3)),
        "menu should show before opening the popup, screen:\n{}",
        h.screen_contents()
    );

    h.send(b"\x1bk"); // Alt-k
    let open = h.wait_until(|scr| popup_lines(scr).last().is_some_and(|l| l.contains('╰')), Duration::from_secs(3));
    assert!(open, "Alt-k should open the settings popup, screen:\n{}", h.screen_contents());
    let scr = h.screen_contents();
    let popup = popup_lines(&scr);
    assert_eq!(popup.len(), 12, "title + 10 rows + footer, screen:\n{scr}");
    // Right-aligned: every popup line ends in the last-but-one column (COLUMNS - 1 = 79 cells).
    for line in &popup {
        assert_eq!(line.chars().count(), 79, "popup line not right-aligned: {line:?}\nscreen:\n{scr}");
        assert!(line.starts_with("     "), "popup should be indented, not at column 0: {line:?}");
    }
    assert!(popup[1].contains("› Suggestions") && popup[1].contains("‹ on ›"), "screen:\n{scr}");
    assert!(popup.iter().any(|l| l.contains("Dropdown rows") && l.trim_end_matches('│').trim_end().ends_with('6')));
    assert!(popup.iter().any(|l| l.contains("Reset to defaults")));
    assert!(!scr.contains("Force removal"), "the dropdown hides while the popup is open, screen:\n{scr}");
    assert!(last_prompt_row(&scr).ends_with("PROMPT> docker rm -"), "screen:\n{scr}");

    // Space turns suggestions off (the old Alt-k behavior); typing in the popup is swallowed.
    h.send(b" ");
    let off = h.wait_until(|scr| popup_lines(scr).get(1).is_some_and(|l| l.contains("‹ off ›")), Duration::from_secs(3));
    assert!(off, "Space should toggle suggestions off, screen:\n{}", h.screen_contents());
    h.send(b"x");
    h.send(b"\x1b");
    let closed = h.wait_until(|scr| !scr.contains("ezlo settings"), Duration::from_secs(3));
    assert!(closed, "Esc should close the popup, screen:\n{}", h.screen_contents());
    let scr = h.screen_contents();
    assert!(last_prompt_row(&scr).ends_with("PROMPT> docker rm -"), "popup keys never edit the line, screen:\n{scr}");
    assert!(!scr.contains("Force removal"), "no suggestions while disabled, screen:\n{scr}");

    h.send(b"f");
    h.pump_output(300);
    assert!(!h.screen_contents().contains("Force removal"), "screen:\n{}", h.screen_contents());

    // Back on: Alt-k, Space, Alt-k (closes too).
    h.send(b"\x1bk");
    assert!(h.wait_until(|scr| scr.contains("ezlo settings"), Duration::from_secs(3)));
    h.send(b" ");
    h.wait_until(|scr| popup_lines(scr).get(1).is_some_and(|l| l.contains("‹ on ›")), Duration::from_secs(3));
    h.send(b"\x1bk");
    let back = h.wait_until(|scr| !scr.contains("ezlo settings") && scr.contains("--force"), Duration::from_secs(3));
    assert!(back, "suggestions come back once re-enabled, screen:\n{}", h.screen_contents());

    // Enter still runs the line unchanged.
    h.send(b"\r");
    let ran = h.wait_until(|scr| scr.contains("RAN_DOCKER:rm -f"), Duration::from_secs(3));
    assert!(ran, "Enter after the popup runs the line, screen:\n{}", h.screen_contents());
}

#[test]
fn zsh_pty_coexistence_with_autosuggestions_widget() {
    // Load a mock `zsh-autosuggestions` widget BEFORE `ezlo.zsh` that writes to POSTDISPLAY. Like
    // the real one, it only writes when the buffer changed, so a later wipe by ezlo would stick.
    // `zmodload zsh/zle` first: add-zle-hook-widget silently does nothing until zle is loaded.
    let pre_script = r#"
zmodload zsh/zle
_mock_autosuggest_pre_redraw() {
  [[ $BUFFER == "$_mock_last_buf" ]] && return
  _mock_last_buf=$BUFFER
  if [[ "$BUFFER" == "unknown_cmd "* ]]; then
    POSTDISPLAY="--autosuggest-tail"
  fi
}
autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-pre-redraw _mock_autosuggest_pre_redraw
"#;
    // An empty cwd: ezlo offers the cwd's files as built-in candidates for a spec-less command's
    // arguments, which would rightly replace the tail.
    let empty = tempfile::tempdir().expect("tempdir");
    let (mut h, _) = PtyZshHarness::spawn(&format!("cd '{}'\n{pre_script}", empty.path().display()));

    // When `ezlo` has no menu and no ghost for `unknown_cmd `, the prior POSTDISPLAY is preserved
    h.send(b"unknown_cmd ");
    let preserved = h.wait_until(
        |scr| scr.contains("unknown_cmd --autosuggest-tail"),
        Duration::from_secs(3),
    );
    // Still there once ezlo's (empty) response had time to land, not just in a transient frame.
    let wiped = h.wait_until(|scr| !scr.contains("unknown_cmd --autosuggest-tail"), Duration::from_millis(500));
    assert!(
        preserved && !wiped,
        "expected mock zsh-autosuggestions POSTDISPLAY to be preserved when ezlo has no menu, screen:\n{}",
        h.screen_contents()
    );
}

/// Fake daemon that only answers queries for `gi`, so the screen right after typing another key
/// shows what ezlo.zsh draws on its own before any response arrives.
fn spawn_gi_only_daemon(socket_path: &std::path::Path) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            if f.first() != Some(&"Q") || f.get(3) != Some(&"gi") {
                continue;
            }
            let us = '\x1f';
            let resp = format!(
                "R{us}{id}{us}0{us}2{us}t status{us}1{us}1{us}6\nW{us}  git{us}2-5:fg=green{us}git{us} {us}cmd{us}the stupid content tracker{us}{us}git  the stupid content tracker\n",
                id = f[1]
            );
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_typing_keeps_overlay_consistent_before_response() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_gi_only_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");

    h.send(b"gi");
    let shown = h.wait_until(
        |scr| scr.contains("PROMPT> git status") && scr.contains("the stupid content tracker"),
        Duration::from_secs(3),
    );
    assert!(shown, "expected ghost + menu for `gi`, screen:\n{}", h.screen_contents());

    // The daemon never answers `git`: the ghost must be trimmed locally (not `gitt status`) and the
    // selected row must keep its highlight instead of flashing unstyled until the next response.
    h.send(b"t");
    let typed = h.wait_until(|scr| scr.contains("PROMPT> git "), Duration::from_secs(3));
    h.pump_output(200);
    let screen = h.vt.screen();
    let contents = screen.contents();
    assert!(typed && contents.contains("PROMPT> git status"), "ghost not trimmed, screen:\n{contents}");
    assert!(!contents.contains("gitt"), "stale ghost shown, screen:\n{contents}");

    let rows: Vec<String> = screen.rows(0, 80).collect();
    let prompt_row = rows.iter().position(|r| r.starts_with("PROMPT> git")).expect("prompt row") as u16;
    let ghost_cell = screen.cell(prompt_row, 12).expect("ghost cell");
    assert_eq!(ghost_cell.contents(), "s");
    assert_eq!(ghost_cell.fgcolor(), vt100::Color::Idx(8), "ghost lost its dim style");
    let sel_cell = screen.cell(prompt_row + 1, 2).expect("selected row cell");
    assert_eq!(sel_cell.contents(), "g");
    assert_eq!(sel_cell.bgcolor(), vt100::Color::Idx(236), "selected row lost its highlight");

    let _ = std::fs::remove_file(&socket_path);
}

/// Fake daemon that answers every query with a single `MENU-ROW` dropdown row.
fn spawn_always_menu_daemon(socket_path: &std::path::Path) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            if f.first() != Some(&"Q") {
                continue;
            }
            let us = '\x1f';
            let resp = format!(
                "R{us}{id}{us}0{us}0{us}{us}1{us}1{us}6\nW{us}  MENU-ROW{us}{us}x{us} {us}cmd{us}menu row\n",
                id = f[1]
            );
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

/// Fake daemon answering every query with 12 dropdown rows (blank gutter cell last, as
/// `format_rows` lays them out) and a 4-row window.
fn spawn_long_menu_daemon(socket_path: &std::path::Path) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            if f.first() != Some(&"Q") {
                continue;
            }
            let us = '\x1f';
            let mut resp = format!("R{us}{id}{us}0{us}0{us}{us}12{us}1{us}4\n", id = f[1]);
            for i in 1..=12 {
                resp += &format!("W{us}  LONG-{i:02}{:20} {us}{us}x{i}{us} {us}arg{us}row {i}\n", "");
            }
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_dropdown_draws_scrollbar_thumb_when_rows_overflow() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_long_menu_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");
    let row = |scr: &str, n: u32| scr.lines().find(|l| l.contains(&format!("LONG-{n:02}"))).map(|l| l.trim_end().to_string());

    h.send(b"x");
    let shown = h.wait_until(|scr| scr.contains("LONG-04"), Duration::from_secs(3));
    assert!(shown, "dropdown should render, screen:\n{}", h.screen_contents());
    // 4 of 12 rows at the top: a 1-cell thumb (round(4*4/12) = 1) on the first row only.
    let scr = h.screen_contents();
    assert!(row(&scr, 1).unwrap().ends_with('┃'), "screen:\n{scr}");
    assert!((2..=4).all(|n| !row(&scr, n).unwrap().ends_with('┃')), "screen:\n{scr}");
    assert!(!scr.contains("LONG-05"));

    // Scrolling to the end moves the thumb to the last visible row.
    for _ in 0..11 {
        h.send(b"\x1b[B");
    }
    let moved = h.wait_until(|scr| row(scr, 12).is_some_and(|r| r.ends_with('┃')), Duration::from_secs(3));
    assert!(moved, "thumb should follow the window, screen:\n{}", h.screen_contents());
    let scr = h.screen_contents();
    assert!((9..=11).all(|n| !row(&scr, n).unwrap().ends_with('┃')), "screen:\n{scr}");

    let _ = std::fs::remove_file(&socket_path);
}

fn last_prompt_row(screen: &str) -> String {
    screen.lines().rfind(|l| l.starts_with("PROMPT> ")).unwrap_or_default().trim_end().to_string()
}

#[test]
fn zsh_pty_repeated_up_walks_history_instead_of_menu() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_always_menu_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");

    h.send(b"echo one\r");
    assert!(h.wait_until(|scr| scr.contains("\none"), Duration::from_secs(3)));
    h.send(b"echo two\r");
    assert!(h.wait_until(|scr| scr.contains("\ntwo"), Duration::from_secs(3)));

    h.send(b"\x1b[A");
    let recalled = h.wait_until(|scr| last_prompt_row(scr) == "PROMPT> echo two", Duration::from_secs(3));
    assert!(recalled, "first Up should recall `echo two`, screen:\n{}", h.screen_contents());
    // Give the daemon time to answer, so a menu would be on screen if ezlo queried the recalled line.
    h.pump_output(300);
    assert!(!h.screen_contents().contains("MENU-ROW"), "no menu for a recalled line, screen:\n{}", h.screen_contents());

    h.send(b"\x1b[A");
    let walked = h.wait_until(|scr| last_prompt_row(scr) == "PROMPT> echo one", Duration::from_secs(3));
    assert!(walked, "second Up should recall `echo one`, screen:\n{}", h.screen_contents());

    // Editing the recalled line brings suggestions back.
    h.send(b"x");
    let back = h.wait_until(|scr| scr.contains("MENU-ROW"), Duration::from_secs(3));
    assert!(back, "editing should re-enable the menu, screen:\n{}", h.screen_contents());

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn zsh_pty_highlights_diagnostic_span_and_renders_footer() {
    let (mut h, _runner) = PtyZshHarness::spawn("");

    h.send(b"docker rm --bogus ");
    let shown = h.wait_until(
        |scr| scr.contains("⚠ unknown option '--bogus'"),
        Duration::from_secs(3),
    );
    assert!(
        shown,
        "expected diagnostic footer even with 0 menu candidates, screen:\n{}",
        h.screen_contents()
    );

    let screen = h.vt.screen();
    let rows: Vec<String> = screen.rows(0, 80).collect();
    let prompt_row = rows
        .iter()
        .position(|r| r.starts_with("PROMPT> docker rm --bogus"))
        .expect("prompt row") as u16;
    // "PROMPT> docker rm " is 18 columns (0..18); "--bogus" is at columns 18..25.
    let flag_cell = screen.cell(prompt_row, 18).expect("flag cell");
    assert_eq!(flag_cell.contents(), "-");
    assert_eq!(
        flag_cell.fgcolor(),
        vt100::Color::Idx(1),
        "expected red foreground on unknown option span"
    );
    assert!(
        flag_cell.underline(),
        "expected underline on unknown option span"
    );
}


#[test]
fn zsh_pty_alt_h_explains_line_until_edit_or_esc() {
    let (mut h, _runner) = PtyZshHarness::spawn("export EZLO_BIN=/nonexistent/ezlo-no-fork-allowed");

    h.send(b"docker rm -f web");
    h.pump_output(300);
    h.send(b"\x1bh"); // Alt-h
    let shown = h.wait_until(
        |scr| {
            scr.contains("docker rm — Remove one or more containers")
                && scr.contains("Force removal of running containers")
                && scr.contains("when --force: container (active)")
        },
        Duration::from_secs(3),
    );
    assert!(shown, "Alt-h should render the explanation below the prompt, screen:\n{}", h.screen_contents());
    assert!(
        h.screen_contents().contains("PROMPT> docker rm -f web"),
        "the buffer is left untouched, screen:\n{}",
        h.screen_contents()
    );

    // Any edit drops the explanation.
    h.send(b"x");
    let cleared = h.wait_until(
        |scr| !scr.contains("when --force: container") && scr.contains("PROMPT> docker rm -f webx"),
        Duration::from_secs(3),
    );
    assert!(cleared, "editing should clear the explanation, screen:\n{}", h.screen_contents());

    // Esc dismisses it too.
    h.send(b"\x1bh");
    assert!(
        h.wait_until(|scr| scr.contains("docker rm — Remove"), Duration::from_secs(3)),
        "explanation again, screen:\n{}",
        h.screen_contents()
    );
    h.send(b"\x1b");
    let dismissed = h.wait_until(|scr| !scr.contains("docker rm — Remove"), Duration::from_secs(3));
    assert!(dismissed, "Esc should dismiss the explanation, screen:\n{}", h.screen_contents());
}

/// Fake daemon that answers `RECALL` with a single `make docs` history row (replacing 0..0).
fn spawn_recall_daemon(socket_path: &std::path::Path) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            if f.first() != Some(&"RECALL") {
                continue;
            }
            let us = '\x1f';
            let resp = format!(
                "R{us}{id}{us}0{us}0{us}{us}1{us}1{us}6\nW{us}  make docs{us}{us}make docs{us}{us}history{us}RECALLED-ROW{us}{us}RECALLED-ROW\n",
                id = f[1]
            );
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_tab_on_empty_prompt_recalls_workflows() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_recall_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");
    h.pump_output(300);

    h.send(b"\t");
    let shown = h.wait_until(|scr| scr.contains("RECALLED-ROW"), Duration::from_secs(5));
    assert!(shown, "Tab on an empty prompt should show recalled lines, screen:\n{}", h.screen_contents());

    h.send(b"\t");
    // Not `last_prompt_row`: under load zsh's PROMPT_SP `%` marker can share the prompt's row.
    let accepted = h.wait_until(
        |scr| scr.lines().any(|l| l.trim_end().ends_with("PROMPT> make docs")),
        Duration::from_secs(3),
    );
    assert!(accepted, "second Tab should insert the recalled line, screen:\n{}", h.screen_contents());

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn zsh_pty_renders_blast_radius_preview_for_globs() {
    let files = tempfile::tempdir().expect("tempdir");
    std::fs::write(files.path().join("a.log"), vec![b'a'; 100]).unwrap();
    std::fs::write(files.path().join("b.log"), vec![b'b'; 200]).unwrap();
    let (mut h, _runner) = PtyZshHarness::spawn(&format!("cd '{}'", files.path().display()));

    h.send(b"rm *.log");
    let shown = h.wait_until(|scr| scr.contains("⇢ rm: 2 files · 300 B"), Duration::from_secs(3));
    assert!(shown, "expected the blast-radius line below the prompt, screen:\n{}", h.screen_contents());

    h.send(b"x");
    let warned = h.wait_until(|scr| scr.contains("⚠ rm: no matches found: *.logx"), Duration::from_secs(3));
    assert!(warned, "an unmatched glob warns instead, screen:\n{}", h.screen_contents());
}

#[test]
fn zsh_pty_completes_jq_paths_from_piped_file() {
    let files = tempfile::tempdir().expect("tempdir");
    std::fs::write(files.path().join("data.json"), r#"{"users":[{"name":"alice"}],"meta":{"total":1}}"#).unwrap();
    let (mut h, _runner) = PtyZshHarness::spawn(&format!("cd '{}'", files.path().display()));

    h.send(b"cat data.json | jq .us");
    let shown = h.wait_until(|scr| scr.contains(".users[].name") && scr.contains("data.json"), Duration::from_secs(3));
    assert!(shown, "expected jq paths sampled from data.json in the menu, screen:\n{}", h.screen_contents());

    h.send(b"\t");
    let accepted = h.wait_until(|scr| last_prompt_row(scr).ends_with("PROMPT> cat data.json | jq .users"), Duration::from_secs(3));
    assert!(accepted, "Tab should insert the top path, screen:\n{}", h.screen_contents());
}

#[test]
fn zsh_pty_renders_positional_signature_with_active_slot() {
    let (mut h, _runner) = PtyZshHarness::spawn("");

    h.send(b"chmod ");
    let shown = h.wait_until(|scr| scr.contains("  chmod  <mode>  <file…>"), Duration::from_secs(3));
    assert!(shown, "expected the positional signature in the status line, screen:\n{}", h.screen_contents());

    let screen = h.vt.screen();
    let rows: Vec<String> = screen.rows(0, 80).collect();
    let sig_row = rows
        .iter()
        .position(|r| r.starts_with("  chmod  <mode>  <file…>"))
        .expect("signature row") as u16;
    let menu_row = rows.iter().position(|r| r.contains("755")).expect("dropdown row") as u16;
    assert!(sig_row > menu_row, "the status line sits below the rows, screen:\n{}", h.screen_contents());
    // "  chmod  " is 9 columns: the active `<mode>` placeholder starts at column 9.
    let cmd_cell = screen.cell(sig_row, 2).expect("command cell");
    assert!(!cmd_cell.underline(), "the command path is not highlighted");
    let active_cell = screen.cell(sig_row, 9).expect("placeholder cell");
    assert_eq!(active_cell.contents(), "<");
    assert!(active_cell.underline(), "expected the active placeholder underlined");
    assert_eq!(active_cell.fgcolor(), vt100::Color::Idx(6), "expected cyan on the active placeholder");

    // Typing a flag hides the hint.
    h.send(b"-");
    let hidden = h.wait_until(|scr| !scr.contains("<mode>"), Duration::from_secs(3));
    assert!(hidden, "the signature hides while typing an option, screen:\n{}", h.screen_contents());
}

/// Fake daemon: every query gets two rows, `AUTO-ROW` (preview `auto`) and `MANUAL-ROW` (preview
/// `manual`); `PANE` answers with one `PANE-<row>-<insert>` line. A nonzero `pane_col` asks for
/// the pane right of the rows, starting at that column.
fn spawn_pane_daemon(socket_path: &std::path::Path, pane_col: usize) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            let us = '\x1f';
            let resp = match f.first() {
                Some(&"Q") => format!(
                    "R{us}{id}{us}0{us}0{us}{us}2{us}1{us}6{us}0{us}0{us}{us}{us}{us}{pane_col}\n\
                     W{us}  AUTO-ROW{us}{us}auto{us} {us}arg{us}auto row{us}auto{us}AUTO-FULL\n\
                     W{us}  MANUAL-ROW{us}{us}manual{us} {us}file{us}manual row{us}manual\n",
                    id = f[1]
                ),
                Some(&"PANE") => format!(
                    "V{us}{id}{us}{row}{us}1\nL{us}fg=blue{us}PANE-{row}-{ins}\n",
                    id = f[1],
                    row = f[2],
                    ins = f.get(3).copied().unwrap_or_default()
                ),
                _ => continue,
            };
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_preview_pane_follows_selection_and_alt_p_toggle() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_pane_daemon(&socket_path, 0);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");

    h.send(b"foo ");
    let shown = h.wait_until(|scr| scr.contains("    PANE-1-auto"), Duration::from_secs(3));
    assert!(shown, "an `auto` row opens the pane below the dropdown, screen:\n{}", h.screen_contents());
    let screen = h.vt.screen();
    let rows: Vec<String> = screen.rows(0, 80).collect();
    let pane_row = rows.iter().position(|r| r.starts_with("    PANE-1-auto")).expect("pane row");
    let menu_row = rows.iter().position(|r| r.contains("MANUAL-ROW")).expect("menu row");
    assert!(pane_row > menu_row, "the pane sits below the dropdown rows, screen:\n{}", h.screen_contents());
    assert_eq!(screen.cell(pane_row as u16, 4).expect("pane cell").fgcolor(), vt100::Color::Idx(4), "daemon style applied");

    // A `manual` row closes it until Alt-P.
    h.send(b"\x1b[B");
    let hidden = h.wait_until(|scr| scr.contains("MANUAL-ROW") && !scr.contains("PANE-"), Duration::from_secs(3));
    assert!(hidden, "manual rows have no pane by default, screen:\n{}", h.screen_contents());

    h.send(b"\x1bp");
    let toggled = h.wait_until(|scr| scr.contains("    PANE-2-manual"), Duration::from_secs(3));
    assert!(toggled, "Alt-P shows the manual row's pane, screen:\n{}", h.screen_contents());

    h.send(b"\x1bp");
    let off = h.wait_until(|scr| scr.contains("MANUAL-ROW") && !scr.contains("PANE-"), Duration::from_secs(3));
    assert!(off, "Alt-P again hides it, screen:\n{}", h.screen_contents());

    // Off sticks for `auto` rows too.
    h.send(b"\x1b[A");
    h.pump_output(300);
    assert!(!h.screen_contents().contains("PANE-"), "toggled off stays off, screen:\n{}", h.screen_contents());

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn zsh_pty_preview_pane_sits_right_of_the_rows() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_pane_daemon(&socket_path, 40);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");

    h.send(b"foo ");
    let shown = h.wait_until(|scr| scr.contains("  PANE-1-auto"), Duration::from_secs(3));
    assert!(shown, "an `auto` row opens the pane, screen:\n{}", h.screen_contents());
    let screen = h.vt.screen();
    let rows: Vec<String> = screen.rows(0, 80).collect();
    let auto_row = rows.iter().position(|r| r.contains("AUTO-ROW")).expect("auto row");
    // The truncated row's full text heads the pane instead of taking a status line.
    assert!(rows[auto_row].contains("  AUTO-FULL"), "the pane starts on the first row's line, screen:\n{}", h.screen_contents());
    assert_eq!(screen.cell(auto_row as u16, 40).expect("gutter cell").contents().trim(), "", "no gutter bar");
    assert!(screen.cell(auto_row as u16, 42).expect("header cell").bold(), "the header is bold");
    let manual_row = rows.iter().position(|r| r.contains("MANUAL-ROW")).expect("manual row");
    assert!(rows[manual_row].contains("  PANE-1-auto"), "the preview follows the header, screen:\n{}", h.screen_contents());
    assert_eq!(screen.cell(manual_row as u16, 42).expect("pane cell").fgcolor(), vt100::Color::Idx(4), "daemon style applied");
    assert!(!rows.iter().any(|r| r.contains("AUTO-FULL") && !r.contains("AUTO-ROW")), "no status line copy, screen:\n{}", h.screen_contents());
    assert!(!rows.iter().any(|r| r.starts_with("    PANE")), "nothing below the dropdown, screen:\n{}", h.screen_contents());

    let _ = std::fs::remove_file(&socket_path);
}

/// Fake daemon: six rows `ROW-<n>` with `auto` previews beside them (column 40); `PANE` answers
/// with 20 lines `P<row>-<nn>` and a visible height of 3.
fn spawn_long_pane_daemon(socket_path: &std::path::Path) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            let us = '\x1f';
            let resp = match f.first() {
                Some(&"Q") => {
                    let mut out = format!("R{us}{id}{us}0{us}0{us}{us}6{us}1{us}6{us}0{us}0{us}{us}{us}{us}40\n", id = f[1]);
                    for n in 1..=6 {
                        out.push_str(&format!("W{us}  ROW-{n}{us}{us}row{n}{us} {us}arg{us}row {n}{us}auto\n"));
                    }
                    out
                }
                Some(&"PANE") => {
                    let (id, row) = (f[1], f[2]);
                    let mut out = format!("V{us}{id}{us}{row}{us}20{us}3\n");
                    for n in 1..=20 {
                        out.push_str(&format!("L{us}{us}P{row}-{n:02}\n"));
                    }
                    out
                }
                _ => continue,
            };
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_preview_pane_fills_the_dropdown_height_and_scrolls() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_long_pane_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");

    h.send(b"foo ");
    let shown = h.wait_until(|scr| scr.contains("P1-06") && scr.contains("ROW-6"), Duration::from_secs(3));
    assert!(shown, "the pane is as tall as the six rows, not 3 lines, screen:\n{}", h.screen_contents());
    let scr = h.screen_contents();
    assert!(!scr.contains("P1-07"), "no taller than the rows, screen:\n{scr}");
    let screen = h.vt.screen();
    let rows: Vec<String> = screen.rows(0, 80).collect();
    let first = rows.iter().position(|r| r.contains("ROW-1")).expect("first row");
    assert_eq!(screen.cell(first as u16, 40).expect("gutter cell").contents(), "┃", "scroll thumb on top");

    // Shift-Down scrolls by half the pane.
    h.send(b"\x1b[1;2B");
    let scrolled = h.wait_until(|scr| scr.contains("P1-04") && scr.contains("P1-09") && !scr.contains("P1-03"), Duration::from_secs(3));
    assert!(scrolled, "Shift-Down scrolls the pane, screen:\n{}", h.screen_contents());
    assert!(h.screen_contents().contains("ROW-1"), "the dropdown itself didn't move");

    // Another row starts at its top again.
    h.send(b"\x1b[B");
    let reset = h.wait_until(|scr| scr.contains("P2-01") && scr.contains("P2-06"), Duration::from_secs(3));
    assert!(reset, "a new row's preview starts at the top, screen:\n{}", h.screen_contents());

    // Shift-Up at the top stays there.
    h.send(b"\x1b[1;2A");
    h.pump_output(300);
    assert!(h.screen_contents().contains("P2-01"), "screen:\n{}", h.screen_contents());

    let _ = std::fs::remove_file(&socket_path);
}

/// Fake daemon for the status line: two rows (the first truncated, with a `detail`), a signature
/// and an effect line; a buffer starting with `w` adds an effect warning.
fn spawn_status_daemon(socket_path: &std::path::Path) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let f: Vec<&str> = line.split('\x1f').collect();
            if f.first() != Some(&"Q") {
                continue;
            }
            let us = '\x1f';
            let warn = f.get(3).is_some_and(|b| b.starts_with('w'));
            let mut resp = format!(
                "R{us}{id}{us}0{us}0{us}{us}2{us}1{us}6{us}0{us}{np}{us}cmd  <x>  <y>{us}5{us}8{us}0\n\
                 W{us}  ROW-ONE…{us}{us}one{us} {us}arg{us}d{us}{us}FULL-DETAIL\n\
                 W{us}  ROW-TWO{us}{us}two{us} {us}arg{us}d{us}{us}\n\
                 P{us}effect{us}EFFECT-LINE\n",
                id = f[1],
                np = if warn { 2 } else { 1 },
            );
            if warn {
                resp.push_str(&format!("P{us}warning{us}WARN-LINE\n"));
            }
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_dropdown_keeps_metadata_to_one_status_line() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_status_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");

    let below_rows = |h: &PtyZshHarness| -> Vec<String> {
        let rows: Vec<String> = h.vt.screen().rows(0, 80).map(|r| r.trim_end().to_string()).collect();
        let two = rows.iter().position(|r| r.contains("ROW-TWO")).expect("second row");
        rows[two + 1..].iter().filter(|r| !r.is_empty()).cloned().collect()
    };

    // A truncated selected row: its full text wins over the signature; effect lines follow it.
    h.send(b"a ");
    let shown = h.wait_until(|scr| scr.contains("FULL-DETAIL"), Duration::from_secs(3));
    assert!(shown, "the truncated row's full text shows, screen:\n{}", h.screen_contents());
    let shown = h.wait_until(|scr| scr.contains("⇢ EFFECT-LINE"), Duration::from_secs(3));
    assert!(shown, "effect lines stay visible under the dropdown, screen:\n{}", h.screen_contents());
    assert_eq!(below_rows(&h), ["  FULL-DETAIL", "  ⇢ EFFECT-LINE"], "one status line + effects, screen:\n{}", h.screen_contents());

    // A row that fits: the signature takes the line.
    h.send(b"\x1b[B");
    let sig = h.wait_until(|scr| scr.contains("  cmd  <x>  <y>"), Duration::from_secs(3));
    assert!(sig, "the signature shows when nothing more important does, screen:\n{}", h.screen_contents());
    assert_eq!(below_rows(&h).len(), 2, "still one status line + effects, screen:\n{}", h.screen_contents());
    drop(h);

    // An effect warning beats both.
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    spawn_status_daemon(&socket_path);
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");
    h.send(b"w ");
    let warned = h.wait_until(|scr| scr.contains("⚠ WARN-LINE"), Duration::from_secs(3));
    assert!(warned, "the effect warning takes the status line, screen:\n{}", h.screen_contents());
    assert_eq!(below_rows(&h), ["  ⚠ WARN-LINE", "  ⇢ EFFECT-LINE"], "one status line + effects, screen:\n{}", h.screen_contents());

    let _ = std::fs::remove_file(&socket_path);
}

const ADB_RECIPE_SPEC: &str = r#"
cmd adb "Android Debug Bridge"

  recipe "Launch an app"
    $ adb shell monkey -p com.example.app -c android.intent.category.LAUNCHER 1

  recipe "List devices"
    $ adb devices -l
"#;

/// Ctrl-G opens `ezlo pick` below a multi-line prompt: the picker must not paint over the
/// prompt's last line (`» `), and the picked command lands after it with the space intact.
#[test]
fn zsh_pty_ctrl_g_picker_keeps_multiline_prompt_intact() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    let specs_dir = temp_dir.path().join("specs");
    std::fs::create_dir_all(&specs_dir).expect("create specs dir");
    let spec_path = specs_dir.join("adb.ezlo");
    std::fs::write(&spec_path, ADB_RECIPE_SPEC).expect("write spec");
    std::fs::set_permissions(&spec_path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).expect("chmod spec");
    let pre = format!(
        "PROMPT=$'PROMPT> top\\n» '\nexport EZLO_BIN='{}'\nexport EZLO_SPECS_DIR='{}'\nexport EZLO_CONFIG_DIR='{}'",
        env!("CARGO_BIN_EXE_ezlo"),
        specs_dir.display(),
        temp_dir.path().join("config").display(),
    );
    // No daemon: the widgets degrade gracefully, and `ezlo pick` loads specs itself.
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path, &pre);
    h.pump_output(300);

    h.send(b"adb ");
    h.pump_output(200);
    h.send(b"\x07");
    let open = h.wait_until(|scr| scr.contains("recipe ❯"), Duration::from_secs(5));
    assert!(open, "Ctrl-G should open the picker, screen:\n{}", h.screen_contents());
    h.pump_output(200);
    let screen = h.screen_contents();
    let rows: Vec<&str> = screen.lines().collect();
    let prompt_row = rows.iter().rposition(|r| r.starts_with("PROMPT> top")).expect("prompt first line");
    assert_eq!(
        rows.get(prompt_row + 1).map(|r| r.trim_end()),
        Some("» adb"),
        "the picker must open below the prompt, not over it, screen:\n{screen}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with(" adb ")),
        "the picker's template row keeps its own leading space, screen:\n{screen}"
    );

    h.send(b"\r");
    let picked = h.wait_until(
        |scr| scr.lines().any(|l| l.trim_end().starts_with("» adb ") && !l.contains("recipe ❯") && l.len() > 10),
        Duration::from_secs(5),
    );
    let screen = h.screen_contents();
    assert!(picked && !screen.contains("recipe ❯"), "picked command should replace the buffer, screen:\n{screen}");
    let rows: Vec<&str> = screen.lines().collect();
    let prompt_row = rows.iter().rposition(|r| r.starts_with("PROMPT> top")).expect("prompt first line");
    assert!(
        rows.get(prompt_row + 1).is_some_and(|r| r.starts_with("» adb ")),
        "the picked command sits after `» ` on the prompt's second line, screen:\n{screen}"
    );
    assert_eq!(screen.matches("PROMPT> top").count(), 1, "the prompt is redrawn in place, screen:\n{screen}");
}

/// Fake daemon: 12 dropdown rows per query (window from `SET rows=`, else 6), `S` replies to `SET`
/// with config values, and every line it gets is recorded.
fn spawn_settings_daemon(socket_path: &std::path::Path, seen: Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut writer = stream.try_clone().expect("clone fake daemon stream");
        let mut rows = 6;
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            seen.lock().expect("lock").push(line.clone());
            let f: Vec<&str> = line.split('\x1f').collect();
            let us = '\x1f';
            let resp = match f.first() {
                Some(&"SET") => {
                    rows = f.iter().find_map(|kv| kv.strip_prefix("rows=")).and_then(|v| v.parse().ok()).unwrap_or(6);
                    format!("S{us}rows=6{us}history_rows=3{us}min_chars=2{us}signature=1{us}diagnostics=1{us}preview=0\n")
                }
                Some(&"Q") => {
                    let mut resp = format!("R{us}{id}{us}0{us}0{us}{us}12{us}1{us}{rows}\n", id = f[1]);
                    for i in 1..=12 {
                        resp += &format!("W{us}  ROW-{i:02}{:20} {us}{us}x{i}{us} {us}arg{us}row {i}\n", "");
                    }
                    resp
                }
                _ => continue,
            };
            if writer.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    });
}

#[test]
fn zsh_pty_popup_changes_dropdown_rows_and_sends_session_overrides() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    spawn_settings_daemon(&socket_path, Arc::clone(&seen));
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path.clone(), "");
    let visible = |scr: &str| (1..=12).filter(|i| scr.contains(&format!("ROW-{i:02}"))).count();

    h.send(b"x");
    assert!(h.wait_until(|scr| visible(scr) == 6, Duration::from_secs(3)), "screen:\n{}", h.screen_contents());

    h.send(b"\x1bk");
    assert!(h.wait_until(|scr| scr.contains("ezlo settings"), Duration::from_secs(3)), "screen:\n{}", h.screen_contents());
    // Config values from the `S` reply are shown as defaults.
    let scr = h.screen_contents();
    let popup = popup_lines(&scr);
    let row = |p: &[String], label: &str| p.iter().find(|l| l.contains(label)).cloned().unwrap_or_default();
    assert!(row(&popup, "Min chars").contains(" 2 "), "screen:\n{scr}");
    assert!(row(&popup, "Effect preview").contains(" off "), "screen:\n{scr}");

    // Down to "Dropdown rows", Left twice: 6 -> 4, highlighted as changed.
    h.send(b"j");
    h.send(b"\x1b[D");
    h.send(b"h");
    let four = h.wait_until(|scr| row(&popup_lines(scr), "Dropdown rows").contains("‹ 4 ›"), Duration::from_secs(3));
    assert!(four, "Left should decrease the rows, screen:\n{}", h.screen_contents());
    let screen = h.vt.screen();
    let lines: Vec<String> = screen.rows(0, 80).collect();
    let r = lines.iter().position(|l| l.contains("Dropdown rows")).expect("rows line") as u16;
    let value_col = lines[r as usize].find('4').map(|b| lines[r as usize][..b].chars().count()).expect("value") as u16;
    assert_eq!(screen.cell(r, value_col).expect("cell").fgcolor(), vt100::Color::Idx(3), "changed values are yellow");

    h.send(b"\x1b");
    let fewer = h.wait_until(|scr| !scr.contains("ezlo settings") && visible(scr) == 4, Duration::from_secs(3));
    assert!(fewer, "the dropdown should show 4 rows, screen:\n{}", h.screen_contents());
    assert!(
        seen.lock().expect("lock").iter().any(|l| l == "SET\x1frows=4"),
        "the override goes to the daemon, got: {:?}",
        seen.lock().expect("lock")
    );

    // Reset to defaults (last row, wrapping up from the first) drops every override.
    h.send(b"\x1bk");
    assert!(h.wait_until(|scr| scr.contains("ezlo settings"), Duration::from_secs(3)));
    h.send(b"k");
    h.send(b"\r");
    let reset = h.wait_until(|scr| row(&popup_lines(scr), "Dropdown rows").trim_end_matches('│').trim_end().ends_with(" 6"), Duration::from_secs(3));
    assert!(reset, "reset restores the default, screen:\n{}", h.screen_contents());
    h.send(b"q");
    let six = h.wait_until(|scr| !scr.contains("ezlo settings") && visible(scr) == 6, Duration::from_secs(3));
    assert!(six, "screen:\n{}", h.screen_contents());
    let last_set = seen.lock().expect("lock").iter().rev().find(|l| l.starts_with("SET")).cloned();
    assert_eq!(last_set.as_deref(), Some("SET"), "reset sends an empty override set");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn zsh_pty_popup_min_chars_override_reaches_the_daemon() {
    let (mut h, _runner) = PtyZshHarness::spawn("");

    // `do` is below the default `[complete] min_chars` (3): no dropdown.
    h.send(b"do");
    h.pump_output(400);
    assert!(!h.screen_contents().contains("docker"), "screen:\n{}", h.screen_contents());

    h.send(b"\x1bk");
    assert!(h.wait_until(|scr| scr.contains("ezlo settings"), Duration::from_secs(3)));
    for _ in 0..3 {
        h.send(b"j");
    }
    h.send(b"--");
    let one = h.wait_until(|scr| popup_lines(scr).iter().any(|l| l.contains("Min chars") && l.contains("‹ 1 ›")), Duration::from_secs(3));
    assert!(one, "screen:\n{}", h.screen_contents());
    h.send(b"\x1b");
    let shown = h.wait_until(|scr| !scr.contains("ezlo settings") && scr.lines().any(|l| l.contains("docker") && !l.starts_with("PROMPT>")), Duration::from_secs(3));
    assert!(shown, "the daemon should suggest after 2 chars now, screen:\n{}", h.screen_contents());
}

/// Fake translator: `fail…` requests fail, `slow…` ones hang, anything else suggests `ls -la`
/// and `ls -lS` after a short "thinking" pause.
const FAKE_TRANSLATOR: &str = r#"read -r q
case "$q" in
  fail*) echo 'API Error: quota exceeded' >&2; exit 1 ;;
  slow*) sleep 10 ;;
esac
sleep 0.4
echo '```sh'
echo 'ls -la'
echo 'ls -lS'
echo '```'
"#;

#[test]
fn zsh_pty_comment_line_translates_to_a_command_in_the_background() {
    let script_dir = tempfile::tempdir().expect("create tempdir");
    let script = script_dir.path().join("fake-llm");
    std::fs::write(&script, FAKE_TRANSLATOR).expect("write translator");
    let command = vec!["/bin/sh".to_owned(), script.display().to_string()];
    let (mut h, _runner) = PtyZshHarness::spawn_with("bindkey -e", |c| c.complete.translate.command = command);

    h.send(b"# list files by size");
    let offered = h.wait_until(|scr| scr.contains("Translate to command"), Duration::from_secs(3));
    assert!(offered, "a comment line offers the translate row, screen:\n{}", h.screen_contents());

    h.send(b"\t");
    let spinning = h.wait_until(|scr| scr.contains("translating… (esc cancels)"), Duration::from_secs(3));
    assert!(spinning, "Tab starts the translation with a status line, screen:\n{}", h.screen_contents());
    assert!(h.screen_contents().contains("PROMPT> # list files by size"), "the comment stays while it runs");

    let suggested = h.wait_until(
        |scr| scr.contains("ls -la") && scr.contains("ls -lS") && !scr.contains("translating"),
        Duration::from_secs(5),
    );
    assert!(suggested, "the answers are offered as rows, screen:\n{}", h.screen_contents());
    assert!(h.screen_contents().contains("PROMPT> # list files by size"), "the comment stays until a row is picked");

    // Down + Tab picks the second suggestion, replacing the whole line.
    h.send(b"\x1b[B\t");
    let replaced = h.wait_until(
        |scr| scr.lines().any(|l| l.trim_end().ends_with("PROMPT> ls -lS")),
        Duration::from_secs(5),
    );
    assert!(replaced, "the picked row replaces the line, screen:\n{}", h.screen_contents());

    // Undo brings the comment back.
    h.send(b"\x1f"); // Ctrl-_
    let undone = h.wait_until(
        |scr| scr.lines().any(|l| l.trim_end().ends_with("PROMPT> # list files by size")),
        Duration::from_secs(3),
    );
    assert!(undone, "undo restores the comment, screen:\n{}", h.screen_contents());

    // Failures show the translator's message and keep the comment.
    h.send(b"\x15# fail please");
    h.wait_until(|scr| scr.contains("PROMPT> # fail please") && scr.contains("Translate to command"), Duration::from_secs(3));
    h.send(b"\t");
    let failed = h.wait_until(
        |scr| scr.contains("✗ translate: `sh` exited with status 1: API Error: quota exceeded"),
        Duration::from_secs(5),
    );
    assert!(failed, "the error is shown below the prompt, screen:\n{}", h.screen_contents());
    assert!(h.screen_contents().contains("PROMPT> # fail please"), "the comment is kept");

    // Esc cancels a running translation (with the default KEYTIMEOUT: nothing may arrive on the
    // socket meanwhile). Ctrl-X Ctrl-L works on any comment line too.
    h.send(b"\x15# slow one");
    h.wait_until(|scr| scr.contains("PROMPT> # slow one"), Duration::from_secs(3));
    h.send(b"\x18\x0c"); // Ctrl-X Ctrl-L
    assert!(
        h.wait_until(|scr| scr.contains("translating…"), Duration::from_secs(3)),
        "Ctrl-X Ctrl-L translates too, screen:\n{}",
        h.screen_contents()
    );
    h.send(b"\x1b");
    let cancelled = h.wait_until(
        |scr| !scr.contains("translating…") && scr.contains("PROMPT> # slow one"),
        Duration::from_secs(3),
    );
    assert!(cancelled, "Esc cancels and keeps the comment, screen:\n{}", h.screen_contents());
}

/// Ctrl-R opens `ezlo history search` seeded with the line: Esc keeps the typed buffer, Enter puts
/// the picked history line on the prompt without running it.
#[test]
fn zsh_pty_ctrl_r_searches_history_and_replaces_the_buffer() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    let histfile = temp_dir.path().join("histfile");
    std::fs::write(
        &histfile,
        ": 1700000000:0;ls -la\n: 1700000100:0;git push origin main\n: 1700000200:0;git status\n",
    )
    .expect("write histfile");
    let pre = format!(
        "export EZLO_BIN='{}'\nexport EZLO_CONFIG_DIR='{}'\nHISTFILE='{}'",
        env!("CARGO_BIN_EXE_ezlo"),
        temp_dir.path().join("config").display(),
        histfile.display(),
    );
    // No daemon: the search reads the history file (and ezlo's database, absent here) itself.
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path, &pre);
    h.pump_output(300);

    h.send(b"push");
    h.pump_output(200);
    h.send(b"\x12");
    let open = h.wait_until(|scr| scr.contains("history ❯ push") && scr.contains("└"), Duration::from_secs(5));
    assert!(open, "Ctrl-R should open the history search, screen:\n{}", h.screen_contents());
    let screen = h.screen_contents();
    assert!(screen.contains("▸ git push origin main"), "the matching line is selected, screen:\n{screen}");
    assert!(!screen.contains("ls -la") && !screen.contains("git status"), "non-matching lines are filtered, screen:\n{screen}");

    h.send(b"\x1b");
    let closed = h.wait_until(|scr| !scr.contains("history ❯"), Duration::from_secs(5));
    let screen = h.screen_contents();
    assert!(closed && last_prompt_row(&screen).ends_with("PROMPT> push"), "Esc keeps the buffer, screen:\n{screen}");

    h.send(b"\x12");
    assert!(h.wait_until(|scr| scr.contains("history ❯ push") && scr.contains("└"), Duration::from_secs(5)));
    h.send(b"\r");
    let picked = h.wait_until(
        |scr| !scr.contains("history ❯") && last_prompt_row(scr).ends_with("PROMPT> git push origin main"),
        Duration::from_secs(5),
    );
    let screen = h.screen_contents();
    assert!(picked, "Enter puts the picked line on the prompt, screen:\n{screen}");
    assert!(!screen.contains("RAN_"), "the picked line is not run, screen:\n{screen}");
}

/// Ctrl-O in the history search runs the picked line right away.
#[test]
fn zsh_pty_ctrl_r_ctrl_o_runs_the_picked_line() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = temp_dir.path().join("ezlo.sock");
    let histfile = temp_dir.path().join("histfile");
    std::fs::write(&histfile, ": 1700000000:0;echo RAN_PICKED_LINE\n: 1700000100:0;ls\n").expect("write histfile");
    let pre = format!(
        "export EZLO_BIN='{}'\nexport EZLO_CONFIG_DIR='{}'\nHISTFILE='{}'",
        env!("CARGO_BIN_EXE_ezlo"),
        temp_dir.path().join("config").display(),
        histfile.display(),
    );
    let mut h = PtyZshHarness::spawn_zsh(temp_dir, socket_path, &pre);
    h.pump_output(300);

    h.send(b"\x12");
    assert!(h.wait_until(|scr| scr.contains("history ❯") && scr.contains("└"), Duration::from_secs(5)));
    h.send(b"picked");
    assert!(h.wait_until(|scr| scr.contains("history ❯ picked") && scr.contains("1/1"), Duration::from_secs(5)));
    h.send(b"\x0f");
    let ran = h.wait_until(|scr| scr.lines().any(|l| l.trim_end() == "RAN_PICKED_LINE"), Duration::from_secs(5));
    let screen = h.screen_contents();
    assert!(ran && !screen.contains("history ❯"), "Ctrl-O runs the picked line, screen:\n{screen}");
}
