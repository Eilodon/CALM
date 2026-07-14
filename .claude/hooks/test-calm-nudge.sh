#!/usr/bin/env bash
# Regression test for calm-nudge.sh's per-file native-Edit-block gate
# (docs/superskills/plans/2026-07-13-calm-agent-experience-upgrade.md,
# Task C1). No test framework exists for this project's shell hooks, so
# this is a plain bash script: run it directly, it prints PASS or exits
# non-zero with a FAIL message naming which assertion broke.
set -euo pipefail
cd "$(dirname "$0")/../.."

session_id_test="test-$$"
# Isolated per-run state dir (2026-07-14 eval-set prerequisite): calm-nudge.sh
# used to hardcode ".calm/.hook-state", so every run of this suite wrote its
# synthetic fixture payloads straight into the SAME decisions.jsonl real
# sessions append to — verified live: 156 of 612 real lines were test-$$
# fixtures, not organic traffic, growing every time CI or a dev runs this
# script. CALM_NUDGE_STATE_DIR overrides that hardcode so this suite's state
# (including its decisions.jsonl writes) lands in a throwaway tmp dir instead.
export CALM_NUDGE_STATE_DIR
CALM_NUDGE_STATE_DIR=$(mktemp -d)
state_dir="$CALM_NUDGE_STATE_DIR"
state_file="$state_dir/${session_id_test}.json"
cleanup() { rm -rf "$CALM_NUDGE_STATE_DIR"; }
trap cleanup EXIT

run_hook() {
  # $1=tool_name  $2=file_path (Edit) or path (edit_context)  $3=symbol (edit_context)
  jq -nc --arg session "$session_id_test" \
    --arg tool "$1" --arg path "${2:-}" --arg symbol "${3:-}" \
    '{session_id: $session, tool_name: $tool, tool_input: {file_path: $path, path: $path, symbol: $symbol}}' \
    | bash .claude/hooks/calm-nudge.sh
}

fail() {
  echo "FAIL: $1"
  exit 1
}

# 1. edit_context on a.rs, then native Edit on a.rs -> ALLOWED (no deny)
run_hook "mcp__calm__edit_context" "crates/calm-core/src/a.rs" "SomeSymbol" >/dev/null
out=$(run_hook "Edit" "crates/calm-core/src/a.rs")
if echo "$out" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1; then
  fail "expected allow for a.rs after its own edit_context, got deny: $out"
fi

# 2. native Edit on a DIFFERENT file b.rs, same session, no edit_context for
#    it -> DENIED even though edit_context was called for a.rs above (this
#    is the exact regression Task C1 fixes: a session-wide unlock would
#    have allowed this).
out=$(run_hook "Edit" "crates/calm-core/src/b.rs")
if ! echo "$out" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1; then
  fail "expected deny for b.rs (never edit_context'd this session), got: $out"
fi
reason=$(echo "$out" | jq -r '.hookSpecificOutput.permissionDecisionReason')
if [[ "$reason" != *"b.rs"* ]]; then
  fail "deny reason must name b.rs specifically, got: $reason"
fi
if [[ "$reason" != *"other file(s), but not this one"* ]]; then
  fail "deny reason must explain that edit_context was satisfied for OTHER files but not this one (message-wording regression), got: $reason"
fi

# 3. edit_context on b.rs now unlocks it too (per-file, additive, not
#    replacing a.rs's earlier unlock).
run_hook "mcp__calm__edit_context" "crates/calm-core/src/b.rs" "OtherSymbol" >/dev/null
out=$(run_hook "Edit" "crates/calm-core/src/b.rs")
if echo "$out" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1; then
  fail "expected allow for b.rs after its own edit_context, got deny: $out"
fi
out=$(run_hook "Edit" "crates/calm-core/src/a.rs")
if echo "$out" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1; then
  fail "expected a.rs to STILL be allowed after b.rs was separately unlocked (per-file state must be additive), got deny: $out"
fi

