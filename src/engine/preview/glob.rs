//! Unquoted glob expansion for the blast-radius preview, following zsh defaults: `*`, `?` and
//! `[...]` (`[!...]`/`[^...]` negated, `a-z` ranges) match within one path segment, a `**`
//! segment matches any number of directories, and leading dots only match explicitly.
//! Words ezlo can't evaluate statically (`$VAR`, `$(...)`, backticks, brace expansion, glob
//! qualifiers, `~user`) are reported as unsupported rather than guessed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Star,
    Quest,
    Class { negated: bool, items: Vec<(char, char)> },
}

/// A shell word turned into a path pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    toks: Vec<Tok>,
}

/// Outcome of parsing a raw shell word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// No unquoted glob characters: the unquoted path.
    Literal(String),
    Glob(Pattern),
    /// Needs the shell to evaluate (expansions, brace lists, qualifiers).
    Unsupported,
}

/// Budget shared by every expansion and size walk of one preview.
pub struct Budget {
    pub entries_left: usize,
    pub deadline: Instant,
}

impl Budget {
    /// Spends one directory entry; `false` once the entry cap or the deadline is hit.
    pub fn spend(&mut self) -> bool {
        if self.entries_left == 0 || Instant::now() >= self.deadline {
            self.entries_left = 0;
            return false;
        }
        self.entries_left -= 1;
        true
    }

    pub fn exhausted(&self) -> bool {
        self.entries_left == 0
    }
}

/// Parses `raw` (as typed, quotes included). A leading `~`/`~/` expands to `$HOME` from `env`.
pub fn parse_word(raw: &str, env: &HashMap<String, String>) -> Word {
    let mut chars: Vec<char> = raw.chars().collect();
    if chars.first() == Some(&'~') {
        match chars.get(1) {
            None | Some('/') => {
                let Some(home) = env.get("HOME").filter(|h| !h.is_empty()) else {
                    return Word::Unsupported;
                };
                chars.splice(0..1, home.chars());
            }
            _ => return Word::Unsupported,
        }
    }

    let mut toks = Vec::with_capacity(chars.len());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    toks.push(Tok::Lit(c));
                }
            }
            Some(_) => match c {
                '"' => quote = None,
                '$' | '`' => return Word::Unsupported,
                '\\' if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                    toks.push(Tok::Lit(chars[i + 1]));
                    i += 1;
                }
                _ => toks.push(Tok::Lit(c)),
            },
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    if let Some(&next) = chars.get(i + 1) {
                        toks.push(Tok::Lit(next));
                        i += 1;
                    }
                }
                '$' | '`' | '(' | ')' | '<' | '>' => return Word::Unsupported,
                '{' if is_brace_expansion(&chars[i..]) => return Word::Unsupported,
                '*' => toks.push(Tok::Star),
                '?' => toks.push(Tok::Quest),
                '[' => match parse_class(&chars, i) {
                    Some((tok, end)) => {
                        toks.push(tok);
                        i = end;
                    }
                    None => toks.push(Tok::Lit('[')),
                },
                _ => toks.push(Tok::Lit(c)),
            },
        }
        i += 1;
    }

    if toks.iter().any(|t| !matches!(t, Tok::Lit(_))) {
        Word::Glob(Pattern { toks })
    } else {
        Word::Literal(
            toks.iter()
                .map(|t| match t {
                    Tok::Lit(c) => *c,
                    _ => unreachable!("only literals left"),
                })
                .collect(),
        )
    }
}

/// `{a,b}` / `{1..3}` up to the matching `}` (brace expansion; zsh's `{}` alone is literal).
fn is_brace_expansion(rest: &[char]) -> bool {
    let Some(close) = rest.iter().position(|&c| c == '}') else {
        return false;
    };
    let inner: String = rest[1..close].iter().collect();
    inner.contains(',') || inner.contains("..")
}

/// Parses `[...]` starting at `open`; returns the token and the index of its `]`.
fn parse_class(chars: &[char], open: usize) -> Option<(Tok, usize)> {
    let mut i = open + 1;
    let negated = matches!(chars.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut items = Vec::new();
    let first = i;
    while let Some(&c) = chars.get(i) {
        if c == ']' && i > first {
            return Some((Tok::Class { negated, items }, i));
        }
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            items.push((c, chars[i + 2]));
            i += 3;
        } else {
            items.push((c, c));
            i += 1;
        }
    }
    None
}

/// Shell-style match of one segment's tokens against `name`.
fn match_segment(toks: &[Tok], name: &[char]) -> bool {
    let (mut t, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match toks.get(t) {
            Some(Tok::Star) => {
                backtrack = Some((t, n));
                t += 1;
                continue;
            }
            Some(Tok::Quest) => true,
            Some(Tok::Lit(c)) => *c == name[n],
            Some(Tok::Class { negated, items }) => {
                items.iter().any(|&(lo, hi)| (lo..=hi).contains(&name[n])) != *negated
            }
            None => false,
        };
        if step {
            t += 1;
            n += 1;
        } else if let Some((bt, bn)) = backtrack {
            t = bt + 1;
            n = bn + 1;
            backtrack = Some((bt, bn + 1));
        } else {
            return false;
        }
    }
    toks[t..].iter().all(|tok| *tok == Tok::Star)
}

