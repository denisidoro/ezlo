mod cursor;
mod nav;

pub(crate) use nav::{collect_active_opts, find_opt, find_short_opt, push_subcommand, resolve_subcommand};
pub use nav::resolve_positional_arg;

use crate::engine::line::{Aliases, ParsedLine};
use crate::spec::ast::{ArgDecl, CmdDecl, OptDecl};
use crate::spec::SpecIndex;
use nav::{child_subcommands, root_commands};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SubcmdCandidate {
    pub name: String,
    pub desc: Option<String>,
    /// Canonical command word when `name` is an alias: only offered once the user types its prefix.
    pub alias_of: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CursorSlot<'a> {
    RootCommand,
    OptValue {
        opt: &'a OptDecl,
        /// Which value slot of `opt` the cursor fills (0 = first; see [`OptDecl::value_at`]).
        value_idx: usize,
        prefix: String,
        replace_span: (usize, usize),
    },
    CommandContext {
        after_double_dash: bool,
        positional_index: usize,
        current_arg: Option<&'a ArgDecl>,
        cluster_prefix: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct WalkState<'a> {
    pub cmd_path: Vec<String>,
    pub matched_cmd: Option<&'a CmdDecl>,
    pub known_root: bool,
    pub subcommands: Vec<SubcmdCandidate>,
    pub active_opts: Vec<&'a OptDecl>,
    pub present_flags: HashMap<String, Option<String>>,
    pub opt_occurrence_count: HashMap<String, usize>,
    /// First value of each option (its `opt.<name>` binding).
    pub opt_values: HashMap<String, String>,
    /// Every value of each option's last occurrence (`jq --arg <name> <value>` → both).
    pub opt_value_lists: HashMap<String, Vec<String>>,
    pub arg_values: HashMap<String, Vec<String>>,
    pub slot: CursorSlot<'a>,
    pub usage_context: String,
    /// Index into `ParsedLine::words` where a nested command line starts: the first word filling a
    /// `cmd`-typed positional slot, when it lies strictly before the cursor word. Words from there
    /// on belong to the nested command and are not interpreted against this command's grammar.
    pub nested_cmd_word_idx: Option<usize>,
    /// What each walked word (after the command word, before the cursor word) bound to, in order.
    pub word_roles: Vec<WordRole<'a>>,
    /// A `greedy` optional-value option right before the cursor word (`journalctl -b <cursor>`):
    /// the cursor word may be its value, so its static values are offered next to the slot's own.
    pub greedy_opt: Option<&'a OptDecl>,
}

/// What one word of the line bound to while walking the grammar (see [`WalkState::word_roles`]).
#[derive(Debug, Clone)]
pub struct WordRole<'a> {
    /// Index into `ParsedLine::words`.
    pub word_idx: usize,
    pub kind: WordRoleKind<'a>,
}

