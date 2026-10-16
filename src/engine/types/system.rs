//! Builtin OS types (`cmd`, `env`, `pid`, `signal`, `port`, `user`, `group`, `host`,
//! `interface`): standard primitives `.ezlo` files reference without a shell generator.
//!
//! Every type yields rows whose first column (`value`) is inserted; the remaining columns (see
//! [`builtin_type_cols`]) are shown and usable in `where`/`rank` clauses.

use super::clause::matches_clauses;
use super::format::continuation_suffix;
use super::{TypeEvalContext, TypeRow};
use crate::engine::generator::{CacheKey, CacheLookup};
use crate::engine::line::shell_quote;
use crate::spec::ast::Clause;
use crate::spec::validate::builtin_type_cols;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Upper bound of executables collected from `$PATH`.
const MAX_CMD_ENTRIES: usize = 20_000;
/// Longest environment value shown as a description.
const MAX_ENV_VALUE_CHARS: usize = 200;

type Rows = Arc<Vec<Vec<String>>>;

pub(super) fn complete_builtin_system(
    kind: &str,
    ctx: &TypeEvalContext<'_>,
    where_chain: &[&[Clause]],
    rank: Option<&[Clause]>,
) -> Vec<TypeRow> {
    let Some(cols) = builtin_type_cols(kind) else {
        return Vec::new();
    };
    let cols: Vec<String> = cols.iter().map(|c| c.to_string()).collect();
    let rows = system_rows(kind, ctx);
    rows.iter()
        .filter(|row| where_chain.iter().all(|clauses| matches_clauses(row, &cols, clauses)))
        .filter_map(|row| {
            let deemphasized = rank.is_some_and(|clauses| !matches_clauses(row, &cols, clauses));
            system_row(&cols, row, deemphasized)
        })
        .collect()
}

fn system_rows(kind: &str, ctx: &TypeEvalContext<'_>) -> Rows {
    match kind {
        "cmd" => cmd_rows(ctx),
        "env" => Arc::new(env_rows(ctx.env)),
        "pid" => cached(ctx, kind, Vec::new(), Duration::from_secs(2), || pid_rows(ctx)),
        "signal" => Arc::new(signal_rows()),
        "port" => cached(ctx, kind, Vec::new(), Duration::from_secs(2), || port_rows(ctx)),
        "user" => cached(ctx, kind, Vec::new(), Duration::from_secs(30), || user_rows(ctx)),
        "group" => cached(ctx, kind, Vec::new(), Duration::from_secs(30), || {
            read_to_string("/etc/group").map(|t| parse_group(&t)).unwrap_or_default()
        }),
        "host" => {
            let home = env_var(ctx.env, "HOME").unwrap_or_default();
            let params = vec![("HOME".to_string(), home.clone())];
            cached(ctx, kind, params, Duration::from_secs(30), || host_rows(Path::new(&home)))
        }
        "interface" => cached(ctx, kind, Vec::new(), Duration::from_secs(10), || interface_rows(ctx)),
        _ => Arc::new(Vec::new()),
    }
}

fn system_row(cols: &[String], row: &[String], rank_deemphasized: bool) -> Option<TypeRow> {
    let value = row.first().filter(|v| !v.is_empty())?;
    let columns: Vec<String> = row.iter().skip(1).cloned().collect();
    let description = columns
        .iter()
        .filter(|c| !c.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" · ");
    let raw_columns = cols
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.clone(), row.get(idx).cloned().unwrap_or_default()))
        .collect();
    Some(TypeRow {
        insert_quoted: shell_quote(value),
        suffix: continuation_suffix(value).to_string(),
        insert_value: value.clone(),
        label: value.clone(),
        description,
        is_dir: false,
        is_file: false,
        rank_deemphasized,
        pending_hint: None,
        raw_columns,
        preview_cmd: None,
        columns,
        style: None,
        icon: None,
    })
}

