use crate::engine::candidate::{Candidate, CandidateKind};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub struct FormattedRow {
    pub display_text: String,
    pub highlights: String,
    /// Part of the label, description or a column was cut to fit.
    pub truncated: bool,
}

/// A laid-out candidate row: the shared row model behind both the zsh dropdown (`format_rows`)
/// and the `ezlo pick` candidate list (`tui::render::draw_candidate_list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLayout {
    /// Padded row text (`  <icon> <label…>  <details>`), exactly `avail` cells wide.
    pub text: String,
    /// `(start, end, style)` char ranges in paint order: later ranges layer over earlier ones.
    /// Styles use zsh `region_highlight` syntax (`fg=cyan`, `fg=blue,bold`, `bold,underline`).
    pub styles: Vec<(usize, usize, &'static str)>,
    /// Part of the label, description or a column was cut to fit.
    pub truncated: bool,
}

impl RowLayout {
    /// zsh `region_highlight` entries (`start-end:style`, space separated).
    pub fn highlights(&self) -> String {
        let entries: Vec<String> = self
            .styles
            .iter()
            .map(|(s, e, st)| format!("{s}-{e}:{st}"))
            .collect();
        entries.join(" ")
    }
}

const GAP: usize = 2;
/// Gap between data columns (wider than `GAP` so short columns don't run together).
const COL_GAP: usize = 3;
const PREFIX_WIDTH: usize = 4;
const MAX_DATA_COL_WIDTH: usize = 48;
const MIN_LABEL_WIDTH: usize = 12;
/// Labels at most this many cells wider than a group's 75th percentile (or twice it, if larger)
/// still widen the group's label column; wider ones are outliers (see `LabelColumns`).
const OUTLIER_SLACK: usize = 8;
/// Right-edge cell of every zsh dropdown row, reserved for the scrollbar thumb.
const GUTTER: usize = 1;

/// Lays out dropdown rows using the whole terminal width (`  <icon> <label…>  <details>`).
///
/// - Data rows (with `columns`: files, history, generator `show` columns) put their columns in a
///   block anchored to the right edge. Columns are matched from the right and share widths within
///   a `CandidateKind` group (`DataGroups`), and the trailing column is right-aligned, so ages
///   end flush with the edge. A label too long for its group's block gets a block sized to its
///   own values instead of being cut.
/// - Description rows share a label width per `CandidateKind` group (ignoring outliers, see
///   `LabelColumns`) and let the description fill the rest of the row.
///
/// - Rows end with a blank `GUTTER` cell where zsh draws the scrollbar thumb (`scroll_thumb`).
/// - Over-long path/identifier labels are cut in the middle, keeping the leaf (`truncate_label`).
///
/// Highlight ranges are char offsets (what zsh `region_highlight` expects).
pub fn format_rows(cands: &[Candidate], term_cols: usize) -> Vec<FormattedRow> {
    let avail = term_cols.clamp(24, 500).saturating_sub(1);
    layout_rows(cands, avail - GUTTER)
        .into_iter()
        .map(|row| FormattedRow {
            highlights: row.highlights(),
            truncated: row.truncated,
            display_text: row.text + &" ".repeat(GUTTER),
        })
        .collect()
}

/// Lays out `cands` into rows exactly `avail` cells wide (see `format_rows` for the structure).
pub fn layout_rows(cands: &[Candidate], avail: usize) -> Vec<RowLayout> {
    layout_rows_capped(cands, avail, usize::MAX)
}

