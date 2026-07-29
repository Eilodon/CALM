#!/usr/bin/env python3
"""B12 -- Tier-1/Tier-2 MCP tool-surface correctness, 6 Tier-0 languages, external OSS repos.

Different axis from every other benchmark in this suite: `b2_call_graph_quality`
and the 2026-07-28 6-lang accuracy benchmark measure **call-graph edge precision/
recall straight out of `index.db`** -- they already answered "is CALM's resolved
graph correct" (formal ~90-93% across all 6 Tier-0 langs, per memory
`calm-tier0-benchmark-rootcause-2026-07-28`). `b10`/`b11` compare `calm` against
competitors, self-repo only. `resolution/` measures tier distribution, no oracle.

This one drives the actual MCP **tool surface** end-to-end via real JSON-RPC
`tools/call` (same harness B4/B6/B10/B11 already use) -- `repo_overview`,
`search`, `source`, `file_overview`, `callers`, `edit_context`, `edit_lines`,
`edit_symbol`, `diff_impact`, `hotspots` -- on real external OSS repos, simulating
what an external user who just installed CALM on their own project actually gets,
not what the underlying database contains.

Methodology (see README.md for the full writeup):
  - Lightweight, tool-independent ground truth: a small per-language regex
    extractor (`ground_truth.py`) + plain `git grep` -- deliberately NOT CALM's
    own tree-sitter parser, so it's a real external oracle. No SCIP toolchain
    reinstalls (rust-analyzer/scip-go/scip-python/scip-typescript/scip-java are
    not all present on this machine right now -- see README for the honest
    "full power" scope this implies).
  - "Full power" = CALM's actual default Cargo feature set
    (`embeddings,tier0-5,scip-overlay` -- crates/calm-cli/Cargo.toml), not a
    reduced build. The `target/release/calm` binary already has this.
  - Each language runs against a throwaway, freshly-cloned copy (see
    `corpora.py::prepare_worktree`) with NO pre-existing `.calm/` -- a genuine
    cold-start simulation, not a warm re-index of a previously-touched corpus.
  - Report only -- this script does not modify any CALM production code. Real
    bugs found are written to `results.json` + a printed summary for the user
    to triage.

Dry-run notes (flask/python, 2026-07-29) -- kept here instead of quietly
rewriting history, same self-audit norm the 2026-07-28 sessions established:
  1. Uniform-random ground-truth sampling picked common-English-word/common-
     stdlib-method identifiers (`index`, `add`, `default`, `fail`...) with
     hundreds of unrelated corpus-wide matches -- not fair samples. Fixed by
     `ground_truth.sample_distinctive` (bounded occurrence count before
     sampling).
  2. The first `git_grep_call_sites` used a bare fixed-string `name(` --
     massively overcounted "call sites" that were actually (a) a substring hit
     inside a longer, unrelated identifier (`jinja_loader(` inside
     `create_global_jinja_loader(`), or (b) OTHER `def name(...)` redefinitions
     of the same name in a different scope (`my_reverse` redefined as ~10
     separate local test-helper closures), never a real call at all. Fixed in
     `ground_truth.py` (word-bounded regex + a same-language definition-line
     filter).
  3. `callers()`'s `direct` list correctly excludes Python instance-method
     calls through an unresolved receiver type (`app.register_blueprint(bp)`)
     -- CALM puts these in `ambiguous`, not `direct`, which is documented,
     correct behavior for dynamic dispatch (matches memory
     `calm-tier0-benchmark-rootcause-2026-07-28`'s python-ambiguous-fan-out
     finding), NOT a zero-recall bug. `check_callers` reads `ambiguous`/
     `ambiguous_count` before flagging anything.
  4. Names redefined in MANY unrelated scopes (`__init__` on every class,
     `create_app`/`Test` repeated per test module) aren't well-posed single-
     target ground truth: grep's "call/definition sites" for them are mostly
     for OTHER same-named symbols, not the one definition actually sampled.
     Fixed by `ground_truth.unique_definitions` (also excludes dunder/magic
     methods), applied before sampling for search/callers/edit checks alike.
  5. `edit_context`'s real response has NO top-level `is_hub` boolean
     (verified against the raw JSON) and `risk_assessment` is a
     `{"level": ..., "reasons": [...]}` dict, not a bare string -- fixed by
     reading `risk_assessment["level"]`, using `level == "high"` as the
     practical proxy for the hub/high-risk confirm-gate case.
  6. The edit probe marker was a hardcoded C-style `/* ... */` comment
     appended to source lines -- invalid syntax on Python (no block
     comments), so every Python edit attempt risked a `PARSE_ERROR` unrelated
     to whatever was actually being tested. Fixed with a per-language
     single-line-comment prefix (`LINE_COMMENT`).
  7. A real, low-risk (not high-risk/hub) function can still trip
     `CONFIRM_REQUIRED` for an unrelated reason: "zero-confirmed-caller
     test-only symbol... only the test harness discovers and runs it by
     convention/reflection" -- CALM's confirm-gate is broader than "hub or
     risk=high". The round-trip checks now retry once with
     `confirm: true` + a reason when this happens, so they still exercise
     the actual edit_lines write/staleness behavior instead of stopping at
     the gate. The DEDICATED hub-gate probe is unaffected (it explicitly
     omits confirm to verify the gate refuses).
  8. The original hub-gate probe used `edit_symbol` to replace a whole
     function body with a bare comment -- on an indentation-sensitive
     language (Python) or a language expecting a full statement, that's
     itself invalid syntax, so it triggered `PARSE_ERROR` before the
     confirm-gate was ever reached, testing the wrong thing. Switched to a
     single trailing-comment append via `edit_lines` on the hub symbol's own
     def line (mirrors the safe transform the round-trip checks already use)
     so a refusal can only be attributed to the confirm-gate itself.

Usage:
    benchmarks/.venv/bin/python benchmarks/b12_tier1_tier2_tool_correctness/run_benchmark.py [--lang python,rust,...] [--keep-worktrees]
"""
from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from mcp_client import MCPClient, MCPError, repo_root_from_here  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpora import CORPORA_LANGS, cleanup_worktree, pinned_commit, prepare_worktree  # noqa: E402
from ground_truth import (  # noqa: E402
    Definition, extract_definitions, git_grep_call_sites, sample_distinctive, unique_definitions,
)

