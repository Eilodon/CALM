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
//!
//! **CCK-R1** (audit follow-up on this same blueprint): each `apply` step
//! now runs its own `CREATE TABLE IF NOT EXISTS` DDL directly, rather than
//! only checking that `STATE_SCHEMA_SQL` already created the tables --
//! `init_state_db_versioned` still runs `init_state_db`'s idempotent full
//! schema first for a fresh install, but a genuine pre-v2/pre-v3 file now
//! gets its missing tables from the migration step itself, not from a
//! same-process coincidence of call order. See each step's own doc comment.

use rusqlite::Connection;

/// Typed failure for the migration executor -- CCK-R1. Replaces a `panic!`
/// on a gap in `STATE_MIGRATIONS` with a recoverable error: a
/// missing-step gap or a failed postcondition check must never crash the
/// whole process, since both are conditions a caller (or `calm doctor`)
/// can meaningfully report and recover from.
#[derive(Debug)]
pub enum StateMigrationError {
    /// No registered step starts at `from` on the way to `target` --
    /// `STATE_MIGRATIONS` has a gap. Distinct from a plain Sqlite error
    /// because this is a programming/registration bug, not a runtime I/O
    /// failure -- callers may want to report it differently (e.g. "this
    /// binary is missing a migration", not "disk error").
    MissingMigration {
        from: i64,
        target: i64,
    },
    /// A migration step's own postcondition check failed (e.g. a table
    /// this step's own DDL should have just created is still missing).
    PostconditionFailed {
        migration: &'static str,
        detail: String,
    },
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for StateMigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMigration { from, target } => write!(
                f,
                "state.db migration executor: no registered step from version {from} \
                 toward target {target} -- STATE_MIGRATIONS is missing an entry"
            ),
            Self::PostconditionFailed { migration, detail } => write!(
                f,
                "state.db migration {migration:?} postcondition failed: {detail}"
            ),
            Self::Sqlite(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StateMigrationError {}

impl From<rusqlite::Error> for StateMigrationError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

/// Lets existing `rusqlite::Result<()>`-returning callers (e.g.
/// `init_state_db_versioned`) keep using `?` unchanged -- the typed
/// variants collapse to a `SqliteFailure` carrying the same message a
/// caller matching only on `Display` (or `.unwrap()`) already saw before
/// this type existed; a caller that wants the structured variants back
/// calls `migrate_state_db_to_current`/`run_migrations_from` directly.
impl From<StateMigrationError> for rusqlite::Error {
    fn from(e: StateMigrationError) -> Self {
        match e {
            StateMigrationError::Sqlite(inner) => inner,
            other => rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(other.to_string()),
            ),
        }
    }
}

/// One forward step in state.db's schema evolution. `apply` must perform
/// only the schema change for `from -> to` -- it must NOT touch `PRAGMA
/// user_version` itself; the executor stamps that after `apply` returns
/// `Ok`, inside the same transaction.
pub struct StateMigration {
    pub from: i64,
    pub to: i64,
    pub name: &'static str,
    pub apply: fn(&Connection) -> Result<(), StateMigrationError>,
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
    StateMigration {
        from: 3,
        to: 4,
        name: "v4_authority_consumed_by_tx_id",
        apply: v3_to_v4_authority_consumed_by_tx_id,
    },
    StateMigration {
        from: 4,
        to: 5,
        name: "v5_authority_policy_decision",
        apply: v4_to_v5_authority_policy_decision,
    },
    StateMigration {
        from: 5,
        to: 6,
        name: "v6_evidence_snapshot_provider_state",
        apply: v5_to_v6_evidence_snapshot_provider_state,
    },
    StateMigration {
        from: 6,
        to: 7,
        name: "v7_change_intent_supersede",
        apply: v6_to_v7_change_intent_supersede,
    },
    StateMigration {
        from: 7,
        to: 8,
        name: "v8_approval_receipts",
        apply: v7_to_v8_approval_receipts,
    },
    StateMigration {
        from: 8,
        to: 9,
        name: "v9_approval_receipt_signature",
        apply: v8_to_v9_approval_receipt_signature,
    },
    StateMigration {
        from: 9,
        to: 10,
        name: "v10_approval_receipt_signature_provenance",
        apply: v9_to_v10_approval_receipt_signature_provenance,
    },
    StateMigration {
        from: 10,
        to: 11,
        name: "v11_pending_reviews",
        apply: v10_to_v11_pending_reviews,
    },
    StateMigration {
        from: 11,
        to: 12,
        name: "v12_authority_risk_vector_fields",
        apply: v11_to_v12_authority_risk_vector_fields,
    },
];

/// v1->v2 (CCK-07): adds `evidence_snapshots`, `change_intents`,
/// `change_intent_targets`. CCK-R1: this step now runs the real `CREATE
/// TABLE IF NOT EXISTS` DDL itself, rather than only checking the tables
/// already exist -- on a fresh install (where `init_state_db` already ran
/// the full current `STATE_SCHEMA_SQL`) this is a harmless no-op re-run;
/// on a genuine pre-v2 file, this step is now what actually creates the
/// tables, instead of silently depending on `init_state_db` having done it
/// first. The DDL below is intentionally a literal duplicate of
/// `STATE_SCHEMA_SQL`'s copy in `schema.rs` (Rust's `concat!` can't
/// compose named `&str` consts, so sharing one literal isn't
/// straightforward) -- `registered_v1_to_v2_migration_matches_a_genuine_
/// pre_v2_database`'s test below drives this step against a hand-built
/// pre-v2-shaped database (not one seeded by `init_state_db`), which
/// proves this step is self-sufficient rather than merely a shared-literal
/// promise.
fn v1_to_v2_evidence_snapshots_and_change_intents(
    conn: &Connection,
) -> Result<(), StateMigrationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS evidence_snapshots (
            snapshot_id            TEXT PRIMARY KEY,
            source_catalog_digest  TEXT NOT NULL,
            graph_generation       INTEGER NOT NULL,
            freshness_class        TEXT NOT NULL,
            created_at             REAL NOT NULL
        );
        CREATE TABLE IF NOT EXISTS change_intents (
            intent_id        TEXT PRIMARY KEY,
            kind             TEXT NOT NULL,
            reason           TEXT NOT NULL,
            snapshot_id      TEXT NOT NULL REFERENCES evidence_snapshots(snapshot_id),
            created_at       REAL NOT NULL,
            idempotency_key  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_change_intents_snapshot ON change_intents(snapshot_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_change_intents_idempotency ON change_intents(idempotency_key) WHERE idempotency_key IS NOT NULL;
        CREATE TABLE IF NOT EXISTS change_intent_targets (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            intent_id      TEXT NOT NULL REFERENCES change_intents(intent_id) ON DELETE CASCADE,
            path           TEXT NOT NULL,
            qualified_name TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_change_intent_targets_intent ON change_intent_targets(intent_id);",
    )?;

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
            return Err(StateMigrationError::PostconditionFailed {
                migration: "v2_evidence_snapshots_and_change_intents",
                detail: format!("table {table:?} does not exist after this step's own DDL ran"),
            });
        }
    }
    Ok(())
}

