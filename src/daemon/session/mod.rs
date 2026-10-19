//! One connected shell: its environment snapshot, message handling and completion queries.

mod history;
mod native;
mod native_rows;
mod pane;
mod refresh;
mod translate;

use super::protocol::{
    build_wire_response, encode_explain, encode_pong, encode_settings, encode_wire_response, parse_client_line,
    ClientMsg, Pong, SessionOverrides, SessionSettings, WireExplain,
};
use super::DaemonShared;
use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::explain::explain_line;
use crate::engine::icons::Glyphs;
use crate::engine::preview::{preview_line, PreviewLine};
use crate::engine::translate::{comment_query, is_comment_line, translate_row};
use crate::engine::{complete_line, CompletionRequest};
use crate::history::merge_history;
use crate::spec::SpecIndex;
use crate::store::now_unix_ts;
use refresh::BackgroundRefresh;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

const MIN_COLS: usize = 24;

/// What the shell last told us about itself. Shared (`Arc`) so background refreshes can keep a
/// snapshot without copying the environment.
#[derive(Clone)]
struct ShellEnv {
    cwd: Arc<Path>,
    vars: Arc<HashMap<String, String>>,
    aliases: Arc<HashMap<String, String>>,
}

struct Session {
    shared: Arc<DaemonShared>,
    out: mpsc::UnboundedSender<String>,
    env: ShellEnv,
    hist_ignore_space: bool,
    histfile: Option<String>,
    cols: usize,
    latest_req_id: Arc<AtomicU64>,
    /// Rows of the last response, so `ACCEPT <row>` can record what was picked.
    last_candidates: Vec<Candidate>,
    last_usage_ctx: String,
    /// `EXEC` (cwd, line, start) waiting for its `STATUS`.
    last_exec: Option<(String, String, std::time::Instant)>,
    /// Settings this shell overrode for itself (`SET`, Alt-E popup).
    overrides: SessionOverrides,
    /// Cancel flag of the running `TRANSLATE`, if any.
    translate_cancel: Option<Arc<AtomicBool>>,
}

pub(super) async fn handle_client(stream: UnixStream, shared: Arc<DaemonShared>) {
    let (read_half, mut write_half) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

    let writer_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write_half.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            let _ = write_half.flush().await;
        }
    });

    let mut session = Session::new(shared, out_tx);
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Some(msg) = parse_client_line(&line) else {
            continue;
        };
        if !session.handle(msg) {
            break;
        }
    }

    drop(session);
    let _ = writer_task.await;
}

