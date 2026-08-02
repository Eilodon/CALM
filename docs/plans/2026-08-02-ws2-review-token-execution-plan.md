---
title: WS-2 execution plan — state-bound review token (P0-5 adopt-plan / A02+A03+A20 master plan)
date: 2026-08-02
status: Phase 1 SHIPPED 2026-08-02 (uncommitted, verify). Phase 2/3 still DESIGN-ONLY, not started.
inputs:
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md (WS-2 §170-212, A02/A03/A20 audit rows)
  - docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md (P0-5 §300-310)
  - docs/plans/2026-08-02-priority-reaudit.md (Tier B item 4, C4)
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md (WS-1/WS-3, for schema columns this plan reuses)
audited_state: HEAD acf2793 + this session's uncommitted WS-1/WS-3/WS-13 Tier-A work
governing_instruction: >
  "rủi ro cao và chưa verify chi tiết thì bắt buộc verify thật kỹ rồi mới tiến hành" — this doc
  is that verification step for WS-2. It touches edit_lines_impl_gated, CALM's primary write
  gate, live on every edit_lines/edit_symbol call. No code in this pass; Phase 1 below is scoped
  to be low-risk-once-reviewed, not yet executed.
---

## 0. What was actually verified before writing this (trust level)

Every claim below with a file:line citation was read fresh via `mcp__calm__source`/`search` this
session, not recalled from the master plan's prose. Specifically read in full:
`edit_lines_impl_gated` (edit.rs:719-1534, all 815 lines), `edit_context`
(guardrails.rs:28-446), `record_edit_context_review`/`edit_context_review`
(common.rs:355-410ish), `compute_touch_risk`/`classify_uncertain_zero_caller`/
`zero_caller_count_is_uncertain` (edit.rs + detail.rs), `graph_generation_state`'s schema +
every caller of it (`rebuild_graph_from_index`, `reindex_all_cancellable_with_phase`,
`incremental_graph_update`, `scip/ingest.rs`, `scip/mod.rs`), the elicitation spec
(`docs/superskills/specs/2026-07-20-calm-elicitation-hub-edit-confirm.md`), and VHEATM's
`schemas/approval-token.schema.json` (native Read, VHEATM is outside this project root).

## 1. Verified findings that reshape scope (like C1-C5 in the priority-reaudit)

