#!/usr/bin/env python3
"""B7 -- Task-Correctness benchmark (Phase 1: fd [Rust], flask [Python];
Phase 2: express [JS], zod [TS], gin [Go]; Phase 3: spring-petclinic [Java]).

Measures whether the CALM-scripted refactor workflow (edit_context -> edit at
each real call site -> diff_impact) completes a real rename task more
correctly than a naive grep-and-edit workflow, using a DETERMINISTIC oracle
(the corpus's own real build/test toolchain + an independently-computed
callsite ground truth) -- no LLM judge, per the design spec's explicit
constraint (docs/superskills/specs/2026-07-30-calm-dfb-levers-design.md §1.2).

Differs from B11/B12 (tool-surface correctness: given a fixed query, is the
ANSWER right) -- B7 measures TASK correctness: given a refactor, does the
whole edit loop complete it without breaking the corpus's own tests or
missing a real call site. This is the Serena-style claim ("8-12 manual steps
-> 1 call, fewer errors") benchmarks/README.md names as B7's origin.

Audit-design mitigations folded in directly (see the spec's Risk Assessment),
not just described:
  - Verified LIVE this session that fd's `cargo test` and flask's
    `uv sync --frozen && uv run pytest` are the correct commands -- a bare
    `pip install pytest` reproducibly broke flask's test_cli.py against a
    too-new pytest (real failure, not hypothetical).
  - Baseline green check runs BEFORE either arm -- a pre-existing
    flaky/failing corpus test is reported as `skipped`, never misattributed
    to an arm (closes the L6 finding).
  - Oracle references are extension-filtered AND bare-identifier, not
    call-shaped (oracle.py::real_references) -- B12's raw git-grep oracle
    would otherwise count doc-prose `.rst` mentions as Python "call sites"
    (found live while picking flask's first candidate, `init_db`), and a
    call-shaped-only pattern misses bare re-export references entirely
    (found live on zod's `prettifyError`: `export { prettifyError }` has no
    trailing paren).

SAFETY: every corpus is a FRESH throwaway clone per (task, arm) -- git clone
--local from B12's read-only pinned source, never the pinned source itself.
Mirrors B12's corpora.py isolation (a crash mid-run just leaves a clone to
garbage-collect), not B11's mutate-then-git-checkout-reset pattern.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import yaml

LIB_DIR = Path(__file__).resolve().parents[1] / "lib"
B12_DIR = Path(__file__).resolve().parents[1] / "b12_tier1_tier2_tool_correctness"
sys.path.insert(0, str(LIB_DIR))
sys.path.insert(0, str(B12_DIR))

from mcp_client import MCPClient, repo_root_from_here  # noqa: E402
import corpora as b12_corpora  # noqa: E402
from oracle import real_references, build_test_gate, baseline_green  # noqa: E402

TASKS_PATH = LIB_DIR / "refactor_tasks.yaml"
# Deliberately OUTSIDE the CALM repo tree, unlike B12's own `.work/` (which is
# fine for B12 since it never actually compiles its corpus copies -- read-only
# tool checks only). B7 is the first benchmark that runs a REAL `cargo test`
# inside the corpus clone, and cargo auto-discovers an ancestor `[workspace]`
# if the clone is nested inside CALM's own workspace -- reproduced live this
# session ("current package believes it's in a workspace when it's not"),
# the exact gotcha corpora.py's own docstring warns about for the PINNED
# source; it applies equally to per-run work copies, which corpora.py never
# needed to worry about for its own read-only use case.
WORK_ROOT = repo_root_from_here().parent / "calm-b7-work"

SRC_EXTS = {"rust": (".rs",), "python": (".py",), "javascript": (".js",), "typescript": (".ts",), "go": (".go",), "java": (".java",)}


def fresh_clone(lang: str, arm: str) -> Path:
    """A brand-new clone per (task, arm) -- arms must never see each other's
    mutations, and this never touches B12's own pinned source or `.work/`."""
    corpus = b12_corpora.get_corpus(lang)
    dest = WORK_ROOT / f"{lang}-{arm}"
    if dest.exists():
        shutil.rmtree(dest)
    WORK_ROOT.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "clone", "--quiet", str(corpus.source), str(dest)], check=True)
    return dest


