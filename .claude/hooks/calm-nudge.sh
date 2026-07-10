#!/usr/bin/env bash
# Project-scoped PreToolUse hook: nudge toward the "calm" (CALM — Coding
# Agent Liveness Map) MCP server's own tools instead of native Read/Grep/Edit/Bash, mirroring
# AGENTS.md's workflow stages. Additive alongside any user-level hooks
# (Claude Code concatenates hooks across settings scopes; it does not let
# a project hook suppress a user-level one).
#
# Two of AGENTS.md's rules are HARD-enforced here (permissionDecision: deny),
# not just nudged, tracked via a per-session state file:
#   - Stage 5: `edit_context` must be called at least once this session
#     before the first native `Edit` of a source-code file (never re-blocked
#     after that — correlating each individual edit to a specific prior
#     edit_context(symbol) call isn't reliable from a shell hook, so this
#     enforces "checked blast radius before starting to edit", not "checked
#     it for every single edit"). `edit_symbol`/`edit_lines` are NOT gated
#     this way — they carry their own per-call risk gate (refuse a
#     hub/high-caller touch without confirm:true), which is stricter than
#     this session-level heuristic, so gating them identically would be
#     redundant, not safer.
#   - Stage 7: `diff_impact` must be called after the most recent write
#     (native Edit/Write OR mcp__calm__edit_lines/edit_symbol) before a
#     `git commit`/`git push` — reset every time any of those four tools
#     writes, satisfied every time `diff_impact` runs, so this one IS
#     precise regardless of which write path was used.
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
# .calm/ is already gitignored (see .gitignore) so state files never
# get committed; created defensively in case `ci init`/`ci serve` hasn't
# run yet in this session.
state_dir=".calm/.hook-state"
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
nudge_counts=$(jq -c '.nudge_counts // {}' <<<"$state" 2>/dev/null || echo '{}')

# --- decision-log JSONL: one line per hook invocation, resolved at exit
# regardless of which code path returned. This is the audit trail the
# per-session *state* file above can't provide on its own — state answers
# "what do I currently believe for this session", the log answers "what
# did every single hook invocation actually decide", after the fact,
# across sessions. Modeled on zzet/gortex's hook-decisions.jsonl: that log
# is literally how their user discovered a 91%-of-calls silently-skipped
# hook bug (issue #241) — grepping decision history for a suspicious gap
# (e.g. many Edit calls, zero denies) is only possible if every
# invocation left a record, not just the ones that happened to deny/nudge.
# Shared across sessions (unlike the per-session state file), so it is
# NOT covered by that file's own mtime-based cleanup — instead capped by
# line count each run so it can't grow unbounded.
decision_log="$state_dir/decisions.jsonl"
decision_log_cap=5000
decision="allow"
decision_detail=""
log_decision() {
  jq -nc \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg session "$session_id" \
    --arg tool "$tool_name" \
    --arg decision "$decision" \
    --arg detail "$decision_detail" \
    --arg file "$file_path" \
    '{ts: $ts, session_id: $session, tool_name: $tool, decision: $decision, detail: $detail, file_path: ($file | select(. != "") // null)}' \
    >>"$decision_log" 2>/dev/null || true
  if [ -f "$decision_log" ]; then
    lines=$(wc -l <"$decision_log" 2>/dev/null || echo 0)
    if [ "${lines:-0}" -gt "$decision_log_cap" ]; then
      tail -n "$decision_log_cap" "$decision_log" >"$decision_log.tmp" 2>/dev/null \
        && mv "$decision_log.tmp" "$decision_log" 2>/dev/null || true
    fi
  fi
}
trap log_decision EXIT

save_state() {
  jq -n --argjson ec "$1" --argjson nd "$2" --argjson nc "$3" \
    '{edit_context_called: $ec, needs_diff_impact: $nd, nudge_counts: $nc}' \
    >"$state_file" 2>/dev/null || true
}

deny() {
  decision="deny"
  decision_detail="$1"
  jq -n --arg reason "$1" \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: $reason}}'
  exit 0
}

# Max times to show the *same* nudge (by key) within one session. LLM agents
# are measurably hypersensitive to nudges (arXiv 2505.11584) but the flip
# side is habituation: an identical reminder fired on every single matching
# call teaches the model to pattern-match it as boilerplate and stop reading
# it, which defeats the point. Two shows is enough to register the first
# time and reinforce once if it was missed; beyond that, silence — either
# the agent has a deliberate reason to keep choosing the native tool (trust
# it, per AGENTS.md's "override only when you have explicit context the
# hint cannot account for"), or repeating it further isn't going to help.
NUDGE_CAP=2

