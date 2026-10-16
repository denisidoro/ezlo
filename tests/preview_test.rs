//! Blast-radius & dry-run effect preview: glob expansion and path summaries for file-mutating
//! commands, spec `preview $ …` dry-runs with `{opt.*}` / `{arg.*}` interpolation, the `P` wire
//! lines and the spec grammar/validation of `preview`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use ezlo::daemon::protocol::{build_wire_response, decode_wire_response, encode_wire_response};
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::preview::{preview_line, PreviewKind, PreviewLine, PreviewOptions};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::CompletionRequest;
use ezlo::spec::span::RuleCode;
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
cmd deploy "Deploy a service"
  opt -f --force "Skip checks"
  opt --region <region> = us | eu "Target region"
  arg <service> "Service to deploy"
  arg [env] = prod | staging
  preview $ deploy-tool plan {opt.force} --region={opt.region} {arg.service} {arg.env} 2>&1

cmd backup "Back up a path"
  arg <path...>
  preview `backup-tool --dry-run {arg.path}`

cmd timeout "Run a command with a time limit"
  arg <duration>
  arg <command...> cmd
"#;

fn index(src: &str) -> SpecIndex {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

struct Fixture {
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    cache: GeneratorCache,
    env: HashMap<String, String>,
    wrappers: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            index: index(SPEC),
            runner: FakeGeneratorRunner::new(),
            cache: GeneratorCache::new(),
            env: HashMap::from([("HOME".to_string(), "/nonexistent-home".to_string())]),
            wrappers: vec!["sudo".to_string()],
        }
    }

    fn preview(&self, line: &str, cwd: &Path) -> Vec<PreviewLine> {
        self.preview_with(line, cwd, &PreviewOptions::default())
    }

    fn preview_with(&self, line: &str, cwd: &Path, opts: &PreviewOptions) -> Vec<PreviewLine> {
        let req = CompletionRequest {
            line,
            cursor: line.chars().count(),
            cwd,
            env: &self.env,
            aliases: &HashMap::new(),
            wrappers: &self.wrappers,
            index: &self.index,
            runner: &self.runner,
            cache: &self.cache,
            usage: &EmptyUsageLookup,
            timeout: Duration::from_millis(100),
            now_ts: 0,
            allow_stale: false,
        };
        preview_line(&req, opts)
    }
}

/// `a.log` (100 B), `b.log` (200 B), `.hidden.log`, `notes.txt`, `build/{x.o,y.o}` (10 B each),
/// `build/deep/z.o` (5 B).
fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    fs::write(p.join("a.log"), vec![b'a'; 100]).unwrap();
    fs::write(p.join("b.log"), vec![b'b'; 200]).unwrap();
    fs::write(p.join(".hidden.log"), b"h").unwrap();
    fs::write(p.join("notes.txt"), b"n").unwrap();
    fs::create_dir_all(p.join("build/deep")).unwrap();
    fs::write(p.join("build/x.o"), vec![0; 10]).unwrap();
    fs::write(p.join("build/y.o"), vec![0; 10]).unwrap();
    fs::write(p.join("build/deep/z.o"), vec![0; 5]).unwrap();
    dir
}

fn texts(lines: &[PreviewLine]) -> Vec<(PreviewKind, &str)> {
    lines.iter().map(|l| (l.kind, l.text.as_str())).collect()
}

#[test]
fn glob_operands_of_rm_are_counted_and_measured() {
    let dir = tree();
    let fx = Fixture::new();
    assert_eq!(
        texts(&fx.preview("rm *.log", dir.path())),
        [(PreviewKind::Effect, "rm: 2 files · 300 B")],
        "dotfiles don't match a leading `*`"
    );
    assert_eq!(
        texts(&fx.preview("rm -f .*.log ?.log", dir.path())),
        [(PreviewKind::Effect, "rm: 3 files · 301 B")]
    );
}

#[test]
fn directories_are_measured_recursively() {
    let dir = tree();
    let fx = Fixture::new();
    assert_eq!(
        texts(&fx.preview("rm -rf build", dir.path())),
        [(PreviewKind::Effect, "rm: 1 dir (+4 inside) · 25 B")]
    );
    assert_eq!(
        texts(&fx.preview("rm build/**/*.o", dir.path())),
        [(PreviewKind::Effect, "rm: 3 files · 25 B")]
    );
}

