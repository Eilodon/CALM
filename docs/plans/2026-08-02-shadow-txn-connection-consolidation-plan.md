---
title: "Shadow-txn connection/transaction consolidation — root-causing and designing the fix for gate criterion 6 (p95)"
date: 2026-08-02
status: "TIER 1 + TIER 2 (Option A+B) IMPLEMENTED + MEASURED, same session -- STILL does NOT close
  gate criterion 6 (overhead dropped from ~41% to ~15% p50 / ~23% p95). Remaining-gap root cause
  now FOUND (fourth pass, \"Remaining-gap root cause\" section): the ~4ms residual txn/ledger cost
  vs a ~28ms reindex_paths-dominated baseline explains essentially all of it -- Tier 3 (ledger
  head_digest) and the maintenance::enqueue commits are both empirically ruled out as meaningful
  levers. No further low-risk code lever identified; closing this fully is a milestone-owner
  decision (accept ~15% floor, or revisit the ≤10% target's ~2.8ms absolute budget), not a code
  change. §5 (research/design) also found a real crash-safety conflict in this doc's own original
  Tier 2 sketch (§3) -- see §5.0."
scope: root-causes the p95 regression measured (and left unfixed) in
  docs/plans/2026-08-02-ws1-enforce-and-critical-risk-execution-plan.md ("~30-50%+ overhead,
  root cause identified as multiple separate open_writer round trips per edit ... out of scope
  for this measurement-only pass"), and designs a tiered fix for gate criterion 6 of the
  "Write-Safety Beta" milestone (docs/plans/2026-08-02-phase1-p0-execution-plan.md §6)
inputs:
  - docs/plans/2026-08-02-ws1-enforce-and-critical-risk-execution-plan.md   # p95 measurement + root-cause pointer this doc follows up on
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md                      # §6 gate criterion 6 def'n
  - docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md       # original ledger wiring design (Part 3)
verified_against: HEAD, this pass — crates/calm-core/src/txn.rs, ledger.rs, maintenance.rs,
  db/conn.rs, db/schema.rs, crates/calm-server/src/tools/edit.rs (edit_lines_impl_gated,
  format_files_impl), crates/calm-cli/src/bin/txn_crash_harness.rs, crates/calm-cli/tests/
  txn_crash_injection.rs all read fresh; Tier 1 (§3) implemented and measured this same pass,
  Tier 2/3 still design-only
---

> **[Đã IMPLEMENTED — quyết định còn treo]** Xem [2026-08-02-phase2-priority-and-ws2-execution-plan.md](2026-08-02-phase2-priority-and-ws2-execution-plan.md) §4 — Tier 1+2 dưới đây đã ship, gate criterion 6 (p95) vẫn ~15%/23% overhead, cần quyết định milestone-owner (chấp nhận floor hay đầu tư thêm), không còn code lever rẻ nào được tìm thấy.

# Shadow-Txn Connection/Transaction Consolidation — Research & Design

**Tier 1 implementation & measurement results (2026-08-02, same session):**

- **Implemented exactly as designed in §3**: `edit_lines_impl_gated` now opens one shared
  `Connection` for the whole guarded critical section (`txn::begin` through the final
  `IndexCommitted`/`Done` advance), replacing 5-6 separate `open_writer` calls with 1.
  `format_files_impl` got the same treatment per-file (`begin`+`FileCommitted`/`Failed` share one
  connection instead of two) plus a reindex/final-advance merge with a fallback-to-independent-open
  if reindex's own open fails (preserving the original's redundancy for that one edge case). Full
  verification: 945 `calm-core` + 301 `calm-server` tests pass (including
  `shadow_tx_replay_state_matches_cached_state_across_edit_lines_edit_symbol_and_format_files`,
  `edit_lines_aborts_when_txn_begin_fails`,
  `format_files_skips_one_file_when_txn_begin_fails_without_aborting_the_batch`, unchanged), the
  fast `txn_journal_survives_kill_at_every_reachable_transition` crash-injection variant, `clippy -D
  warnings` clean, `cargo fmt --check` clean, `diff_impact` reports `signature_changed:false` for
  both touched functions.
- **Re-measured with the exact same git-worktree methodology** (baseline commit `acf2793`, N=200
  in-process `edit_lines` calls, `std::time::Instant`), this time 3 runs per side instead of 2:
  - Baseline (acf2793, no shadow-mode at all): p50 = 28.1 / 28.4 / 30.3 ms (mean ≈28.9 ms), p95 =
    44.8 / 49.5 / 50.3 ms (mean ≈48.2 ms) — tight, consistent cluster, matching the earlier
    baseline measurement almost exactly.
  - Tier 1 (this pass, connections consolidated): p50 = 38.2 / 38.5 / 45.3 ms (mean ≈40.7 ms), p95
    = 52.4 / 81.2 / 70.2 ms (mean ≈67.9 ms) — p50 overhead vs baseline ≈+41%, p95 overhead ≈+41%.
  - **For comparison, the PRE-Tier-1 measurement** (ws1-enforce-and-critical-risk-execution-plan.md):
    p50 overhead ≈+31-35%, p95 overhead ≈+50-100%.
- **Honest conclusion: Tier 1 did NOT close gate criterion 6, and barely moved p50 at all.** The
  connection-open count went from 5-6 down to 1-2 per edit (verified by reading the changed code),
  yet the latency overhead is statistically indistinguishable from before Tier 1 — if anything
  slightly worse on this run, well within the noise band both sides show. This **refutes** this
  document's own original framing (the frontmatter `scope` line above, written before Tier 1 was
  measured) that connection-open overhead was *the* dominant cost. The data instead points at
  **commit count** (§1's "9 commits contributed by WS-1" accounting) as the more likely dominant
  factor — each `advance()` is still 2 separate SQLite commits (its own explicit transaction +
  ledger's separate autocommit), and Tier 1 deliberately left that unchanged (§3 said so explicitly:
  "Commit count is unchanged in this tier"). **Tier 2 (§3, batching commits via `advance_batch`) is
  now the more promising next step, not merely a fallback if Tier 1 "isn't enough."**
- p95 noise: the `#[cfg(feature = "scip-overlay")]` background thread (`run_all_coalesced`,
  spawned on every reindexed edit, default-on feature) plausibly explains both the fat mean-vs-p50
  gap (~120-130 ms mean vs ~30-45 ms p50 on every run, both trees) and the p95 run-to-run variance
  — 200 of these piling up in a ~25s window contend for CPU with the foreground measurement. Both
  trees carry this same confound equally, so the **p50 comparison is the trustworthy number** here;
  p95 should be read as noisy in both directions until that background-thread interaction is
  understood better (out of scope for this pass).
- Tier 1's connection consolidation is still worth keeping even though it didn't move the number:
  it's a strict simplification (fewer file descriptors churned, no behavior change per §2/§3's own
  safety argument, all tests green) — just not the fix for criterion 6. Not reverted.

**Tier 2 implementation & measurement results (2026-08-02, same session, third pass):**

- **Implemented exactly as designed in §5.1/§5.2**: `txn::advance` now folds the ledger mirror into
  its own transaction via a `SAVEPOINT` (`append_ledger_in_savepoint`, shared through a new private
  `write_transition` helper) instead of a second, separate commit — cuts every `advance()` call from
  2 commits to 1, with zero change to state-machine granularity. `txn::advance_many` (new, public)
  batches the SAME state transition across independent `tx_id`s into one transaction — wired into
  `format_files_impl`'s final advance phase (two `advance_many` calls total regardless of file
  count: one for `IndexCommitted`/`Failed`, one for `Done` on whichever tx_ids the first pass
  succeeded for). `edit_lines_impl_gated` (always a single `tx_id`) benefits only from Option A, not
  Option B, exactly as §5.2 predicted. Explicitly did NOT implement merging different states for the
  same `tx_id` (§5.0/§5.3's disallowed case).
- **New tests, all passing**: `advance_transition_survives_even_when_the_ledger_insert_itself_fails`
  (drops the `audit_ledger` table entirely, forcing a real `ledger::append` failure, then asserts
  `advance()` still commits the transition — the exact invariant the savepoint exists to protect),
  `advance_many_batches_independent_tx_ids_and_all_succeed` (3 independent tx_ids in one batch, all
  committed, all ledgered, chain still verifies), `advance_many_one_invalid_transition_does_not_block_the_others`
  (one tx_id already terminal at `Done`, batched alongside a valid one — the valid one still commits,
  the invalid one fails with `InvalidTransition`, neither affects the other). Full verification: 948
  `calm-core` (945+3) + 301 `calm-server` tests pass, the fast crash-injection variant
  (`txn_journal_survives_kill_at_every_reachable_transition`) still passes unmodified, `clippy -D
  warnings` clean, `cargo fmt --check` clean, `diff_impact` reports `signature_changed:false` for
  both `edit_lines_impl_gated` and `format_files_impl` (the new/changed `txn.rs` functions
  themselves obviously do show `signature_changed`/`symbol_is_new`, expected for brand-new code).
- **Re-measured again with the same git-worktree methodology, 3 fresh runs each side** (both sides
  measured back-to-back in the same session to control for machine-load drift — see note below on
  why that mattered):
  - Baseline (acf2793, re-measured fresh rather than reusing Tier 1's older numbers): p50 = 34.96 /
    30.91 / 28.33 ms (mean ≈31.4 ms), p95 = 58.51 / 57.04 / 45.27 ms (mean ≈53.6 ms) — noticeably
    higher than Tier 1's baseline run (mean ≈28.9/48.2 ms) despite being the *identical* commit and
    code, confirming machine load had drifted between measurements over the course of this session.
  - Tier 1 + Tier 2 combined: p50 = 34.53 / 38.33 / 35.63 ms (mean ≈36.16 ms), p95 = 71.57 / 61.67 /
    64.06 ms (mean ≈65.8 ms).
  - **Overhead vs the freshly-paired baseline: p50 ≈+15%, p95 ≈+23%** — down substantially from
    Tier 1 alone's ≈+41%/+41%, and from the original pre-Tier-1 measurement's ≈+31-35%/+50-100%.
    Still doesn't clear the ≤10% target, but this is real, meaningful progress, consistent with the
    commit-count hypothesis (9→6 commits per `edit_lines` call was the Tier 2 Option A prediction).
- **Methodology note, worth internalizing:** re-using an older baseline number (as a first instinct
  might do, since Tier 1's baseline was already measured) would have compared Tier 1+2's ≈36.16 ms
  against Tier 1's stale ≈28.9 ms baseline, reporting a misleadingly-still-bad ≈+41% overhead
  (arithmetically true but comparing across two different machine-load windows). Re-measuring
  baseline fresh in the *same* window as the thing being compared against it is what surfaced the
  real ≈+15% figure. General lesson for any future re-measurement here: always re-run baseline
  alongside the candidate, never reuse an old number, even for the "unchanged" side.
**Remaining-gap root cause (2026-08-02, fourth pass — candidate (a) confirmed, (b)/(c) ruled out):**

Wrote a second, throwaway probe (`bench_edit_lines_component_breakdown`, same discipline — added,
measured, removed) that replicates `edit_lines_impl_gated`'s exact sequence but times each phase
(`open_writer`, `txn::begin`, `atomic_write`, `advance→FileCommitted`, `reindex_paths`,
`maintenance::enqueue`, `advance→IndexCommitted`+`Done`) separately, over the same N=200 iterations,
on this Tier 1+2 tree.

- First run's raw average looked alarming — `reindex_paths` alone averaged 112.8ms, "94.8% of
  total" — until per-iteration min/max revealed why: **min=20.3ms, max=17.97 SECONDS.** A single
  one-time cost (first-ever index of a brand-new file/DB in this micro-benchmark) completely
  swamps the average; the steady-state cost for iterations 2-200 sits consistently around
  20-25ms (occasional jitter up to ~37ms). This is *exactly* why every earlier full end-to-end p95
  bench's `mean` (~120-130ms) so wildly exceeded its `p50`/`p95` (~30-70ms) — not primarily
  background-thread contention as originally guessed in the Tier 1 write-up above, but this same
  one-time-outlier effect (percentiles of 200 samples are immune to a single huge value; the mean
  isn't).
- **The steady-state breakdown answers candidate (a) directly**: `reindex_paths` (~20-25ms) is
  already the dominant cost of a `edit_lines` call, and it was ALREADY dominant in the pre-WS-1
  baseline (nothing in WS-1 touches `reindex_paths` itself) — so it explains the *baseline's* ~28-31
  ms p50, not the *overhead* WS-1 adds on top. The overhead itself is everything else measured:
  `open_writer` ≈1.2ms + `txn::begin` ≈1.5ms + `advance→FileCommitted` ≈0.5ms +
  `maintenance::enqueue` ≈0.1ms + `advance→IndexCommitted`+`Done` ≈0.8ms ≈ **~4.1ms total per
  edit** (`atomic_write` itself, ≈2ms, is pre-existing and NOT part of WS-1's added cost either).
  4.1ms on a ~28ms baseline is **≈+14.6%** — matching the measured ≈+15% p50 overhead almost
  exactly. **The gap is not a mystery: it's this ~4ms, precisely.**
- **This rules out (c) as a meaningful lever**: `maintenance::enqueue`'s own commit measured
  ≈0.1ms — negligible, confirmed empirically, not just assumed.
- **This also puts a hard ceiling on (b)'s (Tier 3) potential**: `ledger::append`'s `head_digest`
  `SELECT` is a single indexed-PK-ordered-by lookup (`db/schema.rs:309`'s `audit_ledger.seq`
  `PRIMARY KEY AUTOINCREMENT`) inside a call (`advance→FileCommitted`/`IndexCommitted`/`Done`, 3 of
  them) that already measures well under 1ms combined for the WHOLE `advance()` call including the
  ledger append. Even eliminating that `SELECT` entirely could only shave a small fraction of an
  already-sub-millisecond-per-call cost — a few tenths of a ms at most across all 3 calls. Tier 3 is
  very unlikely to close the remaining ≈5 percentage points needed to reach ≤10%.
- **Practical conclusion:** the ≈15% p50 / ≈23% p95 overhead is now precisely attributed, not
  guessed at, and there is no further low-risk code lever left in this investigation's scope that
  would plausibly close it. What's left: either (i) accept ~15% as the realistic floor for this
  durability/audit-trail feature at this baseline cost level, or (ii) revisit the ≤10% target itself
  — it implies a total budget of ~2.8ms (10% of a ~28ms baseline) for the ENTIRE journal+ledger
  write path per edit, which is a very tight absolute number for a durable, hash-chained,
  crash-recoverable audit trail to fit inside, now that it's measured rather than assumed. Neither
  path was decided in this pass — that's a milestone-owner call, not a code change.

---

> This document covers Tier 1 (§3, implemented+measured) and Tier 2 (§5, implemented+measured) in
> full; Tier 3 (§3) remains research/design-only, not yet attempted. It quantifies exactly where the
> ~30-50%+ p95 regression comes from, checks whether consolidating connections would
> be safe (there's real precedent already in the codebase), and lays out a tiered implementation
> plan for review before either tier touches `edit_lines_impl_gated`/`format_files_impl` again.

## 1. Root cause, quantified

`edit_lines_impl_gated` (`crates/calm-server/src/tools/edit.rs:736-1661`) opens a **fresh SQLite
connection via `open_writer`** at every one of these points on the synchronous (foreground, p95-
counted) success path:

| Line | What | New in WS-1? |
|---|---|---|
| 1284 | `txn::begin` | yes |
| 1332 | `txn::advance` → `FileCommitted` | yes |
| 1374 | reindex's own `write_conn` | pre-existing |
| 1444 | `maintenance::enqueue(ScipRefresh)` (foreground half, before thread spawn) | yes |
| 1475 | `txn::advance` → `IndexCommitted`, then (same conn) → `Done` | yes |
| 1516 | `maintenance::enqueue(EmbedRefresh)` (foreground half, before thread spawn) | yes |

`scip-overlay` is a **default** feature (`crates/calm-server/Cargo.toml:25`), so line 1444 fires on
every default build whenever the reindex summary isn't a no-op — this isn't a rare feature-gated
path, it's the common case, including in the benchmark build.

So WS-1 added **4-5 new connection opens** per `edit_lines_impl_gated` call, on top of the one
pre-existing reindex connection. Each `open_writer` (`crates/calm-core/src/db/conn.rs:17-27`) does
`Connection::open` + `busy_timeout` + a `PRAGMA synchronous=NORMAL` exec — not free, and paid 5-6
times per edit instead of once.

**Commit count is worse than connection count suggests.** `txn::advance`
(`crates/calm-core/src/txn.rs:246-307`) wraps its `tx_events` insert + `edit_transactions` update in
an explicit `BEGIN IMMEDIATE`/`COMMIT`, then — *after* that commit succeeds —
`ledger::append` (`crates/calm-core/src/ledger.rs:100-118`) runs as a **separate, second, implicit-
autocommit** statement pair (`head_digest`'s `SELECT` + the `INSERT` itself) on the same connection.
That's deliberate (advance's own comment at txn.rs:292-298: ledger mirroring must never affect the
transition outcome, so it can't be inside the same explicit transaction) but it means **every
`advance()` call is 2 separate SQLite commits, not 1**. Across one edit: `begin` (1) +
`FileCommitted` (2) + `IndexCommitted` (2) + `Done` (2) + 2 maintenance `enqueue` autocommits (2) =
**9 commits** contributed by WS-1 alone, on top of whatever reindex's own `rebuild_graph` pass
already does.

`format_files_impl` (`crates/calm-server/src/tools/edit.rs:498-723`) has the **same shape, worse
multiplier**: `begin` (593) and `advance→FileCommitted` (627/643) each open their own connection
**inside the per-file loop** — 2 connections × N formatted files, before the batched reindex (665)
and the already-shared final advance loop (679, one connection for however many files changed).

**Ruled out:** missing indices. `edit_transactions.tx_id` is `PRIMARY KEY`
(`db/schema.rs:245`), `tx_events` has `UNIQUE(tx_id, sequence)` (`:275`, gives `next_sequence` an
index-backed lookup), `audit_ledger.seq` is `PRIMARY KEY AUTOINCREMENT` (`:309`, `head_digest`'s
`ORDER BY seq DESC LIMIT 1` uses it). Every query `advance`/`ledger::append` runs is a single
indexed-row lookup — the cost is connection-open + commit-count overhead, not query planning.

## 2. Is consolidating connections actually safe? — yes, and there's already a precedent

The milestone's own crash-injection suite settles this more directly than I expected.

`crates/calm-cli/src/bin/txn_crash_harness.rs:65-106` — the binary
`crates/calm-cli/tests/txn_crash_injection.rs` drives to satisfy gate criterion 1 ("crash-injection
suite ... kill -9 tại mọi giá trị TxState") — opens **exactly one** connection
(`open_writer` at line 65) and reuses it across `txn::begin` → `atomic_write` →
`advance(FileCommitted)` → `advance(IndexCommitted)` → `advance(Done)`, with `SIGKILL` self-raised
between each step. The suite's entire safety argument (disk never changes without a matching
`tx_events` row, cache never drifts from replay, `recover_incomplete` always finds the crashed txn)
already assumes and passes under a **single shared connection** with sequential explicit
transactions on it. `edit_lines_impl_gated`'s multi-connection pattern is the outlier here, not
something the crash suite's guarantees depend on.

**Honest caveat, not glossed over:** `txn_crash_injection.rs`'s own doc comment (lines 16-23) states
it deliberately does **not** drive `edit_lines_impl_gated`/reindex — so that suite doesn't directly
prove consolidating connections *inside* `edit_lines_impl_gated` is crash-safe, only that the
*pattern* (one connection, sequential explicit transactions) is. The actual safety argument for
Tier 1 below rests on three things: (a) equivalence to the harness's already-accepted pattern, (b)
every call in the sequence is strictly sequential — no `BEGIN` is ever issued while a previous one
on the same connection is still open, so there's no nesting hazard, and (c) the existing shadow-tx
tests that DO exercise `edit_lines_impl_gated`/`format_files_impl` directly
(`shadow_tx_replay_state_matches_cached_state_across_edit_lines_edit_symbol_and_format_files`,
`edit_lines_aborts_when_txn_begin_fails`,
`format_files_skips_one_file_when_txn_begin_fails_without_aborting_the_batch`) assert on row-level
outcomes (`replay_state` vs cached state, error codes, disk content) that a connection-lifetime
refactor shouldn't change at all.

## 3. Tiered design

**Tier 1 — connection consolidation (recommended first step, low risk).** One shared `Connection`
for the whole post-gate synchronous section of `edit_lines_impl_gated`, opened once right before
`txn::begin` and threaded through every subsequent `txn::`/`maintenance::enqueue`/reindex call in
that function, replacing the 5-6 separate `open_writer` calls with 1:

- Open once: `open_writer(&self.db_path)` → keep as an owned `Connection` (or `Option<Connection>`
  if fail-open handling elsewhere in the function still needs to distinguish "never opened" from
  "opened fine").
- `txn::begin`'s fail-closed behavior (line 1284-1309, `TRANSACTION_INIT_FAILED`) is unchanged in
  spirit — the ONE shared open still has to succeed before anything proceeds; only its scope grows
  to cover everything after it.
- Pass the same connection (as `&mut`) to `reindex_paths` at line ~1378 instead of its own separate
  `open_writer` at 1374.
- Reuse it for `maintenance::enqueue(ScipRefresh)` (1444), `advance(IndexCommitted)`/`advance(Done)`
  (1475), and `maintenance::enqueue(EmbedRefresh)` (1516, foreground half only — the background
  thread spawned after still opens its own connection since it must outlive this function's stack
  frame; that part is unchanged).
- Net effect: **5-6 `open_writer` calls → 1** per `edit_lines_impl_gated` call. Commit *count* is
  unchanged in this tier (still one explicit txn per `begin`/`advance` plus one ledger autocommit
  each) — only the connection-open overhead is removed.
- `format_files_impl`: same treatment inside its per-file loop — `begin` (593) and
  `advance→FileCommitted` (627/643) share one connection per file instead of two separate opens,
  cutting that phase from 2N to N connections; the already-shared final batch-advance loop (679) is
  unchanged.
- **Risk:** pure Rust-level connection-lifetime refactor. No SQL, schema, or state-machine change.
  No new error paths — a shared-connection-open failure degrades exactly like today's independent
  failures already do (reindex would fail the same way it does today if its own `open_writer` call
  failed; the difference is now that failure is shared/correlated with the txn journal's failure
  too, which is arguably more honest, since in practice both come from the same DB file and the
  same underlying disk/lock condition). Existing tests should need no rewriting, only a green
  re-run.

**Tier 2 — reduce commit count, not just connection count.** *(Original sketch below, written
before Tier 1 was measured. Tier 1's measurement (see top of doc) showed connection count wasn't
the dominant cost after all, and a fresh review of this sketch found a real crash-safety conflict
in its "batch the IndexCommitted→Done pair" idea — §5 supersedes the specifics here with a
corrected design; this paragraph is kept for history.)* Add a
`txn::advance_batch(conn, tx_id, transitions: &[(TxState, &str, &str)])` that wraps N sequential
transitions in one explicit `BEGIN IMMEDIATE`/`COMMIT` instead of N. This touches `txn::advance`
itself — a hub function, `caller_count=16`, `is_hub=true` per `file_overview` — and needs its own
careful review-before-code pass, the same discipline this session already applied to Change A/B in
`ws1-enforce-and-critical-risk-execution-plan.md`. **Not bundled into Tier 1.**

**Tier 3 — reduce `ledger::append`'s own per-call cost (optional, likely unnecessary).** Skip
`head_digest`'s `SELECT` re-query on every `append` by threading the previously-appended hash
forward across the (up to 3) appends one edit produces — safe in principle since `_guard`/
`_cross_guard` already serialize this whole function, so no concurrent ledger writer can interleave
between them. Touches `ledger.rs`'s public contract directly — the most sensitive of the three
tiers, since it's the tamper-evident audit trail P0-4 exists for. Recommend attempting this **only**
if Tier 1 + Tier 2 together still don't close gate criterion 6.

## 4. Recommended next step

Implement **Tier 1 only** first (both `edit_lines_impl_gated` and `format_files_impl` in the same
pass, since they share the identical root cause and `format_files_impl`'s version is structurally
worse). Then re-run the **exact same** git-worktree p95 methodology already used
(`ws1-enforce-and-critical-risk-execution-plan.md`'s measurement: baseline commit `acf2793` vs the
Tier-1 HEAD, N=200 in-process `edit_lines` calls, 2 runs each side) and report the real number.

Deliberately not predicting a percentage here — the prior measurement's own lesson was "measure,
don't assume" (the earlier assumption that shadow-mode overhead would be "well under 10%" turned
out wrong by a wide margin). If Tier 1 alone closes the ≤10% target, stop there; if not, Tier 2 is
the next candidate, reviewed in its own doc before any code changes, same as this one.

**2026-08-02, second pass — Tier 1 result is in (see top of this doc): it did NOT close the gap,
p50/p95 overhead is statistically unchanged from before Tier 1 (~41% both, vs ~31-35%/~50-100%
before).** This refutes the "connection-open is dominant" framing above and points at commit count
instead. §5 below researches and designs Tier 2 properly, having learned that lesson — including a
crash-safety conflict this original sketch's "batch IndexCommitted→Done" idea didn't anticipate.

## 5. Tier 2, researched (2026-08-02, second pass)

### 5.0 A real conflict found: merging state transitions breaks a milestone-tested guarantee

The original §3 sketch above proposed batching the `IndexCommitted`→`Done` pair (and, for
`format_files_impl`, all N files' pairs) into shared transactions. Re-reading
`crates/calm-cli/tests/txn_crash_injection.rs` closely before touching `txn::advance` (per this
session's own "verify before touching write-path logic" rule) surfaces a real problem with that:

The suite's own module doc (`txn_crash_injection.rs:10-20`) states it drives SIGKILL at exactly 3
points: `PREPARED` (after `begin`), `FILE_COMMITTED` (after the write), and **`INDEX_COMMITTED`
(after the index-refresh advance, before `Done`)** — and `assert_journal_consistent`
(`:132-204`) asserts the crashed transaction's cached state and `replay_state` both land on
`IndexCommitted` specifically, distinguishable from `Done`. That test exists because
`docs/plans/2026-08-02-phase1-p0-execution-plan.md` §6 gate criterion 1 ("crash-injection suite ...
tại mọi giá trị TxState") explicitly wants `IndexCommitted` to be a durably observable, independently
crash-recoverable checkpoint — not merely a transient in-memory step on the way to `Done`.

If `IndexCommitted`→`Done` were merged into one SQL transaction, a crash between "index refreshed"
and "marked Done" could no longer land the transaction in `IndexCommitted` at all: either the merged
commit finishes (state is `Done` directly) or it doesn't (state is still `FileCommitted`, the state
before the batch started). `IndexCommitted` would stop being a real, independently-reachable durable
state in the production code path — silently invalidating exactly the guarantee gate criterion 1
was written to check, even though the *existing* crash-injection test would still pass unmodified
(it drives the standalone `txn_crash_harness.rs` binary, not `edit_lines_impl_gated` — so it
wouldn't even notice this regression; it would keep testing a code path this change no longer
matches). **This is not a hypothetical concern for a future refactor — it is what §3's original Tier
2 sketch, written before this closer read, was actually proposing.** Merging different state
transitions for the *same* `tx_id` is off the table unless someone deliberately decides to relax
that milestone guarantee first — a decision for whoever owns the milestone checklist, not something
to fold into a perf pass silently.

### 5.1 What's still safe: the ledger's *second commit*, not the state machine

Re-examining §1's "9 commits" accounting: only **2 of those commits per `advance()` call are for the
*same* logical event** — `advance` (`txn.rs:246-307`) does its own `BEGIN IMMEDIATE`/`COMMIT` for the
`tx_events` insert + `edit_transactions` update, then — after that commit already succeeded —
calls `ledger::append` (`txn.rs:299`), which does its own **separate, unwrapped** `SELECT`
(`head_digest`) + `INSERT` (`ledger.rs:100-118`) with no transaction statements of its own at all
(no `BEGIN`/`COMMIT` in `append`'s body — it relies entirely on whatever transaction state the
caller's connection happens to be in). That's the real, uncontroversial-to-fix duplication: **one
transition, two commits, for reasons of implementation accident (ledger mirroring was bolted on
"after" for simplicity) rather than any state-machine requirement.**

Because `ledger::append` has no transaction wrapping of its own, folding it into `advance`'s
existing explicit transaction is almost free structurally — but doing it *safely* needs a
`SAVEPOINT`, not just moving the call earlier. Two SQLite behaviors matter here:

- An ordinary constraint failure (e.g. a `UNIQUE` collision on `audit_ledger.event_hash` — vanishing
  odds in practice since it's a SHA-256 of `payload || prev_hash`, but not provably impossible) rolls
  back only the failing *statement*, not the whole transaction — you can catch it and still `COMMIT`
  everything else. This alone would make "just move the call before `COMMIT`" appear to work.
- But a more severe class of error mid-statement (disk-full, I/O error) can force SQLite to abort
  the *entire* transaction, not just the statement — which, without a savepoint, would silently
  revert this design from "ledger failures never affect the transition" back to "ledger failures
  can now take the transition down with them," the exact opposite of `advance`'s own stated
  invariant (`txn.rs:292-298`). A `SAVEPOINT` around just the ledger append is the textbook fix:
  `ROLLBACK TO` the savepoint undoes only the ledger statement's effects on such a failure, while the
  outer transaction (holding the already-successful `tx_events`/`edit_transactions` writes) commits
  normally afterward.

**Design (Option A, recommended):** inside `advance`'s `Ok(payload) => { ... }` arm (`txn.rs:290`),
before `conn.execute_batch("COMMIT;")`, wrap the existing `ledger::append` call in a savepoint:

```rust
Ok(payload) => {
    match (|| -> Result<(), LedgerError> {
        conn.execute_batch("SAVEPOINT ledger_append;")?;
        let result = crate::ledger::append(conn, actor, &payload);
        conn.execute_batch(if result.is_ok() {
            "RELEASE ledger_append;"
        } else {
            "ROLLBACK TO ledger_append; RELEASE ledger_append;"
        })?;
        result.map(|_| ())
    })() {
        Ok(()) | Err(_) => {} // best-effort either way, same as today's `let _ =`
    }
    conn.execute_batch("COMMIT;")?;
    Ok(())
}
```

(Exact error-plumbing/helper shape TBD at implementation time — the point is the `SAVEPOINT`/
`RELEASE`/`ROLLBACK TO` bracketing, not this literal snippet.) This requires **no change to
`ledger.rs` itself** — `append`'s own SQL has no transaction statements to conflict with a
surrounding savepoint. Cuts every individual `advance()` call from 2 commits to 1, uniformly,
everywhere it's called (`edit_lines_impl_gated`, `format_files_impl`, `txn_crash_harness.rs`, all
tests) — with **zero change to state-machine granularity**: every currently-reachable `TxState`
remains its own durably-committed, independently crash-recoverable row, so the crash-injection suite
(and the milestone guarantee behind it) is untouched. Recomputed commit count for one
`edit_lines_impl_gated` call: `begin`=1, `FileCommitted`=1 (was 2), `IndexCommitted`=1 (was 2),
`Done`=1 (was 2), scip-enqueue=1, embed-enqueue=1 → **6 commits (was 9)**, a genuine ~33% reduction,
with Tier 1's connection-count reduction already banked on top.

**Bonus found while designing this:** today's design has a brief window, between the main
`advance()` commit and the ledger's own separate autocommit, where another connection could observe
`tx_events`/`edit_transactions` already updated but the matching `audit_ledger` row not yet written.
Option A removes that window too — both become visible to other readers atomically in the same
commit. Not the motivation for this change, but a real side benefit worth noting.

### 5.2 A second, independent lever for `format_files_impl`: batch across `tx_id`s, not across states

`format_files_impl`'s final loop (`edit.rs` §3 of the earlier Tier 1 diff, `for tx_id in
&shadow_tx_ids`) calls `advance` up to twice per file — but every file's transition is to the
**same** target state (`IndexCommitted`, then `Done`) at the **same point in the function**, for
**independent `tx_id`s**. Unlike §5.0's conflict (merging *different* states for *one* `tx_id`),
batching the *same* state transition across *multiple, independent* `tx_id`s in one shared SQL
transaction changes nothing about any single `tx_id`'s own crash story: if the process dies
mid-batch, whichever `tx_id`s didn't get their row committed yet simply remain at their previous
state — identical to what would happen if their individual `advance()` call had just never been
attempted. `replay_state`/`recover_incomplete` operate per-`tx_id` and don't care whether another
`tx_id`'s row happened to commit in the same physical transaction.

**Design (Option B, recommended for `format_files_impl` specifically):** a
`txn::advance_many(conn, transitions: &[(&str /* tx_id */, TxState, &str, &str)])` that opens one
`BEGIN IMMEDIATE`, loops the same per-transition work `advance` does today (still one `tx_events`
row + one ledger append-in-savepoint per transition, per Option A above), then one `COMMIT`. For a
batch of N formatted files, this brings the final advance phase from up to 2N commits (already down
from 4N after Option A alone) to as few as **2 commits total** (one for the `IndexCommitted` pass
over all N, one for the `Done` pass over all N) — the biggest win of the three tiers for
`format_files_impl`'s multi-file case, though `edit_lines_impl_gated` (always a single `tx_id`)
doesn't benefit from this specific lever at all — only Option A applies there.

### 5.3 What's explicitly NOT being proposed

Merging `IndexCommitted` and `Done` (or any two *different* states) for the *same* `tx_id` into one
transaction — §5.0's conflict. If a future pass wants that anyway (e.g. after re-measuring with
Options A+B and finding the gap still open), it needs an explicit decision to relax gate criterion
1's "every reachable TxState is crash-recoverable" guarantee, likely paired with updating
`txn_crash_harness.rs`/`txn_crash_injection.rs` to match whatever the new, coarser guarantee actually
is — not a decision to make inside a perf-optimization pass.

### 5.4 Recommended next step

Implement Option A first (universal, touches only `txn::advance`, zero state-machine impact,
~33% commit reduction on its own) and Option B alongside it for `format_files_impl` (independent,
same savepoint mechanism, bigger win specifically for multi-file batches). Then re-measure with the
same git-worktree methodology, 3 runs each side (matching what Tier 1's re-measurement used). Given
Tier 1's lesson, do not assume this closes gate criterion 6 — measure it. If commit count turns out
not to be the dominant cost either, the remaining candidates are Tier 3 (§3, ledger's own
`head_digest` re-query) or profiling the reindex/`rebuild_graph` path itself, neither explored yet.
