#!/usr/bin/env bash
# CALM MCP — generic Claude Code PreToolUse/PostToolUse hook.
#
# Scaffolded by `calm init --hooks[=nudge|enforce|off]` into any project
# using CALM MCP. This file is embedded in the `calm` binary
# (`crates/calm-core/src/hooks.rs`'s `HOOKS_SCRIPT` const, via
# `include_str!` on this exact file) and written out verbatim — editing a
# copy in a scaffolded project has no effect on future `calm init --hooks`
# runs, which always rewrite from the embedded version.
#
# WHAT THIS DOES: nudges (or, in `enforce` mode, blocks) two things CALM's
# own tool workflow considers important — calling `edit_context` before a
# native `Edit`/`Write` touches a file CALM tracks, and calling
# `diff_impact` before `git commit`/`git push` after any write. See the
# `calm_workflow` MCP Prompt (call it with no arguments on any CALM-backed
# MCP client) for the full tool workflow this is nudging toward — this
# script deliberately never references a specific document's stage
# numbers, since none is guaranteed to exist in the project it's scaffolded
# into.
#
# WHAT THIS IS NOT: a security boundary. `enforce` mode raises the bar
# against ACCIDENTALLY skipping edit_context/diff_impact — it is not a
# sandbox and cannot constrain an actively evading agent or process. Concretely:
#   - Claude Code's own hook-dispatch mechanism has a documented, multi-
#     version reliability history (anthropics/claude-code#4669, #39344) —
#     Anthropic's own docs recommend the static permission system over
#     hooks for hard enforcement.
#   - The mode this script reads (`.calm/hooks.mode`) is an ordinary file
#     under a dotdir CALM deliberately never gates with edit_context — ANY
#     process with normal write access to this repo (including the very
#     agent this script means to nudge) can disable `enforce` with one
#     write to that file, by deleting this script, or by editing
#     `.claude/settings.json`. This is true of every Claude Code hook, not
#     specific to CALM's implementation.
# Use `enforce` to catch honest mistakes. Do not rely on it for anything
# more than that. See docs/superskills/specs/2026-07-15-calm-hooks-
# transparent-reactivation.md (in the CALM project itself) for the full
# design rationale and the failure modes this framing responds to.
#
# TOGGLING: `calm init --hooks=nudge|enforce|off` changes mode (rewrites
# `.calm/hooks.mode`, no need to touch this script or settings.json).
# `CALM_HOOKS_DISABLE=1` in the environment disables this script entirely
# for one shell/session, no file changes needed. `calm doctor` reports the
# current, actually-active state (cross-checked against real
# `.claude/settings.json` wiring, not just this mode file).
set -uo pipefail

# Read + drain stdin FIRST, before any branch that might exit early (the
# CALM_HOOKS_DISABLE/off-mode short-circuits right below both used to skip
# this, which left the upstream `jq ... | bash calm-hooks.sh` producer
# writing to a reader that already closed its stdin without consuming it —
# a real SIGPIPE (exit 141), caught by this script's own test suite, not
# guessed). Every exit path below now sees a fully-drained stdin.
input=$(cat)
tool_name=$(jq -r '.tool_name // ""' <<<"$input")
command=$(jq -r '.tool_input.command // ""' <<<"$input")
file_path=$(jq -r '.tool_input.file_path // ""' <<<"$input")
session_id=$(jq -r '.session_id // "unknown"' <<<"$input")
hook_event=$(jq -r '.hook_event_name // ""' <<<"$input")

# ---------------------------------------------------------------------------
# Mode: read_hooks_mode's contract (FM1 — see the spec above) — this
# function's stdout is ALWAYS exactly one of nudge|enforce|off, never
# anything else, and it never lets a malformed/unrecognized/future-version
# mode value escalate silently to "enforce". Any ambiguity resolves toward
# "nudge"; a missing file resolves toward "off" (never installed).
# ---------------------------------------------------------------------------
CALM_DIR="${CALM_DIR:-.calm}"
HOOKS_MODE_FILE="$CALM_DIR/hooks.mode"
HOOKS_MODE_SCHEMA="1"

read_hooks_mode() {
  local f="$HOOKS_MODE_FILE"
  [ -f "$f" ] || { echo "off"; return; }
  local schema mode
  schema=$(grep -m1 '^schema=' "$f" 2>/dev/null | cut -d= -f2 | tr -d '[:space:]')
  mode=$(grep -m1 '^mode=' "$f" 2>/dev/null | cut -d= -f2 | tr -d '[:space:]')
  if [ "$schema" != "$HOOKS_MODE_SCHEMA" ]; then
    echo "nudge"
    return
  fi
  case "$mode" in
    nudge|enforce|off) echo "$mode" ;;
    *) echo "nudge" ;;
  esac
}

