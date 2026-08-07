---
title: "Architecture-audit findings — recovery + open-work execution plan"
date: 2026-08-07
status: "RESEARCH COMPLETE, execution not started. Headline finding (see §0): the literal
  '15 remaining findings' from the 28-finding architecture audit are UNRECOVERABLE as a
  list — they exist nowhere in the repo. This plan therefore reframes the task from
  'finish the remaining findings' to 'recover what is recoverable + stop the knowledge
  loss from recurring', and gives a concrete, prioritized execution sequence for both."
scope: >
  Determine what the '28-finding architecture audit' behind commit da3c14f actually left
  open, produce a concrete/verifiable execution plan for it, and fix the process gap that
  made the tail of that audit disappear. Does NOT itself write code or open issues — it is
  a planning artifact (the durable record the audit itself should have produced).
inputs:
  - da3c14f                              # "close 13 real bugs from the architecture audit + split common.rs hotspot" — the ONLY surviving trace of the audit
  - .github/workflows/ci.yml             # calm-guard-dogfood shadow-mode promotion criteria (lines 34-84), the one explicitly-tracked deferred item
  - KNOWN_LIMITATIONS.md                 # the canonical, durable list of what CALM deliberately hasn't done yet
  - CONTRIBUTING.md                      # roadmap (just reconciled against 0.6.0 in d601cb4) — mirrors KNOWN_LIMITATIONS
  - docs/audit/2026-07-12-vheatm-deep-audit.md   # the EXISTING durable-audit convention that the Aug audit failed to follow
  - mcp__calm__fitness_report / test_gap_hotspots (this session)  # evidence for §2
verified_against: HEAD (af37455, branch claude/memory-docs-spec-review-iw7s4t), this pass.
  Every claim below was checked against live source/git this session, not inferred from
  prose: `git log --all --diff-filter=D` (no deleted audit doc), `git log --all --grep`
  (no separate audit commit), fitness_report (all green), test_gap_hotspots (the coreness-7
  write-gate gaps in §2.2).
---

# Architecture-audit findings — recovery + open-work execution plan

## §0. Headline finding: the "remaining findings" are unrecoverable as a list

Commit `da3c14f` ("close 13 real bugs from the architecture audit") is the **only**
surviving trace of a *"line-by-line verification of a 28-finding external audit against
actual source."* Its message lists the **13 confirmed-and-fixed** bugs. The other
**15 findings** (28 − 13) — the ones judged false-positive, already-resolved, or real-but-
deferred — were **never written down anywhere durable**. Verified this session:

- `git log --all --diff-filter=D -- 'docs/**'` → **no** audit/findings doc was ever
  committed and later deleted.
- `git log --all --grep="audit"/"finding" -i` → **no** separate commit or plan doc holds
  the source audit; `da3c14f` is the sole reference.
- The repo already has the right convention — `docs/audit/2026-07-12-vheatm-deep-audit.md`
  is a durable, per-finding audit record — but the August architecture audit **did not
  follow it**. The audit ran inside one agent session; when that session ended, its 15
  unfixed findings ended with it.

**Consequence for the task as literally posed.** "Finish the remaining findings" cannot be
done faithfully, because we cannot know *which* of the 15 were real bugs vs. false
positives without re-deriving them. Any list I invent here would be fabrication, not
recovery. The honest, optimal move is to split the task:

1. **Recover what is genuinely recoverable now** — the durably-recorded open work
   (§1) plus the concrete, evidence-backed items this session independently verified (§2).
2. **Stop the bleeding** — make audit tails durable so this never recurs (§3), and
   re-derive only the highest-risk slice via a *targeted, recorded* re-audit rather than a
   blind 28-item redo (§4). A blind redo is also *inefficient*: the audit's own hit rate
   was 13/28 ≈ 46%, so ~54% of a redo is re-confirming non-bugs.

## §1. What IS durably recorded: `KNOWN_LIMITATIONS.md` is the canonical open list

