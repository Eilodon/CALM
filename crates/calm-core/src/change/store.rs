//! Persistence for `ChangeIntent` -- CCK-07
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! Reads/writes `change_intents`/`change_intent_targets`
//! (`db::state_migrations`'s v1->v2 step). The *observed* half
//! (`change::classify::ObservedChangeKind`) is never persisted here --
//! see `change_intents`'s own doc comment in `STATE_SCHEMA_SQL` for why.

use rusqlite::{Connection, OptionalExtension, params};

use crate::change::classify::{ChangeIntentKind, ChangeKind};
use crate::change::intent::{ChangeIntent, ChangeIntentTarget, IntentStatus};

/// Raw row shape for `change_intents`: (intent_id, kind, reason,
/// snapshot_id, created_at, status, superseded_by_intent_id).
type ChangeIntentRow = (String, String, String, String, f64, String, Option<String>);

/// Inserts `intent` and every one of its `targets` in that order --
/// `change_intent_targets.intent_id` is a foreign key, so target rows
/// would violate it if inserted first. Not wrapped in an explicit
/// transaction here: callers that need atomicity across this insert and
/// something else (e.g. also persisting the `EvidenceSnapshot` it
/// references) are expected to wrap both in their own `conn.transaction()`
/// -- see `authority::snapshot::persist` for the sibling call this is
/// meant to be paired with.
///
/// CCK-11 (audit follow-up): `idempotency_key` is `None` for the pre-
/// existing CCK-10 compat caller (`mint_review_authority_for_edit_context`,
/// a single-symbol review with no idempotency contract of its own) and
/// `Some` for `plan_change` (calm-server/tools/change.rs), whose repeated-
/// call idempotency this column backs -- see `change_intents`'s partial
/// unique index in `STATE_SCHEMA_SQL`/`state_migrations.rs`'s v1->v2 step.
pub fn insert_change_intent(
    conn: &Connection,
    intent: &ChangeIntent,
    idempotency_key: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO change_intents \
         (intent_id, kind, reason, snapshot_id, created_at, idempotency_key, status, \
          superseded_by_intent_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            intent.intent_id,
            intent.kind.0.as_str(),
            intent.reason,
            intent.snapshot_id,
            intent.created_at,
            idempotency_key,
            intent.status.as_str(),
            intent.superseded_by_intent_id,
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

/// `Ok(None)` when no `change_intents` row has this exact `idempotency_key`
/// -- the fast path `plan_change` uses to make repeated calls with the same
/// declared kind/targets return the same `change_id` instead of minting a
/// fresh one every time (see `insert_change_intent`'s own doc comment for
/// the column this backs).
pub fn find_change_intent_by_idempotency_key(
    conn: &Connection,
    idempotency_key: &str,
) -> rusqlite::Result<Option<ChangeIntent>> {
    let intent_id: Option<String> = conn
        .query_row(
            "SELECT intent_id FROM change_intents WHERE idempotency_key = ?1",
            params![idempotency_key],
            |r| r.get(0),
        )
        .optional()?;
    match intent_id {
        Some(id) => get_change_intent(conn, &id),
        None => Ok(None),
    }
}

/// `Ok(None)` when no row matches -- not persisted-but-empty vs.
/// never-existed distinction here, since `change_intents` rows are never
/// deleted except transitively via a target's `ON DELETE CASCADE` (which
/// only removes targets, not the intent itself).
pub fn get_change_intent(
    conn: &Connection,
    intent_id: &str,
) -> rusqlite::Result<Option<ChangeIntent>> {
    let row: Option<ChangeIntentRow> = conn
        .query_row(
            "SELECT intent_id, kind, reason, snapshot_id, created_at, status, \
                    superseded_by_intent_id \
             FROM change_intents WHERE intent_id = ?1",
            params![intent_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        intent_id,
        kind_str,
        reason,
        snapshot_id,
        created_at,
        status_str,
        superseded_by_intent_id,
    )) = row
    else {
        return Ok(None);
    };
    let kind = ChangeKind::parse(&kind_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("change_intents.kind {kind_str:?} is not a known ChangeKind").into(),
        )
    })?;
    let status = IntentStatus::parse(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("change_intents.status {status_str:?} is not a known IntentStatus").into(),
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
        status,
        superseded_by_intent_id,
    }))
}

