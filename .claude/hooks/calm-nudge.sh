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
# 2026-07-14: this script is now wired to BOTH PreToolUse and PostToolUse
# (settings.json) for the Bash matcher — hook_event_name is the one common
# field (per code.claude.com/docs/en/hooks) that tells the two apart. Read
# by nudge()/nudge_or_tally() below so their emitted
# hookSpecificOutput.hookEventName always matches the event actually being
# answered, instead of the hardcoded "PreToolUse" they used before this was
# a dual-event script.
hook_event=$(jq -r '.hook_event_name // ""' <<<"$input")

# F5 (2026-07-14 audit-design mitigation, docs/superskills/specs/2026-07-14-
# calm-agent-experience-round2-fixes.md): true when this is a PreToolUse
# Bash call whose command provably never reaches a branch below that reads
# edit_context_files/needs_diff_impact/nudge_counts -- every Bash
# sub-branch this hook has (git commit/push, grep/rg/ag, find, rustfmt/
# cargo fmt) is gated on one of these exact words appearing (word-boundary,
# same technique those branches themselves use below), so "none of them
# appear" means none of those branches do anything with the parsed state
# either -- real measured overhead paid for zero signal otherwise (session
# 75bc8f50 alone: 385 Bash-triggered hook invocations, well under 15% of
# which touch any sub-branch that needs this state). Deliberately does NOT
# skip mkdir/decision_log/trap below (audit Failure Mode 3: log_decision
# must still fire on every invocation) -- only the session-state cat+3x jq
# parse a few lines down, which this function is checked immediately
# before. Conservative on purpose: a command containing one of these words
# for an unrelated reason (e.g. "cargo build" mentioning nothing here) just
# takes the slow path for no benefit, never the reverse.
bash_command_needs_no_session_state() {
  [ "$tool_name" = "Bash" ] || return 1
  [ "$hook_event" = "PostToolUse" ] && return 1
  ! grep -qE '\b(git|grep|rg|ag|find|rustfmt|fmt)\b' <<<"$command"
}

# --- session state (survives across PreToolUse calls within one session) ---
# .calm/ is already gitignored (see .gitignore) so state files never
# get committed; created defensively in case `ci init`/`ci serve` hasn't
# run yet in this session.
# Override point for test-calm-nudge.sh (2026-07-14 eval-set prerequisite):
# without this, the test harness fed synthetic tool_input payloads straight
# into the SAME decisions.jsonl real sessions write to — verified live, 156 of
# 612 real lines were test-*/-session-id fixtures, not organic traffic, and
# that fraction only grows every time the suite runs (in CI or locally). A
# future eval set sampling this log would need to already know to filter
# `session_id` matching `^test-`; isolating the writes here removes the need
# to filter after the fact at all.
state_dir="${CALM_NUDGE_STATE_DIR:-.calm/.hook-state}"
mkdir -p "$state_dir" 2>/dev/null || true
state_file="$state_dir/${session_id}.json"
# Opportunistic cleanup of stale state from old sessions. F5 (2026-07-14):
# probabilistic, not every invocation -- a maintenance task with no
# correctness requirement to run every time (unlike log_decision, which
# must), unlike a directory scan's cost which IS paid every time it runs.
# 1-in-20 still bounds unbounded growth of $state_dir without paying that
# scan on every single hook call.
if [ $((RANDOM % 20)) -eq 0 ]; then
  # DEBT-010 fix (L3 finding): *.json.lock siblings (introduced by
  # acquire_state_lock/release_state_lock below) weren't covered by the
  # original *.json-only glob -- would have accumulated as orphans forever
  # otherwise, a new self-inflicted debt on top of fixing the race itself.
  find "$state_dir" -maxdepth 1 \( -name '*.json' -o -name '*.json.lock' \) -mtime +1 -delete 2>/dev/null || true
fi

state='{}'
edit_context_files='[]'
needs_diff_impact=false
nudge_counts='{}'
if ! bash_command_needs_no_session_state; then
  if [ -f "$state_file" ]; then
    state=$(cat "$state_file" 2>/dev/null || echo '{}')
  fi
  edit_context_files=$(jq -c '.edit_context_files // []' <<<"$state" 2>/dev/null || echo '[]')
  needs_diff_impact=$(jq -r '.needs_diff_impact // false' <<<"$state" 2>/dev/null || echo false)
  nudge_counts=$(jq -c '.nudge_counts // {}' <<<"$state" 2>/dev/null || echo '{}')