RNG_SEED = 20260729
SAMPLE_N_SEARCH = 20
SAMPLE_N_DEEP = 6
SEARCH_KINDS = ("grep", "symbol", "hybrid")
_CALLABLE_KINDS = ("function", "method", "assigned_function", "prototype_method")
LINE_COMMENT: dict[str, str] = {
    "python": "#", "rust": "//", "go": "//", "java": "//", "javascript": "//", "typescript": "//",
}


def jload(raw: str) -> dict:
    """`json.loads`, but tolerant of trailing extra data after the first
    complete JSON object -- some tool responses concatenate more than one
    text content block (e.g. a caveat/warning appended after the main JSON
    payload) into a single string with no separator, which a strict
    `json.loads` rejects as `Extra data`. We only ever want the primary
    payload here, so decoding just the first object and discarding the rest
    is the right call, not a bug we should chase further."""
    return json.JSONDecoder().raw_decode(raw)[0]


def git_reset(corpus_path: Path, path: str) -> None:
    subprocess.run(["git", "checkout", "--", path], cwd=corpus_path, check=False, capture_output=True)


def _has_ambiguous_signal(data: dict) -> bool:
    amb = data.get("ambiguous")
    if isinstance(amb, list) and amb:
        return True
    count = data.get("ambiguous_count")
    return isinstance(count, int) and count > 0


def _risk_level(ctx: dict) -> str | None:
    return (ctx.get("risk_assessment") or {}).get("level")


def _callable_unique_defs(defs: list[Definition]) -> list[Definition]:
    return [d for d in unique_definitions(defs) if d.kind in _CALLABLE_KINDS and len(d.name) >= 4]


