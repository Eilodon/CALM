//! Supervised filesystem-watch runtime.
//!
//! The OS watcher is deliberately treated as an accelerator, never the source
//! of truth: exact events use the core `ChangeSet` protocol, loss-of-observation
//! signals trigger reconciliation, and a permanently unavailable watcher still
//! leaves a bounded full-reconciliation scheduler running.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::{RecursiveMode, Watcher, recommended_watcher};
use tokio_util::sync::CancellationToken;

use crate::sync_ext::RwLockExt;

/// Health/lifecycle state intentionally separate from indexing phase and graph
/// mode.  A watcher can be unavailable while the last completed index remains
/// valid at its timestamp, and a full graph rebuild says nothing about whether
/// new filesystem observations will arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatcherLifecycle {
    NotStarted,
    Starting,
    Armed,
    Backoff,
    Degraded,
    Stopped,
}

impl WatcherLifecycle {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Starting => "starting",
            Self::Armed => "armed",
            Self::Backoff => "backoff",
            Self::Degraded => "degraded",
            Self::Stopped => "stopped",
        }
    }
}

/// Freshness is deliberately distinct from [`WatcherLifecycle`].  The OS
/// subscription may be armed while a database refresh is retrying, and a
/// stopped subscription may still have a recently reconciled index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatcherFreshness {
    Unknown,
    Fresh,
    Retrying,
    Stale,
}

impl WatcherFreshness {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Fresh => "fresh",
            Self::Retrying => "retrying",
            Self::Stale => "stale",
        }
    }
}

/// The work shape of the last successful watcher refresh.  It is observability
/// only: graph mode remains a property of the indexer, never a proxy for watch
/// liveness or freshness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchRefreshKind {
    IncrementalPaths,
    ContextRebuild,
    CoverageReload,
    FullReconciliation,
}

impl WatchRefreshKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::IncrementalPaths => "incremental_paths",
            Self::ContextRebuild => "context_rebuild",
            Self::CoverageReload => "coverage_reload",
            Self::FullReconciliation => "full_reconciliation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WatcherHealth {
    pub(crate) lifecycle: WatcherLifecycle,
    pub(crate) armed: bool,
    pub(crate) freshness: WatcherFreshness,
    pub(crate) last_event_unix: Option<i64>,
    pub(crate) last_refresh_unix: Option<i64>,
    pub(crate) last_refresh_kind: Option<WatchRefreshKind>,
    pub(crate) last_reconciliation_unix: Option<i64>,
    pub(crate) last_reconciliation_reason: Option<&'static str>,
    pub(crate) last_error: Option<String>,
    /// Failures to create/maintain the OS watcher itself.
    pub(crate) consecutive_failures: u32,
    /// Failed database/graph/overlay refresh attempts while the watcher may
    /// still be armed.  Kept separately so an operationally-live watcher is
    /// never reported as dead merely because its index is stale.
    pub(crate) consecutive_refresh_failures: u32,
}

impl Default for WatcherHealth {
    fn default() -> Self {
        Self {
            lifecycle: WatcherLifecycle::NotStarted,
            armed: false,
            freshness: WatcherFreshness::Unknown,
            last_event_unix: None,
            last_refresh_unix: None,
            last_refresh_kind: None,
            last_reconciliation_unix: None,
            last_reconciliation_reason: None,
            last_error: None,
            consecutive_failures: 0,
            consecutive_refresh_failures: 0,
        }
    }
}

pub(crate) type WatcherHealthHandle = Arc<RwLock<WatcherHealth>>;

pub(crate) fn new_health_handle() -> WatcherHealthHandle {
    Arc::new(RwLock::new(WatcherHealth::default()))
}

/// Runtime tuning is deliberately owned by the supervisor, not inferred from
/// graph output or event volume.  Tests inject a compact schedule; production
/// defaults bound recovery while keeping a healthy watcher cheap.
#[derive(Debug, Clone)]
pub(crate) struct WatchSupervisorConfig {
    pub(crate) debounce: Duration,
    pub(crate) poll_interval: Duration,
    pub(crate) retry_limit: u32,
    pub(crate) retry_initial_backoff: Duration,
    pub(crate) retry_max_backoff: Duration,
    pub(crate) reconciliation_interval: Duration,
}

impl Default for WatchSupervisorConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(500),
            poll_interval: Duration::from_secs(1),
            retry_limit: 5,
            retry_initial_backoff: Duration::from_millis(250),
            retry_max_backoff: Duration::from_secs(15),
            // A healthy backend may silently lose an event without emitting
            // `need_rescan`; fifteen minutes bounds that drift without turning
            // the fallback into the old every-debounce full scan.
            reconciliation_interval: Duration::from_secs(15 * 60),
        }
    }
}