# 4. edit_context(symbol) with NO `path` (the normal way to call it for a
#    globally-unique symbol name -- see EditContextParams' own doc comment)
#    must still unlock that file for a later native Edit, by falling back to
#    a read-only lookup against .calm/index.db mirroring resolve_symbol's own
#    "exactly one row named X" criterion. Uses a real symbol from this repo
#    (crates/calm-server/src/tools/common.rs::resolve_symbol_candidates) so
#    the DB lookup has something real to find -- this is the exact regression
#    a prior session hit editing crates/calm-cli/src/main.rs (see memory
#    calm-two-tooling-bugs-root-cause-2026-07-14).
run_hook_symbol_only() {
  jq -nc --arg session "$session_id_test" --arg tool "$1" --arg symbol "$2" \
    '{session_id: $session, tool_name: $tool, tool_input: {symbol: $symbol}}' \
    | bash .claude/hooks/calm-nudge.sh
}
if [ -f .calm/index.db ]; then
  run_hook_symbol_only "mcp__calm__edit_context" "resolve_symbol_candidates" >/dev/null
  out=$(run_hook "Edit" "crates/calm-server/src/tools/common.rs")
  if echo "$out" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1; then
    fail "expected allow for common.rs after edit_context(symbol-only) resolved it via the DB fallback, got deny: $out"
  fi

  # 5. A symbol name with more than one row in the DB (genuinely ambiguous,
  #    or the real tool call would itself return `ambiguous`) must NOT be
  #    guessed at -- no file gets unlocked, so a same-session native Edit on
  #    an unrelated, never-edit_context'd file still denies as usual.
  multi_name=$(sqlite3 -readonly -separator '|' .calm/index.db \
    "SELECT name FROM symbols GROUP BY name HAVING COUNT(*) > 1 LIMIT 1;" 2>/dev/null || true)
  if [ -n "$multi_name" ]; then
    run_hook_symbol_only "mcp__calm__edit_context" "$multi_name" >/dev/null
    out=$(run_hook "Edit" "crates/calm-core/src/never_edit_contexted.rs")
    if ! echo "$out" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1; then
      fail "expected deny: an ambiguous symbol name (>1 row) must not unlock any file, got: $out"
    fi
  fi
fi

# --- 2026-07-14 conditional-enforcement tests (search/grep "steepest hill"
# redesign): nudge only when CALM actually has leverage, silent otherwise ---
run_hook_grep() {
  # $1=path (may be empty for repo-wide) $2=pattern
  jq -nc --arg session "$session_id_test" --arg path "${1:-}" --arg pat "${2:-}" \
    '{session_id: $session, tool_name: "Grep", tool_input: {path: $path, pattern: $pat}}' \
    | bash .claude/hooks/calm-nudge.sh
}
run_hook_bash() {
  jq -nc --arg session "$session_id_test" --arg cmd "$1" \
    '{session_id: $session, tool_name: "Bash", tool_input: {command: $cmd}}' \
    | bash .claude/hooks/calm-nudge.sh
}
# $1=command $2=stdout $3=returnCodeInterpretation (optional). Shape matches
# a REAL captured PostToolUse payload for Bash (2026-07-14,
# .calm/.hook-state/posttooluse-grep-dump.jsonl), not the official docs
# example (which shows tool_response as a plain string) — see
# handle_post_tool_use's header comment in calm-nudge.sh for how this was
# verified.
run_hook_bash_post() {
  jq -nc --arg session "$session_id_test" --arg cmd "$1" --arg out "${2:-}" --arg rc "${3:-}" \
    '{session_id: $session, hook_event_name: "PostToolUse", tool_name: "Bash",
      tool_input: {command: $cmd},
      tool_response: ({stdout: $out, stderr: "", interrupted: false, isImage: false, noOutputExpected: false}
        + (if $rc != "" then {returnCodeInterpretation: $rc} else {} end))}' \
    | bash .claude/hooks/calm-nudge.sh
}
run_hook_read_path() {
  jq -nc --arg session "$session_id_test" --arg path "$1" \
    '{session_id: $session, tool_name: "Read", tool_input: {file_path: $path}}' \
    | bash .claude/hooks/calm-nudge.sh
}
is_silent() { [ -z "$1" ]; }

# 6. Grep on one specific indexed file with a real regex/free-text pattern
#    -> SILENT (native genuinely has no CALM disadvantage here).
out=$(run_hook_grep "crates/calm-core/src/edit.rs" 'fn\s+\w+\(')
is_silent "$out" || fail "expected silence for single-file regex grep, got: $out"

