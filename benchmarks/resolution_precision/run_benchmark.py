"""WS0 — Precision + false-confidence benchmark (adversarial fixture corpus).

Companion to `benchmarks/resolution/` (which measures tier DISTRIBUTION with no
oracle) and `benchmarks/b12_tier1_tier2_tool_correctness/` (which measures
file-RECALL on real OSS repos via `ground_truth.py`'s git-grep oracle, but
deliberately excludes non-unique/confusable names via `unique_definitions()`
-- see its own docstring). Neither can measure the metric this benchmark
exists for: **false_confidence_rate** -- a call_edges row labeled
`formal`/`resolved` (CALM's own two highest-confidence tiers) whose target is
actually wrong. B15's own README (2026-08-18) explicitly names this gap:
Context+'s `get_blast_radius` scores 100% file-recall via plain substring
matching because the benchmark never penalizes a confidently-wrong claim.

Real-repo ground truth can't test this either: `unique_definitions()`
deliberately samples only globally-unique names specifically BECAUSE
confusable/same-named cases make "which specific definition is this the
oracle for" ill-posed without a compiler proof for every language (most of
CALM's 15+ languages have no SCIP provider on most machines). So the only
tractable way to measure false-confidence on the exact adversarial shapes
this repo's own resolver code comments describe (same-name-across-30-files,
inherited methods, same-directory decoys, ...) is a small, hand-authored
fixture corpus where the correct target is known by construction. See
`fixtures/*/oracle.json` for each fixture's ground truth and its own detailed
`note` field -- read those before citing any aggregate number here, same
discipline B15's README established for its own "read the raw rows" caveat.

Each fixture is indexed as its OWN standalone `--project-root` (own
`.calm/index.db`), exactly like `benchmarks/resolution/run_benchmark.py`'s
`index_corpus` pattern -- never merged into this repo's own CALM index.
"""
from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

CONFIDENT_TIERS = {"formal", "resolved"}
INDEX_TIMEOUT_S = 90


def repo_root_from_here() -> Path:
    return Path(__file__).resolve().parents[2]


def fixtures_dir() -> Path:
    return Path(__file__).resolve().parent / "fixtures"


@dataclass
class FixtureResult:
    name: str
    category: str
    workstream: str
    gate: str
    outcome: str
    emitted: list[tuple[str, str]] = field(default_factory=list)
    expected: list[str] = field(default_factory=list)
    error: str | None = None
    # WS3: set when this call site produced zero `call_edges` but IS present
    # in `ambiguity_groups` -- distinguishes "genuinely silent, no trace
    # anywhere" (MISSING_SILENT/MISSING_RECALL) from "correctly recorded as
    # too-ambiguous-to-resolve" (the exact D3 defect WS3 fixes). See
    # `query_ambiguity_group` and `classify`'s MISSING_RECORDED_AMBIGUOUS.
    ambiguity_group: dict | None = None
    # WS7B: count of `evidence_conflicts` rows for this fixture -- provider
    # (SCIP) proofs skipped because they contradicted a confident static
    # resolution of the same call site (fixture I / D8). Countable, not silent.
    conflicts: int = 0


def write_disable_embeddings_config(fixture_path: Path) -> None:
    """Mirrors `benchmarks/resolution/run_benchmark.py`'s helper of the same
    name -- call-graph fixtures don't need semantic search, and skipping
    embedding generation meaningfully speeds up 12+ tiny per-fixture runs."""
    calm_dir = fixture_path / ".calm"
    calm_dir.mkdir(exist_ok=True)
    cfg_path = calm_dir / "config.json"
    cfg = {}
    if cfg_path.exists():
        try:
            cfg = json.loads(cfg_path.read_text())
        except json.JSONDecodeError:
            cfg = {}
    cfg.setdefault("semantic_search", {})["enabled"] = False
    cfg_path.write_text(json.dumps(cfg, indent=2))


