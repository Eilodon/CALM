from __future__ import annotations
from dataclasses import dataclass, field
from typing import Any


PRESET_TOOL_SETS: dict[str, set[str] | None] = {
    "orient":   {"repo_overview", "locate", "dependencies", "hotspots", "indexing_status"},
    "trace":    {"repo_overview", "locate", "callers", "callees", "path", "session_context"},
    "edit":     {"repo_overview", "locate", "source", "edit_context", "diff_impact", "indexing_status"},
    "compound": {"repo_overview", "locate", "hotspots", "source", "understand",
                 "edit_context", "diff_impact", "session_context", "indexing_status"},
    "full":     None,  # None = all tools
}


@dataclass
class SuggestedNext:
    tool: str
    reason: str
    args: dict[str, str | int | bool] = field(default_factory=dict)


def _raw_suggested_next(tool_name: str, output: dict[str, Any]) -> SuggestedNext | None:
    """
    Pure function — no side effects, no I/O. First-match-wins within each tool.

    `output` is the tool's response dict, optionally enriched with private
    `_*` keys for input params not reflected in the public output schema:
      _kind             (search):  query kind passed to the tool
      _target           (source):  the target symbol/path that was fetched
      _include_metadata (source):  whether include_metadata=True was requested
      _max_hops         (path):    the max_hops value that was used
      _from_symbol      (path):    original from_symbol arg
      _to_symbol        (path):    original to_symbol arg

    Caller (compute_suggested_next) handles preset filtering.
    """
    def h(tool: str, reason: str, args: dict | None = None) -> SuggestedNext:
        return SuggestedNext(tool=tool, reason=reason, args=args or {})

    match tool_name:

        case "repo_overview":
            # Condition 1 beats 2: embeddings retry only meaningful once phase=ready
            if output.get("indexing_phase") != "ready":
                return h("indexing_status",
                         "Monitor until phase=ready before using graph tools")
            if output.get("embeddings_status") == "failed":
                return h("indexing_status", "Recover embeddings",
                         {"retry_embeddings": True})
            return h("locate", "Start exploration")

        case "search":
            results = output.get("results", [])
            kind = output.get("_kind", "symbol")
            if results and kind == "symbol":
                name = results[0].get("name", "")
                return h("locate",
                         "Full context in 1 call (replaces symbol_info)",
                         {"query": name, "kind": "symbol"})
            if not results:
                if kind == "semantic":
                    return h("search",
                             "Semantic index may not cover this — try text or hybrid search",
                             {"kind": "text"})
                if kind == "hybrid":
                    return h("search",
                             "Embeddings may not cover this query — try exact text search "
                             "or broaden wording",
                             {"kind": "text"})
                # text, file, or unrecognised → try hybrid for broader recall
                return h("search", "Try hybrid for broader recall", {"kind": "hybrid"})
            return None

        case "locate":
            results = output.get("results", [])
            top = output.get("top_result") or {}
            sym = top.get("symbol") or {}
            if not results and not top:
                return h("search", "No match — broaden with hybrid search",
                         {"kind": "hybrid"})
            # sym may be AmbiguousResult: {"ambiguous": True, "candidates": [...]}
            if sym.get("ambiguous"):
                candidates = sym.get("candidates", [])
                if candidates:
                    return h("symbol_info", "Disambiguate top result",
                             {"name": candidates[0]["name"],
                              "path": candidates[0]["path"]})
            if sym.get("is_hub"):
                return h("edit_context", "Hub detected — mandatory pre-edit check",
                         {"symbol": sym.get("name", ""), "path": sym.get("path", "")})
            health = sym.get("health") or {}
            if health.get("dead_code_confidence") == "high":
                return h("callers", "Verify dead code — no static callers found",
                         {"symbol": sym.get("name", "")})
            target = (sym.get("name")
                      or (results[0].get("name") if results else None)
                      or (results[0].get("path") if results else ""))
            return h("source", "Read implementation", {"target": target})

        case "symbol_info":
            if output.get("is_hub"):
                return h("edit_context", "Hub — check blast radius before modifying",
                         {"symbol": output.get("name", ""),
                          "path": output.get("path", "")})
            health = output.get("health") or {}
            if health.get("test_files") == []:
                return h("search", "No tests found — search for coverage",
                         {"query": output.get("name", "") + " test", "kind": "text"})
            return h("source", "Read implementation",
                     {"target": output.get("name", "")})

        case "source":
            # is_hub hint only fires when include_metadata=True was requested
            if output.get("_include_metadata"):
                meta = output.get("metadata") or {}
                if meta.get("is_hub"):
                    return h("edit_context", "Hub — mandatory pre-edit context")
            target = output.get("_target", output.get("path", ""))
            return h("callers", "Check who uses this before modifying",
                     {"symbol": target})

        case "callers":
            direct = output.get("direct", [])
            total = output.get("total_direct", 0)
            has_textual = any(e.get("edge_confidence") == "textual" for e in direct)
            if has_textual or total > 10:
                return h("edit_context",
                         "High blast radius or uncertain edges — verify before modifying")
            if 0 < total <= 10 and not has_textual and direct:
                return h("source", "Read top caller implementation",
                         {"target": direct[0]["caller_symbol"]})
            return None

        case "callees":
            if output.get("total_direct", 0) > 0:
                return h("path", "Trace specific call chain")
            return None

        case "dependencies":
            if output.get("imported_by_total", 0) > 20:
                return h("callers", "High fan-in — check symbol blast radius")
            return None

        case "path":
            if output.get("exists"):
                return h("source", "Read meeting node implementation")
            terminated_by = output.get("terminated_by")
            if terminated_by == "timeout":
                # Literal 4 (not relative): small fixed budget to avoid re-timeout
                return h("path", "Retry with smaller max_hops", {"max_hops": 4})
            if terminated_by == "max_hops":
                cur = int(output.get("_max_hops") or 10)
                # Swap direction: BFS can be asymmetric — reverse may find a path
                # the forward direction couldn't within the same hop budget.
                from_sym = output.get("_to_symbol", "")
                to_sym = output.get("_from_symbol", "")
                args: dict[str, Any] = {"max_hops": cur + 4}
                if from_sym:
                    args["from_symbol"] = from_sym
                if to_sym:
                    args["to_symbol"] = to_sym
                return h("path",
                         "Path may exceed hop limit — retry with larger max_hops, "
                         "or check the reverse direction",
                         args)
            return None

        case "edit_context":
            return h("diff_impact", "MANDATORY after changes — verify blast radius")

        case "session_context":
            frontier = output.get("frontier", [])
            if frontier:
                return h("file_overview", "Explore top frontier file",
                         {"path": frontier[0]["path"]})
            return h("repo_overview", "Frontier exhausted — refresh map")

        case "diff_impact":
            # unindexed beats risk: aggregate_risk may be under-estimated when
            # there are unindexed files — resolve index state first.
            unindexed = output.get("unindexed_files", [])
            risk = output.get("aggregate_risk", "unknown")
            affected = output.get("affected_symbols", [])
            if unindexed:
                return h("indexing_status", "Wait for index before treating as safe")
            if risk in ("critical", "high"):
                sym = affected[0]["symbol"] if affected else ""
                return h("callers", "Verify high-risk callers manually",
                         {"symbol": sym})
            if risk == "medium":
                sym = affected[0]["symbol"] if affected else ""
                return h("callers", "Medium-risk changes — spot-check key callers",
                         {"symbol": sym})
            if risk == "unknown":
                return h("indexing_status", "Risk unknown — check index state")
            # risk == "low" AND all indexed → no hint (explicit per spec)
            return None

        case "hotspots":
            hotspots = output.get("hotspots", [])
            if hotspots:
                return h("file_overview", "Inspect highest-risk file",
                         {"path": hotspots[0]["path"]})
            return None

        case "understand":
            # ambiguous field is AmbiguousResult: {"ambiguous": True, "candidates": [...]}
            ambiguous = output.get("ambiguous") or {}
            candidates = ambiguous.get("candidates", [])
            if candidates:
                return h("symbol_info", "Ambiguous — retry with specific candidate",
                         {"name": candidates[0]["name"],
                          "path": candidates[0]["path"]})
            if output.get("is_hub"):
                return h("edit_context", "Hub — mandatory pre-edit check",
                         {"symbol": output.get("name", ""),
                          "path": output.get("path", "")})
            return h("edit_context", "Pre-edit: verify blast radius before modifying",
                     {"symbol": output.get("name", ""),
                      "path": output.get("path", "")})

        case "file_overview":
            symbols = output.get("symbols", [])
            hub = next((s for s in symbols if s.get("is_hub")), None)
            if hub:
                return h("locate", "Inspect hub symbol", {"query": hub["name"]})
            return h("source", "Read a symbol implementation")

        case "indexing_status":
            if output.get("phase") == "ready":
                return h("locate", "Index ready — begin exploration")
            return h("indexing_status",
                     "Still indexing — poll again or use search/source while edges build")

    return None


