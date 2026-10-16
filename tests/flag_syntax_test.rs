//! Flag forms: short `-x` (clusterable), long `--name` (incl. `_`), single-dash long `-name`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest};
use ezlo::spec::span::RuleCode;
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
type target
  $ list_targets
  cols label
  insert label

type everything
  $ list_everything
  cols label
  insert label

cmd bazel test
  opt --test_output <mode> = errors | streamed "Test output mode"
  opt -k --keep_going "Keep going"
  arg <target...> target
    when --keep_going: everything

cmd find
  opt -L "Follow symlinks"
  opt -l "Short flag sharing a prefix with -links"
  opt -a "Short flag"
  opt -n <count> "Short flag with a value, sharing a prefix with -name"
  opt -name <pattern> = "*.txt" "Base name glob"
  opt -links <n> "Number of hard links"
  opt -type <kind> = f | d "File type"
  opt -empty "Empty files and directories"
  arg [path...] target
    when -empty: everything
"#;

fn index() -> SpecIndex {
    let (spec, diags) = parse_and_validate(SPEC);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

fn complete(index: &SpecIndex, line: &str) -> Vec<String> {
    let runner = FakeGeneratorRunner::new();
    runner.set_response("list_targets", vec!["//a:t".to_string()]);
    runner.set_response("list_everything", vec!["//a:all".to_string()]);
    let cache = GeneratorCache::new();
    let env = HashMap::new();
    let aliases = HashMap::new();
    let res = complete_line(&CompletionRequest {
        line,
        cursor: line.chars().count(),
        cwd: Path::new("/tmp"),
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
    });
    res.candidates.into_iter().map(|c| c.insert_value).collect()
}

fn has(values: &[String], want: &str) -> bool {
    values.iter().any(|v| v == want)
}

#[test]
fn flag_forms_parse_with_canonical_names() {
    let idx = index();
    let opt = |path: &str, flag: &str| {
        idx.cmds
            .iter()
            .filter(|c| c.path.join(" ") == path)
            .flat_map(|c| &c.opts)
            .find(|o| o.flags.iter().any(|f| f == flag))
            .map(|o| o.name.clone())
    };
    assert_eq!(opt("bazel test", "--test_output").as_deref(), Some("test_output"));
    assert_eq!(opt("find", "-name").as_deref(), Some("name"));
    assert_eq!(opt("find", "-n").as_deref(), Some("n"));
}

#[test]
fn malformed_flags_are_rejected() {
    for flag in ["--_x", "--", "-", "-_x", "--a.b", "-a.b", "---x"] {
        let src = format!("cmd t\n  opt {flag}\n");
        let (_, diags) = parse_and_validate(&src);
        assert!(
            diags.iter().any(|d| d.code == RuleCode::E02Structure),
            "expected {flag} to be rejected: {diags:#?}"
        );
    }
}

#[test]
fn underscore_flags_complete_and_take_values() {
    let idx = index();
    let flags = complete(&idx, "bazel test --test_o");
    assert!(has(&flags, "--test_output"), "{flags:?}");

    let values = complete(&idx, "bazel test --test_output=");
    assert!(values.iter().any(|v| v.ends_with("streamed")), "{values:?}");

    let args = complete(&idx, "bazel test --test_output=errors ");
    assert!(has(&args, "//a:t"), "{args:?}");

    let when = complete(&idx, "bazel test --keep_going ");
    assert!(has(&when, "//a:all"), "{when:?}");
}

#[test]
fn single_dash_long_flag_prefix_completes_flags_not_a_cluster() {
    let idx = index();
    // `-na` would be `-n` with attached value `a` as a cluster; `-name` must win.
    let flags = complete(&idx, "find -na");
    assert!(has(&flags, "-name"), "{flags:?}");
    // `-li` is a prefix of `-links`, not `-l` followed by an unknown `-i`.
    let flags = complete(&idx, "find -li");
    assert!(has(&flags, "-links"), "{flags:?}");
    // A bare dash offers single-dash long flags alongside the others.
    let flags = complete(&idx, "find -");
    assert!(has(&flags, "-type") && has(&flags, "-L"), "{flags:?}");
}

#[test]
fn single_dash_long_flags_take_values_and_count_as_present() {
    let idx = index();
    let values = complete(&idx, "find -type ");
    assert!(has(&values, "f") && has(&values, "d"), "{values:?}");

    let values = complete(&idx, "find -name ");
    assert!(has(&values, "*.txt"), "{values:?}");

    // `-name` consumed its value, so the next word is positional again.
    let args = complete(&idx, "find -name x ");
    assert!(has(&args, "//a:t"), "{args:?}");

    // Valueless single-dash long flag drives `when -empty:`.
    let args = complete(&idx, "find -empty ");
    assert!(has(&args, "//a:all"), "{args:?}");

    // Already-used flags aren't offered again.
    let flags = complete(&idx, "find -empty -");
    assert!(!has(&flags, "-empty"), "{flags:?}");
}

#[test]
fn short_clusters_still_work_without_a_matching_long_flag() {
    let idx = index();
    // No flag is spelled `-la`: it's the `-l` + `-a` cluster, so both count as used.
    let flags = complete(&idx, "find -la -");
    assert!(!has(&flags, "-a") && !has(&flags, "-l"), "{flags:?}");
    assert!(has(&flags, "-links") && has(&flags, "-L"), "{flags:?}");
    // `-n5` is `-n` with an attached value, so the next word is positional.
    let args = complete(&idx, "find -n5 ");
    assert!(has(&args, "//a:t"), "{args:?}");
}

#[test]
fn exact_single_dash_long_flag_beats_optional_short_value() {
    // `-color` is its own flag, not `-c` with the attached optional value `olor`.
    let (spec, diags) = parse_and_validate(
        "type target\n  $ list_targets\n  cols label\n  insert label\n\ncmd ls\n  opt -c [when] = auto | never \"Short flag with an optional value\"\n  opt -color <when> = always | auto \"Single-dash long flag sharing the -c prefix\"\n  opt -l \"Long listing\"\n  arg [path...] target\n",
    );
    assert!(diags.is_empty(), "{diags:#?}");
    let idx = SpecIndex::from_single(spec);

    let values = complete(&idx, "ls -color ");
    assert!(has(&values, "always") && !has(&values, "//a:t"), "{values:?}");
    let args = complete(&idx, "ls -color always ");
    assert!(has(&args, "//a:t"), "{args:?}");
    // `-c` alone takes no detached value; `-cauto` and `-lc` are clusters.
    for line in ["ls -c ", "ls -cauto ", "ls -lc ", "ls -lcnever "] {
        let args = complete(&idx, line);
        assert!(has(&args, "//a:t") && !has(&args, "always"), "{line:?}: {args:?}");
    }
    let values = complete(&idx, "ls -cn");
    assert_eq!(values.first().map(String::as_str), Some("never"), "attached optional value under the cursor");
}
