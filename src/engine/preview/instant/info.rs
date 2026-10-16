//! Values of read-only commands, as if they had run: `echo`, `pwd`, `whoami`, `hostname`,
//! `basename`/`dirname`, `realpath`/`readlink`, `which`/`type`/`whence`, `printenv`, `nproc`,
//! `test`/`[` and the process names behind `kill` PIDs.

use super::expand::needs_expansion;
use super::{has_opt, split_opts, value, warning, Ctx};
use crate::engine::preview::PreviewLine;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// `echo $USER *.md` → `alice CHANGELOG.md README.md`; only when a word would change (plain text
/// isn't worth echoing back).
pub(super) fn echo(ctx: &mut Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    // zsh `echo`/`print` flags: -n, -e, -E (and combinations); anything else is text.
    let flags = words.iter().take_while(|w| w.len() > 1 && w.starts_with('-') && w[1..].chars().all(|c| "neErl".contains(c))).count();
    let text = &words[flags..];
    if !text.iter().any(|w| needs_expansion(w)) {
        return Vec::new();
    }
    let mut fields = Vec::new();
    for raw in text {
        match super::expand::expand_word(raw, ctx.env, ctx.cwd, ctx.budget) {
            super::expand::Expanded::Fields(f) => fields.extend(f),
            super::expand::Expanded::NoMatch => return vec![warning(format!("no matches found: {raw}"))],
            super::expand::Expanded::Unsupported => return Vec::new(),
        }
    }
    vec![value(fields.join(" "))]
}

/// `pwd` (`-P`: physical). `$PWD` keeps the logical path when it is the same directory.
pub(super) fn pwd(ctx: &Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    if !rest.is_empty() {
        return Vec::new();
    }
    if has_opt(&opts, 'P', "physical") {
        return fs::canonicalize(ctx.cwd).map(|p| vec![value(p.to_string_lossy())]).unwrap_or_default();
    }
    let logical = ctx
        .env
        .get("PWD")
        .filter(|p| fs::canonicalize(p).ok() == fs::canonicalize(ctx.cwd).ok());
    vec![value(logical.cloned().unwrap_or_else(|| ctx.cwd.to_string_lossy().into_owned()))]
}

pub(super) fn whoami(ctx: &Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    if !words.is_empty() {
        return Vec::new();
    }
    // `$USER` first: an NSS lookup can hit the network (LDAP) on every keystroke.
    let user = ctx.env.get("USER").filter(|u| !u.is_empty()).cloned().or_else(|| {
        nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten().map(|u| u.name)
    });
    user.map(|u| vec![value(u)]).unwrap_or_default()
}

pub(super) fn hostname(words: &[&str]) -> Vec<PreviewLine> {
    if !words.is_empty() {
        return Vec::new();
    }
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .map(|h| vec![value(h)])
        .unwrap_or_default()
}

/// `basename path [suffix]`, `basename -a p…`, `dirname p…` (string operations, like coreutils).
pub(super) fn basename_dirname(ctx: &mut Ctx<'_>, cmd: &str, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    let Some(args) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    if args.is_empty() {
        return Vec::new();
    }
    let out: Vec<String> = if cmd == "dirname" {
        args.iter().map(|a| dirname(a)).collect()
    } else if has_opt(&opts, 'a', "multiple") || args.len() > 2 {
        args.iter().map(|a| basename(a, "")).collect()
    } else {
        vec![basename(&args[0], args.get(1).map_or("", String::as_str))]
    };
    vec![value(out.join(" "))]
}

fn basename(path: &str, suffix: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { String::new() } else { "/".to_string() };
    }
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    match base.strip_suffix(suffix) {
        Some(b) if !suffix.is_empty() && !b.is_empty() => b.to_string(),
        _ => base.to_string(),
    }
}

fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.starts_with('/') { "/".into() } else { ".".into() };
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(i) => {
            let d = trimmed[..i].trim_end_matches('/');
            if d.is_empty() { "/".into() } else { d.to_string() }
        }
    }
}