# 7. Same file, but an identifier-shaped pattern -> NUDGE (mcp__calm__locate
#    gives strictly more than a text match here, regardless of file scope).
#    Also checked here for the tool-discovery hint (2026-07-14 redesign):
#    "prefer mcp__calm__X" is only actionable if the agent knows a deferred-
#    tools client might need an explicit discovery step first. Checked on
#    this specific call (not a separate one) since NUDGE_CAP=2 means the
#    SAME key ("grep_tool") only carries the full message twice per session
#    -- a later call would silently fall into tally mode and this assertion
#    would flake depending on how many prior grep_tool nudges ran earlier
#    in this test file.
out=$(run_hook_grep "crates/calm-core/src/edit.rs" "validate_syntax_diff")
echo "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null 2>&1 \
  || fail "expected a nudge for single-file identifier-shaped grep, got: $out"
echo "$out" | jq -r '.hookSpecificOutput.additionalContext' | grep -q "tool-discovery step" \
  || fail "expected TOOL_DISCOVERY_HINT in a grep nudge, got: $out"

# 8. No path at all (repo-wide) -> NUDGE regardless of pattern shape.
out=$(run_hook_grep "" 'fn\s+\w+\(')
echo "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null 2>&1 \
  || fail "expected a nudge for repo-wide grep (no path), got: $out"

# 9. Bash grep targeting exactly one existing file, no -r -> SILENT.
out=$(run_hook_bash 'grep -n "fn validate_syntax" crates/calm-core/src/edit.rs')
is_silent "$out" || fail "expected silence for single-file Bash grep, got: $out"

# 10. Bash grep with -r, at PreToolUse -> SILENT now (2026-07-14 PostToolUse
#     follow-up retired this branch's pre-call nudge in favor of
#     handle_post_tool_use's result-grounded one, so the same real call is
#     never nudged twice -- see that retirement's comment in calm-nudge.sh).
out=$(run_hook_bash 'grep -rn "fn validate_syntax" crates/')
is_silent "$out" || fail "expected silence at PreToolUse for recursive Bash grep (Post now owns this nudge), got: $out"

