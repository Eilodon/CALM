# CALM Context-Intelligence Upgrade Plan (V3-grounded, pragmatic)

> **Status:** WS0-WS5 shipped (uncommitted, 2026-08-19), WS6 evaluation pending · **Date:** 2026-08-18
> · **Base commit:** `5b61d55` (+ `62b4df2`, `0f1dd1c`)
> **Provenance:** derived from an external "CALM vs Context+ / V3" analysis, verified line-by-line
> against real source of **both** repos (CALM @ `5b61d55`, Context+ `ForLoopCodes/contextplus` @
> `main`). Every verifiable claim in that analysis checked out (~98%). This plan keeps only the
> changes that fix a **confirmed** defect and are worth their blast radius; it explicitly defers the
> full "V3 engine" rewrite.
> **Revision (2026-08-18, post-WS2):** a second external review of this plan itself (not the
> original analysis) flagged 4 concrete tightenings, all accepted — see §3 WS4/WS5/WS6 and §2/§4 for
> the sequencing change, and the new WS0 metrics below. Each is a correction to *this plan's own*
> reasoning, not a new defect in CALM.

---

## 0. Why this plan exists (verified findings, not assertions)

The analysis proposed an ideal "Context Intelligence Engine v3 / Evidence-Grounded Context Graph"
built from the best of CALM + Context+. We do **not** rebuild CALM into V3. Instead we adopt V3 as a
**north star** and take the incremental path that captures ~80% of its concrete precision/recall/UX
value at a small fraction of the risk.

### The six V3 laws (our design invariants)

1. **Discovery ≠ evidence** — embedding/BM25 only *finds* a symbol the user might mean.
2. **Ranking ≠ resolution** — semantic similarity / same-directory only *orders*; never promotes a
   candidate to truth.
3. **Evidence is never collapsed early** — import path, receiver type, inheritance, compiler result
   are persisted in their original form.
4. **Uncertainty survives to the API** — multiple/unknown candidates never become an empty list.
5. **Proofs attach to entity revisions, not names.**
6. **Context budget affects presentation only** — token pressure hides detail, never changes graph
   truth.

### Confirmed CALM defects this plan targets (all verified in source)

