use super::guardrails::*;
use super::inspect::*;
use super::*;

impl CodeIntelligenceServer {
    pub fn new(project_root: PathBuf, db_path: PathBuf) -> anyhow::Result<Self> {
        Self::new_with_preset(project_root, db_path, "full".into())
    }

    pub fn new_with_preset(
        project_root: PathBuf,
        db_path: PathBuf,
        preset: String,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = rusqlite::Connection::open(&db_path)?;
        calm_core::db::schema::init_db(&conn)?;
        drop(conn);
        let coverage = calm_core::analysis::coverage::load_coverage(&project_root);
        Ok(Self {
            project_root,
            db_path,
            phase: Arc::new(RwLock::new(IndexingPhase::Scanning)),
            last_index_error: Arc::new(RwLock::new(None)),
            embedder: Arc::new(RwLock::new(None)),
            embed_status: Arc::new(RwLock::new(EmbedStatus::Disabled)),
            coverage: Arc::new(RwLock::new(coverage)),
            session_log: Arc::new(Mutex::new(SessionLog::default())),
            edit_lock: Arc::new(Mutex::new(())),
            preset,
        })
    }
    /// Opens a new dedicated read-only connection to the same DB file.
    /// Sets `PRAGMA query_only = ON` immediately so any accidental write in a
    /// tool handler is rejected at the SQLite level.
    ///
    /// SINGLE_WRITER enforcement: all tool handlers must use this for reads.
    /// Schema init uses a short-lived local connection in `new_with_preset`.
    pub(crate) fn make_read_conn(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        let conn = rusqlite::Connection::open(&self.db_path)?;
        conn.execute_batch("PRAGMA query_only = ON;")?;
        Ok(conn)
    }

