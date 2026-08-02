---
title: "WS-1 enforce-transition + critical-risk-block execution plan — closing gate criteria 3 & 4 of Write-Safety Beta"
date: 2026-08-02
status: IMPLEMENTED 2026-08-02 (same session, after this doc's own review pass — see "Implementation
  status" note right after the opening blockquote below for what shipped vs what's still open)
scope: closes gate criteria 3 ("0 write path bỏ qua EditTransaction") and 4 ("critical-risk edit
  không có approver bị block") of the "Write-Safety Beta" milestone
  (docs/plans/2026-08-02-phase1-p0-execution-plan.md §6) — criteria 1/2/5 already closed
  (docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md Part 2), criterion 6 (p95)
  is a separate measurement task, not gated on this doc
inputs:
  - docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md  # §2.4/§2.5 initial sketch, superseded/refined below
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md                 # §4.6 task 4.8 def'n, §6 gate
  - docs/plans/2026-08-02-ws2-review-token-execution-plan.md          # same governing discipline, precedent for this doc's shape
verified_against: HEAD after this session's txn.rs/tools.rs/recover.rs/toolset.rs/config.rs edits;
  edit.rs itself untouched this session, so all file:line citations below are still current
---

> **[Đã IMPLEMENTED — xem trạng thái hợp nhất hiện hành]** [2026-08-02-phase2-priority-and-ws2-execution-plan.md](2026-08-02-phase2-priority-and-ws2-execution-plan.md) xác nhận cả 2 change (A/B) trong doc này sống trong code hiện tại. Nội dung dưới đây giữ nguyên làm thiết kế/threat-model gốc.

# WS-1 Enforce-Transition + Critical-Risk-Block Execution Plan

> This document is the **required verification step** before either change below touches
> `edit_lines_impl_gated`. No code in this pass. Both changes are scoped to be small once
> reviewed — this is the review, not a green light to skip it.

**Implementation status (2026-08-02, same session, after the review below ran its course):**

- **Change A: IMPLEMENTED.** `high_risk_needs_independent_review` shipped in `edit_lines_impl_gated`
  exactly as designed in §1.2, informed by §1.5's open question (resolved: `bridge_downgrade_eligible`
  is structurally `false` whenever `risk=="high"`, confirmed by reading `edit.rs:942-943` directly —
  no precedence conflict). New error code `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`. Tests:
  `high_risk_edit_off_elicitation_is_blocked_even_with_confirm_and_grounded_reason`,
  `high_risk_edit_can_pass_via_elicitation_ask_then_approved`.
- **Change B: IMPLEMENTED, narrower scope confirmed.** `txn::begin` failure aborts the write in both
  `edit_lines_impl_gated` (`TRANSACTION_INIT_FAILED`) and `format_files_impl` (per-file `error`
  result, batch continues for other files) exactly per §2.2. Dual-read (§2.3 stage 2) shipped
  alongside: `EditLinesOutput.tx_id` now surfaces the shadow transaction id. `needs_repair` hint
  (§2.2's UX nicety for post-begin `advance` failures) deliberately **not** shipped — not gate-
  critical, left for a future pass. Tests: `edit_lines_aborts_when_txn_begin_fails`,
  `format_files_skips_one_file_when_txn_begin_fails_without_aborting_the_batch` (both force a real
  `txn::begin` failure via a read-only DB file, not a mock).
- **Milestone gate impact:** closes criteria 3 and 4 of "Write-Safety Beta"
  (`phase1-p0-execution-plan.md` §6, updated same session). Criterion 6 (p95) was measured
  separately in that same update — **fails** at the currently-measured ~30-50%+ overhead, not the
  targeted ≤10%; a follow-up optimization pass (batching `open_writer` round-trips) is noted there,
  out of scope for this doc.
- Full verification: 945 `calm-core` + 301 `calm-server` tests pass, `clippy -D warnings` clean,
  `cargo fmt --check` clean, `diff_impact` reports `signature_changed:false` on every touched
  function.

---

## 0. Correction to the earlier sketch (§2.5 of toolsurface-writesafety-ledger-research.md)

That doc's §2.5 proposed "when `risk == \"high\"` and elicitation is configured, require it
unconditionally" as new behavior. Re-reading `edit_lines_impl_gated` fresh for this doc (not
re-quoting the earlier sketch) shows **this already happens today**:

- `edit_lines_tool`/`edit_symbol_tool` (`edit.rs:28-43`, `:192-207`): `gate = ElicitGate::Ask` iff
  `self.elicit_setup(&ctx.peer)` returns a timeout (client declared MCP elicitation capability +
  `[edit] elicit_hub_confirm` is on) — `ElicitGate::Off` otherwise (the default).
- `edit_lines_impl_gated:1198`: `if matches!(gate, ElicitGate::Ask) { *ask_out = Some(HubAskContext
  {...}` — placed **after** every machine check (hub/high-risk classification, `cites_real_signal`)
  already passed, per the comment at `:1191-1197` ("every machine check above passed, so this
  write WOULD proceed... Placement inside this hub/high-risk block is what makes non-hub edits
  never elicit"). This fires for **every** touch where `classify_gate`'s `will_block_without_confirm`
  is true — `hub_hit || risk == Some(\"high\") || uncertain_zero_caller.is_some() || force_gate_always`
  (`edit.rs:2078`) — not only the `LowConfidence` zero-caller case WS-2 Phase 1 touched.

**So when elicitation IS configured, a `risk==\"high\"` edit already cannot bypass human review** —
that part of gate criterion 4 is **already satisfied**, no code change needed.

**The actual remaining gap** is narrower than the sketch: when `gate == ElicitGate::Off` (no
elicitation configured — today's default for most installs) **and** `risk == Some(\"high\")`,
`cites_real_signal` alone (confirm:true + reason citing a real caller) is sufficient — the edit
succeeds with **no independent check at all**. This is the one branch gate criterion 4 is actually
about.

---

## 1. Change A — block `risk == \"high\"` when no elicitation path exists

### 1.1 Threat model (precise, not aspirational — same discipline as ws2-plan §2)

- **In scope:** an install that has never configured `[edit] elicit_hub_confirm` (the default) has
  **zero** independent-approval mechanism for a >10-caller (`risk_level_from_caller_count`,
  `detail.rs:525-533`) symbol edit — a well-formed `reason` citing one real caller is entirely
  sufficient today, self-attested by the same agent making the edit.
- **Explicitly NOT in scope:** inventing a "critical" tier. Confirmed (search across `edit.rs`) no
  runtime code branches on any value other than `\"low\"`/`\"medium\"`/`\"high\"` — a 4th tier would
  be a label with no signal behind it. This plan reuses `risk==\"high\"` as the practical proxy for
  what the master plan called \"critical\", exactly as `toolsurface-writesafety-ledger-research.md`
  §2.5 already concluded — that part of the sketch stands, only the trigger condition needed
  correcting (§0 above).
- **Out of scope (T4, same boundary ws2-plan already drew):** a compromised `calm serve` process
  itself. This change defends against an agent operating a normal, uncompromised process with no
  elicitation configured — not process-level compromise (WS-4's territory).

### 1.2 Design

At `edit.rs:1116-1121`, add a second condition alongside the existing
`uncertain_empty_caller_needs_review` branch — call it `high_risk_needs_review`:

```rust
// Existing (WS-2 Phase 1, unchanged):
let uncertain_empty_caller_needs_review = known_caller_qns.is_empty()
    && matches!(uncertain_zero_caller, Some(UncertainZeroCallerReason::LowConfidence));

// New:
let high_risk_needs_independent_review =
    risk.as_deref() == Some(\"high\") && !matches!(gate, ElicitGate::Ask | ElicitGate::Approved);
```

Then extend the existing `cites_real_signal` computation so **either** uncertain condition forces
the same fail-closed shape (`false` unless the elicitation round-trip actually ran):

```rust
let cites_real_signal = if known_caller_qns.is_empty() {
    if uncertain_empty_caller_needs_review || high_risk_needs_independent_review {
        !reason.is_empty() && matches!(gate, ElicitGate::Ask | ElicitGate::Approved)
    } else {
        !reason.is_empty()
    }
} else if high_risk_needs_independent_review {
    false // even a real cited caller doesn't substitute for independent review at this risk tier
} else {
    known_caller_qns.iter().any(|qn| { /* unchanged */ })
};
```

**Why gate on `risk`, not `hub_hit`:** `hub_hit` and `risk==\"high\"` are separate signals in
`classify_gate` (`edit.rs:2078`) — a touch can be `hub_hit` at `risk==\"medium\"` (bridge-only hub,
already has its own `ConfirmOnly` downgrade path, `bridge_downgrade_eligible`) or `risk==\"high\"`
without being a hub at all (>10 callers, non-central function). This change targets exactly the
signal gate criterion 4 names (\"critical\"/high risk), not hub-ness — leaves the existing
bridge-downgrade behavior (`edit_lines_bridge_only_hub_needs_only_confirm_when_callers_are_confident`,
already passing) untouched.

**Fail-closed shape when `ElicitGate::Off`:** no reason string, however well-formed, passes. This is
an intentional, honest tightening — a written reason was never real evidence at this risk tier
without a second check, matching the exact framing WS-2 Phase 1 already used for `LowConfidence`.

### 1.3 What does NOT change

- `risk == \"medium\"`/`\"low\"` touches — untouched, existing `cites_token`/`!reason.is_empty()`
  paths apply as today.
- Bridge-only hub downgrade (`bridge_downgrade_eligible` → `ConfirmOnly`) — untouched; a
  bridge-only hub at `risk==\"high\"` (if that combination is reachable — needs checking, see §1.5)
  would need explicit reasoning about whether the downgrade should still apply given the new
  independent-review requirement.
- `ElicitGate::Ask`/`Approved` behavior — unchanged; this only removes the `Off`-mode escape hatch
  that `risk==\"high\"` currently has.
- WS-2 Phase 1's `UncertainZeroCallerReason` handling — untouched, this is an orthogonal condition
  ORed into the same `if` branch, not a replacement.

### 1.4 Error code and message

New code `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` (distinct from `REASON_NOT_GROUNDED` and
`UNCERTAIN_ZERO_CALLER` — the failure shape differs again: \"your reason and confirm are
structurally insufficient at this risk tier, not just insufficiently grounded\"). Message names the
missing config option (`[edit] elicit_hub_confirm`) explicitly, same pattern as
`UNCERTAIN_ZERO_CALLER`'s message.

### 1.5 Open question to resolve before coding (not resolved by this doc)

Is `hub_hit == false && risk == Some(\"high\") && bridge_downgrade_eligible == true` a reachable
combination? `bridge_downgrade_eligible` is read in `classify_gate` only inside the `hub_hit`
branch's `why` computation (`edit.rs:2085` area) — needs a fresh read of
`compute_touch_risk`/`bridge_downgrade_eligible`'s actual computation (not re-derived here) to
confirm bridge downgrade is hub-only and therefore structurally cannot combine with this change's
non-hub `risk==\"high\"` path. If it CAN combine, task 1 below must decide precedence explicitly
before writing the `if` condition above.

### 1.6 Task breakdown

| # | Task | Depends on |
|---|---|---|
| 1.1 | Resolve §1.5's open question (read `compute_touch_risk`/`bridge_downgrade_eligible` fresh) | none |
| 1.2 | Implement `high_risk_needs_independent_review` per §1.2, informed by 1.1's answer | 1.1 |
| 1.3 | New error code `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` + message | 1.2 |
| 1.4 | Tests (§1.7) | 1.2, 1.3 |

### 1.7 Tests

- `high_risk_edit_off_elicitation_is_blocked_even_with_confirm_and_grounded_reason` — the core
  regression: `risk==\"high\"` fixture, `confirm:true`, `reason` citing a real caller,
  `ElicitGate::Off` → `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`, file unchanged.
- `high_risk_edit_ask_gate_still_requires_elicitation_round_trip` — same fixture through
  `edit_lines_flow` with `ElicitGate::Ask` → `ask.is_some()`, file unchanged until `Approved`.
- `high_risk_edit_approved_after_ask_succeeds` — round-trip proof, mirrors WS-2 Phase 1's
  `empty_caller_set_low_confidence_zero_caller_can_pass_via_elicitation_ask_then_approved`.
- Regression pin: every existing `risk==\"medium\"`/hub-at-medium test (`edit_lines_bridge_only_hub_*`)
  must still pass unmodified — proves this change is scoped to `risk==\"high\"` only.
- Full workspace suite (945 core + 297 server currently) must stay green; `diff_impact` on
  `edit_lines_impl_gated` afterward, expect `signature_changed:false`.

---

## 2. Change B — WS-1 enforce transition (`txn::begin` failure aborts the write)

### 2.1 Current state (verified this session, §2.4 of toolsurface-writesafety-ledger-research.md,
restated here for a self-contained doc)

`edit.rs:1229-1244` (`edit_lines_impl_gated`) and the equivalent block in `format_files_impl`
(`edit.rs:586-601`): `txn::begin` is called, and on `Err` the failure is only
`tracing::warn!(\"shadow txn::begin failed (non-blocking): {e}\")` — the write proceeds regardless.
Every subsequent `txn::advance` call in both functions follows the same
`let _ = ...`/`match ... Err => warn!` non-blocking shape. This is shadow mode by design (master
plan §3 WS-1, phase1-plan §4.4) — this change is the deliberate exit from it, narrowly scoped.

### 2.2 Design — narrower than the master plan's original vision

Master plan WS-1 imagined the whole write path eventually driven by the transaction state machine
(block/rollback per state). Per the same \"don't start broad, start narrow and verified\" discipline
`ws2-review-token-execution-plan.md` §3 already used successfully, this change enforces **only**
the `begin` step:

- If `txn::begin` returns `Err`, **abort the write attempt** — return a typed error
  (`TRANSACTION_INIT_FAILED`) before `atomic_write` runs, instead of proceeding as today.
- **Do NOT** attempt to roll back a write whose `atomic_write` succeeded but whose subsequent
  `advance(FileCommitted)` call failed — at that point disk has already changed; "rolling back"
  means deleting/reverting a file, a materially more dangerous operation than the bookkeeping
  failure it would be reacting to. Keep today's behavior there (`warn!`, non-blocking) but add a
  `needs_repair: true` field (or equivalent) to the tool response pointing at `repair_consistency`,
  so the failure is surfaced instead of silently swallowed.
- Rationale for choosing exactly the `begin` boundary: a `txn::begin` failure (constraint violation,
  SQLite busy beyond retry, disk full at the DB layer) is evidence of an infrastructure problem that
  `atomic_write`'s own disk operations are also likely to hit moments later — failing closed here has
  near-zero false-positive cost, unlike failing closed after disk has already changed.

### 2.3 Rollout (3 stages named in phase1-plan §4.6 task 4.8, this is stage 3)

1. **Shadow (done).**
2. **Dual-read** — verify `tx_id` is actually surfaced in the `edit_lines`/`edit_symbol`/
   `format_files` JSON response today (needs a fresh check — not confirmed in this doc; if absent,
   add it first, a small additive field, before touching failure behavior).
3. **Enforce (this change)** — `begin` failure only, per §2.2.

### 2.4 What does NOT change

- `atomic_write` itself, `WriteAssurance` modes — untouched (WS-3, already shipped).
- Every `advance()` call site after `begin` succeeds — untouched, still non-blocking per §2.2's
  disk-already-changed reasoning.
- `format_files_impl`'s parallel shadow wiring — same treatment, same reasoning, same narrow scope.

### 2.5 Task breakdown

| # | Task | Depends on |
|---|---|---|
| 2.1 | Confirm `tx_id` dual-read status (stage 2) — add if missing | none |
| 2.2 | `TRANSACTION_INIT_FAILED` error path in `edit_lines_impl_gated` on `txn::begin` `Err` | 2.1 |
| 2.3 | Same in `format_files_impl` | 2.1 |
| 2.4 | `needs_repair`/`repair_consistency` hint surfaced on post-`begin` `advance` failure (not a hard block, just visible) | 2.2, 2.3 |
| 2.5 | Tests (§2.6) | 2.2–2.4 |

### 2.6 Tests

- `edit_lines_aborts_when_txn_begin_fails` — needs a way to force `txn::begin` to fail
  deterministically (e.g. a read-only/locked DB fixture, or a poisoned connection) — **fault
  injection design itself needs a decision before coding**, not sketched further here.
- `edit_lines_still_applies_when_txn_advance_fails_after_file_committed` — pins today's
  non-blocking behavior for the post-`begin` case, now additionally asserting the
  `needs_repair`/hint field is present.
- Full crash-injection suite (`txn_crash_injection.rs`) re-run unmodified — must still pass, since
  it exercises real `atomic_write` + `advance` sequences, not `begin` failure.

---

## 3. Milestone gate closure — what remains after both changes land

Referencing `phase1-p0-execution-plan.md` §6 (already updated this session for criteria 1/2/5):

- Criterion 3 (\"0 write path bypasses EditTransaction\") → closed by Change B.
- Criterion 4 (\"critical-risk blocked without approver\") → closed by Change A.
- Criterion 6 (p95 benchmark) → separate measurement task
  (`toolsurface-writesafety-ledger-research.md` §2.2), not blocked on either change here, can run
  in parallel.

All 6 closed only after Change A, Change B, AND the p95 measurement are each independently done —
this doc does not implement any of them, per its own governing instruction.

---

## 4. Explicit not-in-scope for this whole document

- WS-2 Phase 2 (durable `caller_set_digest`, TOCTOU closure) — separate, own plan, not started.
- WS-2 Phase 3 (multi-tier independent approver beyond the binary elicitation-configured/not
  distinction Change A uses) — still no concrete scenario, still deferred per ws2-plan §5.
- WS-4 (provider sandbox) — unrelated, Tier C, own research pass needed first.
- Any signing/HMAC infrastructure — deferred per priority-reaudit.md §6.4.
- Actually writing any of the code in §1/§2 above — this document is the pre-code verification
  step; implementation is a follow-up action once §1.5's open question is resolved.
