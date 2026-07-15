#!/usr/bin/env bash
# Regression test for the generic calm-hooks.sh (scaffolded by
# `calm init --hooks`) — plain bash, mirrors this repo's own
# .claude/hooks/test-calm-nudge.sh conventions (no test framework exists
# for these shell hooks). Run directly: prints PASS lines or exits
# non-zero with a FAIL message naming which assertion broke.
set -euo pipefail
cd "$(dirname "$0")"
script="$(pwd)/calm-hooks.sh"

work=$(mktemp -d)
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

export CALM_DIR="$work/.calm"
export CALM_HOOKS_STATE_DIR="$work/.calm/.hooks-state"
mkdir -p "$CALM_DIR"

set_mode() {
  # $1 = mode (nudge|enforce|off), $2 = schema override (default 1)
  printf 'schema=%s\nmode=%s\nwritten_by=test\nwritten_at=0\n' "${2:-1}" "$1" >"$CALM_DIR/hooks.mode"
}

session_id_test="test-$$"
run_hook() {
  # $1=tool_name  $2=file_path/path  $3=symbol(edit_context)  $4=command(Bash)
  jq -nc --arg session "$session_id_test" --arg tool "$1" \
    --arg path "${2:-}" --arg symbol "${3:-}" --arg command "${4:-}" \
    '{session_id: $session, tool_name: $tool, hook_event_name: "PreToolUse",
      tool_input: {file_path: $path, path: $path, symbol: $symbol, command: $command}}' \
    | bash "$script"
}

fail() { echo "FAIL: $1"; exit 1; }

capture() {
  local errfile; errfile=$(mktemp)
  if out=$("$@" 2>"$errfile"); then hook_ec=0; else hook_ec=$?; fi
  hook_err=$(cat "$errfile"); rm -f "$errfile"
}

fresh_session() { session_id_test="test-$$-$RANDOM"; rm -f "$CALM_HOOKS_STATE_DIR/${session_id_test}.json"; }

# --- 1. off mode (default, no hooks.mode file at all): everything silent, exit 0 ---
rm -f "$CALM_DIR/hooks.mode"
fresh_session
capture run_hook "Edit" "src/a.rs"
[ "$hook_ec" = "0" ] || fail "off/no-file mode: Edit should exit 0, got $hook_ec"
[ -z "$hook_err" ] || fail "off/no-file mode: expected silent stderr, got: $hook_err"
echo "PASS: no hooks.mode file -> silent allow"

# --- 2. nudge mode: Edit without edit_context -> nudge (exit 0, stderr non-empty) ---
set_mode nudge
fresh_session
capture run_hook "Edit" "src/a.rs"
[ "$hook_ec" = "0" ] || fail "nudge mode: Edit should exit 0 (never blocks), got $hook_ec"
echo "$hook_err" | grep -q "nudge — advisory only" || fail "nudge mode: expected nudge-tagged stderr, got: $hook_err"
echo "PASS: nudge mode never blocks, tags message as advisory"

# --- 3. enforce mode: Edit without edit_context -> deny (exit 2) ---
set_mode enforce
fresh_session
capture run_hook "Edit" "src/a.rs"
[ "$hook_ec" = "2" ] || fail "enforce mode: Edit without edit_context should exit 2, got $hook_ec"
echo "$hook_err" | grep -q "enforce — best-effort" || fail "enforce mode: expected enforce-tagged deny, got: $hook_err"
echo "$hook_err" | grep -q "AGENTS.md" && fail "enforce mode: deny message must NOT reference AGENTS.md Stage numbers (message-decoupling requirement) — got: $hook_err"
echo "PASS: enforce mode denies native Edit without prior edit_context, message has no AGENTS.md coupling"

# --- 4. enforce mode: edit_context first, then Edit on same file -> allowed ---
set_mode enforce
fresh_session
run_hook "mcp__calm__edit_context" "src/b.rs" "SomeSymbol" >/dev/null
capture run_hook "Edit" "src/b.rs"
[ "$hook_ec" = "0" ] || fail "enforce mode: Edit after edit_context should exit 0, got $hook_ec: $hook_err"
echo "PASS: enforce mode allows Edit after edit_context ran this session for that file"