# $1 = throttle key (distinct budget per nudge *kind*, not shared), $2 = message
nudge() {
  local key="$1" msg="$2" count
  count=$(jq -r --arg k "$key" '.[$k] // 0' <<<"$nudge_counts")
  if [ "$count" -ge "$NUDGE_CAP" ]; then
    decision="allow_nudge_capped"
    decision_detail="$key"
    exit 0
  fi
  nudge_counts=$(jq -c --arg k "$key" '.[$k] = ((.[$k] // 0) + 1)' <<<"$nudge_counts")
  save_state "$edit_context_called" "$needs_diff_impact" "$nudge_counts"
  decision="nudge"
  decision_detail="$key"
  jq -n --arg msg "$msg" \
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

# Deliberately an denylist, not "not is_code_file()": Grep's `path` can be a
# directory or unset (repo-wide, likely to include real code either way), so
# the safe default is to keep nudging unless the target is unambiguously a
# single file of a kind `ci` never indexes (see repo_overview's 14-language
# list — YAML/Markdown/TOML/JSON/lockfiles aren't in it). An allowlist like
# is_code_file's would misfire on a bare directory path (no matching suffix
# -> wrongly treated as "not code") and suppress a nudge that's still useful.
# NOTE: as of the `search(kind="grep")` addition, `ci` *does* cover these
# extensions too (it scans the filesystem directly, bypassing the parser) —
# this denylist governs whether to point at `search`/`locate` (symbol-aware)
# specifically, not whether `ci` has anything to offer at all.
is_clearly_non_code_file() {
  case "$1" in
    *.yml | *.yaml | *.md | *.toml | *.json | *.lock | *.txt | \
    *.gitignore | *.gitattributes)
      return 0 ;;
    *)
      return 1 ;;
  esac
}

# True if `p` sits under a directory `calm`'s indexer categorically never
# descends into, so a nudge toward index-backed tools (source/file_overview/
# locate/search's symbol-aware kinds) would be actively wrong — file_overview
# on such a path comes back with symbol_count:0, not "not migrated yet".
# Mirrors crates/calm-core/src/walk.rs::build_walker exactly: any dot-prefixed
# *directory* component, or a literal IGNORE_DIRS name (kept as a literal
# list here since a shell hook can't import the Rust const — update both
# together if walk.rs's list changes). Checks directory components only, via
# `dirname` when `p` isn't itself a directory: walk.rs only filters
# directory *names*, not file names ("dot-files were never filtered"), so a
# leaf dotfile (e.g. a top-level `.eslintrc.js`) can still be legitimately
# indexed and must not be suppressed just because its own name starts with
# a dot.
is_definitely_unindexed_path() {
  local p="$1" check_path seg
  if [ -d "$p" ]; then
    check_path="$p"
  else
    check_path=$(dirname -- "$p" 2>/dev/null || echo "$p")
  fi
  local IFS='/'
  for seg in $check_path; do
    case "$seg" in
      "" | .) continue ;;
      .*) return 0 ;;
    esac
    case "$seg" in
      target | node_modules | dist | build | __pycache__ | venv | legacy)
        return 0 ;;
    esac
  done
  return 1
}

# True if `cmd` contains a *real* `git commit`/`git push` invocation — i.e.
# `git` is the first word of some top-level command segment, not merely the
# text "commit"/"push" appearing anywhere in the string (a prior version
# matched \bgit\b.*\b(commit|push)\b against the whole command blob, which
# fired on e.g. `echo "...git commit..."` piping crafted JSON to this very
# script during testing — the substring was inside a quoted payload, never
# executed). Not a full shell parser: splits `cmd` on `;`/`&&`/`||`/`|` into
# segments (plus the bodies of any `$(...)`/`` `...` `` substitutions, which
# genuinely execute too, e.g. `RESULT=$(git commit -m x)`), strips a leading
# VAR=value/sudo/command/exec prefix from each, and requires the first token
# to be `git`/`*/git` — then walks tokens after it, skipping `-flag` forms
# (and the separate value token for `-C`/`--git-dir`/`--work-tree`/`-c`)
# until the first non-flag token, which must be `commit`/`push`. Fails
# toward catching it (not silently allowing) whenever the parse is
# ambiguous, same philosophy as `resolve_git_target_root` below.
is_real_git_commit_or_push() {
  local cmd="$1" segment first tok i extra

  extra=$(grep -oE '\$\([^()]*\)|`[^`]*`' <<<"$cmd")
  extra=$(sed -E 's/^\$\(//; s/\)$//; s/^`//; s/`$//' <<<"$extra")

  while IFS= read -r segment; do
    [ -z "$segment" ] && continue
    segment="${segment#"${segment%%[![:space:]]*}"}"
    while [[ "$segment" =~ ^[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+(.*)$ ]]; do
      segment="${BASH_REMATCH[1]}"
    done
    segment="${segment#sudo }"
    segment="${segment#command }"
    segment="${segment#exec }"

    read -r -a tokens <<<"$segment"
    first="${tokens[0]:-}"
    [[ "$first" == "git" || "$first" == */git ]] || continue

    i=1
    while [ "$i" -lt "${#tokens[@]}" ]; do
      tok="${tokens[$i]}"
      case "$tok" in
        -C | --git-dir | --work-tree | -c)
          i=$((i + 2)) ;;
        -*)
          i=$((i + 1)) ;;
        commit | push)
          return 0 ;;
        *)
          break ;;
      esac
    done
  done < <(printf '%s\n%s\n' "$cmd" "$extra" | sed -E 's/&&|\|\||[;|]/\n/g')

  return 1
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

