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

use hmac::{Hmac, Mac};
use rand::TryRngCore;
use rusqlite::{Connection, OptionalExtension, params};
use std::fmt;
use std::path::Path;

use crate::digest::evidence_digest;

type HmacSha256 = Hmac<sha2::Sha256>;

/// Same size/rationale as `memory.rs`'s `MAC_KEY_LEN` (32 random bytes,
/// the standard HMAC-SHA256 key size) — a deliberately SEPARATE key from
/// `memory.key`, not a shared one: the ledger and project-memory notes
/// protect different data with different blast radii if a key leaks, and
/// there's no reason to couple their compromise.
const LEDGER_KEY_LEN: usize = 32;
const LEDGER_KEY_FILENAME: &str = "audit.key";

/// Genesis previous-hash for the first row in the chain — distinct from
/// any real event-hash output (`sha256:<64 hex>` unkeyed, or
/// `hmac-sha256:<64 hex>` keyed — see `compute_event_hash`), so a
/// corrupted chain that resets to "no prior row" can't be confused with a
/// legitimate genesis link.
const GENESIS_PREV_HASH: &str = "GENESIS";

/// Reads (or lazily creates) this project's ledger signing key from
/// `<calm_dir>/audit.key`. Mirrors `memory.rs::load_or_create_mac_key`'s
/// write-then-restrict-to-0600 pattern exactly (see that function's doc
/// comment for the full rationale) — duplicated rather than shared
/// because the two take different directory arguments (that one joins
/// `.calm` onto a project root itself; this one is already handed the
/// `.calm` dir by `ledger_key_for_conn`, which derives it from the open
/// connection's own file path) and protect different key material.
fn load_or_create_ledger_key(calm_dir: &Path) -> std::io::Result<[u8; LEDGER_KEY_LEN]> {
    let key_path = calm_dir.join(LEDGER_KEY_FILENAME);

    if let Ok(bytes) = std::fs::read(&key_path)
        && bytes.len() == LEDGER_KEY_LEN
    {
        let mut key = [0u8; LEDGER_KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    std::fs::create_dir_all(calm_dir)?;
    let mut key = [0u8; LEDGER_KEY_LEN];
    rand::rngs::OsRng
        .try_fill_bytes(&mut key)
        .map_err(std::io::Error::other)?;
    std::fs::write(&key_path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// The ledger signing key for whatever project `conn` is actually open
/// against — derived from the connection's OWN file path (`rusqlite`'s
/// `Connection::path`), not a threaded-through parameter, so every
/// existing `append`/`verify_chain`/`head_digest` call site keeps working
/// completely unchanged (calm-core's `txn.rs` alone has ~15 call sites
/// across `begin`/`advance`/`advance_many` that would otherwise all need
/// a new parameter). `None` only for a path-less connection (`:memory:`,
/// what every test in this crate uses) or a key-file I/O failure (e.g. a
/// read-only `.calm/`) — both fall back to the unkeyed `evidence_digest`
/// chain this module had before HMAC signing existed, same
/// detection-degrades-gracefully posture `content_mac: NULL` already has
/// in `memory.rs`. Every REAL writer/reader connection
/// (`calm_core::db::conn::open_writer`, `make_read_conn`) always opens a
/// genuine file path, so production usage always gets a key — only this
/// module's own in-memory tests exercise the fallback.
fn ledger_key_for_conn(conn: &Connection) -> Option<[u8; LEDGER_KEY_LEN]> {
    let db_path = conn.path()?;
    let calm_dir = Path::new(db_path).parent()?;
    load_or_create_ledger_key(calm_dir).ok()
}

/// `event_hash = HMAC-SHA256(ledger_key, payload || prev_hash)` when a key
/// is available (every real on-disk ledger), else the original unkeyed
/// `SHA-256(payload || prev_hash)` (in-memory tests only) — see
/// `ledger_key_for_conn`. The two formats are prefix-distinguishable
/// (`hmac-sha256:` vs `evidence_digest`'s own `sha256:`) purely so a human
/// reading raw `event_hash` values can tell which mode produced a row;
/// `append`/`verify_chain` never need to branch on the prefix themselves
/// since both always re-derive the key the same way for the same `conn`.
fn compute_event_hash(conn: &Connection, payload: &str, prev_hash: &str) -> String {
    let input = format!("{payload}|{prev_hash}");
    match ledger_key_for_conn(conn) {
        Some(key) => {
            let mut mac =
                <HmacSha256 as Mac>::new_from_slice(&key).expect("HMAC accepts any key length");
            mac.update(input.as_bytes());
            let bytes = mac.finalize().into_bytes();
            let mut hex = String::with_capacity(bytes.len() * 2 + "hmac-sha256:".len());
            hex.push_str("hmac-sha256:");
            for b in bytes {
                hex.push_str(&format!("{b:02x}"));
            }
            hex
        }
        None => evidence_digest(input.as_bytes()),
    }
}

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

/// Appends one event to the chain, computing `event_hash` off the current
/// head via `compute_event_hash` (HMAC-signed for every real on-disk
/// ledger, see its doc comment). Callers build `payload` themselves as a
/// deterministic, delimited string (same convention `txn.rs`'s `event_id`
/// inputs already use) — this module does not impose a schema on what's
/// being audited, only that it's chained and content-addressed.
pub fn append(conn: &Connection, actor: &str, payload: &str) -> Result<LedgerEntry, LedgerError> {
    let prev_hash = head_digest(conn)?;
    let event_hash = compute_event_hash(conn, payload, &prev_hash);
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
/// `event_hash` (via `compute_event_hash`, HMAC-signed for a real on-disk
/// ledger) from its own `payload`+`prev_hash` and checking that
/// `prev_hash` actually matches the previous row's `event_hash` (or
/// `GENESIS_PREV_HASH` for the first row). Returns the first break found —
/// a tampered `payload`, a tampered `event_hash`, or a deleted row (which
/// surfaces as a `prev_hash` gap at the next surviving row). A row tampered
/// by an actor who can write the SQLite file but does NOT also have
/// `audit.key` can never pass this recomputation, unlike the old unkeyed
/// chain where SQLite write access alone was sufficient to forge a
/// consistent replacement chain from any point forward.
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
        let recomputed = compute_event_hash(conn, &payload, &prev_hash);
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

    /// A real, file-backed connection (unlike every test above, which uses
    /// `:memory:` and so exercises the unkeyed fallback) -- proves the
    /// keyed path actually engages for the kind of connection every real
    /// `calm serve`/`calm index` process opens.
    fn real_db_conn(name: &str) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("ci_ledger_{name}_"))
            .tempdir()
            .unwrap();
        let calm_dir = dir.path().join(".calm");
        std::fs::create_dir_all(&calm_dir).unwrap();
        let conn = crate::db::conn::open_writer(&calm_dir.join("index.db")).unwrap();
        init_db(&conn).unwrap();
        (dir, conn)
    }

    #[test]
    fn real_on_disk_ledger_signs_with_hmac_not_plain_sha256() {
        let (_dir, conn) = real_db_conn("hmac_prefix");
        let entry = append(&conn, "system", "one").unwrap();
        assert!(
            entry.event_hash.starts_with("hmac-sha256:"),
            "a real file-backed connection must produce a keyed event_hash, got: {}",
            entry.event_hash
        );
    }

    #[test]
    fn real_on_disk_ledger_creates_a_0600_key_file() {
        let (dir, conn) = real_db_conn("key_perms");
        append(&conn, "system", "one").unwrap();
        let key_path = dir.path().join(".calm").join(LEDGER_KEY_FILENAME);
        assert!(
            key_path.is_file(),
            "audit.key must be created on first append"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "audit.key must be created at 0600");
        }
    }

    #[test]
    fn reopening_the_same_db_path_reuses_the_same_key_and_still_verifies() {
        let dir = tempfile::Builder::new()
            .prefix("ci_ledger_reopen_")
            .tempdir()
            .unwrap();
        let calm_dir = dir.path().join(".calm");
        std::fs::create_dir_all(&calm_dir).unwrap();
        let db_path = calm_dir.join("index.db");

        {
            let conn = crate::db::conn::open_writer(&db_path).unwrap();
            init_db(&conn).unwrap();
            append(&conn, "system", "one").unwrap();
            append(&conn, "system", "two").unwrap();
        } // conn dropped -- a fresh connection object below must derive the SAME key.

        let conn = crate::db::conn::open_writer(&db_path).unwrap();
        assert_eq!(
            verify_chain(&conn).unwrap(),
            None,
            "a freshly reopened connection to the same DB path must reuse audit.key and \
             still verify rows appended by a prior connection object"
        );
        let third = append(&conn, "system", "three").unwrap();
        assert!(third.event_hash.starts_with("hmac-sha256:"));
    }

    #[test]
    fn tampering_via_direct_sqlite_write_access_alone_is_still_caught_without_the_key() {
        // The actual value HMAC adds over the old plain SHA-256 chain: an
        // attacker who only has SQLite file write access (no audit.key)
        // recomputing a hash the OLD unkeyed scheme would have accepted as
        // "consistent" must still be caught, because they can't reproduce
        // a valid HMAC without the key.
        let (_dir, conn) = real_db_conn("tamper_no_key");
        append(&conn, "system", "one").unwrap();
        let second = append(&conn, "system", "two").unwrap();
        append(&conn, "system", "three").unwrap();

        let forged_payload = "tampered";
        // What an attacker who only ever saw the OLD unkeyed scheme (or
        // doesn't know a key file is involved at all) would compute:
        // plain SHA-256 of payload||prev_hash, no key.
        let forged_hash =
            evidence_digest(format!("{forged_payload}|{}", second.prev_hash).as_bytes());
        conn.execute(
            "UPDATE audit_ledger SET payload = ?1, event_hash = ?2 WHERE seq = 2",
            params![forged_payload, forged_hash],
        )
        .unwrap();

        let break_at = verify_chain(&conn).unwrap();
        assert_eq!(
            break_at.map(|b| b.seq),
            Some(2),
            "a forged row using the OLD unkeyed formula must still fail HMAC verification"
        );
    }
}
