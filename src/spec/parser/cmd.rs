use super::recipe::parse_recipe_node;
use super::{expect_eof, expect_no_children, is_keyword, take_string};
use crate::spec::ast::{ArgDecl, CmdDecl, OptDecl, RecipeDecl, ValueSpec, WhenClause};
use crate::spec::lexer::{BlockNode, LineContent, Token, TokenKind};
use crate::spec::parser::cursor::{is_valid_flag, TokenCursor};
use crate::spec::span::{Diagnostic, RuleCode, Span};

pub(super) fn parse_cmd_node(
    node: &BlockNode,
    tokens: &[Token],
    diags: &mut Vec<Diagnostic>,
) -> Option<(CmdDecl, Vec<RecipeDecl>)> {
    let mut cur = TokenCursor::new(&tokens[1..], node.span);
    let mut path = Vec::new();
    let mut aliases = Vec::new();
    let mut desc = None;
    // Set once an alias group `(name|alias...)` was parsed: it must be the last command word.
    let mut path_closed = false;

    while let Some(tok) = cur.next() {
        match &tok.kind {
            TokenKind::Bare(w) if !path_closed => path.push(w.clone()),
            TokenKind::LParen if !path_closed => {
                path_closed = true;
                let mut names = take_alias_group(&mut cur, tok.span, diags);
                if !names.is_empty() {
                    path.push(names.remove(0));
                    aliases = names;
                }
            }
            TokenKind::String(s) => {
                desc = Some(s.clone());
                break;
            }
            _ => diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                tok.span,
                "expected command word or description string in 'cmd'",
            )),
        }
    }

    if path.is_empty() {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            node.span,
            "'cmd' requires at least one command word",
        ));
        return None;
    }
    expect_eof(&cur, "unexpected tokens after command description", diags);

    let mut opts = Vec::new();
    let mut args = Vec::new();
    let mut nested_recipes = Vec::new();
    let mut preview: Option<(String, Span)> = None;

    for child in &node.children {
        let LineContent::Tokens(ctoks) = &child.content else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                child.span,
                "disallowed raw '$' line directly inside 'cmd' (use 'preview $ ...' for a dry-run preview)",
            ));
            continue;
        };
        let Some(first) = ctoks.first() else { continue };
        let TokenKind::Bare(kw) = &first.kind else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                first.span,
                "expected 'opt', 'arg', 'alias', 'preview', or 'recipe' inside 'cmd'",
            ));
            continue;
        };
        match kw.as_str() {
            "opt" => opts.extend(parse_opt_node(child, ctoks, diags)),
            "arg" => args.extend(parse_cmd_arg_node(child, ctoks, diags)),
            "alias" => aliases.extend(parse_alias_node(child, ctoks, diags)),
            "preview" => {
                if let Some(p) = parse_preview_node(child, ctoks, diags) {
                    if preview.is_some() {
                        diags.push(Diagnostic::error(
                            RuleCode::E04Duplicate,
                            child.span,
                            "duplicate 'preview' line in 'cmd'",
                        ));
                    } else {
                        preview = Some(p);
                    }
                }
            }
            "recipe" => nested_recipes.extend(parse_recipe_node(child, ctoks, path.clone(), diags)),
            other => {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    first.span,
                    format!("disallowed child '{other}' inside 'cmd'"),
                ));
            }
        }
    }

    let (preview, preview_span) = match preview {
        Some((p, s)) => (Some(p), Some(s)),
        None => (None, None),
    };
    Some((
        CmdDecl {
            path,
            aliases,
            desc,
            opts,
            args,
            recipes: Vec::new(),
            preview,
            preview_span,
            span: node.span,
        },
        nested_recipes,
    ))
}

/// `preview $ <shell>` or ``preview `<shell>` `` inside a `cmd`: the dry-run preview command.
fn parse_preview_node(node: &BlockNode, tokens: &[Token], diags: &mut Vec<Diagnostic>) -> Option<(String, Span)> {
    expect_no_children(&node.children, "'preview' line cannot have children", diags);
    match tokens.get(1..) {
        Some(
            [Token {
                kind: TokenKind::Shell(cmd),
                span,
            }],
        ) if !cmd.trim().is_empty() => Some((cmd.clone(), *span)),
        _ => {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                node.span,
                "'preview' requires a shell command ('preview $ <shell>' or a backtick literal)",
            ));
            None
        }
    }
}

