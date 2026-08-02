//! WS-1 task 4.8 crash-injection suite
//! (docs/plans/2026-08-02-phase1-p0-execution-plan.md §6, milestone gate
//! "Write-Safety Beta" item 1: "Crash-injection suite (kill -9 tại mọi giá
//! trị TxState) chạy ≥100 lần mỗi transition ... 0 trường hợp disk thay đổi
//! mà không có tx_events row tương ứng").
//!
//! Drives `crates/calm-cli/src/bin/txn_crash_harness.rs` as a real
//! subprocess and sends it a real, deterministic SIGKILL (via the
//! subprocess raising it on itself right after completing a named step --
//! see that binary's module doc for why self-raise beats an external
//! timed `kill`) at each of the 3 state transitions Phase 1's real code
//! path actually reaches: `PREPARED` (after `txn::begin`, before
//! `atomic_write`), `FILE_COMMITTED` (after `atomic_write` + that advance,
//! before reindex), `INDEX_COMMITTED` (after the index-refresh advance,
//! before `Done`). `VerifyPending`/`RolledBack` are not exercised here:
//! neither is ever produced by any real Phase 1 caller (`VerifyPending`
//! has no producer until WS-6; `RolledBack` is a legal transition target
//! `advance` accepts but `edit_lines_impl_gated` never requests it, always
//! using `Failed` on error) -- injecting a crash at a transition nothing
//! can reach would test the harness, not the product.
//!
//! For each crash point this verifies, on a fresh connection opened after
//! the subprocess died:
//! - the subprocess was actually killed by SIGKILL (sanity: proves the
//!   crash point was reached, not skipped or completed past silently),
//! - disk state matches exactly what the crash point implies (unchanged
//!   before `FILE_COMMITTED` is durable, new content from `FILE_COMMITTED`
//!   onward -- `atomic_write`'s rename is what makes this binary, no
//!   partially-written file is ever observable either way),
//! - `edit_transactions.state` (the cache) exactly equals
//!   `txn::replay_state` (derived purely from `tx_events`) -- the core
//!   "cache never drifts from the log" invariant even under a real kill,
//!   not just the graceful-path tests `txn.rs`'s own unit tests already
//!   cover,
//! - `txn::recover_incomplete` finds this transaction (every crash point
//!   here lands on a non-terminal state) -- ties this suite's evidence
//!   directly to the startup-recovery hook `common.rs::new_with_preset`
//!   already wires in, proving it would actually notice this exact crash
//!   on next startup, not just that the DB rows happen to look right in
//!   isolation.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

const ORIGINAL_CONTENT: &str = "original content\n";
const NEW_CONTENT: &str = "new content\n";

struct CrashOutcome {
    tx_id: Option<String>,
    disk_content: String,
}

/// Keyed by step name AND iteration, not iteration alone -- a bare
/// `index-{iteration}.db` collided across different `step`s sharing the
/// same iteration number, silently reusing a DB file (and therefore an
/// `edit_transactions` row) left behind by an earlier crash point's run
/// against that same iteration. Caught live: `LIMIT 1` with no `ORDER BY`
/// picked the OLDEST row in the shared file, so a `file_committed` run's
/// assertions were checking a stale `prepared`-only tx instead of the one
/// this run actually just created.
fn db_path_for(dir: &Path, run_key: &str) -> std::path::PathBuf {
    dir.join(format!("index-{run_key}.db"))
}

