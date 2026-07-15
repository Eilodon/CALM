---
title: Reactivating "calm init --strict-hooks" as a transparent, best-effort, opt-in/opt-out generic hook — Item B follow-up
date: 2026-07-15
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

`docs/superskills/specs/2026-07-14-calm-mcp-external-onboarding.md` ("Item
B") proposed `calm init --strict-hooks`: scaffold a minimal Claude-Code hook
into an external user's own repo enforcing CALM's 2 hard-gated stages
(`edit_context` before first native `Edit` per file per session;
`diff_impact` before `git commit`/`git push`). That item's own audit-design
pass returned **HOLD — reaffirmed, on stronger grounds**, after external
research surfaced a real multi-version Claude Code deny-reliability bug
class (`anthropics/claude-code` #4669, #39344) and Anthropic's own docs
recommending against hooks for hard enforcement at all. The HOLD listed 6
conditions for revisiting: (a) build on `exit 2` not JSON
`permissionDecision` — done, shipped 2026-07-14 in `calm-nudge.sh` itself;
(b) derive the minimal template *by subtraction* from `calm-nudge.sh`'s
already-fixed logic, not a rewrite; (c) design a shadow-mode-first rollout
before any real external hard-deny ships; (d) fix a cross-item message
coupling (deny/nudge messages hardcode `"AGENTS.md Stage N"` — verified
live today via `grep 'Stage [0-9]' .claude/hooks/calm-nudge.sh`, e.g. line
1127: `deny "MANDATORY per AGENTS.md Stage 5..."` — meaningless in a repo
that never ran `calm init --agents-md`); (e) message this honestly as
best-effort/defense-in-depth, never unbypassable; (f) done.

This session's explicit ask, verbatim intent: research the *optimal*
mechanism to reactivate this, with two hard requirements the user stated
directly — **claims must be lowered to best-effort** (not "strict", not
implying guaranteed enforcement), and **on/off state and what the mechanism
does must be transparently explained and notified to the user at every
relevant point, never silent**. This spec's Design section is written to
satisfy (a)-(f) plus these two explicit requirements together, as one
coherent mechanism rather than bolt-on afterthoughts.

## Design

**Rename away from "strict."** `--strict-hooks` itself is a claims problem
— the flag name asserts a property (guaranteed strictness) the mechanism
cannot deliver, before a user reads a single word of documentation. New
flag: `calm init --hooks[=MODE]` on the existing `Init` command, `MODE` one
of `nudge` (default when `--hooks` given bare), `enforce`, or `off`.

**Two-mode mechanism, mode stored out-of-band from the script itself.**
Rather than writing two different hook script variants (a maintenance/drift
risk — the exact anti-pattern the `calm-guide` Skill's own doc comment
already warns against for AGENTS.md-content duplication), ship ONE
`include_str!`-embedded script, `.claude/hooks/calm-hooks.sh` (neutral
name, no overclaim), that reads its mode from a small state file,
`.calm/hooks.mode` (plain text: `nudge` | `enforce`), written by `calm
init --hooks=MODE`. This has three benefits beyond avoiding drift: (1)
mode toggling after install is a one-line file write, not a re-scaffold —
`calm init --hooks=enforce` on an already-nudge-mode install just rewrites
the state file; (2) the state is trivially inspectable by both a human
(`cat .calm/hooks.mode`) and tooling (`calm doctor`, proposed below) with
no shell-script parsing; (3) it mirrors this codebase's own established
pattern for out-of-band operational state (`daemon.meta`'s
build-identity marker) instead of inventing a new one.

**Satisfying (c), shadow-mode-first, adapted for a single-repo population.**
The internal-repo precedent's "shadow mode" (log what *would* be denied,
measure false-positive rate across real usage, graduate only once proven
safe) assumed an aggregate population to measure against — CALM has no
telemetry/phone-home (`docs/architecture.md`'s own "Local-only" guarantee),
so cross-repo aggregation is categorically unavailable. The adapted
equivalent for N=1: **`nudge` is the default and only mode `--hooks` (bare)
ever installs.** A user experiences the *exact* message content and
trigger conditions the `enforce` mode would use, but every message is
advisory (stderr nudge, tool proceeds) instead of blocking. The user's own
lived session *is* their shadow-mode trial — they see if it fires
sensibly on their workflow before ever opting into it blocking anything.
`enforce` is never silently reached; it requires typing `=enforce`
explicitly, once, with the CLI printing the reliability caveat (see below)
at that exact moment, not just once in a README.

**Satisfying (d), message decoupling.** The generic template's messages
never reference "AGENTS.md Stage N" (that numbering only means something
in *this* repo, which ships an `AGENTS.md` with matching stage headers).
Generic messages instead point at the one channel guaranteed to exist and
stay current regardless of whether `--agents-md` was ever run: *"call
`edit_context` before this edit — see the `calm_workflow` MCP prompt (or
this server's `initialize` instructions) for the full workflow."* This
fully decouples Item B from Item A — either can be installed alone, no
dead pointers in either direction (resolves both Item A's FM2 and Item B's
Abductive Hypothesis 2 from the original spec, at the root, by removing
the coupling rather than managing it). Trade-off named explicitly for
whoever reviews this: a future enhancement *could* detect the
`<!-- calm:workflow:start -->` marker's presence and use richer
stage-numbered messages when it's there, falling back to generic phrasing
otherwise — deliberately deferred out of v1 scope, since the HELD spec's
own Failure Mode 2 already flagged that a from-scratch template regains
complexity fastest exactly where it tries to be clever; a static, always-
correct message is safer for a first ship than a correct-only-if-detection-
works one.

**Satisfying (e) + the user's best-effort requirement, concretely.** Every
message the script ever emits — nudge or (in `enforce` mode) deny — is
prefixed with an explicit mode tag, e.g. `[CALM hooks: nudge — advisory
only, does not block]` or `[CALM hooks: enforce — best-effort, not a
security boundary]`. The `enforce`-mode deny message body itself states
the caveat inline (not just at install time): *"...this is best-effort
defense-in-depth, not a guaranteed block — Claude Code's own hook
reliability has known gaps (anthropics/claude-code#4669, #39344); do not
treat this as a substitute for reviewing the diff yourself."* Matches this
repo's own internal `deny()` framing (already migrated to this tone
2026-07-14) rather than inventing new copy.

**Satisfying the user's transparency/on-off requirement.** Three surfaces,
not one:
1. **`calm init --hooks=MODE` itself prints exactly what changed** — file
   written, settings.json block added/updated, current mode before and
   after, and the one-line "how to change this" pointer (`calm init
   --hooks=nudge|enforce|off`) — mirrors `write_mcp_config_entry`'s
   existing three-state ("wrote"/"up to date"/"exists — pass --force")
   observability convention, extended with the mode transition.
2. **`calm doctor` (existing subcommand, `main.rs`) gains a hooks-status
   line**: reads `.calm/hooks.mode` if present and reports it (`hooks:
   enforce mode, installed <date>` / `hooks: nudge mode` / `hooks: not
   installed`) alongside its existing build-freshness check — one place a
   user already goes to ask "what's CALM's current state in this repo."
3. **Daemon startup logs the active mode once, at INFO**, in
   `.calm/daemon.log` (`crates/calm-cli/src/main.rs::init_daemon_tracing`
   already writes here) — so it's visible operationally to whoever's
   watching daemon logs, not just discoverable by remembering to run `calm
   doctor`.

**Clean, equally-transparent off-switch, two forms.** (1) Permanent:
`calm init --hooks=off` removes `.calm/hooks.mode`, deletes
`.claude/hooks/calm-hooks.sh`, and removes (only) the specific
`{matcher, hooks}` block this tool itself added — identified by its exact
`command` string, the same append-a-new-independent-block strategy the
original spec already validated is safe against Claude Code's confirmed
"all matching blocks fire in parallel" semantics (so removing this one
block cannot affect any other hook the user has independently configured).
(2) Temporary, no file edits: `CALM_HOOKS_DISABLE=1` env var short-circuits
the script to a no-op at the top, for a one-off session — mirrors the
already-shipped `CI_MCP_LAUNCHER_NO_DAEMON=1` precedent for the daemon
opt-out, same naming convention, same "one env var, no file surgery"
ergonomics.

**Satisfying (b), derive by subtraction.** Keep from `calm-nudge.sh`:
`exit 2` mechanism, per-file (not per-session) re-arming, the path-form
false-deny fix (`f3d15e3`), `acquire_state_lock`/`release_state_lock`'s
TOCTOU-safe locking (DEBT-010) including the `{ ...; } 2>/dev/null`
redirect-scoping fix, `is_prose_file`/`is_code_file`/
`is_definitely_unindexed_path`'s classification logic (governs the .md/.txt
downgrade-to-nudge exception, which is generically correct, not
CALM-repo-specific). Drop: the decision-log JSONL analytics layer
(`log_decision`, `emit_commit_tally`), `posttooluse-discovery-dump.sh`'s
companion nudges, and the broader native-Read/Grep advisory layer
(`nudge_or_tally` for `read_native`/`grep_tool`/`bash_grep_*`) — those were
explicitly tuned against *this* session's own observed false-positive
patterns per the F5 audit-design mitigation cited in `calm-nudge.sh`'s own
comments, with no equivalent tuning evidence for an arbitrary external
repo; shipping them untuned risks exactly the "erodes trust in every
future nudge" failure mode `AGENTS.md` itself already names as the reason
Read/Grep nudging isn't hook-enforced even internally. v1 scope is the 2
hard-gated stages only, matching the original Item B design's own scope
decision.

**Out of scope, explicitly.** `docs/pattern-debt-registry.yaml`, `.claude/
skills/` (VHEATM, adr-commit, using-super-skills, and the rest of the
super-skills apparatus) are this team's own development methodology, not
CALM-the-product's identity — `pattern_debt_register`/`pattern_debt_status`
(the 2 actual MCP tools) already ship generically; the process layer around
them does not belong in `calm init`'s scaffolding surface at all, this
session or later.

## Open questions for audit-design

1. Does storing mode in `.calm/hooks.mode` (untracked, gitignored,
   per-checkout) introduce any risk this codebase's existing `.calm/`
   trust boundary doesn't already cover (compare to `daemon.meta`,
   `edit.lock`, `memory.key`)?
2. Is "one shared script keyed by a state-file mode flag" actually simpler
   / less failure-prone than "two separate embedded scripts, nudge-only
   and enforce", given the L1/L3 risk categories the original Item B audit
   used?
3. Does bundling the off-switch (env var + `--hooks=off`) in the *same*
   spec as the enforce mechanism create any new failure mode the original
   audit didn't consider (e.g., a false sense of control if the env var
   check itself has a bug)?
