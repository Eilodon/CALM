//! Per-connection/session state accessors split out of `common.rs`
//! (2026-08-06 hotspot split, second round — see the 2026-07-28 split that
//! extracted `detail.rs`/`outcome.rs`/`toolset.rs`; `common.rs` regrew past
//! `fitness_report`'s `hotspot_risk` gate since then). Plain data — no
//! `#[tool]`-annotated methods live here, so no `#[rmcp::tool_router]` on
//! this `impl` block. Unlike `detail`/`outcome`/`toolset`, deliberately NOT
//! re-exported through `common::*`: every item here is an `impl CalmServer`
//! method (or one private associated const), and Rust resolves
//! `self.method()`/`Self::CONST` against a type's inherent impls crate-wide
//! with no `use` needed for whichever module defines them — a re-export
//! here would have nothing to do and rustc flags it `unused_imports` (see
//! `common.rs`'s own comment at its module-level `use` block). Logic is
//! byte-for-byte identical to the pre-split version.
use super::common::*;
use super::*;

impl CalmServer {
    /// A handle the background indexer uses to advance the phase as it works.
    pub fn phase_handle(&self) -> Arc<RwLock<IndexingPhase>> {
        Arc::clone(&self.phase)
    }

    /// A handle the background indexer uses to publish an error message
    /// when `phase` transitions to `Failed` (see `IndexingPhase::Failed`).
    pub fn last_index_error_handle(&self) -> Arc<RwLock<Option<String>>> {
        Arc::clone(&self.last_index_error)
    }
    /// Shared handle to `last_graph_mode` so the file watcher (which has no
    /// `CalmServer`) can record which rebuild path each incremental reindex
    /// took — mirrors `last_index_error_handle`. See `run_watch_loop`.
    pub fn last_graph_mode_handle(&self) -> Arc<RwLock<Option<String>>> {
        Arc::clone(&self.last_graph_mode)
    }

    /// Shared watcher-health handle for the background supervisor. Unlike
    /// `phase`, it describes observation/reconciliation health, not whether a
    /// prior index build finished successfully.
    pub(crate) fn watcher_health_handle(&self) -> crate::watch_supervisor::WatcherHealthHandle {
        Arc::clone(&self.watcher_health)
    }
    /// Handles the background indexer uses to publish the loaded model + status.
    pub fn embedder_handle(&self) -> Arc<RwLock<Option<Arc<Embedder>>>> {
        Arc::clone(&self.embedder)
    }
    pub fn embed_status_handle(&self) -> Arc<RwLock<EmbedStatus>> {
        self.embed_status.clone()
    }

    pub fn coverage_handle(&self) -> Arc<RwLock<calm_core::analysis::coverage::CoverageData>> {
        self.coverage.clone()
    }

    pub fn last_embed_error_handle(&self) -> Arc<RwLock<Option<String>>> {
        self.last_embed_error.clone()
    }

    pub fn owns_indexer_lock_handle(&self) -> Arc<RwLock<bool>> {
        self.owns_indexer_lock.clone()
    }

    /// Tool names that already satisfy the session-start orientation gate
    /// (`CalmServer::call_tool`, `Config.orientation`) on their own — calling
    /// any of these IS the orientation, so the gate never needs to inject
    /// into, or block around, a call to one of them.
    const ORIENTATION_ADJACENT_TOOLS: &'static [&'static str] =
        &["repo_overview", "indexing_status", "session_context"];

    /// Whether `name` is one of `ORIENTATION_ADJACENT_TOOLS`.
    pub(crate) fn is_orientation_adjacent(name: &str) -> bool {
        Self::ORIENTATION_ADJACENT_TOOLS.contains(&name)
    }

    /// Whether the active (preset-scoped) `tool_router` registers at least
    /// one orientation-adjacent tool. `"block"` mode's only escape hatch is
    /// calling one of them — a preset that excludes the whole `orient`
    /// toolset entirely (e.g. `--preset "security"` alone, or any composed
    /// spec that subtracts it, like `"full,-orient"`) registers none of
    /// them, so a literal block there would refuse every call for the rest
    /// of the connection with no way out at all.
    pub(crate) fn orientation_escape_hatch_available(&self) -> bool {
        self.tool_router
            .list_all()
            .iter()
            .any(|t| Self::is_orientation_adjacent(t.name.as_ref()))
    }

    /// `Config.orientation.mode`, downgraded from `Block` to `Inject` when
    /// `orientation_escape_hatch_available()` is false — see
    /// `calm_core::config::OrientationMode::Block`'s own doc comment for why
    /// a literal block with no escape hatch would deadlock the connection.
    pub(crate) fn effective_orientation_mode(&self) -> calm_core::config::OrientationMode {
        let mode = self.config().orientation.mode;
        if mode == calm_core::config::OrientationMode::Block
            && !self.orientation_escape_hatch_available()
        {
            calm_core::config::OrientationMode::Inject
        } else {
            mode
        }
    }

    /// Content merged into the first non-orientation-adjacent tool response
    /// of a session under `"inject"` mode — deliberately compact (mirrors
    /// `repo_overview`'s `compact:true` shape), since the full
    /// `repo_overview` response remains one real tool call away for an
    /// agent that wants more.
    pub(crate) fn orientation_injection_text(&self) -> String {
        serde_json::json!({
            "_calm_orientation": {
                "note": "Auto-attached: this session hasn't called repo_overview yet. \
                         Call it (or the calm_workflow prompt) for the full 8-stage workflow.",
                "indexing_phase": self.phase_str(),
                "embeddings_status": self.embed_status_str(),
            }
        })
        .to_string()
    }

    /// `{"error": {code, message, recoverable}}`-shaped `ORIENTATION_REQUIRED`
    /// refusal for `"block"` mode — same envelope every other tool-level
    /// error in this server uses (see `error_detail`), so existing
    /// client-side error handling doesn't need a special case for this one.
    pub(crate) fn orientation_required_message(&self) -> String {
        serde_json::to_string(&ErrorOutput {
            error: error_detail(
                "ORIENTATION_REQUIRED",
                "call repo_overview first this session (session-start orientation gate, \
                 [orientation] mode=\"block\" in .calm/config.json) — every other tool is \
                 refused until then",
                true,
            ),
        })
        .unwrap_or_default()
    }

    /// Content merged into a tool response when this connection has written
    /// files (`written_files_snapshot`) that haven't had `diff_impact` run
    /// on them since — surfaced on every response while pending, not just
    /// when `session_context` is asked. Always advisory: `diff_impact`-
    /// before-commit can never be a hard server-side gate at all (an MCP
    /// server has no visibility into a client's own native Bash/Edit tool
    /// calls, e.g. `git commit`), so this only makes an already-real signal
    /// (`session_context.pending_diff_impact`) harder to miss, on every
    /// client uniformly, instead of adding new enforcement.
    pub(crate) fn pending_diff_impact_reminder_text(&self) -> Option<String> {
        let files = self.written_files_snapshot();
        if files.is_empty() {
            return None;
        }
        Some(
            serde_json::json!({
                "_calm_pending_diff_impact": {
                    "note": "Files written this session have not had diff_impact run on \
                             them yet — call diff_impact before commit/push.",
                    "files": files,
                }
            })
            .to_string(),
        )
    }
}