trait ArmedWatch: Send {}
impl<T: Send> ArmedWatch for T {}

/// Factory seam for deterministic lifecycle tests.  It owns both creation and
/// `watch()` so init failure, watch-arming failure, delivery errors, and a
/// disconnected callback channel can be reproduced without an OS backend.
trait WatchFactory: Send + Sync {
    fn arm(
        &self,
        root: &Path,
        sender: mpsc::Sender<notify::Result<notify::Event>>,
    ) -> Result<Box<dyn ArmedWatch>, String>;
}

#[derive(Debug, Default)]
struct NotifyWatchFactory;

impl WatchFactory for NotifyWatchFactory {
    fn arm(
        &self,
        root: &Path,
        sender: mpsc::Sender<notify::Result<notify::Event>>,
    ) -> Result<Box<dyn ArmedWatch>, String> {
        let mut watcher = recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .map_err(|error| format!("watcher initialization failed: {error}"))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| format!("watcher could not watch {}: {error}", root.display()))?;
        Ok(Box::new(watcher))
    }
}

struct WatchRuntime {
    project_root: PathBuf,
    db_path: PathBuf,
    ct: CancellationToken,
    embedder: crate::EmbedderHandle,
    coverage: crate::CoverageHandle,
    graph_mode: Arc<RwLock<Option<String>>>,
    health: WatcherHealthHandle,
    ready: Option<mpsc::Sender<()>>,
}

/// Supervises one OS watcher and the safe fallback that remains after it has
/// exhausted bounded retries.
pub(crate) struct WatchSupervisor {
    runtime: WatchRuntime,
    config: WatchSupervisorConfig,
    factory: Arc<dyn WatchFactory>,
}

