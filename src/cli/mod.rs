//! `ezlo` subcommand dispatch. Argument definitions live in [`args`].

mod args;

pub use args::*;

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

use crate::bundled::BUNDLED_SPECS;
use crate::config::{check_spec_file_security, Config};
use crate::daemon::index_loader::{build_index_cache, collect_spec_files, load_index_isolated, load_merged_index};
use crate::daemon::{query_daemon_status, run_daemon, spawn_detached_daemon, stop_daemon};
use crate::engine::generator::{GeneratorCache, GeneratorRunner, ShellGeneratorRunner};
use crate::engine::rank::{EmptyUsageLookup, UsageLookup};
use crate::engine::explain::explain_line;
use crate::engine::preview::preview_line;
use crate::engine::{complete_line, CompletionRequest};
use crate::history::search::merge_entries;
use crate::history::{dedupe_newest_first, parse_history, resolve_histfile};
use crate::import::{detect_drift, import_command, probe_help, render_drift, run_internal_probe_exec};
use crate::shell::{format_completions, zsh};
use crate::spec::span::Diagnostic;
use crate::spec::{has_errors, parse_and_validate};
use crate::store::{now_unix_ts, read_executions, resolve_repo_root, Store};
use crate::tui::{
    print_or_run_selected, run_pick_interactive, run_search_interactive, PickOptions, PickOutcome,
    SearchOptions, SearchOutcome, EXIT_CODE_ABORT, PICK_GENERATOR_MIN_TTL,
};

/// Hidden subcommand used by `ezlo import` to re-exec itself inside the sandbox. Dispatched
/// before argh so trailing args (e.g. `--help`) pass through verbatim and it stays out of help.
const PROBE_EXEC_CMD: &str = "internal-probe-exec";

