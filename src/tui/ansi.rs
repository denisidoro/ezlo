//! ANSI handling for command previews (sanitizing and converting SGR to ratatui styles).

use super::*;

/// Preserves ANSI SGR sequences (`\x1b[[0-9;:]*m`) and strips all other escape sequences.
pub fn sanitize_preview_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len()
                    && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b':')
                {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'm' {
                    if let Ok(seq) = std::str::from_utf8(&bytes[i..=j]) {
                        out.push_str(seq);
                    }
                    i = j + 1;
                    continue;
                } else {
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    i = (j + 1).min(bytes.len());
                    continue;
                }
            } else if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            } else {
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap_or('\0');
        if ch == '\n' || ch == '\t' || !ch.is_control() {
            out.push(ch);
        }
        i += ch.len_utf8().max(1);
    }
    out
}

/// Converts text containing SGR escapes (as kept by [`sanitize_preview_ansi`]) into styled
/// ratatui text. Raw escapes must never reach the buffer: the terminal would interpret them
/// while ratatui counts them as cells, corrupting the layout.
pub fn ansi_to_text(input: &str) -> Text<'static> {
    let lines = input.split('\n').map(|raw| {
        let mut spans = Vec::new();
        let mut style = Style::default();
        let mut rest = raw;
        while let Some(pos) = rest.find("\x1b[") {
            push_plain(&mut spans, &rest[..pos], style);
            let after = &rest[pos + 2..];
            let Some(end) = after.find('m') else {
                rest = "";
                break;
            };
            style = apply_sgr(style, &after[..end]);
            rest = &after[end + 1..];
        }
        push_plain(&mut spans, rest, style);
        Line::from(spans)
    });
    Text::from(lines.collect::<Vec<_>>())
}

pub(super) fn push_plain(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if !text.is_empty() {
        spans.push(Span::styled(text.replace('\t', "    "), style));
    }
}

pub(super) fn apply_sgr(mut style: Style, params: &str) -> Style {
    let codes: Vec<u16> = params
        .split([';', ':'])
        .map(|c| c.parse().unwrap_or(0))
        .collect();
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        match code {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            30..=37 => style = style.fg(Color::Indexed((code - 30) as u8)),
            90..=97 => style = style.fg(Color::Indexed((code - 90 + 8) as u8)),
            40..=47 => style = style.bg(Color::Indexed((code - 40) as u8)),
            100..=107 => style = style.bg(Color::Indexed((code - 100 + 8) as u8)),
            39 => style = style.fg(Color::Reset),
            49 => style = style.bg(Color::Reset),
            38 | 48 => {
                let (color, used) = extended_color(&codes[i + 1..]);
                if let Some(color) = color {
                    style = if code == 38 { style.fg(color) } else { style.bg(color) };
                }
                i += used;
            }
            _ => {}
        }
        i += 1;
    }
    style
}

/// Parses `5;n` or `2;r;g;b` after a 38/48 code; returns the color and params consumed.
pub(super) fn extended_color(rest: &[u16]) -> (Option<Color>, usize) {
    let byte = |v: &u16| u8::try_from(*v).unwrap_or(u8::MAX);
    match rest {
        [5, n, ..] => (Some(Color::Indexed(byte(n))), 2),
        [2, r, g, b, ..] => (Some(Color::Rgb(byte(r), byte(g), byte(b))), 4),
        _ => (None, rest.len()),
    }
}
