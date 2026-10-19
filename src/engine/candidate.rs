//! Completion candidates: what the engine produces and the clients render/insert.

use crate::engine::icons;
use crate::engine::line::shell_quote;
use crate::engine::types::TypeRow;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CandidateKind {
    Cmd,
    Sub,
    Opt,
    #[default]
    Arg,
    File,
    Dir,
    Recipe,
    History,
    /// "Ask AI" row of a comment line (`# list files`); see `engine::translate`.
    Translate,
    /// A command suggested by the translator for a comment line; replaces the whole line.
    Translated,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Sub => "sub",
            Self::Opt => "opt",
            Self::Arg => "arg",
            Self::File => "file",
            Self::Dir => "dir",
            Self::Recipe => "recipe",
            Self::History => "history",
            Self::Translate => "translate",
            Self::Translated => "translated",
        }
    }

    pub fn icon(self) -> char {
        match self {
            Self::Cmd => icons::CMD,
            Self::Sub => icons::SUB,
            Self::Opt => icons::OPT,
            Self::Arg => icons::ARG,
            Self::File => icons::FILE,
            Self::Dir => icons::DIR,
            Self::Recipe => icons::RECIPE,
            Self::History => icons::HISTORY,
            Self::Translate | Self::Translated => icons::TRANSLATE,
        }
    }

    /// zsh `region_highlight` style for the icon.
    pub fn color_style(self) -> &'static str {
        match self {
            Self::Cmd => "fg=cyan",
            Self::Sub => "fg=magenta",
            Self::Opt => "fg=yellow",
            Self::Arg => "fg=green",
            Self::File => "fg=default",
            Self::Dir => "fg=blue,bold",
            Self::Recipe => "fg=208",
            Self::History => "fg=blue",
            Self::Translate | Self::Translated => "fg=yellow",
        }
    }

    /// zsh `region_highlight` style for the label (`None` keeps the terminal default).
    pub fn label_style(self) -> Option<&'static str> {
        match self {
            Self::Cmd => Some("fg=cyan"),
            Self::Sub => Some("fg=magenta"),
            Self::Opt => Some("fg=yellow"),
            Self::Arg => Some("fg=green"),
            Self::Dir => Some("fg=blue,bold"),
            Self::Recipe => Some("fg=208,bold"),
            Self::Translate => Some("fg=yellow,bold"),
            Self::File | Self::History | Self::Translated => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Candidate {
    pub label: String,
    pub description: String,
    pub insert_value: String,
    pub insert_text: String,
    pub suffix: String,
    pub kind: CandidateKind,
    pub deemphasized: bool,
    pub priority_tier: u8,
    pub score: f64,
    pub match_indices: Vec<usize>,
    /// Data columns rendered aligned across rows; when empty, `description` is the only column.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// Label style override (e.g. executables / symlinks in path completion).
    #[serde(skip)]
    pub style: Option<&'static str>,
    /// Icon override.
    #[serde(skip)]
    pub icon: Option<char>,
    #[serde(skip)]
    pub raw_columns: HashMap<String, String>,
    #[serde(skip)]
    pub preview_cmd: Option<String>,
    #[serde(skip)]
    pub recipe_index: Option<usize>,
    /// Synthesized from learned usage (not declared by a spec); `ezlo explain` provenance.
    #[serde(skip)]
    pub learned: bool,
}

impl Candidate {
    /// A literal word (subcommand, flag, static suggestion), inserted shell-quoted.
    pub fn word(value: &str, description: Option<&str>, kind: CandidateKind, tier: u8) -> Self {
        Self {
            label: value.to_owned(),
            description: description.unwrap_or_default().to_owned(),
            insert_value: value.to_owned(),
            insert_text: shell_quote(value),
            suffix: " ".to_owned(),
            kind,
            priority_tier: tier,
            ..Default::default()
        }
    }

    pub fn with_suffix(mut self, suffix: &str) -> Self {
        suffix.clone_into(&mut self.suffix);
        self
    }

    /// A row produced by a type (generator, values or builtin path).
    pub fn from_row(row: TypeRow, tier: u8) -> Self {
        let kind = if row.is_dir {
            CandidateKind::Dir
        } else if row.is_file {
            CandidateKind::File
        } else {
            CandidateKind::Arg
        };
        Self {
            label: row.label,
            description: row.description,
            insert_value: row.insert_value,
            insert_text: row.insert_quoted,
            suffix: row.suffix,
            kind,
            deemphasized: row.rank_deemphasized,
            priority_tier: tier,
            raw_columns: row.raw_columns,
            preview_cmd: row.preview_cmd,
            columns: row.columns,
            style: row.style,
            icon: row.icon,
            ..Default::default()
        }
    }
}
