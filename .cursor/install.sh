#!/usr/bin/env bash
# Cursor Cloud/Background Agent "install" step (.cursor/environment.json).
# Cached after the first run per Cursor's own docs, so this is the closest
# equivalent Cursor has to Claude Code on the web's Setup Script — see
# docs/cloud-environment-setup.md for the full writeup of the race this
# guards against: `.cursor/mcp.json` points at `scripts/mcp-launcher.sh`,
# which only builds the calm-cli binary inline as a last resort. Without
# this, the very first Background Agent session on this repo hits a cold
# `cargo build -p calm-cli` (~59s measured) that the MCP client's initial
# dial attempt can lose, marking the "calm" server failed for that session.
# Prebuilding here means the launcher's fast path (already-built
# target/release/calm) wins instead.
set -uo pipefail

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Same defensive LFS-pull-before-build as the Claude Code Setup Script and
# `.claude/hooks/session-start-build-calm.sh` — a checkout without git-lfs
# leaves ~130-byte pointer stubs in place of the vendored embedding model
# and the prebuilt .calm-bin/ binary; the build still "succeeds" either way
# (include_bytes! just bakes whatever is on disk in), so this is worth
# getting right before building, not after.
if command -v git >/dev/null 2>&1 && grep -q 'filter=lfs' .gitattributes 2>/dev/null; then
  if ! git lfs version >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
    apt-get install -y git-lfs >/dev/null 2>&1 || true
  fi
  if git lfs version >/dev/null 2>&1; then
    git lfs pull >/dev/null 2>&1 || true
  fi
fi

# Release, not debug: unlike the Claude Code hook (which targets
# target/debug/calm to match a fast SessionStart-hook rebuild every
# session), this only runs once per cached Cursor environment snapshot, so
# the slower release build is worth it — it's also what
# `scripts/mcp-launcher.sh`'s fast path checks for first.
cargo build --release -p calm-cli 2>&1 || true
