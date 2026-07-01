use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ci",
    about = "Code Intelligence — MCP server for codebase analysis"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server over stdio
    Serve {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Database file path
        #[arg(long)]
        db_path: Option<PathBuf>,
        /// Tool preset to register. If not provided, uses preset from config.json (default: "full").
        #[arg(long)]
        preset: Option<String>,
    },
    /// One-shot index of the project
    Index {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
    /// Validate config, DB, tree-sitter, git
    Doctor {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
    /// Check codebase fitness against thresholds (exits 1 if any threshold exceeded)
    FitnessCheck {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Path to thresholds.toml (uses defaults if not provided)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Output results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Initialize .codeindex/ config for a project
    Init {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            project_root,
            db_path,
            preset,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db = db_path.unwrap_or_else(|| ci_server::default_db_path(&root));
            // CLI flag takes precedence; fall back to config.json value (default: "full").
            // Propagate (not swallow) load errors here — an invalid config.json (bad
            // JSON, or an unrecognized preset) should fail server startup loudly
            // rather than silently degrade to defaults.
            let config = ci_core::config::load_config(&root)?;
            let effective_preset = preset.unwrap_or_else(|| config.preset.clone());
            tracing::info!(
                "Starting MCP server for {} (preset={})",
                root.display(),
                effective_preset
            );
            ci_server::serve_stdio_with_preset(root, db, effective_preset).await?;
        }
        Commands::Index { project_root } => {
            let root = std::fs::canonicalize(&project_root)?;
            tracing::info!("Indexing {}", root.display());
            // Register sqlite-vec before opening the connection (no-op unless the
            // `embeddings` feature is built in).
            ci_core::embedding::register_extension();
            let db_path = ci_server::default_db_path(&root);
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut conn = rusqlite::Connection::open(&db_path)?;
            ci_core::db::schema::init_db(&conn)?;
            let phase = std::sync::Arc::new(std::sync::RwLock::new(
                ci_core::types::IndexingPhase::Scanning,
            ));
            ci_core::indexer::pipeline::run_indexing_pipeline(&mut conn, &root, phase)?;
            let symbol_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
            let file_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))?;
            tracing::info!("Indexing complete: {file_count} files, {symbol_count} symbols");
            println!("Indexed {file_count} files, {symbol_count} symbols.");

            // Semantic embeddings — active when `semantic_search.enabled: true` in
            // config.json and compiled with `--features embeddings`.
            let semantic = ci_core::config::load_config(&root)
                .map(|c| c.semantic_search)
                .unwrap_or_default();
            if semantic.enabled {
                print!("Building semantic index...");
                std::io::Write::flush(&mut std::io::stdout()).ok();
                match ci_core::embedding::Embedder::load(&semantic.model, semantic.dimensions) {
                    Ok(embedder) => {
                        ci_core::embedding::create_embedding_table(&conn, semantic.dimensions)?;
                        let n = ci_core::embedding::embed_pending(&conn, &embedder)?;
                        ci_core::embedding::create_chunk_embedding_table(
                            &conn,
                            semantic.dimensions,
                        )?;
                        let nc = ci_core::embedding::embed_pending_chunks(&conn, &embedder)?;
                        println!(" {n} symbols, {nc} code chunks embedded.");
                    }
                    Err(e) => eprintln!("\nEmbeddings skipped: {e}"),
                }
            } else {
                // When the feature is compiled in but not enabled in config, nudge the user.
                #[cfg(feature = "embeddings")]
                println!(
                    "Tip: semantic search is available — add \
                    {{\"semantic_search\":{{\"enabled\":true}}}} to config.json to activate it."
                );
            }
        }
        Commands::Doctor { project_root } => {
            let root = std::fs::canonicalize(&project_root)?;
            ci_server::doctor(&root)?;
        }
        Commands::FitnessCheck {
            project_root,
            config,
            json,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db_path = ci_server::default_db_path(&root);

            let thresholds = ci_core::fitness::load_thresholds(config.as_deref())?;

            let conn = rusqlite::Connection::open(&db_path)
                .unwrap_or_else(|_| rusqlite::Connection::open_in_memory().expect("in-memory DB"));
            ci_core::db::schema::init_db(&conn)?;

            let coverage = ci_core::analysis::coverage::load_coverage(&root);
            let result = ci_core::fitness::run_fitness_check(&conn, &thresholds, &root, &coverage)?;

            // Record today's metrics for later trend comparison (edit_context's
            // `trend` field). Rounded to the day so repeated same-day CI runs
            // collapse onto one row via the UNIQUE(qualified_name, snapshot_at)
            // constraint instead of growing the table on every run. Best-effort:
            // a snapshot failure shouldn't fail the fitness gate itself.
            let snapshot_at = ci_core::fitness::today_utc_date();
            if let Err(e) = ci_core::fitness::snapshot_metrics(&conn, &snapshot_at) {
                tracing::warn!("Failed to snapshot symbol metrics history: {e}");
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Fitness check — {}",
                    if result.passed { "PASS" } else { "FAIL" }
                );
                println!();
                for check in &result.checks {
                    let status = if check.passed { "✓" } else { "✗" };
                    println!("  {status} {}", check.message);
                }
                println!();
                if result.passed {
                    println!("All checks passed.");
                } else {
                    let failed: Vec<&str> = result
                        .checks
                        .iter()
                        .filter(|c| !c.passed)
                        .map(|c| c.metric.as_str())
                        .collect();
                    println!("Failed checks: {}", failed.join(", "));
                }
            }

            if !result.passed {
                std::process::exit(1);
            }
        }
        Commands::Init { project_root } => {
            let root = if project_root.exists() {
                std::fs::canonicalize(&project_root)?
            } else {
                project_root.clone()
            };

            let codeindex_dir = root.join(".codeindex");
            std::fs::create_dir_all(&codeindex_dir)?;

            let config_path = codeindex_dir.join("config.json");
            if config_path.exists() {
                println!("Config already exists at {}", config_path.display());
                println!("Remove it first if you want to reset to defaults.");
            } else {
                std::fs::write(&config_path, ci_core::config::default_config_json())?;
                println!("Created {}", config_path.display());
            }

            println!();
            println!("Next steps:");
            println!(
                "  ci index  --project-root {}  # build the index",
                root.display()
            );
            println!(
                "  ci serve  --project-root {}  # start MCP server",
                root.display()
            );
        }
    }

    Ok(())
}
