#!/usr/bin/env bash
# Project-scoped PreToolUse hook: nudge toward the "ci" (Code Intelligence)
# MCP server's own tools instead of native Read/Grep/Edit/Bash, mirroring
# AGENTS.md's workflow stages. Additive alongside any user-level hooks
# (Claude Code concatenates hooks across settings scopes; it does not let
# a project hook suppress a user-level one).
#
# Two of AGENTS.md's rules are HARD-enforced here (permissionDecision: deny),
# not just nudged, tracked via a per-session state file:
#   - Stage 5: `edit_context` must be called at least once this session
#     before the first `Edit` of a source-code file (never re-blocked after
#     that — correlating each individual edit to a specific prior
#     edit_context(symbol) call isn't reliable from a shell hook, so this
#     enforces "checked blast radius before starting to edit", not
#     "checked it for every single edit").
#   - Stage 7: `diff_impact` must be called after the most recent Edit/Write
#     before a `git commit`/`git push` — reset every time a file changes,
#     satisfied every time `diff_impact` runs, so this one IS precise.
# `Write` is deliberately NOT hard-gated: it also covers brand-new file
# creation, where no symbol exists yet for `edit_context` to look up —
# blocking it would deadlock. It keeps the pre-existing advisory nudge only.
set -uo pipefail

input=$(cat)
tool_name=$(jq -r '.tool_name // ""' <<<"$input")
command=$(jq -r '.tool_input.command // ""' <<<"$input")
file_path=$(jq -r '.tool_input.file_path // ""' <<<"$input")
session_id=$(jq -r '.session_id // "unknown"' <<<"$input")

# --- session state (survives across PreToolUse calls within one session) ---
# .codeindex/ is already gitignored (see .gitignore) so state files never
# get committed; created defensively in case `ci init`/`ci serve` hasn't
# run yet in this session.
state_dir=".codeindex/.hook-state"
mkdir -p "$state_dir" 2>/dev/null || true
state_file="$state_dir/${session_id}.json"
# Opportunistic cleanup of stale state from old sessions — cheap, best-effort.
find "$state_dir" -maxdepth 1 -name '*.json' -mtime +1 -delete 2>/dev/null || true

state='{}'
if [ -f "$state_file" ]; then
  state=$(cat "$state_file" 2>/dev/null || echo '{}')
fi
edit_context_called=$(jq -r '.edit_context_called // false' <<<"$state" 2>/dev/null || echo false)
needs_diff_impact=$(jq -r '.needs_diff_impact // false' <<<"$state" 2>/dev/null || echo false)

save_state() {
  jq -n --argjson ec "$1" --argjson nd "$2" \
    '{edit_context_called: $ec, needs_diff_impact: $nd}' \
    >"$state_file" 2>/dev/null || true
}

deny() {
  jq -n --arg reason "$1" \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: $reason}}'
  exit 0
}

nudge() {
  jq -n --arg msg "$1" \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $msg}}'
  exit 0
}

is_code_file() {
  case "$1" in
    *.py | *.ts | *.tsx | *.js | *.jsx | *.mjs | *.cjs | *.java | *.rs | *.go | \
    *.c | *.h | *.cpp | *.cc | *.cxx | *.hpp | *.hxx | *.cs | *.rb | *.php | \
    *.kt | *.kts | *.swift | *.sh | *.bash)
      return 0 ;;
    *)
      return 1 ;;
  esac
}

