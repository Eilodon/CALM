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
//!
//! **WS3 follow-up:** that ordering constraint doesn't rule out ALL
//! cryptographic hardening, though -- [`insert_approval_receipt`] now signs
//! the receipt row itself (its own HMAC domain, `SIGNING_DOMAIN`, separate
//! from `review::SIGNING_DOMAIN`) so a receipt can be verified as having
//! genuinely come from this function rather than being hand-inserted (e.g.
//! by an attacker with raw `state.db` write access trying to make an audit
//! trail look clean after the fact). See
//! [`verify_approval_receipt_signature`]. Best-effort, not fail-closed like
//! `ReviewAuthority::mint`: a `:memory:` connection (or any other reason
//! `control.key` can't be loaded) leaves `signature` `NULL` rather than
//! refusing the insert -- this is an audit record about an edit that may
//! already be committed, and must never retroactively block or undo one
//! just because a signing side-channel had a hiccup (same fail-open
//! contract every existing caller of this function already relies on).
//!
//! **CCK-30R2 (audit 2026-08-10) -- `signature_provenance`:** the WS3
//! signature alone proves a row's bytes were signed by SOMEONE holding
//! `control.key`, but the `v9->v10` migration originally backfilled every
//! pre-existing (then-unsigned) row with a fresh signature over its
//! existing bytes -- which is indistinguishable, from the signature alone,
//! from a row `insert_approval_receipt` genuinely wrote at the moment the
//! approval happened. A migration-backfilled signature only proves "these
//! bytes existed when the migration ran"; it does NOT prove "this approval
//! was authentic when it happened" -- and the original doc comment above
//! overclaimed the latter for both cases alike. `signature_provenance` (now
//! itself folded into the signed payload, so it can't be silently flipped
//! by anyone with raw write access either) makes the distinction explicit:
//! `"native"` for a row [`insert_approval_receipt`] actually signed at
//! insert time, `"legacy_unverified"` for one whose signature was
//! (re-)derived after the fact by a schema migration. Every row that
//! predates this column collapses to `"legacy_unverified"` -- by the time
//! `v9_to_v10_approval_receipt_signature_provenance` runs, a genuinely
//! `"native"` v9-era row and a `v8_to_v9`-backfilled row are already
//! byte-identical in shape (both just have a non-`NULL` `signature`), so
//! there is no data left to tell them apart; claiming `"native"` for either
//! would repeat the exact overclaim this fix exists to remove.

use rusqlite::{Connection, OptionalExtension, params};

use crate::authority::key::{control_key_for_conn, sign, verify};

/// HMAC domain for this module's own receipt signature -- per-purpose
/// separation from `review::SIGNING_DOMAIN` (`"review-authority-v1"`), same
/// rationale as `key::sign`'s own doc comment: one shared `control.key`,
/// but a signature minted for one purpose can never be replayed as valid
/// for another.
pub(crate) const SIGNING_DOMAIN: &str = "approval-receipt-v1";

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

/// Canonical signed material for one receipt row -- every column except
/// `signature` itself, including `signature_provenance` (CCK-30R2): folding
/// the provenance claim INTO the signed payload is what stops an attacker
/// with raw `state.db` write access from silently flipping a
/// `"legacy_unverified"` row to `"native"` without the signature check
/// noticing. Shared between [`insert_approval_receipt`] (sign at write
/// time), [`verify_approval_receipt_signature`] (re-derive to check), and
/// the `v9_to_v10_approval_receipt_signature_provenance` state migration
/// (re-sign existing rows under the new shape), so all three can never
/// drift into different canonical forms of "the same" payload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn signing_payload(
    receipt_id: &str,
    change_id: Option<&str>,
    authority_id: Option<&str>,
    subject_digest: &str,
    approved_by: &str,
    mechanism: &str,
    decision: &str,
    approved_at: f64,
    tx_id: Option<&str>,
    signature_provenance: &str,
) -> String {
    format!(
        "receipt_id={receipt_id}\nchange_id={}\nauthority_id={}\nsubject_digest={subject_digest}\n\
         approved_by={approved_by}\nmechanism={mechanism}\ndecision={decision}\n\
         approved_at={approved_at}\ntx_id={}\nsignature_provenance={signature_provenance}\n",
        change_id.unwrap_or(""),
        authority_id.unwrap_or(""),
        tx_id.unwrap_or(""),
    )
}

