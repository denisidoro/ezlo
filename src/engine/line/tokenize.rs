use super::ParsedWord;

/// Pushes the result of a `\<next>` escape inside double quotes: only `"`, `\`, `$` and
/// `` ` `` are escapable; anything else keeps the backslash.
fn push_dquote_escape(out: &mut String, next: char) {
    if !matches!(next, '"' | '\\' | '$' | '`') {
        out.push('\\');
    }
    out.push(next);
}

pub fn tokenize_segment(seg: &str, offset: usize) -> Vec<ParsedWord> {
    let chars: Vec<char> = seg.chars().collect();
    let mut words = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let start = i;
        let mut raw = String::new();
        let mut unquoted = String::new();
        let mut quote: Option<char> = None;
        let mut active_quote: Option<char> = None;

        while i < chars.len() {
            let c = chars[i];
            if let Some(q) = active_quote {
                raw.push(c);
                if q == '\'' {
                    if c == '\'' {
                        active_quote = None;
                    } else {
                        unquoted.push(c);
                    }
                } else if c == '\\' && i + 1 < chars.len() {
                    let next = chars[i + 1];
                    raw.push(next);
                    push_dquote_escape(&mut unquoted, next);
                    i += 2;
                    continue;
                } else if c == '"' {
                    active_quote = None;
                } else {
                    unquoted.push(c);
                }
                i += 1;
                continue;
            }

            if c.is_whitespace() {
                break;
            }
            // `$((…))` arithmetic may contain spaces but is one word.
            if c == '$' && chars.get(i + 1) == Some(&'(') && chars.get(i + 2) == Some(&'(') {
                let mut depth = 0usize;
                let mut j = i + 3;
                while j < chars.len() {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' if depth > 0 => depth -= 1,
                        ')' if chars.get(j + 1) == Some(&')') => {
                            j += 2;
                            break;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let end = j.min(chars.len());
                raw.extend(&chars[i..end]);
                unquoted.extend(&chars[i..end]);
                i = end;
                continue;
            }
            if c == '\'' || c == '"' {
                if quote.is_none() {
                    quote = Some(c);
                }
                active_quote = Some(c);
                raw.push(c);
                i += 1;
                continue;
            }
            if c == '\\' {
                raw.push('\\');
                if i + 1 < chars.len() {
                    raw.push(chars[i + 1]);
                    unquoted.push(chars[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            raw.push(c);
            unquoted.push(c);
            i += 1;
        }

        words.push(ParsedWord {
            raw,
            unquoted,
            char_start: offset + start,
            char_end: offset + i,
            quote: active_quote.or(quote),
            unterminated: active_quote.is_some(),
        });
    }

    words
}

pub(crate) fn unquote_partial(raw: &str) -> (String, Option<char>) {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut active_quote: Option<char> = None;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = active_quote {
            if q == '\'' {
                if c == '\'' {
                    active_quote = None;
                } else {
                    out.push(c);
                }
            } else if c == '\\' && i + 1 < chars.len() {
                push_dquote_escape(&mut out, chars[i + 1]);
                i += 2;
                continue;
            } else if c == '"' {
                active_quote = None;
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            active_quote = Some(c);
            i += 1;
            continue;
        }
        if c == '\\' {
            if i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }

    (out, active_quote)
}
