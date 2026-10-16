//! ANSI SGR colors → zsh `region_highlight` spans, so colored preview output (`bat`, `eza`)
//! keeps its colors in the pane. Other escape sequences are dropped.

/// A styled run of a line, in chars: `from..to` gets the zsh style `style` (`fg=203,bold`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Span {
    pub from: usize,
    pub to: usize,
    pub style: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Sgr {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    underline: bool,
}

impl Sgr {
    fn zsh_style(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.fg.as_ref().map(|c| format!("fg={c}")));
        parts.extend(self.bg.as_ref().map(|c| format!("bg={c}")));
        if self.bold {
            parts.push("bold".into());
        }
        if self.underline {
            parts.push("underline".into());
        }
        parts.join(",")
    }

    fn apply(&mut self, params: &str) {
        let mut it = params.split([';', ':']).map(|p| p.parse::<u16>().unwrap_or(0));
        while let Some(p) = it.next() {
            match p {
                0 => *self = Self::default(),
                1 => self.bold = true,
                22 => self.bold = false,
                4 => self.underline = true,
                24 => self.underline = false,
                30..=37 => self.fg = Some((p - 30).to_string()),
                90..=97 => self.fg = Some((p - 90 + 8).to_string()),
                39 => self.fg = None,
                40..=47 => self.bg = Some((p - 40).to_string()),
                100..=107 => self.bg = Some((p - 100 + 8).to_string()),
                49 => self.bg = None,
                38 => self.fg = extended_color(&mut it),
                48 => self.bg = extended_color(&mut it),
                _ => {}
            }
        }
    }
}

/// `5;n` (256 colors) or `2;r;g;b` (true color) after a 38/48.
fn extended_color(it: &mut impl Iterator<Item = u16>) -> Option<String> {
    match it.next()? {
        5 => Some(it.next()?.to_string()),
        2 => {
            let (r, g, b) = (it.next()?, it.next()?, it.next()?);
            Some(format!("#{:02x}{:02x}{:02x}", r.min(255), g.min(255), b.min(255)))
        }
        _ => None,
    }
}

/// Plain text of `line` and the spans its SGR sequences color (adjacent equal styles merged).
pub fn parse(line: &str) -> (String, Vec<Span>) {
    let mut text = String::with_capacity(line.len());
    let mut spans: Vec<Span> = Vec::new();
    let mut state = Sgr::default();
    let mut style = String::new();
    let mut n = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c.is_control() && c != '\t' {
                continue;
            }
            text.push(c);
            match spans.last_mut() {
                Some(s) if s.to == n && s.style == style => s.to += 1,
                _ if !style.is_empty() => spans.push(Span { from: n, to: n + 1, style: style.clone() }),
                _ => {}
            }
            n += 1;
            continue;
        }
        if chars.next_if_eq(&']').is_some() {
            // OSC (title, hyperlink): up to BEL or ST.
            while let Some(d) = chars.next() {
                if d == '\x07' || (d == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
            continue;
        }
        if chars.next_if_eq(&'[').is_none() {
            chars.next();
            continue;
        }
        let mut params = String::new();
        let mut fin = None;
        for d in chars.by_ref() {
            if ('\x40'..='\x7e').contains(&d) {
                fin = Some(d);
                break;
            }
            params.push(d);
        }
        if fin == Some('m') {
            state.apply(&params);
            style = state.zsh_style();
        }
    }
    (text, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: usize, to: usize, style: &str) -> Span {
        Span { from, to, style: style.into() }
    }

    #[test]
    fn colors_become_spans() {
        let (text, spans) = parse("\x1b[38;5;203mh3\x1b[0m \x1b[1;34mdir\x1b[0m");
        assert_eq!(text, "h3 dir");
        assert_eq!(spans, [span(0, 2, "fg=203"), span(3, 6, "fg=4,bold")]);
    }

    #[test]
    fn true_color_and_merging() {
        let (text, spans) = parse("\x1b[38;2;248;248;242mse\x1b[38;2;248;248;242mt\x1b[0m x");
        assert_eq!(text, "set x");
        assert_eq!(spans, [span(0, 3, "fg=#f8f8f2")]);
    }

    #[test]
    fn other_escapes_are_dropped() {
        let (text, spans) = parse("\x1b[2Ka\x1b]0;t\x07b\x1b]8;;u\x1b\\\x1b[4mc");
        assert_eq!(text, "abc");
        assert_eq!(spans, [span(2, 3, "underline")]);
    }
}
