#!/usr/bin/env bash
# SessionStart hook: inject AGENTS.md into the model's context automatically.
# Claude Code auto-loads CLAUDE.md but not AGENTS.md (a different convention),
# so without this the workflow guide only reaches the model if it happens to
# Read the file on its own.
set -euo pipefail

if [ -f AGENTS.md ]; then
  content=$(cat AGENTS.md)
  jq -n --arg msg "$content" '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
fi
