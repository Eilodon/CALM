pub mod telemetry;
pub mod tools;
pub mod watcher;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use ci_core::config::SemanticSearchConfig;
use ci_core::embedding::Embedder;
use ci_core::types::EmbedStatus;
use rmcp::transport::io::stdio;
use tokio_util::sync::CancellationToken;

use tools::CodeIntelligenceServer;

/// Shared handle to the loaded embedder, written by the indexer, read by tools.
pub type EmbedderHandle = Arc<RwLock<Option<Arc<Embedder>>>>;

pub async fn serve_stdio(project_root: PathBuf, db_path: PathBuf) -> Result<()> {
    serve_stdio_with_preset(project_root, db_path, "full".into()).await
}

pub async fn serve_stdio_with_preset(
    project_root: PathBuf,
    db_path: PathBuf,
    preset: String,
) -> Result<()> {
    // Register the sqlite-vec extension before any connection is opened (no-op
    // unless built with the `embeddings` feature).
    ci_core::embedding::register_extension();

    ci_core::gitignore::ensure_gitignore(&project_root)?;

    let server =
        CodeIntelligenceServer::new_with_preset(project_root.clone(), db_path.clone(), preset)?;
    let ct = CancellationToken::new();
    let ct_clone = ct.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received SIGINT, shutting down");
        ct_clone.cancel();
    });

    let semantic = ci_core::config::load_config(&project_root)
        .map(|c| c.semantic_search)
        .unwrap_or_default();

    let indexer_db_path = db_path.clone();
    let indexer_root = project_root.clone();
    let watch_ct = ct.clone();
    let phase = server.phase_handle();
    let embedder = server.embedder_handle();
    let embed_status = server.embed_status_handle();
    let watch_embedder = embedder.clone();
    tokio::task::spawn_blocking(move || {
        tracing::info!("Background indexer thread started");
        if let Ok(mut conn) = rusqlite::Connection::open(&indexer_db_path) {
            let _ = ci_core::db::schema::init_db(&conn);

            // Use incremental reindex when the index already has data — avoids
            // a full re-parse of every file on every server restart.
            let existing_files: i64 = conn
                .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                .unwrap_or(0);

            let index_ok = if existing_files > 0 {
                tracing::info!(
                    "Existing index found ({existing_files} files) — incremental reindex"
                );
                *phase.write().unwrap() = ci_core::types::IndexingPhase::Parsing;
                match ci_core::indexer::pipeline::reindex_changed(&mut conn, &indexer_root) {
                    Ok(summary) => {
                        tracing::info!(
                            "Incremental reindex: {} changed, {} deleted",
                            summary.changed,
                            summary.deleted
                        );
                        *phase.write().unwrap() = ci_core::types::IndexingPhase::Ready;
                        true
                    }
                    Err(e) => {
                        tracing::error!("Incremental reindex failed, falling back to full: {e}");
                        ci_core::indexer::pipeline::run_indexing_pipeline(
                            &mut conn,
                            &indexer_root,
                            phase.clone(),
                        )
                        .is_ok()
                    }
                }
            } else {
                tracing::info!("No existing index — running full index");
                ci_core::indexer::pipeline::run_indexing_pipeline(
                    &mut conn,
                    &indexer_root,
                    phase.clone(),
                )
                .is_ok()
            };

            if index_ok {
                tracing::info!("Background indexing completed");
            } else {
                tracing::error!("Background indexer failed");
                // Reset to Scanning so callers don't see BuildingEdges forever on failure.
                *phase.write().unwrap() = ci_core::types::IndexingPhase::Scanning;
            }
            // Opt-in semantic embeddings, after the graph is built.
            if semantic.enabled {
                bootstrap_embeddings(&conn, &semantic, &embedder, &embed_status);
            }
        }
        // Watch for edits and incrementally reindex (and re-embed) until shutdown.
        watcher::run_watch_loop(indexer_root, indexer_db_path, watch_ct, watch_embedder);
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
    tracing::info!(
        "Loading embedding model `{}` (may download ~30 MB on first run)...",
        semantic.model
    );
    let model = match Embedder::load(&semantic.model, semantic.dimensions) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            tracing::error!("Embedding model load failed: {e}");
            *status.write().unwrap() = EmbedStatus::Failed;
            return;
        }
    };
    if let Err(e) = ci_core::embedding::create_embedding_table(conn, semantic.dimensions) {
        tracing::error!("Embedding table creation failed: {e}");
        *status.write().unwrap() = EmbedStatus::Failed;
        return;
    }
    if let Err(e) = ci_core::embedding::create_chunk_embedding_table(conn, semantic.dimensions) {
        tracing::error!("Chunk embedding table creation failed: {e}");
        *status.write().unwrap() = EmbedStatus::Failed;
        return;
    }
    *status.write().unwrap() = EmbedStatus::Embedding;
    match ci_core::embedding::embed_pending(conn, model.as_ref()) {
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
    match ci_core::embedding::embed_pending_chunks(conn, model.as_ref()) {
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
    project_root.join(".codeindex").join("index.db")
}

pub fn doctor(project_root: &std::path::Path) -> Result<()> {
    use ci_core::db::schema::init_db;
    use rusqlite::Connection;

    let db_path = default_db_path(project_root);

    println!("Project root: {}", project_root.display());
    println!(
        "  exists: {}",
        if project_root.exists() { "YES" } else { "NO" }
    );

    println!("DB path: {}", db_path.display());
    if db_path.exists() {
        let conn = Connection::open(&db_path)?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        println!("  symbols: {count}");
        let file_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))?;
        println!("  files indexed: {file_count}");

        match ci_core::fitness::prune_old_snapshots(&conn) {
            Ok(pruned) => println!(
                "  metrics history: pruned {pruned} snapshot(s) older than {} days",
                ci_core::fitness::METRICS_RETENTION_DAYS
            ),
            Err(e) => println!("  metrics history: prune failed: {e}"),
        }

        println!("  status: OK");
    } else {
        println!("  status: NOT FOUND (run 'ci index' first)");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
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
