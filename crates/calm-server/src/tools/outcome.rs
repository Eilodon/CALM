//! Tool-response envelopes (`ToolOutcome`/`ResolvedOutcome`), error/caveat
//! construction, symbol resolution (`resolve_symbol`/`CandidateRow`), and the
//! `SuggestedNext` helpers — extracted verbatim from `tools/common.rs`
//! (2026-07-28 hotspot split). Every item was `pub(crate)` in `common` and is
//! re-exported through it (`pub(crate) use crate::tools::outcome::*;`), so both
//! the `use super::common::*;` glob across `tools/*.rs` and any explicit
//! `common::…` reference keep resolving unchanged. Logic is byte-for-byte
//! identical to the pre-split version.
use super::common::*;
use super::*;

// ---------------------------------------------------------------------------
// Shared output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct SuggestedNext {
    pub(crate) tool: String,
    pub(crate) reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) args: Option<serde_json::Value>,
    /// Plan 3 §3.5(b): `Some(true)` iff skipping `tool` is actually
    /// hook-enforced (currently only the edit_context/edit_lines/edit_symbol
    /// → diff_impact hints set this, via `suggested_gated`) — every other
    /// hint is left unset (`None`), meaning advisory-only. Lets an agent
    /// tell "you'll be blocked if you skip this" apart from "you probably
    /// want this next" without re-deriving it from AGENTS.md prose each time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gate: Option<bool>,
}

pub(crate) fn suggested(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
        gate: None,
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
        gate: None,
    })
}

/// Plan 3 §3.5(b): same as `suggested`, but for the 2 hints backed by an
/// actual hook enforcement (the pending-diff_impact gate) rather than a
/// convention — sets `gate: Some(true)` so an agent can tell "mandatory,
/// you will be blocked" from "recommended" without re-reading AGENTS.md.
pub(crate) fn suggested_gated(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
        gate: Some(true),
    })
}

/// `router.has_route(tool)` already means exactly "registered and not
/// disabled" (rmcp's own definition) — this used to re-derive the same
/// answer from the `preset` STRING via a separate `preset_tools` match, a
/// second mechanism that could disagree with what `tool_router_for_preset`
/// actually did to the router. Querying the router directly makes it the
/// single source of truth for "is this tool available", matching what
/// `tool_router_for_preset`'s own doc comment already promises for
/// `list_tools`/`call_tool` — now `suggested_next` filtering shares that
/// guarantee too instead of running a parallel, driftable computation.
pub(crate) fn is_tool_available(
    router: &rmcp::handler::server::router::tool::ToolRouter<CalmServer>,
    tool: &str,
) -> bool {
    router.has_route(tool)
}

pub(crate) fn filter_suggested_next(
    sn: Option<SuggestedNext>,
    router: &rmcp::handler::server::router::tool::ToolRouter<CalmServer>,
) -> Option<SuggestedNext> {
    match &sn {
        Some(s) if !is_tool_available(router, &s.tool) => None,
        _ => sn,
    }
}

