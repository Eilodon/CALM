---
title: CALM agent-experience round 2 — hook/AGENTS.md UX findings, root causes, and a distill-guide prompt/skill
date: 2026-07-14
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

A live self-audit this session (reading `AGENTS.md`, `.claude/hooks/calm-nudge.sh`
in full, `settings.json`, both `session-start-*.sh` hooks, and — critically —
querying the real accumulated `.calm/.hook-state/decisions.jsonl` log, 1296
entries across 6 real Claude Code sessions + 11 test/manual sessions) surfaced
5 concrete findings, each now root-caused against real code/data, not guessed:

1. **SessionStart re-injects all ~17KB of AGENTS.md + reruns `cargo build
   --quiet -p calm-cli` on every conversation turn, not once per logical
   session.** Confirmed live, not inferred: `mcp__calm__session_context()`
   called twice in this same conversation returned two different
   `session_started_at` values (07:42:42Z then 07:47:31Z) and a changed
   `other_active_sessions` roster (server-side session id 1 replaced by 13).
   Per AGENTS.md's own Stage 8 rule ("`session_started_at` changed → server
   restarted"), this proves the MCP client reconnects to the CALM daemon on
   every turn in this harness (VSCode-extension / Agent SDK), which Claude
   Code's SessionStart matcher `"*"` (covers `resume` too) reacts to by
   rerunning both `SessionStart` hooks from scratch every time. Neither
   `session-start-agents-md.sh` nor `session-start-build-calm.sh` has any
   idempotency check — unlike `calm-nudge.sh`, which already has a
   session_id-keyed state file pattern (`.calm/.hook-state/<session_id>.json`)
   that could be reused.

2. **The `edit_context` hard-deny gate in `calm-nudge.sh`'s `Edit`/`Write`
   branches applies identically to Markdown/doc files as to real code**, and
   this is not just theoretical — 2 of the only 4 real hard-denies ever
   logged fired on `AGENTS.md` itself and a `docs/plans/*.md` file. Root-caused
   against `crates/calm-core/src/graph/hub.rs::update_is_hub_flags` (lines
   11-86): the hub query source is `WHERE caller_count >= 1 OR coreness > 0`.
   Markdown headings (`kind='heading'`, confirmed via
   `crates/calm-core/src/indexer/pipeline.rs::test_markdown_headings_end_to_end`)
   never get call-graph edges, so `caller_count`/`coreness` are always 0 —
   `is_hub` is provably always `false` for a doc heading. The gate's safety
   value for Markdown is zero; its cost is one forced round-trip. The precise
   code gap: `calm-nudge.sh`'s `Edit` branch (~line 941) never calls the
   already-existing `is_clearly_non_code_file` helper (it's used in the
   `Grep` branch but not `Edit`/`Write`) — an inconsistency between two
   structurally similar branches in the same file.

3. **Aggregate native-vs-CALM tool-choice ratio is ~50/50 despite the nudge
   machinery, but this hides bimodal, enforcement-driven behavior — not a
   messaging problem.** Per-session breakdown of real `decisions.jsonl` data:

   | Session | edit_symbol+edit_lines vs native Edit+Write | source vs native Read |
   |---|---|---|
   | 75bc8f50 | 39 vs 17 (~70% CALM) | 79 vs 46 (~63% CALM) |
   | 262a0331 | 20 vs 18 (~53% CALM) | 25 vs 22 (~53% CALM) |
   | a48f84f9 | 0 vs 22 (0% CALM) | 5 vs 25 (17% CALM) |
   | e466742f | 24 vs 14 (~63% CALM) | 1 vs 26 (4% CALM) |

   The hard-gated action (edit_context-before-edit) is well-complied-with in
   every session. The advisory-only action (prefer `source`/`locate` over
   `Read`) swings 4%-70% session to session. This matches the hook's own
   shadow-mode measurement already in place: 26 logged `would_deny:
   "read_native"` events (native `Read` hitting a file CALM's index confirms
   it tracks — exactly what a hypothetical hard gate would have blocked) vs
   only 1 `would_deny: "grep_tool"`. The infrastructure to decide whether
   hardening Read/Grep is safe already exists (`is_indexed_file` ground
   truth + shadow logging); nobody has acted on the 26 data points yet.

