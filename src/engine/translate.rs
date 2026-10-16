//! Comment lines as natural-language requests: `# list files by size` shows one "translate" row,
//! and accepting it pipes the text into the user's `[complete.translate] command` (an LLM wrapper,
//! never built into ezlo). Each line of its stdout is one suggested one-liner, offered as a
//! dropdown row that replaces the whole line when accepted.
//!
//! ezlo only spawns the configured argv with the request on stdin, enforces the timeout and
//! cleans the output; it never talks to a model itself.

use super::candidate::{Candidate, CandidateKind};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Label of the dropdown row offered for a comment line.
pub const ROW_LABEL: &str = "Translate to command";
/// Output read from the translator at most.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;
/// Suggested commands kept at most.
const MAX_SUGGESTIONS: usize = 10;

/// `[complete.translate]` as runtime options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateOptions {
    /// argv of the translator; empty disables the feature.
    pub command: Vec<String>,
    pub timeout: Duration,
}

impl Default for TranslateOptions {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            timeout: Duration::from_secs(20),
        }
    }
}

impl TranslateOptions {
    pub fn enabled(&self) -> bool {
        self.command.first().is_some_and(|p| !p.trim().is_empty())
    }
}

/// The request of a comment line: the text after the leading `#`s (and optional indentation),
/// trimmed; `None` when the line isn't a comment or has nothing to translate.
pub fn comment_query(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('#')?;
    let text = rest.trim_start_matches('#').trim();
    (!text.is_empty()).then_some(text)
}

/// Whether the line is a comment at all (`#` after optional indentation), even an empty one.
pub fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// The request sent for an explicit translation (`Ctrl-X Ctrl-L`): a comment's text, else the
/// whole line, trimmed.
pub fn translate_query(line: &str) -> &str {
    comment_query(line).unwrap_or_else(|| line.trim().trim_start_matches('#').trim())
}

/// The single dropdown row offered for a comment line.
pub fn translate_row(query: &str, opts: &TranslateOptions) -> Candidate {
    let program = opts
        .command
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or_default();
    let via: Vec<&str> = std::iter::once(program)
        .chain(opts.command.iter().skip(1).map(String::as_str))
        .collect();
    Candidate {
        label: ROW_LABEL.to_owned(),
        description: format!("Tab: ask `{}`", via.join(" ")),
        insert_value: query.to_owned(),
        insert_text: query.to_owned(),
        kind: CandidateKind::Translate,
        ..Default::default()
    }
}

/// One dropdown row per suggested command; accepting one replaces the whole line with it.
pub fn suggestion_rows(commands: &[String]) -> Vec<Candidate> {
    commands
        .iter()
        .map(|cmd| Candidate {
            label: cmd.clone(),
            insert_value: cmd.clone(),
            insert_text: cmd.clone(),
            kind: CandidateKind::Translated,
            ..Default::default()
        })
        .collect()
}

/// Turns the translator's stdout into suggested one-liners, one per line: drops Markdown code
/// fences, blank and `#` comment lines and duplicates, unwraps `` `command` `` and a leading `$ `
/// prompt. `None` when nothing is left.
pub fn clean_output(stdout: &str) -> Option<Vec<String>> {
    let mut commands: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let Some(cmd) = clean_line(line) else { continue };
        if !commands.iter().any(|c| c == cmd) {
            commands.push(cmd.to_owned());
        }
        if commands.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    (!commands.is_empty()).then_some(commands)
}

fn clean_line(line: &str) -> Option<&str> {
    let mut one = line.trim();
    if one.starts_with("```") || one.starts_with('#') {
        return None;
    }
    if one.len() >= 2 && one.starts_with('`') && one.ends_with('`') {
        one = one.trim_matches('`').trim();
    }
    one = one.strip_prefix("$ ").unwrap_or(one).trim();
    (!one.is_empty()).then_some(one)
}

/// Where and how the translator runs.
pub struct TranslateContext<'a> {
    pub cwd: &'a Path,
    /// The shell's exported environment (so `PATH` lookups match the shell); empty keeps the
    /// daemon's own.
    pub env: &'a HashMap<String, String>,
    /// Set to stop a running translation (killed right away).
    pub cancel: &'a AtomicBool,
}

