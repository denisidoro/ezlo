//! `PANE`: the preview pane of one dropdown row, computed off the async runtime.

use super::Session;
use crate::daemon::protocol::{encode_pane, WirePane};
use crate::engine::preview::pane::{candidate_pane, pane_availability, PaneAvailability, PaneContext};
use std::sync::atomic::Ordering;
use std::sync::Arc;

impl Session {
    /// Answers `PANE` for row `row_index` (1-based) of response `id` with a `V` reply. The row is
    /// looked up by its insert text when the index no longer matches (a background refresh may
    /// have reordered rows). The preview runs on a blocking thread (type `preview` commands are
    /// killed after `[complete.pane] timeout`) and is dropped if a newer query arrived meanwhile.
    pub(super) fn pane(&self, id: u64, row_index: usize, insert: &str) {
        let latest = Arc::clone(&self.latest_req_id);
        if latest.load(Ordering::SeqCst) != id {
            return;
        }
        let opts = self.overrides.pane_options(&self.shared.config, self.cols, self.glyphs());
        let cand = self
            .last_candidates
            .get(row_index.saturating_sub(1))
            .filter(|c| c.insert_text == insert)
            .or_else(|| self.last_candidates.iter().find(|c| c.insert_text == insert))
            .filter(|c| pane_availability(c, &opts) != PaneAvailability::None)
            .cloned();
        let Some(cand) = cand else {
            self.send(encode_pane(&WirePane::from_lines(id, row_index, opts.lines, &[])));
            return;
        };
        let shared = Arc::clone(&self.shared);
        let env = self.env.clone();
        let out = self.out.clone();
        tokio::task::spawn_blocking(move || {
            let ctx = PaneContext {
                cwd: &env.cwd,
                env: &env.vars,
                runner: shared.runner.as_ref(),
                cache: &shared.cache,
            };
            let lines = candidate_pane(&cand, &ctx, &opts);
            if latest.load(Ordering::SeqCst) == id {
                let _ = out.send(encode_pane(&WirePane::from_lines(id, row_index, opts.lines, &lines)));
            }
        });
    }
}
