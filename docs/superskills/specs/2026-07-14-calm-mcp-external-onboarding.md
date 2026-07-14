---
title: CALM MCP external-user onboarding — AGENTS.md scaffold, opt-in strict hooks, get_info instructions pointer
date: 2026-07-14
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

CALM's own workflow discipline (8-stage tool sequence, 2 hard-enforced gates)
lives almost entirely in this repo's own dev environment: `AGENTS.md`
(auto-injected via a Claude-Code-only SessionStart hook,
`.claude/hooks/session-start-agents-md.sh`) and `.claude/hooks/calm-nudge.sh`
(the enforcement mechanism). Neither ships with the `calm-mcp` npm package or
release binary — verified by grepping `crates/` for both filenames: only
comment/string references to "AGENTS.md" as a human-readable label exist,
nothing reads or enforces its content from compiled code.

Today (2026-07-14, commit `8708116`) a first fix landed: a `calm_workflow`
MCP **Prompt** (`crates/calm-server/src/tools.rs` `ci_prompts`/`render_prompt`,
lines 436-442 and 510-521) — protocol-native, ships in every binary, reaches
any MCP client, not just Claude Code. Its own comment names the gap it closes:
"AGENTS.md's SessionStart auto-injection is Claude-Code-specific, so any
other MCP client connected to the same `calm serve` gets zero automatic
onboarding today." A companion `calm-guide` Skill
(`.claude/skills/calm-guide/SKILL.md`) was added the same day as the
Claude-Code-specific trigger surface for the same content.

Three follow-on items remain, discussed and roughly designed across this
session, now needing a real pre-implementation audit before any code is
written:

- **A.** `calm init --agents-md` — scaffold a generic, marker-delimited
  workflow section into the external user's own `AGENTS.md`/`CLAUDE.md`, for
  agents/humans that read files rather than call `prompts/get`.
