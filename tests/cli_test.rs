use std::path::Path;
use std::process::{Command, Output};

/// Runs the ezlo binary with all config/cache/data dirs redirected into `home`.
fn ezlo(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ezlo"))
        .args(args)
        .env("HOME", home)
        .env("EZLO_CONFIG_DIR", home.join("config"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .expect("spawn ezlo")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn version_flag_prints_crate_version() {
    let home = tempfile::tempdir().expect("tempdir");
    for flag in ["--version", "-V"] {
        let out = ezlo(home.path(), &[flag]);
        assert!(out.status.success());
        assert_eq!(stdout(&out).trim(), format!("ezlo {}", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn init_emits_zsh_plugin_and_rejects_other_shells() {
    let home = tempfile::tempdir().expect("tempdir");
    for args in [&["init"][..], &["init", "zsh"]] {
        let out = ezlo(home.path(), args);
        assert!(out.status.success());
        assert!(stdout(&out).contains("ezlo zsh integration"));
    }
    for shell in ["bash", "fish", "tcsh"] {
        assert!(!ezlo(home.path(), &["init", shell]).status.success());
    }
}

#[test]
fn complete_prints_tsv() {
    let home = tempfile::tempdir().expect("tempdir");
    // `ls` comes from the bundled fs.ezlo.
    let out = stdout(&ezlo(home.path(), &["complete", "--line", "ls --hum"]));
    assert!(out.lines().any(|l| l.starts_with("--human-readable\t")), "{out}");
}

#[test]
fn parse_accepts_legacy_json_switch() {
    let home = tempfile::tempdir().expect("tempdir");
    let spec = home.path().join("app.ezlo");
    std::fs::write(&spec, "cmd app\n  opt -v --verbose\n").expect("write spec");
    let spec = spec.to_str().expect("utf-8 path");
    for args in [vec!["parse", spec], vec!["parse", "--json", spec]] {
        let out = ezlo(home.path(), &args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout(&out).trim_start().starts_with('{'));
    }
}

#[test]
fn complete_accepts_option_values_starting_with_dash() {
    let home = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["complete", "--json", "--line", "-x"],
        vec!["complete", "--json", "--line", ""],
        vec!["complete", "--json", "--cursor", "3", "git"],
        // Non-ASCII short-flag cluster used to slice mid-char and panic.
        vec!["complete", "--json", "--line", "git -aé"],
    ] {
        let out = ezlo(home.path(), &args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout(&out).trim_start().starts_with('['), "{args:?}: {}", stdout(&out));
    }
}

#[test]
fn unknown_subcommand_and_bad_values_fail() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(!ezlo(home.path(), &["bogus"]).status.success());
    assert!(!ezlo(home.path(), &["complete", "--cursor", "nope"]).status.success());
    assert!(!ezlo(home.path(), &[]).status.success());
}

#[test]
fn internal_probe_exec_is_hidden_and_validates_args() {
    let home = tempfile::tempdir().expect("tempdir");
    let help = ezlo(home.path(), &["--help"]);
    assert!(help.status.success());
    let help = stdout(&help);
    assert!(help.contains("complete") && !help.contains("internal-probe-exec"));

    let out = ezlo(home.path(), &["internal-probe-exec"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: ezlo internal-probe-exec"));
}