# Resolve the git repo root that `cmd` will actually operate on, so the
# commit/push gate only fires for *this* project's repo — not an unrelated
# repo the agent is inspecting/debugging elsewhere (e.g. a scratch clone
# under /tmp, or a test fixture repo). PreToolUse hooks always run with cwd
# pinned to the project root regardless of any `cd` inside `cmd` (this
# harness resets the shell's cwd between Bash calls), so neither `pwd` nor
# the hook JSON's `cwd` field can distinguish this — the command text itself
# is the only signal available. Best-effort, not a real shell parser: honors
# the last explicit `git -C <dir>` / `cd <dir> &&`/`;` before the git call;
# anything it can't confidently resolve falls back to "this repo" (fail
# toward enforcing the gate, not silently skipping it).
resolve_git_target_root() {
  local cmd="$1" explicit_dir=""

  explicit_dir=$(grep -oE 'git[[:space:]]+-C[[:space:]]+[^[:space:]]+' <<<"$cmd" \
    | tail -1 | awk '{print $NF}')

  if [ -z "$explicit_dir" ]; then
    explicit_dir=$(grep -oE 'cd[[:space:]]+[^[:space:]&;]+[[:space:]]*(&&|;)' <<<"$cmd" \
      | tail -1 | sed -E 's/^cd[[:space:]]+//; s/[[:space:]]*(&&|;)$//')
  fi

  if [ -n "$explicit_dir" ]; then
    git -C "$explicit_dir" rev-parse --show-toplevel 2>/dev/null
  else
    git rev-parse --show-toplevel 2>/dev/null
  fi
}

# Record mcp__ci__edit_context / mcp__ci__diff_impact calls as they happen —
# recorded on PreToolUse (before the call runs) since attempting the check is
# what matters here, and PreToolUse is all that's needed to observe it.
if [ "$tool_name" = "mcp__ci__edit_context" ]; then
  save_state true "$needs_diff_impact"
  exit 0
fi
if [ "$tool_name" = "mcp__ci__diff_impact" ]; then
  save_state "$edit_context_called" false
  exit 0
fi

case "$tool_name" in
  Read)
    nudge 'CI available in this repo — prefer mcp__ci__source(symbol) for a symbol-precise read, or mcp__ci__file_overview(path) instead of reading the whole file (AGENTS.md Stage 3).'
    ;;
  Grep)
    nudge 'CI available in this repo — prefer mcp__ci__search(query, kind="hybrid") or mcp__ci__locate(query) instead of Grep (AGENTS.md Stage 2).'
    ;;
  Edit)
    save_state "$edit_context_called" true
    if is_code_file "$file_path" && [ "$edit_context_called" != "true" ]; then
      deny "MANDATORY per AGENTS.md Stage 5 — call mcp__ci__edit_context(symbol) before editing $file_path, never skip (especially if is_hub). Call it once for the symbol you are about to change, then retry this edit."
    fi
    ;;
  Write)
    save_state "$edit_context_called" true
    nudge 'MANDATORY per AGENTS.md Stage 5 — call mcp__ci__edit_context(symbol) before this write if it modifies existing code, never skip (especially if is_hub).'
    ;;
  Bash)
    # Broad on purpose: `git -C <dir> commit` / `git --git-dir=<dir> push` put
    # flags between the subcommand and "commit"/"push", so a tight
    # `git commit` adjacency check misses them entirely (a false negative —
    # worse than the false positive this file otherwise guards against).
    # resolve_git_target_root() + the scope check below is what keeps this
    # broad match from over-firing on unrelated repos.
    if grep -qE '\bgit\b.*\b(commit|push)\b' <<<"$command"; then
      if [ "$needs_diff_impact" = "true" ]; then
        target_root=$(resolve_git_target_root "$command")
        project_root=$(git rev-parse --show-toplevel 2>/dev/null)
        if [ -z "$target_root" ] || [ "$target_root" = "$project_root" ]; then
          deny 'MANDATORY per AGENTS.md Stage 7 — call mcp__ci__diff_impact(staged=true) before this commit/push, never skip. Files changed since the last diff_impact check.'
        fi
      fi
    elif grep -qE '\b(grep|rg|ag)\b' <<<"$command"; then
      nudge 'CI available in this repo — prefer mcp__ci__search / mcp__ci__locate instead of grep via Bash (AGENTS.md Stage 2).'
    elif grep -qE '\bfind\b.*-i?name\b' <<<"$command"; then
      nudge 'CI available in this repo — prefer mcp__ci__file_overview / mcp__ci__dependencies instead of find (AGENTS.md Stage 1-2).'
    fi
    ;;
esac