def compute_suggested_next(
    tool_name: str,
    output: dict,
    available_tools: set[str] | None = None,
) -> SuggestedNext | None:
    hint = _raw_suggested_next(tool_name, output)
    if hint is None:
        return None
    if available_tools is not None and hint.tool not in available_tools:
        return None
    return hint


# ---------------------------------------------------------------------------
# Tool handler stubs
# Replace each body with real logic wired to the indexer backend.
# Each handler receives the raw params dict and must return the output dict.
# Handlers inject private `_*` keys (see _raw_suggested_next docstring) so
# that compute_suggested_next can read input params it needs; _make_tool_fn
# strips those keys before the response reaches the MCP client.
# ---------------------------------------------------------------------------

async def _handle_repo_overview(params: dict, *, ctx: Any) -> dict:
    """RepoOverviewInput → RepoOverviewOutput"""
    raise NotImplementedError("repo_overview handler not wired")


async def _handle_search(params: dict, *, ctx: Any) -> dict:
    """SearchInput → SearchOutput  (injects _kind)"""
    raise NotImplementedError("search handler not wired")


async def _handle_file_overview(params: dict, *, ctx: Any) -> dict:
    """FileOverviewInput → FileOverviewOutput"""
    raise NotImplementedError("file_overview handler not wired")


