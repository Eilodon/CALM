from __future__ import annotations

from typing import Any, ClassVar, Literal

from pydantic import BaseModel, ConfigDict, model_serializer


# ---------------------------------------------------------------------------
# Base
# ---------------------------------------------------------------------------

class _BaseOutput(BaseModel):
    model_config = ConfigDict(populate_by_name=True)
    _absent_when_none: ClassVar[frozenset[str]] = frozenset()

    @model_serializer(mode="wrap")
    def _serialize(self, handler) -> dict:
        d = handler(self)
        for field in self._absent_when_none:
            if d.get(field) is None:
                d.pop(field, None)
        return d


# ---------------------------------------------------------------------------
# Unified Error
# ---------------------------------------------------------------------------

class ErrorBody(BaseModel):
    code: Literal[
        "NOT_FOUND", "INDEX_PARTIAL", "PARSE_FAILED", "TIMEOUT",
        "DB_LOCKED", "INVALID_INPUT", "FEATURE_UNAVAILABLE", "EMBEDDING_FAILED",
    ]
    message: str
    recoverable: bool
    suggestions: list[str] | None = None

    @model_serializer(mode="wrap")
    def _serialize(self, handler) -> dict:
        d = handler(self)
        if d.get("suggestions") is None:
            d.pop("suggestions", None)
        return d


class UnifiedError(BaseModel):
    error: ErrorBody


# ---------------------------------------------------------------------------
# Shared sub-schemas
# ---------------------------------------------------------------------------

class Health(BaseModel):
    dead_code_confidence: Literal["none", "low", "medium", "high"]
    dead_code_source: Literal["static", "static+coverage"]
    caller_count_by_confidence: dict[str, int] | None = None
    test_files: list[str]


class AmbiguousCandidate(BaseModel):
    name: str
    path: str
    kind: str
    line_start: int
    line_end: int
    class_context: str | None = None
    caller_count: int | None = None
    language: str | None = None
    signature: str | None = None


class AmbiguousResult(BaseModel):
    ambiguous: bool = True
    candidates: list[AmbiguousCandidate]


class SuggestedNextSchema(BaseModel):
    tool: str
    reason: str
    args: dict[str, Any] | None = None


class ModuleMapEntry(BaseModel):
    path: str
    language: str
    symbol_count: int
    hub_count: int
    inferred_role: str | None = None


class EntryPoint(BaseModel):
    name: str
    path: str
    kind: str
    line_start: int


class RepoStats(BaseModel):
    total_symbols: int | None = None
    total_files: int
    total_edges: int | None = None
    hub_count: int | None = None


class HealthSummary(BaseModel):
    undocumented_hubs: int
    high_dead_code: int


class ReviewerSuggestion(BaseModel):
    name: str
    source: str
    files: list[str]


# ---------------------------------------------------------------------------
# Tool 1: repo_overview
# ---------------------------------------------------------------------------

class RepoOverviewOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "note", "health_summary",
    })
    languages: list[str]
    indexing_phase: Literal["scanning", "parsing", "building_edges", "ready"]
    embeddings_status: Literal["disabled", "downloading", "embedding", "ready", "failed"]
    module_map: list[ModuleMapEntry]
    total_modules: int
    truncated: bool
    entry_points: list[EntryPoint]
    stats: RepoStats
    workflow_guide: str
    health_summary: HealthSummary | None = None
    note: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 2: search
# ---------------------------------------------------------------------------

class SearchInput(BaseModel):
    query: str
    kind: Literal["symbol", "text", "file", "semantic", "hybrid"] = "symbol"
    limit: int = 10


class SearchResult(BaseModel):
    name: str
    path: str
    kind: str | None = None
    line_start: int | None = None
    line_end: int | None = None
    score: float | None = None
    match_type: str | None = None
    snippet: str | None = None


class SearchOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "note", "suggestions",
    })
    results: list[SearchResult]
    truncated: bool
    degraded: bool = False
    suggestions: list[str] | None = None
    note: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 3: file_overview
# ---------------------------------------------------------------------------

class FileOverviewInput(BaseModel):
    path: str


class FileOverviewSymbol(BaseModel):
    name: str
    qualified_name: str
    kind: str
    line_start: int
    line_end: int
    signature: str
    is_hub: bool = False
    caller_count: int = 0


class FileOverviewOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "inferred_role",
    })
    path: str
    language: str | None = None
    symbols: list[FileOverviewSymbol]
    symbol_count: int
    inferred_role: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 4: symbol_info
