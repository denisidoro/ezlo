//! Blast-radius & dry-run effect preview: what the typed command would do, shown below the prompt
//! while editing. Two sources:
//!
//! - **blast radius** ([`blast`]): for file-mutating commands (`[preview] commands`), unquoted
//!   globs are expanded ([`glob`]) and the affected paths summarized (files, dirs, total size);
//! - **dry run** ([`dryrun`]): a `preview $ <shell>` declared by the matched `cmd` of a spec, run
//!   with the line's `{opt.*}` / `{arg.*}` values interpolated;
//! - **instant** ([`instant`]): computed in-process for cheap commands: values of read-only ones
//!   (`echo $USER`, `pwd`, `which x`) and effects of simple ones (`cd`, `mkdir`, `touch`, `ln`);
//! - **safety** ([`safety`]): warnings for risky lines (`rm -rf ~/*`, an unset `$VAR` in a
//!   destructive operand, `> file` / `cp`/`mv` overwriting files, `git push --force`).
//!
//! [`pane`] is the per-candidate counterpart: a preview of the selected dropdown row (type
//! `preview` command, file head, directory listing), requested by the client on demand.

mod blast;
pub mod dryrun;
pub mod glob;
pub mod instant;
mod safety;
pub mod pane;
pub mod sgr;

use crate::engine::effect::{line_effect, LineEffect};
use crate::engine::line::{char_slice, find_active_segment, parse_command_line, Aliases};
use crate::engine::walker::walk_nested_command_line;
use crate::engine::CompletionRequest;
use crate::spec::ast::Effect;
use glob::Budget;
use serde::Serialize;
use std::time::{Duration, Instant};

/// Commands whose operands get a blast-radius summary by default.
pub const DEFAULT_COMMANDS: [&str; 7] = ["rm", "rmdir", "mv", "cp", "chmod", "chown", "chgrp"];

/// Knobs of [`preview_line`] (from `[preview]` in `config.toml`).
#[derive(Debug, Clone)]
pub struct PreviewOptions {
    /// Blast-radius summaries for these command names (empty disables them).
    pub commands: Vec<String>,
    /// Run `preview $ …` dry-run commands declared by specs.
    pub dry_run: bool,
    /// Instant previews for these command names (empty disables them).
    pub instant: Vec<String>,
    /// Safety warnings (dangerous targets, unset parameters, overwrites, force-pushes).
    pub safety: bool,
    /// The line's declared `remote`/`destroy` effect, when no other preview line describes it.
    pub effect: bool,
    /// Kill timeout of a dry-run command.
    pub timeout: Duration,
    /// Directory entries scanned at most while expanding globs and measuring sizes.
    pub max_entries: usize,
    /// Wall-clock budget of the filesystem scan.
    pub scan_budget: Duration,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            commands: DEFAULT_COMMANDS.map(String::from).to_vec(),
            dry_run: true,
            instant: instant::INSTANT_COMMANDS.map(String::from).to_vec(),
            safety: true,
            effect: true,
            timeout: Duration::from_millis(500),
            max_entries: 20_000,
            scan_budget: Duration::from_millis(50),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewKind {
    /// Blast-radius summary (`rm: 12 files · 3.4 MB`).
    Effect,
    /// A glob that matches nothing (zsh would refuse to run the line).
    Warning,
    /// Output line of a spec `preview` command.
    DryRun,
    /// What a read-only command would print (`echo $USER` → `zelda`).
    Value,
    /// Low-key summary of a harmless command (`cd → ~ · 26 entries`, `ls: 3 dirs · 1 file`).
    Info,
}

impl PreviewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Effect => "effect",
            Self::Warning => "warning",
            Self::DryRun => "dryrun",
            Self::Value => "value",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewLine {
    pub kind: PreviewKind,
    pub text: String,
}

/// Effect preview of the pipeline/list segment under the cursor: the whole segment counts (not
/// only the words before the cursor), at the innermost nested command (`sudo rm *.log`,
/// `timeout 5 rm -r build`).
pub fn preview_line(req: &CompletionRequest<'_>, opts: &PreviewOptions) -> Vec<PreviewLine> {
    let total = req.line.chars().count();
    let segment = find_active_segment(req.line, req.cursor.min(total));
    let mut full = char_slice(req.line, 0, segment.1).to_owned();
    if full.trim().is_empty() {
        return Vec::new();
    }
    full.push(' ');
    let aliases = Aliases::new(req.index, req.aliases);
    let parsed = parse_command_line(&full, segment.1 + 1, aliases, req.wrappers);
    let (parsed, walk) = walk_nested_command_line(req.index, parsed, aliases, req.wrappers, |_, _| {});

    let mut out = Vec::new();
    let mut budget = Budget {
        entries_left: opts.max_entries,
        deadline: Instant::now() + opts.scan_budget,
    };
    if !opts.commands.is_empty() {
        out.extend(blast::blast_radius(&parsed, &walk, &opts.commands, req.cwd, req.env, &mut budget));
    }
    if !opts.instant.is_empty() {
        out.extend(instant::instant_preview(&parsed, req.cursor.min(total), &opts.instant, req.cwd, req.env, req.aliases, &mut budget));
    }
    if opts.dry_run
        && let Some(command) = dryrun::interpolate(&walk, req, &parsed) {
            out.extend(dryrun::run(
                &command,
                &walk.cmd_path,
                req.cwd,
                req.env,
                req.runner,
                req.cache,
                opts.timeout,
            ));
        }
    // Last: zsh's status line shows the last warning, and these matter most.
    let safety = if opts.safety {
        safety::safety_warnings(&parsed, req.cwd, req.env, &mut budget)
    } else {
        Vec::new()
    };
    let described = out
        .iter()
        .chain(&safety)
        .any(|l| matches!(l.kind, PreviewKind::Effect | PreviewKind::Warning | PreviewKind::DryRun));
    if opts.effect && !described {
        out.extend(effect_line(&line_effect(req)));
    }
    out.extend(safety);
    out
}

/// `git push: changes remote state` / `git reset --hard: destroys data` for lines declared
/// `remote` or `destroy`; milder classes stay quiet.
fn effect_line(effect: &LineEffect) -> Option<PreviewLine> {
    let (kind, what) = match effect.level? {
        Effect::Destroy => (PreviewKind::Warning, "destroys data"),
        Effect::Remote => (PreviewKind::Effect, "changes remote state"),
        Effect::Read | Effect::Write => return None,
    };
    Some(PreviewLine {
        kind,
        text: format!("{}: {what}", effect.because.join(", ")),
    })
}
