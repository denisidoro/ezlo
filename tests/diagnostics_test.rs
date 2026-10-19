use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::icons::Glyphs;
use ezlo::daemon::protocol::{
    build_wire_response, decode_wire_response, encode_wire_response, WireDiag,
};
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::{complete_line, CompletionRequest, DiagSeverity, LineDiagnostic};
use ezlo::spec::{parse_and_validate, SpecIndex};
use ezlo::store::Store;

const DIAG_SPEC: &str = "\
cmd deploy # Deploy service
  opt -a # Flag A
  opt -b # Flag B
  opt -v --verbose # Verbose output
  opt --json excludes --yaml # JSON output
  opt --yaml excludes --json # YAML output
  opt -o --output <file> file # Output file
  opt -d --dir <dir> dir # Target directory
  opt -t --tag <tag...> # Repeatable tag
  opt -e --env <env> required # Target environment
  arg <config> file
";

fn run_complete(line: &str, cwd: &Path, env: &HashMap<String, String>) -> Vec<LineDiagnostic> {
    let (spec, diags) = parse_and_validate(DIAG_SPEC);
    assert!(diags.is_empty(), "spec diags: {diags:?}");
    let index = SpecIndex::from_single(spec);
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let store = Store::open_in_memory().expect("store");
    let aliases = HashMap::new();
    let res = complete_line(&CompletionRequest {
        line,
        cursor: line.len(),
        cwd,
        env,
        aliases: &aliases,
        wrappers: &[],
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &store,
        timeout: Duration::from_secs(1),
        now_ts: 1_000,
        allow_stale: false,
    });
    res.diagnostics
}

#[test]
fn diagnostics_unknown_options_and_cursor_guard() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = HashMap::new();

    // Token currently being typed at the cursor is NOT prematurely flagged
    let typing = run_complete("deploy --bog", tmp.path(), &env);
    assert!(typing.is_empty(), "expected no diags while typing flag: {typing:?}");

    // Completed unknown long option is flagged
    let completed = run_complete("deploy -e prod --bogus ", tmp.path(), &env);
    assert_eq!(completed.len(), 1, "{completed:?}");
    assert_eq!(completed[0].severity, DiagSeverity::Error);
    assert_eq!(completed[0].message, "unknown option '--bogus'");
    assert_eq!(&"deploy -e prod --bogus "[completed[0].char_start..completed[0].char_end], "--bogus");

    // Unknown char inside a short flag cluster highlights only the unknown char
    let cluster = run_complete("deploy -e prod -abz ", tmp.path(), &env);
    assert_eq!(cluster.len(), 1, "{cluster:?}");
    assert_eq!(cluster[0].severity, DiagSeverity::Error);
    assert_eq!(cluster[0].message, "unknown option '-z'");
    assert_eq!(&"deploy -e prod -abz "[cluster[0].char_start..cluster[0].char_end], "z");

    // Inline `--flag=val` at the cursor validates the completed `--flag` part immediately
    let inline_cursor = run_complete("deploy --bogus=123", tmp.path(), &env);
    assert_eq!(inline_cursor.len(), 1, "{inline_cursor:?}");
    assert_eq!(inline_cursor[0].message, "unknown option '--bogus'");
    assert_eq!(
        &"deploy --bogus=123"[inline_cursor[0].char_start..inline_cursor[0].char_end],
        "--bogus"
    );

    // Tokens after `--` are positional arguments, not options
    let existing = tmp.path().join("--bogus");
    std::fs::write(&existing, "ok").expect("write --bogus file");
    let after_dd = run_complete("deploy -e prod -- --bogus ", tmp.path(), &env);
    assert!(after_dd.is_empty(), "expected no option diag after `--`: {after_dd:?}");
}

#[test]
fn diagnostics_mutually_exclusive_and_duplicate_options() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = HashMap::new();

    // Mutually exclusive options (`--json` excludes `--yaml`)
    let excl = run_complete("deploy -e prod --json --yaml ", tmp.path(), &env);
    assert_eq!(excl.len(), 1, "{excl:?}");
    assert_eq!(excl[0].severity, DiagSeverity::Error);
    assert_eq!(excl[0].message, "option '--yaml' cannot be used with '--json'");
    assert_eq!(&"deploy -e prod --json --yaml "[excl[0].char_start..excl[0].char_end], "--yaml");

    // Duplicate non-repeatable option (`-v` / `--verbose`)
    let dup_flag = run_complete("deploy -e prod -v --verbose ", tmp.path(), &env);
    assert_eq!(dup_flag.len(), 1, "{dup_flag:?}");
    assert_eq!(dup_flag[0].message, "duplicate option '--verbose'");

    let dup_cluster = run_complete("deploy -e prod -vv ", tmp.path(), &env);
    assert_eq!(dup_cluster.len(), 1, "{dup_cluster:?}");
    assert_eq!(dup_cluster[0].message, "duplicate option '-v'");
    assert_eq!(&"deploy -e prod -vv "[dup_cluster[0].char_start..dup_cluster[0].char_end], "v");

    // Repeatable (`variadic`) option `--tag <tag...>` is allowed multiple times
    let variadic_ok = run_complete("deploy -e prod -t v1 --tag v2 ", tmp.path(), &env);
    assert!(variadic_ok.is_empty(), "variadic option should repeat cleanly: {variadic_ok:?}");
}

