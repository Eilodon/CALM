use super::common::*;
use super::*;

#[rmcp::tool_router(router = "recover_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "indexing_status",
        description = "USE WHEN: you need file-level index stats, embedding error details, or to trigger embedding recovery. NOT a replacement for repo_overview at session start. retry_embeddings=true triggers re-download of embedding model.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn indexing_status(
        &self,
        Parameters(p): Parameters<IndexingStatusParams>,
    ) -> Json<ToolOutcome<IndexingStatusOutput>> {
        Json(self.timed_tool("indexing_status", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let files: i64 = conn
                .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                .unwrap_or(0);
            let symbols: i64 = conn
                .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
                .unwrap_or(0);
            // PATTERN-DEBT call-edges-missing-ruled-out-filter: deliberately
            // NOT filtered by `ruled_out_by_scip` — `edges_indexed` is a raw
            // table-size stat, sibling of `files_indexed`/`symbols_indexed`
            // above (both plain row counts too), not a "confident edges"
            // metric. A disproven-but-still-present row is real DB storage
            // an agent reindexing/debugging cares about; filtering it here
            // would make this number disagree with `SELECT COUNT(*) FROM
            // call_edges` run by hand for no benefit.
            let edges: i64 = conn
                .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
                .unwrap_or(0);
            let last_updated: Option<f64> = conn
                .query_row("SELECT MAX(last_indexed) FROM file_index", [], |r| r.get(0))
                .ok()
                .flatten();
            let mut external_proofs = ExternalProofStatusOutput::default();
            if let Ok(mut stmt) = conn.prepare(
                "SELECT status, COUNT(*) FROM external_proofs GROUP BY status",
            ) && let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            }) {
                for row in rows.flatten() {
                    match row.0.as_str() {
                        "fresh" => external_proofs.fresh = row.1,
                        "stale" => external_proofs.stale = row.1,
                        "legacy" => external_proofs.legacy = row.1,
                        "unverified" => external_proofs.unverified = row.1,
                        "rejected" => external_proofs.rejected = row.1,
                        _ => {}
                    }
                }
            }
            // Tier 1/2 semantic-fact coverage (2026-08-07 roadmap) -- same
            // additive GROUP BY pattern as external_proofs above. Absent
            // rows read as zero (Default), never fabricated.
            let mut semantic_facts = SemanticFactsStatusOutput::default();
            if let Ok(mut stmt) =
                conn.prepare("SELECT confidence, COUNT(*) FROM type_relations GROUP BY confidence")
                && let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            {
                for row in rows.flatten() {
                    semantic_facts.type_relations_total += row.1;
                    match row.0.as_str() {
                        "resolved" => semantic_facts.type_relations_resolved = row.1,
                        "textual" => semantic_facts.type_relations_textual = row.1,
                        _ => {}
                    }
                }
            }
            if let Ok(mut stmt) =
                conn.prepare("SELECT effect_kind, COUNT(*) FROM symbol_effects GROUP BY effect_kind")
                && let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            {
                for row in rows.flatten() {
                    match row.0.as_str() {
                        "explicit_throw" => semantic_facts.explicit_throws = row.1,
                        "write_field" => semantic_facts.write_fields = row.1,
                        _ => {}
                    }
                }
            }
            {
                let mut by_language: std::collections::BTreeMap<String, (i64, i64, i64)> =
                    std::collections::BTreeMap::new();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT s.language, COUNT(*) FROM type_relations tr \
                     JOIN symbols s ON s.qualified_name = tr.from_symbol GROUP BY s.language",
                ) && let Ok(rows) =
                    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                {
                    for row in rows.flatten() {
                        by_language.entry(row.0).or_default().0 += row.1;
                    }
                }
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT s.language, se.effect_kind, COUNT(*) FROM symbol_effects se \
                     JOIN symbols s ON s.qualified_name = se.symbol_qn GROUP BY s.language, se.effect_kind",
                ) && let Ok(rows) = stmt.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                }) {
                    for row in rows.flatten() {
                        let entry = by_language.entry(row.0).or_default();
                        match row.1.as_str() {
                            "explicit_throw" => entry.1 += row.2,
                            "write_field" => entry.2 += row.2,
                            _ => {}
                        }
                    }
                }
                semantic_facts.by_language = by_language
                    .into_iter()
                    .map(|(language, (type_relations, explicit_throws, write_fields))| {
                        SemanticFactsLanguageCountOutput {
                            language,
                            type_relations,
                            explicit_throws,
                            write_fields,
                        }
                    })
                    .collect();
            }
            let architecture_digest = conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(recursive_component), 0), COALESCE(SUM(truncated), 0) \
                     FROM symbol_digests",
                    [],
                    |r| {
                        Ok(ArchitectureDigestStatusOutput {
                            symbols_with_digest: r.get(0)?,
                            recursive_symbols: r.get(1)?,
                            truncated_digests: r.get(2)?,
                        })
                    },
                )
                .unwrap_or_default();

            let identity_migration = conn
                .query_row(
                    "SELECT target_version, status, started_at, completed_at, failed_at, failure_reason,
                            duration_ms, rows_rebuilt, busy_retries, graph_generation
                     FROM identity_migration_state WHERE id = 1",
                    [],
                    |row| {
                        Ok(IdentityMigrationStatusOutput {
                            target_version: row.get(0)?,
                            status: row.get(1)?,
                            started_at: row.get::<_, Option<f64>>(2)?.map(epoch_to_iso8601),
                            completed_at: row.get::<_, Option<f64>>(3)?.map(epoch_to_iso8601),
                            failed_at: row.get::<_, Option<f64>>(4)?.map(epoch_to_iso8601),
                            failure_reason: row.get(5)?,
                            duration_ms: row.get(6)?,
                            rows_rebuilt: row.get(7)?,
                            busy_retries: row.get(8)?,
                            graph_generation: row.get(9)?,
                        })
                    },
                )
                .ok();

            if p.retry_embeddings {
                self.retry_embeddings_if_failed();
            }

            let config = self.config();
            #[cfg(feature = "lsp-overlay")]
            let lsp_providers = [
                ("rust", &calm_core::lsp::provider::RUST_ANALYZER, &config.rust.lsp),
                ("go", &calm_core::lsp::provider::GOPLS, &config.go.lsp),
                ("c", &calm_core::lsp::provider::CLANGD, &config.clang.lsp),
            ].into_iter().map(|(lang, provider, provider_cfg)| {
                let runtime = calm_core::lsp::provider::runtime_status(
                    provider, provider_cfg, &self.project_root, "not_run", 0,
                );
                LspProviderStatusOutput {
                    lang: lang.into(), support_level: runtime.support_level,
                    binary: runtime.binary, version: runtime.version,
                    profile_fingerprint: runtime.profile_fingerprint,
                    context_fingerprint: runtime.context_fingerprint,
                    status: runtime.run_status, reason: runtime.reason,
                }
            }).collect();
            #[cfg(not(feature = "lsp-overlay"))]
            let lsp_providers: Vec<LspProviderStatusOutput> = Vec::new();
            let files_total: i64 = {
                let mut discovered = Vec::new();
                calm_core::indexer::pipeline::collect_source_files(
                    &self.project_root,
                    &config.ignore,
                    &mut discovered,
                );
                discovered.len() as i64
            };

            let phase = self.phase_str();
            let indexing_error = self.last_index_error.read_ok().clone();
            let embeddings_error = self.last_embed_error.read_ok().clone();
            let watcher = WatcherHealthOutput::from(self.watcher_health_handle().read_ok().clone());
            let sn = if phase == "failed" {
                suggested(
                    "indexing_status",
                    "Indexing failed — check indexing_error, fix the underlying issue, then restart or retry",
                )
            } else if watcher.lifecycle == "degraded" || watcher.freshness == "stale" {
                suggested(
                    "indexing_status",
                    "Filesystem watcher is unavailable or index freshness is stale — inspect watcher health; periodic full reconciliation remains the safety fallback",
                )
            } else if phase == "ready" {
                suggested("locate", "Index ready — begin exploration")
            } else {
                suggested(
                    "indexing_status",
                    "Still indexing — poll again or use search/source while edges build",
                )
            };

            #[cfg(feature = "scip-overlay")]
            let scip_overlay = {
                let rust_cfg = self.config().rust;
                calm_core::scip::overlay_status(&conn, &self.project_root, &rust_cfg)
                    .map(ScipOverlayStatusOutput::from)
            };
            #[cfg(not(feature = "scip-overlay"))]
            let scip_overlay: Option<ScipOverlayStatusOutput> = None;

            #[cfg(feature = "scip-overlay")]
            let scip_overlays = {
                let mut stmt = match conn
                    .prepare("SELECT DISTINCT language FROM file_index WHERE language IS NOT NULL")
                {
                    Ok(s) => s,
                    Err(e) => return db_error(e),
                };
                let languages: Vec<String> = match stmt.query_map([], |r| r.get(0)) {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(e) => return db_error(e),
                };
                self.per_language_overlay_statuses(&conn, &languages)
            };
            #[cfg(not(feature = "scip-overlay"))]
            let scip_overlays: Vec<PerLanguageOverlayStatus> = Vec::new();

            ToolOutcome::success(IndexingStatusOutput {
                indexing_phase: phase,
                indexing_error,
                files_indexed: files,
                files_total,
                symbols_indexed: symbols,
                edges_indexed: edges,
                embeddings_status: self.embed_status_str(),
                embeddings_error,
                edges_ready: self.edges_ready(),
                last_updated: last_updated.map(epoch_to_iso8601),
                external_proofs,
                semantic_facts,
                architecture_digest,
                identity_migration,
                graph_mode: self.last_graph_mode.read_ok().clone(),
                watcher,
                scip_overlay,
                scip_overlays,
                lsp_providers,
                formal_resolution_timeouts: calm_core::indexer::pipeline::formal_resolution_timeout_count(),
                #[cfg(feature = "scip-overlay")]
                scip_stack_graphs_overrides: Some(
                    calm_core::scip::ingest::scip_stack_graphs_override_count(),
                ),
                #[cfg(not(feature = "scip-overlay"))]
                scip_stack_graphs_overrides: None,
                #[cfg(not(feature = "stack-graphs-formal"))]
                orphaned_stack_graphs_edges: conn
                    .query_row(
                        "SELECT COUNT(*) FROM call_edges WHERE formal_source = 'stack_graphs'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok()
                    .map(|n| n as u64),
                #[cfg(feature = "stack-graphs-formal")]
                orphaned_stack_graphs_edges: None,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
    /// One `OverlayStatus` per SCIP provider (P2.6) — `scip_overlay` above
    /// stays Rust-only for backward compat with existing callers; this is
    /// the superset covering Go/Python/JS-TS/Java/C#/PHP/C-C++ too. Skips a
    /// provider entirely when `cfg.enabled == Some(false)` (same semantics
    /// as `overlay_status_for` returning `None`) rather than reporting a
    /// misleading `available: false`, and also skips a provider whose
    /// language(s) don't appear in `languages` (`file_index`'s distinct
    /// `language` column) at all — reporting "python: unavailable" for a
    /// repo with zero `.py` files is not actionable information.
    ///
    /// This second skip is a real-latency fix, not just noise reduction:
    /// `overlay_status_for` -> `resolve_binary` for Python/JS falls back to
    /// spawning `npx --yes @sourcegraph/scip-<lang> --version` whenever no
    /// standalone binary is on `PATH` (the common case) — a real npm/npx
    /// round trip, ~1-1.5s each even cache-warm (measured directly against
    /// this repo's own environment). Before this fix, both probes ran
    /// unconditionally on *every* `repo_overview`/`indexing_status` call
    /// regardless of whether the project used Python or JS at all, adding
    /// several fixed seconds — independent of repo size — that could exceed
    /// an MCP client's own response timeout and surface as a spurious
    /// "Connection closed" despite the tool call completing successfully
    /// server-side (root-caused via a 2026-07-21 cross-session investigation
    /// reproducing it against an empty single-file project).
    #[cfg(feature = "scip-overlay")]
    pub(crate) fn per_language_overlay_statuses(
        &self,
        conn: &rusqlite::Connection,
        languages: &[String],
    ) -> Vec<PerLanguageOverlayStatus> {
        let config = self.config();
        // One discovery pass is shared by all provider status checks.  In
        // particular this includes transitive TypeScript `extends` files,
        // which must be fingerprinted exactly as the run path fingerprints
        // them without multiplying filesystem walks by provider count.
        let catalog =
            calm_core::indexer::refresh::InputCatalog::new(&self.project_root, &config.ignore);
        let present = |tags: &[&str]| tags.iter().any(|t| languages.iter().any(|l| l == t));
        // Indexed-file count behind each provider's `last_match_rate` (F5) —
        // the denominator a reader needs to tell a real weak signal from a
        // small-sample artifact. One provider can cover several
        // `file_index.language` tags (javascript = js + ts, c = c + cpp).
        let file_count_for = |tags: &[&str]| -> Option<i64> {
            let placeholders = std::iter::repeat_n("?", tags.len())
                .collect::<Vec<_>>()
                .join(",");
            conn.query_row(
                &format!("SELECT COUNT(*) FROM file_index WHERE language IN ({placeholders})"),
                rusqlite::params_from_iter(tags.iter().copied()),
                |r| r.get(0),
            )
            .ok()
        };
        let mut out = Vec::new();
        if present(&["rust"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::RUST,
                conn,
                &self.project_root,
                &config.rust.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "rust",
                s,
                file_count_for(&["rust"]),
            ));
        }
        if present(&["go"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::GO,
                conn,
                &self.project_root,
                &config.go.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "go",
                s,
                file_count_for(&["go"]),
            ));
        }
        if present(&["python"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::PYTHON,
                conn,
                &self.project_root,
                &config.python.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "python",
                s,
                file_count_for(&["python"]),
            ));
        }
        // TYPESCRIPT is the one provider tagged differently from its
        // `file_index.language` values — it covers both `"javascript"` and
        // `"typescript"` sources under the single `"javascript"` tag (see
        // `PerLanguageOverlayStatus::new` call below), so presence must
        // check both.
        if present(&["javascript", "typescript"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::TYPESCRIPT,
                conn,
                &self.project_root,
                &config.js.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "javascript",
                s,
                file_count_for(&["javascript", "typescript"]),
            ));
        }
        if present(&["java"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::JAVA,
                conn,
                &self.project_root,
                &config.java.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "java",
                s,
                file_count_for(&["java"]),
            ));
        }
        if present(&["csharp"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::CSHARP,
                conn,
                &self.project_root,
                &config.csharp.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "csharp",
                s,
                file_count_for(&["csharp"]),
            ));
        }
        if present(&["php"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::PHP,
                conn,
                &self.project_root,
                &config.php.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "php",
                s,
                file_count_for(&["php"]),
            ));
        }
        // CLANG covers both `"c"` and `"cpp"` `file_index.language` values
        // under the single `"c"` tag — same both-tags reasoning as
        // TYPESCRIPT above.
        if present(&["c", "cpp"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::CLANG,
                conn,
                &self.project_root,
                &config.clang.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "c",
                s,
                file_count_for(&["c", "cpp"]),
            ));
        }
        if present(&["ruby"])
            && let Some(s) = calm_core::scip::overlay_status_for_with_catalog(
                &calm_core::scip::provider::RUBY,
                conn,
                &self.project_root,
                &config.ruby.scip,
                &catalog,
            )
        {
            out.push(PerLanguageOverlayStatus::new(
                "ruby",
                s,
                file_count_for(&["ruby"]),
            ));
        }
        out
    }
    #[tool(
        name = "session_context",
        description = "USE WHEN: after 10+ tool calls without convergence, or when starting a new sub-task. Tracks explored symbols, files, and tool call count.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn session_context(&self) -> Json<ToolOutcome<SessionContextOutput>> {
        Json(self.timed_tool("session_context", || {
            // Release the lock before DB queries — avoid deadlock if db() is also contended.
            let (tool_calls, explored_symbols, explored_files, last_progress_at, session_started_at) = {
                let log = self.session_log.lock_ok();
                (
                    log.tool_calls,
                    log.explored_symbols.keys().cloned().collect::<Vec<_>>(),
                    log.explored_files.keys().cloned().collect::<Vec<_>>(),
                    log.last_progress_at,
                    log.session_started_at.clone(),
                )
            };
            let mut files_pending_diff_impact = self.written_files_snapshot();
            files_pending_diff_impact.sort();
            let pending_diff_impact = !files_pending_diff_impact.is_empty();

            // Purely informational — AGENTS.md already documents "10+ calls
            // without convergence" as the cue to check session_context; this
            // just makes that heuristic checkable instead of guessed. Never
            // overrides suggested_next (pending_diff_impact/frontier still
            // take priority below) — loop-breaking stays the host's call.
            const STUCK_THRESHOLD: u64 = 10;
            let calls_since_progress = tool_calls.saturating_sub(last_progress_at);
            let possibly_stuck = calls_since_progress >= STUCK_THRESHOLD;

            let edges_ready = self.edges_ready();
            let (frontier, frontier_degraded) = if !edges_ready
                || (explored_files.is_empty() && explored_symbols.is_empty())
            {
                (vec![], !edges_ready)
            } else {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                let frontier = compute_frontier_entries(&conn, &explored_files, &explored_symbols);
                (frontier, false)
            };

            // Excludes this connection's own entry — a bare stdio `calm
            // serve` never inserted one in the first place (`session_id ==
            // 0`, see `for_connection`), so this is always empty there.
            // Sorted by `session_id` for deterministic output, not
            // recency — an agent wanting "most recent" can sort
            // client-side on `last_touched_at`.
            let mut other_active_sessions: Vec<SessionSummary> = self
                .active_sessions
                .lock()
                .map(|sessions| {
                    sessions
                        .values()
                        .filter(|s| s.session_id != self.session_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            other_active_sessions.sort_by_key(|s| s.session_id);

            // Backlog B5: purely derived from data already collected above --
            // no new state, no new lock. Checked against the FULL (pre-
            // max_fetched-truncation) `explored_files` so capping the display
            // list below never hides a real overlap.
            let mut overlapping_files: Vec<String> = other_active_sessions
                .iter()
                .filter_map(|s| s.last_touched_file.as_deref())
                .filter(|f| explored_files.iter().any(|e| e == f))
                .map(|f| f.to_string())
                .collect();
            overlapping_files.sort();
            overlapping_files.dedup();

            let sn = if pending_diff_impact {
                // Outranks frontier exploration — an unverified write is the
                // more urgent gap regardless of client/host (this signal
                // doesn't depend on the Claude-Code-only PreToolUse hook).
                // Plan 3 §3.5(b): same pending_diff_impact hook-enforced
                // gate as edit_lines/edit_symbol's own hint, just surfaced
                // here on a later check-in — gate:true for the same reason.
                self.filter_sn(suggested_gated(
                    "diff_impact",
                    "Files written since the last diff_impact — verify blast radius before continuing",
                ))
            } else if !frontier.is_empty() {
                self.filter_sn(suggested_with_args(
                    "file_overview",
                    "Explore top frontier file",
                    serde_json::json!({"path": frontier[0].path}),
                ))
            } else {
                self.filter_sn(suggested(
                    "repo_overview",
                    "Frontier exhausted — refresh map",
                ))
            };

            let max_fetched = self.config().session.max_fetched;
            let unique_files_explored = explored_files.len();
            let truncated =
                explored_symbols.len() > max_fetched || explored_files.len() > max_fetched;
            let explored_symbols = explored_symbols.into_iter().take(max_fetched).collect();
            let explored_files = explored_files.into_iter().take(max_fetched).collect();

            ToolOutcome::success(SessionContextOutput {
                session_started_at,
                tool_calls,
                explored_symbols,
                unique_files_explored,
                truncated,
                explored_files,
                frontier,
                frontier_degraded,
                pending_diff_impact,
                files_pending_diff_impact,
                calls_since_progress,
                possibly_stuck,
                other_active_sessions,
                overlapping_files,
                suggested_next: sn,
            })
        }))
    }
}

pub(crate) fn compute_frontier_entries(
    conn: &rusqlite::Connection,
    explored_files: &[String],
    explored_symbols: &[String],
) -> Vec<FrontierEntry> {
    use std::collections::HashSet;

    let explored_set: HashSet<&str> = explored_files.iter().map(|s| s.as_str()).collect();

    // Set A: files that import any explored file
    let mut set_a: HashSet<String> = HashSet::new();
    if !explored_files.is_empty() {
        query_paths_chunked(
            conn,
            "SELECT DISTINCT from_path FROM import_edges WHERE to_path IN",
            explored_files,
            &mut set_a,
        );
    }

    // Set B: files containing callers of explored symbols. PATTERN-DEBT
    // call-edges-missing-ruled-out-filter: a SCIP-disproven caller must not
    // suggest an unrelated file as a frontier to explore next — condition
    // ordered before `to_symbol IN` since query_paths_chunked appends the
    // `(?1, ...) AND from_path IS NOT NULL` tail right after this prefix.
    let mut set_b: HashSet<String> = HashSet::new();
    if !explored_symbols.is_empty() {
        query_paths_chunked(
            conn,
            "SELECT DISTINCT from_path FROM call_edges WHERE ruled_out_by_scip = 0 AND to_symbol IN",
            explored_symbols,
            &mut set_b,
        );
    }

    // Union minus already-explored; tag each with reason
    let mut result: Vec<FrontierEntry> = set_a
        .union(&set_b)
        .filter(|p| !explored_set.contains(p.as_str()))
        .map(|p| {
            let in_a = set_a.contains(p);
            let in_b = set_b.contains(p);
            let reason = match (in_a, in_b) {
                (true, true) => "both",
                (true, false) => "imported_by_explored",
                _ => "contains_callers_of_explored",
            };
            FrontierEntry {
                path: p.clone(),
                reason: reason.to_string(),
            }
        })
        .collect();

    // Deterministic order: "both" first, then by path
    result.sort_by(|a, b| {
        let rank = |r: &str| match r {
            "both" => 0,
            "imported_by_explored" => 1,
            _ => 2,
        };
        rank(&a.reason)
            .cmp(&rank(&b.reason))
            .then(a.path.cmp(&b.path))
    });
    result
}

// ---------------------------------------------------------------------------
// Tool 1: repo_overview
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema)]
pub(crate) struct FrontierEntry {
    pub(crate) path: String,
    pub(crate) reason: String, // "imported_by_explored" | "contains_callers_of_explored" | "both"
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SessionContextOutput {
    pub(crate) session_started_at: String,
    pub(crate) tool_calls: u64,
    pub(crate) explored_symbols: Vec<String>,
    pub(crate) explored_files: Vec<String>,
    /// True total before any `config.session.max_fetched` truncation of
    /// `explored_symbols`/`explored_files` below.
    pub(crate) unique_files_explored: usize,
    /// True when `explored_symbols`/`explored_files` were capped at
    /// `config.session.max_fetched` — a long session can otherwise dump an
    /// unbounded list into every `session_context` call.
    pub(crate) truncated: bool,
    pub(crate) frontier: Vec<FrontierEntry>,
    pub(crate) frontier_degraded: bool,
    /// True when `edit_lines`/`edit_symbol` wrote a file since the last
    /// `diff_impact` call — a host-agnostic version of the Claude-Code-only
    /// PreToolUse hook's commit/push gate, visible to any MCP client.
    pub(crate) pending_diff_impact: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) files_pending_diff_impact: Vec<String>,
    /// Tool calls since `explored_files`/`explored_symbols` last gained a
    /// genuinely new entry — informational only, never enforced.
    pub(crate) calls_since_progress: u64,
    /// `calls_since_progress >= 10` — matches AGENTS.md's documented "after
    /// 10+ calls without convergence" cue for calling this tool.
    pub(crate) possibly_stuck: bool,
    /// Every *other* connection currently sharing this daemon (this
    /// session's own entry excluded) — always empty under a bare stdio
    /// `calm serve`, where there is only ever one connection by
    /// construction. Lets an agent notice "someone else is already editing
    /// file X" before stepping on the same area, without needing full A2A
    /// protocol support — see `CalmServer::active_sessions`.
    pub(crate) other_active_sessions: Vec<SessionSummary>,
    /// Backlog B5 (docs/plans/2026-07-14-calm-agent-experience-audit-and-
    /// backlog.md): files in `explored_files` (untruncated, before
    /// `max_fetched` capping) that also match some OTHER active session's
    /// `last_touched_file` -- a narrow, purely-derived overlap signal built
    /// entirely from data `other_active_sessions` already carries, not a new
    /// subsystem. Informational only, like `possibly_stuck` -- never gates
    /// or reorders `suggested_next`; no reservation/locking semantics.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) overlapping_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 12: diff_impact
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct IndexingStatusParams {
    /// `true` to re-attempt loading the embedding model and re-embedding,
    /// but only if the current `embeddings_status` is `"failed"` or
    /// `"offline_unavailable"` — a no-op otherwise (already succeeded, or
    /// already in progress).
    #[serde(default)]
    pub(crate) retry_embeddings: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IndexingStatusOutput {
    pub(crate) indexing_phase: String,
    /// Error message from the most recent indexing failure, present only
    /// when `indexing_phase == "failed"` — see `IndexingPhase::Failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) indexing_error: Option<String>,
    pub(crate) files_indexed: i64,
    /// Tier-0 source files currently discoverable on disk (respects
    /// `config.ignore`) — compare against `files_indexed` to see whether the
    /// index is behind what's actually in the project tree.
    pub(crate) files_total: i64,
    pub(crate) symbols_indexed: i64,
    pub(crate) edges_indexed: i64,
    pub(crate) embeddings_status: String,
    /// Error message from the most recent embeddings failure, present only
    /// when `embeddings_status` is `"failed"` or `"offline_unavailable"` —
    /// see `EmbedStatus::Failed`/`OfflineUnavailable`. `"disabled"` means
    /// `semantic_search.enabled` is `false` in config, not a failure — no
    /// error accompanies it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) embeddings_error: Option<String>,
    pub(crate) edges_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_updated: Option<String>,
    /// D4 proof freshness, grouped by persisted evidence state. A formal edge
    /// is only current external proof when its corresponding record is fresh.
    pub(crate) external_proofs: ExternalProofStatusOutput,
    /// Tier 1 semantic facts (2026-08-07 roadmap T1) coverage — extends/
    /// implements, explicit throws, field writes — see
    /// `indexer::semantic_facts`'s module doc comment for exactly what is
    /// and isn't captured per language. All-zero (not absent) on a repo
    /// with no supported languages, same "checked, found none" contract
    /// `SymbolInfoOutput.type_relations`/`.effects` already use.
    pub(crate) semantic_facts: SemanticFactsStatusOutput,
    /// Tier 2 Architecture Digest (2026-08-07 roadmap T2) coverage — see
    /// `graph::digest`'s module doc comment. Compare `symbols_with_digest`
    /// against `symbols_indexed` to see how much of the graph currently
    /// has a rendered digest (only `function`/`method`/`class`/`struct`/
    /// `trait`/`interface`/`constructor` kinds are digestable).
    pub(crate) architecture_digest: ArchitectureDigestStatusOutput,
    /// Diagnostic state of the one-transaction CallSite identity migration.
    /// Absent until a legacy database actually requires that migration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) identity_migration: Option<IdentityMigrationStatusOutput>,
    /// Which graph-rebuild path the most recent non-noop reindex took:
    /// `"full"`, `"incremental"`, or `"full_fallback:<reason>"` (Phase B
    /// L6 — `GraphMode::label`). Absent until this process has served one
    /// non-noop reindex (edit tool or file watcher). Lets an agent confirm
    /// the incremental path is actually engaged rather than silently
    /// falling back to full rebuilds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) graph_mode: Option<String>,
    /// Observation and refresh health of the background file watcher. This is
    /// intentionally not folded into `indexing_phase` or `graph_mode`: those
    /// describe the last index build, while this describes whether future disk
    /// changes are observed and reconciled safely.
    pub(crate) watcher: WatcherHealthOutput,
    /// `None` when this build wasn't compiled with the `scip-overlay` feature,
    /// or `rust.scip.enabled` is explicitly `false` — nothing to report.
    /// Otherwise reflects whether Rust call edges are currently up to date
    /// with SCIP-upgraded (`formal`) confidence — see
    /// `calm_core::scip::overlay_status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scip_overlay: Option<ScipOverlayStatusOutput>,
    /// Superset of `scip_overlay` covering every SCIP provider (P2.6) —
    /// `rust`/`go`/`python`/`javascript`/`java`/`csharp`/`php`/`c` — instead of Rust alone. Empty when
    /// this build lacks the `scip-overlay` feature. A language is omitted
    /// (not present with `available: false`) when its `enabled` config is
    /// explicitly `false`, or when the project has no files in that
    /// language at all (see `per_language_overlay_statuses`'s doc comment —
    /// this also avoids an unconditional `npx`-based probe for languages
    /// the project doesn't use) — nothing to report either way, same as
    /// `scip_overlay` being absent for the config reason.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) scip_overlays: Vec<PerLanguageOverlayStatus>,
    /// Current LSP provider probe/profile evidence. No automatic LSP session is
    /// started here: `status=not_run` remains honest until `lsp_refresh` runs.
    pub(crate) lsp_providers: Vec<LspProviderStatusOutput>,
    /// ADR-A1: count of formal-resolution (StackGraph tier-3) cancellations
    /// since this process started -- see `calm_core::indexer::pipeline::
    /// formal_resolution_timeout_count`. A cancelled resolution (hit
    /// `RESOLVE_TIMEOUT` under load) used to be silently indistinguishable
    /// from "resolved, found nothing", so a file's call edges could flip
    /// between `formal` and `textual` confidence across reindexes with no
    /// visible signal. A nonzero, growing value here means that is
    /// happening on this repo right now -- worth investigating which
    /// files/languages before trusting `formal` counts as fully stable.
    pub(crate) formal_resolution_timeouts: u64,
    /// Số edge SCIP đã ghi đè 1 verdict stack-graphs kể từ khi process khởi
    /// động -- xem `calm_core::scip::ingest::scip_stack_graphs_override_count`.
    /// `None` khi build thiếu feature `scip-overlay` (không có gì để báo
    /// cáo). Một con số dương, tăng dần trên 1 repo thật là tín hiệu đáng
    /// điều tra thêm trước khi tin tuyệt đối `formal_source = 'stack_graphs'`
    /// trên các ngôn ngữ chưa có SCIP overlay (hiện: Java).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scip_stack_graphs_overrides: Option<u64>,
    /// D1 (2026-07-30 stack-graphs-demotion-lever, FM2): count of
    /// `call_edges` rows still carrying `formal_source = 'stack_graphs'`
    /// even though THIS build was compiled without the `stack-graphs-formal`
    /// feature -- i.e. verdicts from a resolver that no longer exists in
    /// this binary, which incremental reindex (only touches dirty files)
    /// can never re-verify or refresh. `None` when this build HAS the
    /// feature (nothing orphaned by definition). A nonzero value here is a
    /// real, permanent trust gap on those specific edges until a full
    /// reindex on a build with the feature restored touches them again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) orphaned_stack_graphs_edges: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct WatcherHealthOutput {
    /// OS watcher lifecycle: `not_started`, `starting`, `armed`, `backoff`,
    /// `degraded`, or `stopped`.
    pub(crate) lifecycle: String,
    /// Whether an OS subscription is currently armed. Independent of whether
    /// the last completed index is fresh.
    pub(crate) armed: bool,
    /// Freshness of the last watcher-driven refresh: `unknown`, `fresh`,
    /// `retrying`, or `stale`.
    pub(crate) freshness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_refresh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_refresh_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_reconciliation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_reconciliation_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
    /// Failures to create/maintain the OS watcher itself.
    pub(crate) consecutive_failures: u32,
    /// Refresh failures while an OS watcher may still be armed.
    pub(crate) consecutive_refresh_failures: u32,
}

impl From<crate::watch_supervisor::WatcherHealth> for WatcherHealthOutput {
    fn from(health: crate::watch_supervisor::WatcherHealth) -> Self {
        Self {
            lifecycle: health.lifecycle.as_str().to_owned(),
            armed: health.armed,
            freshness: health.freshness.as_str().to_owned(),
            last_event: health
                .last_event_unix
                .map(|secs| epoch_to_iso8601(secs as f64)),
            last_refresh: health
                .last_refresh_unix
                .map(|secs| epoch_to_iso8601(secs as f64)),
            last_refresh_kind: health
                .last_refresh_kind
                .map(|kind| kind.as_str().to_owned()),
            last_reconciliation: health
                .last_reconciliation_unix
                .map(|secs| epoch_to_iso8601(secs as f64)),
            last_reconciliation_reason: health.last_reconciliation_reason.map(str::to_owned),
            last_error: health.last_error,
            consecutive_failures: health.consecutive_failures,
            consecutive_refresh_failures: health.consecutive_refresh_failures,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LspProviderStatusOutput {
    pub(crate) lang: String,
    pub(crate) support_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) binary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    pub(crate) profile_fingerprint: String,
    pub(crate) context_fingerprint: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Default, Serialize, JsonSchema)]
pub(crate) struct ExternalProofStatusOutput {
    pub(crate) fresh: u64,
    pub(crate) stale: u64,
    pub(crate) legacy: u64,
    pub(crate) unverified: u64,
    pub(crate) rejected: u64,
}

#[derive(Default, Serialize, JsonSchema)]
pub(crate) struct SemanticFactsStatusOutput {
    pub(crate) type_relations_total: i64,
    /// `target_text` resolved to a real symbol in the same file — see
    /// `db::schema`'s `type_relations` table comment.
    pub(crate) type_relations_resolved: i64,
    /// `target_text` recorded but not resolved (cross-file, or no matching
    /// same-file class) — still a real fact, just not yet linkable.
    pub(crate) type_relations_textual: i64,
    pub(crate) explicit_throws: i64,
    pub(crate) write_fields: i64,
    /// Per-language breakdown, only languages with at least one fact of
    /// either kind. Lets an agent tell "this language is under-covered"
    /// apart from "the repo just has none of this language" — see
    /// `indexer::semantic_facts`'s module doc comment for the per-language
    /// scope cuts (e.g. Go has none of these fact kinds at all).
    pub(crate) by_language: Vec<SemanticFactsLanguageCountOutput>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SemanticFactsLanguageCountOutput {
    pub(crate) language: String,
    pub(crate) type_relations: i64,
    pub(crate) explicit_throws: i64,
    pub(crate) write_fields: i64,
}

#[derive(Default, Serialize, JsonSchema)]
pub(crate) struct ArchitectureDigestStatusOutput {
    pub(crate) symbols_with_digest: i64,
    /// Participates in a call cycle (Tarjan SCC over `Formal`/`Resolved`
    /// edges, or a direct self-loop) — see `graph::digest::compute_recursive_symbols`.
    pub(crate) recursive_symbols: i64,
    /// Digest's callee/effect lists were capped (`MAX_CALLEES_SHOWN` etc in
    /// `graph::digest`) — `rendered_text` is a real subset for these, not
    /// the full picture.
    pub(crate) truncated_digests: i64,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IdentityMigrationStatusOutput {
    pub(crate) target_version: i64,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rows_rebuilt: Option<i64>,
    pub(crate) busy_retries: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) graph_generation: Option<i64>,
}

/// Local mirror of `calm_core::scip::OverlayStatus` — that type lives in
/// `calm-core`, which doesn't depend on `schemars`, so it can't derive
/// `JsonSchema` itself. Only exists when this crate is built with the
/// `scip-overlay` feature (the same gate `calm_core::scip` itself is behind).
#[cfg(feature = "scip-overlay")]
#[derive(Serialize, JsonSchema)]
pub(crate) struct ScipOverlayStatusOutput {
    /// `rust-analyzer` binary was found (PATH/rustup/VS Code) at last check.
    pub(crate) available: bool,
    /// `false` means Rust source has changed since the last overlay run (or
    /// none has ever run) — the next non-noop incremental reindex will
    /// actually invoke rust-analyzer again rather than cache-skip.
    pub(crate) up_to_date: bool,
    /// Fraction (0.0-1.0) of SCIP-resolved call sites represented by a
    /// `formal` edge as of the last real overlay run — absent if it's never
    /// actually run. A low value alongside a healthy `.scip` file usually
    /// means indexer-subroot paths aren't rebased correctly for wherever the
    /// indexer ran (see `parse::parse_index`'s `rebase_prefix`). Stale the
    /// instant `up_to_date` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_match_rate: Option<f64>,
    /// New `call_edges` rows the last real overlay run inserted for a call
    /// site tree-sitter's own candidate selection dropped entirely (e.g. name
    /// fan-out past `MAX_CALLEE_CANDIDATES`) — absent if it's never actually
    /// run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_inserted: Option<usize>,
    /// ISO8601 timestamp of that same last real (non-cache-skip) run,
    /// absent if it's never actually run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_run: Option<String>,
}

#[cfg(feature = "scip-overlay")]
impl From<calm_core::scip::OverlayStatus> for ScipOverlayStatusOutput {
    fn from(s: calm_core::scip::OverlayStatus) -> Self {
        Self {
            available: s.available,
            up_to_date: s.up_to_date,
            last_match_rate: s.last_match_rate,
            last_inserted: s.last_inserted,
            last_run: s.last_run_unix.map(|secs| epoch_to_iso8601(secs as f64)),
        }
    }
}

/// Stub so `IndexingStatusOutput`'s `scip_overlay` field type-checks
/// identically regardless of the `scip-overlay` feature — always `None` when
/// this build lacks the feature (see the `#[cfg(not(...))]` binding at the
/// `indexing_status` call site).
#[cfg(not(feature = "scip-overlay"))]
#[derive(Serialize, JsonSchema)]
pub(crate) struct ScipOverlayStatusOutput {
    pub(crate) available: bool,
    pub(crate) up_to_date: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_match_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_inserted: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_run: Option<String>,
}

/// One `ScipOverlayStatusOutput` tagged with its `file_index.language`
/// value — see `IndexingStatusOutput::scip_overlays`.
#[cfg(feature = "scip-overlay")]
#[derive(Serialize, JsonSchema)]
pub(crate) struct PerLanguageOverlayStatus {
    pub(crate) lang: String,
    #[serde(flatten)]
    pub(crate) status: ScipOverlayStatusOutput,
    /// One-line install command for this language's external SCIP indexer,
    /// present only when `status.available == false`. Turns "raw data, not
    /// a verdict" (see `HealthSummary::weak_cross_reference_languages`'s own
    /// doc comment) into something actionable instead of requiring the
    /// reader to already know each provider's install story from memory —
    /// 2026-07-15 UX audit finding: `available: false` alone gives no path
    /// forward. `None` when `available == true` (nothing to suggest) — a
    /// missing hint is never itself a signal that nothing can be done, see
    /// `scip_install_hint`'s own doc comment for the "no entry yet" case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) install_hint: Option<String>,
    /// Number of indexed source files in this language — the denominator behind
    /// `last_match_rate`, so a near-zero rate over a handful of files reads as
    /// the statistical artifact it is rather than a real cross-reference
    /// quality problem (self-audit F5). Summed across every `file_index.
    /// language` this provider covers (javascript = js + ts, c = c + cpp).
    /// `None` only if the count query itself failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) indexed_file_count: Option<i64>,
}

#[cfg(feature = "scip-overlay")]
impl PerLanguageOverlayStatus {
    fn new(
        lang: &str,
        status: calm_core::scip::OverlayStatus,
        indexed_file_count: Option<i64>,
    ) -> Self {
        let install_hint = if status.available {
            None
        } else {
            scip_install_hint(lang)
        };
        Self {
            lang: lang.to_string(),
            status: ScipOverlayStatusOutput::from(status),
            install_hint,
            indexed_file_count,
        }
    }
}

/// One-line install command per SCIP provider, for `PerLanguageOverlayStatus::
/// install_hint`. Kept as plain strings rather than derived from
/// `calm_core::scip::runner`'s `resolve_binary` functions, because *search*
/// order (PATH/rustup/`$GOBIN`/...) and *install* recommendation are
/// different questions — e.g. python/javascript never need a separate
/// install step at all (bootstrap via `npx` the moment Node/npm are on
/// PATH), which no `resolve_binary` function encodes.
///
/// Verified for real (2026-07-15), not guessed: go/java/csharp/php/ruby/c
/// all installed and ran against a real binary via these exact commands (see
/// `.github/workflows/scip-nightly.yml`, one job per language) in the same
/// audit that added this function; rust's `rustup component add` is the
/// standard upstream install path. Returns `None` for any `lang` not listed
/// here — not an error, just "no one-line install story written yet",
/// mirroring `install_hint`'s own doc comment on why absence isn't a signal.
#[cfg(feature = "scip-overlay")]
fn scip_install_hint(lang: &str) -> Option<String> {
    let hint = match lang {
        "rust" => "rustup component add rust-analyzer",
        "go" => "go install github.com/scip-code/scip-go/cmd/scip-go@v0.2.7",
        "python" => {
            "install Node.js/npm — scip-python bootstraps itself via `npx` once they're on PATH"
        }
        "javascript" => {
            "install Node.js/npm — scip-typescript bootstraps itself via `npx` once they're on PATH"
        }
        "java" => {
            "install via coursier: `cs bootstrap com.sourcegraph:scip-java_2.13:<version> -o \
             scip-java` (needs a JDK + Maven/Gradle already on PATH) — also covers Kotlin in \
             mixed Java/Kotlin projects"
        }
        "csharp" => "dotnet tool install --global scip-dotnet",
        "php" => "composer global require davidrjenni/scip-php",
        "ruby" => {
            "download the platform binary from \
             https://github.com/sourcegraph/scip-ruby/releases/latest (the `gem install \
             scip-ruby` wrapper does not run standalone)"
        }
        "c" => {
            "download the platform binary from \
             https://github.com/sourcegraph/scip-clang/releases/latest — also needs a \
             compile_commands.json at the project root that COVERS the translation \
             units you care about: scip-clang only resolves files that appear in it, \
             so a partial build (tests/examples excluded) leaves those files at \
             tree-sitter's ambiguous fan-out and barely moves the match rate. \
             Generate it with all relevant targets enabled (CMake: \
             -DCMAKE_EXPORT_COMPILE_COMMANDS=ON plus your test/example options, e.g. \
             -DFMT_TEST=ON for fmtlib) — build coverage, not just tool presence, is \
             what determines how much of the graph gets a formal edge (see \
             benchmarks/resolution/README.md)."
        }
        _ => return None,
    };
    Some(hint.to_string())
}

#[cfg(all(test, feature = "scip-overlay"))]
mod scip_install_hint_tests {
    use super::*;

    #[test]
    fn install_hint_is_none_when_available() {
        let status = calm_core::scip::OverlayStatus {
            available: true,
            up_to_date: true,
            last_match_rate: None,
            last_inserted: None,
            last_run_unix: None,
        };
        let out = PerLanguageOverlayStatus::new("go", status, None);
        assert_eq!(
            out.install_hint, None,
            "an available provider has nothing to suggest installing"
        );
    }

    #[test]
    fn install_hint_gives_a_real_command_for_every_known_provider_when_unavailable() {
        let status = calm_core::scip::OverlayStatus {
            available: false,
            up_to_date: false,
            last_match_rate: None,
            last_inserted: None,
            last_run_unix: None,
        };
        for lang in [
            "rust",
            "go",
            "python",
            "javascript",
            "java",
            "csharp",
            "php",
            "ruby",
            "c",
        ] {
            let out = PerLanguageOverlayStatus::new(lang, status.clone(), None);
            assert!(
                out.install_hint.is_some(),
                "expected an install hint for {lang}, got None"
            );
        }
    }

    #[test]
    fn install_hint_is_none_for_an_unknown_language_even_when_unavailable() {
        let status = calm_core::scip::OverlayStatus {
            available: false,
            up_to_date: false,
            last_match_rate: None,
            last_inserted: None,
            last_run_unix: None,
        };
        assert_eq!(scip_install_hint("cobol"), None);
        let out = PerLanguageOverlayStatus::new("cobol", status, None);
        assert_eq!(out.install_hint, None);
    }
}

/// Stub so `IndexingStatusOutput`'s `scip_overlays` field type-checks
/// identically regardless of the `scip-overlay` feature.
#[cfg(not(feature = "scip-overlay"))]
#[derive(Serialize, JsonSchema)]
pub(crate) struct PerLanguageOverlayStatus {
    pub(crate) lang: String,
}

// ---------------------------------------------------------------------------
// Tool 14: locate
// ---------------------------------------------------------------------------
