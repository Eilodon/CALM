export type EdgeConfidence = "formal" | "resolved" | "inferred" | "textual" | "ambiguous" | "unresolved";

export interface Health {
  has_docstring: boolean;
  test_files: string[];
  dead_code_confidence: "none" | "low" | "medium" | "high";
  dead_code_source: "static" | "static+coverage";
  caller_count_by_confidence: { resolved: number; inferred: number; textual: number } | null;
}


export interface RelatedNoteOutput {
  topic: string;
  excerpt: string;
  specificity: "symbol" | "file";
  staleness: "fresh" | "stale" | "gone" | "unknown";
}

export interface SuggestedNext {
  tool: string;
  reason: string;
  args?: Record<string, string | number | boolean>;
}

export interface AmbiguousResult {
  ambiguous: true;
  candidates: {
    name: string;
    path: string;
    kind: string;
    line_start: number;
    line_end: number;
    class_context?: string | null;
    caller_count?: number | null;
    language?: string;
    signature?: string;
  }[];
}

export interface ReviewerSuggestion {
  path: string;
  owners: string[];
  source: "CODEOWNERS" | "git_blame";
}

export interface RepoOverviewInput {
  path?: string;
  include_health?: boolean;
  top_n?: number;
}

export interface RepoOverviewOutput {
  languages: string[];
  indexing_phase: "scanning" | "parsing" | "building_edges" | "ready";
  embeddings_status: "disabled" | "downloading" | "embedding" | "ready" | "failed" | "offline_unavailable";
  module_map: {
    name: string;
    path: string;
    symbol_count: number | null;
    key_exports: string[];
  }[];
  total_modules: number;
  truncated: boolean;
  entry_points: { symbol: string; path: string; kind: string }[];
  stats: { files: number; symbols: number; edges: number };
  health_summary?: { dead_code_count: number; untested_modules: number; undocumented_hubs: number };
  note?: string;
  workflow_guide: string;
  suggested_next?: SuggestedNext;
}

export interface SearchInput {
  query: string;
  kind: "symbol" | "text" | "file" | "semantic" | "hybrid";
  limit?: number;
}

export interface SearchOutput {
  results: (
    | {
        path: string;
        match_type: "exact_file" | "dir_match";
        symbols_in_file: number;
      }
    | {
        name: string;
        path: string;
        line_start: number;
        line_end?: number;
        kind: string;
        match_type: "exact" | "fts" | "semantic" | "hybrid";
        preview: string;
      }
  )[];
  truncated: boolean;
  degraded: boolean;
  suggestions?: string[];
  embeddings_status?: "disabled" | "downloading" | "embedding" | "ready" | "failed";
  suggested_next?: SuggestedNext;
}

export interface FileOverviewInput {
  path: string;
  limit?: number;
}

export interface FileOverviewOutput {
  language: string;
  inferred_role: "service" | "model" | "router" | "utility" | "test" | "config" | null;
  symbols: {
    name: string;
    kind: string;
    signature: string;
    line_start: number;
    line_end: number;
    caller_count: number | null;
    is_hub: boolean | null;
    coreness: number | null;
  }[];
  total_symbols: number;
  truncated: boolean;
  edges_ready: boolean;
  note?: string;
  suggested_next?: SuggestedNext;
}

export interface SymbolInfoInput {
  name: string;
  path?: string;
}

