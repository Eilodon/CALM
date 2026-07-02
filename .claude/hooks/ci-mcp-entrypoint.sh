#!/usr/bin/env bash
# MCP stdio entrypoint for the "ci" server (see .mcp.json). Ensures the
# ci-cli binary exists, then execs it directly — instead of going through
# `cargo run` on every single connection attempt.
#
# Why this exists (full story: docs/cloud-environment-setup.md):
# Claude Code dials configured MCP servers asynchronously, with NO ordering
# guarantee relative to SessionStart hooks completing (confirmed against
# code.claude.com/docs/en/mcp + .../claude-code-on-the-web: MCP startup is
# "non-blocking by default", and SessionStart hooks "typically fire before
# servers finish connecting"). A failed initial connection gets at most 3
# quick retries (~7s of total backoff, v2.1.121+) and is then marked failed
# for the rest of the session — nowhere near enough to cover a cold
# `cargo build` of this workspace (~59s measured: tree-sitter grammars +
# stack-graphs + bundled SQLite). A SessionStart hook that pre-builds the
# binary (session-start-build-ci.sh) can win that race but is NOT
# guaranteed to — it runs concurrently with the connection attempt, not
# strictly before it.
#
# Routing every connection through `cargo run` made this worse than it had
# to be: every connect paid cargo's own freshness-check overhead, and any
# connect could be silently upgraded into a full rebuild if cargo decided
# one was needed — reintroducing the exact race this file exists to avoid.
#
# This script removes that variability: if a binary is already on disk,
# exec it immediately, no cargo involved at all. It only builds here if the
# binary is missing outright — a real fallback for local/non-cloud use
# (no environment Setup Script exists there), NOT a substitute for one in
# cloud sessions. The actual fix for the cold-start race is a Cloud
# environment Setup Script that builds this binary once, before Claude Code
# (and its MCP dialing) ever launches — see docs/cloud-environment-setup.md
# for the exact script to paste into the environment settings UI. That
# config lives outside this repo (Anthropic-side, not git-tracked), which
# is exactly why it needs to be written down somewhere a future reader of
# this repo can find it.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

BIN="target/debug/ci"

if [ ! -x "$BIN" ]; then
  # Build output goes to stderr only — stdout is the MCP JSON-RPC channel,
  # and any stray text there would corrupt the handshake.
  cargo build --quiet -p ci-cli 1>&2
fi

exec "$BIN" serve --project-root . "$@"