/// v2->v3 (CCK-09, #65): adds `review_authorities` and
/// `review_authority_targets`, PLUS a real ALTER TABLE:
/// `edit_transactions.authority_id` is a new column on an EXISTING table, so
/// unlike a brand new table, `STATE_SCHEMA_SQL`'s idempotent `CREATE TABLE IF
/// NOT EXISTS edit_transactions (...)` does NOT retroactively add it to a
/// file that already has that table from a v1/v2 install. Mirrors index.db's
/// own `migrate_add_column` (`db/schema.rs`) idempotency check (`PRAGMA
/// table_info`) rather than reaching across modules to reuse that private
/// helper for one column. CCK-R1: like v1->v2, the new tables are now
/// created by this step's own DDL (see that step's doc comment for why),
/// not merely postcondition-checked. CCK-R6 (audit follow-up): the ALTER
/// TABLE now adds a real FK (`REFERENCES review_authorities(authority_id)
/// ON DELETE SET NULL`) instead of a plain column, `review_authority_targets`
/// gained a `UNIQUE(authority_id, path, qualified_name)` constraint, and the
/// EAV-style `review_authority_evidence` table (write-only, never read
/// outside its own tests, fully redundant with `review_authorities`' own
/// typed columns) is gone entirely -- this branch never shipped `state.db`
/// v3, so there is no upgrade path that needs it preserved.
fn v2_to_v3_review_authorities(conn: &Connection) -> Result<(), StateMigrationError> {
    // CCK-R6 (audit follow-up): the review_authorities/review_authority_targets
    // tables must exist BEFORE the ALTER TABLE below, since that statement's
    // new FK column references review_authorities(authority_id) -- run this
    // batch first so the parent table is never forward-referenced.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_authorities (
            authority_id       TEXT PRIMARY KEY,
            intent_id          TEXT NOT NULL REFERENCES change_intents(intent_id),
            snapshot_id        TEXT NOT NULL REFERENCES evidence_snapshots(snapshot_id),
            graph_generation   INTEGER NOT NULL,
            caller_set_digest  TEXT NOT NULL,
            analysis_version   TEXT NOT NULL,
            policy_digest      TEXT NOT NULL,
            principal          TEXT NOT NULL,
            target_scope_digest TEXT NOT NULL DEFAULT '',
            nonce              TEXT NOT NULL,
            expires_at         REAL NOT NULL,
            signature          TEXT NOT NULL,
            created_at         REAL NOT NULL,
            consumed_at        REAL
        );
        CREATE INDEX IF NOT EXISTS idx_review_authorities_intent ON review_authorities(intent_id);
        CREATE TABLE IF NOT EXISTS review_authority_targets (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            authority_id   TEXT NOT NULL REFERENCES review_authorities(authority_id) ON DELETE CASCADE,
            path           TEXT NOT NULL,
            qualified_name TEXT,
            UNIQUE(authority_id, path, qualified_name)
        );
        CREATE INDEX IF NOT EXISTS idx_review_authority_targets_authority ON review_authority_targets(authority_id);",
    )?;

    let mut stmt = conn.prepare("PRAGMA table_info(edit_transactions)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    if !existing_columns.iter().any(|c| c == "authority_id") {
        conn.execute_batch(
            "ALTER TABLE edit_transactions ADD COLUMN authority_id TEXT \
             REFERENCES review_authorities(authority_id) ON DELETE SET NULL;",
        )?;
    }

    for table in ["review_authorities", "review_authority_targets"] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StateMigrationError::PostconditionFailed {
                migration: "v3_review_authorities",
                detail: format!("table {table:?} does not exist after this step's own DDL ran"),
            });
        }
    }
    Ok(())
}