# ---------------------------------------------------------------------------

class SymbolInfoInput(BaseModel):
    name: str
    path: str | None = None


class SymbolInfoOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "coreness", "class_context",
    })
    name: str
    qualified_name: str
    path: str
    kind: str
    language: str
    line_start: int
    line_end: int
    signature: str
    docstring: str
    caller_count: int
    is_hub: bool
    coreness: int | None = None
    is_entry_point: bool
    edges_ready: bool
    health: Health
    class_context: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 5: source
# ---------------------------------------------------------------------------

class SourceInput(BaseModel):
    target: str
    include_metadata: bool = False
    context_lines: int = 10


class SourceMetadata(BaseModel):
    name: str
    qualified_name: str
    kind: str
    is_hub: bool
    caller_count: int
    coreness: int | None = None


class SourceOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "metadata",
    })
    path: str
    language: str | None = None
    content: str
    line_start: int
    line_end: int
    metadata: SourceMetadata | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 6: callers
# ---------------------------------------------------------------------------

class CallersInput(BaseModel):
    symbol: str
    path: str | None = None
    limit: int = 10
    max_depth: int = 1


class CallerEntry(BaseModel):
    caller_symbol: str
    caller_path: str | None = None
    line: int | None = None
    edge_confidence: Literal["resolved", "inferred", "textual"]


class CallersOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "transitive_count", "transitive_capped",
    })
    symbol: str
    direct: list[CallerEntry]
    total_direct: int
    edges_ready: bool
    transitive_count: int | None = None
    transitive_capped: bool | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 7: callees
# ---------------------------------------------------------------------------

class CalleesInput(BaseModel):
    symbol: str
    path: str | None = None
    limit: int = 10
    max_depth: int = 1


class CalleeEntry(BaseModel):
    callee_symbol: str
    callee_path: str | None = None
    line: int | None = None
    edge_confidence: Literal["resolved", "inferred", "textual"]


class CalleesOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "transitive_count", "transitive_capped",
    })
    symbol: str
    direct: list[CalleeEntry]
    total_direct: int
    edges_ready: bool
    transitive_count: int | None = None
    transitive_capped: bool | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 8: dependencies
# ---------------------------------------------------------------------------

class DependenciesInput(BaseModel):
    path: str


class ImportEntry(BaseModel):
    module_name: str
    resolved_path: str | None = None
    symbols_used: list[str]


class ImportedByEntry(BaseModel):
    from_path: str
    symbols_used: list[str]


class DependenciesOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next",
    })
    path: str
    imports: list[ImportEntry]
    imported_by: list[ImportedByEntry]
    imports_total: int
    imported_by_total: int
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 9: path
# ---------------------------------------------------------------------------

class PathInput(BaseModel):
    from_symbol: str
    to_symbol: str
    from_path: str | None = None
    to_path: str | None = None
    max_hops: int | None = None
    max_paths: int = 3
    timeout_ms: int | None = None


class PathStep(BaseModel):
    symbol: str
    path: str | None = None
    edge_confidence: Literal["resolved", "inferred", "textual"] | None = None


class PathRoute(BaseModel):
    steps: list[PathStep]
    length: int


class PathOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "terminated_by", "exists",
    })
    from_symbol: str
    to_symbol: str
    exists: bool | None = None
    routes: list[PathRoute]
    total_found: int
    hops_clamped: bool = False
    terminated_by: Literal["timeout", "max_hops", "path_count"] | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 10: edit_context
# ---------------------------------------------------------------------------

class EditContextInput(BaseModel):
    symbol: str
    path: str | None = None


class EditContextCallerEntry(BaseModel):
    caller_symbol: str
    caller_path: str | None = None
    line: int | None = None
    edge_confidence: Literal["resolved", "inferred", "textual"]
    is_test: bool = False


class EditContextCalleeEntry(BaseModel):
    callee_symbol: str
    callee_path: str | None = None
    line: int | None = None
    edge_confidence: Literal["resolved", "inferred", "textual"]


class EditContextOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "risk_assessment",
    })
    symbol: str
    path: str
    kind: str
    is_hub: bool
    caller_count: int
    callers: list[EditContextCallerEntry]
    callees: list[EditContextCalleeEntry]
    edges_ready: bool
    signature: str
    risk_assessment: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 11: session_context
# ---------------------------------------------------------------------------

