//! `jq` filter paths of a JSON sample: one document, a stream of concatenated documents or JSON
//! lines (a truncated sample keeps the documents parsed before the cut).

use serde_json::Value;
use std::collections::HashSet;

const MAX_DEPTH: usize = 6;
const MAX_PATHS: usize = 500;
const MAX_DOCS: usize = 200;
/// Array elements inspected for keys (elements of a homogeneous array share their keys).
const MAX_ITEMS: usize = 20;
const MAX_SAMPLE_CHARS: usize = 32;

/// A `jq` path (`.users[].name`) with a description of the value found there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPath {
    pub path: String,
    pub desc: String,
}

pub fn collect_paths(text: &str) -> Vec<JsonPath> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    for doc in stream.take(MAX_DOCS) {
        let Ok(doc) = doc else { break };
        walk(&doc, "", 0, &mut out, &mut seen);
        if out.len() >= MAX_PATHS {
            break;
        }
    }
    out
}

fn walk(value: &Value, prefix: &str, depth: usize, out: &mut Vec<JsonPath>, seen: &mut HashSet<String>) {
    if depth >= MAX_DEPTH {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if out.len() >= MAX_PATHS {
                    return;
                }
                let path = format!("{prefix}{}", key_segment(key));
                push(out, seen, &path, describe(child));
                walk(child, &path, depth + 1, out, seen);
            }
        }
        Value::Array(items) => {
            let path = if prefix.is_empty() { ".[]".to_owned() } else { format!("{prefix}[]") };
            push(out, seen, &path, format!("each of {} items", items.len()));
            for item in items.iter().take(MAX_ITEMS) {
                walk(item, &path, depth + 1, out, seen);
            }
        }
        _ => {}
    }
}

fn push(out: &mut Vec<JsonPath>, seen: &mut HashSet<String>, path: &str, desc: String) {
    if out.len() < MAX_PATHS && seen.insert(path.to_owned()) {
        out.push(JsonPath {
            path: path.to_owned(),
            desc,
        });
    }
}

/// `.key` for identifier keys, `."some key"` otherwise.
fn key_segment(key: &str) -> String {
    let mut chars = key.chars();
    let ident = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ident {
        format!(".{key}")
    } else {
        format!(".{}", serde_json::to_string(key).unwrap_or_default())
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => format!("boolean {b}"),
        Value::Number(n) => format!("number {n}"),
        Value::String(s) => {
            let mut short: String = s.chars().take(MAX_SAMPLE_CHARS).collect();
            if s.chars().count() > MAX_SAMPLE_CHARS {
                short.push('…');
            }
            format!("string {short:?}")
        }
        Value::Array(items) => format!("array [{}]", items.len()),
        Value::Object(map) => format!("object {{{} keys}}", map.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(text: &str) -> Vec<String> {
        collect_paths(text).into_iter().map(|p| p.path).collect()
    }

    #[test]
    fn nested_objects_and_arrays() {
        let p = paths(r#"{"users":[{"name":"a","tags":["x"]},{"name":"b","age":3}],"odd key":1}"#);
        for want in [".users", ".users[]", ".users[].name", ".users[].tags", ".users[].tags[]", ".users[].age", r#"."odd key""#] {
            assert!(p.contains(&want.to_owned()), "missing {want} in {p:?}");
        }
    }

    #[test]
    fn root_array_and_json_lines() {
        assert_eq!(paths(r#"[{"id":1}]"#), [".[]", ".[].id"]);
        assert_eq!(paths("{\"a\":1}\n{\"b\":2}\n{\"c\":"), [".a", ".b"]);
    }

    #[test]
    fn describes_values() {
        let p = collect_paths(r#"{"s":"hello","n":4,"o":{"k":null}}"#);
        let desc = |path: &str| p.iter().find(|j| j.path == path).unwrap().desc.clone();
        assert_eq!(desc(".s"), "string \"hello\"");
        assert_eq!(desc(".n"), "number 4");
        assert_eq!(desc(".o"), "object {1 keys}");
    }

    #[test]
    fn not_json_yields_nothing() {
        assert!(paths("name,age\nbob,3\n").is_empty());
    }
}
