#!/usr/bin/env bash
# Regenerates lcov.info at the repo root so this project's own
# dead-code-confidence analysis (crates/calm-core/src/analysis/dead_code.rs's
# CoverageData plumbing) has real runtime coverage to work with instead of
# silently falling back to CoverageData::none() — see
# crates/calm-core/src/analysis/coverage.rs's COVERAGE_SEARCH_PATHS.
#
# Output is gitignored (already covered by the repo's .gitignore) — this is
# a point-in-time local artifact to regenerate on demand, not something to
# commit and let go stale.
#
# Usage:
#   scripts/gen-coverage.sh
#
# Requires: cargo-llvm-cov (cargo install cargo-llvm-cov --locked)
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "cargo-llvm-cov not found — install with: cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi

cargo llvm-cov --workspace --lcov --output-path lcov.info
echo "Wrote lcov.info ($(wc -l < lcov.info) lines)"
