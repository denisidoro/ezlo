use crate::bundled::load_bundled_specs;
use crate::config::{check_spec_file_security, Config, Source};
use crate::spec::ast::Spec;
use crate::spec::span::{Diagnostic, Severity};
use crate::spec::{has_errors, parse_and_validate, SpecIndex};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

/// Everything about a spec file that affects the index: content (mtime) and the security verdict
/// (mode/owner, which `chmod`/`chown` change without touching mtime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileStamp {
    path: String,
    mtime_secs: u64,
    mtime_nanos: u32,
    mode: u32,
    uid: u32,
    gid: u32,
}

/// Freshness fields of [`CachedIndexDisk`].
#[derive(Deserialize)]
struct CachedIndexHeader {
    version: String,
    sources: Vec<Source>,
    stamps: Vec<FileStamp>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedIndexDisk {
    version: String,
    sources: Vec<Source>,
    stamps: Vec<FileStamp>,
    index: SpecIndex,
}

/// Spec files feeding the index (user, then generated) and their stamps.
struct SpecFiles {
    user: Vec<PathBuf>,
    generated: Vec<PathBuf>,
    stamps: Vec<FileStamp>,
}

impl SpecFiles {
    fn scan(config: &Config) -> Self {
        let user: Vec<PathBuf> = if config.has_source(Source::User) {
            config.spec_dirs().iter().flat_map(|d| collect_spec_files(d)).collect()
        } else {
            Vec::new()
        };
        let generated = if config.has_source(Source::Generated) {
            collect_spec_files(&Config::generated_specs_dir())
        } else {
            Vec::new()
        };
        let stamps = user.iter().chain(&generated).filter_map(|p| file_stamp(p)).collect();
        Self { user, generated, stamps }
    }
}

/// The on-disk index when it is still fresh for the current spec files, without parsing anything.
pub fn load_cached_index(config: &Config) -> Option<SpecIndex> {
    let files = SpecFiles::scan(config);
    try_read_cached_index(&Config::index_cache_path(), env!("CARGO_PKG_VERSION"), &config.sources.enabled, &files.stamps)
}

/// Daemon-side load. A rebuild's parse/merge garbage would stay in the long-lived daemon heap
/// (glibc rarely hands it back: ~8 MB per rebuild), so a stale cache is rebuilt by a short-lived
/// `ezlo daemon --build-index` child and the daemon only deserializes the result. Falls back to
/// building in-process when the child can't produce a fresh cache.
pub fn load_index_isolated(config: &Config) -> SpecIndex {
    ensure_fresh_cache(config);
    if let Some(index) = load_cached_index(config) {
        return index;
    }
    let (index, diags) = load_merged_index(config);
    for line in error_lines(&diags) {
        tracing::warn!("spec index: {line}");
    }
    index
}

/// Makes the on-disk index fresh (rebuilding it in a child when stale) without loading it.
pub fn ensure_fresh_cache(config: &Config) {
    if cache_is_fresh(config) {
        return;
    }
    if let Err(err) = rebuild_cache_in_child() {
        tracing::warn!("spec index: rebuild in child failed: {err:#}");
    }
}

fn cache_is_fresh(config: &Config) -> bool {
    let files = SpecFiles::scan(config);
    let Ok(file) = fs::File::open(Config::index_cache_path()) else {
        return false;
    };
    // Only the header is deserialized; serde skips the `index` payload without building it.
    let Ok(header) = serde_json::from_reader::<_, CachedIndexHeader>(std::io::BufReader::new(file)) else {
        return false;
    };
    header.version == env!("CARGO_PKG_VERSION") && header.sources == config.sources.enabled && header.stamps == files.stamps
}

/// `ezlo daemon --build-index`: refreshes the index cache; errors that reject a spec go to stderr.
pub fn build_index_cache(config: &Config) {
    let (_, diags) = load_merged_index(config);
    for line in error_lines(&diags) {
        eprintln!("{line}");
    }
}

fn error_lines(diags: &[(String, Diagnostic)]) -> impl Iterator<Item = String> + '_ {
    diags
        .iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .map(|(file, d)| format!("{file}: {}", d.message))
}