def _is_confirm_required(resp: dict) -> bool:
    return resp.get("error", {}).get("code") == "CONFIRM_REQUIRED"


def _call_edit_lines_allow_confirm(client: MCPClient, path: str, hunk: dict) -> dict:
    """Call edit_lines; if refused specifically for CONFIRM_REQUIRED, retry
    once with confirm=true + a reason -- see module dry-run note 7. Any OTHER
    refusal (stale hash, parse error, path traversal, ...) is returned as-is,
    unretried, since retrying those would mask a real signal."""
    resp = jload(client.call_tool("edit_lines", {"path": path, "edits": [hunk]}))
    if _is_confirm_required(resp):
        confirmed_hunk = dict(hunk)
        resp = jload(client.call_tool("edit_lines", {
            "path": path, "edits": [confirmed_hunk], "confirm": True,
            "reason": "b12 benchmark probe -- comment-only append, verifying edit_lines round-trip behavior",
        }))
    return resp


# ---------------------------------------------------------------------------
# Per-tool checks
# ---------------------------------------------------------------------------

def check_repo_overview(client: MCPClient, lang: str) -> dict:
    data = jload(client.call_tool("repo_overview", {}))
    langs = data.get("languages", [])
    return {
        "pass": lang in langs and data.get("indexing_phase") in ("ready", "indexing"),
        "languages": langs,
        "indexing_phase": data.get("indexing_phase"),
        "entry_points_count": len(data.get("entry_points", []) or []),
        "core_symbols_count": len(data.get("core_symbols", []) or []),
    }


def check_search(client: MCPClient, corpus_path: Path, defs: list[Definition], seed: int = RNG_SEED) -> dict:
    rng = random.Random(seed)
    sample = sample_distinctive(rng, defs, corpus_path, k=SAMPLE_N_SEARCH)
    per_kind: dict[str, list[bool]] = {k: [] for k in SEARCH_KINDS}
    failures = []
    for d in sample:
        for kind in SEARCH_KINDS:
            args = {"query": d.name, "kind": kind, "limit": 20}
            try:
                results = jload(client.call_tool("search", args)).get("results", [])
            except MCPError as e:
                per_kind[kind].append(False)
                failures.append({"name": d.name, "kind": kind, "error": str(e)})
                continue
            hit = any(r.get("path") == d.path or r.get("name") == d.name for r in results)
            per_kind[kind].append(hit)
            if not hit:
                failures.append({
                    "name": d.name, "kind": kind, "def_path": d.path, "def_line": d.line,
                    "top_results": results[:3],
                })
    file_paths = sorted({d.path for d in defs})
    file_sample = rng.sample(file_paths, k=min(10, len(file_paths))) if file_paths else []
    file_hits = []
    for path in file_sample:
        results = jload(client.call_tool("search", {"query": Path(path).name, "kind": "file", "limit": 10})).get("results", [])
        file_hits.append(any(r.get("path") == path for r in results))
    recall = {k: (sum(v) / len(v) if v else None) for k, v in per_kind.items()}
    recall["file"] = (sum(file_hits) / len(file_hits)) if file_hits else None
    return {"sample_size": len(sample), "recall": recall, "failures": failures[:15]}


