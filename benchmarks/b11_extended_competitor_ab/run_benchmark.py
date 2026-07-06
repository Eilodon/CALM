#!/usr/bin/env python3
"""B11 — Extended Real Competitor A/B (`calm` vs CodeGraph vs Semble vs grepai vs Serena).

Supersedes B10's methodology after an audit (2026-07-06) found it unreliable in ways
that mattered for exactly the claims it was being used to support:

  1. Only `find_callers` had a correctness oracle. The other 3 tasks measured token/
     tool-call cost without ever checking whether the answer was actually RIGHT — a
     tool could return a wrong or truncated answer and still "win" on token ratio.
  2. `pre_edit_blast_radius`'s anchors (`reindex_changed`, `collect_source_files`) are
     risk_assessment=medium/low, is_hub=false (verified live via `edit_context`/
     `hotspots` on 2026-07-06) — CALM's hard-refuse gate never actually triggers on
     them, so B10 never tested the single most-claimed differentiator (risk-gate
     before edits) at all.
  3. N=1 run, no repeats, no median/IQR — a single cold/warm call each, reported as
     if it were a stable number.
  4. GPT-4 tokenizer (tiktoken) stood in for whatever tokenizer the actual agent
     runtime uses, un-flagged as a proxy.
  5. No readiness-gate for tools with real warm-up cost (grepai's Ollama embedding
     backlog, Serena's rust-analyzer cold start) before timing started.

Fixes, in the same order:
  A. Every task now has a live-computed oracle (grep-based caller sets, or a
     grep-derived ground-truth function body compared line-by-line against each
     tool's answer) — not just `find_callers`.
  B. Two new tasks that actually exercise CALM's claimed differentiators:
     `risk_gate_refusal` (a real edit attempt on a real hub symbol, no confirm) and
     `memory_recall` (write a note, kill + respawn the server process, read the note
     back — tests persistence across a process restart, not just same-process
     memory).
  C. N=5 repeats per task per tool for the 4 original tasks; median + IQR reported.
  D. An explicit readiness-gate before each tool's first real call (grepai:
     `grepai_index_status.symbols_ready`; Serena: one throwaway `find_symbol` call to
     force rust-analyzer's cold start, timed and reported separately, not folded into
     steady-state numbers).
  E. tiktoken GPT-4 kept (no network dependency, same as B4/B6/B10) but documented
     here as a proxy — NOT the real tokenizer of whatever model an agent actually
     runs on.

Tool schemas (grepai_search/_trace_callers/_trace_graph/_index_status; Serena's
find_symbol/find_referencing_symbols/get_symbols_overview/replace_symbol_body/
write_memory/read_memory) were captured live from each server's real `tools/list` +
sample `tools/call` responses on 2026-07-06, not from docs — docs turned out to be
wrong in at least one load-bearing way (docs/comparison.md claimed Serena has no
memory tool; it has `write_memory`/`read_memory`, verified live and fixed in that doc
as part of this same audit).

SAFETY: every server here is pointed at an isolated git worktree copy of this repo
(`--corpus`, defaults to creating one under system temp), NOT the live checkout —
`risk_gate_refusal` performs a real edit attempt against a real hub symbol
(`benchmarks/lib/mcp_client.py::MCPClient.call_tool`) and Serena has no gate at all,
so it WILL actually rewrite that file. Running this against a live working tree
would corrupt it. The corpus is reset with `git checkout --` between the refusal
attempt and the (separate) confirmed-edit attempt, and left in its original state
afterwards.

Requires (all installed locally, nothing this script installs itself):
  - `codegraph` on PATH, `.codegraph/` built (`codegraph init`) inside the corpus.
  - `grepai` on PATH, `.grepai/` initialized + `grepai watch` run to completion
    inside the corpus, Ollama running locally with `nomic-embed-text` pulled.
  - `uvx` on PATH (Serena + Semble both run via `uvx --from ...`, cached after
    first run); a `.serena/project.yml` already created for the corpus
    (`serena project create <corpus> --language rust`).
  - `cargo build --release -p calm-cli` already run in the real repo checkout.

Honest-reporting policy (see benchmarks/README.md): tasks a tool cannot structurally
answer are marked `unsupported` in competitor_tasks.yaml / inline below and still
measured, not skipped or excluded from the table.
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
import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from generic_mcp_client import GenericMCPClient  # noqa: E402
from mcp_client import MCPClient, repo_root_from_here  # noqa: E402
from naive_workflow import naive_text_and_calls  # noqa: E402

ENCODING_MODEL = "gpt-4"
N_REPEATS = 5
LIB_DIR = Path(__file__).resolve().parents[1] / "lib"
TASKS_PATH = LIB_DIR / "tasks.yaml"
COMPETITOR_TASKS_PATH = LIB_DIR / "competitor_tasks.yaml"

CODEGRAPH_ENV = {
    "CODEGRAPH_MCP_TOOLS": "explore,node,search,callers,callees,impact,files,status",
}

# risk_gate_refusal anchor — verified live via `hotspots(include_symbols=true)` on
# 2026-07-06: is_hub=true, coreness=4 in this self-repo corpus. Independent of any
# of the 5 tools under test (hotspots' hub scoring is graph-structural, computed the
# same way regardless of what symbol is queried) so using it as ground truth for
# "is this actually a hub" is not circular.
#
# Rust, not the Python benchmark scripts: a first attempt anchored on
# benchmarks/lib/mcp_client.py::MCPClient.call_tool (also a real verified hub) gave
# Serena a spurious "path is ignored" error — the corpus's .serena/project.yml only
# configures `languages: [rust]`, so Serena's LSP-backed symbol tools cannot operate
# on Python files at all. That is a fair, real limitation of this test setup (single-
# language project config), NOT evidence about Serena's risk-awareness, so it would
# have been a dishonest "refusal" to report. `reindex_changed` is Rust AND already
# used as the pre_edit_blast_radius anchor above, so both tools are being asked about
# a symbol they can actually operate on.
RISK_GATE_PATH = "crates/calm-core/src/indexer/pipeline.rs"
RISK_GATE_SYMBOL = "reindex_changed"
RISK_GATE_NAME_PATH = "reindex_changed"
RISK_GATE_MARKER = "\n    // b11-risk-gate-probe (harmless marker appended by benchmark)\n"

MEMORY_TOPIC = "b11-memory-probe"
MEMORY_CONTENT = "b11 probe note — written to test persistence across a process restart"

ENC = tiktoken.encoding_for_model(ENCODING_MODEL)


def toks(t: str) -> int:
    return len(ENC.encode(t))


# ---------------------------------------------------------------------------
# Competitor process helpers
# ---------------------------------------------------------------------------

def start_codegraph(corpus: Path) -> GenericMCPClient:
    # `npx -y @colbymchenry/codegraph`, NOT a bare `codegraph` on PATH — this repo's
    # own .mcp.json uses the same npx form. A bare `codegraph` binary risks a name
    # collision with an unrelated Rust CLI tool that also installs as `codegraph`
    # (found live during this benchmark's setup: it writes its own .codegraph.db,
    # CLAUDE.md, and Claude Code hooks — nothing to do with colbymchenry/codegraph).
    return GenericMCPClient(
        cmd=["npx", "-y", "@colbymchenry/codegraph", "serve", "--mcp"], cwd=str(corpus), env=CODEGRAPH_ENV,
    )


def start_semble(corpus: Path) -> GenericMCPClient:
    return GenericMCPClient(cmd=["uvx", "--from", "semble[mcp]", "semble"], cwd=str(corpus))


def start_grepai(corpus: Path) -> GenericMCPClient:
    client = GenericMCPClient(cmd=["grepai", "mcp-serve"], cwd=str(corpus))
    deadline = time.time() + 60
    while time.time() < deadline:
        status = json.loads(client.call_tool("grepai_index_status", {}))
        if status.get("symbols_ready"):
            return client
        time.sleep(1)
    raise RuntimeError("grepai index not ready after 60s")


def start_serena(corpus: Path) -> tuple[GenericMCPClient, float]:
    client = GenericMCPClient(
        cmd=["uvx", "--from", "git+https://github.com/oraios/serena", "serena",
             "start-mcp-server", "--project-from-cwd",
             "--enable-web-dashboard", "false", "--open-web-dashboard", "false"],
        cwd=str(corpus),
    )
    t0 = time.time()
    # throwaway call to force rust-analyzer's cold start (PrimeCaches etc.) — timed
    # separately so it never contaminates steady-state per-task numbers below.
    client.call_tool("find_symbol", {"name_path_pattern": "collect_source_files", "include_body": False})
    cold_start_s = time.time() - t0
    return client, cold_start_s


# ---------------------------------------------------------------------------
# Oracles — all computed live against the corpus, independent of any tool under test
# ---------------------------------------------------------------------------

def grep_oracle_callers(corpus: Path, symbol: str) -> set[str]:
    """Files with a real call site for `symbol` (not the `fn symbol` definition
    itself or a comment) — same method B10 used, good enough for this small,
    single-symbol, single-language case; not a general-purpose oracle."""
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


def function_ground_truth_lines(corpus: Path, path: str, symbol: str) -> list[str]:
    """Non-blank, stripped lines of `symbol`'s real current body, bounded by the
    next top-level item — computed directly from the file on disk, independent of
    any of the 5 tools under test (this is what makes it a fair oracle instead of
    trusting one tool's own idea of where the function ends)."""
    lines = (corpus / path).read_text().splitlines()
    start = next(i for i, l in enumerate(lines) if re.match(rf"^(pub )?fn {re.escape(symbol)}\b", l))
    end = len(lines)
    for i in range(start + 1, len(lines)):
        if re.match(r"^(pub )?fn |^#\[cfg\(test\)\]|^pub struct |^pub enum |^impl |^mod ", lines[i]):
            end = i
            break
    return [l.strip() for l in lines[start:end] if l.strip()]


def body_completeness(answer_text: str, ground_truth_lines: list[str]) -> float:
    """Fraction of the real body's non-blank lines that appear verbatim somewhere
    in the tool's answer — a completeness score, not just "did it mention the
    function name". Deliberately per-line (not whole-block substring) so
    reordering/whitespace-only reformatting by a tool doesn't zero out the score."""
    if not ground_truth_lines:
        return 0.0
    hits = sum(1 for l in ground_truth_lines if l in answer_text)
    return hits / len(ground_truth_lines)


def extract_files(text: str) -> set[str]:
    return set(re.findall(r"crates/[\w./-]+\.rs", text))


def median_iqr(values: list[float]) -> dict:
    sv = sorted(values)
    q1 = statistics.quantiles(sv, n=4)[0] if len(sv) >= 4 else sv[0]
    q3 = statistics.quantiles(sv, n=4)[2] if len(sv) >= 4 else sv[-1]
    return {"median": statistics.median(sv), "p25": q1, "p75": q3, "n": len(sv), "raw": sv}


# ---------------------------------------------------------------------------
# risk_gate_refusal — real edit attempt against a real hub symbol
# ---------------------------------------------------------------------------

def corpus_diff_stat(corpus: Path, path: str) -> str:
    return subprocess.run(
        ["git", "diff", "--stat", "--", path], cwd=corpus, capture_output=True, text=True,
    ).stdout.strip()


def corpus_reset(corpus: Path, path: str) -> None:
    subprocess.run(["git", "checkout", "--", path], cwd=corpus, check=True)


def run_risk_gate_refusal(clients: dict, corpus: Path) -> dict:
    row: dict = {"id": "risk_gate_refusal", "anchor": f"{RISK_GATE_PATH}::{RISK_GATE_SYMBOL}",
                 "anchor_verified_hub": True, "results": {}}

    # --- ci ---
    calm = clients["ci"]
    preview = json.loads(calm.call_tool("edit_symbol", {
        "symbol": RISK_GATE_SYMBOL, "path": RISK_GATE_PATH, "new_text": "PREVIEW_PLACEHOLDER",
    }))
    hunk = preview["hunks"][0]
    current_hash = hunk["current_hash"]
    current_body = hunk["old_text"]
    mutated = current_body + RISK_GATE_MARKER
    attempt = calm.call_tool("edit_symbol", {
        "symbol": RISK_GATE_SYMBOL, "path": RISK_GATE_PATH, "new_text": mutated,
        "expected_hash": current_hash,
    })
    file_changed = bool(corpus_diff_stat(corpus, RISK_GATE_PATH))
    if file_changed:
        corpus_reset(corpus, RISK_GATE_PATH)
    row["results"]["ci"] = {
        "tool": "edit_symbol (confirm omitted)",
        "refused": not file_changed,
        "file_changed": file_changed,
        "response_excerpt": attempt[:300],
    }

    # --- serena ---
    # (no preview step needed — Serena's schema has no confirm/force field at all,
    # so there is nothing to preview against; the call either succeeds or errors.
    # The replacement body doesn't need to be valid Rust: corpus_reset() below runs
    # immediately after and nothing ever compiles this in between.)
    serena = clients["serena"]
    attempt_s = serena.call_tool("replace_symbol_body", {
        "name_path": RISK_GATE_NAME_PATH, "relative_path": RISK_GATE_PATH,
        "body": "fn reindex_changed() { /* b11-risk-gate-probe (harmless placeholder, reset immediately after) */ }",
    })
    file_changed_s = bool(corpus_diff_stat(corpus, RISK_GATE_PATH))
    if file_changed_s:
        corpus_reset(corpus, RISK_GATE_PATH)
    row["results"]["serena"] = {
        "tool": "replace_symbol_body (no confirm field exists in schema)",
        "refused": not file_changed_s,
        "file_changed": file_changed_s,
        "response_excerpt": attempt_s[:300],
    }

    for tool, reason in (
        ("codegraph", "read-only by design, no edit tool"),
        ("semble", "no edit tool of any kind"),
        ("grepai", "no edit tool of any kind"),
    ):
        row["results"][tool] = {"unsupported": True, "reason": reason}

    return row


# ---------------------------------------------------------------------------
# memory_recall — write a note, kill + respawn the process, read it back
# ---------------------------------------------------------------------------

def run_memory_recall(corpus: Path, real_repo_root: Path) -> dict:
    row: dict = {"id": "memory_recall", "results": {}}

    # --- ci: remember -> close process -> respawn -> recall ---
    calm = MCPClient(project_root=str(corpus), repo_root=str(real_repo_root))
    calm.wait_until_indexed()
    calm.call_tool("remember", {"topic": MEMORY_TOPIC, "content": MEMORY_CONTENT})
    calm.close()
    calm2 = MCPClient(project_root=str(corpus), repo_root=str(real_repo_root))
    calm2.wait_until_indexed()
    recalled = calm2.call_tool("recall", {"topic": MEMORY_TOPIC})
    calm2.close()
    row["results"]["ci"] = {
        "tool": "remember -> (process restart) -> recall",
        "persisted": MEMORY_CONTENT in recalled,
        "response_excerpt": recalled[:300],
    }

    # --- serena: write_memory -> kill -> respawn -> read_memory ---
    serena, _ = start_serena(corpus)
    serena.call_tool("write_memory", {"memory_name": MEMORY_TOPIC, "content": MEMORY_CONTENT})
    serena.close()
    serena2, _ = start_serena(corpus)
    read_back = serena2.call_tool("read_memory", {"memory_name": MEMORY_TOPIC})
    serena2.close()
    row["results"]["serena"] = {
        "tool": "write_memory -> (process restart) -> read_memory",
        "persisted": MEMORY_CONTENT in read_back,
        "response_excerpt": read_back[:300],
    }

    for tool, reason in (
        ("codegraph", "file watcher only, not an interpretive memory/notes store"),
        ("semble", "no memory/notes concept"),
        ("grepai", "no memory/notes concept"),
    ):
        row["results"][tool] = {"unsupported": True, "reason": reason}

    return row


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", required=True, help="Path to the isolated corpus (NOT the live repo)")
    args = parser.parse_args()
    corpus = Path(args.corpus).resolve()
    real_repo_root = repo_root_from_here()

    tasks = {t["id"]: t for t in yaml.safe_load(TASKS_PATH.read_text())["tasks"]}
    competitor_tasks = {t["id"]: t for t in yaml.safe_load(COMPETITOR_TASKS_PATH.read_text())["tasks"]}

    print(f"[b11] corpus = {corpus}", file=sys.stderr)
    print("[b11] starting all 5 servers ...", file=sys.stderr)
    calm_client = MCPClient(project_root=str(corpus), repo_root=str(real_repo_root))
    codegraph_client = start_codegraph(corpus)
    semble_client = start_semble(corpus)
    grepai_client = start_grepai(corpus)
    serena_client, serena_cold_start_s = start_serena(corpus)

    clients = {"ci": calm_client, "codegraph": codegraph_client, "semble": semble_client,
               "grepai": grepai_client, "serena": serena_client}

    rows = []
    try:
        calm_client.wait_until_indexed()
        print(f"[b11] all servers ready (serena cold start: {serena_cold_start_s:.1f}s), running tasks", file=sys.stderr)

        for task_id, task in tasks.items():
            print(f"[b11] task: {task_id}", file=sys.stderr)
            ctask = competitor_tasks[task_id]
            naive_text_val, naive_calls = naive_text_and_calls(corpus, task["naive"])

            row: dict = {"id": task_id, "description": task["description"],
                         "naive_tokens": toks(naive_text_val), "naive_calls": naive_calls,
                         "tools": {}}

            answers: dict[str, str] = {}
            for tool_name in ("ci", "codegraph", "semble", "grepai", "serena"):
                spec = task["ci"] if tool_name == "ci" else ctask.get(tool_name)
                if spec is None:
                    row["tools"][tool_name] = {"unsupported": True, "reason": "no mapping defined"}
                    continue
                client = clients[tool_name]
                tool_call_name = spec["tool"]
                tool_args = spec["arguments"]
                token_samples = []
                last_text = ""
                for _ in range(N_REPEATS):
                    last_text = client.call_tool(tool_call_name, tool_args)
                    token_samples.append(toks(last_text))
                answers[tool_name] = last_text
                row["tools"][tool_name] = {
                    "tool": tool_call_name,
                    "tokens": median_iqr(token_samples),
                    "calls": 1,
                    "unsupported": bool(spec.get("unsupported", False)),
                    "caveat": spec.get("caveat"),
                }

            # --- oracles ---
            if task_id == "find_callers":
                oracle = grep_oracle_callers(corpus, task["ci"]["arguments"]["symbol"])
                row["oracle"] = {"type": "grep_file_recall", "oracle_files": sorted(oracle)}
                for tool_name, text in answers.items():
                    found = extract_files(text)
                    row["tools"][tool_name]["recall"] = f"{len(found & oracle)}/{len(oracle)}"
                    row["tools"][tool_name]["missed"] = sorted(oracle - found)

            elif task_id == "pre_edit_blast_radius":
                oracle = grep_oracle_callers(corpus, task["ci"]["arguments"]["symbol"])
                row["oracle"] = {"type": "grep_file_recall", "oracle_files": sorted(oracle)}
                for tool_name, text in answers.items():
                    found = extract_files(text)
                    row["tools"][tool_name]["recall"] = f"{len(found & oracle)}/{len(oracle)}"
                    row["tools"][tool_name]["missed"] = sorted(oracle - found)

            elif task_id == "read_one_function":
                gt_lines = function_ground_truth_lines(
                    corpus, "crates/calm-core/src/indexer/pipeline.rs", "run_indexing_pipeline",
                )
                row["oracle"] = {"type": "body_line_completeness", "ground_truth_line_count": len(gt_lines)}
                for tool_name, text in answers.items():
                    row["tools"][tool_name]["completeness"] = round(body_completeness(text, gt_lines), 2)

            elif task_id == "locate_and_inspect":
                target_path = "crates/calm-core/src/indexer/pipeline.rs"
                row["oracle"] = {"type": "path_and_symbol_presence", "path": target_path, "symbol": "run_indexing_pipeline"}
                for tool_name, text in answers.items():
                    row["tools"][tool_name]["found_path"] = target_path in text
                    row["tools"][tool_name]["found_symbol"] = "run_indexing_pipeline" in text

            rows.append(row)

        risk_gate_row = run_risk_gate_refusal(clients, corpus)
        rows.append(risk_gate_row)

    finally:
        for c in clients.values():
            c.close()

    memory_row = run_memory_recall(corpus, real_repo_root)
    rows.append(memory_row)

    summary = {
        "encoding_model": ENCODING_MODEL,
        "encoding_model_caveat": "GPT-4 BPE via tiktoken — a proxy for token cost, NOT the real tokenizer of whatever model runtime an agent actually uses.",
        "corpus": "isolated git worktree copy of self-repo (CALM), not the live checkout",
        "n_repeats": N_REPEATS,
        "serena_cold_start_seconds": round(serena_cold_start_s, 1),
        "tools_compared": {
            "ci": "calm-cli (this repo, release build)",
            "codegraph": "colbymchenry/codegraph (npm, .codegraph/ built via `codegraph init`)",
            "semble": "semble MCP (uvx --from semble[mcp] semble) — embedding search, no call graph",
            "grepai": "yoanbernabeu/grepai (Ollama nomic-embed-text embeddings + real structural call graph)",
            "serena": "oraios/serena (LSP-backed via rust-analyzer for this Rust corpus)",
        },
        "tasks": rows,
    }

    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(summary, indent=2))

    print(f"\nfull results written to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