/// `realpath p…`, `readlink -f p` (canonical paths) and plain `readlink link` (the link text).
pub(super) fn realpath(ctx: &mut Ctx<'_>, cmd: &str, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    let Some(args) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    let canonical = cmd == "realpath" || opts.iter().any(|o| o.contains(['f', 'e', 'm']) && !o.starts_with("--"));
    let mut out = Vec::new();
    for a in &args {
        let p = ctx.abs(a);
        if canonical {
            match fs::canonicalize(&p) {
                Ok(c) => out.push(value(c.to_string_lossy())),
                Err(_) if ctx.still_typing(a) => {}
                Err(_) => out.push(warning(format!("{cmd}: {a}: no such file or directory"))),
            }
        } else {
            match fs::read_link(&p) {
                Ok(t) => out.push(value(t.to_string_lossy())),
                Err(_) => out.push(warning(format!("{cmd}: {a}: not a symlink"))),
            }
        }
    }
    out
}

/// Where each name resolves: a shell alias, else the first executable on `$PATH`.
pub(super) fn which(ctx: &mut Ctx<'_>, cmd: &str, words: &[&str]) -> Vec<PreviewLine> {
    let (opts, rest) = split_opts(words);
    let all = has_opt(&opts, 'a', "all");
    let Some(names) = ctx.expand_all(rest.iter().copied()) else { return Vec::new() };
    let path = ctx.env.get("PATH").cloned().unwrap_or_default();
    let mut out = Vec::new();
    for name in names.iter().take(4) {
        if let Some(alias) = ctx.aliases.get(name) {
            out.push(value(format!("{name}: aliased to {alias}")));
            if !all {
                continue;
            }
        }
        let hits: Vec<String> = path
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(d).join(name))
            .filter(|p| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
            .map(|p| p.to_string_lossy().into_owned())
            .take(if all { 4 } else { 1 })
            .collect();
        if hits.is_empty() {
            if !ctx.aliases.contains_key(name) {
                out.push(warning(format!("{cmd}: {name} not found on $PATH")));
            }
        } else {
            out.push(value(hits.join("  ")));
        }
    }
    out
}

pub(super) fn printenv(ctx: &Ctx<'_>, words: &[&str]) -> Vec<PreviewLine> {
    let (_, names) = split_opts(words);
    names
        .iter()
        .take(4)
        .map(|n| match ctx.env.get(*n) {
            Some(v) => value(format!("{n}={v}")),
            None => warning(format!("printenv: {n} is not exported")),
        })
        .collect()
}

pub(super) fn nproc(words: &[&str]) -> Vec<PreviewLine> {
    if !words.is_empty() {
        return Vec::new();
    }
    std::thread::available_parallelism()
        .map(|n| vec![value(n.to_string())])
        .unwrap_or_default()
}

/// `test EXPR` / `[ EXPR ]` for one unary file/string test or one binary comparison.
pub(super) fn test(ctx: &mut Ctx<'_>, cmd: &str, words: &[&str]) -> Vec<PreviewLine> {
    let mut words = words.to_vec();
    if cmd == "[" {
        if words.last() != Some(&"]") {
            return Vec::new();
        }
        words.pop();
    }
    let negate = words.first() == Some(&"!");
    if negate {
        words.remove(0);
    }
    // Operators are matched before expansion so an empty value can't shift them.
    let result = match words.as_slice() {
        [op, operand] if op.starts_with('-') => {
            let Some(v) = ctx.expand_all([*operand]).map(|f| f.join(" ")) else { return Vec::new() };
            unary_test(ctx, op, &v)
        }
        [a, op, b] => {
            let (Some(a), Some(b)) = (ctx.expand_all([*a]), ctx.expand_all([*b])) else { return Vec::new() };
            binary_test(&a.join(" "), op, &b.join(" "))
        }
        [s] => ctx.expand_all([*s]).map(|f| !f.join(" ").is_empty()),
        _ => None,
    };
    match result {
        Some(r) => vec![value(if r != negate { "true (exit 0)" } else { "false (exit 1)" })],
        None => Vec::new(),
    }
}

