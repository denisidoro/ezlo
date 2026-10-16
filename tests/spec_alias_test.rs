//! Top-level spec `alias` declarations: `alias gco = git checkout` makes `gco` complete, validate
//! and explain as `git checkout`, taking precedence over a shell alias of the same name.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::span::{Diagnostic, RuleCode};
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
alias gco = git checkout
alias gcb = git checkout -b
alias j = __jump
alias gfmt = git log "--format=%h %s"

cmd git "Version control"

cmd git checkout "Switch branches"
  opt -b <branch> "Create a new branch"
  opt --force "Discard local changes"
  arg [branch] = main | dev

cmd git log "Show commit logs"
  opt --oneline "One line per commit"
  opt --format <format> "Pretty format"

cmd __jump "Jump to a directory"
  arg [keywords...] = dotfiles | downloads
"#;

fn make_index(src: &str) -> SpecIndex {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

fn diags(src: &str) -> Vec<Diagnostic> {
    parse_and_validate(src).1
}

fn complete(index: &SpecIndex, aliases: &HashMap<String, String>, line: &str) -> CompletionResult {
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let env = HashMap::from([("PATH".to_string(), String::new())]);
    complete_line(&CompletionRequest {
        line,
        cursor: line.chars().count(),
        cwd: Path::new("/"),
        env: &env,
        aliases,
        wrappers: &["sudo".to_string()],
        index,
        runner: &runner,
        cache: &cache,
        usage: &EmptyUsageLookup,
        timeout: Duration::from_secs(1),
        now_ts: 1_700_000_000,
        allow_stale: false,
    })
}

fn has(res: &CompletionResult, value: &str) -> bool {
    res.candidates.iter().any(|c| c.insert_value == value)
}

fn values(res: &CompletionResult) -> Vec<&str> {
    res.candidates
        .iter()
        .map(|c| c.insert_value.as_str())
        .collect()
}

#[test]
fn parses_top_level_aliases() {
    let index = make_index(SPEC);
    assert_eq!(index.aliases["gco"], ["git", "checkout"]);
    assert_eq!(index.aliases["gcb"], ["git", "checkout", "-b"]);
    assert_eq!(index.aliases["gfmt"], ["git", "log", "--format=%h %s"]);
}

#[test]
fn alias_completes_as_its_expansion() {
    let index = make_index(SPEC);
    let res = complete(&index, &HashMap::new(), "gco --");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
    assert!(has(&res, "--force"), "{:?}", values(&res));

    let res = complete(&index, &HashMap::new(), "gco ");
    assert!(has(&res, "main") && has(&res, "dev"), "{:?}", values(&res));
}

#[test]
fn alias_expansion_can_carry_options() {
    let index = make_index(SPEC);
    let res = complete(&index, &HashMap::new(), "gcb feature ");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
    assert!(
        has(&res, "main"),
        "-b consumed 'feature', next is the branch: {:?}",
        values(&res)
    );
}

#[test]
fn alias_to_a_function_name() {
    let index = make_index(SPEC);
    let res = complete(&index, &HashMap::new(), "j ");
    assert_eq!(res.cmd_path, ["__jump"]);
    assert!(has(&res, "dotfiles"), "{:?}", values(&res));
}

#[test]
fn alias_not_expanded_while_typing_the_word() {
    let index = make_index(SPEC);
    let res = complete(&index, &HashMap::new(), "gco");
    assert_ne!(res.cmd_path, ["git", "checkout"]);
}

#[test]
fn spec_alias_wins_over_shell_alias() {
    let index = make_index(SPEC);
    let shell = HashMap::from([("gco".to_string(), "dot git checkout".to_string())]);
    let res = complete(&index, &shell, "gco --");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
}

#[test]
fn shell_aliases_still_apply() {
    let index = make_index(SPEC);
    let shell = HashMap::from([("gl".to_string(), "git log".to_string())]);
    let res = complete(&index, &shell, "gl --");
    assert_eq!(res.cmd_path, ["git", "log"]);
    assert!(has(&res, "--oneline"), "{:?}", values(&res));
}

#[test]
fn higher_tier_alias_wins() {
    let (user, d1) = parse_and_validate("alias gco = git commit\n");
    let (bundled, d2) = parse_and_validate("alias gco = git checkout\nalias gst = git status\n");
    assert!(d1.is_empty() && d2.is_empty());
    let user_tier = [("user.ezlo".to_string(), user)];
    let bundled_tier = [("bundled.ezlo".to_string(), bundled)];
    let mut diags = Vec::new();
    let index = SpecIndex::merge_tiers(&[&user_tier, &bundled_tier], &mut diags);
    assert!(diags.is_empty(), "{diags:#?}");
    assert_eq!(index.aliases["gco"], ["git", "commit"]);
    assert_eq!(index.aliases["gst"], ["git", "status"]);
}

#[test]
fn duplicate_alias_in_tier_warns() {
    let (a, _) = parse_and_validate("alias gco = git checkout\n");
    let (b, _) = parse_and_validate("alias gco = git commit\n");
    let tier = [("a.ezlo".to_string(), a), ("b.ezlo".to_string(), b)];
    let mut diags = Vec::new();
    let index = SpecIndex::merge_tiers(&[&tier], &mut diags);
    assert_eq!(index.aliases["gco"], ["git", "checkout"]);
    assert!(
        diags
            .iter()
            .any(|(f, d)| f == "b.ezlo" && d.code == RuleCode::E04Duplicate),
        "{diags:#?}"
    );
}

#[test]
fn rejects_malformed_aliases() {
    for src in [
        "alias\n",
        "alias gco\n",
        "alias gco git checkout\n",
        "alias gco =\n",
        "alias -x = ls\n",
        "alias gco = git <x>\n",
    ] {
        let d = diags(src);
        assert!(
            d.iter().any(|d| d.code == RuleCode::E02Structure),
            "{src:?}: {d:#?}"
        );
    }
}

#[test]
fn rejects_duplicate_and_shadowing_aliases() {
    let d = diags("alias gco = git checkout\nalias gco = git commit\n");
    assert!(d.iter().any(|d| d.code == RuleCode::E04Duplicate), "{d:#?}");

    let d = diags("alias foo = ls\ncmd foo \"Foo\"\n");
    assert!(d.iter().any(|d| d.code == RuleCode::E04Duplicate), "{d:#?}");

    // An alias expanding to the root it shares a name with (`alias ls = ls -la`) is fine.
    assert!(diags("alias ls = ls -la\ncmd ls \"List\"\n").is_empty());
}