impl WatchSupervisor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn production(
        project_root: PathBuf,
        db_path: PathBuf,
        ct: CancellationToken,
        embedder: crate::EmbedderHandle,
        coverage: crate::CoverageHandle,
        graph_mode: Arc<RwLock<Option<String>>>,
        health: WatcherHealthHandle,
        ready: Option<mpsc::Sender<()>>,
    ) -> Self {
        Self::with_factory(
            project_root,
            db_path,
            ct,
            embedder,
            coverage,
            graph_mode,
            health,
            ready,
            WatchSupervisorConfig::default(),
            Arc::new(NotifyWatchFactory),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_factory(
        project_root: PathBuf,
        db_path: PathBuf,
        ct: CancellationToken,
        embedder: crate::EmbedderHandle,
        coverage: crate::CoverageHandle,
        graph_mode: Arc<RwLock<Option<String>>>,
        health: WatcherHealthHandle,
        ready: Option<mpsc::Sender<()>>,
        config: WatchSupervisorConfig,
        factory: Arc<dyn WatchFactory>,
    ) -> Self {
        Self {
            runtime: WatchRuntime {
                project_root,
                db_path,
                ct,
                embedder,
                coverage,
                graph_mode,
                health,
                ready,
            },
            config,
            factory,
        }
    }

    pub(crate) fn run(mut self) {
        self.update_health(|health| {
            health.lifecycle = WatcherLifecycle::Starting;
            health.armed = false;
        });

        let mut catalog =
            calm_core::indexer::refresh::InputCatalog::for_project(&self.runtime.project_root);
        let mut failures = 0_u32;
        let mut needs_reconciliation_after_rearm = false;

        loop {
            if self.runtime.ct.is_cancelled() {
                self.mark_stopped();
                return;
            }

            let (sender, receiver) = mpsc::channel();
            match self.factory.arm(&self.runtime.project_root, sender) {
                Ok(guard) => {
                    let recovered = std::mem::take(&mut needs_reconciliation_after_rearm);
                    // Arm first, then reconcile once before trusting callbacks.
                    // On a clean input contract this is only a source-hash delta,
                    // but it closes the bootstrap/re-arm interval during which no
                    // notify receiver existed to name a write on disk.
                    let initial_reconciliation = if recovered {
                        calm_core::indexer::refresh::FullReconciliationReason::WatcherError
                    } else {
                        calm_core::indexer::refresh::FullReconciliationReason::WatcherStart
                    };
                    failures = 0;
                    self.update_health(|health| {
                        health.lifecycle = WatcherLifecycle::Armed;
                        health.armed = true;
                        health.consecutive_failures = 0;
                        health.last_error = None;
                    });
                    tracing::info!(
                        "File watcher active on {}",
                        self.runtime.project_root.display()
                    );

                    // Keep the backend alive for the entire session.  A panic
                    // in this worker is converted into health + bounded retry,
                    // never an invisible thread disappearance.
                    let session = catch_unwind(AssertUnwindSafe(|| {
                        self.run_armed_session(
                            &receiver,
                            &mut catalog,
                            guard.as_ref(),
                            Some(initial_reconciliation),
                        )
                    }));
                    match session {
                        Ok(SessionEnd::Cancelled) => {
                            self.mark_stopped();
                            return;
                        }
                        Ok(SessionEnd::Failure(error)) => {
                            self.record_failure(error);
                            needs_reconciliation_after_rearm = true;
                        }
                        Err(_) => {
                            self.record_failure("watch loop panicked; retrying safely".to_owned());
                            needs_reconciliation_after_rearm = true;
                        }
                    }
                }
                Err(error) => {
                    self.record_failure(actionable_watcher_error(&error));
                    needs_reconciliation_after_rearm = true;
                }
            }

            failures = failures.saturating_add(1);
            if failures >= self.config.retry_limit {
                self.enter_degraded();
                self.run_degraded_reconciliation(&mut catalog);
                return;
            }

            self.update_health(|health| {
                health.lifecycle = WatcherLifecycle::Backoff;
                health.armed = false;
                health.consecutive_failures = failures;
            });
            if !self.wait_cancellable(backoff_for(&self.config, failures)) {
                self.mark_stopped();
                return;
            }
            self.update_health(|health| health.lifecycle = WatcherLifecycle::Starting);
        }
    }

    fn run_armed_session(
        &mut self,
        receiver: &mpsc::Receiver<notify::Result<notify::Event>>,
        catalog: &mut calm_core::indexer::refresh::InputCatalog,
        _guard: &dyn ArmedWatch,
        initial_reconciliation: Option<calm_core::indexer::refresh::FullReconciliationReason>,
    ) -> SessionEnd {
        let mut changes = calm_core::indexer::refresh::ChangeSet::default();
        if let Some(reason) = initial_reconciliation {
            changes.require_full_reconciliation(reason);
        }
        let mut initial_reconciliation_pending = initial_reconciliation.is_some();
        if !initial_reconciliation_pending {
            self.signal_ready();
        }
        let mut event_deadline = None;
        let mut retry_deadline = (!changes.is_empty()).then(Instant::now);
        let mut retry_attempts = 0_u32;
        let mut retry_budget_exhausted = false;
        let mut next_reconciliation = Instant::now() + self.config.reconciliation_interval;

        loop {
            if self.runtime.ct.is_cancelled() {
                return SessionEnd::Cancelled;
            }

            let now = Instant::now();
            let next_work = [event_deadline, retry_deadline, Some(next_reconciliation)]
                .into_iter()
                .flatten()
                .min()
                .expect("reconciliation deadline is always present");
            let timeout = next_work
                .saturating_duration_since(now)
                .min(self.config.poll_interval);

            match receiver.recv_timeout(timeout) {
                Ok(event) => {
                    if record_notify_event(event, catalog, &mut changes, &self.runtime.health)
                        && !retry_budget_exhausted
                    {
                        event_deadline = Some(Instant::now() + self.config.debounce);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return SessionEnd::Failure("watcher callback channel disconnected".to_owned());
                }
            }

            let now = Instant::now();
            let reconciliation_due = now >= next_reconciliation;
            let event_due = event_deadline.is_some_and(|due| now >= due);
            let retry_due = retry_deadline.is_some_and(|due| now >= due);
            if !reconciliation_due && !event_due && !retry_due {
                continue;
            }

            if reconciliation_due {
                changes.require_full_reconciliation(
                    calm_core::indexer::refresh::FullReconciliationReason::PeriodicReconciliation,
                );
                retry_attempts = 0;
                retry_budget_exhausted = false;
                next_reconciliation = Instant::now() + self.config.reconciliation_interval;
            }

            match self.consume_changes(&mut changes, catalog) {
                RefreshResult::Completed => {
                    if initial_reconciliation_pending {
                        // `ready` is a writer-quiescence boundary for callers such
                        // as edit/reindex. The notify backend is already armed, so
                        // events are buffered while the startup reconciliation
                        // establishes a fresh baseline.
                        self.signal_ready();
                        initial_reconciliation_pending = false;
                    }
                    event_deadline = None;
                    retry_deadline = None;
                    retry_attempts = 0;
                    retry_budget_exhausted = false;
                }
                RefreshResult::Cancelled => return SessionEnd::Cancelled,
                RefreshResult::Failed => {
                    event_deadline = None;
                    retry_attempts = retry_attempts.saturating_add(1);
                    if retry_attempts >= self.config.retry_limit {
                        retry_deadline = None;
                        retry_budget_exhausted = true;
                        self.mark_refresh_stale();
                    } else {
                        retry_deadline =
                            Some(Instant::now() + backoff_for(&self.config, retry_attempts));
                    }
                }
            }
        }
    }

    fn run_degraded_reconciliation(
        &mut self,
        catalog: &mut calm_core::indexer::refresh::InputCatalog,
    ) {
        tracing::error!(
            "File watcher is degraded after bounded retries; periodic full reconciliation remains active"
        );
        while self.wait_cancellable(self.config.reconciliation_interval) {
            match self.refresh(
                calm_core::indexer::refresh::RefreshRequest::FullReconciliation {
                    reason: calm_core::indexer::refresh::FullReconciliationReason::WatcherError,
                    reload_coverage: true,
                },
                catalog,
            ) {
                RefreshResult::Cancelled => break,
                RefreshResult::Completed | RefreshResult::Failed => {}
            }
        }
        self.mark_stopped();
    }

    fn consume_changes(
        &mut self,
        changes: &mut calm_core::indexer::refresh::ChangeSet,
        catalog: &mut calm_core::indexer::refresh::InputCatalog,
    ) -> RefreshResult {
        if changes.is_empty() {
            return RefreshResult::Completed;
        }
        let request = changes.refresh_request();
        *changes = calm_core::indexer::refresh::ChangeSet::default();
        match self.refresh(request, catalog) {
            RefreshResult::Completed => RefreshResult::Completed,
            RefreshResult::Cancelled => RefreshResult::Cancelled,
            RefreshResult::Failed => {
                // A DB/refresh error means the exact path list is no longer enough
                // evidence of freshness. Retry as an explicit full reconciliation
                // with bounded backoff, never by trusting the original path list.
                changes.require_full_reconciliation(
                    calm_core::indexer::refresh::FullReconciliationReason::WatcherError,
                );
                RefreshResult::Failed
            }
        }
    }

    /// Distinguishes cancellation from refresh failure so the caller can retain
    /// the coalesced ChangeSet and apply bounded retry/backoff.
    fn refresh(
        &mut self,
        request: calm_core::indexer::refresh::RefreshRequest,
        catalog: &mut calm_core::indexer::refresh::InputCatalog,
    ) -> RefreshResult {
        let refreshes_catalog = request.refreshes_input_catalog();
        let refresh_kind = refresh_kind(&request);
        let result = (|| -> anyhow::Result<Option<calm_core::indexer::refresh::RefreshOutcome>> {
            let mut conn = calm_core::db::conn::open_writer(&self.runtime.db_path)?;
            let outcome = match calm_core::indexer::refresh::execute_refresh_cancellable(
                &mut conn,
                &self.runtime.project_root,
                &request,
                &|| self.runtime.ct.is_cancelled(),
            )? {
                calm_core::indexer::refresh::RefreshExecution::Completed(outcome) => outcome,
                calm_core::indexer::refresh::RefreshExecution::Cancelled => return Ok(None),
            };

            if outcome.reload_coverage {
                let reloaded =
                    calm_core::analysis::coverage::load_coverage(&self.runtime.project_root);
                tracing::info!("Coverage inputs refreshed ({})", reloaded.source);
                *self.runtime.coverage.write_ok() = reloaded;
            }
            if let Some(mode) = outcome.graph_mode.as_ref() {
                *self.runtime.graph_mode.write_ok() = Some(mode.label());
            }
            if refreshes_catalog {
                let refreshed_catalog = calm_core::indexer::refresh::InputCatalog::for_project(
                    &self.runtime.project_root,
                );
                calm_core::indexer::refresh::persist_index_input_snapshot(
                    &conn,
                    &refreshed_catalog,
                )?;
                *catalog = refreshed_catalog;
            }
            if outcome.graph_rebuilt {
                if let Some(model) = self.runtime.embedder.read_ok().clone() {
                    if let Err(error) = calm_core::embedding::embed_pending(&conn, model.as_ref()) {
                        tracing::error!("Incremental embedding failed: {error}");
                    }
                    if let Err(error) =
                        calm_core::embedding::embed_pending_chunks(&conn, model.as_ref())
                    {
                        tracing::error!("Incremental chunk embedding failed: {error}");
                    }
                }
                #[cfg(feature = "scip-overlay")]
                if !self.runtime.ct.is_cancelled() {
                    drop(conn);
                    crate::scip_overlay::run_all_coalesced(
                        &self.runtime.project_root,
                        &self.runtime.db_path,
                    );
                }
            }
            Ok(Some(outcome))
        })();

        match result {
            Ok(None) => RefreshResult::Cancelled,
            Ok(Some(outcome)) => {
                let now = unix_now();
                self.update_health(|health| {
                    health.last_refresh_unix = Some(now);
                    health.last_refresh_kind = Some(refresh_kind);
                    if let Some(reason) = outcome.full_reconciliation {
                        health.last_reconciliation_unix = Some(now);
                        health.last_reconciliation_reason =
                            Some(full_reconciliation_reason_label(reason));
                    }
                    health.freshness = WatcherFreshness::Fresh;
                    health.last_error = None;
                    health.consecutive_refresh_failures = 0;
                });
                RefreshResult::Completed
            }
            Err(error) => {
                self.update_health(|health| {
                    health.freshness = WatcherFreshness::Retrying;
                    health.last_error = Some(format!("watcher refresh failed: {error}"));
                    health.consecutive_refresh_failures =
                        health.consecutive_refresh_failures.saturating_add(1);
                });
                tracing::error!("Watcher refresh failed: {error}");
                RefreshResult::Failed
            }
        }
    }

    fn record_failure(&self, error: String) {
        tracing::error!("File watcher failure: {error}");
        self.update_health(|health| {
            health.armed = false;
            health.last_error = Some(error);
            health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        });
    }

    fn enter_degraded(&self) {
        self.update_health(|health| {
            health.lifecycle = WatcherLifecycle::Degraded;
            health.armed = false;
        });
    }

    fn mark_refresh_stale(&self) {
        self.update_health(|health| health.freshness = WatcherFreshness::Stale);
    }

    fn mark_stopped(&self) {
        self.update_health(|health| {
            health.lifecycle = WatcherLifecycle::Stopped;
            health.armed = false;
        });
    }

    fn wait_cancellable(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while !self.runtime.ct.is_cancelled() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return true;
            }
            std::thread::sleep(remaining.min(Duration::from_millis(100)));
        }
        false
    }

    fn signal_ready(&mut self) {
        if let Some(ready) = self.runtime.ready.take() {
            let _ = ready.send(());
        }
    }

    fn update_health(&self, update: impl FnOnce(&mut WatcherHealth)) {
        update(&mut self.runtime.health.write_ok());
    }
}

