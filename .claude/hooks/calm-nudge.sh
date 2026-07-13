#!/usr/bin/env bash
# Project-scoped PreToolUse hook: nudge toward the "calm" (CALM — Coding
# Agent Liveness Map) MCP server's own tools instead of native Read/Grep/Edit/Bash, mirroring
# AGENTS.md's workflow stages. Additive alongside any user-level hooks
# (Claude Code concatenates hooks across settings scopes; it does not let
# a project hook suppress a user-level one).
#
# Two of AGENTS.md's rules are HARD-enforced here (permissionDecision: deny),
# not just nudged, tracked via a per-session state file:
#   - Stage 5: `edit_context` must be called at least once THIS SESSION FOR
#     EACH FILE before the first native `Edit` of that file (re-armed per
#     file, not once for the whole session — a session-wide unlock let an
#     agent review file A's blast radius then silently native-Edit file B,
#     never reviewed at all; see docs/superskills/specs/2026-07-13-calm-
#     agent-experience-upgrade.md). Still per-FILE, not per-symbol:
#     correlating each individual edit to a specific prior
#     edit_context(symbol) call isn't reliable from a shell hook, so this
#     enforces "checked blast radius before starting to edit this file",
#     not "checked it for every single edit". `edit_symbol`/`edit_lines`
#     are NOT gated this way — they carry their own per-call risk gate
#     (refuse a hub/high-caller touch without confirm:true), which is
#     stricter than this per-file heuristic, so gating them identically
#     would be redundant, not safer.
#   - Stage 7: `diff_impact` must be called after the most recent write
#     (native Edit/Write OR mcp__calm__edit_lines/edit_symbol) before a
#     `git commit`/`git push` — reset every time any of those four tools
#     writes, satisfied every time `diff_impact` runs, so this one IS
#     precise regardless of which write path was used.
# `Write` is deliberately NOT hard-gated: it also covers brand-new file
# creation, where no symbol exists yet for `edit_context` to look up —
# blocking it would deadlock. It keeps the pre-existing advisory nudge only.
#
# ADVISORY layer (2026-07-13 redesign, driven by an agent's own retro on why
# it drifted to native tools despite this hook — three levers, each aimed at
# a real friction rather than "nag harder"):
#   1. PRECISION over coverage. A nudge that fires on a *correct* native use
#      (grepping a .log, a piped `... | grep`, a dotdir CALM can't index)
#      teaches the agent to discount every nudge. So the advisory paths now
#      bail out on piped greps, unindexed targets, and — for the Stage-5
#      deny too — files under paths CALM never indexes (this script itself
#      lives under .claude/ and used to get falsely denied).
#   2. ACTIONABLE, not generic. Nudges interpolate the actual query/path so
#      the CALM alternative is copy-paste-ready — removing the "translate my
#      grep into a CALM call" step that made native feel cheaper.
#   3. A CHANGING NUMBER beats a fixed sentence. After the per-kind nudge cap,
#      instead of going fully silent (the blind spot where drift happens),
#      surface a running native-vs-CALM exploration tally every few calls and
#      at the commit checkpoint — a self-referential number resists the
#      habituation an identical reminder can't.
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
edit_context_files=$(jq -c '.edit_context_files // []' <<<"$state" 2>/dev/null || echo '[]')
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
  # Merge onto existing state so orthogonal exploration counters
  # (native_explore / calm_explore, maintained by `bump`) survive a save of
  # the three gate-relevant fields — without the merge every save would wipe
  # them, silently zeroing the tally.
  local prev='{}'
  [ -f "$state_file" ] && prev=$(cat "$state_file" 2>/dev/null || echo '{}')
  jq -n --argjson prev "$prev" --argjson ecf "$1" --argjson nd "$2" --argjson nc "$3" \
    '$prev + {edit_context_files: $ecf, needs_diff_impact: $nd, nudge_counts: $nc}' \
    >"$state_file" 2>/dev/null || true
}

# Increment one orthogonal counter in the state file, preserving every other
# field (unlike save_state, which manages only the three gate fields). Backs
# the native-vs-CALM exploration tally — the "changing number" signal.
bump() {
  local prev='{}'
  [ -f "$state_file" ] && prev=$(cat "$state_file" 2>/dev/null || echo '{}')
  jq -c --arg k "$1" '.[$k] = ((.[$k] // 0) + 1)' <<<"$prev" >"$state_file" 2>/dev/null || true
}