    /// Test-only write connection for seeding fixture data.
    /// Production tool handlers must use `make_read_conn()` instead.
    #[cfg(test)]
    pub(crate) fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db_path).unwrap()
    }

    /// Write connection for `remember` — the one tool handler that isn't
    /// read-only (every other tool must use `make_read_conn()`). Scoped
    /// narrowly: `project_memory` is never touched by the indexer/watcher,
    /// so this doesn't contend with indexing writes in practice; the
    /// `busy_timeout` covers the rare case where SQLite's single-writer-per-
    /// file lock is briefly held by an indexing transaction anyway, rather
    /// than failing the note immediately.
    pub(crate) fn memory_write_conn(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        calm_core::db::conn::open_writer(&self.db_path)
    }
    /// Wraps `telemetry::timed_tool`, additionally bumping the session's tool-call
    /// counter. Kept as a method (rather than changing `timed_tool`'s signature)
    /// since only this type has access to `session_log`.
    pub(crate) fn timed_tool(&self, name: &str, body: impl FnOnce() -> String) -> String {
        if let Ok(mut log) = self.session_log.lock() {
            log.tool_calls += 1;
        }
        crate::telemetry::timed_tool(name, body)
    }

    pub(crate) fn track_symbol(&self, qualified_name: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            let now = log.tool_calls;
            if !log.explored_symbols.contains_key(qualified_name) {
                log.last_progress_at = now;
            }
            log.explored_symbols.insert(qualified_name.to_string(), now);
        }
    }

    pub(crate) fn track_file(&self, path: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            let now = log.tool_calls;
            if !log.explored_files.contains_key(path) {
                log.last_progress_at = now;
            }
            log.explored_files.insert(path.to_string(), now);
        }
    }

    /// Records that `path` was written via `edit_lines`/`edit_symbol` — see
    /// `SessionLog::written_files`. Call once per successful write.
    pub(crate) fn mark_written(&self, path: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            log.written_files.insert(path.to_string());
        }
    }

    /// Read-only snapshot of paths written since the last `diff_impact` call
    /// — for `session_context` to report without clearing anything (only
    /// `diff_impact` itself, via `clear_written_files`, does that).
    pub(crate) fn written_files_snapshot(&self) -> Vec<String> {
        self.session_log
            .lock()
            .map(|log| log.written_files.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Clears the written-files set — `diff_impact` calls this
    /// unconditionally on every call (even for a caller-supplied raw
    /// `diff`/`commits` range unrelated to these specific paths), since
    /// attempting a blast-radius check at all is the signal that matters
    /// here, matching the Claude-Code hook's own "reset every time
    /// diff_impact runs" semantics for its (host-specific) equivalent gate.
    pub(crate) fn clear_written_files(&self) {
        if let Ok(mut log) = self.session_log.lock() {
            log.written_files.clear();
        }
    }

    /// Additive, session-scoped relevance boost for `search`/`locate`
    /// results: a result whose file is import/call-adjacent to something
    /// this session recently explored gets nudged up, so results lean
    /// toward the current work context without ever overriding a strong
    /// text/semantic match. Mutates `results[i].score` in place and re-sorts
    /// — never touches `symbols.is_hub`/`coreness` or any other
    /// DB-persisted, cross-session-shared ranking signal. Returns `true`
    /// when personalization actually adjusted anything, so callers can
    /// report it transparently rather than silently.
    ///
    /// Guaranteed no-op (identical results, in identical order) when this
    /// session hasn't explored anything yet, or `personalization_weight` is
    /// configured to `0.0` — the common case for a session's first calls.
    pub(crate) fn apply_personalization_boost(
        &self,
        conn: &rusqlite::Connection,
        results: &mut [calm_core::search::SearchResult],
    ) -> bool {
        if results.is_empty() {
            return false;
        }
        let weight = calm_core::config::load_config(&self.project_root)
            .map(|c| c.search.personalization_weight)
            .unwrap_or(0.15);
        if weight <= 0.0 {
            return false;
        }
        let (explored_files, explored_symbols, tool_calls) = {
            let log = self.session_log.lock().unwrap();
            (
                log.explored_files.clone(),
                log.explored_symbols.clone(),
                log.tool_calls,
            )
        };
        if explored_files.is_empty() && explored_symbols.is_empty() {
            return false;
        }

        let boosts = compute_proximity_boosts(conn, &explored_files, &explored_symbols, tool_calls);
        if boosts.is_empty() {
            return false;
        }

        let mut adjusted = false;
        for r in results.iter_mut() {
            if let Some(&boost) = boosts.get(&r.path) {
                r.score += weight * boost;
                adjusted = true;
            }
        }
        if adjusted {
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        adjusted
    }

    /// A handle the background indexer uses to advance the phase as it works.
    pub fn phase_handle(&self) -> Arc<RwLock<IndexingPhase>> {
        Arc::clone(&self.phase)
    }

    /// A handle the background indexer uses to publish an error message
    /// when `phase` transitions to `Failed` (see `IndexingPhase::Failed`).
    pub fn last_index_error_handle(&self) -> Arc<RwLock<Option<String>>> {
        Arc::clone(&self.last_index_error)
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

    /// The loaded embedder, if semantic search is ready.
    pub(crate) fn embedder(&self) -> Option<Arc<Embedder>> {
        self.embedder.read().unwrap().clone()
    }

    pub(crate) fn filter_sn(&self, sn: Option<SuggestedNext>) -> Option<SuggestedNext> {
        filter_suggested_next(sn, &self.preset)
    }

    pub(crate) fn embed_status_str(&self) -> String {
        self.embed_status.read().unwrap().as_str().to_string()
    }

    /// Re-runs the embedding bootstrap in the background when it previously
    /// failed (model load, vector-table creation, or embedding all set status
    /// to `Failed`) or was blocked by offline policy (`OfflineUnavailable` —
    /// e.g. the caller since flipped `semantic_search.allow_network_fallback`
    /// to `true` or ran `git lfs pull` and wants to try again). No-op for any
    /// other status: `Ready`/`Embedding`/`Downloading` are already done or in
    /// flight, and `Disabled` means semantic search isn't turned on in
    /// config. Opens its own DB connection so the retry doesn't hold the
    /// shared connection mutex for its duration.
    pub(crate) fn retry_embeddings_if_failed(&self) {
        // Claim the retry synchronously (Failed/OfflineUnavailable ->
        // Downloading) so two overlapping `retry_embeddings` requests can't
        // both spawn a bootstrap.
        {
            let mut status = self.embed_status.write().unwrap();
            if *status != EmbedStatus::Failed && *status != EmbedStatus::OfflineUnavailable {
                return;
            }
            *status = EmbedStatus::Downloading;
        }
        let semantic = calm_core::config::load_config(&self.project_root)
            .unwrap_or_default()
            .semantic_search;
        let db_path = self.db_path.clone();
        let embedder = Arc::clone(&self.embedder);
        let status = Arc::clone(&self.embed_status);
        std::thread::spawn(move || {
            // Catches a panic inside the bootstrap so a bug there (or in a
            // future change to it) can't leave `status` stuck on
            // `Downloading` forever with no thread left to ever flip it —
            // the discarded `JoinHandle` means nothing else would notice.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match calm_core::db::conn::open_writer(&db_path) {
                    Ok(conn) => crate::bootstrap_embeddings(&conn, &semantic, &embedder, &status),
                    Err(e) => {
                        tracing::error!("Embeddings retry: failed to open DB: {e}");
                        *status.write().unwrap() = EmbedStatus::Failed;
                    }
                }
            }));
            if outcome.is_err() {
                tracing::error!("Embeddings retry thread panicked");
                *status.write().unwrap() = EmbedStatus::Failed;
            }
        });
    }
    pub(crate) fn current_phase(&self) -> IndexingPhase {
        *self.phase.read().unwrap()
    }

    /// Canonical `indexing_phase` string for tool responses.
    pub(crate) fn phase_str(&self) -> String {
        self.current_phase().as_str().to_string()
    }

    /// `edges_ready` is true only once the full graph is built.
    pub(crate) fn edges_ready(&self) -> bool {
        self.current_phase() == IndexingPhase::Ready
    }
}