/// Inserts one durable `approval_receipts` row and returns its
/// `receipt_id`. `decision` is always `"approved"` -- both current call
/// sites only ever call this on a successful approval path; a future
/// rejection-receipt caller would need its own explicit `decision` value,
/// not reuse this constructor with a lie in it. See this module's own doc
/// comment for the `signature` column's best-effort signing contract.
pub fn insert_approval_receipt(
    conn: &Connection,
    receipt: &ApprovalReceipt,
) -> rusqlite::Result<String> {
    let receipt_id = new_receipt_id();
    let approved_at = now_epoch_secs();
    const DECISION: &str = "approved";
    // CCK-30R2: the only call site entitled to claim `"native"` -- this IS
    // the moment of insert, so a signature computed here genuinely covers
    // an approval as it actually happened, not a byte-preserving replay of
    // it. Every other path that ever sets `signature` (only the schema
    // migration today) must claim `"legacy_unverified"` instead.
    const PROVENANCE: &str = "native";
    let signature = control_key_for_conn(conn).ok().flatten().map(|key| {
        sign(
            &key,
            SIGNING_DOMAIN,
            &signing_payload(
                &receipt_id,
                receipt.change_id,
                receipt.authority_id,
                receipt.subject_digest,
                receipt.approved_by,
                receipt.mechanism,
                DECISION,
                approved_at,
                receipt.tx_id,
                PROVENANCE,
            ),
        )
    });
    // Mirrors `signature`'s own `None`-iff-no-key symmetry: a provenance
    // claim with nothing behind it (no signature to back it) would be
    // worse than useless -- an unsigned row asserting `"native"` is
    // exactly the "audit trail looks clean" scenario this whole mechanism
    // exists to prevent.
    let signature_provenance = signature.as_ref().map(|_| PROVENANCE);
    conn.execute(
        "INSERT INTO approval_receipts \
         (receipt_id, change_id, authority_id, subject_digest, approved_by, mechanism, \
          decision, approved_at, tx_id, signature, signature_provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'approved', ?7, ?8, ?9, ?10)",
        params![
            receipt_id,
            receipt.change_id,
            receipt.authority_id,
            receipt.subject_digest,
            receipt.approved_by,
            receipt.mechanism,
            approved_at,
            receipt.tx_id,
            signature,
            signature_provenance,
        ],
    )?;
    Ok(receipt_id)
}

/// `Some(true)`/`Some(false)` verifying a stored `approval_receipts` row's
/// `signature` against one freshly recomputed from its own columns and the
/// current `control.key`; `None` when there's nothing meaningful to check
/// (no such row, no stored signature, or no key available for this
/// connection) -- deliberately never conflated with `Some(false)` (a
/// genuine mismatch), which is the one outcome actually worth flagging as
/// tampered or corrupted. CCK-30R2: the recomputed payload now includes
/// whatever `signature_provenance` is currently stored (`""` if somehow
/// still unset), same field the signer bound in -- so tampering with
/// EITHER `signature`-covered column OR the provenance claim itself both
/// surface as `Some(false)`, not just the original column set.
pub fn verify_approval_receipt_signature(
    conn: &Connection,
    receipt_id: &str,
) -> rusqlite::Result<Option<bool>> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        f64,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = conn
        .query_row(
            "SELECT change_id, authority_id, subject_digest, approved_by, mechanism, \
             decision, approved_at, tx_id, signature, signature_provenance \
             FROM approval_receipts WHERE receipt_id = ?1",
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
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        change_id,
        authority_id,
        subject_digest,
        approved_by,
        mechanism,
        decision,
        approved_at,
        tx_id,
        Some(signature),
        provenance,
    )) = row
    else {
        return Ok(None);
    };
    let Some(key) = control_key_for_conn(conn).ok().flatten() else {
        return Ok(None);
    };
    let payload = signing_payload(
        receipt_id,
        change_id.as_deref(),
        authority_id.as_deref(),
        &subject_digest,
        &approved_by,
        &mechanism,
        &decision,
        approved_at,
        tx_id.as_deref(),
        provenance.as_deref().unwrap_or(""),
    );
    Ok(Some(verify(&key, SIGNING_DOMAIN, &payload, &signature)))
}

