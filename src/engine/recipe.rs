use crate::engine::generator::{GeneratorCache, GeneratorRunner};
use crate::engine::line::shell_quote;
use crate::engine::rank::{filter_and_rank_candidates, match_query, EmptyUsageLookup};
use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::types::{evaluate_type_expr, TypeEvalContext};
use crate::spec::ast::{RecipeDecl, Source, TypeExpr};
use crate::spec::validate::{
    analyze_template_slots, collect_type_expr_sources, infer_placeholder_type_expr,
};
use crate::spec::SpecIndex;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ResolvedPlaceholder {
    pub name: String,
    pub variadic: bool,
    pub type_expr: Option<TypeExpr>,
    pub suggestions: Vec<String>,
    pub desc: Option<String>,
    pub depends_on: Vec<String>,
}

pub fn resolve_recipe_placeholders(
    recipe: &RecipeDecl,
    index: &SpecIndex,
) -> Vec<ResolvedPlaceholder> {
    let slot_info = analyze_template_slots(&recipe.template, &index.cmds);
    let type_map = index.type_map();
    let ph_set: HashSet<&str> = recipe.placeholders.iter().map(String::as_str).collect();

    let mut resolved_map: HashMap<String, ResolvedPlaceholder> = HashMap::new();
    for ph in &recipe.placeholders {
        let rarg = recipe.args.iter().find(|a| a.name == *ph);
        let variadic = rarg.map(|a| a.variadic).unwrap_or(false);
        let suggestions = rarg.map(|a| a.suggestions.clone()).unwrap_or_default();
        let desc = rarg.and_then(|a| a.desc.clone());
        let type_expr = infer_placeholder_type_expr(ph, recipe, &slot_info, &type_map);

        let mut depends_on = Vec::new();
        if let Some(te) = &type_expr {
            let sources = collect_type_expr_sources(te, &type_map);
            for src in sources {
                match src {
                    Source::Arg { name, .. } if ph_set.contains(name.as_str()) && !depends_on.contains(&name) => {
                        depends_on.push(name);
                    }
                    Source::Arg { .. } => {}
                    Source::Opt { name, .. } => {
                        if let Some(target_ph) = slot_info.opt_to_placeholder.get(&name)
                            && ph_set.contains(target_ph.as_str())
                                && !depends_on.contains(target_ph)
                            {
                                depends_on.push(target_ph.clone());
                            }
                    }
                    _ => {}
                }
            }
        }

        resolved_map.insert(
            ph.clone(),
            ResolvedPlaceholder {
                name: ph.clone(),
                variadic,
                type_expr,
                suggestions,
                desc,
                depends_on,
            },
        );
    }

    // Dependency-ordered topological sort preserving template order on ties (SYNTAX §7.5)
    let mut remaining: Vec<String> = recipe.placeholders.clone();
    let mut placed: HashSet<String> = HashSet::new();
    let mut ordered = Vec::with_capacity(recipe.placeholders.len());

    while !remaining.is_empty() {
        let next_pos = remaining.iter().position(|ph| {
            resolved_map
                .get(ph)
                .map(|rp| rp.depends_on.iter().all(|dep| placed.contains(dep)))
                .unwrap_or(true)
        });
        let idx = next_pos.unwrap_or(0);
        let chosen = remaining.remove(idx);
        placed.insert(chosen.clone());
        if let Some(rp) = resolved_map.remove(&chosen) {
            ordered.push(rp);
        }
    }

    ordered
}

