# ADR: Formal-resolver timeout observability (A1) + deterministic cancellation ceilings (A2)

## 1. Title

Close the silent-failure half of the formal-resolver nondeterminism bug (A1);
add deterministic cancellation ceilings with an honestly-documented partial
scope (A2). Plus two drive-by doc corrections (B, E) from the same review
cycle.

## 2. Context

A follow-up VHEATM review of `docs/superskills/adrs/2026-07-29-b12-upgrade-plan-three-fixes.md`
found that ADR's own root-cause diagnosis for `golden_equivalence_incremental_vs_fresh_on_real_calm_repo`'s
flake was wrong on mechanism: it blamed the process-global `FORMAL_RESOLVER`
`OnceLock` for reuse-caused nondeterminism, but `FormalResolver::resolve_file`
takes `&self` and builds a fresh local `StackGraph`/`PartialPaths`/`Database`
on every call — reusing the cached resolver cannot itself cause divergence
between two calls on identical input.

The real mechanism, traced end-to-end this cycle: `resolve_file`'s
`RESOLVE_TIMEOUT` (3s wall-clock) can trip under machine load or rayon
contention during parallel indexing. The resulting `Err` was silently
swallowed via `.unwrap_or_default()` (`pipeline.rs`), collapsing to the
identical empty `HashSet` that a genuine "resolved, found nothing" result
also produces — with zero signal distinguishing the two. Consequently, a
file's call edges could flip between `formal` and `textual` confidence
across otherwise-identical reindexes, with no visibility into why.

## 3. Decision

**ADR-A1** (`crates/calm-core/src/indexer/pipeline.rs`): extracted the
`resolve_file(...).unwrap_or_default()` call site into a named
`formally_resolved_names()` helper that pattern-matches `Ok`/`Err`
explicitly. `Err` still degrades to an empty set (unchanged behavior for
the `formally_resolved.contains(...)` check downstream — tier-1/tier-2
confidence is untouched either way), but now increments a new
`FORMAL_RESOLUTION_TIMEOUTS` `AtomicU64` counter and logs a `tracing::warn!`
with the file/language. The counter is surfaced via a new
`indexing_status.formal_resolution_timeouts` field
(`crates/calm-server/src/tools/recover.rs`), regenerating the
`indexing_status` toolsnap.

**ADR-A2** (`crates/calm-core/src/resolver/formal.rs`): added
`BoundedCancellation` (wraps `stack_graphs::CancellationFlag`, `Cell<usize>`
counter) and `TsgBoundedCancellation` (wraps
`tree_sitter_stack_graphs::CancellationFlag`, which requires `Sync` — hence
`AtomicUsize` instead of `Cell`). Both delegate to the existing wall-clock
deadline first, then also trip once their own `check()` invocation count
crosses a calibrated ceiling (`MAX_TSG_BUILD_CANCELLATION_CHECKS =
2_000_000`, `MAX_STITCH_CANCELLATION_CHECKS = 1_000_000`), applied to TSG
graph construction (`build_stack_graph_into`) and the stack-graphs
indexing+stitching phases respectively.

**Mid-cycle course correction (important):** the original intent was for
these ceilings to make `resolve_file`'s outcome fully independent of
machine speed — "same input always truncates at the same point." Calibration
testing against real and synthetic files disproved this for one specific
shape. See §5/§8b for the honest finding and what was actually shipped.

**B**: fixed a stale docstring in `benchmarks/resolution/run_benchmark.py`
(lines 40-45) that still described the pre-FIX1 `symbols`-joined query
instead of the current `file_index`-joined one.

**E**: corrected `docs/superskills/adrs/2026-07-29-b12-upgrade-plan-three-fixes.md`'s
claim that the `<module>` pseudo-caller sentinel leaves "hub/coreness
unaffected" — `refresh_caller_counts` has no join to `symbols` on
`from_symbol`, so non-ambiguous `<module>` edges DO count toward
`caller_count`/hub/coreness, a real (previously understated) positive of
that cycle's FIX1.

## 4. Status

ACCEPTED

## 5. Consequences

**Improved:**
- A cancelled formal resolution is no longer invisible or indistinguishable
  from a genuine empty result (A1) — `indexing_status.formal_resolution_timeouts`
  gives real visibility into how often `RESOLVE_TIMEOUT` fires on any given
  repo.
- Full determinism IS achieved for code shapes whose bottleneck is check
  count rather than per-check cost (A2), and a hard finite ceiling now
  bounds even-worse-than-`_pydecimal.py` explosions, converting a possible
  unbounded/multi-minute hang into a bounded one regardless of shape.
