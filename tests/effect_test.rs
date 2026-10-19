//! Side-effect classes: `effect` / `dryrun` in specs, the line-level classification (segments,
//! nested command lines, wrappers, redirections, unknown parts), and how `explain` and the
//! preview surface it.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use ezlo::bundled::{load_bundled_specs, BUNDLED_SPECS};
use ezlo::engine::effect::{line_effect, LineEffect};
use ezlo::engine::explain::explain_line;
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::preview::{preview_line, PreviewKind, PreviewOptions};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest};
use ezlo::spec::ast::{Effect, Spec};
use ezlo::spec::span::RuleCode;
use ezlo::spec::{has_errors, parse_and_validate, SpecIndex};

const SPEC: &str = r#"
cmd timeout "Run a command with a time limit"
  effect read
  arg <duration>
  arg <command...> cmd

cmd git "Version control"

cmd git status "Show the working tree status"
  effect read

cmd git branch "List, create, or delete branches"
  effect read
  opt -d --delete "Delete a merged branch" effect write
  opt -D "Delete a branch" effect destroy
  opt -l --list "List branches matching a pattern" dryrun
  arg [name] effect write

cmd git push "Update remote refs"
  effect remote
  opt -n --dry-run "Do everything except send updates" dryrun
  opt -f --force "Force update remote refs" effect destroy
  arg [remote]

cmd git stash "Stash changes"
  effect write

cmd git stash list "List stash entries"
  effect read

cmd git stash pop "Apply and remove a stash entry"

cmd sed "Stream editor"
  opt -i --in-place [suffix] "Edit files in place" effect write
  arg <script>
  arg [file...] path

cmd ls "List directory contents"
  effect read
  arg [path...] path

cmd echo "Print arguments"
  effect read
  arg [words...]
"#;

struct Fixture {
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    cache: GeneratorCache,
    env: HashMap<String, String>,
    aliases: HashMap<String, String>,
    wrappers: Vec<String>,
}

impl Fixture {
    fn new(index: SpecIndex) -> Self {
        Self {
            index,
            runner: FakeGeneratorRunner::new(),
            cache: GeneratorCache::new(),
            env: HashMap::new(),
            aliases: HashMap::from([("gp".to_string(), "git push".to_string())]),
            wrappers: vec!["sudo".to_string()],
        }
    }

    fn spec() -> Self {
        let (spec, diags) = parse_and_validate(SPEC);
        assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
        Self::new(SpecIndex::from_single(spec))
    }

    fn req<'a>(&'a self, line: &'a str) -> CompletionRequest<'a> {
        CompletionRequest {
            line,
            cursor: line.chars().count(),
            cwd: Path::new("/nonexistent"),
            env: &self.env,
            aliases: &self.aliases,
            wrappers: &self.wrappers,
            index: &self.index,
            runner: &self.runner,
            cache: &self.cache,
            usage: &EmptyUsageLookup,
            timeout: Duration::from_millis(100),
            now_ts: 0,
            allow_stale: false,
        }
    }

    fn effect(&self, line: &str) -> LineEffect {
        line_effect(&self.req(line))
    }

    fn level(&self, line: &str) -> Option<Effect> {
        self.effect(line).level
    }
}

#[test]
fn command_options_and_positionals_raise_the_effect() {
    let f = Fixture::spec();
    let e = f.effect("git status");
    assert_eq!((e.level, e.because.as_slice()), (Some(Effect::Read), ["git status".to_string()].as_slice()));
    assert!(e.read_only());

    assert_eq!(f.level("git branch"), Some(Effect::Read), "bare `git branch` lists");
    let e = f.effect("git branch feat");
    assert_eq!((e.level, e.because), (Some(Effect::Write), vec!["git branch feat".to_string()]));
    let e = f.effect("git branch -D feat");
    assert_eq!((e.level, e.because), (Some(Effect::Destroy), vec!["git branch -D".to_string()]));
    assert_eq!(f.level("git branch -d feat"), Some(Effect::Write));
}