/// Rows of `kind` from the generator cache, recomputed with `fetch` when missing or stale.
fn cached(
    ctx: &TypeEvalContext<'_>,
    kind: &str,
    params: Vec<(String, String)>,
    ttl: Duration,
    fetch: impl FnOnce() -> Vec<Vec<String>>,
) -> Rows {
    let key = CacheKey {
        base_type: format!("builtin:{kind}"),
        params,
        cwd: PathBuf::new(),
    };
    match ctx.cache.lookup(&key) {
        CacheLookup::Fresh(rows) => rows,
        CacheLookup::Stale(_) | CacheLookup::Miss => ctx.cache.insert(key, fetch(), ttl),
    }
}

/// Variable from the calling shell's environment, falling back to the daemon's own.
fn env_var(env: &HashMap<String, String>, name: &str) -> Option<String> {
    env.get(name).cloned().or_else(|| std::env::var(name).ok())
}

fn read_to_string(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Aliases of the session (uncached, they change per shell) followed by `$PATH` executables.
fn cmd_rows(ctx: &TypeEvalContext<'_>) -> Rows {
    let path = env_var(ctx.env, "PATH").unwrap_or_default();
    let params = vec![("PATH".to_string(), path.clone())];
    let executables = cached(ctx, "cmd", params, Duration::from_secs(10), || path_executables(&path));
    let mut aliases: Vec<(&String, &String)> = ctx.aliases.iter().collect();
    aliases.sort();
    let mut seen = HashSet::new();
    let mut out: Vec<Vec<String>> = aliases
        .into_iter()
        .filter(|(name, _)| seen.insert(name.as_str()))
        .map(|(name, val)| vec![name.clone(), "alias".to_string(), val.clone()])
        .collect();
    out.extend(executables.iter().filter(|row| !seen.contains(row[0].as_str())).cloned());
    Arc::new(out)
}

/// Executables in `path` (colon-separated); the first directory providing a name wins.
pub fn path_executables(path: &str) -> Vec<Vec<String>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| {
                std::fs::metadata(e.path())
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            if out.len() >= MAX_CMD_ENTRIES {
                return out;
            }
            if seen.insert(name.clone()) {
                out.push(vec![name, "command".to_string(), dir.to_string()]);
            }
        }
    }
    out
}

/// Environment variable names with their current value.
pub fn env_rows(env: &HashMap<String, String>) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = env
        .iter()
        .map(|(name, val)| vec![name.clone(), truncate_chars(val, MAX_ENV_VALUE_CHARS)])
        .collect();
    rows.sort();
    rows
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

const PS_COMMAND: &str = "ps -axo pid=,user=,comm=,args=";

fn pid_rows(ctx: &TypeEvalContext<'_>) -> Vec<Vec<String>> {
    let lines = ctx.runner.run_shell(PS_COMMAND, ctx.cwd, ctx.env, &[], ctx.timeout);
    parse_ps(&lines)
}