# True when a Bash command's target is somewhere CALM never indexes (logs,
# dotdirs, build artifacts, /tmp, scratchpad) — a grep/find there has no CALM
# equivalent, so nudging toward search/locate would be crying wolf. Errs
# toward NOT nudging (credibility over coverage): a loose substring match.
command_targets_unindexed() {
  case "$1" in
    *.calm/* | *.log* | *target/* | *node_modules* | *.git/* | *dist/* | \
    *build/* | *__pycache__* | *venv/* | *tmp/* | *.claude/* | *scratchpad*)
      return 0 ;;
    *) return 1 ;;
  esac
}

# Formats the running native-vs-CALM exploration tally. Names the *specific*
# capability the native reads bypassed (session_context / blast-radius
# awareness) so the cost is concrete, not an abstract "prefer CALM".
explore_tally() {
  local s ne ce
  s=$(cat "$state_file" 2>/dev/null || echo '{}')
  ne=$(jq -r '.native_explore // 0' <<<"$s")
  ce=$(jq -r '.calm_explore // 0' <<<"$s")
  printf 'CALM liveness check — this session: %s native code reads/greps vs %s via CALM (search/source/locate). The %s native lookups never touched session_context, so blast-radius and "what have I already explored" awareness is blind to them. A CALM search/source keeps the map live at no extra cost.' \
    "$ne" "$ce" "$ne"
}

# Emitted at the commit checkpoint — a reflection point the agent never skips
# (diff_impact is hard-gated right before it), so a changing number lands here
# in a way a mid-flow nudge doesn't. No-op when nothing was explored natively.
emit_commit_tally() {
  local s ne ce
  s=$(cat "$state_file" 2>/dev/null || echo '{}')
  ne=$(jq -r '.native_explore // 0' <<<"$s")
  ce=$(jq -r '.calm_explore // 0' <<<"$s")
  [ "${ne:-0}" -eq 0 ] 2>/dev/null && return 0
  decision="commit_tally"
  decision_detail="native=$ne calm=$ce"
  jq -n --arg msg "About to commit — this session you explored code natively ${ne}x vs ${ce}x via CALM. Those ${ne} native reads/greps never updated the liveness map (session_context). Leaning on mcp__calm__search/source/locate next time keeps blast-radius awareness intact." \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $msg}}'
  exit 0
}

# Normalize a path to repo-relative form so the edit_context gate compares
# like with like. This is load-bearing: `mcp__calm__edit_context`'s `path`
# arg is repo-relative (CALM's convention), but Claude Code hands native
# Edit/Write an ABSOLUTE `file_path`. An earlier version exact-matched the
# two and thus false-denied every native Edit that a *relative* edit_context
# had legitimately unlocked — the exact cry-wolf this hook exists to avoid.
# Strips the git toplevel prefix from an absolute path; leaves an already
# -relative path (or one outside the repo) unchanged.
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

# True if `path` has had edit_context called for it THIS session — per-FILE
# state (not per-symbol: correlating each individual edit to a specific
# prior edit_context(symbol) call still isn't reliable from a shell hook,
# see this file's header comment; per-file *is* reliably trackable). Both
# the recorded set and the queried path are normalized to repo-relative via
# `to_repo_relative` first, so a relative edit_context and an absolute
# native Edit for the same file match.
file_has_edit_context() {
  local p; p=$(to_repo_relative "$1")
  jq -e --arg p "$p" 'index($p) != null' <<<"$edit_context_files" >/dev/null 2>&1
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

# After the per-kind nudge cap, a native exploration doesn't go fully silent:
# every TALLY_EVERY-th one surfaces the running native-vs-CALM tally instead.
# A *changing* number is the one signal that escapes the habituation NUDGE_CAP
# exists to avoid — it's a fact about the agent's own conduct this call, not a
# fixed sentence to tune out.
TALLY_EVERY=6

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
  save_state "$edit_context_files" "$needs_diff_impact" "$nudge_counts"
  decision="nudge"
  decision_detail="$key"
  jq -n --arg msg "$msg" \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $msg}}'
  exit 0
}

# Like nudge(), but for native *exploration* (Read/Grep/bash-grep/find on
# indexed code): under the cap it shows the actionable message; past the cap
# it falls back to the changing tally every TALLY_EVERY-th native lookup
# rather than pure silence. Assumes the caller already did `bump
# native_explore`, so the tally count reflects this call.
nudge_or_tally() {
  local key="$1" msg="$2" count ne
  count=$(jq -r --arg k "$key" '.[$k] // 0' <<<"$nudge_counts")
  if [ "$count" -lt "$NUDGE_CAP" ]; then
    nudge_counts=$(jq -c --arg k "$key" '.[$k] = ((.[$k] // 0) + 1)' <<<"$nudge_counts")
    save_state "$edit_context_files" "$needs_diff_impact" "$nudge_counts"
    decision="nudge"
    decision_detail="$key"
    jq -n --arg msg "$msg" \
      '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $msg}}'
    exit 0
  fi
  ne=$(jq -r '.native_explore // 0' <<<"$(cat "$state_file" 2>/dev/null || echo '{}')")
  if [ $((ne % TALLY_EVERY)) -eq 0 ] 2>/dev/null; then
    decision="tally"
    decision_detail="$key:native=$ne"
    jq -n --arg msg "$(explore_tally)" \
      '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $msg}}'
    exit 0
  fi
  decision="allow_nudge_capped"
  decision_detail="$key"
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
  local p="$1" check_path seg root
  # Outside the repo root entirely (e.g. a /tmp scratchpad file, a $HOME
  # dotfile): CALM only indexes under the project root, so it can never cover
  # this path — native Read/Edit is correct and edit_context can't resolve a
  # symbol here, so gating on it would cry wolf. Only for ABSOLUTE paths; a
  # relative path is by definition inside the cwd/repo.
  if [ "${p#/}" != "$p" ]; then
    root=$(git rev-parse --show-toplevel 2>/dev/null)
    if [ -n "$root" ] && [ "$p" != "$root" ] && [ "${p#"$root"/}" = "$p" ]; then
      return 0
    fi
  fi
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
  ec_path=$(jq -r '.tool_input.path // ""' <<<"$input")
  [ -n "$ec_path" ] && ec_path=$(to_repo_relative "$ec_path")
  decision_detail="state:edit_context_files+=${ec_path:-<any>}"
  if [ -n "$ec_path" ]; then
    edit_context_files=$(jq -c --arg p "$ec_path" '. + [$p] | unique' <<<"$edit_context_files")
  fi
  # No `path` given (symbol-name-only lookup, ambiguous across files) --
  # can't attribute to one file, so this call alone doesn't unlock any
  # file. Fails toward stricter (still enforcing the gate), not toward
  # silently trusting a guess.
  save_state "$edit_context_files" "$needs_diff_impact" "$nudge_counts"
  exit 0
fi
if [ "$tool_name" = "mcp__calm__diff_impact" ]; then
  # audit F6 (2026-07-12): the CALM-server-side gate (session_context's
  # pending_diff_impact) now only clears on a genuinely successful
  # diff_impact call — a failed one (bad input, git failure, DB error) no
  # longer falsely satisfies it. This hook-side gate CANNOT match that:
  # PreToolUse fires before the tool result is known, so there is no way
  # to tell success from failure here. Left as-is deliberately (defense
  # in depth, hook looser than server) rather than adding a PostToolUse
  # hook — see the AUDIT NOTE on Item 1.3 in
  # docs/plans/2026-07-12-upgrade-plan-1-correctness-safety.md for the
  # real residual gap this leaves (a failing diff_impact call still
  # satisfies this hook's gate) and why it's accepted for Plan 1's scope.
  decision_detail="state:needs_diff_impact=false"
  save_state "$edit_context_files" false "$nudge_counts"
  exit 0
fi
if [ "$tool_name" = "mcp__calm__edit_lines" ] || [ "$tool_name" = "mcp__calm__edit_symbol" ]; then
  # These are calm's own write path (AGENTS.md Stage 6) — a real file write
  # just like native Edit/Write, so the Stage 7 diff_impact gate below must
  # still apply. Not treated as satisfying edit_context_files: they carry
  # their own stricter per-call risk gate instead (see header comment).
  decision_detail="state:needs_diff_impact=true"
  save_state "$edit_context_files" true "$nudge_counts"
  exit 0
fi
# calm's own read/navigation tools — not gated, just counted, so the
# native-vs-CALM tally has a denominator (this hook's matcher in
# settings.json must list them for these to be observed at all).
case "$tool_name" in
  mcp__calm__search | mcp__calm__source | mcp__calm__locate | mcp__calm__file_overview | mcp__calm__understand)
    bump calm_explore
    decision="allow"
    decision_detail="calm_explore++"
    exit 0
    ;;
esac

case "$tool_name" in
  Read)
    if [ -n "$file_path" ] && is_definitely_unindexed_path "$file_path"; then
      : # ci has nothing indexed here (dotdir / build-artifact dir) — native Read is correct
    elif [ -z "$file_path" ] || is_code_file "$file_path"; then
      bump native_explore
      nudge_or_tally read_native "CI available in this repo — prefer mcp__calm__source(symbol) for a symbol-precise read, or mcp__calm__file_overview(path=\"${file_path}\") over reading the whole file (AGENTS.md Stage 3)."
    fi
    ;;
  Grep)
    grep_path=$(jq -r '.tool_input.path // ""' <<<"$input")
    grep_pat=$(jq -r '.tool_input.pattern // ""' <<<"$input")
    if [ -n "$grep_path" ] && is_definitely_unindexed_path "$grep_path"; then
      : # nothing indexed under this path — search/locate would come back empty, not stale
    elif [ -z "$grep_path" ] || ! is_clearly_non_code_file "$grep_path"; then
      bump native_explore
      nudge_or_tally grep_tool "CI available in this repo — prefer mcp__calm__search(query=\"${grep_pat}\", kind=\"hybrid\") or mcp__calm__locate(query=\"${grep_pat}\") for a symbol-aware search, or mcp__calm__search(query=\"${grep_pat}\", kind=\"grep\") for a literal match (also covers files the parser skips) instead of Grep (AGENTS.md Stage 2)."
    fi
    ;;
  Edit)
    decision_detail="state:needs_diff_impact=true"
    save_state "$edit_context_files" true "$nudge_counts"
    if is_definitely_unindexed_path "$file_path"; then
      : # CALM never indexes this path (dotdir/build-artifact) — edit_context
        # can't resolve a symbol here, so demanding it would be crying wolf
        # (this hook itself lives under .claude/ and used to hit exactly that).
        # The Stage 7 diff_impact gate above still applies to the write.
      decision="allow"
      decision_detail="edit_unindexed_path_no_edit_context_required"
    elif is_code_file "$file_path" && ! file_has_edit_context "$file_path"; then
      deny "MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) for a symbol in $file_path before editing it, never skip (especially if is_hub). edit_context was already called this session for other file(s), but not this one — each file needs its own call before its first native Edit. Also consider mcp__calm__edit_symbol/edit_lines (AGENTS.md Stage 6) instead of this native Edit — it can apply the change directly, hash-verified and risk-gated, chaining off edit_context's range_checksum."
    fi
    ;;
  Write)
    save_state "$edit_context_files" true "$nudge_counts"
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
      # Commit allowed (gate satisfied): a reflection point the agent never
      # skips — surface the session's native-vs-CALM tally here, where a
      # changing number actually lands, instead of only mid-flow.
      emit_commit_tally
    elif command_targets_unindexed "$command"; then
      : # grep/find over a log / dotdir / build-artifact / tmp path — not
        # indexed, CALM search would come back empty; native is correct
    elif grep -qE '\|[[:space:]]*(grep|rg|ag)\b' <<<"$command"; then
      : # piped grep = filtering another command's stream output, not
        # searching files — CALM search/locate can't replace it, and a
        # false nudge here erodes trust in every real one
    elif grep -qE '\b(grep|rg|ag)\b' <<<"$command"; then
      bump native_explore
      nudge_or_tally bash_grep 'CI available in this repo — prefer mcp__calm__search(query, kind="hybrid"|"grep") or mcp__calm__locate over a standalone file grep via Bash (AGENTS.md Stage 2).'
    elif grep -qE '\bfind\b.*-i?name\b' <<<"$command"; then
      bump native_explore
      nudge_or_tally bash_find 'CI available in this repo — prefer mcp__calm__search(query, kind="file") or mcp__calm__file_overview / mcp__calm__dependencies over find (AGENTS.md Stage 1-2).'
    fi
    ;;
esac
