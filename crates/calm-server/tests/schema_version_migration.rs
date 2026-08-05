//! Integration tests for the 2026-08-05 schema-version downgrade guard
//! (crates/calm-core/src/db/schema.rs's `INDEX_DB_SCHEMA_VERSION`/
//! `STATE_DB_SCHEMA_VERSION` + `refuse_if_schema_newer`), exercised through
//! the real production entry point (`CalmServer::new`, which calls
//! `new_with_preset`) rather than the raw schema functions calm-core's own
//! unit tests already cover in `schema.rs` -- these prove the WIRING (that
//! the real bootstrap path actually calls the versioned wrappers), plus the
//! specific migration/crash/downgrade scenarios named in the plan this
//! guard responds to: fresh install, restart, upgrade from a pre-versioning
//! install, a crash between schema DDL and the version stamp, and an old
//! binary meeting a file a newer one already touched.

use calm_server::tools::CalmServer;
use std::path::{Path, PathBuf};

fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ci_schema_ver_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn user_version(db_path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn insert_dummy_symbol(conn: &rusqlite::Connection, qualified_name: &str) {
    conn.execute(
        "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, \
         signature, docstring, name_tokens, caller_count, is_hub, is_entry_point) \
         VALUES (?1, 'sym', 'function', 'python', 'f.py', 1, 2, '', '', 'sym', 0, 0, 0)",
        [qualified_name],
    )
    .unwrap();
}

#[test]
fn fresh_install_stamps_both_databases() {
    let dir = temp_project("fresh");
    let db_path = dir.join("index.db");

    let server = CalmServer::new(dir.clone(), db_path.clone()).unwrap();
    drop(server);

    let state_db_path = calm_server::default_state_db_path(&dir);
    assert_eq!(
        user_version(&db_path),
        calm_core::db::schema::INDEX_DB_SCHEMA_VERSION
    );
    assert_eq!(
        user_version(&state_db_path),
        calm_core::db::schema::STATE_DB_SCHEMA_VERSION
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_after_fresh_install_succeeds_and_stays_stamped() {
    let dir = temp_project("restart");
    let db_path = dir.join("index.db");

    let first = CalmServer::new(dir.clone(), db_path.clone()).unwrap();
    drop(first);
    // Simulates a process restart against the same on-disk state -- must
    // not fail just because the previous process already stamped it.
    let second = CalmServer::new(dir.clone(), db_path.clone()).unwrap();
    drop(second);

    let state_db_path = calm_server::default_state_db_path(&dir);
    assert_eq!(
        user_version(&db_path),
        calm_core::db::schema::INDEX_DB_SCHEMA_VERSION
    );
    assert_eq!(
        user_version(&state_db_path),
        calm_core::db::schema::STATE_DB_SCHEMA_VERSION
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn upgrade_from_a_pre_versioning_install_stamps_forward_without_data_loss() {
    // Simulates every real install that existed before this guard shipped:
    // a schema created by the OLD (unversioned) init_db/init_state_db --
    // PRAGMA user_version defaults to 0, never stamped, exactly like every
    // pre-2026-08-05 .calm/index.db and .calm/state.db on disk today.
    let dir = temp_project("upgrade");
    let db_path = dir.join("index.db");
    std::fs::create_dir_all(dir.join(".calm")).unwrap();
    let state_db_path = calm_server::default_state_db_path(&dir);

    {
        let conn = calm_core::db::conn::open_writer(&db_path).unwrap();
        calm_core::db::schema::init_db(&conn).unwrap();
        insert_dummy_symbol(&conn, "pre_existing::sym");
    }
    {
        let state_conn = calm_core::db::conn::open_state_writer(&state_db_path).unwrap();
        calm_core::db::schema::init_state_db(&state_conn).unwrap();
    }
    assert_eq!(
        user_version(&db_path),
        0,
        "pre-upgrade fixture must start unstamped"
    );

    // The new (versioned) binary now opens this exact pre-existing project.
    let server = CalmServer::new(dir.clone(), db_path.clone()).unwrap();
    drop(server);

    assert_eq!(
        user_version(&db_path),
        calm_core::db::schema::INDEX_DB_SCHEMA_VERSION
    );
    assert_eq!(
        user_version(&state_db_path),
        calm_core::db::schema::STATE_DB_SCHEMA_VERSION
    );
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'pre_existing::sym'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "pre-existing data must survive the upgrade");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn crash_between_schema_ddl_and_version_stamp_recovers_cleanly_on_restart() {
    // Simulates a process that died after init_db's own DDL ran but before
    // init_db_versioned's pragma_update -- indistinguishable, from the
    // guard's point of view, from the "upgrade" fixture above: on_disk
    // version is 0, and recovery is just "run the (idempotent) versioned
    // wrapper again on the next process start."
    let dir = temp_project("crash_mid_migration");
    let db_path = dir.join("index.db");
    {
        let conn = calm_core::db::conn::open_writer(&db_path).unwrap();
        calm_core::db::schema::init_db(&conn).unwrap();
    }
    assert_eq!(user_version(&db_path), 0);

    let server = CalmServer::new(dir.clone(), db_path.clone());
    assert!(
        server.is_ok(),
        "restart after an interrupted (pre-stamp) init must recover, not fail: {:?}",
        server.err()
    );
    drop(server.unwrap());
    assert_eq!(
        user_version(&db_path),
        calm_core::db::schema::INDEX_DB_SCHEMA_VERSION
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn running_old_binary_after_a_newer_one_already_upgraded_is_refused_without_data_loss() {
    let dir = temp_project("old_binary");
    let db_path = dir.join("index.db");
    let server = CalmServer::new(dir.clone(), db_path.clone()).unwrap();
    drop(server);

    // Simulate "a newer CALM binary already migrated this file further"
    // by bumping PRAGMA user_version past what this binary knows about.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.pragma_update(
            None,
            "user_version",
            calm_core::db::schema::INDEX_DB_SCHEMA_VERSION + 1,
        )
        .unwrap();
        insert_dummy_symbol(&conn, "future::sym");
    }

    let result = CalmServer::new(dir.clone(), db_path.clone());
    assert!(
        result.is_err(),
        "an old binary meeting a newer-schema file must refuse, not proceed"
    );
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("newer CALM version"),
        "error should explain the version mismatch, got: {msg}"
    );

    // The refusal must not have touched pre-existing data.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'future::sym'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "refusal must not delete or modify existing data");

    let _ = std::fs::remove_dir_all(&dir);
}
