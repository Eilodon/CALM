#!/usr/bin/env python3
"""B10 — Real Competitor A/B benchmark runner.

Usage:
    benchmarks/.venv/bin/python benchmarks/b10_real_competitor_ab/run_benchmark.py

Unlike the competitor numbers quoted in docs/comparison.md (drawn from each
project's own public docs/benchmarks), this script runs REAL tool calls
against REAL installs of `calm`, CodeGraph (colbymchenry/codegraph), and Semble
— same self-repo corpus, same 4 tasks as B4/B6 (benchmarks/lib/tasks.yaml +
competitor_tasks.yaml) — and measures:

  1. token cost   (GPT-4 tokenizer, same methodology as B4)
  2. tool-call count (same methodology as B6 — naive vs 1 MCP call)
  3. accuracy on find_callers — cross-checked against a grep oracle, since
     this is the one task where a wrong-but-plausible answer (missed or
     hallucinated caller) is easy to state precisely and verify.

Requires (all installed locally, nothing this script installs itself):
  - `codegraph` on PATH (`npm i -g @colbymchenry/codegraph`), `.codegraph/`
    already built (`codegraph init` in repo root).
  - `uvx` on PATH (semble runs via `uvx --from semble[mcp] semble`, no
    separate install step — uvx caches the environment after first run).
  - `cargo build --release -p calm-cli` already run (same prerequisite as
    B4/B6).

Honest-reporting policy (see benchmarks/README.md): tasks Semble cannot
structurally answer (no call graph) are marked `unsupported` in
competitor_tasks.yaml and still measured, not skipped or excluded from the
table.
"""

from __future__ import annotations

import json
import re
import statistics
import subprocess
import sys
from pathlib import Path

import tiktoken
import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from generic_mcp_client import GenericMCPClient  # noqa: E402
from mcp_client import MCPClient, repo_root_from_here  # noqa: E402
from naive_workflow import naive_text_and_calls  # noqa: E402

ENCODING_MODEL = "gpt-4"
TASKS_PATH = Path(__file__).resolve().parents[1] / "lib" / "tasks.yaml"
COMPETITOR_TASKS_PATH = Path(__file__).resolve().parents[1] / "lib" / "competitor_tasks.yaml"

CODEGRAPH_ENV = {
    "CODEGRAPH_MCP_TOOLS": "explore,node,search,callers,callees,impact,files,status",
}


def start_codegraph(repo_root: Path) -> GenericMCPClient:
    return GenericMCPClient(
        cmd=["codegraph", "serve", "--mcp"], cwd=str(repo_root), env=CODEGRAPH_ENV,
    )


def start_semble(repo_root: Path) -> GenericMCPClient:
    return GenericMCPClient(
        cmd=["uvx", "--from", "semble[mcp]", "semble"], cwd=str(repo_root),
    )


def grep_oracle_callers(repo_root: Path, symbol: str) -> set[str]:
    """Files with a real call site for `symbol` — i.e. `symbol(` appears and the
    line is not the `fn symbol` definition itself or a comment. Good enough as
    an independent oracle on this small, single-symbol, single-language case;
    not a general-purpose call-graph substitute.
    """
    result = subprocess.run(
        ["grep", "-rn", f"{symbol}(", "crates", "--include=*.rs"],
        cwd=repo_root, capture_output=True, text=True,
    )
    files = set()
    for line in result.stdout.splitlines():
        path, _, rest = line.partition(":")
        _, _, code = rest.partition(":")
        stripped = code.strip()
        if stripped.startswith(("fn ", "pub fn ", "///", "//")):
            continue
        files.add(path)
    return files


def extract_files(text: str) -> set[str]:
    """Best-effort file-path extraction from free-text tool output (both `calm`
    and CodeGraph responses embed `crates/.../file.rs` paths inline)."""
    return set(re.findall(r"crates/[\w./-]+\.rs", text))


