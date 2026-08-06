//! WS-1 durable maintenance outbox
//! (docs/plans/2026-08-02-phase1-p0-execution-plan.md §4.1b/§4.3).
//!
//! Pure DB primitives only. Deliberately knows nothing about
//! `scip_overlay::run_all_coalesced` or `embedding::embed_pending(_chunks)`
//! — those already re-scan/coalesce globally and are idempotent, and stay
//! completely unmodified. This module only closes the durability gap around
//! the fire-and-forget `std::thread::spawn` calls that invoke them (see plan
//! §4.1b for the runtime evidence: `crates/calm-server/src/tools/edit.rs`
//! comment at the spawn site documents a real 2026-07-10 incident where a
//! killed process left formal edges stuck at 0 with nothing recording that
//! a refresh was still owed).
//!
//! Deliberately a GLOBAL singleton per [`MaintenanceKind`]
//! (`dedupe_key = kind`), not a per-path or per-transaction job queue —
//! `run_all_coalesced` and `embed_pending` are whole-repo passes, not
//! per-file jobs, so modeling this any other way would invent a granularity
//! neither subsystem actually has. Not wired into any live call site yet;
//! wiring (plan §4.4/§4.6 task 4.4) touches `tools/edit.rs`'s two existing
//! spawn sites and is deliberately a separate, later change.

use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceKind {
    ScipRefresh,
    EmbedRefresh,
}

impl MaintenanceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MaintenanceKind::ScipRefresh => "scip_refresh",
            MaintenanceKind::EmbedRefresh => "embed_refresh",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scip_refresh" => MaintenanceKind::ScipRefresh,
            "embed_refresh" => MaintenanceKind::EmbedRefresh,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => JobState::Queued,
            "running" => JobState::Running,
            "done" => JobState::Done,
            "failed" => JobState::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaintenanceJob {
    pub job_id: String,
    pub kind: MaintenanceKind,
    pub state: JobState,
    pub triggered_by_tx_id: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
    /// `None` until `mark_completed` runs at least once for this kind's
    /// current row — a job still `Queued`/`Running` has never completed yet.
    pub last_completed_at: Option<f64>,
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Audit 3.4: how long a `mark_running` lease is trusted before
/// `reconcile_stale_at_startup` is willing to treat the row as abandoned.
/// `run_all_coalesced`'s own rerun-loop can genuinely run for tens of
/// seconds on a large multi-language repo (per its own doc comment); this
/// is a generous multiple of that, not a tight heartbeat interval — no
/// explicit heartbeat/lease-extension exists yet (the audit's other
/// suggested option), so every fresh `mark_running` call for the SAME kind
/// (including one from a thread that turns out to be a coalescing
/// follower, not a leader — see `edit.rs`'s SCIP spawn) refreshes the
/// expiry, which in practice re-extends a long leader's lease for free
/// every time a new trigger arrives during its run.
const MAINTENANCE_LEASE_TTL_SECS: f64 = 300.0;

/// A best-effort, process-lifetime-stable identifier for `lease_owner` —
/// doesn't need cryptographic uniqueness (nothing security-sensitive keys
/// off it), just enough to tell "which process instance" apart across a
/// PID reuse: OS PIDs recycle, but combining one with the moment this
/// process first asked for it does not, in any way this reconciliation
/// logic depends on (it only ever compares/reads timestamps, never equates
/// two owner strings for a security decision).
fn process_owner_id() -> &'static str {
    static OWNER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OWNER.get_or_init(|| format!("PID-{}-{:x}", std::process::id(), (now_epoch_secs() * 1e6) as u64))
}

/// job_id doesn't need to be unpredictable or globally unique across
/// projects — `dedupe_key = kind.as_str()` (a real UNIQUE column) is what
/// actually prevents duplicate 'queued'/'running' rows for the same kind;
/// this is just a stable-enough primary key for the row itself.
fn job_id_for(kind: MaintenanceKind) -> String {
    format!(
        "MJB-{}-{:x}",
        kind.as_str(),
        (now_epoch_secs() * 1e6) as u64
    )
}

