use crate::spec::lexer::{Token, TokenKind};
use crate::spec::span::{is_reserved_word, Diagnostic, RuleCode, Span};
use crate::spec::validate::is_builtin_type;

mod cond;
mod type_expr;

pub struct TokenCursor<'a> {
    tokens: &'a [Token],
    pos: usize,
    line_span: Span,
}

impl<'a> TokenCursor<'a> {
    pub fn new(tokens: &'a [Token], line_span: Span) -> Self {
        Self {
            tokens,
            pos: 0,
            line_span,
        }
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    pub fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    /// The token `n` positions ahead of the next one (`peek_nth(0) == peek()`).
    pub fn peek_nth(&self, n: usize) -> Option<&'a Token> {
        self.tokens.get(self.pos + n)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&'a Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }

    pub fn current_span(&self) -> Span {
        self.peek()
            .map(|t| t.span)
            .or_else(|| self.tokens.last().map(|t| t.span))
            .unwrap_or(self.line_span)
    }

    pub fn peek_bare(&self) -> Option<&'a str> {
        match &self.peek()?.kind {
            TokenKind::Bare(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn eat_bare(&mut self, expected: &str) -> bool {
        if self.peek_bare() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn peek_is(&self, kind: &TokenKind) -> bool {
        self.peek().is_some_and(|t| &t.kind == kind)
    }

    /// Consumes the next token if it is `kind`.
    pub fn eat(&mut self, kind: &TokenKind) -> bool {
        let hit = self.peek_is(kind);
        if hit {
            self.pos += 1;
        }
        hit
    }

    /// Parses a type name being declared (`type <name>`): reserved words are rejected.
    pub fn parse_typename(&mut self, diags: &mut Vec<Diagnostic>) -> Option<(String, Span)> {
        self.parse_typename_inner(false, diags)
    }

    /// Parses a referenced type name, which may also be a builtin spelled like a reserved word
    /// (`cmd`).
    pub fn parse_typename_ref(&mut self, diags: &mut Vec<Diagnostic>) -> Option<(String, Span)> {
        self.parse_typename_inner(true, diags)
    }

    fn parse_typename_inner(
        &mut self,
        is_ref: bool,
        diags: &mut Vec<Diagnostic>,
    ) -> Option<(String, Span)> {
        let first = self.next()?;
        let TokenKind::Bare(first_seg) = &first.kind else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                first.span,
                "expected type name",
            ));
            return None;
        };
        let mut name = first_seg.clone();
        let mut span = first.span;

        while let Some(colon_tok) = self.peek() {
            if colon_tok.kind != TokenKind::Colon || colon_tok.span.start.col != span.end.col {
                break;
            }
            let Some(next_tok) = self.tokens.get(self.pos + 1) else {
                break;
            };
            let TokenKind::Bare(seg) = &next_tok.kind else {
                break;
            };
            if next_tok.span.start.col != colon_tok.span.end.col {
                break;
            }
            name.push(':');
            name.push_str(seg);
            span = span.merge(next_tok.span);
            self.pos += 2;
        }

        if !is_valid_typename(&name) {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                span,
                format!("invalid type name '{name}'"),
            ));
        }
        if is_reserved_word(&name) && !(is_ref && is_builtin_type(&name)) {
            diags.push(Diagnostic::error(
                RuleCode::E06ReservedWord,
                span,
                format!("reserved word '{name}' cannot be used as a type name"),
            ));
        }
        Some((name, span))
    }

    pub fn parse_ident(
        &mut self,
        role: &str,
        diags: &mut Vec<Diagnostic>,
    ) -> Option<(String, Span)> {
        let Some(tok) = self.next() else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                self.current_span(),
                format!("expected {role} name"),
            ));
            return None;
        };
        let TokenKind::Bare(s) = &tok.kind else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                tok.span,
                format!("expected {role} name"),
            ));
            return None;
        };
        if is_reserved_word(s) {
            diags.push(Diagnostic::error(
                RuleCode::E06ReservedWord,
                tok.span,
                format!("reserved word '{s}' cannot be used as a {role} name"),
            ));
        }
        Some((s.clone(), tok.span))
    }

    pub fn parse_value(&mut self, diags: &mut Vec<Diagnostic>) -> Option<(String, Span)> {
        let Some(tok) = self.next() else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                self.current_span(),
                "expected value",
            ));
            return None;
        };
        match &tok.kind {
            TokenKind::Bare(s) | TokenKind::String(s) => Some((s.clone(), tok.span)),
            _ => {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    tok.span,
                    "expected word or string value",
                ));
                None
            }
        }
    }
}

pub fn is_valid_typename(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.split(':').all(|seg| {
        let mut chars = seg.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        first.is_ascii_lowercase()
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    })
}

/// Accepted flag forms:
/// - short `-x`: one alphanumeric char (clusterable: `-la`, `-n5`);
/// - long `--name`: e.g. `--dry-run`, `--test_output`;
/// - single-dash long `-name`: e.g. `find -name`, `defaults -currentHost`, `nmap -sS`.
///
/// Long names start with an alphanumeric char followed by alphanumerics, `-` or `_`.
pub fn is_valid_flag(s: &str) -> bool {
    let name = s.strip_prefix("--").or_else(|| s.strip_prefix('-'));
    let Some(name) = name else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// True for single-dash flags longer than one char (`-name`), which are never clusters.
pub fn is_single_dash_long_flag(s: &str) -> bool {
    !s.starts_with("--") && s.starts_with('-') && s.chars().count() > 2
}

pub fn parse_duration_ms(s: &str) -> Option<u64> {
    if let Some(num) = s.strip_suffix("ms") {
        num.parse::<u64>().ok()
    } else if let Some(num) = s.strip_suffix('s') {
        num.parse::<u64>().ok().map(|n| n * 1000)
    } else if let Some(num) = s.strip_suffix('m') {
        num.parse::<u64>().ok().map(|n| n * 60_000)
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<u64>().ok().map(|n| n * 3_600_000)
    } else {
        None
    }
}
