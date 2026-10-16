//! Pipeline-aware completion: `jq` paths, `grep`/`rg` tokens and headers, `awk`/`cut` columns and
//! delimiters synthesized from a bounded in-process sample of the file feeding the stage (upstream
//! `cat`/`head`/`< f` stage, input redirection or the stage's own file operand). Upstream commands
//! are never executed.

use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::SpecIndex;

const JSON: &str = r#"{"users":[{"name":"alice","age":30,"tags":["admin"]},{"name":"bob","email":"b@x"}],"meta":{"total":2,"next page":null}}"#;
const CSV: &str = "name,age,city\nalice,30,Paris\nbob,25,Rome\n";
const LOG: &str = "ERROR disk full\nWARN disk slow\nERROR network down\nINFO started\n";

struct Fixture {
    dir: tempfile::TempDir,
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    env: HashMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("data.json"), JSON).unwrap();
        fs::write(dir.path().join("data.csv"), CSV).unwrap();
        fs::write(dir.path().join("app.log"), LOG).unwrap();
        Self {
            env: HashMap::from([("HOME".to_string(), dir.path().display().to_string())]),
            dir,
            index: SpecIndex::default(),
            runner: FakeGeneratorRunner::new(),
        }
    }

    fn complete(&self, line: &str) -> CompletionResult {
        self.complete_at(line, line.chars().count())
    }

    fn complete_at(&self, line: &str, cursor: usize) -> CompletionResult {
        let cache = GeneratorCache::new();
        let wrappers = vec!["sudo".to_string()];
        complete_line(&CompletionRequest {
            line,
            cursor,
            cwd: self.dir.path(),
            env: &self.env,
            aliases: &HashMap::new(),
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

fn labels(res: &CompletionResult) -> Vec<&str> {
    res.candidates.iter().map(|c| c.label.as_str()).collect()
}

#[test]
fn jq_after_cat_offers_nested_paths() {
    let fx = Fixture::new();
    let res = fx.complete("cat data.json | jq .");
    let got = labels(&res);
    for want in [".users", ".users[]", ".users[].name", ".users[].tags[]", ".users[].email", ".meta.total", r#".meta."next page""#] {
        assert!(got.contains(&want), "missing {want} in {got:?}");
    }
    assert!(res.known_root, "piped candidates are engine-owned (no native fallback)");
    let name = res.candidates.iter().find(|c| c.label == ".users[].name").unwrap();
    assert!(name.description.contains("string \"alice\""), "{}", name.description);
    assert!(name.description.contains("data.json"), "{}", name.description);

    let res = fx.complete("cat data.json | jq .me");
    assert_eq!(labels(&res)[0], ".meta");
    assert_eq!(res.replace_start, "cat data.json | jq ".len());
}

#[test]
fn jq_quoted_filter_completes_relative_to_piped_path() {
    let fx = Fixture::new();
    let res = fx.complete("cat data.json | jq '.users[] | .na");
    let first = &res.candidates[0];
    assert_eq!(first.label, ".name");
    assert_eq!(first.insert_value, ".users[] | .name");
    assert_eq!(first.insert_text, "'.users[] | .name'");
    assert!(!labels(&res).contains(&".users"), "context-relative paths only: {:?}", labels(&res));

    let res = fx.complete("cat data.json | jq '.users | map(.ag");
    assert_eq!(res.candidates[0].label, ".age");
}

#[test]
fn jq_input_from_redirect_operand_head_and_pass_through() {
    let fx = Fixture::new();
    for line in [
        "jq . < data.json",
        "jq . data.json",
        "head -n 3 data.json | jq .",
        "< data.json jq .",
        "cat data.json | sort | jq .",
        "sudo cat ~/data.json | jq -r .",
    ] {
        let cursor = line.find('.').map(|i| i + 1).unwrap();
        let cursor = if line.ends_with(" .") { line.len() } else { cursor };
        let res = fx.complete_at(line, cursor);
        assert!(labels(&res).contains(&".users[].name"), "{line}: {:?}", labels(&res));
    }
}

#[test]
fn never_inspects_opaque_or_non_regular_upstreams() {
    let fx = Fixture::new();
    for line in [
        "curl -s example.com/data.json | jq .",
        "cat data.json || jq .",
        "cat missing.json | jq .",
        "cat *.json | jq .",
        "cat $HOME/data.json | jq .",
        "jq -f prog.jq data.json",
    ] {
        let res = fx.complete(line);
        assert!(!labels(&res).iter().any(|l| l.starts_with(".users")), "{line}: {:?}", labels(&res));
    }

    // A FIFO would block a naive read forever: it must be skipped without opening it.
    let fifo = fx.dir.path().join("pipe.json");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
    let started = Instant::now();
    let res = fx.complete("cat pipe.json | jq .");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!labels(&res).iter().any(|l| l.starts_with(".users")));
}

#[test]
fn large_inputs_are_sampled_from_the_head() {
    let fx = Fixture::new();
    let line = "{\"id\":1,\"payload\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}\n";
    let big = line.repeat((ezlo::engine::pipeline::MAX_SAMPLE_BYTES as usize / line.len()) + 100);
    fs::write(fx.dir.path().join("big.jsonl"), big).unwrap();
    let res = fx.complete("cat big.jsonl | jq .");
    assert!(labels(&res).contains(&".payload"), "{:?}", labels(&res));
}

#[test]
fn grep_patterns_from_tokens_and_headers() {
    let fx = Fixture::new();
    let res = fx.complete("cat app.log | grep ");
    assert_eq!(labels(&res)[..2], ["ERROR", "disk"]);
    assert!(res.candidates[0].description.contains("2× in app.log"));

    let res = fx.complete("cat data.csv | rg -i ");
    assert_eq!(labels(&res)[..3], ["name", "age", "city"]);
    assert!(res.candidates[2].description.contains("column 3 header"));

    let res = fx.complete("grep -e net app.log");
    let res2 = fx.complete_at("grep -e net app.log", "grep -e net".len());
    assert!(labels(&res).iter().all(|l| *l != "network"), "file operand slot is not a pattern");
    assert_eq!(labels(&res2)[0], "network");
}

#[test]
fn awk_separators_and_column_programs() {
    let fx = Fixture::new();
    let res = fx.complete("head data.csv | awk -F ");
    assert_eq!(labels(&res)[0], ",");
    assert!(res.candidates[0].description.contains("comma: 3 columns (name, age, city)"));

    let res = fx.complete("cat data.csv | awk -F");
    assert_eq!(labels(&res)[0], "-F,");

    let res = fx.complete("cat data.csv | awk -F, '{print $");
    let cands: Vec<(&str, &str)> = res.candidates.iter().map(|c| (c.label.as_str(), c.insert_text.as_str())).collect();
    assert!(cands.contains(&("{print $2}", "'{print $2}'")), "{cands:?}");
    let age = res.candidates.iter().find(|c| c.label == "{print $2}").unwrap();
    assert_eq!(age.description, "column 2: header age");

    // Default awk splitting is whitespace.
    let res = fx.complete("cat app.log | awk ");
    let second = res.candidates.iter().find(|c| c.label == "{print $2}").unwrap();
    assert_eq!(second.description, "column 2: e.g. disk");
}

#[test]
fn cut_fields_and_delimiters() {
    let fx = Fixture::new();
    let res = fx.complete_at("cut -d, -f  data.csv", "cut -d, -f ".len());
    assert_eq!(labels(&res)[..3], ["1", "2", "3"]);
    assert_eq!(res.candidates[2].description, "column 3: header city");

    let res = fx.complete("cat data.csv | cut -d");
    assert_eq!(labels(&res)[0], "-d,");
}
