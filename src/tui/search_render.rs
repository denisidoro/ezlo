//! Drawing the history search viewport: a details line (directory, age, exit status of the
//! selected entry), the `history ❯ query` prompt and the bordered list of matching lines.

use super::search::SearchApp;
use super::*;
use crate::engine::rank::layout::truncate_to_width;
use crate::engine::types::format::format_age;
use crate::history::search::{SearchEntry, SearchTerms};

pub(super) fn draw_search(f: &mut Frame<'_>, app: &SearchApp<'_>) {
    let [info_area, prompt_area, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(f.area());

    if let Some(e) = app.selected_entry() {
        f.render_widget(Paragraph::new(details_line(e, app.opts.home.as_deref(), app.opts.now_ts)), info_area);
    }
    let status = list_status(app.selected, app.match_count(), 0);
    draw_prompt(f, prompt_area, app.scope().label(), app.query(), &status);
    draw_rows(f, body_area, app);
}

/// ` ~/src/app · 2h ago · exit 1` (directory and status only when ezlo recorded the run).
fn details_line(e: &SearchEntry, home: Option<&str>, now_ts: i64) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    if let Some(cwd) = &e.cwd {
        spans.push(Span::styled(shorten_home(cwd, home), Style::default().fg(Color::Blue)));
        spans.push(Span::styled(" · ", muted_style()));
    }
    spans.push(Span::styled(age(e.ts, now_ts), muted_style()));
    if let Some(code) = e.exit.filter(|c| *c != 0) {
        spans.push(Span::styled(" · ", muted_style()));
        spans.push(Span::styled(format!("exit {code}"), Style::default().fg(Color::Red)));
    }
    Line::from(spans)
}

fn shorten_home(path: &str, home: Option<&str>) -> String {
    match home.filter(|h| !h.is_empty()).and_then(|h| path.strip_prefix(h)) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

fn age(ts: i64, now_ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    format_age(Duration::from_secs(u64::try_from(now_ts.saturating_sub(ts)).unwrap_or(0)))
}

/// Only the visible window is turned into list items (histories have tens of thousands of lines).
fn draw_rows(f: &mut Frame<'_>, area: Rect, app: &SearchApp<'_>) {
    let win = usize::from(area.height.saturating_sub(2)).max(1);
    let total = app.match_count();
    let mut offset = app.offset.get().min(total.saturating_sub(win));
    if app.selected < offset {
        offset = app.selected;
    } else if app.selected >= offset + win {
        offset = app.selected + 1 - win;
    }
    app.offset.set(offset);

    let inner = usize::from(area.width.saturating_sub(2));
    let items: Vec<ListItem<'_>> = (offset..total.min(offset + win))
        .filter_map(|row| Some((row, app.entry(row)?)))
        .map(|(row, e)| {
            let selected = row == app.selected;
            row_item(entry_line(e, &app.terms, inner, selected, app.opts.now_ts), selected)
        })
        .collect();
    let mut state = ListState::default().with_selected((total > 0).then(|| app.selected - offset));
    f.render_stateful_widget(List::new(items).block(bordered()), area, &mut state);
    draw_scroll_thumb(f, area, total, offset);
}

/// `▸ git push --force          ✗ 2h ago`: the line (newlines shown as `↵`) colored like a shell
/// line (as `ezlo pick` templates) with the query terms highlighted on top, cut to fit, and its
/// age right-aligned (a red `✗` when it last failed).
fn entry_line(e: &SearchEntry, terms: &SearchTerms, width: usize, selected: bool, now_ts: i64) -> Line<'static> {
    let base = if selected { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
    let hl = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let failed = e.exit.is_some_and(|c| c != 0);
    let age = age(e.ts, now_ts);
    let right_w = age.width() + if failed { 2 } else { 0 } + usize::from(!age.is_empty() || failed);
    let left_w = width.saturating_sub(2 + right_w);

    let flat: String = e.line.chars().map(|c| match c { '\n' => '↵', '\t' => ' ', c => c }).collect();
    let (text, text_w) = truncate_to_width(&flat, left_w);
    let marker = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(if selected { "▸ " } else { "  " }, marker)];
    spans.extend(highlight_ranges(literal_spans(&text, true), &terms.ranges(&text), base, hl));
    spans.push(Span::raw(" ".repeat(left_w.saturating_sub(text_w) + usize::from(right_w > 0))));
    if failed {
        spans.push(Span::styled("✗ ", Style::default().fg(Color::Red)));
    }
    spans.push(Span::styled(age, muted_style()));
    Line::from(spans)
}

/// Layers `base` on the syntax-colored `spans`, then `hl` on the byte `ranges` (sorted, disjoint).
fn highlight_ranges(spans: Vec<Span<'_>>, ranges: &[(usize, usize)], base: Style, hl: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut ranges = ranges.iter().peekable();
    let mut at = 0;
    for span in spans {
        let style = base.patch(span.style);
        let text = span.content.as_ref();
        let end = at + text.len();
        let mut cut = at;
        while cut < end {
            while ranges.next_if(|r| r.1 <= cut).is_some() {}
            let (next, is_hl) = match ranges.peek() {
                Some(&&(s, e)) if s <= cut => (e.min(end), true),
                Some(&&(s, _)) => (s.min(end), false),
                None => (end, false),
            };
            if let Some(piece) = text.get(cut - at..next - at) {
                out.push(Span::styled(piece.to_owned(), if is_hl { style.patch(hl) } else { style }));
            }
            cut = next;
        }
        at = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::search::SearchOptions;
    use ratatui::backend::TestBackend;

    fn screen(app: &SearchApp<'_>, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| draw_search(f, app)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect::<String>())
            .collect()
    }

    #[test]
    fn renders_details_prompt_rows_and_failure_marks() {
        let entries = vec![
            SearchEntry::new("make test".into(), 100_000 - 7_200, Some("/home/me/src".into()), Some(2)),
            SearchEntry::new("echo a\necho b".into(), 0, None, None),
        ];
        let opts = SearchOptions {
            query: "e".into(),
            home: Some("/home/me".into()),
            now_ts: 100_000,
            ..Default::default()
        };
        let app = SearchApp::new(&entries, &opts);
        let rows = screen(&app, 40, 6);
        assert_eq!(rows[0].trim_end(), " ~/src · 2h ago · exit 2");
        assert!(rows[1].starts_with(" history ❯ e"), "{rows:?}");
        assert!(rows[1].trim_end().ends_with("1/2"), "{rows:?}");
        assert!(rows[3].starts_with("│▸ make test") && rows[3].trim_end().ends_with("✗ 2h ago│"), "{rows:?}");
        assert!(rows[4].starts_with("│  echo a↵echo b"), "{rows:?}");
        assert_eq!(shorten_home("/home/me", Some("/home/me")), "~");

        let line = entry_line(&entries[0], &SearchTerms::parse("es"), 30, false, 100_000);
        let styled: Vec<(&str, Option<Color>, bool)> = line
            .spans
            .iter()
            .take(5)
            .map(|s| (s.content.as_ref(), s.style.fg, s.style.add_modifier.contains(Modifier::UNDERLINED)))
            .collect();
        assert_eq!(
            styled,
            [
                ("  ", Some(Color::Cyan), false),
                ("make", Some(Color::Blue), false),
                (" ", None, false),
                ("t", None, false),
                ("es", Some(Color::Yellow), true),
            ]
        );
        assert_eq!(shorten_home("/home/meow", Some("/home/me")), "/home/meow");
    }
}
