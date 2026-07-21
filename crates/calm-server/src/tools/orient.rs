use super::common::*;
use super::*;

#[rmcp::tool_router(router = "orient_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "repo_overview",
        description = "ALWAYS call this FIRST at the start of every session — never skip. USE WHEN: starting a new session, switching projects, or after server restart. NOT FOR: per-file details (use file_overview), searching for symbols (use search/locate).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn repo_overview(
        &self,
        Parameters(p): Parameters<RepoOverviewParams>,
    ) -> Json<ToolOutcome<RepoOverviewOutput>> {
        Json(self.timed_tool("repo_overview", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let total_symbols: i64 = conn
                .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
                .unwrap_or(0);
            let total_files: i64 = conn
                .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
                .unwrap_or(0);

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

            // Plan 3 §3.5(a): compact mode skips this query entirely rather
            // than running it and discarding the result.
            const ENTRY_POINTS_LIMIT: usize = 10;
            let entry_points: Vec<EntryPointItem> = if p.compact {
                Vec::new()
            } else {
                // `main`-named functions first (the conventional place to
                // start reading), then by caller_count desc — previously
                // unordered (arbitrary rowid order) with a cap of 20; now
                // ranked and capped tighter now that is_entry_point false
                // positives (struct/enum via detect_entry_point's own-
                // attribute check) are fixed at the source (parser.rs).
                let mut stmt = match conn.prepare(
                    "SELECT qualified_name, path FROM symbols \
                     WHERE is_entry_point = 1 \
                     ORDER BY CASE WHEN name = 'main' THEN 0 ELSE 1 END, caller_count DESC \
                     LIMIT ?1",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error(e),
                };
                match stmt.query_map(rusqlite::params![ENTRY_POINTS_LIMIT as i64], |r| {
                    Ok(EntryPointItem {
                        qualified_name: r.get(0)?,
                        path: r.get(1)?,
                    })
                }) {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(e) => return db_error(e),
                }
            };

            // Top-level directory (or bare filename for root files) of each
            // indexed file, grouped to give a coarse architectural map.
            // Plan 3 §3.5(a): capped to 10 entries under compact mode (was
            // always uncapped) — sorted by file_count desc first, so the
            // cap keeps the most substantial modules.
            let module_map: Vec<ModuleEntry> = {
                let mut stmt = match conn.prepare("SELECT path, symbol_count FROM file_index") {
                    Ok(s) => s,
                    Err(e) => return db_error(e),
                };
                let rows: Vec<(String, i64)> = match stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(e) => return db_error(e),
                };

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
                if p.compact {
                    modules.truncate(10);
                }
                modules
            };

            let hub_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM symbols WHERE is_hub = 1", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            // Plan 3 §3.3 (F10) breakdown — hub_kind is NULL for a non-hub
            // symbol, so these two counts alone (not summed with hub_count)
            // tell the degree-vs-bridge split; a 'both' row counts toward
            // both.
            let hub_degree_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM symbols WHERE hub_kind IN ('degree','both')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let hub_bridge_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM symbols WHERE hub_kind IN ('bridge','both')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            // Root-cause fix for the F10 calibration bug: repo_overview is
            // mandatory-first-call-per-session (AGENTS.md Stage 1), which
            // makes it the one place an agent is guaranteed to see a local
            // config.json/.calm/config.json override before trusting any
            // hub/calibration-sensitive number this server reports.
            let config_override = calm_core::config::resolve_config_path(&self.project_root)
                .map(|path| ConfigOverrideSummary {
                    source: path.display().to_string(),
                    overridden_fields: calm_core::config::diff_from_default(&self.config()),
                });
            #[cfg(feature = "scip-overlay")]
            const WEAK_MATCH_RATE_THRESHOLD: f64 = 0.5;
            // `per_language_overlay_statuses` already restricts itself to
            // languages present in `languages` (see its doc comment) — no
            // separate language filter needed here anymore.
            #[cfg(feature = "scip-overlay")]
            let weak_cross_reference_languages: Vec<super::recover::PerLanguageOverlayStatus> =
                self.per_language_overlay_statuses(&conn, &languages)
                    .into_iter()
                    .filter(|s| {
                        !s.status.available
                            || s.status
                                .last_match_rate
                                .is_some_and(|r| r < WEAK_MATCH_RATE_THRESHOLD)
                    })
                    .collect();
            #[cfg(not(feature = "scip-overlay"))]
            let weak_cross_reference_languages: Vec<super::recover::PerLanguageOverlayStatus> =
                Vec::new();

            let health_summary = HealthSummary {
                hub_count,
                hub_degree_count,
                hub_bridge_count,
                edges_ready: self.edges_ready(),
                config_override,
                weak_cross_reference_languages,
            };

            // Count only, never content — deliberately not auto-surfacing note
            // *text* here (that would be a passive-injection memory pattern;
            // current agent-memory practice, e.g. Letta/MemGPT, favors the
            // agent deciding for itself when to call `recall()` over having
            // content pushed at it). This is just enough signal to make that
            // decision cheaply instead of guessing whether any notes exist.
            let memory_notes_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM project_memory", [], |r| r.get(0))
                .unwrap_or(0);

            // "Repo-map" style architectural skeleton (inspired by Aider's
            // PageRank-ranked repo map) — reuses `coreness` (k-core), which
            // `edit_context`'s hub/risk-gating already computes, instead of
            // standing up a second centrality metric just for this. Ranked
            // by coreness (structural embeddedness in the densest connected
            // subgraph), tie-broken by caller_count then qualified_name for
            // deterministic output. `coreness > 0` on purpose: a symbol at
            // baseline 0 is either isolated or edges aren't built yet, and
            // isn't "core" by any reasonable reading of the word — an empty
            // list is itself the honest answer in that case, not a bug.
            // `is_test = 0` excludes test helpers for the same reason
            // `hotspot::compute_hotspots` does: test code calls production
            // code structurally, which would skew "architectural
            // importance" toward test scaffolding rather than real design.
            // Gated on `edges_ready` (same convention as `symbol_info`'s
            // `coreness` field) so a not-yet-built graph reports honestly
            // empty instead of a stale/partial ranking. Plan 3 §3.5(a):
            // capped to 8 (was always 15) under compact mode.
            let core_symbols_limit: usize = if p.compact { 8 } else { 15 };
            let core_symbols: Vec<CoreSymbolItem> = if self.edges_ready() {
                let mut stmt = match conn.prepare(
                    "SELECT qualified_name, name, kind, path, coreness, caller_count, is_hub \
                     FROM symbols \
                     WHERE is_test = 0 AND coreness IS NOT NULL AND coreness > 0 \
                     ORDER BY coreness DESC, caller_count DESC, qualified_name ASC \
                     LIMIT ?1",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error(e),
                };
                match stmt.query_map(rusqlite::params![core_symbols_limit as i64], |r| {
                    Ok(CoreSymbolItem {
                        qualified_name: r.get(0)?,
                        name: r.get(1)?,
                        kind: r.get(2)?,
                        path: r.get(3)?,
                        coreness: r.get(4)?,
                        caller_count: r.get(5)?,
                        is_hub: r.get::<_, i64>(6)? != 0,
                    })
                }) {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(e) => return db_error(e),
                }
            } else {
                Vec::new()
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

            // Plan 3 §3.5(a): the single biggest chunk of a non-compact
            // response (~600+ tokens) and redundant once the client already
            // has it — e.g. baked into this repo's own AGENTS.md, which
            // every session using this MCP server reads at session start.
            let workflow_guide = (!p.compact).then(|| r#"WORKFLOW (8 stages) — follow suggested_next in every response:
1 ORIENT   : repo_overview (ALWAYS first) → hotspots, fitness_report (optional health snapshot)
2 LOCATE   : locate(query) [= search+file_overview+symbol_info in 1 call] | search(kind="hybrid"|"grep") | file_overview(path)
3 INSPECT  : source(symbol) | understand(query) [= locate+source+callers in 1 call]
4 TRACE    : callers / callees / path / dependencies — map blast radius
5 PRE-EDIT : edit_context(symbol) — MANDATORY before ANY edit, never skip
6 EDIT     : edit_symbol/edit_lines (preferred — hash-verified, risk-gated) | native file tools (new/untracked files only)
7 VERIFY   : diff_impact(staged=true) — MANDATORY before commit/push, never skip
8 RECOVER  : session_context() after 10+ calls | indexing_status() when index unclear
RULES: Never use native grep/read on project files. is_hub:true → extra caution. Follow suggested_next."#.to_string());

            ToolOutcome::success(RepoOverviewOutput {
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
                memory_notes_count,
                core_symbols,
                workflow_guide,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
    #[tool(
        name = "hotspots",
        description = "Proactive churn × complexity analysis. USE WHEN: starting exploration of a codebase or after orientation to identify high-risk files before diving in.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn hotspots(
        &self,
        Parameters(p): Parameters<HotspotsParams>,
    ) -> Json<ToolOutcome<HotspotsOutput>> {
        Json(self.timed_tool("hotspots", || {
            let config = self.config();
            let hc = &config.hotspots;
            let top_n = p.top_n.unwrap_or(hc.default_top_n);
            let since = p.since.unwrap_or_else(|| hc.default_since.clone());
            let min_churn = p.min_churn.unwrap_or(hc.default_min_churn as i64);

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let result = {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                calm_core::analysis::hotspot::compute_hotspots(
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
                gate: None,
            });

            ToolOutcome::success(HotspotsOutput {
                hotspots,
                count,
                git_available: result.git_available,
                since: result.since,
                total_files_analyzed: result.total_files_analyzed,
                hotspot_method: result.hotspot_method,
                note: result.note,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
    #[tool(
        name = "fitness_report",
        description = "Repo-wide codebase health snapshot (hub concentration, dead code, complexity, edge coverage, architecture-boundary/config-drift violations) against configurable thresholds — the same checks `calm fitness-check` runs in CI, queryable mid-session instead of waiting on a pipeline. USE WHEN: you want a big-picture health pulse-check. NOT FOR: per-file/per-symbol risk (use hotspots/edit_context for that).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn fitness_report(&self) -> Json<ToolOutcome<FitnessReportOutput>> {
        Json(self.timed_tool("fitness_report", || {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };

            // Same discovery convention as `calm fitness-check --config
            // thresholds.toml`, applied automatically here since an MCP
            // tool call has no equivalent of a CLI flag — falls back to
            // FitnessThresholds::default() when the file isn't present.
            let config_path = {
                let p = self.project_root.join("thresholds.toml");
                p.exists().then_some(p)
            };
            let thresholds =
                calm_core::fitness::load_thresholds(config_path.as_deref()).unwrap_or_default();
            let boundary_rules =
                calm_core::fitness::load_boundary_rules(config_path.as_deref()).unwrap_or_default();
            let config_drift_doc_paths =
                calm_core::fitness::load_config_drift_doc_paths(config_path.as_deref())
                    .unwrap_or_default();

            let result = match calm_core::fitness::run_fitness_check(
                &conn,
                &thresholds,
                &self.project_root,
                &self.coverage.read_ok(),
                &boundary_rules,
                &config_drift_doc_paths,
            ) {
                Ok(r) => r,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "FITNESS_CHECK_FAILED",
                        &format!("fitness check failed: {e}"),
                        true,
                    ));
                }
            };

            let sn = if result.passed {
                None
            } else {
                suggested(
                    "hotspots",
                    "Fitness check failed — investigate via highest-risk files",
                )
            };

            ToolOutcome::success(FitnessReportOutput {
                passed: result.passed,
                checks: result.checks.into_iter().map(Into::into).collect(),
                metrics: result.metrics.into(),
                boundary_violations: result
                    .boundary_violations
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                config_drift: result.config_drift.into_iter().map(Into::into).collect(),
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FitnessCheckItemOutput {
    pub(crate) metric: String,
    pub(crate) value: f64,
    pub(crate) threshold: f64,
    pub(crate) passed: bool,
    pub(crate) message: String,
}

impl From<calm_core::fitness::FitnessCheckItem> for FitnessCheckItemOutput {
    fn from(c: calm_core::fitness::FitnessCheckItem) -> Self {
        Self {
            metric: c.metric,
            value: c.value,
            threshold: c.threshold,
            passed: c.passed,
            message: c.message,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FitnessMetricsOutput {
    pub(crate) hub_count: i64,
    pub(crate) hub_pct: f64,
    pub(crate) avg_coreness: f64,
    pub(crate) dead_code_pct: f64,
    pub(crate) hotspot_risk: f64,
    pub(crate) edge_coverage_pct: f64,
    pub(crate) high_complexity_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) high_complexity_pct_note: Option<String>,
}

impl From<calm_core::fitness::FitnessMetrics> for FitnessMetricsOutput {
    fn from(m: calm_core::fitness::FitnessMetrics) -> Self {
        Self {
            hub_count: m.hub_count,
            hub_pct: m.hub_pct,
            avg_coreness: m.avg_coreness,
            dead_code_pct: m.dead_code_pct,
            hotspot_risk: m.hotspot_risk,
            edge_coverage_pct: m.edge_coverage_pct,
            high_complexity_pct: m.high_complexity_pct,
            high_complexity_pct_note: m.high_complexity_pct_note,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BoundaryViolationOutput {
    pub(crate) from_path: String,
    pub(crate) to_path: String,
    pub(crate) rule_from: String,
    pub(crate) rule_to: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub(crate) reason: String,
}

impl From<calm_core::analysis::boundaries::BoundaryViolation> for BoundaryViolationOutput {
    fn from(v: calm_core::analysis::boundaries::BoundaryViolation) -> Self {
        Self {
            from_path: v.from_path,
            to_path: v.to_path,
            rule_from: v.rule_from,
            rule_to: v.rule_to,
            reason: v.reason,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ConfigDriftFindingOutput {
    pub(crate) doc_path: String,
    pub(crate) reference: String,
}

impl From<calm_core::analysis::config_drift::ConfigDriftFinding> for ConfigDriftFindingOutput {
    fn from(f: calm_core::analysis::config_drift::ConfigDriftFinding) -> Self {
        Self {
            doc_path: f.doc_path,
            reference: f.reference,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FitnessReportOutput {
    pub(crate) passed: bool,
    pub(crate) checks: Vec<FitnessCheckItemOutput>,
    pub(crate) metrics: FitnessMetricsOutput,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) boundary_violations: Vec<BoundaryViolationOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) config_drift: Vec<ConfigDriftFindingOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
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
    /// Plan 3 §3.3 (F10) breakdown of `hub_count` by `hub_kind` — a
    /// symbol counted in both is one that's simultaneously a degree-hub
    /// and a bridge-hub, so `hub_degree_count + hub_bridge_count` can
    /// exceed `hub_count`.
    pub(crate) hub_degree_count: i64,
    pub(crate) hub_bridge_count: i64,
    pub(crate) edges_ready: bool,
    /// Root-cause fix for the F10 calibration bug: surfaces a local
    /// `config.json`/`.calm/config.json` override that silently shadows
    /// `Config::default()` (previously zero visibility outside a server
    /// log line). `None` when no override file exists — the common case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_override: Option<ConfigOverrideSummary>,
    /// 2026-07-14 audit finding: an agent had no way to learn "cross-
    /// reference resolution for language X is much weaker than for Rust"
    /// without a separate `indexing_status` call most sessions never make
    /// — `repo_overview` is the one call every session is guaranteed to
    /// make (AGENTS.md Stage 1). Populated only for languages this repo
    /// actually has files in (`languages` above) whose SCIP overlay is
    /// either unavailable, or measured below `WEAK_MATCH_RATE_THRESHOLD` —
    /// raw data, not a verdict: a low/absent overlay leaves the
    /// conservative heuristic resolver still providing weaker-confidence
    /// edges, not zero resolution. Empty (the common case for
    /// well-supported languages, or a build without the `scip-overlay`
    /// feature) when nothing qualifies.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) weak_cross_reference_languages: Vec<super::recover::PerLanguageOverlayStatus>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ConfigOverrideSummary {
    /// Path of the config.json/.calm/config.json file being read.
    pub(crate) source: String,
    /// Dot-separated field paths (e.g. `"hub_threshold.min_callers_bridge"`)
    /// that differ from `Config::default()`. Can be empty when an override
    /// file exists but happens to match every default right now — still
    /// worth surfacing `source`, since it will silently start overriding
    /// the moment a code-level default next changes.
    pub(crate) overridden_fields: Vec<String>,
}
#[derive(Deserialize, JsonSchema)]
pub(crate) struct RepoOverviewParams {
    /// Plan 3 §3.5(a): trims a repeat-call response down to the parts that
    /// change session to session — drops `workflow_guide` (redundant once
    /// AGENTS.md has been read once) and `entry_points`, and caps
    /// `core_symbols` to 8 / `module_map` to 10. Defaults to `false` (full
    /// detail) so a session's FIRST `repo_overview` call — before AGENTS.md
    /// has necessarily been internalized — stays complete by default.
    #[serde(default)]
    pub(crate) compact: bool,
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
    /// Empty (and therefore omitted) whenever `compact:true` — see
    /// `RepoOverviewParams::compact`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) entry_points: Vec<EntryPointItem>,
    pub(crate) module_map: Vec<ModuleEntry>,
    pub(crate) health_summary: HealthSummary,
    /// Count only, no content — see `recall()` to actually read them.
    pub(crate) memory_notes_count: i64,
    /// Top symbols by `coreness` (k-core) — an architectural skeleton of
    /// this repo, empty until `health_summary.edges_ready`. See the
    /// `core_symbols` query's comment above for why it's coreness-ranked,
    /// `is_test`-excluded, and `coreness > 0`-floored.
    pub(crate) core_symbols: Vec<CoreSymbolItem>,
    /// `None` whenever `compact:true` — the client is expected to already
    /// have this (e.g. baked into `AGENTS.md`) by the time it calls
    /// `repo_overview` a 2nd+ time in the same repo. See
    /// `RepoOverviewParams::compact`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workflow_guide: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CoreSymbolItem {
    pub(crate) qualified_name: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) coreness: i64,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
}

// ---------------------------------------------------------------------------
// Tool 2: search
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct HotspotsParams {
    /// Max files to return. Defaults to `hotspots.default_top_n` in
    /// config.json (10 out of the box).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_n: Option<usize>,
    /// `git log --since` window for churn analysis, e.g. `"3 months ago"`.
    /// Defaults to `hotspots.default_since` in config.json ("6 months ago"
    /// out of the box).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) since: Option<String>,
    /// Minimum commit count for a file to qualify. Defaults to
    /// `hotspots.default_min_churn` in config.json (2 out of the box).
    /// Set to `0` to also surface high-complexity files with little or no
    /// recent churn ("stable legacy debt") that the default threshold
    /// excludes entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_churn: Option<i64>,
    /// `true` to also list the highest-risk symbols within each hotspot
    /// file, not just the file-level score. `false` (default) is cheaper.
    #[serde(default)]
    pub(crate) include_symbols: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotChurnOutput {
    pub(crate) commit_count: i64,
    /// Commits whose author looks like a bot account (dependabot[bot],
    /// renovate[bot], etc.) — see `churn_source`.
    pub(crate) bot_commit_count: i64,
    /// "unknown" (no churn data — e.g. git unavailable), "human" (no bot
    /// commits), "bot_dominated" (every commit was a bot), or "mixed".
    pub(crate) churn_source: String,
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
    /// Disambiguates two same-named symbols in this file (e.g. a
    /// `#[cfg(feature)]` real impl vs. its stub) — `symbol_info` already
    /// carries these for the same reason.
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct HotspotEntryOutput {
    pub(crate) path: String,
    pub(crate) language: String,
    pub(crate) churn: HotspotChurnOutput,
    pub(crate) complexity: HotspotComplexityOutput,
    /// Churn share (0-1) of the score, normalized against the busiest
    /// candidate file this run — 0.0 when git is unavailable.
    pub(crate) norm_churn: f64,
    /// Complexity share (0-1) of the score, normalized against the most
    /// complex candidate file this run.
    pub(crate) norm_compl: f64,
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

impl From<calm_core::analysis::hotspot::HotspotEntry> for HotspotEntryOutput {
    fn from(h: calm_core::analysis::hotspot::HotspotEntry) -> Self {
        let churn_source = h.churn.churn_source().to_string();
        HotspotEntryOutput {
            path: h.path,
            language: h.language,
            churn: HotspotChurnOutput {
                commit_count: h.churn.commit_count,
                bot_commit_count: h.churn.bot_commit_count,
                churn_source,
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
            norm_churn: h.norm_churn,
            norm_compl: h.norm_compl,
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
                        line_start: s.line_start,
                        line_end: s.line_end,
                    })
                    .collect()
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 16: understand
// ---------------------------------------------------------------------------
