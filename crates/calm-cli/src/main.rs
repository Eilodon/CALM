use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "calm",
    about = "CALM — Coding Agent Liveness Map, an MCP server for codebase analysis",
    version
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
        /// Listen on a Unix domain socket instead of stdio, running as a
        /// shared daemon serving many `calm connect` forwarders off one
        /// background indexer/watcher/embedder (ADR-0005), e.g.
        /// `--listen unix:.calm/daemon.sock`. Opt-in: omitting this flag
        /// keeps today's one-process-per-client stdio behavior unchanged.
        /// Unix-only (errors at runtime on other platforms).
        #[arg(long)]
        listen: Option<String>,
    },
    /// Thin forwarder: connect to (or spawn, if none is live or the live
    /// one is a stale build) the shared daemon for `project_root`, then
    /// relay stdin<->socket verbatim — no MCP/JSON-RPC parsing here at all.
    /// This is what a launcher script points an MCP client's stdio at
    /// instead of `calm serve` to get the daemon's N-processes-collapse-to-
    /// one behavior (ADR-0005). Unix-only.
    #[cfg(unix)]
    Connect {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Database file path — forwarded to the daemon only if this
        /// `connect` call is the one that spawns it (a live daemon already
        /// running keeps whatever it was originally started with; this
        /// can't retroactively change it). Same semantics as `serve
        /// --db-path`.
        #[arg(long)]
        db_path: Option<PathBuf>,
        /// Tool preset — same forwarding caveat as `--db-path` above. If
        /// omitted, the spawned daemon resolves its own default from
        /// config.json exactly as a direct `calm serve` would.
        #[arg(long)]
        preset: Option<String>,
    },
    /// One-shot index of the project
    Index {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Ingest a pre-built `.scip` index file instead of probing for and
        /// running any external indexer binary (P2.6) — the standard way to
        /// get formal-tier edges in a CI/sandboxed environment with no
        /// network access to install one: build the `.scip` file in an
        /// earlier CI step (or another machine) that does have network
        /// access, then pass it here. Parses and ingests only, for every
        /// language's occurrences the file contains; skips every provider's
        /// own auto-detection entirely when set.
        #[cfg(feature = "scip-overlay")]
        #[arg(long)]
        scip_file: Option<PathBuf>,
        /// Path prefix to rebase `--scip-file`'s occurrence paths onto, when
        /// the indexer that produced it ran at a subdirectory of
        /// `project_root` (e.g. a nested Go module) rather than the root
        /// itself — see `scip::parse::parse_scip_file`'s `sub_root`
        /// parameter. Ignored without `--scip-file`.
        #[cfg(feature = "scip-overlay")]
        #[arg(long, requires = "scip_file")]
        sub_root: Option<PathBuf>,
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
    /// Initialize .calm/ config for a project
    Init {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
    /// Write MCP client config (.mcp.json, .cursor/mcp.json,
    /// .vscode/mcp.json) pointing at this binary, so an external project
    /// can use `calm` as its MCP server without checking out this repo.
    Setup {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Overwrite an existing "calm" entry even if it points somewhere
        /// else (e.g. this repo's own scripts/mcp-launcher.sh wiring)
        /// instead of leaving it alone
        #[arg(long)]
        force: bool,
        /// Write a portable `npx -y @eilodon/calm-mcp serve` entry instead
        /// of an absolute path to this binary. Use when the config will be
        /// committed and shared (teammates/CI that don't have this exact
        /// binary) or when you want it to track the published npm release
        /// automatically. Requires Node wherever it runs.
        #[arg(long)]
        npx: bool,
    },
    /// Manually run one or every SCIP provider's indexer right now (P2.6),
    /// bypassing the configured refresh policy — e.g. to force a run for a
    /// `min_interval`/`on_demand` provider without waiting, or as a
    /// standalone step outside `calm index`. Requires an existing index
    /// (run `calm index` first).
    #[cfg(feature = "scip-overlay")]
    ScipRun {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Which provider to refresh: rust, go, python, javascript, java,
        /// csharp, php, c, or "all" (default) for every provider in the
        /// table.
        #[arg(long)]
        lang: Option<String>,
    },
    /// Decode a `.scip` index file to JSON lines (hidden; used by the B2
    /// call-graph-quality benchmark to get oracle occurrences without
    /// duplicating SCIP protobuf parsing outside calm-core).
    #[cfg(feature = "scip-overlay")]
    #[command(hide = true)]
    ScipDump {
        /// Path to the .scip file produced by `rust-analyzer scip`
        scip_path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Daemon mode (`--listen`) detaches from the client's stdio (a future
    // `calm connect` spawns it with every fd null'd) so stderr disappears
    // the moment that happens. Decide the tracing writer *before* the
    // one-shot global `.init()` call below, based on a cheap peek at the
    // parsed command, rather than initializing unconditionally to stderr
    // and having daemon mode silently lose all its own logs.
    let daemon_project_root: Option<PathBuf> = match &cli.command {
        Commands::Serve {
            listen: Some(_),
            project_root,
            ..
        } => Some(project_root.clone()),
        _ => None,
    };

    match &daemon_project_root {
        Some(root) => init_daemon_tracing(root)?,
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::Level::INFO.into()),
                )
                .with_writer(std::io::stderr)
                .init();
        }
    }

    match cli.command {
        Commands::Serve {
            project_root,
            db_path,
            preset,
            listen,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db = db_path.unwrap_or_else(|| calm_server::default_db_path(&root));
            // CLI flag takes precedence; fall back to config.json value (default: "full").
            // Propagate (not swallow) load errors here — an invalid config.json (bad
            // JSON, or an unrecognized preset) should fail server startup loudly
            // rather than silently degrade to defaults.
            let config = calm_core::config::load_config(&root)?;
            let effective_preset = preset.unwrap_or_else(|| config.preset.clone());

            if let Some(listen) = listen {
                #[cfg(unix)]
                {
                    let socket_path = parse_unix_listen(&listen)?;
                    tracing::info!(
                        "Starting CALM daemon for {} (preset={}, socket={})",
                        root.display(),
                        effective_preset,
                        socket_path.display()
                    );
                    return calm_server::daemon::serve_unix_daemon(
                        root,
                        db,
                        effective_preset,
                        socket_path,
                    )
                    .await;
                }
                #[cfg(not(unix))]
                {
                    let _ = listen;
                    anyhow::bail!("--listen is only supported on Unix");
                }
            }

            tracing::info!(
                "Starting MCP server for {} (preset={})",
                root.display(),
                effective_preset
            );
            calm_server::serve_stdio_with_preset(root, db, effective_preset).await?;
        }
        #[cfg(unix)]
        Commands::Connect {
            project_root,
            db_path,
            preset,
        } => {
            calm_server::daemon::connect_or_spawn(project_root, preset, db_path).await?;
        }
        Commands::Index {
            project_root,
            #[cfg(feature = "scip-overlay")]
            scip_file,
            #[cfg(feature = "scip-overlay")]
            sub_root,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            tracing::info!("Indexing {}", root.display());
            let db_path = calm_server::default_db_path(&root);
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut conn = calm_core::db::conn::open_writer(&db_path)?;
            calm_core::db::schema::init_db(&conn)?;
            let phase = std::sync::Arc::new(std::sync::RwLock::new(
                calm_core::types::IndexingPhase::Scanning,
            ));
            calm_core::indexer::pipeline::run_indexing_pipeline(&mut conn, &root, phase)?;
            let symbol_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
            let file_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))?;
            tracing::info!("Indexing complete: {file_count} files, {symbol_count} symbols");
            println!("Indexed {file_count} files, {symbol_count} symbols.");

            // Semantic embeddings — active when `semantic_search.enabled: true` in
            // config.json and compiled with `--features embeddings`.
            let semantic = calm_core::config::load_config(&root)
                .map(|c| c.semantic_search)
                .unwrap_or_default();
            if semantic.enabled {
                print!("Building semantic index...");
                std::io::Write::flush(&mut std::io::stdout()).ok();
                match calm_core::embedding::Embedder::load(&semantic.model, semantic.dimensions) {
                    Ok(embedder) => {
                        // embedder.dim() (real, probed at load time) rather than
                        // semantic.dimensions (config, possibly stale) — see
                        // Embedder::load and create_embedding_table's self-heal.
                        calm_core::embedding::create_embedding_table(&conn, embedder.dim())?;
                        let n = calm_core::embedding::embed_pending(&conn, &embedder)?;
                        calm_core::embedding::create_chunk_embedding_table(&conn, embedder.dim())?;
                        let nc = calm_core::embedding::embed_pending_chunks(&conn, &embedder)?;
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

            // SCIP overlay: upgrade edges to `formal` confidence using an
            // external compiler-grade indexer (rust-analyzer for Rust today).
            // Mirrors the background-indexer path in `calm-server`'s
            // `serve_stdio_with_preset` (crates/calm-server/src/lib.rs) so the
            // one-shot `calm index` CLI gets the same upgrade the MCP server
            // does. Runs after the base graph + embeddings are built;
            // fail-silent by design (see `run_overlay`'s doc comment) — a
            // missing rust-analyzer or any overlay error leaves the syntactic
            // graph untouched.
            //
            // `--scip-file` (P2.6) takes a completely different, much
            // simpler path: parse + ingest that one file directly, skipping
            // every provider's own binary auto-detection entirely — the
            // point is to work in a sandbox with no network access to run
            // any of them.
            #[cfg(feature = "scip-overlay")]
            if let Some(scip_file) = scip_file {
                let sub_root = sub_root.unwrap_or_default();
                let occ = calm_core::scip::parse::parse_scip_file(&scip_file, &sub_root)?;
                let stats = calm_core::scip::ingest::ingest_occurrences(&conn, &occ, true)?;
                if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 {
                    calm_core::indexer::pipeline::refresh_caller_counts(&conn)?;
                }
                println!(
                    "SCIP overlay (from {}): {} edges upgraded, {} fan-out siblings ruled out, \
                     {} inserted, match_rate={:.2}.",
                    scip_file.display(),
                    stats.upgraded,
                    stats.ruled_out,
                    stats.inserted,
                    stats.match_rate
                );
            } else {
                let rust_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.rust)
                    .unwrap_or_default();
                let dirty = calm_core::scip::rust_source_dirty_keys(&conn);
                match calm_core::scip::run_overlay(&conn, &root, &rust_cfg, &dirty) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        // caller_count was computed by rebuild_graph before this
                        // overlay flipped edge_confidence/ruled_out_by_scip on (or
                        // inserted) some edges — refresh it or it goes stale
                        // immediately relative to the columns it's filtered on.
                        if let Err(e) = calm_core::indexer::pipeline::refresh_caller_counts(&conn) {
                            tracing::warn!("caller_count refresh after SCIP overlay failed: {e}");
                        }
                        println!(
                            "SCIP overlay: {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("SCIP overlay error (base graph intact): {e}"),
                }

                let go_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.go)
                    .unwrap_or_default();
                match calm_core::scip::run_go_overlay_and_log(&conn, &root, &go_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (go): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("SCIP overlay (go) error (base graph intact): {e}"),
                }

                let python_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.python)
                    .unwrap_or_default();
                match calm_core::scip::run_python_overlay_and_log(&conn, &root, &python_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (python): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (python) error (base graph intact): {e}")
                    }
                }

                let js_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.js)
                    .unwrap_or_default();
                match calm_core::scip::run_js_overlay_and_log(&conn, &root, &js_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (js): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (js) error (base graph intact): {e}")
                    }
                }

                let java_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.java)
                    .unwrap_or_default();
                match calm_core::scip::run_java_overlay_and_log(&conn, &root, &java_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (java): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (java) error (base graph intact): {e}")
                    }
                }

                let csharp_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.csharp)
                    .unwrap_or_default();
                match calm_core::scip::run_csharp_overlay_and_log(&conn, &root, &csharp_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (csharp): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (csharp) error (base graph intact): {e}")
                    }
                }

                let php_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.php)
                    .unwrap_or_default();
                match calm_core::scip::run_php_overlay_and_log(&conn, &root, &php_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (php): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (php) error (base graph intact): {e}")
                    }
                }

                let clang_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.clang)
                    .unwrap_or_default();
                match calm_core::scip::run_clang_overlay_and_log(&conn, &root, &clang_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (c): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (c) error (base graph intact): {e}")
                    }
                }

                let ruby_cfg = calm_core::config::load_config(&root)
                    .map(|c| c.ruby)
                    .unwrap_or_default();
                match calm_core::scip::run_ruby_overlay_and_log(&conn, &root, &ruby_cfg) {
                    Ok(stats)
                        if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 =>
                    {
                        println!(
                            "SCIP overlay (ruby): {} edges upgraded, {} fan-out siblings ruled out, {} inserted.",
                            stats.upgraded, stats.ruled_out, stats.inserted
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("SCIP overlay (ruby) error (base graph intact): {e}")
                    }
                }
            }
        }
        Commands::Doctor { project_root } => {
            let root = std::fs::canonicalize(&project_root)?;
            calm_server::doctor(&root)?;
        }
        Commands::FitnessCheck {
            project_root,
            config,
            json,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db_path = calm_server::default_db_path(&root);

            let thresholds = calm_core::fitness::load_thresholds(config.as_deref())?;
            let boundary_rules = calm_core::fitness::load_boundary_rules(config.as_deref())?;
            let config_drift_doc_paths =
                calm_core::fitness::load_config_drift_doc_paths(config.as_deref())?;

            let conn = calm_core::db::conn::open_writer(&db_path)
                .unwrap_or_else(|_| rusqlite::Connection::open_in_memory().expect("in-memory DB"));
            calm_core::db::schema::init_db(&conn)?;

            let coverage = calm_core::analysis::coverage::load_coverage(&root);
            let result = calm_core::fitness::run_fitness_check(
                &conn,
                &thresholds,
                &root,
                &coverage,
                &boundary_rules,
                &config_drift_doc_paths,
            )?;

            // Record today's metrics for later trend comparison (edit_context's
            // `trend` field). Rounded to the day so repeated same-day CI runs
            // collapse onto one row via the UNIQUE(qualified_name, snapshot_at)
            // constraint instead of growing the table on every run. Best-effort:
            // a snapshot failure shouldn't fail the fitness gate itself.
            let snapshot_at = calm_core::fitness::today_utc_date();
            if let Err(e) = calm_core::fitness::snapshot_metrics(&conn, &snapshot_at) {
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
                if !result.boundary_violations.is_empty() {
                    println!();
                    println!("Boundary violations:");
                    for v in &result.boundary_violations {
                        let reason = if v.reason.is_empty() {
                            String::new()
                        } else {
                            format!(" — {}", v.reason)
                        };
                        println!(
                            "  {} -> {} (rule: {} -> {}){reason}",
                            v.from_path, v.to_path, v.rule_from, v.rule_to
                        );
                    }
                }
                if !result.config_drift.is_empty() {
                    println!();
                    println!("Config drift (doc references to files that no longer exist):");
                    for f in &result.config_drift {
                        println!("  {}: references \"{}\"", f.doc_path, f.reference);
                    }
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
        #[cfg(feature = "scip-overlay")]
        Commands::ScipRun { project_root, lang } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db_path = calm_server::default_db_path(&root);
            let conn = calm_core::db::conn::open_writer(&db_path)?;
            let config = calm_core::config::load_config(&root).unwrap_or_default();
            let results =
                calm_core::scip::refresh_language(&conn, &root, &config, lang.as_deref())?;
            for (lang, stats) in &results {
                println!(
                    "SCIP overlay ({lang}): {} edges upgraded, {} fan-out siblings ruled out, \
                     {} inserted, match_rate={:.2}.",
                    stats.upgraded, stats.ruled_out, stats.inserted, stats.match_rate
                );
            }
        }
        #[cfg(feature = "scip-overlay")]
        Commands::ScipDump { scip_path } => {
            let occ =
                calm_core::scip::parse::parse_scip_file(&scip_path, std::path::Path::new(""))?;
            let stdout = std::io::stdout();
            let mut w = stdout.lock();
            for o in &occ {
                serde_json::to_writer(
                    &mut w,
                    &serde_json::json!({
                        "file": o.file,
                        "line": o.line,
                        "symbol": o.symbol,
                        "is_def": o.is_def,
                        "is_local": o.is_local,
                    }),
                )?;
                std::io::Write::write_all(&mut w, b"\n")?;
            }
        }
        Commands::Init { project_root } => {
            let root = if project_root.exists() {
                std::fs::canonicalize(&project_root)?
            } else {
                project_root.clone()
            };

            let calm_dir = root.join(".calm");
            std::fs::create_dir_all(&calm_dir)?;

            let config_path = calm_dir.join("config.json");
            if config_path.exists() {
                println!("Config already exists at {}", config_path.display());
                println!("Remove it first if you want to reset to defaults.");
            } else {
                std::fs::write(&config_path, calm_core::config::default_config_json())?;
                println!("Created {}", config_path.display());
            }

            println!();
            println!("Next steps:");
            println!(
                "  calm index  --project-root {}  # build the index",
                root.display()
            );
            println!(
                "  calm serve  --project-root {}  # start MCP server",
                root.display()
            );
        }
        Commands::Setup {
            project_root,
            force,
            npx,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let bin_path = std::env::current_exe()?;
            let bin_str = bin_path.to_string_lossy().into_owned();

            // `--npx` writes a portable `npx -y @eilodon/calm-mcp serve`
            // entry (shareable, tracks the published npm release); the
            // default points at this exact binary via `write_mcp_config`.
            let npx_args = ["-y", "@eilodon/calm-mcp", "serve"];

            println!("Configuring MCP clients in {}", root.display());
            println!();

            // (relative path, top-level JSON key) — VS Code alone uses
            // "servers", not "mcpServers", for its top-level field; same
            // command/args shape otherwise.
            const TARGETS: [(&str, &str); 3] = [
                (".mcp.json", "mcpServers"),
                (".cursor/mcp.json", "mcpServers"),
                (".vscode/mcp.json", "servers"),
            ];
            for (rel_path, top_key) in TARGETS {
                let path = root.join(rel_path);
                let result = if npx {
                    write_mcp_config_entry(&path, top_key, &calm_entry("npx", &npx_args), force)
                } else {
                    write_mcp_config(&path, top_key, &bin_str, force)
                };
                match result {
                    Ok(action) => println!("  {rel_path}: {action}"),
                    Err(e) => println!("  {rel_path}: skipped ({e})"),
                }
            }

            println!();
            println!(
                "Windsurf and JetBrains read MCP config from a global (not \
                project-level) file, so they can't be written here — add \
                this by hand:"
            );
            println!();
            let snippet = if npx {
                manual_mcp_config_snippet("npx", &npx_args)
            } else {
                manual_mcp_config_snippet(&bin_str, &["serve"])
            };
            println!("{snippet}");
        }
    }

    Ok(())
}

