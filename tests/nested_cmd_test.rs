//! Nested command-line delegation: words after a `cmd`-typed positional slot are completed and
//! validated against the nested command's grammar.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
cmd timeout "Run a command with a time limit"
  opt -s --signal <sig> = TERM | KILL "Signal to send on timeout"
  opt -k --kill-after <duration> "Also send KILL after this long"
  opt --preserve-status "Exit with the command's status"
  arg <duration> = 1 | 5 | 10
  arg <command...> cmd

cmd watch "Execute a program periodically"
  opt -n <interval> = 1 | 2 | 5 "Seconds between updates"
  arg <cmd...>

cmd git "Version control"

cmd git checkout "Switch branches"
  opt -b <branch> "Create a new branch"
  opt --force "Discard local changes"
  arg [branch]

cmd git commit "Record changes"
  opt -m --message <msg> "Commit message"
  opt --amend "Amend the previous commit"
"#;

fn make_index(src: &str) -> SpecIndex {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

struct Fixture {
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    env: HashMap<String, String>,
    aliases: HashMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            index: make_index(SPEC),
            runner: FakeGeneratorRunner::new(),
            env: HashMap::from([("PATH".to_string(), String::new())]),
            aliases: HashMap::from([("g".to_string(), "git".to_string())]),
        }
    }

    fn complete(&self, line: &str) -> CompletionResult {
        let cache = GeneratorCache::new();
        let wrappers = vec!["sudo".to_string(), "env".to_string()];
        complete_line(&CompletionRequest {
            line,
            cursor: line.chars().count(),
            cwd: Path::new("/"),
            env: &self.env,
            aliases: &self.aliases,
            wrappers: &wrappers,
            index: &self.index,
            runner: &self.runner,
            cache: &cache,
            usage: &EmptyUsageLookup,
            timeout: Duration::from_secs(1),
            now_ts: 1_700_000_000,
            allow_stale: false,
        })
    }
}

fn values(res: &CompletionResult) -> Vec<&str> {
    res.candidates.iter().map(|c| c.insert_value.as_str()).collect()
}

fn has(res: &CompletionResult, value: &str) -> bool {
    res.candidates.iter().any(|c| c.insert_value == value)
}

#[test]
fn nested_command_gets_subcommands() {
    let fx = Fixture::new();
    let res = fx.complete("timeout 5 git ");
    assert_eq!(res.cmd_path, ["git"]);
    assert!(res.known_root);
    assert!(has(&res, "checkout") && has(&res, "commit"), "{:?}", values(&res));
    assert!(!has(&res, "--signal"), "outer options must not leak: {:?}", values(&res));
}

#[test]
fn nested_command_gets_its_own_options() {
    let fx = Fixture::new();
    let res = fx.complete("timeout -s KILL 5 git checkout --");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
    assert_eq!(res.usage_context, "git checkout");
    assert!(has(&res, "--force"), "{:?}", values(&res));
    assert!(!has(&res, "--preserve-status"), "{:?}", values(&res));
}

#[test]
fn nested_option_value_slot() {
    let fx = Fixture::new();
    let res = fx.complete("timeout 5 git commit -m ");
    assert_eq!(res.usage_context, "git commit -m");
    assert!(res.candidates.iter().all(|c| !c.insert_value.starts_with('-')), "{:?}", values(&res));
}

#[test]
fn cursor_on_cmd_slot_offers_commands() {
    let fx = Fixture::new();
    let res = fx.complete("timeout 5 ");
    assert_eq!(res.cmd_path, ["timeout"]);
    assert!(has(&res, "g"), "aliases are offered as commands: {:?}", values(&res));
}

#[test]
fn outer_options_still_complete_before_cmd_slot() {
    let fx = Fixture::new();
    let res = fx.complete("timeout --");
    assert!(has(&res, "--signal") && has(&res, "--preserve-status"), "{:?}", values(&res));
}

#[test]
fn delegation_is_recursive_and_skips_wrappers() {
    let fx = Fixture::new();
    for line in [
        "timeout 5 timeout 10 git ",
        "timeout 5 watch -n 2 git ",
        "timeout 5 sudo git ",
        "sudo timeout 5 env FOO=1 git ",
    ] {
        let res = fx.complete(line);
        assert_eq!(res.cmd_path, ["git"], "{line}");
        assert!(has(&res, "checkout"), "{line}: {:?}", values(&res));
    }
}

#[test]
fn untyped_cmd_named_slot_delegates() {
    let fx = Fixture::new();
    let res = fx.complete("watch git checkout --f");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
    assert!(has(&res, "--force"), "{:?}", values(&res));
}

#[test]
fn nested_alias_is_expanded() {
    let fx = Fixture::new();
    let res = fx.complete("timeout 5 g ch");
    assert_eq!(res.cmd_path, ["git"]);
    assert!(has(&res, "checkout"), "{:?}", values(&res));
}

#[test]
fn unknown_nested_command_is_not_owned_by_outer_spec() {
    let fx = Fixture::new();
    let res = fx.complete("timeout 5 frobnicate --");
    assert_eq!(res.cmd_path, ["frobnicate"]);
    assert!(!res.known_root);
    assert!(!has(&res, "--signal"), "{:?}", values(&res));
}

#[test]
fn diagnostics_cover_outer_and_nested_levels() {
    let fx = Fixture::new();
    let line = "timeout --bogus 5 git checkout --nope --force x ";
    let res = fx.complete(line);
    let msgs: Vec<&str> = res.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(msgs.contains(&"unknown option '--bogus'"), "{msgs:?}");
    assert!(msgs.contains(&"unknown option '--nope'"), "{msgs:?}");
    assert!(!msgs.iter().any(|m| m.contains("--force")), "nested flags are valid: {msgs:?}");
    let nope = res.diagnostics.iter().find(|d| d.message.contains("--nope")).unwrap();
    let start = line.find("--nope").unwrap();
    assert_eq!((nope.char_start, nope.char_end), (start, start + "--nope".len()));
}

#[test]
fn nested_words_are_not_validated_against_outer_grammar() {
    let fx = Fixture::new();
    let res = fx.complete("timeout 5 git commit --amend -m msg ");
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    let res = fx.complete("timeout 5 ls -la ");
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
}
