#!/usr/bin/env bash
# Regression gate for B2 (benchmarks/b2_call_graph_quality) -- the Rust
# call-graph precision/recall benchmark against a rust-analyzer SCIP oracle.
#
# Written 2026-08-04 after an audit found this repo's benchmark suite had
# rigorous methodology but ZERO of its correctness numbers (B2/B3/B4/B6/B7)
# wired into any CI gate -- thresholds.toml only covers architectural
# fitness metrics (hotspot risk, boundaries, config drift). A real indexer
# regression (e.g. the F1 anonymous-callback JS bug this repo already found
# and fixed once, commit 34918c4) could ship silently on the Rust resolver
# with nothing to catch it. This closes that gap for B2 specifically --
# it's the one benchmark in the suite that's self-contained (self-repo only,
# no external corpus clone, no multi-language build matrix) and therefore
# cheap enough to run in CI, unlike B7 (clones + builds 6 external repos per
# run) which stays a manual/periodic benchmark for cost reasons.
#
# Floors below are set with headroom under the last real measurement
# (benchmarks/b2_call_graph_quality/README.md's "Kết quả đo lần đầu" table),
# same margin-not-exact-match reasoning thresholds.toml's own
# max_hotspot_risk comment already documents for this repo: catch a real
# regression, don't flake on ordinary noise.
#   measured -> floor
#   recall             0.193 -> 0.15  (resolver intentionally does no type
#                                       inference; recall moving at all is
#                                       expected as Tier-0/2 evolves -- this
#                                       floor exists to catch a collapse, not
#                                       to lock the number down)
#   precision           0.795 -> 0.70
#   inferred precision  0.967 -> 0.85
#   resolved precision  0.935 -> 0.80
#   textual precision   0.514 -> 0.40  (already the weakest tier by design --
#                                       agents are told to trust it least --
#                                       but a further collapse is still a
#                                       real regression worth catching)
#
# Usage:
#   scripts/check-b2-thresholds.sh [path-to-results.json]
#   (default: benchmarks/b2_call_graph_quality/results.json)
set -euo pipefail
cd "$(dirname "$0")/.."

results="${1:-benchmarks/b2_call_graph_quality/results.json}"

if [ ! -f "$results" ]; then
    echo "check-b2-thresholds: $results missing -- run benchmarks/b2_call_graph_quality/run_benchmark.py first" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "check-b2-thresholds: jq not found -- install it first" >&2
    exit 1
fi

fail=0

check() {
    local label="$1" floor="$2" actual="$3"
    if [ "$actual" = "null" ]; then
        echo "check-b2-thresholds: $label missing from $results (expected a number)" >&2
        fail=1
        return
    fi
    if ! awk -v a="$actual" -v f="$floor" 'BEGIN { exit !(a+0 >= f+0) }'; then
        echo "check-b2-thresholds: $label = $actual, below floor $floor" >&2
        fail=1
    fi
}

check "overall recall" 0.15 "$(jq -r '.recall' "$results")"
check "overall precision" 0.70 "$(jq -r '.precision' "$results")"
check "inferred-tier precision" 0.85 "$(jq -r '.by_confidence.inferred.precision // "null"' "$results")"
check "resolved-tier precision" 0.80 "$(jq -r '.by_confidence.resolved.precision // "null"' "$results")"
check "textual-tier precision" 0.40 "$(jq -r '.by_confidence.textual.precision // "null"' "$results")"

if [ "$fail" -ne 0 ]; then
    echo "check-b2-thresholds: regression detected -- see floors and rationale in this script's header" >&2
    exit 1
fi
echo "check-b2-thresholds: all B2 metrics at or above their floor."
