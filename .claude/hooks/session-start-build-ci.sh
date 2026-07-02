#!/usr/bin/env bash
# SessionStart hook: best-effort pre-build of the ci-cli binary.
#
# CORRECTION (2026-07-02): the previous version of this comment claimed a
# synchronous build here "fixes it for the rest of the session" — that is
# false, confirmed against code.claude.com/docs/en/mcp and
# .../claude-code-on-the-web. SessionStart hooks and MCP server connection
# attempts are NOT ordered: MCP startup is "non-blocking by default" and
# dials configured servers concurrently with hooks, not after them. A cold
# `cargo build -p ci-cli` (~59s measured: tree-sitter grammars + stack-graphs
# + bundled SQLite) can easily still be running when the "ci" server's
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
# `ci-mcp-entrypoint.sh` (.mcp.json's actual entrypoint, which only builds
# when the binary is *missing*), this always runs `cargo build`, so it also
# catches the binary being *stale* — e.g. mid-session edits to ci's own
# source in a prior session. Kept synchronous so that when it does win the
# race, the win is real (no async handoff for the MCP dial to slip past).
set -uo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  exit 0
fi

build_output=$(cargo build --quiet -p ci-cli 2>&1)
build_status=$?

if [ "$build_status" -ne 0 ]; then
  jq -n --arg msg "ci-cli pre-build failed (exit $build_status) — the ci MCP server will likely fail to connect. Build output:
$build_output" \
    '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
fi
exit 0
