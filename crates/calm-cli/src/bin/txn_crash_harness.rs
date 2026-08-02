//! Standalone subprocess for WS-1 task 4.8's crash-injection suite
//! (docs/plans/2026-08-02-phase1-p0-execution-plan.md §6,
//! crates/calm-cli/tests/txn_crash_injection.rs is the driver). NOT shipped
//! — not part of `release.yml`'s packaged `--bin calm` build, it's just
//! another `[[bin]]` this workspace happens to build, the same way
//! `sigterm_shutdown.rs` already relies on `CARGO_BIN_EXE_calm` for a real
//! subprocess rather than an in-process call.
//!
//! Performs ONE real `txn.rs` begin/advance/atomic_write cycle against a
//! real on-disk DB + file, then raises SIGKILL on itself immediately after
//! completing whichever step `--crash-after` names — never a normal exit at
//! that point, so no `Drop`/cleanup code runs, which is the actual crash
//! this suite needs to inject. Exits 0 normally if `--crash-after` is
//! absent or doesn't match any step (the "no crash" control run).
//!
//! Deliberately does NOT drive the real `edit_lines_impl_gated`/reindex
//! pipeline — what this suite verifies is durability of the
//! `edit_transactions`/`tx_events` journal itself (disk never changes
//! without a corresponding `tx_events` row; `replay_state` always matches
//! the `state` cache column) under a real OS kill, independent of reindex's
//! own correctness, which is already covered elsewhere by the normal test
//! suite. `IndexCommitted` is reached via a simulated "index refreshed"
//! advance rather than a real reindex, for exactly that reason.

use std::path::PathBuf;

fn main() {
    let mut db_path: Option<PathBuf> = None;
    let mut file_path: Option<PathBuf> = None;
    let mut crash_after: Option<String> = None;
    let mut new_content = String::from("new content\n");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db_path = args.next().map(PathBuf::from),
            "--file" => file_path = args.next().map(PathBuf::from),
            "--crash-after" => crash_after = args.next(),
            "--new-content" => new_content = args.next().unwrap_or(new_content),
            other => {
                eprintln!("txn_crash_harness: unknown arg {other}");
                std::process::exit(2);
            }
        }
    }
    let db_path = db_path.expect("--db required");
    let file_path = file_path.expect("--file required");

    // SIGKILL, not `std::process::exit` -- `exit` runs libc atexit handlers
    // and lets Rust's own process-exit path flush/close things, which would
    // mask exactly the crash this suite exists to inject. SIGKILL is
    // uncatchable and unblockable: a true hard stop, indistinguishable from
    // the OS killing this process for any other reason mid-syscall.
    let crash_here = |step: &str| {
        if crash_after.as_deref() == Some(step) {
            unsafe {
                libc::raise(libc::SIGKILL);
            }
            unreachable!("SIGKILL just raised on self");
        }
    };

    let original = std::fs::read_to_string(&file_path).unwrap_or_default();

    let conn = calm_core::db::conn::open_writer(&db_path).expect("open db");
    calm_core::db::schema::init_db(&conn).expect("init db");

    let tx = calm_core::txn::begin(
        &conn,
        "crash-harness-project",
        file_path.file_name().unwrap().to_str().unwrap(),
        &calm_core::digest::evidence_digest(original.as_bytes()),
        &calm_core::digest::evidence_digest(new_content.as_bytes()),
    )
    .expect("txn::begin");
    crash_here("prepared");

    calm_core::edit::atomic_write(&file_path, &new_content).expect("atomic_write");
    calm_core::txn::advance(
        &conn,
        &tx.tx_id,
        calm_core::txn::TxState::FileCommitted,
        "system",
        "atomic_write succeeded",
    )
    .expect("advance FileCommitted");
    crash_here("file_committed");

    calm_core::txn::advance(
        &conn,
        &tx.tx_id,
        calm_core::txn::TxState::IndexCommitted,
        "system",
        "index refreshed (simulated -- see module doc)",
    )
    .expect("advance IndexCommitted");
    crash_here("index_committed");

    calm_core::txn::advance(
        &conn,
        &tx.tx_id,
        calm_core::txn::TxState::Done,
        "system",
        "complete",
    )
    .expect("advance Done");

    println!("{}", tx.tx_id);
}