- **B.** `calm init --strict-hooks` — scaffold a new, minimal Claude-Code
  hook template (NOT the existing 65KB `calm-nudge.sh`, which is this
  repo's fast-evolving internal dogfooding tool) enforcing only the 2
  hard-gated stages (edit_context-before-first-native-Edit-per-file-per-
  session; diff_impact-before-commit/push), plus a new merge function to
  append it into `.claude/settings.json` safely.
- **C.** Extend `CalmServer::get_info()`'s `.with_instructions(...)`
  (`tools.rs:544`, currently just a one-line description) to point at the
  `calm_workflow` prompt, since `ServerInfo.instructions` is delivered to
  every client on the `initialize` handshake — a push channel, whereas
  Prompts are pull-only (a client must call `prompts/get` on its own
  initiative to ever see the content).

## Design

**A.** New shared const in `calm-core` (both `calm-cli` and `calm-server`
already depend on `calm-core` per `Cargo.toml` — verified, no new dependency
edge needed) holding the same condensed workflow text `render_prompt`
currently inlines for `"calm_workflow"`. `calm init --agents-md` writes/
updates a `<!-- calm:workflow:start -->...<!-- calm:workflow:end -->`
marker-delimited block in `AGENTS.md` at the project root: file absent →
create; markers present → idempotent replace-between; file present with
*no* markers → refuse, require `--force` (mirrors `write_mcp_config_entry`'s
existing "exists — pass --force" contract, `main.rs:774-779`). Opt-in,
default off — `calm init` alone is behaviorally unchanged.

**B.** New minimal hook script (not a copy of `calm-nudge.sh`) embedded via
`include_str!`, written to `.claude/hooks/calm-strict.sh` (deliberately
different filename from the internal `calm-nudge.sh`). Merge strategy,
corrected after inspecting this repo's own real `.claude/settings.json`:
`PreToolUse`/`PostToolUse` are JSON **arrays of independent `{matcher,
hooks}` blocks** (confirmed: this repo's own file has one block per
event type, each with its own matcher string), not a single object keyed
by tool name. So the safe merge is: **append a brand-new, self-contained
`{matcher, hooks: [{command: "bash .claude/hooks/calm-strict.sh"}]}`
block**, identified for idempotency by that exact command string, never
touching or parsing any existing block. Opt-in, default off.

**C.** Change the instructions string to name `calm_workflow` explicitly, e.g.
"...Call the `calm_workflow` prompt (no arguments) for the tool workflow
before your first edit." Single call site, no test currently pins the exact
string (verified: `get_info_advertises_tools_capability`/
`get_info_advertises_prompts_capability` only assert capability flags).

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE -- update this section, do not append a second one -->
<!-- last-run: 2026-07-14 | trigger: NORMAL | Item B updated same day with external-research follow-up (official docs + anthropics/claude-code issues) -->

**Tier:** 2 (Production) | **Date:** 2026-07-14

Audited per item, in the order requested.

### Item C — `get_info()` instructions pointer

**Complexity Gate applied:** spec section is 1 paragraph, no persistent
state change, no auth/external integration → per audit-design's own
Complexity Gate this qualifies to skip straight to `writing-plans`. Fast-
tracked rather than padded with invented failure modes.

**One real finding, not blocking:** the proposed wording assumes a model
that reads "call the `calm_workflow` prompt" in `instructions` knows how to
actually invoke an MCP prompt by name — true for Claude Code (which
surfaces prompts as invokable), unverified for clients that don't expose a
`prompts/get` UX. Confirmed empirically, not assumed: this exact session's
own transcript received a live `"MCP Server Instructions... ## calm CALM
(Coding Agent Liveness Map) MCP server — codebase analysis tools"` system
block earlier — i.e. Claude Code demonstrably surfaces `ServerInfo.
instructions` into model context today, in production, in this very
conversation. Flag for the plan: keep the instructions text self-sufficient
(name the prompt AND that its content is the 8-stage workflow) so a client
that can't invoke prompts still conveys partial value, not a dead pointer.

**Gate Result: PASS.** Proceed to implementation directly.

### Item A — `calm init --agents-md`

**Failure Modes**
1. **Marker replace can corrupt user content on multiple or orphaned marker
   occurrences** — MEDIUM — mitigation in plan: NO. Naive first-start/first-
   end matching breaks if a user's own prose ever quotes the marker syntax
   (e.g. writing about this exact feature) or if a marker pair becomes
   unpaired (user hand-deleted one). Required: refuse and require `--force`
   unless the file contains *exactly one* well-formed pair — same
   "leave it untouched on ambiguity" philosophy `write_mcp_config_entry`
   already uses for JSON.
2. **The shared const's own final sentence ("Full detail... AGENTS.md at the
   project root") is a dead pointer for the default (no-flags) external
   user** — MEDIUM — mitigation in plan: NO. `calm init` alone does not
   write `AGENTS.md` (A is opt-in). A user who only gets the `calm_workflow`
   prompt (item C's channel) reads a final line pointing at a file that, by
   default, does not exist in their repo. Individually-correct components,
   broken combination on the most common path.
3. **Extracting the const into `calm-core` moves the source of truth away
   from its only current consumer/test** (`render_prompt`'s calm-server
   tests) — LOW — mitigation in plan: NO. Needs a content-pinning test on
   *both* consumers (the prompt's wire output and the scaffold's file
   output) against the same const, so a future edit's effect on both
   surfaces is visible in one diff, not just one.

**Layer Signals**
- L1 Logic: marker-count branch (0 / unpaired-1 / paired-1 / 2+) is
  currently unspecified — must enumerate all four explicitly before coding.
- L2 Concurrency: no signal for the CLI command itself; ASSUMED (not
  verified) that a live `calm serve`/daemon process doesn't need to be
  told `AGENTS.md` changed — plausible since indexer already skips
  `.claude`-class dotdirs and `AGENTS.md` is prose, not source, but not
  checked against the running indexer's file-watch behavior.
- L3 Data: no schema; ASSUMED not verified — CRLF-vs-LF: this repo's own
  `AGENTS.md` is LF-only; a Windows user's file could be CRLF, and a naive
  replace introduces mixed line endings inside one file (cosmetic, but a
  real git diff-noise footgun).
- L4/L5: no signal.
- L6 Observability: should emit the same three-state ("wrote"/"up to
  date"/"exists — pass --force") result line `write_mcp_config_entry`
  already establishes as this codebase's convention — noted for plan
  consistency, not a new risk.
- L7 Cross-cutting (idempotency): the core promise (rerun converges, no
  duplication) holds *only if* Failure Mode 1's exactly-one-pair
  enforcement is actually implemented — not automatic from "use markers"
  alone.

**Assumptions to Verify**
- **ASSUMED:** live daemon needs no notification of an `AGENTS.md` edit (L2).
- **ASSUMED:** no CRLF-normalization handling needed (L3) — low cost either
  way, cheap to just normalize to the file's detected line ending before
  writing.

**Abductive Hypotheses**
1. **Structural staleness, one layer removed.** The `calm-guide` Skill's own
   doc comment (added the same day, `8708116`) warns against holding "a
   second copy of [AGENTS.md's] content that would silently drift out of
   sync with it over time" — and resolves that *for this repo* by pointing
   back at the single `AGENTS.md`. Item A re-creates exactly that anti-
   pattern *for every external repo that scaffolds it*: a static copy,
   frozen at whatever CALM version wrote it, with no update mechanism —
   unlike the MCP Prompt (item C's channel), which regenerates fresh from
   whatever binary is currently running. A future CALM stage-renumbering or
   tool rename leaves every external repo's committed `AGENTS.md` silently
   wrong, permanently, until someone manually reruns `--force`.
2. **Same-day precedent for exactly this race class.** `save_state`/`bump`'s
   unlocked read-modify-write on hook state (`DEBT-010`) was found and fixed
   *earlier today, in this same session*. Item A's marker-replace is the
   same shape of operation (read whole file → compute new content → write
   whole file) with no lock — two near-simultaneous `calm init --agents-md`
   runs (or one run racing a human hand-editing `AGENTS.md` in an editor at
   the same moment) can lose one write. Lower stakes than hook state (a
   CLI command a human runs once, not a hook firing 10-40x/session), but
   the same root cause reappearing hours after being named and fixed
   elsewhere is worth naming explicitly rather than re-discovering later.

**Gate Result: PASS WITH FLAGS.** Proceed to `writing-plans`, which MUST
include: exactly-one-marker-pair enforcement (FM1), a plan for the dead-
pointer interaction with the default path (FM2 — either make `--agents-md`
part of a bundled default worth reconsidering, or soften the shared const's
final sentence to degrade gracefully when the file doesn't exist), and a
content-pinning test spanning both the prompt and the scaffold (FM3).

### Item B — `calm init --strict-hooks`

**Failure Modes**
1. **Core mechanism assumption is unverified and, if false, produces a
   silently non-functional security feature** — HIGH — mitigation in plan:
   NO. The corrected design appends a new, independent `{matcher, hooks}`
   block rather than merging into an existing one (safer than this
   session's own first draft, which proposed merging into an existing
   matcher's array). But whether Claude Code evaluates *every* array block
   whose matcher matches a given tool call, versus stopping at the first
   match, is not established anywhere in this repo (no test, no doc found).
   If it's first-match-wins and a user's pre-existing broader `PreToolUse`
   matcher happens to match `Edit` and sits earlier in the array,
   `calm-strict.sh` never fires — the user believes they have hard
   enforcement and do not. A feature that fails silently-open is worse
   than no feature, because it's trusted.
2. **A from-scratch minimal template throws away hard-won correctness, not
   just unwanted complexity** — HIGH — mitigation in plan: NO. `calm-
   nudge.sh` earned its current size through real, dated bug fixes still
   visible in its own comments and this session's memory: a per-file
   (not per-session) re-arming fix, a path-form false-deny fix
   (`f3d15e3`), and the `DEBT-010` state-lock TOCTOU fix from *earlier
   today*. A newly-authored "minimal" template covering the same 2 hard
   gates has no reason to already be free of the same bug classes — it is
   likely to *regress into them*, not avoid them by virtue of being
   smaller.
3. **`.claude/settings.json` may contain fields beyond `hooks`** (e.g.
   `permissions`, `env`) not fully visible in this session's own
   inspection (only the `hooks` object was printed) — MEDIUM — mitigation
   in plan: NO. Must be a stated hard requirement, mirroring
   `write_mcp_config_entry`'s own doc comment ("Never touches unrelated
   entries"), not left implicit.

**Layer Signals**
- L1 Logic: Failure Mode 1 *is* the logic gap — array-block evaluation
  semantics must be verified empirically before the merge strategy can be
  called correct.
- L2 Concurrency: ASSUMED, not verified — whether a running Claude Code
  session picks up a hook scaffolded mid-session or needs a restart; minor,
  but should be stated in the CLI's own output so a user isn't confused
  when nothing changes immediately.
- L3 Data: appended block needs a stable identity (exact command string, or
  an embedded version marker mirroring the `daemon.meta` version-comparison
  pattern already used elsewhere in this codebase) for idempotent re-run
  detection.
- L4 Integration: the entire feature's correctness rests on Claude Code's
  own (external, undocumented-in-this-repo) hook-dispatch contract — the
  single largest external dependency in this whole spec.
- L5 Security: this is the one item that installs code with the power to
  *deny* agent actions in someone else's repo. That raises its review bar
  above "template text" to "security-relevant code," and it currently has
  zero dedicated tests, versus `test-calm-nudge.sh`'s 374-line precedent
  for the internal equivalent.
- L6 Observability: **the strongest single finding in this audit.** The
  companion spec this session read to learn the audit format
  (`2026-07-14-calm-remaining-backlog-read-hardening-and-toctou.md`)
  **HELD** — did not ship — a graduated hard-deny gate for Read/Grep in
  *this very repo*, specifically for lacking sufficient shadow-mode
  measurement of false-positive rate, and named the exact discipline
  required: "add shadow-mode measurement for the escalation gate itself
  before it goes live." Item B proposes shipping a hard-deny mechanism
  straight to external repos with **zero shadow-mode measurement possible
  in principle** (they don't exist yet to measure against) — the identical
  validation gate this repo enforced on itself hours earlier, entirely
  absent for the audience with the least ability to debug a silent
  false-deny.
- L7 Cross-cutting (idempotency): re-running `--strict-hooks` must not
  duplicate the appended block — same identity-check need as L3.

**Assumptions to Verify**
- **ASSUMED, now RESOLVED (favorably) by external research below:** multi-block
  `PreToolUse` array evaluates all matching blocks, not first-match-only
  (FM1's original core question) — confirmed against official docs, not
  just inferred. See External Research below.
- **ASSUMED:** a minimal rewrite can match `calm-nudge.sh`'s already-fixed
  edge cases without repeating its bug history (FM2) — unchanged, still
  unverified.

**Abductive Hypotheses**
1. **Direct precedent, same day, same repo.** This repo's own audit-design
   process just held an internal hard-deny gate for insufficient evidence
   hours before this audit ran. Item B is the same risk shape (new hard-
   deny mechanism, no field data) aimed at an audience with *less* context
   and *no* fast iteration loop with the maintainers. Approving B while B's
   own sibling spec just held an easier version of the same idea is an
   inconsistency this audit should name outright, not soften.
2. **Cross-item message coupling reproduces Item A's Failure Mode 2.**
   `calm-nudge.sh`'s real deny messages cite specific instructions ("MANDATORY
   per AGENTS.md Stage 5..."). If `--strict-hooks` ships independently of
   `--agents-md` (both opt-in, no enforced dependency) — the likely case,
   since they're pitched as separately choosable — a user who takes only B
   gets denied with a message pointing at a Stage number in a file that
   doesn't exist in their repo. The agent being denied has no way to
   resolve what "Stage 5" means. Same root interaction bug as Item A's FM2,
   independently rediscovered in Item B — evidence this is a structural
   coupling between A and B, not two unrelated features.

**External Research (2026-07-14, follow-up pass — official docs +
anthropics/claude-code GitHub issues, not just this repo's own code)**

Requested explicitly to check whether external documentation resolves
enough of Item B's HOLD to proceed. It resolves FM1's original narrow
question, but surfaces a bigger, previously-uncounted risk in its place —
net effect strengthens the HOLD, not weakens it.

- **FM1's core question, RESOLVED:** official docs
  (code.claude.com/docs/en/hooks, "Hook handler fields" section) state
  plainly: *"All matching hooks run in parallel, and identical handlers
  are deduplicated automatically."* Multiple independent `{matcher,
  hooks}` blocks that each match a given tool call all fire — not
  first-match-only. The corrected append-a-new-block merge strategy in
  this spec's Design section is NOT silently inert as FM1 feared.
- **NEW finding, more serious than FM1 as originally scoped:** what
  happens when multiple fired hooks *disagree* is not just unverified —
  it is a real, repeated, multi-version production bug class, evidenced
  directly (not inferred) from `anthropics/claude-code` issues:
  - **#4669** (Claude Code v1.0.62-69, filed Aug 2025): `permissionDecision:
    "deny"` silently ignored, tool executes anyway. Three independent
    reporters reproduced it (`JC1738`, `jeubank12`, `tomomonta`); the last
    reported the *entire* PreToolUse blocking mechanism broken (`deny`,
    `ask`, AND `"continue": false` all failed to block). **Auto-closed by
    a housekeeping bot for 60-day inactivity — never confirmed fixed by a
    maintainer.** Checked directly via `gh issue view 4669`, not taken
    from a blog summary.
  - **#39344** (v2.1.84, filed 2026-03-26, labels `bug`+`area:security`):
    a hook returning `"ask"` silently overrode a static `permissions.deny`
    rule entirely — command ran with no prompt. **This one WAS confirmed
    fixed** by maintainer `ashwin-ant`, in **v2.1.101**
    (2026-04-18): "A PreToolUse hook returning permissionDecision 'ask'
    no longer overrides explicit `permissions.deny` rules." Checked via
    `gh issue view 39344`, comments read directly, not summarized secondhand.
  - **#35136:** separately confirmed (as of v2.1.77) that a hook's
    `"allow"` does NOT override a static deny rule — this direction was
    already safe, the bug was only ever in the `ask`-overrides-`deny`
    direction (#39344).
  - Both #4669 and #39344 reference further duplicates/related reports
    (#18312, #22018, #36059, #37420) — this is a recurring problem area
    for the vendor, not a one-off.
- **Most load-bearing finding: Anthropic's own docs recommend AGAINST
  using hooks for hard enforcement at all.** Fetched directly from
  code.claude.com/docs/en/hooks, "Common fields" section: *"Because the
  `if` filter is best-effort, use the [permission system] rather than a
  hook to enforce a hard allow or deny."* This is the vendor's own stated
  design guidance, and it argues directly against Item B's entire premise
  — a hook-based hard-deny gate is explicitly not the mechanism Anthropic
  itself recommends for that job. CALM's use case (contextual, session-
  state-dependent: "has `edit_context` been called for *this* file *this*
  session") can't be expressed as a static `permissions.deny` pattern
  rule, so this doesn't invalidate the goal, but it does mean Item B is
  reaching for a mechanism the vendor documents as not the hard-enforcement
  tool, and the GitHub issue history above shows why: it's genuinely had
  reliability bugs, repeatedly, across many months of versions.
- **Constructive finding for whenever B is revisited:** a community
  workaround (user `yurukusa`, on #39344) recommends `exit 2` (blocking
  error, stderr fed to Claude, JSON/stdout ignored entirely — confirmed in
  official docs' "Exit code output" section) over the JSON
  `permissionDecision: "deny"` form, specifically because it sidesteps the
  whole JSON-output interaction surface where #4669/#39344's bugs lived.
  Worth noting: **`calm-nudge.sh`'s own `deny()` function
  (`.claude/hooks/calm-nudge.sh`) currently uses the JSON
  `permissionDecision: "deny"` form, not `exit 2`** — this repo's own
  internal mechanism has the same theoretical exposure to this bug class
  that Item B would inherit. Worth a separate, smaller follow-up for
  `calm-nudge.sh` itself, independent of whether Item B ever ships.

**Gate Result: HOLD — reaffirmed, on stronger grounds than the original
pass.** FM1 (silently-inert mechanism) is resolved favorably: array-block
evaluation is confirmed safe. But external research replaces it with
evidence-backed findings that are harder to dismiss than an unverified
assumption: a real multi-version bug history in exactly the deny-reliability
property Item B depends on (one confirmed-fixed, one auto-closed without
confirmation), and Anthropic's own documentation directly recommending
against hooks for hard enforcement. FM2 (regression risk from a from-scratch
rewrite) and Abductive 1/2 (same-day internal precedent; cross-item message
coupling) stand unchanged. Required before revisiting, updated: (a) build
on `exit 2`, not JSON `permissionDecision`, for the actual blocking signal,
specifically to avoid the JSON-output interaction bug class #4669/#39344
both lived in; (b) derive the minimal template *by subtraction* from
`calm-nudge.sh`'s already-fixed logic for the 2 kept gates, rather than a
rewrite; (c) design a shadow-mode-first rollout before any real external
hard-deny ships; (d) fix the cross-item message coupling (Abductive 2); (e)
new — message this to users honestly as best-effort/defense-in-depth, never
as guaranteed-unbypassable, matching Anthropic's own framing of what hooks
are and are not reliable for; (f) new — file the same `exit 2` finding as a
follow-up for `calm-nudge.sh` itself, independent of Item B's own fate.
