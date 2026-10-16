//! Shell-line coloring shared by `ezlo pick` templates and history search rows: the command word
//! bold blue, flags cyan, operators/redirections magenta, quoted strings green.
//!
//! A small lexer, not a parser: quotes span whitespace and operators (`'END{print $5}'` is one
//! green string), backslash escapes are honored, operators split words even without spaces
//! (`a|b`), and an unclosed quote runs to the end of the text.

use super::*;

fn command_style() -> Style {
    Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
}

fn operator_style() -> Style {
    Style::default().fg(Color::Magenta)
}

fn flag_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn quote_style() -> Style {
    Style::default().fg(Color::Green)
}

/// Characters that end an unquoted word (besides whitespace). `↵` stands for a newline in
/// flattened multi-line history entries.
const OPERATOR_CHARS: &[char] = &['|', '&', ';', '<', '>', '(', ')', '↵'];

/// Operators, longest first. Separators put the next word in command position; redirections make
/// it a plain target.
const SEPARATORS: &[&str] = &["&&", "||", "|&", ";;", "|", ";", "&", "(", ")", "↵"];
const REDIRECTIONS: &[&str] = &["<<<", "&>>", ">>", "<<", "&>", ">&", "<&", ">|", "<>", ">", "<"];

/// Colors `text` as a shell line; `at_start` tells whether it begins in command position.
pub(super) fn literal_spans(text: &str, at_start: bool) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let mut cmd_pos = at_start;
    let mut after_redirect = false;
    let mut i = 0;
    while let Some(rest) = text.get(i..).filter(|r| !r.is_empty()) {
        let ws = rest.len() - rest.trim_start().len();
        if ws > 0 {
            spans.push(Span::raw(&rest[..ws]));
            i += ws;
            continue;
        }
        if let Some((len, kind)) = operator(rest) {
            spans.push(Span::styled(&rest[..len], operator_style()));
            match kind {
                OpKind::Separator => cmd_pos = true,
                OpKind::Redirection => after_redirect = true,
            }
            i += len;
            continue;
        }
        let len = word_len(rest).max(rest.chars().next().map_or(1, char::len_utf8));
        let word = &rest[..len];
        let style = if after_redirect {
            Style::default()
        } else if cmd_pos && !is_assignment(word) {
            cmd_pos = false;
            command_style()
        } else if word.starts_with('-') {
            flag_style()
        } else {
            Style::default()
        };
        after_redirect = false;
        push_word(&mut spans, word, style);
        i += len;
    }
    spans
}

enum OpKind {
    Separator,
    Redirection,
}

/// Length and kind of the operator at the start of `s`: separators, redirections with an
/// optional fd prefix (`2>`) and duplication target (`2>&1`, `>&-`).
fn operator(s: &str) -> Option<(usize, OpKind)> {
    let fd = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let after_fd = &s[fd..];
    if let Some(op) = REDIRECTIONS.iter().find(|op| after_fd.starts_with(**op)) {
        let mut len = fd + op.len();
        if op.ends_with('&') {
            let tail = &s[len..];
            len += tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-').len();
        }
        return Some((len, OpKind::Redirection));
    }
    if fd > 0 {
        return None;
    }
    SEPARATORS
        .iter()
        .find(|op| s.starts_with(**op))
        .map(|op| (op.len(), OpKind::Separator))
}

/// Byte length of the word at the start of `s`: up to unquoted whitespace or an operator char;
/// quotes (`'…'`, `"…"` with `\"` escapes) and `\x` escapes are part of the word.
fn word_len(s: &str) -> usize {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '\'' | '"' => {
                let mut escaped = false;
                for (_, q) in chars.by_ref() {
                    if q == c && !escaped {
                        break;
                    }
                    escaped = c == '"' && q == '\\' && !escaped;
                }
            }
            c if c.is_whitespace() || OPERATOR_CHARS.contains(&c) => return i,
            _ => {}
        }
    }
    s.len()
}

/// `NAME=value` (a valid shell identifier before the first `=`).
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Splits a word into plain runs (`style`) and quoted runs (green, quotes included).
fn push_word<'a>(spans: &mut Vec<Span<'a>>, word: &'a str, style: Style) {
    let mut plain_start = 0;
    let mut chars = word.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '\'' | '"' => {
                if plain_start < i {
                    spans.push(Span::styled(&word[plain_start..i], style));
                }
                let mut end = word.len();
                let mut escaped = false;
                for (j, q) in chars.by_ref() {
                    if q == c && !escaped {
                        end = j + q.len_utf8();
                        break;
                    }
                    escaped = c == '"' && q == '\\' && !escaped;
                }
                spans.push(Span::styled(&word[i..end], quote_style()));
                plain_start = end;
            }
            _ => {}
        }
    }
    if plain_start < word.len() {
        spans.push(Span::styled(&word[plain_start..], style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colored(text: &str) -> Vec<(&str, Option<Color>)> {
        literal_spans(text, true)
            .into_iter()
            .filter(|s| !s.content.trim().is_empty())
            .map(|s| {
                let content: &str = match s.content {
                    std::borrow::Cow::Borrowed(b) => b,
                    std::borrow::Cow::Owned(_) => unreachable!("literal spans borrow the text"),
                };
                (content, s.style.fg)
            })
            .collect()
    }

    const B: Option<Color> = Some(Color::Blue);
    const M: Option<Color> = Some(Color::Magenta);
    const C: Option<Color> = Some(Color::Cyan);
    const G: Option<Color> = Some(Color::Green);

    #[test]
    fn quoted_strings_span_whitespace_and_operators() {
        assert_eq!(
            colored("df -P / | awk 'END{print $5}'"),
            [("df", B), ("-P", C), ("/", None), ("|", M), ("awk", B), ("'END{print $5}'", G)]
        );
        assert_eq!(
            colored("rm -rf 'Miss Sloane 2016/'"),
            [("rm", B), ("-rf", C), ("'Miss Sloane 2016/'", G)]
        );
        assert_eq!(colored(r#"echo "a \" b" c"#), [("echo", B), (r#""a \" b""#, G), ("c", None)]);
        assert_eq!(colored("echo 'unclosed | x"), [("echo", B), ("'unclosed | x", G)]);
    }

    #[test]
    fn operators_split_words_and_reset_command_position() {
        assert_eq!(colored("a|b&&c"), [("a", B), ("|", M), ("b", B), ("&&", M), ("c", B)]);
        assert_eq!(
            colored("make 2>&1 >out.log; ls"),
            [("make", B), ("2>&1", M), (">", M), ("out.log", None), (";", M), ("ls", B)]
        );
        assert_eq!(colored("FOO=1 cargo t"), [("FOO=1", None), ("cargo", B), ("t", None)]);
        assert_eq!(
            colored("git log --format='%h %s' x"),
            [("git", B), ("log", None), ("--format=", C), ("'%h %s'", G), ("x", None)]
        );
        assert_eq!(colored("echo a\\ b"), [("echo", B), ("a\\ b", None)]);
        assert_eq!(colored("cd x↵ls"), [("cd", B), ("x", None), ("↵", M), ("ls", B)]);
    }
}
