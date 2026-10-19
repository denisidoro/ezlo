//! Drawing the inline picker viewport.

use super::*;

pub(super) fn border_style() -> Style {
    Style::default().fg(Color::Indexed(238))
}

pub(super) fn muted_style() -> Style {
    Style::default().fg(Color::Indexed(244))
}

pub(super) fn placeholder_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn bordered() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style())
}

pub(super) fn draw_ui(f: &mut Frame<'_>, app: &PickApp<'_>) {
    let [template_area, prompt_area, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(f.area());

    match app.stage {
        Stage::SelectRecipe => {
            let no_fill = HashMap::new();
            if let Some((_, r)) = app.filtered_recipes.get(app.recipe_selected) {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(render_template_spans(&r.template, &no_fill, "", ""));
                f.render_widget(Paragraph::new(Line::from(spans)), template_area);
            }
            let status = list_status(app.recipe_selected, app.filtered_recipes.len(), 0);
            draw_prompt(f, prompt_area, "recipe", &app.recipe_query, &status);
            draw_recipe_list(f, body_area, app, &no_fill);
        }
        Stage::FillPlaceholder(idx) => {
            let current_ph = app.placeholders.get(idx);
            let current_name = current_ph.map(|p| p.name.as_str()).unwrap_or("");
            let template = app
                .active_recipe
                .as_ref()
                .map(|r| r.template.as_str())
                .unwrap_or("");

            let mut spans = vec![Span::raw(" ")];
            spans.extend(render_template_spans(
                template,
                &app.filled_values,
                current_name,
                &app.placeholder_query,
            ));
            f.render_widget(Paragraph::new(Line::from(spans)), template_area);

            let status = list_status(
                app.placeholder_selected,
                app.placeholder_candidates.len(),
                app.multi_selected.len(),
            );
            draw_prompt(f, prompt_area, current_name, &app.placeholder_query, &status);

            let has_preview = app
                .placeholder_candidates
                .iter()
                .any(|c| c.preview_cmd.is_some());
            let (list_area, preview_area) = if has_preview {
                let [l, p] = Layout::horizontal([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .areas(body_area);
                (l, Some(p))
            } else {
                (body_area, None)
            };

            draw_candidate_list(f, list_area, app);

            if let Some(area) = preview_area {
                draw_preview(f, area, app);
            }
        }
    }
}

/// The selected candidate's preview, scrolled by `preview_scroll`; the title shows the position
/// and the scroll keys once the text doesn't fit.
pub(super) fn draw_preview(f: &mut Frame<'_>, area: Rect, app: &PickApp<'_>) {
    let text = app.preview_rendered.clone().unwrap_or_default();
    let total = text.lines.len();
    let visible = usize::from(area.height.saturating_sub(2));
    let mut title = vec![Span::styled(" Preview ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))];
    if total > visible {
        let first = usize::from(app.preview_scroll) + 1;
        let last = (first + visible - 1).min(total);
        title.push(Span::styled(format!("{first}-{last}/{total} "), muted_style()));
        title.push(Span::styled("⇅ Shift-↑↓ ", Style::default().fg(Color::Indexed(240))));
    }
    let widget = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((app.preview_scroll, 0))
        .block(bordered().title(Line::from(title)));
    f.render_widget(widget, area);
}

pub(super) fn list_status(selected: usize, total: usize, marked: usize) -> String {
    let pos = if total == 0 { 0 } else { selected + 1 };
    if marked > 0 {
        format!("{marked} marked · {pos}/{total} ")
    } else {
        format!("{pos}/{total} ")
    }
}

/// Input line: `label ❯ query` with the terminal cursor after the query and a right-aligned status.
pub(super) fn draw_prompt(f: &mut Frame<'_>, area: Rect, label: &str, query: &str, status: &str) {
    f.render_widget(
        Paragraph::new(Span::styled(status.to_string(), muted_style())).alignment(Alignment::Right),
        area,
    );

    const ARROW: &str = " ❯ ";
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(label.to_string(), placeholder_style()),
        Span::styled(ARROW, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(
            query.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);

    let cursor_x = 1 + label.width() + ARROW.width() + query.width();
    let max_x = usize::from(area.width.saturating_sub(1));
    let x = area.x + u16::try_from(cursor_x.min(max_x)).unwrap_or(0);
    f.set_cursor_position(Position::new(x, area.y));
}

pub(super) fn draw_recipe_list(
    f: &mut Frame<'_>,
    area: Rect,
    app: &PickApp<'_>,
    no_fill: &HashMap<String, Vec<String>>,
) {
    let desc_w = column_width(
        app.filtered_recipes.iter().map(|(_, r)| r.desc.as_str()),
        area.width,
    );
    let items: Vec<ListItem<'_>> = app
        .filtered_recipes
        .iter()
        .enumerate()
        .map(|(idx, (_, r))| {
            let selected = idx == app.recipe_selected;
            let prefix = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let hl = style.fg(Color::Yellow).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            let marker = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
            let mut spans = vec![Span::styled(prefix, marker)];
            spans.extend(highlight_matches(
                pad_to(&r.desc, desc_w),
                &app.recipe_query,
                style,
                hl,
            ));
            spans.push(Span::raw("  "));
            spans.extend(render_template_spans(&r.template, no_fill, "", ""));
            row_item(Line::from(spans), selected)
        })
        .collect();
    draw_list(f, area, app, items);
}

/// Candidate rows use the zsh dropdown's layout (`engine::rank::layout::layout_rows`): icon,
/// label with match highlights, and right-anchored data columns or a description, with the same
/// kind, file-type and status colors. The row's leading two cells hold the selection marker.
pub(super) fn draw_candidate_list(f: &mut Frame<'_>, area: Rect, app: &PickApp<'_>) {
    // Inside the list borders; the label column is capped relative to the whole window.
    let inner = usize::from(area.width.saturating_sub(2));
    let max_label = usize::from(f.area().width) * PICK_LABEL_PERCENT / 100;
    let layouts = layout_rows_capped(&app.placeholder_candidates, inner, max_label, Glyphs::from_env(app.env));
    let items: Vec<ListItem<'_>> = app
        .placeholder_candidates
        .iter()
        .zip(layouts)
        .enumerate()
        .map(|(i, (c, row))| {
            let selected = i == app.placeholder_selected;
            let checked = app.multi_selected.contains(&c.insert_value);
            let marker = if checked {
                Some('✓')
            } else if selected {
                Some('▸')
            } else {
                None
            };
            row_item(row_line(&row, marker, selected), selected)
        })
        .collect();
    draw_list(f, area, app, items);
}

/// Converts a shared `RowLayout` into a styled line: the zsh style ranges are layered per char
/// (later ranges patch earlier ones, like `region_highlight`) and adjacent equal styles merged.
/// `marker` replaces the row's first cell (the layout's leading `  ` is unstyled padding).
pub(super) fn row_line(row: &RowLayout, marker: Option<char>, selected: bool) -> Line<'static> {
    let chars: Vec<char> = row.text.chars().collect();
    let mut styles = vec![Style::default(); chars.len()];
    for &(start, end, spec) in &row.styles {
        let style = style_from_zsh(spec);
        for s in styles.iter_mut().take(end.min(chars.len())).skip(start) {
            *s = s.patch(style);
        }
    }
    if selected {
        for s in &mut styles {
            *s = s.add_modifier(Modifier::BOLD);
        }
    }
    let mut spans = Vec::new();
    let mut skip = 0;
    if let Some(m) = marker {
        let marker_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        spans.push(Span::styled(format!("{m} "), marker_style));
        skip = 2.min(chars.len());
    }
    let mut run = String::new();
    let mut run_style = None;
    for (ch, st) in chars.iter().zip(&styles).skip(skip) {
        if run_style.is_some_and(|r| r != *st) {
            spans.push(Span::styled(std::mem::take(&mut run), run_style.unwrap_or_default()));
        }
        run_style = Some(*st);
        run.push(*ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style.unwrap_or_default()));
    }
    Line::from(spans)
}

/// Parses a zsh `region_highlight` style (`fg=blue,bold`, `fg=208`, `bold,underline`).
pub(super) fn style_from_zsh(spec: &str) -> Style {
    fn color(v: &str) -> Option<Color> {
        Some(match v {
            "default" => Color::Reset,
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            n => Color::Indexed(n.parse().ok()?),
        })
    }
    spec.split(',').fold(Style::default(), |st, attr| match attr.trim() {
        "bold" => st.add_modifier(Modifier::BOLD),
        "underline" => st.add_modifier(Modifier::UNDERLINED),
        "italic" => st.add_modifier(Modifier::ITALIC),
        "standout" | "reverse" => st.add_modifier(Modifier::REVERSED),
        a => {
            if let Some(c) = a.strip_prefix("fg=").and_then(color) {
                st.fg(c)
            } else if let Some(c) = a.strip_prefix("bg=").and_then(color) {
                st.bg(c)
            } else {
                st
            }
        }
    })
}

pub(super) fn row_item(line: Line<'_>, selected: bool) -> ListItem<'_> {
    let item = ListItem::new(line);
    if selected {
        item.style(Style::default().bg(Color::Indexed(236)))
    } else {
        item
    }
}

/// Renders a scrolling list that keeps the selection visible, persisting the scroll offset.
pub(super) fn draw_list(f: &mut Frame<'_>, area: Rect, app: &PickApp<'_>, items: Vec<ListItem<'_>>) {
    let selected = (!items.is_empty()).then(|| app.selected_index());
    let mut state = ListState::default()
        .with_offset(app.list_offset.get())
        .with_selected(selected);
    let total = items.len();
    f.render_stateful_widget(List::new(items).block(bordered()), area, &mut state);
    app.list_offset.set(state.offset());
    draw_scroll_thumb(f, area, total, state.offset());
}

/// Overdraws the list's right border with the zsh dropdown's 1-cell scrollbar thumb
/// (`engine::rank::layout::scroll_thumb`) when the rows don't fit.
pub(super) fn draw_scroll_thumb(f: &mut Frame<'_>, area: Rect, total: usize, offset: usize) {
    let win = usize::from(area.height.saturating_sub(2));
    let Some((start, len)) = scroll_thumb(total, win, offset) else { return };
    let x = area.right().saturating_sub(1);
    let style = Style::default().fg(Color::Indexed(8));
    for y in (start..start + len).filter_map(|r| u16::try_from(r).ok()) {
        if let Some(cell) = f.buffer_mut().cell_mut((x, area.y + 1 + y)) {
            cell.set_symbol("┃").set_style(style);
        }
    }
}

/// Splits `text` into spans, styling the characters matched by `query` with `hl`.
pub(super) fn highlight_matches(text: String, query: &str, base: Style, hl: Style) -> Vec<Span<'static>> {
    let Some((_, indices)) = match_query(query, &text).filter(|(_, idx)| !idx.is_empty()) else {
        return vec![Span::styled(text, base)];
    };
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hl = false;
    let mut next = indices.iter().peekable();
    for (ci, ch) in text.chars().enumerate() {
        let is_hl = next.peek().is_some_and(|&&i| i == ci);
        if is_hl {
            next.next();
        }
        if is_hl != run_hl && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), if run_hl { hl } else { base }));
        }
        run_hl = is_hl;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hl { hl } else { base }));
    }
    spans
}

