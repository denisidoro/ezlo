use super::{is_valid_placeholder_name, PlaceholderTok, Token, TokenKind};
use crate::spec::span::{Diagnostic, RuleCode, Span};

/// Tokenizes one non-raw spec line. Columns are 1-based chars, starting at `base_col`.
pub(super) fn lex_token_line(
    content: &str,
    line_no: usize,
    base_col: usize,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Token> {
    let mut lx = LineLexer {
        chars: content.chars().collect(),
        i: 0,
        line_no,
        base_col,
        tokens: Vec::new(),
    };

    while let Some(&ch) = lx.chars.get(lx.i) {
        if ch == ' ' || ch == '\t' {
            lx.i += 1;
            continue;
        }
        if ch == '#' && (lx.i == 0 || lx.chars[lx.i - 1].is_whitespace()) {
            break;
        }

        let start_col = lx.col();
        match ch {
            // `<keyword> $ <shell...>`: the rest of the line is a raw shell command (`preview $ …`).
            '$' if !lx.tokens.is_empty() && lx.chars.get(lx.i + 1) == Some(&' ') => {
                let val: String = lx.chars[lx.i + 2..].iter().collect();
                lx.i = lx.chars.len();
                lx.push(TokenKind::Shell(val.trim().to_owned()), start_col);
            }
            '"' => lx.lex_string(start_col, diags),
            '`' => lx.lex_shell(start_col, diags),
            '<' | '[' => lx.lex_placeholder(ch, start_col, diags),
            '>' | ']' => {
                lx.i += 1;
                diags.push(Diagnostic::error(
                    RuleCode::E01Lexical,
                    lx.span(start_col, start_col + 1),
                    format!("unexpected '{ch}'"),
                ));
            }
            '!' if lx.chars.get(lx.i + 1) == Some(&'=') => lx.punct(TokenKind::NotEq, 2),
            _ => match single_char_token(ch) {
                Some(kind) => lx.punct(kind, 1),
                None => lx.lex_word(start_col),
            },
        }
    }

    lx.tokens
}

fn single_char_token(ch: char) -> Option<TokenKind> {
    Some(match ch {
        '!' => TokenKind::Bang,
        '=' => TokenKind::Eq,
        '|' => TokenKind::Pipe,
        '(' => TokenKind::LParen,
        ')' => TokenKind::RParen,
        ',' => TokenKind::Comma,
        ':' => TokenKind::Colon,
        '~' => TokenKind::Tilde,
        _ => return None,
    })
}

fn is_word_delimiter(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '`' | '"' | '<' | '>' | '[' | ']' | '(' | ')' | ',' | '|' | '=' | ':' | '~'
        )
}

struct LineLexer {
    chars: Vec<char>,
    i: usize,
    line_no: usize,
    base_col: usize,
    tokens: Vec<Token>,
}

impl LineLexer {
    fn col(&self) -> usize {
        self.base_col + self.i
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::line(self.line_no, start, end)
    }

    fn push(&mut self, kind: TokenKind, start_col: usize) {
        let span = self.span(start_col, self.col());
        self.tokens.push(Token { kind, span });
    }

    fn punct(&mut self, kind: TokenKind, len: usize) {
        let start_col = self.col();
        self.i += len;
        self.push(kind, start_col);
    }

    fn lex_string(&mut self, start_col: usize, diags: &mut Vec<Diagnostic>) {
        self.i += 1;
        let mut val = String::new();
        let mut closed = false;
        while let Some(&c) = self.chars.get(self.i) {
            if c == '"' {
                self.i += 1;
                closed = true;
                break;
            }
            if c != '\\' {
                val.push(c);
                self.i += 1;
                continue;
            }
            let Some(&esc) = self.chars.get(self.i + 1) else {
                break;
            };
            match esc {
                '"' => val.push('"'),
                '\\' => val.push('\\'),
                'n' => val.push('\n'),
                't' => val.push('\t'),
                other => {
                    let esc_col = self.col();
                    diags.push(Diagnostic::error(
                        RuleCode::E01Lexical,
                        self.span(esc_col, esc_col + 2),
                        format!("unknown string escape '\\{other}'"),
                    ));
                }
            }
            self.i += 2;
        }
        if !closed {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                self.span(start_col, self.col()),
                "unterminated string literal",
            ));
        }
        self.push(TokenKind::String(val), start_col);
    }

    fn lex_shell(&mut self, start_col: usize, diags: &mut Vec<Diagnostic>) {
        self.i += 1;
        let rest = &self.chars[self.i..];
        let (len, closed) = match rest.iter().position(|&c| c == '`') {
            Some(end) => (end, true),
            None => (rest.len(), false),
        };
        let val: String = rest[..len].iter().collect();
        self.i += len + usize::from(closed);
        if !closed {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                self.span(start_col, self.col()),
                "unterminated backtick shell literal",
            ));
        }
        self.push(TokenKind::Shell(val), start_col);
    }

    fn lex_placeholder(&mut self, open: char, start_col: usize, diags: &mut Vec<Diagnostic>) {
        let close = if open == '<' { '>' } else { ']' };
        let required = open == '<';
        self.i += 1;
        let mut inner = String::new();
        let mut closed = false;
        while let Some(&c) = self.chars.get(self.i) {
            if c == close {
                self.i += 1;
                closed = true;
                break;
            }
            if c.is_whitespace() {
                break;
            }
            inner.push(c);
            self.i += 1;
        }
        let span = self.span(start_col, self.col());
        if !closed {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                span,
                format!("unterminated placeholder (expected '{close}')"),
            ));
            return;
        }
        let variadic = inner.ends_with("...");
        if variadic {
            inner.truncate(inner.len() - 3);
        }
        if !is_valid_placeholder_name(&inner) {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                span,
                format!("invalid placeholder name '{inner}'"),
            ));
        }
        self.tokens.push(Token {
            kind: TokenKind::Placeholder(PlaceholderTok {
                name: inner,
                required,
                variadic,
            }),
            span,
        });
    }

    fn lex_word(&mut self, start_col: usize) {
        let mut word = String::new();
        while let Some(&c) = self.chars.get(self.i) {
            if is_word_delimiter(c) || (c == '!' && self.chars.get(self.i + 1) == Some(&'=')) {
                break;
            }
            word.push(c);
            self.i += 1;
        }
        self.push(TokenKind::Bare(word), start_col);
    }
}
