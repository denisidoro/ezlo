use super::ParsedWord;

pub fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Index of the first word at or after `start` that is neither a `VAR=value` assignment nor a
/// configured wrapper (or one of its flags), never crossing the cursor word.
pub(crate) fn skip_assignments_and_wrappers(
    words: &[ParsedWord],
    start: usize,
    cursor_word_idx: Option<usize>,
    wrappers: &[String],
) -> usize {
    let limit = cursor_word_idx.unwrap_or(words.len());
    let mut idx = start;

    loop {
        while idx < limit && is_env_assignment(&words[idx].unquoted) {
            idx += 1;
        }
        if idx >= limit {
            break;
        }
        let w = &words[idx].unquoted;
        if !wrappers.iter().any(|wrap| wrap == w) {
            break;
        }
        let wrapper_name = w.as_str();
        idx += 1;
        while idx < limit {
            let arg = &words[idx].unquoted;
            if wrapper_name == "env" && is_env_assignment(arg) {
                idx += 1;
                continue;
            }
            if arg == "--" {
                idx += 1;
                break;
            }
            if arg.starts_with('-') {
                let takes_val = wrapper_flag_takes_value(wrapper_name, arg);
                idx += 1;
                if takes_val && idx < limit {
                    idx += 1;
                }
                continue;
            }
            break;
        }
    }

    idx
}

fn wrapper_flag_takes_value(wrapper: &str, flag: &str) -> bool {
    if flag.contains('=') {
        return false;
    }
    match wrapper {
        "sudo" => matches!(flag, "-u" | "--user" | "-g" | "--group" | "-C" | "-D" | "-R" | "-T"),
        "env" => matches!(flag, "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"),
        "nice" => matches!(flag, "-n" | "--adjustment"),
        "xargs" => matches!(flag, "-n" | "-L" | "-I" | "-P" | "-s" | "-d" | "-a" | "-E"),
        _ => false,
    }
}