impl Session {
    fn new(shared: Arc<DaemonShared>, out: mpsc::UnboundedSender<String>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            shared,
            out,
            env: ShellEnv {
                cwd: Arc::from(cwd),
                vars: Arc::default(),
                aliases: Arc::default(),
            },
            hist_ignore_space: false,
            histfile: None,
            cols: 80,
            latest_req_id: Arc::new(AtomicU64::new(0)),
            last_candidates: Vec::new(),
            last_usage_ctx: String::new(),
            last_exec: None,
            overrides: SessionOverrides::default(),
            translate_cancel: None,
        }
    }

    /// Row icon glyph set from the shell's `NERD_FONT`.
    fn glyphs(&self) -> Glyphs {
        Glyphs::from_env(&self.env.vars)
    }

    /// Config values with this session's overrides applied.
    fn settings(&self) -> SessionSettings {
        SessionSettings::from_config(&self.shared.config).with_overrides(&self.overrides)
    }

    fn send(&self, msg: String) {
        // The writer only stops when the client is gone; nothing left to do then.
        let _ = self.out.send(msg);
    }

    /// Returns `false` when the connection should be closed.
    fn handle(&mut self, msg: ClientMsg) -> bool {
        match msg {
            ClientMsg::Hello { cols, .. } => self.cols = cols.max(MIN_COLS),
            ClientMsg::Env { cwd, vars } => {
                if !cwd.is_empty() {
                    self.env.cwd = Arc::from(PathBuf::from(cwd));
                }
                self.env.vars = Arc::new(vars);
            }
            ClientMsg::Alias { aliases } => self.env.aliases = Arc::new(aliases),
            ClientMsg::Opts {
                hist_ignore_space,
                cols,
                histfile,
            } => {
                self.hist_ignore_space = hist_ignore_space;
                if histfile.is_some() {
                    self.histfile = histfile;
                }
                if let Some(c) = cols {
                    self.cols = c.max(MIN_COLS);
                }
            }
            ClientMsg::Query { id, cursor, buffer } => {
                self.latest_req_id.store(id, Ordering::SeqCst);
                self.query(id, cursor, &buffer);
            }
            ClientMsg::Accept { row_index, .. } => self.record_accept(row_index),
            ClientMsg::Explain { id, cursor, buffer } => self.explain(id, cursor, &buffer),
            ClientMsg::Recall { id } => {
                self.latest_req_id.store(id, Ordering::SeqCst);
                self.recall(id);
            }
            ClientMsg::Pane { id, row_index, insert } => self.pane(id, row_index, &insert),
            ClientMsg::Native { id, rows } => {
                self.latest_req_id.store(id, Ordering::SeqCst);
                self.native_rows(id, &rows);
            }
            ClientMsg::Translate { id, line } => self.translate(id, &line),
            ClientMsg::Exec { cwd, line } => self.last_exec = Some((cwd, line, std::time::Instant::now())),
            ClientMsg::Status { exit_code } => {
                if let Some((cwd, line, started)) = self.last_exec.take() {
                    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    self.shared.store.record_execution(
                        &cwd,
                        &line,
                        exit_code,
                        now_unix_ts(),
                        duration_ms,
                        self.hist_ignore_space,
                    );
                }
            }
            ClientMsg::Set { overrides } => {
                self.overrides = overrides;
                self.send(encode_settings(&SessionSettings::from_config(&self.shared.config)));
            }
            ClientMsg::Ping => {
                let (cmds, recipes) = self
                    .shared
                    .index
                    .read()
                    .map(|idx| (idx.cmds.len(), idx.recipes.len()))
                    .unwrap_or_default();
                let pid = std::process::id();
                self.send(encode_pong(Pong { pid, cmds, recipes }));
            }
            ClientMsg::Shutdown => {
                self.send("BYE\n".to_owned());
                let _ = self.shared.shutdown_tx.send(true);
                return false;
            }
        }
        true
    }

    /// `row_index` is 1-based; history and translation rows aren't usage of a spec token.
    fn record_accept(&self, row_index: usize) {
        let Some(cand) = self
            .last_candidates
            .get(row_index.saturating_sub(1))
            .filter(|c| !matches!(c.kind, CandidateKind::History | CandidateKind::Translate | CandidateKind::Translated))
        else {
            return;
        };
        self.shared.store.record_accept(
            &self.last_usage_ctx,
            &cand.label,
            cand.kind.as_str(),
            &self.env.cwd.to_string_lossy(),
            now_unix_ts(),
        );
    }

    fn request<'a>(
        &'a self,
        index: &'a SpecIndex,
        buffer: &'a str,
        cursor: usize,
        allow_stale: bool,
    ) -> CompletionRequest<'a> {
        CompletionRequest {
            line: buffer,
            cursor,
            cwd: &self.env.cwd,
            env: &self.env.vars,
            aliases: &self.env.aliases,
            wrappers: &self.shared.config.line.wrappers,
            index,
            runner: self.shared.runner.as_ref(),
            cache: &self.shared.cache,
            usage: &self.shared.store,
            timeout: self.shared.config.generator_timeout_duration(),
            now_ts: now_unix_ts(),
            allow_stale,
        }
    }

    fn query(&mut self, id: u64, cursor: usize, buffer: &str) {
        let settings = self.settings();
        if is_comment_line(buffer) {
            self.comment_query(id, buffer, &settings);
            return;
        }
        if below_min_chars(buffer, settings.min_chars) {
            // Still answer, so the client clears what it showed for the previous buffer.
            self.last_candidates.clear();
            let max_rows = settings.rows;
            let wire = build_wire_response(id, cursor, cursor, "", &[], self.cols, self.glyphs(), max_rows);
            self.send(encode_wire_response(&wire));
            return;
        }
        let Ok(index) = self.shared.index.read().map(|g| Arc::clone(&g)) else {
            return;
        };
        let req = self.request(&index, buffer, cursor, true);
        let mut comp = complete_line(&req);
        let preview = if settings.preview { self.preview(&req) } else { Vec::new() };
        let _ = self.apply_native_fallback(&mut comp, buffer);

        let cwd = self.env.cwd.to_string_lossy().into_owned();
        let at_end = cursor == buffer.chars().count();
        let history_ok = at_end
            && !buffer.trim().is_empty()
            && !(self.hist_ignore_space && buffer.starts_with(' '));
        let file_entries = history_ok.then(|| self.histfile_entries()).flatten();
        let file_entries = file_entries.as_deref().map(Vec::as_slice);
        let hist_rows = if history_ok {
            self.history_rows(&comp, buffer, &cwd, file_entries, &settings)
        } else {
            Vec::new()
        };
        let ghost = self.ghost(buffer, cursor, &cwd, file_entries);

        let refreshes = std::mem::take(&mut comp.pending_refreshes);
        let bg_hist = (!refreshes.is_empty()).then(|| hist_rows.clone());
        let max_rows = settings.rows;
        merge_history(&mut comp.candidates, hist_rows, max_rows);

        let wire = build_wire_response(
            id,
            comp.replace_start,
            comp.replace_end,
            &ghost,
            &comp.candidates,
            self.cols,
            self.glyphs(),
            max_rows,
        )
        .with_diagnostics(if settings.diagnostics { &comp.diagnostics } else { &[] })
        .with_preview(&preview)
        .with_signature(comp.signature.as_ref().filter(|_| settings.signature))
        .with_pane(&comp.candidates, &self.overrides.pane_options(&self.shared.config, self.cols, self.glyphs()));
        self.send(encode_wire_response(&wire));
        self.last_candidates = comp.candidates;
        self.last_usage_ctx = comp.usage_context;

        if let Some(hist_rows) = bg_hist {
            self.spawn_refresh(BackgroundRefresh {
                id,
                cursor,
                buffer: buffer.to_owned(),
                ghost,
                hist_rows,
                refreshes,
                index,
                settings,
                overrides: self.overrides,
            });
        }
    }
}

