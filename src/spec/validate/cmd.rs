use super::type_expr::validate_type_expr;
use crate::spec::ast::{CmdDecl, Composite, OptDecl, TypeDecl, WhenClause};
use crate::spec::preview::{preview_refs, PreviewSource};
use crate::spec::span::{Diagnostic, RuleCode};
use std::collections::{HashMap, HashSet};

pub(super) fn validate_cmd(
    cmd: &CmdDecl,
    all_cmds: &[CmdDecl],
    type_map: &HashMap<&str, &TypeDecl>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut effective_flags: HashSet<&str> = all_cmds
        .iter()
        .filter(|other| other.path.len() < cmd.path.len() && cmd.path.starts_with(&other.path))
        .flat_map(|other| &other.opts)
        .filter(|opt| opt.global)
        .flat_map(|opt| opt.flags.iter().map(String::as_str))
        .collect();

    for opt in &cmd.opts {
        for (flag, fspan) in &opt.flag_spans {
            if !effective_flags.insert(flag) {
                diags.push(Diagnostic::error(
                    RuleCode::E04Duplicate,
                    *fspan,
                    format!(
                        "duplicate option flag '{flag}' in command '{}'",
                        cmd.path.join(" ")
                    ),
                ));
            }
        }
    }

    for opt in &cmd.opts {
        validate_opt(opt, &effective_flags, type_map, diags);
    }

    let mut seen_optional = false;
    let mut variadic_count = 0usize;
    for (idx, arg) in cmd.args.iter().enumerate() {
        if arg.required && seen_optional {
            diags.push(Diagnostic::error(
                RuleCode::E07PositionalArgs,
                arg.span,
                format!(
                    "required argument '<{}>' cannot follow an optional argument",
                    arg.name
                ),
            ));
        }
        if !arg.required {
            seen_optional = true;
        }
        if arg.variadic {
            variadic_count += 1;
            if variadic_count > 1 {
                diags.push(Diagnostic::error(
                    RuleCode::E07PositionalArgs,
                    arg.span,
                    "command cannot have more than one variadic positional argument",
                ));
            } else if idx + 1 != cmd.args.len() {
                diags.push(Diagnostic::error(
                    RuleCode::E07PositionalArgs,
                    arg.span,
                    "variadic positional argument must be the last argument",
                ));
            }
        }
        if let Some(te) = &arg.type_expr {
            validate_type_expr(te, type_map, diags);
        }
        validate_composite(arg.composite.as_deref(), type_map, diags);
        validate_when(&arg.when, &effective_flags, type_map, diags);
    }
    validate_preview(cmd, all_cmds, diags);
}

/// Every `{opt.<name>}` / `{arg.<name>}` of a `preview` must name an option of the command (or a
/// `global` option of an ancestor) or one of its positional args.
fn validate_preview(cmd: &CmdDecl, all_cmds: &[CmdDecl], diags: &mut Vec<Diagnostic>) {
    let (Some(template), Some(span)) = (&cmd.preview, cmd.preview_span) else {
        return;
    };
    let ancestor_globals = all_cmds
        .iter()
        .filter(|other| other.path.len() < cmd.path.len() && cmd.path.starts_with(&other.path))
        .flat_map(|other| &other.opts)
        .filter(|opt| opt.global);
    let opt_names: HashSet<&str> = cmd
        .opts
        .iter()
        .chain(ancestor_globals)
        .map(|o| o.name.as_str())
        .collect();
    for r in preview_refs(template) {
        let (known, kind) = match r.source {
            PreviewSource::Opt => (opt_names.contains(r.name.as_str()), "opt"),
            PreviewSource::Arg => (cmd.args.iter().any(|a| a.name == r.name), "arg"),
        };
        if !known {
            diags.push(Diagnostic::error(
                RuleCode::E05Unresolved,
                span,
                format!("unresolved '{{{kind}.{}}}' in 'preview' of '{}'", r.name, cmd.path.join(" ")),
            ));
        }
    }
}

fn validate_opt(
    opt: &OptDecl,
    effective_flags: &HashSet<&str>,
    type_map: &HashMap<&str, &TypeDecl>,
    diags: &mut Vec<Diagnostic>,
) {
    if opt.value.is_some() {
        for vspec in opt.value_slots() {
            if let Some(te) = &vspec.type_expr {
                validate_type_expr(te, type_map, diags);
            }
            validate_composite(vspec.composite.as_deref(), type_map, diags);
        }
    } else if let Some(first) = opt.when.first() {
        diags.push(Diagnostic::warning(
            RuleCode::W02ValuelessOptWhen,
            first.span,
            "'when' on an option that takes no value has no effect",
        ));
    }

    for (ex_flag, ex_span) in &opt.exclude_spans {
        if !effective_flags.contains(ex_flag.as_str()) {
            diags.push(Diagnostic::error(
                RuleCode::E05Unresolved,
                *ex_span,
                format!("unresolved flag '{ex_flag}' in 'excludes'"),
            ));
        }
    }

    validate_when(&opt.when, effective_flags, type_map, diags);
}

/// Validates the value type of a `kv("<sep>", <type>)` modifier.
fn validate_composite(
    composite: Option<&Composite>,
    type_map: &HashMap<&str, &TypeDecl>,
    diags: &mut Vec<Diagnostic>,
) {
    if let Some(Composite::Kv {
        value_type: Some(te), ..
    }) = composite
    {
        validate_type_expr(te, type_map, diags);
    }
}

/// Checks that every flag referenced by `when` conditions resolves, then validates the clause type.
fn validate_when(
    clauses: &[WhenClause],
    effective_flags: &HashSet<&str>,
    type_map: &HashMap<&str, &TypeDecl>,
    diags: &mut Vec<Diagnostic>,
) {
    for wc in clauses {
        for term in &wc.cond {
            if !effective_flags.contains(term.flag.as_str()) {
                diags.push(Diagnostic::error(
                    RuleCode::E05Unresolved,
                    term.span,
                    format!("unresolved flag '{}' in 'when' condition", term.flag),
                ));
            }
        }
        validate_type_expr(&wc.type_expr, type_map, diags);
    }
}
