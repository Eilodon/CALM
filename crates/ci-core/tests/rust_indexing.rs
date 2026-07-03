use rusqlite::Connection;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_workspace")
}

/// Index the fixture workspace into an in-memory DB and return the connection.
fn index_fixture() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    ci_core::db::schema::init_db(&conn).unwrap();
    let phase = std::sync::Arc::new(std::sync::RwLock::new(
        ci_core::types::IndexingPhase::Scanning,
    ));
    ci_core::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture_root(), phase).unwrap();
    conn
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn pub_use_reexport_is_indexed() {
    let conn = index_fixture();
    // The `pub use engine::Engine;` line must produce an import edge.
    assert!(
        count(
            &conn,
            "SELECT COUNT(*) FROM import_edges \
             WHERE from_path = 'core/src/lib.rs' AND module_name = 'engine'",
        ) >= 1,
        "pub use re-export must be recorded as an import edge"
    );
}