def check_source(client: MCPClient, corpus_path: Path, defs: list[Definition], seed: int = RNG_SEED) -> dict:
    rng = random.Random(seed)
    sample = rng.sample(defs, k=min(15, len(defs))) if defs else []
    mismatches = []
    etag_ok = 0
    contains_ok = 0
    for d in sample:
        data = jload(client.call_tool("source", {"path": d.path, "line": d.line, "end_line": d.line}))
        try:
            actual_line = (corpus_path / d.path).read_text().splitlines()[d.line - 1]
        except (OSError, IndexError):
            continue
        returned = data.get("source", "")
        returned_text = returned.split("\t", 1)[-1] if "\t" in returned else returned
        if returned_text.rstrip("\n") != actual_line:
            mismatches.append({"path": d.path, "line": d.line, "expected": actual_line, "got": returned_text})
        etag = data.get("etag")
        if etag:
            data2 = jload(client.call_tool("source", {
                "path": d.path, "line": d.line, "end_line": d.line, "if_none_match": etag,
            }))
            if data2.get("not_modified") is True:
                etag_ok += 1
        try:
            sym_data = jload(client.call_tool("source", {"symbol": d.name, "path": d.path}))
        except MCPError:
            continue
        ls, le = sym_data.get("line_start"), sym_data.get("line_end")
        if ls is not None and le is not None and ls <= d.line <= le:
            contains_ok += 1
    return {
        "checked": len(sample),
        "byte_exact_mismatches": mismatches,
        "etag_round_trip_ok": f"{etag_ok}/{len(sample)}",
        "symbol_mode_contains_def_line": f"{contains_ok}/{len(sample)}",
    }


def check_file_overview(client: MCPClient, defs: list[Definition], seed: int = RNG_SEED) -> list[dict]:
    rng = random.Random(seed)
    by_file: dict[str, list[Definition]] = {}
    for d in defs:
        by_file.setdefault(d.path, []).append(d)
    candidates = [p for p, ds in by_file.items() if len(ds) >= 2]
    sample = rng.sample(candidates, k=min(8, len(candidates))) if candidates else []
    rows = []
    for path in sample:
        data = jload(client.call_tool("file_overview", {"path": path}))
        symbols = data.get("symbols", []) or []
        gt_names = {d.name for d in by_file[path]}
        found_names = {s.get("name") for s in symbols}
        rows.append({
            "path": path,
            "ground_truth_count": len(gt_names),
            "file_overview_count": len(symbols),
            "overlap": len(gt_names & found_names),
            "missing_examples": sorted(gt_names - found_names)[:5],
        })
    return rows


def check_callers(client: MCPClient, corpus_path: Path, defs: list[Definition], lang: str, seed: int = RNG_SEED) -> list[dict]:
    rng = random.Random(seed)
    func_defs = _callable_unique_defs(defs)
    rng.shuffle(func_defs)
    rows = []
    for d in func_defs:
        if len(rows) >= SAMPLE_N_DEEP:
            break
        grep_sites = git_grep_call_sites(corpus_path, d.name, d.path, d.line, lang)
        if len(grep_sites) < 2:
            continue
        data = jload(client.call_tool("callers", {"symbol": d.name, "path": d.path}))
        direct = data.get("direct", []) or []
        ambiguous_signal = _has_ambiguous_signal(data)
        rows.append({
            "symbol": d.name, "path": d.path,
            "grep_call_sites": len(grep_sites),
            "callers_direct_count": len(direct),
            "ambiguous_signal": ambiguous_signal,
            # only a real finding if CALM found NOTHING at all (not direct, not
            # even flagged ambiguous) while an independent, call-shaped grep
            # oracle found several -- see dry-run note 3 above for why
            # ambiguous-but-present doesn't count as zero recall.
            "zero_recall_bug": len(direct) == 0 and not ambiguous_signal and len(grep_sites) >= 3,
        })
    return rows


def check_hotspots(client: MCPClient, lang: str) -> dict:
    data = jload(client.call_tool("hotspots", {"min_churn": 0, "top_n": 10}))
    listy = None
    for v in data.values():
        if isinstance(v, list):
            listy = v
            break
    note = None
    if lang != "rust":
        note = (
            "corpus is a --depth 1 shallow clone (1 commit of history) -- churn signal is "
            "necessarily near-zero. Corpus limitation, not a CALM defect. Smoke-test only."
        )
    return {"non_empty": bool(listy), "count": (len(listy) if listy is not None else None), "note": note, "raw_keys": list(data.keys())}