/// Parses the rest of an alias group `(name|alias...)` after its `(`: bare words separated by `|`.
/// The first word is the canonical command word, the others are its aliases.
fn take_alias_group(cur: &mut TokenCursor<'_>, open: Span, diags: &mut Vec<Diagnostic>) -> Vec<String> {
    let mut names = Vec::new();
    loop {
        match cur.next().map(|t| &t.kind) {
            Some(TokenKind::Bare(w)) => names.push(w.clone()),
            _ => {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    open,
                    "malformed alias group; expected '(name|alias...)'",
                ));
                return Vec::new();
            }
        }
        match cur.next().map(|t| &t.kind) {
            Some(TokenKind::Pipe) => continue,
            Some(TokenKind::RParen) => return names,
            _ => {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    open,
                    "malformed alias group; expected '|' or ')'",
                ));
                return Vec::new();
            }
        }
    }
}

/// `alias <name>...` inside a `cmd`: alternative spellings of the command's last word.
fn parse_alias_node(node: &BlockNode, tokens: &[Token], diags: &mut Vec<Diagnostic>) -> Vec<String> {
    expect_no_children(&node.children, "'alias' line cannot have children", diags);
    let mut names = Vec::new();
    for tok in &tokens[1..] {
        match &tok.kind {
            TokenKind::Bare(w) if !w.starts_with('-') => names.push(w.clone()),
            _ => diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                tok.span,
                "expected alias word in 'alias' line",
            )),
        }
    }
    if names.is_empty() && tokens.len() == 1 {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            node.span,
            "'alias' requires at least one name",
        ));
    }
    names
}

/// Consumes consecutive valid flag tokens (e.g. `-f`, `--force`).
fn take_flags(cur: &mut TokenCursor<'_>) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    while let Some(tok) = cur.peek() {
        let TokenKind::Bare(f) = &tok.kind else { break };
        if !is_valid_flag(f) {
            break;
        }
        out.push((f.clone(), tok.span));
        cur.next();
    }
    out
}

fn parse_opt_node(
    node: &BlockNode,
    tokens: &[Token],
    diags: &mut Vec<Diagnostic>,
) -> Option<OptDecl> {
    let mut cur = TokenCursor::new(&tokens[1..], node.span);
    let flag_spans = take_flags(&mut cur);

    if flag_spans.is_empty() {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            node.span,
            "'opt' requires at least one flag (e.g. -f or --force)",
        ));
        return None;
    }

    let flags: Vec<String> = flag_spans.iter().map(|(f, _)| f.clone()).collect();
    let name = canonical_opt_name(&flags);

    // One value spec per placeholder: `opt --arg <name> <value>`, `opt --slurpfile <name> <file> path`.
    let mut values = Vec::new();
    while let Some(Token {
        kind: TokenKind::Placeholder(ph),
        span,
    }) = cur.peek()
    {
        cur.next();
        values.push(cur.parse_value_spec(ph, *span, true, diags));
    }
    check_opt_values(&values, diags);
    let mut values = values.into_iter();
    let value = values.next();
    let extra_values: Vec<_> = values.collect();

    let desc = take_string(&mut cur);

    let mut global = false;
    let mut required = false;
    let mut hidden = false;
    let mut greedy = false;
    let mut exclude_spans = Vec::new();

    while let Some(tok) = cur.next() {
        match &tok.kind {
            TokenKind::Bare(m) if m == "global" => global = true,
            TokenKind::Bare(m) if m == "required" => required = true,
            TokenKind::Bare(m) if m == "hidden" => hidden = true,
            TokenKind::Bare(m) if m == "greedy" => {
                if !value.as_ref().is_some_and(|v| v.optional) {
                    diags.push(Diagnostic::error(
                        RuleCode::E02Structure,
                        tok.span,
                        "'greedy' requires an optional value ('opt -b [boot] ... greedy')",
                    ));
                }
                greedy = true;
            }
            TokenKind::Bare(m) if m == "excludes" => {
                let found = take_flags(&mut cur);
                if found.is_empty() {
                    diags.push(Diagnostic::error(
                        RuleCode::E02Structure,
                        tok.span,
                        "'excludes' requires at least one flag",
                    ));
                }
                exclude_spans.extend(found);
            }
            _ => {
                diags.push(Diagnostic::error(
                    RuleCode::E02Structure,
                    tok.span,
                    "unexpected token in 'opt' line; expected modifier ('global', 'required', 'hidden', 'greedy', 'excludes')",
                ));
            }
        }
    }

    let excludes = exclude_spans.iter().map(|(f, _)| f.clone()).collect();
    let when = parse_when_children(&node.children, "opt", diags);

    Some(OptDecl {
        flags,
        name,
        value: value.map(Box::new),
        desc,
        global,
        required,
        hidden,
        excludes,
        when,
        extra_values,
        greedy,
        span: node.span,
        flag_spans,
        exclude_spans,
    })
}