def word_bounded_sub(text: str, old: str, new: str) -> tuple[str, int]:
    pattern = re.compile(rf"(?<![A-Za-z0-9_]){re.escape(old)}(?![A-Za-z0-9_])")
    return pattern.subn(new, text)


def apply_rename_at(corpus: Path, path: str, old_name: str, new_name: str) -> bool:
    """Word-bounded rename of every occurrence of `old_name` anywhere in
    `path` -- deliberately whole-file, not single-line, so a multi-line call
    expression is never half-renamed."""
    f = corpus / path
    if not f.exists():
        return False
    text = f.read_text()
    new_text, n = word_bounded_sub(text, old_name, new_name)
    if n == 0:
        return False
    f.write_text(new_text)
    return True


def run_naive_arm(corpus: Path, task: dict) -> dict:
    """No call graph: grep for the BARE identifier (not just call-shaped
    occurrences -- see oracle.real_references' docstring for why a rename
    needs every reference, not just calls) and rename every hit in every
    matched file within the corpus's own source extension. Simulates a
    thorough naive agent doing a textual rename with no structural knowledge
    of the codebase (same spirit as naive_workflow.py's
    grep_then_cat_matches, extended here to a real mutation instead of just
    a read).

    NOTE: an earlier draft used a call-shaped `NAME\\(` pattern with a
    double-escaped backslash bug (`\\\\(` inside an `rf"..."` raw string --
    an unbalanced/invalid ERE that made `git grep` exit non-zero and print
    nothing, silently zeroing this arm's results). Both the call-shape
    restriction and the escaping bug are fixed here."""
    exts = SRC_EXTS[task["lang"]]
    proc = subprocess.run(
        ["git", "grep", "-l", "-E", rf"(^|[^A-Za-z0-9_]){re.escape(task['symbol'])}($|[^A-Za-z0-9_])"],
        cwd=corpus, capture_output=True, text=True,
    )
    files = [f for f in proc.stdout.splitlines() if f.endswith(exts)]
    edited = [f for f in files if apply_rename_at(corpus, f, task["symbol"], task["new_name"])]
    return {"arm": "naive", "files_touched": sorted(edited), "tool_calls": 1 + len(files)}


_SCIP_PROVIDER_BY_LANG: dict[str, str] = {"typescript": "javascript"}


def run_calm_arm(corpus: Path, task: dict, real_repo_root: Path, lang: str) -> dict:
    """CALM-scripted: ask CALM's own edit_context for the blast radius (this
    IS what a real agent would see -- comparing this set against the
    independent oracle is the callsite-recall metric), then rename the
    definition + every file CALM reported as a direct caller.

    NOTE: CallerEntry has no `path` field (removed as a duplicate of data
    already in `symbol`, per its own doc comment in
    crates/calm-server/src/tools/guardrails.rs) -- the file path is the
    substring of `symbol` before the first `::`. Verified against the real
    edit_context.snap schema before writing this, not guessed.

    2026-08-18 fix: backports B13/B12's `force_scip_refresh` -- without it,
    `edit_context`'s caller-file set (the metric this whole arm exists to
    score) could be read before the async SCIP overlay pass finishes
    upgrading edges to `formal`, understating CALM's real recall on a
    corpus that happens to index fast. See B12's `force_scip_refresh`
    docstring for the live verification this race is real. `lang` needs
    the same typescript->javascript provider-name mapping B12 needed
    (there is no separate "typescript" scip_refresh provider)."""
    client = MCPClient(project_root=str(corpus), repo_root=str(real_repo_root))
    tool_calls = 0
    try:
        client.wait_until_indexed()
        provider = _SCIP_PROVIDER_BY_LANG.get(lang, lang)
        try:
            client.call_tool("scip_refresh", {"lang": provider})
        except Exception:  # noqa: BLE001 -- best-effort, matches B12's posture
            pass
        raw = client.call_tool("edit_context", {"symbol": task["symbol"]})
        tool_calls += 1
        ctx = json.loads(raw)
        caller_files = set()
        for c in ctx.get("callers", []):
            sym = c.get("symbol", "")
            if "::" in sym:
                caller_files.add(sym.split("::", 1)[0])
        caller_files.add(task["def_path"])
        edited = [f for f in sorted(caller_files) if apply_rename_at(corpus, f, task["symbol"], task["new_name"])]
        tool_calls += len(caller_files)
        client.call_tool("diff_impact", {})  # mandatory post-edit verification, AGENTS.md Stage 7
        tool_calls += 1
        return {
            "arm": "calm", "files_touched": sorted(edited),
            "calm_reported_callers": sorted(caller_files), "tool_calls": tool_calls,
        }
    finally:
        client.close()