#[allow(clippy::too_many_arguments)]
pub fn candidates_for_placeholder(
    placeholder: &ResolvedPlaceholder,
    recipe: &RecipeDecl,
    filled_values: &HashMap<String, Vec<String>>,
    index: &SpecIndex,
    cwd: &Path,
    env: &HashMap<String, String>,
    runner: &dyn GeneratorRunner,
    cache: &GeneratorCache,
    timeout: Duration,
    query: &str,
) -> Vec<Candidate> {
    let slot_info = analyze_template_slots(&recipe.template, &index.cmds);
    let mut opt_values = HashMap::new();
    for (opt_name, ph_name) in &slot_info.opt_to_placeholder {
        if let Some(vals) = filled_values.get(ph_name)
            && let Some(first) = vals.first() {
                opt_values.insert(opt_name.clone(), first.clone());
            }
    }

    let present_flags = HashMap::new();
    let no_aliases = HashMap::new();
    let eval_ctx = TypeEvalContext {
        index,
        cwd,
        env,
        aliases: &no_aliases,
        opt_values: &opt_values,
        arg_values: filled_values,
        present_flags: &present_flags,
        runner,
        cache,
        timeout,
        allow_stale: false,
    };

    let mut raw_candidates = Vec::new();
    let mut seen = HashSet::new();

    // Suggestions come before type candidates, and first suggestion is default (SYNTAX §7.5)
    for sug in &placeholder.suggestions {
        seen.insert(sug.clone());
        raw_candidates.push(Candidate {
            label: sug.clone(),
            description: placeholder
                .desc
                .clone()
                .unwrap_or_else(|| "suggested".to_string()),
            insert_value: sug.clone(),
            insert_text: shell_quote(sug),
            suffix: String::new(),
            kind: CandidateKind::Arg,
            priority_tier: 0,
            ..Default::default()
        });
    }

    if let Some(te) = &placeholder.type_expr {
        let mut refreshes = Vec::new();
        let rows = evaluate_type_expr(&eval_ctx, te, query, &mut refreshes);
        for row in rows {
            if !seen.insert(row.insert_value.clone()) {
                continue;
            }
            raw_candidates.push(Candidate {
                label: row.label,
                description: row.description,
                insert_value: row.insert_value,
                insert_text: row.insert_quoted,
                suffix: row.suffix,
                kind: if row.is_dir {
                    CandidateKind::Dir
                } else if row.is_file {
                    CandidateKind::File
                } else {
                    CandidateKind::Arg
                },
                deemphasized: row.rank_deemphasized,
                priority_tier: 1,
                raw_columns: row.raw_columns,
                preview_cmd: row.preview_cmd,
                columns: row.columns,
                style: row.style,
                icon: row.icon,
                ..Default::default()
            });
        }
    }

    filter_and_rank_candidates(
        raw_candidates,
        query,
        &recipe.scope.join(" "),
        &cwd.to_string_lossy(),
        &EmptyUsageLookup,
        0,
    )
}

pub fn substitute_recipe_template(
    raw_template: &str,
    filled: &HashMap<String, Vec<String>>,
) -> String {
    let chars: Vec<char> = raw_template.chars().collect();
    let mut out = String::with_capacity(raw_template.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '<' {
            out.push('<');
            i += 2;
            continue;
        }
        if chars[i] == '<' {
            let mut j = i + 1;
            let mut name = String::new();
            while j < chars.len() && chars[j] != '>' {
                name.push(chars[j]);
                j += 1;
            }
            if j < chars.len()
                && chars[j] == '>'
                && !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                if let Some(vals) = filled.get(&name) {
                    let joined = vals
                        .iter()
                        .map(|v| shell_quote(v))
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push_str(&joined);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    out
}

pub fn search_recipes<'a>(
    index: &'a SpecIndex,
    query: &str,
    active_cmd_path: &[String],
    recipe_usage: &HashMap<String, u64>,
) -> Vec<(usize, &'a RecipeDecl)> {
    let mut scored: Vec<(i64, usize, &'a RecipeDecl)> = Vec::new();

    for (idx, rec) in index.recipes.iter().enumerate() {
        let hay = format!("{} {} {}", rec.desc, rec.template, rec.scope.join(" "));
        let band_bonus = if query.is_empty() {
            100
        } else if let Some((band, _)) =
            match_query(query, &rec.desc).or_else(|| match_query(query, &hay))
        {
            match band {
                crate::engine::rank::MatchBand::Prefix => 200,
                crate::engine::rank::MatchBand::Fuzzy => 100,
            }
        } else {
            continue;
        };

        let scope_bonus = if !rec.scope.is_empty() && active_cmd_path.starts_with(&rec.scope) {
            500 + (rec.scope.len() as i64 * 50)
        } else {
            0
        };

        let usage_bonus = recipe_usage.get(&rec.template).copied().unwrap_or(0) as i64 * 25;
        let total = scope_bonus + band_bonus + usage_bonus;
        scored.push((total, idx, rec));
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .map(|(_, idx, rec)| (idx, rec))
        .collect()
}
