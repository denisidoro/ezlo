use crate::spec::ast::{ArgDecl, CmdDecl, OptDecl, TypeExpr, WhenClause};
use std::collections::HashMap;

/// Flags present in a recipe template, mapped to their (optional) literal value.
type PresentOpts<'t> = HashMap<&'t str, Option<&'t str>>;

#[derive(Default)]
pub struct TemplateSlotInfo<'a> {
    pub opt_to_placeholder: HashMap<String, String>,
    pub placeholder_inferred_type: HashMap<String, &'a TypeExpr>,
}

pub fn analyze_template_slots<'a>(template: &str, cmds: &'a [CmdDecl]) -> TemplateSlotInfo<'a> {
    let mut info = TemplateSlotInfo::default();
    let tokens: Vec<&str> = template.split_whitespace().collect();
    let Some(cmd) = match_cmd(&tokens, cmds) else {
        return info;
    };
    let matched_len = cmd.path.len();

    let active_opts: Vec<&OptDecl> = cmds
        .iter()
        .filter(|other| other.path.len() < cmd.path.len() && cmd.path.starts_with(&other.path))
        .flat_map(|other| &other.opts)
        .filter(|opt| opt.global)
        .chain(&cmd.opts)
        .collect();

    let present_opts = collect_present_opts(&tokens[matched_len..], &active_opts);

    // Second pass: map placeholders to option/positional slots and evaluate `when`. Only the first
    // value of an option is its `opt.<name>` binding; every value slot infers its own type.
    let record_opt = |opt: &'a OptDecl, vi: usize, val: &str, info: &mut TemplateSlotInfo<'a>| {
        let Some(ph) = strip_single_placeholder(val) else {
            return;
        };
        let te = if vi == 0 {
            info.opt_to_placeholder
                .insert(opt.name.clone(), ph.to_string());
            select_opt_effective_type(opt, &present_opts)
        } else {
            opt.value_at(vi).and_then(|v| v.type_expr.as_deref())
        };
        if let Some(te) = te {
            info.placeholder_inferred_type.insert(ph.to_string(), te);
        }
    };

    let mut idx = matched_len;
    let mut pos_idx = 0usize;
    let mut after_double_dash = false;
    while idx < tokens.len() {
        let tok = tokens[idx];
        if !after_double_dash && tok == "--" {
            after_double_dash = true;
            idx += 1;
            continue;
        }
        if !after_double_dash {
            if let Some((flag_part, val_part)) =
                tok.split_once('=').filter(|(f, _)| f.starts_with('-'))
            {
                if let Some(opt) = find_opt_by_flag(&active_opts, flag_part) {
                    record_opt(opt, 0, val_part, &mut info);
                    // `--flag=v1 v2`: the remaining values of a multi-value option follow.
                    let n = opt.extra_values.len().min(tokens.len() - idx - 1);
                    for (vi, val) in tokens[idx + 1..idx + 1 + n].iter().enumerate() {
                        record_opt(opt, vi + 1, val, &mut info);
                    }
                    idx += n;
                }
                idx += 1;
                continue;
            }
            if tok.starts_with('-') && tok.len() > 1 {
                let opt = find_opt_by_flag(&active_opts, tok).or_else(|| {
                    if tok.starts_with("--") {
                        None
                    } else {
                        tok.chars()
                            .last()
                            .and_then(|ch| find_opt_by_short(&active_opts, ch))
                    }
                });
                match opt {
                    Some(opt) => {
                        let n = detached_words(opt, &tokens[idx + 1..]);
                        for (vi, val) in tokens[idx + 1..idx + 1 + n].iter().enumerate() {
                            record_opt(opt, vi, val, &mut info);
                        }
                        idx += 1 + n;
                    }
                    None => idx += 1,
                }
                continue;
            }
        }

        if let Some(ph) = strip_single_placeholder(tok) {
            let arg_decl = cmd
                .args
                .get(pos_idx)
                .or_else(|| cmd.args.last().filter(|a| a.variadic));
            if let Some(te) = arg_decl.and_then(|arg| select_arg_effective_type(arg, &present_opts))
            {
                info.placeholder_inferred_type.insert(ph.to_string(), te);
            }
        }
        pos_idx += 1;
        idx += 1;
    }

    info
}

/// Finds the command with the longest path that prefixes the template tokens.
fn match_cmd<'a>(tokens: &[&str], cmds: &'a [CmdDecl]) -> Option<&'a CmdDecl> {
    if tokens.is_empty() {
        return None;
    }
    let mut matched: Option<&CmdDecl> = None;
    for cmd in cmds {
        let is_prefix = tokens.len() >= cmd.path.len()
            && tokens
                .iter()
                .enumerate()
                .zip(&cmd.path)
                .all(|((i, t), p)| *t == p.as_str() || is_alias_of(cmds, &cmd.path[..=i], t));
        if is_prefix && cmd.path.len() > matched.map_or(0, |m| m.path.len()) {
            matched = Some(cmd);
        }
    }
    matched
}