#[test]
fn unmatched_glob_warns_and_quoted_glob_is_literal() {
    let dir = tree();
    let fx = Fixture::new();
    assert_eq!(
        texts(&fx.preview("rm *.nope", dir.path())),
        [(PreviewKind::Warning, "rm: no matches found: *.nope")]
    );
    assert!(fx.preview("rm '*.log'", dir.path()).is_empty(), "quoted: a literal path that doesn't exist");
    // A single plain file is summarized too, so every operand shape behaves the same.
    let lines = fx.preview("rm a.log", dir.path());
    let single = texts(&lines);
    assert_eq!(single.len(), 1, "{single:?}");
    assert!(single[0].1.starts_with("rm: 1 file · "), "{single:?}");
    assert!(fx.preview("rm $HOME/*.log", dir.path()).is_empty(), "expansions aren't guessed");
}

#[test]
fn command_shapes_skip_modes_and_destinations() {
    let dir = tree();
    let fx = Fixture::new();
    assert_eq!(
        texts(&fx.preview("mv *.log build/", dir.path())),
        [(PreviewKind::Effect, "mv: 2 files · 300 B → build/")]
    );
    assert_eq!(
        texts(&fx.preview("chmod -R 644 *.log notes.txt", dir.path())),
        [(PreviewKind::Effect, "chmod: 3 files · 301 B")]
    );
    assert_eq!(
        texts(&fx.preview("sudo rm *.log", dir.path())),
        [(PreviewKind::Effect, "rm: 2 files · 300 B")],
        "wrappers are skipped"
    );
    assert_eq!(
        texts(&fx.preview("timeout 5 rm *.log", dir.path())),
        [(PreviewKind::Effect, "rm: 2 files · 300 B")],
        "nested cmd slots are followed"
    );
    assert!(fx.preview("ls *.log", dir.path()).is_empty(), "not a mutating command");
}

#[test]
fn only_the_segment_under_the_cursor_counts() {
    let dir = tree();
    let fx = Fixture::new();
    assert!(fx.preview("rm *.log; echo done", dir.path()).is_empty());
    assert_eq!(fx.preview("echo hi && rm *.log", dir.path()).len(), 1);
}

#[test]
fn entry_budget_marks_lower_bounds() {
    let dir = tree();
    // `read_dir` order is filesystem-dependent, so use equal-sized files: any 2 visited entries sum to 20 B.
    let flat = dir.path().join("flat");
    fs::create_dir(&flat).unwrap();
    for name in ["p.o", "q.o", "r.o"] {
        fs::write(flat.join(name), vec![0; 10]).unwrap();
    }
    let fx = Fixture::new();
    let opts = PreviewOptions {
        max_entries: 2,
        ..PreviewOptions::default()
    };
    let lines = fx.preview_with("rm -r flat", dir.path(), &opts);
    assert_eq!(texts(&lines), [(PreviewKind::Effect, "rm: 1 dir (+≥2 inside) · ≥20 B")]);
    let off = PreviewOptions {
        commands: Vec::new(),
        ..PreviewOptions::default()
    };
    assert!(fx.preview_with("rm *.log", dir.path(), &off).is_empty());
}

#[test]
fn spec_preview_interpolates_parsed_values() {
    let fx = Fixture::new();
    fx.runner.set_response(
        "deploy-tool plan",
        vec!["\x1b[32m+ api@prod\x1b[0m".into(), "".into(), "2 changes".into(), "x".into(), "y".into()],
    );
    let cwd = Path::new("/");

    let lines = fx.preview("deploy -f api prod --region eu", cwd);
    assert_eq!(
        texts(&lines),
        [
            (PreviewKind::DryRun, "+ api@prod"),
            (PreviewKind::DryRun, "2 changes"),
            (PreviewKind::DryRun, "x"),
        ],
        "ANSI stripped, blank lines dropped, at most 3 lines"
    );
    let calls = fx.runner.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "deploy-tool plan --force --region=eu api prod 2>&1");

    // Same command line again: served from the short-lived cache.
    fx.preview("deploy -f api prod --region eu", cwd);
    assert_eq!(fx.runner.call_count(), 1);

    // Absent options expand to nothing; values are shell-quoted.
    fx.preview("deploy 'my svc'", cwd);
    assert_eq!(fx.runner.recorded_calls()[1].0, "deploy-tool plan  --region= 'my svc'  2>&1");
}

#[test]
fn spec_preview_waits_for_referenced_args() {
    let fx = Fixture::new();
    fx.runner.set_response("backup-tool", vec!["would copy 3 files".into()]);
    let cwd = Path::new("/");
    assert!(fx.preview("deploy -f ", cwd).is_empty());
    assert!(fx.preview("backup", cwd).is_empty());
    assert_eq!(fx.runner.call_count(), 0, "nothing runs until <service>/<path> is typed");

    let lines = fx.preview("backup a b", cwd);
    assert_eq!(texts(&lines), [(PreviewKind::DryRun, "would copy 3 files")]);
    assert_eq!(fx.runner.recorded_calls()[0].0, "backup-tool --dry-run a b");

    let no_dry_run = PreviewOptions {
        dry_run: false,
        ..PreviewOptions::default()
    };
    assert!(fx.preview_with("backup c", cwd, &no_dry_run).is_empty());
}

