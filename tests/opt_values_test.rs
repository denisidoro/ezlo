//! Optional option values (`opt --color [when]`, `greedy`) and multi-value options
//! (`opt --arg <name> <value>`): grammar, walker, completion, diagnostics, explain and previews.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::candidate::Candidate;
use ezlo::engine::explain::{explain_line, Explanation, TokenExplain};
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::preview::{preview_line, PreviewOptions};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::span::RuleCode;
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
cmd jq "JSON processor"
  opt -r --raw-output "Raw strings"
  opt --arg <name> <value> "Bind a string variable"
  opt --slurpfile <name> <file> file "Bind a variable to a file's JSON values"
  opt --color [when] = auto | always | never "Colorize output"
  arg <filter> = . | ".[]" "jq filter"
  arg [files...] file
  preview $ echo {opt.arg} {arg.filter}

cmd journalctl "Query the journal"
  opt -b --boot [boot] = 0 | -1 | -2 "Messages from a boot" greedy
  opt -f --follow "Follow"
  arg [match...] = "_PID=" | "_COMM="

cmd sed "Stream editor"
  opt -i --in-place [suffix] = .bak "Edit files in place"
  opt -n --quiet "Quiet"
  arg <script>
  arg [files...] file
"#;

fn index() -> SpecIndex {
    let (spec, diags) = parse_and_validate(SPEC);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

struct Fixture {
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    cache: GeneratorCache,
    env: HashMap<String, String>,
    aliases: HashMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            index: index(),
            runner: FakeGeneratorRunner::new(),
            cache: GeneratorCache::new(),
            env: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    fn req<'a>(&'a self, line: &'a str, cursor: usize, cwd: &'a Path) -> CompletionRequest<'a> {
        CompletionRequest {
            line,
            cursor,
            cwd,
            env: &self.env,
            aliases: &self.aliases,
            wrappers: &[],
            index: &self.index,
            runner: &self.runner,
            cache: &self.cache,
            usage: &EmptyUsageLookup,
            timeout: Duration::from_secs(1),
            now_ts: 1_700_000_000,
            allow_stale: false,
        }
    }

    fn complete_at(&self, line: &str, cursor: usize, cwd: &Path) -> CompletionResult {
        complete_line(&self.req(line, cursor, cwd))
    }

    fn complete(&self, line: &str) -> CompletionResult {
        self.complete_at(line, line.chars().count(), Path::new("/nonexistent"))
    }

    fn values(&self, line: &str) -> Vec<String> {
        self.complete(line).candidates.into_iter().map(|c| c.insert_value).collect()
    }

    fn explain(&self, line: &str) -> Explanation {
        let req = self.req(line, line.chars().count(), Path::new("/nonexistent"));
        explain_line(&req, &complete_line(&req), false)
    }
}

fn has(values: &[String], want: &str) -> bool {
    values.iter().any(|v| v == want)
}

fn token<'e>(e: &'e Explanation, text: &str) -> &'e TokenExplain {
    e.commands
        .iter()
        .flat_map(|c| &c.tokens)
        .find(|t| t.text == text)
        .unwrap_or_else(|| panic!("no token {text:?} in {e:#?}"))
}

fn find<'c>(cands: &'c [Candidate], insert: &str) -> &'c Candidate {
    cands
        .iter()
        .find(|c| c.insert_value == insert)
        .unwrap_or_else(|| panic!("no candidate {insert:?} in {:?}", cands.iter().map(|c| &c.insert_value).collect::<Vec<_>>()))
}

#[test]
fn grammar_parses_optional_greedy_and_multi_value_options() {
    let idx = index();
    let opt = |cmd: &str, flag: &str| {
        idx.cmds
            .iter()
            .filter(|c| c.path.join(" ") == cmd)
            .flat_map(|c| &c.opts)
            .find(|o| o.flags.iter().any(|f| f == flag))
            .unwrap_or_else(|| panic!("no {flag} in {cmd}"))
    };

    let arg = opt("jq", "--arg");
    assert_eq!(arg.value.as_ref().map(|v| v.name.as_str()), Some("name"));
    assert_eq!(arg.extra_values.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["value"]);
    assert_eq!(arg.detached_value_count(), 2);
    assert_eq!(arg.value_placeholders(), "<name> <value>");

    let slurp = opt("jq", "--slurpfile");
    assert_eq!(slurp.value_at(0).and_then(|v| v.type_expr.as_deref()), None);
    assert_eq!(slurp.value_at(1).and_then(|v| v.type_expr.as_deref()).map(|t| t.type_ref.name.as_str()), Some("file"));

    let color = opt("jq", "--color");
    assert!(color.optional_value() && !color.greedy);
    assert_eq!(color.detached_value_count(), 0);
    assert_eq!(color.value_placeholders(), "[when]");

    let boot = opt("journalctl", "-b");
    assert!(boot.optional_value() && boot.greedy);
    assert!(boot.takes_detached_value("-1") && boot.takes_detached_value("12") && boot.takes_detached_value("0"));
    assert!(!boot.takes_detached_value("_PID=1") && !boot.takes_detached_value("-f"));

    // `ezlo parse` JSON: new fields only when set, so existing output is unchanged.
    let json = serde_json::to_value(&idx.cmds[0]).expect("json");
    let opts = json["opts"].as_array().expect("opts");
    let by_name = |n: &str| opts.iter().find(|o| o["name"] == n).expect("opt");
    assert_eq!(by_name("arg")["extra_values"][0]["name"], "value");
    assert_eq!(by_name("color")["value"]["optional"], true);
    assert!(by_name("raw-output").get("extra_values").is_none());
    assert!(by_name("arg")["value"].get("optional").is_none());
    let boot_json = serde_json::to_value(boot).expect("json");
    assert_eq!(boot_json["greedy"], true);
}

