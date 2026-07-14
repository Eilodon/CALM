#!/usr/bin/env bash
# SessionStart hook: inject AGENTS.md into the model's context automatically.
# Claude Code auto-loads CLAUDE.md but not AGENTS.md (a different convention),
# so without this the workflow guide only reaches the model if it happens to
# Read the file on its own.
#
# F1 (2026-07-14 audit-design, docs/superskills/specs/2026-07-14-calm-
# agent-experience-round2-fixes.md): SessionStart was re-injecting the full
# ~17KB of AGENTS.md on EVERY conversation turn, not once per logical
# session — confirmed live by capturing a real payload
# (.calm/.hook-state/sessionstart-dump.jsonl, now deleted along with its
# settings.json entry): this harness reconnects to the MCP server every
# turn, and Claude Code's SessionStart matcher "*" fires again on that
# reconnect with `source: "resume"`, same `session_id` as the original
# `source: "startup"`. The audit's own pre-mortem caught a real bug in the
# first draft of this fix: deduping on `session_id` ALONE would also
# wrongly suppress the full inject right after a `/clear` or `/compact` —
# those legitimately need the full guide back (context was just wiped),
# and Claude Code does not appear to mint a new session_id for either (only
# `startup` does, per the captured payload). So the dedup key here is
# `(session_id already seen, AND source == "resume")` specifically — a
# `source` value that is anything else (`startup`, `clear`, `compact`, or
# an unrecognized future value) always gets the full inject, fail-safe
# toward "re-inject", never toward silence.
set -uo pipefail

input=$(cat)
session_id=$(jq -r '.session_id // ""' <<<"$input" 2>/dev/null)
source_kind=$(jq -r '.source // ""' <<<"$input" 2>/dev/null)

seen_dir=".calm/.hook-state/sessionstart-seen"
mkdir -p "$seen_dir" 2>/dev/null || true
seen_marker="$seen_dir/${session_id:-unknown}"

if [ -n "$session_id" ] && [ "$source_kind" = "resume" ] && [ -f "$seen_marker" ]; then
  # Repeat SessionStart for a session already fully oriented this lineage —
  # a short pointer instead of the full guide. Never fully silent: the full
  # detail is one call away (calm-guide Skill / calm_workflow MCP Prompt,
  # F6 — same content, added the same day this fix landed), and this
  # banner is shown every time, not just once, so it survives a context
  # compaction that might have dropped the original full injection or an
  # earlier banner (the abductive risk the audit flagged for F1+F6 together).
  jq -n '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: "CALM MCP is active for this repo (reconnected — AGENTS.md already injected earlier this session). Stage 5 (`edit_context` before edit) and Stage 7 (`diff_impact` before commit/push) are hook-enforced, no exceptions for real code. Full Stage 1-8 workflow on demand: invoke the `calm-guide` Skill, or read `AGENTS.md` at the project root directly."}}'
  exit 0
fi

if [ -f AGENTS.md ]; then
  content=$(cat AGENTS.md)
  touch "$seen_marker" 2>/dev/null || true
  jq -n --arg msg "$content" '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
fi
