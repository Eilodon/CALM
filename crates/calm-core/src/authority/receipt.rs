//! `ApprovalReceipt` -- WS3 (audit follow-up on the master change-control
//! blueprint). `ReviewAuthority.required_approver_class` (CCK-26) is signed
//! and persisted, but names WHO must approve a change without persisting
//! any durable evidence that a real approval actually happened -- a
//! Human-tier authority's `required_approver_class` field is just as
//! present whether or not an actual human ever looked at it. This module
//! is the durable record that closes that gap: one row per approval
//! event, written by the two places an approval decision is actually
//! made --
//! `review_change` (self-attestation, `mechanism = "self_attested"`, at
//! mint time, for `SelfReviewed`) and the edit gate's real MRTR/legacy
//! elicitation round-trip (`mechanism = "elicitation"`, at spend time, for
//! `Human`).
//!
//! Deliberately NOT bound into `ReviewAuthority`'s own signed payload:
//! `review_change` writes its receipt before the authority's approval
//! mechanism for a Human-tier change can even run (that only happens
//! later, at spend time, via the edit gate) -- there is no receipt to sign
//! into a mint-time payload for that tier. Linking receipts to an
//! authority/transaction via `authority_id`/`tx_id` columns instead keeps
//! the two concerns (what was cryptographically authorized vs. what was
//! actually approved by whom) independently auditable without forcing an
//! artificial ordering between them.

use rusqlite::{Connection, params};

/// Everything a caller must supply to persist one approval event. Every
/// field mirrors a column on `approval_receipts` (`db/schema.rs`) --
/// `change_id`/`authority_id`/`tx_id` are optional because the two call
/// sites don't all have all three at receipt-write time (mint has no
/// `tx_id` yet; a pure legacy elicitation approval with no `ReviewAuthority`
/// at all has no `authority_id`).
pub struct ApprovalReceipt<'a> {
    pub change_id: Option<&'a str>,
    pub authority_id: Option<&'a str>,
    /// What was approved, content-addressed -- typically a
    /// `policy_decision_digest` (mint time, before any diff exists) or a
    /// `proposed_digest` (spend time, over the actual before/after
    /// content).
    pub subject_digest: &'a str,
    /// Best available identity for who/what approved this -- CALM has no
    /// user-login system, so this is the same `"session:<id>"` principal
    /// identity `ReviewAuthority::principal` already uses elsewhere, not a
    /// verified human identity.
    pub approved_by: &'a str,
    /// `"self_attested"` or `"elicitation"` -- see this module's own doc
    /// comment for which call site writes which.
    pub mechanism: &'a str,
    pub tx_id: Option<&'a str>,
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn new_receipt_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("RCPT-{nanos:016x}-{counter:08x}-{}", std::process::id())
}

/// Inserts one durable `approval_receipts` row and returns its
/// `receipt_id`. `decision` is always `"approved"` -- both current call
/// sites only ever call this on a successful approval path; a future
/// rejection-receipt caller would need its own explicit `decision` value,
/// not reuse this constructor with a lie in it.
pub fn insert_approval_receipt(
    conn: &Connection,
    receipt: &ApprovalReceipt,
) -> rusqlite::Result<String> {
    let receipt_id = new_receipt_id();
    conn.execute(
        "INSERT INTO approval_receipts \
         (receipt_id, change_id, authority_id, subject_digest, approved_by, mechanism, \
          decision, approved_at, tx_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'approved', ?7, ?8)",
        params![
            receipt_id,
            receipt.change_id,
            receipt.authority_id,
            receipt.subject_digest,
            receipt.approved_by,
            receipt.mechanism,
            now_epoch_secs(),
            receipt.tx_id,
        ],
    )?;
    Ok(receipt_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_state_db;
    use crate::db::state_migrations::migrate_state_db_to_current;

    fn state_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_state_db(&conn).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        conn
    }

    /// `approval_receipts.change_id` is a real FK into `change_intents` --
    /// seeds the row every fixture below that exercises it needs, same
    /// spirit as `authority::review`'s own `seed_intent_and_snapshot`.
    fn seed_intent(conn: &Connection, intent_id: &str) {
        conn.execute(
            "INSERT INTO evidence_snapshots \
             (snapshot_id, source_catalog_digest, graph_generation, freshness_class, created_at) \
             VALUES ('SNP-1', 'digest-1', 5, 'current', 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO change_intents (intent_id, kind, reason, snapshot_id, created_at) \
             VALUES (?1, 'body', 'test fixture', 'SNP-1', 0.0)",
            params![intent_id],
        )
        .unwrap();
    }

    #[test]
    fn insert_approval_receipt_round_trips_every_field() {
        let conn = state_conn();
        seed_intent(&conn, "INT-1");
        let receipt_id = insert_approval_receipt(
            &conn,
            &ApprovalReceipt {
                change_id: Some("INT-1"),
                authority_id: None,
                subject_digest: "policy-decision-1",
                approved_by: "session:abc",
                mechanism: "self_attested",
                tx_id: None,
            },
        )
        .unwrap();

        let (change_id, authority_id, subject_digest, approved_by, mechanism, decision, tx_id): (
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT change_id, authority_id, subject_digest, approved_by, mechanism, \
                 decision, tx_id FROM approval_receipts WHERE receipt_id = ?1",
                params![receipt_id],
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
            .unwrap();
        assert_eq!(change_id.as_deref(), Some("INT-1"));
        assert_eq!(authority_id, None);
        assert_eq!(subject_digest, "policy-decision-1");
        assert_eq!(approved_by, "session:abc");
        assert_eq!(mechanism, "self_attested");
        assert_eq!(decision, "approved");
        assert_eq!(tx_id, None);
    }

    #[test]
    fn insert_approval_receipt_allows_change_id_and_authority_id_to_be_absent() {
        let conn = state_conn();
        // A pure legacy elicitation approval with no ReviewAuthority
        // involved at all -- both nullable columns must accept None.
        let receipt_id = insert_approval_receipt(
            &conn,
            &ApprovalReceipt {
                change_id: None,
                authority_id: None,
                subject_digest: "proposed-digest-1",
                approved_by: "session:abc",
                mechanism: "elicitation",
                tx_id: None,
            },
        )
        .unwrap();
        let (change_id, authority_id): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT change_id, authority_id FROM approval_receipts WHERE receipt_id = ?1",
                params![receipt_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(change_id, None);
        assert_eq!(authority_id, None);
    }

    #[test]
    fn each_receipt_gets_a_distinct_id() {
        let conn = state_conn();
        let one = insert_approval_receipt(
            &conn,
            &ApprovalReceipt {
                change_id: None,
                authority_id: None,
                subject_digest: "d1",
                approved_by: "session:abc",
                mechanism: "self_attested",
                tx_id: None,
            },
        )
        .unwrap();
        let two = insert_approval_receipt(
            &conn,
            &ApprovalReceipt {
                change_id: None,
                authority_id: None,
                subject_digest: "d2",
                approved_by: "session:abc",
                mechanism: "self_attested",
                tx_id: None,
            },
        )
        .unwrap();
        assert_ne!(one, two);
    }
}
