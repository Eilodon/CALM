#!/usr/bin/env bash
# Wrapper script to redirect calm logs to file, keeping only JSON-RPC on stdout.
#
# Dogfooding the daemon (ADR-0005, v1/M6, 2026-07-10): originally forced
# `calm connect` here directly via a hardcoded
# `exec .../target/release/calm connect ...` with zero fallback.
#
# CORRECTION (2026-07-12): that hardcoding caused two separate real outages
# from the same root cause — an absolute path baked into a file tracked in
# git, which is wrong the instant it's read in any checkout other than the
# one it was written on:
#   1. A prior `cargo clean -p calm-server` removed target/release/calm, and
#      this wrapper's bare `exec` failed instantly with "No such file or
#      directory", killing the MCP connection even though target/debug/calm
#      (rebuilt every session by .claude/hooks/session-start-build-calm.sh)
#      was sitting right there unused.
#   2. A separate Claude Code session running against this same repo cloned
#      at a *different* absolute path (/home/user/CALM, some ephemeral/cloud
#      sandbox) hit its own instance of this same problem, "fixed" it by
#      hardcoding *its* path into this file and .mcp.json, and pushed that
#      as commit 6e30bcd (merged via PR #27) — which broke every other
#      checkout (including /home/ybao/B.1/CALM) the moment it was pulled,
#      since /home/user/CALM doesn't exist there. Whichever environment
#      commits last wins and breaks everyone else — a hardcoded absolute
#      path has no business being checked into a file every clone shares.
# `scripts/mcp-launcher.sh` has since grown its own `calm connect` default
# (2026-07-11, see docs/mcp-client-setup.md) plus the proper 3-tier binary
# resolution this wrapper never had (target/release -> target/debug ->
# verified download -> build-from-source, all freshness-checked), and it
# resolves its own project root from `$(dirname "${BASH_SOURCE[0]}")` /
# cwd rather than a baked-in literal — so this now delegates to it instead
# of duplicating (and re-breaking, per-environment) that logic. Kept as a
# thin indirection only for the stderr-to-file redirect below (PID-suffixed
# so concurrent sessions don't clobber one shared log file — the old fixed
# /tmp/calm-mcp.log path was itself a latent bug). Do NOT reintroduce an
# absolute path here or in .mcp.json's `command` field for the same reason.
exec bash "$(dirname "${BASH_SOURCE[0]}")/mcp-launcher.sh" 2>"/tmp/calm-mcp.$$.log"
