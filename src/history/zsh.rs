//! zsh history specifics: metafied bytes and `EXTENDED_HISTORY` lines.

/// Home-relative zsh history files, probed when the shell didn't send `HISTFILE`.
pub const DEFAULT_FILES: &[&str] = &[".zsh_history", ".histfile"];

/// zsh stores bytes >= 0x83 "metafied": `0x83` followed by `byte ^ 0x20`.
pub fn unmetafy(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter();
    while let Some(&b) = it.next() {
        if b == 0x83 {
            if let Some(&n) = it.next() {
                out.push(n ^ 0x20);
            }
        } else {
            out.push(b);
        }
    }
    out
}

/// `: 1700000000:0;cmd` → `(1700000000, "cmd")`.
pub fn split_extended(line: &str) -> Option<(i64, &str)> {
    let rest = line.strip_prefix(": ")?;
    let (meta, cmd) = rest.split_once(';')?;
    let ts = meta.split(':').next()?.trim().parse::<i64>().ok()?;
    Some((ts, cmd))
}
