export type EdgeConfidence = "formal" | "resolved" | "inferred" | "textual" | "ambiguous" | "unresolved";

export interface Health {
  has_docstring: boolean;
  test_files: string[];
  dead_code_confidence: "none" | "low" | "medium" | "high";
  dead_code_source: "static" | "static+coverage";
  caller_count_by_confidence: { resolved: number; inferred: number; textual: number } | null;
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
  note: string;
  suggested_next?: SuggestedNext;
} | AmbiguousResult;

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
    code: "NOT_FOUND" | "INDEX_PARTIAL" | "PARSE_FAILED" | "TIMEOUT" | "DB_LOCKED" | "INVALID_INPUT" | "FEATURE_UNAVAILABLE" | "EMBEDDING_FAILED";
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