def check_edit_workflow(client: MCPClient, corpus_path: Path, defs: list[Definition], lang: str, seed: int = RNG_SEED) -> dict:
    """edit_context + edit_lines (old_text mode + hash mode, each with a
    staleness-reuse probe) + diff_impact + the hub/high-risk confirm gate,
    on real functions per language. See module dry-run notes 5-8 for the
    field-name/marker/confirm-retry fixes this needed after the first
    flask dry run."""
    rng = random.Random(seed)
    func_defs = _callable_unique_defs(defs)
    rng.shuffle(func_defs)
    comment = LINE_COMMENT.get(lang, "//")

    target = None
    hub_example = None
    for d in func_defs[:30]:
        try:
            ctx = jload(client.call_tool("edit_context", {"symbol": d.name, "path": d.path}))
        except MCPError:
            continue
        if "ambiguous" in ctx:
            continue
        level = _risk_level(ctx)
        if level == "high" and hub_example is None:
            hub_example = (d, ctx)
        elif level in ("low", "medium") and target is None:
            target = (d, ctx)
        if target and hub_example:
            break

    result: dict = {}
    if target is None:
        result["skipped"] = "no suitable low/medium-risk candidate found in sample"
        return result

    d, ctx = target
    abs_path = corpus_path / d.path
    lines = abs_path.read_text().splitlines()
    if d.line - 1 >= len(lines):
        result["skipped"] = "sampled def_line out of range after read (file shorter than expected)"
        return result
    original_line = lines[d.line - 1]
    marker = f" {comment} b12-probe"

    # -- old_text mode: round trip (allowing one CONFIRM_REQUIRED retry) +
    # reuse-after-change staleness probe --
    r1 = _call_edit_lines_allow_confirm(client, d.path, {
        "start_line": d.line, "end_line": d.line, "old_text": original_line, "new_text": original_line + marker,
    })
    applied = marker.strip() in abs_path.read_text()
    r2 = jload(client.call_tool("edit_lines", {
        "path": d.path,
        "edits": [{"start_line": d.line, "end_line": d.line, "old_text": original_line, "new_text": "SHOULD_NOT_APPLY_1"}],
    }))
    stale_rejected = "SHOULD_NOT_APPLY_1" not in abs_path.read_text()
    result["old_text_round_trip"] = {
        "applied": applied, "first_response_ok": "error" not in r1,
        "stale_reuse_rejected": stale_rejected, "second_response": r2,
    }
    git_reset(corpus_path, d.path)

    # -- hash mode: round trip (allowing one CONFIRM_REQUIRED retry) +
    # stale-hash-reuse probe --
    src = jload(client.call_tool("source", {"path": d.path, "line": d.line, "end_line": d.line}))
    etag = src.get("etag")
    r3 = _call_edit_lines_allow_confirm(client, d.path, {
        "start_line": d.line, "end_line": d.line, "expected_hash": etag, "new_text": original_line + marker,
    })
    applied_hash = marker.strip() in abs_path.read_text()
    r4 = jload(client.call_tool("edit_lines", {
        "path": d.path,
        "edits": [{"start_line": d.line, "end_line": d.line, "expected_hash": etag, "new_text": "SHOULD_NOT_APPLY_2"}],
    }))
    stale_hash_rejected = "SHOULD_NOT_APPLY_2" not in abs_path.read_text()
    result["hash_round_trip"] = {
        "applied": applied_hash, "first_response_ok": "error" not in r3,
        "stale_hash_reuse_rejected": stale_hash_rejected, "second_response": r4,
    }
    git_reset(corpus_path, d.path)

    # -- diff_impact on a real, comment-only (semantically inert) edit --
    _call_edit_lines_allow_confirm(client, d.path, {
        "start_line": d.line, "end_line": d.line, "old_text": original_line, "new_text": original_line + marker,
    })
    impact = jload(client.call_tool("diff_impact", {}))
    result["diff_impact_on_comment_only_edit"] = impact
    git_reset(corpus_path, d.path)

    # -- edit_context caller_count cross-checked vs an independent grep oracle --
    grep_sites = git_grep_call_sites(corpus_path, d.name, d.path, d.line, lang)
    result["edit_context_vs_grep"] = {
        "symbol": d.name,
        "edit_context_caller_count": len(ctx.get("callers", []) or []),
        "grep_call_sites": len(grep_sites),
    }

    # -- hub/high-risk confirm gate: a single trailing-comment append (safe on
    # every language, so a refusal can only be the confirm-gate itself, not a
    # syntax error) must be refused WITHOUT confirm=true --
    if hub_example:
        hd, _hctx = hub_example
        hd_abs = corpus_path / hd.path
        hd_lines = hd_abs.read_text().splitlines()
        if hd.line - 1 < len(hd_lines):
            hd_original = hd_lines[hd.line - 1]
            r5 = jload(client.call_tool("edit_lines", {
                "path": hd.path,
                "edits": [{
                    "start_line": hd.line, "end_line": hd.line,
                    "old_text": hd_original, "new_text": hd_original + f" {comment} SHOULD_BE_REFUSED",
                }],
            }))
            refused = "SHOULD_BE_REFUSED" not in hd_abs.read_text()
            result["hub_gate_refusal"] = {"symbol": hd.name, "refused": refused, "response": r5}
            if not refused:
                git_reset(corpus_path, hd.path)
        else:
            result["hub_gate_refusal"] = {"skipped": "hub def_line out of range after read"}
    else:
        result["hub_gate_refusal"] = {"skipped": "no high-risk candidate found in sample"}

    return result


