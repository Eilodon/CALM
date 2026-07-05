use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

/// Default `busy_timeout` for every writer connection opened against the
/// index DB — long enough to ride out a concurrent transaction (indexer,
/// watcher, another tool handler) without failing outright, short enough
/// that a genuinely stuck writer doesn't hang a caller forever.
pub const WRITER_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens a write-capable connection to `db_path` with `busy_timeout` set —
/// the one thing every writer site (indexer, watcher, `edit_lines`, `ci
/// index`, `doctor`, `calm fitness-check`, embeddings retry) must do to avoid
/// failing outright on the rare overlap with another in-flight writer,
/// instead of each site re-opening a plain connection and forgetting it.
pub fn open_writer(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(WRITER_BUSY_TIMEOUT)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_writer_sets_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let conn = open_writer(&db_path).unwrap();
        // busy_timeout has no getter in rusqlite; verify indirectly via the
        // PRAGMA it maps to.
        let ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ms, WRITER_BUSY_TIMEOUT.as_millis() as i64);
    }
}
