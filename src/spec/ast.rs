use crate::spec::span::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Spec {
    pub types: Vec<TypeDecl>,
    pub cmds: Vec<CmdDecl>,
    pub recipes: Vec<RecipeDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<AliasDecl>,
}

/// Top-level `alias gco = git checkout`: the command word `gco` is completed, validated and
/// explained as the words `git checkout`, like a shell alias the session didn't report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasDecl {
    pub name: String,
    pub expansion: Vec<String>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TypeDecl {
    Base(BaseType),
    Refine(RefineType),
}

impl TypeDecl {
    pub fn name(&self) -> &str {
        match self {
            Self::Base(b) => &b.name,
            Self::Refine(r) => &r.name,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Base(b) => b.span,
            Self::Refine(r) => r.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseType {
    pub name: String,
    pub params: Vec<Param>,
    pub generator: Generator,
    pub cols: Vec<String>,
    pub insert: String,
    pub show: Vec<String>,
    pub preview: Option<String>,
    pub cache_ms: Option<u64>,
    #[serde(skip)]
    pub span: Span,
    #[serde(skip)]
    pub col_spans: Vec<(String, Span)>,
    #[serde(skip)]
    pub insert_span: Option<Span>,
    #[serde(skip)]
    pub show_spans: Vec<(String, Span)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineType {
    pub name: String,
    pub refines: TypeExpr,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub default: Vec<Source>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    Opt {
        name: String,
        #[serde(skip)]
        span: Span,
    },
    Arg {
        name: String,
        #[serde(skip)]
        span: Span,
    },
    Env {
        name: String,
        #[serde(skip)]
        span: Span,
    },
    Literal {
        value: String,
        #[serde(skip)]
        span: Span,
    },
}

impl Source {
    pub fn span(&self) -> Span {
        match self {
            Self::Opt { span, .. }
            | Self::Arg { span, .. }
            | Self::Env { span, .. }
            | Self::Literal { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Generator {
    Shell {
        command: String,
        #[serde(skip)]
        span: Span,
    },
    Values {
        values: Vec<String>,
        #[serde(skip)]
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeExpr {
    #[serde(rename = "ref")]
    pub type_ref: TypeRef,
    #[serde(rename = "where")]
    pub where_cond: Option<Vec<Clause>>,
    pub rank: Option<Vec<Clause>>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRef {
    pub name: String,
    pub bindings: Vec<Binding>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub name: String,
    pub sources: Vec<Source>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareOp {
    #[serde(rename = "eq")]
    Eq,
    #[serde(rename = "ne")]
    Ne,
    #[serde(rename = "match")]
    Match,
    #[serde(rename = "in")]
    In,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    pub col: String,
    pub op: CompareOp,
    pub values: Vec<String>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CmdDecl {
    pub path: Vec<String>,
    /// Alternative spellings of the last path word (`cmd git (checkout|co)` or an `alias co` child).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub desc: Option<String>,
    pub opts: Vec<OptDecl>,
    pub args: Vec<ArgDecl>,
    pub recipes: Vec<RecipeDecl>,
    /// `preview $ <shell>` (or ``preview `<shell>` ``): side-effect-free dry-run command rendered
    /// below the prompt while editing a line of this command; `{opt.<name>}` / `{arg.<name>}`
    /// expand to the shell-quoted values parsed from the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip)]
    pub preview_span: Option<Span>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptDecl {
    pub flags: Vec<String>,
    pub name: String,
    pub value: Option<Box<ValueSpec>>,
    pub desc: Option<String>,
    pub global: bool,
    pub required: bool,
    /// Recognized everywhere, but only offered once the user types a prefix of one of its flags.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    pub excludes: Vec<String>,
    pub when: Vec<WhenClause>,
    /// Values after the first one (`opt --arg <name> <value>`): a detached spelling consumes one
    /// word per value, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_values: Vec<ValueSpec>,
    /// `greedy` modifier of an optional value (`opt -b [boot] = 0 | -1 greedy`): a detached next
    /// word is also taken as the value when it is one of the static suggestions or an integer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub greedy: bool,
    #[serde(skip)]
    pub span: Span,
    #[serde(skip)]
    pub flag_spans: Vec<(String, Span)>,
    #[serde(skip)]
    pub exclude_spans: Vec<(String, Span)>,
}

impl OptDecl {
    /// Every value slot, in order (empty for a plain flag).
    pub fn value_slots(&self) -> impl Iterator<Item = &ValueSpec> {
        self.value.as_deref().into_iter().chain(&self.extra_values)
    }

    /// The `idx`-th value slot (0 = the first value).
    pub fn value_at(&self, idx: usize) -> Option<&ValueSpec> {
        match idx {
            0 => self.value.as_deref(),
            n => self.extra_values.get(n - 1),
        }
    }

    /// Whether the option's (single) value is optional (`opt --color [when]`): it is only taken
    /// when attached (`--color=always`, `-iSUFFIX`), or detached under `greedy`.
    pub fn optional_value(&self) -> bool {
        self.value.as_deref().is_some_and(|v| v.optional)
    }

    /// Words a detached spelling (`--flag v`, `-f v`) consumes: every value of an option with
    /// mandatory values, none for flags and optional values.
    pub fn detached_value_count(&self) -> usize {
        if self.optional_value() {
            0
        } else {
            self.value_slots().count()
        }
    }

    /// May appear several times (`<value...>` on the last value slot).
    pub fn repeatable(&self) -> bool {
        self.value_slots().last().is_some_and(|v| v.variadic)
    }

    /// Whether a `greedy` optional value takes the detached word `word` as its value: one of the
    /// slot's static suggestions or an integer (`journalctl -b -1`).
    pub fn takes_detached_value(&self, word: &str) -> bool {
        let Some(vs) = self.value.as_deref().filter(|v| v.optional && self.greedy) else {
            return false;
        };
        let digits = word.strip_prefix('-').unwrap_or(word);
        vs.suggestions.iter().any(|s| s == word) || (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    }

    /// `<name>` / `[name]` placeholders of every value slot, space-separated (`<name> <value>`).
    pub fn value_placeholders(&self) -> String {
        self.value_slots().map(ValueSpec::placeholder).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueSpec {
    pub name: String,
    pub variadic: bool,
    /// `[name]` option value: may be omitted (see [`OptDecl::optional_value`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// `list("<sep>")` / `kv("<sep>")` modifier: the value is a multipart single word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite: Option<Box<Composite>>,
    #[serde(rename = "type")]
    pub type_expr: Option<Box<TypeExpr>>,
    pub suggestions: Vec<String>,
    #[serde(skip)]
    pub required: bool,
    #[serde(skip)]
    pub span: Span,
}

impl ValueSpec {
    /// `<name>` / `[name]`, with `...` when variadic.
    pub fn placeholder(&self) -> String {
        let dots = if self.variadic { "..." } else { "" };
        if self.optional {
            format!("[{}{dots}]", self.name)
        } else {
            format!("<{}{dots}>", self.name)
        }
    }
}

/// Composite value modifier of a value slot: the slot's word is made of several parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Composite {
    /// `list(",")`: delimiter-separated unique items, each completed from the slot's values.
    List { sep: String },
    /// `kv("=")` / `kv("@", <type>)`: `key<sep>value`. Keys come from the slot's values (the part
    /// before `sep` of `key<sep>value` rows); values from the rows of the typed key and from the
    /// optional value type.
    Kv {
        sep: String,
        #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
        value_type: Option<TypeExpr>,
    },
}

impl Composite {
    pub fn sep(&self) -> &str {
        match self {
            Self::List { sep } | Self::Kv { sep, .. } => sep,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhenClause {
    pub cond: Vec<WhenTerm>,
    #[serde(rename = "type")]
    pub type_expr: TypeExpr,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhenTerm {
    pub flag: String,
    pub negated: bool,
    pub value: Option<String>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgDecl {
    pub name: String,
    pub required: bool,
    pub variadic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite: Option<Box<Composite>>,
    #[serde(rename = "type")]
    pub type_expr: Option<Box<TypeExpr>>,
    pub suggestions: Vec<String>,
    pub desc: Option<String>,
    pub when: Vec<WhenClause>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeDecl {
    pub desc: String,
    pub scope: Vec<String>,
    pub template: String,
    pub placeholders: Vec<String>,
    pub args: Vec<RecipeArg>,
    #[serde(skip)]
    pub span: Span,
    #[serde(skip)]
    pub template_span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeArg {
    pub name: String,
    pub variadic: bool,
    #[serde(rename = "type")]
    pub type_expr: Option<Box<TypeExpr>>,
    pub suggestions: Vec<String>,
    pub desc: Option<String>,
    #[serde(skip)]
    pub required: bool,
    #[serde(skip)]
    pub span: Span,
}