class ExploredSection(BaseModel):
    symbols: list[dict]
    symbols_total: int
    symbols_truncated: bool
    files: list[str]
    files_total: int


class FrontierEntry(BaseModel):
    path: str
    reason: Literal[
        "imported_by_explored", "contains_callers_of_explored", "both",
    ]


class SessionStats(BaseModel):
    tool_calls: int
    unique_files_explored: int


class SessionContextOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next",
    })
    explored: ExploredSection
    frontier: list[FrontierEntry]
    frontier_degraded: bool = False
    already_fetched: list[dict]
    session_stats: SessionStats
    session_started_at: str
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 12: diff_impact
# ---------------------------------------------------------------------------

class DiffImpactInput(BaseModel):
    diff: str | None = None
    staged: bool | None = None
    commits: str | None = None


class AffectedSymbol(BaseModel):
    symbol: str
    path: str
    kind: str
    change_type: Literal["modified", "added", "deleted", "renamed"]
    signature_changed: bool
    caller_count: int
    is_hub: bool
    edge_confidence_note: str | None = None


class DiffImpactOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "note", "reviewers",
    })
    affected_symbols: list[AffectedSymbol]
    affected_symbols_total: int
    affected_symbols_truncated: bool = False
    aggregate_risk: Literal["low", "medium", "high", "critical", "unknown"]
    unindexed_files: list[str]
    reviewers: list[ReviewerSuggestion] | None = None
    note: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 13: indexing_status
# ---------------------------------------------------------------------------

class IndexingStatusInput(BaseModel):
    retry_embeddings: bool = False


class IndexingStatusOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "embed_error",
    })
    phase: Literal["scanning", "parsing", "building_edges", "ready"]
    files_indexed: int
    files_total: int
    symbols_indexed: int | None = None
    edges_indexed: int | None = None
    embeddings_status: Literal["disabled", "downloading", "embedding", "ready", "failed"]
    embed_error: dict | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 14: locate
# ---------------------------------------------------------------------------

class LocateInput(BaseModel):
    query: str
    kind: Literal["symbol", "text", "file", "semantic", "hybrid"] | None = None
    depth: Literal["search_only", "with_file", "with_symbol"] | None = None
    limit: int | None = None


class LocateTopResult(BaseModel):
    file: FileOverviewOutput | None = None
    symbol: SymbolInfoOutput | AmbiguousResult | None = None


class LocateOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "top_result", "depth_adjusted",
    })
    results: list[SearchResult]
    truncated: bool
    degraded: bool = False
    edges_ready: bool
    top_result: LocateTopResult | None = None
    depth_adjusted: str | None = None
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 15: hotspots
# ---------------------------------------------------------------------------

class HotspotsInput(BaseModel):
    top_n: int | None = None
    since: str | None = None
    min_churn: int | None = None
    include_symbols: bool = False


class HotspotEntry(BaseModel):
    path: str
    hotspot_score: float
    churn: int
    complexity: float
    risk_level: Literal["low", "medium", "high", "critical"]
    symbols: list[dict] | None = None


class HotspotsOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next",
    })
    hotspots: list[HotspotEntry]
    hotspot_method: Literal["git+index", "index_only"]
    note: str
    suggested_next: SuggestedNextSchema | None = None


# ---------------------------------------------------------------------------
# Tool 16: understand
# ---------------------------------------------------------------------------

class UnderstandInput(BaseModel):
    query: str
    kind: Literal["symbol", "text", "file", "semantic", "hybrid"] | None = None


class CallerSummaryEntry(BaseModel):
    caller_symbol: str
    caller_path: str | None = None
    edge_confidence: Literal["resolved", "inferred", "textual"]


class UnderstandOutput(_BaseOutput):
    _absent_when_none: ClassVar[frozenset[str]] = frozenset({
        "suggested_next", "ambiguous", "source", "callers_summary",
        "signature", "docstring", "health", "coreness",
    })
    status: Literal["found", "ambiguous", "not_found"]
    name: str | None = None
    path: str | None = None
    kind: str | None = None
    signature: str | None = None
    docstring: str | None = None
    source: str | None = None
    callers_summary: list[CallerSummaryEntry] | None = None
    is_hub: bool | None = None
    coreness: int | None = None
    edges_ready: bool = False
    health: Health | None = None
    ambiguous: AmbiguousResult | None = None
    suggested_next: SuggestedNextSchema | None = None
