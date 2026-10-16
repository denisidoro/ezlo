pub fn shell_quote(val: &str) -> String {
    if val.is_empty() {
        return "''".to_string();
    }
    let safe = !val.starts_with('=')
        && val.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | ':' | '@' | '+' | ',' | '=' | '%')
        });
    if safe {
        return val.to_string();
    }
    let mut out = String::with_capacity(val.len() + 2);
    out.push('\'');
    for c in val.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}
