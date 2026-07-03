//! Live integration test for the file watcher: spin up `run_watch_loop` on a
//! real temp project, mutate files on disk, and assert the index follows.
//!
//! Uses the real `notify` backend, so it exercises the actual FS-event →
//! debounce → incremental-reindex path end to end.

use std::path::Path;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

fn symbol_count(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap()
}

/// Poll `db` until `want` symbols are present or the deadline passes.
fn wait_for_symbols(db: &Path, want: i64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if symbol_count(db) == want {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    symbol_count(db) == want
}

#[test]
fn watcher_reindexes_add_and_delete() {
    let dir = std::env::temp_dir().join(format!(
        "ci_watch_it_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.py"), "def a():\n    pass\n").unwrap();

    let db_path = dir.join(".codeindex").join("index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        ci_core::db::schema::init_db(&conn).unwrap();
        ci_core::indexer::pipeline::run_indexing_pipeline(
            &mut conn,
            &dir,
            std::sync::Arc::new(std::sync::RwLock::new(
                ci_core::types::IndexingPhase::Scanning,
            )),
        )
        .unwrap();
    }
    assert_eq!(symbol_count(&db_path), 1, "initial index");

    let ct = CancellationToken::new();
    let handle = {
        let (root, db, ct) = (dir.clone(), db_path.clone(), ct.clone());
        let embedder: ci_server::EmbedderHandle = std::sync::Arc::new(std::sync::RwLock::new(None));
        std::thread::spawn(move || ci_server::watcher::run_watch_loop(root, db, ct, embedder))
    };

    // Let the watcher register before mutating the tree.
    std::thread::sleep(Duration::from_millis(400));

    // Add a file → watcher should incrementally index it.
    std::fs::write(dir.join("b.py"), "def b():\n    pass\n").unwrap();
    let added = wait_for_symbols(&db_path, 2, Duration::from_secs(15));

    // Delete it → watcher should drop its symbol.
    std::fs::remove_file(dir.join("b.py")).unwrap();
    let removed = wait_for_symbols(&db_path, 1, Duration::from_secs(15));

    ct.cancel();
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(added, "watcher should have indexed the added file");
    assert!(removed, "watcher should have dropped the deleted file");
}

/// Regression for the concurrency gap identified during Tier B design: an
/// `edit_lines` call's own write+reindex sequence can race the file
/// watcher's independently fs-notify-triggered reindex on the very file it
/// just wrote. Before `busy_timeout` was added to the watcher's write
/// connection (`watcher.rs`), an unlucky overlap of two `reindex_changed`
/// transactions on the same DB file could fail outright with "database is
/// locked" instead of one simply waiting for the other. This simulates
/// `edit_lines_impl`'s exact sequence (atomic_write, then reindex_changed on
/// a busy_timeout'd connection) firing right after mutating the file the
/// watcher is also about to reindex.
#[test]
fn concurrent_edit_write_and_watcher_reindex_does_not_lock_or_go_stale() {
    let dir = std::env::temp_dir().join(format!(
        "ci_watch_race_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.py"), "def a():\n    return 1\n").unwrap();

    let db_path = dir.join(".codeindex").join("index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        ci_core::db::schema::init_db(&conn).unwrap();
        ci_core::indexer::pipeline::run_indexing_pipeline(
            &mut conn,
            &dir,
            std::sync::Arc::new(std::sync::RwLock::new(
                ci_core::types::IndexingPhase::Scanning,
            )),
        )
        .unwrap();
    }
    assert_eq!(symbol_count(&db_path), 1, "initial index");

    let ct = CancellationToken::new();
    let handle = {
        let (root, db, ct) = (dir.clone(), db_path.clone(), ct.clone());
        let embedder: ci_server::EmbedderHandle = std::sync::Arc::new(std::sync::RwLock::new(None));
        std::thread::spawn(move || ci_server::watcher::run_watch_loop(root, db, ct, embedder))
    };
    std::thread::sleep(Duration::from_millis(400));

    // Simulate edit_lines_impl's own write+reindex sequence, firing right
    // after a file write the watcher is independently about to react to —
    // exactly the race B1's busy_timeout is meant to survive.
    ci_core::edit::atomic_write(
        &dir.join("a.py"),
        "def a():\n    return 2\n\n\ndef b():\n    return 3\n",
    )
    .unwrap();
    let race_result = {
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        ci_core::indexer::pipeline::reindex_changed(&mut conn, &dir)
    };
    if let Err(e) = &race_result {
        let msg = e.to_string().to_lowercase();
        assert!(
            !msg.contains("locked") && !msg.contains("busy"),
            "got a lock-contention error despite busy_timeout on both sides: {e}"
        );
    }

    // Both writers should converge on the latest file content (2 symbols),
    // not leave the DB stale relative to disk.
    let converged = wait_for_symbols(&db_path, 2, Duration::from_secs(15));

    ct.cancel();
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        converged,
        "DB must reflect the latest file content once both writers settle"
    );
}