/// Matches of a glob expansion: paths as the shell would spell them (relative to `cwd` unless the
/// pattern is absolute), sorted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub paths: Vec<PathBuf>,
    /// The budget ran out: there may be more matches.
    pub truncated: bool,
}

impl Pattern {
    /// Expands the pattern against the filesystem under `cwd`.
    pub fn expand(&self, cwd: &Path, budget: &mut Budget) -> Expansion {
        let absolute = self.toks.first() == Some(&Tok::Lit('/'));
        let segments: Vec<&[Tok]> = self
            .toks
            .split(|t| *t == Tok::Lit('/'))
            .filter(|s| !s.is_empty())
            .collect();
        let dirs_only = self.toks.last() == Some(&Tok::Lit('/'));
        let mut current: Vec<PathBuf> = vec![if absolute { PathBuf::from("/") } else { PathBuf::new() }];
        let resolve = |p: &Path| if p.as_os_str().is_empty() { cwd.to_path_buf() } else { cwd.join(p) };

        for (idx, seg) in segments.iter().enumerate() {
            let last = idx + 1 == segments.len();
            let mut next = Vec::new();
            if seg.len() == 2 && seg.iter().all(|t| *t == Tok::Star) && !last {
                for base in &current {
                    next.push(base.clone());
                    collect_subdirs(&resolve(base), base, budget, &mut next);
                }
            } else if seg.iter().all(|t| matches!(t, Tok::Lit(_))) {
                let name: String = seg
                    .iter()
                    .map(|t| match t {
                        Tok::Lit(c) => *c,
                        _ => unreachable!("literal segment"),
                    })
                    .collect();
                next.extend(current.iter().map(|base| base.join(&name)));
            } else {
                let explicit_dot = seg.first() == Some(&Tok::Lit('.'));
                for base in &current {
                    let Ok(rd) = fs::read_dir(resolve(base)) else { continue };
                    let mut found: Vec<PathBuf> = Vec::new();
                    for entry in rd.flatten() {
                        if !budget.spend() {
                            break;
                        }
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') && !explicit_dot {
                            continue;
                        }
                        let name_chars: Vec<char> = name.chars().collect();
                        if match_segment(seg, &name_chars) {
                            found.push(base.join(&name));
                        }
                    }
                    found.sort();
                    next.extend(found);
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }

        let paths = current
            .into_iter()
            .filter(|p| !p.as_os_str().is_empty())
            .filter(|p| match fs::symlink_metadata(resolve(p)) {
                Ok(m) => !dirs_only || m.is_dir() || fs::metadata(resolve(p)).is_ok_and(|m| m.is_dir()),
                Err(_) => false,
            })
            .collect();
        Expansion {
            paths,
            truncated: budget.exhausted(),
        }
    }
}

/// Every non-hidden directory below `dir` (not following symlinks), spelled relative to `rel`.
fn collect_subdirs(dir: &Path, rel: &Path, budget: &mut Budget, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut subdirs: Vec<(PathBuf, PathBuf)> = Vec::new();
    for entry in rd.flatten() {
        if !budget.spend() {
            return;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            subdirs.push((entry.path(), rel.join(&name)));
        }
    }
    subdirs.sort();
    for (abs, sub_rel) in subdirs {
        out.push(sub_rel.clone());
        collect_subdirs(&abs, &sub_rel, budget, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(raw: &str) -> Pattern {
        match parse_word(raw, &HashMap::new()) {
            Word::Glob(p) => p,
            other => panic!("expected a glob for {raw:?}, got {other:?}"),
        }
    }

    fn matches(raw: &str, name: &str) -> bool {
        let chars: Vec<char> = name.chars().collect();
        match_segment(&glob(raw).toks, &chars)
    }

    #[test]
    fn quoting_disables_globbing() {
        let env = HashMap::new();
        assert_eq!(parse_word("'*.log'", &env), Word::Literal("*.log".into()));
        assert_eq!(parse_word("\"a b\"\\*", &env), Word::Literal("a b*".into()));
        assert!(matches!(parse_word("\"dir\"/*.log", &env), Word::Glob(_)));
        assert_eq!(parse_word("$HOME/*", &env), Word::Unsupported);
        assert_eq!(parse_word("{a,b}.txt", &env), Word::Unsupported);
        assert_eq!(parse_word("*(.)", &env), Word::Unsupported);
    }

    #[test]
    fn tilde_expands_home() {
        let env = HashMap::from([("HOME".to_owned(), "/home/u".to_owned())]);
        assert_eq!(parse_word("~/x", &env), Word::Literal("/home/u/x".into()));
        assert_eq!(parse_word("~bob/x", &env), Word::Unsupported);
    }

    #[test]
    fn segment_matching() {
        assert!(matches("*.log", "a.log"));
        assert!(!matches("*.log", "a.log.gz"));
        assert!(matches("a?c", "abc"));
        assert!(matches("[a-c]x", "bx"));
        assert!(!matches("[!a-c]x", "bx"));
        assert!(matches("*a*b*", "xxaxxbxx"));
        assert!(matches("[]]", "]"));
        assert!(matches("[*", "[x"), "unterminated class is literal");
        assert_eq!(parse_word("a[", &HashMap::new()), Word::Literal("a[".into()));
    }
}
