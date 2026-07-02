use super::common::*;
use super::*;

impl CodeIntelligenceServer {
    #[tool(
        name = "repo_overview",
        description = "ALWAYS call this FIRST at the start of every session — never skip. USE WHEN: starting a new session, switching projects, or after server restart. NOT FOR: per-file details (use file_overview), searching for symbols (use search/locate)."
    )]
    pub(crate) fn repo_overview(&self) -> String {
        self.timed_tool("repo_overview", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let total_symbols: i64 = conn
                .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
                .unwrap_or(0);
            let total_files: i64 = conn
                .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                .unwrap_or(0);

            let mut stmt = conn
                .prepare("SELECT DISTINCT language FROM file_index WHERE language IS NOT NULL")
                .unwrap();
            let languages: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();

            const ENTRY_POINTS_LIMIT: usize = 20;
            let entry_points: Vec<EntryPointItem> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT qualified_name, path FROM symbols \
                         WHERE is_entry_point = 1 LIMIT ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![ENTRY_POINTS_LIMIT as i64], |r| {
                    Ok(EntryPointItem {
                        qualified_name: r.get(0)?,
                        path: r.get(1)?,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            // Top-level directory (or bare filename for root files) of each
            // indexed file, grouped to give a coarse architectural map.
            let module_map: Vec<ModuleEntry> = {
                let mut stmt = conn
                    .prepare("SELECT path, symbol_count FROM file_index")
                    .unwrap();
                let rows: Vec<(String, i64)> = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect();

                let mut by_module: std::collections::BTreeMap<String, (i64, i64)> =
                    std::collections::BTreeMap::new();
                for (path, symbol_count) in rows {
                    let module = path
                        .split('/')
                        .next()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&path)
                        .to_string();
                    let entry = by_module.entry(module).or_insert((0, 0));
                    entry.0 += 1;
                    entry.1 += symbol_count;
                }
                let mut modules: Vec<ModuleEntry> = by_module
                    .into_iter()
                    .map(|(name, (file_count, symbol_count))| ModuleEntry {
                        name,
                        file_count,
                        symbol_count,
                    })
                    .collect();
                modules.sort_by(|a, b| b.file_count.cmp(&a.file_count).then(a.name.cmp(&b.name)));
                modules
            };

            let hub_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM symbols WHERE is_hub = 1", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            let health_summary = HealthSummary {
                hub_count,
                edges_ready: self.edges_ready(),
            };

            let phase = self.phase_str();
            let embed_status = self.embed_status_str();
            let sn = if phase != "ready" {
                suggested("indexing_status", "Monitor until phase=ready before using graph tools")
            } else if embed_status == "failed" {
                suggested_with_args("indexing_status", "Recover embeddings", serde_json::json!({"retry_embeddings": true}))
            } else {
                suggested("locate", "Start exploration")
            };

            serde_json::to_string_pretty(&RepoOverviewOutput {
                languages,
                indexing_phase: phase,
                embeddings_status: embed_status,
                total_modules: total_files,
                total_symbols,
                total_files,
                truncated: false,
                entry_points,
                module_map,
                health_summary,
                workflow_guide: r#"WORKFLOW (8 stages) — follow suggested_next in every response:
1 ORIENT   : repo_overview (ALWAYS first) → hotspots
2 LOCATE   : locate(query) [= search+file_overview+symbol_info in 1 call] | search(kind="hybrid") | file_overview(path)
3 INSPECT  : source(symbol) | understand(query) [= locate+source+callers in 1 call]
4 TRACE    : callers / callees / path / dependencies — map blast radius
5 PRE-EDIT : edit_context(symbol) — MANDATORY before ANY edit, never skip
6 EDIT     : native file tools only
7 VERIFY   : diff_impact(staged=true) — MANDATORY before commit/push, never skip
8 RECOVER  : session_context() after 10+ calls | indexing_status() when index unclear
RULES: Never use native grep/read on project files. is_hub:true → extra caution. Follow suggested_next."#.into(),
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "hotspots",
        description = "Proactive churn × complexity analysis. USE WHEN: starting exploration of a codebase or after orientation to identify high-risk files before diving in."
    )]
    pub(crate) fn hotspots(&self, #[tool(aggr)] p: HotspotsParams) -> String {
        self.timed_tool("hotspots", || {
            let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
            let hc = &config.hotspots;
            let top_n = p.top_n.unwrap_or(hc.default_top_n);
            let since = p.since.unwrap_or_else(|| hc.default_since.clone());
            let min_churn = p.min_churn.unwrap_or(hc.default_min_churn as i64);

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let result = {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
                };
                ci_core::analysis::hotspot::compute_hotspots(
                    &self.project_root,
                    &conn,
                    hc,
                    top_n,
                    &since,
                    min_churn,
                    p.include_symbols,
                )
            };

            let hotspots: Vec<HotspotEntryOutput> = result
                .hotspots
                .into_iter()
                .map(HotspotEntryOutput::from)
                .collect();
            let count = hotspots.len();

            let sn = hotspots.first().map(|h| SuggestedNext {
                tool: "file_overview".into(),
                reason: "Inspect highest-risk file".into(),
                args: Some(serde_json::json!({"path": h.path})),
            });

            serde_json::to_string_pretty(&HotspotsOutput {
                hotspots,
                count,
                git_available: result.git_available,
                since: result.since,
                total_files_analyzed: result.total_files_analyzed,
                hotspot_method: result.hotspot_method,
                note: result.note,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EntryPointItem {
    pub(crate) qualified_name: String,
    pub(crate) path: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ModuleEntry {
    pub(crate) name: String,
    pub(crate) file_count: i64,
    pub(crate) symbol_count: i64,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HealthSummary {
    pub(crate) hub_count: i64,
    pub(crate) edges_ready: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RepoOverviewOutput {
    pub(crate) languages: Vec<String>,
    pub(crate) indexing_phase: String,
    pub(crate) embeddings_status: String,
    pub(crate) total_modules: i64,
    pub(crate) total_symbols: i64,
    pub(crate) total_files: i64,
    pub(crate) truncated: bool,
    pub(crate) entry_points: Vec<EntryPointItem>,
    pub(crate) module_map: Vec<ModuleEntry>,
    pub(crate) health_summary: HealthSummary,
    pub(crate) workflow_guide: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 2: search
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct HotspotsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_churn: Option<i64>,
    #[serde(default)]
    pub(crate) include_symbols: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotChurnOutput {
    pub(crate) commit_count: i64,
    pub(crate) authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_changed: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotComplexityOutput {
    pub(crate) symbol_count: i64,
    pub(crate) hub_count: i64,
    pub(crate) avg_caller_count: f64,
    pub(crate) connected_coreness_count: i64,
    pub(crate) language: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotSymbolOutput {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) is_hub: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) coreness: Option<i64>,
    pub(crate) caller_count: i64,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotEntryOutput {
    pub(crate) path: String,
    pub(crate) language: String,
    pub(crate) churn: HotspotChurnOutput,
    pub(crate) complexity: HotspotComplexityOutput,
    pub(crate) hotspot_score: f64,
    pub(crate) risk_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_symbols: Option<Vec<HotspotSymbolOutput>>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotsOutput {
    pub(crate) hotspots: Vec<HotspotEntryOutput>,
    pub(crate) count: usize,
    pub(crate) git_available: bool,
    pub(crate) since: String,
    pub(crate) total_files_analyzed: usize,
    pub(crate) hotspot_method: String,
    pub(crate) note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

impl From<ci_core::analysis::hotspot::HotspotEntry> for HotspotEntryOutput {
    fn from(h: ci_core::analysis::hotspot::HotspotEntry) -> Self {
        HotspotEntryOutput {
            path: h.path,
            language: h.language,
            churn: HotspotChurnOutput {
                commit_count: h.churn.commit_count,
                authors: h.churn.authors.into_iter().collect(),
                last_changed: h.churn.last_changed,
            },
            complexity: HotspotComplexityOutput {
                symbol_count: h.complexity.symbol_count,
                hub_count: h.complexity.hub_count,
                avg_caller_count: h.complexity.avg_caller_count,
                connected_coreness_count: h.complexity.connected_coreness_count,
                language: h.complexity.language,
            },
            hotspot_score: h.hotspot_score,
            risk_level: h.risk_level,
            top_symbols: h.top_symbols.map(|syms| {
                syms.into_iter()
                    .map(|s| HotspotSymbolOutput {
                        name: s.name,
                        kind: s.kind,
                        is_hub: s.is_hub,
                        coreness: s.coreness,
                        caller_count: s.caller_count,
                    })
                    .collect()
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 16: understand
// ---------------------------------------------------------------------------
