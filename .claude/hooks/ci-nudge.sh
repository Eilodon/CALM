#!/usr/bin/env bash
# Project-scoped PreToolUse hook: nudge toward the "ci" (Code Intelligence)
# MCP server's own tools instead of native Read/Grep/Edit/Bash, mirroring
# AGENTS.md's workflow stages. Additive alongside any user-level hooks
# (Claude Code concatenates hooks across settings scopes; it does not let
# a project hook suppress a user-level one).
set -euo pipefail

input=$(cat)
tool_name=$(jq -r '.tool_name // ""' <<<"$input")
command=$(jq -r '.tool_input.command // ""' <<<"$input")

msg=""
case "$tool_name" in
  Read)
    msg='CI available in this repo — prefer mcp__ci__source(symbol) for a symbol-precise read, or mcp__ci__file_overview(path) instead of reading the whole file (AGENTS.md Stage 3).'
    ;;
  Grep)
    msg='CI available in this repo — prefer mcp__ci__search(query, kind="hybrid") or mcp__ci__locate(query) instead of Grep (AGENTS.md Stage 2).'
    ;;
  Edit|Write)
    msg='MANDATORY per AGENTS.md Stage 5 — call mcp__ci__edit_context(symbol) before this edit, never skip (especially if is_hub).'
    ;;
  Bash)
    if grep -qE '\b(grep|rg|ag)\b' <<<"$command"; then
      msg='CI available in this repo — prefer mcp__ci__search / mcp__ci__locate instead of grep via Bash (AGENTS.md Stage 2).'
    elif grep -qE '\bfind\b.*-i?name\b' <<<"$command"; then
      msg='CI available in this repo — prefer mcp__ci__file_overview / mcp__ci__dependencies instead of find (AGENTS.md Stage 1-2).'
    elif grep -qE '\bgit[[:space:]]+(commit|push)\b' <<<"$command"; then
      msg='MANDATORY per AGENTS.md Stage 7 — call mcp__ci__diff_impact(staged=true) before this commit/push, never skip.'
    fi
    ;;
esac

if [ -n "$msg" ]; then
  jq -n --arg msg "$msg" '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $msg}}'
fi
