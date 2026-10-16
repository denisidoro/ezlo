use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::ast::Composite;
use ezlo::spec::span::RuleCode;
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
type feature
  values serde derive full tokio

type ssh_option
  values "StrictHostKeyChecking=no" "StrictHostKeyChecking=yes" "ConnectTimeout=5" "IdentitiesOnly=yes"

type user
  values root admin

type host
  values alpha beta

type env_name
  values PATH HOME

cmd cargo
  opt --features <features> list(",") feature "Features to enable"
  opt -p --path <dirs> list(":") = src | tests | benches

cmd ssh
  opt -o <option> kv("=") ssh_option "Set a config option (Key=Value)"
  arg <destination> kv("@", host) user

cmd docker
  opt -e --env <assignment> kv("=", feature) env_name "Set an environment variable"

cmd list
  arg <items> list(",") = a | b | c
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
            env: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Completes with the cursor at `|` (or at the end without one).
    fn complete(&self, line: &str) -> CompletionResult {
        let cursor = line.find('|').map_or(line.chars().count(), |i| line[..i].chars().count());
        let line = line.replace('|', "");
        let cache = GeneratorCache::new();
        let wrappers: Vec<String> = Vec::new();
        complete_line(&CompletionRequest {
            line: &line,
            cursor,
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

fn inserts(res: &CompletionResult) -> Vec<&str> {
    res.candidates.iter().map(|c| c.insert_text.as_str()).collect()
}

fn sorted(mut v: Vec<&str>) -> Vec<&str> {
    v.sort_unstable();
    v
}

#[test]
fn parses_composite_modifiers() {
    let (spec, diags) = parse_and_validate(SPEC);
    assert!(diags.is_empty(), "{diags:#?}");
    let cargo = spec.cmds.iter().find(|c| c.path == ["cargo"]).unwrap();
    let vs = cargo.opts[0].value.as_ref().unwrap();
    assert_eq!(vs.composite.as_deref(), Some(&Composite::List { sep: ",".into() }));
    assert_eq!(vs.type_expr.as_deref().unwrap().type_ref.name, "feature");
    let ssh = spec.cmds.iter().find(|c| c.path == ["ssh"]).unwrap();
    match ssh.args[0].composite.as_deref() {
        Some(Composite::Kv { sep, value_type }) => {
            assert_eq!(sep, "@");
            assert_eq!(value_type.as_ref().unwrap().type_ref.name, "host");
        }
        other => panic!("unexpected composite {other:?}"),
    }
    assert_eq!(ssh.args[0].type_expr.as_deref().unwrap().type_ref.name, "user");
}

#[test]
fn types_named_like_modifiers_still_work() {
    let (spec, diags) = parse_and_validate("type list\n  values a\n\ncmd x\n  arg <l> list\n");
    assert!(diags.is_empty(), "{diags:#?}");
    assert!(spec.cmds[0].args[0].composite.is_none());
    assert_eq!(spec.cmds[0].args[0].type_expr.as_deref().unwrap().type_ref.name, "list");
}

#[test]
fn invalid_composite_modifiers_are_reported() {
    let (_, diags) = parse_and_validate("cmd x\n  arg <l> list(\"\") = a\n");
    assert!(diags.iter().any(|d| d.message.contains("separator")), "{diags:#?}");
    let (_, diags) = parse_and_validate("cmd x\n  arg <l> kv(\"=\", nope)\n");
    assert!(diags.iter().any(|d| d.code == RuleCode::E05Unresolved), "{diags:#?}");
    let (_, diags) = parse_and_validate("cmd x\n  arg <l> list(\",\" = a\n");
    assert!(diags.iter().any(|d| d.message.contains("expected ')'")), "{diags:#?}");
}

#[test]
fn list_completes_last_segment_and_skips_used_items() {
    let fx = Fixture::new();
    let line = "cargo --features serde,";
    let res = fx.complete(line);
    assert_eq!(res.replace_start, line.len());
    assert_eq!(res.replace_end, line.len());
    assert_eq!(sorted(inserts(&res)), ["derive", "full", "tokio"]);

    let res = fx.complete("cargo --features serde,derive,t");
    assert_eq!(inserts(&res), ["tokio"]);
    assert_eq!(res.replace_start, "cargo --features serde,derive,".len());
}

#[test]
fn list_works_with_attached_value_and_static_suggestions() {
    let fx = Fixture::new();
    let line = "cargo --path=src:";
    let res = fx.complete(line);
    assert_eq!(res.replace_start, line.len());
    assert_eq!(sorted(inserts(&res)), ["benches", "tests"]);
}

#[test]
fn list_middle_segment_replaces_only_that_item_without_space() {
    let fx = Fixture::new();
    let res = fx.complete("cargo --features serde,d|x,full");
    assert_eq!(res.replace_start, "cargo --features serde,".len());
    assert_eq!(res.replace_end, "cargo --features serde,dx".len());
    let derive = res.candidates.iter().find(|c| c.insert_value == "derive").unwrap();
    assert_eq!(derive.suffix, "");
    assert!(!inserts(&res).contains(&"full"));
}

#[test]
fn list_positional_past_separator_offers_only_items() {
    let fx = Fixture::new();
    let res = fx.complete("list a,");
    assert_eq!(sorted(inserts(&res)), ["b", "c"]);
    assert_eq!(res.replace_start, "list a,".len());
}

#[test]
fn kv_key_side_offers_unique_keys_with_separator() {
    let fx = Fixture::new();
    let res = fx.complete("ssh -o Str");
    let strict = res
        .candidates
        .iter()
        .find(|c| c.label == "StrictHostKeyChecking")
        .unwrap_or_else(|| panic!("{:?}", inserts(&res)));
    assert_eq!(strict.insert_text, "StrictHostKeyChecking=");
    assert_eq!(strict.suffix, "");
    assert_eq!(strict.description, "no | yes");
    assert_eq!(res.candidates.iter().filter(|c| c.label == "StrictHostKeyChecking").count(), 1);
    assert_eq!(res.replace_start, "ssh -o ".len());
}

#[test]
fn kv_value_side_preserves_key_prefix() {
    let fx = Fixture::new();
    let line = "ssh -o StrictHostKeyChecking=";
    let res = fx.complete(line);
    assert_eq!(res.replace_start, line.len());
    assert_eq!(res.replace_end, line.len());
    assert_eq!(sorted(inserts(&res)), ["no", "yes"]);

    let res = fx.complete("ssh -o ConnectTimeout=");
    assert_eq!(inserts(&res), ["5"]);
}

#[test]
fn kv_positional_with_value_type() {
    let fx = Fixture::new();
    let res = fx.complete("ssh ro");
    assert_eq!(inserts(&res), ["root@"]);
    assert_eq!(res.candidates[0].suffix, "");

    let line = "ssh root@";
    let res = fx.complete(line);
    assert_eq!(res.replace_start, line.len());
    assert_eq!(sorted(inserts(&res)), ["alpha", "beta"]);
}

#[test]
fn kv_option_with_value_type_and_key_only_rows() {
    let fx = Fixture::new();
    let res = fx.complete("docker -e P");
    assert_eq!(inserts(&res), ["PATH="]);
    let res = fx.complete("docker --env=HOME=to");
    assert_eq!(inserts(&res), ["tokio"]);
    assert_eq!(res.replace_start, "docker --env=HOME=".len());
}

#[test]
fn kv_retyping_key_keeps_existing_value() {
    let fx = Fixture::new();
    let res = fx.complete("ssh -o Con|X=5");
    assert_eq!(res.replace_start, "ssh -o ".len());
    assert_eq!(res.replace_end, "ssh -o ConX=".len());
    assert_eq!(inserts(&res).first(), Some(&"ConnectTimeout="));
}

#[test]
fn quoted_composite_words_fall_back_to_whole_word() {
    let fx = Fixture::new();
    let res = fx.complete("cargo --features 'serde,");
    assert_eq!(res.replace_start, "cargo --features ".len());
}

#[test]
fn ssh_spec_uses_kv_for_options() {
    // The ssh spec moved to the user tier (`$DOTFILES/ezlo/specs`); keep its `-o` shape here.
    let src = "cmd ssh \"OpenSSH client\"\n  opt -o <option> kv(\"=\") \"Set a config option (Key=Value)\"\n  arg [host]\n";
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "{diags:#?}");
    let ssh = spec.cmds.iter().find(|c| c.path == ["ssh"]).unwrap();
    let opt = ssh.opts.iter().find(|o| o.flags == ["-o"]).unwrap();
    assert!(matches!(
        opt.value.as_deref().unwrap().composite.as_deref(),
        Some(Composite::Kv { .. })
    ));
}
