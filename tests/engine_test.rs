use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ezlo::engine::complete_line;
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::recipe::{resolve_recipe_placeholders, substitute_recipe_template};
use ezlo::engine::CompletionRequest;
use ezlo::spec::{parse_and_validate, SpecIndex};
use ezlo::tui::{
    pick_viewport_height, sanitize_preview_ansi, PickApp, PickOptions, PickOutcome,
    PICK_GENERATOR_MIN_TTL,
};

const TEST_SPEC: &str = r#"
type container(host = opt.host | env.DOCKER_HOST)
  $ docker_list_containers
  cols id name state
  insert id
  show name state
  preview `docker inspect "$id"`
  cache 5s

type container:running = container where state = running
type container:stopped = container where state in (created, exited, dead)

cmd docker "Container runtime"
  opt -H --host <host> "Daemon socket" global

cmd docker rm "Remove containers"
  opt -f --force "Force removal"
  opt -v --volumes "Remove volumes"
  opt -n --limit <num> "Limit count"
  arg <container...> container:stopped
    when --force: container

cmd docker exec "Exec in container"
  opt -i --interactive
  opt -t --tty
  arg <container> container:running
  arg [command...]

  recipe "Shell into container"
    $ docker exec -it <container> <shell>
    arg <shell> = sh | bash | zsh
"#;

fn make_index(src: &str) -> SpecIndex {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

#[test]
fn engine_short_clustering_and_attached_value() {
    let index = make_index(TEST_SPEC);
    let runner = FakeGeneratorRunner::new();
    runner.set_response(
        "docker_list_containers",
        vec![
            "c1\tweb\trunning".to_string(),
            "c2\tdb\texited".to_string(),
        ],
    );
    let cache = GeneratorCache::new();
    let usage = EmptyUsageLookup;
    let env = HashMap::new();
    let aliases = HashMap::new();
    let wrappers = vec!["sudo".to_string()];

    // `-fv` clusters `-f` (--force) and `-v` (--volumes). Because `-f` is active,
    // `when --force: container` is True, so both running (`c1`) and stopped (`c2`) are active!
    let line = "docker rm -fv ";
    let req = CompletionRequest {
        line,
        cursor: line.len(),
        cwd: Path::new("/tmp"),
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 1_700_000_000,
        allow_stale: false,
    };
    let res = complete_line(&req);
    let values: Vec<&str> = res.candidates.iter().map(|c| c.insert_value.as_str()).collect();
    assert!(values.contains(&"c1"), "expected running container c1 when -f is clustered in -fv: {values:?}");
    assert!(values.contains(&"c2"), "expected stopped container c2: {values:?}");
    assert!(!res.candidates.iter().any(|c| c.deemphasized));
}

#[test]
fn engine_when_pending_and_contradicted() {
    let src = r#"
type item
  $ list_items
  cols id state
  insert id
  show id state

type item:safe = item where state = safe
type item:all = item

cmd tool
  opt -f --force
  opt --mode <m> = fast | slow
  arg <target> item:safe
    when --force: item:all
    when !--force and --mode=fast: item:all
"#;
    let index = make_index(src);
    let runner = FakeGeneratorRunner::new();
    runner.set_response(
        "list_items",
        vec![
            "safe_1\tsafe".to_string(),
            "danger_2\tdanger".to_string(),
        ],
    );
    let cache = GeneratorCache::new();
    let usage = EmptyUsageLookup;
    let env = HashMap::new();
    let aliases = HashMap::new();
    let wrappers = Vec::new();

    // Without `--force`, `when --force: item:all` is PENDING, so `danger_2` is included
    // as de-emphasized with a "requires --force" hint!
    let line = "tool ";
    let res = complete_line(&CompletionRequest {
        line,
        cursor: line.len(),
        cwd: Path::new("/tmp"),
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 100,
        allow_stale: false,
    });
    let safe_cand = res.candidates.iter().find(|c| c.insert_value == "safe_1").expect("safe_1");
    assert!(!safe_cand.deemphasized);
    let danger_cand = res.candidates.iter().find(|c| c.insert_value == "danger_2").expect("danger_2");
    assert!(danger_cand.deemphasized);
    assert!(
        danger_cand.description.contains("requires --force"),
        "expected 'requires --force' in description, got: {}",
        danger_cand.description
    );

    // Refinements `item:safe` and `item:all` must share the base `item` generator cache!
    assert_eq!(runner.call_count(), 1, "expected exactly 1 generator run due to shared base type cache");
}

#[test]
fn engine_long_opt_eq_and_double_dash_terminator() {
    let src = r#"
cmd deploy
  opt --env <env> = dev | staging | prod
  opt --verbose
  arg <file> path
"#;
    let index = make_index(src);
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let usage = EmptyUsageLookup;
    let env = HashMap::new();
    let aliases = HashMap::new();
    let wrappers = Vec::new();

    // Completing `deploy --env=st` should complete `staging` and set replace_start after `=`
    let line = "deploy --env=st";
    let res = complete_line(&CompletionRequest {
        line,
        cursor: line.len(),
        cwd: Path::new("/tmp"),
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 100,
        allow_stale: false,
    });
    assert_eq!(res.replace_start, "deploy --env=".len());
    assert_eq!(res.candidates[0].insert_value, "staging");

    // After `--`, `--` prefix should NOT offer option flags (`--env`, `--verbose`)
    let line2 = "deploy -- --";
    let res2 = complete_line(&CompletionRequest {
        line: line2,
        cursor: line2.len(),
        cwd: Path::new("/tmp"),
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 100,
        allow_stale: false,
    });
    assert!(
        !res2.candidates.iter().any(|c| c.insert_value == "--verbose"),
        "options must not be offered after `--` terminator"
    );
}

#[test]
fn engine_inherited_global_options_aliases_and_wrappers() {
    let index = make_index(TEST_SPEC);
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let usage = EmptyUsageLookup;
    let env = HashMap::new();
    let mut aliases = HashMap::new();
    aliases.insert("drm".to_string(), "docker rm".to_string());
    let wrappers = vec!["sudo".to_string(), "env".to_string()];

    // `sudo env FOO=1 drm -` expands `drm` -> `docker rm`, skips `sudo` and `env FOO=1`,
    // and includes both `docker rm` options (`-f`, `--force`) and inherited global (`-H`, `--host`)
    let line = "sudo env FOO=1 drm -";
    let res = complete_line(&CompletionRequest {
        line,
        cursor: line.len(),
        cwd: Path::new("/tmp"),
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 100,
        allow_stale: false,
    });
    let labels: Vec<&str> = res.candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.iter().any(|l| l.contains("--force")), "expected --force in {labels:?}");
    assert!(labels.iter().any(|l| l.contains("--host")), "expected inherited global --host in {labels:?}");
}

