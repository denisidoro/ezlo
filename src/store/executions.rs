//! Read-only access to the full execution log (every exit status) for history search.

use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection, OpenFlags};

use super::HistoryEntry;

/// The newest `limit` executions, newest first. A missing database reads as empty and is never
/// created (no schema or memory mirror is set up: this is a one-shot read).
pub fn read_executions(db: &Path, limit: usize) -> Result<Vec<HistoryEntry>> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    // Read-write without CREATE: a read-only handle can't open a WAL database whose `-shm` is gone.
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let mut stmt = conn.prepare("SELECT ts, cwd, line, exit FROM executions ORDER BY id DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
        Ok(HistoryEntry {
            ts: r.get(0)?,
            cwd: r.get(1)?,
            line: r.get(2)?,
            exit: r.get(3)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn reads_failed_runs_newest_first_and_never_creates_the_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("none.db");
        assert!(read_executions(&missing, 10).expect("read").is_empty());
        assert!(!missing.exists());

        let db = dir.path().join("ezlo.db");
        let store = Store::open(&db).expect("open");
        store.record_execution_complete("/tmp", "ls", 0, 100, false);
        store.record_execution_complete("/src", "make", 2, 200, false);
        drop(store);
        let rows = read_executions(&db, 10).expect("read");
        let got: Vec<(&str, i32)> = rows.iter().map(|r| (r.line.as_str(), r.exit)).collect();
        assert_eq!(got, [("make", 2), ("ls", 0)]);
        assert_eq!(read_executions(&db, 1).expect("read").len(), 1);
    }
}
