//! User configuration (`config.toml`), grouped into one table per concern:
//!
//! | Table | Concern |
//! |---|---|
//! | `[sources]`, `[sources.<name>]` | where candidates come from (+ per-source settings) |
//! | `[line]` | how a typed command line is interpreted |
//! | `[generators]` | running spec generator commands |
//! | `[complete]`, `[complete.dropdown]`, `[complete.ghost]`, `[complete.pane]`, `[complete.translate]` | inline shell completion UI |
//! | `[pick]` | the `ezlo pick` recipe launcher |
//! | `[preview]` | blast-radius & dry-run effect preview while editing |
//!
//! New options go into the table of the concern they belong to (add a sub-table when a
//! concern grows its own knobs) rather than as new root keys. Filesystem locations are not
//! configurable here; see [`paths`].

mod paths;
mod sections;

pub use paths::{check_spec_file_security, expand_path};
pub use sections::*;

use crate::engine::preview::pane::PaneOptions;
use crate::engine::preview::PreviewOptions;
use crate::engine::translate::TranslateOptions;
use crate::spec::parser::cursor::parse_duration_ms;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sources: SourcesConfig,
    pub line: LineConfig,
    pub generators: GeneratorsConfig,
    pub complete: CompleteConfig,
    pub pick: PickConfig,
    pub preview: PreviewConfig,
}

impl Config {
    pub fn load() -> Self {
        let path = Self::config_file_path();
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&content).unwrap_or_else(|err| {
            eprintln!("ezlo: ignoring invalid config {}: {err}", path.display());
            Self::default()
        })
    }

    /// Prints unknown keys of the config file to stderr. Only called from long-lived or
    /// diagnostic entry points (daemon, `ezlo check`), never per-completion commands.
    pub fn warn_unknown_keys() {
        let path = Self::config_file_path();
        let Ok(content) = fs::read_to_string(&path) else {
            return;
        };
        for key in Self::unknown_keys(&content) {
            eprintln!(
                "ezlo: warning: unknown config key `{key}` in {}",
                path.display()
            );
        }
    }

    /// Dotted paths of keys in `content` that no setting reads (typos, removed options).
    pub fn unknown_keys(content: &str) -> Vec<String> {
        let (Ok(user), Ok(toml::Value::Table(known))) = (
            content.parse::<toml::Table>(),
            toml::Value::try_from(Self::default()),
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_unknown_keys(&user, &known, "", &mut out);
        out.sort();
        out
    }

    pub fn generator_timeout_duration(&self) -> Duration {
        parse_duration_ms(&self.generators.timeout)
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_secs(1))
    }

    pub fn has_source(&self, source: Source) -> bool {
        self.sources.enabled.contains(&source)
    }

    /// `[preview]` as engine options; `None` when previews are disabled.
    pub fn preview_options(&self) -> Option<PreviewOptions> {
        let p = &self.preview;
        p.enabled.then(|| PreviewOptions {
            commands: p.commands.clone(),
            dry_run: p.dry_run,
            instant: p.instant.clone(),
            safety: p.safety,
            timeout: parse_duration_ms(&p.timeout)
                .map(Duration::from_millis)
                .unwrap_or_else(|| PreviewOptions::default().timeout),
            max_entries: p.max_entries,
            ..PreviewOptions::default()
        })
    }

    /// `[complete.pane]` as engine options for a terminal `width` cells wide.
    pub fn pane_options(&self, width: usize) -> PaneOptions {
        let p = &self.complete.pane;
        PaneOptions {
            mode: p.mode,
            lines: p.lines.clamp(1, 20),
            paths: p.paths,
            timeout: parse_duration_ms(&p.timeout)
                .map(Duration::from_millis)
                .unwrap_or_else(|| PaneOptions::default().timeout),
            width,
            position: p.position,
            split: p.width,
            file_commands: p.file_commands.clone(),
            dir_commands: p.dir_commands.clone(),
        }
    }

    /// `[complete.translate]` as runtime options (disabled when `command` is empty).
    pub fn translate_options(&self) -> TranslateOptions {
        let t = &self.complete.translate;
        TranslateOptions {
            command: t.command.clone(),
            timeout: parse_duration_ms(&t.timeout)
                .map(Duration::from_millis)
                .unwrap_or_else(|| TranslateOptions::default().timeout),
        }
    }

    /// `[pick] rows`, `None` meaning "size to the terminal".
    pub fn pick_rows(&self) -> Option<usize> {
        (self.pick.rows > 0).then_some(self.pick.rows)
    }

    /// Resolved user spec directories: `[sources.user] dirs`, else `$EZLO_SPECS_DIR`,
    /// else `<config_dir>/specs`. Supports `~/` and `$VAR`/`${VAR}` prefixes.
    pub fn spec_dirs(&self) -> Vec<PathBuf> {
        if !self.sources.user.dirs.is_empty() {
            return self
                .sources
                .user
                .dirs
                .iter()
                .map(|d| expand_path(d))
                .collect();
        }
        if let Ok(env_dirs) = std::env::var("EZLO_SPECS_DIR") {
            let dirs: Vec<PathBuf> = env_dirs
                .split(':')
                .filter(|d| !d.is_empty())
                .map(expand_path)
                .collect();
            if !dirs.is_empty() {
                return dirs;
            }
        }
        vec![Self::user_specs_dir()]
    }
}