4. **`decisions.jsonl` (the real production log) is contaminated by
   manual/test sessions, contradicting a code comment's own claim.**
   `CALM_NUDGE_STATE_DIR` is exported only inside `test-calm-nudge.sh`
   (lines 18-19). Any ad-hoc manual invocation of `calm-nudge.sh` during
   development (piping synthetic JSON directly to verify one behavior) never
   goes through that wrapper, so it falls through to the hardcoded default
   `.calm/.hook-state` and writes into the same log real sessions use. Verified:
   12 of 17 distinct `session_id` values in the real log are test/manual-tagged
   (`audit-test`, `audit-test2/3/4`, `b2-test-852016`, `b2-verify-854976`,
   `b3-test-801315`, `manual-test-1303174/2/3`, `shadow-test-714742`) — not
   the 6 genuine Claude Code UUIDs. The comment at `calm-nudge.sh` (~line 77)
   claims this isolation "removes the need to filter after the fact at all"
   — false in observed practice. Worse, the doc's own proposed retroactive
   filter (match `session_id` against `^test-`) would still miss
   `b2-verify-854976`, which contains no `test` substring at all — the
   fallback safety net has a hole too.

5. **Hook overhead is paid unconditionally on every Bash call regardless of
   relevance.** `calm-nudge.sh` lines 78-90 (`mkdir`, a `find -mtime +1
   -delete` directory scan, `cat` state file, 3 separate `jq` parses) run
   before the `tool_name` dispatch that would determine whether any of that
   was needed. Real data: session `75bc8f50` alone logged 385 Bash-triggered
   hook invocations, of which the grep/find-specific branches (`bash_grep`,
   `bash_grep_post`, `bash_find`) account for well under 15% of Bash calls
   project-wide — the rest (builds, git status, ls, etc.) still pay full
   state-load cost for a guaranteed silent "allow".

Separately, the user asked to evaluate whether a Claude Code Skill and/or MCP
Prompt that "distills" AGENTS.md's optimal-usage guide — invokable either by
the agent itself (proactive, via the `Skill` tool) or manually by a user
(slash-command) — would out-perform the current always-on SessionStart
injection. Investigated: the 3 existing MCP Prompts (`review_symbol`,
`debug_symbol`, `onboard_area`, plus a 4th, `review_pr`, not yet reflected in
`render_prompt`'s own doc comment which still says "exactly 3 prompts" —
another small doc/code drift) are all task-scoped (need a `symbol`/`path`/
`range` argument) — none teaches "how to use CALM in general." That gap is
real and not previously covered. Also: `session-start-agents-md.sh` is
Claude-Code-specific — any other MCP client connected to the same `calm
serve` (Cursor, plain VS Code MCP, etc.) gets zero automatic onboarding today.

## Design

Five root-caused fixes plus one new capability, ranked by cost/value/risk —
same ranking already proposed in conversation, captured here for audit:

**F1 — idempotent SessionStart (addresses finding 1).**
Add a `session_id`-keyed "seen this session" state file to
`session-start-agents-md.sh`, mirroring `calm-nudge.sh`'s existing pattern.
First occurrence of a given `session_id`: inject full AGENTS.md as today.
Repeat occurrence of the same `session_id` (per the confirmed-live
reconnect-per-turn behavior): inject only the existing 11-line banner plus a
pointer to the full guide via the new Skill/Prompt (F6) — never fully silent,
to avoid the "guidance silently missing after a context compaction" risk.
`session-start-build-calm.sh` gets an independent, cheaper mitigation: skip
the synchronous `cargo build` if a sibling marker file shows a successful
build within the last N seconds, since its actual purpose (win the MCP-dial
race) only needs to run once per real cold start, not once per turn.

**F2 — exempt prose files from the Edit/Write hard-deny gate (finding 2).**
In `calm-nudge.sh`'s `Edit` and `Write` branches, add the same
`is_clearly_non_code_file` check the `Grep` branch already has: when
`is_indexed_file` is true AND `is_clearly_non_code_file` is true, downgrade
from `deny` to the existing `nudge` path instead. Stage 7's `diff_impact`
gate is untouched (still applies to every write regardless of file kind) —
only the pre-edit blast-radius ceremony is removed, and only for a file class
where `is_hub` is provably always false (see hub.rs analysis above).

