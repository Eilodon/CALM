//! Internal query-batching, graph-traversal (`transitive_bfs`), search-boost
//! (`compute_proximity_boosts`/`normalize_then_boost`), caller-count
//! classification, and line-preview helpers — extracted verbatim from
//! `tools/common.rs` (2026-07-28 hotspot split). Re-exported through `common`
//! (`pub(crate) use crate::tools::detail::*;`) so every `use super::common::*;`
//! glob and explicit `common::…` reference keeps resolving unchanged. Logic is
//! byte-for-byte identical to the pre-split version.
use super::common::*;
use super::inspect::*;
use super::*;

/// Build the typed `AmbiguousResult` payload for `ResolvedOutcome::ambiguous`.
pub(crate) fn to_ambiguous(candidates: &[CandidateRow]) -> AmbiguousResult {
    let total = candidates.len();
    let shown = candidates
        .iter()
        .take(MAX_AMBIGUOUS_CANDIDATES)
        .map(CandidateRow::to_ambiguous_candidate)
        .collect();
    AmbiguousResult {
        ambiguous: true,
        total,
        truncated: total > MAX_AMBIGUOUS_CANDIDATES,
        candidates: shown,
    }
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
pub(crate) fn compute_proximity_boosts(
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

        // Files containing a caller of an explored symbol. PATTERN-DEBT
        // call-edges-missing-ruled-out-filter: a SCIP-disproven caller must
        // not boost an unrelated file's ranking — condition ordered before
        // `to_symbol IN` since query_symbol_path_pairs_chunked appends the
        // `(?1, ?2, ...)` placeholder list right after this prefix string.
        let mut callers = Vec::new();
        query_symbol_path_pairs_chunked(
            conn,
            "SELECT to_symbol, from_path FROM call_edges WHERE ruled_out_by_scip = 0 AND to_symbol IN",
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

/// The pure score math behind `CalmServer::apply_personalization_boost`
/// (Plan 3 §3.2), extracted as a free `&self`-free function so it's
/// directly unit-testable without a `CalmServer`/DB fixture. Every
/// result's `score` is min-max normalized to `[0,1]` across `results`
/// FIRST, then `weight * boost` (0 for a result with no entry in `boosts`)
/// is added — normalizing first matters because raw scores are on wildly
/// different scales across search kinds (RRF top-1 ≈ 0.05-0.17, grep/file
/// = 1.0 constant, bm25 1-30+, semantic 0-1); adding a fixed-magnitude
/// boost directly to the raw score (the pre-Plan-3 behavior) let it swamp
/// RRF results outright while doing nothing at all on bm25 — the exact
/// contradiction of `apply_personalization_boost`'s own "never overriding
/// a strong match" doc promise.
///
/// `compute_proximity_boosts` bounds every `boosts` value to `(0.0, 1.0]`,
/// which makes that promise an actual invariant here: two results whose
/// normalized scores differ by more than `weight` can never have their
/// relative order flipped by any boost, since the largest a boost can ever
/// move a score is `weight * 1.0 = weight` — see
/// `personalization_tests::normalize_then_boost_never_flips_a_large_gap`.
///
/// Returns `false` (and leaves `results` completely untouched — not even
/// re-normalized) when `boosts` is empty or none of its keys match any
/// path in `results`, preserving `apply_personalization_boost`'s "no-op
/// when nothing to boost" guarantee. Otherwise rewrites every result's
/// `score` (not just the boosted ones — `score`'s scale changes from raw
/// to normalized, a deliberate, documented trade-off: it was already
/// opaque to callers/agents, and `personalized: true` is the field that
/// reports this happened), re-sorts descending by the new score, and
/// returns `true`.
pub(crate) fn normalize_then_boost(
    results: &mut [calm_core::search::SearchResult],
    boosts: &std::collections::HashMap<String, f64>,
    weight: f64,
) -> bool {
    if boosts.is_empty() || !results.iter().any(|r| boosts.contains_key(&r.path)) {
        return false;
    }

    let (min, max) = results
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), r| {
            (lo.min(r.score), hi.max(r.score))
        });
    let range = max - min;
    let normalize = |s: f64| -> f64 { if range > 0.0 { (s - min) / range } else { 0.5 } };

    for r in results.iter_mut() {
        let boost = boosts.get(&r.path).copied().unwrap_or(0.0);
        r.score = normalize(r.score) + weight * boost;
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    true
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
    /// `path::name` (or `path::Class::name`) — the enclosing file path is
    /// always the substring before the first `::`, since every producer
    /// (tree-sitter extraction in `indexer::pipeline`, SQL indexer, SCIP
    /// overlay ingestion) derives both from the same `rel`/`from_path`
    /// value at the point the symbol/edge is created. A separate `path`
    /// field used to duplicate this verbatim on every entry — pure waste
    /// on a hub symbol with many callers in the same file; split on the
    /// first `::` if you need it standalone.
    pub(crate) symbol: String,
    pub(crate) edge_confidence: String,
    /// Nguồn cụ thể đứng sau `edge_confidence == "formal"`: `"stack_graphs"`
    /// (heuristic per-file — `resolver/formal.rs::FormalResolver::
    /// resolve_file` only ever sees ONE file at a time, no cross-module
    /// stitching) | `"scip"` (exact file,line, có thể cross-module) |
    /// `"lsp"` (runtime probe). `None` khi `edge_confidence != "formal"`,
    /// hoặc build thiếu mọi formal-tier feature. Không suy ra "formal" đáng
    /// tin bằng nhau — xem ADR-0002.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) formal_source: Option<String>,
    /// `"call"` or `"reference"` (SQL view/proc reading a table via
    /// FROM/JOIN) — see `call_edges.edge_kind`. Lets a consumer tell a real
    /// invocation apart from a mere read without misreading a JOIN as a
    /// function call.
    pub(crate) edge_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

/// two calls with the same set of `(symbol, edge_confidence, formal_source,
/// edge_kind, line, preview)` tuples in the same order are guaranteed to
/// hash identically. `formal_source` is included deliberately (D2,
/// 2026-07-30 stack-graphs-demotion-lever): a background SCIP overlay pass
/// can flip `formal_source` (`stack_graphs` -> `scip`) without changing
/// `edge_confidence`/`edge_kind`/`line`/`preview` at all -- omitting it here
/// would let an `if_none_match` caller silently miss that provenance
/// upgrade.
/// Deterministic fingerprint of a caller/callee result set, for
/// `if_none_match`/`etag` conditional-fetch (same pattern as `source`'s own
/// etag — see `range_checksum`/`hash_content`). Includes `preview` (not just
/// the SQL columns) so a call site whose *line content* changed — but not
/// its confidence/path/line-number — still gets a fresh etag; two calls
/// with the same set of `(symbol, edge_confidence, edge_kind, line,
/// preview)` tuples in the same order are guaranteed to hash identically.
/// No separate `path` component: `symbol` is always `path::name` (or
/// `path::Class::name`), so any change to the path is already a change to
/// `symbol` — hashing both would be redundant, not more discriminating.
pub(crate) fn hash_caller_entries<'a>(
    entries: impl IntoIterator<Item = &'a CallerEntry>,
) -> String {
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&e.symbol);
        buf.push('\u{1}');
        buf.push_str(&e.edge_confidence);
        buf.push('\u{1}');
        buf.push_str(e.formal_source.as_deref().unwrap_or(""));
        buf.push('\u{1}');
        buf.push_str(&e.edge_kind);
        buf.push('\u{1}');
        if let Some(l) = e.line {
            buf.push_str(&l.to_string());
        }
        buf.push('\u{1}');
        if let Some(p) = &e.preview {
            buf.push_str(p);
        }
        buf.push('\u{2}');
    }
    calm_core::indexer::pipeline::hash_content(&buf)
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CalleeEntry {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) edge_confidence: String,
    /// See `CallerEntry::formal_source`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) formal_source: Option<String>,
    /// See `CallerEntry::edge_kind`.
    pub(crate) edge_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

/// `CalleeEntry` counterpart of `hash_caller_entries` — same rationale and
/// field set, just for `callees`'s direction.
pub(crate) fn hash_callee_entries<'a>(
    entries: impl IntoIterator<Item = &'a CalleeEntry>,
) -> String {
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&e.symbol);
        buf.push('\u{1}');
        buf.push_str(&e.path);
        buf.push('\u{1}');
        buf.push_str(&e.edge_confidence);
        buf.push('\u{1}');
        buf.push_str(e.formal_source.as_deref().unwrap_or(""));
        buf.push('\u{1}');
        buf.push_str(&e.edge_kind);
        buf.push('\u{1}');
        if let Some(l) = e.line {
            buf.push_str(&l.to_string());
        }
        buf.push('\u{1}');
        if let Some(p) = &e.preview {
            buf.push_str(p);
        }
        buf.push('\u{2}');
    }
    calm_core::indexer::pipeline::hash_content(&buf)
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
            "SELECT from_symbol, from_path, edge_confidence FROM call_edges \
             WHERE to_symbol = ?1 AND ruled_out_by_scip = 0"
        }
        EdgeDirection::Callees => {
            "SELECT to_symbol, to_path, edge_confidence FROM call_edges \
             WHERE from_symbol = ?1 AND ruled_out_by_scip = 0"
        }
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return (vec![], false),
    };

    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(timeout_ms);

    // Audit 3.7: a flat `HashSet<String>` here would permanently poison a
    // node the first time it's SEEN, even via a non-expandable `ambiguous`
    // edge — a later encounter of the SAME node through a confirmed edge
    // would then find it already "visited" and silently drop, losing
    // everything reachable behind it. The value tracks whether the
    // recorded sighting was expandable, so a later confirmed encounter can
    // still promote a blocked node into the next frontier (it just never
    // re-emits a second `TransitiveEntry` for a node already reported).
    let mut visited: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    visited.insert(start_qualified_name.to_string(), true);
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
                // An `ambiguous` edge is index-time fan-out (one call site,
                // one edge per same-named symbol). Reported, because the
                // caller still wants to see it — but never expanded on its
                // own: each such hop would multiply the frontier by the
                // fan-out width, and confidence is not transitive, so
                // anything found behind one inherits its uncertainty while
                // presenting its own edge's confidence. Measured on real
                // corpora before this guard: a depth-3 query returned up to
                // 47% of a whole repo.
                let expandable =
                    edge_confidence != calm_core::types::EdgeConfidence::Ambiguous.as_str();

                match visited.get(&sym_name).copied() {
                    None => {
                        visited.insert(sym_name.clone(), expandable);
                        results.push(TransitiveEntry {
                            symbol: sym_name.clone(),
                            path: sym_path,
                            depth: depth as i64,
                            edge_confidence,
                        });
                        if expandable {
                            next_frontier.push(sym_name);
                        }
                    }
                    Some(false) if expandable => {
                        // Previously seen only via an ambiguous edge and
                        // blocked from expansion — this confirmed encounter
                        // promotes it (SeenAmbiguous -> SeenExpandable). The
                        // node was already reported once; don't duplicate
                        // the entry, just unblock traversal behind it.
                        visited.insert(sym_name.clone(), true);
                        next_frontier.push(sym_name);
                    }
                    _ => {
                        // Already recorded as expandable (or re-seen as
                        // ambiguous with nothing new to offer) — skip.
                    }
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

/// Whether `caller_count == 0` on a risk-relevant symbol should be treated
/// as an opaque/uncertain signal rather than genuine low-blast-radius
/// usage: true when the dead-code heuristic (`is_entry_point`/`is_test`/
/// coverage-aware `compute_dead_code_confidence`) disagrees this symbol
/// looks safely removable. Shared by `edit_context`'s advisory risk
/// escalation and `compute_touch_risk`'s hard write-gate so the two
/// independent consumers of `caller_count` can't silently drift apart the
/// way `risk_level_from_caller_count`'s three former copies once did (see
/// its own docstring) — confirmed live: before this was wired into
/// `compute_touch_risk`, editing a real zero-caller `#[tool(name = "...")]`
/// MCP handler via `edit_lines`/`edit_symbol` bypassed the mandatory
/// confirm/edit_context gate entirely, because `is_hub`/raw `caller_count`
/// alone can't see the framework dispatch that's its actual caller.
pub(crate) fn zero_caller_count_is_uncertain(dead_code_confidence: &str) -> bool {
    matches!(dead_code_confidence, "none" | "low")
}

/// Why a touched symbol's `caller_count == 0` shouldn't be read as "safe to
/// edit without a closer look" — see `zero_caller_count_is_uncertain`. Kept
/// distinct from that boolean so a denial message can name the actual
/// cause instead of defaulting to "entry point" even when the real trigger
/// was `is_test` or a borderline coverage/scope call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UncertainZeroCallerReason {
    /// `is_entry_point`: real invocation is a framework/macro/language
    /// dispatch mechanism (rmcp `#[tool]`, `main`, a trait-dispatch name, a
    /// bodyless trait method declaration, ...) invisible to the static
    /// call graph — `caller_count == 0` here is permanent and structural.
    EntryPoint,
    /// `is_test` (and not also an entry point): the test harness discovers
    /// and runs it by convention/reflection, not a literal call site.
    /// Same static-graph blind spot as `EntryPoint`, different cause and
    /// consequence — nothing external depends on it, so the risk on edit
    /// is to test coverage silently breaking, not production blast radius.
    TestOnly,
    /// Neither of the above, but `compute_dead_code_confidence` still came
    /// back `"none"`/`"low"` for a function/method (e.g. runtime coverage
    /// shows it executing despite no static callers). A genuine but
    /// unlabeled "this doesn't look confidently safe" signal.
    LowConfidence,
}