| # | Claim in master/adopt plan | Verified reality | Consequence for this plan |
|---|---|---|---|
| F1 | Bind review token to `graph_generation` for state-freshness | `graph_generation_state` (schema.rs:147-151) is REAL and live — bumped by `rebuild_graph_from_index`, `reindex_all_cancellable_with_phase`. **But `incremental_graph_update`** (pipeline.rs:1309, the path every single `edit_lines`/`edit_symbol` write actually takes when `indexing.incremental_graph=true`, the default per `[[calm-upgrade-plan-3-execution]]` memory) **never touches it.** An edit to file B that adds/removes a caller of symbol X in file A goes through `incremental_graph_update`, not a full rebuild — `graph_generation` would show no change even though X's real caller set did | `graph_generation` alone is **too coarse** to be the load-bearing staleness signal for a per-symbol review. Record it as auxiliary/diagnostic metadata only (matches the master plan's own payload schema, which lists it as one field among several — not the only one). The load-bearing check must be a fresh digest of the symbol's actual `call_edges` rows, recomputed at commit time and compared to what was captured at review time |
| F2 | A03 "confirm:true is agent self-attestation" — unaddressed | **Partially already shipped**: elicitation veto (`docs/superskills/specs/2026-07-20-calm-elicitation-hub-edit-confirm.md`, live in `edit_lines_impl_gated` lines 1131-1155 via `ElicitGate::Ask`/`HubAskContext`) asks **the human at the client UI** before a hub/high-risk write proceeds, veto-only, fail-closed on decline/timeout/transport-error. **But**: default **off** (`elicit_hub_confirm=false`), requires client MCP-elicitation capability (not universal), and asks the **same operator** driving the agent — not an independent third party. Closes "the model can't unilaterally confirm its own edit" but not "someone other than the agent's own operator reviewed this" | Don't re-solve A03's agent-self-attestation angle — it has a real, tested mitigation already. WS-2 Phase 1 can **reuse** this exact mechanism (extend its trigger condition) instead of building new UI/protocol machinery. The master plan's "High → independent human/policy-bot" tier is a **different, harder** problem (see §4 Phase 3) this finding does not resolve |
| F3 | New schema needed for token storage | `edit_transactions.review_token_id`, `graph_generation_before`, `graph_generation_after` (schema.rs:250,253-254) **already exist** — added as WS-1 scaffolding this session, confirmed zero readers/writers anywhere in the codebase (`grep review_token_id` = 1 hit, the CREATE TABLE itself) | Phase 2 needs **zero new migration** to start binding a review to a transaction — the column is a ready, unused slot |
| F4 | `reason` grounding is pure lexical substring, no real positional binding | `cites_token` (edit.rs:2330) is already **word-boundary-aware** (checks byte before/after aren't `[A-Za-z0-9_]`), matched against `known_caller_qns` — **real caller identifiers `edit_context` actually returned this session**, not arbitrary free text. This is already meaningfully "positional" in spirit, not naive `str.contains` | The `!known_caller_qns.is_empty()` branch (has real callers to cite) is in reasonable shape already — P0-5's actual gap is narrower than "grounding is fake everywhere": it's specifically the empty-set branch (F5) |
| F5 | "empty-caller ⇒ any non-empty reason passes" — the exact bypass P0-5 names | **Confirmed live**, edit.rs:1076-1078 today: `let cites_real_signal = if known_caller_qns.is_empty() { !reason.is_empty() } else { ... }`. Any touched symbol whose `edit_context` review found zero callers lets `reason: "ok"` sail through `REASON_NOT_GROUNDED` | This is the one concrete, reproducible bug Phase 1 closes — see §3 |
| F6 | Positional acknowledgment should bind the full evidence set | `record_edit_context_review` (common.rs:369) stores **at most 5** caller_qns (`.take(5)`) — already capped before it reaches the gate | A future `caller_set_digest` (Phase 2) must be computed from the **full**, untruncated caller list `edit_context` already queries (available before its own display-cap at guardrails.rs:411-419), not the capped 5 stored for display/citation today |
| F7 | Adopt VHEATM's HMAC-signed opaque token (`approval-token.schema.json`: `signature{hmac-sha256,key_id,value}`) | Read the schema directly — accurate as described. But VHEATM needs this because its token crosses a **real process/trust boundary**: an orchestrator issues it, a separately-sandboxed executor (or, per this session's earlier VHEATM re-scan, an out-of-process `signer_service.py` for private-key isolation) verifies it independently. **CALM's `edit_context` (issues the review) and `edit_lines`/`edit_symbol` (would consume it) run in the SAME `calm serve` process**, over the same MCP connection, with direct access to the same DB | An HMAC signature verified by the same process that minted it adds key-generation/storage/rotation complexity for **zero security benefit against the threat that matters here** (a confused or adversarial agent driving that process's own tool calls) — signing only defends against a forger who lacks that process's access, and there isn't one yet. A DB-backed **opaque token id** (server-assigned, looked up by primary key, single-use via a state column) gives every real guarantee needed now: unforgeable (minting it requires the same DB-write access a legitimate `edit_context` call already has), single-use (commit flips a `consumed_at` column), state-bound (row carries the digests to re-check fresh). Defer signing to if/when a review must be verifiable by a party *other than* `calm serve` itself — which is exactly the master plan's own later P2 "dual authority" phase, not this one |

**Net effect of F1-F7**: none of them make WS-2 unnecessary (F5 is a real, confirmed, live bug).
But together they cut the master plan's `ReviewTokenPayload` (10 fields, HMAC signature, new
`prepare_edit`/`commit_edit` tools) down to something much smaller for a first pass: no signing,
no new tools, reuse of two already-shipped mechanisms (elicitation veto, `graph_generation` as
one input among several) plus one already-scaffolded but unused schema slot.

---

## 2. Threat model — precise, not aspirational (P0-5's own naming goal: don't overclaim)

What this plan actually defends against, and what it doesn't:

- **In scope — T1 (confirmed live, F5):** an agent touches a symbol `edit_context` found zero
  DB-visible callers for, and the dead-code heuristic (`compute_dead_code_confidence`) actively
  disagrees this looks safe to edit without a closer look (`UncertainZeroCallerReason::LowConfidence`
  or a still-uncertain `EntryPoint`/`TestOnly`, i.e. `zero_caller_count_is_uncertain` — see F5). Today,
  writing `reason: "ok"` satisfies the gate. It shouldn't.