/// Parses `ps -axo pid=,user=,comm=,args=` lines into `pid name user args` rows, newest first.
pub fn parse_ps(lines: &[String]) -> Vec<Vec<String>> {
    let mut rows: Vec<(u64, Vec<String>)> = lines
        .iter()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut next = || {
                let (word, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                rest = tail.trim_start();
                word
            };
            let pid = next();
            let user = next().to_string();
            let comm = next();
            let pid_num: u64 = pid.parse().ok()?;
            let args = rest.trim_end().to_string();
            if args.starts_with(PS_COMMAND) {
                return None;
            }
            let name = comm.rsplit('/').next().unwrap_or(comm).to_string();
            Some((pid_num, vec![pid.to_string(), name, user, args]))
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// POSIX signals as `name number description` rows (numbers follow the host OS).
pub fn signal_rows() -> Vec<Vec<String>> {
    let table: &[(&str, u8, &str)] = if cfg!(target_os = "macos") {
        &[
            ("HUP", 1, "Hangup / reload configuration"),
            ("INT", 2, "Interrupt (Ctrl-C)"),
            ("QUIT", 3, "Quit and dump core (Ctrl-\\)"),
            ("ILL", 4, "Illegal instruction"),
            ("TRAP", 5, "Trace/breakpoint trap"),
            ("ABRT", 6, "Abort"),
            ("EMT", 7, "Emulator trap"),
            ("FPE", 8, "Floating-point exception"),
            ("KILL", 9, "Kill (cannot be caught)"),
            ("BUS", 10, "Bus error"),
            ("SEGV", 11, "Segmentation fault"),
            ("SYS", 12, "Bad system call"),
            ("PIPE", 13, "Broken pipe"),
            ("ALRM", 14, "Alarm clock"),
            ("TERM", 15, "Terminate gracefully"),
            ("URG", 16, "Urgent socket condition"),
            ("STOP", 17, "Stop (cannot be caught)"),
            ("TSTP", 18, "Terminal stop (Ctrl-Z)"),
            ("CONT", 19, "Continue if stopped"),
            ("CHLD", 20, "Child status changed"),
            ("TTIN", 21, "Background read from terminal"),
            ("TTOU", 22, "Background write to terminal"),
            ("IO", 23, "I/O possible"),
            ("XCPU", 24, "CPU time limit exceeded"),
            ("XFSZ", 25, "File size limit exceeded"),
            ("VTALRM", 26, "Virtual timer expired"),
            ("PROF", 27, "Profiling timer expired"),
            ("WINCH", 28, "Window size changed"),
            ("INFO", 29, "Status request (Ctrl-T)"),
            ("USR1", 30, "User-defined signal 1"),
            ("USR2", 31, "User-defined signal 2"),
        ]
    } else {
        &[
            ("HUP", 1, "Hangup / reload configuration"),
            ("INT", 2, "Interrupt (Ctrl-C)"),
            ("QUIT", 3, "Quit and dump core (Ctrl-\\)"),
            ("ILL", 4, "Illegal instruction"),
            ("TRAP", 5, "Trace/breakpoint trap"),
            ("ABRT", 6, "Abort"),
            ("BUS", 7, "Bus error"),
            ("FPE", 8, "Floating-point exception"),
            ("KILL", 9, "Kill (cannot be caught)"),
            ("USR1", 10, "User-defined signal 1"),
            ("SEGV", 11, "Segmentation fault"),
            ("USR2", 12, "User-defined signal 2"),
            ("PIPE", 13, "Broken pipe"),
            ("ALRM", 14, "Alarm clock"),
            ("TERM", 15, "Terminate gracefully"),
            ("STKFLT", 16, "Coprocessor stack fault"),
            ("CHLD", 17, "Child status changed"),
            ("CONT", 18, "Continue if stopped"),
            ("STOP", 19, "Stop (cannot be caught)"),
            ("TSTP", 20, "Terminal stop (Ctrl-Z)"),
            ("TTIN", 21, "Background read from terminal"),
            ("TTOU", 22, "Background write to terminal"),
            ("URG", 23, "Urgent socket condition"),
            ("XCPU", 24, "CPU time limit exceeded"),
            ("XFSZ", 25, "File size limit exceeded"),
            ("VTALRM", 26, "Virtual timer expired"),
            ("PROF", 27, "Profiling timer expired"),
            ("WINCH", 28, "Window size changed"),
            ("IO", 29, "I/O possible"),
            ("PWR", 30, "Power failure"),
            ("SYS", 31, "Bad system call"),
        ]
    };
    table
        .iter()
        .map(|(name, num, desc)| vec![name.to_string(), num.to_string(), desc.to_string()])
        .collect()
}

/// Listening TCP ports (and bound UDP ports on Linux) as `port proto address process` rows.
fn port_rows(ctx: &TypeEvalContext<'_>) -> Vec<Vec<String>> {
    let proc_net = Path::new("/proc/net");
    let mut rows = if proc_net.join("tcp").exists() {
        ["tcp", "tcp6", "udp", "udp6"]
            .iter()
            .filter_map(|proto| {
                read_to_string(proc_net.join(proto)).map(|text| parse_proc_net(&text, proto))
            })
            .flatten()
            .collect()
    } else {
        let lines = ctx.runner.run_shell(
            "lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null",
            ctx.cwd,
            ctx.env,
            &[],
            ctx.timeout,
        );
        parse_lsof_listen(&lines)
    };
    let mut seen = HashSet::new();
    rows.retain(|row: &Vec<String>| seen.insert((row[0].clone(), row[1].clone())));
    rows.sort_by_key(|row| row[0].parse::<u32>().unwrap_or(u32::MAX));
    rows
}

/// Parses a Linux `/proc/net/{tcp,tcp6,udp,udp6}` table: TCP sockets in `LISTEN` and unconnected
/// UDP sockets.
pub fn parse_proc_net(text: &str, proto: &str) -> Vec<Vec<String>> {
    let listen_state = if proto.starts_with("tcp") { "0A" } else { "07" };
    let proto_label = proto.trim_end_matches('6');
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let local = fields.nth(1)?;
            let state = fields.nth(1)?;
            if state != listen_state {
                return None;
            }
            let (addr_hex, port_hex) = local.split_once(':')?;
            let port = u16::from_str_radix(port_hex, 16).ok()?;
            let address = decode_proc_addr(addr_hex)?;
            Some(vec![port.to_string(), proto_label.to_string(), address, String::new()])
        })
        .collect()
}

