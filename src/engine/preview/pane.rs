//! Candidate preview pane: a compact block of lines describing the *selected dropdown row*,
//! drawn by the zsh client right of the dropdown rows, or below them when the terminal is narrow
//! (`[complete.pane]`, toggled in the Alt-E menu).
//!
//! Two sources:
//!
//! - **type previews**: the `preview `<shell>`` command of the candidate's spec `type`, run with
//!   the row's columns as parameters (as in `ezlo pick`), through the generator runner + cache;
//! - **path previews**: for file / directory candidates, the output of the first installed
//!   `[complete.pane] file_commands` / `dir_commands` entry (`bat`, `eza`; ANSI colors kept as
//!   spans), else built in: the head of a text file (or a size line for binaries) and a packed
//!   listing of a directory.
//!
//! Previews are computed on demand per row (the `PANE` request), never while answering a `Q`, so
//! they cannot slow down typing; rows only carry whether a preview is available.

use super::dryrun::strip_ansi;
use super::sgr::{self, Span};
use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::generator::{CacheKey, CacheLookup, GeneratorCache, GeneratorRunner};
use crate::engine::icons::Glyphs;
use crate::engine::types::format::format_size;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bytes read at most from a file for its head preview.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Directory entries listed at most (the rest is counted).
const MAX_DIR_ENTRIES: usize = 2_000;
/// Type preview output is reused across selections for this long (same command, row and cwd).
const PANE_CACHE_TTL: Duration = Duration::from_secs(5);
/// Preview lines sent to the shell at most; it shows `[complete.pane] lines` of them (more beside a
/// taller dropdown) and scrolls through the rest.
pub const PANE_MAX_LINES: usize = 200;

/// When the pane opens without the pane toggle (Alt-E menu) (`[complete.pane] mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PaneMode {
    /// Every row with a preview when the pane fits beside the dropdown (most rows need one);
    /// only after the pane toggle when it would go below it.
    #[default]
    Auto,
    /// Every row with a preview, path previews included.
    On,
    /// Only after the pane toggle.
    Off,
}

/// Where the pane is drawn relative to the dropdown (`[complete.pane] position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanePosition {
    /// Beside the rows (they are laid out narrower); below them when the terminal is too narrow.
    #[default]
    Right,
    /// Below the dropdown footer.
    Bottom,
}

/// Narrowest dropdown (in cells) left of a right-hand pane; below it the pane goes to the bottom.
const MIN_LIST_WIDTH: usize = 50;
/// Narrowest right-hand pane (in cells).
const MIN_RIGHT_PANE_WIDTH: usize = 24;
/// Cells of the pane's gutter (`    ` below the dropdown, 2 cells plus the right margin beside it; the first holds the scroll thumb).
const PANE_GUTTER: usize = 4;

/// Knobs of the preview pane (from `[complete.pane]` in `config.toml`).
#[derive(Debug, Clone)]
pub struct PaneOptions {
    pub mode: PaneMode,
    /// Visible pane height in lines (beside the rows it grows to the dropdown's height); sent to
    /// the shell with each preview.
    pub lines: usize,
    /// Built-in file head / directory listing previews for path rows.
    pub paths: bool,
    /// Kill timeout of a type preview command.
    pub timeout: Duration,
    /// Terminal width.
    pub width: usize,
    pub position: PanePosition,
    /// Share of the terminal width taken by a right-hand pane, in percent.
    pub split: usize,
    /// Shell commands previewing a text file / directory (`$path`, `$width`, `$icons`); the first whose
    /// program is on `$PATH` wins, none: built in.
    pub file_commands: Vec<String>,
    pub dir_commands: Vec<String>,
    /// Glyph set of the shell (`NERD_FONT`): row icons, and `$icons` (eza `--icons` value).
    pub glyphs: Glyphs,
}

impl Default for PaneOptions {
    fn default() -> Self {
        Self {
            mode: PaneMode::Auto,
            lines: 5,
            paths: true,
            timeout: Duration::from_millis(500),
            width: 80,
            position: PanePosition::Right,
            split: 40,
            file_commands: Vec::new(),
            dir_commands: Vec::new(),
            glyphs: Glyphs::default(),
        }
    }
}