**F3 — targeted next step, not yet a code change: hand-audit the 26 logged
`would_deny: "read_native"` events (finding 3).** Before flipping Read/Grep
to a real hard gate, sample those 26 real events from `decisions.jsonl` and
label each "native was actually fine" vs "CALM tool would have served this
better." This is cheap (read-only analysis of already-logged data) and is the
exact prerequisite the hook's own shadow-mode comment says it exists for. Do
not implement a hard gate before this review — the risk of a bad hard gate
here is asymmetric (Read/Grep fire 10-40x more often than Edit per AGENTS.md's
own "steepest hill" analysis), so this step is a hard prerequisite, not an
optional nicety.

**F4 — allowlist-based session isolation for decisions.jsonl (finding 4).**
Replace the denylist assumption ("test sessions are prefixed `test-`") with a
positive UUIDv4 shape check on `session_id` inside `calm-nudge.sh` itself
(not just the test runner). Any `session_id` that is neither a real UUIDv4
nor has `CALM_NUDGE_STATE_DIR` set gets logged to a separate
`decisions.jsonl.manual` file instead of the shared production log — self-
defending at the write site, not dependent on a human remembering to export
a variable before every ad-hoc manual test.

**F5 — reduce unconditional Bash-hook overhead (finding 5).**
Two independent, additive changes: (a) move the `find -mtime +1 -delete`
session-state cleanup from "every invocation" to a cheap probabilistic
trigger (e.g. 1-in-20), since it's a maintenance task with no correctness
requirement to run every time; (b) add a fast pure-bash pre-check (no `jq`)
for `tool_name == "Bash"` that skips straight to `maybe_nudge_session_context`
when `command` contains none of `git|grep|rg|ag|find|rustfmt|cargo fmt`,
before paying the 3-way `jq` state parse that only the matching branches
actually need.

**F6 — new capability: a general "how to use CALM" distill guide, exposed
both as a Claude Code Skill and an MCP Prompt.** Two delivery mechanisms for
two different trigger modes, not a either/or choice:
- A Claude Code Skill (`.claude/skills/calm-guide/SKILL.md`) with a
  `description` tuned for proactive self-matching (mirroring how `verify`,
  `code-review`, etc. already work live in this environment) — lets the
  agent itself call it mid-session without the user asking.
  Also reachable manually as `/calm-guide`.
- A new MCP Prompt (e.g. `calm_workflow()`, no required argument) added to
  `render_prompt`/`ci_prompts` in `crates/calm-server/src/tools.rs` (fixing
  the stale "exactly 3 prompts" doc comment in the same change) — the only
  mechanism that reaches non-Claude-Code MCP clients, consistent with this
  project's existing multi-client design goal (the cross-SDK interop CI just
  shipped this session exists for exactly this class of concern).

Both surface the same full stage-by-stage content AGENTS.md carries today.
Explicitly NOT a replacement for AGENTS.md's SessionStart injection or for
`calm-nudge.sh`'s hard gates — Finding 3's data shows advisory-only
mechanisms (which a self-triggered Skill inherently is) achieve 4-70%
compliance depending on session, so swapping the forced channel for a
pull-only one would likely regress compliance, not improve it. The
hard-deny gates are already self-contained (each deny message is fully
actionable without AGENTS.md ever having been read — verified against the 4
real deny messages in the log) and are unaffected by any of this. F6's value
is (a) closing the non-Claude-Code onboarding gap, and (b) giving a cheap,
on-demand full-detail reference once F1 shrinks the default injection to a
banner.

**Priority order for implementation** (cost/value, independent of each
other — no sequencing dependency between them except F3 gating any future
Read/Grep hard-gate work):
1. F4 (cheap, fixes a false claim in the code itself)
2. F2 (cheap, removes real measured friction with zero safety cost)
3. F1 (medium cost, needs the "downgrade not silence" safeguard)
4. F6 (medium cost, highest long-term value — closes a real cross-client gap)
5. F3 (analysis only, must happen before any future hard-gate change)
6. F5 (pure performance, lowest urgency)

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE -- update this section, do not append a second one -->
<!-- last-run: 2026-07-14 | trigger: NORMAL -->

**Tier:** 2 (Production) | **Date:** 2026-07-14