fn collect_unknown_keys(
    user: &toml::Table,
    known: &toml::Table,
    prefix: &str,
    out: &mut Vec<String>,
) {
    for (key, value) in user {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (value, known.get(key)) {
            (_, None) => out.push(path),
            (toml::Value::Table(u), Some(toml::Value::Table(k))) => {
                collect_unknown_keys(u, k, &path, out)
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, GhostMode, Source};

    #[test]
    fn tables_override_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [sources]
            enabled = ["user", "bundled"]
            [sources.user]
            dirs = ["/a"]
            [sources.native]
            rc = ["/rc"]
            [line]
            wrappers = ["doas"]
            [generators]
            timeout = "250ms"
            [complete]
            min_chars = 0
            [complete.dropdown]
            rows = 9
            [complete.ghost]
            mode = "off"
            [pick]
            rows = 12
            "#,
        )
        .expect("valid config");
        assert!(cfg.has_source(Source::User) && !cfg.has_source(Source::Native));
        assert_eq!(cfg.sources.user.dirs, ["/a"]);
        assert_eq!(cfg.sources.native.rc, ["/rc"]);
        assert_eq!(cfg.line.wrappers, ["doas"]);
        assert_eq!(cfg.generator_timeout_duration().as_millis(), 250);
        assert_eq!(cfg.complete.min_chars, 0);
        assert_eq!(cfg.complete.dropdown.rows, 9);
        assert_eq!(
            cfg.complete.dropdown.history_rows, 3,
            "unset keys keep their default"
        );
        assert_eq!(cfg.complete.ghost.mode, GhostMode::Off);
        assert_eq!(cfg.pick_rows(), Some(12));
        assert_eq!(Config::default().pick_rows(), None);
    }

    #[test]
    fn unknown_source_rejects_the_file() {
        assert!(toml::from_str::<Config>("[sources]\nenabled = [\"usr\"]").is_err());
    }

    #[test]
    fn unknown_keys_are_reported_with_their_path() {
        let content = "max_rows = 6\n[complete]\nmin_chars = 1\n[complete.dropdown]\nrow = 3\n[sources.user]\ndirs = []\n";
        assert_eq!(
            Config::unknown_keys(content),
            ["complete.dropdown.row", "max_rows"]
        );
    }

    #[test]
    fn dotfiles_config_is_valid_and_fully_known() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ezlo/config.toml");
        // Only present when built inside the author's dotfiles; the standalone repo has no such file.
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let cfg: Config = toml::from_str(&content).expect("ezlo/config.toml must parse");
        assert_eq!(cfg.pick_rows(), Some(10));
        assert_eq!(cfg.translate_options().command, ["dot", "llm", "chat", "zsh"]);
        assert_eq!(Config::unknown_keys(&content), Vec::<String>::new());
    }
}