async def _handle_symbol_info(params: dict, *, ctx: Any) -> dict:
    """SymbolInfoInput → SymbolInfoOutput | AmbiguousResult"""
    raise NotImplementedError("symbol_info handler not wired")


async def _handle_source(params: dict, *, ctx: Any) -> dict:
    """SourceInput → SourceOutput | AmbiguousResult  (injects _target, _include_metadata)"""
    raise NotImplementedError("source handler not wired")


async def _handle_callers(params: dict, *, ctx: Any) -> dict:
    """CallersInput → CallersOutput | AmbiguousResult"""
    raise NotImplementedError("callers handler not wired")


async def _handle_callees(params: dict, *, ctx: Any) -> dict:
    """CalleesInput → CalleesOutput | AmbiguousResult"""
    raise NotImplementedError("callees handler not wired")


async def _handle_dependencies(params: dict, *, ctx: Any) -> dict:
    """DependenciesInput → DependenciesOutput"""
    raise NotImplementedError("dependencies handler not wired")


async def _handle_path(params: dict, *, ctx: Any) -> dict:
    """PathInput → PathOutput | AmbiguousResult  (injects _max_hops, _from_symbol, _to_symbol)"""
    raise NotImplementedError("path handler not wired")


async def _handle_edit_context(params: dict, *, ctx: Any) -> dict:
    """EditContextInput → EditContextOutput | AmbiguousResult"""
    raise NotImplementedError("edit_context handler not wired")