#[test]
fn preview_lines_round_trip_on_the_wire() {
    let lines = vec![
        PreviewLine {
            kind: PreviewKind::Effect,
            text: "rm: 2 files · 300 B".into(),
        },
        PreviewLine {
            kind: PreviewKind::DryRun,
            text: "tab\x1fsep".into(),
        },
    ];
    let resp = build_wire_response(7, 0, 3, "", &[], 80, 6).with_preview(&lines);
    let raw = encode_wire_response(&resp);
    assert!(raw.lines().next().unwrap().ends_with("\x1f0\x1f2\x1f\x1f\x1f\x1f0"), "header carries the P line count");
    let decoded = decode_wire_response(&raw).expect("decodes");
    assert_eq!(decoded.preview.len(), 2);
    assert_eq!(decoded.preview[0].kind, "effect");
    assert_eq!(decoded.preview[1].text, "tabsep");

    // Responses from older daemons (no preview count) still decode.
    let old = "R\x1f1\x1f0\x1f0\x1f\x1f0\x1f0\x1f6\x1f0\n";
    assert!(decode_wire_response(old).expect("decodes").preview.is_empty());
}

#[test]
fn preview_grammar_and_validation() {
    let (spec, diags) = parse_and_validate(SPEC);
    assert!(diags.is_empty());
    let deploy = spec.cmds.iter().find(|c| c.path == ["deploy"]).unwrap();
    assert_eq!(
        deploy.preview.as_deref(),
        Some("deploy-tool plan {opt.force} --region={opt.region} {arg.service} {arg.env} 2>&1")
    );
    let json = serde_json::to_value(deploy).unwrap();
    assert!(json["preview"].as_str().unwrap().starts_with("deploy-tool plan"));

    let (_, diags) = parse_and_validate("cmd x\n  arg <a>\n  preview $ x --dry-run {arg.b} {opt.nope}\n");
    let unresolved: Vec<_> = diags.iter().filter(|d| d.code == RuleCode::E05Unresolved).collect();
    assert_eq!(unresolved.len(), 2, "{diags:#?}");

    let (_, diags) = parse_and_validate("cmd x\n  preview $ a\n  preview $ b\n");
    assert!(diags.iter().any(|d| d.code == RuleCode::E04Duplicate), "{diags:#?}");

    let (_, diags) = parse_and_validate("cmd x\n  preview\n");
    assert!(diags.iter().any(|d| d.code == RuleCode::E02Structure), "{diags:#?}");

    // Global options of ancestors resolve too.
    let (_, diags) = parse_and_validate(
        "cmd tool\n  opt --profile <p> \"Profile\" global\ncmd tool apply\n  preview $ tool plan --profile={opt.profile}\n",
    );
    assert!(diags.is_empty(), "{diags:#?}");
}

#[test]
fn user_overlay_can_add_a_preview() {
    let base = parse_and_validate("cmd deploy \"Deploy\"\n  arg <service>\n").0;
    let overlay = parse_and_validate("cmd deploy\n  preview $ deploy-tool plan {arg.service}\n").0;
    let merged = SpecIndex::merge_ordered(&[("user".into(), overlay), ("bundled".into(), base)], &mut Vec::new());
    let cmd = merged.cmds.iter().find(|c| c.path == ["deploy"]).unwrap();
    assert_eq!(cmd.desc.as_deref(), Some("Deploy"));
    assert_eq!(cmd.preview.as_deref(), Some("deploy-tool plan {arg.service}"));
}

#[test]
fn instant_values_of_read_only_commands() {
    let dir = tree();
    let mut fx = Fixture::new();
    fx.env.insert("USER".into(), "zelda".into());
    fx.env.insert("N".into(), "4".into());
    let value = |line: &str| -> Vec<(PreviewKind, String)> {
        fx.preview(line, dir.path()).into_iter().map(|p| (p.kind, p.text)).collect()
    };
    assert_eq!(value("echo hi $USER"), [(PreviewKind::Value, "hi zelda".to_string())]);
    assert_eq!(value("echo $((N * 2))"), [(PreviewKind::Value, "8".to_string())]);
    assert_eq!(value("echo *.log"), [(PreviewKind::Value, "a.log b.log".to_string())]);
    assert!(value("echo plain words").is_empty(), "nothing to expand");
    assert!(value("echo $(whoami)").is_empty(), "command substitutions aren't run");
    assert!(value("echo $USER > out.txt").is_empty(), "redirections change what runs");
    assert_eq!(value("pwd"), [(PreviewKind::Value, dir.path().to_string_lossy().into_owned())]);
    assert_eq!(value("basename /a/b/c.txt .txt"), [(PreviewKind::Value, "c".to_string())]);
    assert_eq!(value("test -d build"), [(PreviewKind::Value, "true (exit 0)".to_string())]);
    assert_eq!(value("[ -f build ]"), [(PreviewKind::Value, "false (exit 1)".to_string())]);
}

