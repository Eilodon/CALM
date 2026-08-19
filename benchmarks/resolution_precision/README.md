# Resolution Precision — WS0 (false-confidence + precision benchmark)

Implements **WS0** of
[`docs/plans/2026-08-18-context-intelligence-upgrade-plan.md`](../../docs/plans/2026-08-18-context-intelligence-upgrade-plan.md).
Every WS2–WS6 change in that plan must attach a before/after run of this benchmark; a change
merges only if `call_recall` does not drop **and** `false_confidence_rate` does not rise.

## WS7 status: SHIPPED (2026-08-19) — fixture I / D8 false-confidence eliminated, `false_confidence_rate` 0.25 → 0.0

Fixture I (`I_file_symbol_wins_over_import`, the D8 shadowing case) used to emit TWO edges from one
call site: `caller.py::name@resolved` (correct — tier-1 file-symbol-over-import priority) AND
`external.py::name@formal` (WRONG). **Root-caused live (2026-08-19), correcting this fixture's own
oracle note:** the wrong edge is inserted by the *SCIP overlay* (`formal_source = 'scip'`,
`crates/calm-core/src/scip/ingest.rs::insert_missing_exact_edges`), NOT the bundled stack-graphs
`formally_resolved` bare-name upgrade the plan/audit assumed. The call site's persisted confidence
is already `resolved`, so `extract_file_data`'s formal upgrade (gated on `!= Resolved`) never fires
here; scip-python follows the `from external import name` binding and reports `external.py::name`,
missing that the later same-scope `def name` shadows it.

Fix: `insert_missing_exact_edges` now skips inserting a competing `formal`/`scip` edge when the same
call site already carries a CONFIDENT STATIC edge (`resolved`, or non-scip `formal`) to a DIFFERENT
target (`conflicting_confident_static_target`), and RECORDS the disagreement in the new
`evidence_conflicts` table (+ the `scip_static_conflict_count` process counter) so it is countable,
not silently dropped — surfaced as the benchmark's `provider_conflict_rate` (WS7B, seeds the Wave 3
evidence ledger). Deliberately narrow — `ambiguous`/`textual`/
`inferred` edges stay overridable (that is the overlay's job); only a real language-rule resolution
is protected. Regression test (scip-independent, constructed directly):
`crates/calm-core/src/scip/ingest.rs::scip_does_not_add_formal_edge_conflicting_with_confident_static_resolution`.

Full corpus before → after: `false_confidence_rate` **0.25 → 0.0**, `false_confident_site_rate`
0.125 → 0.0, `call_recall` 0.875 (unchanged), no other fixture's outcome changed; fixture I went
FALSE_CONFIDENCE → RECALL_LOWCONF_CORRECT. The eliminated false-confidence now shows up honestly as
`provider_conflict_count` 1 / `provider_conflict_rate` 0.125 — the same site, reclassified from
"confidently wrong" to "measured provider/static disagreement."

The separate stack-graphs `formally_resolved` bare-name upgrade (`pipeline.rs` — upgrades a site to
`formal` when stack-graphs proved *any* same-named reference resolves in the file) is a real but
currently-undemonstrated latent false-confidence risk; tracked as a follow-up, not fixed
speculatively without a failing fixture.

## WS2 status: SHIPPED (2026-08-18) — verify via unit test, not this corpus

`import_path` now threads end-to-end (`resolve_tier1` → `CallSiteData` → `call_sites` column →
`CallSiteRow` → `resolve_sites_to_edges`'s new import-binding narrowing pass, checked before
`module_hint`). Full `cargo test` green (calm-core 1238/1238, calm-server 397/397) including a new
dedicated regression, `test_import_path_narrows_candidates_past_max_callee_candidates`
(`crates/calm-core/src/indexer/pipeline.rs`): 22 decoy `bar()` defs + 1 real
`from lib import bar` binding — asserts both the capture (`call_sites.import_path = 'lib'`) and the
narrowed edge target (`lib.py::bar`, not a decoy, not dropped).

**This benchmark's own numbers are unchanged before → after WS2** (verified: identical
`false_confidence_rate`/`call_recall`/per-fixture outcomes on a full re-run) — not a null result,
an expected one. Finding 4 above already established fixture A is masked by the bundled
stack-graphs overlay (which saves the import-narrowing case independent of the static resolver),
and fixture B has no import at all, so the static-only shape WS2 fixes isn't isolated by any of
the 12 fixtures — every fixture here runs through `calm index`'s full pipeline, overlay included,
so it can't tell "the static resolver fixed it" apart from "the overlay saved it anyway." Only a
direct unit test at the `resolve_sites_to_edges`/`extract_file_data` level (bypassing the overlay
entirely) can isolate this specific defect — which is what the new test above does. Documented as
a permanent limitation of this harness's design in "Known limitations" below, rather than building
a 13th fixture that would need a language with neither stack-graphs coverage nor a bare-import
concept to test cleanly (Python/JS/TS/Rust — the only languages with `resolve_tier1`'s bare
import-map narrowing at all — all have stack-graphs coverage on any dev machine with the relevant
tools installed).

