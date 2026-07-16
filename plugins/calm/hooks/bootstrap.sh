#!/usr/bin/env bash
# Plugin SessionStart bootstrap.
#
# On first use of this plugin in a project, scaffold the CALM tool-workflow
# hook + AGENTS.md via the already-built, idempotent `calm init --hooks
# --agents-md` (crates/calm-core/src/hooks.rs, crates/calm-cli/src/main.rs
# ::apply_hooks_flag). This wires the exec-form `calm hooks-check`
# subcommand (crates/calm-core/src/hooks_check.rs) directly into
# `.claude/settings.json` — no separate script file, no jq/sqlite3-CLI/
# POSIX-flock dependency (see docs/superskills/specs/2026-07-16-calm-
# hooks-native-cli-subcommand.md). This is deliberately NOT a port of the
# CALM project's own .claude/hooks/calm-nudge.sh — that is a ~1200-line
# dogfooding-only tool (session tallies, decision-log JSONL, native-vs-CALM
# exploration counters) meant to evolve fast inside CALM's own repo.
# `hooks-check` covers exactly the 2 hard gates (edit_context before native
# Edit, diff_impact before commit/push) plus the prose-file exception —
# never a security boundary (see that module's own header for the exact
# bypass this can't close).
#
# Two responsibilities, run every SessionStart:
#
# 1. FIRST-TIME SETUP, guarded on TWO signals:
#      - .calm/hooks.mode absent: `calm init --hooks` has never run here.
#      - .claude/hooks/calm-nudge.sh absent: this project doesn't already
#        have a hand-tuned bespoke CALM hook of its own (concretely: CALM's
#        own repo, if this plugin were ever installed there too) that a
#        blind scaffold would step on.
#    Fires `calm init --hooks=enforce --agents-md` exactly once — after
#    that, .calm/hooks.mode exists (it's gitignored/local, so this still
#    fires fresh for every teammate on their own machine even when
#    .claude/settings.json itself is committed and shared) and this branch
#    never runs again for this machine, so it never fights a user who later
#    runs `calm init --hooks=off` on purpose.
#
# 2. DRIFT SELF-HEAL, every SessionStart after the first (`.calm/hooks.mode`
#    already exists): `calm doctor --fix` — repairs a configured-but-not-
#    active install (the binary path baked into `.claude/settings.json` at
#    scaffold time has gone stale — the project directory was moved/
#    renamed, or the npm-resolved `node_modules` layout changed) by
#    re-wiring the SAME already-configured mode (never touches an explicit
#    `off`, never flips nudge<->enforce) against this session's own current
#    binary path. A genuine no-op, cheap, when nothing is stale — verified
#    directly: `doctor_fix_is_a_noop_when_already_healthy` in
#    crates/calm-cli/tests/hooks_doctor_fix.rs asserts a byte-identical
#    settings.json when nothing needed fixing. Silent either way (no
#    SessionStart message) — this is routine maintenance, not something
#    worth narrating every session; `calm doctor` (no --fix) is always
#    available on demand to see the actual state.
#
# Best-effort throughout: on any failure (no Node on PATH, no network for
# npx's first download, ...) this silently does nothing further and retries
# next SessionStart — same "|| true" tolerance as every other hook shipped
# in this project, never blocks the session either way.
#
# Shell-form hook: Claude Code runs this via Git Bash by default on native
# Windows (code.claude.com/docs/en/hooks-guide). If Git Bash isn't on PATH
# there, this hook simply never runs — no crash, no denied tool calls, just
# no auto-scaffold/self-heal (falls back to the manual `calm init --hooks
# --agents-md` / `calm doctor --fix` path, same as before this bootstrap
# existed).
set -uo pipefail

cat >/dev/null # drain the SessionStart JSON payload on stdin — unused here

[ -f .claude/hooks/calm-nudge.sh ] && exit 0
command -v npx >/dev/null 2>&1 || exit 0

if [ ! -f .calm/hooks.mode ]; then
  if npx -y @eilodon/calm-mcp init --hooks=enforce --agents-md \
    --project-root "${CLAUDE_PROJECT_DIR:-.}" >/dev/null 2>&1; then
    printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"CALM plugin: scaffolded a CALM hooks-check entrypoint and an AGENTS.md workflow block into this project (first use here). Stage 5 (edit_context before native Edit) and Stage 7 (diff_impact before commit/push) are now hook-enforced for real code; prose files stay advisory-only. Run `calm init --hooks=off` to remove, or `calm doctor` to inspect the active state."}}'
  fi
else
  npx -y @eilodon/calm-mcp doctor --fix \
    --project-root "${CLAUDE_PROJECT_DIR:-.}" >/dev/null 2>&1 || true
fi

exit 0
