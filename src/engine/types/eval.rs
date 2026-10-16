use super::clause::{filter_and_rank_builtin_rows, matches_clauses};
use super::format::continuation_suffix;
use super::path::complete_builtin_path;
use super::system::complete_builtin_system;
use super::{ParamBinding, PendingRefresh, TypeEvalContext, TypeRow};
use crate::engine::generator::{parse_tsv_rows, CacheKey, CacheLookup};
use crate::engine::line::shell_quote;
use crate::spec::ast::{BaseType, Clause, Generator, Source, TypeDecl, TypeExpr};
use crate::spec::validate::{is_builtin_type, is_path_builtin_type};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// Per-evaluation memo of generator output, so a slot evaluating several clauses over the same
/// base type runs its generator at most once.
pub(super) type RowMemo = HashMap<CacheKey, Arc<Vec<Vec<String>>>>;

/// Constraints accumulated while walking a refinement chain down to its base type.
struct Refinements<'e> {
    where_chain: Vec<&'e [Clause]>,
    /// Outermost `rank` wins.
    rank: Option<&'e [Clause]>,
    /// Outermost binding of each parameter wins.
    bindings: HashMap<&'e str, &'e [Source]>,
}

enum Target<'e> {
    Builtin(&'e str),
    Base(&'e BaseType),
}

/// Follows `expr` through refinements until it reaches a builtin or base type.
/// Returns `None` for unresolved names or refinement cycles.
fn resolve_chain<'e>(
    expr: &'e TypeExpr,
    type_map: &HashMap<&'e str, &'e TypeDecl>,
) -> Option<(Refinements<'e>, Target<'e>)> {
    let mut refs = Refinements {
        where_chain: expr.where_cond.as_deref().into_iter().collect(),
        rank: expr.rank.as_deref(),
        bindings: expr
            .type_ref
            .bindings
            .iter()
            .map(|b| (b.name.as_str(), b.sources.as_slice()))
            .collect(),
    };
    let mut cur_name = expr.type_ref.name.as_str();
    let mut visited = HashSet::new();
    loop {
        // Declared types shadow builtins of the same name.
        let Some(decl) = type_map.get(cur_name) else {
            return is_builtin_type(cur_name).then_some((refs, Target::Builtin(cur_name)));
        };
        if !visited.insert(cur_name) {
            return None;
        }
        let r = match decl {
            TypeDecl::Base(b) => return Some((refs, Target::Base(b))),
            TypeDecl::Refine(r) => &r.refines,
        };
        refs.where_chain.extend(r.where_cond.as_deref());
        if refs.rank.is_none() {
            refs.rank = r.rank.as_deref();
        }
        for b in &r.type_ref.bindings {
            refs.bindings.entry(b.name.as_str()).or_insert(b.sources.as_slice());
        }
        cur_name = r.type_ref.name.as_str();
    }
}

pub(super) fn evaluate_type_expr_inner(
    ctx: &TypeEvalContext<'_>,
    expr: &TypeExpr,
    typed_prefix: &str,
    refreshes: &mut Vec<PendingRefresh>,
    memo: &mut RowMemo,
) -> Vec<TypeRow> {
    let type_map = ctx.index.type_map();
    let Some((refs, target)) = resolve_chain(expr, &type_map) else {
        return Vec::new();
    };
    let base_type = match target {
        Target::Builtin(kind) if is_path_builtin_type(kind) => {
            let rows = complete_builtin_path(kind, typed_prefix, ctx.cwd, ctx.env);
            return filter_and_rank_builtin_rows(rows, &refs.where_chain, refs.rank);
        }
        Target::Builtin(kind) => {
            return complete_builtin_system(kind, ctx, &refs.where_chain, refs.rank);
        }
        Target::Base(b) => b,
    };

    let resolved_params = resolve_params(base_type, &refs.bindings, ctx);
    let raw_rows = fetch_base_rows(base_type, &resolved_params, ctx, refreshes, memo);

    let insert_idx = base_type
        .cols
        .iter()
        .position(|c| c == &base_type.insert)
        .unwrap_or(0);
    let show_indices: Vec<usize> = base_type
        .show
        .iter()
        .filter_map(|sc| base_type.cols.iter().position(|c| c == sc))
        .collect();

    raw_rows
        .iter()
        .filter(|row| {
            refs.where_chain
                .iter()
                .all(|clauses| matches_clauses(row, &base_type.cols, clauses))
        })
        .filter_map(|row| {
            let rank_deemphasized = refs
                .rank
                .is_some_and(|rclauses| !matches_clauses(row, &base_type.cols, rclauses));
            build_row(base_type, row, insert_idx, &show_indices, &resolved_params, rank_deemphasized)
        })
        .collect()
}

