//! Line-based wire protocol between shell clients (`ezlo.zsh`) and the daemon.
//!
//! Every message is one line of `FIELD_SEP`-separated fields, tag first. Responses (`R`) are
//! followed by one `W` line per row, one `D` line per diagnostic and one `P` line per effect
//! preview line; the positional signature hint rides in the `R` header itself (fields 11–13).
//! `W` field 8 tells whether the row has a candidate preview (`auto` | `manual` | empty); the
//! client asks for it with `PANE` and gets a `V` reply, independently of `Q`/`R`.
//! `TRANSLATE <id> <line>` asks the `[complete.translate]` command to turn a comment line into
//! suggested commands (an empty line cancels the running one); answered asynchronously: on success
//! an `R <id>` response with one `translated` row per command (replacing the whole line) followed,
//! in the same write, by `T ok` (the commands); on failure a single `T err` (message). There are
//! no progress heartbeats on purpose: any
//! traffic on the socket while zle waits for the rest of a key sequence restarts its
//! `KEYTIMEOUT`, so a lone Esc (cancel) would never resolve.
//! `SET key=value…` replaces the session's setting overrides (Alt-K popup) and is answered with
//! `S key=value…`: the daemon's config values of the overridable settings.
//! Fields never contain `FIELD_SEP` or newlines (sanitized).

use crate::config::Config;
use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::diagnostics::LineDiagnostic;
use crate::engine::explain::ExplainLine;
use crate::engine::preview::pane::{pane_availability, PaneAvailability, PaneLine, PaneMode, PaneOptions};
use crate::engine::preview::PreviewLine;
use crate::engine::rank::layout::format_rows;
use crate::engine::signature::SignatureHint;
use std::collections::HashMap;
use std::fmt::Write as _;

pub const FIELD_SEP: char = '\x1f';
pub const PROTOCOL_VERSION: &str = "1";
/// Rows sent per response; the client scrolls within them.
pub const MAX_WIRE_ROWS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    Hello {
        version: String,
        shell: String,
        session_id: String,
        cols: usize,
    },
    Env {
        cwd: String,
        vars: HashMap<String, String>,
    },
    Alias {
        aliases: HashMap<String, String>,
    },
    Opts {
        hist_ignore_space: bool,
        cols: Option<usize>,
        histfile: Option<String>,
    },
    Query {
        id: u64,
        cursor: usize,
        buffer: String,
    },
    Accept {
        id: u64,
        row_index: usize,
    },
    /// `ezlo explain` for the line (Alt-H); answered with an `E` response.
    Explain {
        id: u64,
        cursor: usize,
        buffer: String,
    },
    /// Empty-prompt workflow recall (Ctrl-Space, or Tab on an empty buffer): the most frecent
    /// lines of the current directory/repo, answered with an `R` response of history rows.
    Recall {
        id: u64,
    },
    /// Preview pane of row `row_index` (1-based) of response `id`; `insert` (the row's insert
    /// text) guards against rows that changed meanwhile. Answered asynchronously with `V`.
    Pane {
        id: u64,
        row_index: usize,
        insert: String,
    },
    /// Translate comment line `line` (`# list files`) into a command with the configured
    /// translator; an empty `line` cancels the running translation. Answered with an `R` response
    /// of suggestions plus `T ok`, or a `T err` reply.
    Translate {
        id: u64,
        line: String,
    },
    Exec {
        cwd: String,
        line: String,
    },
    Status {
        exit_code: i32,
    },
    /// Session-scoped setting overrides (Alt-K popup), replacing earlier ones; answered with `S`.
    Set {
        overrides: SessionOverrides,
    },
    Ping,
    Shutdown,
}

/// Settings a shell can override for its own session (`SET`); `None` keeps the config value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionOverrides {
    pub rows: Option<usize>,
    pub history_rows: Option<usize>,
    pub min_chars: Option<usize>,
    pub signature: Option<bool>,
    pub diagnostics: Option<bool>,
    pub preview: Option<bool>,
    /// Preview pane mode (Alt-P / popup): `on` / `off` force it, unset follows `[complete.pane]`.
    pub pane: Option<PaneMode>,
}