## WS5 status: SHIPPED (2026-08-19) — fixture E now recalls the true target, `call_recall` 0.75 → 0.875

`call_edges` gains a `candidate_rank INTEGER NOT NULL DEFAULT 0` column (`0` = preferred/same-dir
match, `1+` = alternate, ordinal not a score) — the minimal persistence primitive the post-WS2
review flagged as non-optional: without it, "non-destructive ranking" has no way to mean anything
more than either re-collapsing every candidate to undifferentiated `ambiguous` or silently reverting
to the old destructive-filter behavior at a lower confidence label.

**Scoping correction, found DURING implementation, not assumed from the plan's prose:** the plan's
own D5 root-cause text calls directory "a convention, not a scoping rule" for "Java/C/C++." That's
only true for C/C++. Attempting the ranker relaxation for Go and Java broke two existing regression
tests (`test_go_same_directory_call_resolves_not_fanned_out`,
`test_java_same_package_call_resolves_not_fanned_out`) — both correctly encode REAL, compiler-
enforced scoping: an unqualified Go call can only ever resolve within its own package, and an
unqualified/non-imported Java type reference can only resolve within its own package — a same-
directory match here isn't a heuristic preference, it's a structural certainty the identical-looking
out-of-directory candidate cannot be the real target. C/C++ have no package/namespace-to-directory
correspondence at all, so this plan's own flagship WS5 fixture
(`E_same_dir_decoy_vs_true_target`) is, correctly, a C fixture, not Java as the plan's test-plan
prose loosely suggested. **The shipped scoping: only C/C++ get the non-destructive ranker; Go/Java
keep the pre-WS5 hard filter, unchanged, backed by their own still-passing regression tests.**

`same_dir()`'s C/C++ branch now keeps the FULL surviving candidate set (not just the same-directory
subset) whenever a real out-of-directory alternate exists, tagging the same-directory subset
`candidate_rank = 0`. No new confidence rule was needed: the existing `targets.len() > 1 =>
Ambiguous` rule in the second loop already downgrades this correctly the moment a real alternate
survives instead of being dropped. `callers()`/`callees()` now select `candidate_rank` and stable-
sort the `ambiguous` list by it, so a same-directory match still surfaces first without claiming to
be the sole resolved target.

Full `cargo test` green (calm-core 1240/1240 — the pre-existing
`test_c_same_directory_call_resolves_not_fanned_out` was intentionally rewritten to
`test_c_same_directory_call_ranks_local_first_but_keeps_sibling_candidate`, asserting the new,
correct behavior with a clear comment explaining why the old assertion no longer holds for C; calm-
server 399/399, +1 new `callers_sorts_ambiguous_by_candidate_rank`).