/// Runs the translator with `query` on stdin and returns the suggested commands (at least one),
/// or a short error message. Blocking: call it off the async runtime.
pub fn run_translator(opts: &TranslateOptions, query: &str, ctx: &TranslateContext<'_>) -> Result<Vec<String>, String> {
    let Some((program, args)) = opts.command.split_first().filter(|_| opts.enabled()) else {
        return Err("not configured: set [complete.translate] command in config.toml".to_owned());
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if ctx.cwd.is_dir() {
        cmd.current_dir(ctx.cwd);
    }
    if !ctx.env.is_empty() {
        cmd.env_clear();
        cmd.envs(ctx.env);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run `{program}`: {e}"))?;
    // Messages name the program, not its full path.
    let program = program.rsplit('/').next().unwrap_or(program);
    let pgid = Pid::from_raw(child.id() as i32);

    // Feed stdin and drain both pipes on their own threads, so a chatty translator can't
    // deadlock against us.
    let stdin = child.stdin.take();
    let request = format!("{query}\n");
    std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(request.as_bytes());
        }
    });
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let deadline = Instant::now() + opts.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = killpg(pgid, Signal::SIGKILL);
                let _ = child.wait();
                return Err(format!("`{program}` failed: {e}"));
            }
        }
        let stop = if ctx.cancel.load(Ordering::SeqCst) {
            Some("cancelled".to_owned())
        } else if Instant::now() >= deadline {
            Some(format!("timed out after {}", fmt_duration(opts.timeout)))
        } else {
            None
        };
        if let Some(reason) = stop {
            let _ = killpg(pgid, Signal::SIGKILL);
            let _ = child.wait();
            return Err(reason);
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let collect = |h: Option<std::thread::JoinHandle<String>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    let out = collect(stdout);
    if !status.success() {
        let err = collect(stderr);
        let detail = err.lines().rev().find(|l| !l.trim().is_empty()).map(str::trim);
        let code = status.code().map_or_else(|| "a signal".to_owned(), |c| format!("status {c}"));
        return Err(match detail {
            Some(d) => format!("`{program}` exited with {code}: {d}"),
            None => format!("`{program}` exited with {code}"),
        });
    }
    let commands = clean_output(&out).ok_or_else(|| format!("`{program}` printed no command"))?;
    // Some wrappers report failures on stdout with a zero exit status.
    match commands.first() {
        Some(first) if is_error_line(first) => Err(format!("`{program}`: {first}")),
        _ => Ok(commands),
    }
}

/// Whether a translator's output line is an error report rather than a command.
fn is_error_line(line: &str) -> bool {
    ["Error:", "API Error:", "error:"].iter().any(|p| line.starts_with(p))
}

fn drain(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn fmt_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_query_needs_a_hash_and_text() {
        assert_eq!(comment_query("# list files"), Some("list files"));
        assert_eq!(comment_query("  ##list files  "), Some("list files"));
        assert_eq!(comment_query("#"), None);
        assert_eq!(comment_query("#   "), None);
        assert_eq!(comment_query("ls # files"), None);
        assert!(is_comment_line("  #"));
        assert!(!is_comment_line("ls"));
    }

    #[test]
    fn translate_query_falls_back_to_the_whole_line() {
        assert_eq!(translate_query("# list files"), "list files");
        assert_eq!(translate_query(" list files "), "list files");
    }

    #[test]
    fn clean_output_strips_fences_backticks_and_prompts() {
        assert_eq!(clean_output("```zsh\nls -la\n```\n"), Some(vec!["ls -la".to_owned()]));
        assert_eq!(clean_output("`du -sh *`"), Some(vec!["du -sh *".to_owned()]));
        assert_eq!(clean_output("\n$ git status\n\n"), Some(vec!["git status".to_owned()]));
        assert_eq!(clean_output("```\n```\n  \n# just a comment\n"), None);
    }

    #[test]
    fn clean_output_keeps_one_suggestion_per_line() {
        let out = "```sh\nls -lS\n\n# or\n`du -sh * | sort -h`\n$ ls -lS\n```\n";
        assert_eq!(clean_output(out), Some(vec!["ls -lS".to_owned(), "du -sh * | sort -h".to_owned()]));
        let many: String = (0..20).map(|i| format!("echo {i}\n")).collect();
        assert_eq!(clean_output(&many).map(|c| c.len()), Some(MAX_SUGGESTIONS));
    }

    #[test]
    fn suggestion_rows_replace_the_line_verbatim() {
        let rows = suggestion_rows(&["ls -la".to_owned(), "du -sh *".to_owned()]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.kind == CandidateKind::Translated && r.suffix.is_empty()));
        assert_eq!((rows[1].label.as_str(), rows[1].insert_text.as_str()), ("du -sh *", "du -sh *"));
    }

    #[test]
    fn error_reports_are_not_commands() {
        assert!(is_error_line("API Error: quota exceeded"));
        assert!(is_error_line("Error: no API key"));
        assert!(!is_error_line("grep -r 'Error:' ."));
    }

    #[test]
    fn row_names_the_translator() {
        let opts = TranslateOptions {
            command: vec!["/x/bin/dot".into(), "llm".into(), "chat".into(), "zsh".into()],
            ..Default::default()
        };
        let row = translate_row("list files", &opts);
        assert_eq!(row.kind, CandidateKind::Translate);
        assert_eq!(row.description, "Tab: ask `dot llm chat zsh`");
        assert!(!TranslateOptions::default().enabled());
    }
}
