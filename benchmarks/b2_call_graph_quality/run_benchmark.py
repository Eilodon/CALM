#!/usr/bin/env python3
"""B2 — Call Graph Resolution Quality benchmark runner.

Usage:
    benchmarks/.venv/bin/python benchmarks/b2_call_graph_quality/run_benchmark.py [--repo PATH]

Scope (this implementation): **Rust only**. Uses `rust-analyzer scip` as the
ground-truth oracle for the Rust call graph and measures how well `calm`'s
Tier-0/Tier-2 syntactic resolver (Phase A of the Rust support plan) agrees
with it, broken down by `edge_confidence`.

Requires:
  - `rust-analyzer` on PATH (or resolvable via rustup/VS Code — same
    detection `calm_core::scip::runner::resolve_binary` uses).
  - `calm` built with the `scip-overlay` feature, for the hidden `scip-dump`
    subcommand that decodes the oracle `.scip` file to JSON (reuses
    `calm_core::scip::parse` instead of re-implementing SCIP protobuf decoding
    in Python):
        cargo build --release -p calm-cli --features scip-overlay

Methodology:
  1. Run `rust-analyzer scip <repo> --output oracle.scip`.
  2. Decode it via `ci scip-dump oracle.scip` -> flat occurrences.
  3. Build the oracle edge set: for every non-local reference occurrence,
     resolve its symbol to its (non-local) definition occurrence, giving
     (ref_file, ref_line) -> (def_file, def_line). This mirrors
     `calm_core::scip::ingest::ingest_occurrences`'s own matching exactly, so
     the oracle here is built the same way Phase B's real ingest would use
     it — this benchmark and Phase B are measuring the same underlying
     correspondence.
  4. Run `calm index --project-root <repo>` (default features -- i.e. Phase A
     only, no SCIP overlay applied) and read `call_edges` for Rust files.
  5. precision = |ci ∩ oracle| / |ci|, recall = |ci ∩ oracle| / |oracle|,
     precision also broken down per `edge_confidence` bucket.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sqlite3
import sys
import tempfile
from collections import defaultdict
from pathlib import Path


def repo_root_from_here() -> Path:
    # benchmarks/b2_call_graph_quality/run_benchmark.py -> repo root is 2 levels up
    return Path(__file__).resolve().parents[2]


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def find_rust_analyzer() -> str:
    for candidate in ("rust-analyzer",):
        try:
            run([candidate, "--version"])
            return candidate
        except (OSError, subprocess.CalledProcessError):
            continue
    try:
        out = run(["rustup", "which", "--toolchain", "stable", "rust-analyzer"])
        path = out.stdout.strip()
        if path:
            return path
    except (OSError, subprocess.CalledProcessError):
        pass
    sys.exit(
        "rust-analyzer not found on PATH or via rustup. "
        "Install it (e.g. `rustup component add rust-analyzer`) to run this benchmark."
    )


def build_oracle(occurrences: list[dict]) -> set[tuple[str, int, str, int]]:
    """(ref_file, ref_line) -> (def_file, def_line) edges, mirroring
    calm_core::scip::ingest::ingest_occurrences's own matching.

    2026-08-18 fix: rust-analyzer's scip symbol-naming scheme does not
    disambiguate private/local functions by which independently-compiled
    `tests/*.rs` integration-test binary they belong to -- two files each
    defining their own local `fn run_calm()` (a common, encouraged pattern:
    every integration test binary is self-contained) can get the IDENTICAL
    scip symbol string. The old plain `dict` keyed by that string silently
    let whichever occurrence got processed last "win" as ground truth for
    EVERY file's call sites of that name -- wrongly failing a tool that
    correctly resolved same-file. Verified live on
    `crates/calm-cli/tests/{permissions,hooks}_doctor_fix.rs`'s `run_calm`/
    `calm_bin`/`fresh_project` and `crates/calm-core/tests/
    {golden_graph_equivalence,derived_artifact_versions}.rs`'s
    `index_fresh` -- all 4 collide 2-way. A symbol with >1 distinct def
    location is unusable as ground truth (nothing in the scip dump alone
    says which one a given reference actually targets), so it's dropped
    from the oracle entirely instead of arbitrarily picking one and
    silently mismeasuring every tool that "gets it wrong" against that
    arbitrary pick."""
    def_locations: dict[str, set[tuple[str, int]]] = defaultdict(set)
    for o in occurrences:
        if o["is_def"] and not o["is_local"]:
            def_locations[o["symbol"]].add((o["file"], o["line"]))

    collided = {sym for sym, locs in def_locations.items() if len(locs) > 1}
    def_of = {sym: next(iter(locs)) for sym, locs in def_locations.items() if len(locs) == 1}
    if collided:
        preview = ", ".join(sorted(collided)[:5])
        print(
            f"build_oracle: excluded {len(collided)} symbol(s) with colliding scip def "
            f"locations (ambiguous ground truth, not scored either way): {preview}"
            f"{', ...' if len(collided) > 5 else ''}"
        )

    oracle: set[tuple[str, int, str, int]] = set()
    for o in occurrences:
        if o["is_def"] or o["is_local"] or o["symbol"] in collided:
            continue
        target = def_of.get(o["symbol"])
        if target is None:
            continue
        oracle.add((o["file"], o["line"], target[0], target[1]))
    return oracle


def load_calm_edges(db_path: Path) -> list[tuple[str, int, str, int, str]]:
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT ce.from_path, ce.call_site_line, ce.to_path, s.line_start, ce.edge_confidence "
        "FROM call_edges ce "
        "JOIN symbols s ON s.qualified_name = ce.to_symbol "
        "WHERE ce.from_path LIKE '%.rs' AND ce.to_path LIKE '%.rs' "
        "  AND ce.call_site_line IS NOT NULL"
    ).fetchall()
    conn.close()
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        type=Path,
        default=repo_root_from_here(),
        help="Rust project to measure (default: this repo)",
    )
    parser.add_argument(
        "--calm-bin",
        type=Path,
        default=repo_root_from_here() / "target" / "release" / "calm",
        help="Path to a `calm` binary built with --features scip-overlay",
    )
    parser.add_argument(
        "--ra-features",
        default="all",
        help=(
            "Cargo features to activate for the rust-analyzer oracle run. "
            "Default 'all' (--all-features) so the oracle covers feature-gated "
            "source files CALM's tree-sitter indexer always parses (issue #72); "
            "pass a comma-separated list to narrow it."
        ),
    )
    args = parser.parse_args()
    repo = args.repo.resolve()
    calm_bin = args.calm_bin.resolve()

    if not calm_bin.exists():
        sys.exit(
            f"{calm_bin} not found. Build it first:\n"
            "  cargo build --release -p calm-cli --features scip-overlay"
        )
    ra_bin = find_rust_analyzer()

    with tempfile.TemporaryDirectory() as tmp:
        scip_path = Path(tmp) / "oracle.scip"
        # Oracle coverage fix (issue #72): a bare `rust-analyzer scip <repo>`
        # compiles only DEFAULT Cargo features, so it emits ZERO occurrences
        # for source files gated behind non-default features (bundle.rs behind
        # `index-bundles`, lsp/*.rs behind `lsp-overlay`, http.rs behind
        # `http`, ...). CALM's tree-sitter indexer parses those files
        # UNCONDITIONALLY, so any CALM edge touching them could never match the
        # oracle -- inflating the false-positive count independently of
        # resolver quality. Activating all features aligns the oracle's file
        # coverage with CALM's. `"all"` (not a hand-listed set) because the
        # gating features live on different workspace members (`http` on
        # calm-server, not calm-core) and a per-name feature list would error
        # on the crate that lacks it; --all-features cannot.
        feats = "all" if args.ra_features == "all" else args.ra_features.split(",")
        ra_config = Path(tmp) / "ra-config.json"
        ra_config.write_text(json.dumps({"cargo": {"features": feats}}))
        print(f"Running rust-analyzer scip on {repo} (features={args.ra_features!r}) ...")
        run([ra_bin, "scip", str(repo), "--output", str(scip_path),
             "--config-path", str(ra_config)])

        dump = run([str(calm_bin), "scip-dump", str(scip_path)])
        occurrences = [json.loads(line) for line in dump.stdout.splitlines() if line.strip()]
        print(f"Decoded {len(occurrences)} SCIP occurrences.")

    oracle = build_oracle(occurrences)
    print(f"Oracle edges (non-local ref -> def): {len(oracle)}")

    print(f"Indexing {repo} with `calm index` (Phase A syntactic resolver only) ...")
    run([str(calm_bin), "index", "--project-root", str(repo)])
    db_path = repo / ".calm" / "index.db"
    calm_edges = load_calm_edges(db_path)
    print(f"ci call_edges (Rust, with a call site line): {len(calm_edges)}")

    matched = [e for e in calm_edges if (e[0], e[1], e[2], e[3]) in oracle]
    precision = len(matched) / len(calm_edges) if calm_edges else 0.0
    oracle_hit = {(e[0], e[1], e[2], e[3]) for e in matched}
    recall = len(oracle_hit) / len(oracle) if oracle else 0.0

    # Oracle coverage audit (issue #72): a file the oracle never emitted an
    # occurrence for (still feature-gated even under --all-features, generated,
    # or otherwise excluded) can't corroborate ANY CALM edge originating in it,
    # so those edges depress precision for reasons unrelated to resolver
    # quality. Report coverage explicitly, plus a `precision_on_covered` that
    # scores only edges whose from-file the oracle can actually see -- the
    # honest number check-b2-thresholds.sh's floors should eventually track.
    oracle_files = {o["file"] for o in occurrences}
    calm_from_files = {e[0] for e in calm_edges}
    uncovered_from_files = sorted(f for f in calm_from_files if f not in oracle_files)
    covered_edges = [e for e in calm_edges if e[0] in oracle_files]
    covered_matched = [e for e in covered_edges if (e[0], e[1], e[2], e[3]) in oracle]
    precision_on_covered = (
        len(covered_matched) / len(covered_edges) if covered_edges else 0.0
    )

    by_conf: dict[str, list[tuple]] = defaultdict(list)
    for e in calm_edges:
        by_conf[e[4]].append(e)
    conf_precision = {}
    for conf, edges in sorted(by_conf.items()):
        hit = sum(1 for e in edges if (e[0], e[1], e[2], e[3]) in oracle)
        conf_precision[conf] = {
            "count": len(edges),
            "precision": hit / len(edges) if edges else 0.0,
        }

    print()
    print(f"Overall precision: {precision:.3f}  ({len(matched)}/{len(calm_edges)})")
    print(f"Overall recall:    {recall:.3f}  ({len(oracle_hit)}/{len(oracle)})")
    print()
    print(f"{'confidence':<12} {'count':>8} {'precision':>10}")
    for conf, stats in conf_precision.items():
        print(f"{conf:<12} {stats['count']:>8} {stats['precision']:>10.3f}")

    print()
    print(
        f"Oracle file coverage: {len(oracle_files)} file(s) with >=1 occurrence; "
        f"{len(uncovered_from_files)}/{len(calm_from_files)} CALM from-file(s) "
        f"invisible to the oracle"
    )
    print(
        f"Precision on oracle-covered files only: {precision_on_covered:.3f}  "
        f"({len(covered_matched)}/{len(covered_edges)})"
    )
    if uncovered_from_files:
        preview = ", ".join(uncovered_from_files[:8])
        print(
            f"  uncovered from-files (first 8): {preview}"
            f"{', ...' if len(uncovered_from_files) > 8 else ''}"
        )

    result = {
        "repo": str(repo),
        "oracle_edges": len(oracle),
        "calm_edges": len(calm_edges),
        "precision": precision,
        "recall": recall,
        "by_confidence": conf_precision,
        "oracle_covered_files": len(oracle_files),
        "calm_from_files": len(calm_from_files),
        "uncovered_from_files": len(uncovered_from_files),
        "precision_on_covered": precision_on_covered,
    }
    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(result, indent=2))
    print(f"\nWrote {out_path}")


if __name__ == "__main__":
    main()
