//! Shell integration. ezlo targets zsh only: daemon-backed live dropdown, ghost text and history
//! recording, all in `zsh.rs` (`ezlo init` script) and `zsh/native.rs` (fallback completer).

pub mod zsh;

use crate::engine::CompletionResult;
use std::fmt::Write as _;

/// Renders `ezlo complete` output as plain TSV (`insert_value \t label \t description`; zsh
/// itself uses the daemon, not this command).
pub fn format_completions(result: &CompletionResult) -> String {
    let mut out = String::new();
    for c in &result.candidates {
        let _ = writeln!(out, "{}\t{}\t{}", c.insert_value, c.label, c.description);
    }
    out
}