/// Builds the `{ "command", "args" }` MCP entry every client config shares,
/// so the absolute-binary form and the portable `npx` form differ only in
/// which `command`/`args` get passed in here.
fn calm_entry(command: &str, args: &[&str]) -> serde_json::Value {
    serde_json::json!({ "command": command, "args": args })
}

/// Merges a `"calm"` entry pointing at `bin_path` (invoked with `serve`)
/// into `path`'s top-level `top_key` object. Thin wrapper over
/// `write_mcp_config_entry` for the common absolute-binary case; `calm setup
/// --npx` builds a portable entry and calls the entry form directly.
fn write_mcp_config(
    path: &std::path::Path,
    top_key: &str,
    bin_path: &str,
    force: bool,
) -> Result<&'static str> {
    write_mcp_config_entry(path, top_key, &calm_entry(bin_path, &["serve"]), force)
}

/// Merges `new_entry` as the `"calm"` server under `path`'s top-level
/// `top_key` object, creating the file (and parent dirs) if needed. Never
/// touches unrelated entries — `calm setup` may run in a project that
/// already wires up other MCP servers. Leaves an existing, *different*
/// "calm" entry alone unless `force` is set, so re-running this inside a
/// checkout that deliberately points at something else (e.g. this repo's own
/// scripts/mcp-launcher.sh, which adds freshness checks and a source-build
/// fallback a raw binary path doesn't have) can't silently downgrade it.
fn write_mcp_config_entry(
    path: &std::path::Path,
    top_key: &str,
    new_entry: &serde_json::Value,
    force: bool,
) -> Result<&'static str> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut root_value: serde_json::Value = if path.exists() {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("existing file isn't valid JSON ({e}) — leaving it untouched")
        })?
    } else {
        serde_json::json!({})
    };

    let obj = root_value.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("existing file's top level isn't a JSON object — leaving it untouched")
    })?;
    let servers = obj.entry(top_key).or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("existing \"{top_key}\" field isn't a JSON object — leaving it untouched")
    })?;

    let action = match servers_obj.get("calm") {
        None => "wrote",
        Some(existing) if existing == new_entry => "up to date",
        Some(_) if !force => "exists — pass --force to overwrite",
        Some(_) => "updated",
    };
    if action == "up to date" || action.starts_with("exists") {
        return Ok(action);
    }
    servers_obj.insert("calm".to_string(), new_entry.clone());
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&root_value)?),
    )?;
    Ok(action)
}