/// Whether `word` is a declared alias of the command at `path`.
fn is_alias_of(cmds: &[CmdDecl], path: &[String], word: &str) -> bool {
    cmds.iter().any(|c| c.path == path && c.aliases.iter().any(|a| a == word))
}

/// First pass: collect present options and their values in the template (up to `--`).
fn collect_present_opts<'t>(tokens: &[&'t str], active_opts: &[&'t OptDecl]) -> PresentOpts<'t> {
    let mut present: PresentOpts<'t> = HashMap::new();
    let mark = |opt: &'t OptDecl, val: Option<&'t str>, present: &mut PresentOpts<'t>| {
        for f in &opt.flags {
            present.insert(f, val);
        }
    };
    let mut idx = 0;
    while idx < tokens.len() {
        let tok = tokens[idx];
        if tok == "--" {
            break;
        }
        if let Some((flag_part, val_part)) = tok.split_once('=').filter(|(f, _)| f.starts_with('-'))
        {
            if let Some(opt) = find_opt_by_flag(active_opts, flag_part) {
                mark(opt, Some(val_part), &mut present);
                idx += opt.extra_values.len().min(tokens.len() - idx - 1);
            }
        } else if let Some(opt) = find_opt_by_flag(active_opts, tok) {
            // Exact spelling: `--flag`, `-f`, or a single-dash long flag such as `-name`.
            let n = detached_words(opt, &tokens[idx + 1..]);
            let val = (n > 0).then(|| tokens[idx + 1]);
            idx += n;
            mark(opt, val, &mut present);
        } else if tok.starts_with('-') && !tok.starts_with("--") && tok.len() > 1 {
            let cluster = &tok[1..];
            for (ci, ch) in cluster.char_indices() {
                let Some(opt) = find_opt_by_short(active_opts, ch) else {
                    continue;
                };
                if opt.value.is_none() {
                    mark(opt, None, &mut present);
                    continue;
                }
                let rest = &cluster[ci + ch.len_utf8()..];
                let val = if !rest.is_empty() {
                    idx += opt.extra_values.len().min(tokens.len() - idx - 1);
                    Some(rest)
                } else {
                    let n = detached_words(opt, &tokens[idx + 1..]);
                    let val = (n > 0).then(|| tokens[idx + 1]);
                    idx += n;
                    val
                };
                mark(opt, val, &mut present);
                break;
            }
        }
        idx += 1;
    }
    present
}

/// Template words after a detached flag spelling that are its values: every value of a
/// mandatory-value option (as many as remain), and for a `greedy` optional value the next word
/// when it is a placeholder or a value it takes (see [`OptDecl::takes_detached_value`]).
fn detached_words(opt: &OptDecl, rest: &[&str]) -> usize {
    if opt.optional_value() {
        let takes = rest
            .first()
            .is_some_and(|w| opt.greedy && (strip_single_placeholder(w).is_some() || opt.takes_detached_value(w)));
        return usize::from(takes);
    }
    opt.detached_value_count().min(rest.len())
}

fn find_opt_by_flag<'a>(opts: &[&'a OptDecl], flag: &str) -> Option<&'a OptDecl> {
    opts.iter()
        .copied()
        .find(|o| o.flags.iter().any(|f| f == flag))
}

/// Like [`find_opt_by_flag`] for `-<ch>`, without allocating the flag string.
fn find_opt_by_short<'a>(opts: &[&'a OptDecl], ch: char) -> Option<&'a OptDecl> {
    let is_short = |f: &str| {
        let mut it = f.chars();
        it.next() == Some('-') && it.next() == Some(ch) && it.next().is_none()
    };
    opts.iter()
        .copied()
        .find(|o| o.flags.iter().any(|f| is_short(f)))
}

fn eval_when_clauses<'a>(
    when_clauses: &'a [WhenClause],
    present_opts: &PresentOpts<'_>,
) -> Option<&'a TypeExpr> {
    when_clauses
        .iter()
        .find(|wc| {
            wc.cond.iter().all(|term| {
                let present = present_opts.get(term.flag.as_str());
                if term.negated {
                    present.is_none()
                } else if let Some(expected_val) = &term.value {
                    present.copied().flatten() == Some(expected_val.as_str())
                } else {
                    present.is_some()
                }
            })
        })
        .map(|wc| &wc.type_expr)
}

fn select_opt_effective_type<'a>(
    opt: &'a OptDecl,
    present_opts: &PresentOpts<'_>,
) -> Option<&'a TypeExpr> {
    eval_when_clauses(&opt.when, present_opts).or_else(|| opt.value.as_deref()?.type_expr.as_deref())
}

fn select_arg_effective_type<'a>(
    arg: &'a ArgDecl,
    present_opts: &PresentOpts<'_>,
) -> Option<&'a TypeExpr> {
    eval_when_clauses(&arg.when, present_opts).or(arg.type_expr.as_deref())
}

fn strip_single_placeholder(tok: &str) -> Option<&str> {
    let inner = tok.strip_prefix('<')?.strip_suffix('>')?;
    let valid = !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    valid.then_some(inner)
}