#[test]
fn diagnostics_missing_required_option() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = HashMap::new();

    // Just typing the command and a space (`deploy `) or still typing the first argument (`deploy -a`)
    // does not yet complain about missing required options.
    assert!(run_complete("deploy ", tmp.path(), &env).is_empty());
    assert!(run_complete("deploy -a", tmp.path(), &env).is_empty());

    // Once at least one argument/option word is completed and cursor is on a subsequent word,
    // missing `required` options on the matched command are reported.
    let missing = run_complete("deploy -a ", tmp.path(), &env);
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(missing[0].severity, DiagSeverity::Error);
    assert_eq!(missing[0].message, "missing required option '--env'");
    assert_eq!(&"deploy -a "[missing[0].char_start..missing[0].char_end], "deploy");
}

#[test]
fn diagnostics_nonexistent_file_and_dir_paths_with_expansion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sub_dir = tmp.path().join("configs");
    std::fs::create_dir_all(&sub_dir).expect("mkdir configs");
    let ok_file = sub_dir.join("app.toml");
    std::fs::write(&ok_file, "port = 8080").expect("write app.toml");

    let mut env = HashMap::new();
    env.insert("HOME".to_string(), tmp.path().to_string_lossy().into_owned());
    env.insert("CFG_DIR".to_string(), sub_dir.to_string_lossy().into_owned());

    // Valid existing paths via `~` and `$CFG_DIR` produce no warnings
    let ok = run_complete(
        "deploy -e prod -d ~/configs -o $CFG_DIR/app.toml ${CFG_DIR}/app.toml ",
        tmp.path(),
        &env,
    );
    assert!(ok.is_empty(), "expected no warnings for existing paths: {ok:?}");

    // Non-existent positional file path produces a Warning
    let missing_pos = run_complete("deploy -e prod missing.toml ", tmp.path(), &env);
    assert_eq!(missing_pos.len(), 1, "{missing_pos:?}");
    assert_eq!(missing_pos[0].severity, DiagSeverity::Warning);
    assert_eq!(missing_pos[0].message, "file 'missing.toml' does not exist");
    assert_eq!(
        &"deploy -e prod missing.toml "[missing_pos[0].char_start..missing_pos[0].char_end],
        "missing.toml"
    );

    // Passing a regular file to a `dir` option (`-d`) warns that it is not a directory
    let not_dir = run_complete("deploy -e prod -d ~/configs/app.toml ", tmp.path(), &env);
    assert_eq!(not_dir.len(), 1, "{not_dir:?}");
    assert_eq!(not_dir[0].severity, DiagSeverity::Warning);
    assert_eq!(not_dir[0].message, "'~/configs/app.toml' is not a directory");

    // Inline `--output=missing.txt` completed before cursor highlights only the value span
    let inline_missing = run_complete("deploy -e prod --output=missing.txt ", tmp.path(), &env);
    assert_eq!(inline_missing.len(), 1, "{inline_missing:?}");
    assert_eq!(inline_missing[0].severity, DiagSeverity::Warning);
    assert_eq!(
        &"deploy -e prod --output=missing.txt "[inline_missing[0].char_start..inline_missing[0].char_end],
        "missing.txt"
    );

    // Glob patterns are not flagged as non-existent literal paths
    let glob_ok = run_complete("deploy -e prod *.toml ", tmp.path(), &env);
    assert!(glob_ok.is_empty(), "globs should not warn: {glob_ok:?}");
}

#[test]
fn wire_protocol_encodes_and_decodes_diagnostics() {
    let diags = vec![
        LineDiagnostic {
            char_start: 7,
            char_end: 14,
            severity: DiagSeverity::Error,
            message: "unknown option '--bogus'".to_string(),
        },
        LineDiagnostic {
            char_start: 15,
            char_end: 23,
            severity: DiagSeverity::Warning,
            message: "file 'nope.txt' does not exist".to_string(),
        },
    ];
    let resp = build_wire_response(42, 7, 14, "", &[], 80, Glyphs::Nerd, 6).with_diagnostics(&diags);
    let text = encode_wire_response(&resp);
    assert!(text.starts_with("R\x1f42\x1f7\x1f14\x1f\x1f0\x1f0\x1f6\x1f2\x1f0\x1f\x1f\x1f\x1f0\n"));
    assert!(text.contains("D\x1f7\x1f14\x1ferror\x1funknown option '--bogus'\n"));
    assert!(text.contains("D\x1f15\x1f23\x1fwarning\x1ffile 'nope.txt' does not exist\n"));

    let decoded = decode_wire_response(&text).expect("decode wire response");
    assert_eq!(
        decoded.diagnostics,
        vec![
            WireDiag {
                start: 7,
                end: 14,
                severity: "error".to_string(),
                message: "unknown option '--bogus'".to_string(),
            },
            WireDiag {
                start: 15,
                end: 23,
                severity: "warning".to_string(),
                message: "file 'nope.txt' does not exist".to_string(),
            },
        ]
    );
}