#[test]
fn engine_quoting_on_insert() {
    let src = r#"
type branch
  $ list_branches
  cols name
  insert name

cmd git checkout
  arg <branch>
"#;
    let index = make_index(src);
    let runner = FakeGeneratorRunner::new();
    runner.set_response("list_branches", vec!["feature/my branch".to_string()]);
    let cache = GeneratorCache::new();
    let usage = EmptyUsageLookup;
    let env = HashMap::new();
    let aliases = HashMap::new();
    let wrappers = Vec::new();

    let line = "git checkout ";
    let res = complete_line(&CompletionRequest {
        line,
        cursor: line.len(),
        cwd: Path::new("/tmp"),
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 100,
        allow_stale: false,
    });
    assert_eq!(res.candidates.len(), 1);
    assert_eq!(res.candidates[0].insert_value, "feature/my branch");
    assert_eq!(res.candidates[0].insert_text, "'feature/my branch'");
}

#[test]
fn recipe_placeholder_dependency_order_and_type_inference() {
    let src = r#"
type ns
  values default kube-system

type pod(namespace = opt.namespace)
  $ kubectl get pods -n "$namespace"
  cols name

cmd kubectl
  opt -n --namespace <ns> global

cmd kubectl logs
  arg <pod>

  recipe "Logs in namespace"
    $ kubectl logs <pod> -n <namespace>
"#;
    let index = make_index(src);
    let recipe = &index.recipes[0];
    let resolved = resolve_recipe_placeholders(recipe, &index);

    // Even though `<pod>` appears before `<namespace>` in the template, `<pod>`'s inferred type
    // `pod` has param `namespace = opt.namespace`, and `<namespace>` sits in `-n`'s value slot!
    // Therefore `<namespace>` must be filled BEFORE `<pod>` (SYNTAX.md §7.5).
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].name, "namespace");
    assert_eq!(resolved[1].name, "pod");
    assert_eq!(resolved[1].depends_on, vec!["namespace".to_string()]);

    let mut filled = HashMap::new();
    filled.insert("namespace".to_string(), vec!["kube-system".to_string()]);
    filled.insert("pod".to_string(), vec!["coredns-123".to_string()]);
    let cmd = substitute_recipe_template(&recipe.template, &filled);
    assert_eq!(cmd, "kubectl logs coredns-123 -n kube-system");
}

