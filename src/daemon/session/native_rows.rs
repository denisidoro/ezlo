//! `NATIVE`: matches of the shell's own zsh completion (Tab), laid out as dropdown rows.
//!
//! zsh decides what matches and inserts the picked one (the client replays it through
//! `compadd`); the daemon only renders them like its own rows, so Tab and suggestions look the same.

use super::Session;
use crate::daemon::protocol::{build_wire_response_capped, encode_wire_response, NativeRow, MAX_NATIVE_ROWS};
use crate::engine::candidate::{Candidate, CandidateKind};
use crate::engine::types::path::{path_details, size_deadline};
use std::path::Path;
use std::time::Instant;

impl Session {
    /// Answers `NATIVE` with an `R` response of `rows`, in zsh's order (never re-ranked).
    pub(super) fn native_rows(&mut self, id: u64, rows: &[NativeRow]) {
        let deadline = size_deadline();
        let cands: Vec<Candidate> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| native_candidate(i, row, deadline))
            .collect();
        let max_rows = self.settings().rows;
        let wire = build_wire_response_capped(id, 0, 0, "", &cands, self.cols, self.glyphs(), max_rows, MAX_NATIVE_ROWS)
            .with_pane(&cands, &self.overrides.pane_options(&self.shared.config, self.cols, self.glyphs()));
        self.send(encode_wire_response(&wire));
        self.last_candidates = cands;
        self.last_usage_ctx.clear();
    }
}

/// Row `index` (0-based): its insert text is the 1-based index, which the client maps back to the
/// zsh match. Existing files and directories get the built-in path rows' icon, style, size and age
/// (and a path preview pane).
fn native_candidate(index: usize, row: &NativeRow, deadline: Instant) -> Candidate {
    let kind = match row.kind.as_str() {
        "cmd" => CandidateKind::Cmd,
        "opt" => CandidateKind::Opt,
        _ => CandidateKind::Arg,
    };
    let mut cand = Candidate {
        label: row.label.clone(),
        description: row.description.clone(),
        insert_value: row.label.clone(),
        insert_text: (index + 1).to_string(),
        kind,
        ..Default::default()
    };
    if row.kind != "file" || row.path.is_empty() {
        return cand;
    }
    let Some(details) = path_details(Path::new(&row.path), deadline) else {
        return cand;
    };
    cand.kind = if details.is_dir { CandidateKind::Dir } else { CandidateKind::File };
    if details.is_dir && !cand.label.ends_with('/') {
        cand.label.push('/');
    }
    cand.insert_value.clone_from(&row.path);
    cand.columns = details.columns;
    cand.style = details.style;
    cand.icon = details.icon;
    cand
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, kind: &str, path: &str) -> NativeRow {
        NativeRow {
            label: label.to_owned(),
            description: String::new(),
            kind: kind.to_owned(),
            path: path.to_owned(),
        }
    }

    #[test]
    fn rows_keep_kinds_and_index_insert_texts() {
        let deadline = size_deadline();
        let opt = native_candidate(0, &row("--all", "opt", ""), deadline);
        assert_eq!((opt.kind, opt.insert_text.as_str()), (CandidateKind::Opt, "1"));
        let cmd = native_candidate(4, &row("git", "cmd", ""), deadline);
        assert_eq!((cmd.kind, cmd.insert_text.as_str()), (CandidateKind::Cmd, "5"));
    }

    #[test]
    fn existing_paths_render_like_path_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "hi").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        let deadline = size_deadline();
        let file_path = dir.path().join("a.txt").display().to_string();
        let file = native_candidate(0, &row("a.txt", "file", &file_path), deadline);
        assert_eq!(file.kind, CandidateKind::File);
        assert_eq!(file.insert_value, file_path);
        assert!(file.icon.is_some() && !file.columns.is_empty());
        let sub = native_candidate(1, &row("sub", "file", &dir.path().join("sub").display().to_string()), deadline);
        assert_eq!((sub.kind, sub.label.as_str()), (CandidateKind::Dir, "sub/"));
        let gone = native_candidate(2, &row("gone", "file", &dir.path().join("gone").display().to_string()), deadline);
        assert_eq!(gone.kind, CandidateKind::Arg, "a missing path is a plain row");
    }
}

#[cfg(test)]
mod wire_tests {
    use crate::daemon::protocol::{encode_client_msg, parse_client_line, ClientMsg, NativeRow};

    #[test]
    fn native_message_round_trips_and_drops_partial_rows() {
        let rows = vec![
            NativeRow { label: "a b".into(), description: "desc".into(), kind: "file".into(), path: "/t/a b".into() },
            NativeRow { label: "-v".into(), description: String::new(), kind: "opt".into(), path: String::new() },
        ];
        let msg = ClientMsg::Native { id: 7, rows };
        assert_eq!(parse_client_line(&encode_client_msg(&msg)), Some(msg));
        let partial = "NATIVE\x1f3\x1fx\x1f\x1farg\x1f\x1fy\x1fonly";
        let Some(ClientMsg::Native { id: 3, rows }) = parse_client_line(partial) else {
            panic!("NATIVE should parse");
        };
        assert_eq!(rows.len(), 1);
    }
}
