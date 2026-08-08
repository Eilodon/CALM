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

/// Registered migrations, in ascending `from` order. Empty today:
/// `STATE_DB_SCHEMA_VERSION` is still 1 (CCK-07's v1->v2 bump is the first
/// real entry), and unlike `index.db`, state.db has no v0->v1 ALTER step to
/// express here -- `init_state_db`'s idempotent `CREATE TABLE IF NOT
/// EXISTS` DDL already brings a fresh or unstamped (`user_version == 0`)
/// file to v1 shape on its own. This registry exists so CCK-07 only has to
/// add an entry, not build the executor.
pub const STATE_MIGRATIONS: &[StateMigration] = &[];

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
    run_migrations_from(conn, STATE_MIGRATIONS, super::schema::STATE_DB_SCHEMA_VERSION)
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
    let mut current = if on_disk == 0 { BASELINE_VERSION } else { on_disk };

    while current < target {
        let step = migrations.iter().find(|m| m.from == current).unwrap_or_else(|| {
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
        conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn unstamped_zero_version_db_becomes_target_with_no_migrations_registered() {
        let conn = fresh_conn();
        assert_eq!(user_version(&conn), 0, "fresh in-memory DB starts unstamped");
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
        conn.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)").unwrap();

        fn add_marker_row(conn: &Connection) -> rusqlite::Result<()> {
            conn.execute("INSERT INTO probe (id) VALUES (1)", [])?;
            Ok(())
        }
        let migrations = [StateMigration { from: 1, to: 2, name: "add_marker_row", apply: add_marker_row }];

        run_migrations_from(&conn, &migrations, 2).unwrap();
        assert_eq!(user_version(&conn), 2);
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM probe", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn forced_failure_in_apply_leaves_user_version_unchanged_and_rolls_back_ddl() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)").unwrap();

        fn insert_then_fail(conn: &Connection) -> rusqlite::Result<()> {
            conn.execute("INSERT INTO probe (id) VALUES (1)", [])?;
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some("simulated forced failure".to_string()),
            ))
        }
        let migrations = [StateMigration { from: 1, to: 2, name: "insert_then_fail", apply: insert_then_fail }];

        let err = run_migrations_from(&conn, &migrations, 2);
        assert!(err.is_err(), "forced apply failure must propagate as Err");
        assert_eq!(user_version(&conn), 1, "user_version must stay at its pre-step value");
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM probe", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "the insert inside the failed step must be rolled back too");
    }

    #[test]
    fn restart_after_a_completed_migration_is_idempotent() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)").unwrap();

        fn add_marker_row(conn: &Connection) -> rusqlite::Result<()> {
            conn.execute("INSERT OR IGNORE INTO probe (id) VALUES (1)", [])?;
            Ok(())
        }
        let migrations = [StateMigration { from: 1, to: 2, name: "add_marker_row", apply: add_marker_row }];

        run_migrations_from(&conn, &migrations, 2).unwrap();
        // Simulates a process restart calling the exact same entry point
        // again against an already-migrated file.
        run_migrations_from(&conn, &migrations, 2).unwrap();
        assert_eq!(user_version(&conn), 2);
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM probe", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "re-running past-target migrations must not re-apply them");
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
}
