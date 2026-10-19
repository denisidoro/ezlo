//! `config.toml` sections, one per concern. Each struct mirrors a TOML table and is the only
//! place its defaults live; unset keys inside a table keep their default.

use serde::{Deserialize, Serialize};

/// `[sources]`: where completion candidates come from, plus per-source settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    /// Enabled sources. Order is irrelevant: precedence is fixed (user > bundled > generated,
    /// with native zsh only for commands no spec knows).
    pub enabled: Vec<Source>,
    pub user: UserSourceConfig,
    pub native: NativeSourceConfig,
}

impl Default for SourcesConfig {
    fn default() -> Self {
        Self {
            enabled: vec![
                Source::User,
                Source::Bundled,
                Source::Native,
                Source::Generated,
            ],
            user: UserSourceConfig::default(),
            native: NativeSourceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// `.ezlo` files under `[sources.user] dirs`.
    User,
    /// Specs compiled into the binary.
    Bundled,
    /// Persistent `zsh -f` worker running `_complete`.
    Native,
    /// Specs produced by `ezlo import`.
    Generated,
}

/// `[sources.user]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSourceConfig {
    /// Directories scanned (recursively) for user `.ezlo` files, highest precedence first.
    /// Empty means `$EZLO_SPECS_DIR` (colon-separated) or `<config_dir>/specs`.
    pub dirs: Vec<String>,
}

/// `[sources.native]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeSourceConfig {
    /// Files sourced in the native zsh worker (e.g. to load completion functions).
    pub rc: Vec<String>,
}

/// `[line]`: how a typed command line is interpreted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LineConfig {
    /// Prefix commands skipped to find the real command (`sudo git …` completes `git`).
    pub wrappers: Vec<String>,
}

impl Default for LineConfig {
    fn default() -> Self {
        let wrappers = [
            "sudo", "env", "time", "nice", "nohup", "command", "exec", "xargs",
        ];
        Self {
            wrappers: wrappers.map(String::from).to_vec(),
        }
    }
}

/// `[generators]`: running spec generator commands (shared by completion and `ezlo pick`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneratorsConfig {
    /// Per-generator kill timeout (e.g. `"1s"`, `"500ms"`).
    pub timeout: String,
}

impl Default for GeneratorsConfig {
    fn default() -> Self {
        Self {
            timeout: "1s".to_string(),
        }
    }
}

/// `[complete]`: inline completion in the shell (dropdown + ghost text).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompleteConfig {
    /// Chars to type before the dropdown/ghost text shows up while the line has no space yet,
    /// so typing short commands (`l` → `ls`) doesn't flash every matching binary (0 disables).
    pub min_chars: usize,
    /// Show the matched command's positional signature (`scp  <source…>  [target]`) below the
    /// prompt while the cursor is on a positional slot, with the active slot highlighted.
    pub signature: bool,
    pub dropdown: DropdownConfig,
    pub ghost: GhostConfig,
    pub pane: PaneConfig,
    pub translate: TranslateConfig,
}

impl Default for CompleteConfig {
    fn default() -> Self {
        Self {
            min_chars: 3,
            signature: true,
            dropdown: DropdownConfig::default(),
            ghost: GhostConfig::default(),
            pane: PaneConfig::default(),
            translate: TranslateConfig::default(),
        }
    }
}

/// `[complete.dropdown]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DropdownConfig {
    /// Visible rows (`$EZLO_MAX_ROWS` overrides; capped to the terminal).
    pub rows: usize,
    /// Max shell-history rows mixed in when specs also match; history fills the whole dropdown
    /// when nothing else matches (0 disables history in the dropdown).
    pub history_rows: usize,
}

impl Default for DropdownConfig {
    fn default() -> Self {
        Self {
            rows: 6,
            history_rows: 3,
        }
    }
}