impl Session {
    /// A comment line gets no completions, history or diagnostics: only the "translate to
    /// command" row when `[complete.translate]` is configured and the comment has text.
    fn comment_query(&mut self, id: u64, buffer: &str, settings: &SessionSettings) {
        let opts = self.shared.config.translate_options();
        self.last_candidates = comment_query(buffer)
            .filter(|_| opts.enabled())
            .map(|query| vec![translate_row(query, &opts)])
            .unwrap_or_default();
        self.last_usage_ctx.clear();
        let len = buffer.chars().count();
        let wire = build_wire_response(id, 0, len, "", &self.last_candidates, self.cols, self.glyphs(), settings.rows);
        self.send(encode_wire_response(&wire));
    }
}

impl Session {
    /// Answers `EXPLAIN` with the rendered explanation of the line (`ezlo explain`).
    fn explain(&self, id: u64, cursor: usize, buffer: &str) {
        let Ok(index) = self.shared.index.read().map(|g| Arc::clone(&g)) else {
            return;
        };
        let req = self.request(&index, buffer, cursor, true);
        let mut comp = complete_line(&req);
        let native_applied = self.apply_native_fallback(&mut comp, buffer);
        let mut explanation = explain_line(&req, &comp, native_applied);
        explanation.preview = self.preview(&req);
        let lines = explanation.render();
        self.send(encode_explain(&WireExplain::from_lines(id, &lines)));
    }
}

impl Session {
    /// Blast-radius / dry-run preview of the line (`[preview]`; empty when disabled).
    fn preview(&self, req: &CompletionRequest<'_>) -> Vec<PreviewLine> {
        self.shared
            .config
            .preview_options()
            .map(|opts| preview_line(req, &opts))
            .unwrap_or_default()
    }
}

/// A first word shorter than `min_chars` gets no suggestions; any space lifts the limit.
fn below_min_chars(buffer: &str, min_chars: usize) -> bool {
    let typed = buffer.trim_start();
    !typed.contains(char::is_whitespace) && typed.chars().count() < min_chars
}

#[cfg(test)]
mod tests {
    use super::below_min_chars;

    #[test]
    fn min_chars_applies_to_the_first_word_only() {
        assert!(below_min_chars("l", 3));
        assert!(below_min_chars("  ls", 3));
        assert!(!below_min_chars("git", 3));
        assert!(!below_min_chars("l ", 3));
        assert!(!below_min_chars("l", 0));
    }
}
