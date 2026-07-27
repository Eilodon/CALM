#!/usr/bin/env python3
"""Resolution — multi-language call-graph tier baseline.

Usage:
    benchmarks/.venv/bin/python benchmarks/resolution/run_benchmark.py [--lang go,java,...]

Companion to B2 (`benchmarks/b2_call_graph_quality/`, Rust-only, precision/
recall vs a `rust-analyzer scip` oracle). This benchmark has a different job:
it measures `calm`'s **tier distribution** (formal/resolved/inferred/textual/
ambiguous/unresolved — see `EdgeConfidence`) on real, external, pinned OSS
repos across the 8 languages targeted by
`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`, with **no
oracle** (none of Go/Java/C#/C/C++/JS/PHP/SQL has a SCIP provider wired up
yet — see that plan's Phase 2/3). It exists to answer one question before
Phase 2 lands: how much did Phase 0/1's heuristics (same-dir tier, type_map,
PSR-4, stack-graphs JS, ...) already move the needle per language, so Phase 2
effort can be prioritized by actual remaining gap instead of guesswork.

Requires:
  - `cargo build --release -p calm-cli` (no `scip-overlay` feature needed —
    Phase 2 providers don't exist yet, so there is nothing for it to overlay
    on non-Rust corpora; the Rust ScipProvider still runs harmlessly and
    contributes 0 edges against foreign-language repos).
  - `git` on PATH with network access to GitHub (shallow-clones each corpus
    once into `benchmarks/resolution/corpus/<lang>/`, gitignored, reused on
    subsequent runs unless `--fresh-clone` is passed).

Methodology:
  1. For each language, shallow-clone (`--depth 1`) a small pinned real OSS
     repo (see CORPORA below) into `corpus/<lang>/` if not already present,
     and record the resolved commit SHA for reproducibility (we pin to
     "whatever HEAD of default branch resolved to on first clone", not a
     hand-picked release tag -- recorded in the output so a re-run knows
     exactly what was measured).
  2. Write `<corpus>/.calm/config.json` with `semantic_search.enabled=false`
     before indexing -- this benchmark only reads `call_edges`/`symbols`,
     and embeddings add real wall-clock (~30s+ per medium repo) for a signal
     nobody reads here.
  3. Run `calm index --project-root <corpus>`, timing wall-clock.
  4. Read `.calm/index.db`: join `call_edges.from_symbol` to
     `symbols.qualified_name` to get `symbols.language`, filter to the
     corpus's own designated language (foreign-language noise inside a repo,
     e.g. a JS build script in a Go repo, is dropped from that language's own
     row -- it would show up under its own language if that language were
     also in CORPORA).
  5. `formal_pct`/`resolved_pct` etc. are edge-count share per confidence
     tier. `overlay_match_rate` is reported `null` for every language here
     **on purpose** -- no Phase 2 SCIP provider exists for any of them yet,
     so there is nothing to report; do not confuse this with "0 edges
     upgraded", which would imply a provider ran and failed to help.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

# Corpus key -> the `symbols.language` string `language_for_extension`
# (lang_constants.rs) actually assigns. Most match the corpus key 1:1; JS is
# the one mismatch (`.js` maps to `"javascript"`, not `"js"`) -- get this
# wrong and the benchmark silently reports 0 edges for a real corpus, not a
# real 0 (this bit us on the first real run against express).
DB_LANGUAGE: dict[str, str] = {
    "js": "javascript",
}


def db_language(lang: str) -> str:
    return DB_LANGUAGE.get(lang, lang)


# lang key -> (git clone url, human label for the README/table)
CORPORA: dict[str, tuple[str, str]] = {
    "go": ("https://github.com/gin-gonic/gin.git", "gin"),
    "java": ("https://github.com/spring-projects/spring-petclinic.git", "spring-petclinic"),
    "csharp": ("https://github.com/dotnet-architecture/eShopOnWeb.git", "eShopOnWeb"),
    "c": ("https://github.com/redis/redis.git", "redis"),
    "cpp": ("https://github.com/fmtlib/fmt.git", "fmt"),
    "js": ("https://github.com/expressjs/express.git", "express"),
    "php": ("https://github.com/monicahq/monica.git", "monica"),
    "sql": ("https://github.com/jOOQ/sakila.git", "sakila (multi-dialect mirror)"),
    # Phase E (2026-07-11): the 11 Phase B/C languages, batched into this
    # same run rather than 9 separate clone+run cycles (deliberate per the
    # 25-language-expansion plan's own §1.7 note) — real, modest-size OSS
    # repos, not synthetic fixtures.
    "kotlin": ("https://github.com/square/kotlinpoet.git", "kotlinpoet"),
    "swift": ("https://github.com/apple/swift-argument-parser.git", "swift-argument-parser"),
    "scala": ("https://github.com/lihaoyi/requests-scala.git", "requests-scala"),
    "dart": ("https://github.com/dart-lang/args.git", "args"),
    "lua": ("https://github.com/kikito/middleclass.git", "middleclass"),
    "elixir": ("https://github.com/dashbitco/nimble_options.git", "nimble_options"),
    "haskell": ("https://github.com/kowainik/co-log.git", "co-log"),
    "ocaml": ("https://github.com/ocsigen/lwt.git", "lwt"),
    "zig": ("https://github.com/MasterQ32/zig-args.git", "zig-args"),
    "powershell": ("https://github.com/dahlbyk/posh-git.git", "posh-git"),
    "groovy": ("https://github.com/http-builder-ng/http-builder-ng.git", "http-builder-ng"),
}


def repo_root_from_here() -> Path:
    # benchmarks/resolution/run_benchmark.py -> repo root is 2 levels up
    return Path(__file__).resolve().parents[2]


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def ensure_corpus(lang: str, corpus_dir: Path, fresh_clone: bool) -> str:
    url, _label = CORPORA[lang]
    target = corpus_dir / lang
    if fresh_clone and target.exists():
        shutil.rmtree(target)
    if not target.exists():
        print(f"[{lang}] cloning {url} ...")
        run(["git", "clone", "--depth", "1", "--single-branch", url, str(target)])
    sha = run(["git", "-C", str(target), "rev-parse", "HEAD"]).stdout.strip()
    return sha


def write_disable_embeddings_config(corpus_path: Path) -> None:
    calm_dir = corpus_path / ".calm"
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


def index_corpus(calm_bin: Path, corpus_path: Path) -> float:
    """Removes any stale `.calm/index.db` (keeps our config.json), runs
    `calm index`, returns wall-clock seconds."""
    db_path = corpus_path / ".calm" / "index.db"
    if db_path.exists():
        db_path.unlink()
    start = time.monotonic()
    run([str(calm_bin), "index", "--project-root", str(corpus_path)])
    return time.monotonic() - start


TIERS = ["formal", "resolved", "inferred", "textual", "ambiguous", "unresolved"]


def read_tier_histogram(db_path: Path, lang: str) -> dict:
    db_lang = db_language(lang)
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT ce.edge_confidence, COUNT(*) "
        "FROM call_edges ce "
        "JOIN symbols s ON s.qualified_name = ce.from_symbol "
        "WHERE s.language = ? "
        "GROUP BY ce.edge_confidence",
        (db_lang,),
    ).fetchall()
    file_stats = conn.execute(
        "SELECT COUNT(DISTINCT path), COUNT(*) FROM symbols WHERE language = ?",
        (db_lang,),
    ).fetchone()
    conn.close()
    histogram = {tier: 0 for tier in TIERS}
    for confidence, count in rows:
        histogram[confidence] = histogram.get(confidence, 0) + count
    total = sum(histogram.values())
    pct = {f"{tier}_pct": (histogram[tier] / total if total else 0.0) for tier in TIERS}
    return {
        "edges_total": total,
        "tier_histogram": histogram,
        "files_with_symbols": file_stats[0] or 0,
        "symbols_total": file_stats[1] or 0,
        **pct,
    }


# Specifier roots that denote the current project by language definition, not
# by resembling a file name: Rust's `crate::`/`self::`/`super::`.
INTERNAL_ROOTS = {"crate", "self", "super"}


def read_import_resolution(db_path: Path, lang: str) -> dict:
    """`import_edges.to_path` resolution rate for in-project ("first-party") imports.

    The tier histogram above only covers `call_edges`. Nothing measured
    `import_edges`, even though the `dependencies` tool and the
    `[[boundaries]]` fitness gate both read it -- which is exactly why three
    resolver defects (Rust `super::` inside an inline `mod`, Python dotted
    imports not anchored at the importing file's package root, and
    `sys.path.insert(...)` roots being ignored) went unnoticed.

    A raw resolved/total ratio would be meaningless: the large majority of
    import edges legitimately point *outside* the repo (stdlib, crates.io,
    npm) and must stay NULL. On CALM's own tree that is 534 of 560 edges, so
    the raw ratio reads ~35% while first-party resolution is ~99%. The
    denominator is therefore restricted to edges that plausibly have an
    in-project target.

    "Plausibly first-party" is deliberately over-inclusive: a relative
    specifier, or one whose root segment AND its tail both match something in
    the tree. A third-party module that happens to share a name with a local
    file is counted in the denominator and, if unresolved, drags the reported
    rate down. That bias is the safe one for a metric used to decide where to
    invest effort -- it can understate resolution quality, never overstate it.

    Matching the ROOT ALONE (the rule until 2026-07-27) is not enough, and was
    badly wrong for the JVM: Maven/Gradle lay sources out under
    `src/main/java/org/...`, so the tree literally contains directories named
    `java`, `org` and `com` -- the exact root segments of `java.io.*`,
    `org.springframework.*` and every other vendor package. Every JDK and
    Spring import therefore landed in the denominator. On spring-petclinic
    that reported 386 first-party imports where only 22 exist, understating
    the true rate by ~17x. Requiring the tail to match as well (the imported
    leaf is an indexed file's stem, or its parent segment is a real directory)
    keeps the over-inclusive direction while dropping that whole class of
    phantom denominator entries.
    """
    db_lang = db_language(lang)
    conn = sqlite3.connect(db_path)
    paths = [r[0] for r in conn.execute("SELECT path FROM file_index").fetchall()]
    rows = conn.execute(
        "SELECT ie.module_name, ie.to_path FROM import_edges ie "
        "JOIN file_index fi ON fi.path = ie.from_path WHERE fi.language = ?",
        (db_lang,),
    ).fetchall()
    conn.close()

    # `-`/`_` folded: a Cargo crate `calm-core` is imported as `calm_core`.
    stems = {Path(p).stem.replace("-", "_") for p in paths}
    dirs = {seg.replace("-", "_") for p in paths for seg in Path(p).parent.parts}

    def segments(spec: str) -> list[str]:
        flat = spec.replace("::", "/").replace(".", "/").replace("\\", "/")
        return [s for s in flat.split("/") if s]

    first_party = resolved = 0
    for module, to_path in rows:
        spec = (module or "").strip().strip("\"'")
        if not spec:
            continue
        segs = segments(spec)
        root = segs[0].replace("-", "_") if segs else ""
        # The tail is never matched on its OWN: `std::path` ends in a segment
        # that collides with plenty of repos' own `path` file, and counting
        # that would have shown 52 phantom misses on CALM's own tree. But the
        # root on its own is equally unsound in the other direction (see the
        # JVM case in the docstring), so both ends must agree. INTERNAL_ROOTS
        # are in-project by language definition rather than by name lookup,
        # which a stem/dir match cannot see (a bare `use super::*;` has no
        # name to look up at all).
        leaf = segs[-1].replace("-", "_") if segs else ""
        parent = segs[-2].replace("-", "_") if len(segs) >= 2 else ""
        root_ok = root in INTERNAL_ROOTS or root in stems or root in dirs
        # A single-segment specifier (`import express`) has no tail to check
        # independently -- root and leaf are the same token, so requiring both
        # would be the same test twice, not a stronger one.
        tail_ok = root_ok if len(segs) == 1 else (leaf in stems or (parent != "" and parent in dirs))
        plausible = spec.startswith(".") or (root_ok and tail_ok)
        if plausible:
            first_party += 1
            resolved += 1 if to_path else 0

    return {
        "import_edges_total": len(rows),
        "import_first_party": first_party,
        "import_resolved": resolved,
        "import_resolved_pct": (resolved / first_party) if first_party else 0.0,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--lang",
        type=str,
        default=None,
        help="Comma-separated subset of languages to run (default: all in CORPORA)",
    )
    parser.add_argument(
        "--calm-bin",
        type=Path,
        default=repo_root_from_here() / "target" / "release" / "calm",
        help="Path to a `calm` binary (default features are enough)",
    )
    parser.add_argument(
        "--corpus-dir",
        type=Path,
        default=Path(__file__).parent / "corpus",
        help="Where per-language corpora live (gitignored)",
    )
    parser.add_argument(
        "--fresh-clone",
        action="store_true",
        help="Delete and re-clone each corpus before indexing",
    )
    args = parser.parse_args()

    calm_bin = args.calm_bin.resolve()
    if not calm_bin.exists():
        sys.exit(f"{calm_bin} not found. Build it first:\n  cargo build --release -p calm-cli")

    langs = args.lang.split(",") if args.lang else list(CORPORA.keys())
    unknown = [l for l in langs if l not in CORPORA]
    if unknown:
        sys.exit(f"Unknown language(s): {unknown}. Known: {list(CORPORA.keys())}")

    args.corpus_dir.mkdir(parents=True, exist_ok=True)

    results = []
    for lang in langs:
        _url, label = CORPORA[lang]
        print(f"\n=== {lang} ({label}) ===")
        sha = ensure_corpus(lang, args.corpus_dir, args.fresh_clone)
        corpus_path = args.corpus_dir / lang
        write_disable_embeddings_config(corpus_path)
        wall_time = index_corpus(calm_bin, corpus_path)
        db_path = corpus_path / ".calm" / "index.db"
        stats = read_tier_histogram(db_path, lang)
        stats.update(read_import_resolution(db_path, lang))
        row = {
            "lang": lang,
            "corpus_label": label,
            "commit": sha,
            "wall_time_sec": round(wall_time, 2),
            "overlay_match_rate": None,  # no Phase 2 SCIP provider exists for any of these yet
            **stats,
        }
        results.append(row)
        print(
            f"  {stats['symbols_total']} symbols, {stats['edges_total']} call edges, "
            f"wall={wall_time:.1f}s"
        )
        if stats["edges_total"]:
            print(
                "  tiers: "
                + ", ".join(f"{t}={stats['tier_histogram'][t]}" for t in TIERS if stats["tier_histogram"][t])
            )
        if stats["import_first_party"]:
            print(
                f"  imports: {stats['import_resolved']}/{stats['import_first_party']} "
                f"first-party resolved ({stats['import_resolved_pct']*100:.1f}%), "
                f"{stats['import_edges_total']} edges total"
            )

    print(
        f"\n{'lang':<8} {'edges':>8} {'formal%':>8} {'resolved%':>10} "
        f"{'ambiguous%':>11} {'import1p%':>10} {'wall(s)':>8}"
    )
    for r in results:
        print(
            f"{r['lang']:<8} {r['edges_total']:>8} {r['formal_pct']*100:>7.1f}% "
            f"{r['resolved_pct']*100:>9.1f}% {r['ambiguous_pct']*100:>10.1f}% "
            f"{r['import_resolved_pct']*100:>9.1f}% {r['wall_time_sec']:>8.1f}"
        )

    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(results, indent=2))
    print(f"\nWrote {out_path}")


if __name__ == "__main__":
    main()