- **In scope — T2 (TOCTOU, real but less confirmed as exploited-in-practice):** the caller set for a
  reviewed symbol changes (via an incremental-reindexed edit to an unrelated file) between
  `edit_context` and the later `edit_lines`/`edit_symbol` call, within the 200-tool-call freshness
  window. The review is stale but the call-count clock alone can't see it (F1).
- **Explicitly NOT newly in scope — T3 (agent self-attestation of `confirm:true`):** already has a
  real, tested mitigation (F2, elicitation veto) for opted-in clients. This plan extends its trigger
  condition (Phase 1) rather than re-solving it from scratch.
- **Explicitly OUT of scope for this entire plan — T4 (a compromised/malicious `calm serve` process
  itself):** no in-process token scheme — signed or not — defends against the process that both
  issues and enforces the check being compromised. That threat needs process isolation (WS-4's
  territory) or the cross-process "dual authority" handoff the master plan's own P2 phase names,
  not a token format change.

Calling T1/T2 "solved" beyond this precise scope would repeat exactly the overclaiming P0-5's own
naming correction (`REASON_NOT_GROUNDED` → "not acknowledged", not "not understood") warns against.

---

## 3. Phase 1 — close the confirmed live bypass (T1), reusing only existing mechanisms

**STATUS: SHIPPED 2026-08-02.** Implemented essentially as designed below, with one important
refinement discovered while writing the regression test for the existing
`edit_lines_requires_confirm_for_zero_caller_entry_point` test (which started failing as soon as
the naive `uncertain_zero_caller.is_some()` version below was coded): `compute_dead_code_confidence`
(dead_code.rs:58) returns `"none"` **unconditionally** whenever `is_entry_point || is_test`, so
`UncertainZeroCallerReason::EntryPoint`/`TestOnly` fire on *every* zero-caller entry-point/test
symbol, not just a rare edge case — including this session's own repeated pattern of adding a new
`#[tool(...)]` MCP handler then editing it moments later. Hard-refusing those would have been a
real regression of common, legitimate work. The shipped code narrows the hard-refuse-without-
elicitation path to `Some(UncertainZeroCallerReason::LowConfidence)` specifically — the one variant
with **no structural explanation** for the zero caller count (is_entry_point/is_test are
independently-derived indexer facts; LowConfidence is "the heuristic just doesn't know"). Full
workspace still 943(core)+296(server) passing, clippy `-D warnings` clean, fmt clean,
`diff_impact` confirms `edit_lines_impl_gated` `signature_changed:false`, risk `low`.

### 3.1 Design

Current code (edit.rs:1072-1088, inside the full-gate branch — i.e. already past
`classify_gate`, so this only runs when the touch wasn't eligible for the lighter
`ConfirmOnly` tier):

```rust
let cites_real_signal = if known_caller_qns.is_empty() {
    !reason.is_empty()
} else {
    known_caller_qns.iter().any(|qn| { /* cites_token word-boundary match */ })
};
```

