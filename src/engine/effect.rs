//! Side-effect class of a whole command line, from the `effect` declarations of its specs: every
//! pipeline/list segment, nested command line (`timeout 5 git push`) and output redirection
//! counts, and the line gets the highest class of its parts (`read < write < remote < destroy`).
//! Parts without a declared effect (no spec, or a spec without `effect`) and parts ezlo can't
//! analyze (`"$(…)"`) are reported as unknown instead of being guessed.

use crate::engine::line::{char_slice, parse_command_line, redirect_at, segments, skip_assignments_and_wrappers, Aliases, ParsedLine};
use crate::engine::walker::{walk_nested_command_line, WalkState, WordRoleKind};
use crate::engine::CompletionRequest;
use crate::spec::ast::Effect;
use crate::spec::SpecIndex;
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LineEffect {
    /// Highest declared effect of the line's parts (`None` when no part declares one).
    pub level: Option<Effect>,
    /// What raised the line to `level` (`git push`, `git branch -D`, `> out.txt`).
    pub because: Vec<String>,
    /// Parts whose effect is unknown: commands without a declared `effect`, `"$(…)"`.
    pub unknown: Vec<String>,
}

impl LineEffect {
    /// Whether running the line changes nothing: every part is known to be `read`.
    pub fn read_only(&self) -> bool {
        self.unknown.is_empty() && self.level.is_none_or(|l| l == Effect::Read)
    }