#[test]
fn tui_pick_app_stage1_stage2_variadic_and_abort() {
    let src = r#"
type file_item
  values "a file.txt" "b.txt" "c.txt"

cmd rm
  arg <files...> file_item

  recipe "Remove multiple files"
    $ rm -v <files>
    arg <files...> file_item
"#;
    let index = make_index(src);
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let opts = PickOptions {
        line: Some("rm ".to_string()),
        cursor: Some(3),
        query: None,
        recipe_id: None,
        run: false,
        rows: None,
        cwd: PathBuf::from("/tmp"),
        env: HashMap::new(),
        aliases: HashMap::new(),
        wrappers: Vec::new(),
    };

    let mut app = PickApp::new(&index, &runner, &cache, &opts);

    // Stage 1: press Enter to select "Remove multiple files"
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        None
    );

    // Stage 2: variadic placeholder `<files...>`: toggle first and second items with Space, then Enter
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        None
    );
    let outcome = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        outcome,
        Some(PickOutcome::Selected("rm -v 'a file.txt' b.txt".to_string()))
    );

    // Verify Ctrl-C aborts
    let mut app2 = PickApp::new(&index, &runner, &cache, &opts);
    assert_eq!(
        app2.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(PickOutcome::Aborted)
    );
}

#[test]
fn tui_sanitize_preview_ansi_preserves_sgr_and_strips_other_escapes() {
    let raw = "\x1b[32mgreen\x1b[0m\x1b[2J\x1b]0;window title\x07 normal";
    let clean = sanitize_preview_ansi(raw);
    assert_eq!(clean, "\x1b[32mgreen\x1b[0m normal");
}

const PICK_SPEC: &str = r#"
type svc
  $ list_services
  cols name state
  preview `show_service "$name"`

cmd svc
cmd svc restart
  arg <name> svc

  recipe "Restart service"
    $ svc restart <name>
"#;

fn pick_opts() -> PickOptions {
    PickOptions {
        line: Some("svc restart ".to_string()),
        cursor: Some(12),
        query: None,
        recipe_id: None,
        run: false,
        rows: None,
        cwd: PathBuf::from("/tmp"),
        env: HashMap::new(),
        aliases: HashMap::new(),
        wrappers: Vec::new(),
    }
}

fn svc_runner() -> FakeGeneratorRunner {
    let runner = FakeGeneratorRunner::new();
    runner.set_response(
        "list_services",
        vec![
            "alpha\trunning".to_string(),
            "bravo\tstopped".to_string(),
            "charlie\trunning".to_string(),
        ],
    );
    runner
}

fn press(app: &mut PickApp<'_>, code: KeyCode) -> Option<PickOutcome> {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(app: &mut PickApp<'_>, ch: char) -> Option<PickOutcome> {
    app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL))
}

fn type_str(app: &mut PickApp<'_>, s: &str) {
    for ch in s.chars() {
        press(app, KeyCode::Char(ch));
    }
}

fn list_service_calls(runner: &FakeGeneratorRunner) -> usize {
    runner
        .recorded_calls()
        .iter()
        .filter(|(cmd, _)| cmd.contains("list_services"))
        .count()
}