/// [`layout_rows`] with labels cut at `max_label` cells (`ezlo pick` keeps the first column narrow
/// relative to the whole window, so details stay visible beside a preview).
pub fn layout_rows_capped(cands: &[Candidate], avail: usize, max_label: usize) -> Vec<RowLayout> {
    let body = avail.saturating_sub(PREFIX_WIDTH);
    let max_label = max_label.max(MIN_LABEL_WIDTH);
    let data_groups = DataGroups::new(cands, body);

    let desc_rows = || cands.iter().filter(|c| c.columns.is_empty());
    let any_desc = desc_rows().any(|c| !c.description.is_empty());
    let label_cap = if any_desc { (body * 11 / 20).max(MIN_LABEL_WIDTH) } else { body };
    let label_cols = LabelColumns::new(desc_rows(), any_desc, label_cap.min(max_label).max(1));

    cands
        .iter()
        .map(|c| {
            if c.columns.is_empty() {
                let (col_w, budget) = label_cols.widths(c);
                format_one(c, Details::Description(&c.description), col_w, budget, avail)
            } else {
                let shared = data_groups.get(c.kind);
                let label_w = |cols: &DataColumns| body.saturating_sub(cols.block_width() + GAP).max(1);
                let lw = UnicodeWidthStr::width(c.label.as_str());
                // A label that doesn't fit beside its group's columns gets a block sized to its own
                // values instead (only this row loses the alignment, like description outliers).
                let own;
                let cols = if lw > label_w(shared) {
                    own = DataColumns::new(std::iter::once(c), body);
                    &own
                } else {
                    shared
                };
                let w = label_w(cols).min(max_label);
                format_one(c, Details::Columns(cols), w, w, avail)
            }
        })
        .collect()
}

/// Data columns sized per `CandidateKind` group: history rows don't inherit the widths of, say,
/// container rows with long image names (and vice versa).
struct DataGroups {
    groups: Vec<(CandidateKind, DataColumns)>,
    empty: DataColumns,
}

impl DataGroups {
    fn new(cands: &[Candidate], body: usize) -> Self {
        let mut kinds: Vec<CandidateKind> = Vec::new();
        for c in cands.iter().filter(|c| !c.columns.is_empty()) {
            if !kinds.contains(&c.kind) {
                kinds.push(c.kind);
            }
        }
        let groups = kinds
            .into_iter()
            .map(|k| {
                let rows = cands.iter().filter(move |c| !c.columns.is_empty() && c.kind == k);
                (k, DataColumns::new(rows, body))
            })
            .collect();
        Self { groups, empty: DataColumns { widths: Vec::new() } }
    }

    fn get(&self, kind: CandidateKind) -> &DataColumns {
        self.groups.iter().find(|(k, _)| *k == kind).map_or(&self.empty, |(_, c)| c)
    }
}

/// Label column widths for description rows, sized per `CandidateKind` group (flags, subcommands,
/// recipes, ...) so one long label in a group doesn't push the descriptions of the others right.
/// Within a group, outlier labels (far wider than most) don't widen the column either: their
/// description starts right after the label instead (they are only truncated at `cap`).
struct LabelColumns {
    groups: Vec<(CandidateKind, usize)>,
    cap: usize,
}

impl LabelColumns {
    fn new<'a>(rows: impl Iterator<Item = &'a Candidate>, any_desc: bool, cap: usize) -> Self {
        let mut by_kind: Vec<(CandidateKind, Vec<usize>)> = Vec::new();
        for c in rows {
            let w = UnicodeWidthStr::width(c.label.as_str());
            match by_kind.iter_mut().find(|(k, _)| *k == c.kind) {
                Some((_, ws)) => ws.push(w),
                None => by_kind.push((c.kind, vec![w])),
            }
        }
        let groups = by_kind
            .into_iter()
            .map(|(k, ws)| {
                let w = if any_desc { column_width(ws) } else { ws.into_iter().max().unwrap_or(0) };
                (k, w.clamp(1, cap))
            })
            .collect();
        Self { groups, cap }
    }

    /// `(column width, label budget)` for `c`: descriptions are aligned at the column width;
    /// an outlier label may use up to `cap` cells and pushes only its own description.
    fn widths(&self, c: &Candidate) -> (usize, usize) {
        let col = self
            .groups
            .iter()
            .find(|(k, _)| *k == c.kind)
            .map_or(self.cap, |&(_, w)| w);
        let lw = UnicodeWidthStr::width(c.label.as_str());
        (col, lw.clamp(col, self.cap))
    }
}