def score_arm(oracle_files: set[str], result: dict) -> dict:
    touched = set(result["files_touched"])
    result["recall"] = round(len(touched & oracle_files) / len(oracle_files), 3) if oracle_files else 1.0
    result["missed"] = sorted(oracle_files - touched)
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--task", help="Run only this task id (default: all)")
    args = parser.parse_args()

    real_repo_root = repo_root_from_here()
    tasks = yaml.safe_load(TASKS_PATH.read_text())["tasks"]
    if args.task:
        tasks = [t for t in tasks if t["id"] == args.task]

    rows = []
    for task in tasks:
        print(f"[b7] task: {task['id']}", file=sys.stderr)
        lang = task["lang"]
        exts = SRC_EXTS[lang]
        pinned_source = b12_corpora.get_corpus(task["corpus"]).source

        # real_references (bare-identifier, not call-shaped) is the correct
        # oracle for "does this rename fully compile" -- see its docstring
        # for the real failure this fixed (a bare re-export statement with
        # no trailing paren, missed by an earlier call-shaped pattern).
        oracle_pairs = real_references(
            pinned_source, task["symbol"], task["def_path"], task["def_line"], lang, exts,
        )
        oracle_files = {p for p, _ in oracle_pairs} | {task["def_path"]}

        row: dict = {"id": task["id"], "lang": lang, "oracle_callsite_files": sorted(oracle_files)}

        print(f"[b7]   baseline green check ...", file=sys.stderr)
        baseline_corpus = fresh_clone(lang, "baseline")
        baseline = baseline_green(baseline_corpus, task.get("build_cmd"), task["test_cmd"])
        row["baseline_green"] = baseline.passed
        if not baseline.passed:
            row["skipped_reason"] = "baseline not green -- pre-existing corpus failure, not attributable to either arm"
            row["baseline_output_tail"] = baseline.output[-1500:]
            rows.append(row)
            continue

        print(f"[b7]   naive arm ...", file=sys.stderr)
        naive_corpus = fresh_clone(lang, "naive")
        naive_result = run_naive_arm(naive_corpus, task)
        naive_bt = build_test_gate(naive_corpus, task.get("build_cmd"), task["test_cmd"])
        naive_result["build_pass"] = naive_bt.passed
        naive_result["output_tail"] = naive_bt.output[-1500:]
        row["naive"] = score_arm(oracle_files, naive_result)

        print(f"[b7]   calm arm ...", file=sys.stderr)
        calm_corpus = fresh_clone(lang, "calm")
        calm_result = run_calm_arm(calm_corpus, task, real_repo_root, lang)
        calm_bt = build_test_gate(calm_corpus, task.get("build_cmd"), task["test_cmd"])
        calm_result["build_pass"] = calm_bt.passed
        calm_result["output_tail"] = calm_bt.output[-1500:]
        row["calm"] = score_arm(oracle_files, calm_result)

        rows.append(row)

    summary = {
        "phase": "B7 Phase 1-3 (fd/Rust, flask/Python, express/JS, zod/TS, gin/Go, spring-petclinic/Java)",
        "methodology": "deterministic oracle only (build/test pass + independent "
                        "callsite recall via B12's ground_truth, extension-filtered) "
                        "-- no LLM judge, per design spec constraint",
        "tasks": rows,
    }
    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(summary, indent=2))

    print()
    print("| task | baseline | naive build_pass | naive recall | calm build_pass | calm recall |")
    print("|---|---|---|---|---|---|")
    for row in rows:
        if "skipped_reason" in row:
            print(f"| {row['id']} | SKIPPED | - | - | - | - | ({row['skipped_reason']}) |")
            continue
        n, c = row["naive"], row["calm"]
        print(f"| {row['id']} | green | {n['build_pass']} | {n['recall']} | {c['build_pass']} | {c['recall']} |")
    print(f"\nfull results written to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
