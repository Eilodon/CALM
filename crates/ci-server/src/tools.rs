use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use rmcp::handler::server::tool::Parameters;
use rmcp::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use ci_core::embedding::Embedder;
use ci_core::sanitize::sanitize_source_output;
use ci_core::types::{EmbedStatus, IndexingPhase};

// ---------------------------------------------------------------------------
// Server state
// ---------------------------------------------------------------------------

/// In-memory session tracking — tool call count and the set of symbols/files
/// touched, for the `session_context` tool. Reset only when the server restarts.
#[derive(Default)]
struct SessionLog {
    tool_calls: u64,
    explored_symbols: std::collections::HashSet<String>,
    explored_files: std::collections::HashSet<String>,
}

#[derive(Clone)]
pub struct CodeIntelligenceServer {
    project_root: PathBuf,
    db_path: PathBuf,
    conn: Arc<Mutex<rusqlite::Connection>>,
    /// Current indexing phase, shared with the background indexer thread.
    /// Tools read it to report `indexing_phase` / `edges_ready` honestly instead
    /// of assuming the graph is built.
    phase: Arc<RwLock<IndexingPhase>>,
    /// Loaded embedding model (None until/unless embeddings are enabled+ready),
    /// shared with the background indexer that loads it.
    embedder: Arc<RwLock<Option<Arc<Embedder>>>>,
    /// Embedding pipeline status, surfaced as `embeddings_status`.
    embed_status: Arc<RwLock<EmbedStatus>>,
    /// Coverage data loaded once at startup from lcov/cobertura/etc files, if present.
    coverage: Arc<ci_core::analysis::coverage::CoverageData>,
    session_log: Arc<Mutex<SessionLog>>,
}

impl CodeIntelligenceServer {
    pub fn new(project_root: PathBuf, db_path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = rusqlite::Connection::open(&db_path)?;
        ci_core::db::schema::init_db(&conn)?;
        let coverage = ci_core::analysis::coverage::load_coverage(&project_root);
        Ok(Self {
            project_root,
            db_path,
            conn: Arc::new(Mutex::new(conn)),
            phase: Arc::new(RwLock::new(IndexingPhase::Scanning)),
            embedder: Arc::new(RwLock::new(None)),
            embed_status: Arc::new(RwLock::new(EmbedStatus::Disabled)),
            coverage: Arc::new(coverage),
            session_log: Arc::new(Mutex::new(SessionLog::default())),
        })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().unwrap()
    }

    /// Wraps `telemetry::timed_tool`, additionally bumping the session's tool-call
    /// counter. Kept as a method (rather than changing `timed_tool`'s signature)
    /// since only this type has access to `session_log`.
    fn timed_tool(&self, name: &str, body: impl FnOnce() -> String) -> String {
        if let Ok(mut log) = self.session_log.lock() {
            log.tool_calls += 1;
        }
        crate::telemetry::timed_tool(name, body)
    }

