use std::collections::HashMap;
use std::path::Path;

use ezlo::engine::icons::Glyphs;
use ezlo::daemon::protocol::{
    build_wire_response, decode_explain, decode_wire_response, encode_client_msg, encode_explain,
    encode_wire_response, parse_client_line, ClientMsg, WireExplain,
};
use ezlo::daemon::protocol::{decode_settings, encode_settings, SessionOverrides, SessionSettings};
use ezlo::engine::explain::{ExplainLine, ExplainStyle};
use ezlo::engine::rank::UsageLookup;
use ezlo::engine::signature::SignatureHint;
use ezlo::engine::candidate::{Candidate, CandidateKind};
use ezlo::import::{check_path_allowed, parse_help_text, render_spec_file};
use ezlo::spec::{has_errors, parse_and_validate};
use ezlo::store::Store;
use unicode_width::UnicodeWidthStr;

#[test]
fn protocol_client_msg_roundtrip_with_unicode_cjk_and_emoji() {
    let mut vars = HashMap::new();
    vars.insert("LANG".to_string(), "ja_JP.UTF-8".to_string());
    vars.insert("DOCKER_HOST".to_string(), "unix:///var/run/docker.sock".to_string());

    let mut aliases = HashMap::new();
    aliases.insert("k".to_string(), "kubectl".to_string());
    aliases.insert("ll".to_string(), "ls -la".to_string());

    let messages = vec![
        ClientMsg::Hello {
            version: "1".to_string(),
            shell: "zsh".to_string(),
            session_id: "sess-123".to_string(),
            cols: 120,
        },
        ClientMsg::Env {
            cwd: "/home/zelda/プロジェクト/🚀".to_string(),
            vars,
        },
        ClientMsg::Alias { aliases },
        ClientMsg::Opts {
            hist_ignore_space: true,
            cols: Some(100),
            histfile: Some("/home/zelda/.zsh_history".to_string()),
        },
        ClientMsg::Query {
            id: 42,
            cursor: 14,
            buffer: "git commit -m '日本語 🚀 test'".to_string(),
        },
        ClientMsg::Accept {
            id: 42,
            row_index: 2,
        },
        ClientMsg::Explain {
            id: 43,
            cursor: 7,
            buffer: "git co 日本語".to_string(),
        },
        ClientMsg::Exec {
            cwd: "/tmp/日本語".to_string(),
            line: "echo 🚀".to_string(),
        },
        ClientMsg::Status { exit_code: 0 },
        ClientMsg::Set {
            overrides: SessionOverrides::default(),
        },
        ClientMsg::Set {
            overrides: SessionOverrides {
                rows: Some(4),
                history_rows: Some(0),
                min_chars: Some(1),
                signature: Some(false),
                diagnostics: Some(true),
                preview: Some(false),
                pane: Some(ezlo::engine::preview::pane::PaneMode::Off),
            },
        },
        ClientMsg::Ping,
        ClientMsg::Shutdown,
    ];

    for msg in messages {
        let encoded = encode_client_msg(&msg);
        let decoded = parse_client_line(&encoded).expect("decode client msg");
        assert_eq!(decoded, msg);
    }
}

#[test]
fn protocol_wire_response_roundtrip_and_cjk_emoji_display_width() {
    let candidates = vec![
        Candidate {
            label: "日本語-branch".to_string(),
            description: "CJK branch 🚀 description".to_string(),
            insert_value: "日本語-branch".to_string(),
            insert_text: "日本語-branch".to_string(),
            suffix: " ".to_string(),
            kind: CandidateKind::Arg,
            priority_tier: 1,
            score: 100.0,
            match_indices: vec![0, 1, 2],
            ..Default::default()
        },
        Candidate {
            label: "--force".to_string(),
            description: "Force operation".to_string(),
            insert_value: "--force".to_string(),
            insert_text: "--force".to_string(),
            suffix: " ".to_string(),
            kind: CandidateKind::Opt,
            deemphasized: true,
            priority_tier: 1,
            score: 50.0,
            match_indices: vec![0, 1, 2],
            ..Default::default()
        },
    ];

    let resp = build_wire_response(7, 4, 10, " --help", &candidates, 80, Glyphs::Nerd, 12);
    assert_eq!(resp.max_rows, 12);
    let encoded = encode_wire_response(&resp);
    let decoded = decode_wire_response(&encoded).expect("decode wire response");
    assert_eq!(decoded, resp);

    // Verify display_text fits within terminal column budget using unicode-width
    for row in &decoded.rows {
        let width = UnicodeWidthStr::width(row.display_text.as_str());
        assert!(
            width <= 80,
            "row display width {width} exceeded 80 columns: {:?}",
            row.display_text
        );
    }
}

