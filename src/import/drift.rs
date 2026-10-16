//! Spec drift detection (`ezlo check --drift`): compares the grammar declared in `.ezlo` files with
//! what a command's live `--help` output (scraped via the sandboxed probe) advertises.

use super::{parse_help_text, resolve_binary, run_sandboxed_probe, ScrapedHelp};
use crate::spec::ast::CmdDecl;
use crate::spec::SpecIndex;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fmt::Write;

/// Upstream flags never reported as missing: specs rarely bother declaring them.
const IGNORED_UPSTREAM_FLAGS: &[&str] = &["-h", "--help"];

/// Most `cmd` paths probed per root, so a huge spec can't turn one check into hundreds of probes.
pub const MAX_PROBED_PATHS: usize = 64;

/// An upstream option (one `--help` line) with flags the spec doesn't know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedFlag {
    /// The line's flags missing from the spec (a subset when the spec only lacks an alias).
    pub flags: Vec<String>,
    pub description: Option<String>,
}

/// An upstream subcommand with no `cmd` (or alias) in the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedSubcmd {
    pub name: String,
    pub description: Option<String>,
}

/// Drift of one declared `cmd` path against its `--help` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdDrift {
    pub path: Vec<String>,
    pub added_flags: Vec<AddedFlag>,
    pub added_subcmds: Vec<AddedSubcmd>,
    /// Non-hidden flags declared on this `cmd` that its `--help` no longer lists.
    pub removed_flags: Vec<String>,
    /// Why this path wasn't (fully) compared: probe failure, unparseable or shared help text.
    pub skipped: Option<String>,
}

impl CmdDrift {
    pub fn has_drift(&self) -> bool {
        !self.added_flags.is_empty() || !self.added_subcmds.is_empty() || !self.removed_flags.is_empty()
    }
}

/// Fetches the help text of a full command path (root first), e.g. `["git", "checkout"]`.
pub type HelpFetcher<'a> = dyn FnMut(&[String]) -> Result<String> + 'a;