impl PaneOptions {
    /// Terminal column where a right-hand pane starts (the dropdown rows are laid out up to it);
    /// `None` when the pane goes below the dropdown (configured so, or the terminal is too narrow).
    pub fn right_column(&self) -> Option<usize> {
        if self.position != PanePosition::Right {
            return None;
        }
        let pane = self.width * self.split.clamp(10, 90) / 100;
        let list = self.width.saturating_sub(pane);
        (pane >= MIN_RIGHT_PANE_WIDTH && list >= MIN_LIST_WIDTH).then_some(list)
    }

    /// Cells available to a pane line's text (directory listings are packed into it).
    pub fn text_width(&self) -> usize {
        let start = self.right_column().unwrap_or(0);
        self.width.saturating_sub(start + PANE_GUTTER).max(1)
    }
}

/// Whether (and how) a row can be previewed; sent per row so the client knows when to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAvailability {
    None,
    /// Shown after the pane toggle only.
    Manual,
    /// Shown as soon as the row is selected (unless toggled off).
    Auto,
}

impl PaneAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

/// What a pane line is (styled by the client).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneLineKind {
    /// Preview output / file content.
    Text,
    /// A directory entry line.
    Dir,
    /// Metadata (`binary file · 3.2 KB`, `… 12 more`, `(empty)`).
    Note,
}

impl PaneLineKind {
    /// zsh `region_highlight` style of the line (empty: terminal default).
    pub fn zsh_style(self) -> &'static str {
        match self {
            Self::Text => "",
            Self::Dir => "fg=blue",
            Self::Note => "fg=8",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneLine {
    pub kind: PaneLineKind,
    pub text: String,
    /// Colored runs of `text` (from a tool's ANSI output), drawn over the kind's style.
    pub spans: Vec<Span>,
}

impl PaneLine {
    fn new(kind: PaneLineKind, text: impl Into<String>) -> Self {
        Self { kind, text: text.into(), spans: Vec::new() }
    }

    /// A line of command output: ANSI colors become `spans` when `colors`, else are dropped.
    fn from_ansi(raw: &str, colors: bool) -> Self {
        let (text, mut spans) = sgr::parse(raw);
        let text = text.trim_end().to_owned();
        let len = text.chars().count();
        spans.retain_mut(|s| {
            s.to = s.to.min(len);
            colors && s.from < s.to
        });
        Self { kind: PaneLineKind::Text, text, spans }
    }
}

/// Where a row's preview comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneSource {
    Command,
    Path,
}

fn pane_source(cand: &Candidate, opts: &PaneOptions) -> Option<PaneSource> {
    if cand.preview_cmd.is_some() {
        return Some(PaneSource::Command);
    }
    (opts.paths && matches!(cand.kind, CandidateKind::File | CandidateKind::Dir)).then_some(PaneSource::Path)
}

/// Availability of a preview for `cand` under `opts.mode`.
pub fn pane_availability(cand: &Candidate, opts: &PaneOptions) -> PaneAvailability {
    if pane_source(cand, opts).is_none() {
        return PaneAvailability::None;
    }
    match opts.mode {
        PaneMode::On => PaneAvailability::Auto,
        PaneMode::Off => PaneAvailability::Manual,
        // Opens by itself only where it costs no suggestion lines (beside the rows).
        PaneMode::Auto if opts.right_column().is_some() => PaneAvailability::Auto,
        PaneMode::Auto => PaneAvailability::Manual,
    }
}

/// Everything a preview may touch: the shell's cwd/env and the generator runner + cache.
pub struct PaneContext<'a> {
    pub cwd: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub runner: &'a dyn GeneratorRunner,
    pub cache: &'a GeneratorCache,
}

/// Preview lines of `cand` (at most `PANE_MAX_LINES`); empty when it has no preview.
pub fn candidate_pane(cand: &Candidate, ctx: &PaneContext<'_>, opts: &PaneOptions) -> Vec<PaneLine> {
    let max = PANE_MAX_LINES;
    let mut lines = match pane_source(cand, opts) {
        Some(PaneSource::Command) => command_pane(cand, ctx, opts, max),
        Some(PaneSource::Path) => tool_or_path_pane(&resolve_path(&cand.insert_value, ctx), ctx, opts, max),
        None => Vec::new(),
    };
    lines.truncate(max);
    lines
}

