#!/usr/bin/env bash
# SessionStart hook: pre-build the ci-cli binary synchronously so the "ci"
# stdio MCP server (.mcp.json: `cargo run --quiet -p ci-cli -- serve ...`)
# can finish its handshake inside the MCP client's fixed 30s connection
# timeout.
#
# On a fresh checkout (empty target/), `cargo build -p ci-cli` compiles the
# full dependency tree (tree-sitter grammars, rusqlite bundled, stack-graphs,
# embeddings) — measured ~60s even with a warm crates.io registry cache,
# over 2x the client's timeout, so `cargo run` in .mcp.json reliably times
# out on the very first connection of every fresh session/container.
# Once target/ is warm, `cargo run` reconnects in well under 1s (cargo's own
# freshness check + exec), so paying the compile cost here — before the
# session (and the MCP client's timer) starts — fixes it for the rest of
# the session. Must stay synchronous: async mode would let the MCP connect
# attempt race the build, which is the failure this hook exists to avoid.
set -uo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  exit 0
fi

build_output=$(cargo build --quiet -p ci-cli 2>&1)
build_status=$?

if [ "$build_status" -ne 0 ]; then
  jq -n --arg msg "ci-cli pre-build failed (exit $build_status) — the ci MCP server will likely fail to connect (30s client timeout). Build output:
$build_output" \
    '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
fi
exit 0