/// Production [`HelpFetcher`]: runs `<root> [sub...] --help` (then `-h`, then `help [sub...]`)
/// through the sandboxed probe used by `ezlo import`.
pub fn probe_help(path: &[String]) -> Result<String> {
    let (root, subs) = path.split_first().context("empty command path")?;
    let binary = resolve_binary(root)?;
    let subs: Vec<&str> = subs.iter().map(String::as_str).collect();
    let attempts: [Vec<&str>; 3] = [
        [subs.as_slice(), &["--help"]].concat(),
        [subs.as_slice(), &["-h"]].concat(),
        [&["help"], subs.as_slice()].concat(),
    ];
    let mut last_err = None;
    for args in &attempts {
        match run_sandboxed_probe(&binary, args) {
            Ok(out) if !out.trim().is_empty() => return Ok(out),
            Ok(_) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no help output")))
        .with_context(|| format!("failed to obtain help output from `{}`", path.join(" ")))
}

/// Compares every `cmd` path under `root` declared in `index` (root first, then subcommands in
/// declaration order, at most [`MAX_PROBED_PATHS`]) with the help text `fetch` returns for it.
///
/// - Added flags: options listed upstream (except `-h`/`--help`) with flags known neither to that
///   `cmd` nor as `global` opts of its ancestors.
/// - Added subcommands: listed upstream but not declared as a child `cmd` or alias.
/// - Removed flags: flags of non-hidden opts declared on that `cmd` absent from its help; only
///   checked when the help lists at least one option, so terse or unparseable help can't flag them all.
/// - A subcommand whose help is identical to its parent's (the tool ignored the subcommand) is skipped.
pub fn detect_drift(index: &SpecIndex, root: &str, fetch: &mut HelpFetcher<'_>) -> Vec<CmdDrift> {
    let cmds: Vec<&CmdDecl> = index
        .cmds
        .iter()
        .filter(|c| c.path.first().is_some_and(|r| r == root))
        .take(MAX_PROBED_PATHS)
        .collect();
    let mut fetched: Vec<(Vec<String>, String)> = Vec::new();
    let mut out = Vec::new();
    for cmd in &cmds {
        let mut drift = CmdDrift {
            path: cmd.path.clone(),
            ..CmdDrift::default()
        };
        let text = match fetch(&cmd.path) {
            Ok(text) => text,
            Err(e) => {
                drift.skipped = Some(format!("{e:#}"));
                out.push(drift);
                continue;
            }
        };
        let parent = &cmd.path[..cmd.path.len() - 1];
        let same_as_parent = fetched
            .iter()
            .any(|(p, t)| p.as_slice() == parent && t.trim() == text.trim());
        fetched.push((cmd.path.clone(), text.clone()));
        if same_as_parent {
            drift.skipped = Some("help output is the same as the parent command's".into());
        } else {
            let scraped = parse_help_text(&text);
            if scraped.opts.is_empty() && scraped.subcommands.is_empty() {
                drift.skipped = Some("no options or subcommands found in help output".into());
            } else {
                compare(index, cmd, &scraped, &mut drift);
            }
        }
        out.push(drift);
    }
    out
}

fn compare(index: &SpecIndex, cmd: &CmdDecl, scraped: &ScrapedHelp, drift: &mut CmdDrift) {
    let mut known: BTreeSet<&str> = cmd.opts.iter().flat_map(|o| o.flags.iter().map(String::as_str)).collect();
    for ancestor in index.cmds.iter().filter(|c| c.path.len() < cmd.path.len() && cmd.path.starts_with(&c.path)) {
        known.extend(ancestor.opts.iter().filter(|o| o.global).flat_map(|o| o.flags.iter().map(String::as_str)));
    }
    for opt in &scraped.opts {
        let flags: Vec<String> = opt
            .flags
            .iter()
            .filter(|f| !known.contains(f.as_str()) && !IGNORED_UPSTREAM_FLAGS.contains(&f.as_str()))
            .cloned()
            .collect();
        if !flags.is_empty() {
            drift.added_flags.push(AddedFlag {
                flags,
                description: opt.description.clone(),
            });
        }
    }

    let children: BTreeSet<&str> = index
        .cmds
        .iter()
        .filter(|c| c.path.len() == cmd.path.len() + 1 && c.path.starts_with(&cmd.path))
        .flat_map(|c| c.path.last().into_iter().chain(&c.aliases).map(String::as_str))
        .collect();
    drift.added_subcmds = scraped
        .subcommands
        .iter()
        .filter(|s| !children.contains(s.name.as_str()))
        .map(|s| AddedSubcmd {
            name: s.name.clone(),
            description: s.description.clone(),
        })
        .collect();

    if !scraped.opts.is_empty() {
        let upstream: BTreeSet<&str> = scraped.opts.iter().flat_map(|o| o.flags.iter().map(String::as_str)).collect();
        drift.removed_flags = cmd
            .opts
            .iter()
            .filter(|o| !o.hidden)
            .flat_map(|o| &o.flags)
            .filter(|f| !upstream.contains(f.as_str()))
            .cloned()
            .collect();
    }
}

/// Human-readable report: one block per drifted or skipped path, `+` for upstream additions
/// missing from the spec, `-` for declared flags gone from `--help`, `?` for skipped paths.
pub fn render_drift(drifts: &[CmdDrift]) -> String {
    let mut out = String::new();
    for d in drifts.iter().filter(|d| d.has_drift() || d.skipped.is_some()) {
        let _ = writeln!(out, "{}", d.path.join(" "));
        if let Some(reason) = &d.skipped {
            let _ = writeln!(out, "  ? skipped: {reason}");
        }
        for f in &d.added_flags {
            push_entry(&mut out, '+', "flag", &f.flags.join(", "), f.description.as_deref());
        }
        for s in &d.added_subcmds {
            push_entry(&mut out, '+', "cmd ", &s.name, s.description.as_deref());
        }
        for f in &d.removed_flags {
            push_entry(&mut out, '-', "flag", f, Some("declared in spec, not in --help"));
        }
    }
    out
}

fn push_entry(out: &mut String, sign: char, kind: &str, name: &str, desc: Option<&str>) {
    let _ = match desc.filter(|d| !d.is_empty()) {
        Some(desc) => writeln!(out, "  {sign} {kind} {name:<24} {desc}"),
        None => writeln!(out, "  {sign} {kind} {name}"),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::parse_and_validate;
    use anyhow::bail;
    use std::collections::HashMap;

    fn index(src: &str) -> SpecIndex {
        let (spec, diags) = parse_and_validate(src);
        assert!(!crate::spec::has_errors(&diags), "{diags:?}");
        SpecIndex::from_single(spec)
    }

    fn run(index: &SpecIndex, root: &str, helps: &[(&str, &str)]) -> (Vec<CmdDrift>, Vec<String>) {
        let helps: HashMap<&str, &str> = helps.iter().copied().collect();
        let mut asked = Vec::new();
        let drifts = detect_drift(index, root, &mut |path: &[String]| {
            let key = path.join(" ");
            asked.push(key.clone());
            match helps.get(key.as_str()) {
                Some(text) => Ok(text.to_string()),
                None => bail!("no help for {key}"),
            }
        });
        (drifts, asked)
    }

    const SPEC: &str = r#"
cmd tool "A tool"
  opt -v --verbose "Verbose"
  opt --color "Colorize" global
  opt --old "Removed upstream"
  opt --secret "Undocumented" hidden

cmd tool (build|b) "Build"
  opt -j <jobs> "Jobs"
"#;

    const ROOT_HELP: &str = "Usage: tool [OPTIONS] <COMMAND>

Commands:
  build    Build things
  deploy   Deploy things

Options:
  -v, --verbose   Verbose
  -q, --quiet     Quiet output
      --color     Colorize
  -h, --help      Print help
";

    #[test]
    fn reports_added_and_removed_flags_and_subcommands() {
        let idx = index(SPEC);
        let build_help = "Usage: tool build\n\nOptions:\n  -j <JOBS>       Jobs\n  --release       Release mode\n  --color         Colorize\n";
        let (drifts, asked) = run(&idx, "tool", &[("tool", ROOT_HELP), ("tool build", build_help)]);
        assert_eq!(asked, ["tool", "tool build"]);

        let root = &drifts[0];
        assert_eq!(root.path, ["tool"]);
        assert_eq!(root.skipped, None);
        assert_eq!(
            root.added_flags,
            [AddedFlag {
                flags: vec!["-q".into(), "--quiet".into()],
                description: Some("Quiet output".into()),
            }]
        );
        let subs: Vec<&str> = root.added_subcmds.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(subs, ["deploy"], "aliased `build` is declared");
        assert_eq!(root.removed_flags, ["--old"], "hidden `--secret` is exempt");

        let build = &drifts[1];
        let added: Vec<&str> = build.added_flags.iter().flat_map(|f| f.flags.iter().map(String::as_str)).collect();
        assert_eq!(added, ["--release"], "global `--color` of the parent is known");
        assert!(build.removed_flags.is_empty());
    }

    #[test]
    fn reports_missing_alias_only() {
        let idx = index("cmd tool\n  opt --verbose \"Verbose\"\n");
        let (drifts, _) = run(&idx, "tool", &[("tool", "Options:\n  -v, --verbose   Verbose\n")]);
        assert_eq!(drifts[0].added_flags[0].flags, ["-v"]);
        assert!(drifts[0].removed_flags.is_empty());
    }

    #[test]
    fn in_sync_spec_has_no_drift() {
        let idx = index("cmd tool\n  opt -v --verbose \"Verbose\"\n");
        let (drifts, _) = run(&idx, "tool", &[("tool", "Options:\n  -v, --verbose   Verbose\n  -h, --help  Help\n")]);
        assert!(!drifts[0].has_drift(), "{drifts:?}");
        assert_eq!(render_drift(&drifts), "");
    }

    #[test]
    fn skips_failed_unparseable_and_parent_echo_help() {
        let idx = index(SPEC);
        let (drifts, _) = run(&idx, "tool", &[("tool", "just some prose\n")]);
        assert!(drifts[0].skipped.as_deref().unwrap().contains("no options"));
        assert!(drifts[0].removed_flags.is_empty(), "unparseable help must not flag every opt as removed");
        assert!(drifts[1].skipped.as_deref().unwrap().contains("no help for tool build"));

        let (drifts, _) = run(&idx, "tool", &[("tool", ROOT_HELP), ("tool build", ROOT_HELP)]);
        assert!(drifts[1].skipped.as_deref().unwrap().contains("same as the parent"));
        assert!(!drifts[1].has_drift());
    }

    #[test]
    fn help_without_options_section_does_not_remove_flags() {
        let idx = index(SPEC);
        let help = "Commands:\n  build    Build things\n  deploy   Deploy things\n";
        let (drifts, _) = run(&idx, "tool", &[("tool", help)]);
        assert!(drifts[0].removed_flags.is_empty());
        assert_eq!(drifts[0].added_subcmds.len(), 1);
    }

    #[test]
    fn only_probes_paths_of_the_requested_root() {
        let idx = index("cmd tool\ncmd other\ncmd other sub\n");
        let (drifts, asked) = run(&idx, "other", &[]);
        assert_eq!(asked, ["other", "other sub"]);
        assert_eq!(drifts.len(), 2);
    }

    #[test]
    fn renders_signed_entries() {
        let drifts = vec![CmdDrift {
            path: vec!["tool".into()],
            added_flags: vec![AddedFlag {
                flags: vec!["-q".into(), "--quiet".into()],
                description: Some("Quiet".into()),
            }],
            added_subcmds: vec![AddedSubcmd {
                name: "deploy".into(),
                description: None,
            }],
            removed_flags: vec!["--old".into()],
            skipped: None,
        }];
        let text = render_drift(&drifts);
        assert!(text.starts_with("tool\n"), "{text}");
        assert!(text.contains("  + flag -q, --quiet"), "{text}");
        assert!(text.contains("  + cmd  deploy\n"), "{text}");
        assert!(text.contains("  - flag --old"), "{text}");
    }
}