/// Column width for a group of label widths, ignoring outliers: labels wider than
/// `p75 + max(p75, OUTLIER_SLACK)` don't widen the column.
fn column_width(mut widths: Vec<usize>) -> usize {
    widths.sort_unstable();
    let Some(&max) = widths.last() else { return 0 };
    let p75 = widths[(widths.len() - 1) * 3 / 4];
    let limit = p75 + p75.max(OUTLIER_SLACK);
    if max <= limit {
        return max;
    }
    widths.into_iter().filter(|&w| w <= limit).max().unwrap_or(max)
}

/// Thumb of a 1-cell scrollbar for a `win`-row window at 0-based `offset` into `total` rows:
/// `(first row, length)` within the window, or `None` when everything fits. The zsh dropdown
/// (`_ezlo_render`) mirrors this formula; `ezlo pick` draws it on the list's right border.
pub fn scroll_thumb(total: usize, win: usize, offset: usize) -> Option<(usize, usize)> {
    if win == 0 || total <= win {
        return None;
    }
    let len = ((win * win + total / 2) / total).clamp(1, win);
    let span = total - win;
    let travel = win - len;
    let start = (offset.min(span) * travel + span / 2) / span;
    Some((start, len))
}

/// Shared column widths for a group of data rows.
struct DataColumns {
    widths: Vec<usize>,
}

impl DataColumns {
    /// Columns are matched from the right (a row's last value always lands in the last slot), so
    /// rows with different column counts still share the trailing columns (e.g. ages).
    fn new<'a>(rows: impl Iterator<Item = &'a Candidate> + Clone, body: usize) -> Self {
        let slots = rows.clone().map(|c| c.columns.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; slots];
        for c in rows {
            let offset = slots - c.columns.len();
            for (j, v) in c.columns.iter().enumerate() {
                let w = UnicodeWidthStr::width(v.as_str()).min(MAX_DATA_COL_WIDTH);
                widths[offset + j] = widths[offset + j].max(w);
            }
        }
        let mut cols = Self { widths };
        cols.shrink_to(body.saturating_sub(MIN_LABEL_WIDTH + GAP));
        cols
    }

    /// The value of `c` in `slot`, if any (see `new`).
    fn value<'c>(&self, c: &'c Candidate, slot: usize) -> &'c str {
        let offset = self.widths.len().saturating_sub(c.columns.len());
        slot.checked_sub(offset)
            .and_then(|j| c.columns.get(j))
            .map_or("", String::as_str)
    }

    fn block_width(&self) -> usize {
        let n = self.widths.iter().filter(|&&w| w > 0).count();
        self.widths.iter().sum::<usize>() + COL_GAP * n.saturating_sub(1)
    }

    /// Narrows the widest columns until the block fits `max`.
    fn shrink_to(&mut self, max: usize) {
        while self.block_width() > max {
            let Some(widest) = self.widths.iter_mut().max() else { return };
            if *widest <= 1 {
                return;
            }
            *widest -= 1;
        }
    }
}