/// Runs the type's `preview` command with the row's columns as parameters (cached).
fn command_pane(cand: &Candidate, ctx: &PaneContext<'_>, opts: &PaneOptions, max: usize) -> Vec<PaneLine> {
    let Some(command) = cand.preview_cmd.as_deref() else {
        return Vec::new();
    };
    let params: Vec<(String, String)> = cand
        .raw_columns
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    output_lines(&run_cached(command, params, ctx, opts), max, false)
}

/// Output of `command` run with `params` as env vars, reused for `PANE_CACHE_TTL`.
fn run_cached(command: &str, mut params: Vec<(String, String)>, ctx: &PaneContext<'_>, opts: &PaneOptions) -> Vec<String> {
    params.sort();
    let key = CacheKey {
        base_type: format!("pane:{command}"),
        params: params.clone(),
        cwd: ctx.cwd.to_path_buf(),
    };
    let rows = match ctx.cache.lookup(&key) {
        CacheLookup::Fresh(rows) => rows,
        CacheLookup::Stale(_) | CacheLookup::Miss => {
            let out = ctx.runner.run_shell(command, ctx.cwd, ctx.env, &params, opts.timeout);
            // Room for leading blank lines, trimmed by `output_lines`.
            let rows = out.into_iter().take(2 * PANE_MAX_LINES).map(|l| vec![l]).collect();
            ctx.cache.insert(key, rows, PANE_CACHE_TTL)
        }
    };
    rows.iter().filter_map(|r| r.first().cloned()).collect()
}

/// At most `max` pane lines of command output, without leading / trailing blank lines.
fn output_lines(raw: &[String], max: usize, colors: bool) -> Vec<PaneLine> {
    let mut lines: Vec<PaneLine> = raw
        .iter()
        .map(|l| PaneLine::from_ansi(l, colors))
        .skip_while(|l| l.text.is_empty())
        .take(max)
        .collect();
    while lines.last().is_some_and(|l| l.text.is_empty()) {
        lines.pop();
    }
    lines
}

/// Preview of a path row: the configured tool's output for a directory or a text file, else the
/// built-in one (also for empty, binary and unreadable files, which tools render poorly).
fn tool_or_path_pane(path: &Path, ctx: &PaneContext<'_>, opts: &PaneOptions, max: usize) -> Vec<PaneLine> {
    let width = opts.text_width();
    let builtin = || path_pane(path, width, max);
    let commands = match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => &opts.dir_commands,
        Ok(m) if m.is_file() => &opts.file_commands,
        _ => return builtin(),
    };
    let Some(command) = commands.iter().find(|c| c.split_whitespace().next().is_some_and(|p| on_path(p, ctx))) else {
        return builtin();
    };
    let head = builtin();
    if head.first().is_some_and(|l| l.kind == PaneLineKind::Note) {
        return head;
    }
    let params = vec![
        ("path".to_owned(), path.to_string_lossy().into_owned()),
        ("width".to_owned(), listing_width(width).to_string()),
        ("icons".to_owned(), opts.glyphs.eza_icons().to_owned()),
    ];
    let lines = output_lines(&run_cached(command, params, ctx, opts), max, true);
    if lines.is_empty() { head } else { lines }
}