/// Typed `{"error": {"code","message","recoverable"}}` envelope.
pub(crate) fn error_output(code: &str, message: &str, recoverable: bool) -> ErrorOutput {
    ErrorOutput {
        error: ErrorDetail {
            code: code.into(),
            message: message.into(),
            recoverable,
        },
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

/// Typed not-found envelope for `ResolvedOutcome::not_found`.
pub(crate) fn not_found_error(symbol: &str) -> ErrorOutput {
    error_output(
        "NOT_FOUND",
        &format!("Symbol '{symbol}' not found in index"),
        false,
    )
}

/// Typed `{"error": {"code": "DB_ERROR", ...}}` for the read-connection
/// failure every read-only tool guards against. All tools now emit this
/// shape via `ToolOutcome::error` / `ResolvedOutcome::error`.
pub(crate) fn error_detail(code: &str, message: &str, recoverable: bool) -> ErrorDetail {
    ErrorDetail {
        code: code.into(),
        message: message.into(),
        recoverable,
    }
}
pub(crate) fn db_error<T>(e: impl std::fmt::Display) -> ToolOutcome<T> {
    ToolOutcome::error(error_detail(
        "DB_ERROR",
        &format!("db connection failed: {e}"),
        true,
    ))
}

/// Same as `db_error`, for tools whose success path can also be
/// `Ambiguous` (anything built on `resolve_symbol`).
pub(crate) fn db_error_resolved<T>(e: impl std::fmt::Display) -> ResolvedOutcome<T> {
    ResolvedOutcome::error(error_detail(
        "DB_ERROR",
        &format!("db connection failed: {e}"),
        true,
    ))
}

/// Shared success/error envelope for tools with no ambiguous-name branch
/// (i.e. no `resolve_symbol` call).
///
/// NOT a `#[serde(untagged)]` enum: rmcp 2.2.0's `Json<T>` requires T's
/// JSON Schema to have root `"type": "object"` (`schema_for_output`
/// panics otherwise — an untagged enum's schema is a bare `oneOf`/`anyOf`
/// with no top-level `"type"`). So this is a genuine struct with optional/
/// flattened fields instead. Exactly one of `error` / the flattened `T` is
/// ever `Some` at a time — enforced by only constructing through `error`/
/// `success` below, never a struct literal — which reproduces the exact
/// same wire shape tools emitted as a bare JSON string before this type
/// existed (`{"error": {...}}` or `T`'s fields directly at the root).
#[derive(Serialize, JsonSchema)]
pub(crate) struct ToolOutcome<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    success: Option<T>,
}

impl<T> ToolOutcome<T> {
    pub(crate) fn error(detail: ErrorDetail) -> Self {
        ToolOutcome {
            error: Some(detail),
            success: None,
        }
    }

    pub(crate) fn success(value: T) -> Self {
        ToolOutcome {
            error: None,
            success: Some(value),
        }
    }

    /// Bridges `edit_symbol` (which resolves a name first) into the same
    /// `ResolvedOutcome` envelope as other `resolve_symbol`-based tools.
    pub(crate) fn into_resolved(self) -> ResolvedOutcome<T> {
        if let Some(detail) = self.error {
            ResolvedOutcome::error(detail)
        } else if let Some(value) = self.success {
            ResolvedOutcome::success(value)
        } else {
            ResolvedOutcome::error(error_detail("INTERNAL", "empty ToolOutcome", false))
        }
    }
}

/// Structured, machine-checkable hint attached to a tool result whose
/// literal content (empty list / not-found) could otherwise be misread as
/// proof of absence. `class` lets a safety gate branch without parsing
/// `message`; `message` is the human-readable explanation. Design mirrors
/// zzet/gortex's `ZeroEdgeCaveat` (Apache-2.0) — reimplemented against
/// CALM's own resolver shape, not a line-for-line port.
#[derive(Serialize, JsonSchema)]
pub(crate) struct Caveat {
    pub(crate) class: &'static str,
    pub(crate) message: String,
}

impl Caveat {
    /// The queried symbol did not resolve to anything in the index at all
    /// — the most common cause of an unpleasant "0 results" surprise, and
    /// almost always a typo, wrong case, or a file in an excluded path
    /// rather than the symbol genuinely not existing.
    pub(crate) fn not_found(symbol: &str) -> Self {
        Caveat {
            class: "not_found",
            message: format!(
                "no symbol named '{symbol}' is in the index — likely a typo, wrong \
                 case, or the file lives in an excluded path (target/, node_modules/, \
                 dist/, build/, __pycache__/, venv/, legacy/, dotdirs). CALM also \
                 does not index standard-library, language-builtin, or third-party \
                 dependency symbols (Rust's println!, Python's len, etc.) — if \
                 '{symbol}' is one of those, its absence here is expected, not a \
                 search failure, and search(kind=\"hybrid\") will only surface your \
                 own code that USES it, never its definition. Otherwise run \
                 search(kind=\"hybrid\") to find the exact name before concluding it \
                 doesn't exist — a not-found result here is not proof the symbol is \
                 unused or absent from the codebase."
            ),
        }
    }

    /// The symbol resolved, but the specific edge/usage query on it came
    /// back with zero rows. Distinct from `not_found`: the symbol is real,
    /// but static analysis may simply not see how it's reached (dynamic
    /// dispatch, reflection, string-based invocation, or a public API
    /// consumed outside this repo).
    pub(crate) fn no_direct_usage(symbol: &str) -> Self {
        Caveat {
            class: "no_direct_usage",
            message: format!(
                "'{symbol}' has zero direct callers in the index. This can mean \
                 genuine dead code, but it can also mean call sites use dynamic \
                 dispatch, reflection, or string-based invocation that static \
                 analysis can't resolve, or that '{symbol}' is a public API consumed \
                 outside this repo. Do not treat this as proof of no usage without \
                 also checking dependencies() and the symbol's exported visibility."
            ),
        }
    }

    /// Same "zero direct callers" situation as `no_direct_usage`, but for a
    /// symbol the parser already flagged `is_entry_point` at index time
    /// (`detect_entry_point`/`rust_attr_is_dispatch_signal` and their
    /// per-language equivalents — `main`, a trait-dispatch protocol method,
    /// a decorator/annotation-registered handler, or a macro-attribute
    /// dispatch target such as an rmcp `#[tool(name = "...")]` MCP handler).
    /// For these, zero static callers isn't a "maybe dead, maybe hidden"
    /// judgment call — it's the expected, permanent shape: the real
    /// invocation happens through a mechanism (framework dispatch table,
    /// operator/protocol sugar, decorator registration) that never appears
    /// as a literal call-site token in source, so no amount of indexing —
    /// tree-sitter or a compiler-grade SCIP overlay alike — can ever
    /// populate this count. Distinct wording from `no_direct_usage` so a
    /// caller doesn't read this as a dead-code hint to chase down.
    pub(crate) fn entry_point_dispatch(symbol: &str) -> Self {
        Caveat {
            class: "entry_point_dispatch",
            message: format!(
                "'{symbol}' has zero direct callers in the index, but is flagged as a \
                 language/framework entry point (e.g. an rmcp #[tool(...)] MCP handler, \
                 `main`, a trait-dispatch protocol method, a decorator/annotation-registered \
                 handler, or similar). Its real invocation happens through a mechanism \
                 CALM's static call graph can't lexically capture — a near-zero caller_count \
                 here is expected by design, not a dead-code signal."
            ),
        }
    }

    /// Some, but not all, of a `symbols_batch` call's requested
    /// `qualified_names` matched nothing in the index. Names the first
    /// few missing ids so the caller doesn't have to diff the request
    /// against `results` to see which ones failed.
    pub(crate) fn batch_some_not_found(missing: &[String]) -> Self {
        let sample: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
        Caveat {
            class: "batch_some_not_found",
            message: format!(
                "{} of the requested qualified_names were not found in the index \
                 (e.g. {}). symbols_batch does no fuzzy matching — a near-miss id \
                 comes back found:false rather than silently substituting the \
                 closest name. Run search(kind=\"hybrid\") to get the exact \
                 qualified_name for each missing entry.",
                missing.len(),
                sample.join(", "),
            ),
        }
    }
}

/// Same as `ToolOutcome<T>`, plus the `ambiguous` branch every
/// `resolve_symbol`-based tool can also produce — same flatten-based,
/// root-`type:object` reasoning as `ToolOutcome<T>` above.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ResolvedOutcome<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    ambiguous: Option<AmbiguousResult>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    success: Option<T>,
    /// Advisory hint on an empty/not-found result. Never set alongside a
    /// populated `success` unless a tool opts in via `with_caveat` (e.g.
    /// `callers` on zero direct callers).
    #[serde(skip_serializing_if = "Option::is_none")]
    caveat: Option<Caveat>,
}