    /// `destroy · git branch -D; unknown: make` (`None` for a line without commands).
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(level) = self.level {
            parts.push(format!("{} · {}", level.as_str(), self.because.join(", ")));
        }
        if !self.unknown.is_empty() {
            parts.push(format!("unknown: {}", self.unknown.join(", ")));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    fn raise(&mut self, effect: Effect, because: String) {
        match self.level {
            Some(level) if level > effect => {}
            Some(level) if level == effect => {
                if !self.because.contains(&because) {
                    self.because.push(because);
                }
            }
            _ => {
                self.level = Some(effect);
                self.because = vec![because];
            }
        }
    }

    fn add_unknown(&mut self, what: String) {
        if !self.unknown.contains(&what) {
            self.unknown.push(what);
        }
    }

    fn merge(&mut self, other: LineEffect) {
        if let Some(level) = other.level {
            for because in other.because {
                self.raise(level, because);
            }
        }
        for what in other.unknown {
            self.add_unknown(what);
        }
    }
}

/// Effect of the whole of `req.line` (every segment, wherever the cursor is).
pub fn line_effect(req: &CompletionRequest<'_>) -> LineEffect {
    let aliases = Aliases::new(req.index, req.aliases);
    let mut out = LineEffect::default();
    for (start, end) in segments(req.line) {
        if char_slice(req.line, start, end).trim().is_empty() {
            continue;
        }
        // Cursor past a trailing space: every word of the segment is walked.
        let mut full = char_slice(req.line, 0, end).to_owned();
        full.push(' ');
        let parsed = parse_command_line(&full, end + 1, aliases, req.wrappers);
        out.merge(segment_effect(req.index, parsed, aliases, req.wrappers));
    }
    out
}

fn segment_effect(index: &SpecIndex, mut parsed: ParsedLine, aliases: Aliases<'_>, wrappers: &[String]) -> LineEffect {
    let mut out = LineEffect::default();
    if let Some(w) = parsed.words.iter().find(|w| has_substitution(&w.raw)) {
        out.add_unknown(format!("{} (command substitution)", w.raw));
    }
    strip_redirects(&mut parsed, wrappers, &mut out);
    if parsed.cmd_word_idx >= parsed.words.len() {
        return out;
    }
    let (inner_parsed, inner_walk) =
        walk_nested_command_line(index, parsed, aliases, wrappers, |p, w| out.merge(level_effect(index, p, w)));
    out.merge(level_effect(index, &inner_parsed, &inner_walk));
    out
}

/// Records file-writing redirections (`> f`, `2>> log`; not `/dev/…` nor `2>&1`) and drops every
/// redirection from the words, so their operators and targets aren't walked as arguments.
fn strip_redirects(parsed: &mut ParsedLine, wrappers: &[String], out: &mut LineEffect) {
    let raws: Vec<&str> = parsed.words.iter().map(|w| w.raw.as_str()).collect();
    let mut keep = vec![true; raws.len()];
    let mut i = 0;
    while i < raws.len() {
        let Some(r) = redirect_at(&raws, i) else {
            i += 1;
            continue;
        };
        if r.writes_file() && !r.target.starts_with("/dev/") {
            out.raise(Effect::Write, format!("{} {}", r.op, r.target));
        }
        let end = (i + r.len).min(raws.len());
        keep[i..end].fill(false);
        i = end;
    }
    if keep.iter().all(|k| *k) {
        return;
    }
    let mut flags = keep.into_iter();
    parsed.words.retain(|_| flags.next().unwrap_or(true));
    parsed.cmd_word_idx = skip_assignments_and_wrappers(&parsed.words, 0, None, wrappers);
}

/// A command substitution ezlo can't look into: `"$(…)"` or a backtick inside a quoted word
/// (unquoted ones split the line into their own segments).
fn has_substitution(raw: &str) -> bool {
    if raw.starts_with('\'') {
        return false;
    }
    raw.contains('`') || raw.match_indices("$(").any(|(i, _)| !raw[i + 2..].starts_with('('))
}

/// Effect of one command level: the nearest `effect` declared along its command path, raised by
/// the options and positionals present, or `read` when a `dryrun` option is present.
fn level_effect(index: &SpecIndex, parsed: &ParsedLine, walk: &WalkState<'_>) -> LineEffect {
    let mut out = LineEffect::default();
    let Some(root) = walk.cmd_path.first() else {
        return out;
    };
    if !walk.known_root {
        out.add_unknown(root.clone());
        return out;
    }
    let path = walk.cmd_path.join(" ");
    let declared = (1..=walk.cmd_path.len())
        .rev()
        .find_map(|n| index.find_cmd(&walk.cmd_path[..n]).and_then(|c| c.effect));
    match declared {
        Some(effect) => out.raise(effect, path.clone()),
        None => out.add_unknown(path.clone()),
    }

    let mut dryrun = None;
    for role in &walk.word_roles {
        let word = &parsed.words[role.word_idx].unquoted;
        match &role.kind {
            WordRoleKind::Options(opts) => {
                for opt in opts {
                    if opt.dryrun {
                        dryrun = Some(format!("{path} {word} (dry run)"));
                    }
                    if let Some(effect) = opt.effect {
                        out.raise(effect, format!("{path} {word}"));
                    }
                }
            }
            WordRoleKind::Positional { arg: Some(arg), .. } => {
                if let Some(effect) = arg.effect {
                    out.raise(effect, format!("{path} {word}"));
                }
            }
            _ => {}
        }
    }
    match dryrun {
        Some(because) => LineEffect {
            level: Some(Effect::Read),
            because: vec![because],
            unknown: Vec::new(),
        },
        None => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutions_are_detected_outside_single_quotes() {
        assert!(has_substitution("\"$(rm x)\""));
        assert!(has_substitution("\"`date`\""));
        assert!(!has_substitution("'$(rm x)'"));
        assert!(!has_substitution("\"$((1 + 2))\""));
        assert!(!has_substitution("plain"));
    }

    #[test]
    fn raise_keeps_the_highest_level_and_its_reasons() {
        let mut e = LineEffect::default();
        e.raise(Effect::Read, "ls".into());
        e.raise(Effect::Remote, "git push".into());
        e.raise(Effect::Write, "touch".into());
        e.raise(Effect::Remote, "scp".into());
        assert_eq!(e.level, Some(Effect::Remote));
        assert_eq!(e.because, ["git push", "scp"]);
        assert!(!e.read_only());
    }
}
