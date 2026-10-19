/// One redirection in a command's words: `> f`, `>f`, `2> f`, `&> f`, `>| f`, `>> f`, `< f`, `2>&1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Redirect<'w> {
    pub op: &'w str,
    /// Target word (empty when the line ends right after the operator).
    pub target: &'w str,
    /// Words the redirection spans: 1 when the target is attached (`>f`), 2 when detached (`> f`).
    pub len: usize,
}

impl Redirect<'_> {
    /// Writes to a file (output redirection, not a file-descriptor duplication like `2>&1`).
    pub fn writes_file(&self) -> bool {
        let dup = self.op.ends_with('&') && (self.target == "-" || self.target.chars().all(|c| c.is_ascii_digit()));
        self.op.contains('>') && !self.target.is_empty() && !dup
    }
}

/// Length of the redirection operator `word` starts with: optional fd digits (or the `&` of `&>`),
/// a run of `>`/`<`, then an optional `|` (`>|`) or `&` (`>&`, `<&`).
fn op_len(word: &str) -> Option<usize> {
    let b = word.as_bytes();
    let mut i = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if i == 0 && b.first() == Some(&b'&') {
        i = 1;
    }
    let start = i;
    while b.get(i).is_some_and(|c| matches!(c, b'>' | b'<')) {
        i += 1;
    }
    if i == start {
        return None;
    }
    if b.get(i).is_some_and(|c| matches!(c, b'|' | b'&')) {
        i += 1;
    }
    Some(i)
}

/// The redirection starting at raw word `words[i]`, if any (quoted words are never operators).
pub(crate) fn redirect_at<'w>(words: &[&'w str], i: usize) -> Option<Redirect<'w>> {
    let w = *words.get(i)?;
    if w.starts_with(['\'', '"']) {
        return None;
    }
    let n = op_len(w)?;
    let (op, attached) = w.split_at(n);
    Some(if attached.is_empty() {
        Redirect { op, target: words.get(i + 1).copied().unwrap_or(""), len: 2 }
    } else {
        Redirect { op, target: attached, len: 1 }
    })
}

/// Separates redirections from the other words; returns the remaining words and the
/// `(operator, target)` pairs of the file-writing ones (input redirections and `2>&1` are dropped).
pub(crate) fn split_redirects<'w>(words: &[&'w str]) -> (Vec<&'w str>, Vec<(&'w str, &'w str)>) {
    let mut args = Vec::new();
    let mut redirects = Vec::new();
    let mut i = 0;
    while let Some(&w) = words.get(i) {
        match redirect_at(words, i) {
            Some(r) => {
                if r.writes_file() {
                    redirects.push((r.op, r.target));
                }
                i += r.len;
            }
            None => {
                args.push(w);
                i += 1;
            }
        }
    }
    (args, redirects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_are_split() {
        let (args, red) = split_redirects(&["a", ">", "f", "2>err", ">>log", "<in", "b"]);
        assert_eq!(args, ["a", "b"]);
        assert_eq!(red, [(">", "f"), ("2>", "err"), (">>", "log")]);
    }

    #[test]
    fn fd_duplications_and_combined_redirects() {
        let (args, red) = split_redirects(&["a", "2>&1", "&>", "all.log", ">|", "f", "1>&-", "'>'", "b"]);
        assert_eq!(args, ["a", "'>'", "b"]);
        assert_eq!(red, [("&>", "all.log"), (">|", "f")]);
    }

    #[test]
    fn numbers_and_dashes_are_not_operators() {
        assert_eq!(redirect_at(&["123"], 0), None);
        assert_eq!(redirect_at(&["-n"], 0), None);
        assert_eq!(redirect_at(&[">"], 0), Some(Redirect { op: ">", target: "", len: 2 }));
    }
}