#[test]
fn tui_pick_navigation_wraps_around() {
    let index = make_index(PICK_SPEC);
    let runner = svc_runner();
    let cache = GeneratorCache::new();
    let opts = pick_opts();
    let mut app = PickApp::new(&index, &runner, &cache, &opts);

    // Single recipe: wrapping stays on it.
    press(&mut app, KeyCode::Up);
    assert_eq!(app.selected_index(), 0);
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.selected_index(), 0);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.selected_index(), 2, "Up on the first entry jumps to the last");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.selected_index(), 0, "Down on the last entry jumps to the first");
    ctrl(&mut app, 'n');
    assert_eq!(app.selected_index(), 1);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.selected_index(), 2, "PageDown clamps instead of wrapping");
}

#[test]
fn tui_pick_session_cache_reuses_generator_rows_while_typing() {
    let index = make_index(PICK_SPEC);
    let opts = pick_opts();

    let runner = svc_runner();
    let cache = GeneratorCache::with_min_ttl(PICK_GENERATOR_MIN_TTL);
    let mut app = PickApp::new(&index, &runner, &cache, &opts);
    press(&mut app, KeyCode::Enter);
    type_str(&mut app, "brv");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(list_service_calls(&runner), 1, "generator runs once per session");
    ctrl(&mut app, 'r');
    assert_eq!(list_service_calls(&runner), 2, "Ctrl-R forces a refresh");

    // Without the session floor, an uncached type re-runs on every keystroke.
    let runner = svc_runner();
    let cache = GeneratorCache::new();
    let mut app = PickApp::new(&index, &runner, &cache, &opts);
    press(&mut app, KeyCode::Enter);
    type_str(&mut app, "br");
    assert_eq!(list_service_calls(&runner), 3);
}

#[test]
fn tui_pick_query_editing_keys() {
    let index = make_index(PICK_SPEC);
    let runner = svc_runner();
    let cache = GeneratorCache::new();
    let opts = pick_opts();
    let mut app = PickApp::new(&index, &runner, &cache, &opts);
    press(&mut app, KeyCode::Enter);

    type_str(&mut app, "zz zz");
    ctrl(&mut app, 'w');
    ctrl(&mut app, 'u');
    type_str(&mut app, "brav");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Some(PickOutcome::Selected("svc restart bravo".to_string()))
    );
}

#[test]
fn tui_pick_preview_discards_stale_results() {
    let index = make_index(PICK_SPEC);
    let runner = svc_runner();
    let cache = GeneratorCache::new();
    let opts = pick_opts();
    let mut app = PickApp::new(&index, &runner, &cache, &opts);
    press(&mut app, KeyCode::Enter);

    let first = app.take_preview_request(true).expect("preview for first row");
    assert!(first.command.contains("show_service"));
    assert!(app.take_preview_request(true).is_none(), "unchanged selection needs no rerun");

    press(&mut app, KeyCode::Down);
    let second = app.take_preview_request(true).expect("preview for second row");
    assert_ne!(first.id, second.id);

    app.apply_preview(first.id, &["stale".to_string()]);
    assert_eq!(app.preview_text(), None);
    app.apply_preview(second.id, &["fresh".to_string()]);
    assert_eq!(app.preview_text(), Some("fresh"));
}

#[test]
fn tui_pick_viewport_height_honors_rows() {
    assert_eq!(pick_viewport_height(40, None), 16);
    assert_eq!(pick_viewport_height(100, None), 20);
    assert_eq!(pick_viewport_height(40, Some(5)), 9);
    assert_eq!(pick_viewport_height(40, Some(0)), 5);
    assert_eq!(pick_viewport_height(12, Some(50)), 12);
}

#[test]
fn tui_ansi_to_text_converts_sgr_to_styles() {
    use ratatui::style::{Color, Modifier};
    let text = ezlo::tui::ansi_to_text("\x1b[1;32mok\x1b[0m plain\n\x1b[38;5;208mx\ty");
    assert_eq!(text.lines.len(), 2);
    let first = &text.lines[0].spans;
    assert_eq!(first[0].content, "ok");
    assert_eq!(first[0].style.fg, Some(Color::Indexed(2)));
    assert!(first[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(first[1].content, " plain");
    assert_eq!(first[1].style.fg, None);
    let second = &text.lines[1].spans;
    assert_eq!(second[0].content, "x    y");
    assert_eq!(second[0].style.fg, Some(Color::Indexed(208)));
    assert!(text.lines.iter().flat_map(|l| &l.spans).all(|s| !s.content.contains('\x1b')));
}