pub fn run_cli() -> Result<()> {
    let mut raw = env::args_os().skip(1);
    match raw.next().as_deref().and_then(OsStr::to_str) {
        Some("--version" | "-V") => {
            println!("ezlo {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(PROBE_EXEC_CMD) => return cmd_probe_exec(raw),
        _ => {}
    }

    let cli: Cli = argh::from_env();
    match cli.command {
        Commands::Init(a) => cmd_init(a.shell.as_deref()),
        Commands::Check(a) if a.drift => cmd_check_drift(&a.files),
        Commands::Check(a) => cmd_check(&a.files),
        Commands::Parse(a) => cmd_parse(&a.file),
        Commands::Complete(a) => cmd_complete(a),
        Commands::Explain(a) => cmd_explain(a),
        Commands::Preview(a) => cmd_preview(a),
        Commands::Recall(a) => cmd_recall(a),
        Commands::Pick(a) => cmd_pick(a),
        Commands::Import(a) => cmd_import(&a.cmd),
        Commands::History(a) => match a.action {
            HistoryAction::Import(i) => cmd_history_import(i.path),
            HistoryAction::Search(s) => cmd_history_search(s),
        },
        Commands::Daemon(a) => cmd_daemon(a),
    }
}

/// Working directory and environment of the calling shell (inherited by the CLI process).
fn caller_context() -> (PathBuf, HashMap<String, String>) {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    (cwd, env::vars().collect())
}

fn cmd_probe_exec(mut raw: impl Iterator<Item = OsString>) -> Result<()> {
    const USAGE: &str = "usage: ezlo internal-probe-exec <scratch_dir> <binary> [args...]";
    let (Some(scratch_dir), Some(binary)) = (raw.next(), raw.next()) else {
        bail!(USAGE);
    };
    let args = raw
        .map(|a| {
            a.into_string()
                .map_err(|a| anyhow!("non-UTF-8 argument: {a:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    run_internal_probe_exec(Path::new(&scratch_dir), Path::new(&binary), &args)
}

/// Prints the zsh integration script. `shell` is optional (and must be `zsh`) so the usual
/// `eval "$(ezlo init zsh)"` keeps working.
fn cmd_init(shell: Option<&str>) -> Result<()> {
    if let Some(other) = shell.filter(|s| *s != "zsh") {
        bail!("unsupported shell `{other}`: ezlo only supports zsh");
    }
    let script = zsh::init_script();
    let mut out = io::stdout().lock();
    out.write_all(script.as_bytes())?;
    out.flush()?;
    Ok(())
}

fn cmd_check(files: &[PathBuf]) -> Result<()> {
    let mut had_errors = false;
    let mut checked = 0usize;

    if files.is_empty() {
        for (name, source) in BUNDLED_SPECS {
            checked += 1;
            had_errors |= report_diagnostics(&format!("<{name}>"), source).1;
        }
        Config::warn_unknown_keys();
        let config = Config::load();
        let dirs = std::iter::once(Config::generated_specs_dir()).chain(config.spec_dirs());
        for path in dirs.flat_map(|dir| collect_spec_files(&dir)) {
            checked += 1;
            had_errors |= check_one_file(&path)?;
        }
    } else {
        for path in files {
            checked += 1;
            had_errors |= check_one_file(path)?;
        }
    }

    if had_errors {
        bail!("validation failed");
    }
    println!("checked {checked} spec(s): ok");
    Ok(())
}

/// `ezlo check --drift [<cmd>...]`: compares the merged spec of each command with its live
/// `--help` output (see [`detect_drift`]); fails when any drift is found.
fn cmd_check_drift(cmds: &[PathBuf]) -> Result<()> {
    let config = Config::load();
    let (index, _) = load_merged_index(&config);
    let roots: Vec<String> = if cmds.is_empty() {
        user_spec_roots(&config)
    } else {
        cmds.iter().map(|c| c.to_string_lossy().into_owned()).collect()
    };
    if roots.is_empty() {
        bail!("no user-authored specs found; pass command names to check");
    }

    let (mut drifted, mut compared) = (0usize, 0usize);
    let mut out = io::stdout().lock();
    for root in &roots {
        if !index.cmds.iter().any(|c| c.path.len() == 1 && c.path[0] == *root) {
            eprintln!("{root}: no spec declares `cmd {root}`; skipped");
            continue;
        }
        let drifts = detect_drift(&index, root, &mut probe_help);
        compared += drifts.iter().filter(|d| d.skipped.is_none()).count();
        drifted += drifts.iter().filter(|d| d.has_drift()).count();
        out.write_all(render_drift(&drifts).as_bytes())?;
    }
    out.flush()?;

    if drifted > 0 {
        bail!("drift detected in {drifted} command(s)");
    }
    println!("compared {compared} command(s) against --help: no drift");
    Ok(())
}

/// Root command words declared by the user spec directories, deduplicated in file order.
fn user_spec_roots(config: &Config) -> Vec<String> {
    let mut seen = BTreeSet::new();
    config
        .spec_dirs()
        .iter()
        .flat_map(|dir| collect_spec_files(dir))
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|src| parse_and_validate(&src).0.cmds)
        .filter_map(|cmd| cmd.path.into_iter().next())
        .filter(|root| seen.insert(root.clone()))
        .collect()
}

/// Parses `source`, prints its diagnostics to stderr and returns the spec plus whether it has errors.
fn report_diagnostics(display_name: &str, source: &str) -> (crate::spec::ast::Spec, bool) {
    let (spec, diags) = parse_and_validate(source);
    diags
        .iter()
        .for_each(|d: &Diagnostic| eprint!("{}", d.render(display_name, source)));
    (spec, has_errors(&diags))
}

fn read_spec(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}

fn check_one_file(path: &Path) -> Result<bool> {
    if let Err(sec_err) = check_spec_file_security(path) {
        eprintln!("warning: {sec_err}");
    }
    let source = read_spec(path)?;
    Ok(report_diagnostics(&path.to_string_lossy(), &source).1)
}

fn cmd_parse(file: &Path) -> Result<()> {
    let source = read_spec(file)?;
    let (spec, failed) = report_diagnostics(&file.to_string_lossy(), &source);
    if failed {
        bail!("cannot emit AST JSON due to validation errors");
    }
    println!("{}", serde_json::to_string_pretty(&spec)?);
    Ok(())
}

fn cmd_complete(args: CompleteArgs) -> Result<()> {
    let line = args.line.or(args.positional_line).unwrap_or_default();
    let config = Config::load();
    let (index, _) = load_merged_index(&config);
    let store = Store::open(&Config::db_path()).ok();
    let usage: &dyn UsageLookup = match &store {
        Some(s) => s,
        None => &EmptyUsageLookup,
    };
    let (default_cwd, env_map) = caller_context();
    let cwd = args.cwd.unwrap_or(default_cwd);
    let line_chars = line.chars().count();

    let req = CompletionRequest {
        line: &line,
        cursor: args.cursor.unwrap_or(line_chars).min(line_chars),
        cwd: &cwd,
        env: &env_map,
        aliases: &HashMap::new(),
        wrappers: &config.line.wrappers,
        index: &index,
        runner: &ShellGeneratorRunner,
        cache: &GeneratorCache::new(),
        usage,
        timeout: config.generator_timeout_duration(),
        now_ts: now_unix_ts(),
        allow_stale: false,
    };
    let resp = complete_line(&req);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&resp.candidates)?);
    } else {
        print!("{}", format_completions(&resp));
    }
    Ok(())
}

/// One-shot `ezlo explain`. Without the daemon there are no shell aliases nor native zsh
/// candidates; commands without a spec are reported as falling back to native zsh.
fn cmd_explain(args: ExplainArgs) -> Result<()> {
    let line = args.line.or(args.positional_line).unwrap_or_default();
    let config = Config::load();
    let (index, _) = load_merged_index(&config);
    let store = Store::open(&Config::db_path()).ok();
    let usage: &dyn UsageLookup = match &store {
        Some(s) => s,
        None => &EmptyUsageLookup,
    };
    let (default_cwd, env_map) = caller_context();
    let cwd = args.cwd.unwrap_or(default_cwd);
    let line_chars = line.chars().count();

    let req = CompletionRequest {
        line: &line,
        cursor: args.cursor.unwrap_or(line_chars).min(line_chars),
        cwd: &cwd,
        env: &env_map,
        aliases: &HashMap::new(),
        wrappers: &config.line.wrappers,
        index: &index,
        runner: &ShellGeneratorRunner,
        cache: &GeneratorCache::new(),
        usage,
        timeout: config.generator_timeout_duration(),
        now_ts: now_unix_ts(),
        allow_stale: false,
    };
    let mut explanation = explain_line(&req, &complete_line(&req), false);
    if let Some(opts) = config.preview_options() {
        explanation.preview = preview_line(&req, &opts);
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&explanation)?);
    } else {
        let mut out = io::stdout().lock();
        for l in explanation.render() {
            writeln!(out, "{}", l.text)?;
        }
    }
    Ok(())
}

