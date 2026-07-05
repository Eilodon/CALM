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
    matches!(res, Ok(event) if event.paths.iter().any(|p| is_relevant_path(p)))
}

/// Block on the watch loop until `ct` is cancelled. Intended to run inside a
/// `spawn_blocking` task after the initial full index completes. When an embedder
/// is loaded, newly (re)indexed symbols are embedded after each reindex.
pub fn run_watch_loop(
    project_root: PathBuf,
    db_path: PathBuf,
    ct: CancellationToken,
    embedder: crate::EmbedderHandle,
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
    tracing::info!("File watcher active on {}", project_root.display());

    loop {
        if ct.is_cancelled() {
            break;
        }
        // Wait for the next *relevant* event. Irrelevant noise (DB/WAL writes
        // under .calm, editor temp files) is dropped here so it can neither
        // trigger nor — critically — delay a reindex.
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(res) if event_is_relevant(&res) => {}
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Debounce: reindex once the tree has been quiet (no relevant events) for
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
                    if event_is_relevant(&res) {
                        deadline = std::time::Instant::now() + DEBOUNCE;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        match calm_core::db::conn::open_writer(&db_path) {
            Ok(mut conn) => {
                match calm_core::indexer::pipeline::reindex_changed(&mut conn, &project_root) {
                    Ok(s) if !s.is_noop() => {
                        tracing::info!(
                            "Incremental reindex: {} changed, {} deleted",
                            s.changed,
                            s.deleted
                        );
                        // Embed any symbols/chunks added by this reindex (Layer 1 +
                        // Layer 2 — see indexer::chunker for the latter).
                        if let Some(model) = embedder.read().unwrap().clone() {
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
                        // Re-run the SCIP confidence-upgrade overlay so Rust call
                        // edges touched by *this* reindex don't stay stuck below
                        // `formal` for the rest of a long-running session — before
                        // this, `run_overlay` only ever ran once at server startup
                        // (see `lib.rs`), so any Rust file added/edited afterward
                        // never got upgraded until the next restart. Cheap when
                        // nothing Rust-relevant actually changed: `dirty` (the
                        // current Rust source fingerprint) makes `run_overlay`'s own
                        // cache key skip re-invoking rust-analyzer in that case —
                        // see `run_overlay`'s doc comment.
                        #[cfg(feature = "scip-overlay")]
                        {
                            let rust_cfg = calm_core::config::load_config(&project_root)
                                .map(|c| c.rust)
                                .unwrap_or_default();
                            let dirty = calm_core::scip::rust_source_dirty_keys(&conn);
                            match calm_core::scip::run_overlay(
                                &conn,
                                &project_root,
                                &rust_cfg,
                                &dirty,
                            ) {
                                Ok(stats) if stats.upgraded > 0 || stats.ruled_out > 0 => {
                                    // caller_count was computed by this reindex's
                                    // rebuild_graph before the overlay flipped
                                    // edge_confidence/ruled_out_by_scip on some
                                    // edges — refresh or it goes stale immediately
                                    // relative to the columns it's filtered on.
                                    if let Err(e) =
                                        calm_core::indexer::pipeline::refresh_caller_counts(&conn)
                                    {
                                        tracing::warn!(
                                            "caller_count refresh after incremental SCIP overlay failed: {e}"
                                        );
                                    }
                                    tracing::info!(
                                        "Incremental SCIP overlay: {} edges upgraded, {} fan-out siblings ruled out",
                                        stats.upgraded,
                                        stats.ruled_out
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!(
                                    "Incremental SCIP overlay error (base graph intact): {e}"
                                ),
                            }
                        }
                    }
                    Ok(_) => {}
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
        // Non-source files are ignored.
        assert!(!is_relevant_path(Path::new("README.md")));
    }
}