enum Details<'a> {
    Description(&'a str),
    Columns(&'a DataColumns),
}

/// Accumulates row text while tracking both char offsets (for zsh) and display width.
struct RowBuilder {
    text: String,
    chars: usize,
    width: usize,
    hl: Vec<(usize, usize, &'static str)>,
    truncated: bool,
}

impl RowBuilder {
    fn push(&mut self, s: &str, w: usize, style: Option<&'static str>) {
        let start = self.chars;
        self.text.push_str(s);
        self.chars += s.chars().count();
        self.width += w;
        if let Some(st) = style {
            if self.chars > start {
                self.hl.push((start, self.chars, st));
            }
        }
    }

    fn pad_to(&mut self, target_width: usize) {
        let n = target_width.saturating_sub(self.width);
        self.text.extend(std::iter::repeat_n(' ', n));
        self.chars += n;
        self.width += n;
    }
}

/// `col_w` is the shared label column width (where descriptions start); `label_w` (≥ `col_w`)
/// is how much of it this row's label may use before being truncated.
fn format_one(c: &Candidate, details: Details<'_>, col_w: usize, label_w: usize, avail: usize) -> RowLayout {
    let mut row = RowBuilder {
        text: String::with_capacity(avail),
        chars: 0,
        width: 0,
        hl: Vec::new(),
        truncated: false,
    };
    let dim = c.deemphasized.then_some("fg=8");

    let icon = c.icon.unwrap_or_else(|| c.kind.icon());
    row.push("  ", 2, None);
    let icon_style = dim.or(c.style).unwrap_or_else(|| c.kind.color_style());
    let icon_w = UnicodeWidthChar::width(icon).unwrap_or(1);
    row.push(icon.encode_utf8(&mut [0u8; 4]), icon_w, Some(icon_style));
    row.push(" ", 1, None);

    // Over-long labels are truncated (paths in the middle) and shown in full in the status line.
    let label_budget = label_w.min(avail.saturating_sub(row.width));
    let label = truncate_label(c, label_budget);
    row.truncated |= label.width < UnicodeWidthStr::width(c.label.as_str());
    let label_start = row.chars;
    row.push(&label.text, label.width, dim.or(c.style).or_else(|| c.kind.label_style()));
    if dim.is_none() {
        // Match indices refer to the full label: remap them, and mark the `…` when it hides one.
        let mut hidden = false;
        for &m in &c.match_indices {
            match label.char_pos(m) {
                Some(p) => row.hl.push((label_start + p, label_start + p + 1, "bold,underline")),
                None => hidden = true,
            }
        }
        if let Some(p) = label.ellipsis_pos().filter(|_| hidden) {
            row.hl.push((label_start + p, label_start + p + 1, "bold,underline"));
        }
    }

    match details {
        Details::Description(desc) if !desc.is_empty() => {
            let col_end = PREFIX_WIDTH + col_w.min(label_budget);
            row.pad_to((col_end.max(row.width) + GAP).min(avail));
            let (t, tw) = truncate_to_width(desc, avail.saturating_sub(row.width));
            row.truncated |= tw < UnicodeWidthStr::width(desc);
            row.push(&t, tw, dim);
        }
        Details::Description(_) => {}
        Details::Columns(cols) => push_columns(&mut row, c, cols, dim, avail),
    }
    row.pad_to(avail);

    RowLayout {
        text: row.text,
        styles: row.hl,
        truncated: row.truncated,
    }
}

/// Right-anchored data block. The trailing column (ages, typically) is right-aligned so it ends
/// flush with the edge; other values are left-aligned (producers pad numbers themselves).
fn push_columns(row: &mut RowBuilder, c: &Candidate, cols: &DataColumns, dim: Option<&'static str>, avail: usize) {
    let start = avail.saturating_sub(cols.block_width()).max(row.width + GAP).min(avail);
    row.pad_to(start);
    let last = cols.widths.len().saturating_sub(1);
    let mut first = true;
    for (slot, &w) in cols.widths.iter().enumerate().filter(|(_, &w)| w > 0) {
        if !first {
            let at = row.width + COL_GAP;
            row.pad_to(at.min(avail));
        }
        first = false;
        let col_start = row.width;
        let target = w.min(avail.saturating_sub(col_start));
        let value = cols.value(c, slot);
        let (t, tw) = truncate_to_width(value, target);
        row.truncated |= tw < UnicodeWidthStr::width(value);
        if slot == last {
            row.pad_to(col_start + target - tw);
        }
        row.push(&t, tw, dim.or_else(|| column_style(slot, value)));
        row.pad_to(col_start + target);
    }
}

fn column_style(j: usize, value: &str) -> Option<&'static str> {
    if let Some(s) = status_style(value) {
        return Some(s);
    }
    match j {
        0 => None,
        1 => Some("fg=cyan"),
        2 => Some("fg=yellow"),
        _ => Some("fg=magenta"),
    }
}

/// Semantic colors for well-known state words (docker, systemd, ...).
fn status_style(value: &str) -> Option<&'static str> {
    // A failed last run of a history row (`✗ 2`).
    if value.starts_with('✗') {
        return Some("fg=red");
    }
    let first = value.split_whitespace().next()?.to_ascii_lowercase();
    match first.as_str() {
        "running" | "up" | "active" | "healthy" | "started" => Some("fg=green"),
        "paused" | "restarting" | "created" | "starting" | "pending" => Some("fg=yellow"),
        "exited" | "dead" | "stopped" | "inactive" | "removing" => Some("fg=8"),
        "unhealthy" | "failed" | "error" => Some("fg=red"),
        _ => None,
    }
}

