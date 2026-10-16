use super::{expect_eof, expect_no_children};
use crate::spec::ast::{BaseType, Generator, RefineType, TypeDecl};
use crate::spec::lexer::{BlockNode, LineContent, Token, TokenKind};
use crate::spec::parser::cursor::{parse_duration_ms, TokenCursor};
use crate::spec::span::{Diagnostic, RuleCode, Span};

type Named = (String, Span);

/// Properties collected from the child lines of a base `type` block.
#[derive(Default)]
struct TypeProps {
    shell_gen: Option<Named>,
    values_gen: Option<(Vec<String>, Span)>,
    cols: Option<Vec<Named>>,
    insert: Option<Named>,
    show: Option<Vec<Named>>,
    preview: Option<Named>,
    cache_ms: Option<(u64, Span)>,
}

pub(super) fn parse_type_node(
    node: &BlockNode,
    tokens: &[Token],
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeDecl> {
    let mut cur = TokenCursor::new(&tokens[1..], node.span);
    let (name, name_span) = cur.parse_typename(diags)?;

    if cur.peek_is(&TokenKind::Eq) {
        cur.next();
        let refines = cur.parse_type_expr(diags)?;
        expect_eof(&cur, "unexpected trailing tokens in refinement type", diags);
        if let Some(child) = node.children.first() {
            diags.push(Diagnostic::error(
                RuleCode::E03TypeForm,
                child.span,
                "refinement type cannot have child lines",
            ));
        }
        return Some(TypeDecl::Refine(RefineType {
            name,
            refines,
            span: name_span.merge(node.span),
        }));
    }

    let params = cur.parse_params(diags);
    expect_eof(&cur, "unexpected tokens after type header", diags);

    let mut props = TypeProps::default();
    for child in &node.children {
        expect_no_children(&child.children, "type property lines cannot have children", diags);
        parse_type_child(child, &mut props, diags);
    }

    let generator = build_generator(props.shell_gen, props.values_gen, name_span, diags);

    let col_spans = props.cols.unwrap_or_else(|| vec![("value".to_string(), name_span)]);
    let col_names: Vec<String> = col_spans.iter().map(|(c, _)| c.clone()).collect();
    let (insert_col, insert_span) = match props.insert {
        Some((c, s)) => (c, Some(s)),
        None => (
            col_names
                .first()
                .cloned()
                .unwrap_or_else(|| "value".to_string()),
            None,
        ),
    };
    let show_spans = props.show.unwrap_or_else(|| col_spans.clone());
    let show_cols: Vec<String> = show_spans.iter().map(|(c, _)| c.clone()).collect();

    Some(TypeDecl::Base(BaseType {
        name,
        params,
        generator,
        cols: col_names,
        insert: insert_col,
        show: show_cols,
        preview: props.preview.map(|(p, _)| p),
        cache_ms: props.cache_ms.map(|(m, _)| m),
        span: name_span,
        col_spans,
        insert_span,
        show_spans,
    }))
}

fn parse_type_child(child: &BlockNode, props: &mut TypeProps, diags: &mut Vec<Diagnostic>) {
    let ctoks = match &child.content {
        LineContent::Raw(cmd) => {
            if props.shell_gen.is_some() {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    child.span,
                    "duplicate '$' generator line in type",
                ));
            } else {
                props.shell_gen = Some((cmd.clone(), child.span));
            }
            return;
        }
        LineContent::Tokens(ctoks) => ctoks,
    };
    let Some(first) = ctoks.first() else { return };
    let TokenKind::Bare(prop) = &first.kind else {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            first.span,
            "invalid property in type block",
        ));
        return;
    };
    let mut ccur = TokenCursor::new(&ctoks[1..], child.span);
    let span = child.span;
    match prop.as_str() {
        "values" => {
            check_dup(props.values_gen.is_some(), "values", span, diags);
            let mut vals = Vec::new();
            while !ccur.is_eof() {
                let Some((v, _)) = ccur.parse_value(diags) else { break };
                vals.push(v);
            }
            if vals.is_empty() {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    span,
                    "'values' requires at least one value",
                ));
            }
            props.values_gen = Some((vals, span));
        }
        "cols" => {
            check_dup(props.cols.is_some(), "cols", span, diags);
            props.cols = Some(parse_column_list(&mut ccur, "cols", span, diags));
        }
        "insert" => {
            check_dup(props.insert.is_some(), "insert", span, diags);
            if let Some(pair) = ccur.parse_ident("column", diags) {
                props.insert = Some(pair);
            }
            expect_eof(&ccur, "'insert' takes exactly one column name", diags);
        }
        "show" => {
            check_dup(props.show.is_some(), "show", span, diags);
            props.show = Some(parse_column_list(&mut ccur, "show", span, diags));
        }
        "preview" => {
            check_dup(props.preview.is_some(), "preview", span, diags);
            match ccur.next() {
                Some(Token {
                    kind: TokenKind::Shell(cmd),
                    span,
                }) => props.preview = Some((cmd.clone(), *span)),
                _ => diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    ccur.current_span(),
                    "'preview' requires a backtick shell command",
                )),
            }
        }
        "cache" => {
            check_dup(props.cache_ms.is_some(), "cache", span, diags);
            parse_cache(&mut ccur, props, diags);
        }
        other => {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                first.span,
                format!("disallowed child '{other}' inside 'type' block"),
            ));
        }
    }
}

fn check_dup(present: bool, prop: &str, span: Span, diags: &mut Vec<Diagnostic>) {
    if present {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            span,
            format!("duplicate '{prop}' line in type"),
        ));
    }
}

fn parse_column_list(
    ccur: &mut TokenCursor<'_>,
    prop: &str,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Named> {
    let mut list = Vec::new();
    while !ccur.is_eof() {
        let Some(pair) = ccur.parse_ident("column", diags) else { break };
        list.push(pair);
    }
    if list.is_empty() {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            span,
            format!("'{prop}' requires at least one column name"),
        ));
    }
    list
}

fn parse_cache(ccur: &mut TokenCursor<'_>, props: &mut TypeProps, diags: &mut Vec<Diagnostic>) {
    let Some(Token {
        kind: TokenKind::Bare(dur),
        span,
    }) = ccur.next()
    else {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            ccur.current_span(),
            "'cache' requires a duration (e.g. 2s, 500ms)",
        ));
        return;
    };
    match parse_duration_ms(dur) {
        Some(ms) => props.cache_ms = Some((ms, *span)),
        None => diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            *span,
            format!("invalid duration '{dur}'; expected <n>(ms|s|m|h)"),
        )),
    }
}

fn build_generator(
    shell_gen: Option<Named>,
    values_gen: Option<(Vec<String>, Span)>,
    name_span: Span,
    diags: &mut Vec<Diagnostic>,
) -> Generator {
    match (shell_gen, values_gen) {
        (Some((command, span)), None) => Generator::Shell { command, span },
        (None, Some((values, span))) => Generator::Values { values, span },
        (Some(_), Some((_, span))) => {
            diags.push(Diagnostic::error(
                RuleCode::E03TypeForm,
                span,
                "type block cannot have both '$' generator and 'values'",
            ));
            Generator::Values {
                values: Vec::new(),
                span,
            }
        }
        (None, None) => {
            diags.push(Diagnostic::error(
                RuleCode::E03TypeForm,
                name_span,
                "type block must contain either a '$' generator line or a 'values' line",
            ));
            Generator::Values {
                values: Vec::new(),
                span: name_span,
            }
        }
    }
}
