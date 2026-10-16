//! Fallback to the persistent native zsh completion worker.

use super::Session;
use crate::config::Source;
use crate::engine::rank::filter_and_rank_candidates;
use crate::engine::CompletionResult;
use crate::store::now_unix_ts;
use std::time::Duration;

const NATIVE_TIMEOUT: Duration = Duration::from_millis(300);

impl Session {
    /// Commands without a spec fall back to the persistent native zsh worker. Returns whether the
    /// candidates were replaced by native ones.
    pub(super) fn apply_native_fallback(&self, comp: &mut CompletionResult, buffer: &str) -> bool {
        if comp.known_root || !self.shared.config.has_source(Source::Native) || buffer.trim().is_empty() {
            return false;
        }
        let native = self.shared.native_pool.complete(buffer, &self.env.cwd, NATIVE_TIMEOUT);
        if native.is_empty() {
            return false;
        }
        let prefix: String = buffer
            .chars()
            .skip(comp.replace_start)
            .take(comp.replace_end.saturating_sub(comp.replace_start))
            .collect();
        let ranked = filter_and_rank_candidates(
            native,
            &prefix,
            &comp.usage_context,
            &self.env.cwd.to_string_lossy(),
            &self.shared.store,
            now_unix_ts(),
        );
        if ranked.is_empty() {
            return false;
        }
        comp.candidates = ranked;
        true
    }
}
