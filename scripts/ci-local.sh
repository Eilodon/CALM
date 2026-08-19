#!/usr/bin/env bash
#
# ci-local.sh — run the SAME blocking checks CI runs, locally, in one command.
#
# WHY THIS EXISTS
#   CI (.github/workflows/ci.yml) is split into ~11 parallel jobs so it stays
#   fast on GitHub's runners. That parallelism is good for CI but bad for a
#   human: before a push there was no single command that reproduced every
#   blocking gate, so a "green locally" push could still fail CI on a check
#   the dev never ran (real example: 0a7a8aa passed a local `cargo test` but
#   failed CI on fmt + clippy(type_complexity) + doc-truth, taking three
#   pushes — 0a7a8aa -> b1fd2a6 -> 903d3ef — to go green). This script is the
#   fix: ONE command, run every gate, report ALL failures at once instead of
#   one-per-push.
#
#   It is intentionally NOT wired into ci.yml. CI keeps its parallel-job
#   structure (different runners, feature sets, and rust-cache scopes per job);
#   forcing every job through one serial script would lose that. Instead this
#   script mirrors those jobs and the mapping is kept explicit below, so if a
#   CI job is added/changed, the drift is visible here in review.
#
# CI JOB MAPPING (ci.yml -> phase here)
#   verify (fmt/clippy/test/audit) ...... LINT + TEST (+ audit in --full)
#   embeddings .......................... LINT (clippy) + TEST_MATRIX
#   no-stack-graphs-formal .............. LINT (clippy + ts-unpinned) + TEST_MATRIX
#   all-languages ....................... LINT (clippy) + TEST_MATRIX
#   otel-http-features .................. LINT (clippy) + TEST_MATRIX (+ skew guard)
#   stack-graphs-corpus ................. TEST_MATRIX
#   status-drift ........................ DOCS
#   fitness-check ....................... HEAVY (--full)
#   txn-crash-injection ................. HEAVY (--full)
#   js-client-interop ................... HEAVY (--full, needs npm)
#   scip-nightly: check-b2-thresholds ... HEAVY (--full)
#   calm-guard-dogfood .................. NOT MIRRORED (continue-on-error shadow job)
#
# USAGE
#   scripts/ci-local.sh            # default: LINT + DOCS + TEST (fast, offline)
#   scripts/ci-local.sh --lint     # LINT + DOCS only (fastest pre-push gate)
#   scripts/ci-local.sh --full     # everything, == full CI (slow; some steps need network/npm)
#
# Runs every selected check even if an earlier one fails (does NOT stop on
# first error), then prints a summary and exits non-zero if any failed.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"

MODE="default"
case "${1:-}" in
  --lint) MODE="lint" ;;
  --full) MODE="full" ;;
  ""|--default) MODE="default" ;;
  -h|--help)
    sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
    exit 0 ;;
  *)
    echo "unknown argument: $1 (use --lint | --default | --full | --help)" >&2
    exit 2 ;;
esac

# ---- result collection --------------------------------------------------
PASS=(); FAIL=()
run_check() {
  local label="$1"; shift
  printf '\n\033[1;34m==> %s\033[0m\n' "$label"
  printf '    $ %s\n' "$*"
  if "$@"; then
    PASS+=("$label")
  else
    FAIL+=("$label")
    printf '\033[1;31m    FAILED: %s\033[0m\n' "$label"
  fi
}

# The 11-language + lsp feature string the all-languages CI job uses, kept in
# one place so it can't drift between the clippy and test invocations.
ALL_LANG_FEATURES="tier0-5,lang-kotlin,lang-swift,lang-scala,lang-dart,lang-lua,lang-elixir,lang-haskell,lang-ocaml,lang-zig,lang-powershell,lang-groovy,scip-overlay,lsp-overlay"
NO_SG_FEATURES="embeddings,tier0-5,scip-overlay"

# Verify tree-sitter core is not pulled in transitively by the no-default
# build (mirrors ci.yml no-stack-graphs-formal's "unpinned" guard).
check_tree_sitter_unpinned() {
  local leaked
  leaked=$(cargo tree -p calm-core --no-default-features --features "$NO_SG_FEATURES" 2>&1 | grep -ic "stack-graph" || true)
  if [ "$leaked" -ne 0 ]; then
    echo "stack-graph* leaked into the no-stack-graphs-formal build ($leaked line(s))" >&2
    return 1
  fi
  echo "ok: no stack-graph* in no-default build"
}

