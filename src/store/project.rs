//! Project kinds of a directory (Rust, Node, Go, Python, Bazel, …), detected from marker files in
//! it or its ancestors up to the repository root. History run in a project of the same kind ranks
//! right after the current directory/repo: `cargo nextest run` from another Rust crate is a better
//! guess in a fresh crate than a random line from elsewhere.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// `(marker file, kind index into KINDS)`.
const MARKERS: [(&str, usize); 20] = [
    ("Cargo.toml", 0),
    ("package.json", 1),
    ("go.mod", 2),
    ("pyproject.toml", 3),
    ("requirements.txt", 3),
    ("setup.py", 3),
    ("MODULE.bazel", 4),
    ("WORKSPACE", 4),
    ("WORKSPACE.bazel", 4),
    ("Dockerfile", 5),
    ("docker-compose.yml", 5),
    ("compose.yaml", 5),
    ("build.gradle", 6),
    ("build.gradle.kts", 6),
    ("CMakeLists.txt", 7),
    ("Gemfile", 8),
    ("flake.nix", 9),
    ("pubspec.yaml", 10),
    ("mix.exs", 11),
    ("Makefile", 12),
];
/// Kind names; `make` is last because a Makefile says little next to any other marker.
const KINDS: [&str; 13] = [
    "rust", "node", "go", "python", "bazel", "docker", "gradle", "cmake", "ruby", "nix", "dart", "elixir", "make",
];
/// Ancestors checked above a directory when no repository root stops the walk earlier.
const MAX_DEPTH: usize = 4;
/// Cached directories at most (history has a bounded number of distinct cwds anyway).
const MAX_CACHED: usize = 4096;
/// Markers are re-checked after this long (`cargo init` in a fresh directory).
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Bit set of project kinds (bit `i` = `KINDS[i]`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Kinds(u32);

impl Kinds {
    /// Name of the most specific kind both sets share (`rust` before `make`).
    pub fn shared_name(self, other: Kinds) -> Option<&'static str> {
        let both = self.0 & other.0;
        (0..KINDS.len()).find(|i| both & (1 << i) != 0).map(|i| KINDS[i])
    }
}

/// Memoized [`Kinds`] per directory path.
#[derive(Default)]
pub struct ProjectKinds {
    cache: Mutex<HashMap<String, (Kinds, Instant)>>,
}

impl ProjectKinds {
    pub fn of(&self, dir: &str) -> Kinds {
        let cached = self.cache.lock().ok().and_then(|c| c.get(dir).copied());
        if let Some((k, _)) = cached.filter(|(_, at)| at.elapsed() < CACHE_TTL) {
            return k;
        }
        let kinds = detect(Path::new(dir));
        if let Ok(mut c) = self.cache.lock() {
            if c.len() >= MAX_CACHED {
                c.clear();
            }
            c.insert(dir.to_owned(), (kinds, Instant::now()));
        }
        kinds
    }
}

fn detect(dir: &Path) -> Kinds {
    let mut bits = 0u32;
    for d in dir.ancestors().take(MAX_DEPTH + 1) {
        if d.parent().is_none() {
            break;
        }
        for (marker, kind) in MARKERS {
            if bits & (1 << kind) == 0 && d.join(marker).exists() {
                bits |= 1 << kind;
            }
        }
        if d.join(".git").exists() {
            break;
        }
    }
    Kinds(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_markers_up_to_the_repo_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("crate/src")).unwrap();
        std::fs::write(repo.join("crate/Cargo.toml"), "").unwrap();
        std::fs::write(repo.join("Makefile"), "").unwrap();
        // Above the repo root: ignored.
        std::fs::write(root.path().join("package.json"), "").unwrap();
        let pk = ProjectKinds::default();
        let k = pk.of(&repo.join("crate/src").to_string_lossy());
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("Cargo.toml"), "").unwrap();
        let o = pk.of(&other.path().to_string_lossy());
        assert_eq!(k.shared_name(o), Some("rust"));
        assert_eq!(k.shared_name(Kinds(1 << 1)), None, "package.json above the repo root doesn't count");
    }
}
