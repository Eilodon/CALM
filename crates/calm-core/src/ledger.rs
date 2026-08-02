//! Append-only, hash-chained audit ledger
//! (docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md#p0-4). A durable,
//! tamper-evident channel that runs *alongside* `AUDIT_TARGET` tracing
//! (`calm-server/src/telemetry.rs`) — that stream stays exactly what it is,
//! a SIEM-facing log line; this module is the separate durable store that
//! can prove after the fact whether any row was inserted, edited, or
//! deleted out of band. Mirrors VHEATM's `provenance.py:60-61` hash chain
//! (`event_hash = SHA-256(canonical(payload) || prev_hash)`), reusing this
//! crate's own `evidence_digest` (SHA-256, domain-separated from the
//! FNV-1a `hash_content` cache hash — see digest.rs) for consistency with
//! `txn.rs`'s `tx_events` content-addressed ids.
//!
//! Additive/shadow by construction like `txn.rs`/`maintenance.rs`: `append`
//! is the only write path. Nothing in this crate calls it yet — wiring a
//! real write path (edit_lines, format_files, gate denials, ...) to also
//! emit ledger rows is a separate, deliberately later change, and every
//! future call site is expected to wrap `append` in `.ok()`/`let _ =` so a
//! ledger failure can never block or alter the outcome of the operation it
//! is recording.

use rusqlite::{Connection, OptionalExtension, params};
use std::fmt;

use crate::digest::evidence_digest;

/// Genesis previous-hash for the first row in the chain — distinct from
/// any real `evidence_digest` output (those are always `sha256:<64 hex>`),
/// so a corrupted chain that resets to "no prior row" can't be confused
/// with a legitimate genesis link.
const GENESIS_PREV_HASH: &str = "GENESIS";