#[test]
fn protocol_wire_response_carries_signature_hint() {
    let sig = SignatureHint {
        text: "timeout  <duration>  <command…>".to_string(),
        active: Some((21, 31)),
    };
    let resp = build_wire_response(3, 0, 0, "", &[], 80, Glyphs::Nerd, 6).with_signature(Some(&sig));
    let encoded = encode_wire_response(&resp);
    let header: Vec<&str> = encoded.lines().next().unwrap().split('\x1f').collect();
    assert_eq!(&header[10..13], ["timeout  <duration>  <command…>", "21", "31"]);
    let decoded = decode_wire_response(&encoded).expect("decode wire response");
    assert_eq!(decoded.signature, sig.text);
    assert_eq!(decoded.signature_active, Some((21, 31)));

    // No active slot: empty range fields.
    let resp = build_wire_response(3, 0, 0, "", &[], 80, Glyphs::Nerd, 6)
        .with_signature(Some(&SignatureHint { text: "cp  <src>".into(), active: None }));
    let decoded = decode_wire_response(&encode_wire_response(&resp)).unwrap();
    assert_eq!((decoded.signature.as_str(), decoded.signature_active), ("cp  <src>", None));

    // Older headers without the signature fields still decode.
    let decoded = decode_wire_response("R\x1f1\x1f0\x1f0\x1f\x1f0\x1f0\x1f6\x1f0\x1f0\n").unwrap();
    assert_eq!((decoded.signature.as_str(), decoded.signature_active), ("", None));
    assert_eq!(decoded.pane_col, 0, "no pane column: the pane goes below");
}

#[test]
fn protocol_rows_carry_their_full_text_only_when_truncated() {
    let cand = |label: &str| Candidate {
        label: label.to_string(),
        insert_value: label.to_string(),
        insert_text: label.to_string(),
        description: "what it does".to_string(),
        kind: CandidateKind::Sub,
        ..Default::default()
    };
    let long = "a-very-long-subcommand-name-that-cannot-possibly-fit-in-forty-columns";
    let cands = vec![cand("short"), cand(long)];
    let resp = build_wire_response(1, 0, 0, "", &cands, 40, Glyphs::Nerd, 6);
    assert_eq!(resp.rows[0].detail, "", "fits: nothing to repeat");
    assert_eq!(resp.rows[1].detail, format!("{long}  what it does"));
    let decoded = decode_wire_response(&encode_wire_response(&resp)).unwrap();
    assert_eq!(decoded.rows[1].detail, resp.rows[1].detail);
}

