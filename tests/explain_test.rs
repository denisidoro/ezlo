//! `ezlo explain`: token-by-token dissection of a command line (roles, bindings, `when` clauses,
//! parameter bindings, nested command lines), diagnostics and candidate provenance.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::explain::{explain_line, ExplainStyle, Explanation, TokenExplain};
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::{EmptyUsageLookup, UsageLookup, UsageRecord};
use ezlo::engine::{complete_line, CompletionRequest};
use ezlo::spec::ast::Spec;
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
type container(host = opt.host | env.DOCKER_HOST)
  $ docker -H "$host" ps -a --format '{{.Names}}\t{{.State}}'
  cols name state

cmd timeout "Run a command with a time limit"
  arg <duration> = 1 | 5 | 10
  arg <command...> cmd

cmd git "Version control"

cmd git checkout "Switch branches"
  alias co
  opt -b <branch> "Create a new branch"
  opt --force "Discard local changes"
  arg [branch]

cmd docker "Container runtime"
  opt -H --host <host> "Daemon socket" global

cmd docker rm "Remove containers"
  opt -f --force "Force removal"
  opt --format <fmt> = json | table "Output format"
  arg <container...> container
    when --force: container
"#;

fn spec(src: &str) -> Spec {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    spec
}

/// Usage store that learned one value for every context.
struct LearnedUsage;

impl UsageLookup for LearnedUsage {
    fn lookup_usage(&self, _ctx: &str, _label: &str, _cwd: &str) -> UsageRecord {
        UsageRecord::default()
    }

    fn learned_values(&self, _ctx: &str) -> Vec<String> {
        vec!["learned-branch".to_string()]
    }
}

struct Fixture {
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    env: HashMap<String, String>,
    aliases: HashMap<String, String>,
}

impl Fixture {
    fn new(index: SpecIndex) -> Self {
        Self {
            index,
            runner: FakeGeneratorRunner::new(),
            env: HashMap::from([
                ("PATH".to_string(), String::new()),
                ("DOCKER_HOST".to_string(), "unix:///env.sock".to_string()),
            ]),
            aliases: HashMap::from([("gco".to_string(), "git checkout".to_string())]),
        }
    }

    fn explain_at(&self, line: &str, cursor: usize, usage: &dyn UsageLookup) -> Explanation {
        let cache = GeneratorCache::new();
        let wrappers = vec!["sudo".to_string(), "env".to_string()];
        let req = CompletionRequest {
            line,
            cursor,
            cwd: Path::new("/"),
            env: &self.env,
            aliases: &self.aliases,
            wrappers: &wrappers,
            index: &self.index,
            runner: &self.runner,
            cache: &cache,
            usage,
            timeout: Duration::from_secs(1),
            now_ts: 1_700_000_000,
            allow_stale: false,
        };
        explain_line(&req, &complete_line(&req), false)
    }

    fn explain(&self, line: &str) -> Explanation {
        self.explain_at(line, line.chars().count(), &EmptyUsageLookup)
    }
}

fn token<'e>(e: &'e Explanation, text: &str) -> &'e TokenExplain {
    e.commands
        .iter()
        .flat_map(|c| &c.tokens)
        .find(|t| t.text == text)
        .unwrap_or_else(|| panic!("no token {text:?} in {e:#?}"))
}

#[test]
fn explain_assigns_roles_to_every_word() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));
    let e = f.explain("sudo FOO=1 git co -b feat main --bogus ");

    assert_eq!(e.commands.len(), 1);
    let cmd = &e.commands[0];
    assert_eq!(cmd.path, ["git", "checkout"]);
    assert_eq!(cmd.description.as_deref(), Some("Switch branches"));

    let roles: Vec<(&str, &str)> = cmd.tokens.iter().map(|t| (t.text.as_str(), t.role.as_str())).collect();
    assert_eq!(
        roles,
        [
            ("sudo", "wrapper"),
            ("FOO=1", "assignment"),
            ("git", "command"),
            ("co", "subcommand"),
            ("-b", "option"),
            ("feat", "value"),
            ("main", "argument"),
            ("--bogus", "unknown"),
        ]
    );
    assert!(token(&e, "co").detail.contains("alias of checkout"), "{e:#?}");
    assert!(token(&e, "-b").detail.contains("Create a new branch"));
    assert_eq!(token(&e, "feat").detail, "value of -b <branch>");
    assert_eq!(token(&e, "main").detail, "[branch]");
    assert_eq!(token(&e, "--bogus").start, 31);
    assert!(
        e.diagnostics.iter().any(|d| d.message.contains("--bogus")),
        "pre-flight diagnostics are included: {:#?}",
        e.diagnostics
    );
}

#[test]
fn explain_reports_alias_expansion() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));
    let e = f.explain("gco --force ");
    let cmd = &e.commands[0];
    assert_eq!(cmd.path, ["git", "checkout"]);
    assert_eq!(cmd.notes, ["alias gco → git checkout"]);
    assert_eq!(token(&e, "--force").role, "option");
}

