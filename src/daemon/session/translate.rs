//! `TRANSLATE`: a comment line turned into suggested commands by the `[complete.translate]`
//! translator, run off the async runtime. Success is answered with an `R` response (one
//! `translated` row per command, replacing the whole line) immediately followed by `T ok`;
//! failure with a single `T err`.

use super::Session;
use crate::daemon::protocol::{
    build_wire_response, encode_translate, encode_wire_response, TranslateState, WireTranslate,
};
use crate::engine::translate::{run_translator, suggestion_rows, translate_query, TranslateContext};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

impl Session {
    /// Starts translating `line` (cancelling any translation still running); an empty `line`
    /// only cancels. The child is killed on cancel, timeout or when the client goes away.
    pub(super) fn translate(&mut self, id: u64, line: &str) {
        self.cancel_translation();
        if line.trim().is_empty() {
            return;
        }
        let reply = move |state, lines| encode_translate(&WireTranslate { id, state, lines });
        let query = translate_query(line).to_owned();
        let opts = self.shared.config.translate_options();
        if query.is_empty() {
            self.send(reply(TranslateState::Err, vec!["nothing to translate".to_owned()]));
            return;
        }
        if !opts.enabled() {
            let msg = "not configured: set [complete.translate] command in config.toml";
            self.send(reply(TranslateState::Err, vec![msg.to_owned()]));
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.translate_cancel = Some(Arc::clone(&cancel));
        let env = self.env.clone();
        let out = self.out.clone();
        let (cols, glyphs, max_rows) = (self.cols, self.glyphs(), self.settings().rows);
        let line_len = line.chars().count();
        tokio::task::spawn_blocking(move || {
            let ctx = TranslateContext {
                cwd: &env.cwd,
                env: &env.vars,
                cancel: &cancel,
            };
            let result = run_translator(&opts, &query, &ctx);
            // A cancelled translation stays silent: the client already moved on.
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let msg = match result {
                Ok(commands) => {
                    // One write, so the client sees the rows and the end of the translation together.
                    let rows = suggestion_rows(&commands);
                    let wire = build_wire_response(id, 0, line_len, "", &rows, cols, glyphs, max_rows);
                    encode_wire_response(&wire) + &reply(TranslateState::Ok, commands)
                }
                Err(err) => reply(TranslateState::Err, vec![err]),
            };
            let _ = out.send(msg);
        });
    }

    /// Kills the running translator, if any.
    pub(super) fn cancel_translation(&mut self) {
        if let Some(cancel) = self.translate_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Session {
    /// A shell that disconnects mid-translation doesn't keep its translator running.
    fn drop(&mut self) {
        self.cancel_translation();
    }
}
