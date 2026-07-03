#!/usr/bin/env python3
"""B2 — Call Graph Resolution Quality benchmark runner.

Usage:
    benchmarks/.venv/bin/python benchmarks/b2_call_graph_quality/run_benchmark.py [--repo PATH]

Scope (this implementation): **Rust only**. Uses `rust-analyzer scip` as the
ground-truth oracle for the Rust call graph and measures how well `ci`'s
Tier-0/Tier-2 syntactic resolver (Phase A of the Rust support plan) agrees
with it, broken down by `edge_confidence`.

Requires:
  - `rust-analyzer` on PATH (or resolvable via rustup/VS Code — same
    detection `ci_core::scip::runner::resolve_binary` uses).
  - `ci` built with the `scip-overlay` feature, for the hidden `scip-dump`
    subcommand that decodes the oracle `.scip` file to JSON (reuses
    `ci_core::scip::parse` instead of re-implementing SCIP protobuf decoding
    in Python):
        cargo build --release -p ci-cli --features scip-overlay

Methodology:
  1. Run `rust-analyzer scip <repo> --output oracle.scip`.
  2. Decode it via `ci scip-dump oracle.scip` -> flat occurrences.
  3. Build the oracle edge set: for every non-local reference occurrence,
     resolve its symbol to its (non-local) definition occurrence, giving
     (ref_file, ref_line) -> (def_file, def_line). This mirrors
     `ci_core::scip::ingest::ingest_occurrences`'s own matching exactly, so
     the oracle here is built the same way Phase B's real ingest would use
     it — this benchmark and Phase B are measuring the same underlying
     correspondence.
  4. Run `ci index --project-root <repo>` (default features -- i.e. Phase A
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
    ci_core::scip::ingest::ingest_occurrences's own matching."""
    def_of: dict[str, tuple[str, int]] = {}
    for o in occurrences:
        if o["is_def"] and not o["is_local"]:
            def_of[o["symbol"]] = (o["file"], o["line"])

    oracle: set[tuple[str, int, str, int]] = set()
    for o in occurrences:
        if o["is_def"] or o["is_local"]:
            continue
        target = def_of.get(o["symbol"])
        if target is None:
            continue
        oracle.add((o["file"], o["line"], target[0], target[1]))
    return oracle


def load_ci_edges(db_path: Path) -> list[tuple[str, int, str, int, str]]:
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
        "--ci-bin",
        type=Path,
        default=repo_root_from_here() / "target" / "release" / "ci",
        help="Path to a `ci` binary built with --features scip-overlay",
    )
    args = parser.parse_args()
    repo = args.repo.resolve()
    ci_bin = args.ci_bin.resolve()

    if not ci_bin.exists():
        sys.exit(
            f"{ci_bin} not found. Build it first:\n"
            "  cargo build --release -p ci-cli --features scip-overlay"
        )
    ra_bin = find_rust_analyzer()

    with tempfile.TemporaryDirectory() as tmp:
        scip_path = Path(tmp) / "oracle.scip"
        print(f"Running rust-analyzer scip on {repo} ...")
        run([ra_bin, "scip", str(repo), "--output", str(scip_path)])

        dump = run([str(ci_bin), "scip-dump", str(scip_path)])
        occurrences = [json.loads(line) for line in dump.stdout.splitlines() if line.strip()]
        print(f"Decoded {len(occurrences)} SCIP occurrences.")

    oracle = build_oracle(occurrences)
    print(f"Oracle edges (non-local ref -> def): {len(oracle)}")

    print(f"Indexing {repo} with `ci index` (Phase A syntactic resolver only) ...")
    run([str(ci_bin), "index", "--project-root", str(repo)])
    db_path = repo / ".codeindex" / "index.db"
    ci_edges = load_ci_edges(db_path)
    print(f"ci call_edges (Rust, with a call site line): {len(ci_edges)}")

    matched = [e for e in ci_edges if (e[0], e[1], e[2], e[3]) in oracle]
    precision = len(matched) / len(ci_edges) if ci_edges else 0.0
    oracle_hit = {(e[0], e[1], e[2], e[3]) for e in matched}
    recall = len(oracle_hit) / len(oracle) if oracle else 0.0

    by_conf: dict[str, list[tuple]] = defaultdict(list)
    for e in ci_edges:
        by_conf[e[4]].append(e)
    conf_precision = {}
    for conf, edges in sorted(by_conf.items()):
        hit = sum(1 for e in edges if (e[0], e[1], e[2], e[3]) in oracle)
        conf_precision[conf] = {
            "count": len(edges),
            "precision": hit / len(edges) if edges else 0.0,
        }

    print()
    print(f"Overall precision: {precision:.3f}  ({len(matched)}/{len(ci_edges)})")
    print(f"Overall recall:    {recall:.3f}  ({len(oracle_hit)}/{len(oracle)})")
    print()
    print(f"{'confidence':<12} {'count':>8} {'precision':>10}")
    for conf, stats in conf_precision.items():
        print(f"{conf:<12} {stats['count']:>8} {stats['precision']:>10.3f}")

    result = {
        "repo": str(repo),
        "oracle_edges": len(oracle),
        "ci_edges": len(ci_edges),
        "precision": precision,
        "recall": recall,
        "by_confidence": conf_precision,
    }
    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(result, indent=2))
    print(f"\nWrote {out_path}")


if __name__ == "__main__":
    main()