- Two prior ADR inaccuracies corrected (B, E), keeping the KB trustworthy
  for future agents who read it via `kb-query`.

**Worsened / new debt:**
- **A2 does not fully close the original nondeterminism for every shape.**
  Calibration (2026-07-30) found check-count is not a uniform proxy for
  wall-clock cost: a legitimate 400-method/1703-line synthetic file needs
  ~656K checks in ~3s (linear scaling confirmed at 200/400 methods), while
  the existing deep-self-reference-chain pathological regression fixture
  does NOT cross even 1,000,000 checks within 3s — each check on a long
  chain costs far more wall-clock than a check on ordinary code. A ceiling
  safely calibrated above legitimate-file needs (2,000,000 / 1,000,000)
  therefore does not trip before the wall-clock does for that specific
  shape; `RESOLVE_TIMEOUT` remains the operative, still machine-load-
  sensitive bound there, unchanged from before this ADR. Registered as
  `DEBT-012-formal-resolver-deterministic-cap-honest-limit`.
- Separately (and not chased down further this cycle, out of scope):
  calibration incidentally discovered that ordinary real-world Python
  files above roughly 300-400 methods can ALREADY exceed the pre-existing
  3s `RESOLVE_TIMEOUT` on their own merits, independent of any pathological
  pattern — a real file (`pyparsing/core.py`, 6115 lines) took 3.3s+ even
  with the deterministic ceiling raised to 50,000,000 (i.e. effectively
  disabled). This suggests `RESOLVE_TIMEOUT=3s` may itself be miscalibrated
  for realistic large files, independent of A1/A2 — noted as a `remaining`
  item on DEBT-012, not fixed here (would need its own dedicated
  measurement cycle across a representative corpus, not a one-off change
  riding along with this one).
- Two other real Python files tested during calibration
  (`urllib3/response.py`, `pip/_vendor/distro/distro.py`) failed for an
  unrelated, pre-existing TSG rule execution limitation
  (`UndefinedScopedVariable` on some assignment pattern) — confirmed
  unrelated to A1/A2 (same error class, unaffected by either change) but
  not investigated further; flagged for whoever next touches Python TSG
  rule coverage.

## 6. Alternatives Considered

For A2: content-hash memoization of `resolve_file` results keyed on
`(file_hash, lang)`. Rejected for THIS cycle (deferred to DEBT-012's
`remaining` list) — it doesn't fix determinism for a file's FIRST
resolution (the wall-clock race still happens once), only guarantees
every SUBSEQUENT reindex of unchanged content reuses the same result. It's
a good complementary follow-up, not a substitute for this cycle's ceilings,
and is a larger change (cache invalidation, `Complete`-vs-`Cancelled`
result tagging) than was scoped here.

For A2's calibration: considered keeping the original low ceiling (20,000)
that made the exact pathological fixture "look fixed" fast. Rejected after
discovering it falsely truncates a legitimate 853-line/200-method
real-shape file in 220ms (proven via calibration probe) — accuracy for
ordinary code was judged more valuable than a cosmetically-fast pathological
test result achieved by a miscalibrated constant.

## 7. Evidence

- `cargo test -p calm-core --lib`: 859 passed, 0 failed, 12 ignored
  (pre-existing ignores unaffected). **[verified 2026-07-30]**
- `cargo test -p calm-server --lib`: 275 passed, 0 failed. **[verified 2026-07-30]**
- `cargo test -p calm-core --test parity_test`: 6 passed, 0 failed
  (includes `test_formal_edges_integration`, exercises `resolve_file`
  directly). **[verified 2026-07-30]**
- `UPDATE_TOOLSNAPS=1 cargo test -p calm-server tool_schemas_match_committed_snapshots`:
  regenerated for the new `formal_resolution_timeouts` required field, then
  re-ran clean without `UPDATE_TOOLSNAPS` to confirm stability.
  **[verified 2026-07-30]**
