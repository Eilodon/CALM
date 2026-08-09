//! Persistence for `ChangeIntent` -- CCK-07
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! Reads/writes `change_intents`/`change_intent_targets`
//! (`db::state_migrations`'s v1->v2 step). The *observed* half
//! (`change::classify::ObservedChangeKind`) is never persisted here --
//! see `change_intents`'s own doc comment in `STATE_SCHEMA_SQL` for why.

use rusqlite::{Connection, OptionalExtension, params};

use crate::change::classify::{ChangeIntentKind, ChangeKind};
use crate::change::intent::{ChangeIntent, ChangeIntentTarget};

/// Inserts `intent` and every one of its `targets` in that order --
/// `change_intent_targets.intent_id` is a foreign key, so target rows
/// would violate it if inserted first. Not wrapped in an explicit
/// transaction here: callers that need atomicity across this insert and
/// something else (e.g. also persisting the `EvidenceSnapshot` it
/// references) are expected to wrap both in their own `conn.transaction()`
/// -- see `authority::snapshot::persist` for the sibling call this is
/// meant to be paired with.
pub fn insert_change_intent(conn: &Connection, intent: &ChangeIntent) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO change_intents (intent_id, kind, reason, snapshot_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            intent.intent_id,
            intent.kind.0.as_str(),
            intent.reason,
            intent.snapshot_id,
            intent.created_at
        ],
    )?;
    for target in &intent.targets {
        conn.execute(
            "INSERT INTO change_intent_targets (intent_id, path, qualified_name) VALUES (?1, ?2, ?3)",
            params![intent.intent_id, target.path, target.qualified_name],
        )?;
    }
    Ok(())
}

/// `Ok(None)` when no row matches -- not persisted-but-empty vs.
/// never-existed distinction here, since `change_intents` rows are never
/// deleted except transitively via a target's `ON DELETE CASCADE` (which
/// only removes targets, not the intent itself).
pub fn get_change_intent(
    conn: &Connection,
    intent_id: &str,
) -> rusqlite::Result<Option<ChangeIntent>> {
    let row: Option<(String, String, String, String, f64)> = conn
        .query_row(
            "SELECT intent_id, kind, reason, snapshot_id, created_at \
             FROM change_intents WHERE intent_id = ?1",
            params![intent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((intent_id, kind_str, reason, snapshot_id, created_at)) = row else {
        return Ok(None);
    };
    let kind = ChangeKind::parse(&kind_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("change_intents.kind {kind_str:?} is not a known ChangeKind").into(),
        )
    })?;

    let mut stmt = conn.prepare(
        "SELECT path, qualified_name FROM change_intent_targets WHERE intent_id = ?1 ORDER BY id",
    )?;
    let targets = stmt
        .query_map(params![intent_id], |r| {
            Ok(ChangeIntentTarget {
                path: r.get(0)?,
                qualified_name: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(Some(ChangeIntent {
        intent_id,
        kind: ChangeIntentKind(kind),
        reason,
        snapshot_id,
        targets,
        created_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_state_db;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_state_db(&conn).unwrap();
        crate::db::state_migrations::migrate_state_db_to_current(&conn).unwrap();
        conn
    }

    fn insert_snapshot(conn: &Connection, snapshot_id: &str) {
        conn.execute(
            "INSERT INTO evidence_snapshots (snapshot_id, source_catalog_digest, graph_generation, \
             freshness_class, created_at) VALUES (?1, 'digest', 0, 'current', 0.0)",
            params![snapshot_id],
        )
        .unwrap();
    }

    #[test]
    fn round_trips_an_intent_with_no_targets() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-a");
        let intent = ChangeIntent::new(
            ChangeIntentKind(ChangeKind::Body),
            "fixing a bug",
            "SNP-a",
            vec![],
        );
        insert_change_intent(&conn, &intent).unwrap();

        let loaded = get_change_intent(&conn, &intent.intent_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, intent);
    }

    #[test]
    fn round_trips_an_intent_with_multiple_targets() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-b");
        let targets = vec![
            ChangeIntentTarget {
                path: "a.rs".to_string(),
                qualified_name: Some("a.rs::f".to_string()),
            },
            ChangeIntentTarget {
                path: "b.rs".to_string(),
                qualified_name: None,
            },
        ];
        let intent = ChangeIntent::new(
            ChangeIntentKind(ChangeKind::Signature),
            "widen an API",
            "SNP-b",
            targets,
        );
        insert_change_intent(&conn, &intent).unwrap();

        let loaded = get_change_intent(&conn, &intent.intent_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.targets, intent.targets);
        assert_eq!(loaded.kind, ChangeIntentKind(ChangeKind::Signature));
    }

    #[test]
    fn unknown_intent_id_returns_none_not_an_error() {
        let conn = conn();
        assert_eq!(
            get_change_intent(&conn, "INT-does-not-exist").unwrap(),
            None
        );
    }

    #[test]
    fn deleting_the_intent_cascades_to_its_targets() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-c");
        let targets = vec![ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: None,
        }];
        let intent =
            ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-c", targets);
        insert_change_intent(&conn, &intent).unwrap();

        conn.execute(
            "DELETE FROM change_intents WHERE intent_id = ?1",
            params![intent.intent_id],
        )
        .unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM change_intent_targets WHERE intent_id = ?1",
                params![intent.intent_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            remaining, 0,
            "ON DELETE CASCADE should have removed the target rows too"
        );
    }

    #[test]
    fn insert_rejects_an_intent_whose_snapshot_id_does_not_exist() {
        let conn = conn();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        let intent = ChangeIntent::new(
            ChangeIntentKind(ChangeKind::Body),
            "test",
            "SNP-missing",
            vec![],
        );
        assert!(insert_change_intent(&conn, &intent).is_err());
    }
}