#[test]
fn explain_splits_nested_command_lines() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));
    let e = f.explain("timeout 5 git checkout -b x ");
    assert_eq!(e.commands.len(), 2, "{e:#?}");
    assert_eq!(e.commands[0].path, ["timeout"]);
    assert_eq!(e.commands[1].path, ["git", "checkout"]);
    let outer: Vec<&str> = e.commands[0].tokens.iter().map(|t| t.role.as_str()).collect();
    assert_eq!(outer, ["command", "argument", "nested"]);
    assert!(e.commands[0].tokens[2].detail.contains("<command...>"));
    let inner: Vec<&str> = e.commands[1].tokens.iter().map(|t| t.role.as_str()).collect();
    assert_eq!(inner, ["command", "subcommand", "option", "value"]);
}

#[test]
fn explain_shows_when_clauses_and_parameter_bindings() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));

    let e = f.explain("docker rm web ");
    let b = &token(&e, "web").bindings;
    assert_eq!(b[0], "type container with host = unix:///env.sock ← env.DOCKER_HOST (default)", "{b:#?}");
    assert!(b.iter().any(|l| l == "when --force: container (inactive, requires --force)"), "{b:#?}");

    let e = f.explain("docker -H tcp://h rm --force web ");
    let b = &token(&e, "web").bindings;
    assert!(b[0].contains("host = tcp://h ← opt.host (default)"), "{b:#?}");
    assert!(b.iter().any(|l| l == "when --force: container (active)"), "{b:#?}");

    let e = f.explain("docker rm --format=json ");
    assert!(token(&e, "--format=json").bindings.iter().any(|l| l == "values json | table"));
}

#[test]
fn explain_completion_slot_and_learned_provenance() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));
    let line = "git checkout ";
    let e = f.explain_at(line, line.len(), &LearnedUsage);
    assert_eq!(e.completion.slot, "argument [branch]");
    assert!(
        e.completion.sources.iter().any(|s| s.source == "learned history" && s.count == 1),
        "{:#?}",
        e.completion.sources
    );

    let e = f.explain("git checkout -");
    assert_eq!(e.completion.slot, "option");
    assert_eq!(e.completion.prefix, "-");
    // Untiered index (`from_single`): spec candidates are labeled `spec`.
    assert!(e.completion.sources.iter().all(|s| s.source == "spec"), "{:#?}", e.completion.sources);

    let e = f.explain("frobnicate ");
    assert!(e.completion.native_fallback);
    assert!(!e.commands[0].known);
}

#[test]
fn explain_uses_spec_tier_provenance() {
    let user = spec("cmd git checkout\n  opt --guess \"Guess remote branch\"\n");
    let bundled = spec(SPEC);
    let user_tier = [("user/git.ezlo".to_string(), user)];
    let bundled_tier = [("bundled:git.ezlo".to_string(), bundled)];
    let tiers: [&[(String, Spec)]; 3] = [&user_tier, &bundled_tier, &[]];
    let mut diags = Vec::new();
    let index = SpecIndex::merge_tiers(&tiers, &mut diags);
    let f = Fixture::new(index);

    let e = f.explain("git checkout --");
    let origins: Vec<(&str, &str)> =
        e.commands[0].origins.iter().map(|o| (o.tier.as_str(), o.source.as_str())).collect();
    assert_eq!(origins, [("user spec", "user/git.ezlo"), ("bundled spec", "bundled:git.ezlo")]);
    assert_eq!(e.commands[1..].len(), 0);
    let sources: Vec<&str> = e.completion.sources.iter().map(|s| s.source.as_str()).collect();
    assert_eq!(sources, ["user spec"], "candidates of an overlaid cmd come from its top tier");

    let e = f.explain("docker rm -");
    assert_eq!(e.commands[0].origins[0].tier, "bundled spec");
    assert!(e.completion.sources.iter().all(|s| s.source == "bundled spec"));
}

#[test]
fn explain_targets_the_segment_under_the_cursor() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));
    let line = "git checkout main | timeout 5";
    let e = f.explain_at(line, 4, &EmptyUsageLookup);
    assert_eq!(e.segment, (0, 18));
    assert_eq!(e.commands.len(), 1);
    assert_eq!(e.commands[0].path, ["git", "checkout"]);
    assert_eq!(token(&e, "main").role, "argument");

    let e = f.explain(line);
    assert_eq!(e.commands[0].path, ["timeout"]);
}

#[test]
fn explain_render_lines() {
    let f = Fixture::new(SpecIndex::from_single(spec(SPEC)));
    let lines = f.explain("git co --bogus").render();
    assert_eq!(lines[0].style, ExplainStyle::Header);
    assert_eq!(lines[0].text, "git checkout — Switch branches");
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.starts_with("  co ") && t.contains("subcommand")), "{texts:#?}");
    assert!(lines.iter().any(|l| l.style == ExplainStyle::Warning && l.text.contains("--bogus")), "{texts:#?}");
    let last = lines.last().expect("completion line");
    assert_eq!(last.style, ExplainStyle::Note);
    assert!(last.text.starts_with("▸ completing option from `--bogus`"), "{texts:#?}");
}
