use super::TokenCursor;
use crate::spec::ast::{Binding, Clause, Param, Source, TypeExpr, TypeRef};
use crate::spec::lexer::{Token, TokenKind};
use crate::spec::span::{Diagnostic, RuleCode, Span};

impl TokenCursor<'_> {
    pub fn parse_params(&mut self, diags: &mut Vec<Diagnostic>) -> Vec<Param> {
        if !self.eat(&TokenKind::LParen) {
            return Vec::new();
        }
        let mut params = Vec::new();
        while !self.is_eof() {
            if self.eat(&TokenKind::RParen) {
                return params;
            }
            let Some((name, span)) = self.parse_ident("parameter", diags) else {
                break;
            };
            let mut default = Vec::new();
            let mut end_span = span;
            if self.eat(&TokenKind::Eq) {
                default = self.parse_sources(diags);
                if let Some(last) = default.last() {
                    end_span = last.span();
                }
            }
            params.push(Param {
                name,
                default,
                span: span.merge(end_span),
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        if !self.eat(&TokenKind::RParen) {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                self.current_span(),
                "expected ')' after parameters",
            ));
        }
        params
    }

    pub fn parse_sources(&mut self, diags: &mut Vec<Diagnostic>) -> Vec<Source> {
        let mut sources = Vec::new();
        loop {
            let Some(tok) = self.next() else {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    self.current_span(),
                    "expected value source",
                ));
                break;
            };
            sources.extend(parse_source(tok, diags));
            if !self.eat(&TokenKind::Pipe) {
                break;
            }
        }
        sources
    }

    pub fn parse_type_expr(&mut self, diags: &mut Vec<Diagnostic>) -> Option<TypeExpr> {
        let (name, name_span) = self.parse_typename_ref(diags)?;
        let mut end_span = name_span;
        let mut bindings = Vec::new();

        if self.eat(&TokenKind::LParen) {
            while !self.is_eof() {
                if self.peek_is(&TokenKind::RParen) {
                    break;
                }
                let Some((bname, bspan)) = self.parse_ident("binding", diags) else {
                    break;
                };
                if !self.eat(&TokenKind::Eq) {
                    diags.push(Diagnostic::error(
                        RuleCode::E02Structure,
                        self.current_span(),
                        "expected '=' in parameter binding",
                    ));
                }
                let sources = self.parse_sources(diags);
                let bend = sources.last().map(Source::span).unwrap_or(bspan);
                bindings.push(Binding {
                    name: bname,
                    sources,
                    span: bspan.merge(bend),
                });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            if let Some(rp) = self.peek() {
                if rp.kind == TokenKind::RParen {
                    end_span = rp.span;
                    self.next();
                } else {
                    diags.push(Diagnostic::error(
                        RuleCode::E02Structure,
                        rp.span,
                        "expected ')' after type bindings",
                    ));
                }
            }
        }

        let type_ref = TypeRef {
            name,
            bindings,
            span: name_span.merge(end_span),
        };

        let where_cond = self.parse_clause_section("where", &mut end_span, diags);
        let rank = self.parse_clause_section("rank", &mut end_span, diags);

        Some(TypeExpr {
            type_ref,
            where_cond,
            rank,
            span: name_span.merge(end_span),
        })
    }

    /// Parses `<kw> <cond>` if the next token is `kw`, extending `end_span` to the last clause.
    fn parse_clause_section(
        &mut self,
        kw: &str,
        end_span: &mut Span,
        diags: &mut Vec<Diagnostic>,
    ) -> Option<Vec<Clause>> {
        if !self.eat_bare(kw) {
            return None;
        }
        let clauses = self.parse_cond(diags);
        if let Some(last) = clauses.last() {
            *end_span = last.span;
        }
        Some(clauses)
    }
}

/// Parses one fallback-chain source: a string literal or `opt.X` / `arg.X` / `env.X`.
fn parse_source(tok: &Token, diags: &mut Vec<Diagnostic>) -> Option<Source> {
    let span = tok.span;
    let raw = match &tok.kind {
        TokenKind::String(val) => {
            return Some(Source::Literal {
                value: val.clone(),
                span,
            })
        }
        TokenKind::Bare(raw) => raw,
        _ => {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                span,
                "expected source (opt.X, arg.X, env.X, or string)",
            ));
            return None;
        }
    };
    let Some((prefix, name)) = raw.split_once('.') else {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            span,
            format!("expected source (opt.X, arg.X, env.X, or string), found '{raw}'"),
        ));
        return None;
    };
    let valid_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let name = name.to_string();
    match (prefix, valid_name) {
        ("opt", true) => Some(Source::Opt { name, span }),
        ("arg", true) => Some(Source::Arg { name, span }),
        ("env", true) => Some(Source::Env { name, span }),
        _ => {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                span,
                format!("invalid source reference '{raw}'"),
            ));
            None
        }
    }
}
