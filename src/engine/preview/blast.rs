//! Blast radius of file-mutating commands (`rm`, `mv`, `cp`, `chmod`, `chown`, …): which paths the
//! operands (globs expanded) resolve to, how many files/directories that is and their total size.

use super::glob::{parse_word, Budget, Word};
use super::{PreviewKind, PreviewLine};
use crate::engine::line::ParsedLine;
use crate::engine::walker::{WalkState, WordRoleKind};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How a mutating command uses its operands.
struct Shape {
    /// Leading operands that aren't paths (`chmod MODE`, `chown OWNER`).
    skip_leading: usize,
    /// The last operand is a destination, not an affected path (`mv`, `cp`).
    dest_last: bool,
}

fn shape(cmd: &str) -> Shape {
    match cmd {
        "chmod" | "chown" | "chgrp" => Shape {
            skip_leading: 1,
            dest_last: false,
        },
        "mv" | "cp" => Shape {
            skip_leading: 0,
            dest_last: true,
        },
        _ => Shape {
            skip_leading: 0,
            dest_last: false,
        },
    }
}

#[derive(Debug, Default)]
struct Tally {
    files: usize,
    dirs: usize,
    others: usize,
    bytes: u64,
    /// Entries found inside the affected directories (recursively).
    nested: usize,
    truncated: bool,
}

/// Effect lines for `parsed`/`walk` when its command is one of `commands`. Operands are the
/// positional words (options and their values excluded).
pub(super) fn blast_radius(
    parsed: &ParsedLine,
    walk: &WalkState<'_>,
    commands: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    budget: &mut Budget,
) -> Vec<PreviewLine> {
    let Some(cmd_word) = parsed.words.get(parsed.cmd_word_idx) else {
        return Vec::new();
    };
    let cmd = cmd_word.unquoted.rsplit('/').next().unwrap_or_default();
    if !commands.iter().any(|c| c == cmd) {
        return Vec::new();
    }
    let shape = shape(cmd);
    let mut operands: Vec<&str> = walk
        .word_roles
        .iter()
        .filter(|r| matches!(r.kind, WordRoleKind::Positional { .. }))
        .filter_map(|r| parsed.words.get(r.word_idx))
        .map(|w| w.raw.as_str())
        .skip(shape.skip_leading)
        .collect();
    let dest = if shape.dest_last && operands.len() >= 2 { operands.pop() } else { None };
    if operands.is_empty() {
        return Vec::new();
    }

    let mut affected: Vec<PathBuf> = Vec::new();
    let mut no_match: Vec<&str> = Vec::new();
    let mut truncated = false;
    for raw in operands {
        match parse_word(raw, env) {
            Word::Literal(path) => affected.push(PathBuf::from(path)),
            Word::Glob(pattern) => {
                let expansion = pattern.expand(cwd, budget);
                truncated |= expansion.truncated;
                if expansion.paths.is_empty() && !expansion.truncated {
                    no_match.push(raw);
                }
                affected.extend(expansion.paths);
            }
            Word::Unsupported => {}
        }
    }

    let mut tally = Tally {
        truncated,
        ..Tally::default()
    };
    let mut seen = std::collections::HashSet::new();
    for path in &affected {
        let abs = cwd.join(path);
        if !seen.insert(abs.clone()) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&abs) else { continue };
        if meta.is_dir() {
            tally.dirs += 1;
            measure_dir(&abs, budget, &mut tally);
        } else if meta.is_file() {
            tally.files += 1;
            tally.bytes += meta.len();
        } else {
            tally.others += 1;
        }
    }
    tally.truncated |= budget.exhausted();

    let mut out = Vec::new();
    let total = tally.files + tally.dirs + tally.others;
    if total > 0 {
        out.push(PreviewLine {
            kind: PreviewKind::Effect,
            text: summary(cmd, &tally, dest),
        });
    }
    if !no_match.is_empty() {
        out.push(PreviewLine {
            kind: PreviewKind::Warning,
            text: format!("{cmd}: no matches found: {}", no_match.join(" ")),
        });
    }
    out
}

/// Adds everything below `dir` (not following symlinks) to `tally`, within `budget`.
fn measure_dir(dir: &Path, budget: &mut Budget, tally: &mut Tally) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            if !budget.spend() {
                tally.truncated = true;
                return;
            }
            tally.nested += 1;
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                // Virtual filesystems have no real size (`/proc/kcore` alone reports ~128 TB).
                let path = entry.path();
                if !matches!(path.to_str(), Some("/proc" | "/sys" | "/dev" | "/run")) {
                    stack.push(path);
                }
            } else if meta.is_file() {
                tally.bytes += meta.len();
            }
        }
    }
}

/// `rm: 12 files, 2 dirs (+340 inside) · 3.4 MB` (`≥` when the budget ran out).
fn summary(cmd: &str, t: &Tally, dest: Option<&str>) -> String {
    let at_least = if t.truncated { "≥" } else { "" };
    let mut parts = Vec::new();
    if t.files > 0 {
        parts.push(plural(t.files, "file"));
    }
    if t.dirs > 0 {
        let mut dirs = plural(t.dirs, "dir");
        if t.nested > 0 {
            dirs.push_str(&format!(" (+{at_least}{} inside)", t.nested));
        }
        parts.push(dirs);
    }
    if t.others > 0 {
        parts.push(plural(t.others, "other"));
    }
    let mut text = format!("{cmd}: {} · {at_least}{}", parts.join(", "), format_size(t.bytes));
    if let Some(d) = dest {
        text.push_str(&format!(" → {d}"));
    }
    text
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// SI size (`999 B`, `1.2 kB`, `3.4 MB`, …).
pub(super) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn sizes_are_si() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1000), "1.0 kB");
        assert_eq!(format_size(3_400_000), "3.4 MB");
        assert_eq!(format_size(999_960), "1.0 MB");
    }
}