/// Decodes the little-endian hex address of `/proc/net` tables (IPv4 or IPv6).
fn decode_proc_addr(hex: &str) -> Option<String> {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect::<Option<_>>()?;
    // Each 32-bit word is stored in host (little-endian) order.
    let ordered: Vec<u8> = bytes.chunks(4).flat_map(|w| w.iter().rev().copied()).collect();
    match ordered.len() {
        4 => Some(std::net::Ipv4Addr::new(ordered[0], ordered[1], ordered[2], ordered[3]).to_string()),
        16 => {
            let arr: [u8; 16] = ordered.try_into().ok()?;
            Some(std::net::Ipv6Addr::from(arr).to_string())
        }
        _ => None,
    }
}

/// Parses `lsof -nP -iTCP -sTCP:LISTEN` output (macOS/BSD).
pub fn parse_lsof_listen(lines: &[String]) -> Vec<Vec<String>> {
    lines
        .iter()
        .filter(|l| !l.starts_with("COMMAND"))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let command = fields.first()?;
            let pid = fields.get(1)?;
            let name_idx = fields.iter().position(|f| f.contains(':') && !f.starts_with("TCP"))?;
            let name = fields[name_idx];
            let (address, port) = name.rsplit_once(':')?;
            port.parse::<u16>().ok()?;
            let address = address.trim_start_matches('[').trim_end_matches(']');
            Some(vec![
                port.to_string(),
                "tcp".to_string(),
                address.to_string(),
                format!("{command} ({pid})"),
            ])
        })
        .collect()
}

fn user_rows(ctx: &TypeEvalContext<'_>) -> Vec<Vec<String>> {
    let mut rows = read_to_string("/etc/passwd").map(|t| parse_passwd(&t)).unwrap_or_default();
    if cfg!(target_os = "macos") {
        // Regular macOS accounts live in Directory Services, not `/etc/passwd`.
        let lines = ctx.runner.run_shell("dscl . -list /Users UniqueID", ctx.cwd, ctx.env, &[], ctx.timeout);
        let known: HashSet<String> = rows.iter().map(|r| r[0].clone()).collect();
        rows.extend(lines.iter().filter_map(|line| {
            let (name, uid) = line.split_once(char::is_whitespace)?;
            (!known.contains(name)).then(|| {
                vec![name.to_string(), uid.trim().to_string(), String::new(), String::new(), String::new()]
            })
        }));
    }
    rows
}