The real, deliberately-deferred work is already catalogued and current (7 entries), and
`CONTRIBUTING.md`'s roadmap was reconciled against 0.6.0 this session (commit `d601cb4`),
so the two now agree. Ranked by (security/correctness risk if left) × (tractability),
with the two already-shipped roadmap items removed:

| # | Item (KNOWN_LIMITATIONS.md) | Risk if left | Effort | Blocks anything? |
|---|---|---|---|---|
| L1 | Change-kind risk classification covers **signatures only** — a body-only auth-check removal on a low-fan-in symbol still reads as low risk | **High (security)** | Medium | No; natural continuation of the 0.6.0 signature slice |
| L2 | No multi-file change-set / atomic transaction (`batch_status` is observability only) | Medium (correctness) | High | No |
| L3 | `reason` grounding default still lexical; `cites` opt-in only | Medium (gameable gate) | Low code / High compat (breaking) | No |
| L4 | Verification single-language + unsandboxed (`cargo check` only) | Medium | High (needs exec-policy abstraction first) | Blocks go/ts/py verification |
| L5 | Remote HTTP: size/concurrency cap, no real rate limiting | Low (documented; reverse-proxy is the answer) | Medium | No |
| L6 | Pathological-repo indexing DoS: file-size + parse-timeout caps exist; no AST-node budget or `.calm/` disk quota | Low | Medium | No |
| L7 | CLI binary name `calm` collides with `@finos/calm-cli` | Low (product) | Low code / needs a **product decision** + alias period | No |

This table is the answer to "what work is actually open and recorded." It is NOT the lost
15 findings — it is the properly-kept ledger those findings *should* have been folded into.

## §2. Concrete open items this session verified that are NOT in §1

These are real, evidence-backed, and currently tracked only in a code comment (§2.1) or not
at all (§2.2) — i.e. the same fragility that lost the 15 findings.

### §2.1 `calm-guard-dogfood` shadow-mode promotion (tracked only in a CI comment)

`da3c14f` fixed the dogfood job's merge-base bug but explicitly left it in shadow mode:
*"Documented explicit promotion criteria for taking that job out of shadow mode (not done
yet — no clean track record with the fix in place)."* The criteria live in
`.github/workflows/ci.yml:54-63`; the job is `continue-on-error: true` at `:67`.

- **Open work:** this is an *observation gate*, not a code change today. Watch that the
  criteria hold: **10 consecutive PRs** where the job actually ran with zero infra-category
  failures, and **no open false-positive report >7 days old** against `calm guard`'s risk
  model. When they hold, the execution is a **one-line diff** removing `continue-on-error`.
- **Fragility:** identical to the §0 root cause — the trigger to act lives in a comment on
  a job that is green-by-design (`continue-on-error`), so nothing surfaces when the bar is
  met. Recommend a tracking issue (§3) so the promotion isn't silently missed.

### §2.2 Write-gate core has no direct test on its refusal branches (coreness-7)

`test_gap_hotspots` (this session) ranks these #1/#2 by structural centrality with
`test_files: []`:

- `crates/calm-server/src/tools/edit.rs::edit_lines_impl_gated` (coreness 7)
- `crates/calm-server/src/tools/edit.rs::edit_symbol_flow` (coreness 7)
- `crates/calm-server/src/tools/common.rs::make_state_read_conn` (coreness 6, 8 callers —
  the **new 0.6.0 state.db read path**, so brand-new and untested directly)

Verified the coverage is genuinely indirect: the only integration test touching this area
(`crates/calm-server/tests/watcher_integration.rs:131-198`) *simulates* `edit_lines_impl`'s
write+reindex sequence — it does **not** exercise the gate's **refusal** branches
(`EDIT_CONTEXT_REQUIRED`, `REASON_NOT_GROUNDED`, `confirm`-required on hub/high-risk). Those
are the security-relevant paths of CALM's central safety mechanism, and they are its least
directly-tested code.

- **Open work:** add focused tests that drive each refusal branch through the public
  `edit_lines`/`edit_symbol` entry points. Bounded, no core change, high safety value.