// ---------------------------------------------------------------------------
// Shared output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct SuggestedNext {
    pub(crate) tool: String,
    pub(crate) reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) args: Option<serde_json::Value>,
}

pub(crate) fn suggested(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
    })
}

pub(crate) fn suggested_with_args(
    tool: &str,
    reason: &str,
    args: serde_json::Value,
) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: Some(args),
    })
}

// ---------------------------------------------------------------------------
// Tool Presets — selective tool set definitions
// ---------------------------------------------------------------------------

pub(crate) fn preset_tools(preset: &str) -> Option<&'static [&'static str]> {
    match preset {
        "orient" => Some(&[
            "repo_overview",
            "locate",
            "dependencies",
            "hotspots",
            "fitness_report",
            "indexing_status",
        ]),
        "trace" => Some(&[
            "repo_overview",
            "search",
            "locate",
            "symbol_info",
            "source",
            "callers",
            "callees",
            "path",
            "dependencies",
            "indexing_status",
        ]),
        "edit" => Some(&[
            "repo_overview",
            "search",
            "locate",
            "symbol_info",
            "source",
            "callers",
            "callees",
            "edit_context",
            "edit_lines",
            "edit_symbol",
            "diff_impact",
            "indexing_status",
        ]),
        "compound" => Some(&[
            "repo_overview",
            "locate",
            "hotspots",
            "fitness_report",
            "source",
            "understand",
            "edit_context",
            "diff_impact",
            "session_context",
            "indexing_status",
            "remember",
            "recall",
        ]),
        "full" | "" => None, // None = all tools, no filtering
        _ => None,
    }
}

pub(crate) fn is_tool_available(preset: &str, tool: &str) -> bool {
    match preset_tools(preset) {
        None => true,
        Some(tools) => tools.contains(&tool),
    }
}

pub(crate) fn filter_suggested_next(
    sn: Option<SuggestedNext>,
    preset: &str,
) -> Option<SuggestedNext> {
    match &sn {
        Some(s) if !is_tool_available(preset, &s.tool) => None,
        _ => sn,
    }
}