/// Parses `/etc/passwd` into `user uid name home shell` rows (`name` is the first GECOS field).
pub fn parse_passwd(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 7 || fields[0].is_empty() {
                return None;
            }
            let gecos = fields[4].split(',').next().unwrap_or("");
            Some(vec![
                fields[0].to_string(),
                fields[2].to_string(),
                gecos.to_string(),
                fields[5].to_string(),
                fields[6].to_string(),
            ])
        })
        .collect()
}

/// Parses `/etc/group` into `group gid members` rows.
pub fn parse_group(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 3 || fields[0].is_empty() {
                return None;
            }
            let members = fields.get(3).copied().unwrap_or("");
            Some(vec![fields[0].to_string(), fields[2].to_string(), members.to_string()])
        })
        .collect()
}

/// Hosts from `~/.ssh/config`, `~/.ssh/known_hosts` and `/etc/hosts` (in that order).
fn host_rows(home: &Path) -> Vec<Vec<String>> {
    let ssh = home.join(".ssh");
    let mut rows = Vec::new();
    if let Some(text) = read_to_string(ssh.join("config")) {
        rows.extend(parse_ssh_config_hosts(&text));
    }
    if let Some(text) = read_to_string(ssh.join("known_hosts")) {
        rows.extend(parse_known_hosts(&text));
    }
    if let Some(text) = read_to_string("/etc/hosts") {
        rows.extend(parse_etc_hosts(&text));
    }
    let mut seen = HashSet::new();
    rows.retain(|row| seen.insert(row[0].clone()));
    rows
}

/// Concrete `Host` aliases of an ssh config (patterns with `*`, `?` or `!` are skipped).
pub fn parse_ssh_config_hosts(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
            key.eq_ignore_ascii_case("host").then_some(rest)
        })
        .flat_map(|rest| rest.trim_start_matches(['=', ' ', '\t']).split_whitespace())
        .filter(|h| !h.contains(['*', '?', '!']))
        .map(|h| vec![h.to_string(), "ssh config".to_string()])
        .collect()
}

/// Host names of `known_hosts` entries (hashed entries are unreadable and skipped).
pub fn parse_known_hosts(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let first = fields.next()?;
            let hosts = if first.starts_with('@') { fields.next()? } else { first };
            (!hosts.starts_with('#') && !hosts.starts_with('|')).then_some(hosts)
        })
        .flat_map(|hosts| hosts.split(','))
        .filter_map(|h| {
            // `[host]:port` entries for non-default ports.
            let host = match h.strip_prefix('[') {
                Some(rest) => rest.split_once(']')?.0,
                None => h,
            };
            (!host.is_empty() && !host.contains(['*', '?'])).then(|| vec![host.to_string(), "known_hosts".to_string()])
        })
        .collect()
}

/// Host names of `/etc/hosts`, described by their address.
pub fn parse_etc_hosts(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or("");
            let mut fields = line.split_whitespace();
            let addr = fields.next()?;
            Some(fields.map(move |h| vec![h.to_string(), format!("hosts {addr}")]))
        })
        .flatten()
        .collect()
}

