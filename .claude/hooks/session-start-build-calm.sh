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
# `calm-mcp-entrypoint.sh` (.mcp.json's actual entrypoint, which only builds
# when the binary is *missing*), this always runs `cargo build`, so it also
# catches the binary being *stale* — e.g. mid-session edits to calm's own
# source in a prior session. Kept synchronous so that when it does win the
# race, the win is real (no async handoff for the MCP dial to slip past).
set -uo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  exit 0
fi

# Resolve any unresolved Git LFS pointer stubs (assets/potion-code-16m/* —
# the vendored embedding model — and .calm-bin/**/calm) BEFORE building. Real
# incident this guards against: a checkout without git-lfs installed leaves
# ~130-byte pointer text in place of real file content; `cargo build` still
# succeeds (it just bakes that pointer text into the binary via
# `include_bytes!`), and the failure only surfaces later, silently, as
# `embeddings_status: "failed"` at runtime — not a build error, so nothing
# here would have caught it otherwise. Every step is best-effort and
# non-fatal (`|| true`) — this hook must degrade, not break the session, and
# `Embedder::load`'s own runtime fallback (network download) plus
# `embeddings_status: "offline_unavailable"` messaging are the safety net if
# this doesn't fully resolve it (e.g. no apt, no network, git-lfs install
# blocked). See docs/cloud-environment-setup.md for the full picture.
if command -v git >/dev/null 2>&1 && grep -q 'filter=lfs' .gitattributes 2>/dev/null; then
  if ! git lfs version >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
    apt-get install -y git-lfs >/dev/null 2>&1 || true
  fi
  if git lfs version >/dev/null 2>&1; then
    git lfs pull >/dev/null 2>&1 || true
  fi
fi

build_output=$(cargo build --quiet -p calm-cli 2>&1)
build_status=$?

if [ "$build_status" -ne 0 ]; then
  jq -n --arg msg "calm-cli pre-build failed (exit $build_status) — the calm MCP server will likely fail to connect. Build output:
$build_output" \
    '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
fi
exit 0
