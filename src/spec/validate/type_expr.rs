use super::{builtin_type_cols, is_valid_env_name, resolve_base_type};
use crate::spec::ast::{BaseType, Clause, Source, TypeDecl, TypeExpr};
use crate::spec::span::{Diagnostic, RuleCode};
use std::collections::{HashMap, HashSet};

pub(super) fn validate_base_type(base: &BaseType, diags: &mut Vec<Diagnostic>) {
    let mut param_names = HashSet::new();
    for param in &base.params {
        if !is_valid_env_name(&param.name) {
            diags.push(Diagnostic::error(
                RuleCode::E10EnvName,
                param.span,
                format!("parameter '{}' is not a valid environment variable name", param.name),
            ));
        }
        if !param_names.insert(param.name.as_str()) {
            diags.push(Diagnostic::error(
                RuleCode::E04Duplicate,
                param.span,
                format!("duplicate parameter '{}' in type '{}'", param.name, base.name),
            ));
        }
        check_source_chain(&param.default, diags);
    }

    let mut col_set = HashSet::new();
    for (col, span) in &base.col_spans {
        if !is_valid_env_name(col) {
            diags.push(Diagnostic::error(
                RuleCode::E10EnvName,
                *span,
                format!("column '{col}' is not a valid environment variable name"),
            ));
        }
        if param_names.contains(col.as_str()) {
            diags.push(Diagnostic::error(
                RuleCode::E10EnvName,
                *span,
                format!(
                    "column '{col}' collides with parameter of the same name in type '{}'",
                    base.name
                ),
            ));
        }
        col_set.insert(col.as_str());
    }

    if !col_set.contains(base.insert.as_str()) {
        diags.push(Diagnostic::error(
            RuleCode::E05Unresolved,
            base.insert_span.unwrap_or(base.span),
            format!(
                "insert column '{}' does not exist in cols of type '{}'",
                base.insert, base.name
            ),
        ));
    }

    for (scol, sspan) in &base.show_spans {
        if !col_set.contains(scol.as_str()) {
            diags.push(Diagnostic::error(
                RuleCode::E05Unresolved,
                *sspan,
                format!("show column '{scol}' does not exist in cols of type '{}'", base.name),
            ));
        }
    }
}

fn check_source_chain(sources: &[Source], diags: &mut Vec<Diagnostic>) {
    let mut seen_literal = false;
    for src in sources {
        if seen_literal {
            diags.push(Diagnostic::warning(
                RuleCode::W01UnreachableSource,
                src.span(),
                "unreachable source after string literal in fallback chain",
            ));
        }
        if matches!(src, Source::Literal { .. }) {
            seen_literal = true;
        }
    }
}

pub(super) fn validate_type_expr(
    expr: &TypeExpr,
    type_map: &HashMap<&str, &TypeDecl>,
    diags: &mut Vec<Diagnostic>,
) {
    let ref_name = expr.type_ref.name.as_str();
    for binding in &expr.type_ref.bindings {
        check_source_chain(&binding.sources, diags);
    }

    if let Some(cols) = builtin_type_cols(ref_name).filter(|_| !type_map.contains_key(ref_name)) {
        for binding in &expr.type_ref.bindings {
            diags.push(Diagnostic::error(
                RuleCode::E05Unresolved,
                binding.span,
                format!("builtin type '{ref_name}' does not accept parameter '{}'", binding.name),
            ));
        }
        let col_set: HashSet<&str> = cols.iter().copied().collect();
        validate_expr_clauses(expr, &col_set, ref_name, diags);
        return;
    }

    if !type_map.contains_key(ref_name) {
        diags.push(Diagnostic::error(
            RuleCode::E05Unresolved,
            expr.type_ref.span,
            format!("unresolved type reference '{ref_name}'"),
        ));
        return;
    }

    let Some(base) = resolve_base_type(ref_name, type_map) else {
        return;
    };

    for binding in &expr.type_ref.bindings {
        if !base.params.iter().any(|p| p.name == binding.name) {
            diags.push(Diagnostic::error(
                RuleCode::E05Unresolved,
                binding.span,
                format!(
                    "unresolved parameter '{}' for type '{ref_name}'",
                    binding.name
                ),
            ));
        }
    }

    let col_set: HashSet<&str> = base.cols.iter().map(String::as_str).collect();
    validate_expr_clauses(expr, &col_set, ref_name, diags);
}

/// Validates both the `where` and `rank` clauses of `expr` against `col_set`.
fn validate_expr_clauses(
    expr: &TypeExpr,
    col_set: &HashSet<&str>,
    type_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for clauses in [&expr.where_cond, &expr.rank].into_iter().flatten() {
        validate_clauses(clauses, col_set, type_name, diags);
    }
}

fn validate_clauses(
    clauses: &[Clause],
    col_set: &HashSet<&str>,
    type_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for clause in clauses {
        if !col_set.contains(clause.col.as_str()) {
            diags.push(Diagnostic::error(
                RuleCode::E05Unresolved,
                clause.span,
                format!(
                    "unresolved column '{}' in condition on type '{type_name}'",
                    clause.col
                ),
            ));
        }
    }
}