def check_edit_symbol_insertion(client: MCPClient, corpus_path: Path, defs: list[Definition], lang: str, seed: int = RNG_SEED) -> dict:
    rng = random.Random(seed + 1)
    func_defs = _callable_unique_defs(defs)
    rng.shuffle(func_defs)
    comment = LINE_COMMENT.get(lang, "//")
    for d in func_defs[:15]:
        try:
            ctx = jload(client.call_tool("edit_context", {"symbol": d.name, "path": d.path}))
        except MCPError:
            continue
        if "ambiguous" in ctx or _risk_level(ctx) == "high":
            continue
        marker_line = f"{comment} b12-insertion-probe"
        resp = jload(client.call_tool("edit_symbol", {
            "symbol": d.name, "path": d.path, "position": "append_inside", "new_text": marker_line + "\n",
        }))
        if _is_confirm_required(resp):
            resp = jload(client.call_tool("edit_symbol", {
                "symbol": d.name, "path": d.path, "position": "append_inside", "new_text": marker_line + "\n",
                "confirm": True, "reason": "b12 benchmark probe -- inserting a single comment line, verifying edit_symbol insertion",
            }))
        inserted = marker_line in (corpus_path / d.path).read_text()
        git_reset(corpus_path, d.path)
        return {"symbol": d.name, "path": d.path, "inserted": inserted, "response_ok": "error" not in resp, "response": resp if "error" in resp else None}
    return {"skipped": "no suitable non-high-risk candidate found in sample"}


def check_robustness(client: MCPClient, corpus_path: Path) -> dict:
    rows: dict = {}
    try:
        data = jload(client.call_tool("source", {"path": "../../../../../../etc/passwd", "line": 1, "end_line": 1}))
        rows["path_traversal_source"] = {"blocked": ("error" in data) or not data.get("source"), "raw": data}
    except MCPError as e:
        rows["path_traversal_source"] = {"blocked": True, "raw": str(e)[:300]}

    traversal_target = Path("/tmp/b12-should-not-exist")
    try:
        client.call_tool("edit_lines", {
            "path": "../../../../../../tmp/b12-should-not-exist",
            "edits": [{"start_line": 1, "end_line": 1, "new_text": "pwn\n"}],
        })
        wrote_outside = traversal_target.exists()
        rows["path_traversal_edit_lines"] = {"blocked": not wrote_outside}
        if wrote_outside:
            traversal_target.unlink()
    except MCPError as e:
        rows["path_traversal_edit_lines"] = {"blocked": True, "raw": str(e)[:300]}

    for name, tool, args in [
        ("search_empty_query", "search", {"query": "", "kind": "symbol"}),
        ("search_huge_limit", "search", {"query": "a", "kind": "symbol", "limit": 10_000_000}),
        ("source_nonexistent_symbol", "source", {"symbol": "____definitely_not_a_real_symbol____"}),
        ("callers_nonexistent_symbol", "callers", {"symbol": "____definitely_not_a_real_symbol____"}),
    ]:
        try:
            client.call_tool(tool, args)
            rows[name] = {"crashed": False}
        except MCPError as e:
            rows[name] = {"crashed": True, "detail": str(e)[:300]}
    return rows


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------

