//! Live positional signature hint (`cp  <src>  [dest]`) for the slot under the cursor.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::signature::SignatureHint;
use ezlo::engine::{complete_line, CompletionRequest};
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
cmd cp "Copy"
  opt -f --force "Overwrite"
  opt -t --target <dir> "Target directory"
  arg <src> "Source"
  arg [dest] "Destination"

cmd git "Version control"

cmd git add "Stage files"
  arg <files...> "Files to stage"

cmd timeout "Run with a time limit"
  arg <duration> "Duration"
  arg <command...> cmd

cmd ls "List"

cmd tar "Archive"
  arg <archive> "Archive"
  arg <files...> "Files"
"#;

fn index() -> SpecIndex {
    let (spec, diags) = parse_and_validate(SPEC);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

fn signature_at(line: &str, cursor: usize) -> Option<SignatureHint> {
    let index = index();
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let env = HashMap::from([("PATH".to_string(), String::new())]);
    let aliases = HashMap::new();
    complete_line(&CompletionRequest {
        line,
        cursor,
        cwd: Path::new("/"),
        env: &env,
        aliases: &aliases,
        wrappers: &[],
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &EmptyUsageLookup,
        timeout: Duration::from_secs(1),
        now_ts: 1_700_000_000,
        allow_stale: false,
    })
    .signature
}

fn signature(line: &str) -> Option<SignatureHint> {
    signature_at(line, line.chars().count())
}

/// The highlighted placeholder of the hint, if any.
fn active(sig: &SignatureHint) -> Option<String> {
    sig.active
        .map(|(a, b)| sig.text.chars().skip(a).take(b - a).collect())
}

#[test]
fn signature_marks_the_positional_being_filled() {
    let sig = signature("cp ").expect("signature on the first positional");
    assert_eq!(sig.text, "cp  <src>  [dest]");
    assert_eq!(active(&sig).as_deref(), Some("<src>"));

    let sig = signature("cp a.txt ").expect("signature on the second positional");
    assert_eq!(active(&sig).as_deref(), Some("[dest]"));

    // Still typing the first word: the slot is the word under the cursor.
    let sig = signature("cp a.t").expect("signature while typing");
    assert_eq!(active(&sig).as_deref(), Some("<src>"));
}

#[test]
fn signature_without_active_slot_past_the_last_positional() {
    let sig = signature("cp a b ").expect("signature still shown");
    assert_eq!(sig.text, "cp  <src>  [dest]");
    assert_eq!(sig.active, None);
}

#[test]
fn signature_counts_positionals_around_options() {
    assert_eq!(active(&signature("cp -f ").unwrap()).as_deref(), Some("<src>"));
    assert_eq!(active(&signature("cp -t out a ").unwrap()).as_deref(), Some("[dest]"));
    assert_eq!(active(&signature("cp -- -weird ").unwrap()).as_deref(), Some("[dest]"));
}

#[test]
fn signature_hidden_on_options_and_option_values() {
    assert_eq!(signature("cp -"), None, "typing a flag");
    assert_eq!(signature("cp --for"), None, "typing a long flag");
    assert_eq!(signature("cp -t "), None, "option value slot");
    assert_eq!(signature("cp --target="), None, "attached option value");
}

#[test]
fn signature_hidden_without_declared_positionals_or_spec() {
    assert_eq!(signature("ls "), None, "no positionals");
    assert_eq!(signature("unknowncmd "), None, "no spec");
    assert_eq!(signature("c"), None, "root command slot");
    assert_eq!(signature("git add "), None, "a single positional: the dropdown says it all");
}

#[test]
fn signature_variadic_slot_stays_active() {
    let sig = signature("tar a.tgz x y ").expect("signature");
    assert_eq!(sig.text, "tar  <archive>  <files…>");
    assert_eq!(active(&sig).as_deref(), Some("<files…>"));
}

#[test]
fn signature_follows_nested_command_lines() {
    let sig = signature("timeout ").unwrap();
    assert_eq!(sig.text, "timeout  <duration>  <command…>");
    assert_eq!(active(&sig).as_deref(), Some("<duration>"));
    assert_eq!(active(&signature("timeout 5 ").unwrap()).as_deref(), Some("<command…>"));

    // Once the nested command is typed, the hint describes the innermost command.
    let sig = signature("timeout 5 cp a ").unwrap();
    assert_eq!(sig.text, "cp  <src>  [dest]");
    assert_eq!(active(&sig).as_deref(), Some("[dest]"));
}

#[test]
fn signature_uses_the_cursor_position_not_the_line_end() {
    // Cursor inside `a.txt` of `cp a.txt b`: the first positional is active.
    let sig = signature_at("cp a.txt b", 4).unwrap();
    assert_eq!(active(&sig).as_deref(), Some("<src>"));
}

#[test]
fn signature_active_range_counts_chars_not_bytes() {
    // `…` is multi-byte: the range must still index chars of `text`.
    let sig = signature("timeout 5 ").unwrap();
    let (a, b) = sig.active.unwrap();
    assert_eq!(b, sig.text.chars().count());
    assert_eq!(b - a, "<command…>".chars().count());
}
