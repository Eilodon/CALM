#!/usr/bin/env python3
"""B14 -- Risk calibration: does `calm guard`'s aggregate_risk actually track
real bug-inducing commits, on CALM's OWN git history (not a synthetic corpus)?

Every other risk-adjacent benchmark in this suite (b12's edit_context/
diff_impact checks) drives the tool against freshly-written test fixtures --
it answers "does the mechanism work as coded," not "does the resulting risk
score line up with what actually turned out to be risky." This benchmark
answers the second question, because P1 of the 2026-08-05 CALM-improvements
review named it explicitly: extending the risk model (comment-only/deletion/
visibility signals, WS-2 P2 items) without a calibration baseline first means
extending it blind -- no way to tell if a new signal helps or just adds noise.

Ground truth (SZZ-lite, not full SZZ): a commit C is labeled RISKY if some
later commit whose message starts with `fix(`/`fix:`/`fix ` touches at least
one file C also touched, AND that fix commit is C's own immediate successor
in the (no-merges) commit graph -- i.e. C shipped, then the very next commit
was a fix touching overlapping code. This is deliberately narrow (misses
fixes that land N commits later, or land in a different file that still
depends on C's change) -- see README's "What this does NOT measure" for the
honest accounting of what a narrower/wider window would change. A random
sample of same-era, non-risky, non-doc-only commits is the SAFE population.

Methodology: for each labeled commit, `git worktree add --detach` at the
commit's OWN PARENT, index THAT worktree (embeddings disabled --
`compute_touch_risk` never reads them, and they cost ~3min/run this repo's
size -- see this file's own dry-run notes), then run `calm guard --commits
<parent>..<commit> --json` from inside it and read `aggregate_risk`. Indexing
at the parent (not at today's HEAD) matters: `diff_impact` maps hunk line
ranges onto the CURRENT index's symbol table, so analyzing an old diff
against today's index (files renamed/moved/grown since) would silently
misattribute risk to the wrong symbols entirely -- exactly the class of
oracle bug flagged by this repo's own "audit the oracle before publishing a
benchmark" convention.
"""

from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CALM_BIN = REPO_ROOT / "target" / "debug" / "calm"
WORK_DIR = REPO_ROOT / "benchmarks" / "b14_risk_calibration" / ".work"
FIX_PREFIXES = ("fix(", "fix:", "fix ")
CODE_PREFIXES = ("feat(", "feat:", "fix(", "fix:", "refactor(", "perf(", "style(", "test(")
RNG_SEED = 20260805


def sh(args: list[str], cwd: Path | None = None) -> str:
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True, check=True).stdout


@dataclass
class LabeledCommit:
    sha: str
    parent_sha: str
    subject: str
    label: str  # "risky" | "safe"
    evidence: str = ""  # the fix commit's sha+subject, for risky labels
    overlap_files: int = 0


def mine_history() -> list[LabeledCommit]:
    """SZZ-lite mining over this repo's own (no-merges) commit log -- see
    module docstring for the exact labeling rule."""
    log = sh(["git", "log", "--no-merges", "--pretty=format:%H|%s"], cwd=REPO_ROOT)
    commits = [line.split("|", 1) for line in log.splitlines() if line.strip()]

    def files_of(sha: str) -> set[str]:
        out = sh(["git", "show", "--name-only", "--pretty=format:", sha], cwd=REPO_ROOT)
        return {f for f in out.splitlines() if f.strip()}

    risky: list[LabeledCommit] = []
    risky_parent_shas: set[str] = set()
    for i, (sha, subject) in enumerate(commits):
        if not subject.lower().startswith(FIX_PREFIXES) or i + 1 >= len(commits):
            continue
        parent_sha, parent_subject = commits[i + 1]
        if parent_subject.lower().startswith(FIX_PREFIXES):
            continue  # a fix "fixing" a fix isn't the bug-introducing commit itself
        overlap = files_of(sha) & files_of(parent_sha)
        if not overlap:
            continue
        try:
            grandparent = sh(["git", "rev-parse", f"{parent_sha}^"], cwd=REPO_ROOT).strip()
        except subprocess.CalledProcessError:
            continue  # root commit, no parent to diff against
        risky.append(
            LabeledCommit(
                sha=parent_sha,
                parent_sha=grandparent,
                subject=parent_subject,
                label="risky",
                evidence=f"{sha[:8]} {subject}",
                overlap_files=len(overlap),
            )
        )
        risky_parent_shas.add(parent_sha)

    rng = random.Random(RNG_SEED)
    safe_pool = [
        (sha, subject)
        for sha, subject in commits
        if sha not in risky_parent_shas
        and subject.lower().startswith(CODE_PREFIXES)
        and not subject.lower().startswith(FIX_PREFIXES)
    ]
    rng.shuffle(safe_pool)
    safe: list[LabeledCommit] = []
    for sha, subject in safe_pool:
        if len(safe) >= len(risky):
            break
        try:
            parent = sh(["git", "rev-parse", f"{sha}^"], cwd=REPO_ROOT).strip()
        except subprocess.CalledProcessError:
            continue  # root commit, no parent
        safe.append(LabeledCommit(sha=sha, parent_sha=parent, subject=subject, label="safe"))

    return risky + safe