#[test]
fn instant_effects_of_simple_commands() {
    let dir = tree();
    let fx = Fixture::new();
    let first = |line: &str| fx.preview(line, dir.path()).into_iter().next().map(|p| (p.kind, p.text));
    let (kind, text) = first("cd build").expect("cd preview");
    assert_eq!(kind, PreviewKind::Effect);
    assert!(text.starts_with("cd → ") && text.ends_with("build · 3 entries"), "{text}");
    assert_eq!(first("cd nope"), Some((PreviewKind::Warning, "cd: no such directory: nope".into())));
    // A path still being typed (prefix of an entry, under the cursor) isn't missing yet.
    assert_eq!(first("cd bui"), None);
    assert_eq!(first("ls bui"), None);
    assert_eq!(first("cat note"), None);
    assert_eq!(first("cat note "), Some((PreviewKind::Warning, "cat: not a file: note".into())), "finished word");
    assert_eq!(first("cat note a.log").map(|p| p.0), Some(PreviewKind::Value));
    assert_eq!(fx.preview("cat note a.log", dir.path()).len(), 2, "not under the cursor: warns");
    assert_eq!(first("cat build/deep/q"), Some((PreviewKind::Warning, "cat: not a file: build/deep/q".into())));
    assert_eq!(first("mkdir new"), Some((PreviewKind::Effect, "mkdir: creates new".into())));
    assert_eq!(first("mkdir build"), Some((PreviewKind::Warning, "mkdir: already exists: build".into())));
    assert_eq!(
        first("mkdir x/y"),
        Some((PreviewKind::Warning, "mkdir: missing parent (use -p): x/y".into()))
    );
    assert_eq!(
        first("touch a.log new.txt"),
        Some((PreviewKind::Effect, "touch: creates new.txt · updates the time of a.log".into()))
    );
    let all = fx.preview("ln -s missing link", dir.path());
    assert_eq!(all[0].text, "ln: symlink link → missing");
    assert!(all[1].text.contains("dangling"), "{all:?}");
    let opts = PreviewOptions { instant: Vec::new(), ..PreviewOptions::default() };
    assert!(fx.preview_with("mkdir new", dir.path(), &opts).is_empty(), "[preview] instant = [] disables it");
}

#[test]
fn safety_warnings_for_risky_lines() {
    let dir = tree();
    let mut fx = Fixture::new();
    fx.env.insert("HOME".into(), dir.path().to_string_lossy().into_owned());
    let warnings = |line: &str| -> Vec<String> {
        fx.preview(line, dir.path())
            .into_iter()
            .filter(|p| p.kind == PreviewKind::Warning)
            .map(|p| p.text)
            .collect()
    };
    assert!(warnings("rm -rf ~/*").contains(&"rm: targets everything in your home directory".to_string()));
    assert!(warnings("rm -rf $NOPE/").iter().any(|w| w == "rm: targets the root directory /"));
    assert!(warnings("rm -rf $NOPE/").iter().any(|w| w.contains("$NOPE is not exported")));
    assert!(warnings("rm -rf ${NOPE:-build}").is_empty(), "defaults handle unset parameters");
    assert!(warnings("rm -rf build").is_empty());
    assert_eq!(warnings("echo hi > a.log"), ["`>` overwrites a.log (100 B)"]);
    assert!(warnings("echo hi >> a.log").is_empty());
    assert_eq!(warnings("cp b.log a.log"), ["cp: overwrites a.log"]);
    assert!(warnings("cp -n b.log a.log").is_empty());
    assert_eq!(warnings("cp a.log build"), Vec::<String>::new());
    assert_eq!(warnings("git push -f"), ["git push: force-push rewrites remote history (prefer --force-with-lease)"]);
    assert!(warnings("git push --force-with-lease").is_empty());
    let opts = PreviewOptions { safety: false, ..PreviewOptions::default() };
    assert!(fx.preview_with("git push -f", dir.path(), &opts).is_empty());
}
