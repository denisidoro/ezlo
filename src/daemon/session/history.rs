//! History rows and ghost-text suggestions for a query.

use super::Session;
use crate::config::GhostMode;
use crate::daemon::protocol::{build_wire_response, encode_wire_response, SessionSettings};
use crate::engine::candidate::Candidate;
use crate::engine::{CompletionResult, MAX_CANDIDATES};
use crate::history::{file_matches, history_candidates, resolve_histfile, HistEntry};
use crate::store::now_unix_ts;
use std::sync::Arc;

impl Session {
    pub(super) fn histfile_entries(&self) -> Option<Arc<Vec<HistEntry>>> {
        let process_home;
        let home = match self.env.vars.get("HOME") {
            Some(h) => Some(h.as_str()),
            None => {
                process_home = std::env::var("HOME").ok();
                process_home.as_deref()
            }
        };
        resolve_histfile(self.histfile.as_deref(), home).map(|p| self.shared.histfiles.entries(&p))
    }

    /// ezlo's own history first (cwd-aware), then the shell's history file.
    pub(super) fn history_rows(
        &self,
        comp: &CompletionResult,
        buffer: &str,
        cwd: &str,
        file_entries: Option<&[HistEntry]>,
        settings: &SessionSettings,
    ) -> Vec<Candidate> {
        if settings.history_rows == 0 {
            return Vec::new();
        }
        let limit = if comp.candidates.is_empty() {
            settings.rows.max(1)
        } else {
            settings.history_rows
        };
        let local = self
            .shared
            .store
            .history_matches(buffer, cwd, self.hist_ignore_space, limit);
        // Lines ezlo saw run elsewhere than its own history (e.g. before it started) still show how
        // their last run went.
        let store = &self.shared.store;
        let from_file = file_entries
            .map(|e| file_matches(e, buffer, limit * 2))
            .unwrap_or_default()
            .into_iter()
            .map(|mut m| {
                m.last_run = store.last_run(&m.line);
                m
            });
        history_candidates(
            local.into_iter().chain(from_file),
            comp.replace_start,
            limit,
            now_unix_ts(),
        )
    }

    pub(super) fn ghost(
        &self,
        buffer: &str,
        cursor: usize,
        cwd: &str,
        file_entries: Option<&[HistEntry]>,
    ) -> String {
        if self.shared.config.complete.ghost.mode == GhostMode::Off {
            return String::new();
        }
        let from_store =
            self.shared
                .store
                .suggest_ghost(buffer, cursor, cwd, self.hist_ignore_space);
        if !from_store.is_empty() {
            return from_store;
        }
        file_entries
            .and_then(|e| file_matches(e, buffer, 1).pop())
            .and_then(|m| m.line.get(buffer.len()..).map(str::to_owned))
            .unwrap_or_default()
    }

    /// Answers `RECALL` (empty prompt): ezlo's most frecent lines for the cwd/repo, topped up with
    /// the newest lines of the shell's history file when ezlo hasn't seen enough yet.
    pub(super) fn recall(&mut self, id: u64) {
        let cwd = self.env.cwd.to_string_lossy().into_owned();
        let now = now_unix_ts();
        let mut matches = self
            .shared
            .store
            .workflow_matches(&cwd, self.hist_ignore_space, now, MAX_CANDIDATES);
        if matches.len() < MAX_CANDIDATES {
            if let Some(entries) = self.histfile_entries() {
                matches.extend(file_matches(&entries, "", MAX_CANDIDATES * 2));
            }
        }
        let rows = history_candidates(matches, 0, MAX_CANDIDATES, now);
        let max_rows = self.settings().rows;
        let wire = build_wire_response(id, 0, 0, "", &rows, self.cols, max_rows)
            .with_pane(&rows, &self.overrides.pane_options(&self.shared.config, self.cols));
        self.send(encode_wire_response(&wire));
        self.last_candidates = rows;
        self.last_usage_ctx.clear();
    }
}