#[test]
fn dryrun_options_make_the_command_read_only() {
    let f = Fixture::spec();
    assert_eq!(f.level("git push origin"), Some(Effect::Remote));
    assert_eq!(f.level("git push --force origin"), Some(Effect::Destroy));
    let e = f.effect("git push -n --force origin");
    assert_eq!(e.level, Some(Effect::Read));
    assert_eq!(e.because, ["git push -n (dry run)"]);
    assert!(e.read_only());
    assert!(f.effect("git branch --list 'f*'").read_only(), "a list pattern isn't a new branch");
}

#[test]
fn subcommands_inherit_the_nearest_declared_effect() {
    let f = Fixture::spec();
    let e = f.effect("git stash pop");
    assert_eq!((e.level, e.because), (Some(Effect::Write), vec!["git stash pop".to_string()]));
    assert_eq!(f.level("git stash list"), Some(Effect::Read));
}

#[test]
fn undeclared_commands_are_unknown_not_safe() {
    let f = Fixture::spec();
    let e = f.effect("git");
    assert_eq!((e.level, e.unknown), (None, vec!["git".to_string()]));

    let e = f.effect("make build && git status");
    assert_eq!(e.level, Some(Effect::Read));
    assert_eq!(e.unknown, ["make"]);
    assert!(!e.read_only());

    let e = f.effect("sed -i s/a/b/ notes.txt");
    assert_eq!((e.level, e.unknown.as_slice()), (Some(Effect::Write), ["sed".to_string()].as_slice()));
    assert_eq!(f.effect("sed s/a/b/ notes.txt").level, None);
}

#[test]
fn every_segment_and_nested_command_counts() {
    let f = Fixture::spec();
    assert_eq!(f.level("git status; git push"), Some(Effect::Remote));
    assert_eq!(f.level("git status\ngit push"), Some(Effect::Remote), "newlines separate commands");
    assert_eq!(f.effect("ls | grep x").unknown, ["grep"]);

    let e = f.effect("timeout 5 git push origin");
    assert_eq!((e.level, e.because.as_slice()), (Some(Effect::Remote), ["git push".to_string()].as_slice()));
    assert!(e.unknown.is_empty(), "{e:?}");
    assert_eq!(f.level("sudo git push"), Some(Effect::Remote), "wrappers are skipped");
    assert_eq!(f.level("gp origin"), Some(Effect::Remote), "aliases are expanded");
    assert!(f.effect("FOO=1").read_only(), "assignments alone change nothing");
}

#[test]
fn redirections_write_files() {
    let f = Fixture::spec();
    let e = f.effect("ls > out.txt");
    assert_eq!((e.level, e.because), (Some(Effect::Write), vec!["> out.txt".to_string()]));
    assert_eq!(f.level("echo hi >> log.txt"), Some(Effect::Write));

    let e = f.effect("ls 2>&1 >/dev/null");
    assert!(e.read_only(), "fd duplications and /dev targets write nothing: {e:?}");
    let e = f.effect("ls 2>&1 | git status");
    assert!(e.read_only(), "`2>&1` doesn't split the line at `&`: {e:?}");
}

#[test]
fn command_substitutions_are_never_read_only() {
    let f = Fixture::spec();
    let e = f.effect("echo \"$(git push)\"");
    assert!(e.unknown.iter().any(|u| u.ends_with("(command substitution)")), "{e:?}");
    assert!(!f.effect("echo $(git push)").read_only());
    assert!(f.effect("echo '$(git push)'").read_only(), "single quotes are literal");
}

