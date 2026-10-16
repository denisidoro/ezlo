mod ansi;
mod app;
mod keys;
mod render;
mod run;
mod search;
mod search_render;
mod shell_colors;

pub use ansi::{ansi_to_text, sanitize_preview_ansi};
pub use run::{print_or_run_selected, run_pick_interactive};
pub use search::{run_search_interactive, SearchApp, SearchOptions, SearchOutcome};

// `run.rs` draws via `render::draw_ui` (siblings see each other through `use super::*`).
use render::*;
use search_render::*;
use shell_colors::literal_spans;

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use unicode_width::UnicodeWidthStr;

use crate::engine::generator::{GeneratorCache, GeneratorRunner};
use crate::engine::line::{parse_command_line, Aliases};
use crate::engine::rank::layout::{layout_rows_capped, scroll_thumb, RowLayout};
#[cfg(test)]
use crate::engine::rank::layout::layout_rows;
use crate::engine::rank::match_query;
use crate::engine::candidate::{Candidate};
use crate::engine::recipe::{
    candidates_for_placeholder, resolve_recipe_placeholders, search_recipes,
    substitute_recipe_template, ResolvedPlaceholder,
};
use crate::engine::walker::walk_nested_command_line;
use crate::spec::ast::RecipeDecl;
use crate::spec::SpecIndex;

pub const EXIT_CODE_ABORT: i32 = 130;
/// Floor for generator cache TTLs during a `ezlo pick` session, so keystrokes only re-filter
/// rows that were already fetched. Ctrl-R forces a refresh.
pub const PICK_GENERATOR_MIN_TTL: Duration = Duration::from_secs(300);

const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(80);
const PREVIEW_TIMEOUT: Duration = Duration::from_millis(500);
/// Header lines (template + prompt) plus the list's top and bottom borders.
const CHROME_ROWS: u16 = 4;
const DEFAULT_PAGE_ROWS: usize = 10;

#[derive(Debug, Clone)]
pub struct PickOptions {
    pub line: Option<String>,
    pub cursor: Option<usize>,
    pub query: Option<String>,
    pub recipe_id: Option<String>,
    pub run: bool,
    /// Suggestions visible at once; `None` sizes the picker to ~40% of the terminal.
    pub rows: Option<usize>,
    pub cwd: std::path::PathBuf,
    pub env: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub wrappers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickOutcome {
    Selected(String),
    Aborted,
}

/// A preview command to run off the UI thread; `id` lets stale results be discarded.
#[derive(Debug, Clone)]
pub struct PreviewRequest {
    pub id: u64,
    pub command: String,
    pub params: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    SelectRecipe,
    FillPlaceholder(usize),
}

pub struct PickApp<'a> {
    index: &'a SpecIndex,
    runner: &'a dyn GeneratorRunner,
    cache: &'a GeneratorCache,
    cwd: &'a Path,
    env: &'a HashMap<String, String>,
    scope_prefix: Vec<String>,
    recipe_usage: HashMap<String, u64>,
    stage: Stage,
    started_at_stage2: bool,
    page_rows: usize,
    list_offset: Cell<usize>,

    // Stage 1 state
    recipe_query: String,
    filtered_recipes: Vec<(usize, RecipeDecl)>,
    recipe_selected: usize,

    // Stage 2 state
    active_recipe: Option<RecipeDecl>,
    placeholders: Vec<ResolvedPlaceholder>,
    filled_values: HashMap<String, Vec<String>>,
    placeholder_query: String,
    placeholder_candidates: Vec<Candidate>,
    placeholder_selected: usize,
    multi_selected: Vec<String>,

    // Preview state (debounced, fetched asynchronously by the event loop)
    preview_text: Option<String>,
    /// `preview_text` with SGR sequences converted to styles (parsed once per result).
    preview_rendered: Option<Text<'static>>,
    preview_dirty_since: Option<Instant>,
    preview_last_key: Option<(String, String)>,
    preview_seq: u64,
    /// Lines scrolled past at the top of the preview (Shift/Alt-Up/Down, Shift-PgUp/PgDn).
    preview_scroll: u16,
}

/// Total inline viewport height: `rows` suggestions plus chrome, or ~40% of the terminal.
pub fn pick_viewport_height(term_rows: u16, rows: Option<usize>) -> u16 {
    let auto = ((u32::from(term_rows) * 40) / 100).clamp(8, 20) as u16;
    let wanted = rows.map_or(auto, |r| {
        u16::try_from(r).unwrap_or(u16::MAX).saturating_add(CHROME_ROWS)
    });
    wanted.clamp(CHROME_ROWS + 1, term_rows.max(CHROME_ROWS + 1))
}
