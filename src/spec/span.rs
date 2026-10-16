use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LineCol {
    /// `u32` keeps `Span` at 16 bytes: every AST node carries one.
    pub line: u32,
    pub col: u32,
}

impl LineCol {
    /// Saturates past `u32::MAX` (no real spec file gets there).
    pub const fn new(line: usize, col: usize) -> Self {
        Self {
            line: if line > u32::MAX as usize { u32::MAX } else { line as u32 },
            col: if col > u32::MAX as usize { u32::MAX } else { col as u32 },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Span {
    pub start: LineCol,
    pub end: LineCol,
}

impl Span {
    pub const fn new(start: LineCol, end: LineCol) -> Self {
        Self { start, end }
    }

    pub const fn line(line: usize, start_col: usize, end_col: usize) -> Self {
        Self {
            start: LineCol::new(line, start_col),
            end: LineCol::new(line, end_col),
        }
    }

    pub fn merge(self, other: Self) -> Self {
        if self.start.line == 0 {
            return other;
        }
        if other.start.line == 0 {
            return self;
        }
        Self {
            start: self.start,
            end: other.end,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleCode {
    /// Rule 1: Lexical error (tab indent, sibling indent, unterminated string/backtick, unknown escape)
    E01Lexical,
    /// Rule 2: Disallowed child node or repeated single-occurrence property
    E02Structure,
    /// Rule 3: Type block missing/both `$` and `values`, or refinement with children
    E03TypeForm,
    /// Rule 4: Duplicate type name, cmd path, or flag spelling (including inherited global)
    E04Duplicate,
    /// Rule 5: Unresolved type reference, column name, param name, or excludes/when flag
    E05Unresolved,
    /// Rule 6: Reserved word used as a name
    E06ReservedWord,
    /// Rule 7: Positional arg ordering/variadic violation
    E07PositionalArgs,
    /// Rule 8: Recipe structure (`$` count, unknown arg placeholder, scope prefix mismatch)
    E08RecipeStructure,
    /// Rule 9: Placeholder dependency cycle in a recipe
    E09RecipeCycle,
    /// Rule 10: Invalid env var name for col/param, or col/param name collision
    E10EnvName,
    /// Warning 1: Unreachable sources after a STRING literal in a fallback chain
    W01UnreachableSource,
    /// Warning 2: `when` on an `opt` that takes no value
    W02ValuelessOptWhen,
    /// Warning 3: Refinement name `base:` prefix differs from actual base type
    W03RefinementPrefix,
}

impl fmt::Display for RuleCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::E01Lexical => "E01",
            Self::E02Structure => "E02",
            Self::E03TypeForm => "E03",
            Self::E04Duplicate => "E04",
            Self::E05Unresolved => "E05",
            Self::E06ReservedWord => "E06",
            Self::E07PositionalArgs => "E07",
            Self::E08RecipeStructure => "E08",
            Self::E09RecipeCycle => "E09",
            Self::E10EnvName => "E10",
            Self::W01UnreachableSource => "W01",
            Self::W02ValuelessOptWhen => "W02",
            Self::W03RefinementPrefix => "W03",
        };
        f.write_str(code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: RuleCode,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(code: RuleCode, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            span,
        }
    }

    pub fn warning(code: RuleCode, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message: message.into(),
            span,
        }
    }

    pub fn render(&self, filename: &str, source: &str) -> String {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let line_idx = (self.span.start.line as usize).saturating_sub(1);
        let line_text = source.lines().nth(line_idx).unwrap_or("");
        let start_col = (self.span.start.col as usize).max(1);
        let span_end_col = self.span.end.col as usize;
        let end_col = if self.span.end.line == self.span.start.line && span_end_col > start_col {
            span_end_col
        } else {
            start_col + 1
        };
        let caret_len = end_col.saturating_sub(start_col).max(1);
        let pad = " ".repeat(start_col.saturating_sub(1));
        let carets = "^".repeat(caret_len);
        let line_no = self.span.start.line;
        let gutter_width = line_no.to_string().len();
        let empty_gutter = " ".repeat(gutter_width);

        format!(
            "{sev}[{}]: {}\n  --> {filename}:{}:{}\n  {empty_gutter} |\n  {line_no} | {line_text}\n  {empty_gutter} | {pad}{carets}\n",
            self.code, self.message, self.span.start.line, self.span.start.col
        )
    }
}

pub const RESERVED_WORDS: &[&str] = &[
    "type", "cmd", "opt", "arg", "recipe", "when", "where", "rank", "and", "in", "global",
    "required", "excludes", "cols", "insert", "show", "preview", "cache", "values",
];

pub fn is_reserved_word(word: &str) -> bool {
    RESERVED_WORDS.contains(&word)
}