#[test]
fn grammar_rejects_misplaced_optional_variadic_and_greedy() {
    let cases = [
        ("cmd t\n  opt -x [a] <b>\n", RuleCode::E02Structure, "optional value"),
        ("cmd t\n  opt -x <a> [b]\n", RuleCode::E02Structure, "optional value"),
        ("cmd t\n  opt -x <a...> <b>\n", RuleCode::E02Structure, "only the last value"),
        ("cmd t\n  opt -x <a> \"d\" greedy\n", RuleCode::E02Structure, "'greedy' requires"),
        ("cmd t\n  opt -x \"d\" greedy\n", RuleCode::E02Structure, "'greedy' requires"),
        ("cmd t\n  opt -x <a> <b> nosuchtype\n", RuleCode::E05Unresolved, "nosuchtype"),
    ];
    for (src, code, msg) in cases {
        let (_, diags) = parse_and_validate(src);
        assert!(
            diags.iter().any(|d| d.code == code && d.message.contains(msg)),
            "{src:?}: expected {code:?} containing {msg:?}, got {diags:#?}"
        );
    }
    // Modifiers right after a value aren't taken as its type.
    for src in ["cmd t\n  opt -x [a] greedy\n", "cmd t\n  opt -x <a> hidden\n", "cmd t\n  opt -x <a> <b...> global\n"] {
        let (_, diags) = parse_and_validate(src);
        assert!(diags.is_empty(), "{src:?}: {diags:#?}");
    }
}

#[test]
fn optional_long_value_is_only_taken_attached() {
    let fx = Fixture::new();

    // Both spellings are offered; `--color=` keeps the cursor on the word for the value.
    let res = fx.complete("jq --col");
    let plain = find(&res.candidates, "--color");
    assert_eq!(plain.suffix, " ");
    let attached = find(&res.candidates, "--color=");
    assert_eq!(attached.suffix, "");
    // Mandatory-value options keep a single spelling.
    assert!(!has(&fx.values("jq --ar"), "--arg="));

    let vals = fx.values("jq --color=");
    for v in ["auto", "always", "never"] {
        assert!(has(&vals, v), "{v} in {vals:?}");
    }
    assert_eq!(fx.values("jq --color=al").first().map(String::as_str), Some("always"));

    // A detached word is not the value: it's the filter.
    let vals = fx.values("jq --color ");
    assert!(has(&vals, "."), "filter offered after --color: {vals:?}");
    assert!(!has(&vals, "always"), "{vals:?}");
    assert!(!has(&fx.values("jq --color al"), "always"));
    let e = fx.explain("jq --color always .");
    assert_eq!(token(&e, "always").role, "argument");
    assert_eq!(token(&e, ".").role, "argument");
    assert!(e.diagnostics.is_empty(), "{:?}", e.diagnostics);
    let e = fx.explain("jq --color=always .");
    assert!(token(&e, "--color=always").bindings.iter().any(|b| b.starts_with("values auto | always")));
    assert!(token(&e, "--color=always").detail.contains("--color [when]"));
}

#[test]
fn optional_short_value_must_be_attached() {
    let fx = Fixture::new();
    let e = fx.explain("sed -i.bak s/a/b/ f");
    assert_eq!(token(&e, "-i.bak").role, "option");
    assert!(token(&e, "s/a/b/").detail.contains("<script>"));

    // `sed -i s/a/b/`: the script is the first positional, not the suffix.
    let e = fx.explain("sed -i s/a/b/ f");
    assert!(token(&e, "s/a/b/").detail.contains("<script>"), "{e:#?}");
    // Clustered: `-ni` ends with the optional-value flag, `-in` is `-i` with suffix `n`.
    let e = fx.explain("sed -ni s/a/b/");
    assert!(token(&e, "s/a/b/").detail.contains("<script>"));
    let e = fx.explain("sed -in s/a/b/");
    assert!(token(&e, "s/a/b/").detail.contains("<script>"));
    assert!(e.diagnostics.is_empty(), "{:?}", e.diagnostics);
    assert_eq!(fx.values("sed -i").first().map(String::as_str), Some("-i"));
}

