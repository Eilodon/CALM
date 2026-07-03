use super::common::*;
use super::*;

impl CodeIntelligenceServer {
    #[tool(
        name = "indexing_status",
        description = "USE WHEN: you need file-level index stats, embedding error details, or to trigger embedding recovery. NOT a replacement for repo_overview at session start. retry_embeddings=true triggers re-download of embedding model."
    )]
    pub(crate) fn indexing_status(&self, #[tool(aggr)] p: IndexingStatusParams) -> String {
        self.timed_tool("indexing_status", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
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

            let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
            let files_total: i64 = {
                let mut discovered = Vec::new();
                ci_core::indexer::pipeline::collect_source_files(
                    &self.project_root,
                    &config.ignore,
                    &mut discovered,
                );
                discovered.len() as i64
            };

            let phase = self.phase_str();
            let sn = if phase == "ready" {
                suggested("locate", "Index ready — begin exploration")
            } else {
                suggested(
                    "indexing_status",
                    "Still indexing — poll again or use search/source while edges build",
                )
            };
            serde_json::to_string_pretty(&IndexingStatusOutput {
                indexing_phase: phase,
                files_indexed: files,
                files_total,
                symbols_indexed: symbols,
                edges_indexed: edges,
                embeddings_status: self.embed_status_str(),
                edges_ready: self.edges_ready(),
                last_updated: last_updated.map(epoch_to_iso8601),
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "session_context",
        description = "USE WHEN: after 10+ tool calls without convergence, or when starting a new sub-task. Tracks explored symbols, files, and tool call count."
    )]
    pub(crate) fn session_context(&self) -> String {
        self.timed_tool("session_context", || {
            // Release the lock before DB queries — avoid deadlock if db() is also contended.
            let (tool_calls, explored_symbols, explored_files, last_progress_at, session_started_at) = {
                let log = self.session_log.lock().unwrap();
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
                    Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
                };
                let frontier = compute_frontier_entries(&conn, &explored_files, &explored_symbols);
                (frontier, false)
            };

            let sn = if pending_diff_impact {
                // Outranks frontier exploration — an unverified write is the
                // more urgent gap regardless of client/host (this signal
                // doesn't depend on the Claude-Code-only PreToolUse hook).
                self.filter_sn(suggested(
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

            let max_fetched = ci_core::config::load_config(&self.project_root)
                .map(|c| c.session.max_fetched)
                .unwrap_or_default();
            let unique_files_explored = explored_files.len();
            let truncated =
                explored_symbols.len() > max_fetched || explored_files.len() > max_fetched;
            let explored_symbols = explored_symbols.into_iter().take(max_fetched).collect();
            let explored_files = explored_files.into_iter().take(max_fetched).collect();

            serde_json::to_string_pretty(&SessionContextOutput {
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
                suggested_next: sn,
            })
            .unwrap_or_default()
        })
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) files_pending_diff_impact: Vec<String>,
    /// Tool calls since `explored_files`/`explored_symbols` last gained a
    /// genuinely new entry — informational only, never enforced.
    pub(crate) calls_since_progress: u64,
    /// `calls_since_progress >= 10` — matches AGENTS.md's documented "after
    /// 10+ calls without convergence" cue for calling this tool.
    pub(crate) possibly_stuck: bool,
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
    /// but only if the current `embeddings_status` is `"failed"` — a no-op
    /// otherwise (already succeeded, or already in progress).
    #[serde(default)]
    pub(crate) retry_embeddings: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IndexingStatusOutput {
    pub(crate) indexing_phase: String,
    pub(crate) files_indexed: i64,
    /// Tier-0 source files currently discoverable on disk (respects
    /// `config.ignore`) — compare against `files_indexed` to see whether the
    /// index is behind what's actually in the project tree.
    pub(crate) files_total: i64,
    pub(crate) symbols_indexed: i64,
    pub(crate) edges_indexed: i64,
    pub(crate) embeddings_status: String,
    pub(crate) edges_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 14: locate
// ---------------------------------------------------------------------------
