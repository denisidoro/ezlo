//! Instant previews of filesystem commands: where `cd` lands, what `mkdir`/`touch`/`ln` would
//! create or clobber, how much `cat`/`wc` would read, what `ls` lists and `file` types.

use super::super::blast::format_size;
use super::{effect, has_opt, split_opts, value, warning, Ctx};
use crate::engine::preview::PreviewLine;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Bytes read at most per preview to count lines/words (bigger inputs show only their size).
const MAX_READ: u64 = 1 << 20;
/// Paths named in one preview line at most.
const MAX_NAMED: usize = 3;

/// `cd [dir]` / `pushd [dir]`: the resolved target (`$HOME` without one, `$CDPATH` for bare names)
/// and how many entries it has; a warning when it isn't a directory.
pub(super) fn cd(ctx: &mut Ctx<'_>, cmd: &str, words: &[&str]) -> Vec<PreviewLine> {
    let (_, rest) = split_opts(words);
    let target = match rest.as_slice() {
        [] => match ctx.env.get("HOME") {
            Some(h) => h.clone(),
            None => return Vec::new(),
        },
        ["-"] => match ctx.env.get("OLDPWD") {
            Some(p) => p.clone(),
            None => return Vec::new(),
        },
        [one] => match ctx.expand_all([*one]) {
            Some(f) if f.len() == 1 => f.into_iter().next().unwrap_or_default(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let Some(dir) = resolve_cd(ctx, &target) else {
        if ctx.still_typing(&target) {
            return Vec::new();
        }
        return vec![warning(format!("{cmd}: no such directory: {target}"))];
    };
    let mut text = format!("{cmd} → {}", display(ctx, &dir));
    if let Some(n) = count_entries(&dir, false, ctx) {
        text.push_str(&format!(" · {}", plural(n, "entry", "entries")));
    }
    vec![effect(text)]
}

fn resolve_cd(ctx: &Ctx<'_>, target: &str) -> Option<PathBuf> {
    let direct = ctx.abs(target);
    if direct.is_dir() {
        return Some(normalize(&direct));
    }
    let relative_name = !target.starts_with(['/', '.']);
    let cdpath = ctx.env.get("CDPATH").filter(|_| relative_name)?;
    cdpath
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| ctx.abs(d).join(target))
        .find(|p| p.is_dir())
        .map(|p| normalize(&p))
}

/// Lexically removes `.` and `..` (like zsh's logical `cd`), without resolving symlinks.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `~/x` for paths under `$HOME`.
fn display(ctx: &Ctx<'_>, p: &Path) -> String {
    let s = p.to_string_lossy();
    match ctx.env.get("HOME").filter(|h| h.len() > 1) {
        Some(h) if s == h.as_str() => "~".into(),
        Some(h) => s.strip_prefix(&format!("{h}/")).map_or_else(|| s.to_string(), |r| format!("~/{r}")),
        None => s.into_owned(),
    }
}

fn count_entries(dir: &Path, hidden: bool, ctx: &mut Ctx<'_>) -> Option<usize> {
    let mut n = 0;
    for e in fs::read_dir(dir).ok()?.flatten() {
        if !ctx.budget.spend() {
            return None;
        }
        if hidden || !e.file_name().to_string_lossy().starts_with('.') {
            n += 1;
        }
    }
    Some(n)
}

/// `mkdir [-p] dir…`: which directories would be created, and the ones that would fail.
pub(super) fn mkdir(ctx: &mut Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    let parents = has_opt(&opts, 'p', "parents");
    let Some(dirs) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    let (mut create, mut exists, mut orphan) = (Vec::new(), Vec::new(), Vec::new());
    for d in dirs {
        let p = ctx.abs(&d);
        if p.exists() {
            exists.push(d);
        } else if !parents && !p.parent().is_some_and(Path::is_dir) {
            orphan.push(d);
        } else {
            create.push(d);
        }
    }
    let mut out = Vec::new();
    if !create.is_empty() {
        out.push(effect(format!("mkdir: creates {}", names(&create))));
    }
    if !exists.is_empty() && !parents {
        out.push(warning(format!("mkdir: already exists: {}", names(&exists))));
    }
    if !orphan.is_empty() {
        out.push(warning(format!("mkdir: missing parent (use -p): {}", names(&orphan))));
    }
    out
}

/// `touch file…`: created vs. only re-timestamped files.
pub(super) fn touch(ctx: &mut Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    // `-d`/`-t`/`-r` take a value: too many shapes, skip.
    if opts.iter().any(|o| !o.starts_with("--") && o.contains(['d', 't', 'r'])) {
        return Vec::new();
    }
    let no_create = has_opt(&opts, 'c', "no-create");
    let Some(files) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    let (mut create, mut update, mut orphan) = (Vec::new(), Vec::new(), Vec::new());
    for f in files {
        let p = ctx.abs(&f);
        if p.exists() {
            update.push(f);
        } else if !p.parent().is_some_and(Path::is_dir) {
            orphan.push(f);
        } else if !no_create {
            create.push(f);
        }
    }
    let mut parts = Vec::new();
    if !create.is_empty() {
        parts.push(format!("creates {}", names(&create)));
    }
    if !update.is_empty() {
        parts.push(format!("updates the time of {}", names(&update)));
    }
    let mut out = Vec::new();
    if !parts.is_empty() {
        out.push(effect(format!("touch: {}", parts.join(" · "))));
    }
    if !orphan.is_empty() {
        out.push(warning(format!("touch: no such directory for {}", names(&orphan))));
    }
    out
}

/// `ln [-s] [-f] target [link]`: the link that would be made, a dangling symlink target and a
/// link path that already exists (without `-f`).
pub(super) fn ln(ctx: &mut Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    if opts.iter().any(|o| o.starts_with("--") && !matches!(*o, "--symbolic" | "--force")) {
        return Vec::new();
    }
    let symbolic = has_opt(&opts, 's', "symbolic");
    let force = has_opt(&opts, 'f', "force");
    let Some(args) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    let (target, link) = match args.as_slice() {
        [t] => (t.clone(), basename_of(t)),
        [t, l] => {
            let l = if ctx.abs(l).is_dir() && !fs::symlink_metadata(ctx.abs(l)).is_ok_and(|m| m.is_symlink()) {
                format!("{}/{}", l.trim_end_matches('/'), basename_of(t))
            } else {
                l.clone()
            };
            (t.clone(), l)
        }
        _ => return Vec::new(),
    };
    let link_abs = ctx.abs(&link);
    // A symlink target is resolved relative to the link's directory.
    let target_abs = if symbolic {
        link_abs.parent().map_or_else(|| ctx.abs(&target), |d| d.join(&target))
    } else {
        ctx.abs(&target)
    };
    let kind = if symbolic { "symlink" } else { "hard link" };
    let mut out = vec![effect(format!("ln: {kind} {link} → {target}"))];
    if !target_abs.exists() && !ctx.still_typing(&target) {
        out.push(warning(format!("ln: {target} doesn't exist{}", if symbolic { " (dangling link)" } else { "" })));
    }
    if fs::symlink_metadata(&link_abs).is_ok() {
        if force {
            out.push(warning(format!("ln: replaces existing {link}")));
        } else {
            out.push(warning(format!("ln: {link} already exists (use -f)")));
        }
    }
    out
}

fn basename_of(p: &str) -> String {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string()
}

/// `cat f…` (lines and size it would print) and `wc [-l|-w|-c] f…` (the counts themselves).
pub(super) fn cat_wc(ctx: &mut Ctx<'_>, cmd: &str, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    if rest.is_empty() {
        return Vec::new();
    }
    let Some(files) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    let (mut lines, mut words_n, mut bytes, mut exact) = (0u64, 0u64, 0u64, true);
    let mut missing = Vec::new();
    let mut read_left = MAX_READ;
    for f in &files {
        let p = ctx.abs(f);
        let Ok(meta) = fs::metadata(&p) else {
            missing.push(f.clone());
            continue;
        };
        if !meta.is_file() {
            missing.push(f.clone());
            continue;
        }
        bytes += meta.len();
        if meta.len() > read_left {
            exact = false;
            continue;
        }
        read_left -= meta.len();
        let mut buf = Vec::new();
        if fs::File::open(&p).and_then(|fh| fh.take(MAX_READ).read_to_end(&mut buf)).is_err() {
            exact = false;
            continue;
        }
        lines += buf.iter().filter(|&&b| b == b'\n').count() as u64;
        words_n += buf.split(u8::is_ascii_whitespace).filter(|w| !w.is_empty()).count() as u64;
    }
    let mut out = Vec::new();
    let counted = files.len() - missing.len();
    missing.retain(|f| !ctx.still_typing(f));
    if counted > 0 {
        let size = format_size(bytes);
        let text = if !exact {
            format!("{cmd}: {size}")
        } else if cmd == "wc" {
            let mut parts = Vec::new();
            let only = |c: char, l: &str| has_opt(&opts, c, l);
            let any = only('l', "lines") || only('w', "words") || only('c', "bytes") || only('m', "chars");
            if !any || only('l', "lines") {
                parts.push(plural(lines as usize, "line", "lines"));
            }
            if !any || only('w', "words") {
                parts.push(plural(words_n as usize, "word", "words"));
            }
            if !any || only('c', "bytes") || only('m', "chars") {
                parts.push(size);
            }
            format!("wc: {}", parts.join(" · "))
        } else {
            format!("cat: {} · {size}", plural(lines as usize, "line", "lines"))
        };
        out.push(value(if counted > 1 { format!("{text} in {counted} files") } else { text }));
    }
    if !missing.is_empty() {
        out.push(warning(format!("{cmd}: not a file: {}", names(&missing))));
    }
    out
}

/// `ls [-a] [dir]`: how many entries it lists, split into directories and files.
pub(super) fn ls(ctx: &mut Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    let hidden = has_opt(&opts, 'a', "all") || has_opt(&opts, 'A', "almost-all");
    let operand = match rest.as_slice() {
        [] => None,
        [one] => match ctx.expand_all([*one]) {
            Some(f) if f.len() == 1 => f.into_iter().next(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let dir = operand.as_deref().map_or_else(|| ctx.cwd.to_path_buf(), |o| ctx.abs(o));
    if !dir.is_dir() {
        if dir.exists() || operand.as_deref().is_some_and(|o| ctx.still_typing(o)) {
            return Vec::new();
        }
        return vec![warning(format!("ls: no such directory: {}", display(ctx, &dir)))];
    }
    let Ok(rd) = fs::read_dir(&dir) else { return Vec::new() };
    let (mut dirs, mut files, mut skipped) = (0usize, 0usize, 0usize);
    for e in rd.flatten() {
        if !ctx.budget.spend() {
            return Vec::new();
        }
        if !hidden && e.file_name().to_string_lossy().starts_with('.') {
            skipped += 1;
            continue;
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            dirs += 1;
        } else {
            files += 1;
        }
    }
    let mut text = format!("ls: {} · {}", plural(dirs, "dir", "dirs"), plural(files, "file", "files"));
    if skipped > 0 {
        text.push_str(&format!(" (+{skipped} hidden)"));
    }
    vec![value(text)]
}

/// `file f…`: a quick type guess (directory, symlink, empty, text, executable, binary, size).
pub(super) fn file(ctx: &mut Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (_, rest) = split_opts(words);
    let Some(mut files) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    files.retain(|f| fs::symlink_metadata(ctx.abs(f)).is_ok() || !ctx.still_typing(f));
    files
        .iter()
        .take(MAX_NAMED)
        .map(|f| {
            let p = ctx.abs(f);
            let Ok(meta) = fs::symlink_metadata(&p) else {
                return warning(format!("file: {f}: no such file"));
            };
            let kind = if meta.is_symlink() {
                format!("symlink → {}", fs::read_link(&p).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default())
            } else if meta.is_dir() {
                "directory".into()
            } else if meta.len() == 0 {
                "empty".into()
            } else {
                let mut head = [0u8; 512];
                let n = fs::File::open(&p).and_then(|mut fh| fh.read(&mut head)).unwrap_or(0);
                let head = &head[..n];
                let what = if head.starts_with(b"#!") {
                    "script"
                } else if head.starts_with(b"\x7fELF") {
                    "ELF binary"
                } else if head.contains(&0) {
                    "binary data"
                } else {
                    "text"
                };
                format!("{what} · {}", format_size(meta.len()))
            };
            value(format!("{f}: {kind}"))
        })
        .collect()
}

fn names(items: &[String]) -> String {
    let shown: Vec<&str> = items.iter().take(MAX_NAMED).map(String::as_str).collect();
    let mut s = shown.join(", ");
    if items.len() > MAX_NAMED {
        s.push_str(&format!(" +{} more", items.len() - MAX_NAMED));
    }
    s
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}