/// Reads back `signature_provenance` for one receipt (`None` if the row
/// doesn't exist, or the column is unset -- see `insert_approval_receipt`'s
/// own `None`-iff-no-signature symmetry). Callers building an audit view
/// should show this alongside [`verify_approval_receipt_signature`]'s
/// result: a `Some(true)` verification on a `"legacy_unverified"` row is
/// NOT the same trust level as one on a `"native"` row -- see this module's
/// own doc comment.
pub fn approval_receipt_signature_provenance(
    conn: &Connection,
    receipt_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT signature_provenance FROM approval_receipts WHERE receipt_id = ?1",
        params![receipt_id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
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

    /// A real on-disk connection -- signing needs `control_key_for_conn`
    /// to find a real path, which `:memory:` never has. Same pattern as
    /// `authority::review`'s own `real_state_conn`.
    fn real_state_conn(dir: &std::path::Path) -> Connection {
        let db_path = dir.join(".calm").join("state.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        init_state_db(&conn).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_approval_receipt_signs_when_a_real_control_key_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
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

        let signature: Option<String> = conn
            .query_row(
                "SELECT signature FROM approval_receipts WHERE receipt_id = ?1",
                params![receipt_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            signature
                .as_deref()
                .is_some_and(|s| s.starts_with("hmac-sha256:")),
            "expected a real signature, got {signature:?}"
        );
        assert_eq!(
            verify_approval_receipt_signature(&conn, &receipt_id).unwrap(),
            Some(true)
        );
    }

    #[test]
    fn insert_approval_receipt_claims_native_provenance_when_signed() {
        // CCK-30R2: only a row `insert_approval_receipt` itself signs, at
        // the true moment of approval, may claim "native" -- this is the
        // one call site entitled to it.
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
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
        assert_eq!(
            approval_receipt_signature_provenance(&conn, &receipt_id).unwrap(),
            Some("native".to_string())
        );
        assert_eq!(
            verify_approval_receipt_signature(&conn, &receipt_id).unwrap(),
            Some(true)
        );
    }

    #[test]
    fn tampering_with_signature_provenance_itself_is_detected() {
        // CCK-30R2: signature_provenance is folded INTO the signed
        // payload specifically so an attacker with raw state.db write
        // access can't silently upgrade a "legacy_unverified" row's
        // provenance claim to "native" without the signature noticing.
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
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
        assert_eq!(
            approval_receipt_signature_provenance(&conn, &receipt_id).unwrap(),
            Some("native".to_string()),
            "sanity: insert_approval_receipt claims native before any tampering"
        );
        conn.execute(
            "UPDATE approval_receipts SET signature_provenance = 'legacy_unverified' \
             WHERE receipt_id = ?1",
            params![receipt_id],
        )
        .unwrap();
        assert_eq!(
            verify_approval_receipt_signature(&conn, &receipt_id).unwrap(),
            Some(false),
            "flipping the provenance claim must invalidate the signature, not just \
             flipping subject_digest/etc"
        );
    }

    #[test]
    fn insert_approval_receipt_leaves_signature_null_for_a_memory_only_connection() {
        // Documents the deliberate fail-open contract for `:memory:` (see
        // this module's own doc comment) -- a genuinely path-less
        // connection has no `control.key` to sign with, and that must
        // never turn into a hard insert failure.
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
        let signature: Option<String> = conn
            .query_row(
                "SELECT signature FROM approval_receipts WHERE receipt_id = ?1",
                params![receipt_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(signature, None);
        assert_eq!(
            verify_approval_receipt_signature(&conn, &receipt_id).unwrap(),
            None,
            "nothing to verify without a stored signature"
        );
    }

    #[test]
    fn verify_approval_receipt_signature_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
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
        // Tamper with a signed column directly, bypassing the API entirely
        // -- exactly the "attacker with raw state.db write access" scenario
        // this signature exists to catch.
        conn.execute(
            "UPDATE approval_receipts SET subject_digest = 'forged-digest' WHERE receipt_id = ?1",
            params![receipt_id],
        )
        .unwrap();
        assert_eq!(
            verify_approval_receipt_signature(&conn, &receipt_id).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn verify_approval_receipt_signature_returns_none_for_an_unknown_receipt_id() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        assert_eq!(
            verify_approval_receipt_signature(&conn, "RCPT-does-not-exist").unwrap(),
            None
        );
    }
}
