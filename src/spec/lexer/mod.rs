mod tokens;
mod tree;

use crate::spec::span::{Diagnostic, LineCol, RuleCode, Span};
use std::borrow::Cow;
use tokens::lex_token_line;
use tree::build_block_tree;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderTok {
    pub name: String,
    pub required: bool,
    pub variadic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    String(String),
    Shell(String),
    Placeholder(PlaceholderTok),
    Bare(String),
    Eq,
    NotEq,
    Pipe,
    LParen,
    RParen,
    Comma,
    Colon,
    Tilde,
    Bang,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineContent {
    Raw(String),
    Tokens(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockNode {
    pub indent: usize,
    pub span: Span,
    pub content: LineContent,
    pub children: Vec<BlockNode>,
}

pub fn lex_blocks(source: &str, diags: &mut Vec<Diagnostic>) -> Vec<BlockNode> {
    let normalized: Cow<'_, str> = if source.contains("\r\n") {
        Cow::Owned(source.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(source)
    };
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut flat_lines = Vec::new();
    let mut idx = 0;

    while idx < lines.len() {
        let line = lines[idx];
        let line_no = idx + 1;

        if let Some(tab_pos) = leading_tab_col(line) {
            diags.push(Diagnostic::error(
                RuleCode::E01Lexical,
                Span::line(line_no, tab_pos, tab_pos + 1),
                "tabs are not allowed in leading indentation; use spaces",
            ));
            idx += 1;
            continue;
        }

        let indent = line.bytes().take_while(|&b| b == b' ').count();
        let trimmed_start = &line[indent..];

        if trimmed_start.is_empty() || trimmed_start.starts_with('#') {
            idx += 1;
            continue;
        }

        if let Some(after_dollar) = trimmed_start.strip_prefix("$ ") {
            let start_col = indent + 1;
            let mut raw = after_dollar.trim_end().to_string();
            let mut end_line = line_no;
            let mut end_col = line.chars().count().max(start_col);

            while raw.ends_with('\\') && idx + 1 < lines.len() {
                idx += 1;
                end_line = idx + 1;
                let next_line = lines[idx].trim_end();
                end_col = next_line.chars().count().max(1);
                raw.push('\n');
                raw.push_str(next_line);
            }

            let span = Span::new(
                LineCol::new(line_no, start_col),
                LineCol::new(end_line, end_col),
            );
            flat_lines.push(BlockNode {
                indent,
                span,
                content: LineContent::Raw(raw),
                children: Vec::new(),
            });
            idx += 1;
            continue;
        }

        let tokens = lex_token_line(trimmed_start, line_no, indent + 1, diags);
        if let (Some(first), Some(last)) = (tokens.first(), tokens.last()) {
            let span = first.span.merge(last.span);
            flat_lines.push(BlockNode {
                indent,
                span,
                content: LineContent::Tokens(tokens),
                children: Vec::new(),
            });
        }
        idx += 1;
    }

    build_block_tree(flat_lines, diags)
}

fn leading_tab_col(line: &str) -> Option<usize> {
    line.bytes()
        .position(|b| b != b' ')
        .filter(|&i| line.as_bytes()[i] == b'\t')
        .map(|i| i + 1)
}

pub fn is_valid_placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