### Failure Modes
1. **F1's session_id-only dedup silently drops guidance on `/clear`/`/compact`** -- HIGH -- mitigation in plan: NO (spec revision required, see below). SessionStart's matcher `"*"` covers `startup`/`resume`/`clear`/`compact` alike. `session_id` is stable across a `/clear`/`/compact` (same logical conversation, context wiped) as much as across a per-turn `resume` -- a dedup keyed on `session_id` alone cannot tell these apart, so it would wrongly serve the banner-only response right after a context wipe, exactly when the FULL guide is needed most. The spec never captured the real SessionStart JSON payload to confirm a `source` field exists to distinguish these -- this was asserted as safe without verification.
2. **F2 reuses `is_clearly_non_code_file`'s denylist wholesale, which is broader than the root-cause justification** -- MEDIUM-HIGH -- mitigation in plan: NO. The hub.rs proof (`caller_count`/`coreness` always 0) was established specifically for Markdown headings. The denylist this fix reuses also covers `.yml`/`.json`/`.toml`/`.lock` -- files that CAN have real operational blast radius (a broken CI YAML, a broken `Cargo.lock`) CALM's call graph simply doesn't model, which is a different situation from "a doc heading nobody calls." Downgrading the hard gate for all of them on the Markdown argument alone over-generalizes.
3. **F5's early-exit fast-path could reintroduce the exact silent-skip logging bug this project's own `decisions.jsonl` exists to catch** -- HIGH -- mitigation in plan: NO. `trap log_decision EXIT` (line 151) must still fire on every invocation, including the new fast pure-bash path -- if the fast-path is implemented as an early `exit` before that trap is registered, or bypasses `log_decision` entirely for "efficiency," every Bash call it short-circuits becomes invisible to the audit trail, mirroring the exact gortex issue #241 (91%-of-calls-silently-skipped) the header comment of this same file already cites as the reason per-invocation logging matters.

### Layer Signals
- **L1 Logic:** every new branch in F1/F2/F4/F5 is untested -- the spec's priority list never mentions extending `test-calm-nudge.sh` with a case for any of them. Gap: add test cases before/alongside implementation, not after.
- **L2 Concurrency:** `save_state`/`bump`'s read-modify-write (`cat` + `jq` + redirect, no lock) is a pre-existing TOCTOU race on `.calm/.hook-state/*.json`; F1/F4/F5 all add more code paths touching the same files, widening the surface without fixing the underlying race. Not a new problem, not blocking, but should be a one-line acknowledgment in the plan, not silently inherited.
- **L3 Data:** F4 changes what's captured in `decisions.jsonl` going forward with no export/snapshot step for the *current*, already-contaminated-but-analyzable log -- F3's proposed hand-audit of the 26 `would_deny` events should run (or at least snapshot the file) before F4 ships, so the exact dataset F3 references stays reproducible.
- **L4 Integration:** no signal.
- **L5 Security:** no signal.
- **L6 Observability:** real gap -- none of F1/F2/F4/F5 propose any way to detect their own regression in production. Given Failure Modes 1 and 3 are both *silent*-failure classes, this matters concretely, not abstractly: F1 needs a way to confirm banner-only mode still lets a session complete Stage 1-8 correctly; F5 needs a test asserting `decisions.jsonl` still receives a line on the fast-path.
- **L7 Cross-cutting (idempotency):** see Failure Mode 1 -- this IS the cross-cutting concern. The fix: verify the real SessionStart payload shape first (dump one via a throwaway hook, same methodology this project already used for the PostToolUse Bash-grep payload); dedup key must be `(session_id, source not in {clear, compact})`, not `session_id` alone.