/// Records that `kind` needs a refresh pass, coalescing with any
/// already-queued-or-running job for the same kind. There is exactly one
/// row per kind, identified by `dedupe_key = kind.as_str()` (UNIQUE): a
/// single atomic `INSERT ... ON CONFLICT(dedupe_key) DO UPDATE ... WHERE
/// state IN ('done','failed')` either inserts the row the first time, or
/// resets a terminal row back to 'queued' -- but the `DO UPDATE`'s `WHERE`
/// clause makes the update a no-op when the existing row is already
/// 'queued'/'running', so a burst of concurrent edits collapses onto one
/// pending job with no separate lock, mirroring `OVERLAY_IN_FLIGHT`'s
/// in-memory coalescing but durable across a crash. Call this BEFORE
/// spawning the background thread that will actually run the refresh.
///
/// Returns `true` if this call (re)queued the job (caller should spawn
/// work), `false` if one was already pending (the pending one covers this
/// trigger too, so spawning again would be redundant).
pub fn enqueue(
    conn: &Connection,
    kind: MaintenanceKind,
    triggered_by_tx_id: Option<&str>,
) -> rusqlite::Result<bool> {
    let now = now_epoch_secs();
    let changed = conn.execute(
        "INSERT INTO maintenance_jobs \
             (job_id, job_kind, dedupe_key, state, triggered_by_tx_id, attempts, available_at) \
         VALUES (?1, ?2, ?2, 'queued', ?3, 0, ?4) \
         ON CONFLICT(dedupe_key) DO UPDATE SET \
             state = 'queued', \
             triggered_by_tx_id = excluded.triggered_by_tx_id, \
             attempts = 0, \
             available_at = excluded.available_at, \
             last_error = NULL \
         WHERE maintenance_jobs.state IN ('done', 'failed')",
        params![job_id_for(kind), kind.as_str(), triggered_by_tx_id, now],
    )?;
    Ok(changed > 0)
}

/// Marks the current job for `kind` 'running' — called right before the
/// actual refresh work starts, so a job stuck at 'queued' after a crash
/// (found by `pending_jobs` at startup) is visibly distinguishable from one
/// that's genuinely just been enqueued and not picked up yet.
///
/// Audit 3.3/3.4: the WHERE clause is `state != 'running'` (not the
/// narrower `state = 'queued'` this used to require) so a thread that
/// starts REAL work is always able to record it, even if a differently-
/// raced earlier completer already flipped the row to 'done'/'failed'
/// first (see `mark_completed`'s doc comment for the full race). Also
/// claims a time-bounded lease (`lease_owner`/`lease_expires_at`, columns
/// the schema already reserved but nothing wrote to before this) so
/// `reconcile_stale_at_startup` can tell "a sibling process is still
/// legitimately working on this" apart from "the process that was running
/// this is dead" — see that function's doc comment.
pub fn mark_running(conn: &Connection, kind: MaintenanceKind) -> rusqlite::Result<()> {
    let now = now_epoch_secs();
    conn.execute(
        "UPDATE maintenance_jobs SET state = 'running', attempts = attempts + 1, \
         lease_owner = ?2, lease_expires_at = ?3 \
         WHERE dedupe_key = ?1 AND state != 'running'",
        params![
            kind.as_str(),
            process_owner_id(),
            now + MAINTENANCE_LEASE_TTL_SECS
        ],
    )?;
    Ok(())
}

