//! `preview` templates of `cmd` blocks: `{opt.<name>}` / `{arg.<name>}` placeholders inside the
//! shell command. Any other `{` (shell braces, `${VAR}`) is left untouched.

/// Which parsed value a placeholder refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    Opt,
    Arg,
}

/// One `{opt.<name>}` / `{arg.<name>}` placeholder of a preview template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRef {
    pub source: PreviewSource,
    pub name: String,
    /// Byte range of the whole placeholder (braces included) in the template.
    pub range: (usize, usize),
}

/// Placeholders of `template`, in order.
pub fn preview_refs(template: &str) -> Vec<PreviewRef> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = template[i..].find('{') {
        let start = i + off;
        let rest = &template[start + 1..];
        let parsed = [("opt.", PreviewSource::Opt), ("arg.", PreviewSource::Arg)]
            .into_iter()
            .find_map(|(prefix, source)| {
                let tail = rest.strip_prefix(prefix)?;
                let name_len = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                    .unwrap_or(tail.len());
                (name_len > 0 && tail[name_len..].starts_with('}'))
                    .then(|| (source, &tail[..name_len], 1 + prefix.len() + name_len + 1))
            });
        match parsed {
            Some((source, name, len)) => {
                out.push(PreviewRef {
                    source,
                    name: name.to_owned(),
                    range: (start, start + len),
                });
                i = start + len;
            }
            None => i = start + 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_opt_and_arg_placeholders_only() {
        let refs = preview_refs("git push --dry-run {opt.force} ${HOME} {a,b} {arg.remote} {arg.}");
        let got: Vec<_> = refs.iter().map(|r| (r.source, r.name.as_str())).collect();
        assert_eq!(got, [(PreviewSource::Opt, "force"), (PreviewSource::Arg, "remote")]);
        assert_eq!(refs[0].range, (19, 30));
    }
}