/// v3->v4 (CCK-25, audit follow-up): adds `review_authorities.
/// consumed_by_tx_id`, a new column on an EXISTING table (same shape as
/// v2->v3's `edit_transactions.authority_id` ALTER above) -- provenance-
/// binds a consumed authority to the exact `edit_transactions` row it
/// authorized, set atomically with `consumed_at` by
/// `authority::review::authorize_and_begin_edit` instead of the old
/// two-step (non-atomic) verify_and_consume-then-txn::begin.
fn v3_to_v4_authority_consumed_by_tx_id(conn: &Connection) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(review_authorities)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    if !existing_columns.iter().any(|c| c == "consumed_by_tx_id") {
        conn.execute_batch(
            "ALTER TABLE review_authorities ADD COLUMN consumed_by_tx_id TEXT \
             REFERENCES edit_transactions(tx_id) ON DELETE SET NULL;",
        )?;
    }

    let mut stmt = conn.prepare("PRAGMA table_info(review_authorities)")?;
    let now_has_column = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == "consumed_by_tx_id");
    if !now_has_column {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v4_authority_consumed_by_tx_id",
            detail: "review_authorities.consumed_by_tx_id does not exist after this step's \
                      own DDL ran"
                .to_string(),
        });
    }
    Ok(())
}

/// v4->v5 (CCK-26, audit follow-up): adds `review_authorities.
/// policy_decision_digest`/`risk_vector_digest`/`required_approver_class` --
/// same shape as v3->v4's ALTER above. A real `PolicyEngine::evaluate()`
/// decision (not just a policy-config digest) now backs each authority;
/// `required_approver_class` is what `review_change` gates minting on for
/// a `Human`-required change.
fn v4_to_v5_authority_policy_decision(conn: &Connection) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(review_authorities)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    let new_columns: &[(&str, &str)] = &[
        ("policy_decision_digest", "TEXT NOT NULL DEFAULT ''"),
        ("risk_vector_digest", "TEXT NOT NULL DEFAULT ''"),
        (
            "required_approver_class",
            "TEXT NOT NULL DEFAULT 'self_reviewed'",
        ),
    ];
    for (name, ddl) in new_columns {
        if !existing_columns.iter().any(|c| c == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE review_authorities ADD COLUMN {name} {ddl};"
            ))?;
        }
    }

    let mut stmt = conn.prepare("PRAGMA table_info(review_authorities)")?;
    let now_has_all = {
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        new_columns
            .iter()
            .all(|(name, _)| cols.iter().any(|c| c == name))
    };
    if !now_has_all {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v5_authority_policy_decision",
            detail: "review_authorities is missing one or more of \
                      policy_decision_digest/risk_vector_digest/required_approver_class \
                      after this step's own DDL ran"
                .to_string(),
        });
    }
    Ok(())
}

/// v5->v6 (CCK-26, same audit follow-up): adds
/// `evidence_snapshots.provider_state_digest` -- same ALTER-loop shape as
/// v4->v5. `authority::snapshot::EvidenceSnapshot::snapshot_id` now also
/// binds SCIP/LSP provider run state (`scip_overlay_state`), so a
/// proof-coverage change with no source/config/graph_generation change
/// still mints a fresh snapshot; this column is where that digest is
/// persisted alongside the other three snapshot_id inputs.
fn v5_to_v6_evidence_snapshot_provider_state(conn: &Connection) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(evidence_snapshots)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    if !existing_columns
        .iter()
        .any(|c| c == "provider_state_digest")
    {
        conn.execute_batch(
            "ALTER TABLE evidence_snapshots ADD COLUMN provider_state_digest TEXT NOT NULL DEFAULT '';",
        )?;
    }

    let mut stmt = conn.prepare("PRAGMA table_info(evidence_snapshots)")?;
    let now_has_column = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == "provider_state_digest");
    if !now_has_column {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v6_evidence_snapshot_provider_state",
            detail: "evidence_snapshots.provider_state_digest does not exist after this \
                      step's own DDL ran"
                .to_string(),
        });
    }
    Ok(())
}

/// v6->v7 (CCK-27, audit follow-up): adds `change_intents.status`/
/// `superseded_by_intent_id` -- `plan_change`'s idempotency dedup keyed
/// only on `kind`+`targets` (never `snapshot_id`) meant a repeated call
/// after source drifted still returned the SAME stale `change_id`, so a
/// human approving it via `review_change` could be looking at a
/// declared-vs-observed picture from a snapshot that no longer exists.
/// `change::store::supersede_change_intent` now marks the old intent row
/// `superseded` (and frees its `idempotency_key`) whenever `plan_change`
/// finds drift, and `review_change` refuses to mint against a superseded
/// intent -- see those symbols' own doc comments.
fn v6_to_v7_change_intent_supersede(conn: &Connection) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(change_intents)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    let new_columns: &[(&str, &str)] = &[
        ("status", "TEXT NOT NULL DEFAULT 'active'"),
        (
            "superseded_by_intent_id",
            "TEXT REFERENCES change_intents(intent_id)",
        ),
    ];
    for (name, ddl) in new_columns {
        if !existing_columns.iter().any(|c| c == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE change_intents ADD COLUMN {name} {ddl};"
            ))?;
        }
    }

    let mut stmt = conn.prepare("PRAGMA table_info(change_intents)")?;
    let now_has_all = {
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        new_columns
            .iter()
            .all(|(name, _)| cols.iter().any(|c| c == name))
    };
    if !now_has_all {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v7_change_intent_supersede",
            detail: "change_intents is missing status/superseded_by_intent_id after this \
                      step's own DDL ran"
                .to_string(),
        });
    }
    Ok(())
}

