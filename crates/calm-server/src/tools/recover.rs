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
            let edges: i64 = conn
                .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
                .unwrap_or(0);
            let last_updated: Option<f64> = conn
                .query_row("SELECT MAX(last_indexed) FROM file_index", [], |r| r.get(0))
                .ok()
                .flatten();

            if p.retry_embeddings {
                self.retry_embeddings_if_failed();
            }

            let config = self.config();
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
            let sn = if phase == "failed" {
                suggested(
                    "indexing_status",
                    "Indexing failed — check indexing_error, fix the underlying issue, then restart or retry",
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
                graph_mode: self.last_graph_mode.read_ok().clone(),
                scip_overlay,
                scip_overlays,
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
        let present = |tags: &[&str]| tags.iter().any(|t| languages.iter().any(|l| l == t));
        let mut out = Vec::new();
        if present(&["rust"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::RUST,
                conn,
                &self.project_root,
                &config.rust.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("rust", s));
        }
        if present(&["go"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::GO,
                conn,
                &self.project_root,
                &config.go.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("go", s));
        }
        if present(&["python"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::PYTHON,
                conn,
                &self.project_root,
                &config.python.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("python", s));
        }
        // TYPESCRIPT is the one provider tagged differently from its
        // `file_index.language` values — it covers both `"javascript"` and
        // `"typescript"` sources under the single `"javascript"` tag (see
        // `PerLanguageOverlayStatus::new` call below), so presence must
        // check both.
        if present(&["javascript", "typescript"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::TYPESCRIPT,
                conn,
                &self.project_root,
                &config.js.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("javascript", s));
        }
        if present(&["java"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::JAVA,
                conn,
                &self.project_root,
                &config.java.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("java", s));
        }
        if present(&["csharp"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::CSHARP,
                conn,
                &self.project_root,
                &config.csharp.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("csharp", s));
        }
        if present(&["php"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::PHP,
                conn,
                &self.project_root,
                &config.php.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("php", s));
        }
        // CLANG covers both `"c"` and `"cpp"` `file_index.language` values
        // under the single `"c"` tag — same both-tags reasoning as
        // TYPESCRIPT above.
        if present(&["c", "cpp"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::CLANG,
                conn,
                &self.project_root,
                &config.clang.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("c", s));
        }
        if present(&["ruby"])
            && let Some(s) = calm_core::scip::overlay_status_for(
                &calm_core::scip::provider::RUBY,
                conn,
                &self.project_root,
                &config.ruby.scip,
            )
        {
            out.push(PerLanguageOverlayStatus::new("ruby", s));
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

    // Set B: files containing callers of explored symbols
    let mut set_b: HashSet<String> = HashSet::new();
    if !explored_symbols.is_empty() {
        query_paths_chunked(
            conn,
            "SELECT DISTINCT from_path FROM call_edges WHERE to_symbol IN",
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
    /// Which graph-rebuild path the most recent non-noop reindex took:
    /// `"full"`, `"incremental"`, or `"full_fallback:<reason>"` (Phase B
    /// L6 — `GraphMode::label`). Absent until this process has served one
    /// non-noop reindex (edit tool or file watcher). Lets an agent confirm
    /// the incremental path is actually engaged rather than silently
    /// falling back to full rebuilds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) graph_mode: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
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
}

#[cfg(feature = "scip-overlay")]
impl PerLanguageOverlayStatus {
    fn new(lang: &str, status: calm_core::scip::OverlayStatus) -> Self {
        let install_hint = if status.available {
            None
        } else {
            scip_install_hint(lang)
        };
        Self {
            lang: lang.to_string(),
            status: ScipOverlayStatusOutput::from(status),
            install_hint,
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
        "go" => "go install github.com/scip-code/scip-go/cmd/scip-go@latest",
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
             compile_commands.json at the project root"
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
        let out = PerLanguageOverlayStatus::new("go", status);
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
            let out = PerLanguageOverlayStatus::new(lang, status.clone());
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
        let out = PerLanguageOverlayStatus::new("cobol", status);
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