def main() -> int:
    repo_root = repo_root_from_here()
    tasks = {t["id"]: t for t in yaml.safe_load(TASKS_PATH.read_text())["tasks"]}
    competitor_tasks = {t["id"]: t for t in yaml.safe_load(COMPETITOR_TASKS_PATH.read_text())["tasks"]}
    enc = tiktoken.encoding_for_model(ENCODING_MODEL)

    print(f"[b10] starting calm serve, codegraph serve --mcp, semble for {repo_root} ...", file=sys.stderr)
    calm_client = MCPClient(project_root=".", repo_root=str(repo_root))
    codegraph_client = start_codegraph(repo_root)
    semble_client = start_semble(repo_root)
    try:
        calm_client.wait_until_indexed()
        print("[b10] all servers ready, running tasks", file=sys.stderr)

        rows = []
        for task_id, task in tasks.items():
            ctask = competitor_tasks[task_id]
            naive_text_val, naive_calls = naive_text_and_calls(repo_root, task["naive"])

            calm_text = calm_client.call_tool(task["ci"]["tool"], task["ci"]["arguments"])
            cg_text = codegraph_client.call_tool(ctask["codegraph"]["tool"], ctask["codegraph"]["arguments"])
            sb_text = semble_client.call_tool(ctask["semble"]["tool"], ctask["semble"]["arguments"])

            def toks(t: str) -> int:
                return len(enc.encode(t))

            row = {
                "id": task_id,
                "description": task["description"],
                "naive_tokens": toks(naive_text_val),
                "naive_calls": naive_calls,
                "ci_tool": task["ci"]["tool"],
                "ci_tokens": toks(calm_text),
                "codegraph_tool": ctask["codegraph"]["tool"],
                "codegraph_tokens": toks(cg_text),
                "semble_tool": ctask["semble"]["tool"],
                "semble_tokens": toks(sb_text),
                "semble_unsupported": ctask["semble"].get("unsupported", False),
                # every competitor call here is exactly 1 MCP round-trip,
                # same accounting B6 uses for `calm`
                "ci_calls": 1,
                "codegraph_calls": 1,
                "semble_calls": 1,
            }

            if task_id == "find_callers":
                oracle = grep_oracle_callers(repo_root, task["ci"]["arguments"]["symbol"])
                ci_found = extract_files(calm_text)
                cg_found = extract_files(cg_text)
                row["accuracy"] = {
                    "oracle_files": sorted(oracle),
                    "ci_recall": f"{len(ci_found & oracle)}/{len(oracle)}",
                    "codegraph_recall": f"{len(cg_found & oracle)}/{len(oracle)}",
                    "ci_missed": sorted(oracle - ci_found),
                    "codegraph_missed": sorted(oracle - cg_found),
                }

            rows.append(row)
    finally:
        calm_client.close()
        codegraph_client.close()
        semble_client.close()

    def ratio(naive: int, tool: int) -> float:
        return naive / tool if tool else float("inf")

    summary = {
        "encoding_model": ENCODING_MODEL,
        "corpus": "self (CALM)",
        "tools_compared": {
            "ci": "calm-cli (this repo, release build)",
            "codegraph": "colbymchenry/codegraph v1.2.0 (npm, .codegraph/ built via `codegraph init`)",
            "semble": "semble MCP (uvx --from semble[mcp] semble) — embedding search, no call graph",
        },
        "tasks": rows,
    }

    for tool in ("ci", "codegraph", "semble"):
        token_ratios = [ratio(r["naive_tokens"], r[f"{tool}_tokens"]) for r in rows]
        summary.setdefault("aggregate", {})[tool] = {
            "median_token_ratio": statistics.median(token_ratios),
            "mean_token_ratio": statistics.mean(token_ratios),
        }

    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(summary, indent=2))

    print()
    print("| Task | naive tok | ci tok (ratio) | codegraph tok (ratio) | semble tok (ratio) |")
    print("|---|---|---|---|---|")
    for r in rows:
        flag = " *unsupported*" if r["semble_unsupported"] else ""
        print(
            f"| {r['id']} | {r['naive_tokens']} "
            f"| {r['ci_tokens']} ({ratio(r['naive_tokens'], r['ci_tokens']):.1f}x) "
            f"| {r['codegraph_tokens']} ({ratio(r['naive_tokens'], r['codegraph_tokens']):.1f}x) "
            f"| {r['semble_tokens']} ({ratio(r['naive_tokens'], r['semble_tokens']):.1f}x){flag} |"
        )
    print()
    for tool, agg in summary["aggregate"].items():
        print(f"{tool}: median {agg['median_token_ratio']:.1f}x, mean {agg['mean_token_ratio']:.1f}x")

    acc_row = next((r for r in rows if "accuracy" in r), None)
    if acc_row:
        acc = acc_row["accuracy"]
        print(f"\nfind_callers accuracy vs grep oracle {acc['oracle_files']}:")
        print(f"  ci: {acc['ci_recall']} (missed: {acc['ci_missed'] or 'none'})")
        print(f"  codegraph: {acc['codegraph_recall']} (missed: {acc['codegraph_missed'] or 'none'})")
        print("  semble: N/A — no call graph (embedding search only), task marked unsupported")

    print(f"\nfull results written to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