/// `[complete.pane]`: preview pane of the selected dropdown row (type `preview` command, file
/// head, directory listing), drawn beside or below the dropdown; the Alt-E menu toggles it per shell.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneConfig {
    /// When the pane opens without the pane toggle: `auto` (rows whose type declares a `preview` command),
    /// `on` (every previewable row, files and directories included) or `off` (pane toggle only).
    pub mode: crate::engine::preview::pane::PaneMode,
    /// Visible pane height in lines (capped to the terminal); beside the rows it grows to the
    /// dropdown's height. Longer previews scroll (Shift-Up/Down).
    pub lines: usize,
    /// Built-in previews of file / directory rows (file head, directory listing).
    pub paths: bool,
    /// Kill timeout of a type `preview` command (e.g. `"500ms"`).
    pub timeout: String,
    /// `right` (beside the dropdown rows; below them when the terminal is too narrow) or `bottom`.
    pub position: crate::engine::preview::pane::PanePosition,
    /// Share of the terminal width taken by a right-hand pane, in percent.
    pub width: usize,
    /// Shell commands previewing a text file, tried in order: the first whose program is on
    /// `$PATH` runs with `$path` set (ANSI colors are kept). None installed or `[]`: built-in head.
    pub file_commands: Vec<String>,
    /// Same for directories, with `$width` (cells) set too. None installed or `[]`: built-in listing.
    pub dir_commands: Vec<String>,
}

impl Default for PaneConfig {
    fn default() -> Self {
        Self {
            mode: crate::engine::preview::pane::PaneMode::Auto,
            lines: 5,
            paths: true,
            timeout: "500ms".to_string(),
            position: crate::engine::preview::pane::PanePosition::Right,
            width: 40,
            file_commands: ["bat", "batcat"]
                .map(|b| format!(r#"{b} --color=always --style=plain --paging=never --wrap=never --line-range=:200 "$path""#))
                .to_vec(),
            dir_commands: vec![
                r#"eza --no-quotes --icons="$icons" --color=always --all --group-directories-first --grid --width="$width" "$path""#.to_string(),
            ],
        }
    }
}

/// `[complete.translate]`: comment lines (`# list files by size`) offer a row that turns the text
/// into a command by piping it into `command`. ezlo has no AI of its own: `command` is whatever
/// script talks to your model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslateConfig {
    /// argv of the translator (no shell): gets the request on stdin and prints the command on
    /// stdout; runs in the shell's directory and environment. Empty disables the feature.
    pub command: Vec<String>,
    /// Kill timeout of the translator (e.g. `"20s"`).
    pub timeout: String,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            timeout: "20s".to_string(),
        }
    }
}

/// `[complete.ghost]`: inline suggestion after the cursor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GhostConfig {
    pub mode: GhostMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GhostMode {
    #[default]
    Auto,
    On,
    Off,
}

/// `[pick]`: the interactive recipe launcher (`ezlo pick` / `Ctrl-G`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PickConfig {
    /// Suggestions visible at once (0: ~40% of the terminal height). A plain number rather
    /// than an `Option` so every key has a serialized default (see `Config::unknown_keys`).
    pub rows: usize,
}

/// `[preview]`: blast-radius & dry-run effect preview below the prompt while editing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewConfig {
    /// Master switch for both preview sources.
    pub enabled: bool,
    /// Commands whose operands get a blast-radius summary (globs expanded, paths counted and
    /// measured); empty disables it.
    pub commands: Vec<String>,
    /// Run `preview $ …` dry-run commands declared by spec `cmd` blocks.
    pub dry_run: bool,
    /// Commands with an instant, in-process preview (`echo $USER` → its value, `cd` target,
    /// `mkdir` creating vs. existing); empty disables it.
    pub instant: Vec<String>,
    /// Warnings for risky lines: destructive commands aimed at `/` or `$HOME`, unset parameters in
    /// their operands, `> file` / `cp` / `mv` overwriting files, `git push --force`, `dd of=/dev/…`.
    pub safety: bool,
    /// Show the line's declared `remote`/`destroy` side effect (from spec `effect` declarations)
    /// when no other preview line already describes it.
    pub effect: bool,
    /// Kill timeout of a dry-run command (e.g. `"500ms"`).
    pub timeout: String,
    /// Directory entries scanned at most per preview (glob expansion + size measuring).
    pub max_entries: usize,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            commands: crate::engine::preview::DEFAULT_COMMANDS.map(String::from).to_vec(),
            dry_run: true,
            instant: crate::engine::preview::instant::INSTANT_COMMANDS.map(String::from).to_vec(),
            safety: true,
            effect: true,
            timeout: "500ms".to_string(),
            max_entries: 20_000,
        }
    }
}
