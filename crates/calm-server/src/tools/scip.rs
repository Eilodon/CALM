use super::common::*;
use super::*;

#[rmcp::tool_router(router = "scip_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "scip_refresh",
        description = "Manually run one or every SCIP provider's indexer right now (rust/go/python/javascript/java/csharp/php/c), bypassing the configured refresh policy — e.g. to force a run for an on_demand/min_interval provider without waiting. USE WHEN: you need formal-tier call edges immediately and know a source-of-truth indexer (rust-analyzer/scip-go/scip-python/scip-typescript/scip-java/scip-dotnet/scip-php/scip-clang) is available. Can block for a while (up to a few minutes for a large project, longer for Java/C++'s full build-tool invocation) since it may invoke a real external indexer — not for routine use."
    )]
    pub(crate) fn scip_refresh(
        &self,
        Parameters(p): Parameters<ScipRefreshParams>,
    ) -> Json<ToolOutcome<ScipRefreshOutput>> {
        Json(self.timed_tool("scip_refresh", || {
            #[cfg(feature = "scip-overlay")]
            {
                // A genuine, deliberate 2nd exception to the "only
                // memory_write_conn writes" rule (see its own doc comment)
                // — unlike `project_memory`, this DOES contend with the
                // indexer/watcher's writes to `call_edges`. Accepted here
                // because `scip_refresh` is a rare, explicit, user-initiated
                // action (not a hot path), and `open_writer`'s
                // `busy_timeout` already covers a brief overlap with an
                // in-flight indexing transaction the same way `remember`
                // relies on it doing.
                let conn = match calm_core::db::conn::open_writer(&self.db_path) {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                let config = calm_core::config::load_config(&self.project_root).unwrap_or_default();
                match calm_core::scip::refresh_language(
                    &conn,
                    &self.project_root,
                    &config,
                    p.lang.as_deref(),
                ) {
                    Ok(results) => {
                        let providers: Vec<ScipRefreshProviderOutput> = results
                            .into_iter()
                            .map(|(lang, stats)| ScipRefreshProviderOutput {
                                lang,
                                upgraded: stats.upgraded,
                                ruled_out: stats.ruled_out,
                                inserted: stats.inserted,
                                match_rate: stats.match_rate,
                            })
                            .collect();
                        ToolOutcome::success(ScipRefreshOutput {
                            providers,
                            suggested_next: self.filter_sn(suggested(
                                "indexing_status",
                                "Check per-language scip_overlays for the refreshed state",
                            )),
                        })
                    }
                    Err(e) => ToolOutcome::error(error_detail(
                        "SCIP_REFRESH_FAILED",
                        &e.to_string(),
                        true,
                    )),
                }
            }
            #[cfg(not(feature = "scip-overlay"))]
            {
                let _ = &p.lang;
                ToolOutcome::error(error_detail(
                    "FEATURE_UNAVAILABLE",
                    "this build wasn't compiled with the scip-overlay feature",
                    false,
                ))
            }
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ScipRefreshParams {
    /// Which provider to refresh: "rust", "go", "python", "javascript",
    /// "java", "csharp", "php", "c", or "all" (default) for every provider
    /// in the table.
    #[serde(default)]
    pub(crate) lang: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ScipRefreshOutput {
    pub(crate) providers: Vec<ScipRefreshProviderOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ScipRefreshProviderOutput {
    pub(crate) lang: String,
    pub(crate) upgraded: usize,
    pub(crate) ruled_out: usize,
    pub(crate) inserted: usize,
    pub(crate) match_rate: f64,
}
