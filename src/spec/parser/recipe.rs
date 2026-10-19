use super::{expect_eof, expect_no_children, is_keyword, take_string};
use crate::spec::ast::{RecipeArg, RecipeDecl};
use crate::spec::lexer::{is_valid_placeholder_name, BlockNode, LineContent, Token, TokenKind};
use crate::spec::parser::cursor::TokenCursor;
use crate::spec::span::{Diagnostic, RuleCode, Span};

pub(super) fn parse_recipe_node(
    node: &BlockNode,
    tokens: &[Token],
    scope: Vec<String>,
    diags: &mut Vec<Diagnostic>,
) -> Option<RecipeDecl> {
    let mut cur = TokenCursor::new(&tokens[1..], node.span);
    let desc = match cur.next() {
        Some(Token {
            kind: TokenKind::String(s),
            ..
        }) => s.clone(),
        _ => {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                node.span,
                "'recipe' requires a description string",
            ));
            String::new()
        }
    };
    expect_eof(&cur, "unexpected tokens after recipe description", diags);

    let mut templates: Vec<(&str, Span)> = Vec::new();
    let mut args = Vec::new();

    for child in &node.children {
        expect_no_children(&child.children, "recipe child lines cannot have children", diags);
        match &child.content {
            LineContent::Raw(raw) => templates.push((raw.as_str(), child.span)),
            LineContent::Tokens(ctoks) => args.extend(parse_recipe_arg(child, ctoks, diags)),
        }
    }

    let (raw_template, template_span) = match templates.as_slice() {
        [] => {
            diags.push(Diagnostic::error(
                RuleCode::E08RecipeStructure,
                node.span,
                "recipe is missing a '$' template line",
            ));
            ("", node.span)
        }
        [only] => *only,
        [first, second, ..] => {
            diags.push(Diagnostic::error(
                RuleCode::E08RecipeStructure,
                second.1,
                "recipe cannot have multiple '$' template lines",
            ));
            *first
        }
    };

    let (template, placeholders) = extract_template_placeholders(raw_template);

    Some(RecipeDecl {
        desc,
        scope,
        template,
        placeholders,
        args,
        span: node.span,
        template_span,
    })
}

fn parse_recipe_arg(child: &BlockNode, ctoks: &[Token], diags: &mut Vec<Diagnostic>) -> Option<RecipeArg> {
    let first = ctoks.first()?;
    if !is_keyword(first, "arg") {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            first.span,
            "only '$' and 'arg' are allowed inside 'recipe'",
        ));
        return None;
    }
    let mut acur = TokenCursor::new(&ctoks[1..], child.span);
    let Some(Token {
        kind: TokenKind::Placeholder(ph),
        span: ph_span,
    }) = acur.next()
    else {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            child.span,
            "recipe 'arg' requires a placeholder",
        ));
        return None;
    };
    let vspec = acur.parse_value_spec(ph, *ph_span, false, diags);
    if vspec.composite.is_some() {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            *ph_span,
            "'list'/'kv' modifiers are only supported on 'cmd' option and arg slots",
        ));
    }
    let desc = take_string(&mut acur);
    expect_eof(&acur, "unexpected trailing tokens on recipe 'arg' line", diags);
    Some(RecipeArg {
        name: vspec.name,
        variadic: vspec.variadic,
        type_expr: vspec.type_expr,
        suggestions: vspec.suggestions,
        desc,
        required: vspec.required,
        span: child.span,
    })
}

pub fn extract_template_placeholders(raw: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = raw.chars().collect();
    let mut cleaned = String::with_capacity(raw.len());
    let mut placeholders = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'<')
            && let Some((_, end_idx)) = match_placeholder_at(&chars, i + 1) {
                cleaned.push('<');
                cleaned.extend(&chars[i + 2..=end_idx]);
                i = end_idx + 1;
                continue;
            }
        if chars[i] == '<'
            && let Some((name, end_idx)) = match_placeholder_at(&chars, i) {
                if !placeholders.contains(&name) {
                    placeholders.push(name);
                }
                cleaned.extend(&chars[i..=end_idx]);
                i = end_idx + 1;
                continue;
            }
        cleaned.push(chars[i]);
        i += 1;
    }

    (cleaned, placeholders)
}

fn match_placeholder_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) != Some(&'<') {
        return None;
    }
    let rel_end = chars[start + 1..].iter().position(|&c| c == '>')?;
    let end = start + 1 + rel_end;
    let name: String = chars[start + 1..end].iter().collect();
    is_valid_placeholder_name(&name).then_some((name, end))
}