/// Marks the current job for `kind` 'done' (clearing `last_error`) or
/// 'failed' (recording it) — called after the wrapped `run_all_coalesced`/
/// `embed_pending(_chunks)` call returns, regardless of outcome. Does NOT
/// delete the row: a 'done'/'failed' row's `last_completed_at`/`last_error`
/// is diagnostic history for `maintenance_status()` (plan §4.5), cheap to
/// keep since dedupe only cares about 'queued'/'running' rows.
///
/// Audit 3.3: previously gated `WHERE state IN ('queued', 'running')`,
/// which silently no-ops when a DIFFERENT, later, genuinely-real completion
/// races ahead. Concretely (the embedding spawn in `edit.rs`, serialized by
/// a real mutex, not skip-if-in-flight like the SCIP path): thread A
/// acquires the mutex, embeds, calls `mark_completed` -> state becomes
/// 'done'. Thread B (a second edit's trigger, was genuinely waiting on the
/// same mutex) then acquires it, does its OWN real `embed_pending` pass
/// (which may cover rows A's pass predates), finishes, and calls
/// `mark_completed` too -- but the old WHERE clause required 'queued'/
/// 'running', which no longer matched (A already flipped it to 'done'), so
/// B's completion silently vanished. If the process crashed right after A's
/// premature-relative-to-B "done" but before B's real pass finished, the
/// durable row would say 'done' even though B's trigger was never actually
/// serviced. Unconditional on `state` now (still scoped to this `kind`'s
/// row by `dedupe_key`) -- every call here represents a real, completed
/// unit of work and should always be recorded, not just the first one to
/// arrive. Also clears the lease `mark_running` claimed: the job isn't
/// running anymore regardless of who finishes last.
pub fn mark_completed(
    conn: &Connection,
    kind: MaintenanceKind,
    result: Result<(), &str>,
) -> rusqlite::Result<()> {
    let now = now_epoch_secs();
    match result {
        Ok(()) => conn.execute(
            "UPDATE maintenance_jobs \
             SET state = 'done', last_completed_at = ?2, last_error = NULL, \
                 lease_owner = NULL, lease_expires_at = NULL \
             WHERE dedupe_key = ?1",
            params![kind.as_str(), now],
        )?,
        Err(detail) => conn.execute(
            "UPDATE maintenance_jobs \
             SET state = 'failed', last_completed_at = ?2, last_error = ?3, \
                 lease_owner = NULL, lease_expires_at = NULL \
             WHERE dedupe_key = ?1",
            params![kind.as_str(), now, detail],
        )?,
    };
    Ok(())
}

/// Every job not yet 'done'/'failed' — startup recovery scans this to find
/// a `job_kind` whose trigger (an edit's post-write spawn) fired, but no
/// process ever got to `mark_completed` for it (killed process, or spawn
/// itself never ran). Callers re-invoke the real refresh function for each
/// returned kind (`run_all_coalesced`/`embed_pending`, unchanged) and then
/// call `mark_completed` themselves — this function only surfaces the list,
/// it does not know how to run either refresh.
pub fn pending_jobs(conn: &Connection) -> rusqlite::Result<Vec<MaintenanceJob>> {
    query_jobs(
        conn,
        "SELECT job_id, job_kind, state, triggered_by_tx_id, attempts, last_error, \
             last_completed_at \
         FROM maintenance_jobs WHERE state IN ('queued', 'running') \
         ORDER BY available_at ASC",
    )
}

/// Every job row that exists (both kinds, any state) — for `maintenance_status`
/// (plan §4.7): a human/agent asking "what's the state of background
/// refresh" wants to see a healthy 'done' row too, not just pending ones.
pub fn all_jobs(conn: &Connection) -> rusqlite::Result<Vec<MaintenanceJob>> {
    query_jobs(
        conn,
        "SELECT job_id, job_kind, state, triggered_by_tx_id, attempts, last_error, \
             last_completed_at \
         FROM maintenance_jobs ORDER BY job_kind ASC",
    )
}

fn query_jobs(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<MaintenanceJob>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<f64>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (
            job_id,
            kind_str,
            state_str,
            triggered_by_tx_id,
            attempts,
            last_error,
            last_completed_at,
        ) = row?;
        let Some(kind) = MaintenanceKind::parse(&kind_str) else {
            continue; // unknown future kind: not this version's job to run
        };
        let Some(state) = JobState::parse(&state_str) else {
            continue;
        };
        out.push(MaintenanceJob {
            job_id,
            kind,
            state,
            triggered_by_tx_id,
            attempts,
            last_error,
            last_completed_at,
        });
    }
    Ok(out)
}

