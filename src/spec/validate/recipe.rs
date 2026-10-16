use super::slots::{analyze_template_slots, TemplateSlotInfo};
use super::type_expr::validate_type_expr;
use super::is_path_builtin_type;
use crate::spec::ast::{RecipeDecl, Source, Spec, TypeDecl, TypeExpr, TypeRef};
use crate::spec::span::{Diagnostic, RuleCode, Span};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

pub(super) fn validate_recipe(
    recipe: &RecipeDecl,
    spec: &Spec,
    type_map: &HashMap<&str, &TypeDecl>,
    diags: &mut Vec<Diagnostic>,
) {
    if !recipe.scope.is_empty() && !recipe.template.is_empty() && !template_matches_scope(recipe) {
        diags.push(Diagnostic::error(
            RuleCode::E08RecipeStructure,
            recipe.template_span,
            format!(
                "nested recipe template must start with command path '{}'",
                recipe.scope.join(" ")
            ),
        ));
    }

    let mut seen_args = HashSet::new();
    for rarg in &recipe.args {
        if !seen_args.insert(rarg.name.as_str()) {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                rarg.span,
                format!("duplicate arg '<{}>' in recipe", rarg.name),
            ));
        }
        if !recipe.placeholders.contains(&rarg.name) {
            diags.push(Diagnostic::error(
                RuleCode::E08RecipeStructure,
                rarg.span,
                format!(
                    "recipe arg '<{}>' does not appear in template",
                    rarg.name
                ),
            ));
        }
        if let Some(te) = &rarg.type_expr {
            validate_type_expr(te, type_map, diags);
        }
    }

    if has_recipe_dependency_cycle(recipe, spec, type_map) {
        diags.push(Diagnostic::error(
            RuleCode::E09RecipeCycle,
            recipe.span,
            "placeholder dependency cycle in recipe",
        ));
    }
}

fn template_matches_scope(recipe: &RecipeDecl) -> bool {
    let mut words = recipe.template.split_whitespace();
    recipe.scope.iter().all(|s| words.next() == Some(s.as_str()))
}

pub fn has_recipe_dependency_cycle(
    recipe: &RecipeDecl,
    spec: &Spec,
    type_map: &HashMap<&str, &TypeDecl>,
) -> bool {
    let slot_info = analyze_template_slots(&recipe.template, &spec.cmds);
    let ph_set: HashSet<&str> = recipe.placeholders.iter().map(String::as_str).collect();
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();

    for ph in &recipe.placeholders {
        let ph_str = ph.as_str();
        let mut ph_deps = Vec::new();
        if let Some(te) = infer_placeholder_type_ref(ph_str, recipe, &slot_info, type_map) {
            for src in type_expr_source_refs(&te, type_map) {
                let target_name = match src {
                    Source::Arg { name, .. } => Some(name.as_str()),
                    Source::Opt { name, .. } => slot_info.opt_to_placeholder.get(name).map(String::as_str),
                    _ => None,
                };
                if let Some(&target) = target_name.and_then(|n| ph_set.get(n)) {
                    ph_deps.push(target);
                }
            }
        }
        deps.insert(ph_str, ph_deps);
    }

    let mut state: HashMap<&str, u8> = HashMap::new();
    recipe
        .placeholders
        .iter()
        .any(|ph| dfs_cycle(ph.as_str(), &deps, &mut state))
}

fn dfs_cycle<'a>(
    node: &'a str,
    deps: &HashMap<&'a str, Vec<&'a str>>,
    state: &mut HashMap<&'a str, u8>,
) -> bool {
    match state.get(node).copied().unwrap_or(0) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    state.insert(node, 1);
    if let Some(neighbors) = deps.get(node) {
        for &next in neighbors {
            if dfs_cycle(next, deps, state) {
                return true;
            }
        }
    }
    state.insert(node, 2);
    false
}

pub fn infer_placeholder_type_expr(
    ph: &str,
    recipe: &RecipeDecl,
    slot_info: &TemplateSlotInfo<'_>,
    type_map: &HashMap<&str, &TypeDecl>,
) -> Option<TypeExpr> {
    infer_placeholder_type_ref(ph, recipe, slot_info, type_map).map(Cow::into_owned)
}

/// Borrowing variant of [`infer_placeholder_type_expr`]; only synthesizes a `TypeExpr` when the
/// placeholder name itself names a type.
fn infer_placeholder_type_ref<'a>(
    ph: &str,
    recipe: &'a RecipeDecl,
    slot_info: &TemplateSlotInfo<'a>,
    type_map: &HashMap<&str, &TypeDecl>,
) -> Option<Cow<'a, TypeExpr>> {
    let explicit = recipe
        .args
        .iter()
        .find(|a| a.name == ph)
        .and_then(|a| a.type_expr.as_deref());
    if let Some(te) = explicit.or_else(|| slot_info.placeholder_inferred_type.get(ph).copied()) {
        return Some(Cow::Borrowed(te));
    }
    if !(type_map.contains_key(ph) || is_path_builtin_type(ph)) {
        return None;
    }
    Some(Cow::Owned(TypeExpr {
        type_ref: TypeRef {
            name: ph.to_string(),
            bindings: Vec::new(),
            span: Span::default(),
        },
        where_cond: None,
        rank: None,
        span: Span::default(),
    }))
}

pub fn collect_type_expr_sources(
    expr: &TypeExpr,
    type_map: &HashMap<&str, &TypeDecl>,
) -> Vec<Source> {
    type_expr_source_refs(expr, type_map).into_iter().cloned().collect()
}

/// Borrowing variant of [`collect_type_expr_sources`].
fn type_expr_source_refs<'a>(
    expr: &'a TypeExpr,
    type_map: &HashMap<&'a str, &'a TypeDecl>,
) -> Vec<&'a Source> {
    let mut sources = Vec::new();
    let mut bound_params: HashSet<&str> = HashSet::new();

    for b in &expr.type_ref.bindings {
        bound_params.insert(&b.name);
        sources.extend(&b.sources);
    }

    let mut cur = expr.type_ref.name.as_str();
    let mut visited = HashSet::new();
    while let Some(td) = type_map.get(cur) {
        if !visited.insert(cur) {
            break;
        }
        match td {
            TypeDecl::Base(base) => {
                for param in &base.params {
                    if !bound_params.contains(param.name.as_str()) {
                        sources.extend(&param.default);
                    }
                }
                break;
            }
            TypeDecl::Refine(refine) => {
                for b in &refine.refines.type_ref.bindings {
                    if bound_params.insert(&b.name) {
                        sources.extend(&b.sources);
                    }
                }
                cur = &refine.refines.type_ref.name;
            }
        }
    }

    sources
}