    fn track_symbol(&self, qualified_name: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            log.explored_symbols.insert(qualified_name.to_string());
        }
    }

    fn track_file(&self, path: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            log.explored_files.insert(path.to_string());
        }
    }

    /// A handle the background indexer uses to advance the phase as it works.
    pub fn phase_handle(&self) -> Arc<RwLock<IndexingPhase>> {
        Arc::clone(&self.phase)
    }

    /// Handles the background indexer uses to publish the loaded model + status.
    pub fn embedder_handle(&self) -> Arc<RwLock<Option<Arc<Embedder>>>> {
        Arc::clone(&self.embedder)
    }
    pub fn embed_status_handle(&self) -> Arc<RwLock<EmbedStatus>> {
        Arc::clone(&self.embed_status)
    }

    /// The loaded embedder, if semantic search is ready.
    fn embedder(&self) -> Option<Arc<Embedder>> {
        self.embedder.read().unwrap().clone()
    }

    fn embed_status_str(&self) -> String {
        self.embed_status.read().unwrap().as_str().to_string()
    }

    /// Re-runs the embedding bootstrap in the background when it previously
    /// failed (model load, vector-table creation, or embedding all set status
    /// to `Failed`). No-op for any other status: `Ready`/`Embedding`/
    /// `Downloading` are already done or in flight, and `Disabled` means
    /// semantic search isn't turned on in config. Opens its own DB connection
    /// so the retry doesn't hold the shared connection mutex for its duration.
    fn retry_embeddings_if_failed(&self) {
        // Claim the retry synchronously (Failed -> Downloading) so two
        // overlapping `retry_embeddings` requests can't both spawn a bootstrap.
        {
            let mut status = self.embed_status.write().unwrap();
            if *status != EmbedStatus::Failed {
                return;
            }
            *status = EmbedStatus::Downloading;
        }
        let semantic = ci_core::config::load_config(&self.project_root)
            .unwrap_or_default()
            .semantic_search;
        let db_path = self.db_path.clone();
        let embedder = Arc::clone(&self.embedder);
        let status = Arc::clone(&self.embed_status);
        std::thread::spawn(move || match rusqlite::Connection::open(&db_path) {
            Ok(conn) => crate::bootstrap_embeddings(&conn, &semantic, &embedder, &status),
            Err(e) => {
                tracing::error!("Embeddings retry: failed to open DB: {e}");
                *status.write().unwrap() = EmbedStatus::Failed;
            }
        });
    }

    fn current_phase(&self) -> IndexingPhase {
        *self.phase.read().unwrap()
    }

    /// Canonical `indexing_phase` string for tool responses.
    fn phase_str(&self) -> String {
        self.current_phase().as_str().to_string()
    }

    /// `edges_ready` is true only once the full graph is built.
    fn edges_ready(&self) -> bool {
        self.current_phase() == IndexingPhase::Ready
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

fn not_found_json(symbol: &str) -> String {
    error_json(
        "NOT_FOUND",
        &format!("Symbol '{symbol}' not found in index"),
        false,
    )
}

// ---------------------------------------------------------------------------
// Ambiguity Contract — shared symbol resolver
// ---------------------------------------------------------------------------
//
// `symbols.name` is not unique: the same bare name can appear in many files,
// or more than once in one file (distinct classes' methods). Tools that take
// a bare `symbol` name must not silently pick one match via `LIMIT 1` — per
// CONTRACTS.md they return `AmbiguousResult` instead when the name has
// multiple matches and no `path` was given to disambiguate.

const MAX_AMBIGUOUS_CANDIDATES: usize = 10;

#[derive(Serialize, JsonSchema)]
struct AmbiguousCandidate {
    name: String,
    path: String,
    kind: String,
    line_start: i64,
    line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    class_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct AmbiguousResult {
    ambiguous: bool,
    candidates: Vec<AmbiguousCandidate>,
}

/// One `symbols` row matched by a bare-name (+ optional path) lookup.
/// Carries enough columns to populate either a concrete tool output (e.g.
/// `SymbolInfoOutput`) or an `AmbiguousCandidate` when the lookup turns out
/// to match more than one row.
struct CandidateRow {
    name: String,
    qualified_name: String,
    kind: String,
    path: String,
    line_start: i64,
    line_end: i64,
    signature: String,
    docstring: String,
    caller_count: i64,
    is_hub: bool,
    language: String,
    class_context: Option<String>,
    is_entry_point: bool,
}

impl CandidateRow {
    fn to_symbol_info(&self) -> SymbolInfoOutput {
        SymbolInfoOutput {
            name: self.name.clone(),
            qualified_name: self.qualified_name.clone(),
            kind: self.kind.clone(),
            path: self.path.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            signature: Some(self.signature.clone()).filter(|s| !s.is_empty()),
            docstring: Some(self.docstring.clone()).filter(|s| !s.is_empty()),
            caller_count: self.caller_count,
            is_hub: self.is_hub,
            health: None,
        }
    }

    fn to_ambiguous_candidate(&self) -> AmbiguousCandidate {
        AmbiguousCandidate {
            name: self.name.clone(),
            path: self.path.clone(),
            kind: self.kind.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            class_context: self.class_context.clone(),
            caller_count: Some(self.caller_count),
            language: Some(self.language.clone()).filter(|s| !s.is_empty()),
            signature: Some(self.signature.clone()).filter(|s| !s.is_empty()),
        }
    }
}

/// All `symbols` rows matching `name` (and `path`, when given). Unlike the
/// old per-tool `LIMIT 1` queries, this returns every match so callers can
/// detect ambiguity instead of guessing.
fn resolve_symbol_candidates(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
) -> Vec<CandidateRow> {
    let sql = if path.is_some() {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point
         FROM symbols WHERE name = ?1 AND path = ?2"
    } else {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point
         FROM symbols WHERE name = ?1"
    };

    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<CandidateRow> {
        Ok(CandidateRow {
            name: row.get(0)?,
            qualified_name: row.get(1)?,
            kind: row.get(2)?,
            path: row.get(3)?,
            line_start: row.get(4)?,
            line_end: row.get(5)?,
            signature: row.get(6)?,
            docstring: row.get(7)?,
            caller_count: row.get(8)?,
            is_hub: row.get::<_, i64>(9)? != 0,
            language: row.get(10)?,
            class_context: row.get(11)?,
            is_entry_point: row.get::<_, i64>(12)? != 0,
        })
    };

    let rows = if let Some(path) = path {
        stmt.query_map(rusqlite::params![name, path], map_row)
    } else {
        stmt.query_map(rusqlite::params![name], map_row)
    };

    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

enum SymbolResolution {
    NotFound,
    Ambiguous(Vec<CandidateRow>),
    Found(CandidateRow),
}

/// Resolve a bare symbol name (+ optional path) to exactly one row.
/// `path`, when given, narrows the candidate set (see
/// `resolve_symbol_candidates`), but does not by itself guarantee a unique
/// match — `name` + `path` is not a DB-enforced unique key (only
/// `qualified_name` is), so e.g. two same-named methods on different classes
/// in the same file still resolve as ambiguous even with `path` set.
fn resolve_symbol(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
) -> SymbolResolution {
    let mut candidates = resolve_symbol_candidates(conn, name, path);
    if candidates.is_empty() {
        SymbolResolution::NotFound
    } else if candidates.len() == 1 {
        SymbolResolution::Found(candidates.remove(0))
    } else {
        SymbolResolution::Ambiguous(candidates)
    }
}

fn ambiguous_json(candidates: &[CandidateRow]) -> String {
    let candidates = candidates
        .iter()
        .take(MAX_AMBIGUOUS_CANDIDATES)
        .map(CandidateRow::to_ambiguous_candidate)
        .collect();
    serde_json::to_string_pretty(&AmbiguousResult {
        ambiguous: true,
        candidates,
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

/// Shared by the `file_overview` tool and `locate` (when the top result is a
/// file match), so both surfaces build the same shape from the same query.
fn build_file_overview(conn: &rusqlite::Connection, path: &str) -> FileOverviewOutput {
    let symbols: Vec<FileOverviewSymbol> = {
        let mut stmt = conn
            .prepare(
                "SELECT name, qualified_name, kind, line_start, line_end
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
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<HealthOutput>,
}

#[derive(Serialize, JsonSchema)]
struct HealthOutput {
    dead_code_confidence: String,
    dead_code_source: String,
}

/// Best-effort "is this symbol private/internal" signal from name + signature
/// conventions, per tier-0 language. Used only as a `dead_code_confidence`
/// input — not stored, computed live from columns already in the index.
fn is_private_symbol(language: &str, name: &str, signature: &str) -> bool {
    match language {
        "python" => name.starts_with('_'),
        "rust" => !signature.contains("pub "),
        "go" => name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false),
        "java" => signature.contains("private "),
        "javascript" | "typescript" => !signature.contains("export"),
        _ => false,
    }
}

/// Whether `language` is a tier-0 language with full symbol extraction
/// (vs. the generic textual-only fallback), per `get_lang_constants`.
fn scope_clear_for_language(language: &str) -> bool {
    ci_core::indexer::lang_constants::get_lang_constants(language).is_some()
}

fn build_health(
    coverage: &ci_core::analysis::coverage::CoverageData,
    project_root: &std::path::Path,
    c: &CandidateRow,
) -> HealthOutput {
    let abs_path = project_root.join(&c.path).to_string_lossy().to_string();
    let is_private = is_private_symbol(&c.language, &c.name, &c.signature);
    let scope_clear = scope_clear_for_language(&c.language);
    let (confidence, source) = ci_core::analysis::dead_code::compute_dead_code_confidence(
        &abs_path,
        c.line_start,
        c.line_end,
        c.caller_count,
        c.is_entry_point,
        is_private,
        scope_clear,
        coverage,
    );
    HealthOutput {
        dead_code_confidence: confidence.to_string(),
        dead_code_source: source.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tool 5: source
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct SourceParams {
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default)]
    include_metadata: bool,
}

#[derive(Serialize, JsonSchema)]
struct SourceMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    docstring: Option<String>,
    caller_count: i64,
    is_hub: bool,
}

#[derive(Serialize, JsonSchema)]
struct SourceOutput {
    symbol: String,
    path: String,
    line_start: i64,
    line_end: i64,
    source: String,
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<SourceMetadata>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    transitive: Option<Vec<TransitiveEntry>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    transitive: Option<Vec<TransitiveEntry>>,
}

#[derive(Serialize, JsonSchema)]
struct TransitiveEntry {
    symbol: String,
    path: String,
    depth: i64,
    edge_confidence: String,
}

#[derive(Clone, Copy)]
enum EdgeDirection {
    Callers,
    Callees,
}

/// BFS over `call_edges` beyond the direct neighbors, shared by `callers` and
/// `callees` when `transitive: true`. Bounded by `max_depth` and a wall-clock
/// timeout so a hub symbol can't blow up the response.
fn transitive_bfs(
    conn: &rusqlite::Connection,
    start_qualified_name: &str,
    direction: EdgeDirection,
    max_depth: usize,
    timeout_ms: u64,
) -> Vec<TransitiveEntry> {
    let sql = match direction {
        EdgeDirection::Callers => {
            "SELECT from_symbol, from_path, edge_confidence FROM call_edges WHERE to_symbol = ?1"
        }
        EdgeDirection::Callees => {
            "SELECT to_symbol, to_path, edge_confidence FROM call_edges WHERE from_symbol = ?1"
        }
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(timeout_ms);

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(start_qualified_name.to_string());
    let mut frontier = vec![start_qualified_name.to_string()];
    let mut results = Vec::new();
    let mut depth = 0usize;

    while depth < max_depth && !frontier.is_empty() {
        if start.elapsed() > deadline {
            break;
        }
        depth += 1;
        let mut next_frontier = Vec::new();
        for sym in &frontier {
            let rows = stmt.query_map(rusqlite::params![sym], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1).unwrap_or_default(),
                    row.get::<_, String>(2)?,
                ))
            });
            let Ok(rows) = rows else { continue };
            for (sym_name, sym_path, edge_confidence) in rows.filter_map(|r| r.ok()) {
                if visited.insert(sym_name.clone()) {
                    results.push(TransitiveEntry {
                        symbol: sym_name.clone(),
                        path: sym_path,
                        depth: depth as i64,
                        edge_confidence,
                    });
                    next_frontier.push(sym_name);
                }
            }
        }
        frontier = next_frontier;
    }

    results
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
    from_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_hops: Option<i64>,
}

/// Local mirror of `ci_core::types::TerminatedBy` — that type lives in
/// `ci-core`, which doesn't depend on `schemars`, so it can't derive
/// `JsonSchema` itself.
#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TerminatedByOutput {
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
struct PathOutput {
    from_symbol: String,
    to_symbol: String,
    routes: Vec<Vec<String>>,
    route_count: usize,
    exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminated_by: Option<TerminatedByOutput>,
    hops_clamped: bool,
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
struct DiffImpactParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commits: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct BlastRadiusOutput {
    direct_callers: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct RiskAssessmentOutput {
    level: String,
    reasons: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct AffectedSymbolOutput {
    qualified_name: String,
    name: String,
    path: String,
    kind: String,
    signature_changed: bool,
    blast_radius: BlastRadiusOutput,
    risk_assessment: RiskAssessmentOutput,
}

#[derive(Serialize, JsonSchema)]
struct DiffImpactOutput {
    files_changed: Vec<String>,
    affected_symbols: Vec<AffectedSymbolOutput>,
    unindexed_files: Vec<String>,
    aggregate_risk: String,
    suggested_reviewers: Vec<String>,
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
    /// Set when the requested `depth` was auto-downgraded — see
    /// `CONTRACTS.md`'s `LocateDepth` invariant: `kind ∈ {text, file}` +
    /// `depth = with_symbol` has no meaningful symbol to enrich, so it's
    /// downgraded to `with_file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    depth_adjusted: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool 15: hotspots
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
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
struct HotspotChurnOutput {
    commit_count: i64,
    authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_changed: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct HotspotComplexityOutput {
    symbol_count: i64,
    hub_count: i64,
    avg_caller_count: f64,
    connected_coreness_count: i64,
    language: String,
}

#[derive(Serialize, JsonSchema)]
struct HotspotSymbolOutput {
    name: String,
    kind: String,
    is_hub: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    coreness: Option<i64>,
    caller_count: i64,
}

#[derive(Serialize, JsonSchema)]
struct HotspotEntryOutput {
    path: String,
    language: String,
    churn: HotspotChurnOutput,
    complexity: HotspotComplexityOutput,
    hotspot_score: f64,
    risk_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_symbols: Option<Vec<HotspotSymbolOutput>>,
}

#[derive(Serialize, JsonSchema)]
struct HotspotsOutput {
    hotspots: Vec<HotspotEntryOutput>,
    count: usize,
    git_available: bool,
    since: String,
    total_files_analyzed: usize,
    hotspot_method: String,
    note: String,
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
        self.timed_tool("repo_overview", || {
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
                indexing_phase: self.phase_str(),
                embeddings_status: self.embed_status_str(),
                total_modules: total_files,
                total_symbols,
                total_files,
                truncated: false,
                workflow_guide:
                    "Use locate to find symbols, then source/callers/callees to explore.".into(),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "search",
        description = "FTS5 dual-column search across symbols, text, files. Supports symbol, text, file, semantic, hybrid kinds."
    )]
    fn search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        self.timed_tool("search", || {
            let kind = match p.kind.as_str() {
                "symbol" => ci_core::types::SearchKind::Symbol,
                "text" => ci_core::types::SearchKind::Text,
                "file" => ci_core::types::SearchKind::File,
                "semantic" => ci_core::types::SearchKind::Semantic,
                "hybrid" => ci_core::types::SearchKind::Hybrid,
                _ => ci_core::types::SearchKind::Symbol,
            };

            match ci_core::search::search(
                &self.db(),
                &p.query,
                kind,
                p.limit,
                self.embedder().as_deref(),
            ) {
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
        })
    }

    #[tool(
        name = "file_overview",
        description = "List all symbols in a file — functions, classes, methods with line ranges."
    )]
    fn file_overview(&self, Parameters(p): Parameters<FileOverviewParams>) -> String {
        self.timed_tool("file_overview", || {
            self.track_file(&p.path);
            let conn = self.db();
            serde_json::to_string_pretty(&build_file_overview(&conn, &p.path)).unwrap_or_default()
        })
    }

    #[tool(
        name = "symbol_info",
        description = "Detailed info for a single symbol — signature, docstring, hub status, caller count."
    )]
    fn symbol_info(&self, Parameters(p): Parameters<SymbolInfoParams>) -> String {
        self.timed_tool("symbol_info", || {
            let resolution = {
                let conn = self.db();
                resolve_symbol(&conn, &p.symbol, p.path.as_deref())
            };
            match resolution {
                SymbolResolution::NotFound => not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => ambiguous_json(&candidates),
                SymbolResolution::Found(c) => {
                    self.track_symbol(&c.qualified_name);
                    self.track_file(&c.path);
                    let mut out = c.to_symbol_info();
                    out.health = Some(build_health(&self.coverage, &self.project_root, &c));
                    serde_json::to_string_pretty(&out).unwrap_or_default()
                }
            }
        })
    }

    #[tool(
        name = "source",
        description = "Retrieve source code for a symbol. Output is sanitized for credentials."
    )]
    fn source(&self, Parameters(p): Parameters<SourceParams>) -> String {
        self.timed_tool("source", || {
            let resolution = {
                let conn = self.db();
                resolve_symbol(&conn, &p.symbol, p.path.as_deref())
            };
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let full_path = self.project_root.join(&c.path);
            let source = match std::fs::read_to_string(&full_path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let start = (c.line_start as usize).saturating_sub(1);
                    let end = (c.line_end as usize).min(lines.len());
                    lines[start..end].join("\n")
                }
                Err(_) => "(source file not readable)".into(),
            };
            let sanitized = sanitize_source_output(&source);

            let metadata = p.include_metadata.then(|| SourceMetadata {
                signature: Some(c.signature.clone()).filter(|s| !s.is_empty()),
                docstring: Some(c.docstring.clone()).filter(|s| !s.is_empty()),
                caller_count: c.caller_count,
                is_hub: c.is_hub,
            });

            serde_json::to_string_pretty(&SourceOutput {
                symbol: p.symbol,
                path: c.path,
                line_start: c.line_start,
                line_end: c.line_end,
                source: sanitized,
                language: c.language,
                metadata,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "callers",
        description = "Who calls this symbol? Returns direct callers with edge confidence."
    )]
    fn callers(&self, Parameters(p): Parameters<CallersParams>) -> String {
        self.timed_tool("callers", || {
            let resolution = {
                let conn = self.db();
                resolve_symbol(&conn, &p.symbol, p.path.as_deref())
            };
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let direct: Vec<CallerEntry> = {
                let conn = self.db();
                let mut stmt = conn
                    .prepare(
                        "SELECT from_symbol, from_path, edge_confidence
                         FROM call_edges WHERE to_symbol = ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    Ok(CallerEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let transitive = if p.transitive {
                let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
                let max_depth = p
                    .max_depth
                    .map(|d| (d.max(1) as usize).min(config.callers.max_depth_cap))
                    .unwrap_or(config.callers.max_depth_cap);
                let conn = self.db();
                Some(transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callers,
                    max_depth,
                    config.callers.transitive_timeout_ms,
                ))
            } else {
                None
            };

            let count = direct.len();
            serde_json::to_string_pretty(&CallersOutput {
                symbol: p.symbol,
                direct,
                direct_count: count,
                transitive,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "callees",
        description = "What does this symbol call? Returns direct callees with edge confidence."
    )]
    fn callees(&self, Parameters(p): Parameters<CalleesParams>) -> String {
        self.timed_tool("callees", || {
            let resolution = {
                let conn = self.db();
                resolve_symbol(&conn, &p.symbol, p.path.as_deref())
            };
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let direct: Vec<CalleeEntry> = {
                let conn = self.db();
                let mut stmt = conn
                    .prepare(
                        "SELECT to_symbol, to_path, edge_confidence
                         FROM call_edges WHERE from_symbol = ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    Ok(CalleeEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let transitive = if p.transitive {
                let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
                let max_depth = p
                    .max_depth
                    .map(|d| (d.max(1) as usize).min(config.callees.max_depth_cap))
                    .unwrap_or(config.callees.max_depth_cap);
                let conn = self.db();
                Some(transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callees,
                    max_depth,
                    config.callees.transitive_timeout_ms,
                ))
            } else {
                None
            };

            let count = direct.len();
            serde_json::to_string_pretty(&CalleesOutput {
                symbol: p.symbol,
                direct,
                direct_count: count,
                transitive,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "dependencies",
        description = "Import/export dependencies for a file — what it imports and what imports it."
    )]
    fn dependencies(&self, Parameters(p): Parameters<DependenciesParams>) -> String {
        self.timed_tool("dependencies", || {
            self.track_file(&p.path);
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
        })
    }

    #[tool(
        name = "path",
        description = "Find call paths between two symbols using bidirectional BFS."
    )]
    fn path(&self, Parameters(p): Parameters<PathParams>) -> String {
        self.timed_tool("path", || {
            let from = {
                let conn = self.db();
                resolve_symbol(&conn, &p.from_symbol, p.from_path.as_deref())
            };
            let from = match from {
                SymbolResolution::NotFound => return not_found_json(&p.from_symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => c,
            };
            self.track_symbol(&from.qualified_name);
            self.track_file(&from.path);

            let to = {
                let conn = self.db();
                resolve_symbol(&conn, &p.to_symbol, p.to_path.as_deref())
            };
            let to = match to {
                SymbolResolution::NotFound => return not_found_json(&p.to_symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => c,
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
                let conn = self.db();
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
            serde_json::to_string_pretty(&PathOutput {
                from_symbol: p.from_symbol,
                to_symbol: p.to_symbol,
                routes,
                route_count: count,
                exists,
                terminated_by,
                hops_clamped,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "edit_context",
        description = "Pre-edit blast radius — callers, callees, and risk assessment for a symbol you plan to modify."
    )]
    fn edit_context(&self, Parameters(p): Parameters<EditContextParams>) -> String {
        self.timed_tool("edit_context", || {
            let resolution = {
                let conn = self.db();
                resolve_symbol(&conn, &p.symbol, p.path.as_deref())
            };
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let callers: Vec<CallerEntry> = {
                let conn = self.db();
                let mut stmt = conn
                    .prepare(
                        "SELECT from_symbol, from_path, edge_confidence
                         FROM call_edges WHERE to_symbol = ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    Ok(CallerEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let callees: Vec<CalleeEntry> = {
                let conn = self.db();
                let mut stmt = conn
                    .prepare(
                        "SELECT to_symbol, to_path, edge_confidence
                         FROM call_edges WHERE from_symbol = ?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    Ok(CalleeEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

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
        })
    }

    #[tool(
        name = "session_context",
        description = "Session tracking state — explored symbols, files, tool call count."
    )]
    fn session_context(&self) -> String {
        self.timed_tool("session_context", || {
            let log = self.session_log.lock().unwrap();
            let explored_files: Vec<String> = log.explored_files.iter().cloned().collect();
            serde_json::to_string_pretty(&SessionContextOutput {
                tool_calls: log.tool_calls,
                explored_symbols: log.explored_symbols.iter().cloned().collect(),
                unique_files_explored: explored_files.len(),
                explored_files,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "diff_impact",
        description = "Post-edit blast radius — analyze a diff for affected symbols and risk level. Provide exactly one of: diff, staged, commits."
    )]
    fn diff_impact(&self, Parameters(p): Parameters<DiffImpactParams>) -> String {
        self.timed_tool("diff_impact", || {
            let input_count =
                p.diff.is_some() as u8 + p.staged.is_some() as u8 + p.commits.is_some() as u8;
            if input_count != 1 {
                return error_json(
                    "INVALID_INPUT",
                    "Exactly one of diff, staged, or commits must be provided",
                    false,
                );
            }

            const DIFF_GIT_TIMEOUT_SECS: u64 = 10;
            let diff_text = if let Some(d) = p.diff {
                d
            } else {
                let staged = p.staged.unwrap_or(false);
                let (diff, err) = ci_core::analysis::diff_impact::get_git_diff(
                    &self.project_root,
                    staged,
                    p.commits.as_deref(),
                    DIFF_GIT_TIMEOUT_SECS,
                );
                match diff {
                    Some(d) => d,
                    None => {
                        return error_json(
                            "FEATURE_UNAVAILABLE",
                            &err.unwrap_or_else(|| "git diff failed".into()),
                            true,
                        );
                    }
                }
            };

            let file_diffs = ci_core::analysis::diff_impact::parse_unified_diff(&diff_text);
            let files_changed: Vec<String> = file_diffs.iter().map(|f| f.path.clone()).collect();

            let mut unindexed_files: Vec<String> = Vec::new();
            let mut affected: Vec<std::collections::HashMap<String, serde_json::Value>> =
                Vec::new();

            {
                let conn = self.db();
                for fd in &file_diffs {
                    let symbol_count: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM symbols WHERE path = ?1",
                            rusqlite::params![fd.path],
                            |r| r.get(0),
                        )
                        .unwrap_or(0);
                    if symbol_count == 0 {
                        unindexed_files.push(fd.path.clone());
                        continue;
                    }

                    let mut stmt = conn
                        .prepare(
                            "SELECT qualified_name, name, kind, line_start, line_end, caller_count
                             FROM symbols WHERE path = ?1",
                        )
                        .unwrap();
                    let rows: Vec<(String, String, String, i64, i64, i64)> = stmt
                        .query_map(rusqlite::params![fd.path], |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ))
                        })
                        .unwrap()
                        .filter_map(|r| r.ok())
                        .collect();

                    for (qualified_name, name, kind, line_start, line_end, caller_count) in rows {
                        let overlaps = fd
                            .hunks
                            .iter()
                            .any(|&(hs, he)| !(he < line_start || hs > line_end));
                        if !overlaps {
                            continue;
                        }

                        let sig_end = line_start + (line_end - line_start).min(2);
                        let signature_changed =
                            ci_core::analysis::diff_impact::is_signature_changed(
                                (line_start, sig_end),
                                &fd.hunks,
                            );

                        let base_level = if caller_count > 10 {
                            "high"
                        } else if caller_count > 3 {
                            "medium"
                        } else {
                            "low"
                        };
                        let mut reasons: Vec<String> = Vec::new();
                        let level =
                            ci_core::analysis::diff_impact::escalate_risk_if_signature_changed(
                                signature_changed,
                                base_level,
                                &mut reasons,
                            );

                        let mut m: std::collections::HashMap<String, serde_json::Value> =
                            std::collections::HashMap::new();
                        m.insert("qualified_name".into(), serde_json::json!(qualified_name));
                        m.insert("name".into(), serde_json::json!(name));
                        m.insert("path".into(), serde_json::json!(fd.path));
                        m.insert("kind".into(), serde_json::json!(kind));
                        m.insert(
                            "signature_changed".into(),
                            serde_json::json!(signature_changed),
                        );
                        m.insert(
                            "blast_radius".into(),
                            serde_json::json!({"direct_callers": caller_count}),
                        );
                        m.insert(
                            "risk_assessment".into(),
                            serde_json::json!({"level": level, "reasons": reasons}),
                        );
                        affected.push(m);
                    }
                }
            }

            let aggregate_risk = ci_core::analysis::diff_impact::compute_aggregate_risk(
                &affected,
                &unindexed_files,
            );
            const MAX_AFFECTED_SYMBOLS: usize = 20;
            ci_core::analysis::diff_impact::sort_affected_symbols(
                &mut affected,
                MAX_AFFECTED_SYMBOLS,
            );

            let affected_symbols: Vec<AffectedSymbolOutput> = affected
                .into_iter()
                .filter_map(|m| {
                    serde_json::to_value(m)
                        .ok()
                        .and_then(|v| serde_json::from_value(v).ok())
                })
                .collect();

            let codeowner_patterns =
                ci_core::analysis::codeowners::load_codeowners(&self.project_root);
            let mut suggested_reviewers: Vec<String> = Vec::new();
            for f in &files_changed {
                for owner in ci_core::analysis::codeowners::find_owners(&codeowner_patterns, f) {
                    if !suggested_reviewers.contains(&owner) {
                        suggested_reviewers.push(owner);
                    }
                }
            }

            serde_json::to_string_pretty(&DiffImpactOutput {
                files_changed,
                affected_symbols,
                unindexed_files,
                aggregate_risk,
                suggested_reviewers,
                note: None,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "indexing_status",
        description = "Current index status — files, symbols, edges, embedding state. Can retry embeddings."
    )]
    fn indexing_status(&self, Parameters(p): Parameters<IndexingStatusParams>) -> String {
        self.timed_tool("indexing_status", || {
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
                self.retry_embeddings_if_failed();
            }

            serde_json::to_string_pretty(&IndexingStatusOutput {
                indexing_phase: self.phase_str(),
                files_indexed: files,
                symbols_indexed: symbols,
                edges_indexed: edges,
                embeddings_status: self.embed_status_str(),
                edges_ready: self.edges_ready(),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "locate",
        description = "Compound: search + file_overview + symbol_info in one call. Default depth: with_symbol."
    )]
    fn locate(&self, Parameters(p): Parameters<LocateParams>) -> String {
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

            let search_output = match ci_core::search::search(
                &self.db(),
                &p.query,
                kind,
                limit,
                self.embedder().as_deref(),
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
                                    health: None,
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
                    let conn = self.db();
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
            serde_json::to_string_pretty(&LocateOutput {
                results,
                top_symbol,
                file_overview,
                truncated,
                depth_adjusted,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "hotspots",
        description = "Churn × complexity hotspots — files most likely to cause bugs based on git history."
    )]
    fn hotspots(&self, Parameters(p): Parameters<HotspotsParams>) -> String {
        self.timed_tool("hotspots", || {
            let config = ci_core::config::load_config(&self.project_root).unwrap_or_default();
            let hc = &config.hotspots;
            let top_n = p.top_n.unwrap_or(hc.default_top_n);
            let since = p.since.unwrap_or_else(|| hc.default_since.clone());
            let min_churn = p.min_churn.unwrap_or(hc.default_min_churn as i64);

            let result = {
                let conn = self.db();
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

            let hotspots: Vec<HotspotEntryOutput> =
                result.hotspots.into_iter().map(HotspotEntryOutput::from).collect();
            let count = hotspots.len();

            serde_json::to_string_pretty(&HotspotsOutput {
                hotspots,
                count,
                git_available: result.git_available,
                since: result.since,
                total_files_analyzed: result.total_files_analyzed,
                hotspot_method: result.hotspot_method,
                note: result.note,
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "understand",
        description = "Compound: locate + source + callers in one call. Deep understanding of a symbol."
    )]
    fn understand(&self, Parameters(p): Parameters<UnderstandParams>) -> String {
        self.timed_tool("understand", || {
            let kind_str = p.kind.as_deref().unwrap_or("symbol");
            let kind = match kind_str {
                "text" => ci_core::types::SearchKind::Text,
                "file" => ci_core::types::SearchKind::File,
                _ => ci_core::types::SearchKind::Symbol,
            };

            let search_result =
                ci_core::search::search(&self.db(), &p.query, kind, 1, self.embedder().as_deref());

            let top = search_result
                .ok()
                .and_then(|o| o.results.into_iter().next());

            // Carries `language` alongside `SymbolInfoOutput` (which doesn't have
            // a language field) so `SourceOutput.language` below isn't stubbed.
            let symbol_info: Option<(SymbolInfoOutput, String)> = top.as_ref().and_then(|t| {
                self.db()
                    .query_row(
                        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language
                         FROM symbols WHERE qualified_name = ?1 LIMIT 1",
                        rusqlite::params![t.qualified_name],
                        |row| {
                            Ok((
                                SymbolInfoOutput {
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
                                    health: None,
                                },
                                row.get::<_, String>(10).unwrap_or_default(),
                            ))
                        },
                    )
                    .ok()
            });

            if let Some((info, _)) = symbol_info.as_ref() {
                self.track_symbol(&info.qualified_name);
                self.track_file(&info.path);
            }

            let source_output = symbol_info.as_ref().and_then(|(info, language)| {
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
                    language: language.clone(),
                    metadata: None,
                })
            });

            let callers = symbol_info
                .as_ref()
                .map(|(info, _)| {
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
                symbol: symbol_info.map(|(info, _)| info),
                source: source_output,
                callers_summary: callers,
                edges_ready: Some(self.edges_ready()),
            })
            .unwrap_or_default()
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_ready_follows_indexing_phase() {
        let dir = std::env::temp_dir().join(format!("ci_phase_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        // Fresh server: still scanning, so tools must report edges not ready.
        assert_eq!(server.phase_str(), "scanning");
        assert!(!server.edges_ready());

        // Indexer signals completion via the shared handle.
        *server.phase_handle().write().unwrap() = IndexingPhase::Ready;
        assert_eq!(server.phase_str(), "ready");
        assert!(server.edges_ready());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_impact_raw_diff_maps_to_affected_symbols_and_reviewers() {
        let dir = std::env::temp_dir().join(format!("ci_diff_impact_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::write(dir.join(".github/CODEOWNERS"), "*.rs @rust-team\n").unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    "mod.foo", "foo", "function", "rust", "src/foo.rs", 10i64, 15i64, "fn foo()",
                    "", "foo", 5i64, 0i64, 0i64
                ],
            )
            .unwrap();
        }

        // Hunk touches only the body (lines 14-15), not the signature heuristic
        // range (line_start..line_start+2 = 10-12) — should NOT escalate to high.
        let diff = "diff --git a/src/foo.rs b/src/foo.rs\n\
                     --- a/src/foo.rs\n\
                     +++ b/src/foo.rs\n\
                     @@ -14,1 +14,2 @@ fn foo() {\n\
                      context\n\
                     +new line\n";

        let output = server.diff_impact(Parameters(DiffImpactParams {
            diff: Some(diff.to_string()),
            staged: None,
            commits: None,
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(v["files_changed"], serde_json::json!(["src/foo.rs"]));
        assert_eq!(v["affected_symbols"].as_array().unwrap().len(), 1);
        assert_eq!(v["affected_symbols"][0]["qualified_name"], "mod.foo");
        assert_eq!(v["affected_symbols"][0]["signature_changed"], false);
        assert_eq!(v["aggregate_risk"], "medium");
        assert_eq!(v["suggested_reviewers"], serde_json::json!(["@rust-team"]));
        assert!(v["unindexed_files"].as_array().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_impact_unindexed_file_yields_unknown_risk() {
        let dir = std::env::temp_dir().join(format!("ci_diff_impact_unindexed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        let diff = "diff --git a/src/new.rs b/src/new.rs\n\
                     new file mode 100644\n\
                     --- /dev/null\n\
                     +++ b/src/new.rs\n\
                     @@ -0,0 +1,3 @@\n\
                     +fn new_fn() {}\n";

        let output = server.diff_impact(Parameters(DiffImpactParams {
            diff: Some(diff.to_string()),
            staged: None,
            commits: None,
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(v["unindexed_files"], serde_json::json!(["src/new.rs"]));
        assert_eq!(v["aggregate_risk"], "unknown");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_impact_rejects_multiple_inputs() {
        let dir = std::env::temp_dir().join(format!("ci_diff_impact_multi_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        let output = server.diff_impact(Parameters(DiffImpactParams {
            diff: Some("diff --git a/x b/x\n".into()),
            staged: Some(true),
            commits: None,
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["error"]["code"], "INVALID_INPUT");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_context_tracks_tool_calls_and_explored_state() {
        let dir = std::env::temp_dir().join(format!("ci_session_ctx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    "mod.foo", "foo", "function", "rust", "src/foo.rs", 1i64, 5i64, "fn foo()",
                    "", "foo", 0i64, 0i64, 0i64
                ],
            )
            .unwrap();
        }

        let _ = server.symbol_info(Parameters(SymbolInfoParams {
            symbol: "foo".into(),
            path: None,
        }));
        let _ = server.file_overview(Parameters(FileOverviewParams {
            path: "src/foo.rs".into(),
        }));

        let output = server.session_context();
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert!(v["tool_calls"].as_u64().unwrap() >= 3); // symbol_info + file_overview + session_context itself
        assert_eq!(v["explored_symbols"], serde_json::json!(["mod.foo"]));
        assert_eq!(v["explored_files"], serde_json::json!(["src/foo.rs"]));
        assert_eq!(v["unique_files_explored"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_info_stays_ambiguous_when_path_does_not_uniquely_resolve() {
        let dir = std::env::temp_dir().join(format!("ci_ambig_path_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            for qname in ["ClassA.method", "ClassB.method"] {
                conn.execute(
                    "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![
                        qname, "method", "function", "python", "src/multi.py", 1i64, 5i64, "def method()",
                        "", "method", 0i64, 0i64, 0i64
                    ],
                )
                .unwrap();
            }
        }

        // Same `name` + `path`, but two distinct `qualified_name`s — path alone
        // does not disambiguate, so this must stay ambiguous rather than
        // silently picking the first row.
        let output = server.symbol_info(Parameters(SymbolInfoParams {
            symbol: "method".into(),
            path: Some("src/multi.py".into()),
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(v["ambiguous"], true);
        assert_eq!(v["candidates"].as_array().unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_tool_honors_configured_max_allowed_hops() {
        let dir = std::env::temp_dir().join(format!("ci_path_config_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"path": {"max_allowed_hops": 5}}"#).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            for (qname, name, path) in [("mod.a", "a", "src/a.rs"), ("mod.b", "b", "src/b.rs")] {
                conn.execute(
                    "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![qname, name, "function", "rust", path, 1i64, 2i64, "fn x()", "", name, 0i64, 0i64, 0i64],
                )
                .unwrap();
            }
        }

        // Requested 10 hops exceeds the configured max_allowed_hops=5 — with the
        // old hardcoded literal (20) this would NOT have been clamped.
        let output = server.path(Parameters(PathParams {
            from_symbol: "a".into(),
            to_symbol: "b".into(),
            from_path: None,
            to_path: None,
            max_hops: Some(10),
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(v["hops_clamped"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locate_search_only_depth_skips_enrichment_and_tracking() {
        let dir = std::env::temp_dir().join(format!("ci_locate_depth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params!["mod.foo", "foo", "function", "rust", "src/foo.rs", 1i64, 5i64, "fn foo()", "", "foo", 0i64, 0i64, 0i64],
            )
            .unwrap();
        }

        let output = server.locate(Parameters(LocateParams {
            query: "foo".into(),
            kind: None,
            depth: Some("search_only".into()),
            limit: None,
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert!(v["top_symbol"].is_null());
        assert!(v["file_overview"].is_null());
        assert!(v["depth_adjusted"].is_null());

        let session = server.session_context();
        let sv: serde_json::Value = serde_json::from_str(&session).unwrap();
        assert_eq!(sv["explored_symbols"], serde_json::json!([]));
        assert_eq!(sv["explored_files"], serde_json::json!([]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locate_text_kind_downgrades_default_depth_to_with_file() {
        let dir = std::env::temp_dir().join(format!("ci_locate_downgrade_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params!["mod.foo", "foo bar baz", "function", "rust", "src/foo.rs", 1i64, 5i64, "fn foo()", "", "foo bar baz", 0i64, 0i64, 0i64],
            )
            .unwrap();
        }

        // kind="text" + default depth ("with_symbol") must auto-downgrade per
        // the LocateDepth invariant, since a text match has no symbol to enrich.
        let output = server.locate(Parameters(LocateParams {
            query: "bar".into(),
            kind: Some("text".into()),
            depth: None,
            limit: None,
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(v["depth_adjusted"], "with_file");
        assert!(v["top_symbol"].is_null());
        assert!(!v["file_overview"].is_null());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test: `understand`'s inline SQL used to omit the `language`
    /// column, so `SourceOutput.language` was always empty.
    #[test]
    fn understand_includes_symbol_language_in_source_output() {
        let dir = std::env::temp_dir().join(format!("ci_understand_lang_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("foo.py"), "def foo():\n    pass\n").unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    "foo.py::foo", "foo", "function", "python", "foo.py", 1i64, 2i64, "def foo()",
                    "", "foo", 0i64, 0i64, 0i64
                ],
            )
            .unwrap();
        }

        let output = server.understand(Parameters(UnderstandParams {
            query: "foo".into(),
            kind: None,
        }));
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(v["symbol"]["qualified_name"], "foo.py::foo");
        assert_eq!(v["source"]["language"], "python");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test: `retry_embeddings` used to be a no-op (logged "not yet
    /// implemented" and did nothing). It must now reclaim a `Failed` status and
    /// re-run `bootstrap_embeddings` in the background, while leaving any other
    /// status untouched.
    #[test]
    fn retry_embeddings_if_failed_reclaims_failed_status_and_runs_bootstrap() {
        let dir = std::env::temp_dir().join(format!("ci_retry_embed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CodeIntelligenceServer::new(dir.clone(), dir.join("index.db")).unwrap();

        // A non-Failed status is left untouched — only a prior failure is retried.
        *server.embed_status_handle().write().unwrap() = EmbedStatus::Disabled;
        server.retry_embeddings_if_failed();
        assert_eq!(
            *server.embed_status_handle().read().unwrap(),
            EmbedStatus::Disabled
        );

        // Failed is reclaimed synchronously (-> Downloading) before the bootstrap
        // retry is spawned in the background. With the `embeddings` feature off,
        // `Embedder::load` always fails, so the background thread deterministically
        // cycles Downloading -> Failed again.
        *server.embed_status_handle().write().unwrap() = EmbedStatus::Failed;
        server.retry_embeddings_if_failed();

        let mut saw_downloading = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1000);
        while std::time::Instant::now() < deadline {
            if *server.embed_status_handle().read().unwrap() == EmbedStatus::Downloading {
                saw_downloading = true;
                break;
            }
        }
        assert!(
            saw_downloading,
            "retry should synchronously reclaim Failed -> Downloading before spawning the retry"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1000);
        let mut final_status = *server.embed_status_handle().read().unwrap();
        while final_status != EmbedStatus::Failed && std::time::Instant::now() < deadline {
            final_status = *server.embed_status_handle().read().unwrap();
        }
        assert_eq!(final_status, EmbedStatus::Failed);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
