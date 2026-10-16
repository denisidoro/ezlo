//! Option (`-f` / `--flag`) candidates for the command under the cursor.

use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::rank::classify_match;
use crate::engine::walker::WalkState;
use crate::spec::ast::OptDecl;
use std::collections::HashSet;

fn tier(opt: &OptDecl) -> u8 {
    if opt.required {
        0
    } else {
        2
    }
}

fn flag_candidate(flag: &str, opt: &OptDecl) -> Candidate {
    Candidate::word(flag, opt.desc.as_deref(), CandidateKind::Opt, tier(opt))
}

/// `flag`, plus `--flag=` (no trailing space) for a long flag whose value is optional: the value
/// must be attached there, so `--color always` would not set it.
fn push_flag(out: &mut Vec<Candidate>, flag: &str, opt: &OptDecl) {
    out.push(flag_candidate(flag, opt));
    if opt.optional_value() && flag.starts_with("--") {
        out.push(flag_candidate(&format!("{flag}="), opt).with_suffix(""));
    }
}

/// Options still offerable: not yet used (unless repeatable) and not excluded either way.
fn available_opts<'w>(walk: &'w WalkState<'_>) -> impl Iterator<Item = &'w OptDecl> + 'w {
    let used = |opt: &OptDecl| walk.opt_occurrence_count.get(&opt.name).is_some_and(|&n| n > 0);
    let excluded: HashSet<&str> = walk
        .active_opts
        .iter()
        .filter(|o| used(o))
        .flat_map(|o| o.excludes.iter().map(String::as_str))
        .collect();
    walk.active_opts.iter().copied().filter(move |opt| {
        (opt.repeatable() || !used(opt))
            && !opt.excludes.iter().any(|ex| walk.present_flags.contains_key(ex))
            && !opt.flags.iter().any(|f| excluded.contains(f.as_str()))
    })
}

/// While typing a flag every matching spelling is offered (long first); otherwise only the
/// preferred (long) spelling of each option.
pub fn option_candidates(walk: &WalkState<'_>, typed_prefix: &str, out: &mut Vec<Candidate>) {
    let typing_flag = typed_prefix.starts_with('-');
    for opt in available_opts(walk) {
        if opt.hidden {
            out.extend(
                opt.flags
                    .iter()
                    .filter(|flag| reveals_hidden(typed_prefix, flag))
                    .map(|flag| flag_candidate(flag, opt)),
            );
            continue;
        }
        if typing_flag {
            let long_first = opt
                .flags
                .iter()
                .filter(|f| f.starts_with("--"))
                .chain(opt.flags.iter().filter(|f| !f.starts_with("--")));
            for flag in long_first.filter(|flag| classify_match(typed_prefix, flag).is_some()) {
                push_flag(out, flag, opt);
            }
            continue;
        }
        let preferred = opt
            .flags
            .iter()
            .find(|f| f.starts_with("--"))
            .or_else(|| opt.flags.first());
        let Some(flag) = preferred else { continue };
        if typed_prefix.is_empty() || classify_match(typed_prefix, flag).is_some() {
            push_flag(out, flag, opt);
        }
    }
}

/// A `hidden` option's flag is only offered once the user typed a prefix of it beyond the dashes
/// (`--verb` reveals `--verbose`; `-`, `--` or a non-flag prefix never do).
fn reveals_hidden(typed_prefix: &str, flag: &str) -> bool {
    typed_prefix.starts_with('-')
        && !typed_prefix.trim_start_matches('-').is_empty()
        && flag.starts_with(typed_prefix)
}

/// `-ab<TAB>`: appends each available short flag to the cluster typed so far (`hidden` ones excluded).
pub fn cluster_candidates(walk: &WalkState<'_>, cluster_prefix: &str, out: &mut Vec<Candidate>) {
    for opt in available_opts(walk).filter(|o| !o.hidden) {
        let shorts = opt
            .flags
            .iter()
            .filter(|f| f.len() == 2 && f.starts_with('-') && !f.starts_with("--"));
        for flag in shorts {
            let combined = format!("{cluster_prefix}{}", &flag[1..]);
            out.push(flag_candidate(&combined, opt));
        }
    }
}
