#[cfg(unix)]
pub mod daemon;
mod scip_overlay;
mod sync_ext;
pub mod telemetry;
pub mod tools;
pub mod watcher;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use calm_core::config::SemanticSearchConfig;
use calm_core::embedding::Embedder;
use calm_core::types::EmbedStatus;
use rmcp::transport::stdio;
use tokio_util::sync::CancellationToken;

use sync_ext::RwLockExt;
use tools::CalmServer;

/// Shared handle to the loaded embedder, written by the indexer, read by tools.
pub type EmbedderHandle = Arc<RwLock<Option<Arc<Embedder>>>>;

/// Shared handle to loaded coverage data, reloaded in place by the file
/// watcher whenever the coverage file itself changes on disk.
pub type CoverageHandle = Arc<RwLock<calm_core::analysis::coverage::CoverageData>>;

/// Hard ceiling (whole seconds — `libc::alarm`'s own resolution) on graceful
/// shutdown after SIGINT/SIGTERM, before the kernel itself terminates this
/// process. See `serve_stdio_with_preset`'s SIGTERM handler for why this is
/// a raw POSIX `alarm()` rather than an in-process async timer — generous
/// enough for a real in-flight parse batch/lock-poll/WAL-checkpoint to
/// finish normally, short enough that a client that killed us isn't left
/// wondering for long.
const SHUTDOWN_WATCHDOG_SECS: libc::c_uint = 10;

/// Arms a raw POSIX `alarm()` for `SHUTDOWN_WATCHDOG_SECS` real seconds and
/// deliberately leaves SIGALRM's disposition at its OS default (`Term`).
///
/// Why not an in-process async watchdog (`tokio::time::sleep` + explicit
/// exit)? That was the first approach tried here, and it did not work: `rmcp`'s
/// stdio transport reads stdin via (effectively) `tokio::io::stdin()`, which
/// Tokio's own docs acknowledge is implemented as an ordinary *blocking*
/// `read()` on a dedicated OS thread with "no way to cancel" it — confirmed
/// directly in this investigation via `/proc/<pid>/task/*/wchan` showing a
/// thread parked in `anon_pipe_read` seconds after `ct.cancel()` had already
/// fired. An async watchdog spawned on the *same* Tokio runtime never fired
/// either (its own `tokio::time::sleep` never woke, even at 500ms) — most
/// likely the runtime's timer/IO driver sharing something with whatever that
/// blocked thread was holding, though the exact mechanism was never fully
/// pinned down. A raw `alarm()` sidesteps the question entirely: it's a
/// kernel timer, not a userspace one, so it fires regardless of whether this
/// process's async runtime, worker threads, or libc's `exit()`/`atexit`
/// machinery are healthy — the same reason a plain `kill -ALRM` always works
/// on a wedged process when nothing else will short of SIGKILL.
///
/// If graceful shutdown completes and the process exits normally before the
/// alarm fires, the pending alarm is simply discarded by the OS along with
/// the rest of the process's state — nothing needs to explicitly cancel it.
#[cfg(unix)]
fn arm_shutdown_watchdog() {
    unsafe {
        libc::alarm(SHUTDOWN_WATCHDOG_SECS);
    }
}

#[cfg(not(unix))]
fn arm_shutdown_watchdog() {}

pub async fn serve_stdio(project_root: PathBuf, db_path: PathBuf) -> Result<()> {
    serve_stdio_with_preset(project_root, db_path, "full".into()).await
}

/// Bundle returned by `bootstrap`: a fully-constructed `CalmServer` (index/
/// watcher/embeddings already started in the background) plus the master
/// `CancellationToken` that drives graceful shutdown. Callers choose what to
/// do with `ct`: `serve_stdio_with_preset` moves it directly into
/// `serve_server_with_ct` (today's 1-client-per-process shape, where the
/// service's own lifetime IS the shutdown token's lifetime); a daemon-style
/// caller instead keeps `ct` as its own accept-loop-lifetime token and hands
/// out `ct.child_token()` per connection, so one connection closing can't
/// cancel every other session sharing the daemon.
pub struct Bootstrapped {
    pub server: CalmServer,
    pub ct: CancellationToken,
}