# CALM_HOOKS_DISABLE — temporary, no-file-edit off switch (see header).
if [ "${CALM_HOOKS_DISABLE:-0}" = "1" ]; then
  exit 0
fi

mode=$(read_hooks_mode)
if [ "$mode" = "off" ]; then
  exit 0
fi
MODE_TAG_ENFORCE="[CALM hooks: enforce — best-effort, not a security boundary]"
MODE_TAG_NUDGE="[CALM hooks: nudge — advisory only, never blocks]"
mode_tag() { [ "$mode" = "enforce" ] && echo "$MODE_TAG_ENFORCE" || echo "$MODE_TAG_NUDGE"; }

# ---------------------------------------------------------------------------
# Session state — mirrors the internal calm-nudge.sh pattern by subtraction:
# same lock-protected read-modify-write shape (including the `{ ...; }
# 2>/dev/null` redirect-scoping fix a bare `exec ... 2>/dev/null` needs —
# see calm-nudge.sh's own history for why), same per-session file. Adds one
# field beyond the internal version: last_seen_mode, for the FM3 downgrade-
# notice mitigation below.
# ---------------------------------------------------------------------------
state_dir="${CALM_HOOKS_STATE_DIR:-$CALM_DIR/.hooks-state}"
mkdir -p "$state_dir" 2>/dev/null || true
state_file="$state_dir/${session_id}.json"

acquire_state_lock() {
  { exec {STATE_LOCK_FD}>"${state_file}.lock"; } 2>/dev/null || { STATE_LOCK_FD=""; return; }
  flock -w 2 "$STATE_LOCK_FD" 2>/dev/null || true
}
release_state_lock() {
  [ -n "${STATE_LOCK_FD:-}" ] || return 0
  flock -u "$STATE_LOCK_FD" 2>/dev/null || true
  { exec {STATE_LOCK_FD}>&-; } 2>/dev/null || true
  STATE_LOCK_FD=""
}

state='{}'
[ -f "$state_file" ] && state=$(cat "$state_file" 2>/dev/null || echo '{}')
edit_context_files=$(jq -c '.edit_context_files // []' <<<"$state" 2>/dev/null || echo '[]')
needs_diff_impact=$(jq -r '.needs_diff_impact // false' <<<"$state" 2>/dev/null || echo false)
last_seen_mode=$(jq -r '.last_seen_mode // ""' <<<"$state" 2>/dev/null || echo "")

save_state() {
  acquire_state_lock
  local prev='{}'
  [ -f "$state_file" ] && prev=$(cat "$state_file" 2>/dev/null || echo '{}')
  jq -n --argjson prev "$prev" --argjson ecf "$1" --argjson nd "$2" --arg lsm "$3" \
    '$prev + {edit_context_files: $ecf, needs_diff_impact: $nd, last_seen_mode: $lsm}' \
    >"$state_file" 2>/dev/null || true
  release_state_lock
}

# ---------------------------------------------------------------------------
# FM3 mitigation 2 — tamper-evident mode downgrade. A downgrade can't be
# prevented (see header), but it is never silent: it's logged durably to
# .calm/audit.log and surfaced as a loud, one-time notice on the very next
# hook invocation after it happens.
# ---------------------------------------------------------------------------
if [ -n "$last_seen_mode" ] && [ "$last_seen_mode" != "$mode" ]; then
  downgrade=false
  case "$last_seen_mode:$mode" in
    enforce:nudge|enforce:off|nudge:off) downgrade=true ;;
  esac
  if [ "$downgrade" = "true" ]; then
    audit_log="$CALM_DIR/audit.log"
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "")
    printf '%s\n' "$(jq -nc --arg from "$last_seen_mode" --arg to "$mode" --arg at "$ts" \
      '{event:"hooks_mode_downgraded", from:$from, to:$to, at:$at}')" >>"$audit_log" 2>/dev/null || true
    echo "NOTICE: CALM hooks mode changed from $last_seen_mode to $mode since the last tool call in this session. If this wasn't intentional, run \`calm init --hooks=$last_seen_mode\` to restore. This notice cannot be suppressed and is also recorded in .calm/audit.log." >&2
  fi
fi

