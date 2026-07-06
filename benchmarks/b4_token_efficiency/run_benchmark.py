#!/usr/bin/env python3
"""B4 — Token Efficiency benchmark runner.

Usage:
    benchmarks/.venv/bin/python benchmarks/b4_token_efficiency/run_benchmark.py

Measures, per task, how many LLM tokens (GPT-4 tokenizer) a naive workflow
(cat/grep) costs versus the equivalent `calm` MCP tool call, using real tool
responses from a live `calm serve` process against this repo.

Task definitions and naive-workflow simulation live in ../lib (shared with B6).
"""

from __future__ import annotations

import json
import statistics
import sys
from pathlib import Path

import tiktoken
import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from mcp_client import MCPClient, repo_root_from_here  # noqa: E402
from naive_workflow import naive_text  # noqa: E402

ENCODING_MODEL = "gpt-4"
TASKS_PATH = Path(__file__).resolve().parents[1] / "lib" / "tasks.yaml"


def main() -> int:
    repo_root = repo_root_from_here()
    tasks = yaml.safe_load(TASKS_PATH.read_text())["tasks"]
    enc = tiktoken.encoding_for_model(ENCODING_MODEL)

    print(f"[b4] starting calm serve for {repo_root} ...", file=sys.stderr)
    client = MCPClient(project_root=".", repo_root=str(repo_root))
    try:
        client.wait_until_indexed()
        print("[b4] index ready, running tasks", file=sys.stderr)

        rows = []
        for task in tasks:
            naive = naive_text(repo_root, task["naive"])
            calm_text = client.call_tool(task["ci"]["tool"], task["ci"]["arguments"])

            naive_tokens = len(enc.encode(naive))
            ci_tokens = len(enc.encode(calm_text))
            ratio = naive_tokens / ci_tokens if ci_tokens else float("inf")

            rows.append({
                "id": task["id"],
                "description": task["description"],
                "ci_tool": task["ci"]["tool"],
                "naive_tokens": naive_tokens,
                "ci_tokens": ci_tokens,
                "ratio": ratio,
            })
    finally:
        client.close()

    ratios = [r["ratio"] for r in rows]
    summary = {
        "encoding_model": ENCODING_MODEL,
        "corpus": "self (CALM)",
        "tasks": rows,
        "aggregate": {
            "median_ratio": statistics.median(ratios),
            "mean_ratio": statistics.mean(ratios),
            "min_ratio": min(ratios),
            "max_ratio": max(ratios),
            "note": f"N={len(ratios)} tasks — too small for meaningful p90/p99",
        },
    }

    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(summary, indent=2))

    print()
    print("| Task | ci tool | naive tokens | ci tokens | ratio |")
    print("|---|---|---|---|---|")
    for r in rows:
        print(f"| {r['id']} | {r['ci_tool']} | {r['naive_tokens']} | {r['ci_tokens']} | {r['ratio']:.1f}x |")
    print()
    agg = summary["aggregate"]
    print(f"median ratio: {agg['median_ratio']:.1f}x, mean: {agg['mean_ratio']:.1f}x, "
          f"range: {agg['min_ratio']:.1f}x-{agg['max_ratio']:.1f}x ({agg['note']})")
    print(f"\nfull results written to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