/// Everything `serve_stdio_with_preset` used to do before touching a
/// transport: construct `CalmServer`, install SIGINT/SIGTERM handlers on a
/// fresh `CancellationToken`, and kick off the background indexer/embedder/
/// watcher. Extracted so a non-stdio transport (a daemon accept loop) can
/// reuse this exact bootstrap sequence instead of duplicating it — see
/// `Bootstrapped`'s doc comment for how the two tails differ in what they do
/// with the returned `ct`.
pub async fn bootstrap(
    project_root: PathBuf,
    db_path: PathBuf,
    preset: String,
) -> Result<Bootstrapped> {
    calm_core::gitignore::ensure_gitignore(&project_root)?;

    let server = CalmServer::new_with_preset(project_root.clone(), db_path.clone(), preset)?;
    let ct = CancellationToken::new();
    let ct_clone = ct.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received SIGINT, shutting down");
        arm_shutdown_watchdog();
        ct_clone.cancel();
    });

    // SIGTERM — not just SIGINT — because it's the default (and often only)
    // signal MCP clients/process managers send to stop a stdio child: Node's
    // `ChildProcess.kill()` defaults to SIGTERM, as does plain `kill <pid>`.
    // Without this, only a literal Ctrl+C in an attached terminal reached the
    // graceful-shutdown path (ct.cancel() below) — every other real-world
    // teardown (editor closing the MCP connection, a wrapper script relaying
    // its own SIGTERM) fell straight to the OS default action (immediate
    // terminate), skipping the WAL checkpoint below entirely. Unix-only:
    // SIGTERM has no Windows equivalent and `tokio::signal::unix` doesn't
    // compile there — Windows teardown still goes through SIGINT-equivalent
    // (Ctrl+C/Ctrl+Break) or an abrupt terminate, same as before this change.
    #[cfg(unix)]
    {
        let ct_term = ct.clone();
        tokio::spawn(async move {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut term) => {
                    term.recv().await;
                    tracing::info!("Received SIGTERM, shutting down");
                    arm_shutdown_watchdog();
                    ct_term.cancel();
                }
                Err(e) => tracing::warn!("Failed to install SIGTERM handler: {e}"),
            }
        });
    }

    let semantic = calm_core::config::load_config_or_warn(&project_root).semantic_search;

    let indexer_db_path = db_path.clone();
    let indexer_root = project_root.clone();
    let watch_ct = ct.clone();
    let phase = server.phase_handle();
    let last_index_error = server.last_index_error_handle();
    let embedder = server.embedder_handle();
    let embed_status = server.embed_status_handle();
    let last_embed_error = server.last_embed_error_handle();
    let owns_indexer_lock = server.owns_indexer_lock_handle();
    let coverage = server.coverage_handle();
    let watch_embedder = embedder.clone();
    let watch_coverage = coverage.clone();
    // Phase B T6.5: the watcher records which rebuild path each incremental
    // reindex took into the same shared slot indexing_status reads.
    let watch_graph_mode = server.last_graph_mode_handle();
    // Kept outside the `spawn_blocking` closure below so a panic there (caught
    // via the awaited `JoinHandle`) still has a handle to report through —
    // `phase`/`last_index_error` themselves are moved into that closure.
    let outer_phase = phase.clone();
    let outer_last_index_error = last_index_error.clone();
    tokio::spawn(async move {
        let handle = tokio::task::spawn_blocking(move || {
            tracing::info!("Background indexer thread started");

            // Checked at every safe bail-out point below (lock-acquire wait,
            // between index/reindex parse batches, before starting the SCIP
            // overlay pass) — without this, a SIGTERM arriving mid-run had
            // nothing to interrupt this `spawn_blocking` task with, and
            // Tokio's runtime-drop blocks process exit until every
            // outstanding blocking-pool task returns on its own. See the
            // SIGTERM-hang investigation this closure's cancellation checks
            // fix: a process could take 15+ seconds (or hang until SIGKILL)
            // to exit even after cleanly receiving and handling SIGTERM,
            // because nothing downstream of `ct.cancel()` ever looked at it
            // until the (already cancellation-aware) watch loop far below.
            let cancel = || watch_ct.is_cancelled();

            // Every `calm serve` process used to spawn its own indexer+watcher
            // against the same shared DB — harmless with one process, but N
            // concurrent processes on the same project_root (e.g. multiple
            // editor/MCP-client sessions) meant N redundant reindex loops
            // racing each other, mitigated only by `open_writer`'s
            // `busy_timeout`, never prevented. Only the process that wins
            // this advisory lock runs the indexer+watcher below; a loser
            // still serves tool calls read-only against the DB the winner
            // keeps fresh — and now waits in the background to *become* the
            // owner if that winner ever exits mid-session (see
            // `acquire_blocking`'s doc comment): without that, a loser that
            // started before the owner died would stay read-only for its
            // entire remaining lifetime, since nothing else here ever
            // retried the initial `try_acquire`.
            let lock_dir = indexer_db_path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| indexer_root.clone());
            let _indexer_lock = match calm_core::db::instance_lock::try_acquire(&lock_dir) {
                Some(lock) => lock,
                None => {
                    tracing::info!(
                        "Another calm serve process already owns indexing for this project — \
                         serving read-only for now; will take over automatically if that \
                         process exits"
                    );
                    // Best-effort: if a prior process already indexed this
                    // project, there's real data to serve immediately — don't
                    // leave `phase` stuck at its initial pre-Ready value this
                    // process will now never itself advance. If the DB is
                    // genuinely empty (everyone racing to index a brand new
                    // project simultaneously), leave `phase` as-is; it stays
                    // honestly non-Ready here, but reads still start reflecting
                    // real data as soon as the owning process writes it.
                    if let Ok(conn) = calm_core::db::conn::open_writer(&indexer_db_path) {
                        let existing_files: i64 = conn
                            .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                            .unwrap_or(0);
                        if existing_files > 0 {
                            *phase.write_ok() = calm_core::types::IndexingPhase::Ready;
                        }
                    }
                    // The lock only gates *writes* (new index rows, new embedding
                    // rows) — every process still needs its own live `Embedder`
                    // to embed *queries* at search time, and loading it is a
                    // pure, local, side-effect-free operation (vendored weights,
                    // zero network by default). Without this, every `calm serve`
                    // process that loses the race — i.e. every session opened
                    // against this project after the first one — would report
                    // `embeddings_status: "disabled"` forever, even though the DB
                    // the winner maintains already has real embeddings in it.
                    if semantic.enabled {
                        load_embedder_readonly(
                            &semantic,
                            &embedder,
                            &embed_status,
                            &last_embed_error,
                        );
                    }
                    // Poll (not a single indefinite blocking call) until the
                    // current owner exits and this process can take over —
                    // see `acquire_blocking_cancellable`'s doc comment for why
                    // polling is correct here: an OS `flock` wait has no
                    // interruption mechanism, so `cancel` can only ever be
                    // noticed between attempts, not via a wakeup.
                    match calm_core::db::instance_lock::acquire_blocking_cancellable(
                        &lock_dir, &cancel,
                    ) {
                        Ok(Some(lock)) => {
                            tracing::info!(
                                "Promoted to indexer/watcher owner — previous owner exited"
                            );
                            lock
                        }
                        Ok(None) => {
                            tracing::info!(
                                "Shutdown requested while waiting to become indexer/watcher \
                                 owner — exiting read-only without promoting"
                            );
                            return;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Could not wait to become indexer/watcher owner ({e}) — this \
                                 process will stay read-only for its whole lifetime"
                            );
                            return;
                        }
                    }
                }
            };
            *owns_indexer_lock.write_ok() = true;

            if let Ok(mut conn) = calm_core::db::conn::open_writer(&indexer_db_path) {
                let _ = calm_core::db::schema::init_db(&conn);

                // Use incremental reindex when the index already has data — avoids
                // a full re-parse of every file on every server restart.
                let existing_files: i64 = conn
                    .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                    .unwrap_or(0);

                enum IndexOutcome {
                    Ok,
                    Cancelled,
                    Err(String),
                }

                let index_outcome: IndexOutcome = if existing_files > 0 {
                    tracing::info!(
                        "Existing index found ({existing_files} files) — incremental reindex"
                    );
                    *phase.write_ok() = calm_core::types::IndexingPhase::Parsing;
                    match calm_core::indexer::pipeline::reindex_changed_cancellable(
                        &mut conn,
                        &indexer_root,
                        &cancel,
                    ) {
                        Ok(calm_core::indexer::pipeline::ReindexOutcome::Completed(summary)) => {
                            tracing::info!(
                                "Incremental reindex: {} changed, {} deleted",
                                summary.changed,
                                summary.deleted
                            );
                            *phase.write_ok() = calm_core::types::IndexingPhase::Ready;
                            IndexOutcome::Ok
                        }
                        Ok(calm_core::indexer::pipeline::ReindexOutcome::Cancelled) => {
                            IndexOutcome::Cancelled
                        }
                        Err(e) => {
                            tracing::error!(
                                "Incremental reindex failed, falling back to full: {e}"
                            );
                            match calm_core::indexer::pipeline::run_indexing_pipeline_cancellable(
                                &mut conn,
                                &indexer_root,
                                phase.clone(),
                                &cancel,
                            ) {
                                Ok(calm_core::indexer::pipeline::PipelineOutcome::Completed) => {
                                    IndexOutcome::Ok
                                }
                                Ok(calm_core::indexer::pipeline::PipelineOutcome::Cancelled) => {
                                    IndexOutcome::Cancelled
                                }
                                Err(e) => IndexOutcome::Err(e.to_string()),
                            }
                        }
                    }
                } else {
                    tracing::info!("No existing index — running full index");
                    match calm_core::indexer::pipeline::run_indexing_pipeline_cancellable(
                        &mut conn,
                        &indexer_root,
                        phase.clone(),
                        &cancel,
                    ) {
                        Ok(calm_core::indexer::pipeline::PipelineOutcome::Completed) => {
                            IndexOutcome::Ok
                        }
                        Ok(calm_core::indexer::pipeline::PipelineOutcome::Cancelled) => {
                            IndexOutcome::Cancelled
                        }
                        Err(e) => IndexOutcome::Err(e.to_string()),
                    }
                };

                // Shutdown requested mid-index: stop immediately rather than
                // continuing on to embeddings/SCIP-overlay/the watch loop —
                // exactly the sequence that used to keep this spawn_blocking
                // task (and therefore the whole process) alive well past
                // `ct.cancel()` having already fired.
                if matches!(index_outcome, IndexOutcome::Cancelled) {
                    tracing::info!("Background indexer stopped early — shutdown requested");
                    return;
                }

                let index_ok = match &index_outcome {
                    IndexOutcome::Ok => {
                        tracing::info!("Background indexing completed");
                        *last_index_error.write_ok() = None;
                        true
                    }
                    IndexOutcome::Cancelled => unreachable!("handled above"),
                    IndexOutcome::Err(e) => {
                        tracing::error!("Background indexer failed: {e}");
                        *phase.write_ok() = calm_core::types::IndexingPhase::Failed;
                        *last_index_error.write_ok() = Some(e.clone());
                        false
                    }
                };
                // Opt-in semantic embeddings, after the graph is built.
                if semantic.enabled {
                    bootstrap_embeddings(
                        &conn,
                        &semantic,
                        &embedder,
                        &embed_status,
                        &last_embed_error,
                    );
                }

                #[cfg(feature = "scip-overlay")]
                if index_ok {
                    // Drop the write lock this thread has been holding on `conn`
                    // before fanning out — the 8 language overlays below each
                    // open their own connection instead (see `scip_overlay`'s
                    // doc comment for why that's safe and not just tolerated).
                    drop(conn);
                    scip_overlay::run_all(&indexer_root, &indexer_db_path);
                }
                #[cfg(not(feature = "scip-overlay"))]
                let _ = index_ok;
            }
            // Watch for edits and incrementally reindex (and re-embed) until shutdown.
            watcher::run_watch_loop(
                indexer_root,
                indexer_db_path,
                watch_ct,
                watch_embedder,
                watch_coverage,
                watch_graph_mode,
                None,
            );
        });
        // Await (rather than discard) the indexer thread's handle so a panic
        // inside it — which would otherwise silently strand `phase` at
        // whatever it was mid-run, with nothing left to ever advance it —
        // gets reflected as `Failed` instead. Doesn't delay server startup:
        // this whole block is itself a detached `tokio::spawn`, so `serve`
        // continues to the transport below immediately either way.
        if let Err(join_err) = handle.await {
            tracing::error!("Background indexer thread panicked: {join_err}");
            *outer_phase.write_ok() = calm_core::types::IndexingPhase::Failed;
            *outer_last_index_error.write_ok() =
                Some(format!("indexer thread panicked: {join_err}"));
        }
    });

    Ok(Bootstrapped { server, ct })
}

