# ADR: B12 upgrade plan — three accuracy/effectiveness fixes (F1/F2+F2b/F4)

## 1. Title

Close the three findings from the B12 tool-surface root-cause audit
(`benchmarks/b12_tier1_tier2_tool_correctness/FINDINGS_ROOTCAUSE.md`,
`UPGRADE_PLAN.md`): JS/TS module-level call attribution (FIX1), `edit_context`
write-gate prediction parity (FIX2/F2b), and `diff_impact`'s new-symbol
false-positive on comment-only edits (FIX3/F4).

## 2. Context

The 2026-07-29 B12 audit found CALM's own JS/TS call graph was blind to any
call made outside a *named* function body — a module-level statement, or a
call nested only inside an anonymous callback (`describe('x', () => {
helper() })`). Two independent gates dropped these calls: `walk_calls`
(parser.rs) only emits a call when it has a named enclosing function, and
`extract_file_data` (pipeline.rs) drops even an emitted call if
`(enclosing_name, enclosing_line)` doesn't match an indexed symbol. This was a
major, previously unattributed contributor to the 2026-07-28 JS/TS recall
gaps (distinct from that day's B1 fix).

The same audit found `edit_context`'s `risk_assessment.level` was a *different
quantity* than what the real `edit_lines`/`edit_symbol` write gate actually
blocks on (`hub_hit`, not caller-count-derived risk) — an agent could read
"risk: low" from `edit_context` and still get blocked by `edit_lines`. It also
found `edit_context` was per-symbol while the gate scans the whole touched
line range (catching an enclosing hub class) — two related but distinct gaps
(F2 and F2b).

Separately, `diff_impact`'s `is_new_symbol` flagged a comment-only edit on a
zero-caller symbol as `symbol_is_new: true`, because a unified diff encodes an
in-place edit as remove-old+add-new — every line of the edited symbol's
signature reads as a `+` addition, indistinguishable from a genuine insertion
without also checking for a co-located removal.

## 3. Decision

Implemented all three fixes as scoped in `UPGRADE_PLAN.md`, in the plan's own
recommended order (F4, F2, F1):

- **FIX3/F4** (`crates/calm-core/src/analysis/diff_impact.rs`): `is_new_symbol`
  takes a new `signature_hunk_had_removal: bool` parameter and returns `false`
  whenever it's `true`, regardless of line coverage. The caller
  (`guardrails.rs::diff_impact`) computes that bool via a new
  `signature_range_has_removal(fd, signature_range)` helper, which pairs
  `FileDiff::hunks` with the index-aligned `FileDiff::removed_line_text`
  per-hunk (not per-file) so a removal in an unrelated hunk elsewhere in the
  same diff can't falsely suppress a genuinely new symbol.

- **FIX2/F2b** (`crates/calm-server/src/tools/edit.rs`,
  `crates/calm-server/src/tools/guardrails.rs`): extracted the gate's
  "which tier applies, and why" branch into a new pure function
  `classify_gate(hub_hit, risk, uncertain_zero_caller,
  bridge_downgrade_eligible, force_gate_always) -> GateClassification`
  (`GateRequirement::{None,ConfirmOnly,EditContextConfirmGroundedReason}`).
  `edit_lines_impl_gated` now calls this instead of its own inline
  duplicate logic (behavior-preserving refactor — the outer condition and the
  bridge-tier branch selection are unchanged, only de-duplicated). `edit_context`
  calls `compute_touch_risk` (now `pub(crate)`) over `[c.line_start,
  c.line_end]` — the whole symbol range, which naturally includes an
  enclosing hub class (closes F2b) — **plus** replicates the gate's own
  `bridge_downgrade_eligible` computation (needs `all_caller_edges_confident`,
  also raised to `pub(crate)`) so the predicted `requires` tier doesn't
  overstate a bridge-only hub as needing the full 3-layer gate. Result
  surfaced as a new `gate_prediction: { will_block, is_hub, hub_kind,
  blocking_symbols, requires, reason }` field on `EditContextOutput`.

- **FIX1** (`crates/calm-core/src/indexer/parser.rs`,
  `crates/calm-core/src/indexer/pipeline.rs`): new sentinel
  `pub(crate) const MODULE_ENCLOSING: &str = "<module>"`. `extract_calls_from_tree`
  seeds `walk_calls`'s initial `enclosing` with `Some((MODULE_ENCLOSING, 0))`
  instead of `None`, gated to `matches!(language, "javascript" | "typescript")`
  in this first cut (jsx/mjs/cjs share `"javascript"`, tsx shares
  `"typescript"` — verified via `lang_constants.rs`, no extension gap).
  `extract_file_data`'s `qn_by_loc` lookup now falls back to synthesizing
  `format!("{rel}::<module>")` when the lookup misses **and**
  `enclosing_name == MODULE_ENCLOSING`, instead of dropping the call. This
  string is used only as a `call_edges.from_symbol` value — never inserted
  into `symbols`, so symbol counts/search are unaffected. **Correction
  (2026-07-30, Finding E of the follow-up VHEATM review): `caller_count`/
  hub/coreness are NOT unaffected** — `refresh_caller_counts` computes
  `caller_count` via `COUNT(DISTINCT from_symbol) ... AND edge_confidence
  != 'ambiguous'` with no join to `symbols`, so a non-ambiguous `<module>`
  edge DOES raise its target's `caller_count` (and therefore hub/coreness).
  This is a real, previously-understated positive of FIX1: a JS/TS function
  called only at module level no longer falsely reads as "0 usages /
  possibly dead code".

## 4. Status

ACCEPTED

## 5. Consequences

**Improved:**
- JS/TS call-graph recall rises substantially for any codebase with
  module-level requires/registrations or anonymous-callback-wrapped calls
  (measured: express corpus 499→3174 total call edges, §7).
- `edit_context` output can no longer mislead an agent about whether a write
  will be gated (`gate_prediction` is provably the same computation the real
  gate runs, not a second independent approximation).
- `diff_impact` no longer reports a false "new symbol" on a pure comment edit
  to a pre-existing zero-caller function.
- Along the way, found and fixed a real blind spot in
  `benchmarks/resolution/run_benchmark.py`'s own measurement query (§7) — it
  was structurally unable to see any edge sourced from a non-symbol
  `from_symbol`, which is exactly what FIX1 introduces by design.

**Worsened / new debt:**
- JS/TS formal+resolved *share* of total call edges dropped in aggregate
  (22.4%/19.0% → 14.6%/4.1%) purely because the newly-surfaced module-level
  sites are harder to disambiguate by bare name across many small,
  independently-named files (see §7 — this is concentrated in a corpus's
  `examples/`-style directories, not its core library code, and is the
  `resolve_sites_to_edges` ambiguity logic working as designed, not a new
  bug). Not fixed here; flagged as the natural follow-up territory below.
- FIX1 is deliberately scoped to JS/TS only. Python/Rust/Go/Java/C and every
  other tree-sitter language still silently drops a module-level call,
  unchanged (verified: `test_python_module_level_call_still_dropped_not_gated_to_sentinel`).
  Extending the sentinel universally, and/or indexing significant anonymous
  callbacks (mocha `it`, event handlers) as real symbols for finer per-test
  attribution, is explicitly deferred (UPGRADE_PLAN.md's own follow-up note).
- `benchmarks/resolution/run_benchmark.py`'s `read_tier_histogram` fix
  (JOIN `file_index` instead of `symbols`) is a benchmark-script change, not
  covered by `cargo test` — no automated regression guard exists for this
  Python script.

## 6. Alternatives Considered

For FIX2/F2b: keep `edit_context`'s prediction as a second, independently
maintained approximation of the gate's logic (the pre-existing state).
Rejected — this is precisely the class of bug F2 already was (prediction and
gate silently diverging); a shared `classify_gate` function structurally
prevents the two from drifting apart again, at the cost of a small refactor
of the gate's own branch logic.

For FIX1: extend the sentinel to every language in the same pass, and/or
index anonymous callbacks as real symbols immediately. Rejected for this pass
per the plan's own risk sequencing — scoping to JS/TS first keeps the blast
radius isolated and measurable (Python/Rust/Go/Java/C resolution numbers
provably don't move, §7), and callback-level symbol indexing is a materially
larger design (new symbol-table entries, hub/coreness implications) that
deserves its own dedicated review rather than riding along with this fix.

## 7. Evidence

- `cargo test -p calm-core -p calm-server --lib`: 853 + 274 = 1127 tests
  passed, 0 failed, both before and after the toolsnap schema-snapshot
  regeneration (`UPDATE_TOOLSNAPS=1 cargo test -p calm-server
  tool_schemas_match_committed_snapshots`, needed because `gate_prediction`
  is a new required field). **[verified 2026-07-29]**
- `cargo test -p calm-core --test golden_graph_equivalence`: 12/12 non-heavy
  tests (incremental == full rebuild parity on controlled synthetic fixtures)
  passed. The 1 `#[ignore]`d heavy real-repo test
  (`golden_equivalence_incremental_vs_fresh_on_real_calm_repo`) failed both
  with and without this branch's changes — reproduced on a clean `main`
  checkout twice via `git stash` isolation (once passed, once failed — a
  probabilistic flake, not deterministic). Root cause: its own
  `fresh vs fresh` baseline check (before any incremental comparison even
  starts) shows `edge_confidence` flipping `formal`↔`textual` for the exact
  same edge between two back-to-back `index_fresh()` calls in one process —
  most likely the process-global `FORMAL_RESOLVER` `OnceLock` (`cached_formal_resolver`,
  pipeline.rs) being reused across two "from scratch" indexing passes.
  Confirmed unrelated to any of the 3 fixes: on the second clean-`main` run
  the divergent edge was a TypeScript (zod) pair with zero relation to this
  branch's code. **[verified 2026-07-29 — pre-existing, not introduced here]**
- Parser-level unit tests (`crates/calm-core/src/indexer/parser.rs`):
  `test_javascript_module_level_call_inside_anonymous_callback_attributes_to_module_sentinel`,
  `test_typescript_top_level_and_arrow_callback_calls_attribute_to_module_sentinel`,
  `test_python_module_level_call_still_dropped_not_gated_to_sentinel` — all
  pass. **[verified 2026-07-29]**
- Pipeline-level integration test
  (`extract_file_data_js_module_level_call_gets_synthesized_module_qn`,
  pipeline.rs): confirms the synthesized `"test.js::<module>"` qualified name
  reaches `ExtractedFile.call_sites` AND that `symbol_count` stays at exactly
  1 (the pseudo-caller never leaks into `symbols`). **[verified 2026-07-29]**
- `gate_prediction` parity tests (`crates/calm-server/src/tools.rs`):
  `edit_context_gate_prediction_matches_real_gate_for_hub_symbol` and
  `edit_context_gate_prediction_false_for_low_risk_non_hub_symbol` — each
  calls `edit_context`, reads its `gate_prediction`, then calls the REAL
  `edit_lines` with `confirm: false` on the same range and asserts the
  outcome matches the prediction. Both pass. **[verified 2026-07-29]**
- Resolution benchmark, `expressjs/express` corpus, before/after on the
  IDENTICAL pinned commit, both measured with the corrected
  `file_index`-joined query (`benchmarks/resolution/run_benchmark.py --lang js`):
  baseline 499 total call edges (formal 112/22.4%, resolved 95/19.0%, textual
  132/26.5%, ambiguous 160/32.1%) → FIX1 3174 total call edges (formal
  464/14.6%, resolved 130/4.1%, textual 1707/53.8%, ambiguous 873/27.5%) — a
  6.4× recall increase, absolute formal+resolved edge count also up
  (207→594). Direct DB breakdown by directory: `lib/` (express's real library
  code, 215 edges) is 68.8% formal+resolved with only 2.8% ambiguous; the
  precision dilution is concentrated in `examples/` (385 edges across ~50
  small independent demo scripts sharing generic names like `use`/`render`,
  19.0% formal+resolved, 49.9% ambiguous) — genuine name ambiguity across
  many unrelated small files, not a resolution regression.
  **[verified 2026-07-29, direct SQL queries against `.calm/index.db`]**
- B12 tool-surface benchmark, javascript (express),
  `benchmarks/b12_tier1_tier2_tool_correctness/run_benchmark.py --lang javascript`:
  the plan's own named regression case —
  `callers("test", path="test/res.format.js")` — returns `callers_direct_count: 5`,
  exactly matching the independent grep oracle's `grep_call_sites: 5`,
  `zero_recall_bug: false`. Across the 6-symbol `callers` sample: 0/6
  zero-recall bugs. **[verified 2026-07-29]**

## 8. Owner

Claude Sonnet 5 (agent session), on behalf of the repo owner (gokuderafight@gmail.com).

## 8b. Known Debts (PATTERN-DEBT)

No existing `docs/superskills/pattern-debt.md` entries are affected by this
change (checked: none of the 3 fixes touch a registered DEBT-NNN area from
`docs/pattern-debt-registry.yaml`).

**`golden-repo-formal-resolver-nondeterminism` — PARTIALLY CLOSED 2026-07-30 (ADR-A1 fully; ADR-A2 partially, honest limit found via calibration).**
The original diagnosis above (`FORMAL_RESOLVER` `OnceLock` reuse across two
`index_fresh()` calls) was **wrong on mechanism**: `FormalResolver::resolve_file`
takes `&self` and builds a fresh local `StackGraph`/`PartialPaths`/`Database`
per call — reusing the cached resolver cannot itself cause divergence between
two calls on identical input. The real root cause, found by a follow-up
VHEATM review and fixed same-day: `resolve_file`'s `RESOLVE_TIMEOUT` (3s
wall-clock) can trip under machine load/rayon contention; the cancelled
`Err` was silently swallowed by `.unwrap_or_default()` (pipeline.rs),
indistinguishable from "resolved, genuinely found nothing" — so a file's
edges could flip `formal`↔`textual` across otherwise-identical reindexes.
Fixed via:
  - **ADR-A1 (fully closes the silent-failure half)**: `formally_resolved_names()`
    helper distinguishes `Ok`/`Err`, increments a `FORMAL_RESOLUTION_TIMEOUTS`
    counter + logs on `Err`, surfaced as `indexing_status.formal_resolution_timeouts`.
    A cancelled resolution is no longer invisible or indistinguishable from a
    genuine empty result.
  - **ADR-A2 (partial — see honest limit below)**: `BoundedCancellation`/
    `TsgBoundedCancellation` wrap both the TSG-build and stitching-phase
    deadlines with a deterministic total-check-count ceiling (delegating to
    the existing wall-clock deadline first, preserving the `_pydecimal.py`
    hang-prevention guarantee). Empirically, for this codebase's own
    pathological-class fixture, the actual unbounded work was in TSG graph
    *construction* (`build_stack_graph_into`), not path stitching as
    originally assumed — both phases are now bounded.
  - **Honest limit, found via calibration against real/synthetic large files
    (2026-07-30):** check-count is NOT a uniform proxy for wall-clock cost
    across code shapes. A safely-calibrated ceiling (high enough that a
    legitimate 400-method/1703-line file — needing ~656K checks — is not
    falsely truncated) does **not** trip before the wall-clock does for the
    existing deep-self-reference-chain pathological fixture specifically:
    each check on a long chain costs far more wall-clock than a check on
    ordinary code, so that shape's checks never reach even 1,000,000 within
    3 seconds. For THAT shape, `RESOLVE_TIMEOUT` remains the operative (and
    still machine-load-sensitive) bound, unchanged from before this ADR.
    What ADR-A2 DOES provide: full determinism for shapes whose bottleneck
    genuinely is check count (not per-check cost), and a hard finite
    ceiling against even-worse-than-`_pydecimal.py` explosions regardless
    of per-check cost (bounded, not literally unbounded). Full determinism
    for the deep-chain shape remains open — would need a cost-weighted
    budget (e.g. bounding total `PartialPath` length/complexity processed,
    not just `check()` call count) or a different approach entirely (e.g.
    content-hash memoization, keeping wall-clock as-is but caching
    known-`Complete` results so re-hitting the SAME nondeterministic file
    twice isn't possible — first resolution wins and is reused).
  - Evidence: `test_resolve_file_pathological_class_still_bounded_by_wall_clock_after_adr_a2`
    + `test_resolve_file_legitimate_large_file_not_falsely_truncated` +
    `bounded_cancellation_trips_exactly_after_max_checks_and_delegates_to_inner_first`
    + `tsg_bounded_cancellation_trips_exactly_after_max_checks_and_delegates_to_inner_first`
    (all formal.rs). Full `calm-core`+`calm-server`+`parity_test` suites
    green (859+275+6 passed, 0 failed, 12 ignored).

## 9. Next Cycle Trigger

When `benchmarks/resolution/run_benchmark.py --lang js` (or a future `--lang typescript`
addition to that corpus map) is re-run and the `formal+resolved` share for a
corpus's core library directory (not its examples/test directories) drops
below 50%, OR when the deferred universal-sentinel/callback-symbol follow-up
from UPGRADE_PLAN.md is picked up (extending `MODULE_ENCLOSING` to Python
first, per the follow-up review's REQUIRED-tier ranking), OR when
`indexing_status.formal_resolution_timeouts` (ADR-A1) is observed nonzero
and growing on a real repo — worth investigating which files/languages
before trusting `formal` counts as fully stable, and a signal that
`MAX_TSG_BUILD_CANCELLATION_CHECKS`/`MAX_STITCH_CANCELLATION_CHECKS`
(ADR-A2) may need recalibrating for that repo's actual file sizes — OR
when the ADR-A2 "honest limit" debt (deep-reference-chain shapes remain
wall-clock-bound, not check-count-bound) is picked up, requiring either a
cost-weighted budget (bound `PartialPath` length/complexity, not raw
`check()` count) or content-hash memoization of `resolve_file` results
keyed on `(file_hash, lang)`.

## 10. Cycle Retrospective

- The B12/`resolution/` benchmark's own SQL (`INNER JOIN symbols ON
  s.qualified_name = ce.from_symbol`) assumed every `call_edges.from_symbol`
  is a real indexed symbol — an assumption FIX1 deliberately breaks by
  design (`<module>` is never inserted into `symbols`). Before-instrumenting
  fix, the resolution benchmark reported byte-identical numbers with and
  without FIX1 active, which nearly read as "the fix has zero effect" — it
  was actually the measurement tool being blind, not the fix being inert.
  **Next agent: always inspect what a benchmark's SQL actually JOINs on
  before trusting an unchanged number as "no effect.**"
- The `golden_graph_equivalence` heavy real-repo test looked, at first
  glance, like a FIX1 regression (a JS edge appeared in its failure diff on
  the second run). It was not — the SAME test fails on a clean, unmodified
  `main` checkout too (reproduced twice, with completely unrelated
  Python/C/TypeScript edges each time). **Always get an n≥2 baseline on
  unmodified `main` via `git stash` before attributing a heavy/flaky test's
  failure to your own change — one clean pass on main is not enough
  evidence.**
- `edit_symbol`'s `position="after"` insertion, when the anchor symbol's
  immediately-following doc comment logically belongs to the NEXT item (not
  the anchor), silently attaches the new inserted block BEFORE that
  following doc comment rather than after it — self-caught here (a doc
  comment for `extract_calls_from_tree` ended up sitting above the newly
  inserted `MODULE_ENCLOSING` const instead). **When inserting new code
  directly above an existing item via `position="after"` on the PRECEDING
  item, re-read the actual result before moving on — don't trust the
  insertion point blindly when the next item has its own leading doc
  comment.**
- `CallSiteData`/`ParsedSymbol` (calm-core) don't `#[derive(Debug)]` —
  discovered only at test-compile time when trying to use `{:?}` in an
  assertion message. Cheap enough to work around (map to `(&str, &str)`
  tuples / `Vec<&str>` of names) rather than adding `Debug` to production
  structs as a side effect of writing a test.
- `git stash push -- crates/` (path-scoped) is a clean, low-risk way to get
  an A/B comparison against the current `main` tree without needing a second
  worktree or clone — used 3 times this cycle for isolation checks, each
  time popped back cleanly with no conflicts.