def index_fixture(calm_bin: Path, fixture_path: Path) -> tuple[bool, str]:
    """Fresh `calm index` for one fixture. Returns (ok, stderr_tail).

    A hard subprocess timeout is load-bearing here, not cosmetic: a
    Rust-containing fixture with no `Cargo.toml` of its own lets cargo/
    rust-analyzer auto-detection walk UP the directory tree looking for an
    enclosing workspace -- and every fixture lives inside CALM's own repo,
    whose root Cargo.toml IS a real, large workspace. Verified live
    (2026-08-18): omitting a fixture-local `Cargo.toml` hung a batch run past
    a 2-minute wall clock. Every Rust fixture in this corpus ships its own
    minimal `[workspace]` stub specifically to stop that upward search at the
    fixture boundary -- see fixtures/*/Cargo.toml. The timeout below is the
    second layer of defense, not a substitute for that fix.
    """
    db_path = fixture_path / ".calm" / "index.db"
    if db_path.exists():
        db_path.unlink()
    write_disable_embeddings_config(fixture_path)
    try:
        proc = subprocess.run(
            [str(calm_bin), "index", "--project-root", str(fixture_path)],
            capture_output=True, text=True, timeout=INDEX_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        return False, f"TIMEOUT after {INDEX_TIMEOUT_S}s"
    if proc.returncode != 0:
        return False, proc.stderr[-2000:]
    return True, ""


def query_call_edges(db_path: Path, from_path: str, call_line: int) -> list[tuple[str, str]]:
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT to_symbol, edge_confidence FROM call_edges "
        "WHERE from_path = ? AND call_site_line = ?",
        (from_path, call_line),
    ).fetchall()
    conn.close()
    return [(r[0], r[1]) for r in rows]


def query_conflict_count(db_path: Path) -> int:
    """WS7B: number of recorded provider/static evidence conflicts in this
    fixture's index (see `evidence_conflicts` and
    `crates/calm-core/src/scip/ingest.rs::conflicting_confident_static_target`).
    Tolerates an older DB with no such table (returns 0)."""
    conn = sqlite3.connect(db_path)
    try:
        row = conn.execute("SELECT COUNT(*) FROM evidence_conflicts").fetchone()
        return int(row[0]) if row else 0
    except sqlite3.OperationalError:
        return 0
    finally:
        conn.close()


def query_ambiguity_group(db_path: Path, from_path: str, call_line: int) -> dict | None:
    """WS3: `ambiguity_groups` has no `call_line` column of its own (a
    call_site_id FK, not a coordinate) -- joins through `call_sites` to find
    the row for this exact fixture's one call site, the same way
    `query_call_edges` keys off (from_path, call_line) directly against
    `call_edges`. Absence here (vs. an empty `call_edges` result) is what
    distinguishes a genuinely silent D3 drop from the WS3 fix actually
    firing -- see `classify`'s MISSING_RECORDED_AMBIGUOUS.
    """
    conn = sqlite3.connect(db_path)
    row = conn.execute(
        "SELECT ag.candidate_group_key, ag.candidate_count, ag.reason "
        "FROM ambiguity_groups ag JOIN call_sites cs ON cs.id = ag.call_site_id "
        "WHERE cs.from_path = ? AND cs.call_line = ?",
        (from_path, call_line),
    ).fetchone()
    conn.close()
    if row is None:
        return None
    return {"candidate_group_key": row[0], "candidate_count": row[1], "reason": row[2]}


def classify(
    expected: list[str], emitted: list[tuple[str, str]], ambiguity_group: dict | None = None
) -> str:
    """Returns one of: MISSING_SILENT, MISSING_RECALL, MISSING_RECORDED_AMBIGUOUS,
    FALSE_CONFIDENCE, RECALL_LOWCONF_CORRECT, WRONG_LOWCONF.

    Two axes deliberately kept separate (V3 law: don't collapse evidence
    into one score) -- see this file's module docstring and the plan doc
    this benchmark implements (docs/plans/2026-08-18-context-intelligence-
    upgrade-plan.md WS0): FALSE_CONFIDENCE (a formal/resolved edge pointing
    at the wrong target) is the expensive failure; a low-confidence miss
    (WRONG_LOWCONF) or a low-confidence hit (RECALL_LOWCONF_CORRECT) are
    cheaper, different failure modes entirely. MISSING_RECORDED_AMBIGUOUS
    (WS3) is a THIRD kind of miss: still zero edges, still not a recall hit,
    but no longer indistinguishable from silence -- an agent calling
    `callers()` sees `unresolved_group_count` and a caveat instead of
    reading a clean empty list as proof of no usage.
    """
    expected_set = set(expected)
    all_targets = {t for t, _c in emitted}
    confident_targets = {t for t, c in emitted if c in CONFIDENT_TIERS}

    if not emitted:
        if not expected_set:
            return "MISSING_SILENT"
        return "MISSING_RECORDED_AMBIGUOUS" if ambiguity_group else "MISSING_RECALL"

    false_confidence = confident_targets - expected_set
    if false_confidence:
        return "FALSE_CONFIDENCE"
    if confident_targets & expected_set:
        return "RECALL_LOWCONF_CORRECT"  # confident AND correct, folded in below by caller
    if expected_set & all_targets:
        return "RECALL_LOWCONF_CORRECT"
    return "WRONG_LOWCONF"


