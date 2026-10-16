//! Command-line arguments (`argh`).

use argh::FromArgs;
use std::path::PathBuf;

/// spec-driven shell autocomplete & recipe launcher
#[derive(Debug, FromArgs)]
pub struct Cli {
    #[argh(subcommand)]
    pub command: Commands,
}

#[derive(Debug, FromArgs)]
#[argh(subcommand)]
pub enum Commands {
    Init(InitArgs),
    Check(CheckArgs),
    Parse(ParseArgs),
    Complete(CompleteArgs),
    Explain(ExplainArgs),
    Preview(PreviewArgs),
    Recall(RecallArgs),
    Pick(PickArgs),
    Import(ImportArgs),
    History(HistoryArgs),
    Daemon(DaemonArgs),
}

/// emit the zsh integration script to stdout
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "init")]
pub struct InitArgs {
    /// shell name: only `zsh` is supported (optional)
    #[argh(positional)]
    pub shell: Option<String>,
}

/// validate .ezlo files and report diagnostics with line:col spans
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "check")]
pub struct CheckArgs {
    /// compare specs against live `<cmd> --help` output (sandboxed probe) instead: positionals
    /// become command names (defaults to the roots of user-authored specs)
    #[argh(switch)]
    pub drift: bool,
    /// spec files to validate (defaults to user & generated spec directories), or command names with `--drift`
    #[argh(positional)]
    pub files: Vec<PathBuf>,
}

/// parse a .ezlo file and output its canonical AST JSON
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "parse")]
pub struct ParseArgs {
    /// output as JSON (always on; kept for compatibility)
    #[argh(switch)]
    pub json: bool,
    /// path to the .ezlo file
    #[argh(positional)]
    pub file: PathBuf,
}

/// run a one-shot completion query
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "complete")]
pub struct CompleteArgs {
    /// command line buffer (via --line)
    #[argh(option)]
    pub line: Option<String>,
    /// cursor offset in chars (defaults to the end of the line)
    #[argh(option)]
    pub cursor: Option<usize>,
    /// working directory
    #[argh(option)]
    pub cwd: Option<PathBuf>,
    /// output candidates as JSON
    #[argh(switch)]
    pub json: bool,
    /// command line buffer (positional)
    #[argh(positional, arg_name = "LINE")]
    pub positional_line: Option<String>,
}

/// explain a command line token by token: bindings, diagnostics and candidate provenance
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "explain")]
pub struct ExplainArgs {
    /// command line buffer (via --line)
    #[argh(option)]
    pub line: Option<String>,
    /// cursor offset in chars (defaults to the end of the line)
    #[argh(option)]
    pub cursor: Option<usize>,
    /// working directory
    #[argh(option)]
    pub cwd: Option<PathBuf>,
    /// output the explanation as JSON
    #[argh(switch)]
    pub json: bool,
    /// command line buffer (positional)
    #[argh(positional, arg_name = "LINE")]
    pub positional_line: Option<String>,
}

/// preview a command line's effect: blast radius of file-mutating commands and spec dry-runs
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "preview")]
pub struct PreviewArgs {
    /// command line buffer (via --line)
    #[argh(option)]
    pub line: Option<String>,
    /// cursor offset in chars (defaults to the end of the line)
    #[argh(option)]
    pub cursor: Option<usize>,
    /// working directory
    #[argh(option)]
    pub cwd: Option<PathBuf>,
    /// output the preview lines as JSON (`kind`, `text`)
    #[argh(switch)]
    pub json: bool,
    /// command line buffer (positional)
    #[argh(positional, arg_name = "LINE")]
    pub positional_line: Option<String>,
}

/// list the most frecent successful command lines of a directory (and its git repo)
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "recall")]
pub struct RecallArgs {
    /// working directory (defaults to the current one)
    #[argh(option)]
    pub cwd: Option<PathBuf>,
    /// maximum number of lines
    #[argh(option, default = "20")]
    pub limit: usize,
    /// output as JSON (`line`, `ts`, `in_cwd`)
    #[argh(switch)]
    pub json: bool,
}

/// open interactive recipe picker and placeholder filler
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "pick")]
pub struct PickArgs {
    /// current command line buffer
    #[argh(option)]
    pub line: Option<String>,
    /// cursor offset in chars
    #[argh(option)]
    pub cursor: Option<usize>,
    /// initial search query
    #[argh(option)]
    pub query: Option<String>,
    /// jump directly to a recipe by id or template
    #[argh(option)]
    pub recipe: Option<String>,
    /// execute the selected command immediately instead of printing to stdout
    #[argh(switch)]
    pub run: bool,
    /// suggestions visible at once (overrides `[pick] rows` from config)
    #[argh(option)]
    pub rows: Option<usize>,
}

/// scrape `<cmd> --help` inside a sandbox into `~/.cache/ezlo/generated/<cmd>.ezlo`
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "import")]
pub struct ImportArgs {
    /// command name or path to probe
    #[argh(positional)]
    pub cmd: String,
}

/// manage command history database
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "history")]
pub struct HistoryArgs {
    #[argh(subcommand)]
    pub action: HistoryAction,
}

#[derive(Debug, FromArgs)]
#[argh(subcommand)]
pub enum HistoryAction {
    Import(HistoryImportArgs),
    Search(HistorySearchArgs),
}

/// import zsh history into ezlo's database
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "import")]
pub struct HistoryImportArgs {
    /// history file (default: $HISTFILE, else ~/.zsh_history)
    #[argh(positional)]
    pub path: Option<PathBuf>,
}

/// search shell history interactively (zsh `Ctrl-R`) and print the picked line
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "search")]
pub struct HistorySearchArgs {
    /// initial search query
    #[argh(option)]
    pub query: Option<String>,
    /// history file to merge with ezlo's own log (default: $HISTFILE, else zsh's default)
    #[argh(option)]
    pub histfile: Option<PathBuf>,
    /// skip lines starting with a space (zsh `hist_ignore_space`)
    #[argh(switch)]
    pub ignore_space: bool,
    /// rows visible at once (default: ~60% of the terminal)
    #[argh(option)]
    pub rows: Option<usize>,
}

/// start or query the background completion daemon
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "daemon")]
pub struct DaemonArgs {
    /// detach into the background
    #[argh(switch)]
    pub detach: bool,
    /// query daemon status
    #[argh(switch)]
    pub status: bool,
    /// stop a running daemon
    #[argh(switch)]
    pub stop: bool,
    /// custom Unix socket path
    #[argh(option)]
    pub socket: Option<PathBuf>,
    /// rebuild the spec index cache and exit (used by the daemon to keep parsing out of its memory)
    #[argh(switch)]
    pub build_index: bool,
}