/// One-shot effect preview (the zsh widget gets it from the daemon with every query). Runs even
/// when `[preview] enabled = false`, since it was asked for explicitly.
fn cmd_preview(args: PreviewArgs) -> Result<()> {
    let line = args.line.or(args.positional_line).unwrap_or_default();
    let config = Config::load();
    let (index, _) = load_merged_index(&config);
    let (default_cwd, env_map) = caller_context();
    let cwd = args.cwd.unwrap_or(default_cwd);
    let line_chars = line.chars().count();
    let mut enabled = config.clone();
    enabled.preview.enabled = true;
    let opts = enabled.preview_options().unwrap_or_default();

    let req = CompletionRequest {
        line: &line,
        cursor: args.cursor.unwrap_or(line_chars).min(line_chars),
        cwd: &cwd,
        env: &env_map,
        aliases: &HashMap::new(),
        wrappers: &config.line.wrappers,
        index: &index,
        runner: &ShellGeneratorRunner,
        cache: &GeneratorCache::new(),
        usage: &EmptyUsageLookup,
        timeout: config.generator_timeout_duration(),
        now_ts: now_unix_ts(),
        allow_stale: false,
    };
    let lines = preview_line(&req, &opts);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&lines)?);
    } else {
        let mut out = io::stdout().lock();
        for l in &lines {
            writeln!(out, "{}", l.text)?;
        }
    }
    Ok(())
}