def run_fixture(calm_bin: Path, fixture_path: Path) -> FixtureResult:
    name = fixture_path.name
    oracle_path = fixture_path / "oracle.json"
    oracle = json.loads(oracle_path.read_text())
    category = oracle.get("category", "?")
    workstream = oracle.get("workstream", "?")
    gate = oracle.get("gate", "?")
    expected = oracle.get("expected_targets", [])

    if gate == "blocked_missing_build_feature":
        return FixtureResult(name, category, workstream, gate, "BLOCKED", [], expected,
                              error="requires a non-default cargo feature; see oracle note")

    ok, err = index_fixture(calm_bin, fixture_path)
    if not ok:
        return FixtureResult(name, category, workstream, gate, "INDEX_ERROR", [], expected, error=err)

    cs = oracle["call_site"]
    db_path = fixture_path / ".calm" / "index.db"
    emitted = query_call_edges(db_path, cs["path"], cs["line"])
    ambiguity_group = query_ambiguity_group(db_path, cs["path"], cs["line"]) if not emitted else None
    outcome = classify(expected, emitted, ambiguity_group)
    return FixtureResult(name, category, workstream, gate, outcome, emitted, expected,
                          ambiguity_group=ambiguity_group,
                          conflicts=query_conflict_count(db_path))


MIN_SITES_FOR_COVERAGE_CURVE = 50