#[test]
fn protocol_rows_make_room_for_a_right_hand_pane() {
    use ezlo::engine::preview::pane::{PaneMode, PaneOptions, PanePosition};
    let row = |label: &str, preview: bool| Candidate {
        label: label.to_string(),
        insert_value: label.to_string(),
        insert_text: label.to_string(),
        description: "a description".to_string(),
        kind: CandidateKind::Arg,
        preview_cmd: preview.then(|| "echo hi".to_string()),
        ..Default::default()
    };
    let width = |resp: &ezlo::daemon::protocol::WireResponse| {
        resp.rows.iter().map(|r| UnicodeWidthStr::width(r.display_text.as_str())).max().unwrap_or(0)
    };
    let opts = PaneOptions { width: 120, ..PaneOptions::default() };
    let col = opts.right_column().expect("120 columns fit a right pane");
    assert_eq!(col, 72);
    assert!(opts.text_width() < 120 - col);

    // Most rows open the pane by themselves: every row is laid out up to the pane column.
    let cands = vec![row("web", true), row("api", true), row("db", false)];
    let resp = build_wire_response(1, 0, 0, "", &cands, 120, Glyphs::Nerd, 6).with_pane(&cands, &opts);
    assert_eq!(resp.pane_col, col);
    assert!(width(&resp) < col, "rows end before the pane: {}", width(&resp));
    let decoded = decode_wire_response(&encode_wire_response(&resp)).unwrap();
    assert_eq!(decoded, resp);

    // Only a minority does: full width, and those rows wait for the pane toggle (a pane below costs lines).
    let few = vec![row("web", true), row("a", false), row("b", false)];
    let full = build_wire_response(1, 0, 0, "", &few, 120, Glyphs::Nerd, 6).with_pane(&few, &opts);
    assert_eq!(full.pane_col, 0);
    assert!(width(&full) > col);
    assert_eq!(full.rows[0].pane, "manual");
    let on = PaneOptions { mode: PaneMode::On, ..opts.clone() };
    let forced = build_wire_response(1, 0, 0, "", &few, 120, Glyphs::Nerd, 6).with_pane(&few, &on);
    assert_eq!((forced.pane_col, forced.rows[0].pane.as_str()), (0, "auto"), "`on` keeps it, below");
    let off = PaneOptions { mode: PaneMode::Off, ..opts.clone() };
    assert_eq!(build_wire_response(1, 0, 0, "", &cands, 120, Glyphs::Nerd, 6).with_pane(&cands, &off).pane_col, 0);
    let bottom = PaneOptions { position: PanePosition::Bottom, ..opts.clone() };
    assert_eq!(bottom.right_column(), None);
    assert_eq!(PaneOptions { width: 80, ..opts }.right_column(), None, "80 columns: pane below");
}

#[test]
fn store_history_ghost_and_frecency_decay() {
    let store = Store::open_in_memory().expect("open in-memory store");

    store.record_execution_complete("/work/repo_a", "git checkout feature-alpha", 0, 1_700_000_000, true);

    store.record_execution_complete("/work/repo_b", "git checkout feature-beta", 0, 1_700_000_100, true);

    // Failed command (exit != 0) must not be offered as history ghost
    store.record_execution_complete("/work/repo_a", "git checkout broken-branch", 1, 1_700_000_200, true);

    // In `/work/repo_a`, cwd match (`feature-alpha`) beats newer global (`feature-beta`)
    let ghost_a = store.suggest_ghost("git checkout ", 13, "/work/repo_a", true);
    assert_eq!(ghost_a, "feature-alpha");

    // In `/work/other`, falls back to most recent successful global (`feature-beta`)
    let ghost_other = store.suggest_ghost("git checkout ", 13, "/work/other", true);
    assert_eq!(ghost_other, "feature-beta");

    // HIST_IGNORE_SPACE: line starting with space suppresses ghost when hist_ignore_space=true
    assert_eq!(
        store.suggest_ghost(" git checkout ", 14, "/work/repo_a", true),
        ""
    );

    // Usage tracking & frecency lookup
    store.record_accept("git checkout", "feature-alpha", "arg", "/work/repo_a", 1_700_000_300);
    let rec = store.lookup_usage("git checkout", "feature-alpha", "/work/repo_a");
    assert_eq!(rec.accepts, 1);
    assert!(rec.used_in_cwd);
}

#[test]
fn import_denylist_and_help_parser() {
    // Path denylist must reject /sbin, /usr/sbin, /usr/libexec, /System
    assert!(check_path_allowed(Path::new("/sbin/shutdown")).is_err());
    assert!(check_path_allowed(Path::new("/usr/sbin/reboot")).is_err());
    assert!(check_path_allowed(Path::new("/usr/libexec/helper")).is_err());
    assert!(check_path_allowed(Path::new("/System/Library/foo")).is_err());
    assert!(check_path_allowed(Path::new("/usr/bin/env")).is_ok());

    let sample_help = r#"
mytool - Fast deployment utility

Usage:
  mytool [OPTIONS] <COMMAND>

Options:
  -c, --config <FILE>      Path to config file
  -v, --verbose            Enable verbose output
      --port <PORT>        Listening port

Commands:
  serve      Start the server
  migrate    Run database migrations
"#;
    let scraped = parse_help_text(sample_help);
    assert_eq!(scraped.opts.len(), 3);
    assert_eq!(scraped.subcommands.len(), 2);
    assert_eq!(scraped.subcommands[0].name, "serve");

    let spec_src = render_spec_file("mytool", &scraped);
    let (_, diags) = parse_and_validate(&spec_src);
    assert!(
        !has_errors(&diags),
        "generated spec had validation errors: {diags:#?}\nSpec:\n{spec_src}"
    );
}