#[derive(Debug)]
pub enum LedgerError {
    Db(rusqlite::Error),
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::Db(e) => write!(f, "audit ledger db error: {e}"),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        LedgerError::Db(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LedgerEntry {
    pub seq: i64,
    pub prev_hash: String,
    pub event_hash: String,
    pub ts: f64,
    pub actor: String,
    pub payload: String,
}

/// First point of corruption found by `verify_chain`, if any. A single
/// tampered/deleted row anywhere in the chain invalidates every row after
/// it, so only the first break is meaningful — there is no "list every
/// subsequent row" mode here.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainBreak {
    pub seq: i64,
    pub reason: String,
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The `event_hash` of the most recently appended row, or
/// `GENESIS_PREV_HASH` if the ledger is empty — i.e. the value the next
/// `append` will use as its `prev_hash`.
pub fn head_digest(conn: &Connection) -> Result<String, LedgerError> {
    let hash: Option<String> = conn
        .query_row(
            "SELECT event_hash FROM audit_ledger ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(hash.unwrap_or_else(|| GENESIS_PREV_HASH.to_string()))
}

/// Appends one event to the chain, computing `event_hash =
/// SHA-256(canonical(payload) || prev_hash)` off the current head. Callers
/// build `payload` themselves as a deterministic, delimited string (same
/// convention `txn.rs`'s `event_id` inputs already use) — this module does
/// not impose a schema on what's being audited, only that it's chained and
/// content-addressed.
pub fn append(conn: &Connection, actor: &str, payload: &str) -> Result<LedgerEntry, LedgerError> {
    let prev_hash = head_digest(conn)?;
    let event_hash = evidence_digest(format!("{payload}|{prev_hash}").as_bytes());
    let ts = now_epoch_secs();
    conn.execute(
        "INSERT INTO audit_ledger (prev_hash, event_hash, ts, actor, payload) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![prev_hash, event_hash, ts, actor, payload],
    )?;
    let seq = conn.last_insert_rowid();
    Ok(LedgerEntry {
        seq,
        prev_hash,
        event_hash,
        ts,
        actor: actor.to_string(),
        payload: payload.to_string(),
    })
}

/// Replays the whole chain from `seq=1`, recomputing each row's
/// `event_hash` from its own `payload`+`prev_hash` and checking that
/// `prev_hash` actually matches the previous row's `event_hash` (or
/// `GENESIS_PREV_HASH` for the first row). Returns the first break found —
/// a tampered `payload`, a tampered `event_hash`, or a deleted row (which
/// surfaces as a `prev_hash` gap at the next surviving row).
pub fn verify_chain(conn: &Connection) -> Result<Option<ChainBreak>, LedgerError> {
    let mut stmt = conn
        .prepare("SELECT seq, prev_hash, event_hash, payload FROM audit_ledger ORDER BY seq ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut expected_prev = GENESIS_PREV_HASH.to_string();
    for row in rows {
        let (seq, prev_hash, event_hash, payload) = row?;
        if prev_hash != expected_prev {
            return Ok(Some(ChainBreak {
                seq,
                reason: format!(
                    "prev_hash mismatch: row stores {prev_hash:?}, chain expects {expected_prev:?} \
                     -- a row before this one was likely deleted"
                ),
            }));
        }
        let recomputed = evidence_digest(format!("{payload}|{prev_hash}").as_bytes());
        if recomputed != event_hash {
            return Ok(Some(ChainBreak {
                seq,
                reason: format!(
                    "event_hash mismatch: stored {event_hash:?}, recomputed {recomputed:?} \
                     -- payload or prev_hash was altered after insert"
                ),
            }));
        }
        expected_prev = event_hash;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn append_chains_prev_hash_to_previous_event_hash() {
        let conn = test_conn();
        let first = append(&conn, "system", "actor=system|event=start").unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.prev_hash, GENESIS_PREV_HASH);

        let second = append(&conn, "system", "actor=system|event=next").unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(
            second.prev_hash, first.event_hash,
            "each row's prev_hash must be the prior row's event_hash"
        );
        assert_ne!(second.event_hash, first.event_hash);
    }

    #[test]
    fn head_digest_tracks_the_last_appended_row() {
        let conn = test_conn();
        assert_eq!(
            head_digest(&conn).unwrap(),
            GENESIS_PREV_HASH,
            "an empty ledger's head must be the genesis sentinel"
        );
        let first = append(&conn, "system", "one").unwrap();
        assert_eq!(head_digest(&conn).unwrap(), first.event_hash);
        let second = append(&conn, "system", "two").unwrap();
        assert_eq!(head_digest(&conn).unwrap(), second.event_hash);
    }

    #[test]
    fn verify_chain_passes_on_an_untouched_chain() {
        let conn = test_conn();
        append(&conn, "system", "one").unwrap();
        append(&conn, "system", "two").unwrap();
        append(&conn, "system", "three").unwrap();
        assert_eq!(verify_chain(&conn).unwrap(), None);
    }

    #[test]
    fn verify_chain_is_a_noop_on_an_empty_ledger() {
        let conn = test_conn();
        assert_eq!(verify_chain(&conn).unwrap(), None);
    }

    #[test]
    fn ledger_replays_to_same_head_digest() {
        let conn = test_conn();
        append(&conn, "system", "one").unwrap();
        append(&conn, "system", "two").unwrap();
        let last = append(&conn, "system", "three").unwrap();
        assert_eq!(
            head_digest(&conn).unwrap(),
            last.event_hash,
            "head must match the last appended row's event_hash"
        );
        assert_eq!(
            verify_chain(&conn).unwrap(),
            None,
            "a chain that replays cleanly must report no break"
        );
    }

    #[test]
    fn ledger_detects_tampering_via_broken_hash_chain() {
        let conn = test_conn();
        append(&conn, "system", "one").unwrap();
        append(&conn, "system", "two").unwrap();
        append(&conn, "system", "three").unwrap();

        // Simulate an out-of-band edit: mutate row 2's payload directly,
        // bypassing `append` entirely -- exactly what this module exists
        // to detect.
        conn.execute(
            "UPDATE audit_ledger SET payload = 'tampered' WHERE seq = 2",
            [],
        )
        .unwrap();

        let break_at = verify_chain(&conn).unwrap();
        assert_eq!(
            break_at.map(|b| b.seq),
            Some(2),
            "tampering a row's payload must be caught at that row's own seq"
        );
    }

    #[test]
    fn ledger_detects_a_deleted_row_via_prev_hash_gap() {
        let conn = test_conn();
        append(&conn, "system", "one").unwrap();
        append(&conn, "system", "two").unwrap();
        append(&conn, "system", "three").unwrap();

        conn.execute("DELETE FROM audit_ledger WHERE seq = 2", [])
            .unwrap();

        let break_at = verify_chain(&conn).unwrap();
        assert_eq!(
            break_at.map(|b| b.seq),
            Some(3),
            "deleting a middle row must surface as a prev_hash gap at the next surviving row"
        );
    }

    #[test]
    fn two_independent_ledgers_reach_different_heads_for_different_content() {
        let conn_a = test_conn();
        let conn_b = test_conn();
        let a = append(&conn_a, "system", "same payload").unwrap();
        let b = append(&conn_b, "system", "different payload").unwrap();
        assert_ne!(a.event_hash, b.event_hash);
    }
}