def print_report(results: list[FixtureResult]) -> dict:
    print(f"{'fixture':<40} {'outcome':<22} {'emitted (target @ confidence)'}")
    print("-" * 110)
    for r in results:
        emitted_str = "; ".join(f"{t}@{c}" for t, c in r.emitted) or "(none)"
        if r.ambiguity_group:
            ag = r.ambiguity_group
            emitted_str = f"(none, but ambiguity_groups: {ag['candidate_group_key']} x{ag['candidate_count']})"
        if r.error:
            emitted_str = f"[{r.error}]"
        print(f"{r.name:<40} {r.outcome:<22} {emitted_str}")

    gating = [r for r in results if r.gate not in
              ("informational", "informational_must_stay_honest", "blocked_missing_build_feature")]
    recall_hit_n = sum(1 for r in gating if r.outcome in
                        ("RECALL_LOWCONF_CORRECT", "MISSING_SILENT")
                        or (r.outcome == "FALSE_CONFIDENCE" and set(r.expected) & {t for t, _c in r.emitted}))
    total_gating = len(gating)

    # WS0 post-WS2-review revision (docs/plans/2026-08-18-context-intelligence-
    # upgrade-plan.md §3 WS0, §6 acceptance criteria): edge-level and
    # site-level false-confidence are DISTINCT numbers that can diverge --
    # "1 site -> 4 wrong edges" vs "4 sites -> 1 wrong edge each" collapse to
    # the same edge-level rate but very different blast radius. Both
    # computed here directly from each fixture's raw `emitted` list (one
    # fixture == one call site in this corpus), independent of `classify`'s
    # per-site outcome label so a future multi-site-per-fixture corpus
    # doesn't need `classify` itself to change.
    total_confident_edges = 0
    total_wrong_confident_edges = 0
    sites_with_false_confidence = 0
    # unique_resolution_coverage scaffold (see below): per-site "resolved to
    # exactly one confident-tier target" + "that target was correct" pair,
    # collected now so a future larger corpus can bin by precision without
    # re-instrumenting this loop.
    unique_confident_sites = 0
    unique_confident_correct_sites = 0
    for r in gating:
        expected_set = set(r.expected)
        confident_edges = [(t, c) for t, c in r.emitted if c in CONFIDENT_TIERS]
        wrong_confident_edges = [(t, c) for t, c in confident_edges if t not in expected_set]
        total_confident_edges += len(confident_edges)
        total_wrong_confident_edges += len(wrong_confident_edges)
        if wrong_confident_edges:
            sites_with_false_confidence += 1
        confident_targets = {t for t, _c in confident_edges}
        if len(confident_targets) == 1:
            unique_confident_sites += 1
            if confident_targets <= expected_set:
                unique_confident_correct_sites += 1

    if unique_confident_sites >= MIN_SITES_FOR_COVERAGE_CURVE:
        unique_resolution_coverage = unique_confident_correct_sites / unique_confident_sites
    else:
        unique_resolution_coverage = (
            f"insufficient_sample_size ({unique_confident_sites} uniquely-resolved gating "
            f"sites, need >={MIN_SITES_FOR_COVERAGE_CURVE} to bin a precision curve -- see "
            f"WS0's own plan section for why hand-built fixtures can't supply this yet)"
        )

    summary = {
        "total_fixtures": len(results),
        "gating_fixtures": total_gating,
        # Edge-level (headline): of every confident-tier (formal/resolved)
        # edge emitted across the whole gating corpus, what fraction point
        # at the wrong target.
        "false_confidence_count": total_wrong_confident_edges,
        "confident_edge_count": total_confident_edges,
        "false_confidence_rate": (
            (total_wrong_confident_edges / total_confident_edges) if total_confident_edges else None
        ),
        # Site-level (WS0 post-WS2 review addition): of every gating call
        # site, what fraction carry AT LEAST ONE confidently-wrong edge --
        # denominator is total_gating (every site), not just sites that
        # happened to emit a confident edge, so a resolver change that
        # trades "1 wrong edge on 1 site" for "1 wrong edge each on 4
        # sites" is visible here even when the edge-level rate alone
        # wouldn't move as much.
        "false_confident_site_count": sites_with_false_confidence,
        "false_confident_site_rate": (
            (sites_with_false_confidence / total_gating) if total_gating else None
        ),
        # Design-scaffolded only (see MIN_SITES_FOR_COVERAGE_CURVE) -- real
        # ingredients collected every run, reported as insufficient sample
        # size until the corpus is large enough to bin meaningfully.
        "unique_resolution_coverage": unique_resolution_coverage,
        "call_recall": (recall_hit_n / total_gating) if total_gating else None,
        # WS3: sites with zero call_edges that ARE recorded in
        # ambiguity_groups -- still a recall miss, but no longer
        # indistinguishable from a genuinely silent D3 drop (MISSING_RECALL).
        "missing_recorded_ambiguous_count": sum(
            1 for r in gating if r.outcome == "MISSING_RECORDED_AMBIGUOUS"
        ),
        "missing_silent_count": sum(1 for r in gating if r.outcome == "MISSING_RECALL"),
        # WS7B: provider (SCIP) proofs skipped because they contradicted a
        # confident static resolution of the same call site -- recorded in
        # `evidence_conflicts`, never emitted as a competing formal edge.
        "provider_conflict_count": sum(r.conflicts for r in results),
        "provider_conflict_rate": (
            (sum(r.conflicts for r in gating) / total_gating) if total_gating else None
        ),
        "blocked": [r.name for r in results if r.outcome == "BLOCKED"],
        "index_errors": [r.name for r in results if r.outcome == "INDEX_ERROR"],
    }
    print("-" * 110)
    print(json.dumps(summary, indent=2))
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--calm-bin", type=Path, default=repo_root_from_here() / "target" / "release" / "calm")
    parser.add_argument("--fixture", type=str, default=None, help="Run only this one fixture dir name")
    args = parser.parse_args()

    calm_bin = args.calm_bin.resolve()
    if not calm_bin.exists():
        sys.exit(f"{calm_bin} not found -- build with `cargo build --release -p calm-cli` first")

    fdir = fixtures_dir()
    names = sorted(p.name for p in fdir.iterdir() if p.is_dir() and (p / "oracle.json").exists())
    if args.fixture:
        names = [n for n in names if n == args.fixture]

    results = [run_fixture(calm_bin, fdir / n) for n in names]
    print_report(results)


if __name__ == "__main__":
    main()
