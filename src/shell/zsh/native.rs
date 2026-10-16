use crate::engine::line::shell_quote;
use crate::engine::candidate::{Candidate, CandidateKind};
use nix::pty::openpty;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a native completion result is reused for the same line and cwd.
const NATIVE_CACHE_TTL: Duration = Duration::from_secs(5);
/// Keys are whole typed lines, so expired entries are dropped on every insert; this caps bursts.
const NATIVE_CACHE_MAX: usize = 64;

const BEGIN_MARKER: &str = "__EZLO_COMP_BEGIN__";
const END_MARKER: &str = "__EZLO_COMP_END__";

#[derive(Debug, Clone)]
struct CachedNative {
    candidates: Vec<Candidate>,
    fetched_at: Instant,
}

#[derive(Clone, Default)]
pub struct NativeZshPool {
    cache: Arc<Mutex<HashMap<(String, String), CachedNative>>>,
    worker: Arc<Mutex<Option<PtyZshWorker>>>,
    native_rc: Vec<String>,
}

impl NativeZshPool {
    pub fn new(native_rc: Vec<String>) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            worker: Arc::new(Mutex::new(None)),
            native_rc,
        }
    }

    pub fn complete(&self, line: &str, cwd: &Path, timeout: Duration) -> Vec<Candidate> {
        let key = (line.to_string(), cwd.to_string_lossy().to_string());
        if let Ok(guard) = self.cache.lock() {
            if let Some(entry) = guard.get(&key) {
                if entry.fetched_at.elapsed() < NATIVE_CACHE_TTL {
                    return entry.candidates.clone();
                }
            }
        }

        let Ok(mut wguard) = self.worker.lock() else {
            return Vec::new();
        };
        if wguard.is_none() {
            *wguard = PtyZshWorker::spawn(&self.native_rc);
        }
        let Some(worker) = wguard.as_mut() else {
            return Vec::new();
        };

        let result = match worker.query(line, cwd, timeout) {
            Some(cands) => cands,
            None => {
                *wguard = None;
                Vec::new()
            }
        };

        if let Ok(mut guard) = self.cache.lock() {
            guard.retain(|_, e| e.fetched_at.elapsed() < NATIVE_CACHE_TTL);
            if guard.len() >= NATIVE_CACHE_MAX {
                guard.clear();
            }
            guard.insert(
                key,
                CachedNative {
                    candidates: result.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }
        result
    }
}

struct PtyZshWorker {
    master_file: std::fs::File,
    child: Child,
}

impl Drop for PtyZshWorker {
    fn drop(&mut self) {
        let pid = self.child.id() as i32;
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PtyZshWorker {
    fn spawn(native_rc: &[String]) -> Option<Self> {
        let pty = openpty(None, None).ok()?;
        let slave_dup_in = pty.slave.try_clone().ok()?;
        let slave_dup_out = pty.slave.try_clone().ok()?;
        let slave_dup_err = pty.slave;

        let mut cmd = Command::new("zsh");
        cmd.arg("-f")
            .arg("-i")
            .env("TERM", "xterm-256color")
            .env("PS1", "")
            .env("PROMPT", "")
            .stdin(Stdio::from(slave_dup_in))
            .stdout(Stdio::from(slave_dup_out))
            .stderr(Stdio::from(slave_dup_err))
            .process_group(0);

        let child = cmd.spawn().ok()?;
        let mut master_file = std::fs::File::from(pty.master);

        let mut init_script = String::from(
            "stty -echo 2>/dev/null; unsetopt beep; PS1=''; PROMPT=''; autoload -Uz compinit; compinit -D 2>/dev/null;\n",
        );
        for rc in native_rc {
            init_script.push_str(&format!(
                "[[ -r {} ]] && source {} 2>/dev/null;\n",
                shell_quote(rc),
                shell_quote(rc)
            ));
        }
        init_script.push_str(
            r#"
typeset -ga _ezlo_worker_rows
_ezlo_compadd() {
  local -a hits descs
  local i item insert desc
  for item in "$@"; do
    [[ $item == -A || $item == -D || $item == -O ]] && { builtin compadd "$@"; return; }
  done
  builtin compadd -A hits -D descs "$@"
  for (( i = 1; i <= $#hits; i++ )); do
    insert=${hits[i]}
    item=${(Q)insert}
    desc=${descs[i]//$'\n'/ }
    [[ -n $item && $item != *$'\t'* ]] && _ezlo_worker_rows+=( "${item}"$'\t'"${insert}"$'\t'"${desc}" )
  done
  builtin compadd "$@"
}
_ezlo_cap_widget() {
  _ezlo_worker_rows=()
  functions[compadd]=$functions[_ezlo_compadd]
  { _main_complete } always { unfunction compadd 2>/dev/null }
  compstate[insert]=''
  compstate[list]=''
}
zle -C _ezlo_cap complete-word _ezlo_cap_widget
_ezlo_trigger_widget() {
  local orig_buf=$BUFFER
  local cwd_part=${orig_buf%%$'\x1f'*}
  local line_part=${orig_buf#*$'\x1f'}
  builtin cd -q -- "$cwd_part" 2>/dev/null
  BUFFER=$line_part
  CURSOR=${#BUFFER}
  zle _ezlo_cap
  print -r -- "__EZLO_COMP_BEGIN__"
  local r
  for r in "${_ezlo_worker_rows[@]}"; do
    print -r -- "__EZLO_ROW__${r}"
  done
  print -r -- "__EZLO_COMP_END__"
  BUFFER=''
  CURSOR=0
}
zle -N _ezlo_trigger_widget
bindkey '^X^K' _ezlo_trigger_widget
"#,
        );
        master_file.write_all(init_script.as_bytes()).ok()?;
        master_file.flush().ok()?;

        Some(Self {
            master_file,
            child,
        })
    }

    fn query(&mut self, line: &str, cwd: &Path, timeout: Duration) -> Option<Vec<Candidate>> {
        if line.contains('\n') || line.contains('\x1f') {
            return Some(Vec::new());
        }
        let payload = format!("\x15{}\x1f{}\x18\x0b", cwd.to_string_lossy(), line);
        self.master_file.write_all(payload.as_bytes()).ok()?;
        self.master_file.flush().ok()?;

        let deadline = Instant::now() + timeout;
        let mut buf = String::new();
        let mut temp = [0u8; 4096];

        while Instant::now() < deadline {
            let remaining_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(300) as u16;
            let mut pfd = [PollFd::new(self.master_file.as_fd(), PollFlags::POLLIN)];
            let ready = poll(&mut pfd, PollTimeout::from(remaining_ms)).ok()?;
            if ready == 0 {
                break;
            }
            let n = self.master_file.read(&mut temp).ok()?;
            if n == 0 {
                return None;
            }
            buf.push_str(&String::from_utf8_lossy(&temp[..n]));
            if buf.contains(END_MARKER) {
                break;
            }
        }

        let Some(start_pos) = buf.rfind(BEGIN_MARKER) else {
            return Some(Vec::new());
        };
        let after_start = &buf[start_pos + BEGIN_MARKER.len()..];
        let Some(end_pos) = after_start.find(END_MARKER) else {
            return Some(Vec::new());
        };
        let body = &after_start[..end_pos];

        let mut cands = Vec::new();
        for raw_line in body.lines() {
            let trimmed = raw_line.trim_end_matches('\r');
            if let Some(rest) = trimmed.strip_prefix("__EZLO_ROW__") {
                let mut parts = rest.split('\t');
                let label = parts.next().unwrap_or("").trim();
                let insert = parts.next().unwrap_or(label).trim();
                let desc = parts.next().unwrap_or("").trim();
                if !label.is_empty() {
                    let kind = if label.starts_with('-') {
                        CandidateKind::Opt
                    } else if label.ends_with('/') {
                        CandidateKind::Dir
                    } else {
                        CandidateKind::Arg
                    };
                    cands.push(Candidate {
                        label: label.to_string(),
                        description: desc.to_string(),
                        insert_value: label.to_string(),
                        insert_text: insert.to_string(),
                        suffix: if label.ends_with('/') {
                            String::new()
                        } else {
                            " ".to_string()
                        },
                        kind,
                        priority_tier: 2,
                        ..Default::default()
                    });
                }
            }
        }

        Some(cands)
    }
}