def prepare_worktree(commit: LabeledCommit) -> Path:
    wt = WORK_DIR / commit.parent_sha[:12]
    if wt.exists():
        return wt
    WORK_DIR.mkdir(parents=True, exist_ok=True)
    sh(["git", "worktree", "add", "--detach", str(wt), commit.parent_sha], cwd=REPO_ROOT)
    calm_dir = wt / ".calm"
    calm_dir.mkdir(exist_ok=True)
    # Embeddings are irrelevant to compute_touch_risk (caller_count/is_hub/
    # signature-change only) and cost ~3min/run at this repo's size --
    # disabling them is the difference between this benchmark finishing in
    # tens of minutes vs. multiple hours, not a methodology shortcut.
    (calm_dir / "config.json").write_text(json.dumps({"semantic_search": {"enabled": False}}))
    return wt


def cleanup_worktree(wt: Path) -> None:
    sh(["git", "worktree", "remove", "--force", str(wt)], cwd=REPO_ROOT)


def run_guard_for(commit: LabeledCommit, wt: Path) -> dict:
    index_out = subprocess.run(
        [str(CALM_BIN), "index", "--project-root", str(wt)],
        capture_output=True, text=True, timeout=600,
    )
    if index_out.returncode != 0:
        return {"error": f"calm index failed: {index_out.stderr[-2000:]}"}

    commits_range = f"{commit.parent_sha}..{commit.sha}"
    guard_out = subprocess.run(
        [str(CALM_BIN), "guard", "--project-root", str(wt), "--commits", commits_range, "--json"],
        capture_output=True, text=True, timeout=60,
    )
    try:
        return json.loads(guard_out.stdout)
    except json.JSONDecodeError:
        return {"error": f"calm guard produced non-JSON output: {guard_out.stdout[-2000:]}\nstderr={guard_out.stderr[-1000:]}"}


SEVERITY = {"low": 0, "medium": 1, "high": 2}


def confusion_at_threshold(rows: list[dict], threshold: str) -> dict:
    t = SEVERITY[threshold]
    tp = fp = tn = fn = 0
    for r in rows:
        if "error" in r:
            continue
        actual = SEVERITY.get(r["aggregate_risk"], 0) >= t
        truth = r["label"] == "risky"
        if truth and actual:
            tp += 1
        elif truth and not actual:
            fn += 1
        elif not truth and actual:
            fp += 1
        else:
            tn += 1
    precision = tp / (tp + fp) if (tp + fp) else float("nan")
    recall = tp / (tp + fn) if (tp + fn) else float("nan")
    f1 = 2 * precision * recall / (precision + recall) if (precision + recall) and precision == precision and recall == recall and (precision + recall) > 0 else float("nan")
    annoyance = (tp + fp) / (tp + fp + tn + fn) if (tp + fp + tn + fn) else float("nan")
    return {
        "threshold": threshold, "tp": tp, "fp": fp, "tn": tn, "fn": fn,
        "precision": precision, "recall": recall, "f1": f1,
        "annoyance_rate": annoyance,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sample", type=int, default=None, help="cap risky+safe count each (default: all risky found)")
    parser.add_argument("--keep-worktrees", action="store_true")
    parser.add_argument("--out", default=str(Path(__file__).parent / "results.json"))
    args = parser.parse_args()

    if not CALM_BIN.exists():
        print(f"error: {CALM_BIN} not found -- run `cargo build -p calm-cli` first", file=sys.stderr)
        return 1

    labeled = mine_history()
    risky = [c for c in labeled if c.label == "risky"]
    safe = [c for c in labeled if c.label == "safe"]
    print(f"mined {len(risky)} risky (SZZ-lite) + {len(safe)} safe commits from {REPO_ROOT.name}'s own history", file=sys.stderr)
    if args.sample:
        risky = risky[: args.sample]
        safe = safe[: args.sample]
    labeled = risky + safe

    rows = []
    t0 = time.time()
    for i, commit in enumerate(labeled, 1):
        print(f"[{i}/{len(labeled)}] {commit.label:5s} {commit.sha[:8]} {commit.subject[:70]!r} ...", file=sys.stderr, end=" ", flush=True)
        wt = prepare_worktree(commit)
        try:
            result = run_guard_for(commit, wt)
        finally:
            if not args.keep_worktrees:
                cleanup_worktree(wt)
        row = {
            "sha": commit.sha, "parent_sha": commit.parent_sha, "subject": commit.subject,
            "label": commit.label, "evidence": commit.evidence, "overlap_files": commit.overlap_files,
        }
        if "error" in result:
            row["error"] = result["error"]
            print(f"ERROR: {result['error'][:120]}", file=sys.stderr)
        else:
            row["aggregate_risk"] = result.get("aggregate_risk", "low")
            row["files_changed"] = len(result.get("files_changed", []))
            row["high_risk_symbols"] = [
                s["qualified_name"] for s in result.get("affected_symbols", [])
                if s.get("risk_assessment", {}).get("level") == "high"
            ]
            print(f"-> {row['aggregate_risk']}", file=sys.stderr)
        rows.append(row)

    elapsed = time.time() - t0
    confusion = [confusion_at_threshold(rows, t) for t in ("low", "medium", "high")]

    out = {
        "repo": REPO_ROOT.name,
        "methodology": "SZZ-lite on own git history -- see run_benchmark.py module docstring",
        "risky_count": len(risky),
        "safe_count": len(safe),
        "error_count": sum(1 for r in rows if "error" in r),
        "elapsed_seconds": round(elapsed, 1),
        "confusion_by_threshold": confusion,
        "rows": rows,
    }
    out_path = Path(args.out)
    out_path.write_text(json.dumps(out, indent=2))
    print(f"\nwrote {out_path}", file=sys.stderr)
    print(json.dumps(confusion, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
