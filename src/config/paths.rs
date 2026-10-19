//! Filesystem locations (XDG-aware, overridable via `EZLO_*` env vars) and spec-file checks.

use super::Config;
use nix::unistd::{Gid, Group, Uid, User};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

impl Config {
    pub fn config_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("EZLO_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty() {
                return PathBuf::from(xdg).join("ezlo");
            }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".config").join("ezlo")
    }

    pub fn config_file_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    pub fn user_specs_dir() -> PathBuf {
        Self::config_dir().join("specs")
    }

    pub fn cache_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("EZLO_CACHE_DIR") {
            return PathBuf::from(dir);
        }
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
            && !xdg.is_empty() {
                return PathBuf::from(xdg).join("ezlo");
            }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".cache").join("ezlo")
    }

    pub fn generated_specs_dir() -> PathBuf {
        Self::cache_dir().join("generated")
    }

    pub fn index_cache_path() -> PathBuf {
        Self::cache_dir().join("index.bin")
    }

    pub fn data_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("EZLO_DATA_DIR") {
            return PathBuf::from(dir);
        }
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.is_empty() {
                return PathBuf::from(xdg).join("ezlo");
            }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ezlo")
    }

    pub fn db_path() -> PathBuf {
        Self::data_dir().join("ezlo.db")
    }

    pub fn socket_path() -> PathBuf {
        if let Ok(sock) = std::env::var("EZLO_SOCKET")
            && !sock.is_empty() {
                return PathBuf::from(sock);
            }
        if let Ok(xdg_run) = std::env::var("XDG_RUNTIME_DIR")
            && !xdg_run.is_empty() {
                return PathBuf::from(xdg_run).join("ezlo.sock");
            }
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let uid = Uid::current().as_raw();
        PathBuf::from(tmp)
            .join(format!("ezlo-{uid}"))
            .join("ezlo.sock")
    }
}

/// Expands a leading `~/`, `$VAR/` or `${VAR}/` using the process environment.
pub fn expand_path(raw: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    if raw == "~" {
        return PathBuf::from(home);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return PathBuf::from(home).join(rest);
    }
    if let Some(after) = raw.strip_prefix('$') {
        let (name, rest) = match after.strip_prefix('{').and_then(|a| a.split_once('}')) {
            Some((n, r)) => (n, r),
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        if let Ok(val) = std::env::var(name) {
            return PathBuf::from(format!("{val}{rest}"));
        }
    }
    PathBuf::from(raw)
}

/// Security check (PROMPT.md §9):
/// Spec files must be owned by the current user and writable by nobody else: never world-writable,
/// and group-writable only when the group is the user's private group (umask 002 distros, where
/// e.g. `git checkout` creates 0664 files).
pub fn check_spec_file_security(path: &Path) -> Result<(), String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot stat '{}': {e}", path.display()))?;
    let current_uid = Uid::current();
    if meta.uid() != current_uid.as_raw() {
        return Err(format!(
            "skipping '{}': owned by uid {}, expected {current_uid}",
            path.display(),
            meta.uid()
        ));
    }
    let mode = meta.permissions().mode();
    let world_writable = mode & 0o002 != 0;
    let foreign_group_writable =
        mode & 0o020 != 0 && !is_private_group(Gid::from_raw(meta.gid()), current_uid);
    if world_writable || foreign_group_writable {
        return Err(format!(
            "skipping '{}': file is writable by other users (mode {:04o})",
            path.display(),
            mode & 0o777
        ));
    }
    Ok(())
}

/// True when `gid` is a user-private group of `uid`: same name as the user and no other members.
fn is_private_group(gid: Gid, uid: Uid) -> bool {
    let (Ok(Some(user)), Ok(Some(group))) = (User::from_uid(uid), Group::from_gid(gid)) else {
        return false;
    };
    group.name == user.name && group.mem.iter().all(|m| *m == user.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::getgid;

    fn spec_with_mode(mode: u32) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("x.ezlo");
        fs::write(&path, "cmd x\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
        (dir, path)
    }

    #[test]
    fn accepts_owner_only_writable() {
        let (_d, p) = spec_with_mode(0o644);
        assert!(check_spec_file_security(&p).is_ok());
    }

    #[test]
    fn rejects_world_writable() {
        let (_d, p) = spec_with_mode(0o646);
        assert!(check_spec_file_security(&p).is_err());
    }

    #[test]
    fn group_writable_follows_private_group_rule() {
        let (_d, p) = spec_with_mode(0o664);
        let private = is_private_group(getgid(), Uid::current());
        assert_eq!(check_spec_file_security(&p).is_ok(), private);
    }
}
