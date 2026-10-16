//! Where the data flowing into the stage under the cursor comes from: an input redirection, a file
//! operand of the stage itself, or a side-effect-free upstream stage (`cat f |`, `head -n5 f |`,
//! `< f |`, possibly through pass-through filters like `sort |`). Nothing is ever executed: the
//! named file is read in-process, bounded to `MAX_SAMPLE_BYTES`, and only if it is a regular file
//! (never a FIFO or device, which could block or have side effects).

use super::args::{redirect_path, scan, OptTable};
use super::json::{collect_paths, JsonPath};
use crate::engine::line::{char_slice, find_active_segment, skip_assignments_and_wrappers, tokenize_segment, ParsedWord};
use crate::engine::CompletionRequest;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Bytes read from a sampled file; longer files are cut at the last full line.
pub const MAX_SAMPLE_BYTES: u64 = 1 << 20;
/// Upstream stages followed through pass-through filters (`cat f | sort | uniq | jq`).
const MAX_HOPS: usize = 4;
/// Samples kept by the in-process cache (keyed by path, validated by mtime and size).
const CACHE_ENTRIES: usize = 8;

/// A bounded in-memory sample of a file feeding the pipeline.
#[derive(Debug)]
pub struct Sample {
    pub path: PathBuf,
    pub text: String,
    json: OnceLock<Vec<JsonPath>>,
}

impl Sample {
    pub fn new(path: PathBuf, text: String) -> Self {
        Self {
            path,
            text,
            json: OnceLock::new(),
        }
    }

    /// `jq` paths of the sample (empty when it is not JSON / JSON lines); computed once.
    pub fn json_paths(&self) -> &[JsonPath] {
        self.json.get_or_init(|| collect_paths(&self.text))
    }

    /// File name for descriptions.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

type CacheEntry = (PathBuf, SystemTime, u64, Arc<Sample>);

fn cache() -> &'static Mutex<Vec<CacheEntry>> {
    static CACHE: OnceLock<Mutex<Vec<CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Reads (or reuses) the bounded sample of `path`; `None` unless it is a readable regular file.
pub fn load_sample(path: &Path) -> Option<Arc<Sample>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let len = meta.len();
    if let Ok(entries) = cache().lock() {
        if let Some((_, _, _, s)) = entries.iter().find(|(p, t, l, _)| p == path && *t == mtime && *l == len) {
            return Some(Arc::clone(s));
        }
    }
    let mut buf = Vec::new();
    File::open(path).ok()?.take(MAX_SAMPLE_BYTES).read_to_end(&mut buf).ok()?;
    if len > MAX_SAMPLE_BYTES {
        if let Some(nl) = buf.iter().rposition(|&b| b == b'\n') {
            buf.truncate(nl + 1);
        }
    }
    let sample = Arc::new(Sample::new(path.to_path_buf(), String::from_utf8_lossy(&buf).into_owned()));
    if let Ok(mut entries) = cache().lock() {
        entries.retain(|(p, ..)| p != path);
        if entries.len() >= CACHE_ENTRIES {
            entries.remove(0);
        }
        entries.push((path.to_path_buf(), mtime, len, Arc::clone(&sample)));
    }
    Some(sample)
}

/// Resolves a literal file word against `cwd` (`~` → `$HOME`). Words with expansions, globs or
/// command substitutions are not literal and yield `None`.
pub fn literal_path(text: &str, word: &ParsedWord, cwd: &Path, env: &HashMap<String, String>) -> Option<PathBuf> {
    if text.is_empty() || text == "-" || word.unterminated || word.raw.contains(['$', '`']) {
        return None;
    }
    if word.quote.is_none() && text.contains(['*', '?', '[']) {
        return None;
    }
    let path = if text == "~" || text.starts_with("~/") {
        Path::new(env.get("HOME")?).join(text.trim_start_matches('~').trim_start_matches('/'))
    } else if text.starts_with('~') {
        return None;
    } else {
        PathBuf::from(text)
    };
    Some(if path.is_absolute() { path } else { cwd.join(path) })
}