pub(crate) fn not_found_json(symbol: &str) -> String {
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
pub(crate) struct AmbiguousCandidate {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) class_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) caller_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AmbiguousResult {
    pub(crate) ambiguous: bool,
    /// Total candidates matched before the display cap of
    /// `MAX_AMBIGUOUS_CANDIDATES`. `truncated` is `true` when `total >
    /// candidates.len()`, telling the caller there are more matches than
    /// shown and to narrow with `path`/`line` — the list is never silently
    /// presented as the complete set.
    pub(crate) total: usize,
    pub(crate) truncated: bool,
    pub(crate) candidates: Vec<AmbiguousCandidate>,
}

/// One `symbols` row matched by a bare-name (+ optional path) lookup.
/// Carries enough columns to populate either a concrete tool output (e.g.
/// `SymbolInfoOutput`) or an `AmbiguousCandidate` when the lookup turns out
/// to match more than one row.
pub(crate) struct CandidateRow {
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    pub(crate) signature: String,
    pub(crate) docstring: String,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    pub(crate) language: String,
    pub(crate) class_context: Option<String>,
    pub(crate) is_entry_point: bool,
    pub(crate) is_test: bool,
    pub(crate) coreness: Option<i64>, // from symbols.coreness column
}

impl CandidateRow {
    pub(crate) fn to_symbol_info(&self) -> SymbolInfoOutput {
        SymbolInfoOutput {
            name: self.name.clone(),
            qualified_name: self.qualified_name.clone(),
            kind: self.kind.clone(),
            path: self.path.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            // Extracted verbatim from source at index time — a default
            // parameter value or doc-comment example can embed a real secret,
            // so this must be redacted the same as `source()`'s body text.
            signature: Some(calm_core::sanitize::sanitize_source_output(&self.signature))
                .filter(|s| !s.is_empty()),
            docstring: Some(calm_core::sanitize::sanitize_source_output(&self.docstring))
                .filter(|s| !s.is_empty()),
            caller_count: self.caller_count,
            is_hub: self.is_hub,
            coreness: None, // set by handler based on edges_ready
            health: None,
            suggested_next: None,
        }
    }

