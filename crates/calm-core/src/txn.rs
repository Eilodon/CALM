//! WS-1 durable edit-transaction journal
//! (docs/plans/2026-08-02-phase1-p0-execution-plan.md §4).
//!
//! Mirrors VHEATM's `lifecycle.py::AuditLifecycle`: `edit_transactions.state`
//! is a CACHE, never written directly — [`advance`] is the only function
//! allowed to change it, and every call appends a matching `tx_events` row
//! in the same SQLite transaction. [`replay_state`] recomputes state purely
//! from `tx_events` and is used to self-check the cache never drifts from
//! the log (see the `replay_state_matches_cache_after_*` tests below).
//!
//! Not yet wired into any write path (`edit_lines_impl_gated`,
//! `format_files_impl`) — this module is standalone/shadow per plan §4.6
//! task 4.2. `maintenance_jobs` (the durable outbox for the SCIP/embedding
//! background refreshes) is a deliberately separate concern — see plan
//! §4.1b for why it must not be folded into this state machine.

use rusqlite::{Connection, OptionalExtension, params};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::digest::evidence_digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    Prepared,
    FileCommitted,
    IndexCommitted,
    // Not producible in Phase 1 (no verification pipeline exists yet) — kept
    // as a valid transition target now so WS-6 can start emitting it later
    // without a second migration to widen `allowed_next`. See plan §4.1b.
    VerifyPending,
    Done,
    Failed,
    RolledBack,
}

impl TxState {
    pub fn as_str(self) -> &'static str {
        match self {
            TxState::Prepared => "PREPARED",
            TxState::FileCommitted => "FILE_COMMITTED",
            TxState::IndexCommitted => "INDEX_COMMITTED",
            TxState::VerifyPending => "VERIFY_PENDING",
            TxState::Done => "DONE",
            TxState::Failed => "FAILED",
            TxState::RolledBack => "ROLLED_BACK",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "PREPARED" => TxState::Prepared,
            "FILE_COMMITTED" => TxState::FileCommitted,
            "INDEX_COMMITTED" => TxState::IndexCommitted,
            "VERIFY_PENDING" => TxState::VerifyPending,
            "DONE" => TxState::Done,
            "FAILED" => TxState::Failed,
            "ROLLED_BACK" => TxState::RolledBack,
            _ => return None,
        })
    }

    /// A transaction here will never gain a new `tx_events` row — the disk
    /// (if `Done`) or nothing (if `Failed`/`RolledBack`) is the final word.
    pub fn is_terminal(self) -> bool {
        matches!(self, TxState::Done | TxState::Failed | TxState::RolledBack)
    }
}

/// Mirror `lifecycle.py::ALLOWED_TRANSITIONS` — the ONLY place in this
/// codebase that decides whether a transition is legal. [`advance`] and
/// [`EditTransaction::from_document`]-equivalent replay (`replay_state`)
/// both call this; there is no other path that can move `state`.
fn allowed_next(from: TxState) -> &'static [TxState] {
    use TxState::*;
    match from {
        Prepared => &[FileCommitted, Failed],
        FileCommitted => &[IndexCommitted, Failed, RolledBack],
        // Phase 1 reality (plan §4.1b): base reindex is synchronous and
        // already durable once committed, so IndexCommitted -> Done is the
        // path every real caller takes. VerifyPending exists only so WS-6
        // has a legal transition to land on later.
        IndexCommitted => &[VerifyPending, Done, Failed],
        VerifyPending => &[Done, Failed],
        Done | Failed | RolledBack => &[],
    }
}

#[derive(Debug)]
pub enum TxnError {
    InvalidTransition { from: TxState, to: TxState },
    NotFound { tx_id: String },
    Corrupt(String),
    Db(rusqlite::Error),
}

impl fmt::Display for TxnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TxnError::InvalidTransition { from, to } => write!(
                f,
                "invalid lifecycle transition: {} -> {}",
                from.as_str(),
                to.as_str()
            ),
            TxnError::NotFound { tx_id } => write!(f, "unknown edit transaction: {tx_id}"),
            TxnError::Corrupt(detail) => write!(f, "edit-transaction journal corrupt: {detail}"),
            TxnError::Db(e) => write!(f, "edit-transaction journal db error: {e}"),
        }
    }
}

impl std::error::Error for TxnError {}

