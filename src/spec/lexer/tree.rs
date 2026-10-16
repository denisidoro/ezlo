use super::{BlockNode, LineContent, TokenKind};
use crate::spec::span::{Diagnostic, RuleCode};

pub(super) fn build_block_tree(flat: Vec<BlockNode>, diags: &mut Vec<Diagnostic>) -> Vec<BlockNode> {
    let mut roots: Vec<BlockNode> = Vec::new();
    for node in flat {
        if node.indent == 0 {
            roots.push(node);
            continue;
        }
        let Some(root) = roots.last_mut() else {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                node.span,
                "unexpected indentation on top-level line",
            ));
            continue;
        };
        insert_child(root, node, diags);
    }
    roots
}

fn insert_child(parent: &mut BlockNode, node: BlockNode, diags: &mut Vec<Diagnostic>) {
    let Some(last_child) = parent.children.last_mut() else {
        parent.children.push(node);
        return;
    };
    if node.indent == last_child.indent {
        parent.children.push(node);
    } else if node.indent > last_child.indent && can_open_child_block(last_child, &node) {
        insert_child(last_child, node, diags);
    } else {
        diags.push(Diagnostic::error(
            RuleCode::E01Lexical,
            node.span,
            format!(
                "inconsistent sibling indentation: expected {} spaces, found {}",
                last_child.indent, node.indent
            ),
        ));
    }
}

fn first_keyword(node: &BlockNode) -> Option<&str> {
    match &node.content {
        LineContent::Raw(_) => Some("$"),
        LineContent::Tokens(toks) => match toks.first().map(|t| &t.kind) {
            Some(TokenKind::Bare(s)) => Some(s.as_str()),
            _ => None,
        },
    }
}

fn can_open_child_block(parent: &BlockNode, child: &BlockNode) -> bool {
    match first_keyword(parent) {
        Some("type" | "cmd" | "recipe") => true,
        Some("opt" | "arg") => first_keyword(child) == Some("when"),
        _ => false,
    }
}
