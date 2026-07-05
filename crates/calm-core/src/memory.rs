use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use crate::analysis::config_drift::{build_real_path_index, resolve_reference};
use crate::analysis::doc_refs::extract_path_refs;
use crate::indexer::pipeline::hash_content;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StaleRef {
    pub reference: String,
    /// "changed" (file still exists but its content hash no longer matches
    /// what was captured at `remember` time) or "deleted" (file no longer
    /// exists at all).
    pub status: &'static str,
}

/// Extracts file-path-like references from a `remember`d note's `content`
/// and resolves each against the real project tree, returning
/// `(repo_relative_path, content_hash)` pairs for the ones that exist right
/// now — a "true when written" snapshot for `check_staleness` to later diff
/// against. References that don't resolve to anything are silently skipped:
/// there's nothing to snapshot, and `analysis::config_drift` is the tool for
/// flagging a broken reference itself, not this one.
pub fn capture_refs(
    project_root: &Path,
    ignore_patterns: &[String],
    content: &str,
) -> Vec<(String, String)> {
    let mut refs = extract_path_refs(content);
    refs.sort();
    refs.dedup();
    if refs.is_empty() {
        return Vec::new();
    }

    let real_paths = build_real_path_index(project_root, ignore_patterns);
    let mut out = Vec::new();
    for r in refs {
        let Some(resolved) = resolve_reference(project_root, &real_paths, &r) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(project_root.join(&resolved)) else {
            continue;
        };
        out.push((resolved, hash_content(&text)));
    }
    out
}

/// Replaces the full ref set for `topic` — same "upsert the whole thing"
/// contract as `remember` itself replacing `content` wholesale rather than
/// appending.
pub fn store_refs(
    conn: &Connection,
    topic: &str,
    refs: &[(String, String)],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM project_memory_refs WHERE topic = ?1",
        rusqlite::params![topic],
    )?;
    for (path, hash) in refs {
        conn.execute(
            "INSERT INTO project_memory_refs (topic, ref_path, ref_hash) VALUES (?1, ?2, ?3)",
            rusqlite::params![topic, path, hash],
        )?;
    }
    Ok(())
}

/// Re-checks every ref captured for `topic` against the live file: a
/// content-hash mismatch is `"changed"`, a file that no longer exists at all
/// is `"deleted"`. Empty result means either no refs were ever captured for
/// this topic, or all captured refs still match — callers that need to tell
/// those two cases apart should check whether any refs were stored at all
/// first (e.g. via a row count).
pub fn check_staleness(
    conn: &Connection,
    project_root: &Path,
    topic: &str,
) -> rusqlite::Result<Vec<StaleRef>> {
    let mut stmt =
        conn.prepare("SELECT ref_path, ref_hash FROM project_memory_refs WHERE topic = ?1")?;
    let rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![topic], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut stale = Vec::new();
    for (path, old_hash) in rows {
        match std::fs::read_to_string(project_root.join(&path)) {
            Ok(text) => {
                if hash_content(&text) != old_hash {
                    stale.push(StaleRef {
                        reference: path,
                        status: "changed",
                    });
                }
            }
            Err(_) => stale.push(StaleRef {
                reference: path,
                status: "deleted",
            }),
        }
    }
    stale.sort_by(|a, b| a.reference.cmp(&b.reference));
    Ok(stale)
}

/// Count of refs captured for `topic` — lets a caller distinguish "nothing
/// to check" (0) from "checked, all still fresh" (>0 captured, empty
/// `check_staleness` result).
pub fn ref_count(conn: &Connection, topic: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM project_memory_refs WHERE topic = ?1",
        rusqlite::params![topic],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ci_memory_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn capture_refs_skips_content_with_no_path_like_tokens() {
        let dir = temp_project("no_refs");
        let refs = capture_refs(&dir, &[], "just a plain rationale note, no file mentioned");
        assert!(refs.is_empty());
    }

    #[test]
    fn capture_refs_skips_unresolved_references() {
        let dir = temp_project("unresolved");
        let refs = capture_refs(&dir, &[], "see server.py for the old entry point");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn capture_refs_snapshots_resolved_file_hash() {
        let dir = temp_project("snapshot");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "the gate lives in `fitness.rs`");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "fitness.rs");
        assert_eq!(refs[0].1, hash_content("// v1\n"));
    }

    #[test]
    fn check_staleness_empty_when_nothing_captured() {
        let conn = test_conn();
        let dir = temp_project("nothing_captured");
        let stale = check_staleness(&conn, &dir, "no-such-topic").unwrap();
        assert!(stale.is_empty());
        assert_eq!(ref_count(&conn, "no-such-topic").unwrap(), 0);
    }

    #[test]
    fn check_staleness_detects_changed_file() {
        let conn = test_conn();
        let dir = temp_project("changed");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "see `fitness.rs`");
        store_refs(&conn, "gate-rationale", &refs).unwrap();

        // File changes after the note was written.
        std::fs::write(dir.join("fitness.rs"), "// v2, totally different\n").unwrap();

        let stale = check_staleness(&conn, &dir, "gate-rationale").unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reference, "fitness.rs");
        assert_eq!(stale[0].status, "changed");
    }

    #[test]
    fn check_staleness_detects_deleted_file() {
        let conn = test_conn();
        let dir = temp_project("deleted");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "see `fitness.rs`");
        store_refs(&conn, "gate-rationale", &refs).unwrap();

        std::fs::remove_file(dir.join("fitness.rs")).unwrap();

        let stale = check_staleness(&conn, &dir, "gate-rationale").unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reference, "fitness.rs");
        assert_eq!(stale[0].status, "deleted");
    }

    #[test]
    fn check_staleness_empty_when_file_unchanged() {
        let conn = test_conn();
        let dir = temp_project("unchanged");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "see `fitness.rs`");
        store_refs(&conn, "gate-rationale", &refs).unwrap();

        let stale = check_staleness(&conn, &dir, "gate-rationale").unwrap();
        assert!(stale.is_empty());
        assert_eq!(ref_count(&conn, "gate-rationale").unwrap(), 1);
    }

    #[test]
    fn store_refs_replaces_previous_set_for_same_topic() {
        let conn = test_conn();
        store_refs(&conn, "t", &[("a.rs".to_string(), "hash_a".to_string())]).unwrap();
        store_refs(&conn, "t", &[("b.rs".to_string(), "hash_b".to_string())]).unwrap();

        assert_eq!(ref_count(&conn, "t").unwrap(), 1);
        let dir = temp_project("replace");
        std::fs::write(dir.join("b.rs"), "hash_b").unwrap(); // arbitrary content, hash won't match "hash_b" literal
        // Only confirm the old "a.rs" ref is gone, not resurrected as stale.
        let stale = check_staleness(&conn, &dir, "t").unwrap();
        assert!(stale.iter().all(|s| s.reference != "a.rs"));
    }
}
