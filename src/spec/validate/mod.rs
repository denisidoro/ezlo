mod cmd;
mod recipe;
mod slots;
mod type_expr;

pub use recipe::{collect_type_expr_sources, has_recipe_dependency_cycle, infer_placeholder_type_expr};
pub use slots::{analyze_template_slots, TemplateSlotInfo};

use crate::spec::ast::{AliasDecl, BaseType, CmdDecl, Spec, TypeDecl};
use crate::spec::span::{Diagnostic, RuleCode, Span};
use std::collections::{HashMap, HashSet};

/// Builtin types and the columns their rows expose to `where`/`rank` clauses (`value` is the
/// inserted column). A declared type with the same name shadows the builtin.
const BUILTIN_TYPES: &[(&str, &[&str])] = &[
    ("file", &["value"]),
    ("dir", &["value"]),
    ("path", &["value"]),
    ("cmd", &["value", "kind", "location"]),
    ("env", &["value", "current"]),
    ("pid", &["value", "name", "user", "args"]),
    ("signal", &["value", "number", "desc"]),
    ("port", &["value", "proto", "address", "process"]),
    ("user", &["value", "uid", "name", "home", "shell"]),
    ("group", &["value", "gid", "members"]),
    ("host", &["value", "source"]),
    ("interface", &["value", "state", "address"]),
];

pub fn is_builtin_type(name: &str) -> bool {
    builtin_type_cols(name).is_some()
}

/// Filesystem builtins (`file`, `dir`, `path`): the only builtins a slot or placeholder also gets
/// implicitly from its name (`arg <file>`); system types need an explicit reference.
pub fn is_path_builtin_type(name: &str) -> bool {
    matches!(name, "file" | "dir" | "path")
}

pub fn builtin_type_cols(name: &str) -> Option<&'static [&'static str]> {
    BUILTIN_TYPES.iter().find(|(n, _)| *n == name).map(|(_, cols)| *cols)
}

pub fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn validate_spec(spec: &Spec, diags: &mut Vec<Diagnostic>) {
    let mut type_map: HashMap<&str, &TypeDecl> = HashMap::new();

    for td in &spec.types {
        let name = td.name();
        if type_map.contains_key(name) {
            diags.push(Diagnostic::error(
                RuleCode::E04Duplicate,
                td.span(),
                format!("duplicate type declaration '{name}'"),
            ));
        } else {
            type_map.insert(name, td);
        }
    }

    for td in &spec.types {
        match td {
            TypeDecl::Base(base) => type_expr::validate_base_type(base, diags),
            TypeDecl::Refine(refine) => {
                if let Some((prefix, _)) = refine.name.split_once(':') {
                    let target = refine.refines.type_ref.name.as_str();
                    let resolved_base = resolve_base_type(target, &type_map).map(|b| b.name.as_str());
                    let target_has_prefix = target
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with(':'));
                    if target != prefix && !target_has_prefix && resolved_base != Some(prefix) {
                        diags.push(Diagnostic::warning(
                            RuleCode::W03RefinementPrefix,
                            refine.span,
                            format!(
                                "refinement '{}' has prefix '{prefix}:' but refines '{target}'",
                                refine.name
                            ),
                        ));
                    }
                }
                if let Some(cycle_span) = check_refinement_cycle(&refine.name, &type_map) {
                    diags.push(Diagnostic::error(
                        RuleCode::E05Unresolved,
                        cycle_span,
                        format!("cyclic type refinement involving '{}'", refine.name),
                    ));
                }
                type_expr::validate_type_expr(&refine.refines, &type_map, diags);
            }
        }
    }

    let mut cmd_paths: HashSet<&[String]> = HashSet::new();
    for cmd in &spec.cmds {
        if !cmd_paths.insert(&cmd.path) {
            diags.push(Diagnostic::error(
                RuleCode::E04Duplicate,
                cmd.span,
                format!("duplicate cmd declaration '{}'", cmd.path.join(" ")),
            ));
        }
    }
    validate_aliases(&spec.cmds, diags);
    validate_top_aliases(&spec.aliases, &spec.cmds, diags);

    for cmd in &spec.cmds {
        cmd::validate_cmd(cmd, &spec.cmds, &type_map, diags);
    }

    for recipe in &spec.recipes {
        recipe::validate_recipe(recipe, spec, &type_map, diags);
    }
}

/// Aliases must not collide with a sibling command word or another sibling's alias (E04).
fn validate_aliases(cmds: &[CmdDecl], diags: &mut Vec<Diagnostic>) {
    // (parent path, word) -> spelling owner (canonical last word)
    let mut words: HashMap<(&[String], &str), &str> = HashMap::new();
    for cmd in cmds {
        let Some((last, parent)) = cmd.path.split_last() else { continue };
        words.entry((parent, last.as_str())).or_insert(last.as_str());
    }
    for cmd in cmds {
        let Some((last, parent)) = cmd.path.split_last() else { continue };
        for alias in &cmd.aliases {
            match words.get(&(parent, alias.as_str())) {
                Some(owner) if *owner != last.as_str() || alias == last => {
                    diags.push(Diagnostic::error(
                        RuleCode::E04Duplicate,
                        cmd.span,
                        format!(
                            "alias '{alias}' of '{}' collides with command word '{owner}'",
                            cmd.path.join(" ")
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    words.insert((parent, alias.as_str()), last.as_str());
                }
            }
        }
    }
}

/// Top-level `alias` names must be unique within a file and must not hide a `cmd` root of the
/// same file (which would make its grammar unreachable), unless they expand to that root (E04).
fn validate_top_aliases(aliases: &[AliasDecl], cmds: &[CmdDecl], diags: &mut Vec<Diagnostic>) {
    let roots: HashSet<&str> = cmds.iter().filter_map(|c| c.path.first()).map(String::as_str).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for alias in aliases {
        let name = alias.name.as_str();
        if !seen.insert(name) {
            diags.push(Diagnostic::error(
                RuleCode::E04Duplicate,
                alias.span,
                format!("duplicate alias '{name}'"),
            ));
        }
        let expands_to_self = alias.expansion.first().is_some_and(|w| w == name);
        if roots.contains(name) && !expands_to_self {
            diags.push(Diagnostic::error(
                RuleCode::E04Duplicate,
                alias.span,
                format!("alias '{name}' hides 'cmd {name}' declared in the same file"),
            ));
        }
    }
}

pub fn resolve_base_type<'a>(
    name: &str,
    type_map: &HashMap<&'a str, &'a TypeDecl>,
) -> Option<&'a BaseType> {
    let mut cur = name;
    let mut visited = HashSet::new();
    while let Some(td) = type_map.get(cur) {
        if !visited.insert(cur) {
            return None;
        }
        match td {
            TypeDecl::Base(b) => return Some(b),
            TypeDecl::Refine(r) => cur = &r.refines.type_ref.name,
        }
    }
    None
}

fn check_refinement_cycle(name: &str, type_map: &HashMap<&str, &TypeDecl>) -> Option<Span> {
    let mut cur = name;
    let mut visited = HashSet::new();
    while let Some(td) = type_map.get(cur) {
        if !visited.insert(cur) {
            return Some(td.span());
        }
        match td {
            TypeDecl::Base(_) => return None,
            TypeDecl::Refine(r) => cur = &r.refines.type_ref.name,
        }
    }
    None
}
