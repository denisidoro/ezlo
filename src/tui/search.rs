//! Inline history search (`ezlo history search`, zsh `Ctrl-R`): state, keys and event loop.

use super::keys::{edit_query, move_selection};
use super::run::{open_below_cursor_line, StdoutToTty};
use super::*;
use crate::history::search::{filter_entries, SearchEntry, SearchScope, SearchTerms};

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Initial query (the zsh widget passes the current line).
    pub query: String,
    /// Rows visible at once; `None` sizes the viewport to ~60% of the terminal.
    pub rows: Option<usize>,
    pub cwd: String,
    pub repo_root: Option<String>,
    /// Shown as `~` in the directory line.
    pub home: Option<String>,
    pub now_ts: i64,
}

/// What the user did with the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    /// `Enter`: put the line on the prompt for editing.
    Edit(String),
    /// `Ctrl-O` / `Alt-Enter`: run the line right away.
    Run(String),
    Aborted,
}

pub struct SearchApp<'a> {
    entries: &'a [SearchEntry],
    pub(super) opts: &'a SearchOptions,
    pub(super) query: String,
    pub(super) terms: SearchTerms,
    pub(super) scope: SearchScope,
    /// Indices into `entries` matching the query and scope, newest first.
    pub(super) matches: Vec<u32>,
    pub(super) selected: usize,
    pub(super) page_rows: usize,
    /// First visible row, kept so the selection moves inside the window before it scrolls.
    pub(super) offset: Cell<usize>,
}

impl<'a> SearchApp<'a> {
    pub fn new(entries: &'a [SearchEntry], opts: &'a SearchOptions) -> Self {
        let mut app = Self {
            entries,
            opts,
            query: opts.query.clone(),
            terms: SearchTerms::default(),
            scope: SearchScope::All,
            matches: Vec::new(),
            selected: 0,
            page_rows: opts.rows.unwrap_or(DEFAULT_PAGE_ROWS).max(1),
            offset: Cell::new(0),
        };
        app.refilter();
        app
    }

    fn refilter(&mut self) {
        self.terms = SearchTerms::parse(&self.query);
        self.matches = filter_entries(
            self.entries,
            &self.terms,
            self.scope,
            &self.opts.cwd,
            self.opts.repo_root.as_deref(),
        );
        self.selected = 0;
        self.offset.set(0);
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn scope(&self) -> SearchScope {
        self.scope
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    pub(super) fn entry(&self, row: usize) -> Option<&'a SearchEntry> {
        let idx = usize::try_from(*self.matches.get(row)?).ok()?;
        self.entries.get(idx)
    }

    pub fn selected_entry(&self) -> Option<&'a SearchEntry> {
        self.entry(self.selected)
    }

    fn pick(&self, outcome: fn(String) -> SearchOutcome) -> SearchOutcome {
        self.selected_entry().map_or(SearchOutcome::Aborted, |e| outcome(e.line.clone()))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SearchOutcome> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'g' | 'd') if ctrl => return Some(SearchOutcome::Aborted),
            KeyCode::Esc => return Some(SearchOutcome::Aborted),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => return Some(self.pick(SearchOutcome::Run)),
            KeyCode::Char('o') if ctrl => return Some(self.pick(SearchOutcome::Run)),
            KeyCode::Enter => return Some(self.pick(SearchOutcome::Edit)),
            KeyCode::Char('r') if ctrl => {
                self.scope = self.scope.next(self.opts.repo_root.is_some());
                self.refilter();
                return None;
            }
            _ => {}
        }
        if let Some(sel) = move_selection(self.selected, self.matches.len(), &key, self.page_rows) {
            self.selected = sel;
            return None;
        }
        if edit_query(&mut self.query, &key) {
            self.refilter();
        }
        None
    }
}