/// Classifies *why* `dead_code_confidence` disagreed a zero-caller
/// function/method looks safely removable, or returns `None` if it
/// didn't (`zero_caller_count_is_uncertain` was false). `is_entry_point`
/// takes priority over `is_test` when — in principle — both were somehow
/// true at once, since it's the stronger, more specific signal.
pub(crate) fn classify_uncertain_zero_caller(
    is_entry_point: bool,
    is_test: bool,
    dead_code_confidence: &str,
) -> Option<UncertainZeroCallerReason> {
    if !zero_caller_count_is_uncertain(dead_code_confidence) {
        return None;
    }
    Some(if is_entry_point {
        UncertainZeroCallerReason::EntryPoint
    } else if is_test {
        UncertainZeroCallerReason::TestOnly
    } else {
        UncertainZeroCallerReason::LowConfidence
    })
}

/// Priority ordering when multiple touched symbols disagree on why —
/// mirrors `hub_kind_strength`'s "pick the strongest signal found" shape.
pub(crate) fn uncertain_zero_caller_strength(reason: UncertainZeroCallerReason) -> u8 {
    match reason {
        UncertainZeroCallerReason::EntryPoint => 2,
        UncertainZeroCallerReason::TestOnly => 1,
        UncertainZeroCallerReason::LowConfidence => 0,
    }
}

