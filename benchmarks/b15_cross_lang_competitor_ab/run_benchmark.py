#!/usr/bin/env python3
"""B15 -- CALM vs CodeGraph vs Ctxo vs Context+, cross-language real A/B.

Extends B13 (real CALM-vs-CodeGraph A/B, but only 2-3 corpora, 1 competitor)
to all 6 of CALM's Tier-0 languages (reusing B12's corpus registry verbatim:
fd/Rust, flask/Python, gin/Go, express/JS, zod/TypeScript, spring-petclinic/
Java) and 2 new real competitors, both live-verified in this repo's own
sandbox (not doc-summarized) before being wired in here: Ctxo
(github.com/alperhankendi/Ctxo, MIT, npm-installable, has its own PreToolUse
safety gate -- directly contests CALM's "only tool with a pre-edit gate"
claim) and Context+ (github.com/ForLoopCodes/contextplus, MIT, npx-
installable, 1971 GitHub stars -- has its own memory/RAG tools, directly
contests CALM's "only tool with cross-session memory" claim, and opens its
own README with an unqualified "99% accuracy" claim with zero methodology
-- exactly the marketing anti-pattern this whole benchmark suite exists to
not repeat).

Task measured: file-recall on "who calls this symbol" (CodeGraph:
`codegraph_callers`, Ctxo: `search_symbols` + `find_importers(edgeKinds=
["calls"])`, Context+: `get_blast_radius`, CALM: `callers`) against B12's
independent git-grep oracle (`ground_truth.py`, reused verbatim, INCLUDING
the 2 oracle bugs found+fixed 2026-08-18 while auditing CALM on this exact
corpus registry -- see that commit for what those bugs were and why they'd
have silently mismeasured every tool in this benchmark too, not just CALM).

Known, disclosed scope limits (read before citing any number from this
benchmark):
  - Ctxo has NO plugin for python or rust (verified live: `ctxo install
    python`/`ctxo install rust` both 404 on the real npm registry, not a
    guess from its README) -- it only runs on typescript/javascript/go/java
    here. Silently skipped for python/rust, not scored as 0 -- an absent
    row is not the same claim as a losing row.
  - Every tool here answers a DIFFERENT semantic question under a shared
    "file-recall" label: CodeGraph free-texts an impact summary, Ctxo's
    `find_importers` is edge-typed (asked to filter to "calls" specifically
    but the underlying graph may still include re-export/type-only uses
    depending on its own resolver), Context+'s `get_blast_radius` explicitly
    documents itself as "usages", not "calls". Read the raw per-symbol rows
    in results.json before treating any aggregate percentage as a clean
    apples-to-apples ranking -- same caveat B11's README already gives for
    its own raw token-ratio numbers.
  - Single pass (n_repeats configurable, defaults to 1) per symbol per
    corpus in this initial run -- B13's N=3-repeat discipline is available
    via --n-repeats but wasn't run at that N here for turn-budget reasons;
    re-run with --n-repeats 3 before treating any single row as final.
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

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
CTXO_PKG = "@ctxo/cli@0.11.4"
CONTEXTPLUS_PKG = "contextplus@1.0.8"

# Verified live 2026-08-18 (not from README claims): `ctxo install <lang>`
# 404s on the real npm registry for python/rust; go/java/typescript resolve
# to real @ctxo/lang-* packages. javascript files are picked up by the same
# @ctxo/lang-typescript plugin (confirmed live: installing just the
# typescript plugin made ctxo's own "Language scan" report BOTH
# "typescript ... (plugin available)" AND "javascript ... (plugin
# available)" for the same corpus).
CTXO_LANG_PLUGIN: dict[str, str] = {
    "typescript": "typescript", "javascript": "typescript", "go": "go", "java": "java",
}
CTXO_SUPPORTED_LANGS = set(CTXO_LANG_PLUGIN)

# CALM's 6 Tier-0 languages, all covered by B12's corpus registry.
ALL_LANGS = ("python", "rust", "go", "javascript", "typescript", "java")

N_SAMPLES = 8
MAX_OCCURRENCES = 25  # exclude overly-generic names, same threshold B12/B13 use
WORK_ROOT = Path(__file__).resolve().parent / ".work"
RESULTS_PATH = Path(__file__).resolve().parent / "results.json"


def sh(cmd: list[str], cwd: str | Path | None = None, timeout: int = 600) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)


# ---------------------------------------------------------------------------
# CodeGraph adapter -- reused verbatim from B13 (proven, unchanged since
# 2026-08-02; version pin re-verified still latest on npm 2026-08-18).
# ---------------------------------------------------------------------------

_PATH_RE = re.compile(r"(?:^|[\s`(\[])([A-Za-z0-9_./-]+\.[A-Za-z0-9]{1,8})(?::(\d+))?")


def extract_paths_from_text(text: str) -> set[str]:
    """CodeGraph's callers/impact responses are free-text (markdown-ish),
    not structured JSON -- pull anything that looks like a repo-relative
    file path out of it. Deliberately permissive (over-match a little)
    since undercounting would unfairly deflate CodeGraph's recall; false
    positives here would only ever help, never hurt, its score."""
    return {m.group(1).lstrip("./") for m in _PATH_RE.finditer(text)}


def codegraph_init(corpus_dir: Path) -> dict:
    t0 = time.time()
    proc = sh(["npx", "-y", CODEGRAPH_PKG, "init"], cwd=corpus_dir, timeout=600)
    return {
        "ok": proc.returncode == 0, "seconds": round(time.time() - t0, 2),
        "stderr_tail": proc.stderr[-500:] if proc.returncode != 0 else None,
    }


def start_codegraph(corpus_dir: Path) -> GenericMCPClient:
    return GenericMCPClient(
        cmd=["npx", "-y", CODEGRAPH_PKG, "serve", "--mcp"], cwd=str(corpus_dir), env=CODEGRAPH_ENV,
    )


def codegraph_callers(client: GenericMCPClient, symbol: str, def_path: str) -> set[str]:
    raw = client.call_tool("codegraph_callers", {"symbol": symbol, "file": def_path})
    return extract_paths_from_text(raw)


# ---------------------------------------------------------------------------
# Ctxo adapter -- new, schemas live-verified against a real spawned server
# in this repo's own sandbox before being wired in (see commit message /
# session notes for the discovery transcript, not guessed from the README).
# ---------------------------------------------------------------------------

def ctxo_setup(corpus_dir: Path, lang: str) -> dict:
    """`ctxo install <plugin>` then `ctxo index` -- unlike CodeGraph/
    Context+, Ctxo needs an explicit language-plugin install step before
    indexing picks up any files of that language at all (verified live:
    with no plugin installed, indexing a real 1-file TS project reported
    "Found 0 source files").

    Verified live on spring-petclinic (pure Maven, no package.json): `ctxo
    install <plugin>` refuses outright ("No package.json in the current
    project. Re-run with --global or create a package.json first") even
    for a NON-npm language like java -- its plugin-install mechanism always
    shells out through npm, unconditionally. `--global` failed in this
    sandbox on EACCES (no write access to the system node_modules), which
    is an environment permission issue, not something worth encoding as a
    Ctxo limitation -- so this writes a minimal throwaway `package.json`
    instead when the corpus doesn't already have one, exactly the
    workaround a real user hitting this on a permission-restricted machine
    would reach for. Removed again right after install/index so a real
    corpus that already ships its own package.json is never touched and no
    trace of the throwaway file leaks into scoring.

    2026-08-18 fix (round 1, superseded by round 2 below): the FULL
    6-language run's first pass silently mis-scored Ctxo near-zero on
    go/javascript/typescript (1/11, 0/8, 1/11) while java scored 23/24.
    First hypothesis -- `npm install -D @ctxo/lang-<x>` reporting exit 0
    without actually materializing `node_modules/@ctxo/lang-<x>` -- was
    only half right: a `plugin_dir.is_dir()` check can be True (npm has
    created the directory) while the package is still incomplete inside it
    (verified live: a same-directory follow-up `ctxo index` failed with
    "Cannot find module '@ctxo/lang-typescript/package.json'" -- the
    `package.json` file specifically hadn't landed yet, an npm/filesystem
    flush race, not a permanent Ctxo limitation -- re-running index moments
    later, by hand, succeeded cleanly every time this was checked).

    2026-08-18 fix (round 2): two real bugs closed in the same investigation
    that produced round 1's WRONG "fixed" verdict --
    (a) **`ctxo`'s CLI prints ALL of its real progress output to STDERR,
    not stdout** (verified live via a raw `subprocess.run` -- `stdout` was
    a genuinely empty string while `stderr` had the full "[ctxo] Building
    codebase index... Found N source files" transcript). The prior fix's
    own `indexed_zero_files` check read `index.stdout`, so it was checking
    an always-empty stream -- vacuously "not zero files" on every run,
    including ones that silently indexed 0 files. Now reads BOTH streams.
    (b) The `plugin_dir.is_dir()` check is upgraded to
    `(plugin_dir / "package.json").is_file()` -- an incompletely-flushed
    npm install can leave the directory present but the file that Node's
    own `require()` resolution actually needs still missing, exactly the
    failure mode round 1 didn't catch. Retries up to 3x with a short delay,
    not once -- an fs-flush race can outlast a single immediate retry."""
    plugin = CTXO_LANG_PLUGIN[lang]
    pkg_json = corpus_dir / "package.json"
    wrote_pkg_json = False
    if not pkg_json.exists():
        pkg_json.write_text('{"name": "b15-ctxo-throwaway", "version": "0.0.0", "private": true}\n')
        wrote_pkg_json = True
    t0 = time.time()
    plugin_marker = corpus_dir / "node_modules" / "@ctxo" / f"lang-{plugin}" / "package.json"
    install = None
    install_attempts = 0
    for install_attempts in range(1, 4):
        install = sh(["npx", "-y", CTXO_PKG, "install", plugin, "-y"], cwd=corpus_dir, timeout=180)
        if plugin_marker.is_file():
            break
        time.sleep(2.0)
    plugin_materialized = plugin_marker.is_file()
    index = sh(["npx", "-y", CTXO_PKG, "index"], cwd=corpus_dir, timeout=600)
    if wrote_pkg_json:
        pkg_json.unlink(missing_ok=True)
    index_combined = index.stdout + "\n" + index.stderr
    indexed_zero_files = "Found 0 source files" in index_combined
    indexed_ok_marker = "Index complete" in index_combined
    return {
        "ok": (install.returncode == 0 and index.returncode == 0 and plugin_materialized
               and not indexed_zero_files and indexed_ok_marker),
        "seconds": round(time.time() - t0, 2),
        "wrote_throwaway_package_json": wrote_pkg_json,
        "plugin_materialized": plugin_materialized,
        "install_attempts": install_attempts,
        "indexed_zero_files": indexed_zero_files,
        "indexed_ok_marker": indexed_ok_marker,
        "install_stderr_tail": install.stderr[-500:] if install.returncode != 0 else None,
        "index_output_tail": index_combined[-800:],
    }


def start_ctxo(corpus_dir: Path) -> GenericMCPClient:
    return GenericMCPClient(cmd=["npx", "-y", CTXO_PKG], cwd=str(corpus_dir))


def ctxo_callers(client: GenericMCPClient, symbol: str, def_path: str) -> set[str]:
    """Two real MCP calls, not one -- Ctxo's `find_importers` needs a
    `symbolId` (format `file::name::kind`), not a bare name, so the first
    call resolves the sampled def_path/symbol pair to its real symbolId via
    `search_symbols`, preferring a same-file match (a name can collide
    across files, same class of ambiguity B2's oracle fix on this repo's
    own history already had to account for)."""
    try:
        raw = client.call_tool("search_symbols", {"pattern": symbol, "limit": 25})
        data = json.loads(raw)
    except Exception:  # noqa: BLE001
        return set()
    candidates = data.get("results", []) or []
    norm_def_path = def_path.lstrip("./")
    match = next(
        (c for c in candidates if c.get("name") == symbol and c.get("file", "").lstrip("./") == norm_def_path),
        None,
    )
    if match is None:
        match = next((c for c in candidates if c.get("name") == symbol), None)
    if match is None or "symbolId" not in match:
        return set()
    try:
        raw2 = client.call_tool("find_importers", {"symbolId": match["symbolId"], "edgeKinds": ["calls"]})
        data2 = json.loads(raw2)
    except Exception:  # noqa: BLE001
        return set()
    return {imp.get("file", "").lstrip("./") for imp in (data2.get("importers") or []) if imp.get("file")}


# ---------------------------------------------------------------------------
# Context+ adapter -- new, schema live-verified the same way as Ctxo's.
# ---------------------------------------------------------------------------

def start_contextplus(corpus_dir: Path) -> GenericMCPClient:
    return GenericMCPClient(cmd=["npx", "-y", CONTEXTPLUS_PKG], cwd=str(corpus_dir))


# Matches a file-header line in get_blast_radius's free-text output, e.g.
# "  a.ts:" -- verified live against a real 2-call-site fixture; a line
# must consist of ONLY a path + trailing colon (whitespace aside) to match,
# so prose lines like the leading 'Blast radius for "x": N usages in M
# files' summary line (which has trailing text after its colon) don't.
_CTXPLUS_FILE_HEADER_RE = re.compile(r"^[ \t]*([\w./\\-]+\.[A-Za-z0-9]{1,8}):[ \t]*$", re.MULTILINE)


def contextplus_callers(client: GenericMCPClient, symbol: str, def_path: str) -> set[str]:
    try:
        raw = client.call_tool("get_blast_radius", {"symbol_name": symbol, "file_context": def_path})
    except Exception:  # noqa: BLE001
        return set()
    if not isinstance(raw, str):
        return set()
    return {m.group(1).lstrip("./") for m in _CTXPLUS_FILE_HEADER_RE.finditer(raw)}


# ---------------------------------------------------------------------------
# CALM adapter -- reused verbatim from B13 (extract_paths_from_calm_callers'
# docstring documents a real early harness bug this already caught once).
# ---------------------------------------------------------------------------

def start_calm(corpus_dir: Path, calm_bin: str) -> GenericMCPClient:
    return GenericMCPClient(cmd=[calm_bin, "serve", "--project-root", str(corpus_dir)], cwd=str(corpus_dir))


def wait_calm_indexed(client: GenericMCPClient, timeout: float = 120.0) -> float:
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


def force_scip_refresh(calm: GenericMCPClient, lang: str) -> dict:
    """B13/B12's SCIP-overlay readiness-race fix (2026-08-02, backported to
    B7/B11/B12 2026-08-18) -- without it, CALM's answer could be read before
    the async SCIP overlay pass finishes upgrading edges to `formal`,
    understating its real recall on a corpus that happens to index fast."""
    provider = "javascript" if lang == "typescript" else lang
    try:
        raw = calm.call_tool("scip_refresh", {"lang": provider})
        return json.loads(raw)
    except Exception as e:  # noqa: BLE001
        return {"error": str(e)}


def extract_paths_from_calm_callers(raw: str) -> set[str]:
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


def calm_callers(client: GenericMCPClient, symbol: str, def_path: str) -> set[str]:
    raw = client.call_tool("callers", {"symbol": symbol, "path": def_path})
    return extract_paths_from_calm_callers(raw)


# ---------------------------------------------------------------------------
# Arm abstraction -- one shared loop drives every tool, CALM included.
# ---------------------------------------------------------------------------

@dataclass
class Arm:
    name: str
    start: Callable[[Path], GenericMCPClient]
    callers: Callable[[GenericMCPClient, str, str], set[str]]
    supported_langs: set[str] | None = None  # None = all of ALL_LANGS
    setup: Callable[[Path, str], dict] | None = None  # per-(corpus,lang) setup, e.g. codegraph/ctxo init
    cleanup_globs: list[str] = field(default_factory=list)  # dirs to rm after this corpus's run

    def supports(self, lang: str) -> bool:
        return self.supported_langs is None or lang in self.supported_langs


ARMS: dict[str, Arm] = {
    "codegraph": Arm(
        name="codegraph", start=start_codegraph, callers=codegraph_callers,
        setup=lambda corpus, lang: codegraph_init(corpus), cleanup_globs=[".codegraph"],
    ),
    "ctxo": Arm(
        name="ctxo", start=start_ctxo, callers=ctxo_callers,
        supported_langs=CTXO_SUPPORTED_LANGS, setup=ctxo_setup, cleanup_globs=[".ctxo", "node_modules", "package-lock.json"],
    ),
    "contextplus": Arm(
        name="contextplus", start=start_contextplus, callers=contextplus_callers,
        cleanup_globs=[".mcp_data"],
    ),
}


def sample_symbols(corpus_dir: Path, lang: str, n: int) -> list[dict]:
    """Verbatim from B13 -- same distinctiveness/occurrence-count bounding,
    now benefiting from B2/B12's 2026-08-18 oracle fixes (symbol-collision
    exclusion doesn't apply here since this oracle is git-grep-based, not
    SCIP-based, but the string-literal false-positive fix does)."""
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
    pool.sort(key=lambda x: x["name"])
    step = max(1, len(pool) // n)
    return pool[::step][:n]


def run_corpus(lang: str, corpus_dir: Path, calm_bin: str, pinned_commit: str,
                arm_names: list[str], n_repeats: int = 1) -> dict:
    active_arms = [ARMS[a] for a in arm_names if ARMS[a].supports(lang)]
    skipped = [a for a in arm_names if not ARMS[a].supports(lang)]

    setup_results: dict[str, dict] = {}
    for arm in active_arms:
        if arm.setup is not None:
            print(f"=== {lang}/{arm.name}: setup ===", file=sys.stderr)
            setup_results[arm.name] = arm.setup(corpus_dir, lang)
            if not setup_results[arm.name].get("ok", True):
                print(f"=== {lang}/{arm.name}: setup FAILED, dropping this arm for this corpus ===", file=sys.stderr)

    active_arms = [a for a in active_arms if setup_results.get(a.name, {}).get("ok", True)]

    print(f"=== {lang}: starting {['calm'] + [a.name for a in active_arms]} MCP servers ===", file=sys.stderr)
    calm = start_calm(corpus_dir, calm_bin)
    clients: dict[str, GenericMCPClient] = {}
    for arm in active_arms:
        clients[arm.name] = arm.start(corpus_dir)

    try:
        calm_index_seconds = wait_calm_indexed(calm)
        scip_refresh_result = force_scip_refresh(calm, lang)
        print(f"=== {lang}: calm indexed in {calm_index_seconds}s, scip_refresh={scip_refresh_result}, "
              f"sampling symbols ===", file=sys.stderr)

        samples = sample_symbols(corpus_dir, lang, N_SAMPLES)
        if not samples:
            # Observed once, transiently, during this benchmark's own dry
            # run: a fresh clone + a real npm-backed setup step (ctxo)
            # immediately beforehand produced 0 samples on a corpus that
            # deterministically has 8+ real candidates when queried
            # moments later by hand -- never root-caused (not disk
            # pressure: 21GB free at the time), smells like a transient
            # subprocess/IO hiccup, not a real "this corpus has no
            # symbols" condition. One retry rather than silently reporting
            # a corpus as sample-less.
            print(f"=== {lang}: sample_symbols returned 0, retrying once ===", file=sys.stderr)
            time.sleep(2.0)
            samples = sample_symbols(corpus_dir, lang, N_SAMPLES)
        rows = []
        for s in samples:
            oracle = set(s["oracle_files"])
            row: dict = {"symbol": s["name"], "def_path": s["def_path"], "oracle_files": sorted(oracle)}

            calm_repeats = []
            for _ in range(max(1, n_repeats)):
                try:
                    calm_repeats.append(calm_callers(calm, s["name"], s["def_path"]))
                except Exception:  # noqa: BLE001
                    calm_repeats.append(set())
            calm_files = calm_repeats[0]
            row["calm_recall"] = f"{len(oracle & calm_files)}/{len(oracle)}"
            row["calm_files"] = sorted(calm_files)
            row["calm_repeats_agree"] = all(r == calm_files for r in calm_repeats)

            for arm in active_arms:
                repeats = []
                for _ in range(max(1, n_repeats)):
                    try:
                        repeats.append(arm.callers(clients[arm.name], s["name"], s["def_path"]))
                    except Exception:  # noqa: BLE001
                        repeats.append(set())
                files = repeats[0]
                row[f"{arm.name}_recall"] = f"{len(oracle & files)}/{len(oracle)}"
                row[f"{arm.name}_files"] = sorted(files)
                row[f"{arm.name}_repeats_agree"] = all(r == files for r in repeats)

            rows.append(row)

        return {
            "lang": lang, "pinned_commit": pinned_commit, "n_samples": len(samples), "rows": rows,
            "arms_run": [a.name for a in active_arms], "arms_skipped_unsupported": skipped,
            "setup_results": setup_results,
            "calm_index_seconds": calm_index_seconds, "calm_scip_refresh": scip_refresh_result,
        }
    finally:
        calm.close()
        for c in clients.values():
            c.close()
        shutil.rmtree(corpus_dir / ".calm", ignore_errors=True)
        for arm in active_arms:
            for g in arm.cleanup_globs:
                p = corpus_dir / g
                if p.is_dir():
                    shutil.rmtree(p, ignore_errors=True)
                elif p.exists():
                    p.unlink(missing_ok=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--calm-bin", default=str(REPO_ROOT / "target" / "release" / "calm"))
    ap.add_argument("--langs", default=",".join(ALL_LANGS), help="comma-separated subset of: " + ",".join(ALL_LANGS))
    ap.add_argument("--arms", default="codegraph,ctxo,contextplus", help="comma-separated subset of: " + ",".join(ARMS))
    ap.add_argument("--n-repeats", type=int, default=1)
    ap.add_argument("--keep-corpus", action="store_true", help="don't delete .work/<lang> after (debugging)")
    args = ap.parse_args()

    langs = [l for l in args.langs.split(",") if l]
    arm_names = [a for a in args.arms.split(",") if a]
    for a in arm_names:
        if a not in ARMS:
            sys.exit(f"unknown arm {a!r}, choose from {sorted(ARMS)}")

    calm_sha = sh(["git", "rev-parse", "HEAD"], cwd=REPO_ROOT).stdout.strip()
    calm_dirty = bool(sh(["git", "status", "--porcelain"], cwd=REPO_ROOT).stdout.strip())
    versions = {
        "codegraph": sh(["npx", "-y", CODEGRAPH_PKG, "--version"]).stdout.strip() if "codegraph" in arm_names else None,
        "ctxo": sh(["npx", "-y", CTXO_PKG, "--version"]).stdout.strip() if "ctxo" in arm_names else None,
        "contextplus_package": CONTEXTPLUS_PKG if "contextplus" in arm_names else None,
    }

    results: dict = {
        "meta": {
            "calm_git_sha": calm_sha, "calm_worktree_dirty_at_run": calm_dirty, "calm_bin": args.calm_bin,
            "codegraph_package": CODEGRAPH_PKG, "ctxo_package": CTXO_PKG, "contextplus_package": CONTEXTPLUS_PKG,
            "versions_reported": versions,
            "n_samples_per_corpus": N_SAMPLES, "n_repeats": args.n_repeats,
            "arms_requested": arm_names, "langs_requested": langs,
            "run_started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        },
        "corpora": {},
    }

    WORK_ROOT.mkdir(parents=True, exist_ok=True)
    for lang in langs:
        corpus_dir = WORK_ROOT / lang
        if corpus_dir.exists():
            shutil.rmtree(corpus_dir)
        pinned = b12_corpora.get_corpus(lang)
        sh(["git", "clone", "--quiet", str(pinned.source), str(corpus_dir)])
        commit = sh(["git", "rev-parse", "HEAD"], cwd=corpus_dir).stdout.strip()
        try:
            results["corpora"][lang] = run_corpus(
                lang, corpus_dir, args.calm_bin, commit, arm_names, n_repeats=args.n_repeats,
            )
        except Exception as e:  # noqa: BLE001 -- a crash here IS a finding, not a script bug to hide
            results["corpora"][lang] = {"lang": lang, "fatal": f"{type(e).__name__}: {e}"}
        finally:
            if not args.keep_corpus:
                shutil.rmtree(corpus_dir, ignore_errors=True)

        RESULTS_PATH.write_text(json.dumps(results, indent=2))  # incremental write, survives a later crash

    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