/// Shared width for the first column: the widest entry, capped at half the area.
pub(super) fn column_width<'s>(values: impl Iterator<Item = &'s str>, area_width: u16) -> usize {
    let cap = (area_width as usize / 2).max(10);
    values
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
        .min(cap)
}

pub(super) fn pad_to(s: &str, width: usize) -> String {
    let (t, w) = crate::engine::rank::layout::truncate_to_width(s, width);
    format!("{t}{}", " ".repeat(width.saturating_sub(w)))
}

/// Widest candidate label in `ezlo pick`, as a share of the terminal width (longer ones are cut).
pub(super) const PICK_LABEL_PERCENT: usize = 35;

/// Renders a recipe template: literal text colored like a shell line, filled values in green,
/// the current placeholder (replaced by the live `query` once typing starts) highlighted, and
/// pending placeholders in magenta.
pub(super) fn render_template_spans<'a>(
    template: &'a str,
    filled: &'a HashMap<String, Vec<String>>,
    current_ph: &'a str,
    query: &'a str,
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'>' && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'>' && j > i + 1 {
                if literal_start < i {
                    spans.extend(literal_spans(&template[literal_start..i], literal_start == 0));
                }
                let raw = &template[i + 1..j];
                let name = raw.strip_suffix("...").unwrap_or(raw);
                if let Some(vals) = filled.get(name) {
                    spans.push(Span::styled(
                        vals.join(" "),
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if name == current_ph {
                    let text = if query.is_empty() { &template[i..=j] } else { query };
                    spans.push(Span::styled(
                        text,
                        placeholder_style().add_modifier(Modifier::UNDERLINED),
                    ));
                } else {
                    spans.push(Span::styled(&template[i..=j], Style::default().fg(Color::Magenta)));
                }
                i = j + 1;
                literal_start = i;
                continue;
            }
        }
        i += 1;
    }
    if literal_start < template.len() {
        spans.extend(literal_spans(&template[literal_start..], literal_start == 0));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::candidate::CandidateKind;
    use crate::engine::generator::FakeGeneratorRunner;
    use crate::engine::icons;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn cand(label: &str, kind: CandidateKind) -> Candidate {
        Candidate {
            label: label.to_string(),
            insert_value: label.to_string(),
            kind,
            ..Default::default()
        }
    }

    /// Draws `cands` through `draw_candidate_list` into a `width`×`height` test terminal.
    fn render(cands: Vec<Candidate>, width: u16, height: u16) -> Buffer {
        render_marked(cands, Vec::new(), width, height)
    }

    fn render_marked(cands: Vec<Candidate>, marked: Vec<String>, width: u16, height: u16) -> Buffer {
        let index = SpecIndex::default();
        let runner = FakeGeneratorRunner::new();
        let cache = GeneratorCache::new();
        let opts = PickOptions {
            line: None,
            cursor: None,
            query: None,
            recipe_id: None,
            run: false,
            rows: None,
            cwd: PathBuf::from("/"),
            env: HashMap::from([(icons::NERD_FONT_VAR.to_owned(), "1".to_owned())]),
            aliases: HashMap::new(),
            wrappers: Vec::new(),
        };
        let mut app = PickApp::new(&index, &runner, &cache, &opts);
        app.stage = Stage::FillPlaceholder(0);
        app.placeholder_candidates = cands;
        app.multi_selected = marked;
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| draw_candidate_list(f, f.area(), &app)).unwrap();
        term.backend().buffer().clone()
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn find(buf: &Buffer, y: u16, needle: &str) -> u16 {
        let text = row_text(buf, y);
        let byte = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {text:?}"));
        u16::try_from(text[..byte].chars().count()).unwrap()
    }

    #[test]
    fn candidate_rows_match_the_zsh_dropdown_layout() {
        let mut web = cand("web", CandidateKind::Arg);
        web.columns = vec!["nginx:latest".into(), "running".into()];
        let mut db = cand("database", CandidateKind::Arg);
        db.columns = vec!["postgres".into(), "exited (0)".into()];
        db.match_indices = vec![0];
        let cands = vec![web, db];
        let buf = render(cands.clone(), 60, 4);

        // Same text as the zsh dropdown row (inside the borders, marker in the first cell).
        let zsh = layout_rows(&cands, 58, Glyphs::Nerd);
        let inner: String = row_text(&buf, 2).chars().skip(1).take(58).collect();
        let body = |s: &str| s.chars().skip(2).collect::<String>();
        assert_eq!(body(&inner), body(&zsh[1].text));
        assert!(row_text(&buf, 1).starts_with("│▸ "), "{:?}", row_text(&buf, 1));

        // Nerd Font icon, then the label.
        assert_eq!(buf[(3, 1)].symbol(), icons::ARG.to_string());
        assert_eq!(buf[(3, 1)].fg, Color::Green);
        // Right-anchored, aligned data columns: the trailing column ends at the right border.
        assert_eq!(find(&buf, 1, "nginx"), find(&buf, 2, "postgres"));
        assert!(row_text(&buf, 1).ends_with("running│"));
        assert!(row_text(&buf, 2).ends_with("exited (0)│"));
        // Semantic status colors and the slot color of the first data column.
        assert_eq!(buf[(find(&buf, 1, "running"), 1)].fg, Color::Green);
        assert_eq!(buf[(find(&buf, 2, "exited"), 2)].fg, Color::Indexed(8));
        assert_eq!(buf[(find(&buf, 1, "nginx"), 1)].fg, Color::Reset);
        // Fuzzy match highlight on the label.
        let d = buf[(find(&buf, 2, "database"), 2)].modifier;
        assert!(d.contains(Modifier::BOLD | Modifier::UNDERLINED));
    }

    #[test]
    fn candidate_rows_keep_file_type_and_kind_colors() {
        let mut exe = cand("run.sh", CandidateKind::File);
        exe.style = Some("fg=green,bold");
        exe.icon = Some(icons::EXECUTABLE);
        let dir = cand("src/", CandidateKind::Dir);
        let mut opt = cand("--force", CandidateKind::Opt);
        opt.description = "overwrite".into();
        let buf = render(vec![exe, dir, opt], 50, 5);

        assert_eq!(buf[(3, 1)].symbol(), icons::EXECUTABLE.to_string());
        let label = &buf[(find(&buf, 1, "run.sh"), 1)];
        assert_eq!(label.fg, Color::Green);
        assert!(label.modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(3, 2)].symbol(), icons::DIR.to_string());
        assert_eq!(buf[(find(&buf, 2, "src/"), 2)].fg, Color::Blue);
        assert_eq!(buf[(find(&buf, 3, "--force"), 3)].fg, Color::Yellow);
        assert!(find(&buf, 3, "overwrite") > find(&buf, 3, "--force") + 7);
    }

    #[test]
    fn scrollbar_thumb_overdraws_the_right_border() {
        let cands: Vec<Candidate> = (0..20).map(|i| cand(&format!("c{i}"), CandidateKind::Arg)).collect();
        let buf = render(cands, 30, 7);
        // 5 visible rows of 20 at the top: a 1-cell thumb on the first row's border.
        assert_eq!(buf[(29, 1)].symbol(), "┃");
        assert_eq!(buf[(29, 1)].fg, Color::Indexed(8));
        assert!((2..6).all(|y| buf[(29, y)].symbol() == "│"));

        let few = render(vec![cand("a", CandidateKind::Arg)], 30, 7);
        assert!((1..6).all(|y| few[(29, y)].symbol() == "│"));
    }

    #[test]
    fn marked_rows_show_check_marker() {
        let cands = vec![cand("a", CandidateKind::Arg), cand("b", CandidateKind::Arg)];
        let buf = render_marked(cands, vec!["b".into()], 30, 4);
        assert!(row_text(&buf, 1).starts_with("│▸ "));
        assert!(row_text(&buf, 2).starts_with("│✓ "));
        assert_eq!(buf[(1, 1)].bg, Color::Indexed(236));
    }

    #[test]
    fn zsh_styles_map_to_ratatui() {
        assert_eq!(
            style_from_zsh("fg=blue,bold"),
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
        );
        assert_eq!(style_from_zsh("fg=208"), Style::default().fg(Color::Indexed(208)));
        assert_eq!(style_from_zsh("fg=default"), Style::default().fg(Color::Reset));
        assert_eq!(
            style_from_zsh("bold,underline"),
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        );
        assert_eq!(style_from_zsh("bogus"), Style::default());
    }
}