/// Best-effort: reclaim WAL space on a clean shutdown rather than leaving
/// it to sit around until SQLite's own passive auto-checkpoint happens to
/// fire — which never runs at all while any connection (including a
/// lock-loser process that never got a shutdown signal — see the SIGTERM
/// handler above) still holds an old read snapshot open. Any process may
/// request a checkpoint regardless of indexer-lock ownership; it's a
/// WAL-level operation, harmless and idempotent when there's nothing to
/// reclaim. TRUNCATE can only partially complete ("busy") if another
/// connection is concurrently active, which is not an error — a missed or
/// partial checkpoint here just means the next one (from this process or
/// another) reclaims the rest. This cannot help against SIGKILL, which no
/// userspace code can intercept on any OS; that gap is inherent, not a bug.
///
/// Called exactly once per server lifetime: at the end of the single stdio
/// session for `serve_stdio_with_preset`, or once at daemon accept-loop exit
/// for a daemon — never per-connection, since the checkpoint belongs to the
/// shared DB writer's lifetime, not any individual session's.
pub(crate) fn shutdown_and_checkpoint(db_path: &std::path::Path) {
    if let Ok(conn) = calm_core::db::conn::open_writer(db_path) {
        match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        }) {
            Ok((busy, log_frames, checkpointed)) => tracing::info!(
                "WAL checkpoint on shutdown: busy={busy} log_frames={log_frames} checkpointed={checkpointed}"
            ),
            Err(e) => tracing::warn!("WAL checkpoint on shutdown failed (non-fatal): {e}"),
        }
    }
}