/// Whether `program` would run in the shell's environment (`sh -c` gets the client env, or the
/// daemon's when the client sent none).
fn on_path(program: &str, ctx: &PaneContext<'_>) -> bool {
    if program.contains('/') {
        return ctx.cwd.join(program).is_file();
    }
    let path = if ctx.env.is_empty() { std::env::var("PATH").ok() } else { ctx.env.get("PATH").cloned() };
    path.is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Cells a directory listing may use (the client indents pane lines).
fn listing_width(width: usize) -> usize {
    width.saturating_sub(6).max(16)
}

/// `~`/`~/x` against the shell's `$HOME`, relative paths against its cwd.
fn resolve_path(value: &str, ctx: &PaneContext<'_>) -> PathBuf {
    let home = || {
        ctx.env
            .get("HOME")
            .cloned()
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_default()
    };
    if value == "~" {
        return PathBuf::from(home());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return PathBuf::from(home()).join(rest);
    }
    ctx.cwd.join(value)
}

/// File head or directory listing of `path`.
pub fn path_pane(path: &Path, width: usize, max: usize) -> Vec<PaneLine> {
    let Ok(meta) = std::fs::metadata(path) else {
        return vec![PaneLine::new(PaneLineKind::Note, "(not found)")];
    };
    if meta.is_dir() {
        dir_pane(path, width, max)
    } else if meta.is_file() {
        file_pane(path, meta.len(), max)
    } else {
        vec![PaneLine::new(PaneLineKind::Note, "(special file)")]
    }
}

fn file_pane(path: &Path, size: u64, max: usize) -> Vec<PaneLine> {
    let mut buf = Vec::with_capacity(MAX_HEAD_BYTES.min(usize::try_from(size).unwrap_or(MAX_HEAD_BYTES)));
    let read = std::fs::File::open(path).and_then(|f| f.take(MAX_HEAD_BYTES as u64).read_to_end(&mut buf));
    if read.is_err() {
        return vec![PaneLine::new(PaneLineKind::Note, "(unreadable)")];
    }
    if buf.is_empty() {
        return vec![PaneLine::new(PaneLineKind::Note, "(empty file)")];
    }
    if buf.contains(&0) {
        return vec![PaneLine::new(PaneLineKind::Note, format!("binary file · {}", format_size(size)))];
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .take(max)
        .map(|l| PaneLine::new(PaneLineKind::Text, strip_ansi(l).trim_end().to_owned()))
        .collect()
}

/// Entries packed into rows of `width` (dirs first, `/`-suffixed), with an overflow count.
fn dir_pane(path: &Path, width: usize, max: usize) -> Vec<PaneLine> {
    let Ok(read) = std::fs::read_dir(path) else {
        return vec![PaneLine::new(PaneLineKind::Note, "(unreadable directory)")];
    };
    let mut entries: Vec<(bool, String)> = read
        .take(MAX_DIR_ENTRIES)
        .filter_map(Result::ok)
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir())
                || (e.file_type().is_ok_and(|t| t.is_symlink()) && e.path().is_dir());
            let mut name = strip_ansi(&e.file_name().to_string_lossy());
            if is_dir {
                name.push('/');
            }
            (is_dir, name)
        })
        .collect();
    if entries.is_empty() {
        return vec![PaneLine::new(PaneLineKind::Note, "(empty directory)")];
    }
    // Dirs first, hidden entries last, then by name.
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.starts_with('.').cmp(&b.1.starts_with('.')))
            .then_with(|| a.1.cmp(&b.1))
    });

    let room = listing_width(width);
    let mut lines: Vec<PaneLine> = Vec::new();
    let mut current = String::new();
    let mut idx = 0;
    while idx < entries.len() {
        let name = &entries[idx].1;
        let need = if current.is_empty() { name.chars().count() } else { current.chars().count() + 2 + name.chars().count() };
        if !current.is_empty() && need > room {
            if lines.len() + 1 == max {
                break;
            }
            lines.push(PaneLine::new(PaneLineKind::Dir, std::mem::take(&mut current)));
            continue;
        }
        if !current.is_empty() {
            current.push_str("  ");
        }
        current.push_str(name);
        idx += 1;
    }
    let rest = entries.len() - idx;
    if rest == 0 {
        lines.push(PaneLine::new(PaneLineKind::Dir, current));
    } else if lines.len() + 1 >= max {
        // Last line: as many names as fit next to the overflow count.
        let more = format!("… {rest} more");
        if current.chars().count() + 2 + more.chars().count() <= room {
            lines.push(PaneLine::new(PaneLineKind::Dir, format!("{current}  {more}")));
        } else {
            lines.push(PaneLine::new(PaneLineKind::Dir, current));
            if let Some(last) = lines.last_mut() {
                // Too long: the count replaces the tail.
                let keep = room.saturating_sub(more.chars().count() + 2);
                last.text = format!("{}  {more}", last.text.chars().take(keep).collect::<String>().trim_end());
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::generator::FakeGeneratorRunner;

    fn ctx<'a>(cwd: &'a Path, env: &'a HashMap<String, String>, runner: &'a FakeGeneratorRunner, cache: &'a GeneratorCache) -> PaneContext<'a> {
        PaneContext { cwd, env, runner, cache }
    }

    fn texts(lines: &[PaneLine]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn availability_follows_mode_and_source() {
        let mut opts = PaneOptions::default();
        let mut typed = Candidate::word("web", None, CandidateKind::Arg, 0);
        typed.preview_cmd = Some("docker logs {name}".into());
        let file = Candidate {
            kind: CandidateKind::File,
            ..Candidate::word("a.txt", None, CandidateKind::File, 0)
        };
        let flag = Candidate::word("--force", None, CandidateKind::Opt, 0);
        // 80 columns: the pane would go below the dropdown, so `auto` waits for the pane toggle.
        assert_eq!(pane_availability(&typed, &opts), PaneAvailability::Manual);
        assert_eq!(pane_availability(&file, &opts), PaneAvailability::Manual);
        assert_eq!(pane_availability(&flag, &opts), PaneAvailability::None);
        // Wide terminal: it fits beside the rows, type and path previews alike open by themselves.
        opts.width = 160;
        assert_eq!(pane_availability(&typed, &opts), PaneAvailability::Auto);
        assert_eq!(pane_availability(&file, &opts), PaneAvailability::Auto);
        opts.width = 80;
        opts.mode = PaneMode::On;
        assert_eq!(pane_availability(&file, &opts), PaneAvailability::Auto);
        opts.mode = PaneMode::Off;
        assert_eq!(pane_availability(&typed, &opts), PaneAvailability::Manual);
        opts.paths = false;
        assert_eq!(pane_availability(&file, &opts), PaneAvailability::None);
    }

    #[test]
    fn file_head_binary_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n\x1b[31mtwo\x1b[0m\tx\nthree\nfour\n").unwrap();
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        std::fs::write(dir.path().join("e"), "").unwrap();
        assert_eq!(texts(&path_pane(&dir.path().join("a.txt"), 80, 3)), ["one", "two x", "three"]);
        let bin = path_pane(&dir.path().join("b.bin"), 80, 3);
        assert_eq!(bin[0].kind, PaneLineKind::Note);
        assert!(bin[0].text.starts_with("binary file · 4"), "{bin:?}");
        assert_eq!(texts(&path_pane(&dir.path().join("e"), 80, 3)), ["(empty file)"]);
        assert_eq!(texts(&path_pane(&dir.path().join("nope"), 80, 3)), ["(not found)"]);
    }

    #[test]
    fn directory_listing_packs_dirs_first_and_counts_overflow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        for f in ["a.rs", "b.rs", ".hidden"] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        assert_eq!(texts(&path_pane(dir.path(), 80, 5)), ["src/  a.rs  b.rs  .hidden"]);

        for i in 0..40 {
            std::fs::write(dir.path().join(format!("file-{i:02}.txt")), "").unwrap();
        }
        let lines = path_pane(dir.path(), 40, 2);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].text.starts_with("src/  a.rs  b.rs"), "{lines:?}");
        assert!(lines[1].text.ends_with(" more"), "{lines:?}");
        assert!(lines.iter().all(|l| l.text.chars().count() <= 34), "{lines:?}");
    }

    #[test]
    fn installed_tools_render_text_files_and_dirs_with_colors() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("bat"), "").unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("blob"), [0u8, 1, 2]).unwrap();
        let env = HashMap::from([("PATH".to_owned(), bin.to_string_lossy().into_owned())]);
        let runner = FakeGeneratorRunner::new();
        runner.set_response("bat", vec!["\x1b[38;5;203mfn\x1b[0m main() {}  ".into()]);
        runner.set_response("eza", vec!["never".into()]);
        let cache = GeneratorCache::new();
        let opts = PaneOptions {
            file_commands: vec!["missing-tool \"$path\"".into(), "bat \"$path\"".into()],
            dir_commands: vec!["eza \"$path\"".into()],
            ..PaneOptions::default()
        };
        let c = ctx(dir.path(), &env, &runner, &cache);
        let file = Candidate::word("a.rs", None, CandidateKind::File, 0);
        let lines = candidate_pane(&file, &c, &opts);
        assert_eq!(texts(&lines), ["fn main() {}"]);
        assert_eq!(lines[0].spans, [Span { from: 0, to: 2, style: "fg=203".into() }]);
        let (command, params) = &runner.recorded_calls()[0];
        assert_eq!(command, "bat \"$path\"", "the first installed tool runs");
        assert!(params.contains(&("path".to_owned(), dir.path().join("a.rs").to_string_lossy().into_owned())), "{params:?}");
        // Binary files keep the built-in note; eza isn't on $PATH, so the built-in listing shows.
        let blob = Candidate::word("blob", None, CandidateKind::File, 0);
        assert!(texts(&candidate_pane(&blob, &c, &opts))[0].starts_with("binary file"));
        let root = Candidate::word(".", None, CandidateKind::Dir, 0);
        assert!(texts(&candidate_pane(&root, &c, &opts))[0].starts_with("bin/"));
        assert_eq!(runner.call_count(), 1);
    }

    #[test]
    fn dir_commands_get_the_icons_param_of_the_glyph_set() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("eza"), "").unwrap();
        let env = HashMap::from([("PATH".to_owned(), bin.to_string_lossy().into_owned())]);
        let cache = GeneratorCache::new();
        let root = Candidate::word(".", None, CandidateKind::Dir, 0);
        for (glyphs, icons) in [(Glyphs::Nerd, "always"), (Glyphs::Plain, "never")] {
            let runner = FakeGeneratorRunner::new();
            runner.set_response("eza", vec!["bin".into()]);
            let opts = PaneOptions { dir_commands: vec!["eza --icons=\"$icons\" \"$path\"".into()], glyphs, ..PaneOptions::default() };
            let c = ctx(dir.path(), &env, &runner, &cache);
            candidate_pane(&root, &c, &opts);
            let (_, params) = &runner.recorded_calls()[0];
            assert!(params.contains(&("icons".to_owned(), icons.to_owned())), "{glyphs:?}: {params:?}");
        }
    }

    #[test]
    fn command_preview_runs_with_row_params_and_is_cached() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::new();
        let runner = FakeGeneratorRunner::new();
        runner.set_response("docker logs", vec!["".into(), "\x1b[1mline 1\x1b[0m".into(), "line 2".into(), "line 3".into()]);
        let cache = GeneratorCache::new();
        let mut cand = Candidate::word("web", None, CandidateKind::Arg, 0);
        cand.preview_cmd = Some("docker logs --tail 5 {name}".into());
        cand.raw_columns.insert("name".into(), "web".into());
        // The whole output is sent (the shell scrolls it), not just the visible `lines`.
        let opts = PaneOptions { lines: 2, ..PaneOptions::default() };
        let c = ctx(dir.path(), &env, &runner, &cache);
        assert_eq!(texts(&candidate_pane(&cand, &c, &opts)), ["line 1", "line 2", "line 3"]);
        assert_eq!(texts(&candidate_pane(&cand, &c, &opts)), ["line 1", "line 2", "line 3"]);
        assert_eq!(runner.call_count(), 1, "second preview comes from the cache");
        let (_, params) = &runner.recorded_calls()[0];
        assert!(params.contains(&("name".to_owned(), "web".to_owned())), "{params:?}");
    }

    #[test]
    fn path_rows_resolve_against_cwd_and_home() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/x.txt"), "hello\n").unwrap();
        let env = HashMap::from([("HOME".to_owned(), dir.path().display().to_string())]);
        let runner = FakeGeneratorRunner::new();
        let cache = GeneratorCache::new();
        let sub = dir.path().join("sub");
        let c = ctx(&sub, &env, &runner, &cache);
        let opts = PaneOptions::default();
        let mut rel = Candidate::word("x.txt", None, CandidateKind::File, 0);
        rel.kind = CandidateKind::File;
        assert_eq!(texts(&candidate_pane(&rel, &c, &opts)), ["hello"]);
        let tilde = Candidate::word("~/sub/", None, CandidateKind::Dir, 0);
        assert_eq!(texts(&candidate_pane(&tilde, &c, &opts)), ["x.txt"]);
    }
}
