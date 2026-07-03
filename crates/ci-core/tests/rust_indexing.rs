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

#[test]
fn cross_crate_import_resolves_to_path() {
    let conn = index_fixture();
    // `use demo_core::{...}` in app/src/main.rs must resolve to the demo-core crate.
    // Item imports resolve to the crate root file (lib.rs) that re-exports them.
    let to_path: Option<String> = conn
        .query_row(
            "SELECT to_path FROM import_edges \
             WHERE from_path = 'app/src/main.rs' AND module_name = 'demo_core'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to_path.as_deref(), Some("core/src/lib.rs"));
}

#[test]
fn crate_relative_import_resolves() {
    let conn = index_fixture();
    // `pub use engine::Engine;` in core/src/lib.rs -> engine module -> core/src/engine.rs
    let to_path: Option<String> = conn
        .query_row(
            "SELECT to_path FROM import_edges \
             WHERE from_path = 'core/src/lib.rs' AND module_name = 'engine'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to_path.as_deref(), Some("core/src/engine.rs"));
}

#[test]
fn trait_method_declaration_is_a_symbol() {
    let conn = index_fixture();
    // The trait method `Runner::run` (declaration only, no body) must be indexed,
    // qualified by its trait, so "who declares run" / trait API is queryable.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM symbols \
             WHERE qualified_name = 'core/src/lib.rs::Runner::run' AND kind = 'method'",
        ),
        1
    );
}

#[test]
fn constructor_binding_infers_receiver_type() {
    let conn = index_fixture();
    // `let e = Engine::new(); e.start()` -- e's type is inferred from the
    // constructor, so the call resolves to Engine::start with >= inferred confidence.
    let conf: String = conn
        .query_row(
            "SELECT edge_confidence FROM call_edges \
             WHERE from_symbol = 'app/src/main.rs::main' \
               AND to_symbol = 'core/src/engine.rs::Engine::start'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        matches!(conf.as_str(), "inferred" | "resolved" | "formal"),
        "expected >= inferred, got {conf}"
    );
}