# Record write-relevant mcp__calm__* calls as they happen — recorded on
# PreToolUse (before the call runs) since attempting the check is what
# matters here, and PreToolUse is all that's needed to observe it.
if [ "$tool_name" = "mcp__calm__edit_context" ]; then
  decision_detail="state:edit_context_called=true"
  save_state true "$needs_diff_impact" "$nudge_counts"
  exit 0
fi
if [ "$tool_name" = "mcp__calm__diff_impact" ]; then
  decision_detail="state:needs_diff_impact=false"
  save_state "$edit_context_called" false "$nudge_counts"
  exit 0
fi
if [ "$tool_name" = "mcp__calm__edit_lines" ] || [ "$tool_name" = "mcp__calm__edit_symbol" ]; then
  # These are calm's own write path (AGENTS.md Stage 6) — a real file write
  # just like native Edit/Write, so the Stage 7 diff_impact gate below must
  # still apply. Not treated as satisfying edit_context_called: they carry
  # their own stricter per-call risk gate instead (see header comment).
  decision_detail="state:needs_diff_impact=true"
  save_state "$edit_context_called" true "$nudge_counts"
  exit 0
fi

case "$tool_name" in
  Read)
    if [ -n "$file_path" ] && is_definitely_unindexed_path "$file_path"; then
      : # ci has nothing indexed here (dotdir / build-artifact dir) — native Read is correct
    elif [ -z "$file_path" ] || is_code_file "$file_path"; then
      nudge read_native 'CI available in this repo — prefer mcp__calm__source(symbol) for a symbol-precise read, or mcp__calm__file_overview(path) instead of reading the whole file (AGENTS.md Stage 3).'
    fi
    ;;
  Grep)
    grep_path=$(jq -r '.tool_input.path // ""' <<<"$input")
    if [ -n "$grep_path" ] && is_definitely_unindexed_path "$grep_path"; then
      : # nothing indexed under this path — search/locate would come back empty, not stale
    elif [ -z "$grep_path" ] || ! is_clearly_non_code_file "$grep_path"; then
      nudge grep_tool 'CI available in this repo — prefer mcp__calm__search(query, kind="hybrid") or mcp__calm__locate(query) for a symbol-aware search, or mcp__calm__search(query, kind="grep") for a literal/regex match (also covers files the parser skips) instead of Grep (AGENTS.md Stage 2).'
    fi
    ;;
  Edit)
    decision_detail="state:needs_diff_impact=true"
    save_state "$edit_context_called" true "$nudge_counts"
    if is_code_file "$file_path" && [ "$edit_context_called" != "true" ]; then
      deny "MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) before editing $file_path, never skip (especially if is_hub). Call it once for the symbol you are about to change, then retry this edit. Also consider mcp__calm__edit_symbol/edit_lines (AGENTS.md Stage 6) instead of this native Edit — it can apply the change directly, hash-verified and risk-gated, chaining off edit_context's range_checksum."
    fi
    ;;
  Write)
    save_state "$edit_context_called" true "$nudge_counts"
    nudge write_native 'MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) before this write if it modifies existing code, never skip (especially if is_hub). If this is editing an existing tracked file rather than creating a new one, consider mcp__calm__edit_lines/edit_symbol (AGENTS.md Stage 6) instead — hash-verified, risk-gated, reindexes immediately.'
    ;;
  Bash)
    if is_real_git_commit_or_push "$command"; then
      if [ "$needs_diff_impact" = "true" ]; then
        target_root=$(resolve_git_target_root "$command")
        project_root=$(git rev-parse --show-toplevel 2>/dev/null)
        if [ -z "$target_root" ] || [ "$target_root" = "$project_root" ]; then
          deny 'MANDATORY per AGENTS.md Stage 7 — call mcp__calm__diff_impact(staged=true) before this commit/push, never skip. Files changed since the last diff_impact check.'
        fi
      fi
    elif grep -qE '\b(grep|rg|ag)\b' <<<"$command"; then
      nudge bash_grep 'CI available in this repo — prefer mcp__calm__search(query, kind="hybrid"|"grep") or mcp__calm__locate instead of grep via Bash (AGENTS.md Stage 2).'
    elif grep -qE '\bfind\b.*-i?name\b' <<<"$command"; then
      nudge bash_find 'CI available in this repo — prefer mcp__calm__search(query, kind="file") or mcp__calm__file_overview / mcp__calm__dependencies instead of find (AGENTS.md Stage 1-2).'
    fi
    ;;
esac