#[test]
fn greedy_optional_value_takes_matching_next_word() {
    let fx = Fixture::new();

    // The next word may be the boot value or a match.
    let vals = fx.values("journalctl -b ");
    assert!(has(&vals, "0") && has(&vals, "-1"), "{vals:?}");
    assert!(has(&vals, "_PID="), "{vals:?}");

    // `-1` is taken as the value, not reported as an unknown option.
    let e = fx.explain("journalctl -b -1 -f _PID=1");
    assert_eq!(token(&e, "-1").role, "value");
    assert_eq!(token(&e, "-f").role, "option");
    assert_eq!(token(&e, "_PID=1").role, "argument");
    assert!(e.diagnostics.is_empty(), "{:?}", e.diagnostics);

    // A word that doesn't look like a boot value stays a positional.
    let e = fx.explain("journalctl -b _PID=1");
    assert_eq!(token(&e, "_PID=1").role, "argument");
    assert!(e.diagnostics.is_empty(), "{:?}", e.diagnostics);
    assert!(fx.complete("journalctl -b 5 ").diagnostics.is_empty());
    assert!(has(&fx.values("journalctl -b -1 "), "_PID="));
}

#[test]
fn multi_value_option_consumes_each_value_in_order() {
    let fx = Fixture::new();

    // Each value position is its own slot; the filter comes back after the last one.
    for line in ["jq --arg ", "jq --arg n ", "jq --arg=n ", "jq -r --arg n "] {
        let vals = fx.values(line);
        assert!(!has(&vals, "."), "{line:?} is still on a value: {vals:?}");
    }
    let vals = fx.values("jq --arg n v ");
    assert!(has(&vals, "."), "filter after both values: {vals:?}");
    let vals = fx.values("jq --arg=n v ");
    assert!(has(&vals, "."), "filter after --arg=n v: {vals:?}");

    // Per-value types: the second value of --slurpfile completes files.
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("data.json"), "{}").expect("write");
    let line = "jq --slurpfile n ";
    let res = fx.complete_at(line, line.len(), tmp.path());
    assert!(res.candidates.iter().any(|c| c.insert_value == "data.json"), "{:?}", res.candidates);
    let line = "jq --slurpfile ";
    let res = fx.complete_at(line, line.len(), tmp.path());
    assert!(!res.candidates.iter().any(|c| c.insert_value == "data.json"), "{:?}", res.candidates);

    // The signature hint stays hidden while on option values.
    assert!(fx.complete("jq --arg n ").signature.is_none());
    assert!(fx.complete("jq --arg n v ").signature.is_some());
}

#[test]
fn explain_binds_each_word_to_its_placeholder() {
    let fx = Fixture::new();
    let e = fx.explain("jq --arg n v .");
    assert!(token(&e, "--arg").detail.contains("--arg <name> <value>"), "{e:#?}");
    assert_eq!(token(&e, "n").role, "value");
    assert_eq!(token(&e, "n").detail, "value 1 of --arg <name> (<name> <value>)");
    assert_eq!(token(&e, "v").detail, "value 2 of --arg <value> (<name> <value>)");
    assert_eq!(token(&e, ".").role, "argument");
    assert!(e.diagnostics.is_empty(), "{:?}", e.diagnostics);

    let line = "jq --arg n ";
    let req = fx.req(line, line.len(), Path::new("/nonexistent"));
    let e = explain_line(&req, &complete_line(&req), false);
    assert_eq!(e.completion.slot, "value 2 of --arg <value> (<name> <value>)");
}

#[test]
fn diagnostics_flag_missing_values_only_when_not_typing_them() {
    let fx = Fixture::new();
    // Still typing at the end of the line: no error.
    for line in ["jq --arg ", "jq --arg n", "jq --arg n ", "jq --slurpfile n "] {
        let res = fx.complete(line);
        assert!(res.diagnostics.is_empty(), "{line:?}: {:?}", res.diagnostics);
    }
    // Cursor moved back before the end: the second value can't come anymore.
    let line = "jq --arg  n";
    let res = fx.complete_at(line, 9, Path::new("/nonexistent"));
    let missing: Vec<_> = res.diagnostics.iter().filter(|d| d.message.contains("missing")).collect();
    assert_eq!(missing.len(), 1, "{:?}", res.diagnostics);
    assert_eq!(missing[0].message, "option '--arg' is missing <value>");
    assert_eq!((missing[0].char_start, missing[0].char_end), (3, 8));

    // Per-value path checks: only the file slot is checked.
    let tmp = tempfile::tempdir().expect("tempdir");
    let line = "jq --slurpfile nosuch.json missing.json . ";
    let res = fx.complete_at(line, line.len(), tmp.path());
    let paths: Vec<_> = res.diagnostics.iter().map(|d| (d.char_start, d.message.as_str())).collect();
    assert_eq!(paths.len(), 1, "{paths:?}");
    assert_eq!(paths[0].0, 27, "{paths:?}");
}

#[test]
fn preview_expands_every_value_of_a_multi_value_option() {
    let fx = Fixture::new();
    let line = "jq --arg n 'a b' .";
    let req = fx.req(line, line.len(), Path::new("/"));
    preview_line(&req, &PreviewOptions::default());
    let calls = fx.runner.recorded_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].0.starts_with("echo n 'a b' ."), "{calls:?}");
}
