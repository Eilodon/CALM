#!/usr/bin/env bash
# SessionStart hook: best-effort pre-build of the calm-cli binary.
#
# CORRECTION (2026-07-02): the previous version of this comment claimed a
# synchronous build here "fixes it for the rest of the session" — that is
# false, confirmed against code.claude.com/docs/en/mcp and
# .../claude-code-on-the-web. SessionStart hooks and MCP server connection
# attempts are NOT ordered: MCP startup is "non-blocking by default" and
# dials configured servers concurrently with hooks, not after them. A cold
# `cargo build -p calm-cli` (~59s measured: tree-sitter grammars + stack-graphs
# + bundled SQLite) can easily still be running when the "calm" server's
# initial connection attempt times out — which gets at most 3 quick retries
# (~7s total backoff, v2.1.121+) before Claude Code marks the server failed
# for the rest of the session, with no further retry. This hook can *win*
# that race, but cannot *guarantee* winning it — see
# docs/cloud-environment-setup.md for why the real fix is a Cloud
# environment Setup Script (runs once, before Claude Code launches at all,
# cached across sessions), not anything that runs from SessionStart.
#
# This hook is still worth keeping for two reasons: (1) it's the only
# mechanism available for local/non-cloud Claude Code, where there is no
# environment Setup Script concept at all; (2) unlike
# `mcp-launcher.sh` (.mcp.json's actual entrypoint as of 2026-07-12, after
# retiring the `calm-mcp-wrapper.sh` indirection layer — see its own
# freshness-checked target/release -> target/debug -> download -> build
# fallback), this always runs `cargo build`, so it also catches the binary
# being *stale* — e.g. mid-session edits to calm's own source in a prior
# session. Kept synchronous so that when it does win the race, the win is
# real (no async handoff for the MCP dial to slip past).
set -uo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  exit 0
fi

# UPDATE (2026-07-12): this repo no longer uses Git LFS for anything —
# both .calm-bin (see mcp-launcher.sh's tier 1.5, a GitHub Release download)
# and the vendored embedding model weights (assets/potion-code-16m/
# model.safetensors, see crates/calm-core/build.rs::ensure_embedding_weights,
# a HuggingFace Hub download checksum-verified at compile time) were
# migrated off LFS the same day, after LFS bandwidth exhaustion broke CI,
# this hook's own build, and a nightly workflow simultaneously — see
# docs/superskills/specs/2026-07-12-edge-release-binary-distribution.md.
# The `git lfs pull` step that used to live here is gone: build.rs now
# handles the embedding model fetch itself, before `cargo build` below ever
# needs it, with the same non-fatal degrade-to-placeholder behavior this
# hook's `|| true` philosophy already relies on everywhere else.

build_output=$(cargo build --quiet -p calm-cli 2>&1)
build_status=$?

if [ "$build_status" -ne 0 ]; then
  jq -n --arg msg "calm-cli pre-build failed (exit $build_status) — the calm MCP server will likely fail to connect. Build output:
$build_output" \
    '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
fi
# Best-effort, NON-blocking self-heal for target/release/calm (the actual
# .mcp.json entrypoint's fast path, via mcp-launcher.sh directly since
# 2026-07-12). Only fires when release is missing or older than the
# debug build just confirmed fresh above — i.e. genuinely stale, not on
# every session. Never awaited: a release build measured ~9min in this repo
# (vs. ~59s for debug) would blow this hook's own 300s timeout on nearly
# every run if done synchronously, defeating the whole "win the race"
# purpose above. mkdir is atomic, so N sessions starting around the same
# time queue at most one rebuild rather than N redundant ones (cargo itself
# also serializes concurrent builds against the same target dir, but the
# lock avoids piling up N sleeping cargo processes doing nothing).
#
# Real incident this guards against (2026-07-12): a manual
# `cargo clean -p calm-server` removed target/release/calm entirely, and
# nothing rebuilt it afterward — the old calm-mcp-wrapper.sh's hardcoded
# `exec` had no fallback, so every new MCP session failed outright until
# someone noticed and ran `cargo build --release` by hand.
# `mcp-launcher.sh` (now .mcp.json's direct entrypoint, the wrapper having
# been retired as redundant indirection) falls back to target/debug/calm
# when release is missing, so a session is never fully broken by this
# again — but without this block, it would stay on the slower debug binary
# forever instead of self-healing back to release.
release_bin="target/release/calm"
debug_bin="target/debug/calm"
if [ -x "$debug_bin" ] && { [ ! -x "$release_bin" ] || [ "$debug_bin" -nt "$release_bin" ]; }; then
  mkdir -p .calm 2>/dev/null || true
  lock_dir=".calm/release-build.lock"
  if mkdir "$lock_dir" 2>/dev/null; then
    (
      trap 'rmdir "'"$lock_dir"'" 2>/dev/null' EXIT
      cargo build --release --quiet -p calm-cli --features embeddings,tier0-5,scip-overlay \
        >".calm/release-build.log" 2>&1
    ) >/dev/null 2>&1 &
    disown 2>/dev/null || true
  fi
fi

# Best-effort, NON-blocking install of rust-analyzer (audit-design 2026-07-20:
# confirmed live that CALM's own Rust SCIP overlay needs nothing else to
# start firing automatically -- RustConfig::default() is already
# `enabled: None` (auto-detect: silently no-ops if the binary is absent,
# turns itself on the moment it's found) + `policy: OnSave` (every reindex
# that touches a .rs file re-runs it, no explicit scip_refresh call needed).
# The ONLY missing ingredient in an environment where `caller_count` stays
# textual-only for Rust is the binary itself not being on PATH yet -- this
# closes that gap the same way the release-build self-heal above does: a
# backgrounded, `|| true`-tolerant install that never blocks the race this
# hook exists to win. `rustup which` (not `command -v`) matches
# `scip::runner::resolve_binary`'s own first PATH-then-rustup probe order,
# so this only installs when BOTH would otherwise come up empty.
if command -v rustup >/dev/null 2>&1 && ! rustup which rust-analyzer >/dev/null 2>&1; then
  (rustup component add rust-analyzer >/dev/null 2>&1 || true) &
  disown 2>/dev/null || true
fi

exit 0
