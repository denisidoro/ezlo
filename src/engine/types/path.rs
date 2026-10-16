use super::format::{format_age, format_size};
use super::TypeRow;
use crate::engine::icons;
use crate::engine::line::shell_quote;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{DirEntry, Metadata};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const MAX_PATH_ENTRIES: usize = 5_000;
/// Max entries (files and subdirs, recursively) visited when sizing a single directory.
const DIR_SIZE_MAX_ENTRIES: usize = 100;
/// Total time budget for sizing directories within one completion request.
const DIR_SIZE_BUDGET: Duration = Duration::from_millis(15);

pub fn complete_builtin_path(
    kind: &str,
    typed_prefix: &str,
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Vec<TypeRow> {
    let dirs_only = kind == "dir";
    let (display_dir_prefix, resolved_dir, base_prefix) =
        split_and_resolve_path(typed_prefix, cwd, env);
    let show_hidden = base_prefix.starts_with('.');

    let Ok(entries) = std::fs::read_dir(&resolved_dir) else {
        return Vec::new();
    };
    let entries: Vec<DirEntry> = entries
        .flatten()
        .filter(|e| show_hidden || !e.file_name().to_string_lossy().starts_with('.'))
        .take(MAX_PATH_ENTRIES)
        .collect();
    // Sizing walks subtrees, so it's capped per directory and by a per-request deadline.
    let size_deadline = Instant::now() + DIR_SIZE_BUDGET;
    let now = SystemTime::now();

    let mut out = Vec::new();
    for entry in entries {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let path = entry.path();
        let Ok(lmeta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let meta = std::fs::metadata(&path).ok();
        let is_dir = meta.as_ref().is_some_and(Metadata::is_dir);
        if dirs_only && !is_dir {
            continue;
        }

        let mut candidate_val = format!("{display_dir_prefix}{name_str}");
        if is_dir {
            candidate_val.push('/');
        }

        let info = describe_path(&path, name_str, &lmeta, meta.as_ref(), size_deadline, now);
        let description = std::iter::once(path.to_string_lossy())
            .chain(
                info.columns
                    .iter()
                    .map(|c| Cow::Borrowed(c.trim()))
                    .filter(|c| !c.is_empty()),
            )
            .collect::<Vec<_>>()
            .join(" · ");

        out.push(TypeRow {
            insert_quoted: quote_path_preserving_tilde(&candidate_val),
            suffix: if is_dir {
                String::new()
            } else {
                " ".to_string()
            },
            raw_columns: HashMap::from([("value".to_string(), candidate_val.clone())]),
            insert_value: candidate_val.clone(),
            label: candidate_val,
            description,
            is_dir,
            is_file: !is_dir,
            rank_deemphasized: false,
            pending_hint: None,
            preview_cmd: None,
            columns: info.columns,
            style: info.style,
            icon: info.icon,
        });
    }

    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.label.cmp(&b.label)));
    out
}

struct PathInfo {
    columns: Vec<String>,
    style: Option<&'static str>,
    icon: Option<char>,
}

fn describe_path(
    path: &Path,
    name: &str,
    lmeta: &Metadata,
    meta: Option<&Metadata>,
    size_deadline: Instant,
    now: SystemTime,
) -> PathInfo {
    let is_link = lmeta.file_type().is_symlink();
    let is_dir = meta.is_some_and(Metadata::is_dir);
    let age = meta
        .unwrap_or(lmeta)
        .modified()
        .ok()
        .and_then(|t| now.duration_since(t).ok())
        .map(format_age)
        .unwrap_or_default();

    let size_col = if is_link {
        let target = std::fs::read_link(path)
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("→ {target}")
    } else if is_dir {
        match dir_size(path, DIR_SIZE_MAX_ENTRIES, size_deadline) {
            DirSize::Exact(b) => format!("{:>8}", format_size(b)),
            DirSize::AtLeast(b) => format!("{:>8}", format!("≥ {}", format_size(b))),
            DirSize::Unknown => String::new(),
        }
    } else {
        format!("{:>8}", format_size(meta.map(|m| m.len()).unwrap_or(0)))
    };

    let is_exec = !is_dir && meta.is_some_and(|m| m.permissions().mode() & 0o111 != 0);
    let style = if is_link {
        Some(if is_dir { "fg=cyan,bold" } else { "fg=cyan" })
    } else if is_dir {
        None
    } else if is_exec {
        Some("fg=green,bold")
    } else {
        file_type_style(name)
    };
    let icon = Some(match (is_link, is_dir) {
        (true, true) => icons::DIR_LINK,
        (true, false) => icons::FILE_LINK,
        (false, true) => icons::DIR,
        (false, false) => icons::for_file_name(name)
            .or(is_exec.then_some(icons::EXECUTABLE))
            .unwrap_or(icons::FILE),
    });

    PathInfo {
        columns: vec![size_col, age],
        style,
        icon,
    }
}

