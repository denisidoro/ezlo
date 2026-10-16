use super::expect_no_children;
use crate::spec::ast::AliasDecl;
use crate::spec::lexer::{BlockNode, Token, TokenKind};
use crate::spec::span::{Diagnostic, RuleCode};

/// Top-level `alias <name> = <word>...` (e.g. `alias gco = git checkout`). Words are bare or
/// `"quoted"`; a word holding spec punctuation (`=`, `:`, `|`, …) must be quoted.
pub(super) fn parse_top_alias_node(
    node: &BlockNode,
    tokens: &[Token],
    diags: &mut Vec<Diagnostic>,
) -> Option<AliasDecl> {
    expect_no_children(
        &node.children,
        "top-level 'alias' line cannot have children",
        diags,
    );
    let err = |diags: &mut Vec<Diagnostic>, tok: Option<&Token>, msg: &str| {
        let span = tok.map_or(node.span, |t| t.span);
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            span,
            msg.to_owned(),
        ));
    };

    let name = match tokens.get(1).map(|t| &t.kind) {
        Some(TokenKind::Bare(w)) if !w.starts_with('-') => w.clone(),
        _ => {
            err(
                diags,
                tokens.get(1),
                "expected alias name after 'alias' (alias <name> = <command>...)",
            );
            return None;
        }
    };
    if !matches!(tokens.get(2).map(|t| &t.kind), Some(TokenKind::Eq)) {
        err(
            diags,
            tokens.get(2),
            "expected '=' after the alias name (alias <name> = <command>...)",
        );
        return None;
    }

    let mut expansion = Vec::with_capacity(tokens.len().saturating_sub(3));
    for tok in &tokens[3..] {
        match &tok.kind {
            TokenKind::Bare(w) | TokenKind::String(w) => expansion.push(w.clone()),
            _ => {
                err(
                    diags,
                    Some(tok),
                    "expected a word in the alias expansion; quote words with spec punctuation",
                );
                return None;
            }
        }
    }
    if expansion.is_empty() {
        err(
            diags,
            None,
            "alias expansion is empty (alias <name> = <command>...)",
        );
        return None;
    }

    Some(AliasDecl {
        name,
        expansion,
        span: node.span,
    })
}