| # | Defect | Evidence | V3 law violated |
|---|---|---|---|
| D1 | `resolve_tier1` computes the import target (`bar→foo`) but only `.confidence` is kept; `resolved_path` is a **dead field** — global resolver then re-guesses among all same-named `bar`. | [conservative.rs:76](../../crates/calm-core/src/resolver/conservative.rs#L76) fills it; [pipeline.rs:673](../../crates/calm-core/src/indexer/pipeline.rs#L673) drops it; no other consumer. | Law 3 |
| D2 | `callers()`/`callees()` collapse formal+resolved+inferred+textual into one `direct_count`; only `ambiguous` is split. An agent reads `direct_count: 12` as "12 confirmed." | [trace.rs:121-124](../../crates/calm-server/src/tools/trace.rs#L121); `has_textual` is only a hint [trace.rs:150](../../crates/calm-server/src/tools/trace.rs#L150) | Law 4 |
| D3 | `>MAX_CALLEE_CANDIDATES (20)` same-name candidates → site dropped to `Vec::new()`; the call site vanishes from `callers()` entirely (unknown == nonexistent). | [pipeline.rs:20](../../crates/calm-core/src/indexer/pipeline.rs#L20), [pipeline.rs:1483](../../crates/calm-core/src/indexer/pipeline.rs#L1483) | Law 4 |
| D4 | Inheritance fix (18/8, `62b4df2`) is a **recall patch**: it falls back to unscoped `by_name` marked `weak_receiver→Ambiguous`. It does **not** traverse the `type_relations` graph, so an inherited method resolves as "ambiguous" instead of to the real ancestor declaration. | [pipeline.rs:1220-1227](../../crates/calm-core/src/indexer/pipeline.rs#L1220) | Law 2 |
| D5 | `same_dir` (convention-only for Java/C/C++) narrows **destructively**: once it matches, the true target in another directory is removed from the candidate set, even though `same_dir` is a soft signal. | [pipeline.rs:1468](../../crates/calm-core/src/indexer/pipeline.rs#L1468) | Law 2 |
| D6 | `FormalEdge{reference, definition}` from StackGraph is collapsed to a `HashSet<String>` of reference names; the proven `definition_symbol` is discarded, so it can only bump confidence by name, not pick the target. | [pipeline.rs:590](../../crates/calm-core/src/indexer/pipeline.rs#L590), [pipeline.rs:747](../../crates/calm-core/src/indexer/pipeline.rs#L747) | Law 3, Law 5 |
| D7 | Benchmarks (B15) measure **file-recall only**; a wrong-but-`verified` edge costs nothing, so tools that over-claim look identical to precise ones. Context+ scores 100% recall via pure substring match. | [benchmarks/b15…/README.md](../../benchmarks/b15_cross_lang_competitor_ab/README.md) | measures Laws 1-4 |

### What CALM already does well (do NOT touch)

- **SCIP overlay** is excellent: byte-span + whole-file-hash identity, `graph_generation` staleness
  guard, three constrained authorities (confirm / rule-out / insert-missing), and a rich
  `external_proofs` table already storing `definition_snapshot` + `call_site_identity_version`
  ([ingest.rs:452-501](../../crates/calm-core/src/scip/ingest.rs#L452)). This is CALM's V3-grade
  layer; every resolver change below must keep it authoritative.
- `reference_impact` already implements the V3 "impact envelope" idea
  (`must_change`/`likely_change`/`review`/`textual_only`) ([trace.rs:656-660](../../crates/calm-server/src/tools/trace.rs#L656)).
- Memory subsystem (HMAC, prompt-injection quarantine, staleness) — the analysis itself praises it.

---

## 1. Scope decision

### IN (this plan)

| WS | Name | Fixes | V3 law | Effort | Risk |
|---|---|---|---|---|---|
| **WS0** | Precision + false-confidence benchmark | D7 | measure 1-4 | S–M | Low |
| **WS1** | Confidence distribution in read APIs | D2 | 4, 6 | S | Low |
| **WS2** | Preserve import evidence (`import_path`) | D1 | 3 | M | Low–Med |
| **WS3** | Ambiguity groups for >20 fan-out | D3 | 4 | M | Low–Med |
| **WS4** | Inheritance closure in resolution | D4 | 2 | M–L | Med |
| **WS5** | `same_dir` → non-destructive ranker | D5 | 2 | M | Med |
| **WS6** | Use StackGraph `definition_symbol` (conditional) | D6 | 3,5 | M–L | Med |

### DEFERRED — north star, documented, not scheduled

- **Evidence lattice** (two tables `edge_candidates` + `edge_evidence`, derived verdict instead of
  a stored `confidence` scalar). Intellectually correct (the `EdgeConfidence` enum genuinely
  conflates cardinality × evidence-strength × freshness), but a very large migration whose marginal
  value over WS1-WS6 is unclear. Revisit only after WS0 data shows WS1-WS6 have plateaued.
- **Finer proof invalidation** (structural `CallSiteKey`/`CallSiteRevision` split from
  `FileRevision`). Current whole-file-hash lookup is *churny but safe* (over-invalidates, never
  trusts a stale proof). Pure recompute-cost optimization; low priority.
- **HNSW/ANN backend, progressive Context Planner, 8-tool surface redesign, memory property-graph.**
  These are "V3 as a new product," not CALM fixes. CALM already has embeddings, compound tools, and
  a hardened memory store. Out of scope.

### REJECTED (from Context+, do not adopt)

- Regex/substring reference finding as a primary signal (Context+ `get_blast_radius`,
  `semantic-identifiers.ts` call detection). CALM's tree-sitter call graph is strictly better; keep
  the grep floor only where CALM already has it (`reference_impact` `textual_only` tier).
- Style opinions as write-safety gates (Context+ treats "inline comments = bad"). CALM's write
  kernel stays correctness/security-only; style is a separate configurable lint.

---

## 2. Dependency-ordered roadmap

```
WS0  ── measurement foundation (build FIRST; gates every resolver change)
 │
 ├─ WS1  (independent, ship anytime — no resolver/schema change)
 │
 ├─ WS2 ─┐
 ├─ WS3 ─┤ resolver/schema changes, each validated against WS0
 │       │
 │       ├─ WS4  (independent of WS5 — see below)
 │       └─ WS5  (independent of WS4 — see below)
 │
 └─ WS6  (conditional: only if WS0 shows StackGraph-only languages
          losing precision that SCIP doesn't already cover)
```

**WS4/WS5 dependency, corrected (post-WS2 review):** the original version of this plan sequenced
WS4 strictly before WS5, reasoning that same_dir's non-destructive change should wait until
inheritance recall is "recovered first" so it doesn't strand an inherited target. That reasoning
doesn't hold up: WS5 is purely *additive* (retains candidates instead of discarding them) — it
cannot itself strand anything that survives to the candidate-narrowing stage, regardless of whether
WS4 has already shipped. If anything, WS5 shipped *first* could immediately rescue recall for an
inherited call that currently reaches the unscoped `by_name` fallback and then gets wrongly narrowed
away by a same-directory decoy — a real scenario WS4 alone doesn't touch (WS4 only fixes the
`target_class`-keyed lookup path, not the fallback's own `same_dir` narrowing). WS4 and WS5 touch
different, largely orthogonal branches of `resolve_sites_to_edges` (the `target_class`/inheritance
lookup vs. the post-`same_file` narrowing chain). Treat them as **independent**, each validated
against WS0 on its own, and let the benchmark data — not an assumed ordering — decide which merges
first or whether they land together.

**Rule:** no WS2–WS6 change merges without a WS0 before/after run showing recall did not drop **and**
`false_confidence_rate` (and, post-review, `false_confident_site_rate`) did not rise. This is the
discipline the analysis (§55) demands.

---

## 3. Workstream detail

### WS0 — Precision + false-confidence benchmark (do first)

**Goal.** Make it impossible to ship a resolver change that trades precision for recall silently.

**Root cause.** B15 reports only file-recall (`hit/total oracle files`); a wrong `verified`/`formal`
edge is unpenalized. Context+ reaches 100% recall by substring matching — the benchmark can't tell
it apart from a precise tool.

**Change.**
- Extend `benchmarks/resolution/` (already exists; `run_benchmark.py::main` is an entry point) — do
  **not** overload B15, which is the competitor-A/B harness.
- Emit, per evidence class (`formal`/`resolved`/`inferred`/`textual`/`ambiguous`):
  - `precision` — of edges CALM emitted at this class, fraction whose target is the oracle target.
  - `call_recall` — real oracle calls captured at any class.
  - `ambiguity_recall` — real target present in the `ambiguous` candidate set.
  - **`false_confidence_rate`** — headline metric: edges labeled `resolved`/`formal` whose target is
    wrong. An `ambiguous` miss is cheap; a confidently-wrong edge is the expensive failure.
  - `formal_coverage` — fraction of sites with a compiler proof.
  - **`false_confident_site_rate`** (added post-WS2 review) — fraction of *call sites* (not edges)
    carrying at least one confidently-wrong edge. Edge-level `false_confidence_rate` alone can't
    distinguish "1 site → 4 wrong edges" from "4 sites → 1 wrong edge each" — same edge-level number,
    very different blast radius (one bad site an agent might read once vs. four separate wrong reads).
    Computed alongside `false_confidence_rate` in the same pass; both are load-bearing, neither
    substitutes for the other.
  - **`unique_resolution_coverage @ precision ≥ X`** (added post-WS2 review, design-scaffolded only —
    see note below) — at a chosen precision floor (e.g. 99.5%), what fraction of call sites CALM
    resolves to a *unique* candidate. Answers "how much of the graph can we trust at this precision
    bar" in one number — the metric that would actually decide whether the deferred evidence-lattice
    migration (§1 DEFERRED) is worth its cost. **Not fully computed by the current 12-fixture corpus**
    — a meaningful precision curve needs enough sites to bin by precision, and today's corpus is
    correctness-per-fixture, not volume. Scaffolded in `run_benchmark.py` (the per-site
    confident/wrong classification is already collected) but reported as "insufficient sample size"
    until the corpus grows past ~50+ gating sites. Revisit once WS0 runs against a real OSS corpus,
    not just hand-built fixtures.
- **Adversarial fixture corpus** (`benchmarks/resolution/fixtures/`), deliberately built to troll the
  resolver (analysis §56): same-name fn across 30 files; same-name method across classes; unknown
  receiver type; inherited method; interface dispatch; overload/arity; cross-language name collision;
  same-directory wrong candidate; imported external same-name; module-qualified fn; Rust `Self::`; JS
  module-level callback; reflection/`getattr`; string/config reference; CSS `print-color-adjust`
  (the confirmed Context+ false positive); comments containing symbol names; two identical call
  lines; line insertion before a call site; unrelated-file edit during SCIP; build-config change with
  no source change. Each fixture ships a hand-written oracle.

**Deliverable.** `benchmarks/resolution/README.md` table with the columns above + a committed
baseline `results.json` at `5b61d55`.

**Test plan.** The benchmark *is* the test. Add a CI fitness assertion later once numbers stabilize.

**Effort** S–M · **Risk** Low (no product code touched).

---

### WS1 — Confidence distribution in `callers()` / `callees()`

**Goal (V3 Law 4/6).** Never let `direct_count` read as "N confirmed callers."

**Root cause.** [trace.rs:121-124](../../crates/calm-server/src/tools/trace.rs#L121): `partition` splits
only `ambiguous`; the rest collapse into `direct_count = direct.len()`. Per-edge `edge_confidence` is
present in the `direct` array but never summarized.

**Change (purely additive).**
- Add to `CallersOutput` (and the callees equivalent):
  ```rust
  /// Breakdown of `direct` by edge confidence — `direct_count` is their sum.
  /// A high `textual` share means "found the name," not "confirmed the call."
  direct_by_confidence: ConfidenceBreakdown, // { formal, resolved, inferred, textual }
  ```
- Populate by grouping the existing `direct` vec on `edge_confidence` (no new query, no resolver
  touch). Keep `direct_count` for back-compat.
- Update `__toolsnaps__/callers.snap` + `callees.snap` and the schema doc-comments.

**Blast radius.** `CallersOutput` struct + `callers`/`callees` methods + 2 toolsnaps + tests. No
schema, no resolver.

**Test plan.** Fixture with a symbol having a mix of formal/resolved/inferred/textual callers →
assert the breakdown sums to `direct_count` and each bucket is exact. Reuse
`callers_separates_ambiguous_fanout_from_direct` shape.

**Effort** S · **Risk** Low.

---

### WS2 — Preserve import evidence end-to-end (`import_path`)

**Goal (V3 Law 3).** Stop discarding the import binding `bar→foo` and use it to narrow candidates.

**Root cause.** D1. `resolve_tier1` already returns `resolved_path: Some(module)`
([conservative.rs:76](../../crates/calm-core/src/resolver/conservative.rs#L76)); it dies at
[pipeline.rs:673](../../crates/calm-core/src/indexer/pipeline.rs#L673).

**Change — mirror `module_hint` exactly** (it is the proven template: a nullable `call_sites` column
threaded through the whole pipeline).
1. **Capture:** at [pipeline.rs:673](../../crates/calm-core/src/indexer/pipeline.rs#L673) bind the full
   `ResolveResult`, keep `resolved_path`.
2. **Carry:** add `import_path: Option<String>` to `CallSiteData` (next to `module_hint`
   [pipeline.rs:304](../../crates/calm-core/src/indexer/pipeline.rs#L304)).
3. **Persist:** add column via `migrate_add_column(conn, "call_sites", "import_path", "TEXT")`
   (pattern at [schema.rs:1002](../../crates/calm-core/src/db/schema.rs#L1002)) — additive,
   non-destructive, forward-compatible.
4. **Read back:** extend `CallSiteRow` ([pipeline.rs:105](../../crates/calm-core/src/indexer/pipeline.rs#L105))
   and the SELECT that builds it.
5. **Use:** in `resolve_sites_to_edges`, add a narrowing pass modeled on the `module_hint` block
   ([pipeline.rs:1354-1367](../../crates/calm-core/src/indexer/pipeline.rs#L1354)) — when `import_path`
   is set, prefer candidates whose file/module matches the import binding. Because this is the
   *actual* import binding (a language-level fact, not a convention), it may narrow like `module_hint`
   does (return the matched subset), and a **unique** survivor upgrades to `Resolved`.

**Hygiene note.** `CallSiteRow`/`CallSiteData` are already 15+-field tuples (clippy
`type_complexity`). Adding a field to a positional tuple is error-prone — strongly consider promoting
`CallSiteRow` to a named struct **in the same PR** (mechanical, reduces future-change risk). Optional
but recommended.

**Blast radius.** `parser`/`pipeline`/`schema` in `calm-core` + one migration. Contained to the
indexer; no server/API change. `pipeline.rs` is a hub → **mandatory `edit_context`** and likely a
`HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` gate (see §5).

**Test plan.** Fixture: `from foo import bar` + `bar()` with a second `bar` defined in `baz.py`.
Assert edge resolves to `foo.bar` (Resolved), not ambiguous fan-out to `baz.bar`. Add a negative:
no import → behavior unchanged.

**Effort** M · **Risk** Low–Med.

---

### WS3 — Record ambiguity groups instead of dropping >20-candidate sites

**Goal (V3 Law 4).** "Unknown ≠ nonexistent." A site with 143 same-named candidates should surface
as *uncertain*, not disappear.

**Root cause.** D3. [pipeline.rs:1483](../../crates/calm-core/src/indexer/pipeline.rs#L1483) returns
`Vec::new()` when `t.len() > MAX_CALLEE_CANDIDATES`, so the site produces zero edges and is invisible
to `callers()`/`reference_impact`. (SCIP can later insert the real edge — [ingest.rs:806](../../crates/calm-core/src/scip/ingest.rs#L806) — but only when SCIP is installed.)

**Change.**
- New lightweight table (via migration): `ambiguity_groups(call_site_id, candidate_group_key,
  candidate_count, reason)`. Do **not** materialize 143 edges.
- At the drop site, instead of `Vec::new()`, record `{ call_site_id, group_key: "name:foo",
  candidate_count: 143, reason: "exceeds_max_callee_candidates" }`.
- Surface it: `callers()` gains an `unresolved_group_count` / a caveat ("17 sites may target one of
  143 `foo`; not enough evidence to resolve"); `reference_impact` counts these under a new
  `unresolved_many` sub-signal (adjacent to `review`).

**Blast radius.** Indexer (record) + `trace.rs` read APIs (surface) + migration + toolsnaps.

**Test plan.** Fixture with 25 same-named free functions + 1 call → assert an `ambiguity_group` row
with `candidate_count: 25` and that `callers()` reports it rather than silence.

**Effort** M · **Risk** Low–Med.

---

### WS4 — Inheritance closure as a real resolution mechanism

**Goal (V3 Law 2).** Resolve inherited-method calls to the ancestor declaration, not to "ambiguous."

**Root cause.** D4. The 18/8 fix falls back to unscoped `by_name` marked `weak_receiver`. CALM
already extracts `type_relations` (T1 semantic facts) but `resolve_sites_to_edges` never consults it.

**Change.**
- In `build_resolution_context`, build an inheritance/interface closure map from `type_relations`:
  `class → [ancestors/interfaces...]` (bounded-depth, cycle-safe).
- **Hard gate (post-WS2 review, non-negotiable):** the closure may only traverse `type_relations` rows
  whose own `confidence` is `resolved` (or, once formal type resolution exists, `formal`) — **never**
  `textual`. `extract_file_data` already stamps each relation with its own
  `confidence`/`resolution_source` (`"resolved"`+`"same_file_ast"` for a real same-file class match,
  `"textual"`+`None` when the target text didn't resolve to any known symbol — see
  `TypeRelationData`). A `textual` relation is exactly the same kind of weak evidence
  `resolve_sites_to_edges` already refuses to trust for calls (`weak_receiver`) — using it to justify
  a hard `Resolved`/`Inferred` call edge would fix Law 2 on the call graph while silently reintroducing
  the identical violation one layer up, through the type graph. A `textual` ancestor relation may
  still *widen the ambiguity set* (candidate the site could plausibly target) but must never by itself
  produce a confident single-target resolution.
- In the `target_class` branch ([pipeline.rs:1220](../../crates/calm-core/src/indexer/pipeline.rs#L1220)),
  when the exact `(callee, cls)` lookup misses, walk the closure — **resolved/formal edges only** —
  and try `(callee, ancestor)` for each reachable ancestor **before** the unscoped `by_name` fallback.
  - Unique hit on a `resolved`/`formal`-backed ancestor → real `Resolved`/`Inferred` (this is scoping,
    not a guess), *not* `weak_receiver`.
  - No hit across the confidently-backed closure → keep the current unscoped fallback (unchanged
    behavior) — a `textual`-only ancestor relation does not count as a hit for this purpose.
- **Interface/dynamic dispatch:** when a receiver's static type is an interface/trait with multiple
  implementors, do **not** force one target. Optionally emit a `polymorphic` **dispatch set** rather
  than `ambiguous` ("legitimate polymorphism," analysis §32). *Scope the ancestor-lookup first;
  defer the new `polymorphic` state to a follow-up* — it touches the `EdgeConfidence` enum and every
  exhaustive match, so keep it out of the first PR.

**Blast radius.** `build_resolution_context` + `resolve_sites_to_edges` (hub). Interacts with the
existing `exact_class_matched`/`weak_receiver` logic — must not regress
`test_type_path_call_resolves_scoped_not_fanned_out` or
`test_unresolved_receiver_method_call_does_not_fan_out_to_unrelated_same_named_function`.

**Test plan.** The 2-class Java repro from the 18/8 investigation (`getName`/`isNew` on an ancestor)
→ assert a `resolved`/`inferred` edge to the ancestor declaration, not `ambiguous`. Plus an interface
with 2 implementors → assert the dispatch set (or, pre-`polymorphic`, both edges as candidates, not a
single wrong pick).

**Effort** M–L · **Risk** Med (touches the most-tested function; guard with WS0 + existing
fan-out tests).

---

### WS5 — `same_dir` becomes a non-destructive ranker

**Goal (V3 Law 2).** Soft signals reorder; they must not delete the true target.

**Root cause.** D5. [pipeline.rs:1468](../../crates/calm-core/src/indexer/pipeline.rs#L1468): if
`same_dir()` matches, out-of-directory candidates are discarded — even though for Java/C/C++ the
directory is a *convention*, not a scoping rule. If the real target is in another directory it is
gone, and the `ambiguous` safety net can't help (it's already been removed from the set).

**Change.**
- Keep `same_file` as a **hard** filter for free functions (genuinely Rust/language scoping —
  documented and correct).
- Change **only** `same_dir` from "filter that replaces the candidate set" to a **ranker**: keep all
  candidates, mark same-directory ones as higher-rank.
- **Persistence primitive required (post-WS2 review — this is not optional plumbing, it's the actual
  mechanism).** The current data model persists only `call_edges` + `edge_confidence`; there is no
  field that can represent "A is preferred over B, but B is still a live candidate." Without one,
  "non-destructive ranking" degrades to one of two wrong shapes at insert time: either both A and B
  get emitted as `ambiguous` (the *ranking* — which one is actually likely — evaporates the moment
  the row hits `call_edges`, an agent sees two undifferentiated maybes), or A alone is trusted at
  `textual`/`inferred` while B is dropped (silently back to the old destructive behavior, just at a
  lower confidence label). Neither is what "ranker" means. Add a minimal ordering column —
  `call_edges.candidate_rank INTEGER NOT NULL DEFAULT 0` (`0` = preferred/same-dir match, `1+` =
  alternate, ordinal not a score) — via `migrate_add_column`, same additive pattern as every prior
  column. `callers()`/`callees()` sort `ambiguous` entries by `candidate_rank` so the same-dir match
  surfaces first without CLAIMING to be the sole resolved target. This is a small, targeted primitive
  — not the full evidence-lattice/`edge_evidence` table from §1 DEFERRED — scoped strictly to "can we
  express soft order," nothing more.
  - 1 same-dir candidate + others remain → emit the same-dir one at `candidate_rank = 0`, alternates
    at `candidate_rank = 1`, all still `ambiguous`-tier (not a clean single target).
  - This preserves recall (true out-of-dir target survives, visible, just ranked lower) while making
    the directory *preference* legible instead of silently discarded by storage.
- This is the **incremental** form of the analysis's hard/soft split (§31) — we do not rewrite the
  whole function into a lattice; we reclassify the single weakest destructive signal and give it just
  enough of a persistence primitive to mean what it claims.

**Blast radius.** One branch of `resolve_sites_to_edges`. Watch `same_dir`-dependent tests (the
Go/Java/C/C++ directory-preference cases).

**Test plan.** Fixture: Java call whose true target is in a *sibling* directory while a same-name
decoy sits in the caller's directory → assert the true target is not lost (present as candidate),
and the decoy is preferred for ordering but the site is not falsely `resolved` to the decoy alone.

**Effort** M · **Risk** Med (could shift some edges from a confident single to ambiguous — WS0
must show this is a **precision gain**, not a recall loss).

---

### WS6 — Use StackGraph `definition_symbol` to pick the target (conditional)

**Goal (V3 Law 3/5).** Turn StackGraph from "name has *some* formal resolution" into an actual target
disambiguator.

**Root cause.** D6. [pipeline.rs:590](../../crates/calm-core/src/indexer/pipeline.rs#L590) collapses
`FormalEdge{ref,def}` to a `HashSet<String>` of reference names; the proven `definition_symbol` is
thrown away, so [pipeline.rs:747](../../crates/calm-core/src/indexer/pipeline.rs#L747) can only bump
confidence by name.

**Why conditional.** StackGraph yields symbol *strings*, not `file:line` — mapping them to CALM
`qualified_name`s is non-trivial, and **SCIP already does exact byte-span→target mapping better**
([ingest.rs](../../crates/calm-core/src/scip/ingest.rs)) for languages where SCIP is installed. So WS6
only pays off for languages/repos where StackGraph runs but SCIP is absent. **Gate on WS0 data**: do
WS6 only if the benchmark shows StackGraph-only languages carrying `false_confidence` or lost
precision that SCIP doesn't cover.

**Gate evaluated (2026-08-19): NOT MET — WS6 deferred, no code written.** Every fixture in
`benchmarks/resolution_precision/` was checked against this gate. The corpus's ONE actual
`FALSE_CONFIDENCE` case (fixture I, filed as **D8**) is a *different* defect than WS6's own root
cause (D6): D8 is the bundled stack-graphs overlay *adding* an edge (`external.py::name @ formal`)
that the static resolver had already *correctly excluded* under Python's shadowing-priority rule —
there is no ambiguous candidate SET for a unique `definition_symbol` mapping to disambiguate; the
overlay is simply wrong on its own, with no reconciliation step against the static layer's own
verdict. WS6 as scoped (routing `definition_symbol` through a unique-exact-mapping check before
selecting among several name-matched candidates) would not fix D8 even if shipped — it addresses a
structurally different shape (an ambiguous fan-out StackGraph *could* narrow) than what fixture I
actually demonstrates (a confident-but-wrong single edge with no fan-out to narrow at all). No other
fixture in the corpus exercises WS6's actual root cause either — fixture A's correct `formal`
resolution comes from the overlay's separate `insert_missing_exact_edges` import-scope backfill
(Finding 4), not from `definition_symbol`-based candidate selection. Conclusion: **build WS7
(overlay/static-layer reconciliation, tracking D8) instead if this class of defect is prioritized**
— it has real, measured evidence behind it. WS6 stays deferred until a fixture (or real-repo
evidence) actually shows the D6 shape it targets; per this section's own rule, "an unavailable [gate
condition] is a reason to defer, not a reason to build speculatively."

**Change (if greenlit) — tightened post-WS2 review.** The original wording here ("map
`definition_symbol` by name+file heuristic, then select that candidate") was self-contradicting: it
proposed fixing D6 (formal evidence collapsed through a heuristic) by routing the formal proof
through *another* heuristic before it can pick a target — Law 3 violated in the very fix meant to
restore it. Split explicitly into two outcomes:
- `formally_resolved_names` → return `Vec<FormalEdge>` (or `HashMap<ref, Vec<def>>`) instead of
  `HashSet<String>` (unchanged from the original wording — this part is fine, it's pure evidence
  preservation, no heuristic involved).
- **Formal proof + a UNIQUE, EXACT identity mapping** (StackGraph's `definition_symbol` string
  resolves to exactly one `qualified_name` by a deterministic, non-fuzzy rule — e.g. an exact
  file+line StackGraph already carries, if it carries one) → **may select** that candidate directly.
- **Formal proof + only a heuristic (fuzzy name/file) mapping** → **store and surface as evidence**
  (e.g. a note on the candidate, visible via `inspect(detail="evidence")` once that exists) but **must
  not promote or select a target** on its own. Falls back to today's confidence-bump-by-name behavior,
  unchanged — never a silent upgrade past what the heuristic actually earns.
- Keep SCIP authoritative: SCIP overlay still runs after and can override either outcome above.
- **If StackGraph's own output never carries enough file/range identity to clear the "unique exact
  mapping" bar for a given language** (to be checked empirically before writing any code here), the
  right call is to **defer WS6 entirely** for that language and put the effort into that language's
  SCIP provider instead (already the higher-leverage, already-proven path — see "What CALM already
  does well" above). Do not ship the heuristic-select path merely because it's the only one
  available; an unavailable hard mapping is a reason to defer, not a reason to downgrade the rule.

**Blast radius.** `formally_resolved_names` + its call site + `resolve_sites_to_edges`; feature-gated
(`lang-*`/stack-graphs).

**Test plan.** Feature-gated fixture where two same-name defs exist and StackGraph resolves the site
to a specific one → assert the edge targets the proven def, not a fan-out.

**Effort** M–L · **Risk** Med · **Precondition:** WS0 evidence.

---

## 4. Sequencing & effort summary

| Phase | Workstreams | Gate to proceed |
|---|---|---|
| 0 | **WS0** | none — build first |
| 1 | **WS1**, **WS2** (parallel) | WS0 baseline committed |
| 2 | **WS3**, **WS4**, **WS5** (independent — see §2's corrected dependency note) | Phase 1 merged; WS0 shows no regression |
| 3 | **WS6** (conditional) | WS0 evidence that StackGraph-only langs need it |

Order rationale: WS1 is the cheapest visible win; ship it early for immediate agent-UX benefit. WS4
and WS5 no longer have a hard ordering between them (corrected post-WS2 review — see §2); both merge
whenever their own WS0 before/after run is clean, independent of the other's status.

---

## 5. Risks, guardrails & process

- **Hub / high-risk edits.** `pipeline.rs` (`resolve_sites_to_edges`, `build_resolution_context`,
  `extract_file_data`) is a hub; editing it via CALM's own tools will likely trip
  `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`. Per house rule, **ask via `AskUserQuestion` at every
  occurrence** — no session-scoped blanket approval.
- **Mandatory gates.** `edit_context` before every edit (hook-enforced); `diff_impact` before every
  commit (hook-enforced). `diff_impact` on `resolve_sites_to_edges` can false-positive on pure
  insertions — cross-check against `git diff`.
- **Every resolver PR (WS2–WS6) must attach a WS0 before/after run.** Merge only if `call_recall`
  did not drop **and** `false_confidence_rate` did not rise.
- **No subagents** — verify/plan/implement inline (house rule).
- **ADR + PATTERN-DEBT.** WS4/WS5 (resolution-semantics changes) warrant an ADR before merge
  (`adr-commit`); note any deliberately-deferred sub-item (e.g. the `polymorphic` state) in
  PATTERN-DEBT.
- **Migrations are additive only** (`migrate_add_column` / `CREATE TABLE IF NOT EXISTS`) — never a
  destructive index.db rebuild.
- **SCIP stays authoritative.** No WS may weaken the SCIP overlay's confirm/rule-out/insert authority
  or its `graph_generation` staleness guard.

---

## 6. Acceptance criteria

- **WS0: DONE.** Committed baseline with per-class `precision`, `call_recall`, `ambiguity_recall`,
  `false_confidence_rate`, `false_confident_site_rate`, `formal_coverage`, over the adversarial
  corpus; `unique_resolution_coverage @ precision ≥ X` scaffolded (reported as insufficient-sample
  until the corpus is large enough to bin meaningfully). See `benchmarks/resolution_precision/README.md`.
- **WS1: DONE.** `callers()`/`callees()` return an exact `direct_by_confidence` breakdown summing to
  `direct_count`; toolsnaps updated.
- **WS2: DONE.** Imported-symbol call resolves to the import target (not fan-out); `import_path`
  column present and populated; no regression on non-import calls.
- **WS3: DONE.** >20-candidate site produces an `ambiguity_group` row and is visible (not silent) in
  `callers()`/`reference_impact`.
- **WS4: DONE.** Inherited-method call resolves to the ancestor declaration as `resolved`/`inferred`
  (not `ambiguous`); no regression on the existing fan-out-prevention tests. Fixture C confirms this
  live (`ambiguous` → `inferred`).
- **WS5: DONE, scope corrected during implementation.** True out-of-directory target is never removed
  from the candidate set **for C/C++** (verified live: fixture E's true target, previously completely
  absent, now surfaces as a ranked candidate — `call_recall` 0.75 → 0.875, no precision cost);
  `candidate_rank` column present and populated. Go/Java were found to need the OPPOSITE treatment —
  both have real, compiler-enforced package scoping (not "directory as convention," as this plan's
  own D5 text originally claimed for them) — an unqualified same-directory match there is a
  structural certainty, not a heuristic, confirmed by two existing regression tests
  (`test_go_same_directory_call_resolves_not_fanned_out`,
  `test_java_same_package_call_resolves_not_fanned_out`) that broke under the naive all-four-languages
  interpretation and were correctly left on the pre-WS5 hard-filter behavior instead. See
  `benchmarks/resolution_precision/README.md`'s "WS5 status" section for the full reasoning.
- **WS6: DEFERRED (gate evaluated 2026-08-19, not met).** No fixture demonstrates the D6 shape WS6
  targets; the corpus's one real false-confidence case (fixture I / D8) is a different defect
  entirely (overlay/static-layer reconciliation) that WS6 as scoped would not fix. See §3 WS6 for
  the full reasoning and the recommendation to track D8 as a new WS7 instead if prioritized.
- **Global: DONE through WS5.** Full `cargo test` green (calm-core 1240/1240, calm-server 399/399,
  full `cargo test --workspace` green) at every workstream boundary; WS0 `false_confidence_rate`
  non-increasing across the whole sequence so far (0.333 baseline → 0.25 after WS2's edge-level
  recomputation → unchanged through WS3/WS4/WS5); `call_recall` strictly improved (0.75 → 0.875, via
  WS5), never regressed.

---

## 7. One-line thesis

> CALM is already a near-SOTA static-analysis database with a V3-grade SCIP layer. Its remaining gap
> is that it **collapses evidence too early** (D1, D6) and **hides uncertainty** (D2, D3) — not that
> it lacks HNSW or a context planner. Fix the evidence model incrementally (WS1–WS5) and CALM gets
> most of "V3" without a rewrite. Do the measurement (WS0) first so every step is provably a
> precision gain.
