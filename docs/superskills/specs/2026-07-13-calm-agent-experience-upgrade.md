---
title: CALM agent-experience upgrade — safety-by-construction + UX friction reduction
date: 2026-07-13
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

Two bugs fixed earlier this session (F10-era `apply_hunks` symbol-boundary
fusion causing false `PARSE_ERROR`; silent local `config.json` override
masking `Config::default()` changes) share a root pattern: CALM's current
safety properties often depend on *this specific agent's* care, memory, or
just-read source familiarity, not on a structural guarantee. Comparing this
session's clean CALM usage against a prior Claude session's (which
progressively lost trust in CALM near the end and partially reverted to
native file tools) showed that the clean run was explained by inherited
`MEMORY.md` caution and a pre-investigated task shape — not by CALM itself
being safe for an arbitrary agent, model, or cold-start repo. Separately,
both sessions independently hit the same concrete UX friction points using
CALM's edit tools day to day.

## Design

Four tiers of proposed CALM upgrades, ranked by combined leverage across
accuracy, risk reduction, agent cognitive load, and UX smoothness (full
reasoning in conversation; see `calm-safety-by-construction-design-lens.md`
in the assistant's memory for the underlying litmus test: "strip away
agent memory/model/repo history — does the guarantee still hold?").

**Status (verified against real code 2026-07-14, not memory/comments — see the
VHEATM re-audit that triggered this check): items 1-3 below are IMPLEMENTED
and tested, not proposals. Verified live:** `symbols.boundary_ambiguous`
column exists and is written by `graph::boundary::update_boundary_ambiguous_flags`
(called from both reindex paths in `indexer/pipeline.rs`); `fitness_report`
surfaces `boundary_ambiguous_count` with a `max_boundary_ambiguous_count`
threshold (`fitness.rs`); `edit_symbol`'s replace path refuses when
`c.boundary_ambiguous` is set (`tools/edit.rs:139`, tests in `tools.rs`
`edit_symbol_replace_refuses_a_boundary_ambiguous_symbol` +
`edit_symbol_old_text_mode_refuses_on_boundary_ambiguous_symbol`); the
`old_text` small-text-match mode (item 2) is live on `edit_symbol`; the
per-(file,symbol) native-Edit re-arm (item 3) is live in
`.claude/hooks/calm-nudge.sh`. A `fitness_report` count > 0 for
`boundary_ambiguous_count` reflects real ambiguous symbols the mechanism
correctly caught, not a missing feature — do not re-implement item 1 or 2
without re-checking the code first. Items 4-8 were not re-verified in this
pass; do not assume their status from this note.

**Tier 1 — highest combined leverage:**
1. Index-time symbol-boundary-integrity check: at parse time (`walk_symbols`),
   compare consecutive top-level symbols' start/end rows; if two symbols
   share a physical line, store a `boundary_ambiguous` flag per symbol
   (new `symbols` column, same pattern as the existing `hub_kind` column).
   Surface as a new `fitness_report` health category; `edit_symbol`'s
   replace path checks the flag and refuses proactively, before attempting
   a write, regardless of what caused the ambiguity.
2. Small-text-match edit mode: accept `old_text`/`new_text` (no line
   numbers, no pre-fetched hash) scoped to a resolved symbol's range;
   server searches for `old_text`, requires exactly one match, reuses the
   existing "ambiguous" reporting UX (already used by `resolve_symbol`) if
   0 or >1 matches are found, then applies via the existing hash-verified
   `apply_hunks` pipeline.
3. Re-arm the native-Edit-block hook per (file, symbol) touched this
   session, not once per session — currently "first native Edit this
   session is denied until edit_context called" unlocks native Edit for
   the *entire remaining session* after the first `edit_context` call,
   regardless of which file is later touched.

**Tier 2 — cheap, do opportunistically:**
4. Default anchor for brand-new module-level content — `position=
   "top_of_file"`/`"end_of_file"` on `edit_symbol`, not requiring an
   existing sibling symbol as an anchor.
5. Suppress the "content also appears elsewhere" ambiguity warning when
   `position` is `"before"`/`"after"`/`"append_inside"` — those modes
   already re-anchor via a fresh live parse, so the hash-collision warning
   (meant for raw line-range hash matching) doesn't apply and is pure noise.
6. Daemon respawn sends an MCP `tools/list_changed` notification so a
   connected client refreshes its cached tool schema, instead of the
   client silently keeping a stale schema for the rest of the session.

**Tier 3 — needs careful design before building, not an easy win:**
7. Scope `REASON_NOT_GROUNDED` down for edits that are provably
   behavior-preserving (e.g. pure reformatting/whitespace), so a
   zero-semantic-change edit doesn't require fabricating a caller-citing
   `reason`. Requires a real structural (AST-shape-ignoring-trivia)
   equivalence check between old and new content, not a naive
   whitespace-stripped text comparison — the latter would be unsound for
   indentation-significant languages.

**Tier 4 — protects future development quality, not today's runtime UX:**
8. Property-based/fuzz test harness for `apply_hunks`' line-splice
   invariants (random original file + random hunks, asserting the output
   always round-trips cleanly through `split_lines_inclusive`) — the
   missing-trailing-newline bug fixed this session slipped past 20+
   hand-written unit tests in `edit.rs`.

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-13 | trigger: NORMAL (no prior brainstorming spec existed; audited directly from chat-researched design per explicit user request) -->

**Tier:** 2 | **Date:** 2026-07-13

### Failure Modes
1. `boundary_ambiguous` flag goes stale/wrong after a reindex and nobody
   notices, because the project has already shipped this *exact* class of
   bug twice this session alone (F12's config mtime-cache, and the Bug 2
   root cause: a cached value not being correctly recomputed/invalidated)
   — a new per-symbol flag computed at parse time is one more piece of
   state that must be correctly invalidated on every partial reindex path,
   not just the common one — HIGH — mitigation in plan: NO (not yet
   specified; needs an explicit invalidation/idempotency test before this
   is buildable, see Assumptions).
2. Small-text-match mode (item 2) inherits and likely *worsens* the exact
   ambiguous-match problem it's meant to reduce friction for: this
   session's own edits already hit "content also appears elsewhere (34-46
   identical occurrences)" warnings on whole-line hash matching; scoping
   down to short sub-line snippets only increases collision probability in
   boilerplate-heavy code (this repo has many `Err(e) => return
   db_error(e)`-style repeated lines) — MEDIUM — mitigation in plan: NO
   (design doesn't yet specify graceful disambiguation/context-expansion
   UX for the 0-or-N-match case, only "reuse ambiguous reporting").