export type SymbolInfoOutput = {
  name: string;
  kind: string;
  signature: string;
  docstring: string;
  path: string;
  line_start: number;
  line_end: number;
  language: string;
  caller_count: number | null;
  is_hub: boolean | null;
  coreness: number | null;
  health: Health;
  edges_ready: boolean;
  note?: string;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

export interface SourceInput {
  target: string;
  context_lines?: number;
  include_metadata?: boolean;
}

export type SourceOutput = {
  content: string;
  path: string;
  line_start: number;
  line_end: number;
  token_estimate: number;
  data_source: "disk";
  cached: boolean;
  metadata?: {
    language: string;
    caller_count: number | null;
    is_hub: boolean | null;
    coreness: number | null;
    health: Health;
    edges_ready: boolean;
  };
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

export interface CallersInput {
  symbol: string;
  path?: string;
  max_depth?: number;
  limit?: number;
}

export type CallersOutput = {
  direct: { caller_symbol: string; caller_path: string; line: number; preview: string; edge_confidence: EdgeConfidence; edge_kind: string }[];
  total_direct: number;
  truncated: boolean;
  transitive_count?: number | null;
  transitive_capped?: boolean;
  edges_ready: boolean;
  note: string;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

export interface CalleesInput {
  symbol: string;
  path?: string;
  max_depth?: number;
  limit?: number;
}

export type CalleesOutput = {
  direct: { callee_symbol: string; callee_path: string; line: number; preview: string; edge_confidence: EdgeConfidence; edge_kind: string }[];
  total_direct: number;
  truncated: boolean;
  transitive_count?: number | null;
  transitive_capped?: boolean;
  edges_ready: boolean;
  note: string;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

export interface DependenciesInput {
  path: string;
}

export interface DependenciesOutput {
  imports: { module: string; resolved_path?: string; symbols_used: string[] }[];
  imports_total: number;
  imports_truncated: boolean;
  imported_by: { path: string; symbols_used: string[] }[];
  imported_by_total: number;
  imported_by_truncated: boolean;
  edges_ready: boolean;
  note: string;
  suggested_next?: SuggestedNext;
}

export interface PathInput {
  from_symbol: string;
  from_path?: string;
  to_symbol: string;
  to_path?: string;
  max_paths?: number;
  max_hops?: number;
  timeout_ms?: number;
}

export type PathOutput = {
  exists: boolean | null;
  direction: "from→to";
  routes: { steps: { symbol: string; path: string; line_start: number; line_end: number; kind: string; edge_confidence?: EdgeConfidence }[]; length: number }[];
  total_found: number;
  truncated: boolean;
  terminated_by: null | "path_count" | "max_hops" | "timeout";
  hops_clamped: boolean;
  edges_ready: boolean;
  note: string;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

export interface EditContextInput {
  symbol: string;
  path?: string;
}

export type EditContextOutput = {
  target: { signature: string; source: string; path: string; line_start: number; line_end: number; data_source: "disk" };
  callers: { symbol: string; signature: string; path: string; line: number; edge_confidence: EdgeConfidence }[];
  callers_truncated: boolean;
  callers_total: number;
  caller_selection: "priority_ranked";
  callees: { symbol: string; signature: string; path: string; line: number; edge_confidence: EdgeConfidence }[];
  callees_truncated: boolean;
  callees_total: number;
  callee_selection: "priority_ranked";
  blast_radius: { direct_callers: number; transitive_callers: number; files_affected: number };
  risk_assessment: { level: "low" | "medium" | "high" | "critical"; reasons: string[] };
  edges_ready: boolean;
  index_freshness: { last_sync_ms: number; pending_files: number; stale_callers: boolean };
  /**
   * Notes saved via `remember` that reference this symbol's file, surfaced
   * automatically (no separate `recall` call needed). Empty is the common
   * case. On a hub file, a note only qualifies if its text mentions this
   * symbol's name (`specificity: "symbol"`) — a plain file-level match
   * (`specificity: "file"`) is only used on non-hub files, to avoid one
   * stale note burying every symbol in a large/important file. A note
   * whose text trips the same prompt-injection heuristic `source`'s
   * `content_warning` uses is dropped from this automatic surface (still
   * visible via an explicit `recall` call).
   */
  related_notes: RelatedNoteOutput[];
  note: string;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;


export interface EditHunkInput {
  start_line: number;
  end_line: number;
  /** Omit to preview this range's hash/content instead of writing. */
  expected_hash?: string;
  new_text: string;
}

export interface EditLinesInput {
  path: string;
  /** Must be disjoint (non-overlapping); applied bottom-up in one call. */
  edits: EditHunkInput[];
  /**
   * Required `true` when any touched range falls inside a `risk_assessment:
   * "high"` symbol or one with `is_hub: true`. On its own this is no longer
   * sufficient for such a touch — see `reason`.
   */
  confirm?: boolean;
  /**
   * Required (non-empty, and referencing a real caller `edit_context`
   * returned for the touched symbol) when touching a hub/high-risk symbol;
   * ignored otherwise. `edit_context` must also have been called for that
   * symbol THIS session — a stale/never-run review rejects with
   * `EDIT_CONTEXT_REQUIRED` before `reason`/`confirm` are even checked. See
   * `UnifiedError`'s `EDIT_CONTEXT_REQUIRED`/`CONFIRM_REQUIRED`/
   * `REASON_NOT_GROUNDED` codes for the 3-layer gate this backs.
   */
  reason?: string;
}

export interface EditSymbolInput {
  /** Bare symbol name (not a `path::name` qualified name). */
  symbol: string;
  path?: string;
  line?: number;
  /** Ignored by insertion `position` modes, which anchor/hash themselves. */
  expected_hash?: string;
  new_text: string;
  position?: "replace" | "before" | "after" | "append_inside";
  /** Same gate as `EditLinesInput.confirm` — see there. */
  confirm?: boolean;
  /** Same gate as `EditLinesInput.reason` — see there. */
  reason?: string;
}

export interface TouchedSymbolOutput {
  qualified_name: string;
  caller_count: number;
  is_hub: boolean;
}

export interface EditHunkResultOutput {
  start_line: number;
  end_line: number;
  status: "applied" | "preview" | "conflict";
  /** Hash of the range's content before this call — retry with this as `expected_hash`. */
  current_hash: string;
  old_text: string;
  /** Only present when `status == "applied"`. */
  new_end_line?: number;
  /** Present when this range's pre-edit content is byte-identical to N other line windows of the file — a hash match proves content, not position. */
  other_matches?: number;
}

/**
 * `edit_lines` returns this directly; `edit_symbol` returns
 * `EditLinesOutput | AmbiguousResult` (name resolution can be ambiguous).
 */
export interface EditLinesOutput {
  path: string;
  applied: boolean;
  hunks: EditHunkResultOutput[];
  /** "clean" | "skipped_unrecognized_language" — absent when nothing was written. */
  parse_status?: string | null;
  /** Symbols overlapping the touched ranges (post-edit positions once applied). */
  touched_symbols: TouchedSymbolOutput[];
  risk_assessment?: string | null;
  /** `true` only when `applied` but the post-write index refresh failed — the file on disk is correct, do NOT re-apply. */
  index_stale?: boolean | null;
  note?: string | null;
  suggested_next?: SuggestedNext;
}


export interface PatternDebtRegisterInput {
  /** Bare symbol name (not a `path::name` qualified name). */
  symbol: string;
  path?: string;
  line?: number;
  /** Free-text description of the bug/duplication pattern this anchor tracks. */
  note: string;
}

export type PatternDebtRegisterOutput = {
  /** Stable id for this anchor — pass to `pattern_debt_status(topic)`. Always equal to `anchor_qualified_name` at registration time. */
  topic: string;
  anchor_qualified_name: string;
  /** Similar-instance count at registration time (excludes the anchor itself), from `search(kind="similar")` above a fixed similarity threshold. */
  baseline_count: number;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

export interface PatternDebtStatusInput {
  /** Check one anchor by its topic. Omit to check every currently-`open` anchor instead (capped — see the output's `truncated`). */
  topic?: string;
}

export interface PatternDebtLocationOutput {
  qualified_name: string;
  path: string;
  line_start: number | null;
  score: number;
}

export interface PatternDebtEntryOutput {
  topic: string;
  anchor_qualified_name: string;
  note: string;
  baseline_count: number;
  /**
   * `"open"` (similar instances still found) | `"resolved"` (none found
   * this check) | `"anchor_lost"` (the symbol was renamed/removed/split
   * since registration — never silently reported as resolved), or a
   * `"<status> (check_unavailable_this_run)"` suffix when this particular
   * check couldn't run (e.g. embeddings not ready) — the persisted status
   * shown is what it was *before* this call, never guessed.
   */
  status: string;
  /** Similar-instance count from this check. `null`/absent when the anchor is lost or the check was unavailable. */
  current_count?: number | null;
  remaining_locations: PatternDebtLocationOutput[];
  checked_at: string;
}

export interface PatternDebtStatusOutput {
  entries: PatternDebtEntryOutput[];
  /** `true` when more than the cap (30) of open entries exist — re-run with an explicit `topic` to check the rest. */
  truncated: boolean;
  suggested_next?: SuggestedNext;
}

export interface SessionContextInput {}

export interface SessionContextOutput {
  explored: {
    symbols: { name: string; path: string; caller_count: number | null; is_hub: boolean | null }[];
    symbols_total: number;
    symbols_truncated: boolean;
    files: string[];
    files_total: number;
  };
  frontier: { path: string; reason: string; connection_count: number }[];
  frontier_degraded: boolean;
  frontier_note: string;
  already_fetched: { symbol?: string; path: string; line_start: number; line_end: number }[];
  session_stats: { tool_calls: number; unique_files_explored: number };
  session_started_at: string;
  suggested_next?: SuggestedNext;
}

export interface DiffImpactInput {
  diff?: string;
  staged?: boolean;
  commits?: string;
}

export interface DiffImpactOutput {
  affected_symbols: {
    symbol: string;
    path: string;
    line_start: number;
    line_end: number;
    kind: string;
    change_type: "modified" | "added" | "deleted" | "renamed";
    signature_changed: boolean;
    risk_assessment: { level: "low" | "medium" | "high" | "critical"; reasons: string[] };
  }[];
  affected_symbols_total: number;
  affected_symbols_truncated: boolean;
  aggregate_risk: "low" | "medium" | "high" | "critical" | "unknown";
  blast_radius: { direct_callers: number; transitive_callers: number; files_affected: number };
  high_risk_callers: { symbol: string; path: string; line: number; reason: string; edge_confidence: EdgeConfidence }[];
  high_risk_callers_truncated: boolean;
  edges_ready: boolean;
  edge_confidence_note?: string;
  unindexed_files: string[];
  suggested_reviewers?: ReviewerSuggestion[];
  note?: string;
  suggested_next?: SuggestedNext;
}

export interface IndexingStatusInput {
  retry_embeddings?: boolean;
}

export interface IndexingStatusOutput {
  phase: "scanning" | "parsing" | "building_edges" | "ready";
  edges_ready: boolean;
  embeddings_status: "disabled" | "downloading" | "embedding" | "ready" | "failed" | "offline_unavailable";
  embeddings_error?: { reason: "download_failed" | "model_corrupt" | "oom" | "embed_failed"; message: string; retry_count: number };
  stats: {
    files_indexed: number;
    files_total: number;
    symbols_indexed: number | null;
    edges_indexed: number | null;
  };
  last_updated: string;
  suggested_next?: SuggestedNext;
}

export interface LocateInput {
  query: string;
  kind?: "symbol" | "text" | "file" | "semantic" | "hybrid";
  limit?: number;
  depth?: "search_only" | "with_file" | "with_symbol";
}

export interface LocateOutput {
  results: SearchOutput["results"];
  truncated: boolean;
  degraded: boolean;
  suggestions?: string[];
  depth_adjusted?: "with_file" | "search_only";
  top_result?: {
    file: {
      language: string;
      inferred_role: "service" | "model" | "router" | "utility" | "test" | "config" | null;
      symbols: {
        name: string;
        kind: string;
        signature: string;
        line_start: number;
        line_end: number;
        caller_count: number | null;
        is_hub: boolean | null;
        coreness: number | null;
      }[];
      total_symbols: number;
      file_truncated: boolean;
    };
    symbol?: SymbolInfoOutput;
  };
  edges_ready: boolean;
  /** Notes referencing `top_result.symbol`'s file — same rules as `EditContextOutput.related_notes`. Empty when there's no top result or no matching notes. */
  related_notes: RelatedNoteOutput[];
  note?: string;
  embeddings_status?: "disabled" | "downloading" | "embedding" | "ready" | "failed";
  suggested_next?: SuggestedNext;
}

export interface HotspotsInput {
  top_n?: number;
  since?: string;
  min_churn?: number;
  include_symbols?: boolean;
}

export interface HotspotsOutput {
  hotspots: {
    path: string;
    language: string;
    churn: {
      commit_count: number;
      unique_authors: number;
      last_changed: string | null;
    };
    complexity: {
      symbol_count: number;
      hub_count: number;
      connected_coreness_count: number;
      avg_caller_count: number;
    };
    hotspot_score: number;
    risk_level: "low" | "medium" | "high" | "critical";
    top_symbols?: {
      name: string;
      kind: string;
      is_hub: boolean;
      coreness: number;
      caller_count: number | null;
    }[];
  }[];
  git_available: boolean;
  since: string;
  total_files_analyzed: number;
  hotspot_method: "git+index" | "index_only";
  note: string;
  suggested_next?: SuggestedNext;
}

export interface UnderstandInput {
  query: string;
  kind?: "symbol" | "hybrid";
}

/**
 * "found"    → name/path and other result fields are populated; ambiguous is absent.
 * "ambiguous" → ambiguous field is populated; all other result fields are absent.
 * Absent status (legacy) → consumers should infer from presence of `ambiguous` field.
 */
export interface UnderstandOutput {
  status?: "found" | "ambiguous";
  name?: string;
  path?: string;
  kind?: string;
  language?: string;
  signature?: string;
  docstring?: string;
  is_hub?: boolean;
  coreness?: number | null;
  health?: Health;
  source?: string;
  line_start?: number;
  line_end?: number;
  callers_summary?: {
    name: string;
    path: string;
    edge_confidence: EdgeConfidence;
    line: number;
  }[];
  total_callers?: number;
  edges_ready?: boolean;
  ambiguous?: AmbiguousResult;
  suggested_next?: SuggestedNext;
}

/**
 * AMBIGUOUS is intentionally absent: ambiguity is always resolved in-band via
 * `ambiguous: true` fields in each tool's output (AmbiguousResult union).
 * UnifiedError with code AMBIGUOUS is never emitted by any tool.
 */
export interface UnifiedError {
  error: {
    /**
     * Illustrative, not exhaustive — this file mirrors a subset of tools
     * (see the comment at the top for full coverage caveats). Codes shown
     * here span the general-purpose ones plus the `edit_lines`/`edit_symbol`
     * write-gate-specific ones (`CONFIRM_REQUIRED`, `EDIT_CONTEXT_REQUIRED`,
     * `REASON_NOT_GROUNDED`) and `pattern_debt_register`'s
     * `EMBEDDINGS_NOT_READY`.
     */
    code:
      | "NOT_FOUND"
      | "INDEX_PARTIAL"
      | "PARSE_FAILED"
      | "TIMEOUT"
      | "DB_LOCKED"
      | "INVALID_INPUT"
      | "FEATURE_UNAVAILABLE"
      | "EMBEDDING_FAILED"
      | "CONFIRM_REQUIRED"
      | "EDIT_CONTEXT_REQUIRED"
      | "REASON_NOT_GROUNDED"
      | "EMBEDDINGS_NOT_READY";
    message: string;
    recoverable: boolean;
    suggestions?: string[];
  };
}

export interface ConfigJson {
  preset: string;
  languages: string[];
  ignore: string[];
  entry_points: string[];
  hub_threshold: {
    top_pct: number;
    min_callers: number;
    min_callers_bridge: number;
    coreness_pct: number;
  };
  semantic_search: {
    enabled: boolean;
    model: string;
    dimensions: number;
    index_on_startup: boolean;
  };
  search: {
    text_chunk_context_lines: number;
    text_max_chunk_lines: number;
    rrf_k: number;
  };
  path: {
    default_max_hops: number;
    max_allowed_hops: number;
    timeout_ms: number;
  };
  callers: { max_depth_cap: number; transitive_timeout_ms: number };
  callees: { max_depth_cap: number; transitive_timeout_ms: number };
  dependencies: { max_imports: number; max_imported_by: number };
  edit_context: { max_callers: number; max_callees: number };
  diff_impact: { max_high_risk_callers: number; max_affected_symbols: number };
  session_context: { max_explored_symbols_in_response: number };
  hotspots: {
    default_top_n: number;
    default_since: string;
    default_min_churn: number;
    risk_critical_threshold: number;
    risk_high_threshold: number;
    risk_medium_threshold: number;
  };
}
