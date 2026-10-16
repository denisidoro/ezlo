pub mod clause;
mod eval;
pub mod format;
pub mod path;
mod system;
mod when;

pub use when::{evaluate_when_status, WhenStatus};

use crate::engine::generator::{CacheKey, GeneratorCache, GeneratorRunner};
use crate::spec::ast::{TypeExpr, TypeRef, WhenClause};
use crate::spec::span::Span;
use crate::spec::validate::is_path_builtin_type;
use crate::spec::SpecIndex;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TypeRow {
    pub insert_value: String,
    pub insert_quoted: String,
    pub suffix: String,
    pub label: String,
    pub description: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub rank_deemphasized: bool,
    pub pending_hint: Option<String>,
    pub raw_columns: HashMap<String, String>,
    pub preview_cmd: Option<String>,
    /// Extra show columns, aligned across rows by the renderer.
    pub columns: Vec<String>,
    pub style: Option<&'static str>,
    pub icon: Option<char>,
}

/// How one parameter of a referenced base type resolves on the current line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ParamBinding {
    pub name: String,
    pub value: Option<String>,
    /// Source that supplied `value` (`opt.remote`, `env.X`, `"literal"`), or the whole fallback chain
    /// when nothing did.
    pub source: String,
    /// Bound by the type reference (or a refinement) rather than by the parameter's default.
    pub explicit: bool,
}

#[derive(Debug, Clone)]
pub struct PendingRefresh {
    pub cache_key: CacheKey,
    pub command: String,
    pub num_cols: usize,
    pub ttl: Duration,
}

pub struct TypeEvalContext<'a> {
    pub index: &'a SpecIndex,
    pub cwd: &'a Path,
    pub env: &'a HashMap<String, String>,
    /// Shell aliases of the calling session (rows of the builtin `cmd` type).
    pub aliases: &'a HashMap<String, String>,
    pub opt_values: &'a HashMap<String, String>,
    pub arg_values: &'a HashMap<String, Vec<String>>,
    pub present_flags: &'a HashMap<String, Option<String>>,
    pub runner: &'a dyn GeneratorRunner,
    pub cache: &'a GeneratorCache,
    pub timeout: Duration,
    pub allow_stale: bool,
}

pub fn evaluate_slot_types(
    ctx: &TypeEvalContext<'_>,
    declared_type: Option<&TypeExpr>,
    fallback_name: &str,
    when_clauses: &[WhenClause],
    typed_prefix: &str,
    refreshes: &mut Vec<PendingRefresh>,
) -> Vec<TypeRow> {
    let mut selected_primary: Option<&TypeExpr> = None;
    let mut pending_clauses: Vec<(&TypeExpr, String)> = Vec::new();

    for wc in when_clauses {
        match evaluate_when_status(wc, ctx.present_flags) {
            WhenStatus::True => {
                selected_primary.get_or_insert(&wc.type_expr);
            }
            WhenStatus::Pending(hint) => pending_clauses.push((&wc.type_expr, hint)),
            WhenStatus::Contradicted => {}
        }
    }

    let primary_expr = selected_primary
        .or(declared_type)
        .map(Cow::Borrowed)
        .or_else(|| fallback_type_expr(ctx, fallback_name).map(Cow::Owned));

    let mut out = Vec::new();
    let mut seen_inserts = HashSet::new();
    let mut slot_memo = eval::RowMemo::new();

    if let Some(expr) = &primary_expr {
        let rows = eval::evaluate_type_expr_inner(ctx, expr, typed_prefix, refreshes, &mut slot_memo);
        for row in rows {
            seen_inserts.insert(row.insert_value.clone());
            out.push(row);
        }
    }

    for (pexpr, hint) in pending_clauses {
        let rows = eval::evaluate_type_expr_inner(ctx, pexpr, typed_prefix, refreshes, &mut slot_memo);
        for mut row in rows {
            if !seen_inserts.insert(row.insert_value.clone()) {
                continue;
            }
            row.rank_deemphasized = true;
            row.description = if row.description.is_empty() {
                format!("({hint})")
            } else {
                format!("{} ({hint})", row.description)
            };
            row.pending_hint = Some(hint.clone());
            out.push(row);
        }
    }

    out
}

/// Synthesizes a bare reference to `name` when it names a declared or filesystem builtin type.
fn fallback_type_expr(ctx: &TypeEvalContext<'_>, name: &str) -> Option<TypeExpr> {
    if ctx.index.find_type(name).is_none() && !is_path_builtin_type(name) {
        return None;
    }
    Some(TypeExpr {
        type_ref: TypeRef {
            name: name.to_string(),
            bindings: Vec::new(),
            span: Span::default(),
        },
        where_cond: None,
        rank: None,
        span: Span::default(),
    })
}

/// How the parameters of the base type behind `expr` resolve with the values typed so far.
pub fn type_param_bindings(ctx: &TypeEvalContext<'_>, expr: &TypeExpr) -> Vec<ParamBinding> {
    eval::param_bindings(ctx, expr)
}

pub fn evaluate_type_expr(
    ctx: &TypeEvalContext<'_>,
    expr: &TypeExpr,
    typed_prefix: &str,
    refreshes: &mut Vec<PendingRefresh>,
) -> Vec<TypeRow> {
    let mut memo = eval::RowMemo::new();
    eval::evaluate_type_expr_inner(ctx, expr, typed_prefix, refreshes, &mut memo)
}