/// Manual MCP config snippet for clients that only read a global (not
/// project-level) config file — Windsurf (`~/.codeium/windsurf/mcp_config.json`)
/// and JetBrains AI Assistant (its own settings UI) — mirrors
/// docs/mcp-client-setup.md's existing hand-written snippet for those two.
/// Takes the same `command`/`args` the project-level files got, so the
/// printed snippet matches whichever form (`--npx` or absolute path) the
/// caller chose.
fn manual_mcp_config_snippet(command: &str, args: &[&str]) -> String {
    let doc = serde_json::json!({ "mcpServers": { "calm": calm_entry(command, args) } });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).expect("static JSON is always serializable")
    )
}

/// Parses `--listen`'s `unix:PATH` form into a socket path. v1 supports
/// only the `unix:` scheme (Unix domain sockets) — no TCP, matching
/// ADR-0005's decision to keep the daemon off any network-reachable
/// transport by default.
#[cfg(unix)]
fn parse_unix_listen(listen: &str) -> Result<PathBuf> {
    listen
        .strip_prefix("unix:")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("--listen must be of the form unix:PATH, got {listen:?}"))
}

/// Global tracing subscriber for daemon mode: once this process detaches
/// with every fd null'd (see `crates/calm-server/src/daemon.rs`'s doc
/// comment), stderr disappears — logging to a file instead is the only way
/// to debug an idle-timeout eviction or a background-indexer panic after
/// the fact. `project_root` is the raw (possibly relative/uncanonicalized)
/// CLI value; re-canonicalized here independently since this runs before
/// `Commands::Serve`'s own canonicalization.
fn init_daemon_tracing(project_root: &std::path::Path) -> Result<()> {
    let root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let calm_dir = root.join(".calm");
    // `calm_server::daemon::create_calm_dir` (atomic 0700), NOT a plain
    // `create_dir_all` — this runs *before* `serve_unix_daemon`'s own call
    // to the same function (tracing has to be initialized before anything
    // can log), so a loose `create_dir_all` here would win the race to
    // create `.calm/` first, and `create_calm_dir`'s "already exists →
    // treat as success" branch would then silently leave it at whatever
    // permissive default this one used instead of `0700`. Found via
    // `daemon_calm_dir_and_socket_have_restrictive_permissions`
    // (`crates/calm-cli/tests/daemon_integration.rs`), not by inspection.
    calm_server::daemon::create_calm_dir(&calm_dir)?;

    use tracing_subscriber::prelude::*;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(calm_dir.join("daemon.log"))?;
    let human_layer = tracing_subscriber::fmt::layer()
        .with_writer(log_file)
        .with_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        );

    // Structured, SIEM-ingestible sibling of `daemon.log`: same process,
    // separate file, JSON-formatted, scoped to only the
    // `calm_server::telemetry::AUDIT_TARGET` target via `filter_fn` — every
    // other INFO-level line (the bulk of daemon.log) is excluded here, so
    // this file only ever holds edit-decision events (EDIT_CONTEXT_REQUIRED/
    // CONFIRM_REQUIRED/REASON_NOT_GROUNDED denials, and applied writes with
    // their before/after hashes) instead of duplicating the human log in a
    // different format.
    let audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(calm_dir.join("audit.log"))?;
    let audit_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(audit_file)
        .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
            meta.target() == calm_server::telemetry::AUDIT_TARGET
        }));

    tracing_subscriber::registry()
        .with(human_layer)
        .with(audit_layer)
        .init();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_mcp_config_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");

        let action = write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", false).unwrap();

        assert_eq!(action, "wrote");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["calm"]["command"],
            "/usr/local/bin/calm"
        );
        assert_eq!(
            written["mcpServers"]["calm"]["args"],
            serde_json::json!(["serve"])
        );
    }

    #[test]
    fn write_mcp_config_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".vscode").join("mcp.json");

        write_mcp_config(&path, "servers", "/usr/local/bin/calm", false).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn write_mcp_config_preserves_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"foo","args":[]}}}"#,
        )
        .unwrap();

        write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", false).unwrap();

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["other"]["command"], "foo");
        assert_eq!(
            written["mcpServers"]["calm"]["command"],
            "/usr/local/bin/calm"
        );
    }

    #[test]
    fn write_mcp_config_rerun_with_same_binary_is_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");

        write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", false).unwrap();
        let action = write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", false).unwrap();

        assert_eq!(action, "up to date");
    }

    #[test]
    fn write_mcp_config_leaves_different_existing_entry_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let original =
            r#"{"mcpServers":{"calm":{"command":"bash","args":["scripts/mcp-launcher.sh"]}}}"#;
        std::fs::write(&path, original).unwrap();

        let action = write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", false).unwrap();

        assert!(action.starts_with("exists"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn write_mcp_config_force_overwrites_different_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"calm":{"command":"bash","args":["scripts/mcp-launcher.sh"]}}}"#,
        )
        .unwrap();

        let action = write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", true).unwrap();

        assert_eq!(action, "updated");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["calm"]["command"],
            "/usr/local/bin/calm"
        );
    }

    #[test]
    fn write_mcp_config_invalid_json_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, "not json").unwrap();

        let result = write_mcp_config(&path, "mcpServers", "/usr/local/bin/calm", false);

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    #[test]
    fn manual_mcp_config_snippet_is_valid_json_with_command() {
        let snippet = manual_mcp_config_snippet("/usr/local/bin/calm", &["serve"]);
        let parsed: serde_json::Value = serde_json::from_str(&snippet).unwrap();
        assert_eq!(
            parsed["mcpServers"]["calm"]["command"],
            "/usr/local/bin/calm"
        );
        assert_eq!(
            parsed["mcpServers"]["calm"]["args"],
            serde_json::json!(["serve"])
        );
    }

    #[test]
    fn calm_entry_builds_npx_form() {
        let entry = calm_entry("npx", &["-y", "@eilodon/calm-mcp", "serve"]);
        assert_eq!(entry["command"], "npx");
        assert_eq!(
            entry["args"],
            serde_json::json!(["-y", "@eilodon/calm-mcp", "serve"])
        );
    }

    #[test]
    fn write_mcp_config_entry_writes_npx_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let entry = calm_entry("npx", &["-y", "@eilodon/calm-mcp", "serve"]);

        let action = write_mcp_config_entry(&path, "mcpServers", &entry, false).unwrap();

        assert_eq!(action, "wrote");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["calm"]["command"], "npx");
        assert_eq!(
            written["mcpServers"]["calm"]["args"],
            serde_json::json!(["-y", "@eilodon/calm-mcp", "serve"])
        );
    }
}