3. Per-file/symbol re-arm (item 3) could *increase* native-Edit reversion
   instead of reducing it, if shipped without also upgrading the gate's
   error message to explicitly name "you haven't called edit_context on
   *this* file/symbol this session" — this is the same trust-erosion
   dynamic identified in this session's own reflection (a confusing gate
   hit mid-task made a prior agent generalize "CALM is unreliable here" to
   unrelated subsequent calls); tightening a gate without tightening its
   diagnosability repeats that exact mechanism at a new site — HIGH —
   mitigation in plan: NO (design currently only specifies the gate
   change, not a paired message upgrade).

### Layer Signals
- L2 Concurrency: `boundary_ambiguous` (item 1) is a new column on the
  same `symbols` table already subject to the multi-client/cross-process
  write races this project fixed before (see memory: multi-client
  concurrency fixes, cross-process edit race) — must reuse the existing
  write-lock discipline, not a new ad hoc path.
- L3 Data: item 1 needs an actual schema migration on `symbols` — the
  `hub_kind` column addition (F10) is a usable precedent to follow, not a
  green field.
- L5 Security: item 7 (Tier 3) loosens a safety gate (`REASON_NOT_GROUNDED`)
  — same *family* of change this project's own memory already flags
  ("Security bypass needs explicit ask — never auto-apply ... ask the user
  first"). Not identical (not a CVE/advisory bypass), but gate-loosening
  changes deserve the same explicit-sign-off posture, not a silent fold
  into a routine implementation pass.
- L6 Observability: item 1's `fitness_report` surfacing is pull-based (an
  agent must think to call it) — consistent with every other existing
  `fitness_report` check, so not a new gap introduced by this design, just
  worth naming as an inherited limitation.
- Other layers: no signal.

### Assumptions to Verify
- ASSUMED: `reindex_paths` always fully re-parses the touched file (so
  `boundary_ambiguous` recomputes correctly for both symbols on either
  side of an edit) — not verified against any partial/symbol-level
  incremental-reindex code path that might skip recomputing an adjacent,
  untouched symbol's flag.
- ASSUMED: short `old_text` snippets in item 2 will "usually" be unique
  enough to match exactly once — not verified against this repo's actual
  boilerplate density; the 34-46x collision counts observed live this
  session suggest the opposite for short/common snippets specifically.
- ASSUMED: re-arming the block hook per file (item 3) is "cheap" (a few
  hundred tokens) — asserted, not measured against a real multi-file
  refactor task.
- DEFERRED/TBD: item 7's exact structural-equivalence mechanism
  ("AST-shape-ignoring-trivia") has no concrete implementation yet, and
  needs to be sound per-language (indentation-significant languages like
  Python cannot use a naive whitespace-stripped comparison) before this
  tier is even plannable.

### Abductive Hypotheses
1. Interaction between two individually-fine features: if a symbol is
   already flagged `boundary_ambiguous` (item 1), does the new
   small-text-match mode (item 2) still scope its search to that symbol's
   `[line_start, line_end]` correctly? If that symbol's own boundary is
   the thing in question, the search range itself may be wrong, silently
   missing the intended match or matching in unintended territory. Item 2
   should explicitly check and refuse (or widen scope with a warning) when
   the target symbol carries the item-1 flag — not currently cross-
   referenced in the design.
2. Failure only visible at scale: a repo with large amounts of
   near-identical generated/vendored code would produce pathological
   false-positive floods for item 1 (many legitimately-adjacent-looking
   symbols in minified output) and pathological match-collisions for item
   2 (short snippets matching hundreds of times) simultaneously. Partially
   mitigated already since CALM's indexer categorically skips
   `node_modules`/`vendor`/`dist`/etc., but item 1's new code path must be
   confirmed to respect the same exclusion list rather than introducing an
   independent one that could drift out of sync.

### Gate Result
<!-- PASS WITH FLAGS -->
PASS WITH FLAGS — proceed to `writing-plans`, but the plan MUST fold in
these mitigations as first-class tasks, not follow-ups:
- Item 1 ships with an explicit flag-invalidation test (edit a file,
  confirm the flag clears/updates on reindex) and reuses existing
  write-lock discipline + the `hub_kind` migration pattern.
- Item 2 ships with a defined 0-or-N-match UX (at minimum: report the N
  locations found, same shape as symbol-ambiguity resolution) and is
  explicitly gated behind item 1's `boundary_ambiguous` flag per
  abductive hypothesis 1.
- Item 3 ships together with an upgraded gate error message in the same
  change — never land the stricter gate before the clearer message.
- Item 7 (Tier 3) requires explicit user sign-off on the specific
  structural-equivalence mechanism before implementation starts — do not
  fold into a routine pass alongside Tier 1/2 items.
