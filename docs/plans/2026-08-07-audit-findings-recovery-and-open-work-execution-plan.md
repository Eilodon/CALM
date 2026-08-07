---
title: "Architecture-audit findings — recovery + open-work execution plan"
date: 2026-08-07
status: "RESEARCH COMPLETE, execution not started. Headline finding (see §0): the literal
  '15 remaining findings' were UNRECOVERABLE *from the repo* — no doc, issue, or deleted
  file — but were RECOVERED out-of-band from the audit session (2026-08-07) and folded in
  as §7. 'Not in version control' is not the same as 'gone' — which is exactly the
  fragility §3 exists to fix. This plan therefore reframes the task from
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

## §0. Headline finding: the "remaining findings" are unrecoverable *from the repo*

> **Update 2026-08-07 (see §7):** the findings were recovered out-of-band from the audit
> session and folded in below. §0's reasoning stands for the *repo* — they were in no doc,
> issue, or deleted file — but they were not gone: they survived in one session's context.
> That is the fragility §3 addresses, demonstrated live.

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

- It does **not** *fabricate* the 15 findings. They were unrecoverable from the repo when
  this plan was first written (§0); they were later supplied from the audit session and
  recorded verbatim in §7 rather than invented.
- It does **not** open GitHub issues or write code — those are execution steps (§5),
  gated on maintainer go-ahead, and the issue-creation ones are outward-facing.
- It does **not** re-run the full audit blind (§4 explains why that is the wrong shape).

## §7. Recovered findings (folded in 2026-08-07 — from the audit session, not the repo)

§0 concluded the 15 non-fixed findings were "unrecoverable." Correction: they were
unrecoverable *from the repo* (no doc, issue, or deleted file — all verified), but the
auditor still held them in the originating session and supplied them on 2026-08-07. "Not
in version control" is not "gone" — and that gap is exactly what §3 exists to close. They
are recorded below as the durable artifact the audit should have produced (executing §3).
Dispositions are the auditor's; two verifiable items were independently confirmed and
fixed this session (marked ▶).

### Cat 1 — Deliberate trade-offs (self-disclosed; fixing breaks a design decision)
| ID | Finding | Recorded in |
|---|---|---|
| 6.2 | `dependencies` follows a glob re-export exactly 1 hop (A→B→C not traversed); commented as intentional | — |
| 9.2 | HTTP has a resource floor (16 MiB body, 64 concurrent, bearer), not a real DoS policy; docs say front with a reverse proxy | §1 L5 |
| 10.2 | path-component-swap TOCTOU (canonicalize↔write race) is outside the current threat model; noted in code | — |
| 10.3 | `verify_change` = `cargo check` only, single-lang, opt-in, unsandboxed; multi-lang needs an exec-policy abstraction first | §1 L4 |
| 10.4 | No true multi-file change-set transaction; `batch_status` is observability only | §1 L2 |
| 10.5 | `reason` default is lexical substring (gameable); `cites` is the exact-match opt-out; full deprecation is breaking | §1 L3 |
| 12.1 | Indexing-DoS mitigation partial: 8 MiB/file + 5 s parse timeout; no AST-node budget or `.calm/` disk quota | §1 L6 |

### Cat 2 — Needs a large redesign, not a fix (deliberately untouched)
| ID | Finding |
|---|---|
| 4.1 | `formal_source` authority (Stack Graphs vs SCIP vs LSP) treated as equal by graph algos (coreness/hub); audit self-rates this OVERSTATED (formal_source already surfaced with its own etag). Real fix = split scope_resolved / target_exact / proof_generation into a new model |
| 4.2 | Review token (`edit_context`) binds only the caller-set digest, not the full authority snapshot (graph generation, watcher freshness, provider generation, policy version) — an undefined contract, not a patchable bug. Adjacent to §2.2's write-gate focus, different axis |

### Cat 3 — Meta-tooling / process investment (not a single bug)
| ID | Finding |
|---|---|
| 11.1a ▶ | Stale comments point to a deleted KNOWN_LIMITATIONS section ("share one SQLite file"). CONFIRMED — 2 sites (`conn.rs:40`, `lib.rs:727`), fixed with this addendum |
| 11.1b | Make KNOWN_LIMITATIONS machine-readable (TOML + `evidence_assertion` CI-checkable) — new system, ≈ §3 |
| 11.2 | guarantee-catalog (`docs/guarantee-levels.toml`) has format/presence checks but no per-ID contract test (e.g. `guarantee::txn.begin_before_write`) for semantic drift — new test framework |

### Cat 4 — Needs telemetry that doesn't exist yet
| ID | Finding |
|---|---|
| 12.4 | 37-tool surface risks wrong-tool selection; optimizing needs real per-tool usage telemetry — matches the workflow-facade blocker (prior research) |
| 12.2 | SQLite connection init not consolidated (busy_timeout / WAL / foreign_keys spread across factory + schema bootstrap); unverified whether real or theoretical |
| 12.3 | Derived metrics (coreness / hotspot risk / dead-code confidence) carry no provenance (graph generation, edge policy version, sample size) |

### Cat 5 — Never verified in the audit (was unknown true/false)
| ID | Finding |
|---|---|
| 7.1 ▶ | Docs say `min_churn=0` surfaces stable complexity debt, but the impl only iterated `churn_map` so zero-churn files never qualified. CONFIRMED A REAL BUG — fixed (impl-fix) with this addendum |
| 8.1 | quarantine doc contradiction (tool schema vs guarantee catalog on `include_quarantined=false`); semantics fine, wording needs syncing |
| 7.2 / 7.3 | calibration history short (threshold 0.80 vs observed max ~0.15) + B14 lens framing — "measurement maturity", not a bug |

### Cat 6 — Technical follow-ups flagged outside that batch
| Finding |
|---|
| `indexer/pipeline.rs` is now the largest remaining hotspot (CI `hotspot_risk`) after the common.rs split — own task |
| `test-calm-nudge.sh` asserts a stale `lib.rs` length (34 vs actual 132 lines); stale fixture, no logic impact |

### Verification results (this session)
- **7.1 — CONFIRMED, fixed (impl).** `compute_hotspots` (`hotspot.rs`) seeded candidates from `churn_map` when git is available; a zero-churn file is absent from that map, and the `churn × complexity` score would zero it out even if added. Fix: when `min_churn == 0`, seed candidates from the complexity index and rank by complexity alone (mirroring `compute_absolute_hotspot_risk`, which was already correct). Regression test added; `min_churn ≥ 1` behavior unchanged.
- **11.1a — CONFIRMED, 2 sites (report said 1), fixed.** `conn.rs:40` additionally carried a stale *factual* premise ("share one SQLite file is safe") now that state.db is a separate file. Both comments rewritten to reference the split's rationale in-place. Note: `fitness_report`'s `config_drift` gate reads 0 here because it only catches references to nonexistent *files*, not deleted *sections* within an existing file — this class slips the current gate, which supports 11.1b.
