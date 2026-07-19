//! File watcher that drives incremental reindexing while `calm serve` runs.
//!
//! Watches the project tree and, after a debounce window, re-parses only the
//! files that changed. Events under `.calm/` (where the DB lives) and other
//! non-source paths are ignored — otherwise the index's own writes would trigger
//! an endless reindex loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher, recommended_watcher};
use tokio_util::sync::CancellationToken;

use crate::tools::common::RwLockExt;

/// Quiet period after the last event before a reindex fires.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Directory names whose subtrees never warrant a reindex.
const IGNORE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    "venv",
];

/// A path is relevant when it is a tier-0 source file and no path component is
/// dot-prefixed (e.g. `.git`, `.calm`) or an ignored build directory.
fn is_relevant_path(path: &Path) -> bool {
    for comp in path.components() {
        if let std::path::Component::Normal(os) = comp
            && let Some(s) = os.to_str()
            && (s.starts_with('.') || IGNORE_DIRS.contains(&s))
        {
            return false;
        }
    }
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(calm_core::indexer::lang_constants::language_for_extension)
        .is_some()
}

fn event_is_relevant(res: &notify::Result<notify::Event>) -> bool {
    // `need_rescan()` is `notify`'s own signal that the OS watch backend may
    // have dropped events (e.g. the kernel's inotify queue overflowed under
    // heavy I/O) — path-matching against `is_relevant_path` doesn't apply to
    // a "something changed, we don't know what" notice, so treat it as
    // relevant unconditionally. This is what lets a missed add/delete event
    // still get picked up: the very next tick's reindex is a full-tree walk
    // regardless of which event triggered it (see `run_watch_loop`), so
    // reacting to the rescan flag heals a dropped event without needing to
    // know which file it was about.
    matches!(res, Ok(event) if event.need_rescan() || event.paths.iter().any(|p| is_relevant_path(p)))
}

/// True when `path` is one of the fixed coverage-report locations
/// `calm_core::analysis::coverage::load_coverage` looks for (`lcov.info`,
/// `coverage.xml`, `.coverage`, etc.) — checked by exact path match rather
/// than folded into `is_relevant_path`, since some of these (`.coverage`,
/// `.nyc_output/lcov.info`) are legitimately dot-prefixed and would
/// otherwise be rejected by that function's VCS/build-dir filtering.
fn is_coverage_path(path: &Path, project_root: &Path) -> bool {
    calm_core::analysis::coverage::COVERAGE_SEARCH_PATHS
        .iter()
        .any(|&(relative, _)| path == project_root.join(relative))
}

fn event_touches_coverage(res: &notify::Result<notify::Event>, project_root: &Path) -> bool {
    matches!(res, Ok(event) if event.paths.iter().any(|p| is_coverage_path(p, project_root)))
}

