use super::nav::{find_opt, find_short_opt, resolve_positional_arg};
use super::{CursorSlot, OptTracker};
use crate::spec::ast::{CmdDecl, OptDecl};
use crate::spec::parser::cursor::is_single_dash_long_flag;

/// Classifies the word under the cursor when it may be an option (or an option with an
/// attached value), falling back to a positional slot.
pub(super) fn inspect_cursor_token_for_opt<'a>(
    typed_prefix: &str,
    replace_span: (usize, usize),
    active_opts: &[&'a OptDecl],
    matched_cmd: Option<&'a CmdDecl>,
    pos_idx: usize,
    tracker: &mut OptTracker,
    usage_context: &mut String,
) -> CursorSlot<'a> {
    let positional = |cluster_prefix: Option<String>| CursorSlot::CommandContext {
        after_double_dash: false,
        positional_index: pos_idx,
        current_arg: matched_cmd.and_then(|c| resolve_positional_arg(c, pos_idx)),
        cluster_prefix,
    };

    // `--flag=<value>` / `-f=<value>`
    if let Some((flag_part, val_part)) = typed_prefix
        .split_once('=')
        .filter(|(f, _)| f.starts_with('-'))
    {
        if let Some(opt) = find_opt(active_opts, flag_part) {
            let val_start = replace_span.0 + flag_part.chars().count() + 1;
            usage_context.push(' ');
            usage_context.push_str(flag_part);
            return CursorSlot::OptValue {
                value_idx: 0,
                opt,
                prefix: val_part.to_string(),
                replace_span: (val_start, replace_span.1.max(val_start)),
            };
        }
    }

    if !(typed_prefix.starts_with('-') && !typed_prefix.starts_with("--") && typed_prefix.len() > 2)
    {
        return positional(None);
    }

    // `-na` may be the start of a single-dash long flag (`-name`): complete flags, not a cluster.
    let extends_long_flag = active_opts
        .iter()
        .flat_map(|o| &o.flags)
        .any(|f| is_single_dash_long_flag(f) && f.starts_with(typed_prefix));
    if extends_long_flag {
        return positional(None);
    }

    // Short cluster `-abc` or `-n<value>`
    let cluster = &typed_prefix[1..];
    for (ci, (byte_idx, ch)) in cluster.char_indices().enumerate() {
        let Some(opt) = find_short_opt(active_opts, ch) else {
            continue;
        };
        let attached = &cluster[byte_idx + ch.len_utf8()..];
        if opt.value.is_some() && !attached.is_empty() {
            let val_start = replace_span.0 + 1 + ci + 1;
            usage_context.push_str(" -");
            usage_context.push(ch);
            return CursorSlot::OptValue {
                value_idx: 0,
                opt,
                prefix: attached.to_string(),
                replace_span: (val_start, replace_span.1.max(val_start)),
            };
        }
        tracker.record(opt, None);
    }
    // Everything but the char being typed (char-wise: the cluster may hold non-ASCII chars).
    let mut head = typed_prefix.chars();
    head.next_back();
    positional(Some(head.as_str().to_owned()))
}