### Assumptions to Verify
- **ASSUMED:** "the MCP client reconnects to the CALM daemon every turn" -- strongly evidenced (session_started_at changed mid-conversation) but never directly observed at the SessionStart-hook-payload level. Verify by capturing one real SessionStart JSON payload before coding F1.
- **ASSUMED:** SessionStart's hook JSON contains a `session_id` field at all, and a `source`/event-type field -- neither has been captured from a real payload in this repo yet.
- **ASSUMED:** "all repeat SessionStart firings are harmless to skip" -- disproven by the L7 finding above (clear/compact firings are not harmless to skip).
- **ASSUMED:** "`is_clearly_non_code_file`'s existing denylist is an appropriate proxy for 'no real blast radius'" -- shown too broad in Failure Mode 2.
- **ASSUMED:** "no other tooling currently depends on `decisions.jsonl` containing 100% of manual-test traffic" -- not checked before proposing F4.
- **ASSUMED (deferred, not blocking):** F6's Skill will actually achieve reliable proactive self-triggering -- Finding 3's own data (4-70% compliance variance for advisory-only mechanisms) argues this is optimistic on its own; mitigated only as long as F1's banner keeps pointing to it every session, which depends on Failure Mode 1 being fixed first.

### Abductive Hypotheses
1. **F1 (banner-only after first fire) + F6 (full guide moved off SessionStart, onto on-demand Skill/Prompt) + this environment's own documented context-compaction behavior interact badly:** a mid-session compaction can summarize away both the original full injection AND the banner's pointer text. Nothing then prompts the agent to re-invoke the Skill, since the hint that it exists was itself just compacted away -- the agent could end a long session with zero CALM guidance in context, strictly worse than today's wasteful-but-always-present design. Each component is individually reasonable; the failure only emerges from the combination plus a correct, unrelated fourth component (the harness's own compaction). HIGH.
2. **F4's UUID allowlist creates a false sense of a unified, trustworthy `session_id` key across the whole stack at scale:** `calm-nudge.sh`'s `session_id` (Claude Code UUIDs) and CALM-server's own internal session numbering (small sequential ints, visible via `session_context()` -- observed live this session as 1, 3, 6, 8, 13) are two entirely separate namespaces with no correlation. A future cross-layer analysis tool (plausible, given how much this project already correlates `daemon.log`/`audit.log`/`decisions.jsonl`) could be misled into treating a now-"clean" `decisions.jsonl` as fully correlatable against CALM-server's own session concept, when F4 only cleaned the Claude-Code-layer half. MEDIUM -- worth a one-line doc note, not blocking.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
HOLD on F1 and F2 specifically -- both have a concrete, cheaply-fixable design defect (not just an implementation-time mitigation): F1's dedup key must become source-aware, F2's scope must narrow from the full non-code denylist to prose-only. F4, F5, F6 proceed as PASS WITH FLAGS once their listed mitigations (test coverage, log-ordering guarantee, snapshot-before-write-change, doc note) are carried into the plan. F3 is analysis-only and unaffected. Revised F1/F2 design applied directly below rather than looping back through brainstorming, since both fixes are narrow and already fully specified by this audit.

## Revisions Applied Post-Audit

