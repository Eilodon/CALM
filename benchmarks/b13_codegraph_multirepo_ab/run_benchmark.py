#!/usr/bin/env python3
"""B13 -- CALM vs CodeGraph, multi-repo real A/B (Phase 1: fd + CALM self-repo).

Extends B11 (real CALM-vs-CodeGraph A/B, but self-repo/Rust-only, CodeGraph
pinned at a now-stale v1.2.0) and B12 (6 pinned external OSS repos with an
independent regex/git-grep oracle, but CALM-only -- no competitor was ever run
against that corpus). This benchmark reuses B12's corpus registry + oracle
verbatim and adds a real CodeGraph arm to it, plus one new task neither B10/
B11/B12 measured: freshness under a live, external (non-tool-mediated) edit.

Design doc: docs/plans/2026-08-02-calm-vs-codegraph-fair-benchmark-research.md

Phase 1 scope (this run): fd (external, Rust, already pinned by B12) + CALM's
own self-repo (isolated git worktree -- dogfooding, per the user's explicit
ask). flask/Python and fmt/C++ are Phase 2 (see README) -- descoped this pass
for two disclosed reasons, not silently: (1) this machine hit 97%/7.9GB-free
disk pressure mid-run (checked live via `df`), each extra corpus needs a
throwaway clone + a `.codegraph/` index that isn't small (215MB observed for
CALM's own repo); (2) turn/time budget. Both corpora used here are cleaned up
(worktree removed / `.codegraph` deleted) immediately after their pass.

Version pinning (the single most-violated discipline in this suite's own
benchmark history -- see design doc Sec 3, item 11):
  - CALM: whatever binary CALM_BIN points at; this run recorded its exact
    `git rev-parse HEAD` INDEPENDENTLY of `calm --version` (which only ever
    prints the Cargo.toml package version, e.g. "0.4.0", identical whether
    you're on the tagged release or 5 unreleased commits past it -- exactly
    the ambiguity that bit this suite before).
  - CodeGraph: pinned EXPLICITLY as `@colbymchenry/codegraph@1.5.0` in every
    spawn command, not a bare `@colbymchenry/codegraph`. Verified live during
    this benchmark's own setup that the bare form can resolve to a STALE
    cached version (`npx -y @colbymchenry/codegraph --version` returned
    1.4.1 from local npx cache; `npm view ... version` said the true latest
    was 1.5.0) -- a new, previously-undocumented version-drift trap, added to
    the design doc's pitfall catalogue.

Oracle: reuses B12's `ground_truth.py` verbatim (regex definition extraction
+ word-bounded `git grep` call-site extraction, independent of either tool's
own parser) -- NOT a new oracle invented for this benchmark, per the "don't
build a new corpus/oracle from scratch" recommendation in the design doc.
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
LIB_DIR = REPO_ROOT / "benchmarks" / "lib"
B12_DIR = REPO_ROOT / "benchmarks" / "b12_tier1_tier2_tool_correctness"
sys.path.insert(0, str(LIB_DIR))
sys.path.insert(0, str(B12_DIR))

from generic_mcp_client import GenericMCPClient  # noqa: E402
import corpora as b12_corpora  # noqa: E402
from ground_truth import (  # noqa: E402
    extract_definitions,
    git_grep_call_sites,
    unique_definitions,
    total_occurrences,
)

CODEGRAPH_PKG = "@colbymchenry/codegraph@1.5.0"
CODEGRAPH_ENV = {
    "CODEGRAPH_MCP_TOOLS": "explore,node,search,callers,callees,impact,files,status",
}
N_SAMPLES = 8
MAX_OCCURRENCES = 25  # exclude overly-generic names, same threshold family as B12's dry-run fix
WORK_ROOT = Path(__file__).resolve().parent / ".work"
RESULTS_PATH = Path(__file__).resolve().parent / "results.json"


def sh(cmd: list[str], cwd: str | Path | None = None, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)


def codegraph_init(corpus_dir: Path) -> dict:
    t0 = time.time()
    proc = sh(["npx", "-y", CODEGRAPH_PKG, "init"], cwd=corpus_dir, timeout=600)
    return {
        "ok": proc.returncode == 0,
        "seconds": round(time.time() - t0, 2),
        "stderr_tail": proc.stderr[-500:] if proc.returncode != 0 else None,
    }


def start_codegraph(corpus_dir: Path) -> GenericMCPClient:
    return GenericMCPClient(
        cmd=["npx", "-y", CODEGRAPH_PKG, "serve", "--mcp"], cwd=str(corpus_dir), env=CODEGRAPH_ENV,
    )


def start_calm(corpus_dir: Path, calm_bin: str) -> GenericMCPClient:
    return GenericMCPClient(cmd=[calm_bin, "serve", "--project-root", str(corpus_dir)], cwd=str(corpus_dir))


def wait_calm_indexed(client: GenericMCPClient, timeout: float = 120.0) -> float:
    """Bug this harness itself had (found while debugging the flask corpus,
    see README/design-doc): this used to loop until `indexing_phase=="ready"`
    with no check for `"failed"` -- a real, permanent indexing failure looked
    identical to a slow-but-working index until the timeout fired, wasting
    the full timeout window and reporting a misleading TimeoutError instead
    of the real `indexing_error`. Fail fast on `failed` instead."""
    t0 = time.time()
    deadline = t0 + timeout
    while time.time() < deadline:
        raw = client.call_tool("indexing_status", {})
        try:
            status = json.loads(raw)
        except json.JSONDecodeError:
            status = {}
        phase = status.get("indexing_phase")
        if phase == "ready":
            return round(time.time() - t0, 2)
        if phase == "failed":
            raise RuntimeError(f"calm indexing failed: {status.get('indexing_error')}")
        time.sleep(1.0)
    raise TimeoutError(f"calm never reached indexing_phase=ready within {timeout}s")


_PATH_RE = re.compile(r"(?:^|[\s`(\[])([A-Za-z0-9_./-]+\.[A-Za-z0-9]{1,8})(?::(\d+))?")


def extract_paths_from_text(text: str) -> set[str]:
    """CodeGraph's callers/impact responses are free-text (markdown-ish),
    not structured JSON -- unlike CALM's. Pull anything that looks like a
    repo-relative file path out of it. Deliberately permissive (over-match
    a little) since undercounting would unfairly deflate CodeGraph's recall;
    false positives here would only ever help, never hurt, its score."""
    return {m.group(1).lstrip("./") for m in _PATH_RE.finditer(text)}


def extract_paths_from_calm_callers(raw: str) -> set[str]:
    """CALM's real `callers` response (verified live, not assumed -- an
    earlier draft of this harness assumed a separate `path` key per entry
    and silently scored CALM 0/N on every sample until this was caught by
    manually inspecting the raw JSON): each caller's location lives inside
    `symbol`, a qualified name shaped `repo/relative/path.ext::Type::method`
    -- there is no separate `path` field. Split on the `::` qualifier
    separator and take the file-path prefix."""
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return extract_paths_from_text(raw)
    paths: set[str] = set()
    for key in ("direct", "ambiguous", "transitive"):
        for entry in data.get(key, []) or []:
            qn = entry.get("symbol", "")
            p = qn.split("::", 1)[0] if "::" in qn else qn
            if p:
                paths.add(p.lstrip("./"))
    return paths


def sample_symbols(corpus_dir: Path, lang: str, n: int) -> list[dict]:
    defs = unique_definitions(extract_definitions(corpus_dir, lang))
    defs = [d for d in defs if d.kind in ("function", "method")]
    pool = []
    for d in defs:
        occ = total_occurrences(corpus_dir, d.name)
        if occ == 0 or occ > MAX_OCCURRENCES:
            continue
        sites = git_grep_call_sites(corpus_dir, d.name, d.path, d.line, lang)
        if not sites:
            continue
        pool.append({
            "name": d.name, "def_path": d.path, "def_line": d.line,
            "oracle_files": sorted({p for p, _ in sites}),
        })
    pool.sort(key=lambda x: x["name"])  # deterministic before sampling
    step = max(1, len(pool) // n)
    return pool[::step][:n]


def run_callers_recall(corpus_dir: Path, lang: str, calm: GenericMCPClient, cg: GenericMCPClient,
                        n_repeats: int = 1) -> dict:
    """`n_repeats` > 1 repeats each tool's SAME query back-to-back on the SAME
    live server instance (no re-clone/re-index between repeats) -- this is
    B11's exact repeat rationale (catch transient MCP/process hiccups), not
    resampling different symbols. Recall is scored from the FIRST repeat;
    any repeat whose file-set disagrees with the first is recorded, not
    averaged away, so a flaky/nondeterministic answer is visible in the
    output instead of silently smoothed into a median."""
    samples = sample_symbols(corpus_dir, lang, N_SAMPLES)
    rows = []
    for s in samples:
        oracle = set(s["oracle_files"])
        calm_repeats, cg_repeats = [], []
        for _ in range(max(1, n_repeats)):
            try:
                calm_raw = calm.call_tool("callers", {"symbol": s["name"], "path": s["def_path"]})
                calm_repeats.append(extract_paths_from_calm_callers(calm_raw))
            except Exception as e:  # noqa: BLE001
                calm_repeats.append(set())
            try:
                cg_raw = cg.call_tool("codegraph_callers", {"symbol": s["name"], "file": s["def_path"]})
                cg_repeats.append(extract_paths_from_text(cg_raw))
            except Exception as e:  # noqa: BLE001
                cg_repeats.append(set())
        calm_files, cg_files = calm_repeats[0], cg_repeats[0]
        calm_hit = len(oracle & calm_files)
        cg_hit = len(oracle & cg_files)
        rows.append({
            "symbol": s["name"], "def_path": s["def_path"], "oracle_files": sorted(oracle),
            "calm_recall": f"{calm_hit}/{len(oracle)}", "calm_files": sorted(calm_files),
            "codegraph_recall": f"{cg_hit}/{len(oracle)}", "codegraph_files": sorted(cg_files),
            "calm_repeats_agree": all(r == calm_files for r in calm_repeats),
            "codegraph_repeats_agree": all(r == cg_files for r in cg_repeats),
            "n_repeats": n_repeats,
        })
    return {"lang": lang, "n_samples": len(samples), "rows": rows}


def run_freshness_probe(corpus_dir: Path, lang: str, calm: GenericMCPClient, cg: GenericMCPClient,
                         target_symbol: str, target_def_path: str, new_caller_file: str) -> dict:
    """Append a brand-new call site to `target_symbol` via a PLAIN file write
    (bypassing both tools' own edit mechanisms -- neither gets a "hey, I just
    edited this" signal beyond whatever file-watcher/auto-sync it runs on its
    own), then query both immediately (t=0) and after a fixed grace window,
    to characterize real auto-sync latency instead of asserting a single
    pass/fail cutoff. CodeGraph markets "auto syncs on code changes"; CALM's
    incremental watcher is documented (docs/architecture.md) as hash-diff
    triggered, not instant on every fsync."""
    new_file = corpus_dir / new_caller_file
    marker = f"fn __b13_freshness_probe_caller() {{ {target_symbol}(); }}\n" if lang == "rust" else \
        f"def __b13_freshness_probe_caller():\n    {target_symbol}()\n"
    with open(new_file, "a") as f:
        f.write("\n" + marker)

    def check(client: GenericMCPClient, tool: str, args: dict, extractor) -> bool:
        try:
            raw = client.call_tool(tool, args)
        except Exception:  # noqa: BLE001
            return False
        return any(new_caller_file.lstrip("./") in p or p in new_caller_file for p in extractor(raw))

    t0_calm = check(calm, "callers", {"symbol": target_symbol, "path": target_def_path}, extract_paths_from_calm_callers)
    t0_cg = check(cg, "codegraph_callers", {"symbol": target_symbol, "file": target_def_path}, extract_paths_from_text)
    time.sleep(3.0)
    t3_calm = check(calm, "callers", {"symbol": target_symbol, "path": target_def_path}, extract_paths_from_calm_callers)
    t3_cg = check(cg, "codegraph_callers", {"symbol": target_symbol, "file": target_def_path}, extract_paths_from_text)

    new_file.unlink(missing_ok=True)
    return {
        "target_symbol": target_symbol, "new_caller_file": new_caller_file,
        "calm_sees_it_t0s": t0_calm, "calm_sees_it_t3s": t3_calm,
        "codegraph_sees_it_t0s": t0_cg, "codegraph_sees_it_t3s": t3_cg,
    }


def run_corpus(lang: str, corpus_dir: Path, calm_bin: str, pinned_commit: str, do_freshness: bool,
               n_repeats: int = 1) -> dict:
    print(f"=== {lang}: codegraph init ===", file=sys.stderr)
    init_result = codegraph_init(corpus_dir)
    if not init_result["ok"]:
        return {"lang": lang, "pinned_commit": pinned_commit, "error": "codegraph init failed",
                "detail": init_result}

    print(f"=== {lang}: starting calm + codegraph MCP servers ===", file=sys.stderr)
    calm = start_calm(corpus_dir, calm_bin)
    cg = start_codegraph(corpus_dir)
    try:
        calm_index_seconds = wait_calm_indexed(calm)
        print(f"=== {lang}: calm indexed in {calm_index_seconds}s, running callers_recall (x{n_repeats}) ===",
              file=sys.stderr)
        result = run_callers_recall(corpus_dir, lang, calm, cg, n_repeats=n_repeats)
        result["pinned_commit"] = pinned_commit
        result["calm_index_seconds"] = calm_index_seconds
        result["codegraph_init_seconds"] = init_result["seconds"]

        if do_freshness and result["rows"]:
            probe_row = result["rows"][0]
            new_file = f"__b13_freshness_probe.{'rs' if lang == 'rust' else 'py'}"
            print(f"=== {lang}: freshness probe on {probe_row['symbol']} ===", file=sys.stderr)
            result["freshness_probe"] = run_freshness_probe(
                corpus_dir, lang, calm, cg, probe_row["symbol"], probe_row["def_path"], new_file,
            )
        return result
    finally:
        calm.close()
        cg.close()
        shutil.rmtree(corpus_dir / ".codegraph", ignore_errors=True)
        shutil.rmtree(corpus_dir / ".calm", ignore_errors=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--calm-bin", default=str(REPO_ROOT / "target" / "debug" / "calm"))
    ap.add_argument("--corpora", default="rust,self",
                     help="comma-separated: rust,self,python (Phase 2 adds python/flask)")
    ap.add_argument("--n-repeats", type=int, default=1,
                     help="repeat each callers query N times per symbol (B11-style: catches "
                          "transient MCP hiccups, not resampling different symbols)")
    args = ap.parse_args()

    calm_sha = sh(["git", "rev-parse", "HEAD"], cwd=REPO_ROOT).stdout.strip()
    calm_dirty = bool(sh(["git", "status", "--porcelain"], cwd=REPO_ROOT).stdout.strip())
    codegraph_ver = sh(["npx", "-y", CODEGRAPH_PKG, "--version"]).stdout.strip()

    results: dict = {
        "meta": {
            "calm_git_sha": calm_sha,
            "calm_worktree_dirty_at_run": calm_dirty,
            "calm_bin": args.calm_bin,
            "codegraph_package": CODEGRAPH_PKG,
            "codegraph_version_reported": codegraph_ver,
            "n_samples_per_corpus": N_SAMPLES,
            "n_repeats": args.n_repeats,
            "run_started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        },
        "corpora": {},
    }

    requested = args.corpora.split(",")

    # External, B12-pinned corpora (rust=fd, python=flask) -- same throwaway-clone
    # pattern for both, generalized instead of a fd-only special case.
    for lang, result_key in (("rust", "fd_rust_external"), ("python", "flask_python_external")):
        if lang not in requested:
            continue
        WORK_ROOT.mkdir(parents=True, exist_ok=True)
        ext_dir = WORK_ROOT / lang
        if ext_dir.exists():
            shutil.rmtree(ext_dir)
        pinned = b12_corpora.get_corpus(lang)
        sh(["git", "clone", "--quiet", str(pinned.source), str(ext_dir)])
        commit = sh(["git", "-C", str(ext_dir), "rev-parse", "HEAD"]).stdout.strip()
        try:
            results["corpora"][result_key] = run_corpus(
                lang, ext_dir, args.calm_bin, commit, do_freshness=True, n_repeats=args.n_repeats,
            )
        finally:
            shutil.rmtree(ext_dir, ignore_errors=True)

    if "self" in requested:
        WORK_ROOT.mkdir(parents=True, exist_ok=True)
        self_dir = WORK_ROOT / "calm-self"
        if self_dir.exists():
            sh(["git", "worktree", "remove", "--force", str(self_dir)], cwd=REPO_ROOT)
            shutil.rmtree(self_dir, ignore_errors=True)
        sh(["git", "worktree", "add", "--detach", str(self_dir), "HEAD"], cwd=REPO_ROOT)
        try:
            results["corpora"]["calm_self_repo"] = run_corpus(
                "rust", self_dir, args.calm_bin, calm_sha, do_freshness=True, n_repeats=args.n_repeats,
            )
        finally:
            sh(["git", "worktree", "remove", "--force", str(self_dir)], cwd=REPO_ROOT)
            shutil.rmtree(self_dir, ignore_errors=True)

    RESULTS_PATH.write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
