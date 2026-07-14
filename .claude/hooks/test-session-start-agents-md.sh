#!/usr/bin/env bash
# Regression test for session-start-agents-md.sh's F1 source-aware dedup
# (2026-07-14 audit-design, docs/superskills/specs/2026-07-14-calm-agent-
# experience-round2-fixes.md). Plain bash, same style as test-calm-nudge.sh:
# run directly, prints PASS or exits non-zero naming which assertion broke.
set -euo pipefail

work_dir=$(mktemp -d)
cleanup() { rm -rf "$work_dir"; }
trap cleanup EXIT

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cp "$repo_root/AGENTS.md" "$work_dir/AGENTS.md"
cp "$repo_root/.claude/hooks/session-start-agents-md.sh" "$work_dir/session-start-agents-md.sh"
cd "$work_dir"

fail() {
  echo "FAIL: $1"
  exit 1
}

run_start() {
  # $1=session_id  $2=source
  jq -nc --arg sid "$1" --arg src "$2" \
    '{session_id: $sid, source: $src, hook_event_name: "SessionStart"}' \
    | bash session-start-agents-md.sh
}

full_len() { echo -n "$1" | jq -r '.hookSpecificOutput.additionalContext' | wc -c; }

agents_md_len=$(wc -c < AGENTS.md)

# 1. startup, fresh session_id -> FULL inject, marker created.
out=$(run_start "sid-aaa" "startup")
len=$(full_len "$out")
if [ "$len" -lt "$((agents_md_len / 2))" ]; then
  fail "expected full AGENTS.md injection on startup, got $len chars (AGENTS.md is $agents_md_len)"
fi
[ -f ".calm/.hook-state/sessionstart-seen/sid-aaa" ] \
  || fail "expected a seen-marker to be created for sid-aaa after startup"

# 2. resume, SAME session_id (marker now exists) -> banner-only, short.
out=$(run_start "sid-aaa" "resume")
len=$(full_len "$out")
if [ "$len" -ge 2000 ]; then
  fail "expected a short banner-only response on resume of an already-seen session, got $len chars"
fi
echo "$out" | jq -r '.hookSpecificOutput.additionalContext' | grep -q "calm-guide" \
  || fail "expected the banner-only response to point at the calm-guide Skill, got: $out"

# 3. resume, a DIFFERENT never-seen session_id -> FULL inject (fail-safe:
#    dedup only applies to a session_id already recorded as seen).
out=$(run_start "sid-never-seen" "resume")
len=$(full_len "$out")
if [ "$len" -lt "$((agents_md_len / 2))" ]; then
  fail "expected full injection for a never-seen session_id even with source=resume, got $len chars"
fi

# 4. clear, SAME session_id as test 1 -> FULL inject. This is the exact bug
#    audit-design caught in the first draft of F1 (session_id-only dedup
#    would have wrongly served the banner here, right after context was
#    wiped and the full guide is needed most).
out=$(run_start "sid-aaa" "clear")
len=$(full_len "$out")
if [ "$len" -lt "$((agents_md_len / 2))" ]; then
  fail "expected full injection on source=clear even for an already-seen session_id, got $len chars"
fi

# 5. compact, SAME session_id -> FULL inject, same reasoning as clear.
out=$(run_start "sid-aaa" "compact")
len=$(full_len "$out")
if [ "$len" -lt "$((agents_md_len / 2))" ]; then
  fail "expected full injection on source=compact even for an already-seen session_id, got $len chars"
fi

# 6. Missing `source` field entirely -> FULL inject (fail toward
#    re-injecting on any unrecognized/absent shape, never toward silence).
out=$(jq -nc --arg sid "sid-aaa" '{session_id: $sid, hook_event_name: "SessionStart"}' \
  | bash session-start-agents-md.sh)
len=$(full_len "$out")
if [ "$len" -lt "$((agents_md_len / 2))" ]; then
  fail "expected full injection when source is missing entirely, got $len chars"
fi

echo "PASS"