pub fn truncate_to_width(s: &str, max_width: usize) -> (String, usize) {
    let t = truncate_end(s, max_width);
    (t.text, t.width)
}

/// Head cells a middle-truncated label keeps at least before its preserved leaf segment.
const MIN_HEAD: usize = 3;
/// Below this budget a middle cut leaves too little on either side; tail truncation is used.
const MIN_MIDDLE_WIDTH: usize = 8;
/// Separators delimiting the leaf segment a middle cut preserves, in priority order.
const LEAF_SEPARATORS: [char; 3] = ['/', ':', '-'];

/// A string cut to a width budget, mapping original char indices to displayed ones (so match
/// highlights survive truncation). Chars `[0, head)` and `[tail_start, len)` are kept; a cut
/// shows `…` between them (at char `head`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncated {
    pub text: String,
    pub width: usize,
    head: usize,
    tail_start: usize,
    cut: bool,
}

impl Truncated {
    /// Displayed char index of the original char `i`, or `None` if it was cut away.
    pub fn char_pos(&self, i: usize) -> Option<usize> {
        if i < self.head {
            Some(i)
        } else if i >= self.tail_start {
            Some(self.head + usize::from(self.cut) + (i - self.tail_start))
        } else {
            None
        }
    }

    /// Displayed char index of the `…`, if anything was cut.
    pub fn ellipsis_pos(&self) -> Option<usize> {
        self.cut.then_some(self.head)
    }
}

/// Tail truncation: `head…`.
fn truncate_end(s: &str, max_width: usize) -> Truncated {
    let len = s.chars().count();
    if max_width == 0 {
        return Truncated { text: String::new(), width: 0, head: 0, tail_start: len, cut: false };
    }
    let total_w = UnicodeWidthStr::width(s);
    if total_w <= max_width {
        return Truncated { text: s.to_string(), width: total_w, head: len, tail_start: len, cut: false };
    }
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    let mut cur_w = 0;
    let mut head = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > budget {
            break;
        }
        out.push(ch);
        cur_w += cw;
        head += 1;
    }
    out.push('…');
    Truncated { text: out, width: cur_w + 1, head, tail_start: len, cut: true }
}

/// Middle truncation for paths and hierarchical identifiers (`head…tail`): the trailing leaf
/// segment (after the last `/`, else `:`, else `-`; a trailing `/` doesn't count) is always kept
/// when it fits next to `MIN_HEAD` head cells, and the tail grows to about half the budget, so
/// candidates sharing a long common prefix stay distinguishable. Falls back to tail truncation
/// for tiny budgets and strings without a separator.
pub fn truncate_middle(s: &str, max_width: usize) -> Truncated {
    if max_width < MIN_MIDDLE_WIDTH || UnicodeWidthStr::width(s) <= max_width {
        return truncate_end(s, max_width);
    }
    let chars: Vec<char> = s.chars().collect();
    let cw = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    let tail_width = |from: usize| chars[from..].iter().map(|&c| cw(c)).sum::<usize>();
    let budget = max_width - 1;
    let Some(leaf_w) = leaf_starts(&chars)
        .map(tail_width)
        .find(|&w| w + MIN_HEAD <= budget)
    else {
        return truncate_end(s, max_width);
    };

    // Tail: at least the leaf, else about half the budget (wide chars may leave it a bit short).
    let target = leaf_w.max(budget / 2);
    let (mut tail_start, mut tail_w) = (chars.len(), 0);
    while tail_start > 0 && tail_w + cw(chars[tail_start - 1]) <= target {
        tail_start -= 1;
        tail_w += cw(chars[tail_start]);
    }
    let (mut head, mut head_w) = (0, 0);
    while head < tail_start && head_w + cw(chars[head]) + tail_w <= budget {
        head_w += cw(chars[head]);
        head += 1;
    }
    let mut text: String = chars[..head].iter().collect();
    text.push('…');
    text.extend(&chars[tail_start..]);
    Truncated { text, width: head_w + 1 + tail_w, head, tail_start, cut: true }
}