**F1 (revised):** dedup key becomes `(session_id, is_clear_or_compact)`, not `session_id` alone. Before implementing, capture one real `SessionStart` hook JSON payload (throwaway dump, same method already used for the PostToolUse Bash-grep payload) to confirm the actual field name/values Claude Code sends for the source event type. Full AGENTS.md injects whenever `source` is `startup`/`clear`/`compact`, or whenever `source` is absent from the confirmed schema (fail toward re-injecting, not toward silence, exactly `is_real_git_commit_or_push`'s existing "ambiguous parse fails toward enforcing" philosophy applied to a new context). Only a confirmed `resume` fires the banner-only path.

**F2 (revised):** scope narrowed from `is_clearly_non_code_file` (which also matches `.yml`/`.json`/`.toml`/`.lock`) to a new, smaller prose-only check (`.md`/`.txt` only) -- config/lock files keep the full hard gate, since they can carry real operational blast radius outside CALM's call graph. Introduce a distinct helper (e.g. `is_prose_file`) rather than repurposing the existing denylist, so the two concerns ("not source code, for search-nudge purposes" vs "provably has no blast radius, for edit-gate purposes") don't silently drift back together if either list changes later.

**F5 (mitigation, not a design change):** the fast pure-bash pre-check must sit *after* `trap log_decision EXIT` is registered (already line 151, before any dispatch) and must still fall through to it — implemented as an early `return`-equivalent that still reaches the trap, never a bare `exit` that could bypass it. Add one `test-calm-nudge.sh` case asserting a `cargo build`-style irrelevant Bash command still produces exactly one `decisions.jsonl` line.

## F3 Results — hand-audit of the would_deny:read_native shadow log

Ran 2026-07-14, after F2/F4 shipped. 27 real events in `decisions.jsonl` carry `would_deny: "read_native"` (grew from the 26 seen during the audit pass — a few landed during this same implementation session). Split by extension:

- **11/27 (41%) are `.md` files** (`AGENTS.md` ×6, `README.md` ×2,
  `docs/plans/2026-07-14-calm-agent-experience-audit-and-backlog.md` ×1,
  `docs/superskills/specs/2026-07-13-calm-agent-experience-upgrade.md` ×2).
  Whole-file native `Read` on prose is frequently the *correct* choice —
  narrative docs don't decompose into an independently-useful symbol the
  way a function does — so this slice is NOT a clean "miss" pool. Same
  underlying insight as F2 (prose vs. code needs a different rule), now
  confirmed on the read side too, not just edit_context.
- **16/27 (59%) are `.rs` files**, and every one of the 9 distinct files
  behind them is substantial: `crates/calm-server/src/tools.rs` (8183
  lines), `crates/calm-server/src/tools/edit.rs` (1640),
  `crates/calm-server/src/tools/inspect.rs` (1132),
  `crates/calm-core/src/edit.rs` (1102), `crates/calm-server/src/tools/
  orient.rs` (772), `crates/calm-cli/tests/daemon_integration.rs` (542),
  `crates/calm-server/src/tools/recover.rs` (606),
  `crates/calm-core/src/memory.rs` (484),
  `crates/calm-server/src/tools/testgap.rs` (313). Zero short/borderline
  files in this set — `file_worth_symbol_read`'s 80-line threshold is
  doing its job correctly for code. Every one of these 16 is a genuine,
  clean case where `source(symbol)`/`file_overview` would have avoided
  flooding context with thousands of irrelevant lines.

**Conclusion for any future Read/Grep hard-gate work:** the real-code slice
(16/27, 59%) supports that a hard gate would be both safe and correctly
targeted — no false-positive candidates found among them. The prose slice
(11/27, 41%) should NOT feed that decision as-is; a future hard gate must
reuse F2's `is_prose_file()` exemption (or an equivalent) rather than
gating on `is_indexed_file` + length alone, the same way F2 already had to
learn for the edit_context gate. This halves the effective evidence pool
for "is hardening safe" from 27 down to 16 — still zero observed false
positives in that narrowed pool, but a smaller sample than the raw count
suggested. No hard-gate code changed as part of this pass (F3 was
analysis-only per the audit-design gate).

## F1 Results — finalized with a real captured payload

The temporary `sessionstart-discovery-dump.sh` hook captured a real `SessionStart` payload for this exact conversation before any permanent logic was written, confirming both assumptions the audit flagged as unverified:

```json
{"session_id": "0727c625-8339-4ef1-b2c5-dfd873942623", "hook_event_name": "SessionStart", "source": "resume"}
```

`session_id` is present; `source` is present with exactly the values expected (`startup` for two other concurrent sessions on this repo, `resume` for this session's own reconnect). Implemented the audit's revised design exactly: `session-start-agents-md.sh` now keys a "seen" marker (`.calm/.hook-state/sessionstart-seen/<session_id>`) and only serves the short banner when `source == "resume"` **and** that marker already exists — any other `source` value (`startup`, `clear`, `compact`, or absent/unrecognized) always gets the full `AGENTS.md` injection, fail-safe toward re-injecting rather than silence.

Verified live across 6 scenarios (`.claude/hooks/test-session-start-agents-md.sh`, all PASS, run twice for idempotency):
1. `startup`, fresh `session_id` → full inject, marker created.
2. `resume`, same `session_id` (marker exists) → short banner naming the `calm-guide` Skill.
3. `resume`, a *different* never-seen `session_id` → full inject (dedup only applies to an already-seen id).
4. `clear`, same `session_id` as (1) → full inject — **this is the exact bug the audit's pre-mortem caught** in the first draft (session_id-only dedup would have wrongly served the banner right after context was wiped).
5. `compact`, same `session_id` → full inject, same reasoning as (4).
6. `source` field missing entirely → full inject (fail-safe).

The temporary discovery-dump hook and its `settings.json` entry were removed once the schema was confirmed and the permanent logic landed, per its own header comment's instruction.