#[test]
fn preview_shows_remote_and_destroy_effects() {
    let f = Fixture::spec();
    let lines = |line: &str, opts: &PreviewOptions| -> Vec<(PreviewKind, String)> {
        preview_line(&f.req(line), opts).into_iter().map(|l| (l.kind, l.text)).collect()
    };
    let opts = PreviewOptions::default();
    assert_eq!(lines("git push origin", &opts), [(PreviewKind::Effect, "git push: changes remote state".to_string())]);
    assert_eq!(lines("git branch -D feat", &opts), [(PreviewKind::Warning, "git branch -D: destroys data".to_string())]);
    assert!(lines("git branch feat", &opts).is_empty(), "milder effects stay quiet");
    assert!(lines("git push -n origin", &opts).is_empty());
    let off = PreviewOptions { effect: false, ..PreviewOptions::default() };
    assert!(lines("git branch -D feat", &off).is_empty());
}

#[test]
fn explain_reports_the_line_effect() {
    let f = Fixture::spec();
    let req = f.req("git status && git branch -D feat");
    let e = explain_line(&req, &complete_line(&req), false);
    assert_eq!(e.effect.level, Some(Effect::Destroy));
    let rendered: Vec<String> = e.render().into_iter().map(|l| l.text).collect();
    assert!(rendered.iter().any(|l| l == "  effect: destroy · git branch -D"), "{rendered:#?}");

    let req = f.req("make");
    let e = explain_line(&req, &complete_line(&req), false);
    let rendered: Vec<String> = e.render().into_iter().map(|l| l.text).collect();
    assert!(rendered.iter().any(|l| l == "  effect: unknown: make"), "{rendered:#?}");
}

#[test]
fn malformed_effect_declarations_are_errors() {
    let codes = |src: &str| -> Vec<RuleCode> { parse_and_validate(src).1.into_iter().map(|d| d.code).collect() };
    assert!(codes("cmd x \"X\"\n  effect bogus\n").contains(&RuleCode::E02Structure));
    assert!(codes("cmd x \"X\"\n  effect read\n  effect write\n").contains(&RuleCode::E04Duplicate));
    assert!(codes("cmd x \"X\"\n  opt -f \"F\" effect\n").contains(&RuleCode::E02Structure));
    assert!(codes("cmd x \"X\"\n  arg <a> \"A\" effect read junk\n").contains(&RuleCode::E02Structure));
    assert!(codes("type effect\n  values a\n").contains(&RuleCode::E06ReservedWord));

    let (spec, diags) = parse_and_validate("cmd x \"X\"\n  opt -n [n] \"N\" dryrun effect write\n  arg [a] effect destroy\n");
    assert!(diags.is_empty(), "{diags:#?}");
    let cmd = &spec.cmds[0];
    assert!(cmd.opts[0].dryrun);
    assert_eq!(cmd.opts[0].effect, Some(Effect::Write));
    assert_eq!(cmd.args[0].effect, Some(Effect::Destroy));
}

#[test]
fn bundled_specs_declare_effects() {
    for (name, src) in BUNDLED_SPECS {
        let (_, diags) = parse_and_validate(src);
        assert!(!has_errors(&diags), "{name}: {diags:#?}");
    }
    let bundled = load_bundled_specs();
    let none: &[(String, Spec)] = &[];
    let mut diags = Vec::new();
    let f = Fixture::new(SpecIndex::merge_tiers(&[none, bundled.as_slice(), none], &mut diags));
    for (line, level) in [
        ("git log --oneline", Effect::Read),
        ("git status && git diff", Effect::Read),
        ("git commit -m msg", Effect::Write),
        ("git push origin main", Effect::Remote),
        ("git push --force origin main", Effect::Destroy),
        ("git reset --hard HEAD~1", Effect::Destroy),
        ("git clean -fd", Effect::Destroy),
        ("git clean -n", Effect::Read),
        ("rm -rf build", Effect::Destroy),
        ("mkdir -p out && ls out", Effect::Write),
    ] {
        let e = f.effect(line);
        assert_eq!(e.level, Some(level), "{line}: {e:?}");
        assert!(e.unknown.is_empty(), "{line}: {e:?}");
    }
}