const CALL_SITE_PREVIEW_MAX_CHARS: usize = 160;

/// Read the trimmed source line at `line` (1-indexed) from `project_root/path`
/// for a `CallerEntry`/`CalleeEntry` preview. Best-effort: missing files, a
/// line number past EOF, or a `None` line all just yield `None` rather than
/// an error — a preview is a convenience, not load-bearing.
/// Same semantics as `line_preview`, but reads each distinct file at most
/// once regardless of how many `items` reference it (audit F11) — a hub
/// symbol's `callers`/`callees` rows routinely repeat the same file dozens
/// of times, each of which used to be its own full-file `read_to_string`.
/// Returns previews in the same order as `items`.
pub(crate) fn line_previews_batched(
    project_root: &std::path::Path,
    items: &[(String, Option<i64>)],
) -> Vec<Option<String>> {
    let mut file_cache: std::collections::HashMap<&str, Option<String>> =
        std::collections::HashMap::new();
    items
        .iter()
        .map(|(path, line)| {
            let line = (*line)?;
            if line < 1 {
                return None;
            }
            let content = file_cache
                .entry(path.as_str())
                .or_insert_with(|| std::fs::read_to_string(project_root.join(path)).ok())
                .as_ref()?;
            let raw = content.lines().nth((line - 1) as usize)?.trim();
            if raw.is_empty() {
                return None;
            }
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
        })
        .collect()
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

    // Plan 3 §3.2 — tests for the pure `normalize_then_boost` score math
    // (not `compute_proximity_boosts`, which the tests above already
    // cover and this doesn't touch).

    fn sr(path: &str, score: f64) -> calm_core::search::SearchResult {
        calm_core::search::SearchResult {
            name: path.to_string(),
            qualified_name: path.to_string(),
            path: path.to_string(),
            kind: None,
            line_start: None,
            line_end: None,
            score,
            match_type: "symbol".to_string(),
            snippet: None,
            is_test: false,
            churn_score: None,
            coreness: None,
        }
    }

    #[test]
    fn normalize_then_boost_leaves_results_untouched_when_no_path_matches() {
        let mut results = vec![sr("unrelated.rs", 0.5)];
        let mut boosts = HashMap::new();
        boosts.insert("other.rs".to_string(), 1.0);
        let adjusted = normalize_then_boost(&mut results, &boosts, 0.15);
        assert!(!adjusted);
        assert_eq!(
            results[0].score, 0.5,
            "score must be left exactly as-is, not even normalized, when nothing matches"
        );
    }

    #[test]
    fn normalize_then_boost_regression_top1_survives_neighbor_boost() {
        // Reproduces the exact scenario from the original audit (F3): an RRF
        // result set where the best match (top-1, score ~0.071) sits far
        // above a "neighbor of an explored file" at rank 8 (score ~0.036) —
        // before Plan 3's normalize-first fix, raw `score += weight * boost`
        // (weight 0.15, boost up to 1.0) let the rank-8 neighbor jump
        // straight to rank 1. Fails on the pre-fix math, passes after.
        let scores = [0.071, 0.065, 0.058, 0.052, 0.047, 0.042, 0.039, 0.036];
        let mut results: Vec<_> = scores
            .iter()
            .enumerate()
            .map(|(i, &score)| sr(&format!("p{i}.rs"), score))
            .collect();

        let mut boosts = HashMap::new();
        boosts.insert("p7.rs".to_string(), 1.0); // rank-8 (0-indexed 7), max boost

        let adjusted = normalize_then_boost(&mut results, &boosts, 0.15);
        assert!(adjusted);
        assert_eq!(
            results[0].path,
            "p0.rs",
            "the strongest original match must stay rank 1 — got order: {:?}",
            results.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn normalize_then_boost_never_flips_a_large_gap() {
        // Property test: 50 random result sets spanning the 4 real score
        // scales (RRF ~0.03-0.2, bm25 1-30, grep/file constant 1.0,
        // semantic/cosine 0-1) plus random boosts in (0,1] — for every
        // pair whose *normalized* scores differ by more than `weight`,
        // boost must never flip their relative order. True by construction
        // once boost is bounded to <=1.0 (the max any boost can move a
        // score is `weight * 1.0`), but tested directly against the real
        // implementation rather than just argued algebraically.
        let weight = 0.15;
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next_u64 = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let next_f64 = |lo: f64, hi: f64, s: &mut dyn FnMut() -> u64| {
            let r = (s() >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
            lo + r * (hi - lo)
        };

        for trial in 0..50 {
            let n = 6;
            let results_seed: Vec<(String, f64)> = (0..n)
                .map(|i| {
                    let score = match i % 4 {
                        0 => next_f64(0.03, 0.2, &mut next_u64),
                        1 => next_f64(1.0, 30.0, &mut next_u64),
                        2 => 1.0,
                        _ => next_f64(0.0, 1.0, &mut next_u64),
                    };
                    (format!("p{trial}_{i}.rs"), score)
                })
                .collect();
            let mut results: Vec<_> = results_seed.iter().map(|(p, s)| sr(p, *s)).collect();

            let (min, max) = results
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), r| {
                    (lo.min(r.score), hi.max(r.score))
                });
            let range = max - min;
            let norm_of = |s: f64| if range > 0.0 { (s - min) / range } else { 0.5 };
            let normalized: Vec<f64> = results.iter().map(|r| norm_of(r.score)).collect();
            let paths: Vec<String> = results.iter().map(|r| r.path.clone()).collect();

            let mut boosts = HashMap::new();
            for path in &paths {
                if next_u64() % 2 == 0 {
                    boosts.insert(path.clone(), next_f64(0.0001, 1.0, &mut next_u64));
                }
            }
            if boosts.is_empty() {
                continue; // guaranteed no-op — nothing to check
            }

            assert!(normalize_then_boost(&mut results, &boosts, weight));
            let rank_of = |path: &str| results.iter().position(|r| r.path == path).unwrap();

            for i in 0..n {
                for j in (i + 1)..n {
                    let gap = (normalized[i] - normalized[j]).abs();
                    if gap <= weight {
                        continue;
                    }
                    let (stronger, weaker) = if normalized[i] > normalized[j] {
                        (i, j)
                    } else {
                        (j, i)
                    };
                    assert!(
                        rank_of(&paths[stronger]) < rank_of(&paths[weaker]),
                        "trial {trial}: normalized gap {gap:.4} > weight {weight} but boost \
                         flipped {} (norm {:.4}) behind {} (norm {:.4})",
                        paths[stronger],
                        normalized[stronger],
                        paths[weaker],
                        normalized[weaker]
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod transitive_bfs_tests {
    use super::*;

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        calm_core::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn edge(conn: &rusqlite::Connection, from: &str, to: &str, confidence: &str, ruled_out: i64) {
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, from_path, to_path, \
             edge_confidence, ruled_out_by_scip, edge_kind) \
             VALUES (?1, ?2, ?1, ?2, ?3, ?4, 'call')",
            rusqlite::params![from, to, confidence, ruled_out],
        )
        .unwrap();
    }

    /// `callers`' own direct query (tools/trace.rs) filters `ruled_out_by_scip = 0`
    /// — an edge SCIP has *proven* wrong must not reappear just because the
    /// caller asked for the transitive view.
    #[test]
    fn skips_scip_ruled_out_edges() {
        let conn = test_conn();
        edge(&conn, "caller_ok", "target", "resolved", 0);
        edge(&conn, "caller_disproven", "target", "resolved", 1);

        let (entries, _) = transitive_bfs(&conn, "target", EdgeDirection::Callers, 3, 5_000);
        let names: Vec<&str> = entries.iter().map(|e| e.symbol.as_str()).collect();

        assert!(names.contains(&"caller_ok"));
        assert!(
            !names.contains(&"caller_disproven"),
            "ruled_out_by_scip edge was traversed: {names:?}"
        );
    }

    /// An `ambiguous` edge is index-time fan-out: one call site emitting an edge
    /// to EVERY same-named symbol. Reporting it as a reachable node is fine (the
    /// direct path does too, in its own bucket), but *expanding* it multiplies
    /// the frontier by the fan-out width at every hop — measured on real corpora
    /// as up to 47% of an entire repo returned from one depth-3 query. So an
    /// ambiguous node is a leaf: reported, never expanded.
    #[test]
    fn reports_but_does_not_expand_ambiguous_edges() {
        let conn = test_conn();
        edge(&conn, "fanout", "target", "ambiguous", 0);
        edge(&conn, "behind_fanout", "fanout", "resolved", 0);

        let (entries, _) = transitive_bfs(&conn, "target", EdgeDirection::Callers, 3, 5_000);
        let names: Vec<&str> = entries.iter().map(|e| e.symbol.as_str()).collect();

        assert!(
            names.contains(&"fanout"),
            "ambiguous neighbour should still be reported: {names:?}"
        );
        assert!(
            !names.contains(&"behind_fanout"),
            "BFS compounded THROUGH an ambiguous edge: {names:?}"
        );
    }

    /// The stop must be specific to ambiguity, not a blanket depth-1 cap.
    #[test]
    fn still_expands_normally_through_confident_edges() {
        let conn = test_conn();
        edge(&conn, "mid", "target", "resolved", 0);
        edge(&conn, "deep", "mid", "formal", 0);

        let (entries, _) = transitive_bfs(&conn, "target", EdgeDirection::Callers, 3, 5_000);
        let names: Vec<&str> = entries.iter().map(|e| e.symbol.as_str()).collect();

        assert!(names.contains(&"mid"));
        assert!(
            names.contains(&"deep"),
            "confident chain was cut short: {names:?}"
        );
    }

    /// Audit 3.7 regression: a node first seen (and blocked from expansion)
    /// via an ambiguous edge must still be expandable once a LATER, confirmed
    /// encounter of the SAME node arrives via a different path — a flat
    /// `visited: HashSet` would permanently poison it on the first sighting
    /// regardless of confidence, silently dropping everything reachable
    /// behind it. Structured across depths (not row order within one SQL
    /// query) so the ordering is deterministic:
    ///   depth 1: target <- fanout (ambiguous, blocked) ; target <- via (resolved)
    ///   depth 2: via <- fanout (resolved)  — same node, now confirmed
    ///   depth 3: fanout <- behind_fanout (resolved) — only reachable if the
    ///            depth-2 confirmed encounter was allowed to promote `fanout`
    ///            into the frontier.
    #[test]
    fn confirmed_encounter_unpoisons_a_node_first_seen_via_an_ambiguous_edge() {
        let conn = test_conn();
        edge(&conn, "fanout", "target", "ambiguous", 0);
        edge(&conn, "via", "target", "resolved", 0);
        edge(&conn, "fanout", "via", "resolved", 0);
        edge(&conn, "behind_fanout", "fanout", "resolved", 0);

        let (entries, _) = transitive_bfs(&conn, "target", EdgeDirection::Callers, 3, 5_000);
        let names: Vec<&str> = entries.iter().map(|e| e.symbol.as_str()).collect();

        assert!(names.contains(&"fanout"), "{names:?}");
        assert!(names.contains(&"via"), "{names:?}");
        assert!(
            names.contains(&"behind_fanout"),
            "a confirmed re-encounter of an ambiguous-blocked node must still \
             unblock traversal behind it: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|n| **n == "fanout").count(),
            1,
            "promoting a node to expandable must not re-emit a duplicate entry: {names:?}"
        );
    }
}