#[derive(Debug, Clone)]
pub enum WordRoleKind<'a> {
    /// Extends the command path; `canonical` differs from the word for subcommand aliases.
    Subcommand { canonical: String },
    /// One or more options (`--flag`, `--flag=v`, `-abc`, `-nVALUE`); empty when none is known.
    Options(Vec<&'a OptDecl>),
    /// A separate value word of the preceding option: its `index`-th value slot.
    OptValue { opt: &'a OptDecl, index: usize },
    /// The `--` end-of-options marker.
    DoubleDash,
    /// Positional number `index` (0-based), bound to `arg` when the command declares one.
    Positional { index: usize, arg: Option<&'a ArgDecl> },
    /// Starts a nested command line (fills the `cmd`-typed slot `arg`).
    NestedCommand { arg: Option<&'a ArgDecl> },
}

/// Upper bound on nested `cmd` slot delegations per line (`sudo timeout 5 watch git …`).
const MAX_NESTED_DEPTH: usize = 8;

/// Options seen so far on the line: which flags are present, how often, and their values.
#[derive(Default)]
struct OptTracker {
    present_flags: HashMap<String, Option<String>>,
    occurrences: HashMap<String, usize>,
    values: HashMap<String, String>,
    value_lists: HashMap<String, Vec<String>>,
}

impl OptTracker {
    fn record(&mut self, opt: &OptDecl, val: Option<&str>) {
        self.record_values(opt, val.map(|v| vec![v.to_string()]).unwrap_or_default());
    }

    /// Records one occurrence of `opt` with all its values (empty for a flag).
    fn record_values(&mut self, opt: &OptDecl, vals: Vec<String>) {
        *self.occurrences.entry(opt.name.clone()).or_insert(0) += 1;
        let first = vals.first().cloned();
        if let Some(v) = &first {
            self.values.insert(opt.name.clone(), v.clone());
            self.value_lists.insert(opt.name.clone(), vals);
        }
        for f in &opt.flags {
            self.present_flags.insert(f.clone(), first.clone());
        }
    }
}

/// An option word still waiting for detached values in the following words.
struct PendingOpt<'a> {
    opt: &'a OptDecl,
    /// The flag spelling used (for the usage context).
    flag: &'a str,
    /// Values collected so far (`--arg=n` already holds the first one).
    values: Vec<String>,
}

impl<'a> PendingOpt<'a> {
    fn new(opt: &'a OptDecl, flag: &'a str, values: Vec<String>) -> Self {
        Self { opt, flag, values }
    }
}

pub fn walk_command_line<'a>(index: &'a SpecIndex, parsed: &ParsedLine) -> WalkState<'a> {
    let cursor_limit = parsed.cursor_word_idx.unwrap_or(parsed.words.len());

    if parsed.cmd_word_idx >= cursor_limit {
        return WalkState {
            cmd_path: Vec::new(),
            matched_cmd: None,
            known_root: false,
            subcommands: root_commands(index),
            active_opts: Vec::new(),
            present_flags: HashMap::new(),
            opt_occurrence_count: HashMap::new(),
            opt_values: HashMap::new(),
            opt_value_lists: HashMap::new(),
            arg_values: HashMap::new(),
            slot: CursorSlot::RootCommand,
            usage_context: String::new(),
            nested_cmd_word_idx: None,
            word_roles: Vec::new(),
            greedy_opt: None,
        };
    }

    let root_word = &parsed.words[parsed.cmd_word_idx].unquoted;
    let canonical_root = resolve_subcommand(index, &[], root_word);
    let known_root = canonical_root.is_some();

    let mut cmd_path = vec![canonical_root.unwrap_or(root_word).to_string()];
    let mut active_opts = collect_active_opts(index, &cmd_path);
    let mut tracker = OptTracker::default();

    let mut after_double_dash = false;
    let mut positional_words: Vec<&str> = Vec::new();
    let mut pending_opt: Option<PendingOpt<'a>> = None;
    let mut nested_cmd_word_idx = None;
    let mut word_roles: Vec<WordRole<'a>> = Vec::new();
    let mut role = |word_idx: usize, kind: WordRoleKind<'a>| word_roles.push(WordRole { word_idx, kind });

    for (word_idx, word) in parsed
        .words
        .iter()
        .enumerate()
        .take(cursor_limit)
        .skip(parsed.cmd_word_idx + 1)
        .map(|(i, w)| (i, &w.unquoted))
    {
        if let Some(mut pending) = pending_opt.take() {
            let opt = pending.opt;
            if opt.optional_value() {
                // `greedy`: the word is the value only when it looks like one; else it's a new word.
                if opt.takes_detached_value(word) {
                    tracker.record(opt, Some(word.as_str()));
                    role(word_idx, WordRoleKind::OptValue { opt, index: 0 });
                    continue;
                }
                tracker.record(opt, None);
            } else {
                role(word_idx, WordRoleKind::OptValue { opt, index: pending.values.len() });
                pending.values.push(word.clone());
                if pending.values.len() < opt.detached_value_count() {
                    pending_opt = Some(pending);
                } else {
                    tracker.record_values(opt, pending.values);
                }
                continue;
            }
        }

        if !after_double_dash && word == "--" {
            after_double_dash = true;
            role(word_idx, WordRoleKind::DoubleDash);
            continue;
        }

        if !after_double_dash && word.starts_with('-') && word.len() > 1 {
            let mut matched = Vec::new();
            pending_opt = consume_option_word(word, &active_opts, &mut tracker, &mut matched);
            role(word_idx, WordRoleKind::Options(matched));
            continue;
        }

        // Check if this non-option word extends `cmd_path` as a subcommand (only before positionals)
        if !after_double_dash && positional_words.is_empty() && push_subcommand(index, &mut cmd_path, word) {
            active_opts = collect_active_opts(index, &cmd_path);
            let canonical = cmd_path.last().cloned().unwrap_or_default();
            role(word_idx, WordRoleKind::Subcommand { canonical });
            continue;
        }

        let pos_idx = positional_words.len();
        let arg = index
            .find_cmd(&cmd_path)
            .and_then(|c| resolve_positional_arg(c, pos_idx));
        if is_cmd_slot(index, &cmd_path, pos_idx) {
            nested_cmd_word_idx = Some(word_idx);
            role(word_idx, WordRoleKind::NestedCommand { arg });
            break;
        }
        role(word_idx, WordRoleKind::Positional { index: pos_idx, arg });
        positional_words.push(word.as_str());
    }

    let matched_cmd = index.find_cmd(&cmd_path);
    let mut arg_values: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(cmd) = matched_cmd {
        for (pidx, pval) in positional_words.iter().enumerate() {
            if let Some(arg_decl) = resolve_positional_arg(cmd, pidx) {
                arg_values
                    .entry(arg_decl.name.clone())
                    .or_default()
                    .push(pval.to_string());
            }
        }
    }

    let subcommands = if after_double_dash || !positional_words.is_empty() {
        Vec::new()
    } else {
        child_subcommands(index, &cmd_path)
    };

    // Determine cursor slot
    let mut usage_context = if known_root {
        cmd_path.join(" ")
    } else {
        parsed.words[parsed.cmd_word_idx..cursor_limit]
            .iter()
            .map(|w| w.unquoted.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let pos_idx = positional_words.len();
    let mut greedy_opt = None;
    if let Some(pending) = pending_opt.take_if(|p| p.opt.optional_value()) {
        tracker.record(pending.opt, None);
        greedy_opt = Some(pending.opt);
    }
    let slot = if let Some(PendingOpt { opt, flag, values }) = pending_opt {
        if !flag.is_empty() {
            usage_context.push(' ');
            usage_context.push_str(flag);
        }
        // Later values of a multi-value option learn under their own placeholder.
        if let Some(vs) = opt.value_at(values.len()).filter(|_| !values.is_empty()) {
            usage_context.push(' ');
            usage_context.push_str(&vs.placeholder());
        }
        CursorSlot::OptValue {
            opt,
            value_idx: values.len(),
            prefix: parsed.typed_prefix.clone(),
            replace_span: parsed.replace_span,
        }
    } else if !after_double_dash {
        cursor::inspect_cursor_token_for_opt(
            &parsed.typed_prefix,
            parsed.replace_span,
            &active_opts,
            matched_cmd,
            pos_idx,
            &mut tracker,
            &mut usage_context,
        )
    } else {
        CursorSlot::CommandContext {
            after_double_dash: true,
            positional_index: pos_idx,
            current_arg: matched_cmd.and_then(|c| resolve_positional_arg(c, pos_idx)),
            cluster_prefix: None,
        }
    };

    WalkState {
        cmd_path,
        matched_cmd,
        known_root,
        subcommands,
        active_opts,
        present_flags: tracker.present_flags,
        opt_occurrence_count: tracker.occurrences,
        opt_values: tracker.values,
        opt_value_lists: tracker.value_lists,
        arg_values,
        slot,
        usage_context,
        nested_cmd_word_idx,
        word_roles,
        greedy_opt,
    }
}

/// Whether positional `pos_idx` of the command at `cmd_path` is a `cmd`-typed slot: declared as
/// `arg <name> cmd`, or an untyped, suggestion-less `arg <cmd...>` (name-as-type fallback).
fn is_cmd_slot(index: &SpecIndex, cmd_path: &[String], pos_idx: usize) -> bool {
    let Some(arg) = index
        .find_cmd(cmd_path)
        .and_then(|c| resolve_positional_arg(c, pos_idx))
    else {
        return false;
    };
    match &arg.type_expr {
        Some(te) => te.type_ref.name == "cmd",
        None => arg.suggestions.is_empty() && arg.name == "cmd",
    }
}

/// Walks `parsed`, delegating to the nested command line whenever a `cmd`-typed positional slot is
/// filled before the cursor (`timeout 5 git checkout <TAB>` completes `git checkout`). `outer` sees
/// every enclosing level (its parsed line and walk) before the line is re-rooted. Returns the
/// innermost parsed line and walk.
pub fn walk_nested_command_line<'a>(
    index: &'a SpecIndex,
    mut parsed: ParsedLine,
    aliases: Aliases<'_>,
    wrappers: &[String],
    mut outer: impl FnMut(&ParsedLine, &WalkState<'a>),
) -> (ParsedLine, WalkState<'a>) {
    let mut walk = walk_command_line(index, &parsed);
    for _ in 0..MAX_NESTED_DEPTH {
        let Some(nested_idx) = walk.nested_cmd_word_idx else {
            break;
        };
        outer(&parsed, &walk);
        parsed.reroot(nested_idx, aliases, wrappers);
        walk = walk_command_line(index, &parsed);
    }
    (parsed, walk)
}

/// Records a completed option word (`--flag=v`, `--flag`, `-abc`, `-nVALUE`). Returns the option
/// still waiting for values in the next words, if any: a mandatory-value option spelled without
/// (all) its values, or a `greedy` optional-value option spelled without one. Every option the
/// word spells is appended to `matched`.
fn consume_option_word<'a>(
    word: &str,
    active_opts: &[&'a OptDecl],
    tracker: &mut OptTracker,
    matched: &mut Vec<&'a OptDecl>,
) -> Option<PendingOpt<'a>> {
    if let Some((flag_part, val_part)) = word.split_once('=') {
        if let Some(opt) = find_opt(active_opts, flag_part) {
            matched.push(opt);
            let flag = opt.flags.iter().find(|f| f.as_str() == flag_part).map_or("", String::as_str);
            return with_first_value(opt, flag, val_part, tracker);
        }
        return None;
    }

    // Exact spelling first: `--flag`, `-f`, or a single-dash long flag such as `-name`.
    if let Some(opt) = find_opt(active_opts, word) {
        matched.push(opt);
        let flag = opt
            .flags
            .iter()
            .find(|f| f.as_str() == word)
            .or_else(|| opt.flags.first())
            .map_or("", String::as_str);
        return detached(opt, flag, tracker);
    }
    if word.starts_with("--") {
        return None;
    }

    // Short flag or cluster `-abc` or `-nVALUE`
    let cluster = &word[1..];
    for (ci, ch) in cluster.char_indices() {
        let Some(opt) = find_short_opt(active_opts, ch) else {
            continue;
        };
        matched.push(opt);
        if opt.value.is_none() {
            tracker.record(opt, None);
            continue;
        }
        let flag = opt
            .flags
            .iter()
            .find(|f| f.len() == 2 && f.starts_with('-') && f.ends_with(ch))
            .or_else(|| opt.flags.first())
            .map_or("", String::as_str);
        let attached = &cluster[ci + ch.len_utf8()..];
        if attached.is_empty() {
            return detached(opt, flag, tracker);
        }
        return with_first_value(opt, flag, attached, tracker);
    }
    None
}

/// `opt` spelled without an attached value: wait for its values, unless they are optional.
fn detached<'a>(opt: &'a OptDecl, flag: &'a str, tracker: &mut OptTracker) -> Option<PendingOpt<'a>> {
    let waits = opt.detached_value_count() > 0 || (opt.optional_value() && opt.greedy);
    if waits {
        return Some(PendingOpt::new(opt, flag, Vec::new()));
    }
    tracker.record(opt, None);
    None
}

/// `opt` spelled with an attached first value (`--flag=v`, `-nV`): done, unless more values follow.
fn with_first_value<'a>(
    opt: &'a OptDecl,
    flag: &'a str,
    value: &str,
    tracker: &mut OptTracker,
) -> Option<PendingOpt<'a>> {
    if opt.extra_values.is_empty() {
        tracker.record(opt, Some(value));
        return None;
    }
    Some(PendingOpt::new(opt, flag, vec![value.to_owned()]))
}