to_repo_relative() {
  local p="$1" root
  case "$p" in
    /*)
      root=$(git rev-parse --show-toplevel 2>/dev/null)
      if [ -n "$root" ] && [ "${p#"$root"/}" != "$p" ]; then
        printf '%s' "${p#"$root"/}"
      else
        printf '%s' "$p"
      fi
      ;;
    *) printf '%s' "$p" ;;
  esac
}

is_prose_file() {
  case "$1" in
    *.md|*.MD|*.txt|*.TXT) return 0 ;;
    *) return 1 ;;
  esac
}

file_has_edit_context() {
  local p; p=$(to_repo_relative "$1")
  jq -e --arg p "$p" 'index($p) != null' <<<"$edit_context_files" >/dev/null 2>&1
}

# deny(): exit 2 + stderr, the primitive Claude Code mechanism (not the
# JSON permissionDecision form — see anthropics/claude-code#4669/#39344,
# and calm-nudge.sh's own header for the full external-research citation
# this choice is based on). Never called outside mode=enforce.
deny() {
  echo "$(mode_tag) $1" >&2
  exit 2
}

nudge() {
  echo "$(mode_tag) $1" >&2
}

EDIT_CONTEXT_POINTER="Call the \`calm_workflow\` MCP prompt (no arguments) for the full CALM tool workflow."

# Persist last_seen_mode=$mode unconditionally, on every single invocation,
# BEFORE the tool_name dispatch below -- this is what makes the downgrade
# notice above fire exactly once per actual downgrade instead of repeating
# forever: by the next invocation, last_seen_mode already matches $mode, so
# the comparison stops tripping. edit_context_files/needs_diff_impact are
# passed through unchanged here; branches below that need to change them
# call save_state again with the updated values.
save_state "$edit_context_files" "$needs_diff_impact" "$mode"

case "$tool_name" in
  mcp__calm__edit_context)
    ec_path=$(jq -r '.tool_input.path // ""' <<<"$input")
    ec_symbol=$(jq -r '.tool_input.symbol // ""' <<<"$input")
    [ -n "$ec_path" ] && ec_path=$(to_repo_relative "$ec_path")
    if [ -z "$ec_path" ] && [ -n "$ec_symbol" ]; then
      db="$CALM_DIR/index.db"
      if [ -f "$db" ] && command -v sqlite3 >/dev/null 2>&1; then
        escaped=${ec_symbol//\'/\'\'}
        rows=$(sqlite3 -readonly -separator '|' "$db" \
          "SELECT path FROM symbols WHERE name = '$escaped';" 2>/dev/null || true)
        row_count=$(printf '%s\n' "$rows" | grep -c . 2>/dev/null || echo 0)
        if [ "${row_count:-0}" -eq 1 ] 2>/dev/null; then
          ec_path=$(to_repo_relative "$rows")
        fi
      fi
    fi
    if [ -n "$ec_path" ]; then
      edit_context_files=$(jq -c --arg p "$ec_path" '. + [$p] | unique' <<<"$edit_context_files")
    fi
    save_state "$edit_context_files" "$needs_diff_impact" "$mode"
    exit 0
    ;;
  mcp__calm__diff_impact)
    save_state "$edit_context_files" false "$mode"
    exit 0
    ;;
  mcp__calm__edit_lines|mcp__calm__edit_symbol)
    save_state "$edit_context_files" true "$mode"
    exit 0
    ;;
  Edit|Write)
    [ -n "$file_path" ] || exit 0
    if is_prose_file "$file_path"; then
      # A doc heading is provably never is_hub (no call-graph edge exists
      # for it) — always advisory here regardless of mode, matching the
      # internal tool's own documented exception.
      if ! file_has_edit_context "$file_path"; then
        nudge "RECOMMENDED — call mcp__calm__edit_context before editing $file_path. Not required for prose (.md/.txt never carries a call-graph edge). $EDIT_CONTEXT_POINTER"
      fi
      save_state "$edit_context_files" true "$mode"
      exit 0
    fi
    if ! file_has_edit_context "$file_path"; then
      msg="edit_context has not been called for $file_path this session. $EDIT_CONTEXT_POINTER Prefer mcp__calm__edit_lines/edit_symbol over native Edit/Write for any file CALM tracks — hash-verified, risk-gated, reindexes immediately."
      if [ "$mode" = "enforce" ]; then
        deny "$msg"
      else
        nudge "$msg"
      fi
    fi
    save_state "$edit_context_files" true "$mode"
    exit 0
    ;;
  Bash)
    # Simplified from the internal calm-nudge.sh's git-commit detection —
    # this generic version matches `git commit`/`git push` as a word-
    # bounded substring of the command, without that version's additional
    # repo-target-root resolution (mirrors the internal tool's stricter
    # behavior only partially; documented reduction, not an oversight —
    # see the spec this script's header points at).
    if grep -qE '(^|[;&|]|\s)git\s+(commit|push)(\s|$)' <<<"$command"; then
      if [ "$needs_diff_impact" = "true" ]; then
        msg="A tracked file changed since the last diff_impact call. Call mcp__calm__diff_impact before committing/pushing. $EDIT_CONTEXT_POINTER"
        if [ "$mode" = "enforce" ]; then
          deny "$msg"
        else
          nudge "$msg"
        fi
      fi
    fi
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
