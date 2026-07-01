#!/usr/bin/env python3
"""B6 — Tool-Call Efficiency benchmark runner.

Usage:
    benchmarks/.venv/bin/python benchmarks/b6_tool_call_efficiency/run_benchmark.py

Distinct from B4 (token count): measures how many discrete tool invocations
(grep, then one file read per match) a naive workflow needs versus a single
`ci` MCP tool call — this is round-trip / latency overhead, independent of
payload size. Same task set as B4 (../lib/tasks.yaml), reused rather than
duplicated.

Each ci task is, by construction, exactly 1 MCP call. We still invoke the
real `ci serve` per task (instead of just asserting "1") to confirm the tool
actually returns non-empty content for each task's real symbol/query.
"""

from __future__ import annotations

import json
import statistics
import sys
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from mcp_client import MCPClient, repo_root_from_here  # noqa: E402
from naive_workflow import naive_text_and_calls  # noqa: E402

TASKS_PATH = Path(__file__).resolve().parents[1] / "lib" / "tasks.yaml"


def main() -> int:
    repo_root = repo_root_from_here()
    tasks = yaml.safe_load(TASKS_PATH.read_text())["tasks"]

    print(f"[b6] starting ci serve for {repo_root} ...", file=sys.stderr)
    client = MCPClient(project_root=".", repo_root=str(repo_root))
    try:
        client.wait_until_indexed()
        print("[b6] index ready, running tasks", file=sys.stderr)

        rows = []
        for task in tasks:
            _, naive_calls = naive_text_and_calls(repo_root, task["naive"])
            ci_text = client.call_tool(task["ci"]["tool"], task["ci"]["arguments"])
            if not ci_text.strip():
                raise RuntimeError(f"{task['id']}: ci tool returned empty content — not a valid 1-call answer")
            ci_calls = 1

            reduction_pct = (1 - ci_calls / naive_calls) * 100

            rows.append({
                "id": task["id"],
                "description": task["description"],
                "ci_tool": task["ci"]["tool"],
                "naive_calls": naive_calls,
                "ci_calls": ci_calls,
                "reduction_pct": reduction_pct,
            })
    finally:
        client.close()

    reductions = [r["reduction_pct"] for r in rows]
    summary = {
        "corpus": "self (Code-Intelligence)",
        "tasks": rows,
        "aggregate": {
            "median_reduction_pct": statistics.median(reductions),
            "mean_reduction_pct": statistics.mean(reductions),
            "min_reduction_pct": min(reductions),
            "max_reduction_pct": max(reductions),
            "note": f"N={len(reductions)} tasks — too small for meaningful p90/p99",
        },
    }

    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(summary, indent=2))

    print()
    print("| Task | ci tool | naive calls | ci calls | reduction |")
    print("|---|---|---|---|---|")
    for r in rows:
        print(f"| {r['id']} | {r['ci_tool']} | {r['naive_calls']} | {r['ci_calls']} | {r['reduction_pct']:.0f}% |")
    print()
    agg = summary["aggregate"]
    print(f"median reduction: {agg['median_reduction_pct']:.0f}%, mean: {agg['mean_reduction_pct']:.0f}%, "
          f"range: {agg['min_reduction_pct']:.0f}%-{agg['max_reduction_pct']:.0f}% ({agg['note']})")
    print(f"\nfull results written to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