pub async fn serve_stdio_with_preset(
    project_root: PathBuf,
    db_path: PathBuf,
    preset: String,
) -> Result<()> {
    let Bootstrapped { server, ct } = bootstrap(project_root, db_path.clone(), preset).await?;

    let transport = stdio();
    let service: rmcp::service::RunningService<rmcp::RoleServer, _> =
        rmcp::service::serve_server_with_ct(server, transport, ct)
            .await
            .map_err(|e| anyhow::anyhow!("Server init error: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    shutdown_and_checkpoint(&db_path);

    tracing::info!("Server shut down cleanly");
    Ok(())
}
/// call: the configured model is the vendored default, that vendored asset
/// is unusable (see `calm_core::embedding::default_vendored_asset_unusable`),
/// and the config has not opted into a network fallback. Pulled out as a
/// pure function (taking the three already-evaluated booleans, not
/// re-deriving them) so the policy logic is unit-testable without touching
/// the real vendored asset or the network — see the `tests` module below.
fn embeddings_blocked_by_offline_policy(
    is_default_model: bool,
    vendored_asset_unusable: bool,
    allow_network_fallback: bool,
) -> bool {
    is_default_model && vendored_asset_unusable && !allow_network_fallback
}

/// Load the embedding model, create the vector table, embed all symbols, and
/// publish the model + status. Runs on the indexer thread after the graph is
/// built (and again from `indexing_status`'s `retry_embeddings` after a prior
/// failure). A no-op surface when the `embeddings` feature is off (load fails).
/// Load the configured embedding model into `embedder` + `status`, honoring
/// the offline-fallback policy. This is the safe, side-effect-free half of
/// embeddings bootstrap — no DB writes, no network unless the vendored asset
/// is broken AND `allow_network_fallback` already consented to a download.
/// Every `calm serve` process for a given project can and should run this
/// independently, regardless of which one won the indexer lock in
/// `serve_stdio_with_preset`: each process needs its own live `Embedder` to
/// embed *queries* at search time (the winner's DB rows are shared, an
/// in-memory model instance is not) — only writing new embedding rows to
/// that shared DB needs to stay exclusive to the lock owner. Returns the
/// loaded model on success, having left `status` at `Downloading` (still
/// mid-flight — the caller decides what "done" means: the lock owner still
/// has table/row work left, `load_embedder_readonly` below is done
/// immediately). Returns `None` on failure, having already set `status` and
/// `last_embed_error`.
fn load_embedder_model(
    semantic: &SemanticSearchConfig,
    embedder: &EmbedderHandle,
    status: &Arc<RwLock<EmbedStatus>>,
    last_embed_error: &Arc<RwLock<Option<String>>>,
) -> Option<Arc<Embedder>> {
    *status.write_ok() = EmbedStatus::Downloading;
    *last_embed_error.write_ok() = None;
    if embeddings_blocked_by_offline_policy(
        semantic.model == calm_core::embedding::DEFAULT_MODEL_ID,
        calm_core::embedding::default_vendored_asset_unusable(),
        semantic.allow_network_fallback,
    ) {
        let msg = "Vendored embedding model is an unresolved Git LFS pointer and \
             semantic_search.allow_network_fallback is false — embeddings unavailable this \
             run. Run `git lfs pull` to fix the vendored asset, or set \
             allow_network_fallback=true to download it instead, then retry_embeddings."
            .to_string();
        tracing::warn!("{msg}");
        *status.write_ok() = EmbedStatus::OfflineUnavailable;
        *last_embed_error.write_ok() = Some(msg);
        return None;
    }
    if semantic.model == calm_core::embedding::DEFAULT_MODEL_ID {
        tracing::info!(
            "Loading embedding model `{}` (vendored in the binary, no network needed)...",
            semantic.model
        );
    } else {
        tracing::info!(
            "Loading embedding model `{}` (may download from the HuggingFace Hub on first run)...",
            semantic.model
        );
    }
    let model = match Embedder::load(&semantic.model, semantic.dimensions) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            let msg = format!("Embedding model load failed: {e}");
            tracing::error!("{msg}");
            *status.write_ok() = EmbedStatus::Failed;
            *last_embed_error.write_ok() = Some(msg);
            return None;
        }
    };
    *embedder.write_ok() = Some(model.clone());
    Some(model)
}

