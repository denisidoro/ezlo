//! Static word expansion for instant previews: `$VAR`, `${VAR}`, `$((arithmetic))` and a leading
//! `~` are resolved from the shell's exported environment, then unquoted globs are expanded like
//! zsh does by default (parameter values themselves are never globbed). Anything that needs the
//! shell to run code (`$(...)`, backticks, `$1`, `$?`, `${x:-y}`) makes the word unsupported.

use super::super::glob::{parse_word, Budget, Word};
use std::collections::HashMap;
use std::path::Path;

/// A word after expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expanded {
    /// The resulting fields (a glob may produce several; never empty).
    Fields(Vec<String>),
    /// An unquoted glob that matched nothing (zsh refuses to run the line).
    NoMatch,
    /// Needs the shell to evaluate.
    Unsupported,
}

/// Expands `raw` (as typed, quotes included) against `env`, globbing relative to `cwd`.
pub fn expand_word(raw: &str, env: &HashMap<String, String>, cwd: &Path, budget: &mut Budget) -> Expanded {
    let Some(requoted) = substitute_params(raw, env) else {
        return Expanded::Unsupported;
    };
    match parse_word(&requoted, env) {
        Word::Literal(s) => Expanded::Fields(vec![s]),
        Word::Glob(pattern) => {
            let exp = pattern.expand(cwd, budget);
            if exp.paths.is_empty() {
                return if exp.truncated { Expanded::Unsupported } else { Expanded::NoMatch };
            }
            Expanded::Fields(exp.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect())
        }
        Word::Unsupported => Expanded::Unsupported,
    }
}

/// Whether `raw` has anything [`expand_word`] would change: a parameter, a leading `~` or an
/// unquoted glob character (plain words aren't worth echoing back).
pub fn needs_expansion(raw: &str) -> bool {
    if raw.starts_with('~') {
        return true;
    }
    let mut quote: Option<char> = None;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') => quote = None,
            (Some('\''), _) => {}
            (_, '\\') => {
                chars.next();
            }
            (_, '$') => return true,
            (Some('"'), '"') => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '*' | '?' | '[') => return true,
            (None, _) => {}
        }
    }
    false
}

/// Replaces parameter expansions with single-quoted values, keeping the rest of `raw` as typed
/// so quoting and globbing still apply to it. `None` when a construct can't be resolved.
fn substitute_params(raw: &str, env: &HashMap<String, String>) -> Option<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut in_double = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' if !in_double => {
                let end = chars[i + 1..].iter().position(|&x| x == '\'').map(|p| i + 1 + p)?;
                out.extend(&chars[i..=end]);
                i = end + 1;
                continue;
            }
            '\\' => {
                out.push(c);
                if let Some(&n) = chars.get(i + 1) {
                    out.push(n);
                }
                i += 2;
                continue;
            }
            '"' => in_double = !in_double,
            '`' => return None,
            '$' => {
                let (value, next) = parameter(&chars, i, env)?;
                // Close a double quote around the single-quoted value, then reopen it.
                if in_double {
                    out.push('"');
                }
                out.push('\'');
                out.push_str(&value.replace('\'', "'\\''"));
                out.push('\'');
                if in_double {
                    out.push('"');
                }
                i = next;
                continue;
            }
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    Some(out)
}

