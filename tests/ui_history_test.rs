use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use ezlo::config::{expand_path, Config};
use ezlo::engine::candidate::{Candidate, CandidateKind};
use ezlo::engine::icons::{self, Glyphs};
use ezlo::engine::rank::layout::format_rows;
use ezlo::engine::types::format::{format_age, format_size};
use ezlo::engine::types::path::complete_builtin_path;
use ezlo::history::{
    dedupe_newest_first, file_matches, history_candidates, merge_history, parse_history, unmetafy,
    HistMatch,
};
use ezlo::store::Store;
use unicode_width::UnicodeWidthStr;

fn cand(label: &str, kind: CandidateKind, desc: &str, columns: &[&str]) -> Candidate {
    Candidate {
        label: label.to_string(),
        description: desc.to_string(),
        insert_value: label.to_string(),
        insert_text: label.to_string(),
        suffix: " ".to_string(),
        kind,
        columns: columns.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn history_parses_zsh_extended_plain_and_metafied() {
    let mut raw = b": 1700000000:0;git status\n: 1700000100:0;echo multi \\\nline\nls -la\n: 1700000200:0;docker ps\n".to_vec();
    // "café" metafied: é = 0xC3 0xA9 → 0xA9 >= 0x83 is stored as 0x83, 0xA9^0x20
    raw.extend_from_slice(b": 1700000300:0;echo caf\xc3\x83\x89\n");
    assert_eq!(unmetafy(b"\x83\x89"), vec![0xA9]);

    let entries = parse_history(&raw, 42);
    let lines: Vec<&str> = entries.iter().map(|e| e.line.as_str()).collect();
    assert_eq!(
        lines,
        vec!["git status", "ls -la", "docker ps", "echo café"]
    );
    assert_eq!(entries[0].ts, 1_700_000_000);
    assert_eq!(entries[1].ts, 42);
    assert_eq!(entries[2].ts, 1_700_000_200);
}

#[test]
fn history_dedupes_newest_first_and_matches_prefix() {
    let raw = b": 1:0;git pull\n: 2:0;git push\n: 3:0;git pull\n: 4:0;git\n";
    let entries = dedupe_newest_first(parse_history(raw, 0));
    let lines: Vec<&str> = entries.iter().map(|e| e.line.as_str()).collect();
    assert_eq!(lines, vec!["git", "git pull", "git push"]);
    assert_eq!(entries[1].ts, 3);

    let m = file_matches(&entries, "git p", 10);
    assert_eq!(m.len(), 2);
    // Exact match of the buffer is not offered
    assert!(file_matches(&entries, "git", 10)
        .iter()
        .all(|h| h.line != "git"));
}

#[test]
fn history_rows_insert_rest_of_line_and_land_in_first_window() {
    let matches = vec![
        HistMatch {
            line: "git push -u origin HEAD".into(),
            ts: 1_000,
            in_cwd: true,
            ..Default::default()
        },
        HistMatch {
            line: "git push -u origin HEAD".into(),
            ts: 900,
            in_cwd: false,
            ..Default::default()
        },
        HistMatch {
            line: "git pull --rebase".into(),
            ts: 0,
            in_cwd: false,
            ..Default::default()
        },
    ];
    let rows = history_candidates(matches, 4, 3, 4_600);
    assert_eq!(rows.len(), 2, "duplicates collapse");
    assert_eq!(rows[0].kind, CandidateKind::History);
    assert_eq!(rows[0].insert_text, "push -u origin HEAD");
    assert_eq!(rows[0].suffix, "");
    assert_eq!(
        rows[0].columns,
        vec!["here".to_string(), String::new(), "1h ago".to_string()]
    );

    let mut cands: Vec<Candidate> = (0..10)
        .map(|i| cand(&format!("sub{i}"), CandidateKind::Sub, "", &[]))
        .collect();
    merge_history(&mut cands, rows, 6);
    assert_eq!(cands.len(), 12);
    assert_eq!(cands[4].kind, CandidateKind::History);
    assert_eq!(cands[5].kind, CandidateKind::History);
    assert_eq!(cands[3].label, "sub3");
}

#[test]
fn layout_uses_full_width_and_aligns_columns() {
    let cols = 160;
    let cands = vec![
        cand(
            "feature/very-long-branch-name-that-is-long",
            CandidateKind::Arg,
            "",
            &["Add thing", "2 days ago"],
        ),
        cand(
            "main",
            CandidateKind::Arg,
            "",
            &["Merge pull request #12 from foo", "3 hours ago"],
        ),
        cand("--force", CandidateKind::Opt, "Force the operation", &[]),
    ];
    let rows = format_rows(&cands, cols, Glyphs::Nerd);
    for r in &rows {
        assert_eq!(
            UnicodeWidthStr::width(r.display_text.as_str()),
            cols - 1,
            "{:?}",
            r.display_text
        );
        assert!(!r.display_text.contains(['①', '②', '③']));
    }
    // Label longer than the old 36-col cap is shown in full on a wide terminal
    assert!(rows[0]
        .display_text
        .contains("feature/very-long-branch-name-that-is-long"));
    // Data columns are aligned: second column starts at the same offset on both data rows
    let off = |row: &str, needle: &str| row.find(needle).unwrap_or(usize::MAX);
    assert_eq!(
        off(&rows[0].display_text, "Add thing"),
        off(&rows[1].display_text, "Merge pull")
    );
    // The trailing column is right-aligned: both ages end at the right edge.
    let end = |row: &str| row.trim_end().len();
    assert_eq!(end(&rows[0].display_text), end(&rows[1].display_text));
    // Prose descriptions are not grey anymore; labels get the kind color
    assert!(!rows[2].highlights.contains("fg=8"));
    assert!(rows[2].highlights.contains("fg=yellow"));
}

#[test]
fn layout_right_anchors_data_columns_for_paths_and_history() {
    let cols = 120;
    let cands = vec![
        cand("Calibre/", CandidateKind::Dir, "", &[" 3 items", "1y ago"]),
        cand(
            "Downloads/",
            CandidateKind::Dir,
            "",
            &["16 items", "2mo ago"],
        ),
        cand(
            "ls 'Calibre/T.S. Eliot/cover.jpg'",
            CandidateKind::History,
            "",
            &["here", "1h ago"],
        ),
        cand("ls -lah", CandidateKind::History, "", &["", "2h ago"]),
    ];
    let rows = format_rows(&cands, cols, Glyphs::Nerd);
    let text: Vec<&str> = rows.iter().map(|r| r.display_text.trim_end()).collect();
    // Ages (trailing column) are right-aligned across file and history rows alike.
    let end = |s: &str| UnicodeWidthStr::width(s);
    assert!(text.iter().all(|t| end(t) == end(text[0])), "{text:#?}");
    // Columns are sized per kind: dir rows align their item counts, history rows their markers.
    let off = |row: &str, needle: &str| row.find(needle).unwrap_or(usize::MAX);
    assert_eq!(off(text[0], " 3 items"), off(text[1], "16 items"), "{text:#?}");
    assert!(text[1].contains("16 items   2mo ago"), "{:?}", text[1]);
    assert!(text[2].contains("ls 'Calibre/T.S. Eliot/cover.jpg'"), "{:?}", text[2]);
}

#[test]
fn layout_fits_narrow_terminals_and_dims_deemphasized() {
    let mut c = cand(
        "日本語-branch-with-a-long-name",
        CandidateKind::Arg,
        "",
        &["CJK 🚀 subject that is long", "1d ago"],
    );
    c.deemphasized = true;
    for cols in [24, 40, 80] {
        let rows = format_rows(std::slice::from_ref(&c), cols, Glyphs::Nerd);
        assert!(UnicodeWidthStr::width(rows[0].display_text.as_str()) < cols);
        assert!(rows[0].highlights.contains("fg=8"));
    }
}

#[test]
fn path_rows_have_size_age_and_type_styles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir(root.join("sub")).expect("mkdir");
    fs::write(root.join("sub/a"), "x").expect("write");
    fs::write(root.join("sub/b"), "y").expect("write");
    fs::write(root.join("notes.md"), "hello world").expect("write");
    fs::write(root.join("run.sh"), "#!/bin/sh\n").expect("write");
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).expect("chmod");
    std::os::unix::fs::symlink("notes.md", root.join("link")).expect("symlink");
    fs::create_dir_all(root.join("big/nested")).expect("mkdir");
    for i in 0..150 {
        fs::write(root.join(format!("big/nested/{i}")), "z").expect("write");
    }

    let env = HashMap::new();
    let rows = complete_builtin_path("path", "", root, &env);
    let get = |l: &str| {
        rows.iter()
            .find(|r| r.label == l)
            .unwrap_or_else(|| panic!("missing {l}"))
    };

    let sub = get("sub/");
    assert!(sub.is_dir && sub.columns[0].trim() == "2 B", "{:?}", sub.columns);
    let big = get("big/");
    assert!(big.columns[0].trim().starts_with("≥ "), "{:?}", big.columns);
    let notes = get("notes.md");
    assert!(notes.is_file);
    assert_eq!(notes.columns[0].trim(), "11 B");
    assert_eq!(notes.columns[1], "just now");
    assert_eq!(notes.style, Some("fg=yellow"));
    assert_eq!(get("run.sh").style, Some("fg=green,bold"));
    let link = get("link");
    assert_eq!(link.columns[0], "→ notes.md");
    assert_eq!(link.icon, Some(icons::FILE_LINK));
    assert_eq!(notes.icon, icons::for_file_name("notes.md"));
    assert!(notes.icon.is_some());
    assert_eq!(get("run.sh").icon, icons::for_file_name("run.sh"));
    assert_eq!(sub.icon, Some(icons::DIR));

    let dirs = complete_builtin_path("dir", "", root, &env);
    assert!(dirs.iter().all(|r| r.is_dir));
}