- A1 TDD: `formally_resolved_names_ok_returns_edge_names_unaffected` +
  `formally_resolved_names_err_returns_empty_and_increments_counter`
  (pipeline.rs) — RED (compile error, symbols didn't exist) → GREEN.
  **[verified 2026-07-30]**
- A2 TDD: `bounded_cancellation_trips_exactly_after_max_checks_and_delegates_to_inner_first`
  + `tsg_bounded_cancellation_trips_exactly_after_max_checks_and_delegates_to_inner_first`
  (formal.rs) directly unit-test the ceiling mechanism (trips at exactly
  `max_checks + 1`, delegates to inner first) independent of stack-graphs
  resolution behavior. **[verified 2026-07-30]**
- A2 calibration data (measured on this machine via a since-removed
  diagnostic probe, not committed): 200-method/853-line synthetic file →
  328,200 TSG-build checks, 2.4s, `Ok`. 400-method/1703-line synthetic file
  → 655,901 checks, ~3.0s, hits the pre-existing wall-clock during the
  subsequent stitching phase. Pathological fixture (300 methods × 30-deep
  `self.aN` chains) → does not cross 1,000,000 TSG-build checks within
  3.4-3.5s across 3 independent cap values tested (20,000 / 700,000 /
  1,000,000 / 3,000,000), confirming its check-rate is far lower than the
  realistic shape's. **[verified 2026-07-30, single-machine measurement —
  not cross-machine, flagged as a limit of this evidence itself]**
- `test_resolve_file_pathological_class_still_bounded_by_wall_clock_after_adr_a2`
  passes (elapsed < `RESOLVE_TIMEOUT` + 2s, matching the pre-existing
  sibling test's own tolerance — i.e. no regression, no improvement for
  this shape). **[verified 2026-07-30]**
- `test_resolve_file_legitimate_large_file_not_falsely_truncated` passes
  (60-method file resolves `Ok`). **[verified 2026-07-30]**

## 8. Owner

Claude Opus 5 (agent session), on behalf of the repo owner (gokuderafight@gmail.com).

## 8b. Known Debts (PATTERN-DEBT)

- `DEBT-012-formal-resolver-deterministic-cap-honest-limit` (NEW, opened
  2026-07-30, `docs/pattern-debt-registry.yaml`, status: open, urgency:
  medium): the A2 honest limit described in §5/§7 above — deep-reference-
  chain shapes remain wall-clock-bound (not check-count-bound), full
  cross-machine determinism for that shape remains open. Also carries the
  incidental `RESOLVE_TIMEOUT` possibly-too-tight-for-large-legitimate-files
  finding and the 2 unrelated TSG rule execution failures as `remaining`
  items for whoever picks this up next.
- No other existing `docs/pattern-debt-registry.yaml` entries are affected.

## 9. Next Cycle Trigger

When `indexing_status.formal_resolution_timeouts` is observed nonzero and
growing on a real (non-CALM) repo over a full day of normal agent use —
worth pulling the specific files/languages triggering it and checking
whether they're ordinary-sized (pointing at `RESOLVE_TIMEOUT` itself being
too tight) or genuinely pathological (pointing at DEBT-012's cost-weighted-
budget remaining item) — OR when a second real-world instance of
`golden_equivalence_incremental_vs_fresh_on_real_calm_repo`'s flake is
captured with its specific diverging edge logged, to determine whether
that shape is check-count-bound (A2 already helps) or per-check-cost-bound
(A2 does not help, DEBT-012 remains fully open).

## 10. Cycle Retrospective

- The original ADR's "OnceLock reuse" root-cause was accepted without
  verifying that `resolve_file` actually shares mutable state across
  calls — it doesn't (`&self`, fresh local graph every call). **Lesson:**
  a plausible-sounding mechanism for an observed symptom still needs a
  code-level check that the blamed component actually has the capability
  to cause it, not just correlation with the timing of the observation.
- The single biggest surprise this cycle: "total operation count" is
  **not** a shape-independent proxy for "wall-clock work" the way it's
  often assumed to be in cancellation-budget designs. A deep-chain shape
  and a wide-independent shape can need wildly different real time per
  unit of counted work. Any future "replace wall-clock with a deterministic
  counter" design should calibrate against BOTH a realistic-large-file case
  AND the specific pathological case it's meant to catch, empirically,
  before assuming a single threshold serves both.
- Would design differently if starting over: measure the check-rate
  divergence between shapes FIRST (a 30-minute investment) before writing
  any wrapper code — would have caught the tension before implementing
  two full wrapper structs that then needed their claims walked back.
- Debt knowingly created: DEBT-012. Chosen over blocking this cycle on a
  fully general cost-weighted-budget redesign, which is a materially
  larger change (touches `PartialPath` internals) than the observability
  win (A1) and partial-determinism win (A2) already banked this cycle.
- Signal for next cycle: `indexing_status.formal_resolution_timeouts` is
  now the load-bearing telemetry for this whole area — if it stays at 0
  across real usage, DEBT-012 is lower priority than it looks today; if it
  climbs, prioritize accordingly.
