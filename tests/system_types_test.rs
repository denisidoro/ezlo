use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::{complete_line, CompletionRequest, CompletionResult};
use ezlo::spec::span::RuleCode;
use ezlo::spec::{parse_and_validate, SpecIndex};

const SPEC: &str = r#"
cmd kill "Send a signal"
  opt -s <sig> signal "Signal to send"
  arg <pid...> pid

cmd killsafe
  arg <sig> signal where number in (9, 15)

cmd which
  arg <name> cmd

cmd printenv
  arg <var> env

cmd chown
  arg <owner> user

cmd chgrp
  arg <grp> group

cmd connect
  arg <host> host

cmd ifup
  arg <dev> interface

cmd untyped
  arg <signal>
"#;

fn make_index(src: &str) -> SpecIndex {
    let (spec, diags) = parse_and_validate(src);
    assert!(diags.is_empty(), "spec diagnostics: {diags:#?}");
    SpecIndex::from_single(spec)
}

struct Fixture {
    index: SpecIndex,
    runner: FakeGeneratorRunner,
    env: HashMap<String, String>,
    aliases: HashMap<String, String>,
}

impl Fixture {
    fn new(src: &str) -> Self {
        Self {
            index: make_index(src),
            runner: FakeGeneratorRunner::new(),
            env: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    fn complete(&self, line: &str) -> CompletionResult {
        let cache = GeneratorCache::new();
        let wrappers = vec!["sudo".to_string(), "env".to_string()];
        complete_line(&CompletionRequest {
            line,
            cursor: line.chars().count(),
            cwd: Path::new("/"),
            env: &self.env,
            aliases: &self.aliases,
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

fn values(res: &CompletionResult) -> Vec<&str> {
    res.candidates.iter().map(|c| c.insert_value.as_str()).collect()
}

#[test]
fn signal_type_lists_posix_signals_with_numbers() {
    let fx = Fixture::new(SPEC);
    let res = fx.complete("kill -s TE");
    let term = res
        .candidates
        .iter()
        .find(|c| c.insert_value == "TERM")
        .unwrap_or_else(|| panic!("TERM missing: {:?}", values(&res)));
    assert!(term.description.contains("15"), "{term:?}");
    assert_eq!(term.columns.first().map(String::as_str), Some("15"));
}

#[test]
fn builtin_where_clause_filters_system_rows() {
    let fx = Fixture::new(SPEC);
    let res = fx.complete("killsafe ");
    let mut vals = values(&res);
    vals.sort();
    assert_eq!(vals, ["KILL", "TERM"]);
}

#[test]
fn builtin_system_type_rejects_unknown_columns() {
    let (_, diags) = parse_and_validate("cmd k\n  arg <sig> signal where nope = 1\n");
    assert!(
        diags.iter().any(|d| d.code == RuleCode::E05Unresolved && d.message.contains("nope")),
        "{diags:#?}"
    );
    let (_, diags) = parse_and_validate("cmd k\n  arg <p> pid(x = env.X)\n");
    assert!(diags.iter().any(|d| d.message.contains("does not accept parameter")), "{diags:#?}");
}

#[test]
fn cmd_is_a_type_reference_but_not_a_declarable_name() {
    let (_, diags) = parse_and_validate("type mycmd = cmd where kind = alias\n\ncmd w\n  arg <c> cmd\n  opt -e <prog> cmd\n");
    assert!(diags.is_empty(), "{diags:#?}");
    let (_, diags) = parse_and_validate("type cmd\n  values a b\n");
    assert!(diags.iter().any(|d| d.code == RuleCode::E06ReservedWord), "{diags:#?}");
}

#[test]
fn declared_type_shadows_builtin() {
    let fx = Fixture::new("type signal\n  values TERM KILL\n\ncmd k\n  arg <sig> signal\n");
    let res = fx.complete("k ");
    let mut vals = values(&res);
    vals.sort();
    assert_eq!(vals, ["KILL", "TERM"]);
}

#[test]
fn system_types_need_explicit_reference() {
    let fx = Fixture::new(SPEC);
    let res = fx.complete("untyped ");
    assert!(!values(&res).contains(&"TERM"), "{:?}", values(&res));
}

#[test]
fn pid_type_parses_process_table() {
    let fx = Fixture::new(SPEC);
    fx.runner.set_response(
        "ps -axo",
        vec![
            "    1 root     systemd         /sbin/init".to_string(),
            " 4242 zelda    /usr/bin/node   node server.js --port 3000".to_string(),
        ],
    );
    let res = fx.complete("kill ");
    let node = res
        .candidates
        .iter()
        .find(|c| c.insert_value == "4242")
        .unwrap_or_else(|| panic!("pid missing: {:?}", values(&res)));
    assert_eq!(node.label, "4242");
    assert!(node.description.contains("node server.js"), "{node:?}");
    assert!(node.description.starts_with("node · zelda"), "{node:?}");
}

#[test]
fn cmd_type_lists_path_executables_and_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("ezlotool");
    fs::write(&tool, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(dir.path().join("ezlodata"), "").unwrap();

    let mut fx = Fixture::new(SPEC);
    fx.env.insert("PATH".into(), dir.path().to_string_lossy().into_owned());
    fx.aliases.insert("ezloalias".into(), "ls -la".into());
    let res = fx.complete("which ezlo");
    let vals = values(&res);
    assert!(vals.contains(&"ezlotool"), "{vals:?}");
    assert!(vals.contains(&"ezloalias"), "{vals:?}");
    assert!(!vals.contains(&"ezlodata"), "{vals:?}");
    let alias = res.candidates.iter().find(|c| c.insert_value == "ezloalias").unwrap();
    assert_eq!(alias.description, "alias · ls -la");
}

#[test]
fn env_type_lists_variables_with_values() {
    let mut fx = Fixture::new(SPEC);
    fx.env.insert("EDITOR".into(), "nvim".into());
    let res = fx.complete("printenv ED");
    let ed = res.candidates.iter().find(|c| c.insert_value == "EDITOR").expect("EDITOR");
    assert_eq!(ed.description, "nvim");
}

#[test]
fn host_type_reads_ssh_config_and_known_hosts() {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    fs::create_dir(&ssh).unwrap();
    fs::write(ssh.join("config"), "Host ezlobox *.wild\n  HostName 10.1.2.3\n").unwrap();
    fs::write(ssh.join("known_hosts"), "ezloknown.example,10.9.9.9 ssh-ed25519 AAAA\n").unwrap();

    let mut fx = Fixture::new(SPEC);
    fx.env.insert("HOME".into(), home.path().to_string_lossy().into_owned());
    let res = fx.complete("connect ezlo");
    let vals = values(&res);
    assert!(vals.contains(&"ezlobox"), "{vals:?}");
    assert!(vals.contains(&"ezloknown.example"), "{vals:?}");
    assert!(!vals.iter().any(|v| v.contains('*')), "{vals:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn user_group_and_interface_types_read_system_tables() {
    let fx = Fixture::new(SPEC);
    assert!(values(&fx.complete("chown roo")).contains(&"root"));
    assert!(values(&fx.complete("chgrp roo")).contains(&"root"));
    if Path::new("/sys/class/net/lo").exists() {
        assert!(values(&fx.complete("ifup l")).contains(&"lo"));
    }
}

fn assign_fixture() -> Fixture {
    let mut fx = Fixture::new(SPEC);
    fx.env.insert("EDITOR".into(), "nvim".into());
    fx.env.insert("PATH".into(), "/usr/bin:/bin".into());
    fx
}

#[test]
fn leading_assignment_value_offers_current_value_and_paths() {
    let fx = assign_fixture();
    let res = fx.complete("EDITOR=");
    assert_eq!(res.replace_start, 7);
    assert_eq!(res.candidates.first().map(|c| c.insert_value.as_str()), Some("nvim"));
    assert!(values(&res).contains(&"usr/"), "{:?}", values(&res));
    assert!(res.known_root, "assignments must not fall back to native completion");
}

#[test]
fn colon_list_assignment_completes_last_segment() {
    let fx = assign_fixture();
    let line = "PATH=$PATH:/us";
    let res = fx.complete(line);
    assert_eq!(res.replace_start, line.len() - 3);
    assert_eq!(res.replace_end, line.len());
    assert!(values(&res).contains(&"/usr/"), "{:?}", values(&res));

    let res = fx.complete("PATH=");
    let first = res.candidates.first().expect("candidates");
    assert_eq!(first.insert_text, "$PATH");
    assert_eq!(first.suffix, ":");
}

#[test]
fn assignment_after_env_wrapper_and_before_command() {
    let fx = assign_fixture();
    let res = fx.complete("env EDITOR=");
    assert_eq!(res.candidates.first().map(|c| c.insert_value.as_str()), Some("nvim"));
    let res = fx.complete("FOO=1 EDITOR=nv");
    assert_eq!(values(&res).first(), Some(&"nvim"));
}

#[test]
fn export_completes_names_then_values() {
    let fx = assign_fixture();
    let res = fx.complete("export ED");
    let ed = res.candidates.first().expect("candidates");
    assert_eq!(ed.label, "EDITOR");
    assert_eq!(ed.insert_text, "EDITOR=");
    assert!(ed.suffix.is_empty());
    assert_eq!(ed.description, "nvim");

    let res = fx.complete("export ");
    assert!(values(&res).contains(&"PATH="));

    let res = fx.complete("export EDITOR=");
    assert_eq!(res.replace_start, "export EDITOR=".len());
    assert_eq!(values(&res).first(), Some(&"nvim"));
}

#[test]
fn uppercase_command_prefix_offers_variable_names() {
    let fx = assign_fixture();
    let res = fx.complete("EDI");
    assert_eq!(values(&res).first(), Some(&"EDITOR="));
    let res = fx.complete("ki");
    assert!(!values(&res).contains(&"EDITOR="));
}

#[test]
fn parameters_complete_inside_any_word() {
    let mut fx = Fixture::new(SPEC);
    fx.env.insert("USER".into(), "zelda".into());
    fx.env.insert("XDG_CONFIG_HOME".into(), "/c".into());
    // A command without a spec still completes `$NAME`, replacing only the name.
    let res = fx.complete("echo $US");
    assert_eq!(values(&res)[0], "USER");
    assert_eq!((res.replace_start, res.replace_end), (6, 8));
    assert_eq!(res.candidates[0].description, "zelda");
    // Braced names get the closing brace; mid-word parameters work too.
    let res = fx.complete("ls \"${XDG_C");
    assert_eq!(values(&res)[0], "XDG_CONFIG_HOME}");
    let res = fx.complete("cat a/$XDG_C");
    assert_eq!(values(&res)[0], "XDG_CONFIG_HOME");
    // Single quotes and escapes keep `$` literal.
    assert!(!values(&fx.complete("echo '$US")).contains(&"USER"));
    assert!(!values(&fx.complete("echo \\$US")).contains(&"USER"));
}