    pub(crate) fn to_ambiguous_candidate(&self) -> AmbiguousCandidate {
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
pub(crate) fn resolve_symbol_candidates(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
) -> Vec<CandidateRow> {
    let sql = if path.is_some() {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness
         FROM symbols WHERE name = ?1 AND path = ?2 ORDER BY path, line_start"
    } else {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness
         FROM symbols WHERE name = ?1 ORDER BY path, line_start"
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
            is_test: row.get::<_, i64>(13)? != 0,
            coreness: row.get(14)?,
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

pub(crate) enum SymbolResolution {
    NotFound,
    Ambiguous(Vec<CandidateRow>),
    Found(Box<CandidateRow>),
}

/// Resolve a bare symbol name (+ optional path, + optional disambiguating
/// `line`) to exactly one row. `path` narrows the candidate set (see
/// `resolve_symbol_candidates`) but does not by itself guarantee a unique
/// match — `name` + `path` is not a DB-enforced unique key (only
/// `qualified_name` is), so e.g. two same-named functions in the same file
/// (a common shape in this codebase: `#[cfg(feature = "x")]` real impl vs.
/// `#[cfg(not(feature = "x"))]` stub, both named identically) still resolve
/// as ambiguous even with `path` set. `line` breaks that tie: when given, it
/// narrows to whichever candidate's `[line_start, line_end]` contains it —
/// exactly the range every `Ambiguous` response already echoes back per
/// candidate, so a caller that got `ambiguous: true` can retry once with
/// the `line_start` of the one it meant. A `line` that matches none of the
/// candidates is ignored (falls back to the unnarrowed set) rather than
/// forcing `NotFound` — a stale/wrong hint should degrade to the old
/// behavior, not make an otherwise-resolvable symbol disappear.
pub(crate) fn resolve_symbol(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
    line: Option<i64>,
) -> SymbolResolution {
    let mut candidates = resolve_symbol_candidates(conn, name, path);
    if let Some(line) = line {
        let in_range = |c: &CandidateRow| c.line_start <= line && line <= c.line_end;
        if candidates.iter().any(in_range) {
            candidates.retain(in_range);
        }
    }
    if candidates.is_empty() {
        SymbolResolution::NotFound
    } else if candidates.len() == 1 {
        SymbolResolution::Found(Box::new(candidates.remove(0)))
    } else {
        SymbolResolution::Ambiguous(candidates)
    }
}

pub(crate) fn ambiguous_json(candidates: &[CandidateRow]) -> String {
    let total = candidates.len();
    let shown = candidates
        .iter()
        .take(MAX_AMBIGUOUS_CANDIDATES)
        .map(CandidateRow::to_ambiguous_candidate)
        .collect();
    serde_json::to_string_pretty(&AmbiguousResult {
        ambiguous: true,
        total,
        truncated: total > MAX_AMBIGUOUS_CANDIDATES,
        candidates: shown,
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Frontier computation helper (for session_context)
// ---------------------------------------------------------------------------

/// Runs `{sql_prefix} (?, ?, ...) AND from_path IS NOT NULL` in chunks of ≤999
/// to stay within SQLite's SQLITE_LIMIT_VARIABLE_NUMBER, accumulating distinct
/// `from_path` values into `out`.
pub(crate) fn query_paths_chunked(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    params: &[String],
    out: &mut std::collections::HashSet<String>,
) {
    const CHUNK: usize = 999;
    for chunk in params.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("{sql_prefix} ({placeholders}) AND from_path IS NOT NULL");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let _ = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    row.get::<_, String>(0)
                })
                .map(|rows| {
                    for r in rows.flatten() {
                        out.insert(r);
                    }
                });
        }
    }
}

// ---------------------------------------------------------------------------
// Personalization boost helper (for search/locate)
// ---------------------------------------------------------------------------

/// Runs `{sql_prefix} (?, ?, ...){sql_suffix}` in chunks of ≤999 to stay
/// within SQLite's SQLITE_LIMIT_VARIABLE_NUMBER, accumulating `(a, b)` row
/// pairs — the two-column counterpart to `query_paths_chunked` above, needed
/// here because `compute_proximity_boosts` must know *which* explored anchor
/// a candidate connects to (to look up that anchor's own recency), not just
/// whether one exists.
fn query_pairs_chunked(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    sql_suffix: &str,
    params: &[String],
    out: &mut Vec<(String, String)>,
) {
    const CHUNK: usize = 999;
    for chunk in params.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("{sql_prefix} ({placeholders}){sql_suffix}");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let _ = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map(|rows| {
                    for r in rows.flatten() {
                        out.push(r);
                    }
                });
        }
    }
}

/// Same as `query_pairs_chunked` but the second column (`call_edges.from_path`/
/// `to_path`) is nullable — a call edge's enclosing file isn't always known.
fn query_symbol_path_pairs_chunked(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    params: &[String],
    out: &mut Vec<(String, Option<String>)>,
) {
    const CHUNK: usize = 999;
    for chunk in params.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("{sql_prefix} ({placeholders})");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let _ = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map(|rows| {
                    for r in rows.flatten() {
                        out.push(r);
                    }
                });
        }
    }
}