/// Effective values of the overridable settings. The `S` reply carries the config's, so the
/// client can show them and tell which of its overrides differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSettings {
    /// `[complete.dropdown] rows`
    pub rows: usize,
    /// `[complete.dropdown] history_rows`
    pub history_rows: usize,
    /// `[complete] min_chars`
    pub min_chars: usize,
    /// `[complete] signature`
    pub signature: bool,
    /// Line diagnostics in responses (not configurable in `config.toml`: always on there).
    pub diagnostics: bool,
    /// `[preview] enabled`
    pub preview: bool,
}

/// Upper bounds applied to overrides, so a bogus client can't ask for absurd work.
const MAX_OVERRIDE_HISTORY_ROWS: usize = 20;
const MAX_OVERRIDE_MIN_CHARS: usize = 20;

impl SessionOverrides {
    fn parse<'a>(parts: impl Iterator<Item = &'a str>) -> Self {
        let mut o = Self::default();
        let num = |v: &str, lo: usize, hi: usize| v.parse::<usize>().ok().map(|n| n.clamp(lo, hi));
        let flag = |v: &str| match v {
            "1" | "true" | "on" => Some(true),
            "0" | "false" | "off" => Some(false),
            _ => None,
        };
        // Unknown keys are ignored, so newer clients still talk to older daemons.
        for (k, v) in parts.filter_map(|kv| kv.split_once('=')) {
            match k {
                "rows" => o.rows = num(v, 1, MAX_WIRE_ROWS),
                "history_rows" => o.history_rows = num(v, 0, MAX_OVERRIDE_HISTORY_ROWS),
                "min_chars" => o.min_chars = num(v, 0, MAX_OVERRIDE_MIN_CHARS),
                "signature" => o.signature = flag(v),
                "diagnostics" => o.diagnostics = flag(v),
                "preview" => o.preview = flag(v),
                "pane" => {
                    o.pane = match v {
                        "on" => Some(PaneMode::On),
                        "off" => Some(PaneMode::Off),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
        o
    }

    fn push_fields(&self, out: &mut String) {
        let s = FIELD_SEP;
        let nums = [("rows", self.rows), ("history_rows", self.history_rows), ("min_chars", self.min_chars)];
        for (k, v) in nums {
            if let Some(v) = v {
                let _ = write!(out, "{s}{k}={v}");
            }
        }
        let flags = [("signature", self.signature), ("diagnostics", self.diagnostics), ("preview", self.preview)];
        for (k, v) in flags {
            if let Some(v) = v {
                let _ = write!(out, "{s}{k}={}", u8::from(v));
            }
        }
        if let Some(mode) = self.pane {
            let v = if mode == PaneMode::Off { "off" } else { "on" };
            let _ = write!(out, "{s}pane={v}");
        }
    }

    /// `[complete.pane]` options with this shell's pane mode applied.
    pub fn pane_options(&self, config: &Config, cols: usize) -> PaneOptions {
        let mut opts = config.pane_options(cols);
        if let Some(mode) = self.pane {
            opts.mode = mode;
        }
        opts
    }
}

impl SessionSettings {
    pub fn from_config(config: &Config) -> Self {
        let c = &config.complete;
        Self {
            rows: c.dropdown.rows,
            history_rows: c.dropdown.history_rows,
            min_chars: c.min_chars,
            signature: c.signature,
            diagnostics: true,
            preview: config.preview.enabled,
        }
    }

    pub fn with_overrides(self, o: &SessionOverrides) -> Self {
        Self {
            rows: o.rows.unwrap_or(self.rows),
            history_rows: o.history_rows.unwrap_or(self.history_rows),
            min_chars: o.min_chars.unwrap_or(self.min_chars),
            signature: o.signature.unwrap_or(self.signature),
            diagnostics: o.diagnostics.unwrap_or(self.diagnostics),
            preview: o.preview.unwrap_or(self.preview),
        }
    }
}

/// `S rows=… history_rows=… min_chars=… signature=0|1 diagnostics=0|1 preview=0|1`.
pub fn encode_settings(settings: &SessionSettings) -> String {
    let all = SessionOverrides {
        rows: Some(settings.rows),
        history_rows: Some(settings.history_rows),
        min_chars: Some(settings.min_chars),
        signature: Some(settings.signature),
        diagnostics: Some(settings.diagnostics),
        preview: Some(settings.preview),
        pane: None,
    };
    let mut out = String::from("S");
    all.push_fields(&mut out);
    out.push('\n');
    out
}

/// Parses an `S` reply; keys it lacks keep `fallback`'s values.
pub fn decode_settings(line: &str, fallback: SessionSettings) -> Option<SessionSettings> {
    let mut parts = line.trim_end_matches(['\r', '\n']).split(FIELD_SEP);
    if parts.next()? != "S" {
        return None;
    }
    Some(fallback.with_overrides(&SessionOverrides::parse(parts)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRow {
    pub display_text: String,
    pub highlights: String,
    pub insert_text: String,
    pub suffix: String,
    pub kind: String,
    pub description: String,
    /// Candidate preview availability (`auto` | `manual` | empty: none).
    pub pane: String,
    /// The row's full text (label and description) when the layout cut part of it; empty when
    /// everything fits. Shown in the status line or on top of the preview pane.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireDiag {
    pub start: usize,
    pub end: usize,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireResponse {
    pub id: u64,
    pub start: usize,
    pub end: usize,
    pub ghost: String,
    pub selected: usize,
    /// Dropdown window height from `[complete.dropdown] rows` (0: let the client decide).
    pub max_rows: usize,
    pub rows: Vec<WireRow>,
    pub diagnostics: Vec<WireDiag>,
    /// Effect preview lines (`P <kind> <text>`, kind `effect` | `warning` | `dryrun`).
    pub preview: Vec<WirePreview>,
    /// Positional signature hint (`git checkout  <branch>  [paths…]`; empty: none).
    pub signature: String,
    /// Char range of the active placeholder in `signature`.
    pub signature_active: Option<(usize, usize)>,
    /// Terminal column where the preview pane starts, right of the rows (laid out up to it);
    /// 0: the pane, if any, goes below the dropdown.
    pub pane_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirePreview {
    pub kind: String,
    pub text: String,
}

impl WireResponse {
    pub fn with_diagnostics(mut self, diagnostics: &[LineDiagnostic]) -> Self {
        self.diagnostics = diagnostics
            .iter()
            .map(|d| WireDiag {
                start: d.char_start,
                end: d.char_end,
                severity: d.severity.as_str().to_owned(),
                message: sanitize_field(&d.message),
            })
            .collect();
        self
    }

    pub fn with_preview(mut self, preview: &[PreviewLine]) -> Self {
        self.preview = preview
            .iter()
            .map(|p| WirePreview {
                kind: p.kind.as_str().to_owned(),
                text: sanitize_field(&p.text),
            })
            .collect();
        self
    }

    /// Fills each row's preview availability (`rows` are the first `candidates`). When most rows
    /// (history rows aside) open the pane by themselves and it fits on the right, the rows are laid
    /// out narrower so the pane sits beside them (`pane_col`); the layout stays the same while the
    /// selection moves. Otherwise, in `auto` mode, those rows wait for Alt-P: a pane below the
    /// dropdown would cost suggestion lines, and a lone previewable row shouldn't squeeze the rest.
    pub fn with_pane(mut self, candidates: &[Candidate], opts: &PaneOptions) -> Self {
        let (mut auto, mut eligible) = (0usize, 0usize);
        for (row, cand) in self.rows.iter_mut().zip(candidates) {
            let avail = pane_availability(cand, opts);
            if cand.kind != CandidateKind::History {
                eligible += 1;
                auto += usize::from(avail == PaneAvailability::Auto);
            }
            avail.as_str().clone_into(&mut row.pane);
        }
        let Some(col) = opts.right_column().filter(|_| auto * 2 > eligible) else {
            if opts.mode == PaneMode::Auto {
                let manual = PaneAvailability::Manual.as_str();
                for row in self.rows.iter_mut().filter(|r| r.pane == PaneAvailability::Auto.as_str()) {
                    manual.clone_into(&mut row.pane);
                }
            }
            return self;
        };
        let shown = &candidates[..self.rows.len().min(candidates.len())];
        for ((row, cand), formatted) in self.rows.iter_mut().zip(shown).zip(format_rows(shown, col)) {
            row.display_text = sanitize_field(&formatted.display_text);
            row.highlights = sanitize_field(&formatted.highlights);
            row.detail = row_detail(cand, formatted.truncated);
        }
        self.pane_col = col;
        self
    }

    pub fn with_signature(mut self, signature: Option<&SignatureHint>) -> Self {
        match signature {
            Some(sig) => {
                self.signature = sanitize_field(&sig.text);
                self.signature_active = sig.active;
            }
            None => {
                self.signature.clear();
                self.signature_active = None;
            }
        }
        self
    }
}

/// Reply to `EXPLAIN`: `E <id> <n>` followed by `n` lines `L <style> <text>`, where `style` is a
/// zsh `region_highlight` style for the whole line (possibly empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireExplain {
    pub id: u64,
    pub lines: Vec<(String, String)>,
}

impl WireExplain {
    pub fn from_lines(id: u64, lines: &[ExplainLine]) -> Self {
        Self {
            id,
            lines: lines
                .iter()
                .map(|l| (l.style.zsh_style().to_owned(), sanitize_field(&l.text)))
                .collect(),
        }
    }
}

pub fn encode_explain(resp: &WireExplain) -> String {
    let s = FIELD_SEP;
    let mut out = format!("E{s}{}{s}{}\n", resp.id, resp.lines.len());
    for (style, text) in &resp.lines {
        let _ = writeln!(out, "L{s}{}{s}{}", sanitize_field(style), sanitize_field(text));
    }
    out
}

pub fn decode_explain(raw: &str) -> Option<WireExplain> {
    let mut lines = raw.lines();
    let mut header = lines.next()?.split(FIELD_SEP);
    if header.next()? != "E" {
        return None;
    }
    let id = header.next()?.parse().ok()?;
    let n: usize = header.next()?.parse().ok()?;
    let lines = (0..n)
        .map(|_| {
            let mut parts = lines.next()?.split(FIELD_SEP);
            if parts.next()? != "L" {
                return None;
            }
            let style = parts.next()?.to_owned();
            Some((style, parts.next().unwrap_or_default().to_owned()))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(WireExplain { id, lines })
}

/// Reply to `PANE`: `V <id> <row> <n> <height>` followed by `n` lines `L <style> <text> <spans>`
/// (as in `E`, plus colored runs `from-to:style …` in chars of `text`). The shell shows `height` lines (`[complete.pane] lines`; more beside a taller dropdown)
/// and scrolls through the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirePane {
    pub id: u64,
    pub row: usize,
    pub height: usize,
    /// `(style, text, spans)`.
    pub lines: Vec<(String, String, String)>,
}

impl WirePane {
    pub fn from_lines(id: u64, row: usize, height: usize, lines: &[PaneLine]) -> Self {
        Self {
            id,
            row,
            height,
            lines: lines
                .iter()
                .map(|l| {
                    let spans: Vec<String> = l.spans.iter().map(|s| format!("{}-{}:{}", s.from, s.to, s.style)).collect();
                    (l.kind.zsh_style().to_owned(), sanitize_field(&l.text), spans.join(" "))
                })
                .collect(),
        }
    }
}

pub fn encode_pane(resp: &WirePane) -> String {
    let s = FIELD_SEP;
    let mut out = format!("V{s}{}{s}{}{s}{}{s}{}\n", resp.id, resp.row, resp.lines.len(), resp.height);
    for (style, text, spans) in &resp.lines {
        let _ = writeln!(out, "L{s}{}{s}{}{s}{}", sanitize_field(style), sanitize_field(text), sanitize_field(spans));
    }
    out
}

pub fn decode_pane(raw: &str) -> Option<WirePane> {
    let mut lines = raw.lines();
    let mut header = lines.next()?.split(FIELD_SEP);
    if header.next()? != "V" {
        return None;
    }
    let id = header.next()?.parse().ok()?;
    let row = header.next()?.parse().ok()?;
    let n: usize = header.next()?.parse().ok()?;
    let height = header.next()?.parse().ok()?;
    let lines = (0..n)
        .map(|_| {
            let mut parts = lines.next()?.split(FIELD_SEP);
            if parts.next()? != "L" {
                return None;
            }
            let style = parts.next()?.to_owned();
            let text = parts.next().unwrap_or_default().to_owned();
            Some((style, text, parts.next().unwrap_or_default().to_owned()))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(WirePane { id, row, height, lines })
}

/// State of a `T` reply to `TRANSLATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateState {
    /// Done: the lines are the suggested commands (already sent as the rows of an `R` response).
    Ok,
    /// Failed, timed out or cancelled: the line is the reason.
    Err,
}

impl TranslateState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Err => "err",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => Self::Ok,
            "err" => Self::Err,
            _ => return None,
        })
    }
}

/// Reply to `TRANSLATE`: `T <id> <state> <n>` followed by `n` lines `L <style> <text>` (as in `E`,
/// style empty): the suggested commands (one per line) for `ok`, the message for `err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireTranslate {
    pub id: u64,
    pub state: TranslateState,
    pub lines: Vec<String>,
}

pub fn encode_translate(resp: &WireTranslate) -> String {
    let s = FIELD_SEP;
    let mut out = format!("T{s}{}{s}{}{s}{}\n", resp.id, resp.state.as_str(), resp.lines.len());
    for text in &resp.lines {
        let _ = writeln!(out, "L{s}{s}{}", sanitize_field(text));
    }
    out
}

pub fn decode_translate(raw: &str) -> Option<WireTranslate> {
    let mut lines = raw.lines();
    let mut header = lines.next()?.split(FIELD_SEP);
    if header.next()? != "T" {
        return None;
    }
    let id = header.next()?.parse().ok()?;
    let state = TranslateState::parse(header.next()?)?;
    let n: usize = header.next()?.parse().ok()?;
    let lines = (0..n)
        .map(|_| {
            let mut parts = lines.next()?.split(FIELD_SEP);
            if parts.next()? != "L" {
                return None;
            }
            parts.next()?;
            Some(parts.next().unwrap_or_default().to_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    Some(WireTranslate { id, state, lines })
}

/// Reply to `PING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub pid: u32,
    pub cmds: usize,
    pub recipes: usize,
}

fn parse_kv_fields<'a>(parts: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    parts
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn push_kv_fields(out: &mut String, map: &HashMap<String, String>) {
    let clean = |s: &str| !s.contains(FIELD_SEP) && !s.contains('\n');
    for (k, v) in map.iter().filter(|(k, v)| clean(k) && clean(v)) {
        let _ = write!(out, "{FIELD_SEP}{k}={v}");
    }
}

pub fn parse_client_line(line: &str) -> Option<ClientMsg> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split(FIELD_SEP);
    let msg = match parts.next()? {
        "HELLO" => ClientMsg::Hello {
            version: parts.next()?.to_owned(),
            shell: parts.next()?.to_owned(),
            session_id: parts.next()?.to_owned(),
            cols: parts.next().and_then(|s| s.parse().ok()).unwrap_or(80),
        },
        "ENV" => ClientMsg::Env {
            cwd: parts.next()?.to_owned(),
            vars: parse_kv_fields(parts),
        },
        "ALIAS" => ClientMsg::Alias {
            aliases: parse_kv_fields(parts),
        },
        "OPTS" => parse_opts(parts),
        "Q" => ClientMsg::Query {
            id: parts.next()?.parse().ok()?,
            cursor: parts.next()?.parse().ok()?,
            buffer: parts.next().unwrap_or_default().to_owned(),
        },
        "ACCEPT" => ClientMsg::Accept {
            id: parts.next()?.parse().ok()?,
            row_index: parts.next()?.parse().ok()?,
        },
        "EXPLAIN" => ClientMsg::Explain {
            id: parts.next()?.parse().ok()?,
            cursor: parts.next()?.parse().ok()?,
            buffer: parts.next().unwrap_or_default().to_owned(),
        },
        "RECALL" => ClientMsg::Recall {
            id: parts.next()?.parse().ok()?,
        },
        "PANE" => ClientMsg::Pane {
            id: parts.next()?.parse().ok()?,
            row_index: parts.next()?.parse().ok()?,
            insert: parts.next().unwrap_or_default().to_owned(),
        },
        "TRANSLATE" => ClientMsg::Translate {
            id: parts.next()?.parse().ok()?,
            line: parts.next().unwrap_or_default().to_owned(),
        },
        "EXEC" => ClientMsg::Exec {
            cwd: parts.next()?.to_owned(),
            line: parts.next().unwrap_or_default().to_owned(),
        },
        "STATUS" => ClientMsg::Status {
            exit_code: parts.next()?.parse().ok()?,
        },
        "SET" => ClientMsg::Set {
            overrides: SessionOverrides::parse(parts),
        },
        "PING" => ClientMsg::Ping,
        "SHUTDOWN" => ClientMsg::Shutdown,
        _ => return None,
    };
    Some(msg)
}

fn parse_opts<'a>(parts: impl Iterator<Item = &'a str>) -> ClientMsg {
    let mut hist_ignore_space = false;
    let mut cols = None;
    let mut histfile = None;
    for (k, v) in parts.filter_map(|kv| kv.split_once('=')) {
        match k {
            "hist_ignore_space" => hist_ignore_space = v == "1" || v == "true",
            "COLUMNS" | "cols" => cols = v.parse().ok(),
            "HISTFILE" => histfile = Some(v.to_owned()).filter(|h| !h.is_empty()),
            _ => {}
        }
    }
    ClientMsg::Opts {
        hist_ignore_space,
        cols,
        histfile,
    }
}

pub fn encode_client_msg(msg: &ClientMsg) -> String {
    let s = FIELD_SEP;
    let mut out = match msg {
        ClientMsg::Hello {
            version,
            shell,
            session_id,
            cols,
        } => format!("HELLO{s}{version}{s}{shell}{s}{session_id}{s}{cols}"),
        ClientMsg::Env { cwd, vars } => {
            let mut out = format!("ENV{s}{cwd}");
            push_kv_fields(&mut out, vars);
            out
        }
        ClientMsg::Alias { aliases } => {
            let mut out = String::from("ALIAS");
            push_kv_fields(&mut out, aliases);
            out
        }
        ClientMsg::Opts {
            hist_ignore_space,
            cols,
            histfile,
        } => {
            let mut out = format!("OPTS{s}hist_ignore_space={}", u8::from(*hist_ignore_space));
            if let Some(c) = cols {
                let _ = write!(out, "{s}COLUMNS={c}");
            }
            if let Some(h) = histfile {
                let _ = write!(out, "{s}HISTFILE={h}");
            }
            out
        }
        ClientMsg::Query { id, cursor, buffer } => format!("Q{s}{id}{s}{cursor}{s}{buffer}"),
        ClientMsg::Accept { id, row_index } => format!("ACCEPT{s}{id}{s}{row_index}"),
        ClientMsg::Explain { id, cursor, buffer } => format!("EXPLAIN{s}{id}{s}{cursor}{s}{buffer}"),
        ClientMsg::Recall { id } => format!("RECALL{s}{id}"),
        ClientMsg::Pane { id, row_index, insert } => format!("PANE{s}{id}{s}{row_index}{s}{insert}"),
        ClientMsg::Translate { id, line } => format!("TRANSLATE{s}{id}{s}{line}"),
        ClientMsg::Exec { cwd, line } => format!("EXEC{s}{cwd}{s}{line}"),
        ClientMsg::Status { exit_code } => format!("STATUS{s}{exit_code}"),
        ClientMsg::Set { overrides } => {
            let mut out = String::from("SET");
            overrides.push_fields(&mut out);
            out
        }
        ClientMsg::Ping => "PING".to_owned(),
        ClientMsg::Shutdown => "SHUTDOWN".to_owned(),
    };
    out.push('\n');
    out
}

pub fn encode_pong(pong: Pong) -> String {
    let s = FIELD_SEP;
    format!("PONG{s}{}{s}{}{s}{}\n", pong.pid, pong.cmds, pong.recipes)
}

pub fn parse_pong(line: &str) -> Option<Pong> {
    let mut parts = line.trim_end_matches(['\r', '\n']).split(FIELD_SEP);
    if parts.next()? != "PONG" {
        return None;
    }
    Some(Pong {
        pid: parts.next()?.parse().ok()?,
        cmds: parts.next()?.parse().ok()?,
        recipes: parts.next()?.parse().ok()?,
    })
}

pub fn build_wire_response(
    id: u64,
    start: usize,
    end: usize,
    ghost: &str,
    candidates: &[Candidate],
    term_cols: usize,
    max_rows: usize,
) -> WireResponse {
    let shown = &candidates[..candidates.len().min(MAX_WIRE_ROWS)];
    let rows: Vec<WireRow> = shown
        .iter()
        .zip(format_rows(shown, term_cols))
        .map(|(cand, formatted)| WireRow {
            detail: row_detail(cand, formatted.truncated),
            display_text: sanitize_field(&formatted.display_text),
            highlights: sanitize_field(&formatted.highlights),
            insert_text: sanitize_field(&cand.insert_text),
            suffix: sanitize_field(&cand.suffix),
            kind: cand.kind.as_str().to_owned(),
            description: sanitize_field(&cand.description),
            pane: String::new(),
        })
        .collect();
    WireResponse {
        id,
        start,
        end,
        ghost: sanitize_field(ghost),
        selected: usize::from(!rows.is_empty()),
        max_rows,
        rows,
        diagnostics: Vec::new(),
        preview: Vec::new(),
        signature: String::new(),
        signature_active: None,
        pane_col: 0,
    }
}

/// `WireRow::detail`: `label  description` for a row the layout truncated.
fn row_detail(cand: &Candidate, truncated: bool) -> String {
    if !truncated {
        return String::new();
    }
    let full = if cand.description.is_empty() {
        cand.label.clone()
    } else {
        format!("{}  {}", cand.label, cand.description)
    };
    sanitize_field(&full)
}

pub fn encode_wire_response(resp: &WireResponse) -> String {
    let s = FIELD_SEP;
    let (sig_start, sig_end) = resp
        .signature_active
        .map_or((String::new(), String::new()), |(a, b)| (a.to_string(), b.to_string()));
    let mut out = format!(
        "R{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}\n",
        resp.id,
        resp.start,
        resp.end,
        resp.ghost,
        resp.rows.len(),
        resp.selected,
        resp.max_rows,
        resp.diagnostics.len(),
        resp.preview.len(),
        sanitize_field(&resp.signature),
        sig_start,
        sig_end,
        resp.pane_col
    );
    for row in &resp.rows {
        let _ = writeln!(
            out,
            "W{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}{s}{}",
            row.display_text,
            row.highlights,
            row.insert_text,
            row.suffix,
            row.kind,
            row.description,
            row.pane,
            row.detail
        );
    }
    for diag in &resp.diagnostics {
        let _ = writeln!(
            out,
            "D{s}{}{s}{}{s}{}{s}{}",
            diag.start,
            diag.end,
            sanitize_field(&diag.severity),
            sanitize_field(&diag.message)
        );
    }
    for p in &resp.preview {
        let _ = writeln!(out, "P{s}{}{s}{}", sanitize_field(&p.kind), sanitize_field(&p.text));
    }
    out
}

pub fn decode_wire_response(raw: &str) -> Option<WireResponse> {
    let mut lines = raw.lines();
    let mut header = lines.next()?.split(FIELD_SEP);
    if header.next()? != "R" {
        return None;
    }
    let id = header.next()?.parse().ok()?;
    let start = header.next()?.parse().ok()?;
    let end = header.next()?.parse().ok()?;
    let ghost = header.next()?.to_owned();
    let nrows: usize = header.next()?.parse().ok()?;
    let selected = header.next()?.parse().ok()?;
    let max_rows = header.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let ndiags: usize = header.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let npreview: usize = header.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let signature = header.next().unwrap_or_default().to_owned();
    let sig_start = header.next().and_then(|v| v.parse().ok());
    let sig_end = header.next().and_then(|v| v.parse().ok());
    let pane_col = header.next().and_then(|v| v.parse().ok()).unwrap_or(0);

    let rows = (0..nrows)
        .map(|_| decode_row(lines.next()?))
        .collect::<Option<Vec<_>>>()?;
    let diagnostics = (0..ndiags)
        .map(|_| decode_diag(lines.next()?))
        .collect::<Option<Vec<_>>>()?;
    let preview = (0..npreview)
        .map(|_| decode_preview(lines.next()?))
        .collect::<Option<Vec<_>>>()?;
    Some(WireResponse {
        id,
        start,
        end,
        ghost,
        selected,
        max_rows,
        rows,
        diagnostics,
        preview,
        signature,
        signature_active: sig_start.zip(sig_end),
        pane_col,
    })
}

fn decode_preview(line: &str) -> Option<WirePreview> {
    let mut parts = line.split(FIELD_SEP);
    if parts.next()? != "P" {
        return None;
    }
    Some(WirePreview {
        kind: parts.next()?.to_owned(),
        text: parts.next().unwrap_or_default().to_owned(),
    })
}

fn decode_row(line: &str) -> Option<WireRow> {
    let mut parts = line.split(FIELD_SEP);
    if parts.next()? != "W" {
        return None;
    }
    Some(WireRow {
        display_text: parts.next()?.to_owned(),
        highlights: parts.next()?.to_owned(),
        insert_text: parts.next()?.to_owned(),
        suffix: parts.next()?.to_owned(),
        kind: parts.next()?.to_owned(),
        description: parts.next().unwrap_or_default().to_owned(),
        pane: parts.next().unwrap_or_default().to_owned(),
        detail: parts.next().unwrap_or_default().to_owned(),
    })
}

fn decode_diag(line: &str) -> Option<WireDiag> {
    let mut parts = line.split(FIELD_SEP);
    if parts.next()? != "D" {
        return None;
    }
    Some(WireDiag {
        start: parts.next()?.parse().ok()?,
        end: parts.next()?.parse().ok()?,
        severity: parts.next()?.to_owned(),
        message: parts.next().unwrap_or_default().to_owned(),
    })
}

fn sanitize_field(s: &str) -> String {
    s.chars()
        .filter(|&c| c != FIELD_SEP && c != '\n' && c != '\r')
        .collect()
}
