use super::common::*;
use super::*;

impl CodeIntelligenceServer {
    #[tool(
        name = "callers",
        description = "USE WHEN: you need to know who calls a specific symbol — blast radius scan, refactoring impact. USE THIS for SYMBOL-LEVEL call sites. NOT for file-level imports (use dependencies). vs edit_context: callers is for exploration; edit_context is the mandatory pre-edit tool."
    )]
    pub(crate) fn callers(&self, #[tool(aggr)] p: CallersParams) -> String {
        self.timed_tool("callers", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let resolution = resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line);
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let direct: Vec<CallerEntry> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT from_symbol, from_path, edge_confidence, call_site_line
                         FROM call_edges WHERE to_symbol = ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    let path: String = row.get::<_, String>(1).unwrap_or_default();
                    let line: Option<i64> = row.get(3)?;
                    Ok(CallerEntry {
                        symbol: row.get(0)?,
                        preview: line_preview(&self.project_root, &path, line),
                        path,
                        edge_confidence: row.get(2)?,
                        line,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let (transitive, transitive_count, transitive_capped) = if p.transitive {
                let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
                let max_depth = p
                    .max_depth
                    .map(|d| (d.max(1) as usize).min(config.callers.max_depth_cap))
                    .unwrap_or(config.callers.max_depth_cap);
                let (entries, capped) = transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callers,
                    max_depth,
                    config.callers.transitive_timeout_ms,
                );
                let count = entries.len();
                (Some(entries), Some(count), Some(capped))
            } else {
                (None, None, None)
            };

            let count = direct.len();
            let has_textual = direct
                .iter()
                .any(|e| e.edge_confidence == "textual" || e.edge_confidence == "ambiguous");
            let sn = if has_textual || count > 10 {
                suggested(
                    "edit_context",
                    "High blast radius or uncertain edges — verify before modifying",
                )
            } else if count > 0 {
                suggested_with_args(
                    "source",
                    "Read top caller implementation",
                    serde_json::json!({"target": direct[0].symbol}),
                )
            } else {
                None
            };
            serde_json::to_string_pretty(&CallersOutput {
                symbol: p.symbol,
                edges_ready: self.edges_ready(),
                direct,
                direct_count: count,
                transitive,
                transitive_count,
                transitive_capped,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "callees",
        description = "USE WHEN: you need to trace what a symbol calls — understanding logic flow, internal deps. NOT for finding who calls this symbol (use callers). vs callers: callers=upward (who calls X); callees=downward (what X calls)."
    )]
    pub(crate) fn callees(&self, #[tool(aggr)] p: CalleesParams) -> String {
        self.timed_tool("callees", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let resolution = resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line);
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let direct: Vec<CalleeEntry> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT to_symbol, to_path, edge_confidence, call_site_line
                         FROM call_edges WHERE from_symbol = ?1",
                    )
                    .unwrap();
                // The call site lives in the symbol being inspected (`c.path`),
                // not in the callee's own file (`to_path`).
                let from_path = c.path.clone();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    let line: Option<i64> = row.get(3)?;
                    Ok(CalleeEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                        preview: line_preview(&self.project_root, &from_path, line),
                        line,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let (transitive, transitive_count, transitive_capped) = if p.transitive {
                let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
                let max_depth = p
                    .max_depth
                    .map(|d| (d.max(1) as usize).min(config.callees.max_depth_cap))
                    .unwrap_or(config.callees.max_depth_cap);
                let (entries, capped) = transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callees,
                    max_depth,
                    config.callees.transitive_timeout_ms,
                );
                let count = entries.len();
                (Some(entries), Some(count), Some(capped))
            } else {
                (None, None, None)
            };

            let count = direct.len();
            let sn = if count > 0 {
                suggested("path", "Trace specific call chain")
            } else {
                None
            };
            serde_json::to_string_pretty(&CalleesOutput {
                symbol: p.symbol,
                edges_ready: self.edges_ready(),
                direct,
                direct_count: count,
                transitive,
                transitive_count,
                transitive_capped,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "dependencies",
        description = "USE WHEN: you need to understand file-level architectural connections. USE THIS for FILE-LEVEL import graph. NOT for symbol-level call sites (use callers/callees). vs callers/callees: dependencies is file-level; callers/callees is symbol-level."
    )]
    pub(crate) fn dependencies(&self, #[tool(aggr)] p: DependenciesParams) -> String {
        self.timed_tool("dependencies", || {
            self.track_file(&p.path);
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let dep_config = ci_core::config::load_config(&self.project_root)
                .map(|c| c.dependencies)
                .unwrap_or_default();

            let mut stmt_imports = conn
                .prepare(
                    "SELECT from_path, COALESCE(to_path, ''), module_name, symbols_used
                     FROM import_edges WHERE from_path = ?1 LIMIT ?2",
                )
                .unwrap();

            let imports: Vec<ImportEntry> = stmt_imports
                .query_map(
                    rusqlite::params![p.path, dep_config.max_imports as i64 + 1],
                    |row| {
                        Ok(ImportEntry {
                            from_path: row.get(0)?,
                            to_path: row.get(1)?,
                            module_name: row.get(2)?,
                            symbols_used: parse_symbols_used(&row.get::<_, String>(3)?),
                        })
                    },
                )
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            let imports_truncated = imports.len() > dep_config.max_imports;
            let imports = imports.into_iter().take(dep_config.max_imports).collect();

            // Drop the first statement before preparing the second on the same conn
            drop(stmt_imports);
            let mut stmt_imported_by = conn
                .prepare(
                    "SELECT from_path, COALESCE(to_path, ''), module_name, symbols_used
                     FROM import_edges WHERE to_path = ?1 LIMIT ?2",
                )
                .unwrap();

            let imported_by: Vec<ImportEntry> = stmt_imported_by
                .query_map(
                    rusqlite::params![p.path, dep_config.max_imported_by as i64 + 1],
                    |row| {
                        Ok(ImportEntry {
                            from_path: row.get(0)?,
                            to_path: row.get(1)?,
                            module_name: row.get(2)?,
                            symbols_used: parse_symbols_used(&row.get::<_, String>(3)?),
                        })
                    },
                )
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            let imported_by_truncated = imported_by.len() > dep_config.max_imported_by;
            let imported_by = imported_by
                .into_iter()
                .take(dep_config.max_imported_by)
                .collect::<Vec<_>>();

            let sn = if imported_by.len() > 20 {
                suggested("callers", "High fan-in — check symbol blast radius")
            } else {
                None
            };
            serde_json::to_string_pretty(&DependenciesOutput {
                path: p.path,
                imports,
                imports_truncated,
                imported_by,
                imported_by_truncated,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "path",
        description = "USE WHEN: you need to trace if and how symbol A can reach symbol B through call chain. Bidirectional BFS — cycles terminate cleanly. path is DIRECTED: A→B ≠ B→A. terminated_by=null + exists=true/false → certain result."
    )]
    pub(crate) fn path(&self, #[tool(aggr)] p: PathParams) -> String {
        self.timed_tool("path", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let from = { resolve_symbol(&conn, &p.from_symbol, p.from_path.as_deref(), p.from_line) };
            let from = match from {
                SymbolResolution::NotFound => return not_found_json(&p.from_symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&from.qualified_name);
            self.track_file(&from.path);

            let to = { resolve_symbol(&conn, &p.to_symbol, p.to_path.as_deref(), p.to_line) };
            let to = match to {
                SymbolResolution::NotFound => return not_found_json(&p.to_symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&to.qualified_name);
            self.track_file(&to.path);

            let path_config = ci_core::config::load_config(&self.project_root)
                .unwrap_or_default()
                .path;

            let requested_hops = p.max_hops.unwrap_or(path_config.default_max_hops as i64);
            let hops_clamped = !(0..=path_config.max_allowed_hops as i64).contains(&requested_hops);
            let max_hops = requested_hops.clamp(0, path_config.max_allowed_hops as i64) as usize;

            let result = {
                ci_core::graph::path::bidirectional_bfs_path(
                    &conn,
                    &from.qualified_name,
                    &to.qualified_name,
                    max_hops,
                    5,
                    path_config.timeout_ms,
                )
            };

            let (routes, exists, terminated_by) = match result {
                Ok(r) => (
                    r.routes
                        .into_iter()
                        .map(|path| path.into_iter().map(|step| step.symbol).collect())
                        .collect::<Vec<Vec<String>>>(),
                    r.exists,
                    r.terminated_by.map(TerminatedByOutput::from),
                ),
                Err(_) => (vec![], None, None),
            };

            let count = routes.len();
            let sn = if matches!(&terminated_by, Some(TerminatedByOutput::Timeout)) {
                suggested_with_args("path", "Retry with smaller max_hops", serde_json::json!({"max_hops": 4}))
            } else if matches!(&terminated_by, Some(TerminatedByOutput::MaxHops)) {
                let new_hops = requested_hops + 4;
                suggested_with_args("path", "Path may exceed hop limit — retry with larger max_hops, or check the reverse direction",
                    serde_json::json!({"max_hops": new_hops, "from_symbol": p.to_symbol, "to_symbol": p.from_symbol}))
            } else if exists == Some(true) {
                suggested("source", "Read meeting node implementation")
            } else {
                None
            };
            serde_json::to_string_pretty(&PathOutput {
                from_symbol: p.from_symbol,
                to_symbol: p.to_symbol,
                routes,
                route_count: count,
                exists,
                terminated_by,
                hops_clamped,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct CallersParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file — any line within
    /// the intended candidate's range (see an earlier `ambiguous` response's
    /// `line_start`/`line_end`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// `true` to also do a multi-hop BFS (`transitive`/`transitive_count`
    /// in the output) beyond direct callers. `false` (default) returns
    /// only direct callers — cheaper.
    #[serde(default)]
    pub(crate) transitive: bool,
    /// Max BFS depth when `transitive` is set. Clamped to
    /// `callers.max_depth_cap` in config.json (4 out of the box); ignored
    /// if `transitive` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_depth: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CallersOutput {
    pub(crate) symbol: String,
    pub(crate) edges_ready: bool,
    pub(crate) direct: Vec<CallerEntry>,
    pub(crate) direct_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive: Option<Vec<TransitiveEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_capped: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 7: callees
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct CalleesParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file — any line within
    /// the intended candidate's range (see an earlier `ambiguous` response's
    /// `line_start`/`line_end`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// `true` to also do a multi-hop BFS (`transitive`/`transitive_count`
    /// in the output) beyond direct callees. `false` (default) returns
    /// only direct callees — cheaper.
    #[serde(default)]
    pub(crate) transitive: bool,
    /// Max BFS depth when `transitive` is set. Clamped to
    /// `callees.max_depth_cap` in config.json (4 out of the box); ignored
    /// if `transitive` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_depth: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CalleesOutput {
    pub(crate) symbol: String,
    pub(crate) edges_ready: bool,
    pub(crate) direct: Vec<CalleeEntry>,
    pub(crate) direct_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive: Option<Vec<TransitiveEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_capped: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct DependenciesParams {
    /// Repo-relative path of the file whose imports/importers to list, e.g.
    /// `crates/ci-core/src/embedding.rs`.
    pub(crate) path: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ImportEntry {
    pub(crate) from_path: String,
    pub(crate) to_path: String,
    pub(crate) module_name: String,
    pub(crate) symbols_used: Vec<String>,
}

pub(crate) fn parse_symbols_used(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DependenciesOutput {
    pub(crate) path: String,
    pub(crate) imports: Vec<ImportEntry>,
    pub(crate) imports_truncated: bool,
    pub(crate) imported_by: Vec<ImportEntry>,
    pub(crate) imported_by_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 9: path
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct PathParams {
    /// Bare name of the starting symbol (not a `path::name` qualified name).
    pub(crate) from_symbol: String,
    /// Bare name of the target symbol to reach.
    pub(crate) to_symbol: String,
    /// Narrows `from_symbol` to one file when the bare name is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from_path: Option<String>,
    /// Same as `from_path`, for `to_symbol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) to_path: Option<String>,
    /// Disambiguates a same-named `from_symbol` in the same file — any line
    /// within the intended candidate's range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from_line: Option<i64>,
    /// Same as `from_line`, for `to_symbol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) to_line: Option<i64>,
    /// Max BFS depth to search before giving up. Defaults to
    /// `path.default_max_hops` in config.json (8 out of the box), clamped
    /// to `path.max_allowed_hops` (20 out of the box).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_hops: Option<i64>,
}

/// Local mirror of `ci_core::types::TerminatedBy` — that type lives in
/// `ci-core`, which doesn't depend on `schemars`, so it can't derive
/// `JsonSchema` itself.

#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminatedByOutput {
    Timeout,
    MaxHops,
    PathCount,
}

impl From<ci_core::types::TerminatedBy> for TerminatedByOutput {
    fn from(t: ci_core::types::TerminatedBy) -> Self {
        match t {
            ci_core::types::TerminatedBy::Timeout => TerminatedByOutput::Timeout,
            ci_core::types::TerminatedBy::MaxHops => TerminatedByOutput::MaxHops,
            ci_core::types::TerminatedBy::PathCount => TerminatedByOutput::PathCount,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PathOutput {
    pub(crate) from_symbol: String,
    pub(crate) to_symbol: String,
    pub(crate) routes: Vec<Vec<String>>,
    pub(crate) route_count: usize,
    pub(crate) exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) terminated_by: Option<TerminatedByOutput>,
    pub(crate) hops_clamped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