def run_language(lang: str, keep_worktree: bool = False) -> dict:
    print(f"[{lang}] preparing fresh worktree ...", file=sys.stderr)
    corpus_path = prepare_worktree(lang)
    commit = pinned_commit(lang)
    row: dict = {"lang": lang, "pinned_commit": commit, "corpus_path": str(corpus_path)}
    t0 = time.monotonic()
    defs = extract_definitions(corpus_path, lang)
    row["ground_truth_definitions_found"] = len(defs)
    if not defs:
        row["fatal"] = "ground-truth extractor found 0 definitions -- cannot benchmark this language"
        return row

    client = None
    try:
        client = MCPClient(project_root=str(corpus_path), repo_root=str(repo_root_from_here()))
        client.wait_until_indexed(timeout=240.0)
        row["repo_overview"] = check_repo_overview(client, lang)
        row["search"] = check_search(client, corpus_path, defs)
        row["source"] = check_source(client, corpus_path, defs)
        row["file_overview"] = check_file_overview(client, defs)
        row["callers"] = check_callers(client, corpus_path, defs, lang)
        row["hotspots"] = check_hotspots(client, lang)
        row["edit_workflow"] = check_edit_workflow(client, corpus_path, defs, lang)
        row["edit_symbol_insertion"] = check_edit_symbol_insertion(client, corpus_path, defs, lang)
        row["robustness"] = check_robustness(client, corpus_path)
    except MCPError as e:
        row["fatal"] = f"MCPError: {e}"
    except Exception as e:  # noqa: BLE001 -- a crash here IS a finding, not a script bug to hide
        row["fatal"] = f"{type(e).__name__}: {e}\n{traceback.format_exc()[-2000:]}"
    finally:
        if client is not None:
            client.close()
        if not keep_worktree:
            cleanup_worktree(lang)
    row["wall_clock_sec"] = round(time.monotonic() - t0, 1)
    return row


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lang", default=None, help="Comma-separated subset, e.g. python,rust")
    parser.add_argument("--keep-worktrees", action="store_true", help="Don't delete .work/<lang> after the run (debugging)")
    args = parser.parse_args()

    langs = args.lang.split(",") if args.lang else list(CORPORA_LANGS)
    results = []
    for lang in langs:
        print(f"=== {lang} ===", file=sys.stderr)
        try:
            results.append(run_language(lang, keep_worktree=args.keep_worktrees))
        except Exception as e:  # noqa: BLE001
            results.append({"lang": lang, "fatal": f"setup failed before client start: {type(e).__name__}: {e}"})

    out_path = Path(__file__).resolve().parent / "results.json"
    out_path.write_text(json.dumps(results, indent=2, default=str))
    print(f"\nWrote {out_path}")

    for row in results:
        print(f"\n--- {row['lang']} ---")
        if "fatal" in row:
            print(f"  FATAL: {row['fatal'][:300]}")
            continue
        print(f"  ground truth defs: {row['ground_truth_definitions_found']}, wall clock: {row.get('wall_clock_sec')}s")
        print(f"  search recall: {row['search']['recall']}")
        print(f"  source byte-exact mismatches: {len(row['source']['byte_exact_mismatches'])}/{row['source']['checked']}")
        print(f"  callers zero-recall bugs: {sum(1 for c in row['callers'] if c['zero_recall_bug'])}/{len(row['callers'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