# Guard against opentelemetry core version skew (mirrors otel-http-features).
check_otel_skew() {
  local versions
  versions=$(cargo metadata --format-version 1 --features otel 2>/dev/null \
    | jq -r '.packages[] | select(.name == "opentelemetry") | .version' | sort -u)
  local n
  n=$(printf '%s\n' "$versions" | grep -c . || true)
  if [ "$n" -gt 1 ]; then
    echo "opentelemetry appears at >1 version (skew): $versions" >&2
    return 1
  fi
  echo "ok: opentelemetry single version: ${versions:-<none>}"
}

# ---- LINT (always) ------------------------------------------------------
run_check "fmt --check"                 cargo fmt --all -- --check
run_check "clippy (workspace)"          cargo clippy --workspace --all-targets -- -D warnings
run_check "clippy (embeddings)"         cargo clippy -p calm-core --all-targets --features embeddings -- -D warnings
run_check "clippy (no-stack-graphs)"    cargo clippy -p calm-core --all-targets --no-default-features --features "$NO_SG_FEATURES" -- -D warnings
run_check "clippy (all-languages+lsp)"  cargo clippy --workspace --all-targets --features "$ALL_LANG_FEATURES" -- -D warnings
run_check "clippy (otel)"               cargo clippy -p calm-cli --features otel -- -D warnings
run_check "clippy (http)"               cargo clippy -p calm-server -p calm-cli --features http -- -D warnings
run_check "tree-sitter unpinned guard"  check_tree_sitter_unpinned

# ---- DOCS (always) ------------------------------------------------------
run_check "status.generated.md fresh"   ./scripts/gen-status.sh --check
run_check "hand-authored doc truth"     ./scripts/check-doc-truth.sh
run_check "claims registry consistent"  ./scripts/check-claims-registry.sh

# ---- TEST (default + full) ---------------------------------------------
if [ "$MODE" != "lint" ]; then
  run_check "test (workspace)"          cargo test --workspace
fi

# ---- TEST_MATRIX + HEAVY (full only) -----------------------------------
if [ "$MODE" = "full" ]; then
  run_check "test (embeddings)"         cargo test -p calm-core --features embeddings
  run_check "test (no-stack-graphs)"    cargo test -p calm-core --no-default-features --features "$NO_SG_FEATURES"
  run_check "test (all-languages+lsp)"  cargo test --workspace --features "$ALL_LANG_FEATURES"
  run_check "test (otel)"               cargo test -p calm-cli --features otel
  run_check "otel version-skew guard"   check_otel_skew
  run_check "test (http)"               cargo test -p calm-server -p calm-cli --features http
  run_check "stack-graphs corpus"       cargo test --test parity_test test_formal_edges -- --nocapture
  run_check "b2 thresholds"             ./scripts/check-b2-thresholds.sh
  run_check "adr staleness"             ./scripts/check-adr-staleness.sh
  # Networked / heaviest — only in --full, and tolerated-missing where a tool
  # isn't installed rather than silently skipped.
  if command -v cargo-audit >/dev/null 2>&1; then
    run_check "cargo audit"             cargo audit
  else
    echo "note: cargo-audit not installed; run 'cargo install cargo-audit --locked' to include it" >&2
  fi
  run_check "fitness-check (build+index)" bash -c '
    cargo build --bin calm &&
    ./target/debug/calm index --project-root . &&
    ./target/debug/calm fitness-check --project-root . --config thresholds.toml'
  run_check "txn crash injection"       cargo test -p calm-cli --test txn_crash_injection -- --ignored
fi

# ---- summary ------------------------------------------------------------
printf '\n\033[1m================ ci-local summary (mode: %s) ================\033[0m\n' "$MODE"
for p in "${PASS[@]:-}"; do [ -n "$p" ] && printf '  \033[1;32mPASS\033[0m  %s\n' "$p"; done
for f in "${FAIL[@]:-}"; do [ -n "$f" ] && printf '  \033[1;31mFAIL\033[0m  %s\n' "$f"; done
printf '\033[1m%s passed, %s failed\033[0m\n' "${#PASS[@]}" "${#FAIL[@]}"

if [ "${#FAIL[@]}" -ne 0 ]; then
  exit 1
fi
echo "all selected CI checks passed."