/// Network interfaces from `/sys/class/net` (Linux) or `ifconfig -l` (macOS/BSD).
fn interface_rows(ctx: &TypeEvalContext<'_>) -> Vec<Vec<String>> {
    let sys = Path::new("/sys/class/net");
    if let Ok(entries) = std::fs::read_dir(sys) {
        let attr = |name: &str, file: &str| {
            read_to_string(sys.join(name).join(file))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let mut rows: Vec<Vec<String>> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .map(|name| {
                let state = attr(&name, "operstate");
                let address = attr(&name, "address");
                vec![name, state, address]
            })
            .collect();
        rows.sort();
        return rows;
    }
    ctx.runner
        .run_shell("ifconfig -l", ctx.cwd, ctx.env, &[], ctx.timeout)
        .iter()
        .flat_map(|line| line.split_whitespace())
        .map(|name| vec![name.to_string(), String::new(), String::new()])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn parses_ps_rows_newest_first_without_ps_itself() {
        let rows = parse_ps(&lines(
            "    1 root     systemd         /sbin/init splash\n  420 zelda    /usr/bin/zsh    -zsh\n  999 zelda    ps              ps -axo pid=,user=,comm=,args=",
        ));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ["420", "zsh", "zelda", "-zsh"]);
        assert_eq!(rows[1], ["1", "systemd", "root", "/sbin/init splash"]);
    }

    #[test]
    fn parses_proc_net_listen_sockets() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000\n   1: 0100007F:A2B4 0100007F:1F90 01 00000000:00000000";
        assert_eq!(parse_proc_net(tcp, "tcp"), vec![vec!["8080", "tcp", "127.0.0.1", ""]]);
        let tcp6 = "  sl  local_address rem_address st\n   0: 00000000000000000000000001000000:0016 00000000000000000000000000000000:0000 0A";
        assert_eq!(parse_proc_net(tcp6, "tcp6"), vec![vec!["22", "tcp", "::1", ""]]);
    }

    #[test]
    fn parses_lsof_listen_lines() {
        let rows = parse_lsof_listen(&lines(
            "COMMAND   PID  USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\nnode    4242 zelda   23u  IPv6 0xabc      0t0  TCP *:3000 (LISTEN)\npostgres 77 zelda 7u IPv6 0x1 0t0 TCP [::1]:5432 (LISTEN)",
        ));
        assert_eq!(rows[0], ["3000", "tcp", "*", "node (4242)"]);
        assert_eq!(rows[1], ["5432", "tcp", "::1", "postgres (77)"]);
    }

    #[test]
    fn parses_passwd_and_group() {
        let users = parse_passwd("# comment\nroot:x:0:0:root:/root:/bin/bash\nzelda:x:1000:1000:Zelda F,,,:/home/zelda:/bin/zsh\nbroken:x");
        assert_eq!(users, vec![
            vec!["root", "0", "root", "/root", "/bin/bash"],
            vec!["zelda", "1000", "Zelda F", "/home/zelda", "/bin/zsh"],
        ]);
        let groups = parse_group("wheel:x:10:root,zelda\nstaff:*:20:");
        assert_eq!(groups, vec![vec!["wheel", "10", "root,zelda"], vec!["staff", "20", ""]]);
    }

    #[test]
    fn parses_host_sources() {
        let cfg = parse_ssh_config_hosts("Host hyrule box\n  HostName 10.0.0.2\nHost *.corp !bad\nhost=alpha\n");
        let names: Vec<&str> = cfg.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(names, ["hyrule", "box", "alpha"]);

        let known = parse_known_hosts("github.com,140.82.1.1 ssh-ed25519 AAAA\n[git.example]:2222 ssh-rsa AAAA\n|1|hash= ssh-rsa AAAA\n@cert-authority *.corp ssh-rsa AAAA");
        let names: Vec<&str> = known.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(names, ["github.com", "140.82.1.1", "git.example"]);

        let hosts = parse_etc_hosts("127.0.0.1 localhost # loop\n::1 ip6-localhost ip6-loopback\n# 10.0.0.1 ignored");
        assert_eq!(hosts[0], ["localhost", "hosts 127.0.0.1"]);
        assert_eq!(hosts.len(), 3);
    }

    #[test]
    fn signal_table_has_common_signals() {
        let rows = signal_rows();
        let term = rows.iter().find(|r| r[0] == "TERM").expect("TERM");
        assert_eq!(term[1], "15");
        assert!(rows.iter().any(|r| r[0] == "USR1"));
        assert!(rows.iter().all(|r| r.len() == builtin_type_cols("signal").map_or(0, <[_]>::len)));
    }

    #[test]
    fn env_rows_are_sorted_and_truncated() {
        let env = HashMap::from([
            ("PATH".to_string(), "x".repeat(500)),
            ("EDITOR".to_string(), "nvim".to_string()),
        ]);
        let rows = env_rows(&env);
        assert_eq!(rows[0], ["EDITOR", "nvim"]);
        assert_eq!(rows[1][1].chars().count(), MAX_ENV_VALUE_CHARS);
    }
}
