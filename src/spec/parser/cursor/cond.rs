use super::{is_valid_flag, TokenCursor};
use crate::spec::ast::{Clause, CompareOp, Composite, ValueSpec, WhenTerm};
use crate::spec::lexer::{PlaceholderTok, TokenKind};
use crate::spec::span::{is_reserved_word, Diagnostic, RuleCode, Span};
use crate::spec::validate::is_builtin_type;

impl TokenCursor<'_> {
    pub fn parse_cond(&mut self, diags: &mut Vec<Diagnostic>) -> Vec<Clause> {
        let mut clauses = Vec::new();
        while let Some((col, col_span)) = self.parse_ident("column", diags) {
            let Some(op_tok) = self.next() else {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    col_span,
                    "expected comparison operator ('=', '!=', '~', or 'in')",
                ));
                break;
            };
            match &op_tok.kind {
                TokenKind::Eq | TokenKind::NotEq | TokenKind::Tilde => {
                    let op = match op_tok.kind {
                        TokenKind::Eq => CompareOp::Eq,
                        TokenKind::NotEq => CompareOp::Ne,
                        _ => CompareOp::Match,
                    };
                    if let Some((val, val_span)) = self.parse_value(diags) {
                        clauses.push(Clause {
                            col,
                            op,
                            values: vec![val],
                            span: col_span.merge(val_span),
                        });
                    }
                }
                TokenKind::Bare(w) if w == "in" => {
                    if !self.eat(&TokenKind::LParen) {
                        diags.push(Diagnostic::error(
                            RuleCode::E02Structure,
                            self.current_span(),
                            "expected '(' after 'in'",
                        ));
                    }
                    let (values, end_span) = self.parse_in_list(op_tok.span, diags);
                    clauses.push(Clause {
                        col,
                        op: CompareOp::In,
                        values,
                        span: col_span.merge(end_span),
                    });
                }
                _ => {
                    diags.push(Diagnostic::error(
                        RuleCode::E02Structure,
                        op_tok.span,
                        "expected '=', '!=', '~', or 'in'",
                    ));
                    break;
                }
            }
            if !self.eat_bare("and") {
                break;
            }
        }
        clauses
    }

    pub fn parse_suggestions(&mut self, diags: &mut Vec<Diagnostic>) -> Vec<String> {
        if !self.eat(&TokenKind::Eq) {
            return Vec::new();
        }
        let mut list = Vec::new();
        while let Some((val, _)) = self.parse_value(diags) {
            list.push(val);
            if !self.eat(&TokenKind::Pipe) {
                break;
            }
        }
        list
    }

    pub fn parse_value_spec(
        &mut self,
        ph: &PlaceholderTok,
        ph_span: Span,
        in_opt: bool,
        diags: &mut Vec<Diagnostic>,
    ) -> ValueSpec {
        let composite = self.parse_composite(diags);
        let type_expr = match self.peek().map(|tok| (&tok.kind, tok.span)) {
            Some((TokenKind::Bare(w), _))
                if w == "effect"
                    || (in_opt && matches!(w.as_str(), "global" | "required" | "hidden" | "greedy" | "excludes" | "dryrun")) =>
            {
                None
            }
            Some((TokenKind::Bare(w), span)) if is_reserved_word(w) && !is_builtin_type(w) => {
                diags.push(Diagnostic::error(
                    RuleCode::E06ReservedWord,
                    span,
                    format!("reserved word '{w}' cannot be used as a type name"),
                ));
                self.next();
                None
            }
            Some((TokenKind::Bare(_), _)) => self.parse_type_expr(diags),
            _ => None,
        };

        let suggestions = self.parse_suggestions(diags);
        ValueSpec {
            name: ph.name.clone(),
            variadic: ph.variadic,
            optional: in_opt && !ph.required,
            composite: composite.map(Box::new),
            type_expr: type_expr.map(Box::new),
            suggestions,
            required: ph.required,
            span: ph_span,
        }
    }

    /// Parses an optional composite modifier `list("<sep>")` / `kv("<sep>"[, <type>])`. The
    /// keyword is only a modifier when followed by `("`, so types named `list`/`kv` stay usable.
    fn parse_composite(&mut self, diags: &mut Vec<Diagnostic>) -> Option<Composite> {
        let kw = self.peek_bare().filter(|w| matches!(*w, "list" | "kv"))?;
        let opens = matches!(self.peek_nth(1).map(|t| &t.kind), Some(TokenKind::LParen))
            && matches!(self.peek_nth(2).map(|t| &t.kind), Some(TokenKind::String(_)));
        if !opens {
            return None;
        }
        self.next();
        self.next();
        let sep_tok = self.next()?;
        let TokenKind::String(sep) = &sep_tok.kind else {
            return None;
        };
        if sep.is_empty() || sep.chars().any(char::is_whitespace) {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                sep_tok.span,
                format!("'{kw}' separator must be non-empty and contain no whitespace"),
            ));
        }
        let value_type = if kw == "kv" && self.eat(&TokenKind::Comma) {
            self.parse_type_expr(diags)
        } else {
            None
        };
        if !self.eat(&TokenKind::RParen) {
            let expected = if kw == "kv" { "',' or ')'" } else { "')'" };
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                self.current_span(),
                format!("expected {expected} after '{kw}' separator"),
            ));
        }
        let sep = sep.clone();
        Some(match kw {
            "list" => Composite::List { sep },
            _ => Composite::Kv { sep, value_type },
        })
    }

    pub fn parse_when_cond(&mut self, diags: &mut Vec<Diagnostic>) -> Vec<WhenTerm> {
        let mut terms = Vec::new();
        loop {
            let start_span = self.current_span();
            let negated = self.eat(&TokenKind::Bang);
            let Some(flag_tok) = self.next() else {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    start_span,
                    "expected flag in 'when' condition",
                ));
                break;
            };
            let TokenKind::Bare(flag) = &flag_tok.kind else {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    flag_tok.span,
                    "expected flag in 'when' condition",
                ));
                break;
            };
            if !is_valid_flag(flag) {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    flag_tok.span,
                    format!("invalid flag '{flag}' in 'when' condition"),
                ));
            }
            let mut end_span = flag_tok.span;
            let value = if self.eat(&TokenKind::Eq) {
                self.parse_value(diags).map(|(v, vspan)| {
                    end_span = vspan;
                    v
                })
            } else {
                None
            };
            terms.push(WhenTerm {
                flag: flag.clone(),
                negated,
                value,
                span: start_span.merge(end_span),
            });
            if !self.eat_bare("and") {
                break;
            }
        }
        terms
    }

    /// Parses `v1, v2, ...)` after `in (`; returns the values and the span of the last token read.
    fn parse_in_list(&mut self, start: Span, diags: &mut Vec<Diagnostic>) -> (Vec<String>, Span) {
        let mut values = Vec::new();
        let mut end_span = start;
        while !self.is_eof() {
            if let Some(rp) = self.peek().filter(|t| t.kind == TokenKind::RParen) {
                end_span = rp.span;
                self.next();
                break;
            }
            let Some((v, vspan)) = self.parse_value(diags) else {
                break;
            };
            end_span = vspan;
            values.push(v);
            if !self.eat(&TokenKind::Comma) && !self.peek_is(&TokenKind::RParen) {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    self.current_span(),
                    "expected ',' or ')' in 'in (...)' list",
                ));
                break;
            }
        }
        (values, end_span)
    }
}