/// Char indices where a preserved tail (separator + leaf segment) may start, in
/// `LEAF_SEPARATORS` priority order. A leading separator (`/abs`, `-flag`) and trailing `/`s
/// (directories) don't delimit a leaf.
fn leaf_starts(chars: &[char]) -> impl Iterator<Item = usize> + '_ {
    let mut end = chars.len();
    while end > 1 && chars[end - 1] == '/' {
        end -= 1;
    }
    LEAF_SEPARATORS
        .iter()
        .filter_map(move |&sep| chars.get(1..end)?.iter().rposition(|&c| c == sep).map(|i| i + 1))
}

/// Label truncation for `c`: values that are paths or hierarchical identifiers (files, dirs,
/// branches, containers, ... = `File`/`Dir`/`Arg` rows) are cut in the middle, everything else
/// (flags, subcommands, recipes, history lines) at the end.
fn truncate_label(c: &Candidate, max_width: usize) -> Truncated {
    match c.kind {
        CandidateKind::File | CandidateKind::Dir | CandidateKind::Arg => truncate_middle(&c.label, max_width),
        _ => truncate_end(&c.label, max_width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(label: &str, kind: CandidateKind, desc: &str) -> Candidate {
        Candidate {
            label: label.to_string(),
            insert_value: label.to_string(),
            kind,
            description: desc.to_string(),
            ..Default::default()
        }
    }

    /// Display column where `needle` starts in `row` (icons here are 1 cell wide).
    fn col(row: &RowLayout, needle: &str) -> usize {
        let byte = row.text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {:?}", row.text));
        UnicodeWidthStr::width(&row.text[..byte])
    }

    #[test]
    fn long_label_in_another_group_does_not_push_flag_descriptions() {
        let cands = vec![
            cand("-f", CandidateKind::Opt, "force"),
            cand("--all", CandidateKind::Opt, "everything"),
            cand("deploy {service} --env={env} --tag={tag}", CandidateKind::Recipe, "deploy it"),
        ];
        let rows = layout_rows(&cands, 100);
        // Flags share their own (narrow) column...
        assert_eq!(col(&rows[0], "force"), col(&rows[1], "everything"));
        assert_eq!(col(&rows[1], "everything"), PREFIX_WIDTH + 5 + GAP);
        // ...while the recipe's description sits after its full label.
        assert_eq!(col(&rows[2], "deploy it"), PREFIX_WIDTH + 40 + GAP);
        assert!(rows.iter().all(|r| UnicodeWidthStr::width(r.text.as_str()) == 100));
    }

    #[test]
    fn outlier_label_within_a_group_only_pushes_its_own_description() {
        let mut cands: Vec<Candidate> = ["add", "rm", "ls", "show", "push"]
            .iter()
            .map(|l| cand(l, CandidateKind::Sub, "short"))
            .collect();
        cands.push(cand("generate-completion-scripts", CandidateKind::Sub, "outlier"));
        let rows = layout_rows(&cands, 100);
        let normal = col(&rows[0], "short");
        assert_eq!(normal, PREFIX_WIDTH + 4 + GAP);
        assert!(rows[..5].iter().all(|r| col(r, "short") == normal));
        // Not truncated: the label fits the cap, so it keeps its full text.
        assert!(rows[5].text.contains("generate-completion-scripts"));
        assert_eq!(col(&rows[5], "outlier"), PREFIX_WIDTH + 27 + GAP);
    }

    #[test]
    fn similar_label_widths_share_one_column() {
        let cands = vec![
            cand("-v", CandidateKind::Opt, "a"),
            cand("--verbose", CandidateKind::Opt, "b"),
            cand("--no-verify", CandidateKind::Opt, "c"),
        ];
        let rows = layout_rows(&cands, 80);
        assert_eq!(col(&rows[0], " a "), col(&rows[2], " c "));
    }

    #[test]
    fn over_cap_labels_are_still_truncated() {
        let long = "x".repeat(200);
        let cands = vec![cand("ab", CandidateKind::Sub, "d"), cand(&long, CandidateKind::Sub, "desc")];
        let rows = layout_rows(&cands, 60);
        assert!(rows[1].text.contains('…'));
        assert!(rows[1].text.contains("desc"));
        assert_eq!(UnicodeWidthStr::width(rows[1].text.as_str()), 60);
    }

    #[test]
    fn format_rows_reserve_a_blank_gutter_cell() {
        let cands = vec![cand("--force", CandidateKind::Opt, &"d".repeat(300))];
        let rows = format_rows(&cands, 80);
        assert_eq!(UnicodeWidthStr::width(rows[0].display_text.as_str()), 79);
        assert!(rows[0].display_text.ends_with("… "), "{:?}", rows[0].display_text);
    }

    #[test]
    fn middle_truncation_keeps_the_leaf_segment() {
        let t = truncate_middle("src/engine/rank/layout.rs", 16);
        assert_eq!(t.text, "src/e…/layout.rs");
        assert_eq!(t.width, 16);
        // Directories: the trailing `/` belongs to the leaf.
        assert_eq!(truncate_middle("home/zelda/projects/ezlo/", 14).text, "home/ze…/ezlo/");
        // A long leaf grows the tail past the separator into about half the budget.
        let t = truncate_middle("feature/JIRA-1234-add-retry-logic", 20);
        assert!(t.text.ends_with("-logic"), "{:?}", t.text);
        assert_eq!(t.width, 20);
        // `:` (image tags) and `-` (container replicas) delimit leaves when there is no `/`.
        assert!(truncate_middle("registry.example.com:5000-tag", 12).text.ends_with("-tag"));
        assert!(truncate_middle("myproject_production_worker-12", 12).text.ends_with("-12"));
        // No separator, a tiny budget, or a fitting string: plain tail truncation / unchanged.
        assert_eq!(truncate_middle("abcdefghijklmnop", 10).text, "abcdefghi…");
        assert_eq!(truncate_middle("a/b/cdefgh", 6).text, "a/b/c…");
        assert_eq!(truncate_middle("a/b", 10).text, "a/b");
        // Wide chars never overflow the budget.
        let t = truncate_middle("文書/日本語のファイル名/レポート.txt", 17);
        assert!(t.width <= 17 && t.text.ends_with("レポート.txt"), "{t:?}");
        assert_eq!(UnicodeWidthStr::width(t.text.as_str()), t.width);
    }

    #[test]
    fn truncated_char_positions_skip_the_cut() {
        let t = truncate_middle("src/engine/rank/layout.rs", 16);
        assert_eq!(t.char_pos(0), Some(0));
        assert_eq!(t.char_pos(5), None);
        assert_eq!(t.ellipsis_pos(), Some(5));
        // `l` of `layout.rs` (char 16) lands right after `src/e…/`.
        assert_eq!(t.char_pos(16), Some(7));
        assert_eq!(t.char_pos(24), Some(15));
        let end = truncate_end("abcdef", 4);
        assert_eq!((end.char_pos(2), end.char_pos(3), end.ellipsis_pos()), (Some(2), None, Some(3)));
    }

    #[test]
    fn path_rows_keep_distinct_basenames_and_match_highlights() {
        let prefix = "services/backend/internal/handlers/";
        let mut cands: Vec<Candidate> = ["user_profile.go", "user_session.go"]
            .iter()
            .map(|f| cand(&format!("{prefix}{f}"), CandidateKind::File, ""))
            .collect();
        // Fuzzy match on `s` of `services`, `i` of `internal` and `p` of `profile`.
        let p = prefix.chars().count() + 5;
        cands[0].match_indices = vec![0, 17, p];
        // Flags keep tail truncation.
        cands.push(cand(&format!("--{}", "x-".repeat(30)), CandidateKind::Opt, ""));
        let rows = layout_rows(&cands, 34);
        assert!(rows[0].text.contains("…/user_profile.go"), "{:?}", rows[0].text);
        assert!(rows[1].text.contains("…/user_session.go"), "{:?}", rows[1].text);
        assert!(rows[2].text.trim_end().ends_with('…'), "{:?}", rows[2].text);
        let chars: Vec<char> = rows[0].text.chars().collect();
        let underlined: Vec<char> = rows[0]
            .styles
            .iter()
            .filter(|s| s.2 == "bold,underline")
            .map(|&(s, _, _)| chars[s])
            .collect();
        // `i` of `internal` (char 17) is cut away, so the `…` is marked instead.
        assert_eq!(underlined, vec!['s', 'p', '…']);
        assert!(rows.iter().all(|r| UnicodeWidthStr::width(r.text.as_str()) == 34));
    }

    #[test]
    fn data_columns_are_sized_per_kind() {
        let mut container = cand("homeassistant", CandidateKind::Arg, "");
        container.columns = vec!["Up 3 days (healthy)".into(), "ghcr.io/home-assistant/home-assistant:stable".into()];
        let mut hist = cand("docker stop homeassistant", CandidateKind::History, "");
        hist.columns = vec![String::new(), "2d".into()];
        let rows = layout_rows(&[container, hist], 80);
        // The history row isn't squeezed by the container's wide image column.
        assert!(rows[1].text.contains("docker stop homeassistant"), "{:?}", rows[1].text);
        assert!(rows[1].text.trim_end().ends_with("2d"));
        assert!(rows.iter().all(|r| UnicodeWidthStr::width(r.text.as_str()) == 80));
    }

    #[test]
    fn a_long_label_gets_its_own_column_block() {
        let mut short = cand("a", CandidateKind::Arg, "");
        short.columns = vec!["x".repeat(40), "1d".into()];
        let mut long = cand("a-rather-long-container-name-here", CandidateKind::Arg, "");
        long.columns = vec!["up".into(), "2d".into()];
        let rows = layout_rows(&[short, long], 70);
        assert!(rows[1].text.contains("a-rather-long-container-name-here"), "{:?}", rows[1].text);
        assert!(rows[1].text.trim_end().ends_with("2d"));
    }

    #[test]
    fn capped_layout_cuts_labels() {
        let mut c = cand(&"y".repeat(60), CandidateKind::Arg, "");
        c.columns = vec!["v".into()];
        let rows = layout_rows_capped(&[c], 100, 20);
        assert!(rows[0].text.contains('…'));
        assert!(!rows[0].text.contains(&"y".repeat(20)));
    }

    #[test]
    fn scroll_thumb_tracks_the_window() {
        assert_eq!(scroll_thumb(5, 6, 0), None);
        assert_eq!(scroll_thumb(6, 6, 0), None);
        assert_eq!(scroll_thumb(12, 6, 0), Some((0, 3)));
        assert_eq!(scroll_thumb(12, 6, 6), Some((3, 3)));
        assert_eq!(scroll_thumb(12, 6, 3), Some((2, 3)));
        // Long lists keep a 1-cell thumb that reaches both ends.
        assert_eq!(scroll_thumb(1000, 6, 0), Some((0, 1)));
        assert_eq!(scroll_thumb(1000, 6, 994), Some((5, 1)));
        assert_eq!(scroll_thumb(1000, 6, 5000), Some((5, 1)));
    }
}