enum DirSize {
    Exact(u64),
    /// The walk hit the entry cap or the deadline; the real size is at least this.
    AtLeast(u64),
    /// The deadline had already passed before the walk started.
    Unknown,
}

/// Sums apparent file sizes under `root` without following symlinks, visiting at most
/// `max_entries` entries and stopping at `deadline`.
fn dir_size(root: &Path, max_entries: usize, deadline: Instant) -> DirSize {
    if Instant::now() >= deadline {
        return DirSize::Unknown;
    }
    let mut total = 0u64;
    let mut visited = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if visited >= max_entries || Instant::now() >= deadline {
                return DirSize::AtLeast(total);
            }
            visited += 1;
            // `DirEntry::metadata` doesn't follow symlinks, so link cycles can't recurse.
            let Ok(m) = entry.metadata() else {
                continue;
            };
            if m.is_dir() {
                stack.push(entry.path());
            } else {
                total += m.len();
            }
        }
    }
    DirSize::Exact(total)
}

fn file_type_style(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "zst" | "rar" | "deb" | "rpm"
        | "dmg" => Some("fg=red"),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "heic" | "mp4" | "mkv" | "mov"
        | "avi" | "mp3" | "flac" | "wav" | "ogg" | "m4a" => Some("fg=magenta"),
        "md" | "txt" | "pdf" | "rst" | "org" => Some("fg=yellow"),
        _ => None,
    }
}

/// Splits `typed_prefix` into (displayed dir part, resolved dir, basename prefix).
fn split_and_resolve_path<'p>(
    typed_prefix: &'p str,
    cwd: &Path,
    env: &HashMap<String, String>,
) -> (&'p str, PathBuf, &'p str) {
    let (dir_part, base_part) = match typed_prefix.rfind('/') {
        Some(pos) => typed_prefix.split_at(pos + 1),
        None => ("", typed_prefix),
    };

    let expanded_dir = expand_leading_vars(dir_part, env);
    let resolved = if expanded_dir.is_empty() {
        cwd.to_path_buf()
    } else {
        // `join` replaces the base when the path is absolute.
        cwd.join(&*expanded_dir)
    };

    (dir_part, resolved, base_part)
}

fn expand_leading_vars<'d>(dir_part: &'d str, env: &HashMap<String, String>) -> Cow<'d, str> {
    let home = || {
        env.get("HOME")
            .cloned()
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_default()
    };

    if dir_part == "~/" {
        return Cow::Owned(format!("{}/", home()));
    }
    let home_rest = dir_part
        .strip_prefix("~/")
        .or_else(|| dir_part.strip_prefix("$HOME/"))
        .or_else(|| dir_part.strip_prefix("${HOME}/"));
    if let Some(rest) = home_rest {
        return Cow::Owned(format!("{}/{rest}", home()));
    }
    let var_expansion = dir_part.strip_prefix('$').and_then(|after_dollar| {
        let (var_name, rest) = after_dollar.split_once('/')?;
        let clean_var = var_name
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(var_name);
        env.get(clean_var).map(|val| format!("{val}/{rest}"))
    });
    var_expansion.map_or(Cow::Borrowed(dir_part), Cow::Owned)
}

fn quote_path_preserving_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if rest.is_empty() {
            return "~/".to_string();
        }
        return format!("~/{}", shell_quote(rest));
    }
    if let Some(rest) = path.strip_prefix("$HOME/") {
        return format!("$HOME/{}", shell_quote(rest));
    }
    shell_quote(path)
}
