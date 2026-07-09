use super::common::*;
use super::*;

impl CalmServer {
    #[tool(
        name = "search",
        description = "USE THIS INSTEAD OF native grep, text search, or file browsing tools. USE WHEN: you don't have an exact file path and line number. kind=hybrid has highest recall. NOT FOR: inspecting a file you already have (use file_overview). vs locate: search returns a result list; locate returns search + symbol metadata in one call."
    )]
    pub(crate) fn search(&self, #[tool(aggr)] p: SearchParams) -> String {
        self.timed_tool("search", || {
            if p.kind == "grep" {
                return self.search_grep_impl(&p);
            }
            if p.kind == "similar" {
                return self.search_similar_impl(&p);
            }
            let kind = match p.kind.as_str() {
                "symbol" => calm_core::types::SearchKind::Symbol,
                "text" => calm_core::types::SearchKind::Text,
                "file" => calm_core::types::SearchKind::File,
                "semantic" => calm_core::types::SearchKind::Semantic,
                "hybrid" => calm_core::types::SearchKind::Hybrid,
                _ => calm_core::types::SearchKind::Symbol,
            };

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let rrf_k = calm_core::config::load_config(&self.project_root)
                .map(|c| c.search.rrf_k as f64)
                .unwrap_or(calm_core::search::DEFAULT_RRF_K);
            let kind_str = p.kind.as_str();
            match calm_core::search::search(
                &conn,
                &p.query,
                kind,
                search_fetch_limit(&p),
                self.embedder().as_deref(),
                rrf_k,
            ) {
                Ok(mut output) => {
                    let filter_truncated =
                        apply_include_tests_filter(&mut output.results, p.limit, p.include_tests);
                    let personalized = self.apply_personalization_boost(&conn, &mut output.results);
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
                            snippet: r.snippet,
                        })
                        .collect();
                    // Fallback chain is a DAG, not a cycle: symbol/file -> hybrid;
                    // semantic -> text; hybrid -> text (or straight to grep when
                    // degraded — see below); text -> grep. grep itself (handled
                    // in search_grep_impl) is the terminal node: it reads raw file
                    // content directly, so it's the only kind that can find
                    // module-level const/static that isn't extracted as a symbol
                    // in any Tier-0 language (JS/TS included — resolve_name_node
                    // only resolves `const`/`let` bindings whose value is itself a
                    // function, not plain data constants).
                    let sn = if !results.is_empty() && kind_str == "symbol" {
                        suggested_with_args("locate", "Full context in 1 call (replaces symbol_info)", serde_json::json!({"query": results[0].name, "kind": "symbol"}))
                    } else if results.is_empty() && kind_str == "semantic" {
                        suggested_with_args("search", "Semantic index may not cover this — try text or hybrid search", serde_json::json!({"kind": "text"}))
                    } else if results.is_empty() && kind_str == "hybrid" && output.degraded {
                        // Degraded hybrid's FTS component is search_symbol, which
                        // matches name+docstring+signature with no column filter —
                        // a strict superset of search_text's {docstring signature}
                        // filtered match set. If this came up empty, text is
                        // guaranteed empty too, so skip straight to grep.
                        suggested_with_args("search", "Embeddings inactive and hybrid (name/docstring/signature FTS) found nothing — text search can't find more (its match set is a subset of hybrid's). Try grep over raw file content, which also covers symbols never extracted at all (e.g. module-level const/static)", serde_json::json!({"kind": "grep"}))
                    } else if results.is_empty() && kind_str == "hybrid" {
                        suggested_with_args("search", "Try exact text search", serde_json::json!({"kind": "text"}))
                    } else if results.is_empty() && kind_str == "text" {
                        suggested_with_args("search", "Text/symbol index may not cover this — module-level const/static isn't extracted as a symbol in any Tier-0 language, so it's invisible to symbol/text/hybrid search. Try grep over raw file content", serde_json::json!({"kind": "grep"}))
                    } else if results.is_empty() {
                        // symbol, file, or any other kind
                        suggested_with_args("search", "Try hybrid for broader recall", serde_json::json!({"kind": "hybrid"}))
                    } else {
                        None
                    };
                    serde_json::to_string_pretty(&SearchOutput {
                        results,
                        truncated: output.truncated || filter_truncated,
                        degraded: output.degraded,
                        note: output.note,
                        personalized,
                        suggested_next: self.filter_sn(sn),
                    })
                    .unwrap_or_default()
                }
                Err(e) => serde_json::to_string_pretty(&SearchOutput {
                    results: vec![],
                    truncated: false,
                    degraded: true,
                    note: Some(format!("Search error: {e}")),
                    personalized: false,
                    suggested_next: None,
                })
                .unwrap_or_default(),
            }
        })
    }

    /// `search(kind="grep")` implementation — kept separate from the
    /// DB-backed `calm_core::search::search` dispatch because it needs
    /// `project_root` and walks the filesystem directly instead of
    /// querying the index. See `calm_core::search::search_grep`.
    fn search_grep_impl(&self, p: &SearchParams) -> String {
        let conn = match self.make_read_conn() {
            Ok(c) => c,
            Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
        };
        let ignore_patterns = calm_core::config::load_config(&self.project_root)
            .map(|c| c.ignore)
            .unwrap_or_default();
        match calm_core::search::search_grep(
            &conn,
            &self.project_root,
            &p.query,
            p.glob.as_deref(),
            p.case_insensitive,
            p.context,
            &ignore_patterns,
            search_fetch_limit(p),
        ) {
            Ok(mut output) => {
                let filter_truncated =
                    apply_include_tests_filter(&mut output.results, p.limit, p.include_tests);
                let personalized = self.apply_personalization_boost(&conn, &mut output.results);
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
                        snippet: r.snippet,
                    })
                    .collect();
                // grep is the terminal node of the fallback chain: it reads raw
                // file content directly, so it's strictly broader than
                // symbol/text/hybrid (which all depend on a `symbols` row
                // existing). Looping back to "try hybrid" here would just
                // re-enter the hybrid -> text -> grep chain that led to this
                // call in the first place. If grep found nothing, no other
                // search kind will either — surface that via `note` instead.
                let note = output.note.or_else(|| {
                    results.is_empty().then(|| {
                        "No matches via grep either (broadest search — raw regex over disk content). The term likely doesn't exist verbatim in this repo, or check spelling/case/glob".to_string()
                    })
                });
                serde_json::to_string_pretty(&SearchOutput {
                    results,
                    truncated: output.truncated || filter_truncated,
                    degraded: output.degraded,
                    note,
                    personalized,
                    suggested_next: self.filter_sn(None),
                })
                .unwrap_or_default()
            }
            Err(e) => serde_json::to_string_pretty(&SearchOutput {
                results: vec![],
                truncated: false,
                degraded: true,
                note: Some(format!("grep search error: {e}")),
                personalized: false,
                suggested_next: None,
            })
            .unwrap_or_default(),
        }
    }

    /// `kind="similar"` path: vector-similarity KNN anchored at `p.path`+
    /// `p.line` instead of embedding `p.query` (which is ignored here).
    fn search_similar_impl(&self, p: &SearchParams) -> String {
        let (Some(path), Some(line)) = (p.path.as_deref(), p.line) else {
            return serde_json::to_string_pretty(&SearchOutput {
                results: vec![],
                truncated: false,
                degraded: true,
                note: Some("kind=\"similar\" requires both `path` and `line`".to_string()),
                personalized: false,
                suggested_next: None,
            })
            .unwrap_or_default();
        };
        let conn = match self.make_read_conn() {
            Ok(c) => c,
            Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
        };
        match calm_core::search::search_similar(&conn, path, line, search_fetch_limit(p)) {
            Ok(mut output) => {
                let filter_truncated =
                    apply_include_tests_filter(&mut output.results, p.limit, p.include_tests);
                let personalized = self.apply_personalization_boost(&conn, &mut output.results);
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
                        snippet: r.snippet,
                    })
                    .collect();
                let note = output.note.or_else(|| {
                    results.is_empty().then(|| {
                        "No similar code found elsewhere in the index — this may be genuinely unique code, or the embeddings index is still building".to_string()
                    })
                });
                serde_json::to_string_pretty(&SearchOutput {
                    results,
                    truncated: output.truncated || filter_truncated,
                    degraded: output.degraded,
                    note,
                    personalized,
                    suggested_next: self.filter_sn(None),
                })
                .unwrap_or_default()
            }
            Err(e) => serde_json::to_string_pretty(&SearchOutput {
                results: vec![],
                truncated: false,
                degraded: true,
                note: Some(format!("similar search error: {e}")),
                personalized: false,
                suggested_next: None,
            })
            .unwrap_or_default(),
        }
    }
    #[tool(
        name = "locate",
        description = "Compound: search + file_overview + symbol_info in 1 call (66% reduction). USE INSTEAD OF calling search then file_overview then symbol_info separately. NOT FOR: reading source (use source after locate), pre-edit (use edit_context)."
    )]
    pub(crate) fn locate(&self, #[tool(aggr)] p: LocateParams) -> String {
        self.timed_tool("locate", || {
            let kind_str = p.kind.as_deref().unwrap_or("symbol");
            let kind = match kind_str {
                "text" => calm_core::types::SearchKind::Text,
                "file" => calm_core::types::SearchKind::File,
                "semantic" => calm_core::types::SearchKind::Semantic,
                "hybrid" => calm_core::types::SearchKind::Hybrid,
                _ => calm_core::types::SearchKind::Symbol,
            };
            let limit = p.limit.unwrap_or(10);

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let rrf_k = calm_core::config::load_config(&self.project_root)
                .map(|c| c.search.rrf_k as f64)
                .unwrap_or(calm_core::search::DEFAULT_RRF_K);
            let mut search_output = match calm_core::search::search(
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
            let personalized =
                self.apply_personalization_boost(&conn, &mut search_output.results);

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
                    snippet: r.snippet.clone(),
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
                                    // Verbatim source text at index time — redact
                                    // the same as `source()`'s body (see common.rs's
                                    // `to_symbol_info`, which this mirrors).
                                    signature: row
                                        .get::<_, String>(6)
                                        .ok()
                                        .map(|s| calm_core::sanitize::sanitize_source_output(&s))
                                        .filter(|s| !s.is_empty()),
                                    docstring: row
                                        .get::<_, String>(7)
                                        .ok()
                                        .map(|s| calm_core::sanitize::sanitize_source_output(&s))
                                        .filter(|s| !s.is_empty()),
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
                    build_file_overview(&conn, &t.path, Some(LOCATE_FILE_OVERVIEW_SYMBOL_CAP))
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

            // Same acyclic fallback chain as search()'s sn logic (see there for
            // the rationale): don't just say "try hybrid" regardless of what
            // kind was already tried — that's how kind=hybrid empty results
            // used to loop back to suggesting hybrid again.
            let sn = if let Some(sym) = top_symbol.as_ref() {
                if sym.is_hub {
                    suggested_with_args("edit_context", "Hub detected — mandatory pre-edit check", serde_json::json!({"symbol": sym.name, "path": sym.path}))
                } else if sym.caller_count == 0 {
                    suggested_with_args("callers", "No callers found — verify dead code before deleting", serde_json::json!({"symbol": sym.name}))
                } else {
                    suggested_with_args("source", "Read implementation", serde_json::json!({"target": results[0].name}))
                }
            } else if results.is_empty() && kind_str == "hybrid" && search_output.degraded {
                suggested_with_args("search", "Embeddings inactive and hybrid found nothing — try grep over raw file content (also covers symbols never extracted at all, e.g. module-level const/static)", serde_json::json!({"kind": "grep"}))
            } else if results.is_empty() && kind_str == "hybrid" {
                suggested_with_args("search", "Try exact text search", serde_json::json!({"kind": "text"}))
            } else if results.is_empty() && kind_str == "text" {
                suggested_with_args("search", "Text/symbol index may not cover this — module-level const/static isn't extracted as a symbol in any Tier-0 language. Try grep over raw file content", serde_json::json!({"kind": "grep"}))
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
                personalized,
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
            let mut out = build_file_overview(&conn, &p.path, None);

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
    /// Free-text query. Required for every `kind` except `"similar"`, where
    /// it's ignored — pass `path`+`line` instead (an anchor location has no
    /// text query to embed; its own stored chunk vector is reused as-is).
    pub(crate) query: String,
    /// One of `"symbol"` (default, name/signature match), `"text"` (FTS
    /// over code body), `"file"` (path match — glob syntax like `*.md` or
    /// `src/**` also works, otherwise plain substring), `"grep"` (real
    /// regex over raw file content read from disk, including files the
    /// indexer never parses — Cargo.toml, docs/*.md, etc.; see `glob`/
    /// `case_insensitive`/`context`), `"semantic"` (embedding KNN — needs
    /// the `embeddings` feature and a ready index), `"hybrid"` (RRF
    /// fusion of text + symbol-identity + code-chunk vectors), or
    /// `"similar"` (embedding KNN anchored at `path`+`line` instead of a
    /// text query — "find code that looks like *this location*", not "find
    /// code matching these words"; needs `embeddings` and `path`+`line`).
    /// Any other value silently falls back to `"symbol"`.
    #[serde(default = "default_symbol")]
    pub(crate) kind: String,
    /// Max results to return. Default 10.
    #[serde(default = "default_limit")]
    pub(crate) limit: usize,
    /// `kind="grep"` only: `query` is treated as a regex pattern by default
    /// — set this to a glob (e.g. `"*.rs"`, `"docs/**"`) to restrict which
    /// files are scanned.
    pub(crate) glob: Option<String>,
    /// `kind="grep"` only: case-insensitive regex match. Default `false`.
    #[serde(default)]
    pub(crate) case_insensitive: bool,
    /// `kind="grep"` only: number of context lines shown before/after each
    /// match in the returned snippet. Default 0.
    #[serde(default)]
    pub(crate) context: usize,
    /// `kind="similar"` only, required: repo-relative path of the anchor
    /// location whose code you want to find elsewhere in the repo.
    pub(crate) path: Option<String>,
    /// `kind="similar"` only, required: 1-indexed line within `path` that
    /// selects the anchor chunk (the smallest indexed chunk spanning this
    /// line).
    pub(crate) line: Option<i64>,
    /// `false` to hard-exclude test code (`is_test`) from results. Default
    /// `true` (current behavior, unchanged): tests are only soft-penalized
    /// via `NOISE_PENALTY`, which a descriptively-named test that closely
    /// paraphrases the query can still outrank the real implementation
    /// against — pass `false` when the query has nothing to do with tests
    /// to exclude them outright instead of just down-ranking them. May
    /// return fewer than `limit` results if enough hits were tests (a
    /// modest overfetch covers most cases but isn't retried further).
    #[serde(default = "default_include_tests")]
    pub(crate) include_tests: bool,
}

pub(crate) fn default_symbol() -> String {
    "symbol".into()
}

pub(crate) fn default_limit() -> usize {
    10
}

pub(crate) fn default_include_tests() -> bool {
    true
}

/// Internal fetch size passed to the underlying `calm_core` search —
/// deliberately larger than `p.limit` when `include_tests` is `false`, so
/// `apply_include_tests_filter` below has enough of a pool to drop test
/// hits from without immediately starving the response. Not a guarantee:
/// a file with unusually many tests can still under-fill `limit`.
pub(crate) fn search_fetch_limit(p: &SearchParams) -> usize {
    if p.include_tests {
        p.limit
    } else {
        p.limit.saturating_mul(2).max(p.limit + 5)
    }
}

/// Hard-excludes `is_test` rows when `include_tests` is `false` (vs
/// `NOISE_PENALTY`'s soft score demotion inside `rrf_merge_n`, which a
/// descriptively-named test that closely paraphrases the query can still
/// outrank the real implementation against), then truncates back down to
/// `limit`. Returns `true` if the filter itself dropped enough rows that
/// truncation now reflects the filter rather than (or in addition to) the
/// underlying search's own limit.
pub(crate) fn apply_include_tests_filter(
    results: &mut Vec<calm_core::search::SearchResult>,
    limit: usize,
    include_tests: bool,
) -> bool {
    if !include_tests {
        results.retain(|r| !r.is_test);
    }
    let filter_truncated = results.len() > limit;
    results.truncate(limit);
    filter_truncated
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
    /// `kind="grep"` only: the matched line with `context` lines of
    /// surrounding text, 1-indexed and marked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snippet: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SearchOutput {
    pub(crate) results: Vec<SearchResultItem>,
    pub(crate) truncated: bool,
    pub(crate) degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    /// `true` when results were re-ranked toward this session's explored
    /// files/symbols (see `CalmServer::apply_personalization_boost`)
    /// — `false` for a cold session (nothing explored yet) or when
    /// `search.personalization_weight` is configured to `0.0`.
    pub(crate) personalized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 3: file_overview
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct FileOverviewParams {
    /// Repo-relative path, e.g. `crates/calm-core/src/embedding.rs`.
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
    /// `true` when `symbols` was capped below `symbol_count` (only `locate`
    /// caps; the `file_overview` tool always returns the full list).
    pub(crate) symbols_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

/// Cap on how many symbols `locate` embeds from the top hit's `file_overview`.
/// `locate` is a compound convenience where the overview is a bonus, so a large
/// file must not flood the response; the standalone `file_overview` tool passes
/// `None` for the complete list. `symbols_truncated` flags when this trims.
const LOCATE_FILE_OVERVIEW_SYMBOL_CAP: usize = 40;

/// Shared by the `file_overview` tool and `locate` (when the top result is a
/// file match), so both surfaces build the same shape from the same query.
/// `max_symbols` caps the returned `symbols` list (setting `symbols_truncated`
/// when it trims); `None` returns every symbol in the file.
pub(crate) fn build_file_overview(
    conn: &rusqlite::Connection,
    path: &str,
    max_symbols: Option<usize>,
) -> FileOverviewOutput {
    let mut symbols: Vec<FileOverviewSymbol> = {
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
                // Verbatim source text at index time — redact the same as
                // `source()`'s body (see common.rs's `to_symbol_info`).
                signature: calm_core::sanitize::sanitize_source_output(&row.get::<_, String>(7)?),
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
    // `symbol_count` is the true file total; the visible `symbols` list may be
    // capped below it (for `locate`), in which case `symbols_truncated` is set.
    let symbols_truncated = matches!(max_symbols, Some(cap) if symbol_count > cap);
    if let Some(cap) = max_symbols {
        symbols.truncate(cap);
    }
    FileOverviewOutput {
        path: path.to_string(),
        language,
        symbols,
        symbol_count,
        symbols_truncated,
        suggested_next: None,
    }
}

// ---------------------------------------------------------------------------
// Tool 4: symbol_info
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct LocateParams {
    pub(crate) query: String,
    /// Same values as `search`'s `kind` — `"symbol"` (default), `"text"`,
    /// `"file"`, `"semantic"`, or `"hybrid"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    /// How much to enrich the top hit beyond `results`, in increasing
    /// cost/token size:
    /// - `"search_only"`: just `results` — cheapest, use when you only
    ///   need the match list (e.g. checking existence, or `kind` is
    ///   `"text"`/`"file"` where there's no symbol to enrich anyway).
    /// - `"with_file"`: adds `file_overview` (every symbol in the top
    ///   hit's file, with signatures) — can be large for a big file.
    /// - `"with_symbol"` (default): adds both `file_overview` and
    ///   `top_symbol` (full metadata for just the top hit).
    ///
    /// `kind` `"text"`/`"file"` auto-downgrades `"with_symbol"` to
    /// `"with_file"` (reported via `depth_adjusted`) since a text/file
    /// match has no single symbol to enrich.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) depth: Option<String>,
    /// Max entries in `results`. Default 10.
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
    /// See `SearchOutput::personalized`.
    pub(crate) personalized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 15: hotspots
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Regression for `locate`'s token cost: `build_file_overview` must cap the
    /// symbol list when `max_symbols` is set (the `locate` path), reporting the
    /// true `symbol_count` and flagging `symbols_truncated`; `None` (the
    /// `file_overview` tool path) returns every symbol untrimmed.
    #[test]
    fn build_file_overview_caps_symbols_only_when_requested() {
        let conn = Connection::open_in_memory().unwrap();
        calm_core::db::schema::init_db(&conn).unwrap();
        for i in 0..45 {
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES (?1, ?2, 'function', 'rust', 'big.rs', ?3, ?3, '', '', ?2, 0, 0, 0)",
                rusqlite::params![format!("big.rs::f{i}"), format!("f{i}"), (i + 1) as i64],
            )
            .unwrap();
        }

        let capped = build_file_overview(&conn, "big.rs", Some(40));
        assert_eq!(
            capped.symbol_count, 45,
            "symbol_count is the true file total"
        );
        assert_eq!(
            capped.symbols.len(),
            40,
            "visible list capped to max_symbols"
        );
        assert!(capped.symbols_truncated);

        let full = build_file_overview(&conn, "big.rs", None);
        assert_eq!(full.symbol_count, 45);
        assert_eq!(full.symbols.len(), 45, "no cap returns the full list");
        assert!(!full.symbols_truncated);
    }

    fn stub_search_result(qn: &str, is_test: bool) -> calm_core::search::SearchResult {
        calm_core::search::SearchResult {
            name: qn.into(),
            qualified_name: qn.into(),
            path: "x.rs".into(),
            kind: None,
            line_start: None,
            line_end: None,
            score: 0.0,
            match_type: "symbol".into(),
            snippet: None,
            is_test,
        }
    }

    #[test]
    fn apply_include_tests_filter_keeps_all_when_include_tests_true() {
        let mut results = vec![stub_search_result("a", false), stub_search_result("b", true)];
        let truncated = apply_include_tests_filter(&mut results, 10, true);
        assert_eq!(results.len(), 2, "include_tests=true is the current, unchanged behavior");
        assert!(!truncated);
    }

    #[test]
    fn apply_include_tests_filter_excludes_tests_when_false() {
        let mut results = vec![
            stub_search_result("a", false),
            stub_search_result("b", true),
            stub_search_result("c", false),
        ];
        let truncated = apply_include_tests_filter(&mut results, 10, false);
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|r| r.qualified_name != "b"),
            "is_test result must be hard-excluded, not just score-penalized"
        );
        assert!(!truncated, "under the limit even after filtering, not truncated");
    }

    #[test]
    fn apply_include_tests_filter_reports_truncated_when_still_over_limit() {
        let mut results = vec![
            stub_search_result("a", false),
            stub_search_result("b", false),
            stub_search_result("c", false),
        ];
        let truncated = apply_include_tests_filter(&mut results, 2, true);
        assert_eq!(results.len(), 2);
        assert!(truncated);
    }
}
