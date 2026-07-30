#!/usr/bin/env python3
"""B3 — Search Quality benchmark runner.

Usage:
    benchmarks/.venv/bin/python benchmarks/b3_search_quality/run_benchmark.py

Measures NDCG@10 for `ci search` (kind=symbol, i.e. FTS-only — see note
below on kind=hybrid) against a naive-grep baseline, on a curated ground
truth of realistic short queries against this repo (queries.yaml).

Naive-grep baseline: `grep -l <keyword>` in file-scan order (no relevance
ranking at all) — this is the floor a ranked search tool should clear.
Relevance is collapsed to file level for this comparison (a file's grade is
the max grade of any ground-truth symbol it contains), since grep doesn't
resolve to symbols.

kind=hybrid is also queried per task and its `degraded` flag reported
honestly: hybrid needs the `embeddings` Cargo feature compiled in AND a
downloaded model — most environments running this script (including plain
`cargo build`) won't have that, so hybrid falls back to FTS-only and its
NDCG will equal kind=symbol's. That is reported, not hidden or worked
around, per this project's benchmark methodology (see ../README.md).
"""

from __future__ import annotations

import json
import math
import statistics
import sys
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from mcp_client import MCPClient, repo_root_from_here  # noqa: E402
from naive_workflow import naive_grep_ranked_files  # noqa: E402

QUERIES_PATH = Path(__file__).parent / "queries.yaml"
NDCG_K = 10


def dcg(grades: list[int]) -> float:
    return sum(g / math.log2(i + 2) for i, g in enumerate(grades))


def ndcg_at_k(ranked_grades: list[int], ideal_grades: list[int], k: int = NDCG_K) -> float:
    ideal = dcg(sorted(ideal_grades, reverse=True)[:k])
    if ideal <= 0:
        return 0.0
    return dcg(ranked_grades[:k]) / ideal


def ci_search_ndcg(client: MCPClient, query: str, kind: str, relevance: dict[tuple[str, str], int]) -> tuple[float, bool]:
    raw = client.call_tool("search", {"query": query, "kind": kind, "limit": NDCG_K})
    out = json.loads(raw)
    results = out.get("results", [])
    grades = [relevance.get((r.get("name"), r.get("path")), 0) for r in results]
    ideal_grades = list(relevance.values())
    return ndcg_at_k(grades, ideal_grades), bool(out.get("degraded", False))


def naive_grep_ndcg(repo_root: Path, pattern: str, file_relevance: dict[str, int]) -> float:
    files = naive_grep_ranked_files(repo_root, pattern, ["crates/**/*.rs"])
    grades = [file_relevance.get(f, 0) for f in files]
    ideal_grades = list(file_relevance.values())
    return ndcg_at_k(grades, ideal_grades)