enum SessionEnd {
    Cancelled,
    Failure(String),
}

/// A refresh failure is neither a watcher crash nor cancellation. Callers must
/// retain the ChangeSet and schedule the safe full-reconciliation retry.
enum RefreshResult {
    Completed,
    Cancelled,
    Failed,
}

fn refresh_kind(request: &calm_core::indexer::refresh::RefreshRequest) -> WatchRefreshKind {
    match request {
        calm_core::indexer::refresh::RefreshRequest::Noop => WatchRefreshKind::IncrementalPaths,
        calm_core::indexer::refresh::RefreshRequest::Incremental {
            source_paths,
            context_paths,
            reload_coverage,
        } if !context_paths.is_empty() => WatchRefreshKind::ContextRebuild,
        calm_core::indexer::refresh::RefreshRequest::Incremental { source_paths, .. }
            if !source_paths.is_empty() =>
        {
            WatchRefreshKind::IncrementalPaths
        }
        calm_core::indexer::refresh::RefreshRequest::Incremental {
            reload_coverage: true,
            ..
        } => WatchRefreshKind::CoverageReload,
        calm_core::indexer::refresh::RefreshRequest::Incremental { .. } => {
            WatchRefreshKind::IncrementalPaths
        }
        calm_core::indexer::refresh::RefreshRequest::FullReconciliation { .. } => {
            WatchRefreshKind::FullReconciliation
        }
    }
}