/// v7->v8 (WS3, audit follow-up): adds `approval_receipts` -- a durable
/// record that a `ReviewAuthority`'s `required_approver_class` was
/// actually satisfied (self-attestation at mint for SelfReviewed, a real
/// MRTR/legacy elicitation round-trip at spend for Human), not just a
/// signed claim with nothing behind it. New table, so (per CCK-R1's own
/// precedent) this step's own DDL creates it directly rather than only
/// postcondition-checking it.
fn v7_to_v8_approval_receipts(conn: &Connection) -> Result<(), StateMigrationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS approval_receipts (
            receipt_id     TEXT PRIMARY KEY,
            change_id      TEXT REFERENCES change_intents(intent_id),
            authority_id   TEXT REFERENCES review_authorities(authority_id) ON DELETE SET NULL,
            subject_digest TEXT NOT NULL,
            approved_by    TEXT NOT NULL,
            mechanism      TEXT NOT NULL,
            decision       TEXT NOT NULL,
            approved_at    REAL NOT NULL,
            tx_id          TEXT REFERENCES edit_transactions(tx_id) ON DELETE SET NULL
        );
        CREATE INDEX IF NOT EXISTS idx_approval_receipts_change ON approval_receipts(change_id);
        CREATE INDEX IF NOT EXISTS idx_approval_receipts_authority ON approval_receipts(authority_id);",
    )?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='approval_receipts')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v8_approval_receipts",
            detail: "approval_receipts does not exist after this step's own DDL ran".to_string(),
        });
    }
    Ok(())
}

/// v8->v9 (WS3 follow-up): adds `approval_receipts.signature` -- an
/// HMAC-SHA256 (`control.key`, domain `"approval-receipt-v1"`) over every
/// other column on the row. NOT bound into `ReviewAuthority`'s own signed
/// payload -- see `authority::receipt`'s module doc comment for why that
/// ordering is structurally impossible (a receipt is always written
/// strictly after the authority it references already exists, for both
/// approval mechanisms).
///
/// Nullable, and best-effort at both migration and insert time, matching
/// receipt-writing's existing fail-open posture (a signing hiccup must
/// never block or invalidate an already-legitimate approval) -- most
/// notably, a `:memory:` state connection genuinely has no `control.key`
/// (see `authority::key::control_key_for_conn`'s own doc comment), so
/// every existing unsigned-receipt test fixture keeps working unchanged.
///
/// Backfills every pre-existing row too, not just new ones: every column a
/// signature covers is already stored on each row, so a real `control.key`
/// (the common case for any real install) can sign them retroactively
/// rather than leaving every receipt written before this migration
/// permanently unsigned.
///
/// **CCK-30R2 correction (audit 2026-08-10):** this step's signature alone
/// does NOT prove a row was "genuinely written by `insert_approval_receipt`
/// rather than hand-inserted", contrary to what an earlier version of this
/// comment claimed for the backfill branch specifically. A signature this
/// step computes over a pre-existing row only proves the row's bytes
/// existed (and were signed) when THIS MIGRATION ran -- it cannot prove
/// the approval it describes was authentic when it happened, since an
/// attacker with raw `state.db` write access before this migration ever
/// ran could have hand-inserted the exact same row and had it backfilled
/// identically. `v9_to_v10_approval_receipt_signature_provenance` below
/// makes that distinction explicit and durable via `signature_provenance`.
fn v8_to_v9_approval_receipt_signature(conn: &Connection) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(approval_receipts)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    if !existing_columns.iter().any(|c| c == "signature") {
        conn.execute_batch("ALTER TABLE approval_receipts ADD COLUMN signature TEXT;")?;
    }

    if let Ok(Some(key)) = crate::authority::key::control_key_for_conn(conn) {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
            f64,
            Option<String>,
        )> = {
            let mut select = conn.prepare(
                "SELECT receipt_id, change_id, authority_id, subject_digest, approved_by, \
                 mechanism, decision, approved_at, tx_id FROM approval_receipts \
                 WHERE signature IS NULL",
            )?;
            select
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect()
        };
        for (
            receipt_id,
            change_id,
            authority_id,
            subject_digest,
            approved_by,
            mechanism,
            decision,
            approved_at,
            tx_id,
        ) in rows
        {
            // v9_to_v10 (which adds `signature_provenance`) always runs
            // right after this step in the same upgrade chain and
            // re-signs every row this step touches -- the exact string
            // passed here is transient, but "legacy_unverified" is the
            // semantically honest one: this branch backfills a signature
            // for a row that was never signed at its own insert time.
            let payload = crate::authority::receipt::signing_payload(
                &receipt_id,
                change_id.as_deref(),
                authority_id.as_deref(),
                &subject_digest,
                &approved_by,
                &mechanism,
                &decision,
                approved_at,
                tx_id.as_deref(),
                "legacy_unverified",
            );
            let signature = crate::authority::key::sign(
                &key,
                crate::authority::receipt::SIGNING_DOMAIN,
                &payload,
            );
            conn.execute(
                "UPDATE approval_receipts SET signature = ?1 WHERE receipt_id = ?2",
                rusqlite::params![signature, receipt_id],
            )?;
        }
    }

    let mut stmt = conn.prepare("PRAGMA table_info(approval_receipts)")?;
    let now_has_column = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == "signature");
    if !now_has_column {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v9_approval_receipt_signature",
            detail: "approval_receipts.signature does not exist after this step's own DDL ran"
                .to_string(),
        });
    }
    Ok(())
}