Proposed change — branch on the **already-computed** `uncertain_zero_caller` aggregate
(`Option<UncertainZeroCallerReason>`, already destructured in scope at edit.rs:917/944/971 from
`compute_touch_risk`'s return — no new query, no new struct):

```rust
let cites_real_signal = if known_caller_qns.is_empty() {
    if uncertain_zero_caller.is_some() {
        // The dead-code heuristic itself disagrees zero-callers is safe
        // here (same signal edit_context's own risk escalation and the
        // bridge-downgrade eligibility check already trust). A free-text
        // reason cannot manufacture confidence the system doesn't have --
        // deliberately NOT keyword-matched against the reason string here
        // (an agent could learn "always mention entry_point" exactly the
        // way the current `!reason.is_empty()` bypass is trivially
        // learnable). Fails closed: false unless a human veto elsewhere
        // in this function's flow (see 3.1b) actually approved it.
        false
    } else {
        !reason.is_empty()
    }
} else {
    known_caller_qns.iter().any(|qn| { /* unchanged */ })
};
```

**3.1b — where the "fails closed" path gets an actual escape hatch.** A hard `false` alone would
regress every legitimate confirmed-safe-but-uncertain edit to an unconditional refusal with no
recourse for installs that never configured elicitation (today's default — most of them). Extend
the *trigger condition* for the existing elicitation veto (edit.rs:1138, `matches!(gate,
ElicitGate::Ask)` block) to also fire for this specific case, not only on
`gate_classification.will_block_without_confirm`'s hub/high-risk condition as today. When
elicitation is configured, this asks the human instead of trusting agent prose — a strictly
stronger signal than any reason string. When elicitation is **not** configured
(`elicit_hub_confirm=false`, the default), this case has no override and is a hard refusal — an
intentional, honest tightening: a written reason was never real evidence here, it just felt like a
check.

**CORRECTION (found while implementing, see the STATUS box above):** the narrower condition is
`known_caller_qns.is_empty() && uncertain_zero_caller == Some(UncertainZeroCallerReason::LowConfidence)`,
**not** `uncertain_zero_caller.is_some()` as originally drafted here. `EntryPoint`/`TestOnly` are
*always* set whenever a zero-caller function/method is an entry point or test (`dead_code.rs:58`
returns `"none"` unconditionally for those, not conditionally) — they are common, structurally-
explained cases the system already independently verified, not the uncertain case P0-5 is actually
about. Only `LowConfidence` (no `is_entry_point`/`is_test` explanation, the heuristic simply
disagrees) gets the hard-refuse-without-elicitation treatment.

### 3.2 What does NOT change

- `known_caller_qns` non-empty branch — untouched, already reasonably grounded (F4).
- The `EntryPoint`/`TestOnly` empty-caller case (and the `uncertain_zero_caller == None`
  confirmed-safe case) — untouched, `!reason.is_empty()` still applies. This is the exact case
  this session's own `edit_context` → `confirm:true` + reason pattern used successfully ~10+ times,
  including for brand-new `#[tool(...)]` MCP handler methods (always `is_entry_point`, always 0
  static callers by construction) — pinned by the existing
  `edit_lines_requires_confirm_for_zero_caller_entry_point` test, which now doubles as the
  regression guard for this distinction (it started failing the moment the fix was too broad, which
  is exactly what caught the correction above).
- `FRESHNESS_WINDOW_CALLS=200` call-count freshness — untouched in Phase 1 (T2 is Phase 2's job).
- `ElicitGate::Off` behavior for installs that never touch config — identical to today for every
  case except the newly-refused uncertain-zero-caller one (which was a real gap, not a feature).

### 3.3 Tests — SHIPPED (`crates/calm-server/src/tools.rs`)

- `empty_caller_set_low_confidence_zero_caller_is_refused_with_elicitation_off` — the T1
  regression, using a fixture with an unrecognized `language` (`"cobol"`) so neither
  `is_entry_point`/`is_test` nor `is_private`/`scope_clear` explain the zero-caller count, landing
  exactly on `LowConfidence`: `reason: "trust me, totally safe, definitely fine"`, `confirm: true`
  → `UNCERTAIN_ZERO_CALLER`, file unchanged.
- `empty_caller_set_low_confidence_zero_caller_can_pass_via_elicitation_ask_then_approved` — same
  fixture through `edit_lines_flow` with explicit `ElicitGate`: `Ask` → `ELICITATION_PENDING` +
  `ask.is_some()`, file unchanged; `Approved` (the same params) → `applied: true`, file updated.
  Proves the escape hatch this phase adds actually round-trips, not just that Off refuses.
- Existing `edit_lines_requires_confirm_for_zero_caller_entry_point` — kept as the non-regression
  pin (see the correction note in 3.1b): still passes unmodified, proving `EntryPoint`-classified
  zero-caller symbols keep the pre-Phase-1 "any non-blank reason" bar.
- Full suite: 943 (calm-core) + 296 (calm-server, +2 from this phase) passing, clippy
  `-D warnings` clean on both crates, `cargo fmt --check` clean, `diff_impact` confirms
  `edit_lines_impl_gated` `signature_changed:false` / risk `low`.

### 3.4 Error code — DECIDED: `UNCERTAIN_ZERO_CALLER` (new code)

Shipped as a new code rather than reusing `REASON_NOT_GROUNDED`, per the reasoning already in this
section's draft: the failure shape is fundamentally different ("your reason didn't cite a caller"
vs "no reason can satisfy this, it needs elicitation or further investigation"). Message text
explicitly names the missing config option (`[edit] elicit_hub_confirm`) and suggests
`callers`/`understand` as the investigation path.

---

## 4. Phase 2 — durable, state-bound review record (T2)

**Design (not yet task-broken to PR-size — Phase 1 should land and soak first):**

- New table `prepared_reviews` (or reuse `edit_transactions` + its already-unused
  `review_token_id`/`graph_generation_before` columns — needs a decision: a review can predate a
  transaction existing at all, since `edit_context` runs before any write is proposed, so a
  separate table is probably the better fit; `edit_transactions.review_token_id` would then be a
  foreign key set once a transaction actually consumes one).
- `record_edit_context_review` (F6) extended to also persist a `caller_set_digest` —
  `evidence_digest` (reuse WS-3's `crates/calm-core/src/digest.rs`, already SHA-256,
  already the trust-boundary digest convention this session established) over the **full**
  caller_qns list (sorted, joined) computed at guardrails.rs:382, before its `.take(5)` cap.
- At `edit_lines_impl_gated`/`edit_symbol` time, recompute the same digest fresh from live
  `call_edges` for the touched symbol and compare — mismatch ⇒ stale review, same
  `EDIT_CONTEXT_REQUIRED`-shaped refusal as "never reviewed", not a silent pass.
  `graph_generation` recorded alongside purely as diagnostic metadata (F1) — never the
  pass/fail decision by itself.
- `FRESHNESS_WINDOW_CALLS=200` stays as an additional (not replacement) cheap pre-filter — no
  reason to drop a fast check that's already correct in the common case just because a slower,
  precise one now also exists.

This phase needs its own follow-up verification pass (in particular: cost of recomputing a
caller-set digest on every gated write — likely cheap, `call_edges` is already indexed and this
session's `edit_context` already does the equivalent query every time, but should be measured, not
assumed) before it gets a task breakdown. Not blocking Phase 1.

---

## 5. Phase 3 — approval-principal tiering + independent approver (explicitly NOT scoped here)

The master plan's Low/Medium/High/Critical tiering, and "High → independent human/policy-bot",
needs a concept CALM's current single-operator deployment model has no real definition for yet:
*independent from whom*, exactly? Elicitation (F2) already gets a human, but it's the same person
who told the agent what to do — not independent in the sense a multi-reviewer team workflow means.
Building this without a concrete usage scenario (team mode? CI-triggered agent with a named human
owner distinct from whoever queued the job? a policy webhook?) risks the same "build the fence
before knowing what field it's around" mistake explicitly avoided for WS-4 this session. **Defer,
same reasoning as WS-4's Tier C placement** — needs its own research+plan pass once a real scenario
exists, not a spec-shaped guess now.

---

## 6. Explicit not-in-scope for this whole document

- Any change to `atomic_write`/`WriteAssurance` (WS-3, already done).
- HMAC/Ed25519 signing infrastructure, key management (F7 — deferred until a real cross-process
  need exists).
- `prepare_edit`/`commit_edit` as new top-level MCP tools replacing `edit_lines`/`edit_symbol`'s
  in-line gate — Phase 1/2 both extend the EXISTING tools' gate logic in place, no new tool
  surface. Revisit only if Phase 2's design turns out to need a genuinely separate "prepare" step
  the current single-call flow can't express (not yet demonstrated).
- WS-1 task 4.8 (shadow→enforce) — unrelated Tier B item, tracked separately (see session's
  concurrent research).

## 7. Checklist — Phase 1

- [x] This doc reviewed/confirmed (user: "Nếu đã verify đủ và chính xác rồi thì tiến hành thực thi
      đi fen", 2026-08-02).
- [x] 3.4's error-code question decided — `UNCERTAIN_ZERO_CALLER`.
- [x] Confirmed `cites_real_signal`'s only 2 callers are `edit_lines_flow`/`edit_symbol_flow`
      (both funnel through the single `edit_lines_impl_gated`, unchanged shape from
      `resolve_repo_path`'s 6-caller pattern — verified via `edit_context()` before editing, not
      re-asserted from the earlier read).
- [x] Full workspace test suite green (943 core + 296 server) + clippy `-D warnings` + fmt clean,
      `diff_impact` risk `low`/`signature_changed:false` on the touched symbol.
- [ ] Not yet committed to git (uncommitted working-tree change, per this session's established
      rhythm of not committing without an explicit request).

## 8. Checklist — Phase 2/3 (unchanged, not started)

- [ ] Phase 2 design needs its own re-verification pass before coding (per its own §4 note: cost
      of a live `caller_set_digest` recompute on every gated write should be measured).
- [ ] Phase 3 needs a concrete usage scenario before any design work starts (§5).
