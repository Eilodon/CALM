use super::common::*;
use super::*;

impl CodeIntelligenceServer {
    #[tool(
        name = "search",
        description = "USE THIS INSTEAD OF native grep, text search, or file browsing tools. USE WHEN: you don't have an exact file path and line number. kind=hybrid has highest recall. NOT FOR: inspecting a file you already have (use file_overview). vs locate: search returns a result list; locate returns search + symbol metadata in one call."
    )]
    pub(crate) fn search(&self, #[tool(aggr)] p: SearchParams) -> String {
        self.timed_tool("search", || {
            let kind = match p.kind.as_str() {
                "symbol" => ci_core::types::SearchKind::Symbol,
                "text" => ci_core::types::SearchKind::Text,
                "file" => ci_core::types::SearchKind::File,
                "semantic" => ci_core::types::SearchKind::Semantic,
                "hybrid" => ci_core::types::SearchKind::Hybrid,
                _ => ci_core::types::SearchKind::Symbol,
            };

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let rrf_k = ci_core::config::load_config(&self.project_root)
                .map(|c| c.search.rrf_k as f64)
                .unwrap_or(ci_core::search::DEFAULT_RRF_K);
            let kind_str = p.kind.as_str();
            match ci_core::search::search(
                &conn,
                &p.query,
                kind,
                p.limit,
                self.embedder().as_deref(),
                rrf_k,
            ) {
                Ok(output) => {
                    let results: Vec<SearchResultItem> = output
                        .results
                        .into_iter()
                        .map(|r| SearchResultItem {
                            name: r.name,
                            path: r.path,
                            kind: r.kind,
                            line_start: r.line_start,
                            line_end: r.line_end,
                            score: Some(r.score),
                            match_type: Some(r.match_type),
                        })
                        .collect();
                    let sn = if !results.is_empty() && kind_str == "symbol" {
                        suggested_with_args("locate", "Full context in 1 call (replaces symbol_info)", serde_json::json!({"query": results[0].name, "kind": "symbol"}))
                    } else if results.is_empty() && kind_str != "hybrid" && kind_str != "semantic" {
                        suggested_with_args("search", "Try hybrid for broader recall", serde_json::json!({"kind": "hybrid"}))
                    } else if results.is_empty() && kind_str == "semantic" {
                        suggested_with_args("search", "Semantic index may not cover this — try text or hybrid search", serde_json::json!({"kind": "text"}))
                    } else if results.is_empty() && kind_str == "hybrid" {
                        suggested_with_args("search", "Embeddings may not cover this query — try exact text search or broaden wording", serde_json::json!({"kind": "text"}))
                    } else {
                        None
                    };
                    serde_json::to_string_pretty(&SearchOutput {
                        results,
                        truncated: output.truncated,
                        degraded: output.degraded,
                        note: output.note,
                        suggested_next: self.filter_sn(sn),
                    })
                    .unwrap_or_default()
                }
                Err(e) => serde_json::to_string_pretty(&SearchOutput {
                    results: vec![],
                    truncated: false,
                    degraded: true,
                    note: Some(format!("Search error: {e}")),
                    suggested_next: None,
                })
                .unwrap_or_default(),
            }
        })
    }

    #[tool(
        name = "locate",
        description = "Compound: search + file_overview + symbol_info in 1 call (66% reduction). USE INSTEAD OF calling search then file_overview then symbol_info separately. NOT FOR: reading source (use source after locate), pre-edit (use edit_context)."
    )]
    pub(crate) fn locate(&self, #[tool(aggr)] p: LocateParams) -> String {
        self.timed_tool("locate", || {
            let kind_str = p.kind.as_deref().unwrap_or("symbol");
            let kind = match kind_str {
                "text" => ci_core::types::SearchKind::Text,
                "file" => ci_core::types::SearchKind::File,
                "semantic" => ci_core::types::SearchKind::Semantic,
                "hybrid" => ci_core::types::SearchKind::Hybrid,
                _ => ci_core::types::SearchKind::Symbol,
            };
            let limit = p.limit.unwrap_or(10);

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let rrf_k = ci_core::config::load_config(&self.project_root)
                .map(|c| c.search.rrf_k as f64)
                .unwrap_or(ci_core::search::DEFAULT_RRF_K);
            let search_output = match ci_core::search::search(
                &conn,
                &p.query,
                kind,
                limit,
                self.embedder().as_deref(),
                rrf_k,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return serde_json::to_string_pretty(&ErrorOutput {
                        error: ErrorDetail {
                            code: "DB_LOCKED".into(),
                            message: format!("Search failed: {e}"),
                            recoverable: true,
                        },
                    })
                    .unwrap_or_default();
                }
            };

            let results: Vec<SearchResultItem> = search_output
                .results
                .iter()
                .map(|r| SearchResultItem {
                    name: r.name.clone(),
                    path: r.path.clone(),
                    kind: r.kind.clone(),
                    line_start: r.line_start,
                    line_end: r.line_end,
                    score: Some(r.score),
                    match_type: Some(r.match_type.clone()),
                })
                .collect();

            let top = search_output.results.first();

            // INVARIANT (CONTRACTS.md): kind ∈ {text, file} + depth = with_symbol
            // → auto-downgrade to with_file (a text/file match has no symbol to
            // enrich), and report the adjustment in `depth_adjusted`.
            let requested_depth = p.depth.as_deref().unwrap_or("with_symbol");
            let mut effective_depth = match requested_depth {
                "search_only" => "search_only",
                "with_file" => "with_file",
                _ => "with_symbol",
            };
            let mut depth_adjusted: Option<String> = None;
            if matches!(kind_str, "text" | "file") && effective_depth == "with_symbol" {
                effective_depth = "with_file";
                depth_adjusted = Some("with_file".to_string());
            }

            let top_symbol = if effective_depth == "with_symbol" {
                top.and_then(|t| {
                    conn
                        .query_row(
                            "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub
                             FROM symbols WHERE qualified_name = ?1 LIMIT 1",
                            rusqlite::params![t.qualified_name],
                            |row| {
                                Ok(SymbolInfoOutput {
                                    name: row.get(0)?,
                                    qualified_name: row.get(1)?,
                                    kind: row.get(2)?,
                                    path: row.get(3)?,
                                    line_start: row.get(4)?,
                                    line_end: row.get(5)?,
                                    signature: row.get::<_, String>(6).ok().filter(|s| !s.is_empty()),
                                    docstring: row.get::<_, String>(7).ok().filter(|s| !s.is_empty()),
                                    caller_count: row.get(8)?,
                                    is_hub: row.get::<_, i64>(9)? != 0,
                                    coreness: None,
                                    health: None,
                                    suggested_next: None,
                                })
                            },
                        )
                        .ok()
                })
            } else {
                None
            };

            let file_overview = if effective_depth == "search_only" {
                None
            } else {
                top.map(|t| {
                    build_file_overview(&conn, &t.path)
                })
            };

            if effective_depth != "search_only"
                && let Some(t) = top
            {
                self.track_file(&t.path);
                if t.match_type != "file" {
                    self.track_symbol(&t.qualified_name);
                }
            }

            let truncated = search_output.truncated;

            let sn = if let Some(sym) = top_symbol.as_ref() {
                if sym.is_hub {
                    suggested_with_args("edit_context", "Hub detected — mandatory pre-edit check", serde_json::json!({"symbol": sym.name, "path": sym.path}))
                } else if sym.caller_count == 0 {
                    suggested_with_args("callers", "No callers found — verify dead code before deleting", serde_json::json!({"symbol": sym.name}))
                } else {
                    suggested_with_args("source", "Read implementation", serde_json::json!({"target": results[0].name}))
                }
            } else if results.is_empty() {
                suggested_with_args("search", "No match — broaden with hybrid search", serde_json::json!({"kind": "hybrid"}))
            } else if results.len() > 1 && results[0].name == results[1].name {
                suggested_with_args("symbol_info", "Multiple matches for same name — disambiguate", serde_json::json!({"symbol": results[0].name, "path": results[0].path}))
            } else {
                suggested_with_args("source", "Read implementation", serde_json::json!({"target": results[0].name}))
            };

            serde_json::to_string_pretty(&LocateOutput {
                results,
                top_symbol,
                file_overview,
                truncated,
                depth_adjusted,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "file_overview",
        description = "USE WHEN: you have a file path and want to see its symbols, structure, and inferred role. vs source: file_overview shows ALL symbols in a file; source reads ONE symbol's body. vs dependencies: file_overview shows what's INSIDE the file; dependencies shows what the file IMPORTS/IS IMPORTED BY."
    )]
    pub(crate) fn file_overview(&self, #[tool(aggr)] p: FileOverviewParams) -> String {
        self.timed_tool("file_overview", || {
            self.track_file(&p.path);
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let mut out = build_file_overview(&conn, &p.path);

            let hub_name: Option<String> = conn
                .prepare("SELECT name FROM symbols WHERE path = ?1 AND is_hub = 1 LIMIT 1")
                .ok()
                .and_then(|mut s| s.query_row(rusqlite::params![p.path], |r| r.get(0)).ok());
            out.suggested_next = if let Some(hub) = hub_name {
                suggested_with_args(
                    "locate",
                    "Inspect hub symbol",
                    serde_json::json!({"query": hub}),
                )
            } else {
                suggested("source", "Read a symbol implementation")
            };
            serde_json::to_string_pretty(&out).unwrap_or_default()
        })
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ErrorOutput {
    pub(crate) error: ErrorDetail,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ErrorDetail {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) recoverable: bool,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct SearchParams {
    pub(crate) query: String,
    #[serde(default = "default_symbol")]
    pub(crate) kind: String,
    #[serde(default = "default_limit")]
    pub(crate) limit: usize,
}

pub(crate) fn default_symbol() -> String {
    "symbol".into()
}

pub(crate) fn default_limit() -> usize {
    10
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SearchResultItem {
    pub(crate) name: String,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) match_type: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SearchOutput {
    pub(crate) results: Vec<SearchResultItem>,
    pub(crate) truncated: bool,
    pub(crate) degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 3: file_overview
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct FileOverviewParams {
    pub(crate) path: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FileOverviewSymbol {
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) kind: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    pub(crate) signature: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FileOverviewOutput {
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    pub(crate) symbols: Vec<FileOverviewSymbol>,
    pub(crate) symbol_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

/// Shared by the `file_overview` tool and `locate` (when the top result is a
/// file match), so both surfaces build the same shape from the same query.
pub(crate) fn build_file_overview(conn: &rusqlite::Connection, path: &str) -> FileOverviewOutput {
    let symbols: Vec<FileOverviewSymbol> = {
        let mut stmt = conn
            .prepare(
                "SELECT name, qualified_name, kind, line_start, line_end, \
                 COALESCE(caller_count, 0), is_hub, signature
                 FROM symbols WHERE path = ?1 ORDER BY line_start",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![path], |row| {
            Ok(FileOverviewSymbol {
                name: row.get(0)?,
                qualified_name: row.get(1)?,
                kind: row.get(2)?,
                line_start: row.get(3)?,
                line_end: row.get(4)?,
                caller_count: row.get(5)?,
                is_hub: row.get::<_, i64>(6)? != 0,
                signature: row.get(7)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    };

    let language: Option<String> = conn
        .query_row(
            "SELECT language FROM file_index WHERE path = ?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .ok();

    let symbol_count = symbols.len();
    FileOverviewOutput {
        path: path.to_string(),
        language,
        symbols,
        symbol_count,
        suggested_next: None,
    }
}

// ---------------------------------------------------------------------------
// Tool 4: symbol_info
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct LocateParams {
    pub(crate) query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) depth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<usize>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LocateOutput {
    pub(crate) results: Vec<SearchResultItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_symbol: Option<SymbolInfoOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_overview: Option<FileOverviewOutput>,
    pub(crate) truncated: bool,
    /// Set when the requested `depth` was auto-downgraded — see
    /// `CONTRACTS.md`'s `LocateDepth` invariant: `kind ∈ {text, file}` +
    /// `depth = with_symbol` has no meaningful symbol to enrich, so it's
    /// downgraded to `with_file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) depth_adjusted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 15: hotspots
// ---------------------------------------------------------------------------