impl From<rusqlite::Error> for TxnError {
    fn from(e: rusqlite::Error) -> Self {
        TxnError::Db(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditTransaction {
    pub tx_id: String,
    pub project_id: String,
    pub path: String,
    pub base_digest: String,
    pub proposed_digest: String,
    pub state: TxState,
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Best-effort-unique, roughly time-sortable id — same rationale as
/// `edit::write_nonce`: uniqueness (not unpredictability) is the only
/// property relied on, and `edit_transactions.tx_id` is a real PRIMARY KEY
/// so a residual collision fails loudly (`rusqlite::Error`) rather than
/// silently overwriting another transaction. No new dependency (`ulid`/
/// `uuid`) pulled in for this — see plan §8 item 4 precedent (sha2 was
/// already a workspace dep; this needs none at all).
fn new_tx_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("TXN-{nanos:016x}-{counter:08x}-{}", std::process::id())
}

/// Content-addressed event id, mirroring
/// `provenance.py::expected_journal_event_id` — same digest algorithm
/// (WS-3's `evidence_digest`, SHA-256) as every other trust-boundary
/// identity in this plan, so a future CALM<->VHEATM handoff needs no
/// translation layer.
fn event_id(canonical_payload: &str) -> String {
    format!("EVT-{}", evidence_digest(canonical_payload.as_bytes()))
}

fn current_state(conn: &Connection, tx_id: &str) -> Result<Option<TxState>, TxnError> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM edit_transactions WHERE tx_id = ?1",
            params![tx_id],
            |row| row.get(0),
        )
        .optional()?;
    match state {
        None => Ok(None),
        Some(s) => TxState::parse(&s)
            .map(Some)
            .ok_or_else(|| TxnError::Corrupt(format!("unknown state {s:?} for tx {tx_id}"))),
    }
}

fn next_sequence(conn: &Connection, tx_id: &str) -> Result<i64, TxnError> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(sequence) FROM tx_events WHERE tx_id = ?1",
        params![tx_id],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

/// Opens `tx_id` in `Prepared` state. Both the `edit_transactions` row and
/// its matching `tx_events` seq-1 row are written inside one SQLite
/// transaction (`BEGIN IMMEDIATE` / `COMMIT`) — a process killed between the
/// two statements rolls back both, never leaving one without the other.
/// (This is the deterministic, non-OS-level equivalent of a crash-injection
/// test for this specific atomicity property: see
/// `begin_is_atomic_with_its_seq_1_event` below, which forces a real
/// rollback via an explicit `ROLLBACK` and asserts neither row survives.)
pub fn begin(
    conn: &Connection,
    project_id: &str,
    path: &str,
    base_digest: &str,
    proposed_digest: &str,
) -> Result<EditTransaction, TxnError> {
    let tx_id = new_tx_id();
    let now = now_epoch_secs();
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let result = (|| -> Result<(), TxnError> {
        conn.execute(
            "INSERT INTO edit_transactions \
             (tx_id, project_id, path, base_digest, proposed_digest, state, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'PREPARED', ?6, ?6)",
            params![tx_id, project_id, path, base_digest, proposed_digest, now],
        )?;
        let payload = format!("{tx_id}|created|PREPARED|system|begin|1|{now}");
        conn.execute(
            "INSERT INTO tx_events \
             (event_id, tx_id, sequence, from_state, to_state, actor, reason, occurred_at) \
             VALUES (?1, ?2, 1, 'created', 'PREPARED', 'system', 'begin', ?3)",
            params![event_id(&payload), tx_id, now],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(EditTransaction {
                tx_id,
                project_id: project_id.to_string(),
                path: path.to_string(),
                base_digest: base_digest.to_string(),
                proposed_digest: proposed_digest.to_string(),
                state: TxState::Prepared,
            })
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Mirrors a just-written (not yet committed) transition into the tamper-
/// evident, hash-chained `audit_ledger` -- inside the caller's currently
/// open explicit transaction, via a `SAVEPOINT` (docs/plans/2026-08-02-
/// shadow-txn-connection-consolidation-plan.md §5.1). This is what lets
/// [`write_transition`] fold the ledger mirror into the SAME commit as the
/// `tx_events`/`edit_transactions` write instead of a second, separate
/// commit -- while still preserving P0-4's invariant that a ledger failure
/// must never affect the transition's own outcome: an ordinary constraint
/// failure (e.g. a `UNIQUE` collision on `event_hash`) only rolls back this
/// one statement, but a more severe failure (disk-full, I/O error) could
/// force SQLite to abort the WHOLE enclosing transaction instead -- the
/// `SAVEPOINT` is what contains that to just this statement's own effects;
/// `ROLLBACK TO` undoes only the ledger write, never the transition write
/// that already happened moments earlier in the same transaction. Best-
/// effort by construction: swallows every error it can, same as the plain
/// `let _ = ledger::append(...)` this replaces.
fn append_ledger_in_savepoint(conn: &Connection, actor: &str, payload: &str) {
    if conn.execute_batch("SAVEPOINT ledger_append;").is_err() {
        return;
    }
    let outcome = crate::ledger::append(conn, actor, payload);
    let _ = conn.execute_batch(if outcome.is_ok() {
        "RELEASE ledger_append;"
    } else {
        "ROLLBACK TO ledger_append; RELEASE ledger_append;"
    });
}

/// Core per-transition write shared by [`advance`] (one transition inside
/// its own `BEGIN IMMEDIATE`/`COMMIT`) and [`advance_many`] (N transitions
/// across independent `tx_id`s inside ONE `BEGIN IMMEDIATE`/`COMMIT` --
/// docs/plans/2026-08-02-shadow-txn-connection-consolidation-plan.md §5.2).
/// Assumes the caller has ALREADY verified `to` is a legal transition from
/// `from` (both callers do this immediately beforehand, using the same
/// `current_state`+`allowed_next` check `advance` always used). Issues no
/// `BEGIN`/`COMMIT`/`ROLLBACK` of its own -- the caller owns the enclosing
/// explicit transaction; this only inserts the `tx_events` row, updates
/// `edit_transactions.state`, and mirrors into the ledger.
fn write_transition(
    conn: &Connection,
    tx_id: &str,
    from: TxState,
    to: TxState,
    actor: &str,
    reason: &str,
) -> Result<(), TxnError> {
    let now = now_epoch_secs();
    let sequence = next_sequence(conn, tx_id)?;
    let payload = format!(
        "{tx_id}|{}|{}|{actor}|{reason}|{sequence}|{now}",
        from.as_str(),
        to.as_str()
    );
    conn.execute(
        "INSERT INTO tx_events \
         (event_id, tx_id, sequence, from_state, to_state, actor, reason, occurred_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            event_id(&payload),
            tx_id,
            sequence,
            from.as_str(),
            to.as_str(),
            actor,
            reason,
            now
        ],
    )?;
    conn.execute(
        "UPDATE edit_transactions SET state = ?1, updated_at = ?2 WHERE tx_id = ?3",
        params![to.as_str(), now, tx_id],
    )?;
    append_ledger_in_savepoint(conn, actor, &payload);
    Ok(())
}

/// The only function allowed to change `edit_transactions.state`. Validates
/// `to` against [`allowed_next`], then writes the `tx_events` row and the
/// `state`/`updated_at` update inside one SQLite transaction — same
/// atomicity guarantee as [`begin`].
pub fn advance(
    conn: &Connection,
    tx_id: &str,
    to: TxState,
    actor: &str,
    reason: &str,
) -> Result<(), TxnError> {
    let current = current_state(conn, tx_id)?.ok_or_else(|| TxnError::NotFound {
        tx_id: tx_id.to_string(),
    })?;
    if !allowed_next(current).contains(&to) {
        return Err(TxnError::InvalidTransition { from: current, to });
    }
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let result = write_transition(conn, tx_id, current, to, actor, reason);
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Batches N `advance()`-equivalent transitions for INDEPENDENT `tx_id`s
/// into ONE explicit transaction (docs/plans/2026-08-02-shadow-txn-
/// connection-consolidation-plan.md §5.2) -- e.g. `format_files_impl`
/// advancing every formatted file's own transaction to `IndexCommitted` (or
/// `Done`) together, instead of one `BEGIN`/`COMMIT` pair per file. Safe
/// specifically because every transition here is for a DIFFERENT `tx_id`:
/// on a crash mid-batch, whichever `tx_id`s didn't get their row committed
/// yet simply remain at their previous state, identical to their individual
/// `advance()` call never having been attempted -- `replay_state`/
/// `recover_incomplete` operate per-`tx_id` and don't care which physical
/// transaction wrote a given row. Each transition is additionally wrapped in
/// its own `SAVEPOINT`, so one transition's failure (e.g. `NotFound` or
/// `InvalidTransition`) never blocks or rolls back any other transition in
/// the same batch -- matching exactly what N separate `advance()` calls
/// would have done, just sharing one commit.
///
/// **Not for batching different states of the SAME `tx_id`** (e.g.
/// `IndexCommitted` then `Done` for one transaction) -- that would make an
/// intermediate `TxState` unreachable as an independently durable, crash-
/// recoverable checkpoint, which is exactly what gate criterion 1's crash-
/// injection suite (`crates/calm-cli/tests/txn_crash_injection.rs`) checks
/// for. See the design doc's §5.0/§5.3 for why that specific case is
/// deliberately not supported here.
///
/// Returns one `Result` per input transition, in the same order, so a
/// caller can handle each exactly as it would have from N separate
/// `advance()` calls.
pub fn advance_many(
    conn: &Connection,
    transitions: &[(&str, TxState, &str, &str)],
) -> Vec<Result<(), TxnError>> {
    if transitions.is_empty() {
        return Vec::new();
    }
    if let Err(e) = conn.execute_batch("BEGIN IMMEDIATE;") {
        let msg = format!("could not begin batch transaction: {e}");
        return transitions
            .iter()
            .map(|_| Err(TxnError::Corrupt(msg.clone())))
            .collect();
    }
    let results: Vec<Result<(), TxnError>> = transitions
        .iter()
        .enumerate()
        .map(|(i, (tx_id, to, actor, reason))| -> Result<(), TxnError> {
            let savepoint = format!("advance_many_{i}");
            if let Err(e) = conn.execute_batch(&format!("SAVEPOINT {savepoint};")) {
                return Err(TxnError::from(e));
            }
            let outcome = (|| -> Result<(), TxnError> {
                let current = current_state(conn, tx_id)?.ok_or_else(|| TxnError::NotFound {
                    tx_id: (*tx_id).to_string(),
                })?;
                if !allowed_next(current).contains(to) {
                    return Err(TxnError::InvalidTransition {
                        from: current,
                        to: *to,
                    });
                }
                write_transition(conn, tx_id, current, *to, actor, reason)
            })();
            let _ = conn.execute_batch(&if outcome.is_ok() {
                format!("RELEASE {savepoint};")
            } else {
                format!("ROLLBACK TO {savepoint}; RELEASE {savepoint};")
            });
            outcome
        })
        .collect();
    let _ = conn.execute_batch("COMMIT;");
    results
}

/// Replays every `tx_events` row for `tx_id` in sequence order and returns
/// the state that replay derives — independent of, and used to check
/// against, the `edit_transactions.state` cache. Mirrors
/// `AuditLifecycle.from_document`.
pub fn replay_state(conn: &Connection, tx_id: &str) -> Result<TxState, TxnError> {
    let mut stmt =
        conn.prepare("SELECT to_state FROM tx_events WHERE tx_id = ?1 ORDER BY sequence ASC")?;
    let mut rows = stmt.query(params![tx_id])?;
    let mut state: Option<TxState> = None;
    while let Some(row) = rows.next()? {
        let to_state: String = row.get(0)?;
        state = Some(TxState::parse(&to_state).ok_or_else(|| {
            TxnError::Corrupt(format!(
                "unknown state {to_state:?} in tx_events for {tx_id}"
            ))
        })?);
    }
    state.ok_or_else(|| TxnError::NotFound {
        tx_id: tx_id.to_string(),
    })
}

/// Every transaction not yet in a terminal state (`Done`/`Failed`/
/// `RolledBack`) — startup recovery scans this to find work a crashed
/// process left dangling. Callers decide what "recovery" means per state
/// (repair_consistency, re-verify digest on disk, etc.) — this function
/// only surfaces the list.
pub fn recover_incomplete(conn: &Connection) -> Result<Vec<EditTransaction>, TxnError> {
    let mut stmt = conn.prepare(
        "SELECT tx_id, project_id, path, base_digest, proposed_digest, state \
         FROM edit_transactions \
         WHERE state NOT IN ('DONE', 'FAILED', 'ROLLED_BACK') \
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (tx_id, project_id, path, base_digest, proposed_digest, state_str) = row?;
        let state = TxState::parse(&state_str).ok_or_else(|| {
            TxnError::Corrupt(format!("unknown state {state_str:?} for tx {tx_id}"))
        })?;
        out.push(EditTransaction {
            tx_id,
            project_id,
            path,
            base_digest,
            proposed_digest,
            state,
        });
    }
    Ok(out)
}

/// Single-transaction lookup by `tx_id`, for `edit_transaction_status` (plan
/// §4.7) — same row shape as [`recover_incomplete`], no state filter.
pub fn get(conn: &Connection, tx_id: &str) -> Result<Option<EditTransaction>, TxnError> {
    conn.query_row(
        "SELECT tx_id, project_id, path, base_digest, proposed_digest, state \
         FROM edit_transactions WHERE tx_id = ?1",
        params![tx_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        },
    )
    .optional()?
    .map(
        |(tx_id, project_id, path, base_digest, proposed_digest, state_str)| {
            let state = TxState::parse(&state_str).ok_or_else(|| {
                TxnError::Corrupt(format!("unknown state {state_str:?} for tx {tx_id}"))
            })?;
            Ok(EditTransaction {
                tx_id,
                project_id,
                path,
                base_digest,
                proposed_digest,
                state,
            })
        },
    )
    .transpose()
}

/// Most recent transaction for `path` (by `created_at`), for
/// `repair_consistency` (plan §4.7) called with a path instead of a tx_id --
/// an agent diagnosing "is this file's last edit consistent" usually has the
/// path handy, not the tx_id.
pub fn latest_for_path(conn: &Connection, path: &str) -> Result<Option<EditTransaction>, TxnError> {
    let tx_id: Option<String> = conn
        .query_row(
            "SELECT tx_id FROM edit_transactions WHERE path = ?1 ORDER BY created_at DESC LIMIT 1",
            params![path],
            |row| row.get(0),
        )
        .optional()?;
    match tx_id {
        None => Ok(None),
        Some(tx_id) => get(conn, &tx_id),
    }
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
    fn begin_creates_prepared_transaction_with_matching_event() {
        let conn = test_conn();
        let tx = begin(
            &conn,
            "proj",
            "src/lib.rs",
            "sha256:base",
            "sha256:proposed",
        )
        .unwrap();
        assert_eq!(tx.state, TxState::Prepared);

        let cached = current_state(&conn, &tx.tx_id).unwrap();
        assert_eq!(cached, Some(TxState::Prepared));

        let replayed = replay_state(&conn, &tx.tx_id).unwrap();
        assert_eq!(
            replayed,
            TxState::Prepared,
            "replay must match the cached state"
        );

        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tx_events WHERE tx_id = ?1",
                params![tx.tx_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 1, "begin must write exactly one tx_events row");
    }

    #[test]
    fn begin_is_atomic_with_its_seq_1_event() {
        // Deterministic substitute for an OS-level SIGKILL-between-statements
        // test (plan §8 item 3): force a rollback partway through the same
        // multi-statement sequence begin() uses, and assert neither the
        // edit_transactions row nor the tx_events row survives — proving
        // the two writes really are one atomic unit, not two independent
        // ones that could be observed half-done after a real crash.
        let conn = test_conn();
        let tx_id = "TXN-test-atomicity";
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        conn.execute(
            "INSERT INTO edit_transactions \
             (tx_id, project_id, path, base_digest, proposed_digest, state, created_at, updated_at) \
             VALUES (?1, 'proj', 'f.rs', 'b', 'p', 'PREPARED', 0, 0)",
            params![tx_id],
        )
        .unwrap();
        // Crash simulated here, before the matching tx_events row is
        // written and before COMMIT.
        conn.execute_batch("ROLLBACK;").unwrap();

        let cached = current_state(&conn, tx_id).unwrap();
        assert_eq!(
            cached, None,
            "rollback must leave no edit_transactions row behind"
        );
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tx_events WHERE tx_id = ?1",
                params![tx_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 0);
    }

    #[test]
    fn advance_follows_allowed_transitions_to_done() {
        let conn = test_conn();
        let tx = begin(&conn, "proj", "f.rs", "b", "p").unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::FileCommitted,
            "system",
            "wrote to disk",
        )
        .unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::IndexCommitted,
            "system",
            "reindexed",
        )
        .unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::Done,
            "system",
            "base index consistent",
        )
        .unwrap();

        assert_eq!(
            current_state(&conn, &tx.tx_id).unwrap(),
            Some(TxState::Done)
        );
        assert_eq!(replay_state(&conn, &tx.tx_id).unwrap(), TxState::Done);

        let sequences: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT sequence FROM tx_events WHERE tx_id = ?1 ORDER BY sequence")
                .unwrap();
            stmt.query_map(params![tx.tx_id], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            sequences,
            vec![1, 2, 3, 4],
            "sequence must be contiguous 1..=N"
        );
    }

    #[test]
    fn advance_mirrors_every_committed_transition_into_the_audit_ledger() {
        // P0-4 wiring (docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md
        // #part-3): every committed `advance()` call must also append one chained row
        // to `audit_ledger`, mirroring the `tx_events` payload it just wrote.
        let conn = test_conn();
        let tx = begin(&conn, "proj", "f.rs", "b", "p").unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::FileCommitted,
            "system",
            "wrote to disk",
        )
        .unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::IndexCommitted,
            "system",
            "reindexed",
        )
        .unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::Done,
            "system",
            "base index consistent",
        )
        .unwrap();