async def _handle_session_context(params: dict, *, ctx: Any) -> dict:
    """SessionContextInput → SessionContextOutput"""
    raise NotImplementedError("session_context handler not wired")


async def _handle_diff_impact(params: dict, *, ctx: Any) -> dict:
    """DiffImpactInput → DiffImpactOutput"""
    raise NotImplementedError("diff_impact handler not wired")


async def _handle_indexing_status(params: dict, *, ctx: Any) -> dict:
    """IndexingStatusInput → IndexingStatusOutput"""
    raise NotImplementedError("indexing_status handler not wired")


async def _handle_locate(params: dict, *, ctx: Any) -> dict:
    """LocateInput → LocateOutput"""
    raise NotImplementedError("locate handler not wired")


async def _handle_hotspots(params: dict, *, ctx: Any) -> dict:
    """HotspotsInput → HotspotsOutput"""
    raise NotImplementedError("hotspots handler not wired")


async def _handle_understand(params: dict, *, ctx: Any) -> dict:
    """UnderstandInput → UnderstandOutput"""
    raise NotImplementedError("understand handler not wired")


_PRIVATE_KEYS = frozenset({
    "_kind", "_target", "_include_metadata",
    "_max_hops", "_from_symbol", "_to_symbol",
})


def _make_tool_fn(
    handler,
    name: str,
    available_tools: set[str] | None,
):
    """Wrap a handler: inject suggested_next, strip private context keys."""
    async def tool_fn(params: dict, *, ctx: Any = None) -> dict:
        output: dict = await handler(params, ctx=ctx)
        suggestion = compute_suggested_next(name, output, available_tools)
        for k in _PRIVATE_KEYS:
            output.pop(k, None)
        if suggestion is not None:
            sn: dict[str, Any] = {"tool": suggestion.tool, "reason": suggestion.reason}
            if suggestion.args:
                sn["args"] = suggestion.args
            output["suggested_next"] = sn
        return output
    return tool_fn


ALL_TOOLS: dict[str, Any] = {
    "repo_overview":   _handle_repo_overview,
    "search":          _handle_search,
    "file_overview":   _handle_file_overview,
    "dependencies":    _handle_dependencies,
    "symbol_info":     _handle_symbol_info,
    "source":          _handle_source,
    "callers":         _handle_callers,
    "callees":         _handle_callees,
    "path":            _handle_path,
    "edit_context":    _handle_edit_context,
    "session_context": _handle_session_context,
    "diff_impact":     _handle_diff_impact,
    "indexing_status": _handle_indexing_status,
    "locate":          _handle_locate,
    "hotspots":        _handle_hotspots,
    "understand":      _handle_understand,
}


def register_tools(mcp_server, preset: str = "full") -> None:
    if preset not in PRESET_TOOL_SETS:
        valid = list(PRESET_TOOL_SETS.keys())
        raise ValueError(
            f"[codeindex] Unknown preset: {preset!r}. Valid options: {valid}"
        )
    allowed = PRESET_TOOL_SETS[preset]
    for tool_name, handler in ALL_TOOLS.items():
        if allowed is None or tool_name in allowed:
            mcp_server.register_tool(
                tool_name, _make_tool_fn(handler, tool_name, allowed)
            )


def init_server(project_root):
    import codeindex.codeowners as codeowners
    from codeindex.coverage_reader import CoverageReader

    codeowners_patterns = codeowners.load_codeowners(project_root)
    if codeowners_patterns:
        print(f"[codeindex] CODEOWNERS loaded: {len(codeowners_patterns)} patterns")

    coverage_data = CoverageReader.load(project_root)
    if coverage_data.source != "none":
        print(f"[codeindex] Coverage data loaded: {coverage_data.source}")

    return codeowners_patterns, coverage_data