/// An optional value (`[when]`) must be the option's only value, and only the last value may be
/// variadic (`<value...>` makes the whole option repeatable).
fn check_opt_values(values: &[ValueSpec], diags: &mut Vec<Diagnostic>) {
    let last = values.len().saturating_sub(1);
    for (i, v) in values.iter().enumerate() {
        if v.optional && values.len() > 1 {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                v.span,
                format!("optional value '[{}]' must be the option's only value", v.name),
            ));
        }
        if v.variadic && i != last {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                v.span,
                format!("only the last value of an option can be variadic ('<{}...>')", v.name),
            ));
        }
    }
}

fn canonical_opt_name(flags: &[String]) -> String {
    flags
        .iter()
        .find_map(|f| f.strip_prefix("--"))
        .or_else(|| flags.first().and_then(|f| f.strip_prefix('-')))
        .unwrap_or("")
        .to_string()
}

fn parse_cmd_arg_node(
    node: &BlockNode,
    tokens: &[Token],
    diags: &mut Vec<Diagnostic>,
) -> Option<ArgDecl> {
    let mut cur = TokenCursor::new(&tokens[1..], node.span);
    let Some(Token {
        kind: TokenKind::Placeholder(ph),
        span: ph_span,
    }) = cur.next()
    else {
        diags.push(Diagnostic::error(
            RuleCode::E02Structure,
            node.span,
            "'arg' requires a placeholder (e.g. <name> or [name])",
        ));
        return None;
    };
    let vspec = cur.parse_value_spec(ph, *ph_span, false, diags);
    let desc = take_string(&mut cur);
    expect_eof(&cur, "unexpected trailing tokens on 'arg' line", diags);

    let when = parse_when_children(&node.children, "arg", diags);

    Some(ArgDecl {
        name: vspec.name,
        required: vspec.required,
        variadic: vspec.variadic,
        composite: vspec.composite,
        type_expr: vspec.type_expr,
        suggestions: vspec.suggestions,
        desc,
        when,
        span: node.span,
    })
}

fn parse_when_children(
    children: &[BlockNode],
    parent_kind: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<WhenClause> {
    let mut list = Vec::new();
    for child in children {
        expect_no_children(&child.children, "'when' line cannot have children", diags);
        let LineContent::Tokens(ctoks) = &child.content else {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                child.span,
                format!("disallowed raw '$' line inside '{parent_kind}'"),
            ));
            continue;
        };
        let Some(first) = ctoks.first() else { continue };
        if !is_keyword(first, "when") {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                first.span,
                format!("only 'when' is allowed as a child of '{parent_kind}'"),
            ));
            continue;
        }
        let mut cur = TokenCursor::new(&ctoks[1..], child.span);
        let cond = cur.parse_when_cond(diags);
        if !cur.eat(&TokenKind::Colon) {
            diags.push(Diagnostic::error(
                RuleCode::E02Structure,
                cur.current_span(),
                "expected ':' after 'when' condition",
            ));
        }
        if let Some(type_expr) = cur.parse_type_expr(diags) {
            list.push(WhenClause {
                cond,
                type_expr,
                span: child.span,
            });
        }
        expect_eof(&cur, "unexpected trailing tokens on 'when' line", diags);
    }
    list
}