fn full_reconciliation_reason_label(
    reason: calm_core::indexer::refresh::FullReconciliationReason,
) -> &'static str {
    match reason {
        calm_core::indexer::refresh::FullReconciliationReason::NotifyRescan => "notify_rescan",
        calm_core::indexer::refresh::FullReconciliationReason::WatcherStart => "watcher_start",
        calm_core::indexer::refresh::FullReconciliationReason::WatcherError => "watcher_error",
        calm_core::indexer::refresh::FullReconciliationReason::UnsafePath => "unsafe_path",
        calm_core::indexer::refresh::FullReconciliationReason::UnsafeRename => "unsafe_rename",
        calm_core::indexer::refresh::FullReconciliationReason::ConfigurationChanged => {
            "configuration_changed"
        }
        calm_core::indexer::refresh::FullReconciliationReason::PeriodicReconciliation => {
            "periodic_reconciliation"
        }
    }
}

fn record_notify_event(
    notification: notify::Result<notify::Event>,
    catalog: &calm_core::indexer::refresh::InputCatalog,
    changes: &mut calm_core::indexer::refresh::ChangeSet,
    health: &WatcherHealthHandle,
) -> bool {
    match notification {
        Err(error) => {
            changes.require_full_reconciliation(
                calm_core::indexer::refresh::FullReconciliationReason::WatcherError,
            );
            let mut state = health.write_ok();
            state.last_event_unix = Some(unix_now());
            state.last_error = Some(format!("notify delivery error: {error}"));
            true
        }
        Ok(event) => {
            let needs_rescan = event.need_rescan();
            let is_rename = matches!(
                event.kind,
                notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
            );
            let classes = event
                .paths
                .iter()
                .map(|path| catalog.classify(path))
                .collect::<Vec<_>>();
            let unsafe_rename = is_rename
                && (event.paths.len() != 2
                    || classes
                        .iter()
                        .any(calm_core::indexer::refresh::ChangeClass::is_unsafe));

            if needs_rescan {
                changes.require_full_reconciliation(
                    calm_core::indexer::refresh::FullReconciliationReason::NotifyRescan,
                );
            }
            if unsafe_rename {
                changes.require_full_reconciliation(
                    calm_core::indexer::refresh::FullReconciliationReason::UnsafeRename,
                );
            }

            let mut relevant = needs_rescan || unsafe_rename;
            for class in classes {
                relevant |= !matches!(class, calm_core::indexer::refresh::ChangeClass::Ignored);
                changes.record_class(class);
            }
            if relevant {
                health.write_ok().last_event_unix = Some(unix_now());
            }
            relevant
        }
    }
}