/// Turns one generator row into a candidate row; `None` when the insert column is empty.
fn build_row(
    base_type: &BaseType,
    row: &[String],
    insert_idx: usize,
    show_indices: &[usize],
    resolved_params: &[(String, String)],
    rank_deemphasized: bool,
) -> Option<TypeRow> {
    let insert_val = row.get(insert_idx).filter(|v| !v.is_empty())?;

    let show_vals: Vec<&str> = show_indices
        .iter()
        .map(|&idx| row.get(idx).map(String::as_str).unwrap_or(""))
        .collect();
    let (label, columns) = match show_vals.iter().position(|s| !s.is_empty()) {
        Some(first) => (
            show_vals[first].to_string(),
            show_vals[first + 1..].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ),
        None => (insert_val.clone(), Vec::new()),
    };
    let description = columns
        .iter()
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" · ");

    let mut raw_columns = HashMap::with_capacity(resolved_params.len() + base_type.cols.len());
    raw_columns.extend(resolved_params.iter().cloned());
    for (cidx, cname) in base_type.cols.iter().enumerate() {
        raw_columns.insert(cname.clone(), row.get(cidx).cloned().unwrap_or_default());
    }

    Some(TypeRow {
        insert_quoted: shell_quote(insert_val),
        suffix: continuation_suffix(insert_val).to_string(),
        insert_value: insert_val.clone(),
        label,
        description,
        is_dir: false,
        is_file: false,
        rank_deemphasized,
        pending_hint: None,
        raw_columns,
        preview_cmd: base_type.preview.clone(),
        columns,
        style: None,
        icon: None,
    })
}

fn resolve_params(
    base: &BaseType,
    bindings: &HashMap<&str, &[Source]>,
    ctx: &TypeEvalContext<'_>,
) -> Vec<(String, String)> {
    base.params
        .iter()
        .map(|param| {
            let val = bindings
                .get(param.name.as_str())
                .and_then(|bound| eval_sources(bound, ctx))
                .or_else(|| eval_sources(&param.default, ctx));
            (param.name.clone(), val.unwrap_or_default())
        })
        .collect()
}

/// Returns the first non-empty value from a fallback chain of sources.
fn eval_sources(sources: &[Source], ctx: &TypeEvalContext<'_>) -> Option<String> {
    eval_sources_with_origin(sources, ctx).map(|(val, _)| val)
}

/// Like [`eval_sources`], also returning the source that supplied the value.
fn eval_sources_with_origin<'s>(sources: &'s [Source], ctx: &TypeEvalContext<'_>) -> Option<(String, &'s Source)> {
    sources.iter().find_map(|src| {
        let val = match src {
            Source::Opt { name, .. } => ctx.opt_values.get(name),
            Source::Arg { name, .. } => ctx.arg_values.get(name).and_then(|vals| vals.first()),
            Source::Env { name, .. } => ctx.env.get(name),
            Source::Literal { value, .. } => return Some((value.clone(), src)),
        };
        val.filter(|v| !v.is_empty()).map(|v| (v.clone(), src))
    })
}

/// Spec spelling of a parameter source (`opt.host`, `arg.repo`, `env.HOME`, `"literal"`).
fn source_label(src: &Source) -> String {
    match src {
        Source::Opt { name, .. } => format!("opt.{name}"),
        Source::Arg { name, .. } => format!("arg.{name}"),
        Source::Env { name, .. } => format!("env.{name}"),
        Source::Literal { value, .. } => format!("\"{value}\""),
    }
}

/// How the parameters of the base type behind `expr` resolve on the current line (`ezlo explain`).
pub(super) fn param_bindings(ctx: &TypeEvalContext<'_>, expr: &TypeExpr) -> Vec<ParamBinding> {
    let type_map = ctx.index.type_map();
    let Some((refs, Target::Base(base))) = resolve_chain(expr, &type_map) else {
        return Vec::new();
    };
    base.params
        .iter()
        .map(|param| {
            let bound = refs.bindings.get(param.name.as_str()).copied();
            let chain = bound.unwrap_or(&param.default);
            let resolved = bound
                .and_then(|b| eval_sources_with_origin(b, ctx))
                .map(|(v, src)| (v, src, true))
                .or_else(|| eval_sources_with_origin(&param.default, ctx).map(|(v, src)| (v, src, false)));
            match resolved {
                Some((value, src, explicit)) => ParamBinding {
                    name: param.name.clone(),
                    value: Some(value),
                    source: source_label(src),
                    explicit,
                },
                None => ParamBinding {
                    name: param.name.clone(),
                    value: None,
                    source: chain.iter().map(source_label).collect::<Vec<_>>().join(" | "),
                    explicit: bound.is_some(),
                },
            }
        })
        .collect()
}

fn fetch_base_rows(
    base: &BaseType,
    params: &[(String, String)],
    ctx: &TypeEvalContext<'_>,
    refreshes: &mut Vec<PendingRefresh>,
    memo: &mut RowMemo,
) -> Arc<Vec<Vec<String>>> {
    let command = match &base.generator {
        Generator::Values { values, .. } => {
            return Arc::new(values.iter().map(|v| vec![v.clone()]).collect());
        }
        Generator::Shell { command, .. } => command,
    };
    let ttl = Duration::from_millis(base.cache_ms.unwrap_or(0));
    let key = CacheKey {
        base_type: base.name.clone(),
        params: params.to_vec(),
        cwd: ctx.cwd.to_path_buf(),
    };
    if let Some(cached) = memo.get(&key) {
        return Arc::clone(cached);
    }
    let rows = match ctx.cache.lookup(&key) {
        CacheLookup::Fresh(rows) => rows,
        CacheLookup::Stale(rows) if ctx.allow_stale => {
            refreshes.push(PendingRefresh {
                cache_key: key.clone(),
                command: command.clone(),
                num_cols: base.cols.len(),
                ttl,
            });
            rows
        }
        CacheLookup::Stale(_) | CacheLookup::Miss => {
            let lines = ctx.runner.run_shell(command, ctx.cwd, ctx.env, params, ctx.timeout);
            let parsed = parse_tsv_rows(&lines, base.cols.len());
            ctx.cache.insert(key.clone(), parsed, ttl)
        }
    };
    memo.insert(key, Arc::clone(&rows));
    rows
}