impl<T> ResolvedOutcome<T> {
    pub(crate) fn error(detail: ErrorDetail) -> Self {
        ResolvedOutcome {
            error: Some(detail),
            ambiguous: None,
            success: None,
            caveat: None,
        }
    }

    /// Bridges the existing `SymbolResolution` match arms: `NotFound`/
    /// `Ambiguous` map onto their typed shape here. `Found` is left to the
    /// caller — it needs tool-specific work (`track_symbol`, health
    /// lookups, ...) that doesn't belong in a generic helper.
    pub(crate) fn not_found(symbol: &str) -> Self {
        let mut out = Self::error(not_found_error(symbol).error);
        out.caveat = Some(Caveat::not_found(symbol));
        out
    }

    pub(crate) fn ambiguous(candidates: &[CandidateRow]) -> Self {
        ResolvedOutcome {
            error: None,
            ambiguous: Some(to_ambiguous(candidates)),
            success: None,
            caveat: None,
        }
    }

    pub(crate) fn success(value: T) -> Self {
        ResolvedOutcome {
            error: None,
            ambiguous: None,
            success: Some(value),
            caveat: None,
        }
    }

    /// Attaches an advisory caveat to an already-built success result —
    /// e.g. `callers` on a resolved symbol with zero direct callers. Never
    /// overrides `error`/`ambiguous`; only meaningful after `success`.
    pub(crate) fn with_caveat(mut self, caveat: Caveat) -> Self {
        self.caveat = Some(caveat);
        self
    }
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

pub(crate) const MAX_AMBIGUOUS_CANDIDATES: usize = 10;

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
    pub(crate) boundary_ambiguous: bool,
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
) -> rusqlite::Result<Vec<CandidateRow>> {
    let sql = if path.is_some() {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE name = ?1 AND path = ?2 ORDER BY path, line_start"
    } else {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE name = ?1 ORDER BY path, line_start"
    };

    // audit F9: `?` on both statement-level failures below (a genuine DB/
    // schema problem) — a single malformed *row* still doesn't kill the
    // whole result set (see the `filter_map` further down, deliberately
    // unchanged), only a failure to even prepare/execute the query does.
    let mut stmt = conn.prepare(sql).inspect_err(|e| {
        tracing::warn!("resolve_symbol_candidates: prepare failed for {name:?}: {e}");
    })?;

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
            boundary_ambiguous: row.get::<_, i64>(15)? != 0,
        })
    };

    let rows = if let Some(path) = path {
        stmt.query_map(rusqlite::params![name, path], map_row)
    } else {
        stmt.query_map(rusqlite::params![name], map_row)
    };

    match rows {
        Ok(iter) => Ok(iter.filter_map(|r| r.ok()).collect()),
        Err(e) => {
            tracing::warn!("resolve_symbol_candidates: query_map failed for {name:?}: {e}");
            Err(e)
        }
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
) -> rusqlite::Result<SymbolResolution> {
    let mut candidates = resolve_symbol_candidates(conn, name, path)?;
    if let Some(line) = line {
        let in_range = |c: &CandidateRow| c.line_start <= line && line <= c.line_end;
        if candidates.iter().any(in_range) {
            candidates.retain(in_range);
        }
    }
    Ok(if candidates.is_empty() {
        SymbolResolution::NotFound
    } else if candidates.len() == 1 {
        SymbolResolution::Found(Box::new(candidates.remove(0)))
    } else {
        SymbolResolution::Ambiguous(candidates)
    })
}
