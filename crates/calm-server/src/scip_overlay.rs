//! Runs all 9 language-specific SCIP confidence-upgrade overlays (rust, go,
//! python, js, java, csharp, php, clang, ruby) concurrently instead of
//! sequentially. Shared by both places that used to inline this as ~8 (now
//! 9) near-identical sequential blocks: the startup indexer in `lib.rs` and
//! the incremental-reindex watcher loop in `watcher.rs`.
//!
//! Why parallel is safe here even though every branch touches the same
//! database: the expensive part of each branch is spawning + polling an
//! external per-language indexer (rust-analyzer/gopls/scip-python/...) via
//! `scip::runner::run_indexer`, which needs no DB access at all while it
//! runs — only the final `ingest_occurrences` write at the very end needs a
//! connection, and each thread opens its own (`open_writer`), so SQLite's
//! own WAL + `busy_timeout` — not an in-process lock — serializes those
//! brief writes exactly like any other two writers already have to. Each
//! provider's `run_overlay_for` also checks `provider_has_any_files` before
//! doing anything else, so an irrelevant language's branch returns near-
//! instantly without ever really costing a thread's worth of work — the
//! common case (a project using 1-2 languages) barely changes; it's
//! multi-language monorepos (this repo included, via its own multi-lang
//! test fixtures) where several branches do real, slow, independent work
//! that this actually parallelizes.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_core::scip::ingest::IngestStats;

/// Coalescing gate for `run_all_coalesced` — one overlay pass in flight at a
/// time process-wide, with at most one queued rerun. Without this, rapid
/// successive triggers (e.g. an agent making several `edit_lines` calls in a
/// row) would stack concurrent rust-analyzer batch runs, each pegging CPU
/// for ~20s+ on a repo this size.
static OVERLAY_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static OVERLAY_RERUN: AtomicBool = AtomicBool::new(false);

/// `run_all`, but concurrent callers coalesce: if a pass is already running,
/// flag a single rerun (so edges from the newest reindex still get covered
/// once the current pass finishes) and return immediately. Used by every
/// *incremental* trigger — the watcher loop and the edit tools' post-write
/// reindex — while startup (`lib.rs`) keeps calling `run_all` directly (a
/// single, naturally serialized call).
pub fn run_all_coalesced(root: &Path, db_path: &Path) {
    if OVERLAY_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        OVERLAY_RERUN.store(true, Ordering::Release);
        return;
    }
    loop {
        OVERLAY_RERUN.store(false, Ordering::Release);
        run_all(root, db_path);
        if !OVERLAY_RERUN.load(Ordering::Acquire) {
            break;
        }
    }
    OVERLAY_IN_FLIGHT.store(false, Ordering::Release);
    // Close the lost-wakeup window: a rerun flagged between the final load
    // above and the in-flight release would otherwise be dropped silently.
    if OVERLAY_RERUN.swap(false, Ordering::AcqRel) {
        run_all_coalesced(root, db_path);
    }
}

/// Runs the rust + go + python + js + java + csharp + php + clang SCIP
/// overlays concurrently against `db_path`, each on its own DB connection.
/// `root` is the project root to scan. Logs a per-language summary exactly
/// like the old sequential version did. Best-effort throughout: a failure in
/// one language's overlay (or its own DB connection) is logged and does not
/// affect any other language's run.
pub fn run_all(root: &Path, db_path: &Path) {
    // Loaded once up front, not once per language via 8 separate concurrent
    // load_config(...).unwrap_or_default() calls -- each independently
    // re-read and re-parsed config.json, silently discarding a malformed
    // config 8x over. `Config`'s fields are read-only from here on, so a
    // shared `&config.<lang>` borrow into each std::thread::scope closure
    // is sound without cloning.
    let config = calm_core::config::load_config_or_warn(root);
    std::thread::scope(|s| {
        s.spawn(|| {
            run_one("rust", db_path, |conn| {
                let dirty = calm_core::scip::rust_source_dirty_keys(conn);
                let stats = calm_core::scip::run_overlay(conn, root, &config.rust, &dirty)?;
                // caller_count was computed by the reindex that ran before this
                // overlay pass, before the overlay flipped
                // edge_confidence/ruled_out_by_scip on (or inserted) some edges —
                // refresh it or it goes stale again immediately relative to the
                // columns it's filtered on. The other 7 languages' `_and_log`
                // helpers already do this internally (via `run_and_refresh`);
                // `run_overlay` is the one raw entry point that doesn't, so it's
                // done here instead.
                if (stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0)
                    && let Err(e) = calm_core::indexer::pipeline::refresh_caller_counts(conn)
                {
                    tracing::warn!("caller_count refresh after SCIP overlay (rust) failed: {e}");
                }
                Ok(stats)
            });
        });
        s.spawn(|| {
            run_one("go", db_path, |conn| {
                calm_core::scip::run_go_overlay_and_log(conn, root, &config.go)
            });
        });
        s.spawn(|| {
            run_one("python", db_path, |conn| {
                calm_core::scip::run_python_overlay_and_log(conn, root, &config.python)
            });
        });
        s.spawn(|| {
            run_one("js", db_path, |conn| {
                calm_core::scip::run_js_overlay_and_log(conn, root, &config.js)
            });
        });
        s.spawn(|| {
            run_one("java", db_path, |conn| {
                calm_core::scip::run_java_overlay_and_log(conn, root, &config.java)
            });
        });
        s.spawn(|| {
            run_one("csharp", db_path, |conn| {
                calm_core::scip::run_csharp_overlay_and_log(conn, root, &config.csharp)
            });
        });
        s.spawn(|| {
            run_one("php", db_path, |conn| {
                calm_core::scip::run_php_overlay_and_log(conn, root, &config.php)
            });
        });
        s.spawn(|| {
            run_one("c", db_path, |conn| {
                calm_core::scip::run_clang_overlay_and_log(conn, root, &config.clang)
            });
        });
        s.spawn(|| {
            run_one("ruby", db_path, |conn| {
                calm_core::scip::run_ruby_overlay_and_log(conn, root, &config.ruby)
            });
        });
    });
}

/// Opens its own connection against `db_path`, runs `run` against it, and
/// logs the outcome the same way every one of the old sequential call sites
/// did — a fresh connection per call (not a shared one across threads)
/// because `rusqlite::Connection` isn't `Sync`, and because SQLite's own
/// WAL + busy_timeout already handle the resulting extra-connection write
/// contention (see `crates/calm-core/src/db/conn.rs`'s `WRITER_BUSY_TIMEOUT`).
fn run_one(
    lang: &str,
    db_path: &Path,
    run: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<IngestStats>,
) {
    let conn = match calm_core::db::conn::open_writer(db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("SCIP overlay ({lang}): failed to open DB connection: {e}");
            return;
        }
    };
    match run(&conn) {
        Ok(stats) if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 => {
            tracing::info!(
                "SCIP overlay ({lang}): {} edges upgraded to formal, {} fan-out siblings ruled out, {} inserted",
                stats.upgraded,
                stats.ruled_out,
                stats.inserted
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("SCIP overlay ({lang}) error (base graph intact): {e}"),
    }
}
