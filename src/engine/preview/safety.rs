//! Safety warnings for the line being typed, independent of specs and computed in-process:
//!
//! - destructive commands aimed at `/`, `$HOME`, a top-level system directory or a glob over them
//!   (`rm -rf ~/*`, `chmod -R 777 /`), including through an unset parameter (`rm -rf $DIR/`);
//! - operands of destructive commands that use a parameter that isn't exported or is empty;
//! - `> file` redirections and `cp`/`mv` destinations that would overwrite an existing file;
//! - `git push --force` (without `--force-with-lease`), `dd of=/dev/…`, `mkfs`, `chmod 777`.

use super::glob::Budget;
use super::instant::expand::{expand_word, Expanded};
use super::{PreviewKind, PreviewLine};
use crate::engine::line::{split_redirects, ParsedLine};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Commands whose path operands are destroyed or rewritten.
const DESTRUCTIVE: [&str; 9] = ["rm", "rmdir", "mv", "cp", "chmod", "chown", "chgrp", "rsync", "shred"];
/// Top-level directories no recursive command should touch as a whole.
const SYSTEM_DIRS: [&str; 16] = [
    "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib64", "/opt", "/proc", "/root", "/sbin", "/sys",
    "/usr", "/var", "/System", "/Users",
];

fn warn(text: String) -> PreviewLine {
    PreviewLine { kind: PreviewKind::Warning, text }
}

/// Safety warnings for the command in `parsed` (the innermost one, wrappers already skipped).
pub(super) fn safety_warnings(
    parsed: &ParsedLine,
    cwd: &Path,
    env: &HashMap<String, String>,
    budget: &mut Budget,
) -> Vec<PreviewLine> {
    let Some(cmd_word) = parsed.words.get(parsed.cmd_word_idx) else {
        return Vec::new();
    };
    let cmd = cmd_word.unquoted.rsplit('/').next().unwrap_or_default();
    let words: Vec<&str> = parsed.words[parsed.cmd_word_idx + 1..]
        .iter()
        .map(|w| w.raw.as_str())
        .filter(|w| !w.is_empty())
        .collect();
    let (args, redirects) = split_redirects(&words);
    let mut out = Vec::new();

    let opts: Vec<&str> = args.iter().copied().filter(|w| w.starts_with('-') && *w != "-").collect();
    let operands: Vec<&str> = args.iter().copied().filter(|w| !w.starts_with('-')).collect();
    let recursive = opts.iter().any(|o| {
        o.strip_prefix("--").map_or_else(|| o.contains(['r', 'R']), |l| l == "recursive")
    });

    if DESTRUCTIVE.contains(&cmd) {
        let skip = usize::from(matches!(cmd, "chmod" | "chown" | "chgrp"));
        for raw in operands.iter().skip(skip) {
            if let Some(name) = unset_parameter(raw, env) {
                out.push(warn(format!("{cmd}: ${name} is not exported or empty here: `{raw}` may expand to something else")));
            }
            let strict = matches!(cmd, "rm" | "rmdir" | "shred") || recursive;
            if let Some(what) = strict.then(|| dangerous_target(raw, cwd, env, budget)).flatten() {
                out.push(warn(format!("{cmd}: targets {what}")));
            }
        }
        if cmd == "chmod" && operands.first().is_some_and(|m| m.ends_with("777") || m.ends_with("666") || *m == "o+w" || *m == "a+w") {
            out.push(warn("chmod: makes files world-writable".into()));
        }
        if matches!(cmd, "cp" | "mv") {
            out.extend(overwrites(cmd, &opts, &operands, cwd, env, budget));
        }
    }
    match cmd {
        "git" if words.first() == Some(&"push") => {
            let force = words.iter().any(|w| *w == "--force" || *w == "-f" || (w.starts_with('+') && w.len() > 1));
            let lease = words.iter().any(|w| w.starts_with("--force-with-lease") || w.starts_with("--force-if-includes"));
            if force && !lease {
                out.push(warn("git push: force-push rewrites remote history (prefer --force-with-lease)".into()));
            }
        }
        "dd" => {
            if let Some(dev) = words.iter().find_map(|w| w.strip_prefix("of=")).filter(|d| d.starts_with("/dev/") && !matches!(*d, "/dev/null" | "/dev/zero" | "/dev/stdout" | "/dev/stderr")) {
                out.push(warn(format!("dd: writes raw data to the device {dev}")));
            }
        }
        c if c.starts_with("mkfs") => out.push(warn(format!("{c}: formats a filesystem (erases the device)"))),
        _ => {}
    }
    for (op, target) in redirects {
        if op.contains(">>") || target.starts_with("/dev/") {
            continue;
        }
        if let Expanded::Fields(f) = expand_word(target, env, cwd, budget) {
            let Some(path) = f.first() else { continue };
            if let Some(meta) = fs::metadata(cwd.join(path)).ok().filter(|m| m.is_file() && m.len() > 0) {
                out.push(warn(format!("`{op}` overwrites {path} ({})", super::blast::format_size(meta.len()))));
            }
        }
    }
    out
}