/// One-shot empty-prompt recall (the zsh widget asks the daemon instead).
fn cmd_recall(args: RecallArgs) -> Result<()> {
    let store = Store::open(&Config::db_path())?;
    let cwd = args.cwd.unwrap_or_else(|| caller_context().0);
    let matches = store.workflow_matches(&cwd.to_string_lossy(), false, now_unix_ts(), args.limit);
    if args.json {
        let rows: Vec<_> = matches
            .iter()
            .map(|m| serde_json::json!({ "line": m.line, "ts": m.ts, "in_cwd": m.in_cwd }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        let mut out = io::stdout().lock();
        for m in &matches {
            writeln!(out, "{}", m.line)?;
        }
    }
    Ok(())
}

fn cmd_pick(args: PickArgs) -> Result<()> {
    let config = Config::load();
    let (index, _) = load_merged_index(&config);
    let runner: Arc<dyn GeneratorRunner> = Arc::new(ShellGeneratorRunner);
    // Generators run once per session; filtering on keystrokes reuses their rows (Ctrl-R refreshes).
    let cache = GeneratorCache::with_min_ttl(PICK_GENERATOR_MIN_TTL);
    let (cwd, env) = caller_context();
    let run = args.run;

    let opts = PickOptions {
        line: args.line,
        cursor: args.cursor,
        query: args.query,
        recipe_id: args.recipe,
        run,
        rows: args.rows.or(config.pick_rows()),
        cwd,
        env,
        aliases: HashMap::new(),
        wrappers: config.line.wrappers,
    };

    match run_pick_interactive(&index, runner, &cache, &opts)? {
        PickOutcome::Selected(cmd) => print_or_run_selected(&cmd, run),
        PickOutcome::Aborted => std::process::exit(EXIT_CODE_ABORT),
    }
}

fn cmd_import(cmd_name: &str) -> Result<()> {
    let (out_path, scraped) = import_command(cmd_name)?;
    println!(
        "imported `{cmd_name}` ({} options, {} subcommands) -> {}",
        scraped.opts.len(),
        scraped.subcommands.len(),
        out_path.display()
    );
    Ok(())
}

fn cmd_history_import(path: Option<PathBuf>) -> Result<()> {
    let hist_path = path.or_else(|| {
        let histfile = env::var("HISTFILE").ok();
        let home = env::var("HOME").ok();
        resolve_histfile(histfile.as_deref(), home.as_deref())
    });
    let Some(hist_path) = hist_path else {
        bail!("could not find a shell history file; pass a path explicitly");
    };
    let store = Store::open(&Config::db_path())?;
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_owned());
    let count = store.import_histfile(&hist_path, &cwd)?;
    println!(
        "imported {count} history entries from {}",
        hist_path.display()
    );
    Ok(())
}

/// `ezlo history search` exit status when the picked line should run right away (`Ctrl-O`).
const EXIT_CODE_SEARCH_RUN: i32 = 3;

/// Executions read for `ezlo history search` (unique lines are usually far fewer).
const SEARCH_MAX_EXECUTIONS: usize = 50_000;

fn cmd_history_search(args: HistorySearchArgs) -> Result<()> {
    let home = env::var("HOME").ok();
    let histfile = args.histfile.or_else(|| {
        let histfile = env::var("HISTFILE").ok();
        resolve_histfile(histfile.as_deref(), home.as_deref())
    });
    let file_entries = histfile
        .and_then(|p| fs::read(p).ok())
        .map(|b| dedupe_newest_first(parse_history(&b, 0)))
        .unwrap_or_default();
    // A broken or locked database shouldn't take the shell's history search down with it.
    let store_rows = read_executions(&Config::db_path(), SEARCH_MAX_EXECUTIONS).unwrap_or_default();
    let entries = merge_entries(store_rows, file_entries, args.ignore_space);

    let cwd = caller_context().0;
    let opts = SearchOptions {
        query: args.query.unwrap_or_default(),
        rows: args.rows,
        repo_root: resolve_repo_root(&cwd),
        cwd: cwd.to_string_lossy().into_owned(),
        home,
        now_ts: now_unix_ts(),
    };
    match run_search_interactive(&entries, &opts)? {
        SearchOutcome::Edit(line) => print_or_run_selected(&line, false),
        SearchOutcome::Run(line) => {
            print_or_run_selected(&line, false)?;
            std::process::exit(EXIT_CODE_SEARCH_RUN)
        }
        SearchOutcome::Aborted => std::process::exit(EXIT_CODE_ABORT),
    }
}

fn cmd_daemon(args: DaemonArgs) -> Result<()> {
    let socket_path = args.socket.unwrap_or_else(Config::socket_path);

    if args.status {
        println!("{}", query_daemon_status(&socket_path)?);
        return Ok(());
    }
    if args.stop {
        stop_daemon(&socket_path)?;
        println!("stopped");
        return Ok(());
    }
    if args.detach {
        return spawn_detached_daemon(&socket_path);
    }
    if args.build_index {
        build_index_cache(&Config::load());
        return Ok(());
    }

    Config::warn_unknown_keys();
    let config = Config::load();
    let index = load_index_isolated(&config);
    let store = Store::open(&Config::db_path()).or_else(|_| Store::open_in_memory())?;
    let runner = Arc::new(ShellGeneratorRunner);

    // One runtime thread instead of one per core (saves worker stacks and allocator arenas).
    // Background work (generator refreshes, translations, panes) runs on `spawn_blocking`; only a
    // cold generator run (bounded by `generator_timeout`) can delay other shells' queries.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(run_daemon(socket_path, config, store, index, runner))
}