/// Loads the first of `words` that names a readable regular file.
pub fn first_sample<'w>(
    req: &CompletionRequest<'_>,
    words: impl IntoIterator<Item = (&'w str, &'w ParsedWord)>,
) -> Option<Arc<Sample>> {
    words
        .into_iter()
        .filter_map(|(text, w)| literal_path(text, w, req.cwd, req.env))
        .find_map(|p| load_sample(&p))
}

/// What an upstream stage feeds downstream.
enum Feed {
    File(Arc<Sample>),
    /// A filter reading its own stdin (keep looking further upstream).
    PassThrough,
    /// Anything else: an arbitrary command whose output is unknown without running it.
    Opaque,
}

/// Sample of the data piped into the segment starting at char `segment_start`, when the stage
/// before the `|` is a side-effect-free file read.
pub fn upstream_sample(req: &CompletionRequest<'_>, segment_start: usize) -> Option<Arc<Sample>> {
    let chars: Vec<char> = req.line.chars().collect();
    let mut seg_start = segment_start;
    for _ in 0..MAX_HOPS {
        let pipe = seg_start.checked_sub(1)?;
        if chars.get(pipe) != Some(&'|') || (pipe > 0 && chars[pipe - 1] == '|') {
            return None;
        }
        let (start, end) = find_active_segment(req.line, pipe);
        let words = tokenize_segment(char_slice(req.line, start, end), start);
        match stage_feed(req, &words) {
            Feed::File(sample) => return Some(sample),
            Feed::PassThrough => seg_start = start,
            Feed::Opaque => return None,
        }
    }
    None
}

/// File-reading commands whose stdout is (a slice of) their input, with their value options.
fn reader_options(name: &str) -> Option<OptTable> {
    let (short, long): (&'static str, &'static [(&'static str, usize)]) = match name {
        "cat" | "tac" => ("", &[]),
        "bat" | "batcat" => ("lrmH", &[("--language", 1), ("--line-range", 1), ("--style", 1), ("--theme", 1), ("--highlight-line", 1), ("--map-syntax", 1)]),
        "head" | "tail" => ("nc", &[("--lines", 1), ("--bytes", 1)]),
        "sort" => ("ktoST", &[("--key", 1), ("--field-separator", 1), ("--output", 1), ("--buffer-size", 1), ("--temporary-directory", 1)]),
        "uniq" => ("fsw", &[("--skip-fields", 1), ("--skip-chars", 1), ("--check-chars", 1)]),
        _ => return None,
    };
    Some(OptTable { short, long })
}

fn stage_feed(req: &CompletionRequest<'_>, words: &[ParsedWord]) -> Feed {
    let cmd_idx = skip_assignments_and_wrappers(words, 0, None, req.wrappers);
    let Some(cmd) = words.get(cmd_idx) else {
        return Feed::Opaque;
    };
    // `< file |`: a bare redirection, no command.
    if is_bare_redirect(cmd) {
        let s = scan(&words[cmd_idx..], &OptTable { short: "", long: &[] });
        return match s.input {
            Some(i) if s.operands.is_empty() => load_word(req, &words[cmd_idx + i]),
            _ => Feed::Opaque,
        };
    }
    let name = cmd.unquoted.rsplit('/').next().unwrap_or_default();
    let Some(table) = reader_options(name) else {
        return Feed::Opaque;
    };
    let rest = &words[cmd_idx + 1..];
    let s = scan(rest, &table);
    if let Some(i) = s.input {
        return load_word(req, &rest[i]);
    }
    if s.operands.is_empty() {
        return Feed::PassThrough;
    }
    let operands = s.operands.iter().map(|&i| (rest[i].unquoted.as_str(), &rest[i]));
    first_sample(req, operands).map_or(Feed::Opaque, Feed::File)
}

/// Whether the stage starts with an input redirection instead of a command (`< file`).
fn is_bare_redirect(word: &ParsedWord) -> bool {
    word.quote.is_none() && word.raw.starts_with('<') && !word.raw.starts_with("<<") && !word.raw.starts_with("<(")
}

fn load_word(req: &CompletionRequest<'_>, word: &ParsedWord) -> Feed {
    first_sample(req, [(redirect_path(word), word)]).map_or(Feed::Opaque, Feed::File)
}
