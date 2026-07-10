use super::common::*;
use super::*;

#[rmcp::tool_router(router = "lsp_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "lsp_refresh",
        description = "Manually run the LSP resolve-time overlay (rust-analyzer textDocument/definition over stdio) right now, bypassing the configured refresh policy — this overlay never runs automatically on save by default (rust.lsp.policy defaults to on_demand, unlike SCIP's on_save). Upgrades ambiguous/textual call edges to formal by resolving each call site interactively against a live rust-analyzer session. USE WHEN: you need formal-tier call edges for Rust immediately and rust-analyzer is available. Can be slow — spawns a persistent LSP server and does one round-trip per unresolved call site — not for routine/automatic use."
    )]
    pub(crate) fn lsp_refresh(
        &self,
        Parameters(_p): Parameters<LspRefreshParams>,
    ) -> Json<ToolOutcome<LspRefreshOutput>> {
        Json(self.timed_tool("lsp_refresh", || {
            #[cfg(feature = "lsp-overlay")]
            {
                // Same contended-write exception as scip_refresh (see its own
                // comment) — a rare, explicit, user-initiated action, not a
                // hot path, covered by open_writer's busy_timeout.
                let conn = match calm_core::db::conn::open_writer(&self.db_path) {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                let config = calm_core::config::load_config(&self.project_root).unwrap_or_default();
                match calm_core::lsp::refresh(&conn, &self.project_root, &config, true) {
                    Ok(stats) => ToolOutcome::success(LspRefreshOutput {
                        upgraded: stats.upgraded,
                        attempted: stats.attempted,
                        match_rate: stats.match_rate,
                        suggested_next: self.filter_sn(suggested(
                            "indexing_status",
                            "Check the graph for newly formal edges",
                        )),
                    }),
                    Err(e) => {
                        ToolOutcome::error(error_detail("LSP_REFRESH_FAILED", &e.to_string(), true))
                    }
                }
            }
            #[cfg(not(feature = "lsp-overlay"))]
            {
                ToolOutcome::error(error_detail(
                    "FEATURE_UNAVAILABLE",
                    "this build wasn't compiled with the lsp-overlay feature",
                    false,
                ))
            }
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct LspRefreshParams {}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LspRefreshOutput {
    /// Edges actually flipped to `formal`/`formal_source='lsp'` (counted
    /// from UPDATE rowcounts, so a concurrent reindex can't inflate it).
    pub(crate) upgraded: usize,
    /// Call sites queried against the live rust-analyzer session — a low
    /// `upgraded/attempted` ratio usually means the residual edges are ones
    /// rust-analyzer itself can't resolve (macros, dynamic dispatch), since
    /// batch SCIP already claimed everything the same engine could prove.
    pub(crate) attempted: usize,
    pub(crate) match_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