/// The value of the expansion starting at `chars[start] == '$'` and the index after it.
fn parameter(chars: &[char], start: usize, env: &HashMap<String, String>) -> Option<(String, usize)> {
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    match chars.get(start + 1) {
        Some('(') if chars.get(start + 2) == Some(&'(') => {
            let body_start = start + 3;
            let mut depth = 0i32;
            let mut j = body_start;
            while j + 1 < chars.len() {
                match chars[j] {
                    '(' => depth += 1,
                    ')' if depth > 0 => depth -= 1,
                    ')' if chars[j + 1] == ')' => {
                        let expr: String = chars[body_start..j].iter().collect();
                        return Some((arith::eval(&expr, env)?.to_string(), j + 2));
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        Some('{') => {
            let close = chars[start + 2..].iter().position(|&c| c == '}').map(|p| start + 2 + p)?;
            let name: String = chars[start + 2..close].iter().collect();
            let valid = !name.is_empty() && name.chars().all(is_name) && !name.starts_with(|c: char| c.is_ascii_digit());
            valid.then(|| (env.get(&name).cloned().unwrap_or_default(), close + 1))
        }
        Some(&c) if is_name(c) && !c.is_ascii_digit() => {
            let end = chars[start + 1..].iter().position(|&c| !is_name(c)).map_or(chars.len(), |p| start + 1 + p);
            let name: String = chars[start + 1..end].iter().collect();
            Some((env.get(&name).cloned().unwrap_or_default(), end))
        }
        // A lone `$` (end of word, before a space or quote) is literal.
        None | Some('"' | '\'' | ' ') => Some(("$".to_string(), start + 1)),
        _ => None,
    }
}

/// Integer arithmetic of `$((…))`: `+ - * / % **`, parentheses, unary signs and variable names
/// (unset or non-numeric ones count as 0, like zsh).
pub mod arith {
    use std::collections::HashMap;

    pub fn eval(expr: &str, env: &HashMap<String, String>) -> Option<i64> {
        let toks = tokenize(expr)?;
        let mut p = Parser { toks: &toks, pos: 0, env };
        let v = p.sum()?;
        (p.pos == toks.len()).then_some(v)
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        Num(i64),
        Name(String),
        Op(&'static str),
    }

    fn tokenize(s: &str) -> Option<Vec<Tok>> {
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() {
                i += 1;
            } else if c.is_ascii_digit() {
                let end = chars[i..].iter().position(|c| !c.is_ascii_digit()).map_or(chars.len(), |p| i + p);
                out.push(Tok::Num(chars[i..end].iter().collect::<String>().parse().ok()?));
                i = end;
            } else if c.is_ascii_alphabetic() || c == '_' || c == '$' {
                let from = if c == '$' { i + 1 } else { i };
                let end = chars[from..]
                    .iter()
                    .position(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
                    .map_or(chars.len(), |p| from + p);
                if end == from {
                    return None;
                }
                out.push(Tok::Name(chars[from..end].iter().collect()));
                i = end;
            } else if c == '*' && chars.get(i + 1) == Some(&'*') {
                out.push(Tok::Op("**"));
                i += 2;
            } else {
                out.push(Tok::Op(match c {
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '%' => "%",
                    '(' => "(",
                    ')' => ")",
                    _ => return None,
                }));
                i += 1;
            }
        }
        Some(out)
    }

    struct Parser<'a> {
        toks: &'a [Tok],
        pos: usize,
        env: &'a HashMap<String, String>,
    }

    impl Parser<'_> {
        fn eat(&mut self, op: &str) -> bool {
            if matches!(self.toks.get(self.pos), Some(Tok::Op(o)) if *o == op) {
                self.pos += 1;
                true
            } else {
                false
            }
        }

        fn sum(&mut self) -> Option<i64> {
            let mut v = self.product()?;
            loop {
                if self.eat("+") {
                    v = v.checked_add(self.product()?)?;
                } else if self.eat("-") {
                    v = v.checked_sub(self.product()?)?;
                } else {
                    return Some(v);
                }
            }
        }

        fn product(&mut self) -> Option<i64> {
            let mut v = self.power()?;
            loop {
                if self.eat("*") {
                    v = v.checked_mul(self.power()?)?;
                } else if self.eat("/") {
                    v = v.checked_div(self.power()?)?;
                } else if self.eat("%") {
                    v = v.checked_rem(self.power()?)?;
                } else {
                    return Some(v);
                }
            }
        }

        fn power(&mut self) -> Option<i64> {
            let base = self.unary()?;
            if self.eat("**") {
                let exp = self.power()?;
                return base.checked_pow(u32::try_from(exp).ok()?);
            }
            Some(base)
        }

        fn unary(&mut self) -> Option<i64> {
            if self.eat("-") {
                return self.unary()?.checked_neg();
            }
            if self.eat("+") {
                return self.unary();
            }
            if self.eat("(") {
                let v = self.sum()?;
                return self.eat(")").then_some(v);
            }
            let tok = self.toks.get(self.pos)?.clone();
            self.pos += 1;
            match tok {
                Tok::Num(n) => Some(n),
                Tok::Name(n) => Some(self.env.get(&n).and_then(|v| v.trim().parse().ok()).unwrap_or(0)),
                Tok::Op(_) => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn env() -> HashMap<String, String> {
        HashMap::from([
            ("USER".into(), "zelda".into()),
            ("HOME".into(), "/home/zelda".into()),
            ("N".into(), "4".into()),
            ("Q".into(), "it's *".into()),
        ])
    }

    fn expand(raw: &str) -> Expanded {
        let mut budget = Budget { entries_left: 100, deadline: Instant::now() + Duration::from_secs(1) };
        expand_word(raw, &env(), Path::new("/nonexistent"), &mut budget)
    }

    fn one(s: &str) -> Expanded {
        Expanded::Fields(vec![s.to_string()])
    }

    #[test]
    fn parameters_and_quotes() {
        assert_eq!(expand("$USER"), one("zelda"));
        assert_eq!(expand("${USER}x"), one("zeldax"));
        assert_eq!(expand("\"hi $USER!\""), one("hi zelda!"));
        assert_eq!(expand("'$USER'"), one("$USER"));
        assert_eq!(expand("\\$USER"), one("$USER"));
        assert_eq!(expand("$UNSET_VAR"), one(""));
        assert_eq!(expand("~/x"), one("/home/zelda/x"));
        // Values are neither re-split nor globbed.
        assert_eq!(expand("$Q"), one("it's *"));
        assert_eq!(expand("$((N * 2 + 1))"), one("9"));
        assert_eq!(expand("$(( (1+2)**2 % 5 ))"), one("4"));
    }

    #[test]
    fn unsupported_constructs() {
        for raw in ["$(whoami)", "`id`", "$1", "$?", "${USER:-x}", "$((1/0))"] {
            assert_eq!(expand(raw), Expanded::Unsupported, "{raw}");
        }
    }

    #[test]
    fn detects_words_worth_expanding() {
        assert!(needs_expansion("$USER"));
        assert!(needs_expansion("~/x"));
        assert!(needs_expansion("*.rs"));
        assert!(needs_expansion("\"$HOME\""));
        assert!(!needs_expansion("hello"));
        assert!(!needs_expansion("'$USER'"));
        assert!(!needs_expansion("\"*.rs\""));
    }
}