#[test]
fn size_and_age_formatting() {
    assert_eq!(format_size(999), "999 B");
    assert_eq!(format_size(4_200), "4.2 kB");
    assert_eq!(format_size(12_000_000), "12 MB");
    assert_eq!(format_age(std::time::Duration::from_secs(5)), "just now");
    assert_eq!(format_age(std::time::Duration::from_secs(7_200)), "2h ago");
    assert_eq!(
        format_age(std::time::Duration::from_secs(3 * 86_400)),
        "3d ago"
    );
}

#[test]
fn spec_dirs_from_config_expand_home_and_vars() {
    let home = std::env::var("HOME").unwrap_or_default();
    let cfg = Config {
        sources: ezlo::config::SourcesConfig {
            user: ezlo::config::UserSourceConfig {
                dirs: vec!["~/specs".into(), "/abs/specs".into()],
            },
            ..Default::default()
        },
        ..Config::default()
    };
    let dirs = cfg.spec_dirs();
    assert_eq!(dirs[0], std::path::Path::new(&home).join("specs"));
    assert_eq!(dirs[1], std::path::PathBuf::from("/abs/specs"));
    assert_eq!(
        expand_path("$HOME/x"),
        std::path::PathBuf::from(format!("{home}/x"))
    );
    assert_eq!(
        expand_path("${HOME}/y"),
        std::path::PathBuf::from(format!("{home}/y"))
    );
}

#[test]
fn store_history_matches_prefers_cwd() {
    let store = Store::open_in_memory().expect("store");
    store.record_execution_complete("/a", "make build", 0, 1, false);
    store.record_execution_complete("/b", "make test", 0, 2, false);
    store.record_execution_complete("/b", "make broken", 1, 3, false);
    let m = store.history_matches("make ", "/a", false, 5);
    let lines: Vec<&str> = m.iter().map(|h| h.line.as_str()).collect();
    assert_eq!(lines, vec!["make build", "make test"]);
    assert!(m[0].in_cwd && !m[1].in_cwd);
}
