//! zsh history parsing: plain lines or `EXTENDED_HISTORY` (`: <ts>:<dur>;<cmd>`), metafied.

use super::{zsh, HistEntry};

/// Parses a zsh history file. Multi-line entries are dropped since they can't be typed on
/// a single-line buffer. Returns entries in file (chronological) order.
pub fn parse_history(bytes: &[u8], fallback_ts: i64) -> Vec<HistEntry> {
    let text = String::from_utf8_lossy(&zsh::unmetafy(bytes)).into_owned();
    parse_lines(&text, fallback_ts)
}

fn parse_lines(text: &str, fallback_ts: i64) -> Vec<HistEntry> {
    let mut out = Vec::new();
    let mut continuing = false;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if continuing {
            continuing = line.ends_with('\\');
            continue;
        }
        if line.ends_with('\\') {
            continuing = true;
            continue;
        }
        let (ts, cmd) = zsh::split_extended(line).unwrap_or((fallback_ts, line));
        push_entry(&mut out, ts, cmd);
    }
    out
}

pub(super) fn push_entry(out: &mut Vec<HistEntry>, ts: i64, cmd: &str) {
    let cmd = cmd.trim_end();
    if cmd.trim().is_empty() || cmd.contains('\x1f') {
        return;
    }
    out.push(HistEntry {
        ts,
        line: cmd.to_owned(),
    });
}
