use std::path::Path;

use rusqlite::Connection;

/// Persists a per-file churn signal into `symbols.churn_score`, normalized
/// to `[0, 1]` at write time so `search`'s ranking read is a plain column
/// read with no repo-wide max lookup on the query path.
///
/// Lives under `graph/` (not `analysis/`), mirroring `graph::coreness` --
/// both are called from the indexer pipeline (`rebuild_graph`/
/// `incremental_graph_update`), and `thresholds.toml` declares a
/// zero-tolerance boundary rule forbidding `indexer/ -> analysis/`.
/// `analysis::hotspot` already computes churn too, but for a different
/// purpose (ranked, capped `hotspots` tool output computed on demand);
/// this is a persisted, repo-wide column refreshed on every reindex, a
/// different shape entirely.
///
/// `NULL` (not `0.0`) when git is unavailable: `0.0` means "measured, this
/// file had zero commits in the window", `NULL` means "couldn't measure at
/// all". Conflating them would silently de-rank an entire repo's search
/// results the instant git becomes unavailable, which is a materially
/// different situation from a genuinely inactive file.
pub fn update_churn_scores(
    conn: &Connection,
    project_root: &Path,
    since: &str,
) -> rusqlite::Result<()> {
    let (commits, git_available) = crate::git::commits_with_files_cached(project_root, since);
    if !git_available {
        conn.execute("UPDATE symbols SET churn_score = NULL", [])?;
        return Ok(());
    }

    let signals = crate::git::file_signals(&commits);
    let max_commit_count = signals
        .values()
        .map(|s| s.commit_count)
        .max()
        .unwrap_or(0)
        .max(1);

    // Baseline first: once git itself is available, every indexed file is
    // "measured, zero churn in this window" by default -- NULL is reserved
    // strictly for the git-unavailable branch above, never used here as an
    // implicit "didn't get around to it yet" placeholder.
    conn.execute("UPDATE symbols SET churn_score = 0.0", [])?;

    let mut stmt = conn.prepare("UPDATE symbols SET churn_score = ?1 WHERE path = ?2")?;
    for (path, sig) in &signals {
        let normalized = sig.commit_count as f64 / max_commit_count as f64;
        stmt.execute(rusqlite::params![normalized, path])?;
    }
    Ok(())
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

    fn insert_symbol(conn: &Connection, path: &str, name: &str) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES (?1, ?2, 'function', 'python', ?3, 1, 1)",
            rusqlite::params![format!("{path}::{name}"), name, path],
        )
        .unwrap();
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn test_git_unavailable_sets_null_not_zero() {
        let conn = test_conn();
        insert_symbol(&conn, "a.py", "f");
        let dir = std::env::temp_dir().join(format!("ci_churn_no_git_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        update_churn_scores(&conn, &dir, "1 year").unwrap();

        let score: Option<f64> = conn
            .query_row(
                "SELECT churn_score FROM symbols WHERE path = 'a.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            score, None,
            "git-unavailable must leave churn_score NULL, not 0.0"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unchanged_file_gets_zero_not_null_when_git_available() {
        let dir = std::env::temp_dir().join(format!("ci_churn_zero_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "t@t.test"]);
        run_git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("hot.py"), "1").unwrap();
        run_git(&dir, &["add", "hot.py"]);
        run_git(&dir, &["commit", "-q", "-m", "first"]);

        let conn = test_conn();
        insert_symbol(&conn, "hot.py", "f");
        insert_symbol(&conn, "cold.py", "g"); // never committed at all

        update_churn_scores(&conn, &dir, "1 year").unwrap();

        let hot: Option<f64> = conn
            .query_row(
                "SELECT churn_score FROM symbols WHERE path = 'hot.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hot, Some(1.0), "sole committed file must normalize to 1.0");

        let cold: Option<f64> = conn
            .query_row(
                "SELECT churn_score FROM symbols WHERE path = 'cold.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            cold,
            Some(0.0),
            "a file git legitimately never touched must read as measured-zero, not NULL/unknown"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_normalizes_relative_to_max_commit_count() {
        let dir = std::env::temp_dir().join(format!("ci_churn_norm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "t@t.test"]);
        run_git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("busy.py"), "1").unwrap();
        std::fs::write(dir.join("quiet.py"), "1").unwrap();
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("busy.py"), "2").unwrap();
        run_git(&dir, &["add", "busy.py"]);
        run_git(&dir, &["commit", "-q", "-m", "second"]);
        std::fs::write(dir.join("busy.py"), "3").unwrap();
        run_git(&dir, &["add", "busy.py"]);
        run_git(&dir, &["commit", "-q", "-m", "third"]);

        let conn = test_conn();
        insert_symbol(&conn, "busy.py", "f");
        insert_symbol(&conn, "quiet.py", "g");

        update_churn_scores(&conn, &dir, "1 year").unwrap();

        let busy: f64 = conn
            .query_row(
                "SELECT churn_score FROM symbols WHERE path = 'busy.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let quiet: f64 = conn
            .query_row(
                "SELECT churn_score FROM symbols WHERE path = 'quiet.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(busy, 1.0, "3 commits is the max -> normalizes to 1.0");
        assert!(
            (quiet - 1.0 / 3.0).abs() < 1e-9,
            "1 of 3 commits -> ~0.333, got {quiet}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
