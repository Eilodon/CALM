//! Forward-migration executor for `state.db` -- CCK-01
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! `index.db` has had an incremental ALTER-migration story since
//! `run_migrations`/`migrate_add_column` (`db/schema.rs`); `state.db` never
//! did -- `b677a9e` added `STATE_DB_SCHEMA_VERSION` + a downgrade guard, but
//! nothing to carry an old file *forward*. This module is that executor,
//! deliberately mirroring `migrate_add_column`'s registered-step shape
//! rather than inventing a new pattern.
//!
//! Contract per step: `BEGIN IMMEDIATE` -> apply N->N+1 -> stamp `PRAGMA
//! user_version=N+1` -> `COMMIT`, all in the one transaction. `PRAGMA
//! user_version` is transactional in SQLite (it's part of the database file
//! header, written through the same journal/WAL as every other page), so a
//! crash or a forced failure inside `apply` rolls the stamp back together
//! with the DDL -- the file is left at exactly its pre-step version, never
//! partially bumped. That is what makes "stamp only after success" cheap
//! and correct here rather than needing a separate reconciliation pass.

use rusqlite::Connection;

/// One forward step in state.db's schema evolution. `apply` must perform
/// only the schema change for `from -> to` -- it must NOT touch `PRAGMA
/// user_version` itself; the executor stamps that after `apply` returns
/// `Ok`, inside the same transaction.
pub struct StateMigration {
    pub from: i64,
    pub to: i64,
    pub name: &'static str,
    pub apply: fn(&Connection) -> rusqlite::Result<()>,
}

/// Registered migrations, in ascending `from` order.
pub const STATE_MIGRATIONS: &[StateMigration] = &[
    StateMigration {
        from: 1,
        to: 2,
        name: "v2_evidence_snapshots_and_change_intents",
        apply: v1_to_v2_evidence_snapshots_and_change_intents,
    },
    StateMigration {
        from: 2,
        to: 3,
        name: "v3_review_authorities",
        apply: v2_to_v3_review_authorities,
    },
];

/// v1->v2 (CCK-07): adds `evidence_snapshots`, `change_intents`,
/// `change_intent_targets`. Unlike `index.db`'s ALTER-style steps, state.db
/// has no incremental-DDL story of its own (see `init_state_db`'s doc
/// comment) -- `STATE_SCHEMA_SQL`'s `CREATE TABLE IF NOT EXISTS` statements
/// already create these tables on every startup, fresh or not, and
/// `init_state_db_versioned` always runs `init_state_db` *before* this
/// executor. So by the time this `apply` runs, the tables already exist;
/// its only real job is the postcondition check CCK-01's contract asks
/// for -- confirming that assumption instead of silently trusting it, so a
/// future `STATE_SCHEMA_SQL` edit that accidentally drops one of these
/// `CREATE TABLE` statements fails loudly here instead of only surfacing
/// as a confusing "no such table" much later, at first real use.
fn v1_to_v2_evidence_snapshots_and_change_intents(conn: &Connection) -> rusqlite::Result<()> {
    for table in [
        "evidence_snapshots",
        "change_intents",
        "change_intent_targets",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!(
                    "state.db v1->v2 migration postcondition failed: table {table:?} does not \
                     exist -- STATE_SCHEMA_SQL should have created it before this step ran"
                )),
            ));
        }
    }
    Ok(())
}

/// v2->v3 (CCK-09, #65): adds `review_authorities`, `review_authority_targets`,
/// `review_authority_evidence` (same new-table-only, postcondition-check-only
/// story as v1->v2 -- see that step's doc comment) PLUS a real ALTER TABLE:
/// `edit_transactions.authority_id` is a new column on an EXISTING table, so
/// unlike a brand new table, `STATE_SCHEMA_SQL`'s idempotent `CREATE TABLE IF
/// NOT EXISTS edit_transactions (...)` does NOT retroactively add it to a
/// file that already has that table from a v1/v2 install. This is CCK-01's
/// executor's first real ALTER-style step -- mirrors index.db's own
/// `migrate_add_column` (`db/schema.rs`) idempotency check (`PRAGMA
/// table_info`) rather than reaching across modules to reuse that private
/// helper for one column.
fn v2_to_v3_review_authorities(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(edit_transactions)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    if !existing_columns.iter().any(|c| c == "authority_id") {
        conn.execute_batch("ALTER TABLE edit_transactions ADD COLUMN authority_id TEXT;")?;
    }

    for table in [
        "review_authorities",
        "review_authority_targets",
        "review_authority_evidence",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!(
                    "state.db v2->v3 migration postcondition failed: table {table:?} does not \
                     exist -- STATE_SCHEMA_SQL should have created it before this step ran"
                )),
            ));
        }
    }
    Ok(())
}

