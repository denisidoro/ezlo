//! Best-effort scraping of `--help` output into options and subcommands.

use super::{ScrapedHelp, ScrapedOpt, ScrapedSubcmd};
use crate::spec::parser::cursor::is_valid_flag;
use crate::spec::span::is_reserved_word;
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

#[derive(PartialEq, Eq)]
enum Section {
    Header,
    Options,
    Commands,
    Other,
}

/// `None` when the pattern fails to compile; scraping then just skips that kind of line.
fn opt_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^\s{0,12}((?:-[A-Za-z0-9?]|--[A-Za-z0-9][A-Za-z0-9_-]*)(?:(?:,\s*|\s+|\s*\|\s*)(?:-[A-Za-z0-9?]|--[A-Za-z0-9][A-Za-z0-9_-]*))*)(?:(?:[=\s]+|\[=)(?:<([A-Za-z0-9_.-]+)>|\[([A-Za-z0-9_.-]+)\]|([A-Z][A-Z0-9_-]{1,15}))\]?)?(?:(?:\s{2,}|\t+)(.+))?$",
        )
        .ok()
    })
    .as_ref()
}

fn subcmd_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s{2,8}([a-z][a-z0-9_-]{1,24})(?:,\s*[a-z0-9_-]+)*\s{2,}(.+)$").ok())
        .as_ref()
}

fn section_header(lower: &str) -> Option<Section> {
    if lower.ends_with("options:") || lower.ends_with("flags:") {
        Some(Section::Options)
    } else if lower.ends_with("commands:") || lower.ends_with("subcommands:") {
        Some(Section::Commands)
    } else if lower.starts_with("usage:") || lower.starts_with("examples:") {
        Some(Section::Other)
    } else {
        None
    }
}

pub fn parse_help_text(text: &str) -> ScrapedHelp {
    let mut description = None;
    let mut opts: Vec<ScrapedOpt> = Vec::new();
    let mut subcommands = Vec::new();
    let mut seen_flags = BTreeSet::new();
    let mut seen_subcmds = BTreeSet::new();

    let mut section = Section::Header;
    let mut awaiting_opt_desc = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            awaiting_opt_desc = false;
            continue;
        }

        if let Some(next) = section_header(&trimmed.to_ascii_lowercase()) {
            section = next;
            awaiting_opt_desc = false;
            continue;
        }

        if section == Section::Header && description.is_none() && !trimmed.starts_with('-') {
            description = Some(trimmed.to_string());
        }

        if trimmed.starts_with('-') {
            awaiting_opt_desc = false;
            if let Some(parsed) = parse_opt_line(line, &mut seen_flags) {
                if let Some(opt) = parsed {
                    awaiting_opt_desc = opt.description.is_none();
                    opts.push(opt);
                }
                continue;
            }
        } else if awaiting_opt_desc && line.starts_with("      ") {
            // Description wrapped onto the line after the flags.
            if let Some(last) = opts.last_mut().filter(|o| o.description.is_none()) {
                last.description = Some(trimmed.to_string());
            }
            awaiting_opt_desc = false;
            continue;
        } else {
            awaiting_opt_desc = false;
        }

        if section == Section::Commands {
            subcommands.extend(parse_subcmd_line(line, &mut seen_subcmds));
        }
    }

    ScrapedHelp {
        description,
        opts,
        subcommands,
    }
}

/// `None` if `line` isn't an option line; `Some(None)` if it is but declares no new flags.
fn parse_opt_line(line: &str, seen_flags: &mut BTreeSet<String>) -> Option<Option<ScrapedOpt>> {
    let caps = opt_re()?.captures(line)?;
    let flags_str = caps.get(1).map_or("", |m| m.as_str());
    let flags: Vec<String> = flags_str
        .split(|c: char| c == ',' || c == '|' || c.is_whitespace())
        .map(str::trim)
        .filter(|p| is_valid_flag(p) && seen_flags.insert(p.to_string()))
        .map(str::to_string)
        .collect();
    if flags.is_empty() {
        return Some(None);
    }
    let value_placeholder = caps
        .get(2)
        .or_else(|| caps.get(3))
        .or_else(|| caps.get(4))
        .map(|m| m.as_str().to_string());
    let description = caps.get(5).map(|m| m.as_str().trim().to_string());
    Some(Some(ScrapedOpt {
        flags,
        value_placeholder,
        description,
    }))
}

fn parse_subcmd_line(line: &str, seen: &mut BTreeSet<String>) -> Option<ScrapedSubcmd> {
    let caps = subcmd_re()?.captures(line)?;
    let name = caps.get(1).map_or("", |m| m.as_str());
    if name.is_empty() || is_reserved_word(name) || !seen.insert(name.to_string()) {
        return None;
    }
    Some(ScrapedSubcmd {
        name: name.to_string(),
        description: caps.get(2).map(|m| m.as_str().trim().to_string()),
        opts: Vec::new(),
    })
}
