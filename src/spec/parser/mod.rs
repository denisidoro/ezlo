mod alias;
mod cmd;
pub mod cursor;
mod recipe;
mod type_decl;

pub use recipe::extract_template_placeholders;

use crate::spec::ast::Spec;
use crate::spec::lexer::{lex_blocks, BlockNode, LineContent, Token, TokenKind};
use crate::spec::parser::cursor::TokenCursor;
use crate::spec::span::{Diagnostic, RuleCode};

pub fn parse_spec_raw(source: &str) -> (Spec, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let roots = lex_blocks(source, &mut diags);
    let mut spec = Spec::default();

    for node in &roots {
        let LineContent::Tokens(tokens) = &node.content else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                node.span,
                "raw '$' line is not allowed at file root",
            ));
            continue;
        };
        let Some(first) = tokens.first() else {
            continue;
        };
        let TokenKind::Bare(kw) = &first.kind else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                first.span,
                "expected 'type', 'cmd', 'recipe', or 'alias' at top level",
            ));
            continue;
        };
        match kw.as_str() {
            "type" => {
                if let Some(td) = type_decl::parse_type_node(node, tokens, &mut diags) {
                    spec.types.push(td);
                }
            }
            "cmd" => {
                if let Some((cmd, mut nested_recipes)) = cmd::parse_cmd_node(node, tokens, &mut diags) {
                    spec.cmds.push(cmd);
                    spec.recipes.append(&mut nested_recipes);
                }
            }
            "recipe" => {
                if let Some(rec) = recipe::parse_recipe_node(node, tokens, Vec::new(), &mut diags) {
                    spec.recipes.push(rec);
                }
            }
            "alias" => {
                if let Some(alias) = alias::parse_top_alias_node(node, tokens, &mut diags) {
                    spec.aliases.push(alias);
                }
            }
            other => {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    first.span,
                    format!("unexpected top-level declaration '{other}'; expected 'type', 'cmd', 'recipe', or 'alias'"),
                ));
            }
        }
    }

    (spec, diags)
}

/// Consumes the next token if it is a string literal and returns its contents.
fn take_string(cur: &mut TokenCursor<'_>) -> Option<String> {
    let Some(Token {
        kind: TokenKind::String(s),
        ..
    }) = cur.peek()
    else {
        return None;
    };
    cur.next();
    Some(s.clone())
}

/// Reports an E02 error at the cursor position if any tokens remain.
fn expect_eof(cur: &TokenCursor<'_>, msg: &str, diags: &mut Vec<Diagnostic>) {
    if !cur.is_eof() {
        diags.push(Diagnostic::error(RuleCode::E02Structure, cur.current_span(), msg));
    }
}

/// Reports an E02 error if a line that must be a leaf has child lines.
fn expect_no_children(children: &[BlockNode], msg: &str, diags: &mut Vec<Diagnostic>) {
    if let Some(first) = children.first() {
        diags.push(Diagnostic::error(RuleCode::E02Structure, first.span, msg));
    }
}

fn is_keyword(tok: &Token, kw: &str) -> bool {
    matches!(&tok.kind, TokenKind::Bare(k) if k == kw)
}
