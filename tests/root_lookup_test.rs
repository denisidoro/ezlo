//! `SpecIndex` root-word lookups must agree with a plain scan over `cmds`.

use ezlo::spec::{has_errors, parse_and_validate, SpecIndex};

const SRC: &str = r#"cmd (kubectl|k) "Kubernetes"

cmd kubectl get "Get resources"

cmd git "Git"

cmd git (checkout|co) "Switch branches"

cmd kubectl get pods "Pods"
"#;

fn index() -> SpecIndex {
    let (spec, diags) = parse_and_validate(SRC);
    assert!(!has_errors(&diags), "unexpected errors: {diags:?}");
    SpecIndex::from_single(spec)
}

fn path(words: &str) -> Vec<String> {
    words.split_whitespace().map(str::to_string).collect()
}

fn scan<'a>(index: &'a SpecIndex, prefix: &[String]) -> Vec<&'a [String]> {
    index
        .cmds
        .iter()
        .filter(|c| c.path.starts_with(prefix))
        .map(|c| c.path.as_slice())
        .collect()
}

#[test]
fn prefix_lookup_matches_full_scan_in_declaration_order() {
    let index = index();
    for prefix in ["", "kubectl", "kubectl get", "git", "git co", "missing", "kubectl missing"] {
        let prefix = path(prefix);
        let got: Vec<&[String]> = index.cmds_with_prefix(&prefix).map(|c| c.path.as_slice()).collect();
        assert_eq!(got, scan(&index, &prefix), "prefix {prefix:?}");
    }
}

#[test]
fn find_cmd_returns_exact_path_only() {
    let index = index();
    assert_eq!(index.find_cmd(&path("kubectl get pods")).map(|c| c.path.len()), Some(3));
    assert!(index.find_cmd(&path("kubectl pods")).is_none());
    assert!(index.find_cmd(&path("nope")).is_none());
}

#[test]
fn root_words_and_root_aliases_resolve() {
    let index = index();
    assert!(index.has_root("kubectl"));
    assert!(index.has_root("git"));
    assert!(!index.has_root("k"));
    assert_eq!(index.root_alias_target("k"), Some("kubectl"));
    // Subcommand aliases aren't root aliases.
    assert_eq!(index.root_alias_target("co"), None);
}