/// Per-path proximity boost in `(0.0, 1.0]`, derived from this session's
/// explored files/symbols: a candidate path gets the *best* (most recent)
/// connection found among —
/// - files adjacent to an explored file via `import_edges`, either direction
/// - files containing a caller of an explored symbol, via `call_edges`
///
/// Weight decays with `now - last_touch` (in tool-calls, not wall-clock) via
/// `1.0 / (1.0 + distance)`, so a file explored on the immediately preceding
/// call outweighs one from 20 calls ago. Paths with no connection at all are
/// simply absent from the result (implicit boost 0), not zero-valued
/// entries — callers should use `.get(path)` and treat a miss as no boost.
fn compute_proximity_boosts(
    conn: &rusqlite::Connection,
    explored_files: &std::collections::HashMap<String, u64>,
    explored_symbols: &std::collections::HashMap<String, u64>,
    now: u64,
) -> std::collections::HashMap<String, f64> {
    let mut boosts: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let decay = |touch: u64| 1.0 / (1.0 + now.saturating_sub(touch) as f64);
    let bump = |boosts: &mut std::collections::HashMap<String, f64>, path: String, w: f64| {
        let entry = boosts.entry(path).or_insert(0.0);
        if w > *entry {
            *entry = w;
        }
    };

    if !explored_files.is_empty() {
        let anchors: Vec<String> = explored_files.keys().cloned().collect();

        // Files that import an explored file.
        let mut importers = Vec::new();
        query_pairs_chunked(
            conn,
            "SELECT from_path, to_path FROM import_edges WHERE to_path IN",
            " AND from_path IS NOT NULL",
            &anchors,
            &mut importers,
        );
        for (from_path, to_path) in &importers {
            if let Some(&touch) = explored_files.get(to_path) {
                bump(&mut boosts, from_path.clone(), decay(touch));
            }
        }

        // Files an explored file imports.
        let mut imported = Vec::new();
        query_pairs_chunked(
            conn,
            "SELECT from_path, to_path FROM import_edges WHERE from_path IN",
            " AND to_path IS NOT NULL",
            &anchors,
            &mut imported,
        );
        for (from_path, to_path) in &imported {
            if let Some(&touch) = explored_files.get(from_path) {
                bump(&mut boosts, to_path.clone(), decay(touch));
            }
        }
    }

    if !explored_symbols.is_empty() {
        let anchors: Vec<String> = explored_symbols.keys().cloned().collect();

        // Files containing a caller of an explored symbol.
        let mut callers = Vec::new();
        query_symbol_path_pairs_chunked(
            conn,
            "SELECT to_symbol, from_path FROM call_edges WHERE to_symbol IN",
            &anchors,
            &mut callers,
        );
        for (symbol, from_path) in &callers {
            if let (Some(&touch), Some(path)) = (explored_symbols.get(symbol), from_path) {
                bump(&mut boosts, path.clone(), decay(touch));
            }
        }
    }

    boosts
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SymbolInfoOutput {
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) docstring: Option<String>,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    pub(crate) coreness: Option<i64>, // null when edges not yet built; 0 = isolated; >0 = k-core depth
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) health: Option<HealthOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CallerEntry {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) edge_confidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CalleeEntry {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) edge_confidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TransitiveEntry {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) depth: i64,
    pub(crate) edge_confidence: String,
}

#[derive(Clone, Copy)]
pub(crate) enum EdgeDirection {
    Callers,
    Callees,
}

/// BFS over `call_edges` beyond the direct neighbors, shared by `callers` and
/// `callees` when `transitive: true`. Bounded by `max_depth` and a wall-clock
/// timeout so a hub symbol can't blow up the response. Returns `(entries,
/// capped)` — `capped` is true when the BFS stopped early (depth limit hit
/// with a non-empty frontier remaining, or the timeout fired) rather than
/// because there was nothing left to explore.
pub(crate) fn transitive_bfs(
    conn: &rusqlite::Connection,
    start_qualified_name: &str,
    direction: EdgeDirection,
    max_depth: usize,
    timeout_ms: u64,
) -> (Vec<TransitiveEntry>, bool) {
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
        Err(_) => return (vec![], false),
    };

    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(timeout_ms);

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(start_qualified_name.to_string());
    let mut frontier = vec![start_qualified_name.to_string()];
    let mut results = Vec::new();
    let mut depth = 0usize;
    let mut capped = false;

    while depth < max_depth && !frontier.is_empty() {
        if start.elapsed() > deadline {
            capped = true;
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
        if !capped && depth >= max_depth && !next_frontier.is_empty() {
            capped = true;
        }
        frontier = next_frontier;
    }

    (results, capped)
}

/// Shared caller-count risk tiering used by `edit_context`, `diff_impact`,
/// and `edit_lines`/`edit_symbol`'s risk gate — previously three independent
/// copies of the same `>10`/`>3` thresholds had drifted apart as separate
/// inline `if`/`else` chains. Centralized here so all three read the same
/// policy and can't silently diverge again.
pub(crate) fn risk_level_from_caller_count(caller_count: i64) -> &'static str {
    if caller_count > 10 {
        "high"
    } else if caller_count > 3 {
        "medium"
    } else {
        "low"
    }
}

const CALL_SITE_PREVIEW_MAX_CHARS: usize = 160;

