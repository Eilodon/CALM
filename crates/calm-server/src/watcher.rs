//! Public compatibility entry point for the supervised watch runtime.
//!
//! The policy and lifecycle live in `watch_supervisor`: keeping this thin
//! facade preserves the existing integration-test/public call shape while
//! making watcher health and recovery independently testable.

use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};

use tokio_util::sync::CancellationToken;

/// Block on a production watch loop until `ct` is cancelled.
///
/// Existing callers that do not expose status still get the full supervised
/// behavior; bootstrap uses [`run_watch_loop_with_health`] to publish its
/// shared health handle through `indexing_status`.
#[allow(clippy::too_many_arguments)]
pub fn run_watch_loop(
    project_root: PathBuf,
    db_path: PathBuf,
    ct: CancellationToken,
    embedder: crate::EmbedderHandle,
    coverage: crate::CoverageHandle,
    graph_mode: Arc<RwLock<Option<String>>>,
    ready: Option<mpsc::Sender<()>>,
) {
    run_watch_loop_with_health(
        project_root,
        db_path,
        ct,
        embedder,
        coverage,
        graph_mode,
        crate::watch_supervisor::new_health_handle(),
        ready,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_watch_loop_with_health(
    project_root: PathBuf,
    db_path: PathBuf,
    ct: CancellationToken,
    embedder: crate::EmbedderHandle,
    coverage: crate::CoverageHandle,
    graph_mode: Arc<RwLock<Option<String>>>,
    health: crate::watch_supervisor::WatcherHealthHandle,
    ready: Option<mpsc::Sender<()>>,
) {
    crate::watch_supervisor::WatchSupervisor::production(
        project_root,
        db_path,
        ct,
        embedder,
        coverage,
        graph_mode,
        health,
        ready,
    )
    .run();
}