/// v9->v10 (CCK-30R2, audit 2026-08-10): adds
/// `approval_receipts.signature_provenance` and re-derives `signature` for
/// every row that already has one, now over the new payload shape (see
/// `authority::receipt::signing_payload`'s own doc comment). Every such row
/// is labeled `"legacy_unverified"`, never `"native"` -- by the time this
/// step runs, a row genuinely signed at insert time by
/// `insert_approval_receipt` under v9 and a row `v8_to_v9_...`'s own
/// backfill signed wholesale (which may itself have been hand-inserted
/// before THAT migration ever ran) are already byte-identical in shape:
/// both just have a non-`NULL` `signature`. There is no data left at this
/// point to tell them apart, so claiming `"native"` for either would
/// repeat the exact provenance-laundering this step exists to fix. Going
/// forward, only [`crate::authority::receipt::insert_approval_receipt`]
/// itself -- signing at the true moment of approval -- may claim
/// `"native"`.
///
/// If no `control.key` is available to re-sign under the new shape, the
/// old-shape `signature` is nulled out (along with `signature_provenance`,
/// which stays unset) rather than left in place: an un-migrated signature
/// would silently fail `verify_approval_receipt_signature` the moment a
/// key DOES become available later, misreporting an untouched row as
/// tampered. Same fail-open posture as every other best-effort signing
/// path in this module, just applied to a payload-shape migration instead
/// of a fresh insert.
fn v9_to_v10_approval_receipt_signature_provenance(
    conn: &Connection,
) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(approval_receipts)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    if !existing_columns.iter().any(|c| c == "signature_provenance") {
        conn.execute_batch("ALTER TABLE approval_receipts ADD COLUMN signature_provenance TEXT;")?;
    }

    const PROVENANCE: &str = "legacy_unverified";
    if let Ok(Some(key)) = crate::authority::key::control_key_for_conn(conn) {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
            f64,
            Option<String>,
        )> = {
            let mut select = conn.prepare(
                "SELECT receipt_id, change_id, authority_id, subject_digest, approved_by, \
                 mechanism, decision, approved_at, tx_id FROM approval_receipts \
                 WHERE signature IS NOT NULL AND signature_provenance IS NULL",
            )?;
            select
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect()
        };
        for (
            receipt_id,
            change_id,
            authority_id,
            subject_digest,
            approved_by,
            mechanism,
            decision,
            approved_at,
            tx_id,
        ) in rows
        {
            let payload = crate::authority::receipt::signing_payload(
                &receipt_id,
                change_id.as_deref(),
                authority_id.as_deref(),
                &subject_digest,
                &approved_by,
                &mechanism,
                &decision,
                approved_at,
                tx_id.as_deref(),
                PROVENANCE,
            );
            let signature = crate::authority::key::sign(
                &key,
                crate::authority::receipt::SIGNING_DOMAIN,
                &payload,
            );
            conn.execute(
                "UPDATE approval_receipts SET signature = ?1, signature_provenance = ?2 \
                 WHERE receipt_id = ?3",
                rusqlite::params![signature, PROVENANCE, receipt_id],
            )?;
        }
    } else {
        conn.execute_batch(
            "UPDATE approval_receipts SET signature = NULL, signature_provenance = NULL \
             WHERE signature IS NOT NULL AND signature_provenance IS NULL;",
        )?;
    }

    let mut stmt = conn.prepare("PRAGMA table_info(approval_receipts)")?;
    let now_has_column = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == "signature_provenance");
    if !now_has_column {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v10_approval_receipt_signature_provenance",
            detail: "approval_receipts.signature_provenance does not exist after this step's \
                      own DDL ran"
                .to_string(),
        });
    }
    Ok(())
}

/// v10->v11 (audit 2026-08-10 follow-up, "calm review"): adds
/// `pending_reviews` -- a durable, MCP-protocol-independent second channel
/// for the independent review `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`
/// requires. Verified empirically the same day: a connected MCP client can
/// complete the elicitation round-trip (no timeout, no capability error)
/// while never actually showing anything to a human -- CALM had no way to
/// detect that, and no alternative channel at all. `calm-cli review` reads
/// and writes this table directly from a real, TTY-gated terminal session,
/// independent of whichever MCP client happens to be connected. A row here
/// is not itself an authority to write anything -- `edit_lines_impl_gated`
/// still requires a fresh, matching content fingerprint AND `status =
/// 'approved'` before treating a retry as reviewed, the exact same trust
/// tier `ElicitGate::Approved` already requires (see
/// `authority::receipt::insert_approval_receipt`'s `mechanism =
/// "cli_manual_review"` value, written only on real approval).
fn v10_to_v11_pending_reviews(conn: &Connection) -> Result<(), StateMigrationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_reviews (
            review_id     TEXT PRIMARY KEY,
            tool          TEXT NOT NULL,
            path          TEXT NOT NULL,
            fingerprint   TEXT NOT NULL,
            diff_preview  TEXT NOT NULL,
            risk          TEXT,
            hub_kind      TEXT,
            reason        TEXT,
            status        TEXT NOT NULL DEFAULT 'pending',
            created_at    REAL NOT NULL,
            expires_at    REAL NOT NULL,
            decided_at    REAL,
            decided_by    TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_pending_reviews_lookup \
         ON pending_reviews(path, fingerprint, status);
        CREATE INDEX IF NOT EXISTS idx_pending_reviews_status ON pending_reviews(status);",
    )?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='pending_reviews')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v11_pending_reviews",
            detail: "pending_reviews does not exist after this step's own DDL ran".to_string(),
        });
    }
    Ok(())
}