/// Read the trimmed source line at `line` (1-indexed) from `project_root/path`
/// for a `CallerEntry`/`CalleeEntry` preview. Best-effort: missing files, a
/// line number past EOF, or a `None` line all just yield `None` rather than
/// an error — a preview is a convenience, not load-bearing.
pub(crate) fn line_preview(
    project_root: &std::path::Path,
    path: &str,
    line: Option<i64>,
) -> Option<String> {
    let line = line?;
    if line < 1 {
        return None;
    }
    let content = std::fs::read_to_string(project_root.join(path)).ok()?;
    let raw = content.lines().nth((line - 1) as usize)?.trim();
    if raw.is_empty() {
        return None;
    }
    // Raw disk content, never indexed/DB-stored — redact here directly, same
    // as `source()`'s body text, since this shared helper is the only point
    // every caller's preview ever passes through.
    let sanitized = calm_core::sanitize::sanitize_source_output(raw);
    if sanitized.chars().count() > CALL_SITE_PREVIEW_MAX_CHARS {
        Some(format!(
            "{}…",
            sanitized
                .chars()
                .take(CALL_SITE_PREVIEW_MAX_CHARS)
                .collect::<String>()
        ))
    } else {
        Some(sanitized)
    }
}

// ---------------------------------------------------------------------------
// Tool 8: dependencies
// ---------------------------------------------------------------------------

#[cfg(test)]
mod personalization_tests {
    use super::*;
    use std::collections::HashMap;

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        calm_core::db::schema::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn empty_explored_state_yields_no_boosts() {
        let conn = test_conn();
        let boosts = compute_proximity_boosts(&conn, &HashMap::new(), &HashMap::new(), 5);
        assert!(boosts.is_empty());
    }

    #[test]
    fn boosts_file_that_imports_an_explored_file() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('a.rs', 'b.rs', 'b')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("b.rs".to_string(), 3u64); // touched at tool-call 3

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 4);
        // now(4) - touch(3) = 1 -> decay = 1/(1+1) = 0.5
        assert_eq!(boosts.get("a.rs"), Some(&0.5));
    }

    #[test]
    fn boosts_file_an_explored_file_imports_too() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('a.rs', 'b.rs', 'b')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("a.rs".to_string(), 3u64);

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 4);
        assert_eq!(boosts.get("b.rs"), Some(&0.5));
    }

    #[test]
    fn more_recent_touch_decays_less_than_older_touch() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('recent.rs', 'anchor.rs', 'anchor')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('stale.rs', 'old_anchor.rs', 'old_anchor')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("anchor.rs".to_string(), 9u64); // touched 1 call ago
        explored_files.insert("old_anchor.rs".to_string(), 0u64); // touched 10 calls ago

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 10);
        let recent = boosts["recent.rs"];
        let stale = boosts["stale.rs"];
        assert!(
            recent > stale,
            "recently-touched anchor must decay less: recent={recent} stale={stale}"
        );
    }

    #[test]
    fn boosts_file_containing_caller_of_an_explored_symbol() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, from_path, to_path) \
             VALUES ('caller_fn', 'target_fn', 'caller_file.rs', 'target_file.rs')",
            [],
        )
        .unwrap();
        let mut explored_symbols = HashMap::new();
        explored_symbols.insert("target_fn".to_string(), 2u64);

        let boosts = compute_proximity_boosts(&conn, &HashMap::new(), &explored_symbols, 2);
        // now(2) - touch(2) = 0 -> decay = 1/(1+0) = 1.0 (just-touched anchor)
        assert_eq!(boosts.get("caller_file.rs"), Some(&1.0));
    }

    #[test]
    fn takes_the_best_boost_when_multiple_anchors_connect_to_the_same_path() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('shared.rs', 'old.rs', 'old')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('shared.rs', 'fresh.rs', 'fresh')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("old.rs".to_string(), 0u64);
        explored_files.insert("fresh.rs".to_string(), 5u64);

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 5);
        // Must take the fresher connection's weight (1.0), not the older one's (1/6).
        assert_eq!(boosts.get("shared.rs"), Some(&1.0));
    }
}