# 10b. Same recursive grep, at PostToolUse, with a real multi-file stdout
#      (3 files) -> NUDGE naming the actual count.
out=$(run_hook_bash_post 'grep -rn "fn validate_syntax" crates/' \
  "crates/a.rs:1:fn validate_syntax() {}
crates/b.rs:2:fn validate_syntax_diff() {}
crates/c.rs:3:fn validate_syntax_other() {}")
echo "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null 2>&1 \
  || fail "expected a PostToolUse nudge for a 3-file Bash grep match, got: $out"
echo "$out" | jq -r '.hookSpecificOutput.additionalContext' | grep -q "3 files" \
  || fail "expected the PostToolUse nudge to name the real file count (3), got: $out"
echo "$out" | jq -e '.hookSpecificOutput.hookEventName == "PostToolUse"' >/dev/null 2>&1 \
  || fail "expected hookEventName to be PostToolUse (not hardcoded PreToolUse), got: $out"

# 10c. Recursive grep at PostToolUse, only 1 matched file -> SILENT (below
#      the >=3-file leverage threshold).
out=$(run_hook_bash_post 'grep -rn "fn validate_syntax" crates/' \
  "crates/a.rs:1:fn validate_syntax() {}")
is_silent "$out" || fail "expected silence for a 1-file PostToolUse Bash grep match, got: $out"

# 10d. Recursive grep at PostToolUse, zero matches -> NUDGE toward
#      search/locate (a literal grep can miss a renamed/reworded symbol a
#      fuzzy/hybrid search would still find).
out=$(run_hook_bash_post 'grep -rn "definitely_not_a_real_symbol_xyz" crates/' "" "No matches found")
echo "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null 2>&1 \
  || fail "expected a PostToolUse nudge for a zero-match repo-wide Bash grep, got: $out"

# 10e. Single-file Bash grep at PostToolUse -> SILENT regardless of match
#      count (native genuinely has no CALM disadvantage there, same as Pre).
out=$(run_hook_bash_post 'grep -n "fn validate_syntax" crates/calm-core/src/edit.rs' \
  "crates/calm-core/src/edit.rs:1:fn validate_syntax() {}")
is_silent "$out" || fail "expected silence for a single-file PostToolUse Bash grep, got: $out"

# 10f. Piped grep at PostToolUse -> SILENT (filtering another command's
#      stream, not a file search -- same guard as Pre).
out=$(run_hook_bash_post 'cat crates/a.rs | grep foo' \
  "crates/a.rs:1:foo
crates/b.rs:2:foo
crates/c.rs:3:foo")
is_silent "$out" || fail "expected silence for a piped Bash grep at PostToolUse, got: $out"

# 11. Deny messages also carry the tool-discovery hint -- edit_context is
#     just as deferrable as search/locate, and a hard deny whose only
#     remediation is an unloadable tool is a real deadlock, not just noise.
out=$(run_hook "Edit" "crates/calm-core/src/never_edit_contexted2.rs")
echo "$out" | jq -r '.hookSpecificOutput.permissionDecisionReason' | grep -q "tool-discovery step" \
  || fail "expected TOOL_DISCOVERY_HINT in the edit_context deny reason, got: $out"

# 12. Read on a short (<80 line) real indexed source file -> SILENT.
out=$(run_hook_read_path "$(pwd)/crates/calm-core/src/lib.rs")
is_silent "$out" || fail "expected silence for a short (34-line) indexed file Read, got: $out"

# 13. Read on a long real indexed source file -> NUDGE.
out=$(run_hook_read_path "$(pwd)/crates/calm-server/src/tools/edit.rs")
echo "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null 2>&1 \
  || fail "expected a nudge for a long indexed file Read, got: $out"

# 14-17. Proactive session_context reminder (2026-07-14 self-audit finding:
# session_context existed but nothing ever prompted calling it). Uses its
# own isolated session id so its call count starts clean regardless of
# what tests 1-13 already did to session_id_test's state.
run_hook_tool() {
  jq -nc --arg session "session-context-reminder-test" --arg tool "$1" \
    '{session_id: $session, tool_name: $tool, tool_input: {}}' \
    | bash .claude/hooks/calm-nudge.sh
}

# 14. First 24 mcp__calm__search calls -> SILENT (below the every-25 threshold).
for i in $(seq 1 24); do
  out=$(run_hook_tool "mcp__calm__search")
  is_silent "$out" || fail "expected silence on search call #$i (below threshold), got: $out"
done

# 15. The 25th call -> REMINDER, naming session_context.
out=$(run_hook_tool "mcp__calm__search")
echo "$out" | jq -r '.hookSpecificOutput.additionalContext' | grep -q "session_context" \
  || fail "expected a session_context reminder on the 25th otherwise-silent call, got: $out"

# 16. Calling mcp__calm__session_context itself -> SILENT (it's the
#     acknowledgment, not another otherwise-silent call to count), and
#     resets the counter.
out=$(run_hook_tool "mcp__calm__session_context")
is_silent "$out" || fail "expected silence on the session_context call itself, got: $out"

# 17. Next 24 calls after the reset -> SILENT again (proves the counter
#     actually reset to 0, not just coincidentally still under 25 from
#     continuing the old count).
for i in $(seq 1 24); do
  out=$(run_hook_tool "mcp__calm__search")
  is_silent "$out" || fail "expected silence on post-reset search call #$i, got: $out"
done

# 18. Raw `rustfmt`/`cargo fmt` via Bash -> NUDGE toward format_files (2026-07-14
#     self-audit: a raw rustfmt invocation silently reformatted an unrelated
#     sibling file on this exact repo).
out=$(run_hook_bash 'rustfmt --edition 2024 crates/calm-core/src/config.rs')
echo "$out" | jq -r '.hookSpecificOutput.additionalContext' | grep -q "format_files" \
  || fail "expected a format_files nudge for a raw rustfmt invocation, got: $out"

out=$(run_hook_bash 'cargo fmt --all -- --check')
echo "$out" | jq -r '.hookSpecificOutput.additionalContext' | grep -q "format_files" \
  || fail "expected a format_files nudge for a raw cargo fmt invocation, got: $out"

echo "PASS"
