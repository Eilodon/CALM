---
title: CALM remaining backlog — graduated Read/Grep hard-gate + DEBT-007 state-file locking
date: 2026-07-14
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

Two items were deliberately left open by the prior audit-design pass
(`docs/superskills/specs/2026-07-14-calm-agent-experience-round2-fixes.md`,
F3 and the L2 finding) rather than resolved blind. Both now have real
measurement behind them, gathered before writing this design — same
"verify against real data first" discipline the rest of this project uses.

**A. Read/Grep is still advisory-only, but the mechanism that was supposed
to inform hardening it now has real data.** `AGENTS.md` Mandatory Rule 4
documents *why* this was left soft on purpose: "a hard deny on every
Read/Grep proved too failure-prone... it fires 10-40x more often per
session than an edit, and it competes with a deeper pretraining habit than
Edit/Write does." The shadow-mode `would_deny` field exists specifically
"so the false-positive rate can be measured from real sessions before ever
flipping enforcement." F3 (prior pass) did that measurement: 27 real
`would_deny: "read_native"` events, split 11 prose (not real misses, same
insight as F2) / 16 real code files, **zero false positives**, every file
substantial (313-8183 lines). 16 clean samples across ~4-5 real sessions is
real evidence, but still a modest N against a mechanism that fires far more
often than Edit — the asymmetric-risk argument in Rule 4 is a reason for
caution in *how* we harden, not a reason to keep doing nothing forever.

**B. `DEBT-007-hook-state-toctou-race`** (just opened this session,
`docs/pattern-debt-registry.yaml`) flagged `calm-nudge.sh`'s
`save_state()`/`bump()` read-modify-write on `.calm/.hook-state/
<session_id>.json` as unlocked, but rated it `low` urgency assuming real
concurrent same-session hook invocations are rare. **That assumption was
wrong — measured just now, not assumed:** `decisions.jsonl` has 92 distinct
(timestamp, session_id) seconds where 2+ hook invocations landed in the
exact same wall-clock second, out of ~1300 total lines. This is consistent
with parallel tool-call dispatch within a single assistant turn (the
pattern this exact session's own tool calls have used repeatedly, per its
own operating instructions to batch independent calls together) — not a
rare edge case. The registry's `low`/`current_control: "severity thực tế
thấp hơn lý thuyết"` note is now stale and should be corrected as part of
this pass, independent of whether a fix ships.

## Design

**A. Graduated escalation for `read_native`/`grep_tool`, not an immediate
hard deny.** Reuses the existing `nudge_counts[key]` counter
(`calm-nudge.sh`) rather than new state:
- 1st and 2nd matching occurrence of a session (same criteria as today's
  shadow `would_deny`: `is_indexed_file` + `file_worth_symbol_read`/
  leverage check, **and now also `! is_prose_file`** — this is the exact
  fix F3 said any future hardening must carry, not carried yet): nudge,
  unchanged from today.
- 3rd+ occurrence of the *same key* (`read_native` or `grep_tool`
  specifically — NOT `bash_find`/`bash_rustfmt`/`write_native`, which have
  no shadow-mode evidence behind them and must stay nudge-forever) in the
  same session: **deny**, with a message that names the specific tool
  (`source`/`file_overview`/`locate`) and explicitly says this is the 3rd
  time this session, not a generic rule statement.
- New function `nudge_or_deny(key, msg, deny_msg)`, parallel to
  `nudge_or_tally`, used only by the Read and Grep branches in place of
  their current `nudge_or_tally` call.
- Why graduated, not immediate: an agent that reads the nudge and adapts
  after the first or second occurrence never hits a deny at all — zero
  added friction for the common case Rule 4 worried about (a single
  legitimate native use). Only demonstrated, repeated disregard within one
  session escalates. This directly answers Rule 4's stated objection
  (frequency-driven trust erosion from a blanket hard gate) without
  discarding the real evidence F3 gathered.
- Add the `! is_prose_file` check to the Read/Grep advisory path too (not
  just the future hard-gate), closing the exact gap F3 flagged: today a
  `.md` file can still accumulate toward `nudge_counts[read_native]` even
  though F2 already proved prose has no blast radius to protect.

**B. Lock `save_state`/`bump`'s read-modify-write with `flock`.**
`.calm/.hook-state/<session_id>.json` gets a sibling lock file
(`<session_id>.json.lock`); both functions wrap their cat→jq→write
sequence in `flock -w 2 "$lock_fd"` (2s timeout, fail-open to today's
unlocked behavior on timeout rather than blocking a tool call
indefinitely — a lost nudge_counts increment is low-cost, a hung hook
invocation is not). `decisions.jsonl`'s own append (`log_decision`) is
NOT touched — `>>` appends are already atomic for writes under
`PIPE_BUF`, and every line here is well under that, so the race is
specific to the read-modify-write JSON state files, not the log.

**Priority correction from the stale registry note:** given the 92-window
measurement, this should ship as a real fix this pass, not deferred
pending "measure first" — the measurement is done, and it says fix it.

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE -- update this section, do not append a second one -->
<!-- last-run: 2026-07-14 | trigger: NORMAL -->

**Tier:** 2 (Production) | **Date:** 2026-07-14

