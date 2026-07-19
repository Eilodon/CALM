---
SPEC_APPROVED: true
---

# Elicitation for Hub-Edit Confirm (human-in-the-loop veto)

## Problem

CALM's hub/high-risk edit gate (`CONFIRM_REQUIRED` → `confirm:true` +
`EDIT_CONTEXT_REQUIRED` + `REASON_NOT_GROUNDED` in
`crates/calm-server/src/tools/edit.rs::edit_lines_impl`) is **agent
self-confirmation**: the same model that wants the edit also supplies the
confirmation. The safety-by-construction litmus test
(strip agent memory/model — does the guarantee hold?) says this is the gate's
weakest link: a sufficiently confused agent will happily write `confirm:true`
with a technically-grounded reason.

MCP elicitation (spec 2025-06-18, supported by rmcp 2.2.0 behind the
`elicitation` feature flag) lets the server ask the **human at the client UI**
a typed question mid-tool-call. No other code-intelligence MCP server ships
this; it upgrades CALM's core differentiator.

## Proposal (Option A — veto, never bypass)

Elicitation is an **additional human veto on top of the existing gate, never
a replacement for any part of it**:

- The agent must still pass the full machine gate exactly as today
  (`confirm:true`, fresh `edit_context` this session for degree/both-hubs,
  grounded `reason`). Elicitation can only turn an edit that WOULD have been
  written into a refusal — never the reverse. An agent that omits `confirm`
  still gets `CONFIRM_REQUIRED`; the human is never asked to compensate for
  an agent that skipped its own review.
- Only after the machine gate passes, and only when ALL of the following
  hold, the server elicits:
  1. config opt-in: `[edit] elicit_hub_confirm = true` (default **false** —
     zero behavior change for every existing install, same rollout precedent
     as the LSP overlays),
  2. the connected client declared the `elicitation` capability with form
     mode (`peer.supported_elicitation_modes()` contains `Form`),
  3. the edit touches a hub / high-risk range (the same `compute_touch_risk`
     verdict the gate itself uses).
- Question shape (typed, `elicit_safe!`-marked flat struct):
  `HubEditApproval { approve: bool }`, message summarizing: tool name, file,
  symbol/range, hub kind, caller count, and the agent's `reason` verbatim.

## Decision semantics (fail-closed everywhere)

| Elicitation outcome | Result |
|---|---|
| Accept + `approve: true` | edit proceeds (machine gate already passed) |
| Accept + `approve: false`, Decline, or Cancel | refuse, new error code `USER_DECLINED` (non-retryable without change: message tells the agent to present the human's veto back to the user, not to retry) |
| Timeout (config `elicit_timeout_secs`, default 120) | refuse, `ELICITATION_TIMEOUT` — the human was asked; proceeding on silence would make the veto decorative |
| Transport/`ElicitationError` (client declared capability but misbehaves) | refuse, `ELICITATION_FAILED` — fail closed, log at `AUDIT_TARGET` |

Every elicitation decision (asked/approved/declined/timeout/failed) is logged
to the existing audit trail (`telemetry.rs` `AUDIT_TARGET`) alongside the
current `CONFIRM_REQUIRED` decision points.

## Design

### Async surface (the one structural change)

`edit_lines` and `edit_symbol` `#[tool]` wrappers in
`crates/calm-server/src/tools/edit.rs` become `async fn` and gain a
`Peer<RoleServer>` parameter (rmcp's `#[tool]` supports both; the other 27
tools stay sync/unchanged). The sync `edit_lines_impl` stays sync. Flow in
the wrapper:

1. Fast pre-checks (config off, or capability absent, or `confirm != true`)
   → call impl exactly as today, zero new code on that path.
2. Otherwise run a **sync pre-check** `hub_elicit_precheck(path, ranges)`
   reusing `compute_touch_risk` to decide "would this write touch a
   hub/high-risk range". For `edit_symbol`, the symbol→range resolution
   inside `edit_symbol`'s existing body runs BEFORE the impl call today
   (position-mode hunk construction); the precheck slots in after that
   resolution — no duplicated resolution logic.
3. If hub-touching → `peer.elicit_with_timeout::<HubEditApproval>(msg,
   Some(timeout))` → apply the decision table above.
4. On approval → call the sync impl (which still runs its own full machine
   gate — the precheck is advisory for "should we ask", never a substitute
   for the enforcement inside the impl; a precheck/impl disagreement can
   only produce an unnecessary question or an unasked-but-refused edit,
   never an unguarded write).

### Config

`[edit]` table in config.json (loaded via existing `load_config_or_warn`):
`elicit_hub_confirm: bool` (default false), `elicit_timeout_secs: u64`
(default 120). Surfaced in `repo_overview.health_summary.config_override`
like every other non-default (existing `diff_from_default()` mechanism).

### Cargo

`rmcp = { version = "2", features = ["server", "transport-io", "macros",
"elicitation"] }` — one feature added; `elicit::<T>` additionally requires
the `schemars` feature, which `server` already implies (verify at compile
time; if not implied, add it explicitly).

### Daemon/forwarder pass-through (no change expected)

`calm connect` forwarders relay stdin<->socket verbatim in both directions
(ADR-0005), and each daemon connection is its own rmcp service with its own
`Peer` — a server→client elicitation request therefore reaches exactly the
client that issued the edit, and multi-client sessions can't cross-talk by
construction. Risk to verify in tests: a second connected client must be
unaffected while one client's elicitation is pending (no shared-lock held
across the `await` — the elicitation await must NOT hold the cross-process
edit lock or any `RwLock` guard).

