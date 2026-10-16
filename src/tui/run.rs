//! Terminal setup, event loop and preview worker for `ezlo pick`.

use super::*;

pub fn run_pick_interactive(
    index: &SpecIndex,
    runner: Arc<dyn GeneratorRunner>,
    cache: &GeneratorCache,
    opts: &PickOptions,
) -> Result<PickOutcome> {
    let mut app = PickApp::new(index, runner.as_ref(), cache, opts);
    if let Some(done) = app.is_immediately_complete() {
        return Ok(PickOutcome::Selected(done));
    }

    let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    // One row is kept for the caller's prompt line, which stays visible above the viewport.
    let viewport_height = pick_viewport_height(term_rows.saturating_sub(1), opts.rows);
    app.page_rows = usize::from(viewport_height.saturating_sub(CHROME_ROWS)).max(1);

    let preview = spawn_preview_worker(Arc::clone(&runner), opts.cwd.clone(), opts.env.clone());

    let _stdout_guard = StdoutToTty::new(&tty)?;
    terminal::enable_raw_mode()?;
    let origin_col = open_below_cursor_line(&tty);
    let backend = CrosstermBackend::new(tty);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(viewport_height),
        },
    )?;

    let res = run_event_loop(&mut terminal, &mut app, &preview);

    let _ = terminal.clear();
    if let Some(col) = origin_col {
        // `clear` leaves the cursor at the viewport's top-left: step back to where the caller was.
        let _ = crossterm::execute!(
            terminal.backend_mut(),
            crossterm::cursor::MoveUp(1),
            crossterm::cursor::MoveToColumn(col)
        );
    }
    let _ = terminal::disable_raw_mode();
    res
}

/// An inline viewport starts at column 0 of the cursor's row, so opening it there paints over
/// the line being edited (e.g. a prompt's `» `, whose blank cells ratatui's diff then skips,
/// leaving `»adb …`). Moves to a fresh line first, like fzf's `--height`, and returns the
/// original column so the cursor can be restored on exit; `None` if nothing was moved.
pub(super) fn open_below_cursor_line(mut tty: &std::fs::File) -> Option<u16> {
    let (col, _) = crossterm::cursor::position().ok()?;
    tty.write_all(b"\r\n").ok()?;
    tty.flush().ok()?;
    Some(col)
}

pub(super) struct PreviewChannel {
    requests: Sender<PreviewRequest>,
    results: Receiver<(u64, Vec<String>)>,
}

/// Runs preview commands on a background thread so slow previews never block typing.
/// Queued requests collapse to the most recent one.
pub(super) fn spawn_preview_worker(
    runner: Arc<dyn GeneratorRunner>,
    cwd: PathBuf,
    env: HashMap<String, String>,
) -> PreviewChannel {
    let (req_tx, req_rx) = mpsc::channel::<PreviewRequest>();
    let (res_tx, res_rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(mut req) = req_rx.recv() {
            while let Ok(newer) = req_rx.try_recv() {
                req = newer;
            }
            let lines = runner.run_shell(&req.command, &cwd, &env, &req.params, PREVIEW_TIMEOUT);
            if res_tx.send((req.id, lines)).is_err() {
                break;
            }
        }
    });
    PreviewChannel {
        requests: req_tx,
        results: res_rx,
    }
}

/// crossterm writes terminal queries (e.g. the cursor-position request used by inline
/// viewports) to stdout; when stdout is captured (`picked=$(ezlo pick)`) they never reach the
/// terminal. Points fd 1 at the tty for the TUI's lifetime and restores it on drop.
pub(super) struct StdoutToTty {
    saved: Option<std::os::fd::RawFd>,
}

impl StdoutToTty {
    pub(super) fn new(tty: &std::fs::File) -> Result<Self> {
        use std::os::fd::AsRawFd;
        if terminal::is_raw_mode_enabled().unwrap_or(false) || nix::unistd::isatty(1).unwrap_or(false) {
            return Ok(Self { saved: None });
        }
        let _ = io::stdout().flush();
        let saved = nix::unistd::dup(1)?;
        nix::unistd::dup2(tty.as_raw_fd(), 1)?;
        Ok(Self { saved: Some(saved) })
    }
}

impl Drop for StdoutToTty {
    fn drop(&mut self) {
        let Some(saved) = self.saved else { return };
        let _ = io::stdout().flush();
        let _ = nix::unistd::dup2(saved, 1);
        let _ = nix::unistd::close(saved);
    }
}

pub(super) fn run_event_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut PickApp<'_>,
    preview: &PreviewChannel,
) -> Result<PickOutcome> {
    loop {
        if let Some(req) = app.take_preview_request(false) {
            let _ = preview.requests.send(req);
        }
        while let Ok((id, lines)) = preview.results.try_recv() {
            app.apply_preview(id, &lines);
        }
        terminal.draw(|f| draw_ui(f, app))?;

        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        // Drain every queued key before redrawing, so fast typing doesn't render each step.
        loop {
            if let Some(outcome) = handle_event(app, event::read()?) {
                return Ok(outcome);
            }
            if !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

pub(super) fn handle_event(app: &mut PickApp<'_>, ev: Event) -> Option<PickOutcome> {
    let Event::Key(key) = ev else {
        return None;
    };
    if key.kind == KeyEventKind::Release {
        return None;
    }
    app.handle_key(key)
}

pub fn print_or_run_selected(cmd: &str, run: bool) -> Result<()> {
    if run {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .status()?;
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
    } else {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{cmd}")?;
        stdout.flush()?;
    }
    Ok(())
}
