//! Live positional signature hint: the matched command's positional arguments rendered as
//! `scp  <source…>  [target]`, with the slot under the cursor marked so the shell can highlight it.

use crate::engine::line::ParsedLine;
use crate::engine::walker::{CursorSlot, WalkState};
use crate::spec::ast::ArgDecl;

/// Separator between the command path and each positional placeholder.
const GAP: &str = "  ";
/// Commands with fewer positionals get no hint.
const MIN_POSITIONALS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHint {
    /// `git checkout  <branch>  [paths…]`
    pub text: String,
    /// Char range of the active placeholder in `text`; `None` once the cursor is past the last
    /// (non-variadic) positional.
    pub active: Option<(usize, usize)>,
}

/// The positional signature of the command the cursor is in, when the cursor sits on a positional
/// slot (not on an option, an option value or a short-flag cluster) of a spec'd command that
/// declares at least `MIN_POSITIONALS` positionals (with a single one the hint says nothing the
/// dropdown doesn't). For nested command lines this is the innermost command.
pub fn signature_hint(parsed: &ParsedLine, walk: &WalkState<'_>) -> Option<SignatureHint> {
    let CursorSlot::CommandContext {
        after_double_dash,
        positional_index,
        cluster_prefix: None,
        ..
    } = &walk.slot
    else {
        return None;
    };
    if !walk.known_root || (!after_double_dash && parsed.typed_prefix.starts_with('-')) {
        return None;
    }
    let cmd = walk.matched_cmd.filter(|c| c.args.len() >= MIN_POSITIONALS)?;
    let active_idx = match cmd.args.len() {
        n if *positional_index < n => Some(*positional_index),
        n if cmd.args[n - 1].variadic => Some(n - 1),
        _ => None,
    };

    let mut text = walk.cmd_path.join(" ");
    let mut active = None;
    for (i, arg) in cmd.args.iter().enumerate() {
        text.push_str(GAP);
        let start = text.chars().count();
        text.push_str(&placeholder(arg));
        if active_idx == Some(i) {
            active = Some((start, text.chars().count()));
        }
    }
    Some(SignatureHint { text, active })
}

/// `<name>` when required, `[name]` when optional; variadic slots end in `…`.
fn placeholder(arg: &ArgDecl) -> String {
    let ellipsis = if arg.variadic { "…" } else { "" };
    if arg.required {
        format!("<{}{ellipsis}>", arg.name)
    } else {
        format!("[{}{ellipsis}]", arg.name)
    }
}
