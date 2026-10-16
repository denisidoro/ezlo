//! `ezlo check --drift`: live `--help` probing of fixture CLIs (shell scripts printing canned help).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const SPEC: &str = r#"
cmd fixtool "Fixture tool"
  opt -v --verbose "Verbose"
  opt --old "Gone upstream"
  opt --secret "Undocumented" hidden

cmd fixtool build "Build"
  opt -j <jobs> "Jobs"
"#;

/// `fixtool --help` lists a new flag and subcommand; `fixtool build --help` a new flag.
const FIXTURE: &str = r#"#!/bin/sh
if [ "$1" = build ]; then
  printf '%s\n' 'Usage: fixtool build' '' 'Options:' '  -j <JOBS>       Jobs' '      --release   Release mode'
  exit 0
fi
printf '%s\n' 'Usage: fixtool [OPTIONS] <COMMAND>' '' 'Commands:' '  build    Build things' '  deploy   Deploy things' '' 'Options:' '  -v, --verbose   Verbose' '  -q, --quiet     Quiet output' '  -h, --help      Print help'
"#;

const IN_SYNC_FIXTURE: &str = r#"#!/bin/sh
printf '%s\n' 'Usage: synctool' '' 'Options:' '  -v, --verbose   Verbose'
"#;

fn write_file(path: &Path, content: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Runs ezlo with config/cache/data and specs under `home`, and only `home/bin` on `PATH`.
fn ezlo(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ezlo"))
        .args(args)
        .env("HOME", home)
        .env("EZLO_CONFIG_DIR", home.join("config"))
        .env("EZLO_SPECS_DIR", home.join("specs"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("PATH", home.join("bin"))
        .output()
        .expect("spawn ezlo")
}

fn setup() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    write_file(&home.path().join("specs/fixtool.ezlo"), SPEC, 0o644);
    write_file(&home.path().join("specs/synctool.ezlo"), "cmd synctool\n  opt -v --verbose \"Verbose\"\n", 0o644);
    write_file(&home.path().join("bin/fixtool"), FIXTURE, 0o755);
    write_file(&home.path().join("bin/synctool"), IN_SYNC_FIXTURE, 0o755);
    home
}

/// The probe needs Landlock + seccomp on Linux; skip (rather than fail) where the kernel lacks it.
fn sandbox_unavailable(out: &Output) -> bool {
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    all.contains("Landlock") || all.contains("seccomp") || all.contains("sandbox-exec")
}

#[test]
fn drift_reports_added_and_removed_entries() {
    let home = setup();
    let out = ezlo(home.path(), &["check", "--drift", "fixtool"]);
    if sandbox_unavailable(&out) {
        eprintln!("skipping: probe sandbox unavailable");
        return;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "drift must fail the check\n{stdout}\n{stderr}");
    assert!(stderr.contains("drift detected in 2 command(s)"), "{stderr}");
    assert!(stdout.contains("+ flag -q, --quiet"), "{stdout}");
    assert!(stdout.contains("+ cmd  deploy"), "{stdout}");
    assert!(stdout.contains("- flag --old"), "{stdout}");
    assert!(!stdout.contains("--secret"), "hidden opts are exempt\n{stdout}");
    assert!(!stdout.contains("flag -h"), "-h/--help is never reported\n{stdout}");
    assert!(stdout.contains("fixtool build\n  + flag --release"), "{stdout}");
}

#[test]
fn drift_defaults_to_user_spec_roots_and_passes_when_in_sync() {
    let home = setup();
    let out = ezlo(home.path(), &["check", "--drift", "synctool"]);
    if sandbox_unavailable(&out) {
        eprintln!("skipping: probe sandbox unavailable");
        return;
    }
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("compared 1 command(s) against --help: no drift"));

    // No names: every root of the user specs (fixtool drifts, synctool doesn't).
    let out = ezlo(home.path(), &["check", "--drift"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(stdout.contains("fixtool\n"), "{stdout}");
    assert!(!stdout.contains("synctool"), "{stdout}");
}

#[test]
fn drift_skips_unknown_commands_and_missing_binaries() {
    let home = setup();
    let out = ezlo(home.path(), &["check", "--drift", "nospec"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("nospec: no spec declares `cmd nospec`"));

    fs::remove_file(home.path().join("bin/synctool")).unwrap();
    let out = ezlo(home.path(), &["check", "--drift", "synctool"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "a missing binary is skipped, not drift");
    assert!(stdout.contains("? skipped:") && stdout.contains("not found in PATH"), "{stdout}");
}