fn rebuild_cache_in_child() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    // Only the real binary understands `daemon --build-index` (not test harnesses embedding the daemon).
    anyhow::ensure!(exe.file_stem().is_some_and(|s| s == "ezlo"), "not running as ezlo: {}", exe.display());
    let out = Command::new(exe)
        .args(["daemon", "--build-index"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        tracing::warn!("spec index: {line}");
    }
    anyhow::ensure!(out.status.success(), "exited with {}", out.status);
    Ok(())
}

pub fn load_merged_index(config: &Config) -> (SpecIndex, Vec<(String, Diagnostic)>) {
    let cache_path = Config::index_cache_path();
    let SpecFiles {
        user: user_files,
        generated: gen_files,
        stamps,
    } = SpecFiles::scan(config);

    let pkg_version = env!("CARGO_PKG_VERSION").to_string();
    if let Some(cached) = try_read_cached_index(&cache_path, &pkg_version, &config.sources.enabled, &stamps)
    {
        return (cached, Vec::new());
    }

    let mut user_specs: Vec<(String, Spec)> = Vec::new();
    let mut bundled_specs: Vec<(String, Spec)> = Vec::new();
    let mut gen_specs: Vec<(String, Spec)> = Vec::new();
    let mut all_diags: Vec<(String, Diagnostic)> = Vec::new();

    if config.has_source(Source::User) {
        load_specs_from_list(&user_files, &mut user_specs, &mut all_diags);
    }
    if config.has_source(Source::Bundled) {
        bundled_specs.extend(load_bundled_specs());
    }
    if config.has_source(Source::Generated) {
        load_specs_from_list(&gen_files, &mut gen_specs, &mut all_diags);
    }

    let tiers: [&[(String, Spec)]; 3] = [&user_specs, &bundled_specs, &gen_specs];
    let index = SpecIndex::merge_tiers(&tiers, &mut all_diags);
    write_cached_index(&cache_path, &pkg_version, &config.sources.enabled, &stamps, &index);
    (index, all_diags)
}

fn load_specs_from_list(
    files: &[PathBuf],
    ordered_specs: &mut Vec<(String, Spec)>,
    all_diags: &mut Vec<(String, Diagnostic)>,
) {
    for path in files {
        let display_name = path.to_string_lossy().to_string();
        if let Err(sec_err) = check_spec_file_security(path) {
            tracing::warn!("{sec_err}");
            continue;
        }
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };
        let (spec, diags) = parse_and_validate(&src);
        let rejected = has_errors(&diags);
        for d in diags {
            all_diags.push((display_name.clone(), d));
        }
        if !rejected {
            ordered_specs.push((display_name, spec));
        }
    }
}

pub fn collect_spec_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_spec_files_rec(dir, &mut out);
    out.sort();
    out
}

fn collect_spec_files_rec(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_spec_files_rec(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some(crate::spec::SPEC_EXT) {
            out.push(p);
        }
    }
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(FileStamp {
        path: path.to_string_lossy().to_string(),
        mtime_secs: mtime.as_secs(),
        mtime_nanos: mtime.subsec_nanos(),
        mode: meta.mode(),
        uid: meta.uid(),
        gid: meta.gid(),
    })
}

fn try_read_cached_index(
    cache_path: &Path,
    version: &str,
    sources: &[Source],
    stamps: &[FileStamp],
) -> Option<SpecIndex> {
    let file = fs::File::open(cache_path).ok()?;
    let cached: CachedIndexDisk = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
    if cached.version == version && cached.sources == sources && cached.stamps == stamps {
        Some(cached.index)
    } else {
        None
    }
}

fn write_cached_index(
    cache_path: &Path,
    version: &str,
    sources: &[Source],
    stamps: &[FileStamp],
    index: &SpecIndex,
) {
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let payload = CachedIndexDisk {
        version: version.to_string(),
        sources: sources.to_vec(),
        stamps: stamps.to_vec(),
        index: index.clone(),
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    // Write-then-rename so concurrent readers (daemon, CLI) never see a half-written cache.
    let tmp = cache_path.with_extension(format!("tmp.{}", std::process::id()));
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, cache_path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn chmod_changes_stamp_without_touching_mtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("x.ezlo");
        fs::write(&path, "cmd x\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).expect("chmod");
        let before = file_stamp(&path).expect("stamp");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let after = file_stamp(&path).expect("stamp");
        assert_eq!(before.mtime_secs, after.mtime_secs);
        assert_eq!(before.mtime_nanos, after.mtime_nanos);
        assert_ne!(before, after);
    }
}
