#!/usr/bin/env python3
"""Ad-hoc paired capability test: `calm` vs grepai — NOT the B10/B11 benchmark suite.

Scope, per explicit user request: only test capability pairs where both tools have a
genuinely equivalent tool — skip anything asymmetric. No naive baseline, no N=5/IQR,
no `unsupported` bookkeeping. Just: same real target, both tools, side by side.

Measures per pair: token cost (tiktoken GPT-4 — still a proxy, not the real runtime
tokenizer), accuracy (grep-derived, tool-independent oracle where one applies), and
speed (wall-clock per call, median of 3 real repeats — lighter than B11's N=5/IQR but
enough to smooth transient process/IO noise without pretending to statistical rigor).

Pairs tested:
  1. search        — calm `search`             vs grepai `grepai_search`
  2. find_callers   — calm `callers`            vs grepai `grepai_trace_callers`
  3. find_callees   — calm `callees`            vs grepai `grepai_trace_callees`
  4. call_graph     — calm `callers`+`callees`  vs grepai `grepai_trace_graph`
                       (grepai bundles both directions in 1 call; calm needs 2 — that
                       call-count difference is itself part of what's being measured,
                       not hidden)
  5. index_status   — calm `indexing_status`    vs grepai `grepai_index_status`

Run against an isolated git worktree corpus (--corpus), not the live repo — no writes
happen in this script, but grepai/calm both leave index state (.grepai/, .calm/) that
shouldn't land in the live checkout.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

import tiktoken

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from generic_mcp_client import GenericMCPClient  # noqa: E402
from mcp_client import MCPClient, repo_root_from_here  # noqa: E402

ENC = tiktoken.encoding_for_model("gpt-4")
N_REPEATS = 3


def toks(t: str) -> int:
    return len(ENC.encode(t))


def timed_call(client, tool: str, args: dict, n: int = N_REPEATS) -> tuple[str, dict]:
    times = []
    text = ""
    for _ in range(n):
        t0 = time.perf_counter()
        text = client.call_tool(tool, args)
        times.append(time.perf_counter() - t0)
    return text, {"median_s": round(statistics.median(times), 3), "raw_s": [round(t, 3) for t in times]}


def grep_oracle_callers(corpus: Path, symbol: str) -> set[str]:
    result = subprocess.run(
        ["grep", "-rn", f"{symbol}(", "crates", "--include=*.rs"],
        cwd=corpus, capture_output=True, text=True,
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


def grep_oracle_callees(corpus: Path, path: str, symbol: str) -> set[str]:
    """Real function-call identifiers found inside `symbol`'s body that are
    independently confirmed to be functions/methods defined somewhere in the repo
    (cross-checked via a second grep, not trusted from any tool under test)."""
    lines = (corpus / path).read_text().splitlines()
    start = next(i for i, l in enumerate(lines) if re.match(rf"^(pub )?fn {re.escape(symbol)}\b", l))
    end = len(lines)
    for i in range(start + 1, len(lines)):
        if re.match(r"^(pub )?fn |^#\[cfg\(test\)\]|^pub struct |^pub enum |^impl |^mod ", lines[i]):
            end = i
            break
    body = "\n".join(lines[start:end])
    candidates = set(re.findall(r"\b([a-z_][a-zA-Z0-9_]*)\s*\(", body)) - {symbol}
    real = set()
    for name in candidates:
        check = subprocess.run(
            ["grep", "-rlE", rf"(pub )?fn {name}\b", "crates", "--include=*.rs"],
            cwd=corpus, capture_output=True, text=True,
        )
        if check.stdout.strip():
            real.add(name)
    return real


def extract_files(text: str) -> set[str]:
    return set(re.findall(r"crates/[\w./-]+\.rs", text))


def extract_names(text: str) -> set[str]:
    return set(re.findall(r"\b[a-z_][a-zA-Z0-9_]*\b", text.lower()))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", required=True)
    args = parser.parse_args()
    corpus = Path(args.corpus).resolve()
    real_repo_root = repo_root_from_here()

    print(f"[pairs] corpus = {corpus}", file=sys.stderr)
    calm = MCPClient(project_root=str(corpus), repo_root=str(real_repo_root))
    grepai = GenericMCPClient(cmd=["grepai", "mcp-serve"], cwd=str(corpus))

    rows = []
    try:
        calm.wait_until_indexed()
        deadline = time.time() + 60
        while time.time() < deadline:
            status = json.loads(grepai.call_tool("grepai_index_status", {}))
            if status.get("symbols_ready"):
                break
            time.sleep(1)
        print("[pairs] both ready, running pairs", file=sys.stderr)

        # 1. search — kind="hybrid" explicitly: `search`'s default kind="symbol" is
        # exact name/signature matching, not a fair opponent for grepai's semantic
        # search on a natural-language query (verified live: default kind returned an
        # empty/near-empty result for this phrase — tool's own docs recommend hybrid
        # for "don't have an exact file path/line", exactly this case).
        query = "run_indexing_pipeline function implementation"
        calm_text, calm_t = timed_call(calm, "search", {"query": query, "kind": "hybrid"})
        grepai_text, grepai_t = timed_call(grepai, "grepai_search", {"query": query, "limit": 3})
        rows.append({
            "pair": "search", "calm_tool": "search", "grepai_tool": "grepai_search",
            "calm_tokens": toks(calm_text), "grepai_tokens": toks(grepai_text),
            "calm_timing": calm_t, "grepai_timing": grepai_t,
            "calm_found_target": "run_indexing_pipeline" in calm_text and "pipeline.rs" in calm_text,
            "grepai_found_target": "run_indexing_pipeline" in grepai_text and "pipeline.rs" in grepai_text,
            "calm_preview": calm_text[:300], "grepai_preview": grepai_text[:300],
        })

        # 2. find_callers
        symbol = "collect_source_files"
        oracle = grep_oracle_callers(corpus, symbol)
        calm_text, calm_t = timed_call(calm, "callers", {"symbol": symbol})
        grepai_text, grepai_t = timed_call(grepai, "grepai_trace_callers", {"symbol": symbol})
        calm_found = extract_files(calm_text)
        grepai_found = extract_files(grepai_text)
        rows.append({
            "pair": "find_callers", "calm_tool": "callers", "grepai_tool": "grepai_trace_callers",
            "oracle_files": sorted(oracle),
            "calm_tokens": toks(calm_text), "grepai_tokens": toks(grepai_text),
            "calm_timing": calm_t, "grepai_timing": grepai_t,
            "calm_recall": f"{len(calm_found & oracle)}/{len(oracle)}",
            "grepai_recall": f"{len(grepai_found & oracle)}/{len(oracle)}",
            "calm_preview": calm_text[:300], "grepai_preview": grepai_text[:300],
        })

        # 3. find_callees
        symbol = "reindex_changed"
        path = "crates/calm-core/src/indexer/pipeline.rs"
        oracle = grep_oracle_callees(corpus, path, symbol)
        calm_text, calm_t = timed_call(calm, "callees", {"symbol": symbol})
        grepai_text, grepai_t = timed_call(grepai, "grepai_trace_callees", {"symbol": symbol})
        calm_names = extract_names(calm_text)
        grepai_names = extract_names(grepai_text)
        oracle_lower = {n.lower() for n in oracle}
        rows.append({
            "pair": "find_callees", "calm_tool": "callees", "grepai_tool": "grepai_trace_callees",
            "oracle_callees": sorted(oracle),
            "calm_tokens": toks(calm_text), "grepai_tokens": toks(grepai_text),
            "calm_timing": calm_t, "grepai_timing": grepai_t,
            "calm_recall": f"{len(calm_names & oracle_lower)}/{len(oracle_lower)}",
            "grepai_recall": f"{len(grepai_names & oracle_lower)}/{len(oracle_lower)}",
            "calm_preview": calm_text[:300], "grepai_preview": grepai_text[:300],
        })

        # 4. call_graph (bidirectional) — calm needs 2 calls, grepai needs 1
        symbol = "reindex_changed"
        t0 = time.perf_counter()
        callers_text, callers_t = timed_call(calm, "callers", {"symbol": symbol}, n=1)
        callees_text, callees_t = timed_call(calm, "callees", {"symbol": symbol}, n=1)
        calm_graph_text = callers_text + callees_text
        calm_wall = time.perf_counter() - t0
        grepai_text, grepai_t = timed_call(grepai, "grepai_trace_graph", {"symbol": symbol, "depth": 2})
        rows.append({
            "pair": "call_graph", "calm_tool": "callers+callees (2 calls)", "grepai_tool": "grepai_trace_graph (1 call)",
            "calm_tokens": toks(calm_graph_text), "grepai_tokens": toks(grepai_text),
            "calm_calls": 2, "grepai_calls": 1,
            "calm_single_run_wall_s": round(calm_wall, 3), "grepai_timing": grepai_t,
            "calm_preview": calm_graph_text[:300], "grepai_preview": grepai_text[:300],
        })

        # 5. index_status
        calm_text, calm_t = timed_call(calm, "indexing_status", {})
        grepai_text, grepai_t = timed_call(grepai, "grepai_index_status", {})
        rows.append({
            "pair": "index_status", "calm_tool": "indexing_status", "grepai_tool": "grepai_index_status",
            "calm_tokens": toks(calm_text), "grepai_tokens": toks(grepai_text),
            "calm_timing": calm_t, "grepai_timing": grepai_t,
            "calm_reports_ready": '"ready"' in calm_text,
            "grepai_reports_ready": "true" in grepai_text.lower(),
            "calm_preview": calm_text[:300], "grepai_preview": grepai_text[:300],
        })

    finally:
        calm.close()
        grepai.close()

    out_path = Path(__file__).parent / "grepai_vs_calm_pairs_results.json"
    out_path.write_text(json.dumps(rows, indent=2))
    print(f"\nresults written to {out_path}")

    print("\n| Pair | calm tok | grepai tok | calm speed (median s) | grepai speed (median s) |")
    print("|---|---|---|---|---|")
    for r in rows:
        ct = r.get("calm_timing", {}).get("median_s", r.get("calm_single_run_wall_s"))
        gt = r.get("grepai_timing", {}).get("median_s")
        print(f"| {r['pair']} | {r['calm_tokens']} | {r['grepai_tokens']} | {ct} | {gt} |")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
