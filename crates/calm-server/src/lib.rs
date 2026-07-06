pub mod telemetry;
pub mod tools;
pub mod watcher;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use calm_core::config::SemanticSearchConfig;
use calm_core::embedding::Embedder;
use calm_core::types::EmbedStatus;
use rmcp::transport::io::stdio;
use tokio_util::sync::CancellationToken;

use tools::CodeIntelligenceServer;

/// Shared handle to the loaded embedder, written by the indexer, read by tools.
pub type EmbedderHandle = Arc<RwLock<Option<Arc<Embedder>>>>;

/// Shared handle to loaded coverage data, reloaded in place by the file
/// watcher whenever the coverage file itself changes on disk.
pub type CoverageHandle = Arc<RwLock<calm_core::analysis::coverage::CoverageData>>;

pub async fn serve_stdio(project_root: PathBuf, db_path: PathBuf) -> Result<()> {
    serve_stdio_with_preset(project_root, db_path, "full".into()).await
}

pub async fn serve_stdio_with_preset(
    project_root: PathBuf,
    db_path: PathBuf,
    preset: String,
) -> Result<()> {
    calm_core::gitignore::ensure_gitignore(&project_root)?;

    let server =
        CodeIntelligenceServer::new_with_preset(project_root.clone(), db_path.clone(), preset)?;
    let ct = CancellationToken::new();
    let ct_clone = ct.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received SIGINT, shutting down");
        ct_clone.cancel();
    });

    let semantic = calm_core::config::load_config(&project_root)
        .map(|c| c.semantic_search)
        .unwrap_or_default();

    let indexer_db_path = db_path.clone();
    let indexer_root = project_root.clone();
    let watch_ct = ct.clone();
    let phase = server.phase_handle();
    let last_index_error = server.last_index_error_handle();
    let embedder = server.embedder_handle();
    let embed_status = server.embed_status_handle();
    let coverage = server.coverage_handle();
    let watch_embedder = embedder.clone();
    let watch_coverage = coverage.clone();
    // Kept outside the `spawn_blocking` closure below so a panic there (caught
    // via the awaited `JoinHandle`) still has a handle to report through —
    // `phase`/`last_index_error` themselves are moved into that closure.
    let outer_phase = phase.clone();
    let outer_last_index_error = last_index_error.clone();
    tokio::spawn(async move {
        let handle = tokio::task::spawn_blocking(move || {
            tracing::info!("Background indexer thread started");

            // Every `calm serve` process used to spawn its own indexer+watcher
            // against the same shared DB — harmless with one process, but N
            // concurrent processes on the same project_root (e.g. multiple
            // editor/MCP-client sessions) meant N redundant reindex loops
            // racing each other, mitigated only by `open_writer`'s
            // `busy_timeout`, never prevented. Only the process that wins
            // this advisory lock runs the indexer+watcher below; a loser
            // still serves tool calls read-only against the DB the winner
            // keeps fresh.
            let lock_dir = indexer_db_path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| indexer_root.clone());
            let Some(_indexer_lock) = calm_core::db::instance_lock::try_acquire(&lock_dir) else {
                tracing::info!(
                    "Another calm serve process already owns indexing for this project — skipping redundant indexer/watcher"
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
                        *phase.write().unwrap() = calm_core::types::IndexingPhase::Ready;
                    }
                }
                return;
            };

            if let Ok(mut conn) = calm_core::db::conn::open_writer(&indexer_db_path) {
                let _ = calm_core::db::schema::init_db(&conn);

                // Use incremental reindex when the index already has data — avoids
                // a full re-parse of every file on every server restart.
                let existing_files: i64 = conn
                    .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                    .unwrap_or(0);

                let index_result: Result<(), String> = if existing_files > 0 {
                    tracing::info!(
                        "Existing index found ({existing_files} files) — incremental reindex"
                    );
                    *phase.write().unwrap() = calm_core::types::IndexingPhase::Parsing;
                    match calm_core::indexer::pipeline::reindex_changed(&mut conn, &indexer_root) {
                        Ok(summary) => {
                            tracing::info!(
                                "Incremental reindex: {} changed, {} deleted",
                                summary.changed,
                                summary.deleted
                            );
                            *phase.write().unwrap() = calm_core::types::IndexingPhase::Ready;
                            Ok(())
                        }
                        Err(e) => {
                            tracing::error!(
                                "Incremental reindex failed, falling back to full: {e}"
                            );
                            calm_core::indexer::pipeline::run_indexing_pipeline(
                                &mut conn,
                                &indexer_root,
                                phase.clone(),
                            )
                            .map_err(|e| e.to_string())
                        }
                    }
                } else {
                    tracing::info!("No existing index — running full index");
                    calm_core::indexer::pipeline::run_indexing_pipeline(
                        &mut conn,
                        &indexer_root,
                        phase.clone(),
                    )
                    .map_err(|e| e.to_string())
                };

                let index_ok = match &index_result {
                    Ok(()) => {
                        tracing::info!("Background indexing completed");
                        *last_index_error.write().unwrap() = None;
                        true
                    }
                    Err(e) => {
                        tracing::error!("Background indexer failed: {e}");
                        *phase.write().unwrap() = calm_core::types::IndexingPhase::Failed;
                        *last_index_error.write().unwrap() = Some(e.clone());
                        false
                    }
                };
                // Opt-in semantic embeddings, after the graph is built.
                if semantic.enabled {
                    bootstrap_embeddings(&conn, &semantic, &embedder, &embed_status);
                }

                #[cfg(feature = "scip-overlay")]
                if index_ok {
                    let rust_cfg = calm_core::config::load_config(&indexer_root)
                        .map(|c| c.rust)
                        .unwrap_or_default();
                    let dirty = calm_core::scip::rust_source_dirty_keys(&conn);
                    match calm_core::scip::run_overlay(&conn, &indexer_root, &rust_cfg, &dirty) {
                        Ok(stats) if stats.upgraded > 0 || stats.ruled_out > 0 => {
                            // caller_count was computed by rebuild_graph before this
                            // overlay flipped edge_confidence/ruled_out_by_scip on
                            // some edges — refresh it or it goes stale again
                            // immediately relative to the columns it's filtered on.
                            if let Err(e) =
                                calm_core::indexer::pipeline::refresh_caller_counts(&conn)
                            {
                                tracing::warn!(
                                    "caller_count refresh after SCIP overlay failed: {e}"
                                );
                            }
                            tracing::info!(
                                "SCIP overlay: {} edges upgraded, {} fan-out siblings ruled out",
                                stats.upgraded,
                                stats.ruled_out
                            );
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("SCIP overlay error (base graph intact): {e}"),
                    }
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
            *outer_phase.write().unwrap() = calm_core::types::IndexingPhase::Failed;
            *outer_last_index_error.write().unwrap() =
                Some(format!("indexer thread panicked: {join_err}"));
        }
    });

    let transport = stdio();
    let service: rmcp::service::RunningService<rmcp::RoleServer, _> =
        rmcp::service::serve_server_with_ct(server, transport, ct)
            .await
            .map_err(|e: std::io::Error| anyhow::anyhow!("Server init error: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!("Server shut down cleanly");
    Ok(())
}
/// True when semantic search should stop before ever attempting a network
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
pub fn bootstrap_embeddings(
    conn: &rusqlite::Connection,
    semantic: &SemanticSearchConfig,
    embedder: &EmbedderHandle,
    status: &Arc<RwLock<EmbedStatus>>,
) {
    *status.write().unwrap() = EmbedStatus::Downloading;
    if embeddings_blocked_by_offline_policy(
        semantic.model == calm_core::embedding::DEFAULT_MODEL_ID,
        calm_core::embedding::default_vendored_asset_unusable(),
        semantic.allow_network_fallback,
    ) {
        tracing::warn!(
            "Vendored embedding model is an unresolved Git LFS pointer and \
             semantic_search.allow_network_fallback is false — embeddings unavailable this \
             run. Run `git lfs pull` to fix the vendored asset, or set \
             allow_network_fallback=true to download it instead, then retry_embeddings."
        );
        *status.write().unwrap() = EmbedStatus::OfflineUnavailable;
        return;
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
            tracing::error!("Embedding model load failed: {e}");
            *status.write().unwrap() = EmbedStatus::Failed;
            return;
        }
    };
    // `model.dim()` (real, probed at load time) rather than
    // `semantic.dimensions` (config, possibly stale) — see
    // `Embedder::load` and `create_embedding_table`'s self-heal.
    if let Err(e) = calm_core::embedding::create_embedding_table(conn, model.dim()) {
        tracing::error!("Embedding table creation failed: {e}");
        *status.write().unwrap() = EmbedStatus::Failed;
        return;
    }
    if let Err(e) = calm_core::embedding::create_chunk_embedding_table(conn, model.dim()) {
        tracing::error!("Chunk embedding table creation failed: {e}");
        *status.write().unwrap() = EmbedStatus::Failed;
        return;
    }
    *status.write().unwrap() = EmbedStatus::Embedding;
    match calm_core::embedding::embed_pending(conn, model.as_ref()) {
        Ok(n) => tracing::info!("Embedded {n} symbols"),
        Err(e) => {
            tracing::error!("Embedding failed: {e}");
            *status.write().unwrap() = EmbedStatus::Failed;
            return;
        }
    }
    // Layer 2: code-body chunks (see indexer::chunker). Same model, same
    // failure handling as the symbol layer above — both draw on the same
    // connection/model, so a real failure here is as fatal as there.
    match calm_core::embedding::embed_pending_chunks(conn, model.as_ref()) {
        Ok(n) => tracing::info!("Embedded {n} code chunks"),
        Err(e) => {
            tracing::error!("Chunk embedding failed: {e}");
            *status.write().unwrap() = EmbedStatus::Failed;
            return;
        }
    }
    *embedder.write().unwrap() = Some(model);
    *status.write().unwrap() = EmbedStatus::Ready;
    tracing::info!("Embeddings ready");
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

    println!("\nAll checks passed.");
    Ok(())
}

/// Short (12-char) git HEAD SHA for `project_root`, matching the format
/// `calm_core::BUILD_INFO` uses (`git rev-parse --short=12 HEAD` at build
/// time) so the two can be compared as plain strings. `None` when this
/// isn't a git checkout or git isn't available — not an error, just means
/// freshness can't be checked.
fn current_git_head_short(project_root: &std::path::Path) -> Option<String> {
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