fi

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
# F4 (2026-07-14 audit-design fix): a real Claude Code session_id is always
# a UUIDv4 -- verified against 6 real sessions in this repo's own
# decisions.jsonl before writing this check. CALM_NUDGE_STATE_DIR already
# isolates the automated test suite (test-calm-nudge.sh) into its own temp
# dir, so it's excluded here. What it does NOT cover: ad-hoc manual
# invocations during development (piping synthetic JSON straight to this
# script to verify one behavior, bypassing test-calm-nudge.sh entirely) --
# those fall through to the DEFAULT state_dir and, before this fix, wrote
# straight into the same decisions.jsonl real sessions use. Verified live:
# 12 of 17 distinct session_id values in this repo's real log were exactly
# this class of contamination ("audit-test", "manual-test-1303174",
# "b2-verify-854976" -- note the last one has no "test" substring at all, so
# a denylist on session_id shape would have missed it; this is a positive
# allowlist instead, which a new ad-hoc naming scheme can't silently evade).
# Routes to a SEPARATE file rather than dropping the line entirely --
# manual/test decisions are still useful to inspect locally, just not mixed
# into the log real-session analysis (e.g. a future eval set, see B3/F3)
# should be able to trust as "real traffic only".
is_real_session_id() {
  [[ "$1" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]
}
if [ -z "${CALM_NUDGE_STATE_DIR:-}" ] && ! is_real_session_id "$session_id"; then
  decision_log="$state_dir/decisions.jsonl.manual"
fi
decision="allow"
decision_detail=""
# Shadow-mode signal (2026-07-14): set to a nudge key (e.g. "read_native")
# whenever file_index-backed ground truth confirms a native Read/Grep hit a
# file CALM actually has indexed — i.e. exactly the case a FUTURE hard-deny
# gate (mirroring edit_context's) would block. Logged alongside the real
# (still advisory) decision so the false-positive rate can be measured from
# real sessions before ever flipping enforcement — never changes
# permissionDecision itself.
would_deny=""
# The query/pattern/command a Grep or Bash call actually ran — 2026-07-14
# eval-set prerequisite (docs/plans/2026-07-14-search-grep-steepest-hill-followups.md):
# without this, decisions.jsonl's existing `file_path` field is ALWAYS null for
# Grep/Bash (they key their tool_input as `pattern`/`path` or `command`, never
# `file_path` — verified against real log output), which made it impossible to
# reconstruct "what was this call actually looking for" for later hand-labeling.
# Set by the Grep/Bash case branches below; left empty (logs as null) for every
# other tool_name. Truncated in log_decision itself (not at the assignment
# site) so every writer gets the cap for free, and capped short specifically to
# keep decisions.jsonl from ballooning and to limit (not eliminate — a secret
# in the first 200 chars of a Bash command would still land here) incidental
# sensitive-content exposure in a log file kept outside git.
decision_query=""
log_decision() {
  local query_snip="${decision_query:0:200}"
  jq -nc \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg session "$session_id" \
    --arg tool "$tool_name" \
    --arg decision "$decision" \
    --arg detail "$decision_detail" \
    --arg file "$file_path" \
    --arg would_deny "$would_deny" \
    --arg query "$query_snip" \
    '{ts: $ts, session_id: $session, tool_name: $tool, decision: $decision, detail: $detail, file_path: ($file | select(. != "") // null), would_deny: ($would_deny | select(. != "") // null), query: ($query | select(. != "") // null)}' \
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

# DEBT-010 fix (2026-07-14, docs/pattern-debt-registry.yaml; audit-design
# PASS WITH FLAGS, docs/superskills/specs/2026-07-14-calm-remaining-
# backlog-read-hardening-and-toctou.md): save_state/bump/
# maybe_nudge_session_context all read-modify-write the same
# $state_file ("cat" + jq + redirect, no lock between the read and the
# write) — a real race, not theoretical: measured directly against this
# repo's own decisions.jsonl, 92 distinct (timestamp, session_id) seconds
# where 2+ hook invocations landed in the exact same wall-clock second,
# consistent with this harness's own parallel tool-call dispatch within one
# assistant turn. fd-based flock, NOT `flock -c "..."` — that form runs the
# wrapped command in a subshell, which would silently drop any variable the
# command needs to set for its caller (L2 finding from the audit that
# reviewed this exact fix). 2s timeout, fails OPEN (proceeds unlocked
# rather than blocking a tool call indefinitely on contention) — every
# field these three functions write is an advisory-only counter/cache
# (nudge_counts, edit_context_files, since_session_context); no currently-
# shipped hard gate depends on one of these updates surviving a lost race,
# so an occasional dropped increment is the acceptable failure mode, a hung
# hook invocation is not.
STATE_LOCK_FD=""
# 2026-07-14, found while migrating deny() to exit-2 (see deny()'s own
# comment): `exec {FD}>file 2>/dev/null` -- a bare `exec` with ONLY
# redirects, no command -- applies EVERY one of its redirects to the
# CURRENT SHELL PERMANENTLY, not just to a single command. The trailing
# `2>/dev/null` here was meant to suppress an error message if opening the
# lock file itself failed, but it silently, permanently redirected this
# script's real stderr to /dev/null for the rest of the process the very
# first time this ran successfully -- invisible until now purely because
# nothing previously read this script's stderr (the old deny()
# communicated over stdout instead). Wrapping the exec in a `{ ...; }`
# group scopes `2>/dev/null` to just that group (bash saves/restores the
# fd around a redirected group, unlike a bare `exec`), while the
# `{STATE_LOCK_FD}>file` redirect inside it still applies permanently as
# intended -- that's the one part of this we actually want to persist
# until release_state_lock closes it.
acquire_state_lock() {
  { exec {STATE_LOCK_FD}>"${state_file}.lock"; } 2>/dev/null || { STATE_LOCK_FD=""; return; }
  flock -w 2 "$STATE_LOCK_FD" 2>/dev/null || true
}
release_state_lock() {
  [ -n "$STATE_LOCK_FD" ] || return 0
  flock -u "$STATE_LOCK_FD" 2>/dev/null || true
  { exec {STATE_LOCK_FD}>&-; } 2>/dev/null || true
  STATE_LOCK_FD=""
}

save_state() {
  # Merge onto existing state so orthogonal exploration counters
  # (native_explore / calm_explore, maintained by `bump`) survive a save of
  # the three gate-relevant fields — without the merge every save would wipe
  # them, silently zeroing the tally.
  acquire_state_lock
  local prev='{}'
  [ -f "$state_file" ] && prev=$(cat "$state_file" 2>/dev/null || echo '{}')
  jq -n --argjson prev "$prev" --argjson ecf "$1" --argjson nd "$2" --argjson nc "$3" \
    '$prev + {edit_context_files: $ecf, needs_diff_impact: $nd, nudge_counts: $nc}' \
    >"$state_file" 2>/dev/null || true
  release_state_lock
}

# Increment one orthogonal counter in the state file, preserving every other
# field (unlike save_state, which manages only the three gate fields). Backs
# the native-vs-CALM exploration tally — the "changing number" signal.
bump() {
  acquire_state_lock
  local prev='{}'
  [ -f "$state_file" ] && prev=$(cat "$state_file" 2>/dev/null || echo '{}')
  jq -c --arg k "$1" '.[$k] = ((.[$k] // 0) + 1)' <<<"$prev" >"$state_file" 2>/dev/null || true
  release_state_lock
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

# --- 2026-07-14 "steepest hill" redesign: conditional enforcement for
# search/grep specifically ---
#
# A user-supplied analysis (grounded in MCP-Bench data + a documented Claude
# Code community report that even Anthropic's OWN Grep/Read native tools get
# bypassed for raw Bash `cat`/`grep` without a blocking hook) argued that
# search/grep is the single steepest hill to nudge, for three compounding
# reasons: (1) "type a query -> reach for grep" is a pretraining-level
# reflex, reinforced a SECOND time by Claude Code's own system prompt
# training a model to prefer ITS OWN native Grep/Read over raw Bash --
# meaning an MCP tool has to out-compete two stacked habits, not one, unlike
# Edit/Write which face only the second; (2) search/grep fires 10-40x more
# often per session than edit/write (MCP-Bench: ~20-80 tool calls/task), so
# even an IDENTICAL per-call compliance rate produces a visibly worse
# aggregate ratio purely from volume; (3) a mutating action gets more model
# "caution" (slow down, re-read guidance) than a read-only one, which gets
# processed as a cheap reflexive action.
#
# The actionable recommendation, and the one implemented below: don't treat
# compliance as one blob. Nudge (or, if ever hardened, deny) ONLY the case
# CALM actually wins clearly -- multi-file/repo-wide scope, or a
# symbol/identifier-shaped query even when scoped to one file (locate
# returns definition+callers+type, strictly more than a text match). Stay
# SILENT for a single-file grep with a real regex/free-text pattern --
# ripgrep genuinely has no CALM equivalent there, and nudging it anyway is
# exactly the false-positive class this file's header (lever #1) already
# identifies as corrosive to every OTHER, genuinely-useful nudge. This also
# directly serves the frequency-effect problem above: fewer, better-targeted
# nudges land more of NUDGE_CAP's two "full message" shows on cases that
# actually matter, instead of burning them on trivial single-file lookups.

# True when `pat` looks like a plain identifier (a symbol/function/class
# name) rather than a real regex or free-text phrase -- mcp__calm__locate
# gives strictly more (definition + callers + type) than a text match for
# exactly this query shape, regardless of file scope, so it's worth nudging
# even when grep_path names one specific file.
pattern_looks_like_identifier() {
  [[ "$1" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]
}

# True when a Bash grep/rg/ag invocation clearly targets ONE existing
# regular file rather than a directory/recursive scope -- the case where
# mcp__calm__search has no real leverage over native (same one-round-trip
# cost, no query-translation step saved). Heuristic, not a shell parser
# (same tolerance as is_real_git_commit_or_push above): any `-r`/`-R`/
# `--recursive` flag, any directory argument, or anything other than
# EXACTLY one existing-regular-file token defaults to "multi-file" (nudge),
# since an ambiguous parse should fail toward the existing, already-safe
# behavior, not toward new silence.
bash_grep_targets_single_file() {
  local cmd="$1" tok file_count=0
  case "$cmd" in
    *-r* | *-R* | *--recursive*) return 1 ;;
  esac
  for tok in $cmd; do
    case "$tok" in
      -*) continue ;;
    esac
    if [ -d "$tok" ]; then
      return 1
    elif [ -f "$tok" ]; then
      file_count=$((file_count + 1))
    fi
  done
  [ "$file_count" -eq 1 ]
}

# A short file is cheap to read in full either way -- source(symbol) or
# file_overview only pay off once reading the whole file is real waste
# relative to what was actually needed. 80 lines: roughly "longer than one
# screen", not a CALM-internal number with any other significance.
READ_WORTH_NUDGE_LINE_THRESHOLD=80
file_worth_symbol_read() {
  local n
  n=$(wc -l <"$1" 2>/dev/null || echo 0)
  [ "${n:-0}" -ge "$READ_WORTH_NUDGE_LINE_THRESHOLD" ] 2>/dev/null
}

# Appended to every "prefer mcp__calm__X" nudge message: some MCP clients
# (this environment's harness among them) defer a server's tool schemas
# until an explicit discovery step requests them, so "prefer
# mcp__calm__search" can be technically correct advice that's still
# impossible to act on if that tool was never loaded into context this
# session -- the agent has no way to distinguish "this tool doesn't exist"
# from "this tool exists but I never asked for its schema" without being
# told the second case is possible. Deliberately does NOT name a specific
# client mechanism (e.g. "call ToolSearch") -- that would be actively wrong
# guidance in a client that loads every tool upfront (the majority of MCP
# clients today) and have no way to detect which kind of client is running
# from inside a shell hook. Generic enough to be correct everywhere, still
# concrete enough to prompt a real check instead of silent non-compliance.
TOOL_DISCOVERY_HINT=" (If mcp__calm__* tools don't appear in your available tools yet this session, your MCP client may defer tool schemas until requested — check for a tool-discovery step before assuming they're unavailable.)"

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

# 2026-07-14 self-audit finding: session_context exists specifically to
# surface "have I made real progress lately" (calls_since_progress /
# possibly_stuck), "what's still unverified" (files_pending_diff_impact),
# and "who else is here" (other_active_sessions) — exactly the awareness a
# long multi-hour session needs. But nothing in this hook ever pushed
# toward it: the only existing reference (emit_commit_tally, above) cites
# it as a REASON to prefer CALM search over native reads, not as a
# standalone thing to go call. Verified live on a real ~4-hour, 4-deliverable
# session: session_context was never called once, despite comfortably
# clearing the 10-call "possibly_stuck" threshold session_context computes
# server-side — a well-designed signal that stayed completely invisible
# because nothing ever prompted checking it.
#
# SESSION_CONTEXT_REMINDER_EVERY is deliberately higher than TALLY_EVERY
# (6): this hook only fires on a SUBSET of real tool calls (the
# settings.json PreToolUse matcher — Read/Edit/Write/Grep/Bash plus a
# handful of mcp__calm__* tools; repo_overview/hotspots/callers/callees/
# TodoWrite/Agent/etc never reach it), so the true per-session call count
# always runs higher than what `since_session_context` observes. 25 is a
# deliberately conservative proxy for "a genuinely long stretch", not a
# literal call-count threshold.
SESSION_CONTEXT_REMINDER_EVERY=25

# Called once, unconditionally, at the very end of the dispatch below (after
# `esac`) — every earlier path that had something more important to say
# already `exit 0`'d before reaching here, so this can never suppress or
# outrank a real deny/nudge; it only ever fires on an otherwise-silent
# "allow" call. Resets the counter on an actual session_context call
# (acknowledging the reminder, not just receiving it) rather than on any
# CALM tool call, so the reminder can't be silenced by unrelated activity.
#
# Reads/writes the state file FRESH from disk (like `bump`, NOT via the
# `$state` snapshot loaded once at script start) — this function always
# runs last, after any `bump native_explore`/`bump calm_explore` earlier in
# this same invocation already wrote their own fresh read-modify-write to
# disk; using the stale start-of-script `$state` here would silently
# clobber those with an outdated copy.
maybe_nudge_session_context() {
  # DEBT-010 fix: same shared state-file lock as save_state/bump — this
  # function does its own independent read-modify-write cycle on
  # $state_file (by design, see its own header comment above), so it
  # shares the exact same race and needs the exact same protection.
  # Released before EITHER exit point below (not just at the bottom) so
  # the lock is never held past the last write in a given call.
  acquire_state_lock
  local prev
  prev=$(cat "$state_file" 2>/dev/null || echo '{}')
  if [ "$tool_name" = "mcp__calm__session_context" ]; then
    if [ "$(jq -r '.since_session_context // 0' <<<"$prev")" != "0" ]; then
      jq -c '.since_session_context = 0' <<<"$prev" >"$state_file" 2>/dev/null || true
    fi
    release_state_lock
    return 0
  fi
  local n
  n=$(jq -r '((.since_session_context // 0) + 1)' <<<"$prev")
  jq -c --argjson n "$n" '.since_session_context = $n' <<<"$prev" >"$state_file" 2>/dev/null || true
  release_state_lock
  if [ $((n % SESSION_CONTEXT_REMINDER_EVERY)) -eq 0 ]; then
    decision="session_context_reminder"
    decision_detail="since_session_context=$n"
    jq -n --arg n "$n" --arg evt "${hook_event:-PreToolUse}" \
      '{hookSpecificOutput: {hookEventName: $evt, additionalContext: ("This session has reached " + $n + " CALM-visible tool calls without a session_context check. It reports calls-since-progress (\"possibly_stuck\"), files still pending diff_impact, and other active sessions on this daemon — call mcp__calm__session_context() to check in.")}}'
    exit 0
  fi
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

# Ground-truth (not guessed) check: does CALM's own index actually track this
# exact file? `file_index` (`.calm/index.db`) is keyed by repo-relative path
# (PRIMARY KEY, so this is a single fast lookup) and is populated for EVERY
# walked file regardless of language or symbol count — the same table
# `repo_overview`'s `total_files` and `file_overview` draw from. Found live
# 2026-07-14 investigating why a native Read on AGENTS.md (293 lines, 13
# indexed heading symbols) never even got nudged: the OLD path-shape guess
# (`is_code_file`'s extension allowlist) doesn't include `.md`, so any
# indexed markdown/docs file was silently invisible to the Read/Grep nudge —
# a false NEGATIVE, the opposite failure from is_definitely_unindexed_path's
# occasional false positives. This replaces guessing with certainty for the
# "is this specific file indexed" question; `is_definitely_unindexed_path`
# stays in use as the fallback for paths this can't confirm either way
# (not yet indexed, or DB unavailable) so existing coverage never shrinks.
is_indexed_file() {
  local p; p=$(to_repo_relative "$1")
  local db=".calm/index.db"
  [ -f "$db" ] || return 1
  local escaped="${p//\'/\'\'}"
  local hit
  hit=$(sqlite3 -readonly "$db" "SELECT 1 FROM file_index WHERE path = '$escaped' LIMIT 1;" 2>/dev/null || true)
  [ "$hit" = "1" ]
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

# Denies the current PreToolUse call (Stage 5 edit_context, Stage 7
# diff_impact -- see this file's own header and this function's call
# sites). Signals via exit code 2 + stderr, not the JSON
# `permissionDecision: "deny"` + `exit 0` form this used until 2026-07-14:
# verified externally (`gh issue view`, not a blog summary --
# docs/superskills/specs/2026-07-14-calm-mcp-external-onboarding.md's Item
# B external-research addendum) that the JSON form has a real, repeated,
# multi-version bug history in anthropics/claude-code -- #4669 (deny
# silently ignored, tool ran anyway, 3 independent reporters, never
# confirmed fixed, auto-closed by an inactivity bot) and #39344 (a
# DIFFERENT hook's "ask" silently overrode a static permissions.deny rule,
# fixed only in v2.1.101). Exit code 2 is the more primitive mechanism
# (official docs: "Exit 2 means a blocking error... stderr text is fed
# back to Claude... PreToolUse blocks the tool call") and sidesteps the
# whole JSON-output interaction surface those bugs lived in.
#
# Still best-effort, not a claim of unbypassable enforcement -- Anthropic's
# own docs say to prefer the static permission system over hooks for hard
# enforcement, precisely because hooks have this bug history. CALM's
# per-file/per-session contextual logic ("has edit_context run for THIS
# file THIS session") can't be expressed as a static permission rule, so a
# hook is what's available here, used honestly as defense-in-depth rather
# than a guarantee nothing can bypass it.
deny() {
  decision="deny"
  decision_detail="$1"
  echo "$1" >&2
  exit 2
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
#
# 2026-07-14 backlog B2: NUDGE_CAP was counting *how many times shown* as a
# proxy for "lesson learned", then going FULLY AND PERMANENTLY silent past
# it — including no longer incrementing nudge_counts, so the count itself
# froze at NUDGE_CAP forever and there was no way to tell "shown twice, never
# happened again" apart from "shown twice, then happened 50 more times
# silently". Fixed the same way nudge_or_tally already handles this for
# native-explore: keep counting past the cap, and resurface every
# TALLY_EVERY-th occurrence with the actual count instead of staying silent
# forever. Deliberately NOT auto-escalating to a deny here (that would skip
# the shadow-mode measurement step already agreed for Read/Grep in Part A of
# the backlog doc) — this only ever changes message cadence, never
# permissionDecision.
nudge() {
  local key="$1" msg="$2" count shown
  count=$(jq -r --arg k "$key" '.[$k] // 0' <<<"$nudge_counts")
  nudge_counts=$(jq -c --arg k "$key" '.[$k] = ((.[$k] // 0) + 1)' <<<"$nudge_counts")
  save_state "$edit_context_files" "$needs_diff_impact" "$nudge_counts"
  if [ "$count" -lt "$NUDGE_CAP" ]; then
    decision="nudge"
    decision_detail="$key"
    jq -n --arg msg "$msg" --arg evt "${hook_event:-PreToolUse}" \
      '{hookSpecificOutput: {hookEventName: $evt, additionalContext: $msg}}'
    exit 0
  fi
  shown=$((count + 1))
  if [ $((shown % TALLY_EVERY)) -eq 0 ]; then
    decision="nudge_repeat_tally"
    decision_detail="$key:count=$shown"
    jq -n --arg msg "$msg" --arg n "$shown" --arg evt "${hook_event:-PreToolUse}" \
      '{hookSpecificOutput: {hookEventName: $evt, additionalContext: ("This is occurrence #" + $n + " of the same situation this session, without a change in approach: " + $msg)}}'
    exit 0
  fi
  decision="allow_nudge_capped"
  decision_detail="$key"
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
    jq -n --arg msg "$msg" --arg evt "${hook_event:-PreToolUse}" \
      '{hookSpecificOutput: {hookEventName: $evt, additionalContext: $msg}}'
    exit 0
  fi
  ne=$(jq -r '.native_explore // 0' <<<"$(cat "$state_file" 2>/dev/null || echo '{}')")
  if [ $((ne % TALLY_EVERY)) -eq 0 ] 2>/dev/null; then
    decision="tally"
    decision_detail="$key:native=$ne"
    jq -n --arg msg "$(explore_tally)" --arg evt "${hook_event:-PreToolUse}" \
      '{hookSpecificOutput: {hookEventName: $evt, additionalContext: $msg}}'
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

# F2-revised (2026-07-14 audit-design fix): deliberately NARROWER than
# is_clearly_non_code_file above -- this gates the Edit/Write hard-DENY
# (blast-radius ceremony), not the Grep/search advisory nudge, so it needs a
# provable-safe justification, not just "not real code". Proven against
# crates/calm-core/src/graph/hub.rs::update_is_hub_flags: its hub query
# source is `WHERE caller_count >= 1 OR coreness > 0`, and a Markdown
# heading symbol (`kind='heading'`, see
# crates/calm-core/src/indexer/parser.rs's markdown extraction) never gets a
# call-graph edge, so caller_count/coreness are always 0 -- is_hub is
# provably always false for prose. That proof does NOT extend to
# .yml/.json/.toml/.lock: a broken CI workflow YAML or a broken Cargo.lock
# has real operational blast radius CALM's call graph simply doesn't model,
# which is a different risk shape from "a doc heading nobody calls" --
# is_clearly_non_code_file's broader denylist is the wrong proxy for THIS
# gate specifically (audit-design Failure Mode 2, see
# docs/superskills/specs/2026-07-14-calm-agent-experience-round2-fixes.md).
# Kept as its own function rather than folding into is_clearly_non_code_file
# so the two concerns can't silently drift back together if either list
# changes later -- see that function's own doc comment for why it needs to
# stay a denylist for its purpose too.
is_prose_file() {
  case "$1" in
    *.md | *.txt)
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

# --- PostToolUse: just-in-time nudge grounded in the REAL result of a Bash
# grep/rg/ag call, not a pre-call guess (2026-07-14 follow-up to the
# "steepest hill" redesign — docs/plans/2026-07-14-search-grep-steepest-hill-followups.md,
# "PostToolUse just-in-time nudge"). Built only after capturing and reading a
# REAL payload from this exact repo/hook (.calm/.hook-state/posttooluse-grep-dump.jsonl,
# via the still-wired posttooluse-discovery-dump.sh), not against the official
# docs example alone — that example shows Bash's tool_response as a plain
# string, but the real captured payload here is a structured object:
# {stdout, stderr, interrupted, isImage, noOutputExpected,
# [returnCodeInterpretation]}. Confirmed live: (a) PostToolUse fires
# regardless of grep's own exit code (0 or 1) as long as the Bash tool call
# itself doesn't error; (b) `returnCodeInterpretation: "No matches found"`
# appears on at least some zero-match cases — a free, pre-labeled signal,
# used here with an empty-stdout fallback for when it's absent; (c) explicit
# `--color=always` leaks raw ANSI straight into stdout with no equivalent of
# the native Grep tool's per-match `ansi_codes` flag — stripped defensively
# below even though grep's own default (--color=auto) never colorizes into a
# non-tty pipe in practice.
#
# The native `Grep` tool's own tool_response.matches[] schema (structured:
# path/line_number/line_text/ansi_codes) is doc-derived only
# (code.claude.com/docs/en/hooks) and was NOT live-verified — this harness has
# no native Grep tool to call. So this function deliberately handles ONLY the
# Bash path; a real Grep-tool PostToolUse event still just falls through to
# posttooluse-discovery-dump.sh (still wired in settings.json) until a real
# payload from an environment that has the tool confirms its shape too.
#
# Owns bump(native_explore) + the nudge for "multi-file/zero-match bash
# grep" EXCLUSIVELY. The old PreToolUse Bash branch used to nudge on this
# same condition pre-call (key "bash_grep", generic message, no result
# grounding) — that branch is retired below (see its comment) specifically
# so one real call is never nudged twice (once generic pre-call, once
# grounded post-call) and native_explore is never double-counted for it.
handle_post_tool_use() {
  [ "$tool_name" = "Bash" ] || return 0
  is_real_git_commit_or_push "$command" && return 0
  command_targets_unindexed "$command" && return 0
  grep -qE '\|[[:space:]]*(grep|rg|ag)\b' <<<"$command" && return 0
  grep -qE '\b(grep|rg|ag)\b' <<<"$command" || return 0
  if bash_grep_targets_single_file "$command"; then
    decision_detail="post:bash_grep_single_file_silent"
    return 0
  fi

  decision_query="$command"
  local resp stdout_text rc_note file_count
  resp=$(jq -c '.tool_response // {}' <<<"$input")
  stdout_text=$(jq -r 'if type == "object" then (.stdout // "") else (. // "") end' <<<"$resp")
  rc_note=$(jq -r 'if type == "object" then (.returnCodeInterpretation // "") else "" end' <<<"$resp")
  # Strip ANSI before counting — see header comment (c).
  stdout_text=$(sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g' <<<"$stdout_text")

  bump native_explore
  if [ "$rc_note" = "No matches found" ] || [ -z "$stdout_text" ]; then
    decision_detail="post:bash_grep_zero_match"
    nudge_or_tally bash_grep_post "CI available in this repo — that repo-wide Bash grep just matched nothing. A literal grep misses renamed/differently-worded symbols that mcp__calm__search(query, kind=\"hybrid\") or mcp__calm__locate can still find (AGENTS.md Stage 2).${TOOL_DISCOVERY_HINT}"
    return 0
  fi
  file_count=$(awk -F: '{print $1}' <<<"$stdout_text" | sort -u | grep -c .)
  if [ "${file_count:-0}" -ge 3 ]; then
    decision_detail="post:bash_grep_multi_file=$file_count"
    nudge_or_tally bash_grep_post "CI available in this repo — that Bash grep just matched across ${file_count} files. mcp__calm__search(query, kind=\"hybrid\"|\"grep\") returns all of them ranked in one call instead (AGENTS.md Stage 2).${TOOL_DISCOVERY_HINT}"
  else
    decision_detail="post:bash_grep_few_files=${file_count:-0}_silent"
  fi
}

# PostToolUse events short-circuit here, before any of the PreToolUse-only
# logic below (edit_context tracking, diff_impact gating, etc. all assume a
# PreToolUse call's semantics — e.g. "block this" — which don't apply once
# the tool has already run). handle_post_tool_use itself calls
# nudge_or_tally, which exits the script directly when it emits a message;
# the explicit `exit 0` here only covers the silent/no-op paths through it.
if [ "$hook_event" = "PostToolUse" ]; then
  decision="post_tool_use"
  handle_post_tool_use
  exit 0
fi

# Record write-relevant mcp__calm__* calls as they happen — recorded on
# PreToolUse (before the call runs) since attempting the check is what
# matters here, and PreToolUse is all that's needed to observe it.
if [ "$tool_name" = "mcp__calm__edit_context" ]; then
  ec_path=$(jq -r '.tool_input.path // ""' <<<"$input")
  ec_symbol=$(jq -r '.tool_input.symbol // ""' <<<"$input")
  [ -n "$ec_path" ] && ec_path=$(to_repo_relative "$ec_path")
  # `path` is legitimately optional on edit_context (only needed to
  # disambiguate a same-named symbol across files -- see EditContextParams'
  # own doc comment), so an agent calling it the normal way for a unique
  # symbol name has no reason to pass it. But this hook only ever sees the
  # REQUEST at PreToolUse, never the tool's response (Claude Code hooks
  # don't expose tool_response here the way a PostToolUse hook would) --
  # so without this fallback, edit_context_files silently never gains that
  # file, and the very next native Edit on it is falsely denied even though
  # edit_context genuinely ran and resolved it. Mirror
  # resolve_symbol_candidates' own query (crates/calm-server/src/tools/
  # common.rs) read-only against the same DB the server queries: if $db has
  # exactly one row named $ec_symbol, that IS the file the real call just
  # resolved to (same table, same `name = ?` match, same "exactly one row"
  # criterion `resolve_symbol` uses to decide Found vs. Ambiguous) -- record
  # it. Any other row count (0 or >1) means the real call either found
  # nothing or came back `ambiguous` (no symbol actually resolved), so
  # staying silent here is correct, not just conservative.
  if [ -z "$ec_path" ] && [ -n "$ec_symbol" ]; then
    db=".calm/index.db"
    if [ -f "$db" ]; then
      escaped=${ec_symbol//\'/\'\'}
      rows=$(sqlite3 -readonly -separator '|' "$db" \
        "SELECT path FROM symbols WHERE name = '$escaped';" 2>/dev/null || true)
      row_count=$(printf '%s\n' "$rows" | grep -c . 2>/dev/null || echo 0)
      if [ "${row_count:-0}" -eq 1 ] 2>/dev/null; then
        ec_path=$(to_repo_relative "$rows")
      fi
    fi
  fi
  decision_detail="state:edit_context_files+=${ec_path:-<any>}"
  if [ -n "$ec_path" ]; then
    edit_context_files=$(jq -c --arg p "$ec_path" '. + [$p] | unique' <<<"$edit_context_files")
  fi
  save_state "$edit_context_files" "$needs_diff_impact" "$nudge_counts"
  # Silent state-update exit, same reasoning as the calm-read-tools branch
  # above: this would otherwise never reach the tail's own call, and
  # edit_context is exactly the kind of frequent, otherwise-silent call a
  # long edit-heavy session is dominated by.
  maybe_nudge_session_context
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
  # diff_impact is arguably the single best natural checkpoint for this
  # reminder — an agent that just finished verifying blast radius is
  # exactly at a reflection point, same spirit as emit_commit_tally firing
  # at the commit checkpoint.
  maybe_nudge_session_context
  exit 0
fi
if [ "$tool_name" = "mcp__calm__edit_lines" ] || [ "$tool_name" = "mcp__calm__edit_symbol" ]; then
  # These are calm's own write path (AGENTS.md Stage 6) — a real file write
  # just like native Edit/Write, so the Stage 7 diff_impact gate below must
  # still apply. Not treated as satisfying edit_context_files: they carry
  # their own stricter per-call risk gate instead (see header comment).
  decision_detail="state:needs_diff_impact=true"
  save_state "$edit_context_files" true "$nudge_counts"
  maybe_nudge_session_context
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
    # This branch always exits below, so it would otherwise never reach
    # maybe_nudge_session_context's own call at the very end of this
    # script — a real gap: these 5 tools are the single most common calls
    # in a typical session (verified live: they dominated a real ~4-hour
    # session that never once reached the tail as a result), so skipping
    # this check here would leave the reminder unable to fire for exactly
    # the sessions it matters most for. Checked here explicitly instead —
    # still only emits if the threshold is actually hit, still a plain
    # return (not an exit) otherwise, so the bare "allow" below is
    # unaffected on every other call.
    maybe_nudge_session_context
    exit 0
    ;;
esac

case "$tool_name" in
  Read)
    # Ground truth (is_indexed_file, file_index table) takes priority over the
    # path-shape guesses below: if CALM confirms this exact file is indexed,
    # nudge AND record would_deny (2026-07-14 shadow-mode measurement for a
    # possible future hard gate — see is_indexed_file's doc comment). This
    # also fixes a real false negative the old is_code_file-only check had:
    # a `.md` file (e.g. AGENTS.md itself, 13 indexed heading symbols) was
    # never nudged before, since `.md` isn't in is_code_file's extension list.
    # Falls back to the pre-existing path-shape heuristics when file_index
    # can't confirm either way (not yet (re)indexed, or DB unavailable) so
    # existing coverage never shrinks.
    should_nudge=false
    if [ -n "$file_path" ]; then
      if is_indexed_file "$file_path"; then
        # 2026-07-14 conditional-enforcement redesign: a short file is cheap
        # to read whole either way, so only nudge once the file is long
        # enough that source(symbol)/file_overview's savings are real (see
        # this file's "steepest hill" header block above).
        if file_worth_symbol_read "$file_path"; then
          should_nudge=true
          would_deny="read_native"
        fi
      elif is_definitely_unindexed_path "$file_path"; then
        : # ci has nothing indexed here (dotdir / build-artifact dir) — native Read is correct
      elif is_code_file "$file_path" && file_worth_symbol_read "$file_path"; then
        should_nudge=true
      fi
    else
      should_nudge=true
    fi
    if [ "$should_nudge" = true ]; then
      bump native_explore
      nudge_or_tally read_native "CI available in this repo — prefer mcp__calm__source(symbol) for a symbol-precise read, or mcp__calm__file_overview(path=\"${file_path}\") over reading the whole file (AGENTS.md Stage 3).${TOOL_DISCOVERY_HINT}"
    fi
    ;;
  Grep)
    grep_path=$(jq -r '.tool_input.path // ""' <<<"$input")
    grep_pat=$(jq -r '.tool_input.pattern // ""' <<<"$input")
    decision_query="$grep_pat"
    # Same ground-truth-first structure as Read above, now also gated on
    # whether CALM actually has leverage here (2026-07-14 redesign, see
    # "steepest hill" header block above): multi-file/repo-wide scope, or a
    # symbol-shaped pattern even in one file. A single-file grep for a real
    # regex/free-text phrase is left silent — native genuinely wins there.
    should_nudge=false
    calm_has_leverage=false
    if [ -z "$grep_path" ] || [ -d "$grep_path" ] || pattern_looks_like_identifier "$grep_pat"; then
      calm_has_leverage=true
    fi
    if [ -n "$grep_path" ]; then
      if is_indexed_file "$grep_path"; then
        if [ "$calm_has_leverage" = true ]; then
          should_nudge=true
          would_deny="grep_tool"
        fi
      elif is_definitely_unindexed_path "$grep_path"; then
        : # nothing indexed under this path — search/locate would come back empty, not stale
      elif ! is_clearly_non_code_file "$grep_path" && [ "$calm_has_leverage" = true ]; then
        should_nudge=true
      fi
    elif [ "$calm_has_leverage" = true ]; then
      should_nudge=true
    fi
    if [ "$should_nudge" = true ]; then
      bump native_explore
      nudge_or_tally grep_tool "CI available in this repo — prefer mcp__calm__search(query=\"${grep_pat}\", kind=\"hybrid\") or mcp__calm__locate(query=\"${grep_pat}\") for a symbol-aware search, or mcp__calm__search(query=\"${grep_pat}\", kind=\"grep\") for a literal match (also covers files the parser skips) instead of Grep (AGENTS.md Stage 2).${TOOL_DISCOVERY_HINT}"
    fi
    ;;
  Edit)
    decision_detail="state:needs_diff_impact=true"
    save_state "$edit_context_files" true "$nudge_counts"
    # Ground truth (is_indexed_file) takes priority over the path-shape amnesty
    # below: a source file whose path happens to contain a segment that LOOKS
    # like an ignored dir name (e.g. a Rust module literally named `target`)
    # would otherwise slip through is_definitely_unindexed_path's guess and
    # skip the edit_context requirement entirely -- a real safety gap (missing
    # a mandatory pre-edit check), not just nudge noise. If file_index confirms
    # this file IS tracked, the requirement below always applies regardless of
    # what the amnesty heuristic would have guessed (2026-07-14 B3 audit).
    if is_indexed_file "$file_path"; then
      if ! file_has_edit_context "$file_path"; then
        # F2-revised (2026-07-14 audit-design fix): a prose file (.md/.txt)
        # never has a real blast radius to review (see is_prose_file's doc
        # comment — hub.rs proves caller_count/coreness are always 0 for a
        # Markdown heading), so the mandatory hard-deny ceremony downgrades
        # to an advisory nudge here instead. Deliberately NOT
        # is_clearly_non_code_file (which also matches .yml/.json/.toml/
        # .lock — files that CAN carry real operational blast radius CALM's
        # call graph doesn't model) — see is_prose_file's own doc comment.
        if is_prose_file "$file_path"; then
          nudge edit_prose_no_context "RECOMMENDED per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) for a heading in $file_path before editing it. Not mandatory here: Markdown/text headings never carry call-graph edges, so edit_context's blast-radius check is always trivially empty for prose (is_hub is provably always false for a doc heading). Also consider mcp__calm__edit_lines (AGENTS.md Stage 6) instead of this native Edit.${TOOL_DISCOVERY_HINT}"
        else
          deny "MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) for a symbol in $file_path before editing it, never skip (especially if is_hub). edit_context was already called this session for other file(s), but not this one — each file needs its own call before its first native Edit. Also consider mcp__calm__edit_symbol/edit_lines (AGENTS.md Stage 6) instead of this native Edit — it can apply the change directly, hash-verified and risk-gated, chaining off edit_context's range_checksum.${TOOL_DISCOVERY_HINT}"
        fi
      fi
    elif is_definitely_unindexed_path "$file_path"; then
      : # CALM never indexes this path (dotdir/build-artifact) — edit_context
        # can't resolve a symbol here, so demanding it would be crying wolf
        # (this hook itself lives under .claude/ and used to hit exactly that).
        # The Stage 7 diff_impact gate above still applies to the write.
      decision="allow"
      decision_detail="edit_unindexed_path_no_edit_context_required"
    elif is_code_file "$file_path" && ! file_has_edit_context "$file_path"; then
      deny "MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) for a symbol in $file_path before editing it, never skip (especially if is_hub). edit_context was already called this session for other file(s), but not this one — each file needs its own call before its first native Edit. Also consider mcp__calm__edit_symbol/edit_lines (AGENTS.md Stage 6) instead of this native Edit — it can apply the change directly, hash-verified and risk-gated, chaining off edit_context's range_checksum.${TOOL_DISCOVERY_HINT}"
    fi
    ;;
  Write)
    save_state "$edit_context_files" true "$nudge_counts"
    if is_indexed_file "$file_path"; then
      # Same ground-truth-first reasoning as Edit above -- a tracked file
      # always gets the advisory nudge regardless of path-shape guessing.
      nudge write_native "MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) before this write if it modifies existing code, never skip (especially if is_hub). If this is editing an existing tracked file rather than creating a new one, consider mcp__calm__edit_lines/edit_symbol (AGENTS.md Stage 6) instead — hash-verified, risk-gated, reindexes immediately.${TOOL_DISCOVERY_HINT}"
    elif is_definitely_unindexed_path "$file_path"; then
      : # CALM never indexes this path (outside the repo root entirely, or a
        # dotdir/build-artifact dir) — edit_context can't resolve a symbol
        # here, so the nudge would be crying wolf (found live 2026-07-14:
        # every Write under this session's own memory directory, well
        # outside the repo, still fired this nudge — the Edit branch above
        # already got this is_definitely_unindexed_path guard in the
        # 2026-07-13 redesign, Write was missed).
    else
      nudge write_native "MANDATORY per AGENTS.md Stage 5 — call mcp__calm__edit_context(symbol) before this write if it modifies existing code, never skip (especially if is_hub). If this is editing an existing tracked file rather than creating a new one, consider mcp__calm__edit_lines/edit_symbol (AGENTS.md Stage 6) instead — hash-verified, risk-gated, reindexes immediately.${TOOL_DISCOVERY_HINT}"
    fi
    ;;
  Bash)
    decision_query="$command"
    if is_real_git_commit_or_push "$command"; then
      if [ "$needs_diff_impact" = "true" ]; then
        target_root=$(resolve_git_target_root "$command")
        project_root=$(git rev-parse --show-toplevel 2>/dev/null)
        if [ -z "$target_root" ] || [ "$target_root" = "$project_root" ]; then
          deny "MANDATORY per AGENTS.md Stage 7 — call mcp__calm__diff_impact(staged=true) before this commit/push, never skip. Files changed since the last diff_impact check.${TOOL_DISCOVERY_HINT}"
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
      # 2026-07-14 PostToolUse follow-up: this branch used to nudge
      # pre-call for any non-single-file bash grep (generic message, no
      # result grounding). Retired in favor of handle_post_tool_use's
      # bash_grep_post nudge, which fires AFTER the call with the REAL
      # match count (or confirmed zero matches) instead of a guess — see
      # handle_post_tool_use's header comment. Left silent here
      # deliberately so one real call is never nudged twice (once
      # generic pre-call, once grounded post-call); the single-file case
      # stays silent for the same reason it always was (native genuinely
      # has no CALM disadvantage there).
      :
    elif grep -qE '\bfind\b.*-i?name\b' <<<"$command"; then
      bump native_explore
      nudge_or_tally bash_find "CI available in this repo — prefer mcp__calm__search(query, kind=\"file\") or mcp__calm__file_overview / mcp__calm__dependencies over find (AGENTS.md Stage 1-2).${TOOL_DISCOVERY_HINT}"
    elif grep -qE '\b(rustfmt|cargo[[:space:]]+fmt)\b' <<<"$command"; then
      # 2026-07-14 self-audit finding, not a guess: a raw `rustfmt
      # <files...>` shell invocation on THIS exact repo silently
      # reformatted a file that was never in the argument list —
      # `rustfmt` resolves the owning Cargo package for any positional
      # file arg and walks its whole `mod` tree, not just the files
      # actually passed. mcp__calm__format_files avoids this by
      # construction: it pipes each file through rustfmt over stdin only
      # (no positional file arg, so no package/mod-tree resolution at
      # all — see calm_core::format's own doc comment) and writes back
      # through the same atomic-write path every other CALM edit uses,
      # so it can only ever touch the exact paths it was given.
      nudge bash_rustfmt "CI available in this repo — prefer mcp__calm__format_files(paths=[...]) over a raw rustfmt/cargo fmt invocation. A bare \`rustfmt <files>\` resolves the owning Cargo package and reformats its WHOLE mod tree, not just the files listed — confirmed live on this exact repo, not a theoretical risk. format_files pipes each file through rustfmt over stdin only, so it can never touch a file outside its own paths list.${TOOL_DISCOVERY_HINT}"
    fi
    ;;
esac

# Reaching here means nothing above this point had anything more important
# to say for this call (every deny/nudge exits the script directly) — the
# one safe place to add a low-priority, never-suppresses-anything session
# awareness check. See maybe_nudge_session_context's own header comment.
maybe_nudge_session_context