def main() -> int:
    repo_root = repo_root_from_here()
    queries = yaml.safe_load(QUERIES_PATH.read_text())["queries"]

    print(f"[b3] starting calm serve for {repo_root} ...", file=sys.stderr)
    client = MCPClient(project_root=".", repo_root=str(repo_root))
    try:
        client.wait_until_indexed()
        print("[b3] index ready, running queries", file=sys.stderr)

        rows = []
        for q in queries:
            relevance = {(r["name"], r["path"]): r["grade"] for r in q["relevant"]}
            file_relevance: dict[str, int] = {}
            for r in q["relevant"]:
                file_relevance[r["path"]] = max(file_relevance.get(r["path"], 0), r["grade"])

            symbol_ndcg, _ = ci_search_ndcg(client, q["query"], "symbol", relevance)
            hybrid_ndcg, hybrid_degraded = ci_search_ndcg(client, q["query"], "hybrid", relevance)
            # kind="semantic" isolates the embedding backend's own NDCG,
            # pre-RRF-fusion -- added for D (docs/superskills/plans/2026-07-30-
            # calm-dfb-levers-design.md §3, audit-design Finding A2): a
            # hybrid-only score dilutes a new backend's contribution with the
            # FTS leg, so a before/after embedding-backend comparison needs
            # this number specifically, not just ndcg_hybrid.
            semantic_ndcg, semantic_degraded = ci_search_ndcg(client, q["query"], "semantic", relevance)
            grep_ndcg = naive_grep_ndcg(repo_root, q["grep_pattern"], file_relevance)

            rows.append({
                "id": q["id"],
                "query": q["query"],
                "ndcg_symbol": symbol_ndcg,
                "ndcg_hybrid": hybrid_ndcg,
                "hybrid_degraded": hybrid_degraded,
                "ndcg_semantic": semantic_ndcg,
                "semantic_degraded": semantic_degraded,
                "ndcg_naive_grep": grep_ndcg,
            })
    finally:
        client.close()

    symbol_scores = [r["ndcg_symbol"] for r in rows]
    hybrid_scores = [r["ndcg_hybrid"] for r in rows]
    semantic_scores = [r["ndcg_semantic"] for r in rows]
    grep_scores = [r["ndcg_naive_grep"] for r in rows]
    any_hybrid_active = any(not r["hybrid_degraded"] for r in rows)
    # Finding A2's actual requirement: not "at least one query" but "every
    # query" -- a run with even one degraded query already measured nothing
    # for that query, and averaging it in silently understates a real
    # embedding-backend delta. This is the number a D before/after comparison
    # must check before trusting mean_ndcg_semantic/mean_ndcg_hybrid at all.
    all_queries_active = all(
        not r["hybrid_degraded"] and not r["semantic_degraded"] for r in rows
    )

    summary = {
        "corpus": "self (CALM)",
        "metric": f"NDCG@{NDCG_K}",
        "queries": rows,
        "aggregate": {
            "mean_ndcg_symbol": statistics.mean(symbol_scores),
            "mean_ndcg_hybrid": statistics.mean(hybrid_scores),
            "mean_ndcg_semantic": statistics.mean(semantic_scores),
            "mean_ndcg_naive_grep": statistics.mean(grep_scores),
            "hybrid_active_for_any_query": any_hybrid_active,
            "all_queries_active": all_queries_active,
            "note": (
                "hybrid was NOT degraded for at least one query — semantic layer contributed."
                if any_hybrid_active
                else "hybrid was degraded (FTS-only fallback) for EVERY query in this run — "
                     "embeddings feature/model unavailable in this environment, so ndcg_hybrid "
                     "== ndcg_symbol here is an environment limitation, not a finding about "
                     "hybrid search quality. Re-run with --features embeddings + a downloaded "
                     "model to get a real hybrid measurement."
            ),
            "backend_comparison_note": (
                "SAFE to use for a before/after embedding-backend comparison "
                "(mean_ndcg_semantic and mean_ndcg_hybrid reflect a real embedding "
                "backend for every query)."
                if all_queries_active
                else "NOT SAFE for a before/after embedding-backend comparison -- at least "
                     "one query was degraded (FTS-only fallback), so mean_ndcg_semantic/"
                     "mean_ndcg_hybrid partially reflect 'no backend ran' rather than the "
                     "backend under test. Fix the environment (embeddings feature compiled "
                     "in + model available) and re-run before trusting any delta."
            ),
        },
    }

    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(summary, indent=2))

    print()
    print("| Query | ci (symbol) | ci (hybrid) | ci (semantic) | naive grep | degraded? |")
    print("|---|---|---|---|---|---|")
    for r in rows:
        degraded = r["hybrid_degraded"] or r["semantic_degraded"]
        print(
            f"| {r['id']} | {r['ndcg_symbol']:.3f} | {r['ndcg_hybrid']:.3f} | "
            f"{r['ndcg_semantic']:.3f} | {r['ndcg_naive_grep']:.3f} | {'yes' if degraded else 'no'} |"
        )
    print()
    agg = summary["aggregate"]
    print(
        f"mean NDCG@{NDCG_K} — ci(symbol): {agg['mean_ndcg_symbol']:.3f}, "
        f"ci(hybrid): {agg['mean_ndcg_hybrid']:.3f}, ci(semantic): {agg['mean_ndcg_semantic']:.3f}, "
        f"naive grep: {agg['mean_ndcg_naive_grep']:.3f}"
    )
    print(agg["note"])
    print(agg["backend_comparison_note"])
    if not all_queries_active:
        print(
            "\n*** WARNING: this run is NOT trustworthy for a D (embedding backend) "
            "before/after comparison -- see backend_comparison_note above. ***",
            file=sys.stderr,
        )
    print(f"\nfull results written to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