        // begin() itself never calls advance(), so only these 3 advance() calls
        // should have appended to the ledger -- not a 4th row for PREPARED.
        let ledger_rows: Vec<(i64, String)> = {
            let mut stmt = conn
                .prepare("SELECT seq, payload FROM audit_ledger ORDER BY seq")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            ledger_rows.len(),
            3,
            "one ledger row per committed advance() call, none for begin()'s PREPARED insert"
        );
        for (_, payload) in &ledger_rows {
            assert!(
                payload.contains(&tx.tx_id),
                "ledger payload must mirror the tx_events payload this transition wrote"
            );
        }
        assert_eq!(
            crate::ledger::verify_chain(&conn).unwrap(),
            None,
            "a freshly-written chain produced entirely by advance() must verify clean"
        );
    }

    /// Tier 2 Option A (docs/plans/2026-08-02-shadow-txn-connection-
    /// consolidation-plan.md §5.1): the ledger append now happens INSIDE
    /// advance()'s own transaction via a SAVEPOINT instead of a second,
    /// separate commit afterward. This proves the invariant that change was
    /// designed to preserve still holds: even when the ledger insert itself
    /// fails outright (simulated here by dropping `audit_ledger` after
    /// `init_db` already created it, so `ledger::append`'s INSERT gets a
    /// real "no such table" error), the transition's own tx_events row and
    /// edit_transactions.state update must still commit normally.
    #[test]
    fn advance_transition_survives_even_when_the_ledger_insert_itself_fails() {
        let conn = test_conn();
        conn.execute_batch("DROP TABLE audit_ledger;").unwrap();

        let tx = begin(&conn, "proj", "a.py", "base", "new").unwrap();
        let result = advance(
            &conn,
            &tx.tx_id,
            TxState::FileCommitted,
            "system",
            "wrote to disk",
        );

        assert!(
            result.is_ok(),
            "advance() must still commit the transition even though the ledger insert \
             itself failed: {result:?}"
        );
        let cached = get(&conn, &tx.tx_id).unwrap().unwrap();
        assert_eq!(cached.state, TxState::FileCommitted);
        assert_eq!(
            replay_state(&conn, &tx.tx_id).unwrap(),
            TxState::FileCommitted,
            "tx_events row must exist despite the ledger failure -- replay_state derives \
             purely from tx_events, independent of the ledger entirely"
        );
    }

    /// Tier 2 Option B (docs/plans/2026-08-02-shadow-txn-connection-
    /// consolidation-plan.md §5.2): `advance_many` batches the SAME state
    /// transition across independent tx_ids (format_files_impl's use case)
    /// into one shared transaction. Every tx_id must still get its own
    /// correct tx_events row, cached state, and ledger entry -- batching the
    /// commit must not blur or lose any individual tx_id's own history.
    #[test]
    fn advance_many_batches_independent_tx_ids_and_all_succeed() {
        let conn = test_conn();
        let tx_a = begin(&conn, "proj", "a.py", "ba", "na").unwrap();
        let tx_b = begin(&conn, "proj", "b.py", "bb", "nb").unwrap();
        let tx_c = begin(&conn, "proj", "c.py", "bc", "nc").unwrap();

        let results = advance_many(
            &conn,
            &[
                (tx_a.tx_id.as_str(), TxState::FileCommitted, "system", "r"),
                (tx_b.tx_id.as_str(), TxState::FileCommitted, "system", "r"),
                (tx_c.tx_id.as_str(), TxState::FileCommitted, "system", "r"),
            ],
        );

        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(
                r.is_ok(),
                "every independent tx_id's transition should succeed: {r:?}"
            );
        }
        for tx in [&tx_a, &tx_b, &tx_c] {
            let cached = get(&conn, &tx.tx_id).unwrap().unwrap();
            assert_eq!(cached.state, TxState::FileCommitted);
            assert_eq!(
                replay_state(&conn, &tx.tx_id).unwrap(),
                TxState::FileCommitted
            );
        }
        let ledger_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_ledger", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            ledger_rows, 3,
            "one ledger row per batched transition, same as 3 separate advance() calls"
        );
        assert_eq!(
            crate::ledger::verify_chain(&conn).unwrap(),
            None,
            "a chain produced by one batched advance_many call must still verify clean"
        );
    }

    /// Tier 2 Option B, failure isolation: one tx_id's transition being
    /// invalid (already terminal, or wrong current state) must not block or
    /// roll back any OTHER tx_id's transition in the same
    /// `advance_many` batch -- matching exactly what N separate `advance()`
    /// calls would have done.
    #[test]
    fn advance_many_one_invalid_transition_does_not_block_the_others() {
        let conn = test_conn();
        let tx_ok = begin(&conn, "proj", "ok.py", "b1", "n1").unwrap();
        let tx_bad = begin(&conn, "proj", "bad.py", "b2", "n2").unwrap();
        // Advance tx_bad to a terminal state OUTSIDE the batch, so its batched
        // transition below is guaranteed invalid (Done has no allowed_next).
        advance(&conn, &tx_bad.tx_id, TxState::FileCommitted, "system", "r").unwrap();
        advance(&conn, &tx_bad.tx_id, TxState::IndexCommitted, "system", "r").unwrap();
        advance(&conn, &tx_bad.tx_id, TxState::Done, "system", "r").unwrap();

        let results = advance_many(
            &conn,
            &[
                (tx_ok.tx_id.as_str(), TxState::FileCommitted, "system", "r"),
                (tx_bad.tx_id.as_str(), TxState::FileCommitted, "system", "r"),
            ],
        );

        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok(), "tx_ok's transition: {:?}", results[0]);
        assert!(
            matches!(results[1], Err(TxnError::InvalidTransition { .. })),
            "tx_bad's transition must fail as InvalidTransition (Done is terminal): {:?}",
            results[1]
        );

        assert_eq!(
            get(&conn, &tx_ok.tx_id).unwrap().unwrap().state,
            TxState::FileCommitted,
            "tx_ok must still have committed despite tx_bad's failure in the same batch"
        );
        assert_eq!(
            get(&conn, &tx_bad.tx_id).unwrap().unwrap().state,
            TxState::Done,
            "tx_bad must remain unchanged at Done, not corrupted by its own failed attempt"
        );
    }

    #[test]
    fn advance_never_ledgers_a_rejected_transition() {
        let conn = test_conn();
        let tx = begin(&conn, "proj", "f.rs", "b", "p").unwrap();
        // allowed_next(Prepared) == [FileCommitted, Failed] -- Prepared -> Prepared is
        // rejected before BEGIN IMMEDIATE (and therefore before the ledger append) is
        // ever reached.
        let err = advance(&conn, &tx.tx_id, TxState::Prepared, "system", "no-op").unwrap_err();
        assert!(matches!(err, TxnError::InvalidTransition { .. }));

        let ledger_row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_ledger", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            ledger_row_count, 0,
            "a transition rejected by allowed_next() must never reach the ledger"
        );
    }

    #[test]
    fn advance_rejects_out_of_order_transition() {
        let conn = test_conn();
        let tx = begin(&conn, "proj", "f.rs", "b", "p").unwrap();
        let err = advance(&conn, &tx.tx_id, TxState::Done, "system", "skip ahead").unwrap_err();
        assert!(matches!(
            err,
            TxnError::InvalidTransition {
                from: TxState::Prepared,
                to: TxState::Done
            }
        ));
        // Rejected transition must not have written anything.
        assert_eq!(
            current_state(&conn, &tx.tx_id).unwrap(),
            Some(TxState::Prepared)
        );
        assert_eq!(replay_state(&conn, &tx.tx_id).unwrap(), TxState::Prepared);
    }

    #[test]
    fn advance_rejects_transition_from_terminal_state() {
        let conn = test_conn();
        let tx = begin(&conn, "proj", "f.rs", "b", "p").unwrap();
        advance(&conn, &tx.tx_id, TxState::Failed, "system", "write failed").unwrap();
        let err = advance(&conn, &tx.tx_id, TxState::FileCommitted, "system", "retry").unwrap_err();
        assert!(matches!(
            err,
            TxnError::InvalidTransition {
                from: TxState::Failed,
                to: TxState::FileCommitted
            }
        ));
    }

    #[test]
    fn advance_unknown_tx_id_returns_not_found() {
        let conn = test_conn();
        let err = advance(&conn, "TXN-does-not-exist", TxState::Done, "system", "x").unwrap_err();
        assert!(matches!(err, TxnError::NotFound { .. }));
    }

    #[test]
    fn replay_state_reflects_an_interrupted_sequence() {
        // Equivalent of "process crashed after FileCommitted, before
        // IndexCommitted": simply never call the next advance(). A fresh
        // reader (simulating recovery after restart) must see exactly
        // FileCommitted from both the cache and independent replay.
        let conn = test_conn();
        let tx = begin(&conn, "proj", "f.rs", "b", "p").unwrap();
        advance(
            &conn,
            &tx.tx_id,
            TxState::FileCommitted,
            "system",
            "wrote to disk",
        )
        .unwrap();

        assert_eq!(
            current_state(&conn, &tx.tx_id).unwrap(),
            Some(TxState::FileCommitted)
        );
        assert_eq!(
            replay_state(&conn, &tx.tx_id).unwrap(),
            TxState::FileCommitted
        );
    }

    #[test]
    fn recover_incomplete_finds_only_non_terminal_transactions() {
        let conn = test_conn();
        let stuck = begin(&conn, "proj", "stuck.rs", "b1", "p1").unwrap();
        advance(
            &conn,
            &stuck.tx_id,
            TxState::FileCommitted,
            "system",
            "wrote",
        )
        .unwrap();

        let finished = begin(&conn, "proj", "done.rs", "b2", "p2").unwrap();
        advance(
            &conn,
            &finished.tx_id,
            TxState::FileCommitted,
            "system",
            "wrote",
        )
        .unwrap();
        advance(
            &conn,
            &finished.tx_id,
            TxState::IndexCommitted,
            "system",
            "reindexed",
        )
        .unwrap();
        advance(&conn, &finished.tx_id, TxState::Done, "system", "done").unwrap();

        let failed = begin(&conn, "proj", "failed.rs", "b3", "p3").unwrap();
        advance(&conn, &failed.tx_id, TxState::Failed, "system", "disk full").unwrap();

        let incomplete = recover_incomplete(&conn).unwrap();
        let incomplete_ids: Vec<&str> = incomplete.iter().map(|t| t.tx_id.as_str()).collect();
        assert_eq!(incomplete_ids, vec![stuck.tx_id.as_str()]);
        assert_eq!(incomplete[0].state, TxState::FileCommitted);
    }

    #[test]
    fn two_interleaved_transactions_keep_independent_contiguous_sequences() {
        let conn = test_conn();
        let a = begin(&conn, "proj", "a.rs", "ba", "pa").unwrap();
        let b = begin(&conn, "proj", "b.rs", "bb", "pb").unwrap();
        advance(
            &conn,
            &a.tx_id,
            TxState::FileCommitted,
            "system",
            "a step 1",
        )
        .unwrap();
        advance(
            &conn,
            &b.tx_id,
            TxState::FileCommitted,
            "system",
            "b step 1",
        )
        .unwrap();
        advance(
            &conn,
            &a.tx_id,
            TxState::IndexCommitted,
            "system",
            "a step 2",
        )
        .unwrap();

        assert_eq!(
            replay_state(&conn, &a.tx_id).unwrap(),
            TxState::IndexCommitted
        );
        assert_eq!(
            replay_state(&conn, &b.tx_id).unwrap(),
            TxState::FileCommitted
        );
    }

    #[test]
    fn get_returns_none_for_unknown_tx_id() {
        let conn = test_conn();
        assert!(get(&conn, "TXN-does-not-exist").unwrap().is_none());
    }

    #[test]
    fn get_returns_the_current_cached_row() {
        let conn = test_conn();
        let tx = begin(&conn, "proj", "src/a.rs", "sha256:base", "sha256:proposed").unwrap();
        advance(&conn, &tx.tx_id, TxState::FileCommitted, "system", "wrote").unwrap();

        let fetched = get(&conn, &tx.tx_id).unwrap().expect("tx must exist");
        assert_eq!(fetched.tx_id, tx.tx_id);
        assert_eq!(fetched.path, "src/a.rs");
        assert_eq!(fetched.state, TxState::FileCommitted);
        assert_eq!(fetched.base_digest, "sha256:base");
        assert_eq!(fetched.proposed_digest, "sha256:proposed");
    }
}
