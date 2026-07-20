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
        /// Self-heal a configured-but-not-active hooks install (entrypoint
        /// missing — e.g. the project directory was moved/renamed since
        /// `calm init --hooks` last ran, or the npm-resolved binary path
        /// changed) by re-running the equivalent of `calm init
        /// --hooks=<current mode>` with this binary's own current path.
        /// Never touches an explicit `--hooks=off`, and never changes
        /// nudge<->enforce — only repairs a stale/missing entrypoint for
        /// whichever mode is already configured. A no-op (prints the same
        /// report as without this flag) when nothing needs fixing.
        #[arg(long)]
        fix: bool,
    },
    /// Check codebase fitness against thresholds (exits 1 if any threshold exceeded)
    FitnessCheck {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Path to thresholds.toml. Numeric thresholds (hub_count,
        /// dead_code_pct, etc.) fall back to FitnessThresholds::default()
        /// when omitted, but [[boundaries]] and [config_drift] have no
        /// such default (an empty rule set) — omitting this when your repo
        /// declares either silently checks neither, not "uses defaults".
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
        /// Also scaffold the CALM MCP tool workflow into AGENTS.md at the
        /// project root, inside a `<!-- calm:workflow:start/end -->`
        /// marker block shared verbatim with the `calm_workflow` MCP
        /// Prompt (`calm_core::workflow::CALM_WORKFLOW_GUIDE`). Rerunning
        /// is idempotent (replaces only the marked block). Off by default
        /// — `calm init` alone never touches AGENTS.md.
        #[arg(long)]
        agents_md: bool,
        /// With `--agents-md`: append the CALM block to an existing
        /// AGENTS.md that has no marker yet, instead of refusing. Has no
        /// effect once a marker block already exists (that rerun is
        /// always safe without `--force` — see `write_agents_md_block`).
        #[arg(long)]
        force: bool,
        /// Scaffold a generic Claude Code hook (`.claude/hooks/calm-hooks.sh`,
        /// wired into `.claude/settings.json`) nudging toward `edit_context`
        /// before native `Edit`/`Write` and `diff_impact` before commit/push.
        /// Off by default — `calm init` alone never touches hooks. Bare
        /// `--hooks` means `nudge` (advisory only, never blocks — this is
        /// the mechanism's own shadow-mode-equivalent trial). `--hooks=enforce`
        /// upgrades specific gates to a hard `exit 2` deny — best-effort
        /// defense-in-depth, NOT a security boundary (see the scaffolded
        /// script's own header comment for the exact bypass this cannot
        /// close). `--hooks=off` cleanly removes everything this wrote.
        /// See `calm doctor` to check the actually-active state at any time.
        #[arg(long, num_args = 0..=1, default_missing_value = "nudge")]
        hooks: Option<String>,
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
    /// PreToolUse/PostToolUse hook backend, invoked by `.claude/settings.json`
    /// exec-form wiring (`calm init --hooks` scaffolds this automatically) —
    /// not meant to be run by hand. Reads one Claude Code hook JSON payload
    /// from stdin, decides allow/nudge/deny per `calm_core::hooks_check`,
    /// and reports the result via exit code (0 = allow/nudge, 2 = deny) plus
    /// an optional stderr message — see that module for the full contract,
    /// including its fail-open guarantee on malformed stdin.
    #[command(hide = true)]
    HooksCheck {
        /// Project root directory this hook call is scoped to.
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
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

            // Config loaded once up front — previously each of the 10 blocks
            // below re-read + re-parsed config.json independently via
            // load_config(...).unwrap_or_default(), silently discarding a
            // malformed config 10x over; load_config_or_warn logs once instead.
            let config = calm_core::config::load_config_or_warn(&root);

            // Semantic embeddings — active when `semantic_search.enabled: true` in
            // config.json and compiled with `--features embeddings`.
            let semantic = &config.semantic_search;
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
                let rust_cfg = &config.rust;
                let dirty = calm_core::scip::rust_source_dirty_keys(&conn);
                match calm_core::scip::run_overlay(&conn, &root, rust_cfg, &dirty) {
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

                let go_cfg = &config.go;
                match calm_core::scip::run_go_overlay_and_log(&conn, &root, go_cfg) {
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

                let python_cfg = &config.python;
                match calm_core::scip::run_python_overlay_and_log(&conn, &root, python_cfg) {
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

                let js_cfg = &config.js;
                match calm_core::scip::run_js_overlay_and_log(&conn, &root, js_cfg) {
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

                let java_cfg = &config.java;
                match calm_core::scip::run_java_overlay_and_log(&conn, &root, java_cfg) {
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

                let csharp_cfg = &config.csharp;
                match calm_core::scip::run_csharp_overlay_and_log(&conn, &root, csharp_cfg) {
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

                let php_cfg = &config.php;
                match calm_core::scip::run_php_overlay_and_log(&conn, &root, php_cfg) {
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

                let clang_cfg = &config.clang;
                match calm_core::scip::run_clang_overlay_and_log(&conn, &root, clang_cfg) {
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

                let ruby_cfg = &config.ruby;
                match calm_core::scip::run_ruby_overlay_and_log(&conn, &root, ruby_cfg) {
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
        Commands::Doctor { project_root, fix } => {
            let root = std::fs::canonicalize(&project_root)?;
            if fix {
                let calm_dir = root.join(".calm");
                let mode = calm_core::hooks::read_hooks_mode_file(&calm_dir);
                if mode == calm_core::hooks::HooksMode::Off {
                    println!(
                        "hooks: off — nothing to fix (run `calm init --hooks=nudge|enforce` to turn it on)\n"
                    );
                } else if let Err(e) = apply_hooks_flag(&root, mode.as_str()) {
                    println!("hooks: --fix failed: {e}\n");
                } else {
                    println!();
                }
            }
            calm_server::doctor(&root)?;
        }
        Commands::FitnessCheck {
            project_root,
            config,
            json,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db_path = calm_server::default_db_path(&root);
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let thresholds = calm_core::fitness::load_thresholds(config.as_deref())?;
            let boundary_rules = calm_core::fitness::load_boundary_rules(config.as_deref())?;
            let config_drift_doc_paths =
                calm_core::fitness::load_config_drift_doc_paths(config.as_deref())?;

            // Propagate (not swallow) an open failure — silently falling back to
            // an ephemeral in-memory DB here made `calm fitness-check` report a
            // misleading PASS against zero symbols on any real infra error (e.g.
            // a locked/corrupted index.db), with no signal why. Matches the
            // policy on `Serve`'s own load_config(&root)? above.
            let conn = calm_core::db::conn::open_writer(&db_path)?;
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
            let config = calm_core::config::load_config_or_warn(&root);
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
        Commands::Init {
            project_root,
            agents_md,
            force,
            hooks,
        } => {
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

            if agents_md {
                match write_agents_md_block(&root, force) {
                    Ok(action) => println!("AGENTS.md: {action}"),
                    Err(e) => println!("AGENTS.md: skipped ({e})"),
                }
            }

            if let Some(mode_str) = hooks
                && let Err(e) = apply_hooks_flag(&root, &mode_str)
            {
                println!("hooks: skipped ({e})");
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
        Commands::HooksCheck { project_root } => {
            let root = std::fs::canonicalize(&project_root).unwrap_or(project_root);
            let code = calm_core::hooks_check::run(std::io::stdin(), std::io::stderr(), &root);
            std::process::exit(code);
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

/// Writes/updates `calm init --agents-md`'s managed block inside
/// `<project_root>/AGENTS.md` -- `calm_core::workflow::CALM_WORKFLOW_GUIDE`,
/// the same const the `calm_workflow` MCP Prompt renders, wrapped in a
/// `<!-- calm:workflow:start/end -->` marker pair so a rerun can find and
/// replace exactly its own prior output without touching anything else in
/// the file. Mirrors `write_mcp_config_entry`'s "never touch ambiguous
/// state" philosophy, adapted for a markdown file instead of JSON:
///
/// - file absent -> create it with just the marked block (LF line endings,
///   this repo's own convention).
/// - file present, exactly one well-formed marker pair -> idempotent
///   replace between the markers -- CALM's own managed content, safe
///   without `--force`, same as re-running never needs `--force` for its
///   own `"calm"` key in `write_mcp_config_entry`.
/// - file present, zero markers -> refuse unless `force`; `--force`
///   appends the block to the end of the existing file instead of
///   touching its content.
/// - file present, malformed markers (only one of the pair, or either
///   marker appears more than once) -> always refuse, even with
///   `--force` -- this is the one case where we can't tell what's safe to
///   touch, matching `write_mcp_config_entry`'s unconditional refusal on
///   invalid JSON (that case ignores `force` too).
///
/// Preserves the target file's existing line-ending convention (CRLF if
/// the file already uses it) for the inserted block, so a rerun on a
/// Windows-authored file doesn't introduce mixed line endings.
fn write_agents_md_block(root: &std::path::Path, force: bool) -> Result<&'static str> {
    use calm_core::workflow::{AGENTS_MD_MARKER_END, AGENTS_MD_MARKER_START, CALM_WORKFLOW_GUIDE};

    let path = root.join("AGENTS.md");
    let block_body = format!(
        "{AGENTS_MD_MARKER_START}\n\
         ## CALM MCP workflow\n\
         \n\
         {CALM_WORKFLOW_GUIDE}\n\
         \n\
         _This block is managed by `calm init --agents-md` \u{2014} content \
         between the markers is replaced on rerun; edit freely outside \
         them._\n\
         {AGENTS_MD_MARKER_END}"
    );

    if !path.exists() {
        std::fs::write(&path, format!("{block_body}\n"))?;
        return Ok("wrote");
    }

    let existing = std::fs::read_to_string(&path)?;
    let uses_crlf = existing.contains("\r\n");
    let block_body = if uses_crlf {
        block_body.replace('\n', "\r\n")
    } else {
        block_body
    };
    let nl = if uses_crlf { "\r\n" } else { "\n" };

    let start_count = existing.matches(AGENTS_MD_MARKER_START).count();
    let end_count = existing.matches(AGENTS_MD_MARKER_END).count();

    match (start_count, end_count) {
        (0, 0) => {
            if !force {
                anyhow::bail!(
                    "AGENTS.md exists without a calm:workflow marker \u{2014} pass --force to append the CALM block, or add the markers by hand"
                );
            }
            let double_nl = format!("{nl}{nl}");
            let sep: &str = if existing.is_empty() || existing.ends_with(&double_nl) {
                ""
            } else if existing.ends_with(nl) {
                nl
            } else {
                &double_nl
            };
            let updated = format!("{existing}{sep}{block_body}{nl}");
            std::fs::write(&path, updated)?;
            Ok("appended")
        }
        (1, 1) => {
            let start_idx = existing.find(AGENTS_MD_MARKER_START).unwrap();
            let end_idx = existing.find(AGENTS_MD_MARKER_END).unwrap() + AGENTS_MD_MARKER_END.len();
            if end_idx <= start_idx {
                anyhow::bail!(
                    "AGENTS.md's calm:workflow end marker appears before its start marker \u{2014} leaving it untouched, fix by hand"
                );
            }
            let before = &existing[..start_idx];
            let after = existing[end_idx..].trim_start_matches(['\n', '\r']);
            let mut updated = String::with_capacity(existing.len() + block_body.len());
            updated.push_str(before);
            updated.push_str(&block_body);
            updated.push_str(nl);
            updated.push_str(after);
            if updated == existing {
                return Ok("up to date");
            }
            std::fs::write(&path, updated)?;
            Ok("updated")
        }
        _ => anyhow::bail!(
            "AGENTS.md has an unexpected number of calm:workflow markers (start={start_count}, end={end_count}) \u{2014} leaving it untouched, this needs a manual fix, not --force"
        ),
    }
}

/// Applies `calm init --hooks[=MODE]` end to end: validates `mode_str`,
/// scaffolds `.claude/hooks/calm-hooks.sh` (for `nudge`/`enforce`) or
/// removes everything this command previously wrote (for `off`), writes
/// `.calm/hooks.mode`, merges/removes the `.claude/settings.json` block,
/// and prints exactly what changed plus the current mode transition —
/// satisfying the "never silent" requirement from
/// docs/superskills/specs/2026-07-15-calm-hooks-transparent-reactivation.md
/// at the one point a human is guaranteed to see output: the command they
/// just ran.
fn apply_hooks_flag(root: &std::path::Path, mode_str: &str) -> Result<()> {
    use calm_core::hooks::{self, CLAUDE_SETTINGS_REL_PATH, HOOKS_SCRIPT_REL_PATH, HooksMode};

    let mode = HooksMode::parse(mode_str).ok_or_else(|| {
        anyhow::anyhow!(
            "unrecognized --hooks value {mode_str:?} — expected one of: nudge, enforce, off"
        )
    })?;

    let calm_dir = root.join(".calm");
    let previous_mode = hooks::read_hooks_mode_file(&calm_dir);
    let settings_path = root.join(CLAUDE_SETTINGS_REL_PATH);
    let legacy_script_path = root.join(HOOKS_SCRIPT_REL_PATH);

    if mode == HooksMode::Off {
        let mode_removed = hooks::remove_hooks_mode_file(&calm_dir)?;
        let block_action = hooks::remove_hooks_settings_block(&settings_path)?;
        // Cleans up a pre-2026-07-16 legacy install's script file too, if
        // present — a fresh install never writes one (see below), so this
        // is a no-op there.
        let script_removed = if legacy_script_path.exists() {
            std::fs::remove_file(&legacy_script_path)?;
            true
        } else {
            false
        };
        println!("hooks: {previous_mode} -> off");
        println!(
            "  .calm/hooks.mode: {}",
            if mode_removed {
                "removed"
            } else {
                "was already absent"
            }
        );
        println!("  {CLAUDE_SETTINGS_REL_PATH}: {block_action}");
        println!(
            "  {HOOKS_SCRIPT_REL_PATH}: {}",
            if script_removed {
                "removed"
            } else {
                "was already absent"
            }
        );
        return Ok(());
    }

    // Exec-form entrypoint: the exact binary running THIS `calm init
    // --hooks` invocation is guaranteed to exist and work right now — no
    // PATH/npx resolution ambiguity, no bash/jq/sqlite3-CLI/flock
    // dependency (see docs/superskills/specs/2026-07-16-calm-hooks-
    // native-cli-subcommand.md). No script file is scaffolded for a fresh
    // install; `write_hooks_settings_block` atomically swaps away any
    // legacy shell-form block a prior version of this command left behind.
    let bin_path = std::env::current_exe()?;
    let bin_str = bin_path.to_string_lossy().into_owned();

    hooks::write_hooks_mode_file(&calm_dir, mode, calm_core::BUILD_INFO)?;
    let block_action = hooks::write_hooks_settings_block(&settings_path, &bin_str)?;

    println!("hooks: {previous_mode} -> {mode}");
    println!("  entrypoint: {bin_str} hooks-check");
    println!("  {CLAUDE_SETTINGS_REL_PATH}: {block_action}");
    println!();
    if mode == HooksMode::Nudge {
        println!(
            "  Advisory only — never blocks a tool call. This is the mechanism's own \
             shadow-mode trial: watch it fire for a while before opting into `--hooks=enforce`."
        );
    } else {
        println!(
            "  \u{26a0} best-effort defense-in-depth, NOT a security boundary. Any process with \
             normal write access to this repo (including the agent this is meant to nudge) can \
             disable it with one write to .calm/hooks.mode, or by editing \
             {CLAUDE_SETTINGS_REL_PATH} — true of every Claude Code hook, not specific to CALM. \
             A downgrade is never silent: it's logged to .calm/audit.log and surfaced on the \
             next tool call, but it cannot be prevented. Use this to catch honest mistakes, not \
             to constrain an actively evading agent."
        );
    }
    println!("  Check the actually-active state any time with `calm doctor`.");
    println!(
        "  Change mode with `calm init --hooks=nudge|enforce|off`; `CALM_HOOKS_DISABLE=1` \
         disables it for one shell session with no file changes."
    );
    Ok(())
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
    #[cfg(unix)]
    calm_server::daemon::create_calm_dir(&calm_dir)?;
    // Windows: this function only runs on the `--listen` match arm in
    // `main()` above, and `--listen` itself immediately bails with
    // "only supported on Unix" the moment `Commands::Serve` is actually
    // handled — so on non-Unix this is a dead-but-still-compiled path,
    // never real daemon work. `calm_server::daemon` doesn't exist on
    // non-Unix (`#[cfg(unix)] pub mod daemon;`, lib.rs) so it can't call
    // the atomic-0700 helper above; plain `create_dir_all` is enough for
    // a directory about to be abandoned anyway. Found via the
    // windows-build-experiment.yml probe (2026-07-15 first run): every C
    // dependency (bundled SQLite, ~24 tree-sitter grammars, onig via the
    // tokenizers crate) compiled clean under MSVC — this missing cfg-gate
    // was the ONLY compile error blocking a Windows build.
    #[cfg(not(unix))]
    std::fs::create_dir_all(&calm_dir)?;

    use tracing_subscriber::prelude::*;

    let mut log_opts = std::fs::OpenOptions::new();
    log_opts.create(true).append(true);
    // 0600 at creation time (existing files keep whatever mode they already
    // have -- `mode()` only affects the O_CREAT case): found via a real
    // audit 2026-07-14 that this and `audit.log` below both inherited the
    // umask-derived default (0664 observed), inconsistent with the 0600
    // this workspace deliberately uses for `.calm/memory.key` (see its own
    // doc comment) and `.calm/` itself (`create_calm_dir`, 0700). Neither
    // file holds secret material today, but both reveal file paths and
    // session activity to any other local user on a shared box, which
    // wasn't the intended posture.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_opts.mode(0o600);
    }
    let log_file = log_opts.open(calm_dir.join("daemon.log"))?;
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
    let mut audit_opts = std::fs::OpenOptions::new();
    audit_opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        audit_opts.mode(0o600);
    }
    let audit_file = audit_opts.open(calm_dir.join("audit.log"))?;
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

    #[test]
    fn agents_md_block_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();

        let action = write_agents_md_block(dir.path(), false).unwrap();

        assert_eq!(action, "wrote");
        let text = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(text.contains(calm_core::workflow::AGENTS_MD_MARKER_START));
        assert!(text.contains(calm_core::workflow::AGENTS_MD_MARKER_END));
        assert!(text.contains(calm_core::workflow::CALM_WORKFLOW_GUIDE));
        assert!(!text.contains("\r\n"), "new file must use LF");
    }

    #[test]
    fn agents_md_block_rerun_is_idempotent_up_to_date() {
        let dir = tempfile::tempdir().unwrap();

        write_agents_md_block(dir.path(), false).unwrap();
        let first = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let action = write_agents_md_block(dir.path(), false).unwrap();
        let second = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();

        assert_eq!(action, "up to date");
        assert_eq!(first, second, "rerun must be byte-identical, not grow");
    }

    #[test]
    fn agents_md_block_rerun_preserves_content_outside_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");

        write_agents_md_block(dir.path(), false).unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        let hand_written = format!(
            "# My notes\n\nSome human-written context.\n\n{original}\n\nMore notes after.\n"
        );
        std::fs::write(&path, &hand_written).unwrap();

        let action = write_agents_md_block(dir.path(), false).unwrap();

        assert_eq!(action, "updated");
        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("# My notes"));
        assert!(updated.contains("Some human-written context."));
        assert!(updated.contains("More notes after."));
        assert_eq!(
            updated
                .matches(calm_core::workflow::AGENTS_MD_MARKER_START)
                .count(),
            1
        );

        // Idempotent from here on too.
        let action2 = write_agents_md_block(dir.path(), false).unwrap();
        assert_eq!(action2, "up to date");
    }

    #[test]
    fn agents_md_block_refuses_existing_file_without_marker_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(
            &path,
            "# Existing project notes\n\nHand-written, no CALM block.\n",
        )
        .unwrap();

        let err = write_agents_md_block(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("--force"));
        let unchanged = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            unchanged,
            "# Existing project notes\n\nHand-written, no CALM block.\n"
        );

        let action = write_agents_md_block(dir.path(), true).unwrap();
        assert_eq!(action, "appended");
        let appended = std::fs::read_to_string(&path).unwrap();
        assert!(appended.contains("# Existing project notes"));
        assert!(appended.contains(calm_core::workflow::AGENTS_MD_MARKER_START));

        // Second run (even without --force) now hits the (1,1) idempotent
        // path, not the zero-marker path — --force is only needed once.
        let action2 = write_agents_md_block(dir.path(), false).unwrap();
        assert_eq!(action2, "up to date");
    }

    #[test]
    fn agents_md_block_refuses_malformed_markers_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");

        // Only a start marker, no matching end — orphaned pair.
        std::fs::write(
            &path,
            format!(
                "{}\nsome content, never closed\n",
                calm_core::workflow::AGENTS_MD_MARKER_START
            ),
        )
        .unwrap();
        let err = write_agents_md_block(dir.path(), true).unwrap_err();
        assert!(err.to_string().contains("unexpected number"));

        // Two start markers — ambiguous, also refused even with force.
        std::fs::write(
            &path,
            format!(
                "{}\n{}\ncontent\n{}\n",
                calm_core::workflow::AGENTS_MD_MARKER_START,
                calm_core::workflow::AGENTS_MD_MARKER_START,
                calm_core::workflow::AGENTS_MD_MARKER_END
            ),
        )
        .unwrap();
        let err = write_agents_md_block(dir.path(), true).unwrap_err();
        assert!(err.to_string().contains("unexpected number"));
    }

    #[test]
    fn agents_md_block_preserves_crlf_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "# Windows-authored notes\r\n\r\nContent.\r\n").unwrap();

        write_agents_md_block(dir.path(), true).unwrap();
        let appended = std::fs::read_to_string(&path).unwrap();
        assert!(appended.contains("\r\n"));
        assert!(
            !appended.replace("\r\n", "").contains('\n'),
            "every newline must be paired with \\r once CRLF is detected, got: {appended:?}"
        );

        // Rerun stays idempotent under CRLF too.
        let action = write_agents_md_block(dir.path(), false).unwrap();
        assert_eq!(action, "up to date");
    }
}
