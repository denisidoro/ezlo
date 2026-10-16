//! `preview $ <shell>` of `cmd` blocks: interpolates `{opt.<name>}` / `{arg.<name>}` with the
//! values parsed from the line and runs the (side-effect-free) dry-run command.

use super::{PreviewKind, PreviewLine};
use crate::engine::generator::{CacheKey, CacheLookup, GeneratorCache, GeneratorRunner};
use crate::engine::line::shell_quote;
use crate::engine::walker::WalkState;
use crate::spec::preview::{preview_refs, PreviewSource};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Output lines kept from a dry-run command.
pub const MAX_DRYRUN_LINES: usize = 3;
/// Dry-run output is reused across keystrokes for this long (same command line and cwd).
const DRYRUN_CACHE_TTL: Duration = Duration::from_secs(2);

/// The preview command of the matched command with its placeholders expanded; `None` when the
/// command declares no preview or a referenced required positional isn't typed yet.
pub fn interpolate(walk: &WalkState<'_>) -> Option<String> {
    let cmd = walk.matched_cmd?;
    let template = cmd.preview.as_deref()?;
    let mut out = String::with_capacity(template.len());
    let mut last = 0;
    for r in preview_refs(template) {
        out.push_str(&template[last..r.range.0]);
        last = r.range.1;
        match r.source {
            PreviewSource::Arg => match walk.arg_values.get(&r.name).filter(|v| !v.is_empty()) {
                Some(vals) => out.push_str(&vals.iter().map(|v| shell_quote(v)).collect::<Vec<_>>().join(" ")),
                // Optional positionals expand to nothing; required ones must be typed first.
                None if cmd.args.iter().any(|a| a.name == r.name && !a.required) => {}
                None => return None,
            },
            PreviewSource::Opt => {
                // Every value of a multi-value option (`--arg <name> <value>`), each quoted.
                if let Some(vals) = walk.opt_value_lists.get(&r.name).filter(|v| !v.is_empty()) {
                    out.push_str(&vals.iter().map(|v| shell_quote(v)).collect::<Vec<_>>().join(" "));
                } else if walk.opt_occurrence_count.get(&r.name).is_some_and(|&n| n > 0) {
                    // A flag without value: its canonical spelling.
                    let opt = walk.active_opts.iter().find(|o| o.name == r.name);
                    let flag = opt.and_then(|o| o.flags.iter().find(|f| f.starts_with("--")).or(o.flags.first()));
                    out.push_str(flag.map_or("", String::as_str));
                }
            }
        }
    }
    out.push_str(&template[last..]);
    Some(out)
}

/// Runs `command` (cached briefly per cwd) and turns its first output lines into preview lines.
pub(super) fn run(
    command: &str,
    cmd_path: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    runner: &dyn GeneratorRunner,
    cache: &GeneratorCache,
    timeout: Duration,
) -> Vec<PreviewLine> {
    let key = CacheKey {
        base_type: format!("preview:{}", cmd_path.join(" ")),
        params: vec![("command".to_owned(), command.to_owned())],
        cwd: cwd.to_path_buf(),
    };
    let rows = match cache.lookup(&key) {
        CacheLookup::Fresh(rows) => rows,
        CacheLookup::Stale(_) | CacheLookup::Miss => {
            let lines = runner.run_shell(command, cwd, env, &[], timeout);
            let rows = lines
                .iter()
                .map(|l| strip_ansi(l))
                .filter(|l| !l.trim().is_empty())
                .take(MAX_DRYRUN_LINES)
                .map(|l| vec![l])
                .collect();
            cache.insert(key, rows, DRYRUN_CACHE_TTL)
        }
    };
    rows.iter()
        .filter_map(|r| r.first())
        .map(|text| PreviewLine {
            kind: PreviewKind::DryRun,
            text: text.clone(),
        })
        .collect()
}

/// Drops ANSI escape sequences (CSI/OSC) and other control characters.
pub(super) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' || (n == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' {
            out.push(' ');
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strips_escape_sequences() {
        assert_eq!(strip_ansi("\x1b[1;32mok\x1b[0m\tdone"), "ok done");
        assert_eq!(strip_ansi("\x1b]8;;http://x\x1b\\link\x1b]8;;\x07"), "link");
    }
}
