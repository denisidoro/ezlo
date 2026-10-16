//! Synchronous client side of the daemon: spawning, status and shutdown (used by the CLI).

use super::protocol::{encode_client_msg, parse_pong, ClientMsg};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

pub fn spawn_detached_daemon(socket_path: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut cmd = Command::new(exe);
    // glibc keeps a malloc arena per thread that ever allocated (blocking-pool threads included)
    // and rarely returns their memory; cap them unless the caller chose a value.
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        cmd.env("MALLOC_ARENA_MAX", "2");
    }
    cmd.arg("daemon")
        .env("EZLO_SOCKET", socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("failed to spawn background daemon")?;
    Ok(())
}

/// Sends one message and reads one reply line.
fn request(stream: UnixStream, msg: &ClientMsg) -> Result<String> {
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    (&stream).write_all(encode_client_msg(msg).as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply)
}

pub fn query_daemon_status(socket_path: &Path) -> Result<String> {
    let stream = UnixStream::connect(socket_path).with_context(|| {
        format!("daemon is not running (no socket at {})", socket_path.display())
    })?;
    let reply = request(stream, &ClientMsg::Ping)?;
    let Some(pong) = parse_pong(&reply) else {
        bail!("unexpected daemon response: {}", reply.trim_end());
    };
    Ok(format!(
        "running (pid={}, socket={}, cmds={}, recipes={})",
        pong.pid,
        socket_path.display(),
        pong.cmds,
        pong.recipes
    ))
}

/// Asks a running daemon to exit; a missing daemon is not an error.
pub fn stop_daemon(socket_path: &Path) -> Result<()> {
    if let Ok(stream) = UnixStream::connect(socket_path) {
        // Best effort: the daemon may close the socket before replying.
        let _ = request(stream, &ClientMsg::Shutdown);
    }
    Ok(())
}
