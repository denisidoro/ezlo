use std::time::Duration;

/// Suffix to append after accepting a value: none when the value is obviously incomplete
/// (`host:`, `dir/`, `key=`), a space otherwise.
pub fn continuation_suffix(value: &str) -> &'static str {
    if value.ends_with([':', '/', '=']) {
        ""
    } else {
        " "
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

pub fn format_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", s / 60),
        3_600..=86_399 => format!("{}h ago", s / 3_600),
        86_400..=2_591_999 => format!("{}d ago", s / 86_400),
        2_592_000..=31_535_999 => format!("{}mo ago", s / 2_592_000),
        _ => format!("{}y ago", s / 31_536_000),
    }
}
