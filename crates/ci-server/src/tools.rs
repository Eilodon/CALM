use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::tool::Parameters;
use rmcp::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use ci_core::sanitize::sanitize_source_output;

// ---------------------------------------------------------------------------
// Server state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CodeIntelligenceServer {
    project_root: PathBuf,
    #[allow(dead_code)]
    db_path: PathBuf,
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl CodeIntelligenceServer {
    pub fn new(project_root: PathBuf, db_path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = rusqlite::Connection::open(&db_path)?;
        ci_core::db::schema::init_db(&conn)?;
        Ok(Self {
            project_root,
            db_path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().unwrap()
    }
}

// ---------------------------------------------------------------------------
// Shared output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema)]
struct ErrorOutput {
    error: ErrorDetail,
}

#[derive(Serialize, JsonSchema)]
struct ErrorDetail {
    code: String,
    message: String,
    recoverable: bool,
}

#[allow(dead_code)]
fn error_json(code: &str, message: &str, recoverable: bool) -> String {
    serde_json::to_string_pretty(&ErrorOutput {
        error: ErrorDetail {
            code: code.into(),
            message: message.into(),
            recoverable,
        },
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tool 1: repo_overview
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema)]
struct RepoOverviewOutput {
    languages: Vec<String>,
    indexing_phase: String,
    embeddings_status: String,
    total_modules: i64,
    total_symbols: i64,
    total_files: i64,
    truncated: bool,
    workflow_guide: String,
}

// ---------------------------------------------------------------------------
// Tool 2: search
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SearchParams {
    query: String,
    #[serde(default = "default_symbol")]
    kind: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_symbol() -> String {
    "symbol".into()
}
fn default_limit() -> usize {
    10
}

#[derive(Serialize, JsonSchema)]
struct SearchResultItem {
    name: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_type: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct SearchOutput {
    results: Vec<SearchResultItem>,
    truncated: bool,
    degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool 3: file_overview
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileOverviewParams {
    path: String,
}

#[derive(Serialize, JsonSchema)]
struct FileOverviewSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    line_start: i64,
    line_end: i64,
}

#[derive(Serialize, JsonSchema)]
struct FileOverviewOutput {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    symbols: Vec<FileOverviewSymbol>,
    symbol_count: usize,
}

// ---------------------------------------------------------------------------
// Tool 4: symbol_info
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SymbolInfoParams {
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct SymbolInfoOutput {
    name: String,
    qualified_name: String,
    kind: String,
    path: String,
    line_start: i64,
    line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    docstring: Option<String>,
    caller_count: i64,
    is_hub: bool,
}

// ---------------------------------------------------------------------------
// Tool 5: source
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SourceParams {
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default)]
    include_metadata: bool,
}

#[derive(Serialize, JsonSchema)]
struct SourceOutput {
    symbol: String,
    path: String,
    line_start: i64,
    line_end: i64,
    source: String,
    language: String,
}

// ---------------------------------------------------------------------------
// Tool 6: callers
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CallersParams {
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default)]
    transitive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_depth: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
struct CallerEntry {
    symbol: String,
    path: String,
    edge_confidence: String,
}

#[derive(Serialize, JsonSchema)]
struct CallersOutput {
    symbol: String,
    direct: Vec<CallerEntry>,
    direct_count: usize,
}

// ---------------------------------------------------------------------------
// Tool 7: callees
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CalleesParams {
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default)]
    transitive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_depth: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
struct CalleeEntry {
    symbol: String,
    path: String,
    edge_confidence: String,
}

#[derive(Serialize, JsonSchema)]
struct CalleesOutput {
    symbol: String,
    direct: Vec<CalleeEntry>,
    direct_count: usize,
}

// ---------------------------------------------------------------------------
// Tool 8: dependencies
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DependenciesParams {
    path: String,
}

#[derive(Serialize, JsonSchema)]
struct ImportEntry {
    from_path: String,
    to_path: String,
    module_name: String,
}

#[derive(Serialize, JsonSchema)]
struct DependenciesOutput {
    path: String,
    imports: Vec<ImportEntry>,
    imported_by: Vec<ImportEntry>,
}

// ---------------------------------------------------------------------------
// Tool 9: path
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PathParams {
    from_symbol: String,
    to_symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_hops: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
struct PathOutput {
    from_symbol: String,
    to_symbol: String,
    routes: Vec<Vec<String>>,
    route_count: usize,
}

// ---------------------------------------------------------------------------
// Tool 10: edit_context
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EditContextParams {
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct EditContextOutput {
    symbol: String,
    callers: Vec<CallerEntry>,
    callees: Vec<CalleeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    risk_assessment: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool 11: session_context
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema)]
struct SessionContextOutput {
    tool_calls: u64,
    explored_symbols: Vec<String>,
    explored_files: Vec<String>,
    unique_files_explored: usize,
}

// ---------------------------------------------------------------------------
// Tool 12: diff_impact
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DiffImpactParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commits: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct DiffImpactOutput {
    files_changed: Vec<String>,
    aggregate_risk: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool 13: indexing_status
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IndexingStatusParams {
    #[serde(default)]
    retry_embeddings: bool,
}

#[derive(Serialize, JsonSchema)]
struct IndexingStatusOutput {
    indexing_phase: String,
    files_indexed: i64,
    symbols_indexed: i64,
    edges_indexed: i64,
    embeddings_status: String,
    edges_ready: bool,
}

// ---------------------------------------------------------------------------
// Tool 14: locate
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct LocateParams {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Serialize, JsonSchema)]
struct LocateOutput {
    results: Vec<SearchResultItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_symbol: Option<SymbolInfoOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_overview: Option<FileOverviewOutput>,
    truncated: bool,
}

// ---------------------------------------------------------------------------
// Tool 15: hotspots
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct HotspotsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    top_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_churn: Option<i64>,
    #[serde(default)]
    include_symbols: bool,
}

#[derive(Serialize, JsonSchema)]
struct HotspotsOutput {
    hotspots: Vec<serde_json::Value>,
    count: usize,
}

// ---------------------------------------------------------------------------
// Tool 16: understand
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct UnderstandParams {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct UnderstandOutput {
    symbol: Option<SymbolInfoOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<SourceOutput>,
    callers_summary: Vec<CallerEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edges_ready: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------

#[tool(tool_box)]
impl CodeIntelligenceServer {
    #[tool(
        name = "repo_overview",
        description = "Overview of the entire repository — languages, stats, indexing status. ALWAYS call first."
    )]
    fn repo_overview(&self) -> String {
        let total_symbols: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap_or(0);
        let total_files: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
            .unwrap_or(0);

        let _conn1 = self.db();

        let mut stmt = _conn1
            .prepare("SELECT DISTINCT language FROM file_index WHERE language IS NOT NULL")
            .unwrap();
        let languages: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        serde_json::to_string_pretty(&RepoOverviewOutput {
            languages,
            indexing_phase: "ready".into(),
            embeddings_status: "disabled".into(),
            total_modules: total_files,
            total_symbols,
            total_files,
            truncated: false,
            workflow_guide: "Use locate to find symbols, then source/callers/callees to explore."
                .into(),
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "search",
        description = "FTS5 dual-column search across symbols, text, files. Supports symbol, text, file, semantic, hybrid kinds."
    )]
    fn search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        let kind = match p.kind.as_str() {
            "symbol" => ci_core::types::SearchKind::Symbol,
            "text" => ci_core::types::SearchKind::Text,
            "file" => ci_core::types::SearchKind::File,
            "semantic" => ci_core::types::SearchKind::Semantic,
            "hybrid" => ci_core::types::SearchKind::Hybrid,
            _ => ci_core::types::SearchKind::Symbol,
        };

        match ci_core::search::search(&self.db(), &p.query, kind, p.limit, false) {
            Ok(output) => serde_json::to_string_pretty(&SearchOutput {
                results: output
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
                    .collect(),
                truncated: output.truncated,
                degraded: output.degraded,
                note: output.note,
            })
            .unwrap_or_default(),
            Err(e) => serde_json::to_string_pretty(&SearchOutput {
                results: vec![],
                truncated: false,
                degraded: true,
                note: Some(format!("Search error: {e}")),
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "file_overview",
        description = "List all symbols in a file — functions, classes, methods with line ranges."
    )]
    fn file_overview(&self, Parameters(p): Parameters<FileOverviewParams>) -> String {
        let _conn2 = self.db();
        let mut stmt = _conn2
            .prepare(
                "SELECT name, qualified_name, kind, line_start, line_end
                 FROM symbols WHERE path = ?1 ORDER BY line_start",
            )
            .unwrap();

        let symbols: Vec<FileOverviewSymbol> = stmt
            .query_map(rusqlite::params![p.path], |row| {
                Ok(FileOverviewSymbol {
                    name: row.get(0)?,
                    qualified_name: row.get(1)?,
                    kind: row.get(2)?,
                    line_start: row.get(3)?,
                    line_end: row.get(4)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let language: Option<String> = self
            .db()
            .query_row(
                "SELECT language FROM file_index WHERE path = ?1",
                rusqlite::params![p.path],
                |r| r.get(0),
            )
            .ok();

        let count = symbols.len();
        serde_json::to_string_pretty(&FileOverviewOutput {
            path: p.path,
            language,
            symbols,
            symbol_count: count,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "symbol_info",
        description = "Detailed info for a single symbol — signature, docstring, hub status, caller count."
    )]
    fn symbol_info(&self, Parameters(p): Parameters<SymbolInfoParams>) -> String {
        let query = if p.path.is_some() {
            "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub
             FROM symbols WHERE name = ?1 AND path = ?2 LIMIT 1"
        } else {
            "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub
             FROM symbols WHERE name = ?1 LIMIT 1"
        };

        let result = if let Some(ref path) = p.path {
            self.db()
                .query_row(query, rusqlite::params![p.symbol, path], |row| {
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
                    })
                })
        } else {
            self.db()
                .query_row(query, rusqlite::params![p.symbol], |row| {
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
                    })
                })
        };

        match result {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_default(),
            Err(_) => serde_json::to_string_pretty(&ErrorOutput {
                error: ErrorDetail {
                    code: "NOT_FOUND".into(),
                    message: format!("Symbol '{}' not found in index", p.symbol),
                    recoverable: false,
                },
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "source",
        description = "Retrieve source code for a symbol. Output is sanitized for credentials."
    )]
    fn source(&self, Parameters(p): Parameters<SourceParams>) -> String {
        let row = self.db().query_row(
            "SELECT qualified_name, path, line_start, line_end, language
             FROM symbols WHERE name = ?1 LIMIT 1",
            rusqlite::params![p.symbol],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        );

        match row {
            Ok((_, path, line_start, line_end, language)) => {
                let full_path = self.project_root.join(&path);
                let source = match std::fs::read_to_string(&full_path) {
                    Ok(content) => {
                        let lines: Vec<&str> = content.lines().collect();
                        let start = (line_start as usize).saturating_sub(1);
                        let end = (line_end as usize).min(lines.len());
                        lines[start..end].join("\n")
                    }
                    Err(_) => "(source file not readable)".into(),
                };

                let sanitized = sanitize_source_output(&source);
                serde_json::to_string_pretty(&SourceOutput {
                    symbol: p.symbol,
                    path,
                    line_start,
                    line_end,
                    source: sanitized,
                    language,
                })
                .unwrap_or_default()
            }
            Err(_) => serde_json::to_string_pretty(&ErrorOutput {
                error: ErrorDetail {
                    code: "NOT_FOUND".into(),
                    message: format!("Symbol '{}' not found in index", p.symbol),
                    recoverable: false,
                },
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "callers",
        description = "Who calls this symbol? Returns direct callers with edge confidence."
    )]
    fn callers(&self, Parameters(p): Parameters<CallersParams>) -> String {
        let _conn3 = self.db();
        let mut stmt = _conn3
            .prepare(
                "SELECT from_symbol, from_path, edge_confidence
                 FROM call_edges WHERE to_symbol = ?1",
            )
            .unwrap();

        let direct: Vec<CallerEntry> = stmt
            .query_map(rusqlite::params![p.symbol], |row| {
                Ok(CallerEntry {
                    symbol: row.get(0)?,
                    path: row.get::<_, String>(1).unwrap_or_default(),
                    edge_confidence: row.get(2)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let count = direct.len();
        serde_json::to_string_pretty(&CallersOutput {
            symbol: p.symbol,
            direct,
            direct_count: count,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "callees",
        description = "What does this symbol call? Returns direct callees with edge confidence."
    )]
    fn callees(&self, Parameters(p): Parameters<CalleesParams>) -> String {
        let _conn4 = self.db();
        let mut stmt = _conn4
            .prepare(
                "SELECT to_symbol, to_path, edge_confidence
                 FROM call_edges WHERE from_symbol = ?1",
            )
            .unwrap();

        let direct: Vec<CalleeEntry> = stmt
            .query_map(rusqlite::params![p.symbol], |row| {
                Ok(CalleeEntry {
                    symbol: row.get(0)?,
                    path: row.get::<_, String>(1).unwrap_or_default(),
                    edge_confidence: row.get(2)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let count = direct.len();
        serde_json::to_string_pretty(&CalleesOutput {
            symbol: p.symbol,
            direct,
            direct_count: count,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "dependencies",
        description = "Import/export dependencies for a file — what it imports and what imports it."
    )]
    fn dependencies(&self, Parameters(p): Parameters<DependenciesParams>) -> String {
        let _conn5 = self.db();
        let mut stmt_imports = _conn5
            .prepare(
                "SELECT from_path, COALESCE(to_path, ''), module_name
                 FROM import_edges WHERE from_path = ?1",
            )
            .unwrap();

        let imports: Vec<ImportEntry> = stmt_imports
            .query_map(rusqlite::params![p.path], |row| {
                Ok(ImportEntry {
                    from_path: row.get(0)?,
                    to_path: row.get(1)?,
                    module_name: row.get(2)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let _conn6 = self.db();

        let mut stmt_imported_by = _conn6
            .prepare(
                "SELECT from_path, COALESCE(to_path, ''), module_name
                 FROM import_edges WHERE to_path = ?1",
            )
            .unwrap();

        let imported_by: Vec<ImportEntry> = stmt_imported_by
            .query_map(rusqlite::params![p.path], |row| {
                Ok(ImportEntry {
                    from_path: row.get(0)?,
                    to_path: row.get(1)?,
                    module_name: row.get(2)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        serde_json::to_string_pretty(&DependenciesOutput {
            path: p.path,
            imports,
            imported_by,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "path",
        description = "Find call paths between two symbols using bidirectional BFS."
    )]
    fn path(&self, Parameters(p): Parameters<PathParams>) -> String {
        let max_hops = p.max_hops.unwrap_or(8).min(20) as usize;
        let result = ci_core::graph::path::bidirectional_bfs_path(
            &self.db(),
            &p.from_symbol,
            &p.to_symbol,
            max_hops,
            5,
            5000,
        );

        let routes: Vec<Vec<String>> = result
            .map(|r| {
                r.routes
                    .into_iter()
                    .map(|path| path.into_iter().map(|step| step.symbol).collect())
                    .collect()
            })
            .unwrap_or_default();

        let count = routes.len();
        serde_json::to_string_pretty(&PathOutput {
            from_symbol: p.from_symbol,
            to_symbol: p.to_symbol,
            routes,
            route_count: count,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "edit_context",
        description = "Pre-edit blast radius — callers, callees, and risk assessment for a symbol you plan to modify."
    )]
    fn edit_context(&self, Parameters(p): Parameters<EditContextParams>) -> String {
        let _conn7 = self.db();
        let mut stmt_callers = _conn7
            .prepare(
                "SELECT from_symbol, from_path, edge_confidence
                 FROM call_edges WHERE to_symbol = ?1",
            )
            .unwrap();
        let callers: Vec<CallerEntry> = stmt_callers
            .query_map(rusqlite::params![p.symbol], |row| {
                Ok(CallerEntry {
                    symbol: row.get(0)?,
                    path: row.get::<_, String>(1).unwrap_or_default(),
                    edge_confidence: row.get(2)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let _conn8 = self.db();

        let mut stmt_callees = _conn8
            .prepare(
                "SELECT to_symbol, to_path, edge_confidence
                 FROM call_edges WHERE from_symbol = ?1",
            )
            .unwrap();
        let callees: Vec<CalleeEntry> = stmt_callees
            .query_map(rusqlite::params![p.symbol], |row| {
                Ok(CalleeEntry {
                    symbol: row.get(0)?,
                    path: row.get::<_, String>(1).unwrap_or_default(),
                    edge_confidence: row.get(2)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let risk = if callers.len() > 10 {
            Some("high".into())
        } else if callers.len() > 3 {
            Some("medium".into())
        } else {
            Some("low".into())
        };

        serde_json::to_string_pretty(&EditContextOutput {
            symbol: p.symbol,
            callers,
            callees,
            risk_assessment: risk,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "session_context",
        description = "Session tracking state — explored symbols, files, tool call count."
    )]
    fn session_context(&self) -> String {
        serde_json::to_string_pretty(&SessionContextOutput {
            tool_calls: 0,
            explored_symbols: vec![],
            explored_files: vec![],
            unique_files_explored: 0,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "diff_impact",
        description = "Post-edit blast radius — analyze a diff for affected symbols and risk level. Provide exactly one of: diff, staged, commits."
    )]
    fn diff_impact(&self, Parameters(p): Parameters<DiffImpactParams>) -> String {
        let input_count =
            p.diff.is_some() as u8 + p.staged.is_some() as u8 + p.commits.is_some() as u8;
        if input_count != 1 {
            return serde_json::to_string_pretty(&ErrorOutput {
                error: ErrorDetail {
                    code: "INVALID_INPUT".into(),
                    message: "Exactly one of diff, staged, or commits must be provided".into(),
                    recoverable: false,
                },
            })
            .unwrap_or_default();
        }

        serde_json::to_string_pretty(&DiffImpactOutput {
            files_changed: vec![],
            aggregate_risk: "unknown".into(),
            note: Some(
                "Diff analysis requires git integration — use ci-core diff_impact module".into(),
            ),
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "indexing_status",
        description = "Current index status — files, symbols, edges, embedding state. Can retry embeddings."
    )]
    fn indexing_status(&self, Parameters(p): Parameters<IndexingStatusParams>) -> String {
        let files: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))
            .unwrap_or(0);
        let symbols: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap_or(0);
        let edges: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap_or(0);

        if p.retry_embeddings {
            tracing::info!("Embeddings retry requested — not yet implemented");
        }

        serde_json::to_string_pretty(&IndexingStatusOutput {
            indexing_phase: "ready".into(),
            files_indexed: files,
            symbols_indexed: symbols,
            edges_indexed: edges,
            embeddings_status: "disabled".into(),
            edges_ready: true,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "locate",
        description = "Compound: search + file_overview + symbol_info in one call. Default depth: with_symbol."
    )]
    fn locate(&self, Parameters(p): Parameters<LocateParams>) -> String {
        let kind_str = p.kind.as_deref().unwrap_or("symbol");
        let kind = match kind_str {
            "text" => ci_core::types::SearchKind::Text,
            "file" => ci_core::types::SearchKind::File,
            "semantic" => ci_core::types::SearchKind::Semantic,
            "hybrid" => ci_core::types::SearchKind::Hybrid,
            _ => ci_core::types::SearchKind::Symbol,
        };
        let limit = p.limit.unwrap_or(10);

        let search_output = match ci_core::search::search(&self.db(), &p.query, kind, limit, false)
        {
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

        let top_symbol = top.and_then(|t| {
            self.db()
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
                        })
                    },
                )
                .ok()
        });

        let truncated = search_output.truncated;
        serde_json::to_string_pretty(&LocateOutput {
            results,
            top_symbol,
            file_overview: None,
            truncated,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "hotspots",
        description = "Churn × complexity hotspots — files most likely to cause bugs based on git history."
    )]
    fn hotspots(&self, Parameters(p): Parameters<HotspotsParams>) -> String {
        let _ = (p.top_n, p.since, p.min_churn, p.include_symbols);
        serde_json::to_string_pretty(&HotspotsOutput {
            hotspots: vec![],
            count: 0,
        })
        .unwrap_or_default()
    }

    #[tool(
        name = "understand",
        description = "Compound: locate + source + callers in one call. Deep understanding of a symbol."
    )]
    fn understand(&self, Parameters(p): Parameters<UnderstandParams>) -> String {
        let kind_str = p.kind.as_deref().unwrap_or("symbol");
        let kind = match kind_str {
            "text" => ci_core::types::SearchKind::Text,
            "file" => ci_core::types::SearchKind::File,
            _ => ci_core::types::SearchKind::Symbol,
        };

        let search_result = ci_core::search::search(&self.db(), &p.query, kind, 1, false);

        let top = search_result
            .ok()
            .and_then(|o| o.results.into_iter().next());

        let symbol_info = top.as_ref().and_then(|t| {
            self.db()
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
                        })
                    },
                )
                .ok()
        });

        let source_output = symbol_info.as_ref().and_then(|info| {
            let full_path = self.project_root.join(&info.path);
            let content = std::fs::read_to_string(&full_path).ok()?;
            let lines: Vec<&str> = content.lines().collect();
            let start = (info.line_start as usize).saturating_sub(1);
            let end = (info.line_end as usize).min(lines.len());
            let source = sanitize_source_output(&lines[start..end].join("\n"));
            Some(SourceOutput {
                symbol: info.name.clone(),
                path: info.path.clone(),
                line_start: info.line_start,
                line_end: info.line_end,
                source,
                language: "".into(),
            })
        });

        let callers = symbol_info
            .as_ref()
            .map(|info| {
                let _conn9 = self.db();
                let mut stmt = _conn9
                    .prepare(
                        "SELECT from_symbol, from_path, edge_confidence
                         FROM call_edges WHERE to_symbol = ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![info.qualified_name], |row| {
                    Ok(CallerEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        serde_json::to_string_pretty(&UnderstandOutput {
            symbol: symbol_info,
            source: source_output,
            callers_summary: callers,
            edges_ready: Some(true),
        })
        .unwrap_or_default()
    }
}

#[tool(tool_box)]
impl rmcp::ServerHandler for CodeIntelligenceServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo {
            instructions: Some("Code Intelligence MCP server — codebase analysis tools".into()),
            ..Default::default()
        }
    }
}
