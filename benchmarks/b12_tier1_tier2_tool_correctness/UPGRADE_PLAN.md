# CALM accuracy/effectiveness upgrade plan — from the B12 root-cause audit

Derived from `FINDINGS_ROOTCAUSE.md`. Every fix below is anchored to the exact function
read during the audit and validated against the real end-to-end path (indexer → DB →
tool). F3 needs no change (refuted). Ordering is by ROI: **F1 first** (biggest accuracy
lever), then F2/F2b, then F4.

Global validation harness for all three: re-run `benchmarks/b12_tier1_tier2_tool_correctness/`
(the tool-surface oracle) + `benchmarks/resolution/` (tier distribution, 6 Tier-0 langs) +
`cargo test -p calm-core -p calm-server` + the `golden_graph_equivalence` test (incremental
== full rebuild must still hold). Each fix ships behind its own tests + a B12 re-run showing
the specific finding cleared.

---

## FIX 1 (HIGH) — capture calls made outside a named-function body (JS/TS first)

**Root cause recap:** two independent gates drop these calls.
- Gate A — `walk_calls` ([parser.rs:1589](../../crates/calm-core/src/indexer/parser.rs#L1589)):
  a call is emitted only when `current` (nearest enclosing *named* function) `is_some()`.
- Gate B — `extract_file_data` ([pipeline.rs:508](../../crates/calm-core/src/indexer/pipeline.rs#L508)):
  even an emitted `RawCall` is dropped unless `qn_by_loc.get((enclosing_name, enclosing_line))`
  finds an indexed symbol at that exact (name, line).

The deliberate `extract_calls` docstring ("Top-level calls … are skipped — they have no
caller symbol") is the intent to change.

**Design — a per-file module pseudo-caller (no new indexed symbol):**

1. Introduce a sentinel, `const MODULE_ENCLOSING: &str = "<module>"` (angle brackets — cannot
   collide with any real identifier), enclosing line `0`.
2. `extract_calls_from_tree` / `walk_calls` entry: start the walk with
   `current = Some((MODULE_ENCLOSING.to_string(), 0))` instead of `None`, **gated to
   `matches!(language, "javascript" | "typescript")`** in the first cut. Inside a named
   function the existing code overwrites `current`, so the sentinel only survives for calls
   whose nearest function ancestor is anonymous / module-level — exactly the dropped set.
   No other walk logic changes; the Elixir `is_definition_macro_call` phantom-self-call guard
   is untouched.
3. `extract_file_data`: when `qn_by_loc` has no entry for `(enclosing_name, enclosing_line)`
   **and** `enclosing_name == MODULE_ENCLOSING`, synthesize `enc_qn = format!("{rel}::<module>")`
   directly instead of dropping (a `.or_else` on the existing `if let Some(enc_qn) = …`). The
   pseudo qn is used only as a `call_edges.from_symbol` string — **not** inserted into
   `symbols`, so symbol counts, `search`, `hotspots`, hub/coreness are all unaffected.

**Why this surfaces end-to-end (verified):**
- Resolution (`resolve_sites_to_edges`) matches the callee by name with the existing
  same-file preference (`from_path` = the file), so `test(app)` → resolved edge
  `<module> → test/res.format.js::test`.
- `callers()` uses `LEFT JOIN symbols ON qualified_name = from_symbol`
  ([trace.rs:46](../../crates/calm-server/src/tools/trace.rs#L46)) — a non-symbol `from_symbol`
  returns fine, shown as a direct caller with its `from_path`.
- `refresh_caller_counts` counts `DISTINCT from_symbol … edge_confidence != 'ambiguous'`
  ([pipeline.rs:1290](../../crates/calm-core/src/indexer/pipeline.rs#L1290)), so the target's
  `caller_count` rises → risk/hub/dead-code all correct.

**Edge cases / guards:**
- Ambiguous fan-out (`app.use(x)` at module level → many `use` candidates) already becomes
  `ambiguous` in `resolve_sites_to_edges` — no precision regression, same handling as today.
- A call at module level to a name with no matching symbol (`require`, `describe`) yields no
  candidate → no edge, same as now.
- Don't emit a self-edge: a top-level IIFE calling itself is not a concern (anonymous, no name
  to match). The `<module>` pseudo has no callee named `<module>`.

**Tests:** unit test in `parser.rs` — a JS source with `describe('x', function(){ helper() });
function helper(){}` must produce a call site `(<module> → helper)`; a TS file mixing a
top-level `foo()` and an `it(() => bar())`. Integration: index the express fixture, assert
`callers("test", path="test/res.format.js")` ≥ 5 direct.

**Blast radius / risk:** MEDIUM. Scoped to JS/TS in the first cut, so Python/Rust/Go/Java/C
resolution numbers don't move. Expect JS/TS `call_sites`/edge counts to jump substantially
(express was 695 total — will multiply) and JS/TS recall in `benchmarks/resolution/` to rise.
Re-run resolution + B12 to confirm precision (formal/resolved share) doesn't collapse.

**Follow-up (separate task):** after JS/TS is validated with an oracle, consider extending the
sentinel to all languages (Python `if __name__` blocks, etc.), and — higher fidelity — indexing
significant anonymous callbacks (mocha `it`, event handlers) as real symbols so attribution is
per-test-case, not per-file. Both are additive to this change.

---

## FIX 2 (MED) — make `edit_context` predict the write-gate exactly (closes F2 + F2b)

**Root cause recap:** `EditContextOutput` omits `is_hub`; its `risk_assessment.level` is
caller-count-derived, a *different* quantity than the gate's `hub_hit`
([edit.rs:1682](../../crates/calm-server/src/tools/edit.rs#L1682)); and the gate scans every
symbol overlapping the edit **range** (incl. the enclosing class), while `edit_context` is
per-symbol (F2b).

**Design — reuse the gate's own function as the single source of truth:**

1. Change `compute_touch_risk` ([edit.rs:1662](../../crates/calm-server/src/tools/edit.rs#L1662))
   from module-private `fn` to `pub(crate) fn`; likewise expose `UncertainZeroCallerReason` and
   `TouchedSymbolOutput` to the `guardrails` module (they already derive Serialize).
2. In `edit_context` ([guardrails.rs:21](../../crates/calm-server/src/tools/guardrails.rs#L21)),
   after resolving `c`, call
   `compute_touch_risk(&conn, &c.path, &[(c.line_start, c.line_end)], &self.coverage.read_ok())`.
   Because `symbols_overlapping_ranges` covers the whole line range, the returned `touched`
   naturally includes the **enclosing class** when the symbol sits inside one — this is what
   makes it predict F2b, not just F2.
3. Add a `gate_prediction` field to `EditContextOutput`:
   ```
   gate_prediction: {
     will_block: bool,                 // hub_hit || risk=="high" || uncertain_zero_caller.is_some() || always_require_edit_context
     is_hub: bool,                     // this symbol's own c.is_hub
     hub_kind: Option<String>,         // strongest hub_kind across the touched range
     blocking_symbols: Vec<String>,    // touched symbols that are hubs / high-risk (names the enclosing class here)
     requires: "none" | "confirm" | "edit_context+confirm+grounded_reason",
     reason: Option<String>,           // same `why` string the gate would emit
   }
   ```
   Compute `requires`/`reason` by mirroring the gate's branch logic
   ([edit.rs:886-1000](../../crates/calm-server/src/tools/edit.rs#L886)) — ideally by
   extracting that branch into a shared `fn classify_gate(hub_hit, risk, uncertain, bridge_eligible,
   force) -> GateRequirement` used by BOTH the gate and this prediction, so they can never drift.
4. Keep the existing `risk_assessment` (advisory review-risk, richer: entropy + dead-code) but
   document in its doc comment that `gate_prediction` — not `risk_assessment.level` — is what
   determines a write block.

**Tests:** on the spring-petclinic fixture, `edit_context("initFindForm")` must return
`gate_prediction.will_block == true` with `blocking_symbols` containing `OwnerController`
(the enclosing hub) and `requires == "edit_context+confirm+grounded_reason"`; a plain
low-risk non-hub function returns `will_block == false`. Add a **parity test**: for a sampled
symbol, `edit_context(...).gate_prediction.will_block` equals what a real
`edit_lines(no confirm)` actually returns (blocked vs applied).

**Blast radius / risk:** LOW. Additive output field + a visibility change + optional
gate-branch extraction. No behavior change to the gate itself. The parity test is the guard
against future drift.

---

## FIX 3 (LOW) — stop `diff_impact` calling an in-place-edited symbol "new"

**Root cause recap:** `is_new_symbol` ([diff_impact.rs:324](../../crates/calm-core/src/analysis/diff_impact.rs#L324))
returns true when every signature line is in `added_lines` and `caller_count == 0`. A unified
diff encodes an in-place single-line edit as remove-old + add-new, so the edited signature line
counts as "added" — indistinguishable from a fresh insert. `FileDiff` already carries the signal
to disambiguate: `removed_line_text: Vec<Vec<String>>`, index-aligned with `hunks`
([diff_impact.rs:121-125](../../crates/calm-core/src/analysis/diff_impact.rs#L121)).

**Design — require a pure insertion (added, with no co-located removal):**

1. Extend `is_new_symbol` to also receive the per-hunk removal info (pass `fd: &FileDiff`, or a
   precomputed `signature_hunk_had_removals: bool`). Signature: keep it a pure function —
   `is_new_symbol(signature_range, file_is_new, added_lines, caller_count, hunks_with_removals)`.
2. New rule: a symbol is new iff `file_is_new`, **or** (`caller_count == 0` **and** every
   signature line is in `added_lines` **and** no hunk overlapping the signature range has any
   removed line). Compute the last clause at the call site
   ([guardrails.rs:547](../../crates/calm-server/src/tools/guardrails.rs#L547)) where `fd` is in
   scope: `fd.hunks.iter().zip(&fd.removed_line_text).any(|((hs,he), removed)| overlaps(sig_range,(hs,he)) && !removed.is_empty())`.

**Edge case:** replacing a deleted region with a brand-new symbol (removal + genuinely new
symbol in one hunk) now reports `symbol_is_new: false` — a rare, safe false-negative (it just
falls through to the existing `signature_changed` path, which needs the *text* to differ). Far
preferable to today's false-positive-on-every-comment-edit. Note it in the doc comment.

**Tests:** unit tests in `diff_impact.rs` — (a) a hand-built diff that appends ` # comment` to a
zero-caller `def`'s line → `is_new_symbol == false`; (b) a pure `+`-only insertion of a new
function → `is_new_symbol == true`. Integration: the B12 `edit_workflow.diff_impact_on_comment_only_edit`
check must show no `symbol_is_new: true` on the comment-only edit.

**Blast radius / risk:** LOW, self-contained to `analysis::diff_impact` + its one call site.

---

## Sequencing & effort

| Fix | Value | Effort | Risk | Depends on |
|---|---|---|---|---|
| F1 (JS/TS module-caller) | HIGH | ~M | MED | — (validate w/ resolution + B12) |
| F2+F2b (gate_prediction) | MED | ~S–M | LOW | — |
| F4 (is_new_symbol) | LOW | ~S | LOW | — |

Recommended: land **F4** and **F2** first (small, low-risk, independently testable), then **F1**
(the big lever) with a full resolution + B12 re-run and an oracle sanity-check on JS/TS
precision before considering the universal/callback-indexing follow-ups.

Each fix must go through CALM's own `edit_context` → `edit_lines/edit_symbol` → `diff_impact`
gates when implemented (dogfood), and an ADR per `adr-commit` before merge.

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-29 | trigger: NORMAL (manual, plan predates specs/ frontmatter convention) -->

**Tier:** 2 (Production — this is CALM's own write-gate + core indexer, dogfooded live) | **Date:** 2026-07-29

All three designs were re-verified line-for-line against the current on-disk code
(`walk_calls` parser.rs:1528-1711, `extract_file_data` pipeline.rs:314-603,
`compute_touch_risk`/gate branch edit.rs:1662-1740 + 830-1040, `edit_context`
guardrails.rs:21-369, `is_new_symbol`/`FileDiff` diff_impact.rs:106-338) before this audit —
no drift from the plan's line citations found.

### Failure Modes
1. FIX1 — JS/TS `caller_count`/hub/coreness shift for existing symbols once module-level
   calls resolve (e.g. a previously non-hub `test` helper crossing the hub threshold) changes
   `edit_lines`/`edit_symbol` write-gate behavior for unrelated future edits, not just
   accuracy-reporting — MED — mitigation in plan: YES (plan already calls for a resolution +
   B12 re-run to confirm precision doesn't collapse; add an explicit check that hub-count
   deltas are reviewed, not just recall).
2. FIX2 — `gate_prediction.requires` diverges from the real gate on a **bridge-only hub**
   specifically: `compute_touch_risk` alone returns only `(risk, hub_hit, hub_kind,
   uncertain_zero_caller, touched)` — it does NOT compute `bridge_downgrade_eligible`
   (edit.rs:860-870, needs `all_caller_edges_confident`, a second query over `touched`'s
   hub qualified_names). Calling only `compute_touch_risk` as the plan's step 2 literally
   describes would make `gate_prediction` classify every hub touch as requiring the full
   `edit_context+confirm+grounded_reason` tier even when the real gate would accept a bare
   `confirm:true` — HIGH (a real prediction-vs-gate mismatch, i.e. exactly the F2 class of bug
   this fix exists to close, reintroduced in a narrower spot) — mitigation in plan: NO as
   written; REQUIRED CHANGE: `edit_context` must also compute `bridge_downgrade_eligible` by
   replicating edit.rs:860-870 (needs `all_caller_edges_confident` raised to `pub(crate)`
   alongside `compute_touch_risk`), and `classify_gate` must take it as a 5th input.
3. FIX3/F4 — multi-hunk file: the `hunks.iter().zip(&fd.removed_line_text)` pairing is
   positional, not keyed — any future change to how `FileDiff` is built that adds/reorders
   hunks without keeping `removed_line_text` index-aligned silently breaks this check with no
   type-level guard — LOW (no such change is in scope here) — mitigation in plan: NO explicit
   guard, but add a unit test asserting the invariant (`hunks.len() == removed_line_text.len()`)
   so a future regression fails loudly instead of silently misattributing removals.

### Layer Signals
- L1 Logic: FIX1's sentinel-survival argument (`current` is a stack-local, cloned not shared,
  so a named-function reassignment in one branch can't leak into a sibling branch) verified
  correct by reading `walk_calls`' actual recursion (line 1537 `let mut current = enclosing`,
  line 1706 `current.clone()` passed to each child) — no signal, design is sound.
- L2 Concurrency: `extract_file_data` runs per-file in parallel (own docstring confirms) but
  the `<module>` sentinel is per-call-stack, not shared mutable state — no signal.
- L3 Data: checked `call_edges` schema (parity_test.rs's `build_synthetic_db`) — `from_symbol
  TEXT NOT NULL`, no FOREIGN KEY to `symbols.qualified_name` — the plan's core safety claim
  ("pseudo qn used only as a string, never inserted into `symbols`") cannot violate a DB
  constraint that doesn't exist. Confirmed safe, not just assumed.
- L4 Integration: n/a, no external API.
- L5 Security: n/a, no auth/privilege surface touched by any of the three fixes.
- L6 Observability: FIX2's `gate_prediction` is the only one that changes an agent-facing
  contract; the plan's own parity test (§FIX2 Tests) is the detection mechanism — no gap.
- L7 Cross-cutting: checked the FIX1 language gate for an extension gap (tsx/jsx mapping to a
  different `language` string than `"typescript"`/`"javascript"`, which would silently exclude
  them from `matches!(language, "javascript" | "typescript")`) — verified via
  `lang_constants.rs`: `jsx`/`mjs`/`cjs` all map to `language: "javascript"`, `tsx` maps to
  `language: "typescript"`. No gap; ABDUCTIVE-2 below is refuted by evidence, not just assumed.

### Assumptions to Verify
- ASSUMED (plan FIX1 §"Why this surfaces end-to-end"): `resolve_sites_to_edges`' same-file
  preference is enough to keep module-level fan-out (`app.use(x)`) from flooding `ambiguous`
  edges at a materially different rate than existing same-name calls already do. Stated as
  established behavior in the plan; not independently re-verified against current
  `resolve_sites_to_edges` code in this audit — verify via the mandated resolution/ re-run's
  precision numbers, not by inspection alone.
- ASSUMED (plan FIX2 §Tests): the `OwnerController`/`initFindForm` spring-petclinic fixture
  still produces a bridge/degree hub shape matching the described test — fixtures drift;
  confirm the fixture's current hub classification before writing the assertion, don't assume
  the audit-era snapshot still holds.

### Abductive Hypotheses
1. **Combined-effect interaction**: FIX1 (more JS/TS call edges → some symbols' `caller_count`
   crosses the hub threshold) lands together with FIX2 (gate_prediction now accurately mirrors
   the real gate, including hub_hit). Individually each is correct; combined, a JS/TS symbol
   that was previously safely low-risk can become gate-blocked in the same PR that also made
   gate_prediction trustworthy — neither fix's own test suite would catch this since each
   tests in isolation. Mitigation: run B12 + resolution AFTER both land together (per the
   plan's own global validation harness), not just after each individually, and diff the
   before/after `is_hub`/hub_count on the JS/TS fixtures specifically.
2. **tsx/jsx language-string gap** — investigated under L7 above; REFUTED, not a live risk.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
PASS WITH FLAGS — proceed to implementation. FIX2's HIGH finding (bridge_downgrade_eligible
missing from gate_prediction) is a required scope addition, not optional polish — implement it
as part of FIX2, not deferred. All other findings are covered by tests already specified in the
plan or are advisory (documentation/oracle-verification steps).