#[test]
fn learned_usage_candidates_for_untyped_args_opts_and_unknown_commands() {
    use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
    use ezlo::engine::{complete_line, CompletionRequest};
    use ezlo::spec::SpecIndex;
    use std::time::Duration;

    let src = r#"
cmd deploy
  opt -t --tag <tag> "Release tag"
  opt -v --verbose "Verbose output"
  arg <target>

cmd bare
  opt -q --quiet
"#;
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "{diags:#?}");
    let index = SpecIndex::from_single(spec);
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let store = Store::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("local.txt"), "x").expect("write");
    let cwd_str = dir.path().to_string_lossy().into_owned();

    store.record_execution_complete(&cwd_str, "deploy --tag v1.2.0", 0, 100, false);
    store.record_execution_complete(&cwd_str, "deploy prod-eu", 0, 110, false);
    store.record_accept("deploy", "staging-us", "arg", &cwd_str, 120);
    store.record_execution_complete(&cwd_str, "bare learned-pos", 0, 130, false);
    store.record_execution_complete(&cwd_str, "mycustom --profile work", 0, 140, false);
    store.record_execution_complete(&cwd_str, "mycustom run-job", 0, 150, false);

    let env = HashMap::new();
    let aliases = HashMap::new();
    let query = |line: &str| {
        complete_line(&CompletionRequest {
            line,
            cursor: line.chars().count(),
            cwd: dir.path(),
            env: &env,
            aliases: &aliases,
            wrappers: &[],
            index: &index,
            runner: &runner,
            cache: &cache,
            usage: &store,
            timeout: Duration::from_secs(1),
            now_ts: 200,
            allow_stale: false,
        })
    };

    // 1. Untyped option value works for both `--tag` and its short alias `-t`
    let opt_res = query("deploy --tag ");
    assert_eq!(opt_res.candidates[0].insert_value, "v1.2.0");
    assert_eq!(opt_res.candidates[0].kind, CandidateKind::Arg);
    let short_opt_res = query("deploy -t ");
    assert_eq!(short_opt_res.candidates[0].insert_value, "v1.2.0");

    // 2. Untyped positional arg offers learned values ordered by frecency (accept > single exec)
    let pos_res = query("deploy ");
    let pos_vals: Vec<&str> = pos_res
        .candidates
        .iter()
        .map(|c| c.insert_value.as_str())
        .collect();
    assert_eq!(pos_vals[0], "staging-us", "{pos_vals:?}");
    assert_eq!(pos_vals[1], "prod-eu", "{pos_vals:?}");
    assert_eq!(pos_res.candidates[0].kind, CandidateKind::Arg);

    // 3. Command with no declared positional arg surfaces learned values above path fallbacks
    let bare_res = query("bare ");
    assert_eq!(bare_res.candidates[0].insert_value, "learned-pos");
    assert!(
        bare_res.candidates.iter().any(|c| c.insert_value == "local.txt"),
        "expected path fallback after learned value"
    );

    // 4. Unknown command offers learned positional args (skipping `-` flags), learned flags on `-`,
    // and learned flag values after the flag.
    let unk_pos = query("mycustom ");
    assert_eq!(unk_pos.candidates[0].insert_value, "run-job");
    assert_eq!(unk_pos.candidates[0].kind, CandidateKind::Arg);
    assert!(
        !unk_pos.candidates.iter().any(|c| c.insert_value == "--profile"),
        "learned flags should be skipped when not typing `-`"
    );

    let unk_flag = query("mycustom -");
    assert_eq!(unk_flag.candidates[0].insert_value, "--profile");
    assert_eq!(unk_flag.candidates[0].kind, CandidateKind::Opt);

    let unk_flag_val = query("mycustom --profile ");
    assert_eq!(unk_flag_val.candidates[0].insert_value, "work");
    assert_eq!(unk_flag_val.candidates[0].kind, CandidateKind::Arg);
}

