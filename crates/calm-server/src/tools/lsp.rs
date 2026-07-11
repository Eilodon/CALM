use super::common::*;
use super::*;

#[rmcp::tool_router(router = "lsp_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "lsp_refresh",
        description = "Manually run one or every LSP resolve-time overlay provider right now (rust-analyzer/gopls/clangd), bypassing the configured refresh policy — none of these run automatically on save by default (policy defaults to on_demand). Upgrades ambiguous/textual call edges to formal by resolving each call site interactively against a live LSP session. USE WHEN: you need formal-tier call edges for rust/go/c/cpp immediately and the relevant LSP server (rust-analyzer/gopls/clangd) is available. Can be slow — spawns a persistent LSP server and does one round-trip per unresolved call site — not for routine/automatic use. NETWORK: none of these three spawn via npx, but once running they may indirectly trigger the underlying build tool (cargo/go) to fetch dependencies if the project's own cache is incomplete — inherited from the environment, not added by CALM.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub(crate) fn lsp_refresh(
        &self,
        Parameters(p): Parameters<LspRefreshParams>,
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
                match calm_core::lsp::refresh_language(
                    &conn,
                    &self.project_root,
                    &config,
                    p.lang.as_deref(),
                ) {
                    Ok(results) => {
                        let providers: Vec<LspRefreshProviderOutput> = results
                            .into_iter()
                            .map(|(lang, stats)| LspRefreshProviderOutput {
                                lang,
                                upgraded: stats.upgraded,
                                attempted: stats.attempted,
                                match_rate: stats.match_rate,
                            })
                            .collect();
                        ToolOutcome::success(LspRefreshOutput {
                            providers,
                            suggested_next: self.filter_sn(suggested(
                                "indexing_status",
                                "Check the graph for newly formal edges",
                            )),
                        })
                    }
                    Err(e) => {
                        ToolOutcome::error(error_detail("LSP_REFRESH_FAILED", &e.to_string(), true))
                    }
                }
            }
            #[cfg(not(feature = "lsp-overlay"))]
            {
                let _ = &p.lang;
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
pub(crate) struct LspRefreshParams {
    /// Which provider to refresh: "rust" (rust-analyzer), "go" (gopls), "c"
    /// (clangd, covers both .c and .cpp), or "all" (default) for every
    /// provider in the table.
    #[serde(default)]
    pub(crate) lang: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LspRefreshOutput {
    pub(crate) providers: Vec<LspRefreshProviderOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LspRefreshProviderOutput {
    pub(crate) lang: String,
    /// Edges actually flipped to `formal`/`formal_source='lsp'` (counted
    /// from UPDATE rowcounts, so a concurrent reindex can't inflate it).
    pub(crate) upgraded: usize,
    /// Call sites queried against the live LSP session — a low
    /// `upgraded/attempted` ratio usually means the residual edges are ones
    /// this provider itself can't resolve (macros, dynamic dispatch), since
    /// batch SCIP (where one exists for this language) already claimed
    /// everything the same engine could prove.
    pub(crate) attempted: usize,
    pub(crate) match_rate: f64,
}