/// The first parameter of `raw` (outside single quotes) that isn't exported or is empty.
fn unset_parameter(raw: &str, env: &HashMap<String, String>) -> Option<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut in_single = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\'' => in_single = !in_single,
            '\\' => i += 1,
            '$' if !in_single => {
                let braced = chars.get(i + 1) == Some(&'{');
                let from = i + 1 + usize::from(braced);
                let name: String = chars[from.min(chars.len())..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .collect();
                // `${x:-default}` and friends handle the unset case themselves.
                let has_default = braced && chars.get(from + name.len()).is_some_and(|c| *c != '}');
                if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && !has_default
                    && env.get(&name).is_none_or(String::is_empty)
                {
                    return Some(name);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Describes `raw` when it resolves to `/`, `$HOME`, a system directory, or a glob over one of
/// them (`~/*`), or to everything in the cwd when the cwd is one of them (`rm -rf *` in `~`).
fn dangerous_target(raw: &str, cwd: &Path, env: &HashMap<String, String>, budget: &mut Budget) -> Option<String> {
    // Expand first: `$UNSET/` is `/`, so slashes only get trimmed afterwards.
    let base = raw.trim_end_matches('*');
    let globbed = base.len() != raw.len();
    let expanded = if base.is_empty() {
        String::new()
    } else {
        match expand_word(base, env, cwd, budget) {
            Expanded::Fields(f) if f.len() == 1 => f.into_iter().next().unwrap_or_default(),
            _ => return None,
        }
    };
    let trimmed = expanded.trim_end_matches('/');
    let resolved = if !expanded.is_empty() && trimmed.is_empty() {
        PathBuf::from("/")
    } else if trimmed.is_empty() {
        // An empty word only matters as a glob base (`*` = everything in the cwd).
        if !globbed {
            return None;
        }
        cwd.to_path_buf()
    } else {
        normalize(&cwd.join(trimmed))
    };
    let home = env.get("HOME").filter(|h| h.len() > 1).map(|h| normalize(Path::new(h)));
    let what = if resolved == Path::new("/") {
        "the root directory /".to_string()
    } else if home.as_deref() == Some(resolved.as_path()) {
        "your home directory".to_string()
    } else if resolved.parent() == Some(Path::new("/")) && SYSTEM_DIRS.iter().any(|d| Path::new(d) == resolved) {
        format!("the system directory {}", resolved.display())
    } else {
        return None;
    };
    Some(if globbed { format!("everything in {what}") } else { what })
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `cp`/`mv` destinations that already exist as files (unless `-n`/`-i`/`-b`/`-u` protect them).
fn overwrites(
    cmd: &str,
    opts: &[&str],
    operands: &[&str],
    cwd: &Path,
    env: &HashMap<String, String>,
    budget: &mut Budget,
) -> Vec<PreviewLine> {
    let protected = opts.iter().any(|o| match o.strip_prefix("--") {
        Some(l) => matches!(l, "no-clobber" | "interactive" | "backup" | "update") || l.starts_with("backup=") || l.starts_with("update="),
        None => o.contains(['n', 'i', 'b', 'u']),
    });
    if protected || operands.len() < 2 || opts.iter().any(|o| *o == "-t" || o.starts_with("--target-directory")) {
        return Vec::new();
    }
    let expand = |raw: &str, budget: &mut Budget| match expand_word(raw, env, cwd, budget) {
        Expanded::Fields(f) => Some(f),
        _ => None,
    };
    let Some(dest) = expand(operands[operands.len() - 1], budget).filter(|f| f.len() == 1) else {
        return Vec::new();
    };
    let dest = cwd.join(&dest[0]);
    let mut hit = Vec::new();
    if dest.is_dir() {
        for raw in &operands[..operands.len() - 1] {
            for src in expand(raw, budget).unwrap_or_default() {
                let Some(name) = Path::new(src.trim_end_matches('/')).file_name() else { continue };
                let target = dest.join(name);
                if target.is_file() && cwd.join(&src) != target {
                    hit.push(target);
                }
            }
        }
    } else if dest.is_file() && operands.len() == 2 {
        hit.push(dest);
    }
    if hit.is_empty() {
        return Vec::new();
    }
    let rel = |p: &PathBuf| p.strip_prefix(cwd).unwrap_or(p).display().to_string();
    let mut names: Vec<String> = hit.iter().take(3).map(rel).collect();
    if hit.len() > 3 {
        names.push(format!("+{} more", hit.len() - 3));
    }
    vec![warn(format!("{cmd}: overwrites {}", names.join(", ")))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_parameters() {
        let env = HashMap::from([("SET".to_string(), "x".to_string()), ("EMPTY".to_string(), String::new())]);
        assert_eq!(unset_parameter("$NOPE/x", &env).as_deref(), Some("NOPE"));
        assert_eq!(unset_parameter("\"${EMPTY}\"", &env).as_deref(), Some("EMPTY"));
        assert_eq!(unset_parameter("$SET/x", &env), None);
        assert_eq!(unset_parameter("'$NOPE'", &env), None);
        assert_eq!(unset_parameter("${NOPE:-/tmp}", &env), None);
    }
}