fn backoff_for(config: &WatchSupervisorConfig, failures: u32) -> Duration {
    let factor = 1_u32
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u32::MAX);
    config
        .retry_initial_backoff
        .saturating_mul(factor)
        .min(config.retry_max_backoff)
}

fn actionable_watcher_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("inotify") || lower.contains("too many open files") {
        format!(
            "{error}; filesystem watch limit may be exhausted — increase fs.inotify.max_user_watches or reduce concurrent watchers"
        )
    } else {
        error.to_owned()
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn test_root(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "calm_watch_supervisor_{label}_{}_{}",
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[derive(Default)]
    struct FakeFactory {
        outcomes: Arc<std::sync::Mutex<VecDeque<Result<(), String>>>>,
        sender: Arc<std::sync::Mutex<Option<mpsc::Sender<notify::Result<notify::Event>>>>>,
        attempts: Arc<std::sync::atomic::AtomicU32>,
    }

    impl FakeFactory {
        fn with_outcomes(outcomes: impl IntoIterator<Item = Result<(), String>>) -> Self {
            Self {
                outcomes: Arc::new(std::sync::Mutex::new(outcomes.into_iter().collect())),
                ..Self::default()
            }
        }
    }

    impl WatchFactory for FakeFactory {
        fn arm(
            &self,
            _root: &Path,
            sender: mpsc::Sender<notify::Result<notify::Event>>,
        ) -> Result<Box<dyn ArmedWatch>, String> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.outcomes.lock().unwrap().pop_front().unwrap_or(Ok(())) {
                Ok(()) => {
                    *self.sender.lock().unwrap() = Some(sender);
                    Ok(Box::new(()))
                }
                Err(error) => Err(error),
            }
        }
    }

    fn test_config() -> WatchSupervisorConfig {
        WatchSupervisorConfig {
            debounce: Duration::from_millis(1),
            poll_interval: Duration::from_millis(1),
            retry_limit: 2,
            retry_initial_backoff: Duration::from_millis(1),
            retry_max_backoff: Duration::from_millis(2),
            reconciliation_interval: Duration::from_millis(20),
        }
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("condition did not become true before timeout");
    }

    #[test]
    fn exhausted_factory_failures_surface_degraded_health_and_keep_no_false_arm() {
        let root = test_root("degraded");
        let health = new_health_handle();
        let factory = Arc::new(FakeFactory::with_outcomes([
            Err("inotify watch limit".to_owned()),
            Err("inotify watch limit".to_owned()),
        ]));
        let ct = CancellationToken::new();
        let supervisor = WatchSupervisor::with_factory(
            root.clone(),
            root.join("index.db"),
            ct.clone(),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(
                calm_core::analysis::coverage::CoverageData::none(),
            )),
            Arc::new(RwLock::new(None)),
            health.clone(),
            None,
            test_config(),
            factory.clone(),
        );
        let worker = std::thread::spawn(move || supervisor.run());

        wait_until(|| health.read_ok().lifecycle == WatcherLifecycle::Degraded);
        let state = health.read_ok().clone();
        assert!(!state.armed);
        assert_eq!(state.consecutive_failures, 2);
        assert!(
            state
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("fs.inotify.max_user_watches"))
        );
        assert_eq!(
            factory.attempts.load(std::sync::atomic::Ordering::SeqCst),
            2
        );

        ct.cancel();
        worker.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rearm_reconciliation_catches_drift_created_while_the_watcher_was_down() {
        let root = test_root("rearm_reconciliation");
        let calm_dir = root.join(".calm");
        std::fs::create_dir_all(&calm_dir).unwrap();
        // Markdown is tier-0 indexed but has no external SCIP provider, which
        // keeps this recovery test deterministic and focused on reconciliation.
        let source = root.join("README.md");
        std::fs::write(&source, "# Before\n").unwrap();
        let db_path = calm_dir.join("index.db");
        {
            let mut conn = calm_core::db::conn::open_writer(&db_path).unwrap();
            calm_core::db::schema::init_db(&conn).unwrap();
            calm_core::indexer::pipeline::run_indexing_pipeline(
                &mut conn,
                &root,
                Arc::new(RwLock::new(calm_core::types::IndexingPhase::Scanning)),
            )
            .unwrap();
        }

        // This write deliberately happens during the observed gap: the first
        // arm fails, so no event can be trusted to name this path. The rearm
        // must reconcile disk rather than wait for a future save.
        let updated = "# After\n";
        std::fs::write(&source, updated).unwrap();
        let health = new_health_handle();
        let factory = Arc::new(FakeFactory::with_outcomes([
            Err("transient watcher failure".to_owned()),
            Ok(()),
        ]));
        let ct = CancellationToken::new();
        let supervisor = WatchSupervisor::with_factory(
            root.clone(),
            db_path.clone(),
            ct.clone(),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(
                calm_core::analysis::coverage::CoverageData::none(),
            )),
            Arc::new(RwLock::new(None)),
            health.clone(),
            None,
            test_config(),
            factory.clone(),
        );
        let worker = std::thread::spawn(move || supervisor.run());

        wait_until(|| {
            let state = health.read_ok();
            state.armed
                && state.freshness == WatcherFreshness::Fresh
                && state.last_reconciliation_reason == Some("watcher_error")
        });

        let indexed_hash: String = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT hash FROM file_index WHERE path = 'README.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed_hash,
            calm_core::indexer::pipeline::hash_content(updated),
            "the post-rearm full reconciliation must see drift created during the watch gap"
        );
        assert_eq!(
            factory.attempts.load(std::sync::atomic::Ordering::SeqCst),
            2
        );

        ct.cancel();
        worker.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn startup_reconciliation_catches_drift_before_the_first_observed_event() {
        let root = test_root("startup_reconciliation");
        let calm_dir = root.join(".calm");
        std::fs::create_dir_all(&calm_dir).unwrap();
        let source = root.join("README.md");
        std::fs::write(&source, "# Before\n").unwrap();
        let db_path = calm_dir.join("index.db");
        {
            let mut conn = calm_core::db::conn::open_writer(&db_path).unwrap();
            calm_core::db::schema::init_db(&conn).unwrap();
            calm_core::indexer::pipeline::run_indexing_pipeline(
                &mut conn,
                &root,
                Arc::new(RwLock::new(calm_core::types::IndexingPhase::Scanning)),
            )
            .unwrap();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&root);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
        }

        // This is the bootstrap gap: the initial index has committed, but no
        // notify callback exists yet to name this write.  The first armed
        // session must perform a cheap source-hash reconciliation before it
        // trusts events alone.
        let updated = "# After\n";
        std::fs::write(&source, updated).unwrap();
        let health = new_health_handle();
        let factory = Arc::new(FakeFactory::with_outcomes([Ok(())]));
        let ct = CancellationToken::new();
        let mut config = test_config();
        config.reconciliation_interval = Duration::from_secs(5);
        let supervisor = WatchSupervisor::with_factory(
            root.clone(),
            db_path.clone(),
            ct.clone(),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(
                calm_core::analysis::coverage::CoverageData::none(),
            )),
            Arc::new(RwLock::new(None)),
            health.clone(),
            None,
            config,
            factory,
        );
        let worker = std::thread::spawn(move || supervisor.run());

        wait_until(|| {
            let state = health.read_ok();
            state.armed
                && state.freshness == WatcherFreshness::Fresh
                && state.last_reconciliation_reason == Some("watcher_start")
        });
        let indexed_hash: String = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT hash FROM file_index WHERE path = 'README.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed_hash,
            calm_core::indexer::pipeline::hash_content(updated),
            "startup reconciliation must catch writes in the bootstrap observation gap"
        );

        ct.cancel();
        worker.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normal_source_event_uses_incremental_paths_without_reconciliation() {
        let root = test_root("incremental_source");
        let calm_dir = root.join(".calm");
        std::fs::create_dir_all(&calm_dir).unwrap();
        let source = root.join("README.md");
        std::fs::write(&source, "# Before\n").unwrap();
        let db_path = calm_dir.join("index.db");
        {
            let mut conn = calm_core::db::conn::open_writer(&db_path).unwrap();
            calm_core::db::schema::init_db(&conn).unwrap();
            calm_core::indexer::pipeline::run_indexing_pipeline(
                &mut conn,
                &root,
                Arc::new(RwLock::new(calm_core::types::IndexingPhase::Scanning)),
            )
            .unwrap();
        }

        let health = new_health_handle();
        let factory = Arc::new(FakeFactory::with_outcomes([Ok(())]));
        let ct = CancellationToken::new();
        let mut config = test_config();
        // Leave enough room to prove this save does not become a periodic full
        // reconciliation merely because the test scheduler is fast.
        config.reconciliation_interval = Duration::from_secs(5);
        let supervisor = WatchSupervisor::with_factory(
            root.clone(),
            db_path.clone(),
            ct.clone(),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(
                calm_core::analysis::coverage::CoverageData::none(),
            )),
            Arc::new(RwLock::new(None)),
            health.clone(),
            None,
            config,
            factory.clone(),
        );
        let worker = std::thread::spawn(move || supervisor.run());
        wait_until(|| {
            let state = health.read_ok();
            state.armed
                && state.freshness == WatcherFreshness::Fresh
                && state.last_reconciliation_reason == Some("watcher_start")
        });

        let updated = "# After\n";
        std::fs::write(&source, updated).unwrap();
        let mut event = notify::Event::new(notify::EventKind::Other);
        event.paths.push(source.clone());
        factory
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(Ok(event))
            .unwrap();

        wait_until(|| {
            let state = health.read_ok();
            state.freshness == WatcherFreshness::Fresh
                && state.last_refresh_kind == Some(WatchRefreshKind::IncrementalPaths)
        });
        let state = health.read_ok().clone();
        assert_eq!(
            state.last_reconciliation_reason,
            Some("watcher_start"),
            "a normal source save must not request another full reconciliation"
        );
        let indexed_hash: String = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT hash FROM file_index WHERE path = 'README.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed_hash,
            calm_core::indexer::pipeline::hash_content(updated)
        );

        ct.cancel();
        worker.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn notify_rescan_and_unsafe_rename_become_explicit_full_reconciliation() {
        let root = test_root("events");
        let catalog = calm_core::indexer::refresh::InputCatalog::new(&root, &[]);
        let health = new_health_handle();
        let mut changes = calm_core::indexer::refresh::ChangeSet::default();

        record_notify_event(
            Ok(notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)),
            &catalog,
            &mut changes,
            &health,
        );
        assert!(matches!(
            changes.refresh_request(),
            calm_core::indexer::refresh::RefreshRequest::FullReconciliation {
                reason: calm_core::indexer::refresh::FullReconciliationReason::NotifyRescan,
                ..
            }
        ));

        let mut rename_changes = calm_core::indexer::refresh::ChangeSet::default();
        let mut rename = notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Name(notify::event::RenameMode::Any),
        ));
        rename.paths = vec![root.join("src/only-one-side.rs")];
        record_notify_event(Ok(rename), &catalog, &mut rename_changes, &health);
        assert!(matches!(
            rename_changes.refresh_request(),
            calm_core::indexer::refresh::RefreshRequest::FullReconciliation {
                reason: calm_core::indexer::refresh::FullReconciliationReason::UnsafeRename,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