/// Spawns one `txn_crash_harness` subprocess with `--crash-after step` (or
/// no crash at all if `step` is `None`), waits for it to die, and returns
/// what's observable afterward. Panics if a crash was requested but the
/// process didn't actually die by SIGKILL -- that would mean this test
/// stopped testing what it claims to.
fn run_one(dir: &Path, run_key: &str, step: Option<&str>) -> CrashOutcome {
    let db_path = db_path_for(dir, run_key);
    let file_path = dir.join(format!("a-{run_key}.txt"));
    std::fs::write(&file_path, ORIGINAL_CONTENT).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_txn_crash_harness"));
    cmd.arg("--db")
        .arg(&db_path)
        .arg("--file")
        .arg(&file_path)
        .arg("--new-content")
        .arg(NEW_CONTENT);
    if let Some(step) = step {
        cmd.arg("--crash-after").arg(step);
    }
    let output = cmd.output().expect("spawn txn_crash_harness");

    if let Some(step) = step {
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "run {run_key} step {step:?}: harness must have been killed by SIGKILL, not \
             exited normally (stdout={:?} stderr={:?}) -- otherwise this run tested nothing",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    } else {
        assert!(
            output.status.success(),
            "control run (no crash) must exit 0: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let disk_content = std::fs::read_to_string(&file_path).unwrap_or_default();
    let tx_id = if step.is_some() {
        // The harness only prints tx_id on the no-crash success path; for a
        // crash run, recover it from the DB directly the way a real
        // recovering process would (there's exactly one transaction ever
        // created against this DB, now that db_path is unique per run_key).
        let conn = calm_core::db::conn::open_writer(&db_path).ok();
        conn.and_then(|c| {
            c.query_row("SELECT tx_id FROM edit_transactions LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
        })
    } else {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };

    CrashOutcome {
        tx_id,
        disk_content,
    }
}

/// Verifies the invariants this whole suite exists to check, for one
/// already-crashed run. `expect_disk_new_content` is `true` from
/// `FILE_COMMITTED` onward (the point `atomic_write`'s rename made
/// durable), `false` only for the `PREPARED` crash point.
fn assert_journal_consistent(
    dir: &Path,
    run_key: &str,
    step: &str,
    outcome: &CrashOutcome,
    expect_disk_new_content: bool,
    expect_state: calm_core::txn::TxState,
) {
    let tx_id = outcome
        .tx_id
        .as_ref()
        .unwrap_or_else(|| panic!("run {run_key} step {step}: no tx_id recovered"));

    assert_eq!(
        outcome.disk_content,
        if expect_disk_new_content {
            NEW_CONTENT
        } else {
            ORIGINAL_CONTENT
        },
        "run {run_key} step {step}: disk content must match exactly what this crash point \
         implies -- atomic_write's rename is the only thing that can change it, and it's \
         synchronous, not partial"
    );

    let db_path = db_path_for(dir, run_key);
    let conn = calm_core::db::conn::open_writer(&db_path).expect("reopen db after crash");

    let cached = calm_core::txn::get(&conn, tx_id)
        .expect("txn::get")
        .unwrap_or_else(|| panic!("run {run_key} step {step}: tx {tx_id} not found"));
    assert_eq!(
        cached.state, expect_state,
        "run {run_key} step {step}: cached edit_transactions.state must match the crash point"
    );

    let replayed = calm_core::txn::replay_state(&conn, tx_id).expect("replay_state");
    assert_eq!(
        replayed, cached.state,
        "run {run_key} step {step}: replay_state (derived purely from tx_events) must never \
         drift from the edit_transactions.state cache, even under a real kill -- a mismatch \
         here would mean the journal itself is untrustworthy"
    );

    if expect_disk_new_content {
        // Every crash point past PREPARED changed the disk -- the core
        // claim this whole suite verifies: that change is never observable
        // without a corresponding tx_events row explaining it. `replay_state`
        // succeeding at all above already proves at least one row exists;
        // this asserts it's specifically the FILE_COMMITTED transition, not
        // some earlier or unrelated one.
        let mut stmt = conn
            .prepare(
                "SELECT COUNT(*) FROM tx_events WHERE tx_id = ?1 AND to_state = 'FILE_COMMITTED'",
            )
            .unwrap();
        let count: i64 = stmt.query_row([tx_id], |r| r.get(0)).unwrap();
        assert_eq!(
            count, 1,
            "run {run_key} step {step}: disk changed but no FILE_COMMITTED tx_events row \
             explains it"
        );
    }

    let incomplete = calm_core::txn::recover_incomplete(&conn).expect("recover_incomplete");
    assert!(
        incomplete.iter().any(|t| &t.tx_id == tx_id),
        "run {run_key} step {step}: recover_incomplete must find this crashed, non-terminal \
         transaction -- this is exactly what common.rs::new_with_preset's startup hook calls \
         on every launch, so this proves that hook would actually notice this crash, not just \
         that the DB rows look right in isolation"
    );
}

/// Runs `iterations` crash-and-verify cycles for one step name.
fn run_crash_point(dir: &Path, step: &str, expect_disk_new_content: bool, iterations: usize) {
    use calm_core::txn::TxState;
    let expect_state = match step {
        "prepared" => TxState::Prepared,
        "file_committed" => TxState::FileCommitted,
        "index_committed" => TxState::IndexCommitted,
        other => panic!("unknown step {other}"),
    };
    for i in 0..iterations {
        let run_key = format!("{step}-{i}");
        let outcome = run_one(dir, &run_key, Some(step));
        assert_journal_consistent(
            dir,
            &run_key,
            step,
            &outcome,
            expect_disk_new_content,
            expect_state,
        );
    }
}

/// Fast variant, always part of the normal test suite -- routine
/// regression coverage for the mechanism itself (does self-raise SIGKILL
/// at each step actually land where expected, are the invariants correct
/// at all), without paying the milestone's literal ≥100-iteration cost on
/// every `cargo test --workspace`.
#[test]
fn txn_journal_survives_kill_at_every_reachable_transition() {
    let dir = tempfile::tempdir().unwrap();
    run_crash_point(dir.path(), "prepared", false, 5);
    run_crash_point(dir.path(), "file_committed", true, 5);
    run_crash_point(dir.path(), "index_committed", true, 5);
}

/// Milestone-literal variant (execution plan §6 item 1: "≥100 lần mỗi
/// transition"). `#[ignore]`d by default -- 300 real subprocess spawns is
/// too slow to pay on every routine `cargo test --workspace`; run
/// explicitly via `cargo test --test txn_crash_injection -- --ignored`, or
/// wire into a dedicated (nightly/milestone-gate) CI job per the plan
/// doc's own framing of this as a later checkpoint, not routine PR CI.
#[test]
#[ignore]
fn txn_journal_survives_kill_at_every_reachable_transition_stress_100x() {
    const ITERATIONS: usize = 100;
    let dir = tempfile::tempdir().unwrap();
    run_crash_point(dir.path(), "prepared", false, ITERATIONS);
    run_crash_point(dir.path(), "file_committed", true, ITERATIONS);
    run_crash_point(dir.path(), "index_committed", true, ITERATIONS);
}