pub fn run_search_interactive(entries: &[SearchEntry], opts: &SearchOptions) -> Result<SearchOutcome> {
    let mut app = SearchApp::new(entries, opts);

    let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    // One row is kept for the caller's prompt line, which stays visible above the viewport.
    let viewport_height = search_viewport_height(term_rows.saturating_sub(1), opts.rows);
    app.page_rows = usize::from(viewport_height.saturating_sub(CHROME_ROWS)).max(1);

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

    let res = search_event_loop(&mut terminal, &mut app);

    let _ = terminal.clear();
    if let Some(col) = origin_col {
        let _ = crossterm::execute!(
            terminal.backend_mut(),
            crossterm::cursor::MoveUp(1),
            crossterm::cursor::MoveToColumn(col)
        );
    }
    let _ = terminal::disable_raw_mode();
    res
}

/// Total inline viewport height: `rows` lines plus chrome, or ~60% of the terminal (taller than
/// `ezlo pick`: history is long and scanning it is the point).
pub fn search_viewport_height(term_rows: u16, rows: Option<usize>) -> u16 {
    let auto = ((u32::from(term_rows) * 60) / 100).clamp(12, 40) as u16;
    let wanted = rows.map_or(auto, |r| u16::try_from(r).unwrap_or(u16::MAX).saturating_add(CHROME_ROWS));
    wanted.clamp(CHROME_ROWS + 1, term_rows.max(CHROME_ROWS + 1))
}

fn search_event_loop<W: Write>(terminal: &mut Terminal<CrosstermBackend<W>>, app: &mut SearchApp<'_>) -> Result<SearchOutcome> {
    loop {
        terminal.draw(|f| draw_search(f, app))?;
        // Nothing runs in the background: block until input, then drain queued keys before redrawing.
        loop {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Release {
                    if let Some(outcome) = app.handle_key(key) {
                        return Ok(outcome);
                    }
                }
            }
            if !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn entries() -> Vec<SearchEntry> {
        vec![
            SearchEntry::new("git push".into(), 30, Some("/repo/sub".into()), Some(0)),
            SearchEntry::new("make test".into(), 20, Some("/repo".into()), Some(2)),
            SearchEntry::new("git status".into(), 10, None, None),
        ]
    }

    #[test]
    fn typing_filters_moves_and_enter_picks() {
        let entries = entries();
        let opts = SearchOptions { query: "git".into(), cwd: "/repo".into(), ..Default::default() };
        let mut app = SearchApp::new(&entries, &opts);
        assert_eq!(app.match_count(), 2);
        assert_eq!(app.handle_key(key(KeyCode::Down, KeyModifiers::NONE)), None);
        assert_eq!(app.selected_entry().map(|e| e.line.as_str()), Some("git status"));
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE));
        assert_eq!(app.match_count(), 1);
        assert_eq!(
            app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SearchOutcome::Edit("git push".into()))
        );
    }

    #[test]
    fn viewport_defaults_to_most_of_the_terminal() {
        assert_eq!(search_viewport_height(49, None), 29);
        assert_eq!(search_viewport_height(15, None), 12);
        assert_eq!(search_viewport_height(100, None), 40);
        assert_eq!(search_viewport_height(49, Some(5)), 9);
        assert_eq!(search_viewport_height(8, Some(50)), 8);
    }

    #[test]
    fn ctrl_r_cycles_scope_and_escape_aborts() {
        let entries = entries();
        let opts = SearchOptions { cwd: "/repo".into(), repo_root: Some("/repo".into()), ..Default::default() };
        let mut app = SearchApp::new(&entries, &opts);
        let ctrl_r = key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        app.handle_key(ctrl_r);
        assert_eq!((app.scope(), app.match_count()), (SearchScope::Repo, 2));
        app.handle_key(ctrl_r);
        assert_eq!((app.scope(), app.match_count()), (SearchScope::Dir, 1));
        app.handle_key(ctrl_r);
        assert_eq!((app.scope(), app.match_count()), (SearchScope::All, 3));
        assert_eq!(app.handle_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL)), Some(SearchOutcome::Run("git push".into())));
        assert_eq!(app.handle_key(key(KeyCode::Enter, KeyModifiers::ALT)), Some(SearchOutcome::Run("git push".into())));
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)), Some(SearchOutcome::Aborted));
        assert_eq!(app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)), Some(SearchOutcome::Aborted));
    }
}