Fixture E (`E_same_dir_decoy_vs_true_target`, this plan's own WS5 flagship) moved from
`WRONG_LOWCONF` (the ONLY edge pointed at the wrong decoy, true target completely absent) to
`RECALL_LOWCONF_CORRECT` (both `dir1/decoy.c::foo@ambiguous` and `dir2/real.c::foo@ambiguous`
present — the true target recovered, not silently dropped). `call_recall` improved **0.75 → 0.875**
— a real, measurable recall gain — with `false_confidence_rate`/`false_confident_site_rate`
unchanged (0.25 / 0.125): exactly the "precision gain, not a recall loss" the plan's own WS5
acceptance criterion requires, verified live rather than assumed.

## WS4 status: SHIPPED (2026-08-19) — fixture C now `inferred`, not `ambiguous`

`build_resolution_context` now also builds an inheritance/interface closure from `type_relations`
(`build_inheritance_closure`): class/interface BARE name (matching `target_class`/`by_name_class`'s
own keying) → transitive `extends`/`implements` ancestors, grouped by depth, cycle-safe, bounded to
`MAX_INHERITANCE_DEPTH` (12) hops. **Hard gate, non-negotiable:** only `type_relations` rows whose
own `confidence` is `resolved` ever feed it — never `textual` — matching the post-WS2-review
correction to this plan. `resolve_cross_file_type_relations` (previously called near the END of
`rebuild_graph`/`incremental_graph_update`, well AFTER `resolve_sites_to_edges` had already run) was
moved to run BEFORE `build_resolution_context` in both functions — verified safe: that pass only
ever reads `symbols`/`type_relations`, never `call_edges`/`ctx`, so moving *when* it runs changes no
other pass's behavior, but was required for the closure to ever see current-pass data instead of
one-pass-stale `to_symbol`/`confidence` values.

In the `target_class` branch, an exact `(callee, cls)` `by_name_class` miss now tries
`resolve_via_inheritance_closure` — nearest-declaring-ancestor-first, **level-aware** (not a flat
closest-first list) — before falling through to the old unscoped `by_name` fallback. Level-awareness
matters: an early flat-list version of this (caught before shipping, not in a released build) would
have silently picked whichever of two SAME-DEPTH ancestors happened to come first in source order
when both declared the same method name (e.g. `interface Mixed extends IA, IB` both declaring
`foo()`) — a genuine tie, not a confident resolution. The shipped version unions every match at each
depth level and only trusts a **singleton** union; a tie at the nearest declaring level correctly
falls through unchanged to the old ambiguous-fan-out behavior, never forces a pick.

Full `cargo test` green (calm-core 1240/1240, calm-server 398/398) including two new regressions:
`test_java_formal_parameter_resolves_inherited_superclass_method` (strengthened — this is the plan's
own cited 18/8-investigation fixture; now additionally asserts `edge_confidence` is `resolved` or
`inferred`, never `ambiguous`) and `test_java_tied_ancestors_at_same_depth_do_not_force_a_pick` (new
— the plan's second WS4 test-plan bullet: two same-depth ancestors both declaring the method must
survive as two separate `ambiguous` candidates, not collapse to one confident wrong pick).

Fixture C (`C_inherited_method_ancestor`) moved from `Base.java::Base::foo@ambiguous` to
`Base.java::Base::foo@inferred` — the WS4 acceptance criterion, live on this benchmark's own
flagship fixture. Fixture D (`D_interface_polymorphic_dispatch`, multi-implementor interface
dispatch) is unchanged, as designed: `Handler` itself declares `handle()`, so that call resolves via
the ordinary exact `by_name_class` match and never reaches the new closure code at all — WS4
deliberately defers the "legitimate polymorphism" case (the plan's own future `polymorphic`
`EdgeConfidence` variant) to a follow-up, not this PR. `false_confidence_rate`/`call_recall`
unchanged (0.25 / 0.75) — see the rerun section below for the full before/after table.

## WS3 status: SHIPPED (2026-08-19) — fixture B now MISSING_RECORDED_AMBIGUOUS, not silent

`resolve_sites_to_edges` now returns `(Vec<CallEdge>, Vec<AmbiguityGroup>)`: the branch that used to
drop an over-`MAX_CALLEE_CANDIDATES` site to `Vec::new()` with no trace anywhere now also records
one `ambiguity_groups` row (`call_site_id`, `from_path`, `candidate_group_key` = the raw callee
name, `candidate_count`, `reason`). `rebuild_graph`/`incremental_graph_update` persist it
(full-sweep DELETE+insert / `from_path`-scoped DELETE+insert, mirroring `call_edges`'s own scoping).
`callers()` gains `unresolved_group_count` (row count keyed on the resolved symbol's bare name) and
a dedicated caveat (`Caveat::unresolved_ambiguity_groups`) that fires even when `direct_count > 0` —
unlike the generic zero-usage caveats, this one is not conditioned on an otherwise-empty result, since
these sites are invisible to `direct`/`ambiguous` regardless of how many confirmed callers exist
elsewhere. `reference_impact()` gains the sibling `unresolved_many_count`. Full `cargo test` green
(calm-core 1239/1239, calm-server 398/398) including two new regressions:
`test_overflow_candidates_recorded_as_ambiguity_group_not_dropped_silently` (calm-core, the exact
25-decoy shape this plan's own test spec calls for) and `callers_reports_unresolved_ambiguity_groups`
(calm-server, asserts the caveat fires alongside a nonzero `direct_count`).

This benchmark itself was extended to consume the new signal: `query_ambiguity_group` joins
`ambiguity_groups` through `call_sites` on `(from_path, call_line)`, and `classify()` gained a third
kind of "zero edges" outcome — `MISSING_RECORDED_AMBIGUOUS` — distinct from both `MISSING_SILENT`
(nothing exists) and `MISSING_RECALL` (a real target exists, genuinely no trace). Fixture B, this
plan's flagship WS3 case, moved from `MISSING_RECALL` to `MISSING_RECORDED_AMBIGUOUS` with this run
— still not a recall hit (the true `call_edges` target is still absent, by design: WS3 explicitly
does not materialize the 25-candidate list as edges), but no longer indistinguishable from silence
either in this benchmark's own classification or in `callers()`'s real response shape.

## Why this exists, and why it isn't `benchmarks/resolution/` or B12/B15

- [`benchmarks/resolution/`](../resolution/) measures tier **distribution** on real OSS repos —
  explicitly "không có oracle" (no ground truth) per its own README. Can't measure correctness.
- [`benchmarks/b12_tier1_tier2_tool_correctness/ground_truth.py`](../b12_tier1_tier2_tool_correctness/ground_truth.py)
  (which B15 also reuses) measures file-**recall** on real OSS repos via `git grep`, but its own
  `unique_definitions()` deliberately samples only **globally-unique** names — "does tool X find
  calls to THIS exact one" isn't well-posed otherwise (see its docstring). That's the right call for
  a recall benchmark, but it structurally **excludes** every confusable/same-named shape this
  benchmark exists to test.
- Neither can measure **false_confidence_rate**: an edge CALM labels `formal`/`resolved` (its own
  two highest-confidence tiers) whose target is actually wrong. B15's own README already names this
  gap explicitly — Context+'s `get_blast_radius` scores 100% file-recall via plain substring
  matching because that benchmark's scoring never penalizes a confidently-wrong claim.

So: a small, hand-authored fixture corpus where the correct target is known **by construction**,
each fixture mapped to one specific resolver code path (cited by `pipeline.rs` line number in its
`oracle.json`). Each fixture is indexed as its own standalone `--project-root` — never merged into
this repo's own CALM index (same isolation pattern as `benchmarks/resolution/`'s cloned corpora).

**Read `fixtures/*/oracle.json` before citing any number below** — same discipline B15's README
established. Every oracle's `note` field records the exact empirical behavior observed, not an
assumption.

## Metrics

| Metric | Meaning |
|---|---|
| `false_confidence_rate` | **Edge-level, the headline metric.** Of every confident-tier (`formal`/`resolved`) edge emitted across the whole gating corpus, fraction whose target is wrong — an `ambiguous`/`textual` miss is cheap; a confidently-wrong edge is the expensive failure (V3 law: don't collapse evidence into one score). |
| `false_confident_site_rate` | **Site-level** (added post-WS2 review). Of every gating call site (not edge), fraction carrying *at least one* confidently-wrong edge — denominator is all gating sites, not just sites that emitted a confident edge. Can diverge from the edge-level rate: "1 site → 4 wrong edges" vs. "4 sites → 1 wrong edge each" are the same edge-level number but very different blast radius. |
| `unique_resolution_coverage` | Design-scaffolded only (added post-WS2 review). At sites CALM resolves to exactly one confident-tier target, fraction where that target is correct. Real ingredients collected every run; reported as `insufficient_sample_size` until the corpus has ≥50 uniquely-resolved gating sites (today: 2) — this 12-fixture corpus is correctness-per-fixture, not volume, so it can't bin a real precision curve yet. |
| `call_recall` | Of gating fixtures, fraction where the real target was found at *any* confidence (including as one candidate among several). |
| `missing_recorded_ambiguous_count` | (WS3) Count of gating sites with zero `call_edges` that ARE present in `ambiguity_groups` — recorded-but-unresolved, not silent. |
| per-fixture `outcome` | One of `MISSING_SILENT` (correctly found nothing — no real target exists), `MISSING_RECALL` (a real target exists, zero edges emitted, and no `ambiguity_groups` row either — genuinely silent), `MISSING_RECORDED_AMBIGUOUS` (WS3: zero edges, but the site IS recorded in `ambiguity_groups` — not silent, just not resolved), `FALSE_CONFIDENCE` (a formal/resolved edge targets the wrong symbol), `RECALL_LOWCONF_CORRECT` (right target found, just not confidently), `WRONG_LOWCONF` (wrong target only, at low confidence — a recall miss, not a confidence miss), `BLOCKED` (needs a non-default build feature this baseline's binary lacks). |

Fixtures tagged `informational`/`informational_must_stay_honest`/`blocked_missing_build_feature`
in their `gate` field are excluded from the aggregate metrics — they document known, accepted
limitations (reflection, interface dispatch, missing build feature), not resolver defects.

## Run

```bash
cargo build --release -p calm-cli   # produces target/release/calm
python3 benchmarks/resolution_precision/run_benchmark.py
```

`--fixture <name>` runs a single fixture. Each run re-indexes fresh (deletes any stale
`.calm/index.db`); nothing here is committed to git except the fixture sources and oracles
themselves — `.calm/` directories are gitignored (see repo `.gitignore`).

## Baseline (2026-08-18, `calm` @ `5b61d55` + `0f1dd1c` + `62b4df2`)

| fixture | category | workstream | outcome | emitted |
|---|---|---|---|---|
| A_import_narrows_fanout | import_narrowing | WS2 (informational — see below) | RECALL_LOWCONF_CORRECT | `lib.py::bar@formal` |
| B_fanout_over_cap_dropped | fanout_over_cap | **WS3** | **MISSING_RECALL** | *(none)* |
| C_inherited_method_ancestor | inheritance_closure | **WS4** | RECALL_LOWCONF_CORRECT (confidence too low) | `Base.java::Base::foo@ambiguous` |
| D_interface_polymorphic_dispatch | polymorphic_dispatch | WS4-extension (deferred, informational) | WRONG_LOWCONF | `Handler.java::Handler::handle@inferred` |
| E_same_dir_decoy_vs_true_target | same_dir_destructive_narrowing | **WS5** | **WRONG_LOWCONF** | `dir1/decoy.c::foo@textual` |
| F_weak_receiver_no_false_positive | regression_sanity | guard | RECALL_LOWCONF_CORRECT | `lib.rs::get@ambiguous` |
| G_cross_language_no_collision | regression_sanity | guard | RECALL_LOWCONF_CORRECT | `lib.py::foo@formal` |
| H_module_qualifier_wins | regression_sanity | guard | RECALL_LOWCONF_CORRECT | `src/telemetry.rs::write@resolved` |
| I_file_symbol_wins_over_import | shadowing_false_confidence | **NEW FINDING (D8)** | **FALSE_CONFIDENCE** | `caller.py::name@resolved` (correct) **+** `external.py::name@formal` (wrong) |
| J_reflection_getattr_invisible | known_limitation | none (informational) | MISSING_SILENT (correct — honest) | *(none)* |
| K_overload_arity_elixir | arity_gate | guard | BLOCKED | needs `--features lang-elixir` rebuild |
| L_css_false_positive_floor | Context+ comparison | none (comparison) | RECALL_LOWCONF_CORRECT | `PetTypeFormatter.java::PetTypeFormatter::print@inferred` |

```json
{
  "total_fixtures": 12,
  "gating_fixtures": 8,
  "false_confidence_count": 1,
  "confident_edge_count": 3,
  "false_confidence_rate": 0.333,
  "call_recall": 0.75,
  "blocked": ["K_overload_arity_elixir"]
}
```

Reproduced across 2 consecutive full-batch runs and 3 consecutive isolated runs of fixture A
(2026-08-18) — see "A flake worth knowing about" below for the one exception.

## Rerun (2026-08-19, post-WS2 + WS3 + WS4 + WS5, `calm` @ working tree)

Fixtures B, C, and E change; every other outcome is byte-identical to the 2026-08-18 baseline above
(WS3 only touches the >20-candidate drop path; WS4 only touches the inheritance-closure fallback;
WS5 only touches the C/C++ `same_dir` branch; WS2's import narrowing was already reflected in the
baseline's fixture A). `confident_edge_count`/`false_confidence_rate` are now computed edge-level
(see the WS0 metrics-table revision above), which is why the raw numbers differ slightly from the
baseline block even though nothing about fixtures A/D/F–L actually changed — the *fixture-level*
false-confidence classification (1 fixture in FALSE_CONFIDENCE, fixture I) is identical.

| fixture | outcome (was → now) | emitted (was → now) |
|---|---|---|
| B_fanout_over_cap_dropped | `MISSING_RECALL` → **`MISSING_RECORDED_AMBIGUOUS`** | `(none)` → `(none, but ambiguity_groups: helper x25)` |
| C_inherited_method_ancestor | `RECALL_LOWCONF_CORRECT` (unchanged) | `Base.java::Base::foo@ambiguous` → **`Base.java::Base::foo@inferred`** |
| E_same_dir_decoy_vs_true_target | `WRONG_LOWCONF` → **`RECALL_LOWCONF_CORRECT`** | `dir1/decoy.c::foo@textual` → **`dir1/decoy.c::foo@ambiguous; dir2/real.c::foo@ambiguous`** |

```json
{
  "total_fixtures": 12,
  "gating_fixtures": 8,
  "false_confidence_count": 1,
  "confident_edge_count": 4,
  "false_confidence_rate": 0.25,
  "false_confident_site_count": 1,
  "false_confident_site_rate": 0.125,
  "unique_resolution_coverage": "insufficient_sample_size (2 uniquely-resolved gating sites, need >=50 to bin a precision curve)",
  "call_recall": 0.875,
  "missing_recorded_ambiguous_count": 1,
  "missing_silent_count": 0,
  "blocked": ["K_overload_arity_elixir"]
}
```

`call_recall` improved **0.75 → 0.875** — WS5's fixture E recovering the previously-lost true
target, entirely attributable to that one fixture (WS3 deliberately does not create a `call_edges`
row for the overflow case, see the "WS3 status" section above, so fixture B contributes no recall
change). `false_confidence_rate`/`false_confident_site_rate` unchanged (0.25 / 0.125) — a clean
recall gain with no precision cost, exactly the WS5 acceptance bar. Directly verified against
fixture B's own `.calm/index.db`: `SELECT * FROM ambiguity_groups` returns exactly `(1, 1,
'caller.py', 'helper', 25, 'unscoped_candidates_exceeded_max_callee_candidates')`.

## Findings

### 1. D3 (fanout drop) is real — **fixed by WS3** (2026-08-19) — fixture B

25 same-named free functions, **no import**, bare call. `call_sites.confidence` is set to
`textual` at extraction time; `call_edges` is still (deliberately) empty for this site — WS3 never
materializes the 25-candidate list as edges, that would just trade one failure mode for another
(false confidence via mass fan-out). What changed: the site no longer vanishes with zero trace.
`resolve_sites_to_edges` now also returns `Vec<AmbiguityGroup>`, and the branch that used to hit
`Vec::new()` at what was [pipeline.rs:1483](../../crates/calm-core/src/indexer/pipeline.rs#L1483)
(no import binding for the formal overlay to exploit — see finding 3 below for the case where an
import DOES save it) now additionally records `{call_site_id, from_path: "caller.py",
candidate_group_key: "helper", candidate_count: 25, reason:
"unscoped_candidates_exceeded_max_callee_candidates"}` in the new `ambiguity_groups` table.
`callers(symbol="helper")` now reports `unresolved_group_count: 1` plus a caveat naming the 25-way
ambiguity, instead of a clean, misleadingly-confident empty result. See "WS3 status" above for the
full change and its two new regression tests.

### 2. D5 (same-dir destructive narrowing) was real — **fixed by WS5** (2026-08-19) — fixture E

True target in a sibling directory (`dir2/real.c::foo`); a same-directory decoy
(`dir1/decoy.c::foo`) sits next to the caller. `call_edges` used to emit **exactly one** edge — to
the **wrong** decoy, at `textual` confidence, with the true target **completely absent**, not even
as a co-candidate — confirming `same_dir()`'s replace-not-rank behavior at what was
[pipeline.rs:1468](../../crates/calm-core/src/indexer/pipeline.rs#L1468). WS5 changed `same_dir`'s
C/C++ branch (scoped to C/C++ only — see "WS5 status" above for why Go/Java keep the old hard-filter
behavior, backed by real compiler-enforced package scoping their languages actually have) to keep
the FULL candidate set instead of replacing it: both `dir1/decoy.c::foo` and `dir2/real.c::foo` now
survive as `ambiguous` candidates, `candidate_rank`-sorted so the same-directory decoy still
surfaces first in `callers()`/`callees()`, but no longer at the cost of erasing the real target.

### 3. D4 (inheritance) recall was fine, confidence was wrong — **fixed by WS4** (2026-08-19) — fixture C

`Child extends Base` (no override), call via a `Child`-typed variable. The edge always correctly
targeted `Base.java::Base::foo` (the `62b4df2` fallback already worked for recall) but sat at
`edge_confidence = 'ambiguous'`, not `resolved`/`inferred` — the fallback found the right answer for
the wrong reason (unscoped by-name match, not real inheritance-graph traversal) and was marked
low-confidence accordingly. WS4 (`resolve_via_inheritance_closure`, consulting `type_relations`
`extends`/`implements` rows, gated to `confidence = 'resolved'` only) now finds `Base::foo` via the
real inheritance closure before ever reaching the unscoped fallback — same target, now `inferred`.
See "WS4 status" above for the full change and its two new regression tests.

### 4. The bundled stack-graphs overlay already masks D1/D3 for bare-name imports — fixture A

This was the most important calibration finding, and it **revised WS2's scope before WS2 shipped**.
Fixture A (22 same-named decoys + 1 real import target, same shape as B but *with* an import)
resolves **correctly** to `lib.py::bar` at `formal` both before and after WS2 — at the time this
was first written, NOT because the static resolver's `resolve_tier1` threaded `resolved_path` (it
didn't yet — see the WS2-status note at the top of this file, now shipped), but because
`calm index` runs an **in-process, bundled stack-graphs formal resolver unconditionally**, for
every stack-graphs-covered language, regardless of whether any external SCIP binary
(`scip-python`, etc.) is installed — confirmed live: this benchmark's dev machine has no
`scip-python` on `$PATH` at all (`which scip-python` → not found), yet the log still shows
`SCIP overlay (python): ... 1 edges inserted`. That overlay's gap-insert mechanism
([ingest.rs:806](../../crates/calm-core/src/scip/ingest.rs#L806),
`insert_missing_exact_edges`) fires specifically because the *static* candidate algebra drops this
site exactly like B (23 candidates > 20), and the overlay backfills the one true target from real
import-scope resolution.

**Practical implication:** D1's raw defect (`resolved_path` computed by `resolve_tier1` and
discarded in `extract_file_data`) was already masked, for THIS shape, by the overlay in every
stack-graphs-covered language (Python confirmed live; JS/TS/Rust share the same `FormalResolver`
machinery and are expected — not yet independently verified — to behave the same) — which is why
WS2 (now shipped, see the status note at the top of this file) was correctly scoped as
defense-in-depth rather than the flagship recall-recovery fix: the overlay has a bounded time
budget (`RESOLVE_TIMEOUT`/`MAX_TSG_BUILD_CANCELLATION_CHECKS`) and can time out or be disabled, and
WS2 is the *only* fix for languages with no `FormalLanguageConfig` entry at all (Java's fixture C
above shows Java gets no such rescue for method-dispatch shapes). Kept `informational` in this
corpus for that reason — its own before/after numbers can't move here (see the WS2-status note);
**WS3 (fixture B) is the sharper, still-fully-open fanout gap** and is the next target.

### 5. NEW finding, not anticipated by the original source-level analysis — fixture I

`from external import name`, then a **later same-file** `def name():` in the same module scope.
Real Python semantics: the later definition unconditionally shadows the import — calling `name()`
always invokes the local one. `call_edges` emits **two** edges from the same call site:

- `caller.py::name @ resolved` — correct (tier1's file-symbol-over-import priority rule, working
  as designed and already tested by `test_resolve_priority_file_symbol_over_import`).
- `external.py::name @ formal` — **wrong**. The bundled stack-graphs overlay independently proved
  the import binding resolves and inserted a second edge, without modeling that the later
  same-scope definition shadows it.

This is a **live, reproducible false-confidence example** at CALM's own highest confidence tier,
found empirically while building this benchmark — not predicted by the original analysis this plan
is based on. Root cause: the overlay and the static resolver's shadowing-priority rule are two
independent authorities with no reconciliation step, and the overlay is allowed to *add* an edge
the static layer already correctly excluded. **Filed as D8 — out of WS2's original scope, closer to
WS6 (overlay/static-layer reconciliation) territory or a new WS7; not fixed as part of this
benchmark's own scope, only measured and documented.**

### 6. CALM is structurally immune to Context+'s exact published false positive — fixture L

Mirrors the live Context+ bug documented in
[`benchmarks/b15_cross_lang_competitor_ab/README.md`](../b15_cross_lang_competitor_ab/README.md)
(`get_blast_radius("print", ...)` matching CSS `print-color-adjust`). CALM's call graph targets
`PetTypeFormatter.java::print` correctly and the CSS file is never a candidate **at all** — not a
resolver heuristic working correctly, but categorical: tree-sitter's call-site grammar has no
production for a CSS rule, so a `.css` file can never produce a `call_sites` row in the first
place. `reference_impact`'s own grep floor would still textually match `print` inside the CSS file
if run — but confirmed by re-reading [trace.rs:656-660](../../crates/calm-server/src/tools/trace.rs#L656),
a pure-grep hit is *always* bucketed `textual_only`, never `must_change`/`likely_change` (those
require a real `call_edge`/`import_edge`) — so CALM never over-claims confidence on it the way
Context+ does. Comparison fixture, not a resolver gate.

### 7. Elixir fixture blocked by build config, not measured

`K_overload_arity_elixir` produced **zero** `call_sites`/symbols — the release binary used for this
baseline was built with default features only; Elixir requires
`--features lang-elixir` (per `benchmarks/resolution/README.md`'s own build invocation). Documents
a baseline-run limitation, not a resolver finding. Re-run once a `lang-elixir`-enabled binary is
available.

### A flake worth knowing about

One full-batch run (out of 3 total: 2 full-batch + isolated re-runs of fixture A) showed fixture A
as `MISSING_RECALL` (zero edges) instead of the otherwise-consistent `RECALL_LOWCONF_CORRECT`.
Re-running fixture A in isolation immediately after was 100% consistent (3/3) with the correct
result, and a second full-batch run also matched. Root cause not chased down — plausibly transient
resource contention around the in-process stack-graphs overlay when 12 fixtures index in quick
succession (no external process/network dependency to blame; `scip-python` isn't even installed on
this machine). Noted here rather than hidden, same "audit the oracle, report the raw rows" posture
this repo's other benchmark READMEs (B12, B15) already established. Worth `--n-repeats 3`-style
majority-vote treatment (matching B13's own discipline) before this specific fixture's number is
treated as load-bearing in CI.

## Known limitations of this benchmark itself

- 12 fixtures is a *floor*, not a ceiling — the plan's own §56 adversarial list names more shapes
  (overload/arity in a language other than Elixir, reflection in more languages, module-qualified
  collision across packages, ...) not yet covered.
- Single-language-pair fixtures only; no fixture yet stresses the *combination* of two defects at
  once (e.g. an inherited method that's also over the `MAX_CALLEE_CANDIDATES` cap).
- No fixture yet exercises proof invalidation (an unrelated same-file edit churning an
  `external_proofs` row) — that's D-adjacent but belongs to a different, not-yet-built harness
  (WS0's scope is resolver precision, not proof-freshness).