/// Startup reconciliation (plan §4.6) — called once from
/// `CalmServer::new_with_preset`, i.e. once per real process start (every
/// launch path funnels through it: stdio, unix daemon, and the CLI's direct
/// invocation all call `bootstrap` -> `new_with_preset`). This does NOT
/// re-invoke `run_all_coalesced`/`embed_pending` itself:
/// `crates/calm-server/src/lib.rs::bootstrap` already runs a full,
/// unconditional SCIP-overlay + embedding pass on every startup for
/// whichever process wins the indexer lock (see `bootstrap_embeddings` and
/// the `scip_overlay::run_all` call right after it) — re-triggering the
/// same work here would just race a second copy of it. This function's
/// only job is to stop `maintenance_status` from lying (showing a job
/// "running" forever after the process that was running it is long gone).
///
/// Audit 3.4: a row still `queued`/`running` at this exact point does NOT
/// necessarily belong to a dead process — CALM allows multiple processes
/// against the same project (`crates/calm-core/src/db/instance_lock.rs`:
/// only one becomes the indexer-lock owner, but every connected process
/// still serves edit tool calls, and an edit's post-write spawn is what
/// enqueues these jobs — see `edit.rs`). A second process starting up while
/// a live sibling is mid-refresh must not fail the sibling's real, still-
/// running job out from under it. Only a `running` row whose lease
/// (`mark_running`'s `lease_owner`/`lease_expires_at`) has actually expired
/// is treated as abandoned; a `running` row with a live lease is left
/// alone. A `queued` row has never been claimed (no lease was ever set for
/// it — `lease_expires_at IS NULL`), so it's always reconciled, same as the
/// pre-lease behavior: unlike a live `running` job, nothing here can prove
/// that trigger's own process is still alive, but it also hasn't yet done
/// any of the actual refresh work this function needs to avoid interrupting
/// (`retry_maintenance` remains the explicit escape hatch if that turns out
/// to be wrong).
pub fn reconcile_stale_at_startup(conn: &Connection) -> rusqlite::Result<Vec<MaintenanceJob>> {
    let now = now_epoch_secs();
    // Same row shape/parsing as `query_jobs`, but that helper takes no
    // parameters — this is the one caller that needs to bind `now` for the
    // lease-expiry comparison, so it's inlined rather than widening
    // `query_jobs`'s signature for a single use site.
    let stale = {
        let mut stmt = conn.prepare(
            "SELECT job_id, job_kind, state, triggered_by_tx_id, attempts, last_error, \
                 last_completed_at \
             FROM maintenance_jobs \
             WHERE state IN ('queued', 'running') \
                 AND (lease_expires_at IS NULL OR lease_expires_at < ?1) \
             ORDER BY available_at ASC",
        )?;
        let rows = stmt.query_map(params![now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<f64>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (job_id, kind_str, state_str, triggered_by_tx_id, attempts, last_error, last_completed_at) =
                row?;
            let (Some(kind), Some(state)) =
                (MaintenanceKind::parse(&kind_str), JobState::parse(&state_str))
            else {
                continue;
            };
            out.push(MaintenanceJob {
                job_id,
                kind,
                state,
                triggered_by_tx_id,
                attempts,
                last_error,
                last_completed_at,
            });
        }
        out
    };
    for job in &stale {
        conn.execute(
            "UPDATE maintenance_jobs \
             SET state = 'failed', last_completed_at = ?2, lease_owner = NULL, \
                 lease_expires_at = NULL, \
                 last_error = 'process restarted before this job completed (lease expired or \
                 never claimed) -- this process''s own startup indexing will run a fresh full \
                 pass if it becomes the indexer-lock owner' \
             WHERE dedupe_key = ?1 AND state IN ('queued', 'running') \
                 AND (lease_expires_at IS NULL OR lease_expires_at < ?3)",
            params![job.kind.as_str(), now, now],
        )?;
    }
    Ok(stale)
}

/// Explicit, human/agent-requested re-run (plan §4.7 `retry_maintenance`) —
/// unlike [`enqueue`], this forces `state` back to `queued` even from
/// `running`/`queued` (an explicit ask to retry overrides in-flight
/// coalescing; there is nothing else this row could be waiting on that
/// justifies silently ignoring the request). The caller is responsible for
/// actually spawning the refresh, exactly as at any other call site.
pub fn force_requeue(conn: &Connection, kind: MaintenanceKind) -> rusqlite::Result<()> {
    let now = now_epoch_secs();
    let changed = conn.execute(
        "UPDATE maintenance_jobs SET state = 'queued', attempts = 0, available_at = ?2, \
             last_error = NULL \
         WHERE dedupe_key = ?1",
        params![kind.as_str(), now],
    )?;
    if changed == 0 {
        // No row for this kind yet (never triggered once) -- same shape as
        // enqueue's insert branch, just unconditional.
        conn.execute(
            "INSERT INTO maintenance_jobs \
                 (job_id, job_kind, dedupe_key, state, attempts, available_at) \
             VALUES (?1, ?2, ?2, 'queued', 0, ?3)",
            params![job_id_for(kind), kind.as_str(), now],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_state_db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_state_db(&conn).unwrap();
        conn
    }

    #[test]
    fn enqueue_creates_a_queued_job_for_a_new_kind() {
        let conn = test_conn();
        let created = enqueue(&conn, MaintenanceKind::ScipRefresh, Some("TXN-a")).unwrap();
        assert!(created);

        let pending = pending_jobs(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, MaintenanceKind::ScipRefresh);
        assert_eq!(pending[0].state, JobState::Queued);
        assert_eq!(pending[0].triggered_by_tx_id.as_deref(), Some("TXN-a"));
    }

    #[test]
    fn enqueue_coalesces_when_a_job_is_already_queued() {
        let conn = test_conn();
        assert!(enqueue(&conn, MaintenanceKind::ScipRefresh, Some("TXN-a")).unwrap());
        // A second edit triggers the same kind before the first job ran --
        // must coalesce into the existing queued row, not create a second.
        assert!(!enqueue(&conn, MaintenanceKind::ScipRefresh, Some("TXN-b")).unwrap());

        let pending = pending_jobs(&conn).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "must still be exactly one queued scip_refresh job"
        );
    }

    #[test]
    fn enqueue_does_not_coalesce_across_different_kinds() {
        let conn = test_conn();
        assert!(enqueue(&conn, MaintenanceKind::ScipRefresh, None).unwrap());
        assert!(enqueue(&conn, MaintenanceKind::EmbedRefresh, None).unwrap());

        let pending = pending_jobs(&conn).unwrap();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn enqueue_after_completion_creates_a_fresh_job() {
        let conn = test_conn();
        assert!(enqueue(&conn, MaintenanceKind::ScipRefresh, None).unwrap());
        mark_running(&conn, MaintenanceKind::ScipRefresh).unwrap();
        mark_completed(&conn, MaintenanceKind::ScipRefresh, Ok(())).unwrap();
        assert!(pending_jobs(&conn).unwrap().is_empty());

        // A later edit triggers another refresh -- must be a genuinely new
        // job, not blocked by the old dedupe_key row (state != queued/running
        // anymore).
        assert!(enqueue(&conn, MaintenanceKind::ScipRefresh, None).unwrap());
        assert_eq!(pending_jobs(&conn).unwrap().len(), 1);
    }

    #[test]
    fn mark_completed_failure_is_recorded_and_visible() {
        let conn = test_conn();
        enqueue(&conn, MaintenanceKind::EmbedRefresh, None).unwrap();
        mark_running(&conn, MaintenanceKind::EmbedRefresh).unwrap();
        mark_completed(
            &conn,
            MaintenanceKind::EmbedRefresh,
            Err("model load failed"),
        )
        .unwrap();

        let last_error: Option<String> = conn
            .query_row(
                "SELECT last_error FROM maintenance_jobs WHERE dedupe_key = 'embed_refresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_error.as_deref(), Some("model load failed"));

        let state: String = conn
            .query_row(
                "SELECT state FROM maintenance_jobs WHERE dedupe_key = 'embed_refresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "failed",
            "a failed job stays queryable, not silently vanished"
        );
    }

    #[test]
    fn pending_jobs_finds_a_job_stuck_at_running_after_a_simulated_crash() {
        // Substitute for a real kill -9 between mark_running and
        // mark_completed (same rationale as txn.rs's interrupted-sequence
        // test): simply never call mark_completed. A fresh reader
        // (recovery after restart) must still see it as pending.
        let conn = test_conn();
        enqueue(&conn, MaintenanceKind::ScipRefresh, Some("TXN-crash")).unwrap();
        mark_running(&conn, MaintenanceKind::ScipRefresh).unwrap();

        let pending = pending_jobs(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, JobState::Running);
        assert_eq!(pending[0].attempts, 1);
    }

    #[test]
    fn all_jobs_includes_terminal_rows_pending_jobs_would_hide() {
        let conn = test_conn();
        enqueue(&conn, MaintenanceKind::ScipRefresh, None).unwrap();
        mark_running(&conn, MaintenanceKind::ScipRefresh).unwrap();
        mark_completed(&conn, MaintenanceKind::ScipRefresh, Ok(())).unwrap();
        enqueue(&conn, MaintenanceKind::EmbedRefresh, None).unwrap();

        assert!(
            pending_jobs(&conn).unwrap().len() == 1,
            "the done scip_refresh row must not show up as pending"
        );
        let all = all_jobs(&conn).unwrap();
        assert_eq!(
            all.len(),
            2,
            "all_jobs must surface both kinds regardless of state"
        );
        let scip = all
            .iter()
            .find(|j| j.kind == MaintenanceKind::ScipRefresh)
            .unwrap();
        assert_eq!(scip.state, JobState::Done);
        assert!(scip.last_completed_at.is_some());
        let embed = all
            .iter()
            .find(|j| j.kind == MaintenanceKind::EmbedRefresh)
            .unwrap();
        assert_eq!(embed.state, JobState::Queued);
        assert!(embed.last_completed_at.is_none());
    }

/// Backdates `dedupe_key`'s lease so it reads as already-expired, standing
    /// in for "enough wall-clock time passed since the crash that the lease
    /// this process claimed via `mark_running` is no longer current."
    fn expire_lease(conn: &Connection, kind: MaintenanceKind) {
        conn.execute(
            "UPDATE maintenance_jobs SET lease_expires_at = ?2 WHERE dedupe_key = ?1",
            params![kind.as_str(), now_epoch_secs() - 1.0],
        )
        .unwrap();
    }

    #[test]
    fn reconcile_stale_at_startup_fails_a_row_stuck_running_and_leaves_done_rows_alone() {
        let conn = test_conn();
        // Simulate a crash: a previous "process" enqueued+started scip_refresh
        // but never reached mark_completed, and enough time has passed since
        // that its mark_running lease has now expired.
        enqueue(&conn, MaintenanceKind::ScipRefresh, Some("TXN-crash")).unwrap();
        mark_running(&conn, MaintenanceKind::ScipRefresh).unwrap();
        expire_lease(&conn, MaintenanceKind::ScipRefresh);
        // A second kind completed cleanly before the crash -- must be untouched.
        enqueue(&conn, MaintenanceKind::EmbedRefresh, None).unwrap();
        mark_running(&conn, MaintenanceKind::EmbedRefresh).unwrap();
        mark_completed(&conn, MaintenanceKind::EmbedRefresh, Ok(())).unwrap();

        let reconciled = reconcile_stale_at_startup(&conn).unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].kind, MaintenanceKind::ScipRefresh);

        assert!(
            pending_jobs(&conn).unwrap().is_empty(),
            "the stuck-running row must no longer read as pending after reconciliation"
        );
        let all = all_jobs(&conn).unwrap();
        let scip = all
            .iter()
            .find(|j| j.kind == MaintenanceKind::ScipRefresh)
            .unwrap();
        assert_eq!(scip.state, JobState::Failed);
        assert!(
            scip.last_error
                .as_deref()
                .unwrap_or("")
                .contains("restarted")
        );
        let embed = all
            .iter()
            .find(|j| j.kind == MaintenanceKind::EmbedRefresh)
            .unwrap();
        assert_eq!(
            embed.state,
            JobState::Done,
            "a cleanly completed job must not be touched by startup reconciliation"
        );
    }

    #[test]
    fn reconcile_stale_at_startup_is_a_noop_on_a_fresh_db() {
        let conn = test_conn();
        assert!(reconcile_stale_at_startup(&conn).unwrap().is_empty());
        assert!(all_jobs(&conn).unwrap().is_empty());
    }

    /// Audit 3.4's core scenario: a second CALM process for the same project
    /// starts up (calling this) while a live sibling process is genuinely
    /// still mid-refresh. Before the lease, EVERY `running` row was
    /// unconditionally failed here regardless of whether the process that
    /// started it was still alive -- this must no longer happen for a row
    /// whose lease hasn't expired yet.
    #[test]
    fn reconcile_stale_at_startup_leaves_a_live_siblings_running_job_alone() {
        let conn = test_conn();
        enqueue(&conn, MaintenanceKind::ScipRefresh, Some("TXN-live")).unwrap();
        mark_running(&conn, MaintenanceKind::ScipRefresh).unwrap();
        // No expire_lease() call -- the lease `mark_running` just claimed is
        // still fresh, standing in for "the process that owns it hasn't
        // crashed, it's genuinely still running".

        let reconciled = reconcile_stale_at_startup(&conn).unwrap();
        assert!(
            reconciled.is_empty(),
            "a running job with an unexpired lease must not be reconciled: {reconciled:?}"
        );

        let all = all_jobs(&conn).unwrap();
        let scip = all
            .iter()
            .find(|j| j.kind == MaintenanceKind::ScipRefresh)
            .unwrap();
        assert_eq!(
            scip.state,
            JobState::Running,
            "a live sibling's job must still read as running, not falsely failed"
        );
    }

    /// Audit 3.3 (the embed-mutex race, see `mark_completed`'s doc comment):
    /// a second, later, genuinely real completion for the SAME kind must
    /// still be recorded even though an earlier completion already flipped
    /// the row to a terminal state -- the old `WHERE state IN
    /// ('queued','running')` guard would have silently dropped it.
    #[test]
    fn mark_completed_is_not_silently_dropped_by_an_earlier_racing_completion() {
        let conn = test_conn();
        enqueue(&conn, MaintenanceKind::EmbedRefresh, None).unwrap();
        mark_running(&conn, MaintenanceKind::EmbedRefresh).unwrap();
        // First (earlier) completer finishes -- state becomes 'done'.
        mark_completed(&conn, MaintenanceKind::EmbedRefresh, Ok(())).unwrap();
        let first_completed_at: f64 = conn
            .query_row(
                "SELECT last_completed_at FROM maintenance_jobs WHERE dedupe_key = 'embed_refresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // A second, later thread -- e.g. one that was genuinely still
        // waiting on EMBED_BG's mutex and only now finished its own real
        // embed_pending pass -- reports its own completion. Must not
        // silently no-op just because the row already reads 'done'.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let result = mark_completed(&conn, MaintenanceKind::EmbedRefresh, Err("second pass failed"));
        assert!(result.is_ok());

        let (state, last_error, last_completed_at): (String, Option<String>, f64) = conn
            .query_row(
                "SELECT state, last_error, last_completed_at FROM maintenance_jobs \
                 WHERE dedupe_key = 'embed_refresh'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            state, "failed",
            "the second, later completion must overwrite the first, not be dropped"
        );
        assert_eq!(last_error.as_deref(), Some("second pass failed"));
        assert!(
            last_completed_at > first_completed_at,
            "the recorded completion time must reflect the LATER real completion"
        );
    }

    #[test]
    fn force_requeue_overrides_an_in_flight_job() {
        let conn = test_conn();
        enqueue(&conn, MaintenanceKind::ScipRefresh, None).unwrap();
        mark_running(&conn, MaintenanceKind::ScipRefresh).unwrap();
        // enqueue() would refuse to touch a running row -- force_requeue must not.
        assert!(!enqueue(&conn, MaintenanceKind::ScipRefresh, None).unwrap());

        force_requeue(&conn, MaintenanceKind::ScipRefresh).unwrap();
        let pending = pending_jobs(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, JobState::Queued);
        assert_eq!(pending[0].attempts, 0);
    }

    #[test]
    fn force_requeue_creates_a_row_when_none_existed_yet() {
        let conn = test_conn();
        force_requeue(&conn, MaintenanceKind::EmbedRefresh).unwrap();
        let pending = pending_jobs(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, MaintenanceKind::EmbedRefresh);
        assert_eq!(pending[0].state, JobState::Queued);
    }
}