- **Caveat (precision):** `dead_code_confidence: "none"` means these ARE called from
  non-test code; the WS-1 enforce-transition suite (0.5.0) likely exercises the *happy*
  path already. Scope the new tests to the refusal branches specifically — confirm before
  duplicating existing happy-path coverage.

## §3. The meta-fix (highest leverage, cheapest): make audit tails durable

This is the single most valuable item in the plan, because it is what actually addresses
the "memory" concern and prevents the next audit from evaporating the same way.

1. **Adopt a standing convention:** every audit lands a dated `docs/audit/YYYY-MM-DD-*.md`
   with **one row per finding** and an explicit disposition —
   `fixed` (+ commit) / `false-positive` (+ why) / `deferred` (+ tracking issue link).
   The precedent already exists (`docs/audit/2026-07-12-vheatm-deep-audit.md`); this just
   makes following it non-optional. Cheapest possible enforcement: add
   `docs/audit/` to the `gen-status.sh` / doc-drift awareness, or simply a line in
   `CONTRIBUTING.md`'s "Before opening a PR" that an audit PR must include its findings doc.
2. **Deferred findings become GitHub issues**, linked from both the audit doc and (if
   product-level) `KNOWN_LIMITATIONS.md`. The repo currently has **zero** open issues —
   the deferred tail of every past audit is invisible.
3. **Backfill once:** reconstruct as much of the `da3c14f` audit's disposition table as the
   commit message supports (the 13 fixed are recoverable verbatim; mark the 15 others
   `unrecoverable — session-local, not recorded`) so the gap itself is on the record.

## §4. Re-audit recommendation: targeted, recorded — not a blind 28-item redo

Do **not** blindly re-run the full audit (inefficient: ~54% false-positive re-confirmation,
and `fitness_report` is all-green so nothing structural is rotting). Instead, spend one
focused, **recorded** (§3) re-audit on the highest-risk concentration the evidence points
at: the **edit / write-gate subsystem** (`tools/edit.rs`, `tools/guardrails.rs`,
`core/edit.rs`, `txn.rs`). Rationale: it is coreness-7 core logic, it is the
security-critical path, and §2.2 shows it is the least directly-tested. That single slice
either surfaces a real live issue or produces a clean, durable result — both strictly
better than the current "we don't know" state, at a fraction of a full redo's cost. The
`vheatm` skill (available in-session) is the natural driver, and its output must land as a
`docs/audit/` doc per §3.

## §5. Prioritized execution sequence

Ordered by (leverage ÷ effort), each item independently shippable:

1. **§3 meta-fix — durable audit convention + backfill note.** Docs-only, ~1 short PR.
   Highest leverage: makes every future audit's tail survive. **Do first.**
2. **§2.2 write-gate refusal-branch tests.** Bounded, no core change, high safety value,
   closes a concrete coreness-7 gap. **Do second.**
3. **§2.1 dogfood-promotion tracking issue.** Trivial; prevents a second silent loss.
4. **§4 targeted re-audit of the edit/write-gate subsystem**, recorded per §3. Medium
   effort; this is the *actual* faithful recovery of "did the 15 findings include a live
   edit-path bug?" for the one subsystem where it matters most.
5. **§1 L1 (change-kind risk classification).** Highest-risk *recorded* limitation and the
   natural next slice after 0.6.0's signature work. Medium effort, security value.
6. **§1 L2–L7** as capacity allows, in the table's order. L7 (binary rename) needs a
   product decision before any code — surface to the maintainer, don't self-start.

## §6. What this plan deliberately does NOT do

- It does **not** enumerate 15 specific "remaining findings." They are unrecoverable
  (§0); inventing them would be fabrication.
- It does **not** open GitHub issues or write code — those are execution steps (§5),
  gated on maintainer go-ahead, and the issue-creation ones are outward-facing.
- It does **not** re-run the full audit blind (§4 explains why that is the wrong shape).