fn unary_test(ctx: &Ctx<'_>, op: &str, v: &str) -> Option<bool> {
    let p = ctx.abs(v);
    let meta = || fs::metadata(&p);
    Some(match op {
        "-z" => v.is_empty(),
        "-n" => !v.is_empty(),
        "-e" | "-a" => meta().is_ok(),
        "-f" => meta().is_ok_and(|m| m.is_file()),
        "-d" => meta().is_ok_and(|m| m.is_dir()),
        "-s" => meta().is_ok_and(|m| m.len() > 0),
        "-L" | "-h" => fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()),
        "-x" => meta().is_ok_and(|m| m.permissions().mode() & 0o111 != 0),
        "-r" => nix::unistd::access(&p, nix::unistd::AccessFlags::R_OK).is_ok(),
        "-w" => nix::unistd::access(&p, nix::unistd::AccessFlags::W_OK).is_ok(),
        _ => return None,
    })
}

fn binary_test(a: &str, op: &str, b: &str) -> Option<bool> {
    let num = |s: &str| s.trim().parse::<i64>().ok();
    Some(match op {
        "=" | "==" => a == b,
        "!=" => a != b,
        "-eq" => num(a)? == num(b)?,
        "-ne" => num(a)? != num(b)?,
        "-lt" => num(a)? < num(b)?,
        "-le" => num(a)? <= num(b)?,
        "-gt" => num(a)? > num(b)?,
        "-ge" => num(a)? >= num(b)?,
        _ => return None,
    })
}

/// `kill [-SIG] pid…`: the process name of each PID (Linux `/proc`), so a mistyped PID shows.
pub(super) fn kill(words: &[&str]) -> Vec<PreviewLine> {
    let mut sig = "TERM".to_string();
    let mut pids = Vec::new();
    let mut it = words.iter();
    while let Some(&w) = it.next() {
        if w == "-s" || w == "-n" {
            if let Some(s) = it.next() {
                sig = s.trim_start_matches("SIG").to_string();
            }
        } else if let Some(s) = w.strip_prefix('-').filter(|s| !s.is_empty()) {
            if s == "l" || s == "L" {
                return Vec::new();
            }
            sig = s.trim_start_matches("SIG").to_string();
        } else if let Ok(pid) = w.parse::<u32>() {
            pids.push(pid);
        } else {
            // Job specs (`%1`) and names: nothing to look up.
            return Vec::new();
        }
    }
    if pids.is_empty() || !Path::new("/proc/self").exists() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for pid in pids {
        match fs::read_to_string(format!("/proc/{pid}/comm")) {
            Ok(name) => found.push(format!("{pid} {}", name.trim())),
            Err(_) => missing.push(pid.to_string()),
        }
    }
    let mut out = Vec::new();
    if !found.is_empty() {
        out.push(super::effect(format!("kill -{sig}: {}", found.join(", "))));
    }
    if !missing.is_empty() {
        out.push(warning(format!("kill: no such process: {}", missing.join(" "))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_and_dirname_follow_coreutils() {
        assert_eq!(basename("/a/b/c.txt", ".txt"), "c");
        assert_eq!(basename("/a/b/", ""), "b");
        assert_eq!(basename("/", ""), "/");
        assert_eq!(basename(".txt", ".txt"), ".txt");
        assert_eq!(dirname("/a/b/c"), "/a/b");
        assert_eq!(dirname("c"), ".");
        assert_eq!(dirname("/c"), "/");
        assert_eq!(dirname("a//b/"), "a");
    }

    #[test]
    fn comparisons() {
        assert_eq!(binary_test("3", "-lt", "10"), Some(true));
        assert_eq!(binary_test("a", "=", "a"), Some(true));
        assert_eq!(binary_test("a", "-lt", "b"), None);
    }
}