4. Any reason `calm doctor` is the wrong home for hooks-status vs. a new
   dedicated `calm hooks status` subcommand?

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE -- update this section, do not append a second one -->
<!-- last-run: 2026-07-15 | trigger: NORMAL -->

**Tier:** 2 (Production — third-party distribution, no PII/payments/regulated data, but a trust/enforcement mechanism shipped into unknown external repos raises the bar within Tier 2) | **Date:** 2026-07-15

### Failure Modes

1. **Unrecognized/corrupted `.calm/hooks.mode` value causes an unsafe default** — MEDIUM-HIGH — mitigation in plan: NO. The design never specifies what the script does when the mode file holds something other than exactly `nudge`/`enforce`/`off` (empty, wrong case, a future-version mode name an older script doesn't recognize, or vice versa after a CALM downgrade). Two unsafe directions are both plausible from an unspecified branch: silently treating unknown-as-`nudge` (fails toward safety, defensible) versus a shell script's default fallthrough silently treating unknown-as-`enforce` or crashing PreToolUse entirely (fails toward blocking everything, or toward no protection at all with no error surfaced). Must be an explicit, tested branch, not implicit shell fallthrough.
2. **The three status-reporting surfaces (`calm doctor`, daemon startup log, install-time output) can each independently drift from what's *actually* enforced** — HIGH — mitigation in plan: NO. All three currently read `.calm/hooks.mode` as ground truth, but the file's mere presence does not prove `.claude/settings.json` still has the wiring block (a user, another tool, or a settings.json merge could remove it while the mode file survives) — `calm doctor` would then report `hooks: enforce mode` while nothing is actually being invoked. This is exactly the "trusted but not real" shape the original Item B audit already named as worse than no feature at all, now reproduced one layer down in the new status-reporting mechanism meant to fix transparency.
3. **The mode-file/settings.json state that controls `enforce` mode is not protected by the mechanism itself, so an agent can trivially self-disable it** — HIGH — mitigation in plan: NO. `.calm/` is a dotdir, and `is_definitely_unindexed_path`/`IGNORE_DIRS`-class logic in the existing hook explicitly carves dotdirs out of the edit_context-required path — meaning a native `Write` to `.calm/hooks.mode` (flipping `enforce`→`nudge`/`off`) is *itself never gated* by the mechanism it controls. One uncontested native `Write` fully defeats `enforce` mode before the agent's very next action. This is a sharper, more concrete instance of "best-effort, not a security boundary" than the spec currently states out loud — the spec should name this exact bypass explicitly in user-facing docs, not just gesture at reliability caveats in the abstract.

### Layer Signals

- L1 Logic: Failure Mode 1 *is* the untested branch — the mode-parsing logic needs an enumerated, tested case for every value including empty/malformed/unknown-future-mode, with a stated safe default (recommend: default to `nudge`, log a warning, never silently escalate to `enforce` and never crash the hook dispatch entirely).
- L2 Concurrency: mostly clean — a small text file read by multiple near-simultaneous PreToolUse invocations is not a realistic torn-read risk given single-shot-write semantics. One real requirement: `calm init --hooks=MODE`'s write to `.calm/hooks.mode` should be atomic (temp+rename), matching this codebase's own established atomic-write convention elsewhere, rather than a naive truncate-write a concurrently-firing hook could read mid-write.
- L3 Data: `.calm/hooks.mode` has no version/schema marker — a future CALM release that changes mode semantics (new mode name, different meaning) has no way to detect it's reading a file an older or newer `calm init` wrote. Feeds directly into Failure Mode 1's mitigation — recommend a structured format (e.g. `mode=enforce\nwritten_by=0.2.0`) instead of a bare token.
- L4 Integration: unchanged from the original Item B audit — the mechanism's correctness still rests entirely on Claude Code's own (external, undocumented-here) hook-dispatch contract. Not re-litigated here; carried forward as still-true.
- L5 Security: this is where Failure Mode 3 lives formally. Worth stating plainly for the plan: no purely file-based hook mechanism can defend its own control state against an agent that already has unrestricted native `Write` — this is not a gap unique to this design, it's a structural ceiling on what *any* hook-based approach can promise, which is precisely why requirement (e)'s "best-effort, not unbypassable" framing must say so concretely (name the `.calm/hooks.mode` bypass path itself), not just gesture at "known reliability gaps" in the abstract.
- L6 Observability: the 3-surface design is good in principle but currently under-specified per Failure Mode 2 — `calm doctor` must check settings.json wiring presence *in addition to* the mode file and report a third, explicit state ("mode file says enforce, but no hook wiring found in settings.json — not actually active") rather than trusting the mode file alone. Also worth adding: `calm doctor` should report if `CALM_HOOKS_DISABLE=1` is currently set in the environment, so a user who set it once for a one-off session and forgot doesn't spend time confused about why nudges stopped.
- L7 Cross-cutting (idempotency): re-running `--hooks=MODE` must converge without duplicating the settings.json block — already solved by the exact-command-string identity check inherited from the original spec's validated design. One gap: `--hooks=off` when nothing was ever installed should no-op cleanly with a clear "nothing to remove" message, not error — must be an explicit enumerated case, not assumed.

### Assumptions to Verify

- **ASSUMED:** a stderr-prefixed mode tag on every hook message is sufficient to satisfy "transparent notification to users" — unverified whether a human operator (vs. the agent/model) ever actually sees PreToolUse hook stderr in a typical Claude Code session, versus it only reaching the model's context. The user's explicit requirement was transparency *to users*, plural, meaning the human — `calm doctor` (pull, on-demand) and daemon.log (passive, requires knowing to look) both under-serve a human who isn't actively checking. Needs a concrete answer in the plan, not left implicit.
- **ASSUMED:** the exact settings.json command string used for idempotent block identification never needs to change across CALM versions. If a future version's invocation gains a flag or changes form, old removal/detection logic silently stops recognizing blocks written by prior versions. Same versioning gap as L3/Failure Mode 1, different surface.
- **ASSUMED:** users will discover and correctly use `CALM_HOOKS_DISABLE=1` by analogy by `CI_MCP_LAUNCHER_NO_DAEMON=1` — no verification this naming pattern is actually discoverable rather than just internally consistent.

### Abductive Hypotheses

1. **Interaction between individually-correct components: three status surfaces have three different staleness properties for the same nominal fact.** `calm doctor` and the hook script itself both fresh-read `.calm/hooks.mode` on every invocation (correctly current). The daemon, per the design's item 3, logs the mode once at startup — under the already-shipped shared-daemon-by-default model (ADR-0005, a long-lived process), a `calm init --hooks=enforce` run *while the daemon is already up* leaves the daemon's log showing a stale mode indefinitely, while actual enforcement (which doesn't route through the daemon at all — it's a Claude Code hook, not a CALM RPC) is already correctly live. A user cross-checking daemon.log against observed behavior mid-session hits a real contradiction between two "authoritative-looking" surfaces, for reasons neither one's own logic is individually wrong about.
2. **Adversarial/supply-chain scenario at distribution scale.** Nothing cryptographically binds `.claude/settings.json`'s hook invocation to CALM's own embedded script content — it's a plain file path. Once this ships broadly enough that external repo templates/onboarding scaffolds start bundling a pre-configured `.calm/hooks.mode=enforce` + settings.json block (a realistic scale outcome, not far-fetched), a compromised template or a prior malicious commit could leave the mode file and settings.json wiring untouched while swapping `.claude/hooks/calm-hooks.sh`'s actual content — every status surface would report "enforce mode, working as intended" while executing attacker-controlled code on every tool call. Full mitigation (e.g. hashing the script and having `calm doctor` verify it against the embedded version) is likely out of v1 scope by this codebase's own "don't overbuild before real need" norm, but the risk should be a named, conscious deferral in the plan, not an unknown unknown discovered later.

### Gate Result

**PASS WITH FLAGS.** No finding requires a fundamentally different mechanism — unlike the original Item B HOLD (which rested on an unresolved *vendor-level* reliability gap with no local fix available), every finding here is about this design's own internal consistency and honesty, and each has a concrete, buildable mitigation within CALM's own control. Proceed to `writing-plans`, which MUST include: an explicit, tested state machine for `.calm/hooks.mode` parsing with a safe default on any unrecognized value (FM1); `calm doctor` cross-checking settings.json wiring presence against the mode file and reporting drift as its own explicit state, plus reporting `CALM_HOOKS_DISABLE` if set (FM2); user-facing documentation and in-message copy that names the `.calm/hooks.mode` self-disable path *concretely*, not just an abstract reliability caveat (FM3, and the concrete form of requirement (e)); a version/schema marker in the mode file (L3); atomic writes for the mode file (L2); a concrete answer for how a human operator (not just the agent) is expected to actually see mode-change/deny notifications (Assumption 1); and an explicit note in the plan naming Abductive 2 (script-content integrity at distribution scale) as a conscious v1 non-goal rather than an unconsidered gap.

## Failure Mode Mitigations (detailed design, 2026-07-15 follow-up)

Concrete fixes for all 3 failure modes above, researched to the point of being directly implementable — not just named as risks. Each ties back to its FM number.

### FM1 — structured mode file + explicit safe-default state machine

Replace the bare-token `.calm/hooks.mode` with a small structured format from v1 (avoids ever needing a bare-token→structured migration later):

```
schema=1
mode=enforce
written_by=0.2.0
written_at=2026-07-15T10:32:00Z
```

Parser contract (one shared function, used by both the hook script and `calm doctor`, so there is exactly one place this logic can be wrong):

```sh
read_hooks_mode() {                       # stdout: nudge|enforce|off, never anything else
  local f="$CALM_DIR/hooks.mode"
  [ -f "$f" ] || { echo "off"; return; }  # no file = never installed = fully inert
  local schema mode
  schema=$(grep -m1 '^schema=' "$f" 2>/dev/null | cut -d= -f2)
  mode=$(grep -m1 '^mode=' "$f"   2>/dev/null | cut -d= -f2 | tr -d '[:space:]')
  if [ "$schema" != "1" ]; then
    warn_once "hooks.mode: unrecognized schema '$schema' — defaulting to nudge"
    echo "nudge"; return
  fi
  case "$mode" in
    nudge|enforce|off) echo "$mode" ;;
    *) warn_once "hooks.mode: unrecognized mode '$mode' — defaulting to nudge"
       echo "nudge" ;;
  esac
}
```

Safety property, stated as the actual invariant to test: **the function's output is always exactly one of `nudge`/`enforce`/`off`, it never crashes, and it never returns `enforce` unless the file both exists and cleanly parses to `mode=enforce`** — any ambiguity resolves toward `nudge`, any absence resolves toward `off`. `write` path (`calm init --hooks=MODE`) uses atomic temp-file+rename, matching this codebase's existing atomic-write convention (already used on the main edit path per `docs/architecture.md`'s "Atomic writes, immediate reindex"). Test matrix for the plan: missing file, empty file, garbage content, `schema=0`/missing schema, unrecognized future `mode=` value, each of the 3 valid values, trailing-whitespace/CRLF variants — mirrors `test-calm-nudge.sh`'s existing precedent of enumerating exactly this kind of case table.

### FM2 — status surfaces verify actual wiring, not just the mode file; daemon log stays fresh

`calm doctor`'s hooks-status check becomes a 3-way cross-check, not a single file read:

1. `read_hooks_mode()` → `configured_mode`.
2. If `off`/absent → report `hooks: not installed`, done.
3. Parse `.claude/settings.json`, search for a hooks block whose `command` equals the single shared constant used by both the installer and this checker (e.g. `bash .claude/hooks/calm-hooks.sh`) — one source of truth, not two independently-maintained string literals.
4. Report exactly one of:
   - `hooks: {mode} mode, active` — mode file valid, wiring found, script file present+readable.
   - `hooks: {mode} mode CONFIGURED BUT NOT ACTIVE — settings.json wiring missing. Run \`calm init --hooks={mode}\` to reinstall.` — mode file says on, wiring isn't there.
   - `hooks: wiring present but script file missing — run \`calm init --hooks={mode}\` to reinstall.` — inverse gap.
5. If `CALM_HOOKS_DISABLE` is set in the environment, prefix the whole report with a note that hooks are inert regardless of the above — closes the "forgot I set the env var" confusion named in Assumptions.

Daemon-log staleness (Abductive 1) fix: do not log mode once at startup and call it done. Piggyback on the daemon's **already-existing** idle-check timer (`IDLE_CHECK_INTERVAL`, `daemon.rs`) — on each tick, cheaply `stat` `.calm/hooks.mode`'s mtime; only re-read and emit a fresh INFO log line when it changed since the last tick. Zero new subsystem (reuses infrastructure that already polls on a cadence), and it makes the daemon-log surface eventually-consistent instead of frozen-at-startup, closing the specific contradiction Abductive 1 described rather than just caveating it in docs.

### FM3 — honest ceiling, made concrete + tamper-evident, with a named (not built) escalation path

This one cannot be fully "fixed" — any file a hook reads is, structurally, writable by whatever it's meant to gate (dotdirs are deliberately exempt from `edit_context` gating, for good reason elsewhere in this codebase). Three layered mitigations, none of which claim to close the gap, all of which make it honest and loud instead of silent:

1. **Name the exact bypass in user-facing copy, not an abstract caveat.** Both `calm init --hooks=enforce`'s own output and any docs state directly: *"`enforce` raises the bar against accidentally skipping `edit_context`/`diff_impact` — it is not a sandbox. Any process with normal write access to this repo can disable it with one write to `.calm/hooks.mode`, by deleting `.claude/hooks/calm-hooks.sh`, or by editing `.claude/settings.json` — true of every Claude Code hook, not specific to CALM. Use it to catch honest mistakes, not to constrain an actively evading agent."* This is the concrete form of the user's "hạ claim xuống best-effort" instruction — naming the exact mechanism, not gesturing at "known limitations."
2. **Make a mode downgrade tamper-evident within the session, not silent.** The hook script already maintains small session-state (per `calm-nudge.sh`'s existing `save_state` pattern) — extend it to cache the last-seen mode. On every invocation, if the freshly-read mode is *lower-privilege* than the last-seen one (`enforce`→`nudge`/`off`, or `nudge`→`off`) and the change wasn't this script's own doing via a just-completed `calm init` run, (a) append a `{"event":"hooks_mode_downgraded","from":...,"to":...,"at":...}` line to the existing `.calm/audit.log` (already a JSON edit-decision log per `docs/architecture.md`), and (b) surface exactly one loud, transcript-visible notice on that invocation: *"NOTICE: CALM hooks mode changed from enforce to {mode} since the last tool call — if unintended, run \`calm init --hooks=enforce\` to restore."* A deny can be bypassed the same way it was just bypassed, so this is a notice, not a second gate — the point is durability of evidence, not prevention.
3. **Name, but do not build, a stronger v2 escalation.** Under the shared-daemon-by-default architecture (live since 2026-07-11), the daemon could hold mode in memory, changed only via an explicit RPC from `calm init` rather than a bare file read — a bare file write would then be insufficient to change *live* enforcement, and forcing a daemon restart to pick up a new value is already comparatively loud (existing stale-build-respawn logging). Real complexity cost (new RPC surface, a second code path for non-daemon `calm serve`) — per this codebase's own repeated "don't build ahead of evidence" pattern (see the original Item B HOLD's own reasoning), this is recorded here as a conscious, evidence-gated future option, not v1 scope. Do not implement until real usage shows the file-write bypass is actually being hit in practice.

### Additional plan requirement surfaced by this pass: verify human visibility live, don't assume it

Assumption 1 (does a human, not just the model, actually see PreToolUse deny/nudge stderr in the Claude Code transcript UI?) is answered by neither research nor precedent in this repo — it must be answered by a live test before the messaging design is called final: fire a real `enforce`-mode deny in an actual Claude Code session and observe exactly what appears in the human-facing transcript, matching this codebase's own consistently-applied "verify live, don't assume" discipline (see e.g. the D.1-D.4 SCIP/LSP provider verification methodology). If the human does not see it by default, the plan needs a different channel for the human specifically (e.g. `calm doctor` remains the deliberate pull surface, and the notice-on-downgrade from FM3 mitigation 2 should be treated as the loudest available push).

## Implementation status (2026-07-15, same-day follow-up)

**Shipped, all mitigations from the Risk Assessment applied, not committed yet (uncommitted working tree — awaiting explicit go-ahead to commit):**

- `crates/calm-core/assets/hooks/calm-hooks.sh` — the generic script, derived by subtraction from `.claude/hooks/calm-nudge.sh` per (b): kept `exit 2`, per-file re-arming, the lock-protected `{ ...; } 2>/dev/null`-scoped state save, the prose (.md/.txt) downgrade-to-advisory exception. Dropped the decision-log JSONL analytics, `posttooluse-discovery-dump.sh` companion, and the native-Read/Grep advisory layer (out of v1 scope per the spec). Messages never reference `AGENTS.md` stage numbers (verified by an explicit test assertion) — point at the `calm_workflow` MCP prompt instead (d).
- `crates/calm-core/src/hooks.rs` — `HooksMode` (Nudge/Enforce/Off), `read_hooks_mode_file`/`write_hooks_mode_file` (FM1's exact safe-default state machine — schema-versioned, atomic temp+rename write), `write_hooks_settings_block`/`remove_hooks_settings_block` (idempotent, isolation-tested against unrelated `.claude/settings.json` keys and blocks), `check_hooks_status`/`HooksStatus` (FM2's 3-way cross-check: mode file + actual settings.json wiring + script presence + `CALM_HOOKS_DISABLE` env — never reports "active" on the mode file's word alone).
- `calm init --hooks[=nudge|enforce|off]` (`crates/calm-cli/src/main.rs`) — bare `--hooks` defaults to `nudge` (`num_args = 0..=1, default_missing_value`). Install/mode-change output prints the mode transition, what changed, and — for `enforce` — the concrete bypass path inline (FM3 mitigation 1, verbatim in the code, not paraphrased here). `--hooks=off` cleanly removes the mode file, settings.json block, and script file; no-ops cleanly if nothing was installed.
- `calm doctor` (`crates/calm-server/src/lib.rs`) — reports `hooks_status.summary()`: `not installed` / `{mode} mode, active` / `{mode} mode CONFIGURED BUT NOT ACTIVE — ... wiring missing` / `wiring present but script missing`, plus a `CALM_HOOKS_DISABLE` note when set.
- FM3 mitigation 2 (tamper-evident downgrade) — implemented in the script itself: `last_seen_mode` persisted in per-session state, a downgrade (`enforce`→`nudge`/`off`, `nudge`→`off`) appends a `hooks_mode_downgraded` event to `.calm/audit.log` and surfaces one `NOTICE:` on the very next hook invocation, then stops repeating (verified by a dedicated test asserting it fires exactly once).

**Real bug found and fixed by the test suite, not anticipated by this spec**: the original draft read `.calm/hooks.mode` and short-circuited (`CALM_HOOKS_DISABLE=1`, `mode=off`) *before* draining stdin (`input=$(cat)`). Since Claude Code's hook invocation is `jq ... | bash calm-hooks.sh`, an early `exit 0` left the `jq` producer writing to a reader that had already closed its end without consuming it — a real SIGPIPE (exit 141), caught by assertion 8 in `test-calm-hooks.sh` on first run, not guessed. Fixed by moving `input=$(cat)` to immediately after `set -uo pipefail`, before every other branch.

**Verified:**
- `crates/calm-core/src/hooks.rs`: 21 unit tests (FM1's full parse matrix — missing/empty/garbage/wrong-schema/unrecognized-mode/CRLF — plus settings.json merge idempotency/isolation/invalid-JSON-safety, plus `HooksStatus` cross-check states), all passing.
- `crates/calm-core/assets/hooks/test-calm-hooks.sh`: 10 assertions (off/nudge/enforce behavior for both gates, prose downgrade, `CALM_HOOKS_DISABLE`, FM1's corrupted-file/wrong-schema safe-default, FM3's downgrade-notice-fires-exactly-once), all passing.
- Full workspace: `cargo build --workspace` clean, `cargo test --workspace` — 0 failures (one `daemon_integration.rs` test flaked once under parallel execution, confirmed pre-existing/unrelated by 2 clean isolated reruns, not caused by this change), `cargo clippy --workspace --all-targets` clean.
- `mcp__calm__diff_impact` — `aggregate_risk: high` driven entirely by `apply_hooks_flag` (brand-new function, `signature_changed: true`), the known diff_impact-on-pure-insertion false positive (see `[[calm-diff-impact-signature-false-positive]]`); confirmed via `callers` that its one real call site already matches the signature correctly.

**Deliberately deferred, named not built (unchanged from the Risk Assessment):**
- Daemon-side idle-check re-log on `.calm/hooks.mode` mtime change (Abductive 1's staleness fix) — `calm doctor` and the hook script itself are both always-fresh; the daemon-log surface was the lowest-priority of the three per the spec and was scoped out of this pass to keep the diff reviewable. `calm doctor` remains the authoritative on-demand source in the meantime.
- FM3 mitigation 3 (daemon-mediated mode authority via RPC) — explicitly not v1 scope per the spec's own evidence-gating rule.
- The live-in-Claude-Code-transcript visibility check (this section's own preceding "Additional plan requirement") — not yet run; messaging design should be treated as provisional until it is.