### Non-goals

- URL-mode elicitation (`elicit_url`) — form mode only.
- Eliciting for anything besides the hub/high-risk edit gate (no elicit on
  `format_files`, non-hub edits, or read tools).
- Replacing/weakening any existing error code or gate tier (bridge-only
  lighter tier included — it still elicits when config+capability say so,
  since it's still a hub write).
- Changing the default OFF (a follow-up may add an `"auto"` mode after
  real-world client-support data exists).

## Known risks for the audit to weigh

1. **Holding locks across `.await`** — the elicitation await must sit
   BEFORE `edit_lines_impl` acquires the cross-process edit lock; if the
   precheck needs a DB read it must drop the connection before awaiting.
2. **rmcp `#[tool]` async+Peer signature** — assumed supported (rmcp docs
   show async tool methods with `Peer` extraction); if the macro rejects
   mixed sync/async within one `#[tool_router]` impl block, fallback is
   making all four edit-module tools async with the other three never
   awaiting.
3. **Capability lies** — a client declaring form elicitation but never
   answering: covered by timeout → fail-closed refusal, but the agent's
   tool call blocks for the full timeout; message must tell the agent what
   happened so it doesn't retry in a loop.
4. **Precheck divergence from the impl's own gate** (edit_symbol position
   modes construct hunks before the impl call; `old_text` hunks have no
   range until matched) — for `old_text`-mode hunks the precheck uses the
   hunk's `[start_line, end_line]` window as the conservative touch range
   (superset of the eventual match — may over-ask, never under-asks).
5. **Test-harness impact** — existing sync tests call
   `server.edit_lines(...)` directly; the wrapper going async requires
   either a tokio test runtime for those call sites or keeping the sync
   impl callable directly (tests target `edit_lines_impl` via a thin sync
   test shim, preserving all ~40 existing gate tests unchanged).

## Test plan

- Unit: decision-table tests (approve/deny/timeout/error → exact error
  codes), precheck hub/non-hub classification, config default-off short
  circuit, capability-absent short circuit.
- Integration (rmcp test client with elicitation handler, mirroring rmcp's
  own `tests/test_elicitation.rs` pattern): end-to-end approve → write
  lands; decline → no write, `USER_DECLINED`; no-capability client →
  behavior byte-identical to today.
- Toolsnaps: `UPDATE_TOOLSNAPS=1` — expected NO schema change (params
  unchanged); if snaps change, that's a regression to explain.
- Full workspace suite green before commit.

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-20 | trigger: NORMAL -->

**Tier:** 2 (Production — shipped OSS write-path gate; no PII/payments) | **Date:** 2026-07-20

```
CONTEXT_MODE:      DESIGN
STAKEHOLDER:       CALM maintainer + downstream agent users (multi-client daemon)
GOAL:              pre-mortem before implementation
AUDIT_TARGET_TIER: 2
```

### Failure Modes
1. **Lock/guard held across the elicitation `.await` stalls or deadlocks the
   whole daemon** — one human deliberating for up to 120s while a `RwLock`
   guard or the cross-process edit lock is held blocks every other connected
   client's calls (or deadlocks a second edit). The design enables it because
   the edit path was written sync-with-locks and gains its first-ever await
   point mid-flow. — **HIGH** — mitigation in plan: YES (elicit strictly
   before impl/lock acquisition; drop any DB connection before await;
   REQUIRED test: second client's tool call completes while client A's
   elicitation is pending).
2. **Capability-declared-but-answer-path-broken turns every hub edit into a
   120s hang → users disable the feature or the whole gate** — the actual
   request traverses the `calm connect` forwarder and (a stated target!) MCP
   gateways; any hop that forwards initialize capabilities but drops
   server→client requests produces systematic `ELICITATION_TIMEOUT`. —
   **HIGH** — mitigation in plan: PARTIAL (fail-closed is safe but unusable;
   plan MUST add: (a) code-verify + integration-test forwarder pass-through
   through a real daemon+connect pair, (b) the timeout error message must
   name the config key to turn the feature off — no silent auto-downgrade,
   which would let one induced timeout convert veto-on into veto-off).
