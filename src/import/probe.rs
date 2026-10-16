//! Locating a binary and running it through the sandboxed `internal-probe-exec` re-entry.

use anyhow::{bail, Context, Result};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DENYLIST_PREFIXES: &[&str] = &["/sbin", "/usr/sbin", "/usr/libexec", "/System"];
const WALL_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_OUTPUT_BYTES: usize = 512 * 1024;

pub fn check_path_allowed(binary_path: &Path) -> Result<()> {
    let canonical = fs::canonicalize(binary_path).unwrap_or_else(|_| binary_path.to_path_buf());
    for path_to_check in [binary_path, canonical.as_path()] {
        let s = path_to_check.to_string_lossy();
        for prefix in DENYLIST_PREFIXES {
            if s.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')) {
                bail!(
                    "refusing to probe `{}`: path is under restricted system directory `{prefix}`. Write a .ezlo file manually instead.",
                    binary_path.display()
                );
            }
        }
    }
    Ok(())
}

pub fn resolve_binary(cmd: &str) -> Result<PathBuf> {
    let candidate = Path::new(cmd);
    if candidate.components().count() > 1 {
        if candidate.is_file() {
            check_path_allowed(candidate)?;
            return Ok(candidate.to_path_buf());
        }
        bail!("binary `{cmd}` not found");
    }

    let path_var = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path_var) {
        let full = dir.join(cmd);
        if full.is_file() {
            check_path_allowed(&full)?;
            return Ok(full);
        }
    }
    bail!("command `{cmd}` not found in PATH")
}

pub fn run_sandboxed_probe(binary: &Path, args: &[&str]) -> Result<String> {
    check_path_allowed(binary)?;

    let scratch_dir = env::temp_dir().join(format!("ezlo-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch_dir)?;

    let self_exe = env::current_exe().context("failed to determine ezlo executable path")?;
    let mut cmd = if cfg!(target_os = "macos") {
        let sandbox_bin = Path::new("/usr/bin/sandbox-exec");
        if !sandbox_bin.is_file() {
            let _ = fs::remove_dir_all(&scratch_dir);
            bail!("sandbox-exec not found on macOS; refusing to run unsandboxed. Write a .ezlo spec manually.");
        }
        let scratch_escaped = scratch_dir.to_string_lossy().replace('"', "\\\"");
        let profile = format!(
            "(version 1) (allow default) (deny file-write* (subpath \"/\")) (allow file-write* (subpath \"{scratch_escaped}\")) (deny network*)"
        );
        let mut c = Command::new(sandbox_bin);
        c.arg("-p").arg(profile).arg(&self_exe);
        c
    } else {
        Command::new(&self_exe)
    };

    cmd.arg("internal-probe-exec")
        .arg(&scratch_dir)
        .arg(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().context("failed to spawn sandboxed probe")?;
    let pid = child.id() as i32;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let out_handle = std::thread::spawn(move || read_capped(stdout_pipe));
    let err_handle = std::thread::spawn(move || read_capped(stderr_pipe));

    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() >= WALL_TIMEOUT {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_dir_all(&scratch_dir);
            bail!("probe timed out after {} ms", WALL_TIMEOUT.as_millis());
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let stdout_bytes = out_handle.join().unwrap_or_default();
    let stderr_bytes = err_handle.join().unwrap_or_default();
    let _ = fs::remove_dir_all(&scratch_dir);

    let stdout_str = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes).into_owned();

    if stderr_str.contains("Landlock") || stderr_str.contains("seccomp") || stderr_str.contains("refusing to") {
        bail!("{}", stderr_str.trim());
    }

    if !stdout_str.trim().is_empty() {
        Ok(stdout_str)
    } else if !stderr_str.trim().is_empty() {
        Ok(stderr_str)
    } else if status.success() {
        Ok(String::new())
    } else {
        bail!("probe exited with status {status}")
    }
}

/// Reads at most `MAX_OUTPUT_BYTES` from a child pipe; read errors just truncate the output.
fn read_capped(pipe: Option<impl Read>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(p) = pipe {
        let _ = p.take(MAX_OUTPUT_BYTES as u64).read_to_end(&mut buf);
    }
    buf
}