/// Load the embedding model, create the vector tables, embed all pending
/// symbols/chunks, and publish `Ready`. Only the process holding the indexer
/// lock (see `serve_stdio_with_preset`) should call this — it writes to the
/// shared DB. Runs on the indexer thread after the graph is built (and again
/// from `indexing_status`'s `retry_embeddings` after a prior failure, when
/// this process is the lock owner — see `retry_embeddings_if_failed`). A
/// no-op surface when the `embeddings` feature is off (`Embedder::load`
/// always fails in that build).
pub fn bootstrap_embeddings(
    conn: &rusqlite::Connection,
    semantic: &SemanticSearchConfig,
    embedder: &EmbedderHandle,
    status: &Arc<RwLock<EmbedStatus>>,
    last_embed_error: &Arc<RwLock<Option<String>>>,
) {
    let Some(model) = load_embedder_model(semantic, embedder, status, last_embed_error) else {
        return;
    };
    // `model.dim()` (real, probed at load time) rather than
    // `semantic.dimensions` (config, possibly stale) — see
    // `Embedder::load` and `create_embedding_table`'s self-heal.
    if let Err(e) = calm_core::embedding::create_embedding_table(conn, model.dim()) {
        let msg = format!("Embedding table creation failed: {e}");
        tracing::error!("{msg}");
        *status.write_ok() = EmbedStatus::Failed;
        *last_embed_error.write_ok() = Some(msg);
        return;
    }
    if let Err(e) = calm_core::embedding::create_chunk_embedding_table(conn, model.dim()) {
        let msg = format!("Chunk embedding table creation failed: {e}");
        tracing::error!("{msg}");
        *status.write_ok() = EmbedStatus::Failed;
        *last_embed_error.write_ok() = Some(msg);
        return;
    }
    *status.write_ok() = EmbedStatus::Embedding;
    match calm_core::embedding::embed_pending(conn, model.as_ref()) {
        Ok(n) => tracing::info!("Embedded {n} symbols"),
        Err(e) => {
            let msg = format!("Embedding failed: {e}");
            tracing::error!("{msg}");
            *status.write_ok() = EmbedStatus::Failed;
            *last_embed_error.write_ok() = Some(msg);
            return;
        }
    }
    // Layer 2: code-body chunks (see indexer::chunker). Same model, same
    // failure handling as the symbol layer above — both draw on the same
    // connection/model, so a real failure here is as fatal as there.
    match calm_core::embedding::embed_pending_chunks(conn, model.as_ref()) {
        Ok(n) => tracing::info!("Embedded {n} code chunks"),
        Err(e) => {
            let msg = format!("Chunk embedding failed: {e}");
            tracing::error!("{msg}");
            *status.write_ok() = EmbedStatus::Failed;
            *last_embed_error.write_ok() = Some(msg);
            return;
        }
    }
    *status.write_ok() = EmbedStatus::Ready;
    tracing::info!("Embeddings ready");
}