#[test]
fn repo_scoped_memory_shares_affinity_across_subdirectories() {
    use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
    use ezlo::engine::{complete_line, CompletionRequest};
    use ezlo::spec::SpecIndex;
    use ezlo::store::resolve_repo_root;
    use std::time::Duration;

    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    let sub_a = repo.join("crates/alpha");
    let sub_b = repo.join("crates/beta");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(&sub_a).expect("mkdir sub_a");
    std::fs::create_dir_all(&sub_b).expect("mkdir sub_b");
    std::fs::create_dir_all(&outside).expect("mkdir outside");

    let repo_str = repo.to_string_lossy().into_owned();
    let sub_a_str = sub_a.to_string_lossy().into_owned();
    let sub_b_str = sub_b.to_string_lossy().into_owned();
    let outside_str = outside.to_string_lossy().into_owned();

    assert_eq!(resolve_repo_root(&sub_a).as_deref(), Some(repo_str.as_str()));
    assert_eq!(resolve_repo_root(&sub_b).as_deref(), Some(repo_str.as_str()));
    assert_eq!(resolve_repo_root(&outside), None);

    let store = Store::open_in_memory().expect("store");
    store.record_execution_complete(&sub_a_str, "runner target-repo", 0, 1_000, false);
    store.record_execution_complete(&outside_str, "runner target-outside", 0, 1_000, false);

    // `sub_b` shares repo root with `sub_a`, so `used_in_cwd` is true in `sub_b` and false in `outside`
    assert!(store.lookup_usage("runner", "target-repo", &sub_b_str).used_in_cwd);
    assert!(!store.lookup_usage("runner", "target-outside", &sub_b_str).used_in_cwd);

    // Ghost text in `sub_b` prefers the same-repo history from `sub_a` over newer outside history
    store.record_execution_complete(&outside_str, "runner global-newer", 0, 2_000, false);
    assert_eq!(
        store.suggest_ghost("runner ", 7, &sub_b_str, false),
        "target-repo"
    );

    // History matches in `sub_b` rank same-repo (`sub_a`) ahead of `outside` and mark `in_cwd = true`
    let matches = store.history_matches("runner ", &sub_b_str, false, 5);
    assert_eq!(matches[0].line, "runner target-repo");
    assert!(matches[0].in_cwd);
    assert_eq!(matches[1].line, "runner global-newer");
    assert!(!matches[1].in_cwd);

    // Candidate ranking in `sub_b` boosts `target-repo` above `target-outside` via the repo cwd bonus
    let (spec, _) = parse_and_validate("cmd runner\n  arg <job>\n");
    let index = SpecIndex::from_single(spec);
    let runner = FakeGeneratorRunner::new();
    let cache = GeneratorCache::new();
    let env = HashMap::new();
    let aliases = HashMap::new();
    let res = complete_line(&CompletionRequest {
        line: "runner target-",
        cursor: 14,
        cwd: &sub_b,
        env: &env,
        aliases: &aliases,
        wrappers: &[],
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &store,
        timeout: Duration::from_secs(1),
        now_ts: 2_000,
        allow_stale: false,
    });
    assert_eq!(res.candidates[0].insert_value, "target-repo");
    assert_eq!(res.candidates[1].insert_value, "target-outside");
}


#[test]
fn protocol_explain_response_roundtrip_and_sanitizes_lines() {
    let lines = vec![
        ExplainLine {
            style: ExplainStyle::Header,
            text: "git checkout — Switch branches".to_string(),
        },
        ExplainLine {
            style: ExplainStyle::Token,
            text: "  co  subcommand\x1fwith\nnoise".to_string(),
        },
        ExplainLine {
            style: ExplainStyle::Error,
            text: "  ✗ unknown option '--bogus'".to_string(),
        },
    ];
    let wire = WireExplain::from_lines(7, &lines);
    let encoded = encode_explain(&wire);
    assert!(encoded.starts_with("E\x1f7\x1f3\n"), "{encoded:?}");
    assert_eq!(encoded.lines().count(), 4);
    let decoded = decode_explain(&encoded).expect("decode explain");
    assert_eq!(decoded, wire);
    assert_eq!(decoded.lines[0].0, "bold");
    assert_eq!(decoded.lines[1], (String::new(), "  co  subcommandwithnoise".to_string()));
    assert_eq!(decoded.lines[2].0, "fg=red");
}

