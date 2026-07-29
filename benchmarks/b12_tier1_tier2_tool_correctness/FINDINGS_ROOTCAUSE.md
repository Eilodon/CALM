# B12 Deep Root-Cause Audit — CALM Tier-1/Tier-2 tool findings

Method: VHEATM 16 (CODE mode, Tier 2, Full). Every root cause is anchored to real
CALM source read this session and confirmed by an independent empirical reproduction
against a freshly-indexed express/spring-petclinic DB — not inferred from the benchmark
alone (VHEATM Principle #2: "Documented ≠ Verified"; Principle #4: adversarial pass).

**Verdict on the 4 original findings:** 3 confirmed real (1 HIGH-systemic, 1 MED, 1 LOW),
**1 refuted** (F3 was a benchmark misread). Plus one secondary structural finding (F2b)
and one benchmark-methodology gap surfaced by the audit itself.

---

## F1 — HIGH / systemic. JS/TS call graph is blind to calls outside a named-function body.

**Original symptom:** `callers()` returned 0 direct AND 0 ambiguous for 3 express symbols
(`test`, `shouldNotHaveBody`, `testRequestedRedirect`) that `git grep` shows called 5/4/12×.

**Root cause (confirmed by code + DB):**
`walk_calls` ([crates/calm-core/src/indexer/parser.rs:1528](../../crates/calm-core/src/indexer/parser.rs#L1528))
only records a call when `current` (the nearest enclosing symbol) `is_some()` —
see the guard `&& let Some((enc_name, enc_line)) = &current` at
[parser.rs:1589](../../crates/calm-core/src/indexer/parser.rs#L1589). `current` is set
**only** for a node in `function_node_types` whose `resolve_name_node` yields a name
([parser.rs:1545-1552](../../crates/calm-core/src/indexer/parser.rs#L1545)). An **anonymous**
function expression / arrow (a `describe`/`it`/middleware/`.then`/`.forEach` callback) has
no name, so it never updates `current`. Therefore any call whose nearest function ancestor
is an anonymous callback with **no named-function ancestor** — i.e. essentially all
module-level and top-level-callback code — is silently dropped. It never reaches the
`call_sites` table, so no edge of any confidence is ever created.

**Empirical proof (fresh express index, `.calm/index.db`):**
- `test/res.format.js`: the only indexed symbol is `test` (fn, L182-248). All 47 captured
  call sites fall in L183-241 (inside `test`'s body). The 5 real `test(app…)` calls at
  L90-177 (inside module-level `it()` callbacks) produced **0** call sites.
- Corpus-wide: **every** one of the 695 call sites has `enclosing_qn` of kind `function`;
  **zero** are module-level or attributed to a non-symbol scope
  (`SELECT COUNT(*) … WHERE enclosing_qn NOT IN symbols → 0`).
- The whole express repo indexed to **249 symbols / 695 call sites** — implausibly low,
  because the test suite (the bulk of the code) is anonymous `describe`/`it` callbacks that
  are neither indexed as symbols nor walked for calls.

**Blast radius:** language-agnostic in principle (the guard is generic) but JS/TS are hit
hardest by far — module-level code and anonymous callbacks are idiomatic there (mocha/jest,
express middleware, promise chains, array iterators). This is a **major, previously
unattributed contributor to the JS 3.5% / TS 4.1% call-graph recall** recorded in the
2026-07-28 6-lang benchmark — distinct from the B1 property-assigned-function *symbol*
gap already fixed; B1 was about `to_symbol` extraction, this is about `from_symbol`
(enclosing) attribution. Affects `callers`, `callees`, `blast_radius`, `caller_count`,
and every hub/coreness signal derived from the call graph, for JS/TS.

**Fix direction (not applied):** attribute a call to the nearest enclosing *indexed* scope
even when that scope is anonymous — e.g. synthesize a stable enclosing id for a top-level
callback (by its host call + position), or fall back to a file/module-level pseudo-symbol
so module-level calls are recorded rather than dropped. Any fix must avoid re-introducing
the phantom-self-call class the Elixir `is_definition_macro_call` guard already handles.

---

## F2 — MED. `edit_context` omits the exact fields the write-gate blocks on.

**Original symptom:** the pre-edit tool's `risk_assessment.level` doesn't predict whether
`edit_lines`/`edit_symbol` will be blocked; error messages reference `is_hub=true` but
`edit_context` never returns `is_hub`.

**Root cause (confirmed by code):** `EditContextOutput`, constructed at
[guardrails.rs:346-367](../../crates/calm-server/src/tools/guardrails.rs#L346), has **no
`is_hub` field** — even though the resolved candidate `c.is_hub` is in hand and used one
line earlier for `related_notes` ([guardrails.rs:200](../../crates/calm-server/src/tools/guardrails.rs#L200)).
The write-gate blocks on `hub_hit`, which `compute_touch_risk` derives as
`hub_hit |= row.is_hub` from the **same** `symbols.is_hub` column
([edit.rs:1682](../../crates/calm-server/src/tools/edit.rs#L1682)). Meanwhile
`edit_context`'s `risk_assessment.level` is computed from
`risk_level_from_caller_count(confirmed_caller_count)` plus dead-code/entropy escalation
([guardrails.rs:219-295](../../crates/calm-server/src/tools/guardrails.rs#L219)) — a
**different** quantity than `hub_hit`. So a symbol can be `level:"medium"` yet a hub
(gate fires), or `level:"high"` yet not a hub. Verified live: on Go a `level:"high"` symbol
was `is_hub:false` and edited with no confirm; on Java a `level:"medium"` symbol was a hub
and hard-required `edit_context`+`confirm`.

**Fix direction:** surface `is_hub` (and ideally `hub_kind` + the `uncertain_zero_caller`
classification) on `EditContextOutput`, or a single `will_require_confirm` boolean the gate
and the tool both compute from one source.

### F2b — secondary, structural (found during F2). Symbol-scope vs range-scope mismatch.

Even exposing `is_hub` won't fully close the gap: the gate runs `compute_touch_risk` over
**every symbol overlapping the edit's line range** (`symbols_overlapping_ranges`), including
the **enclosing class**, whereas `edit_context` is called per **symbol**. Reproduced live:
`edit_context("initFindForm")` returns `level:"medium"`, but editing that method's line is
blocked with `EDIT_CONTEXT_REQUIRED` naming the enclosing **`OwnerController`** class
(a hub) — a symbol the agent was never told to review. A complete fix must reconcile the
two scopes (e.g. edit_context should report the enclosing container's hub status too).

---

## F3 — REFUTED. No Java PARSE_ERROR false positive.

**Original claim:** appending `// comment` to a Java method signature was rejected as a
syntax error. **This was a misread of my own benchmark output.** The PARSE_ERROR in the
result came from the benchmark's *stale-reuse probe* (r2), which replaces the entire
method-signature line `public String initFindForm() {` with the bare word
`SHOULD_NOT_APPLY_1` — that genuinely breaks Java syntax (orphaned `return`, unbalanced
braces), so `validate_syntax_diff` correctly returns `Some(false)`.

**Empirical proof (fresh spring-petclinic, real MCP calls):**
- Append `// b12-probe` to `initFindForm`'s line → reaches the gate (EDIT_CONTEXT_REQUIRED),
  which is emitted *after* validation ([edit.rs:787 vs 886](../../crates/calm-server/src/tools/edit.rs#L787))
  ⇒ **validation passed** (append is clean).
- Replace the signature line with a bare word → **PARSE_ERROR**, correct.

`validate_syntax_diff` ([edit.rs:459](../../crates/calm-core/src/edit.rs#L459)) only rejects
when the edit **strictly increases** tree-sitter error nodes vs the original — it already
tolerates pre-existing grammar-coverage gaps. Working as designed. No fix needed.

*(Benchmark-methodology note, not a CALM bug: when r1 is blocked by the gate and never
applies, the `old_text`-mode stale-reuse probe (r2) still sees the unmodified line, so its
"rejection" can be caused by syntax invalidity rather than staleness detection — the Java
row's `stale_reuse_rejected:true` was a misleading pass. Worth hardening B12's staleness
assertion to require r1 actually applied first.)*

---

## F4 — LOW. `diff_impact` mislabels an in-place-edited zero-caller symbol as new.

**Original symptom:** a comment-only append to a function's signature line made
`diff_impact` report `symbol_is_new: true` (while correctly `signature_changed: false`).

**Root cause (confirmed by code + two real-diff observations):**
`is_new_symbol` ([crates/calm-core/src/analysis/diff_impact.rs:324-338](../../crates/calm-core/src/analysis/diff_impact.rs#L324)):
after the `caller_count > 0 → false` guard, it returns true when **every** line of the
signature range is in `added_lines`. A unified diff represents an **in-place** single-line
edit (comment append, reformat, rename) as remove-old + add-new, so the edited signature
line *is* an added line — indistinguishable from a freshly inserted symbol. For a
single-line signature this makes the result guaranteed; the `caller_count > 0` guard only
rescues symbols that already have indexed callers, so **zero-caller symbols (test functions,
entry points, private helpers) are always vulnerable.** Confirmed on real working-tree diffs
for Python `test_child_and_parent_subdomain` and TS `_trim`. The function's own docstring
anticipates the hand-authored-diff case but misses this real-disk-diff case: the symbol
isn't "unchanged," its signature line was edited in place, yet it isn't new.

**Blast radius:** low — `signature_changed` stays correct and risk isn't escalated; the harm
is a misleading `symbol_is_new:true` + a "newly added symbol" risk reason on a pre-existing
symbol. Same diff-heuristic family as the known `signature_changed` rustfmt false-positive.

**Fix direction:** disambiguate insert-vs-modify by also consulting removed lines at the same
location (a pure insertion adds without a co-located removal; an in-place edit removes the old
signature line and adds the new one). `is_new_symbol` currently only receives `added_lines`.

---

## Recommended priority

1. **F1** (HIGH) — biggest accuracy lever; restores the JS/TS call graph that callers/
   callees/blast-radius/hub signals all depend on. Also the missing piece of the 07-28
   JS/TS low-recall story.
2. **F2 + F2b** (MED) — cheap correctness/UX win on the mandatory pre-edit tool; F2 is a
   few output fields, F2b needs the scope reconciliation.
3. **F4** (LOW) — small, self-contained heuristic fix.
4. **F3** — no action (refuted); optionally harden the B12 staleness assertion.