# --- 5. enforce mode: prose file (.md) without edit_context -> nudge only, never deny ---
set_mode enforce
fresh_session
capture run_hook "Edit" "docs/README.md"
[ "$hook_ec" = "0" ] || fail "enforce mode: prose file should never be denied, got $hook_ec: $hook_err"
echo "$hook_err" | grep -qi "recommended" || fail "enforce mode: prose file should still get an advisory nudge, got: $hook_err"
echo "PASS: enforce mode downgrades prose (.md) files to advisory-only, never deny"

# --- 6. enforce mode: needs_diff_impact after edit_lines, then git commit -> deny ---
set_mode enforce
fresh_session
run_hook "mcp__calm__edit_lines" "src/c.rs" >/dev/null
capture run_hook "Bash" "" "" "git commit -m test"
[ "$hook_ec" = "2" ] || fail "enforce mode: git commit after edit_lines w/o diff_impact should exit 2, got $hook_ec"
echo "PASS: enforce mode denies git commit when a write is pending diff_impact"

# --- 7. enforce mode: diff_impact resets the gate, commit then allowed ---
set_mode enforce
fresh_session
run_hook "mcp__calm__edit_lines" "src/c.rs" >/dev/null
run_hook "mcp__calm__diff_impact" >/dev/null
capture run_hook "Bash" "" "" "git commit -m test"
[ "$hook_ec" = "0" ] || fail "enforce mode: git commit after diff_impact should exit 0, got $hook_ec: $hook_err"
echo "PASS: diff_impact call clears the pending-commit gate"

# --- 8. CALM_HOOKS_DISABLE=1 short-circuits even enforce mode entirely ---
set_mode enforce
fresh_session
export CALM_HOOKS_DISABLE=1
capture run_hook "Edit" "src/a.rs"
unset CALM_HOOKS_DISABLE
[ "$hook_ec" = "0" ] || fail "CALM_HOOKS_DISABLE=1: should always exit 0, got $hook_ec"
[ -z "$hook_err" ] || fail "CALM_HOOKS_DISABLE=1: should be fully silent, got: $hook_err"
echo "PASS: CALM_HOOKS_DISABLE=1 short-circuits to silent allow regardless of mode"

# --- 9. FM1: corrupted/unrecognized mode file never escalates to enforce ---
printf 'garbage not a real file\n' >"$CALM_DIR/hooks.mode"
fresh_session
capture run_hook "Edit" "src/a.rs"
[ "$hook_ec" = "0" ] || fail "corrupted hooks.mode: must default to nudge (never deny), got exit $hook_ec"
echo "PASS: corrupted hooks.mode content defaults safely to nudge, never enforce"

set_mode enforce 999
fresh_session
capture run_hook "Edit" "src/a.rs"
[ "$hook_ec" = "0" ] || fail "wrong schema version: must default to nudge (never deny), got exit $hook_ec"
echo "PASS: unrecognized schema version defaults safely to nudge, never enforce"

# --- 10. FM3: mode downgrade (enforce -> nudge) is logged + surfaced exactly once ---
set_mode enforce
fresh_session
run_hook "mcp__calm__edit_context" "src/a.rs" "X" >/dev/null   # establishes last_seen_mode=enforce
rm -f "$CALM_DIR/audit.log"
set_mode nudge
capture run_hook "Edit" "src/a.rs"
echo "$hook_err" | grep -q "NOTICE: CALM hooks mode changed from enforce to nudge" \
  || fail "downgrade notice missing on first post-downgrade call: $hook_err"
[ -f "$CALM_DIR/audit.log" ] || fail "downgrade must be logged to .calm/audit.log"
grep -q "hooks_mode_downgraded" "$CALM_DIR/audit.log" || fail "audit.log missing hooks_mode_downgraded event"
capture run_hook "Edit" "src/a.rs"
echo "$hook_err" | grep -q "NOTICE: CALM hooks mode changed" \
  && fail "downgrade notice fired a SECOND time on an unchanged mode — should fire exactly once: $hook_err"
echo "PASS: mode downgrade is logged to audit.log and surfaced exactly once, not repeated"

echo
echo "ALL PASS (10 assertions)"