/// Lightweight embeddings bootstrap for a `calm serve` process that lost the
/// indexer-lock race (see the `else` branch in `serve_stdio_with_preset`, and
/// `retry_embeddings_if_failed` for the same case on a manual retry): loads
/// this process's own `Embedder` so query-time semantic search works here
/// too, without ever touching the DB — creating tables and writing new
/// embedding rows stays the lock owner's job alone (`bootstrap_embeddings`
/// above). Before this existed, every `calm serve` process past the first one
/// opened against a given project reported `embeddings_status: "disabled"`
/// forever, even once the winning process had already embedded everything.
/// Caller is expected to have already checked `semantic.enabled`.
pub(crate) fn load_embedder_readonly(
    semantic: &SemanticSearchConfig,
    embedder: &EmbedderHandle,
    status: &Arc<RwLock<EmbedStatus>>,
    last_embed_error: &Arc<RwLock<Option<String>>>,
) {
    if load_embedder_model(semantic, embedder, status, last_embed_error).is_some() {
        *status.write_ok() = EmbedStatus::Ready;
        tracing::info!(
            "Embedder loaded for query-time semantic search (another process owns \
             indexing/embedding writes for this project)"
        );
    }
}

pub fn default_db_path(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".calm").join("index.db")
}

pub fn doctor(project_root: &std::path::Path) -> Result<()> {
    use calm_core::db::schema::init_db;

    println!("Build: {}", calm_core::BUILD_INFO);
    match current_git_head_short(project_root) {
        Some(head) if calm_core::BUILD_INFO.starts_with(&head) => {
            println!("  matches current HEAD ({head}) — up to date");
        }
        Some(head) => {
            println!(
                "  \u{26a0} STALE — this binary was built from a different commit than \
                 current HEAD ({head}). A running `calm serve` process keeps using whatever \
                 was loaded at its own start, even after a fresh `cargo build` replaces \
                 the file on disk — restart the server process to pick it up. \
                 (`cargo build -p calm-cli` then restart, or reconnect your MCP client.)"
            );
        }
        None => {
            println!("  (not a git checkout, or git unavailable — can't check freshness)");
        }
    }

    let db_path = default_db_path(project_root);

    println!("Project root: {}", project_root.display());
    println!(
        "  exists: {}",
        if project_root.exists() { "YES" } else { "NO" }
    );

    println!("DB path: {}", db_path.display());
    if db_path.exists() {
        let conn = calm_core::db::conn::open_writer(&db_path)?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        println!("  symbols: {count}");
        let file_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))?;
        println!("  files indexed: {file_count}");

        match calm_core::fitness::prune_old_snapshots(&conn) {
            Ok(pruned) => println!(
                "  metrics history: pruned {pruned} snapshot(s) older than {} days",
                calm_core::fitness::METRICS_RETENTION_DAYS
            ),
            Err(e) => println!("  metrics history: prune failed: {e}"),
        }

        println!("  status: OK");
    } else {
        println!("  status: NOT FOUND (run 'calm index' first)");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = calm_core::db::conn::open_writer(&db_path)?;
        init_db(&conn)?;
        println!("  created empty DB");
    }

    let grammars = ["python", "typescript", "javascript", "java", "rust", "go"];
    println!("Tree-sitter grammars: {}", grammars.join(", "));
    println!("  status: BUNDLED (compiled in)");

    if let Ok(output) = std::process::Command::new("git").arg("--version").output() {
        if output.status.success() {
            let ver = String::from_utf8_lossy(&output.stdout);
            println!("Git: {}", ver.trim());
        } else {
            println!("Git: NOT FOUND (hotspots/diff_impact will be limited)");
        }
    } else {
        println!("Git: NOT FOUND (hotspots/diff_impact will be limited)");
    }

    let hooks_status = calm_core::hooks::check_hooks_status(project_root);
    println!("{}", hooks_status.summary());
    if hooks_status.disabled_by_env {
        println!("  (CALM_HOOKS_DISABLE=1 is set in this shell -- unset it to re-enable)");
    }

    println!("\nAll checks passed.");
    Ok(())
}

