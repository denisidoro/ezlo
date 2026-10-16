//! Spec overlays across tiers, subcommand aliases and `hidden` options.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::ast::Spec;
use ezlo::spec::span::RuleCode;
use ezlo::spec::{has_errors, parse_and_validate, SpecIndex};

const BASE: &str = r#"
cmd git "Version control"
  opt -C <dir> "Run as if started in dir" global

cmd git (checkout|co) "Switch branches"
  opt -b <branch> "Create a new branch"
  opt -f --force "Discard local changes"
  opt --guess "Guess remote branch" hidden
  arg [branch] = main | dev

cmd git commit "Record changes"
  alias ci
  opt -m --message <msg> "Commit message"
  opt -v --verbose "Show diff" hidden

cmd (kubectl|k) "Kubernetes CLI"

cmd kubectl get "Display resources"
"#;

fn spec(src: &str) -> Spec {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    spec
}

fn complete(index: &SpecIndex, line: &str) -> CompletionResult {
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let env = HashMap::from([("PATH".to_string(), String::new())]);
    let aliases = HashMap::new();
    complete_line(&CompletionRequest {
        line,
        cursor: line.chars().count(),
        cwd: Path::new("/"),
        env: &env,
        aliases: &aliases,
        wrappers: &[],
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

fn base_index() -> SpecIndex {
    SpecIndex::from_single(spec(BASE))
}

#[test]
fn alias_syntax_parses_both_forms() {
    let s = spec(BASE);
    let co = s.cmds.iter().find(|c| c.path == ["git", "checkout"]).expect("checkout");
    assert_eq!(co.aliases, ["co"]);
    assert_eq!(co.desc.as_deref(), Some("Switch branches"));
    let ci = s.cmds.iter().find(|c| c.path == ["git", "commit"]).expect("commit");
    assert_eq!(ci.aliases, ["ci"]);
    let k = s.cmds.iter().find(|c| c.path == ["kubectl"]).expect("kubectl");
    assert_eq!(k.aliases, ["k"]);
    assert!(ci.opts.iter().any(|o| o.name == "verbose" && o.hidden));
    assert!(ci.opts.iter().any(|o| o.name == "message" && !o.hidden));
}

#[test]
fn malformed_alias_group_and_collisions_are_errors() {
    for src in ["cmd git (checkout|\n", "cmd git (checkout co)\n", "cmd git (co) extra\n"] {
        let (_, diags) = parse_and_validate(src);
        assert!(diags.iter().any(|d| d.code == RuleCode::E02Structure), "{src:?}: {diags:#?}");
    }
    let (_, diags) = parse_and_validate("cmd git\ncmd git commit\ncmd git checkout\n  alias commit\n");
    assert!(diags.iter().any(|d| d.code == RuleCode::E04Duplicate), "{diags:#?}");
    let (_, diags) = parse_and_validate("cmd git\ncmd git a\n  alias x\ncmd git b\n  alias x\n");
    assert!(has_errors(&diags), "{diags:#?}");
}

#[test]
fn alias_walks_like_canonical_subcommand() {
    let index = base_index();
    let res = complete(&index, "git co --f");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
    assert!(has(&res, "--force"));

    let res = complete(&index, "git co ");
    assert!(has(&res, "main") && has(&res, "dev"));

    let res = complete(&index, "k ");
    assert!(res.known_root);
    assert_eq!(res.cmd_path, ["kubectl"]);
    assert!(has(&res, "get"));
}

#[test]
fn aliases_are_offered_only_for_a_matching_prefix() {
    let index = base_index();
    let res = complete(&index, "git ");
    assert!(has(&res, "checkout") && has(&res, "commit"));
    assert!(!has(&res, "co") && !has(&res, "ci"), "{:?}", res.candidates);

    let res = complete(&index, "git c");
    assert!(has(&res, "checkout") && has(&res, "co") && has(&res, "ci"));
    let co = res.candidates.iter().find(|c| c.insert_value == "co").expect("co");
    assert!(co.description.contains("alias of checkout"), "{}", co.description);

    let res = complete(&index, "");
    assert!(has(&res, "kubectl") && !has(&res, "k"));
    let res = complete(&index, "k");
    assert!(has(&res, "k"));
}

#[test]
fn hidden_options_need_a_typed_prefix() {
    let index = base_index();
    for line in ["git commit ", "git commit -", "git commit --"] {
        let res = complete(&index, line);
        assert!(has(&res, "--message"), "{line:?}");
        assert!(!has(&res, "--verbose") && !has(&res, "-v"), "{line:?}: {:?}", res.candidates);
    }
    let res = complete(&index, "git commit --ver");
    assert!(has(&res, "--verbose"));
    let res = complete(&index, "git commit -v");
    assert!(has(&res, "-v"));
    let res = complete(&index, "git co --gu");
    assert!(has(&res, "--guess"));
}

#[test]
fn hidden_options_and_aliases_are_known_to_diagnostics() {
    let index = base_index();
    let res = complete(&index, "git ci --verbose -m x ");
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    let res = complete(&index, "git co --guess --bogus ");
    assert_eq!(res.diagnostics.len(), 1, "{:?}", res.diagnostics);
    assert!(res.diagnostics[0].message.contains("--bogus"));
}

#[test]
fn user_tier_overlays_bundled_grammar() {
    let user = [(
        "user/git.ezlo".to_string(),
        spec(
            r#"
cmd git checkout "Check out (user)"
  alias sw
  opt -f --force "Force it" hidden
  opt --track "Track upstream"
  arg [branch] = feature

cmd git wip "Commit everything as WIP"
"#,
        ),
    )];
    let bundled = [("bundled:git.ezlo".to_string(), spec(BASE))];
    let tiers: [&[(String, Spec)]; 2] = [&user, &bundled];
    let mut diags = Vec::new();
    let index = SpecIndex::merge_tiers(&tiers, &mut diags);
    assert!(diags.is_empty(), "{diags:#?}");

    let co = index.cmds.iter().find(|c| c.path == ["git", "checkout"]).expect("checkout");
    assert_eq!(co.desc.as_deref(), Some("Check out (user)"));
    assert_eq!(co.aliases, ["co", "sw"]);
    let names: Vec<&str> = co.opts.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["b", "force", "guess", "track"]);
    let force = co.opts.iter().find(|o| o.name == "force").expect("force");
    assert!(force.hidden);
    assert_eq!(force.desc.as_deref(), Some("Force it"));
    assert_eq!(co.args.len(), 1);
    assert_eq!(co.args[0].suggestions, ["feature"]);

    // Base-only paths survive; user-only paths are added.
    for path in [&["git"][..], &["git", "commit"], &["kubectl", "get"], &["git", "wip"]] {
        assert!(index.cmds.iter().any(|c| c.path == path), "{path:?}");
    }

    let res = complete(&index, "git sw ");
    assert!(has(&res, "feature") && !has(&res, "main"));
    assert!(has(&res, "--track") && !has(&res, "--force"));
    let res = complete(&index, "git co -C ");
    assert_eq!(res.cmd_path, ["git", "checkout"]);
    let res = complete(&index, "git ");
    assert!(has(&res, "wip") && has(&res, "commit"));
}

#[test]
fn overlay_dedupes_recipes_by_scope_and_template() {
    let recipe = "recipe \"Undo last commit\"\n  $ git reset HEAD~1\n";
    let user = [("u.ezlo".to_string(), spec(&format!("cmd git\n{recipe}")))];
    let bundled = [(
        "b.ezlo".to_string(),
        spec(&format!("cmd git\n{recipe}recipe \"Other\"\n  $ git status\n")),
    )];
    let tiers: [&[(String, Spec)]; 2] = [&user, &bundled];
    let mut diags = Vec::new();
    let index = SpecIndex::merge_tiers(&tiers, &mut diags);
    let templates: Vec<&str> = index.recipes.iter().map(|r| r.template.as_str()).collect();
    assert_eq!(templates, ["git reset HEAD~1", "git status"]);
}

#[test]
fn recipe_templates_may_use_aliases() {
    let src = format!("{BASE}\nrecipe \"New branch\"\n  $ git co -b <branch>\n");
    let (_, diags) = parse_and_validate(&src);
    assert!(diags.is_empty(), "{diags:#?}");
}