/// CCK-27 (audit follow-up): marks `old_intent_id` `Superseded` by a freshly
/// declared replacement and inserts that replacement, in the specific
/// 3-statement order both of `change_intents`' own constraints require:
///   1. clear `old_intent_id`'s `idempotency_key` -- frees it under the
///      partial unique index (`WHERE idempotency_key IS NOT NULL`) *before*
///      `new_intent` tries to claim the same key, with no FK involved yet.
///   2. insert `new_intent` under `new_idempotency_key` (now unblocked).
///   3. point `old_intent_id` at `new_intent.intent_id` -- only valid now
///      that row exists, since `superseded_by_intent_id` is a real FK to
///      `change_intents(intent_id)`.
///
/// Steps 1 and 3 touch the same row but can't be merged into one UPDATE:
/// merging would set the FK column before its target row exists. Callers
/// are expected to run this inside their own transaction (same pattern as
/// `insert_change_intent`'s own doc comment) -- `plan_change` is the one
/// production caller, invoked in place of a bare `insert_change_intent`
/// whenever a repeated call detects the intent's snapshot has drifted.
pub fn supersede_change_intent(
    conn: &Connection,
    old_intent_id: &str,
    new_intent: &ChangeIntent,
    new_idempotency_key: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE change_intents SET idempotency_key = NULL WHERE intent_id = ?1",
        params![old_intent_id],
    )?;
    insert_change_intent(conn, new_intent, new_idempotency_key)?;
    conn.execute(
        "UPDATE change_intents SET status = 'superseded', superseded_by_intent_id = ?2 \
         WHERE intent_id = ?1",
        params![old_intent_id, new_intent.intent_id],
    )?;
    Ok(())
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
        insert_change_intent(&conn, &intent, None).unwrap();

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
        insert_change_intent(&conn, &intent, None).unwrap();

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
    fn find_by_idempotency_key_returns_none_when_unset() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-d");
        let intent = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-d", vec![]);
        insert_change_intent(&conn, &intent, None).unwrap();
        assert_eq!(
            find_change_intent_by_idempotency_key(&conn, "some-key").unwrap(),
            None
        );
    }

    #[test]
    fn find_by_idempotency_key_round_trips_and_rejects_a_duplicate_key() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-e");
        let intent = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-e", vec![]);
        insert_change_intent(&conn, &intent, Some("plan-key-1")).unwrap();

        let found = find_change_intent_by_idempotency_key(&conn, "plan-key-1")
            .unwrap()
            .unwrap();
        assert_eq!(found, intent);

        // A second intent must not be able to reuse the same idempotency
        // key -- that would defeat the whole point of plan_change's
        // repeated-call dedup (two different intents both claiming to be
        // "the" answer for one key).
        let other = ChangeIntent::new(
            ChangeIntentKind(ChangeKind::Signature),
            "other",
            "SNP-e",
            vec![],
        );
        assert!(insert_change_intent(&conn, &other, Some("plan-key-1")).is_err());
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
        insert_change_intent(&conn, &intent, None).unwrap();

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
        assert!(insert_change_intent(&conn, &intent, None).is_err());
    }

    #[test]
    fn a_freshly_inserted_intent_round_trips_as_active_with_no_superseded_by() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-f");
        let intent = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-f", vec![]);
        insert_change_intent(&conn, &intent, None).unwrap();

        let loaded = get_change_intent(&conn, &intent.intent_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, IntentStatus::Active);
        assert_eq!(loaded.superseded_by_intent_id, None);
    }

    #[test]
    fn supersede_marks_the_old_intent_inserts_the_new_one_and_frees_the_idempotency_key() {
        let conn = conn();
        insert_snapshot(&conn, "SNP-g1");
        insert_snapshot(&conn, "SNP-g2");
        let old = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-g1", vec![]);
        insert_change_intent(&conn, &old, Some("plan-key-g")).unwrap();

        let new = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-g2", vec![]);
        supersede_change_intent(&conn, &old.intent_id, &new, Some("plan-key-g")).unwrap();

        let loaded_old = get_change_intent(&conn, &old.intent_id).unwrap().unwrap();
        assert_eq!(loaded_old.status, IntentStatus::Superseded);
        assert_eq!(
            loaded_old.superseded_by_intent_id,
            Some(new.intent_id.clone())
        );

        let loaded_new = get_change_intent(&conn, &new.intent_id).unwrap().unwrap();
        assert_eq!(loaded_new.status, IntentStatus::Active);

        // The key now resolves to the new intent, not the superseded one --
        // this is the whole point: a repeated plan_change call for the
        // same kind+targets now sees fresh evidence instead of stale.
        let found = find_change_intent_by_idempotency_key(&conn, "plan-key-g")
            .unwrap()
            .unwrap();
        assert_eq!(found.intent_id, new.intent_id);
    }
}