/// v11->v12 (Wave 10, item 3): adds `review_authorities.mint_*` columns --
/// the raw `RiskVector` fields a mint actually decided against, not just
/// their digest. Before this, `RiskVector` was "compute-only... nothing
/// persisted yet" (see `policy/model.rs`'s own module doc comment) -- a
/// spend-time recomputation that legitimately differs from mint time on
/// even one field (e.g. `touches_uncovered_code`, closed for two of three
/// mint paths by Wave 10 item 1) changed `risk_vector_digest` and failed
/// `StaleRiskVector` outright, with no way to tell whether the drift made
/// the spend MORE risky than what was reviewed, or just DIFFERENT.
/// Storing the raw fields lets `verify_only` fall back to a sound,
/// per-field `spend ⊑ mint` comparison (`RiskVector::is_covered_by`) when
/// the digests don't match outright -- grounded in `policy::evaluate`'s
/// own provable monotonicity (every axis only ever RAISES the decided
/// risk level, never lowers it), so `current ⊑ minted` implies
/// `evaluate(current)` cannot be riskier than what was already approved.
/// `mint_hub_kind`/`mint_risk_rule_floor` are nullable (mirrors
/// `RiskVector`'s own `Option<...>` shape); every other new column is a
/// 0/1 bool or the empty-string default the existing digest columns
/// already use the same pattern for.
fn v11_to_v12_authority_risk_vector_fields(conn: &Connection) -> Result<(), StateMigrationError> {
    let mut stmt = conn.prepare("PRAGMA table_info(review_authorities)")?;
    let existing_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    let new_columns: &[(&str, &str)] = &[
        ("mint_caller_count_level", "TEXT NOT NULL DEFAULT ''"),
        ("mint_is_hub", "INTEGER NOT NULL DEFAULT 0"),
        ("mint_hub_kind", "TEXT"),
        ("mint_signature_changed", "INTEGER NOT NULL DEFAULT 0"),
        ("mint_uncertain_zero_caller", "INTEGER NOT NULL DEFAULT 0"),
        ("mint_risk_rule_floor", "TEXT"),
        ("mint_kind_mismatch", "INTEGER NOT NULL DEFAULT 0"),
        ("mint_touches_manifest", "INTEGER NOT NULL DEFAULT 0"),
        ("mint_touches_uncovered_code", "INTEGER NOT NULL DEFAULT 0"),
    ];
    for (name, ddl) in new_columns {
        if !existing_columns.iter().any(|c| c == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE review_authorities ADD COLUMN {name} {ddl};"
            ))?;
        }
    }

    let mut stmt = conn.prepare("PRAGMA table_info(review_authorities)")?;
    let now_has_all = {
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        new_columns
            .iter()
            .all(|(name, _)| cols.iter().any(|c| c == name))
    };
    if !now_has_all {
        return Err(StateMigrationError::PostconditionFailed {
            migration: "v12_authority_risk_vector_fields",
            detail: "review_authorities is missing one or more mint_* RiskVector columns \
                      after this step's own DDL ran"
                .to_string(),
        });
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
pub fn migrate_state_db_to_current(conn: &Connection) -> Result<(), StateMigrationError> {
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
) -> Result<(), StateMigrationError> {
    let on_disk: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut current = if on_disk == 0 {
        BASELINE_VERSION
    } else {
        on_disk
    };

    while current < target {
        let step = match migrations.iter().find(|m| m.from == current) {
            Some(s) => s,
            None => {
                return Err(StateMigrationError::MissingMigration {
                    from: current,
                    target,
                });
            }
        };
        debug_assert_eq!(
            step.to,
            step.from + 1,
            "state.db migrations must be consecutive single-version steps (got {}: {} -> {})",
            step.name,
            step.from,
            step.to
        );

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let step_result = (|| -> Result<(), StateMigrationError> {
            (step.apply)(conn)?;
            conn.pragma_update(None, "user_version", step.to)?;
            Ok(())
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

    /// A hand-built database using ONLY the tables state.db had at v1 --
    /// deliberately NOT built by calling `init_state_db` (which always
    /// creates the FULL current schema, v2/v3 tables included, defeating
    /// the point of testing an old-shaped file). CCK-R1's whole point is
    /// that migrations must be self-sufficient against a database that
    /// genuinely never had `evidence_snapshots`/`change_intents`/
    /// `review_authorities` etc, not just against a fresh install that
    /// happens to already have them.
    fn v1_shaped_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE project_memory (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                topic       TEXT NOT NULL UNIQUE,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            CREATE TABLE edit_transactions (
                tx_id                   TEXT PRIMARY KEY,
                project_id              TEXT NOT NULL,
                path                    TEXT NOT NULL,
                base_digest             TEXT NOT NULL,
                proposed_digest         TEXT NOT NULL,
                review_token_id         TEXT,
                state                   TEXT NOT NULL DEFAULT 'PREPARED',
                temp_path               TEXT,
                graph_generation_before INTEGER,
                graph_generation_after  INTEGER,
                created_at              REAL NOT NULL,
                updated_at              REAL NOT NULL,
                error_code              TEXT,
                error_detail            TEXT
            );
            CREATE TABLE tx_events (
                event_id    TEXT PRIMARY KEY,
                tx_id       TEXT NOT NULL REFERENCES edit_transactions(tx_id) ON DELETE CASCADE,
                sequence    INTEGER NOT NULL,
                from_state  TEXT NOT NULL,
                to_state    TEXT NOT NULL,
                actor       TEXT NOT NULL,
                reason      TEXT NOT NULL,
                occurred_at REAL NOT NULL,
                UNIQUE(tx_id, sequence)
            );
            CREATE TABLE audit_ledger (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                prev_hash  TEXT NOT NULL,
                event_hash TEXT NOT NULL UNIQUE,
                ts         REAL NOT NULL,
                actor      TEXT NOT NULL,
                payload    TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn
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

        fn add_marker_row(conn: &Connection) -> Result<(), StateMigrationError> {
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

        fn insert_then_fail(conn: &Connection) -> Result<(), StateMigrationError> {
            conn.execute("INSERT INTO probe (id) VALUES (1)", [])?;
            Err(StateMigrationError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some("simulated forced failure".to_string()),
            )))
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

        fn add_marker_row(conn: &Connection) -> Result<(), StateMigrationError> {
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
    fn a_gap_in_the_registered_migration_chain_returns_a_typed_error_instead_of_panicking() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 1).unwrap();
        // target 3 with no `from: 1` entry registered -- must not silently
        // return Ok(()) while leaving the file two versions behind, and
        // (CCK-R1) must not panic either -- a recoverable, typed error.
        let migrations: [StateMigration; 0] = [];
        let err = run_migrations_from(&conn, &migrations, 3);
        assert!(matches!(
            err,
            Err(StateMigrationError::MissingMigration { from: 1, target: 3 })
        ));
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

        for table in ["review_authorities", "review_authority_targets"] {
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
    fn registered_v3_to_v4_migration_adds_consumed_by_tx_id_column() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 3).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(review_authorities)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            columns.iter().any(|c| c == "consumed_by_tx_id"),
            "review_authorities.consumed_by_tx_id should exist"
        );
    }

    #[test]
    fn registered_v4_to_v5_migration_adds_policy_decision_columns() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 4).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(review_authorities)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for expected in [
            "policy_decision_digest",
            "risk_vector_digest",
            "required_approver_class",
        ] {
            assert!(
                columns.iter().any(|c| c == expected),
                "review_authorities.{expected} should exist"
            );
        }
    }

    #[test]
    fn registered_v11_to_v12_migration_adds_mint_risk_vector_columns() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 11).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(review_authorities)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for expected in [
            "mint_caller_count_level",
            "mint_is_hub",
            "mint_hub_kind",
            "mint_signature_changed",
            "mint_uncertain_zero_caller",
            "mint_risk_rule_floor",
            "mint_kind_mismatch",
            "mint_touches_manifest",
            "mint_touches_uncovered_code",
        ] {
            assert!(
                columns.iter().any(|c| c == expected),
                "review_authorities.{expected} should exist"
            );
        }
    }

    #[test]
    fn registered_v5_to_v6_migration_adds_provider_state_digest_column() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 5).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(evidence_snapshots)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            columns.iter().any(|c| c == "provider_state_digest"),
            "evidence_snapshots.provider_state_digest should exist"
        );
    }

    #[test]
    fn registered_v6_to_v7_migration_adds_change_intent_supersede_columns() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 6).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(change_intents)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for expected in ["status", "superseded_by_intent_id"] {
            assert!(
                columns.iter().any(|c| c == expected),
                "change_intents.{expected} should exist"
            );
        }
    }

    #[test]
    fn registered_v8_to_v9_migration_backfills_signatures_for_pre_existing_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(".calm").join("state.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        init_state_db(&conn).unwrap();
        conn.pragma_update(None, "user_version", 8).unwrap();

        // A row exactly as v8's own DDL would have inserted it -- no
        // `signature` column supplied, so it lands NULL, simulating a
        // receipt written before this migration ever ran.
        conn.execute(
            "INSERT INTO approval_receipts \
             (receipt_id, change_id, authority_id, subject_digest, approved_by, mechanism, \
              decision, approved_at, tx_id) \
             VALUES ('RCPT-pre-migration', NULL, NULL, 'digest-1', 'session:abc', \
                     'self_attested', 'approved', 123.0, NULL)",
            [],
        )
        .unwrap();

        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );

        let signature: Option<String> = conn
            .query_row(
                "SELECT signature FROM approval_receipts WHERE receipt_id = 'RCPT-pre-migration'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            signature
                .as_deref()
                .is_some_and(|s| s.starts_with("hmac-sha256:")),
            "pre-existing row should have been backfilled with a real signature, got {signature:?}"
        );
        assert_eq!(
            crate::authority::receipt::verify_approval_receipt_signature(
                &conn,
                "RCPT-pre-migration"
            )
            .unwrap(),
            Some(true)
        );
    }

    #[test]
    fn registered_v9_to_v10_migration_labels_every_pre_existing_signed_row_legacy_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(".calm").join("state.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        init_state_db(&conn).unwrap();
        conn.pragma_update(None, "user_version", 9).unwrap();

        // Simulates a row that was genuinely signed at insert time under
        // real v9 code (a true "native" approval, by construction) --
        // CCK-30R2's whole point is that this step can NOT tell this row
        // apart from one v8->v9 merely backfilled, so it must still
        // collapse to "legacy_unverified", never "native".
        let payload = crate::authority::receipt::signing_payload(
            "RCPT-was-native-under-v9",
            None,
            None,
            "digest-1",
            "session:abc",
            "self_attested",
            "approved",
            123.0,
            None,
            // v9 had no `signature_provenance` field at all -- passing ""
            // here reproduces exactly the payload shape v9's own
            // `insert_approval_receipt` would have signed over.
            "",
        );
        let key = crate::authority::key::control_key_for_conn(&conn)
            .unwrap()
            .unwrap();
        let signature =
            crate::authority::key::sign(&key, crate::authority::receipt::SIGNING_DOMAIN, &payload);
        conn.execute(
            "INSERT INTO approval_receipts \
             (receipt_id, change_id, authority_id, subject_digest, approved_by, mechanism, \
              decision, approved_at, tx_id, signature) \
             VALUES ('RCPT-was-native-under-v9', NULL, NULL, 'digest-1', 'session:abc', \
                     'self_attested', 'approved', 123.0, NULL, ?1)",
            rusqlite::params![signature],
        )
        .unwrap();

        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );

        assert_eq!(
            crate::authority::receipt::approval_receipt_signature_provenance(
                &conn,
                "RCPT-was-native-under-v9"
            )
            .unwrap(),
            Some("legacy_unverified".to_string()),
            "a row this step cannot prove was native must never be relabeled native"
        );
        assert_eq!(
            crate::authority::receipt::verify_approval_receipt_signature(
                &conn,
                "RCPT-was-native-under-v9"
            )
            .unwrap(),
            Some(true),
            "the re-signed row must still verify under its new (legacy_unverified) shape"
        );
    }

    #[test]
    fn registered_v10_to_v11_migration_creates_pending_reviews_table() {
        let conn = fresh_conn();
        conn.pragma_update(None, "user_version", 10).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' \
                 AND name='pending_reviews')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "pending_reviews should exist after migrating to current"
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(pending_reviews)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for expected in [
            "review_id",
            "tool",
            "path",
            "fingerprint",
            "diff_preview",
            "risk",
            "hub_kind",
            "reason",
            "status",
            "created_at",
            "expires_at",
            "decided_at",
            "decided_by",
        ] {
            assert!(
                columns.iter().any(|c| c == expected),
                "pending_reviews.{expected} should exist"
            );
        }
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

    /// CCK-R1's actual regression test: drives the real registered
    /// migrations against a database that genuinely never had v2/v3
    /// tables (`v1_shaped_conn`, NOT `init_state_db`) -- proves the
    /// migration steps create the tables themselves rather than merely
    /// verifying `init_state_db` already did, and that unrelated
    /// pre-existing data (project memory, the edit-transaction journal,
    /// the audit ledger) survives the upgrade untouched.
    #[test]
    fn registered_migrations_are_self_sufficient_against_a_genuine_pre_v2_database() {
        let conn = v1_shaped_conn();
        conn.execute(
            "INSERT INTO project_memory (topic, content, created_at, updated_at) \
             VALUES ('gotcha', 'remember this', 't0', 't0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edit_transactions \
             (tx_id, project_id, path, base_digest, proposed_digest, state, created_at, updated_at) \
             VALUES ('TXN-old', 'proj', 'a.rs', 'base', 'proposed', 'DONE', 0.0, 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audit_ledger (prev_hash, event_hash, ts, actor, payload) \
             VALUES ('genesis', 'h1', 0.0, 'test', '{}')",
            [],
        )
        .unwrap();

        migrate_state_db_to_current(&conn).unwrap();

        assert_eq!(
            user_version(&conn),
            super::super::schema::STATE_DB_SCHEMA_VERSION
        );
        for table in [
            "evidence_snapshots",
            "change_intents",
            "change_intent_targets",
            "review_authorities",
            "review_authority_targets",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                exists,
                "{table} should exist after migrating a genuine pre-v2 database"
            );
        }

        let memory_content: String = conn
            .query_row(
                "SELECT content FROM project_memory WHERE topic = 'gotcha'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(memory_content, "remember this", "memory rows must survive");

        let (tx_state, authority_id): (String, Option<String>) = conn
            .query_row(
                "SELECT state, authority_id FROM edit_transactions WHERE tx_id = 'TXN-old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(tx_state, "DONE", "edit_transactions rows must survive");
        assert_eq!(authority_id, None);

        let ledger_payload: String = conn
            .query_row(
                "SELECT payload FROM audit_ledger WHERE event_hash = 'h1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ledger_payload, "{}", "audit ledger rows must survive");
    }
}