### Failure Modes
1. **`nudge_counts[key]` escalation conflates "repeated disregard of the same warning" with "N independent, individually-defensible native reads of N different files"** -- HIGH -- mitigation in plan: NO. The counter is keyed `(session_id, "read_native")`, not per-file -- 3 legitimate one-off reads of 3 unrelated large files (no learned pattern, no disregard, just 3 separate real needs) hit the same 3rd-strike deny as 3 repeated ignorings of the identical warning. This directly contradicts the design's own stated intent ("only demonstrated, repeated disregard... escalates") -- as specified, the counter does not actually measure that.
2. **The "16 clean samples" evidence base is far thinner than presented -- checked just now, not assumed.** -- HIGH -- mitigation in plan: NO. Session breakdown of the 16 real-code `would_deny:read_native` events: 13 from ONE session (`e466742f`), 1 from a second real session (`262a0331`), and **2 from synthetic test-fixture sessions** (`manual-test2-1303980`, `shadow-test-714742`) that should have been excluded under this same day's own F4 methodology (positive UUID allowlist for "real traffic"). True independent evidence is 2 real sessions, not 16 events -- nowhere near enough to justify introducing a new deny-gate on the single most frequent tool-call type in every session (AGENTS.md Rule 4's own "10-40x more often than an edit" point cuts hardest here).
3. **Item B's `flock` fail-open assumption ("a lost nudge_counts increment is low-cost") is TRUE today but becomes FALSE if Item A ships in the same pass** -- MEDIUM -- mitigation in plan: NO. Once `nudge_counts` gates a real deny (Item A), a lock-timeout-dropped increment either silently prevents the 3rd-strike from ever firing, or (if two racing writers both fail open) double-counts and fires prematurely. B's own stated low-cost framing was written against the OLD (cosmetic-tally-only) semantics of this same counter.

### Layer Signals
- **L1 Logic:** where exactly `! is_prose_file` gets inserted into Read/Grep's existing nested if/elif isn't specified -- real risk of landing in the wrong branch, mirroring the identical asymmetry F2 first found in Edit (and that Grep's own fallback branch still exhibits with `is_clearly_non_code_file` today).
- **L2 Concurrency:** this IS Item B's subject. The design says "wrap in `flock -w 2`" without specifying the bash idiom -- `flock -c "..."` runs the wrapped command in a subshell, and if any future code inside that block needs to set a variable visible to the caller, a subshell silently breaks that. Needs the fd-based form (`exec {fd}>"$lock"; flock -w 2 "$fd"`) specified before coding, not left to whoever implements it.
- **L3 Data:** the new `<session_id>.json.lock` sibling files aren't covered by the existing `find $state_dir -mtime +1 -delete -name '*.json'` cleanup (glob doesn't match `*.json.lock`) -- orphan accumulation, a new self-inflicted minor debt if not extended.
- **L4 Integration:** no signal.
- **L5 Security:** no signal.
- **L6 Observability:** the design adds a new deny path with no way to measure ITS OWN false-positive rate after shipping -- breaks the exact "shadow-mode before enforcing" discipline that got Item A this far in the first place. If Item A proceeds at all (see Gate Result), it needs its own `would_deny`-style shadow measurement BEFORE going live, not after.
- **L7 Cross-cutting (idempotency):** same identity-conflation class as the audit-design Gotcha added earlier today (SessionStart `session_id`+`source`) -- now recurring in a second, independent design the same day. Worth a more general Gotcha, not just this one instance (see Gotcha entry below).

### Assumptions to Verify
- **ASSUMED, now DISPROVEN:** "16 clean samples... is real evidence" -- checked: 13/16 from one session, 2/16 synthetic. Corrected in Failure Mode 2.
- **ASSUMED, now DISPROVEN under Item A:** `flock`'s fail-open "lost increment is low-cost" -- true standalone, false if Item A ships alongside it (Failure Mode 3).
- **ASSUMED:** exact `is_prose_file` insertion point in Read/Grep's branch structure -- not specified (L1).

### Abductive Hypotheses
1. **A long, real, multi-topic session (this exact conversation is one) never resets the escalation counter between unrelated sub-tasks.** `nudge_counts` persists for the whole `session_id` lifetime; a session that pivots across several genuinely unrelated areas of work (as this conversation did: build-check -> UX audit -> implementation -> this backlog pass) could accumulate 3 native reads across 3 completely unrelated contexts, hours apart, and trip a deny that has nothing to do with "didn't learn from the nudge in this specific situation." The per-session scope is the right proxy for "this conversation," but not for "this coherent task," and the two diverge exactly in the sessions most worth having a good experience in (long, varied ones). HIGH.
2. **Gameable-threshold risk:** an agent (this one or a future one) that notices a 3-strike deny could learn to interleave 1-2 token CALM-tool calls between native reads purely to avoid tripping the counter, without any real change in tool preference -- optimizing for "avoid the deny" rather than "prefer the better tool," which is a worse outcome than today's non-blocking tally. Speculative (T3, about future model behavior, not measured) -- flagged as lower-confidence than the other findings, not blocking on its own.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
**HOLD on Item A** (Read/Grep graduated hard-gate). Two HIGH findings both undercut the core premise: the evidence base is ~2 independent sessions, not 16 (Failure Mode 2), and the counter as designed doesn't measure what it claims to (Failure Mode 1). Required before revisiting: (a) keep collecting shadow-mode `would_deny` data with the F4 UUID filter applied from the start, across more distinct real sessions, not just more raw events from the same few sessions; (b) redesign the counter to distinguish situations (e.g., per-file, or require no intervening CALM-tool call) before it can claim to measure "repeated disregard"; (c) add shadow-mode measurement for the escalation gate itself before it goes live.

**PASS WITH FLAGS on Item B** (DEBT-007 `flock` fix) -- proceeds independently of Item A's HOLD (Failure Mode 3's concern doesn't apply while Item A stays unshipped). Flags for the plan: use the fd-based `flock` idiom (L2), extend the cleanup glob to cover `*.json.lock` (L3). Re-review Failure Mode 3 if Item A is ever revisited later.