/// A DB stamped 0 (created before `b677a9e`'s downgrade guard existed, or
/// never opened by a versioned entry point) and one stamped 1 (the only
/// version that has ever shipped) both mean "v1 baseline" -- see the
/// blueprint's §1 additional-accuracy-notes for why 0 and 1 must be treated
/// identically here rather than 0 being a distinct "older" version.
const BASELINE_VERSION: i64 = 1;

/// Runs every registered migration from `conn`'s current `PRAGMA
/// user_version` (normalizing 0 to [`BASELINE_VERSION`] first) up to
/// [`schema::STATE_DB_SCHEMA_VERSION`](super::schema::STATE_DB_SCHEMA_VERSION),
/// then stamps that target explicitly. Called from `init_state_db_versioned`
/// between the downgrade-guard refusal and `init_state_db`'s idempotent DDL
/// having already run -- `apply` fns may assume baseline tables exist.
pub fn migrate_state_db_to_current(conn: &Connection) -> rusqlite::Result<()> {
    run_migrations_from(
        conn,
        STATE_MIGRATIONS,
        super::schema::STATE_DB_SCHEMA_VERSION,
    )
}

/// `pub(crate)` so integration tests can drive a synthetic migration list
/// (including a deliberately-failing step) without needing a real v2
/// migration to exist yet -- see `tests/state_schema_migrations.rs`.
pub(crate) fn run_migrations_from(
    conn: &Connection,
    migrations: &[StateMigration],
    target: i64,
) -> rusqlite::Result<()> {
    let on_disk: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut current = if on_disk == 0 {
        BASELINE_VERSION
    } else {
        on_disk
    };

    while current < target {
        let step = migrations
            .iter()
            .find(|m| m.from == current)
            .unwrap_or_else(|| {
                panic!(
                    "state.db migration executor: no registered step from version {current} \
                 toward target {target} -- STATE_MIGRATIONS is missing an entry"
                )
            });
        debug_assert_eq!(
            step.to,
            step.from + 1,
            "state.db migrations must be consecutive single-version steps (got {}: {} -> {})",
            step.name,
            step.from,
            step.to
        );

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let step_result = (|| -> rusqlite::Result<()> {
            (step.apply)(conn)?;
            conn.pragma_update(None, "user_version", step.to)
        })();
        match step_result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                current = step.to;
            }
            Err(e) => {
                // Undoes both the DDL and (if it was reached) the
                // user_version write together -- the file is left exactly
                // at its pre-step version, never partially migrated.
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
    }

    // Always stamp explicitly, even when zero migrations ran -- a fresh or
    // `user_version == 0` file must still come out of this function stamped
    // to `target`, not left at its raw on-disk value, or the downgrade
    // guard has nothing to check next time.
    conn.pragma_update(None, "user_version", target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_state_db;

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_state_db(&conn).unwrap();
        conn
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn unstamped_zero_version_db_becomes_target_with_no_migrations_registered() {
        let conn = fresh_conn();
        assert_eq!(
            user_version(&conn),
            0,
            "fresh in-memory DB starts unstamped"
        );
        run_migrations_from(&conn, &[], 1).unwrap();
        assert_eq!(user_version(&conn), 1);
    }

    #[test]
    fn already_stamped_at_target_is_a_no_op() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        run_migrations_from(&conn, &[], 1).unwrap();
        assert_eq!(user_version(&conn), 1);
    }

    #[test]
    fn baseline_one_and_unstamped_zero_reach_the_same_target() {
        let a = fresh_conn(); // user_version left at 0
        let b = fresh_conn();
        b.pragma_update(None, "user_version", 1).unwrap();

        run_migrations_from(&a, &[], 1).unwrap();
        run_migrations_from(&b, &[], 1).unwrap();
        assert_eq!(user_version(&a), user_version(&b));
    }

    #[test]
    fn successful_step_applies_and_stamps_atomically() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)")
            .unwrap();

        fn add_marker_row(conn: &Connection) -> rusqlite::Result<()> {
            conn.execute("INSERT INTO probe (id) VALUES (1)", [])?;
            Ok(())
        }
        let migrations = [StateMigration {
            from: 1,
            to: 2,
            name: "add_marker_row",
            apply: add_marker_row,
        }];

        run_migrations_from(&conn, &migrations, 2).unwrap();
        assert_eq!(user_version(&conn), 2);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM probe", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn forced_failure_in_apply_leaves_user_version_unchanged_and_rolls_back_ddl() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)")
            .unwrap();

        fn insert_then_fail(conn: &Connection) -> rusqlite::Result<()> {
            conn.execute("INSERT INTO probe (id) VALUES (1)", [])?;
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some("simulated forced failure".to_string()),
            ))
        }
        let migrations = [StateMigration {
            from: 1,
            to: 2,
            name: "insert_then_fail",
            apply: insert_then_fail,
        }];

        let err = run_migrations_from(&conn, &migrations, 2);
        assert!(err.is_err(), "forced apply failure must propagate as Err");
        assert_eq!(
            user_version(&conn),
            1,
            "user_version must stay at its pre-step value"
        );
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM probe", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rows, 0,
            "the insert inside the failed step must be rolled back too"
        );
    }

    #[test]
    fn restart_after_a_completed_migration_is_idempotent() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)")
            .unwrap();

        fn add_marker_row(conn: &Connection) -> rusqlite::Result<()> {
            conn.execute("INSERT OR IGNORE INTO probe (id) VALUES (1)", [])?;
            Ok(())
        }
        let migrations = [StateMigration {
            from: 1,
            to: 2,
            name: "add_marker_row",
            apply: add_marker_row,
        }];

        run_migrations_from(&conn, &migrations, 2).unwrap();
        // Simulates a process restart calling the exact same entry point
        // again against an already-migrated file.
        run_migrations_from(&conn, &migrations, 2).unwrap();
        assert_eq!(user_version(&conn), 2);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM probe", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rows, 1,
            "re-running past-target migrations must not re-apply them"
        );
    }

    #[test]
    #[should_panic(expected = "no registered step from version 1")]
    fn a_gap_in_the_registered_migration_chain_panics_loudly_instead_of_silently_stalling() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        // target 3 with no `from: 1` entry registered -- must not silently
        // return Ok(()) while leaving the file two versions behind.
        let migrations: [StateMigration; 0] = [];
        let _ = run_migrations_from(&conn, &migrations, 3);
    }

    #[test]
    fn registered_v1_to_v2_migration_creates_the_new_tables_and_stamps_version() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        for table in [
            "evidence_snapshots",
            "change_intents",
            "change_intent_targets",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(exists, "{table} should exist after migrating to current");
        }
    }

    #[test]
    fn unstamped_zero_version_db_reaches_current_via_the_real_registered_migration() {
        let conn = fresh_conn();
        assert_eq!(user_version(&conn), 0);
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
    }

    #[test]
    fn registered_v2_to_v3_migration_creates_new_tables_and_adds_authority_id_column() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 2).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );

        for table in [
            "review_authorities",
            "review_authority_targets",
            "review_authority_evidence",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(exists, "{table} should exist after migrating to current");
        }
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(edit_transactions)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            columns.iter().any(|c| c == "authority_id"),
            "edit_transactions.authority_id should exist"
        );
    }

    #[test]
    fn v2_to_v3_alter_table_preserves_existing_edit_transactions_rows() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO edit_transactions \
             (tx_id, project_id, path, base_digest, proposed_digest, state, created_at, updated_at) \
             VALUES ('TXN-1', 'proj', 'a.rs', 'base', 'proposed', 'PREPARED', 0.0, 0.0)",
            [],
        )
        .unwrap();

        migrate_state_db_to_current(&conn).unwrap();

        let (path, authority_id): (String, Option<String>) = conn
            .query_row(
                "SELECT path, authority_id FROM edit_transactions WHERE tx_id = 'TXN-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            path, "a.rs",
            "pre-existing row must survive the ALTER untouched"
        );
        assert_eq!(
            authority_id, None,
            "a pre-existing row gets NULL for the new column, not an error"
        );
    }

    #[test]
    fn v2_to_v3_alter_table_is_idempotent_when_the_column_already_exists() {
        // Simulates a fresh v0 install jumping straight to v3: init_state_db's
        // own CREATE TABLE already includes authority_id, so the v2->v3 step's
        // ALTER TABLE must detect that and skip, not fail with "duplicate
        // column name".
        let conn = fresh_conn();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
    }
}
