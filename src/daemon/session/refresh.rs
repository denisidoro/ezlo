//! Background re-run of stale generators, re-sending the response when it is still current.

use super::Session;
use crate::daemon::protocol::{build_wire_response, encode_wire_response, SessionOverrides, SessionSettings};
use crate::engine::candidate::Candidate;
use crate::engine::generator::parse_tsv_rows;
use crate::engine::preview::preview_line;
use crate::engine::types::PendingRefresh;
use crate::engine::{complete_line, CompletionRequest};
use crate::history::merge_history;
use crate::spec::SpecIndex;
use crate::store::now_unix_ts;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub(super) struct BackgroundRefresh {
    pub(super) id: u64,
    pub(super) cursor: usize,
    pub(super) buffer: String,
    pub(super) ghost: String,
    pub(super) hist_rows: Vec<Candidate>,
    pub(super) refreshes: Vec<PendingRefresh>,
    pub(super) index: Arc<SpecIndex>,
    /// The session's effective settings when the query ran.
    pub(super) settings: SessionSettings,
    /// The session's pane overrides when the query ran.
    pub(super) overrides: SessionOverrides,
}

impl Session {
    /// Re-runs stale generators off the async runtime, then re-sends the response for `id` unless a
    /// newer query superseded it meanwhile.
    pub(super) fn spawn_refresh(&self, job: BackgroundRefresh) {
        let shared = Arc::clone(&self.shared);
        let latest = Arc::clone(&self.latest_req_id);
        let env = self.env.clone();
        let out = self.out.clone();
        let cols = self.cols;

        tokio::task::spawn_blocking(move || {
            let current = || latest.load(Ordering::SeqCst) == job.id;
            let timeout = shared.config.generator_timeout_duration();
            for item in job.refreshes {
                if !current() {
                    return;
                }
                let lines = shared.runner.run_shell(
                    &item.command,
                    &env.cwd,
                    &env.vars,
                    &item.cache_key.params,
                    timeout,
                );
                shared.cache.insert(
                    item.cache_key,
                    parse_tsv_rows(&lines, item.num_cols),
                    item.ttl,
                );
            }
            if !current() {
                return;
            }
            let req = CompletionRequest {
                line: &job.buffer,
                cursor: job.cursor,
                cwd: &env.cwd,
                env: &env.vars,
                aliases: &env.aliases,
                wrappers: &shared.config.line.wrappers,
                index: &job.index,
                runner: shared.runner.as_ref(),
                cache: &shared.cache,
                usage: &shared.store,
                timeout,
                now_ts: now_unix_ts(),
                allow_stale: false,
            };
            let mut fresh = complete_line(&req);
            let settings = job.settings;
            let preview = shared
                .config
                .preview_options()
                .filter(|_| settings.preview)
                .map(|opts| preview_line(&req, &opts))
                .unwrap_or_default();
            merge_history(&mut fresh.candidates, job.hist_rows, settings.rows);
            if !current() {
                return;
            }
            let wire = build_wire_response(
                job.id,
                fresh.replace_start,
                fresh.replace_end,
                &job.ghost,
                &fresh.candidates,
                cols,
                settings.rows,
            )
            .with_diagnostics(if settings.diagnostics { &fresh.diagnostics } else { &[] })
            .with_preview(&preview)
            .with_signature(fresh.signature.as_ref().filter(|_| settings.signature))
            .with_pane(&fresh.candidates, &job.overrides.pane_options(&shared.config, cols));
            let _ = out.send(encode_wire_response(&wire));
        });
    }
}
