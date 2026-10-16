//! Instant previews, computed in-process without spawning anything:
//!
//! - **values** of cheap read-only commands, as if they had run (`echo $USER` → `zelda`, `pwd`,
//!   `whoami`, `basename`, `realpath`, `which`, `printenv`, `test`/`[`, `nproc`, `$((…))`);
//! - **effects** of simple state-changing ones that the blast radius doesn't cover (`cd` target,
//!   `mkdir`/`touch` creating vs. existing paths, `ln` link → target, `kill` process names);
//! - **sizes** of what a reader would dump (`cat`, `wc`, `ls`).
//!
//! Words are expanded statically ([`expand`]); a word ezlo can't evaluate (`$(...)`) disables
//! the preview of its command instead of guessing. A missing path under the cursor that still
//! prefixes an existing entry (`cat READM`) is being typed, not missing: it gets no warning.

pub mod expand;
mod files;
mod info;

use super::glob::Budget;
use super::{PreviewKind, PreviewLine};
use crate::engine::line::ParsedLine;
use expand::{expand_word, Expanded};
use std::collections::HashMap;
use std::path::Path;

/// Commands with an instant preview (also the `[preview] instant` default).
pub const INSTANT_COMMANDS: [&str; 26] = [
    "echo", "print", "pwd", "whoami", "hostname", "basename", "dirname", "realpath", "readlink", "which", "type",
    "whence", "printenv", "nproc", "test", "[", "cd", "pushd", "mkdir", "touch", "ln", "kill", "cat", "wc", "ls",
    "file",
];

/// What an instant preview can look at.
pub(super) struct Ctx<'a> {
    pub cwd: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub aliases: &'a HashMap<String, String>,
    pub budget: &'a mut Budget,
    /// The expanded operand under the cursor, if any.
    pub typing: Option<String>,
}

impl Ctx<'_> {
    /// Expands every word; `None` when one of them can't be evaluated or a glob matches nothing.
    pub fn expand_all<'w>(&mut self, words: impl IntoIterator<Item = &'w str>) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for raw in words {
            match expand_word(raw, self.env, self.cwd, self.budget) {
                Expanded::Fields(f) => out.extend(f),
                Expanded::NoMatch | Expanded::Unsupported => return None,
            }
        }
        Some(out)
    }

    /// `path` resolved against the shell's cwd.
    pub fn abs(&self, path: &str) -> std::path::PathBuf {
        self.cwd.join(path)
    }

    /// Whether `path` (an expanded operand) is the word under the cursor and still a prefix of
    /// an existing entry, i.e. not finished yet (`cd /bi`).
    pub fn still_typing(&mut self, path: &str) -> bool {
        if self.typing.as_deref() != Some(path) || path.ends_with('/') {
            return false;
        }
        let abs = self.abs(path);
        let (Some(dir), Some(name)) = (abs.parent(), abs.file_name()) else { return false };
        let name = name.to_string_lossy();
        let Ok(entries) = std::fs::read_dir(dir) else { return false };
        for e in entries.flatten() {
            if !self.budget.spend() {
                return false;
            }
            if e.file_name().to_string_lossy().starts_with(name.as_ref()) {
                return true;
            }
        }
        false
    }
}

pub(super) fn value(text: impl Into<String>) -> PreviewLine {
    PreviewLine { kind: PreviewKind::Value, text: one_line(&text.into()) }
}

pub(super) fn effect(text: impl Into<String>) -> PreviewLine {
    PreviewLine { kind: PreviewKind::Effect, text: one_line(&text.into()) }
}

pub(super) fn warning(text: impl Into<String>) -> PreviewLine {
    PreviewLine { kind: PreviewKind::Warning, text: one_line(&text.into()) }
}

/// Preview text stays on one line: newlines show as `⏎`, and it is capped (zsh cuts it to the
/// terminal width anyway).
fn one_line(s: &str) -> String {
    const MAX_CHARS: usize = 300;
    let mut out: String = s.chars().take(MAX_CHARS).map(|c| if c == '\n' { '⏎' } else { c }).collect();
    if s.chars().count() > MAX_CHARS {
        out.push('…');
    }
    out
}

/// Splits leading option words (`-n`, `-la`, `--all`) from operands; `--` ends the options.
pub(super) fn split_opts<'w>(words: &[&'w str]) -> (Vec<&'w str>, Vec<&'w str>) {
    let mut opts = Vec::new();
    let mut i = 0;
    while let Some(&w) = words.get(i) {
        if w == "--" {
            i += 1;
            break;
        }
        if w.len() > 1 && w.starts_with('-') {
            opts.push(w);
            i += 1;
        } else {
            break;
        }
    }
    (opts, words[i..].to_vec())
}

/// Whether short option letter `c` or long option `long` is among `opts`.
pub(super) fn has_opt(opts: &[&str], c: char, long: &str) -> bool {
    opts.iter().any(|o| match o.strip_prefix("--") {
        Some(l) => !long.is_empty() && l == long,
        None => o[1..].contains(c),
    })
}

/// Instant preview of the command in `parsed` when it is one of `commands`; `cursor` (chars into
/// the line) tells which word is still being typed.
pub(super) fn instant_preview(
    parsed: &ParsedLine,
    cursor: usize,
    commands: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    aliases: &HashMap<String, String>,
    budget: &mut Budget,
) -> Vec<PreviewLine> {
    let Some(cmd_word) = parsed.words.get(parsed.cmd_word_idx) else {
        return Vec::new();
    };
    let cmd = cmd_word.unquoted.as_str();
    if !commands.iter().any(|c| c == cmd) {
        return Vec::new();
    }
    let words: Vec<&str> = parsed.words[parsed.cmd_word_idx + 1..]
        .iter()
        .map(|w| w.raw.as_str())
        .filter(|w| !w.is_empty())
        .collect();
    // Redirections and here-strings change what the command does; leave those lines alone.
    if words.iter().any(|w| w.starts_with(['<', '>']) || w.contains(">&") || w.ends_with('>')) {
        return Vec::new();
    }
    let mut ctx = Ctx { cwd, env, aliases, budget, typing: None };
    ctx.typing = parsed.words[parsed.cmd_word_idx + 1..]
        .iter()
        .find(|w| !w.raw.is_empty() && w.char_start <= cursor && cursor <= w.char_end)
        .and_then(|w| ctx.expand_all([w.raw.as_str()]))
        .filter(|f| f.len() == 1)
        .and_then(|f| f.into_iter().next());
    match cmd {
        "echo" | "print" => info::echo(&mut ctx, &words),
        "pwd" => info::pwd(&ctx, &words),
        "whoami" => info::whoami(&ctx, &words),
        "hostname" => info::hostname(&words),
        "basename" | "dirname" => info::basename_dirname(&mut ctx, cmd, &words),
        "realpath" | "readlink" => info::realpath(&mut ctx, cmd, &words),
        "which" | "type" | "whence" => info::which(&mut ctx, cmd, &words),
        "printenv" => info::printenv(&ctx, &words),
        "nproc" => info::nproc(&words),
        "test" | "[" => info::test(&mut ctx, cmd, &words),
        "kill" => info::kill(&words),
        "cd" | "pushd" => files::cd(&mut ctx, cmd, &words),
        "mkdir" => files::mkdir(&mut ctx, &words),
        "touch" => files::touch(&mut ctx, &words),
        "ln" => files::ln(&mut ctx, &words),
        "cat" | "wc" => files::cat_wc(&mut ctx, cmd, &words),
        "ls" => files::ls(&mut ctx, &words),
        "file" => files::file(&mut ctx, &words),
        _ => Vec::new(),
    }
}