/// Block on the watch loop until `ct` is cancelled. Intended to run inside a
/// `spawn_blocking` task after the initial full index completes. When an embedder
/// is loaded, newly (re)indexed symbols are embedded after each reindex.
/// Block on the watch loop until `ct` is cancelled. Intended to run inside a
/// `spawn_blocking` task after the initial full index completes. When an embedder
/// is loaded, newly (re)indexed symbols are embedded after each reindex.
pub fn run_watch_loop(
    project_root: PathBuf,
    db_path: PathBuf,
    ct: CancellationToken,
    embedder: crate::EmbedderHandle,
    coverage: crate::CoverageHandle,
    // Phase B T6.5: shared slot indexing_status reads to report which
    // rebuild path the most recent reindex took.
    graph_mode: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    // Fired once the OS-level watch is actually armed (right after
    // `watcher.watch` returns `Ok`), so a caller that needs to know the
    // watch is live before mutating the tree (namely this module's own
    // integration tests) can wait on a real signal instead of guessing a
    // sleep duration long enough to outlast this thread's own scheduling
    // delay under load. `None` in production (`bootstrap` below) — nothing
    // there needs to block on watch-armed.
    ready: Option<std::sync::mpsc::Sender<()>>,
) {
    let (tx, rx) = mpsc::channel();
    let mut watcher = match recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("File watcher init failed: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&project_root, RecursiveMode::Recursive) {
        tracing::error!(
            "File watcher could not watch {}: {e}",
            project_root.display()
        );
        return;
    }
    if let Some(ready) = ready {
        let _ = ready.send(());
    }
    tracing::info!("File watcher active on {}", project_root.display());
    // Checked between reindex parse batches too (not just once per loop
    // iteration here) — a single `reindex_changed` call on a large changed
    // set (e.g. a git branch switch touching thousands of files) can itself
    // run long enough to matter during shutdown, even though this outer loop
    // already re-checks `ct` every ~1s between events.
    let cancel = || ct.is_cancelled();

    loop {
        if ct.is_cancelled() {
            break;
        }
        // Wait for the next *relevant* event: either a tier-0 source file (see
        // `is_relevant_path`) or one of the fixed coverage-report locations
        // (see `is_coverage_path`). Irrelevant noise (DB/WAL writes under
        // .calm, editor temp files) is dropped here so it can neither trigger
        // nor — critically — delay a reindex/coverage-reload.
        let mut coverage_touched;
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(res) => {
                let source_relevant = event_is_relevant(&res);
                coverage_touched = event_touches_coverage(&res, &project_root);
                if !source_relevant && !coverage_touched {
                    continue;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Debounce: reindex/reload once the tree has been quiet (no relevant events) for
        // DEBOUNCE. Only relevant events extend the window — a steady stream of
        // irrelevant events must not starve it.
        let mut deadline = std::time::Instant::now() + DEBOUNCE;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(res) => {
                    if ct.is_cancelled() {
                        return;
                    }
                    let source_relevant = event_is_relevant(&res);
                    if event_touches_coverage(&res, &project_root) {
                        coverage_touched = true;
                    }
                    if source_relevant || coverage_touched {
                        deadline = std::time::Instant::now() + DEBOUNCE;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if coverage_touched {
            let reloaded = calm_core::analysis::coverage::load_coverage(&project_root);
            tracing::info!("Coverage file changed — reloaded ({})", reloaded.source);
            *coverage.write_ok() = reloaded;
        }

        match calm_core::db::conn::open_writer(&db_path) {
            Ok(mut conn) => {
                match calm_core::indexer::pipeline::reindex_changed_cancellable(
                    &mut conn,
                    &project_root,
                    &cancel,
                ) {
                    Ok(calm_core::indexer::pipeline::ReindexOutcome::Completed(s))
                        if !s.is_noop() =>
                    {
                        let mode = s.graph_mode.label();
                        *graph_mode.write_ok() = Some(mode.clone());
                        tracing::info!(
                            graph_mode = %mode,
                            "Incremental reindex: {} changed, {} deleted",
                            s.changed,
                            s.deleted
                        );
                        // Embed any symbols/chunks added by this reindex (Layer 1 +
                        // Layer 2 — see indexer::chunker for the latter).
                        if let Some(model) = embedder.read_ok().clone() {
                            if let Err(e) =
                                calm_core::embedding::embed_pending(&conn, model.as_ref())
                            {
                                tracing::error!("Incremental embedding failed: {e}");
                            }
                            if let Err(e) =
                                calm_core::embedding::embed_pending_chunks(&conn, model.as_ref())
                            {
                                tracing::error!("Incremental chunk embedding failed: {e}");
                            }
                        }
                        // Re-run the SCIP confidence-upgrade overlays so call
                        // edges touched by *this* reindex don't stay stuck below
                        // `formal` for the rest of a long-running session — before
                        // this, overlays only ever ran once at server startup (see
                        // `lib.rs`), so a file added/edited afterward never got
                        // upgraded until the next restart. Cheap when nothing
                        // relevant to a given language actually changed: each
                        // provider's own cache key skips re-invoking its external
                        // tool in that case — see `scip_overlay`'s doc comment for
                        // why running all 8 concurrently (rather than the
                        // sequential loop this used to be) is safe.
                        #[cfg(feature = "scip-overlay")]
                        if !cancel() {
                            // Drop this thread's write connection before fanning
                            // out — the 8 language overlays below each open their
                            // own instead.
                            drop(conn);
                            crate::scip_overlay::run_all_coalesced(&project_root, &db_path);
                        }
                    }
                    Ok(calm_core::indexer::pipeline::ReindexOutcome::Completed(_)) => {}
                    Ok(calm_core::indexer::pipeline::ReindexOutcome::Cancelled) => break,
                    Err(e) => tracing::error!("Incremental reindex failed: {e}"),
                }
            }
            Err(e) => tracing::error!("File watcher could not open DB: {e}"),
        }
    }
    tracing::info!("File watcher stopped");
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relevant_paths() {
        assert!(is_relevant_path(Path::new("src/main.rs")));
        assert!(is_relevant_path(Path::new("pkg/app.py")));
        // DB writes and VCS noise must be ignored to avoid reindex loops.
        assert!(!is_relevant_path(Path::new(".calm/index.db")));
        assert!(!is_relevant_path(Path::new("proj/.git/index")));
        assert!(!is_relevant_path(Path::new("target/debug/foo.rs")));
        // Markdown headings are indexed (see `extract_markdown_symbols`),
        // so a README edit must trigger a reindex like any source file.
        assert!(is_relevant_path(Path::new("README.md")));
        // Genuinely non-indexed extensions are still ignored.
        assert!(!is_relevant_path(Path::new("NOTES.txt")));
    }
    #[test]
    fn coverage_paths_matched_by_exact_relative_location() {
        let root = Path::new("/proj");
        assert!(is_coverage_path(Path::new("/proj/lcov.info"), root));
        assert!(is_coverage_path(
            Path::new("/proj/coverage/lcov.info"),
            root
        ));
        // `.coverage`/`.nyc_output` are legitimately dot-prefixed — must still
        // count, unlike `is_relevant_path`'s dot-dir rejection.
        assert!(is_coverage_path(Path::new("/proj/.coverage"), root));
        assert!(is_coverage_path(
            Path::new("/proj/.nyc_output/lcov.info"),
            root
        ));
        assert!(is_coverage_path(Path::new("/proj/coverage.xml"), root));
        assert!(is_coverage_path(Path::new("/proj/coverage.out"), root));
        // Unrelated files, and files under a different root, must not match.
        assert!(!is_coverage_path(Path::new("/proj/src/main.rs"), root));
        assert!(!is_coverage_path(Path::new("/proj/nested/lcov.info"), root));
        assert!(!is_coverage_path(Path::new("/other/lcov.info"), root));
    }
}