3. **The `reason` string becomes an outbound injection surface into the
   human's approval UI** — agent-authored text is rendered by client UIs;
   a prompt-injection-shaped or wall-of-text reason rubber-stamps approval;
   CALM's existing response sanitizer does NOT cover this new outbound
   channel. — **MED (HIGH if unsanitized)** — mitigation in plan: YES
   (route the elicit message through the existing sanitization layer,
   hard-cap reason length, plain text only).

### Layer Signals
- **L1 Logic:** precheck-vs-impl asymmetry for `old_text` hunks (window
  superset may over-ask; must never under-ask). REQUIRED test: superset
  property + bridge-only tier still elicits + `format_files` never elicits.
- **L2 Concurrency:** covered by FM1; additionally, file changed by another
  session while elicitation pends → existing `expected_hash` staleness check
  already refuses — verify with a test, no new mechanism.
- **L3 Data:** new config fields must serde-default so every existing
  config.json parses (no `[edit]` table → feature off). Low risk.
- **L4 Integration:** map rmcp `ElicitationError` variants exhaustively —
  malformed accept payload, client disconnect mid-elicitation, and timeout
  must each land on a distinct, tested fail-closed error.
- **L5 Security:** FM3; plus the veto's strength is bounded by client
  behavior we don't control (a client MAY answer elicitations
  programmatically — nothing in MCP forces a human hand). Per this skill's
  gotcha list this is a label-based restriction: document it as a
  limitation, log every decision to AUDIT_TARGET regardless, never market
  it as a hard guarantee.
- **L6 Observability:** decisions logged at AUDIT_TARGET (asked/approved/
  declined/timeout/failed + elapsed ms). No new metric endpoint in v1 —
  acceptable, flagged.
- **L7 Cross-cutting (idempotency/rate):** agent retry loop after
  USER_DECLINED re-asks the human indefinitely — human-harassment DoS.
  REQUIRED mitigation: per-session declined-cache keyed by
  `(path, hunk_content_hash)` — NOT by path alone (per the 2026-07 gotcha:
  identity-reuse ≠ safe-to-dedup; changed content must re-ask), returning
  cached USER_DECLINED without re-eliciting.

### Assumptions to Verify
- **ASSUMED:** rmcp `#[tool]` accepts an `async fn` with a `Peer<RoleServer>`
  param mixed with sync fns in the same `#[tool_router]` impl block →
  compile-spike FIRST, before any refactor.
- **ASSUMED:** rmcp `server` feature implies `schemars` (needed by
  `elicit::<T>`) → compile-verify.
- **ASSUMED:** daemon runs one rmcp service per connection and the forwarder
  relays server→client JSON-RPC verbatim (ADR-0005 says no parsing, but this
  was not re-read this session) → read daemon.rs/forwarder before coding;
  integration test through a real daemon.
- **ASSUMED:** "client declares elicitation ⇒ a human sees the form" —
  unverifiable by construction; documented limitation (see L5).

### Abductive Hypotheses
- **Ab1 (correct components, bad emergent):** headless/CI sessions whose
  client core declares elicitation (capability is client-global, not
  per-session-interactivity) + fail-closed timeout + agent auto-retry =
  every hub edit costs 120s × retries in CI; pipelines that used to pass
  start timing out at the job level with no error pointing at CALM's config.
  Mitigation: timeout error message names the config key; docs call out CI
  explicitly; declined/timeout cache also caps repeat cost within a session.
- **Ab2 (adversarial at scale):** approval fatigue — if hub edits elicit
  often, humans reflexively approve and the veto degrades to decoration
  exactly where it matters; an injected agent can deliberately generate
  benign-looking asks to build reflex before the malicious one. Mitigation:
  scope stays hub-only (rare by design), message must carry caller-count +
  hub-kind so each ask is decision-relevant; docs recommend keeping the
  feature off for bulk-refactor sessions.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
PASS WITH FLAGS — proceed; implementation MUST include: (1) no lock/guard
held across the elicit await + concurrent-second-client test [FM1]; (2)
forwarder pass-through verified in code AND by integration test, timeout
message names the off-switch, no silent auto-downgrade [FM2]; (3) reason
sanitized + length-capped in the elicit message [FM3]; (4) per-session
declined-cache keyed by (path, hunk_content_hash) [L7]; (5) compile-spike
the rmcp async+Peer tool signature before refactoring [Assumption 1].
