//! The sandboxed side of a probe: resource limits, Landlock + seccomp (Linux) or sandbox-exec
//! (macOS), then `exec` of the target binary.

use super::probe::check_path_allowed;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Runs inside `ezlo internal-probe-exec <scratch_dir> <binary> [args...]`
pub fn run_internal_probe_exec(scratch_dir: &Path, binary: &Path, args: &[String]) -> Result<()> {
    use nix::sys::resource::{setrlimit, Resource};
    use std::os::unix::process::CommandExt;

    check_path_allowed(binary)?;

    // New session & process group so parent can killpg on timeout
    let _ = nix::unistd::setsid();

    // Resource limits: CPU = 2s, FSIZE = 512 KiB
    setrlimit(Resource::RLIMIT_CPU, 2, 2).context("failed to set RLIMIT_CPU")?;
    setrlimit(Resource::RLIMIT_FSIZE, 512 * 1024, 512 * 1024)
        .context("failed to set RLIMIT_FSIZE")?;

    apply_os_sandbox(scratch_dir)?;

    let err = Command::new(binary)
        .args(args)
        .current_dir(scratch_dir)
        .env("LC_ALL", "C")
        .env("PAGER", "cat")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .exec();

    Err(anyhow::Error::from(err).context("failed to exec target binary in sandbox"))
}

#[cfg(target_os = "linux")]
fn apply_os_sandbox(scratch_dir: &Path) -> Result<()> {
    use landlock::{
        Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };
    use nix::libc;
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule};

    let abi = ABI::V3;
    let read_access = AccessFs::from_read(abi);
    let all_access = AccessFs::from_all(abi);

    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all_access)
        .context("Landlock sandbox is not supported by this kernel; refusing to run unsandboxed. Write a .ezlo spec manually.")?
        .create()
        .context("Failed to create Landlock ruleset; refusing to run unsandboxed.")?
        .add_rule(PathBeneath::new(
            PathFd::new("/").context("Failed to open / for Landlock")?,
            read_access,
        ))
        .context("Failed to add read-only / rule to Landlock")?
        .add_rule(PathBeneath::new(
            PathFd::new(scratch_dir).context("Failed to open scratch dir for Landlock")?,
            all_access,
        ))
        .context("Failed to add scratch dir rule to Landlock")?
        .restrict_self()
        .context("Failed to enforce Landlock sandbox; refusing to run unsandboxed.")?;

    if matches!(status.ruleset, RulesetStatus::NotEnforced) {
        bail!("Landlock sandbox was not enforced by the kernel; refusing to execute probe unsandboxed. Write a .ezlo spec manually.");
    }

    let arch = get_seccomp_arch()?;
    let blocked_syscalls = [
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
    ];
    let rules: std::collections::BTreeMap<i64, Vec<SeccompRule>> = blocked_syscalls
        .into_iter()
        .map(|sys| (sys, Vec::new()))
        .collect();

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .context("Failed to build seccomp network filter")?;

    let prog: BpfProgram = filter
        .try_into()
        .context("Failed to compile seccomp filter to BPF")?;
    seccompiler::apply_filter(&prog)
        .context("Failed to apply seccomp filter; refusing to run unsandboxed.")?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn get_seccomp_arch() -> Result<seccompiler::TargetArch> {
    #[cfg(target_arch = "x86_64")]
    {
        Ok(seccompiler::TargetArch::x86_64)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Ok(seccompiler::TargetArch::aarch64)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        bail!("Unsupported architecture for seccomp sandbox; write a .ezlo spec manually.")
    }
}

#[cfg(target_os = "macos")]
fn apply_os_sandbox(_scratch_dir: &Path) -> Result<()> {
    // On macOS, sandbox-exec wraps the outer invocation in `run_sandboxed_probe`.
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        bail!("macOS sandbox-exec is unavailable; refusing to execute probe unsandboxed. Write a .ezlo spec manually.");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn apply_os_sandbox(_scratch_dir: &Path) -> Result<()> {
    bail!("No OS sandbox available on this platform; refusing to execute probe unsandboxed. Write a .ezlo spec manually.")
}
