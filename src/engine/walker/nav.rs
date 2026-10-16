//! Spec navigation helpers: command-path lookups, active options, and subcommand listings.

use super::SubcmdCandidate;
use crate::spec::ast::{ArgDecl, CmdDecl, OptDecl};
use crate::spec::SpecIndex;
use std::collections::HashSet;

pub(crate) fn find_opt<'a>(opts: &[&'a OptDecl], flag: &str) -> Option<&'a OptDecl> {
    opts.iter().copied().find(|o| o.flags.iter().any(|f| f == flag))
}

/// Like [`find_opt`] for `-<ch>`, without allocating the flag string.
pub(crate) fn find_short_opt<'a>(opts: &[&'a OptDecl], ch: char) -> Option<&'a OptDecl> {
    let is_short = |f: &str| {
        let mut it = f.chars();
        it.next() == Some('-') && it.next() == Some(ch) && it.next().is_none()
    };
    opts.iter().copied().find(|o| o.flags.iter().any(|f| is_short(f)))
}

pub fn resolve_positional_arg(cmd: &CmdDecl, pos_idx: usize) -> Option<&ArgDecl> {
    cmd.args
        .get(pos_idx)
        .or_else(|| cmd.args.last().filter(|a| a.variadic))
}

/// Extends `cmd_path` with `word` when it names (or aliases) a child command; returns whether it did.
pub(crate) fn push_subcommand(index: &SpecIndex, cmd_path: &mut Vec<String>, word: &str) -> bool {
    match resolve_subcommand(index, cmd_path, word) {
        Some(canonical) => {
            let canonical = canonical.to_string();
            cmd_path.push(canonical);
            true
        }
        None => false,
    }
}

/// Options of the command at `cmd_path` followed by `global` options of its ancestors.
pub(crate) fn collect_active_opts<'a>(index: &'a SpecIndex, cmd_path: &[String]) -> Vec<&'a OptDecl> {
    let local = index
        .cmds_with_prefix(cmd_path)
        .filter(|cmd| cmd.path == cmd_path)
        .flat_map(|cmd| &cmd.opts);
    // Ancestors share `cmd_path`'s root word.
    let global = index
        .cmds_with_prefix(cmd_path.get(..1).unwrap_or_default())
        .filter(|cmd| cmd.path.len() < cmd_path.len() && cmd_path.starts_with(&cmd.path))
        .flat_map(|cmd| &cmd.opts)
        .filter(|opt| opt.global);
    local.chain(global).collect()
}

/// Canonical spelling of `word` as a child command of `parent` (empty `parent` => root command):
/// the word itself when some command path extends `parent` with it, else the command word that
/// declares `word` as an alias.
pub(crate) fn resolve_subcommand<'a>(index: &'a SpecIndex, parent: &[String], word: &'a str) -> Option<&'a str> {
    if parent.is_empty() {
        return if index.has_root(word) { Some(word) } else { index.root_alias_target(word) };
    }
    let depth = parent.len();
    let mut canonical = None;
    for cmd in index.cmds_with_prefix(parent) {
        if cmd.path.len() <= depth {
            continue;
        }
        if cmd.path[depth] == word {
            return Some(word);
        }
        if canonical.is_none() && cmd.path.len() == depth + 1 && cmd.aliases.iter().any(|a| a == word) {
            canonical = Some(cmd.path[depth].as_str());
        }
    }
    canonical
}

pub(super) fn root_commands(index: &SpecIndex) -> Vec<SubcmdCandidate> {
    child_subcommands(index, &[])
}

/// Direct children of `prefix` (root commands when empty), followed by their aliases.
pub(super) fn child_subcommands(index: &SpecIndex, prefix: &[String]) -> Vec<SubcmdCandidate> {
    let next_idx = prefix.len();
    let mut seen = HashSet::new();
    let mut out: Vec<SubcmdCandidate> = index
        .cmds_with_prefix(prefix)
        .filter(|cmd| cmd.path.len() > next_idx)
        .map(|cmd| &cmd.path[..=next_idx])
        .filter(|child_path| seen.insert(child_path[next_idx].as_str()))
        .map(|child_path| SubcmdCandidate {
            name: child_path[next_idx].clone(),
            desc: find_desc(index, child_path),
            alias_of: None,
        })
        .collect();
    let aliases: Vec<SubcmdCandidate> = index
        .cmds_with_prefix(prefix)
        .filter(|cmd| cmd.path.len() == next_idx + 1)
        .flat_map(|cmd| cmd.aliases.iter().map(move |alias| (cmd, alias)))
        .filter(|(_, alias)| seen.insert(alias.as_str()))
        .map(|(cmd, alias)| SubcmdCandidate {
            name: alias.clone(),
            desc: cmd.desc.clone(),
            alias_of: Some(cmd.path[next_idx].clone()),
        })
        .collect();
    out.extend(aliases);
    out
}
/// Description of the command declared exactly at `path`, if any.
fn find_desc(index: &SpecIndex, path: &[String]) -> Option<String> {
    index.find_cmd(path).and_then(|c| c.desc.clone())
}