#[test]
fn protocol_set_parses_overrides_leniently_and_settings_apply_them() {
    let us = '\x1f';
    // zsh sends booleans as on/off; unknown keys and junk are ignored; numbers are clamped.
    let line = format!("SET{us}rows=999{us}min_chars=1{us}signature=off{us}preview=on{us}future_key=1{us}junk");
    let Some(ClientMsg::Set { overrides }) = parse_client_line(&line) else {
        panic!("SET should parse");
    };
    assert_eq!(
        overrides,
        SessionOverrides {
            rows: Some(ezlo::daemon::protocol::MAX_WIRE_ROWS),
            history_rows: None,
            min_chars: Some(1),
            signature: Some(false),
            diagnostics: None,
            preview: Some(true),
            pane: None,
        }
    );
    assert_eq!(parse_client_line(&format!("SET{us}rows=0")), Some(ClientMsg::Set {
        overrides: SessionOverrides { rows: Some(1), ..SessionOverrides::default() }
    }));

    let config = ezlo::config::Config::default();
    let defaults = SessionSettings::from_config(&config);
    assert_eq!(defaults.rows, config.complete.dropdown.rows);
    assert!(defaults.diagnostics);
    let effective = defaults.with_overrides(&overrides);
    assert_eq!(effective.rows, ezlo::daemon::protocol::MAX_WIRE_ROWS);
    assert_eq!(effective.history_rows, config.complete.dropdown.history_rows, "unset keeps the config value");
    assert!(!effective.signature);
    assert_eq!(defaults.with_overrides(&SessionOverrides::default()), defaults);
}

#[test]
fn protocol_settings_reply_roundtrip() {
    let settings = SessionSettings {
        rows: 8,
        history_rows: 2,
        min_chars: 0,
        signature: false,
        diagnostics: true,
        preview: false,
    };
    let encoded = encode_settings(&settings);
    assert_eq!(
        encoded,
        "S\x1frows=8\x1fhistory_rows=2\x1fmin_chars=0\x1fsignature=0\x1fdiagnostics=1\x1fpreview=0\n"
    );
    let fallback = SessionSettings::from_config(&ezlo::config::Config::default());
    assert_eq!(decode_settings(&encoded, fallback), Some(settings));
    // Keys an older daemon doesn't send keep the fallback.
    let partial = decode_settings("S\x1frows=3", fallback).expect("decode");
    assert_eq!(partial.rows, 3);
    assert_eq!(partial.min_chars, fallback.min_chars);
    assert_eq!(decode_settings("R\x1f1", fallback), None);
}

#[test]
fn protocol_translate_request_and_reply_roundtrip() {
    use ezlo::daemon::protocol::{decode_translate, encode_translate, TranslateState, WireTranslate};

    for line in ["# list files by size", ""] {
        let msg = ClientMsg::Translate { id: 4, line: line.to_owned() };
        assert_eq!(parse_client_line(&encode_client_msg(&msg)), Some(msg));
    }

    let ok = WireTranslate {
        id: 4,
        state: TranslateState::Ok,
        lines: vec!["for f in *; do".into(), "  echo \"$f\"\x1f".into(), "done".into()],
    };
    let raw = encode_translate(&ok);
    assert!(raw.starts_with("T\x1f4\x1fok\x1f3\n"), "{raw:?}");
    let back = decode_translate(&raw).expect("decode T");
    assert_eq!(back.lines[1], "  echo \"$f\"", "fields are sanitized");
    assert_eq!((back.id, back.state, back.lines.len()), (4, TranslateState::Ok, 3));

    let err = WireTranslate { id: 5, state: TranslateState::Err, lines: vec!["timed out after 20s".into()] };
    assert_eq!(decode_translate(&encode_translate(&err)), Some(err));
    assert_eq!(decode_translate("T\x1f5\x1fmaybe\x1f0\n"), None);
}