/// Short (12-char) git HEAD SHA for `project_root`, matching the format
/// `calm_core::BUILD_INFO` uses (`git rev-parse --short=12 HEAD` at build
/// time) so the two can be compared as plain strings. `None` when this
/// isn't a git checkout or git isn't available — not an error, just means
/// freshness can't be checked.
pub(crate) fn current_git_head_short(project_root: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::embeddings_blocked_by_offline_policy as blocked;

    /// All three conditions must hold — a custom (non-default) model, a fine
    /// vendored asset, or an allowed network fallback each independently
    /// mean "don't block", only their conjunction does.
    #[test]
    fn embeddings_blocked_by_offline_policy_only_when_all_three_conditions_hold() {
        assert!(
            blocked(true, true, false),
            "default model + unusable vendored asset + fallback disabled -> blocked"
        );
        assert!(
            !blocked(false, true, false),
            "a custom model was never going to use the vendored asset — unaffected"
        );
        assert!(
            !blocked(true, false, false),
            "vendored asset is fine — no fallback ever needed"
        );
        assert!(
            !blocked(true, true, true),
            "fallback explicitly allowed — proceed to Embedder::load's own fallback"
        );
        assert!(!blocked(false, false, false));
        assert!(!blocked(false, false, true));
        assert!(!blocked(false, true, true));
        assert!(!blocked(true, false, true));
    }
}
