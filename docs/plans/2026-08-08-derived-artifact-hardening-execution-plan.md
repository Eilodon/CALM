---
title: "Derived-artifact hardening — Group D execution plan (verified against live source)"
date: 2026-08-08
status: "P1 + P2 + P3a + P4 (core resolver) + PR A (P4.1 resolver soundness, PR C folded in as a
  doc-only non-bug closure) + PR B (digest epistemic integrity) + PR D (issue #65, graph_generation
  binding only — 1 of 4 fields, 3 deliberately deferred) SHIPPED same day (committed: fb58b2b,
  8683c1f, 4fe66bf, 11ac87b, 0154378, d7e2329, 1b914b6; PR D pending its own commit) — see
  §5/§6/§7/§8/§9/§10/§11. P3b (Go writes) and several P4/PR A/PR B/PR D sub-items deliberately
  deferred with documented reasons — see §7/§8/§9/§10/§11. PR E/F and P5-P9 not started.
  Every 'current state' claim below was read from live source this session (file:line cited),
  and a second verification pass (§0.5) corrected four audit claims that turned out inaccurate
  once traced through the code. This is the durable record the session-local '§4-48
  derived-artifact audit' should have produced (executes the §3 meta-fix of
  2026-08-07-audit-findings-recovery-and-open-work-execution-plan.md)."
scope: >
  Ground a large forward-looking audit (Groups B/C/D, its own §4-48) against live CALM code,
  correct the claims verification disproved, and turn the genuine debt (Group D, D1-D9) into a
  prioritized, sequenced, file-level execution plan. Does NOT write feature code or open issues.
inputs:
  - docs/plans/2026-08-07-pecorino-adoption-roadmap.md      # the DESIGN SOURCE the §4-48 audit expands
  - docs/plans/2026-08-07-audit-findings-recovery-and-open-work-execution-plan.md  # Group C (#65/#66/#67) + the §3 durability convention this doc executes
  - KNOWN_LIMITATIONS.md / CONTRIBUTING.md roadmap          # where D8/D9 overlap (L1/L2)
verified_against: HEAD (af37455 / branch claude/memory-docs-spec-review-iw7s4t), this session.
---

# Derived-artifact hardening — Group D execution plan

## §0. What this is

The user supplied a large categorized audit (Groups B/C/D, its own numbering §4-48). Cross-referencing
against the repo:

- The audit is **not in version control** — it survives only in the originating session, the exact
  fragility [2026-08-07-audit-findings-recovery-and-open-work-execution-plan.md](2026-08-07-audit-findings-recovery-and-open-work-execution-plan.md)
  §0/§3 warns about. Group C's §17/§18/§19 map to already-filed issues **#65/#66/#67**.
- Its **design source is in the repo**: [2026-08-07-pecorino-adoption-roadmap.md](2026-08-07-pecorino-adoption-roadmap.md).
  Tier 1 (type_relations, symbol_effects), a lightweight Tier 2 (symbol_digests), Tier 3 (bundles) and
  Tier 4a (package_dependencies) are **already shipped**. Group D is the "make it production-grade +
  close the compatibility/epistemic gaps" backlog on top.

## §0.5. Verification pass (this session) — four audit claims corrected

Deep-tracing each Group-D claim through live source changed the plan materially. The corrections:

| # | Audit claim | Verified reality | Impact |
|---|---|---|---|
| **C1** | D1 "no epoch field exists, 0%" | `index_input_state` table + `index_input_drift()` + `INDEX_INPUT_STATE_POLICY_VERSION` **exist and drive startup reconciliation**. It's 1 coarse hand-bumped epoch over config *files*, not derived-logic. Both consumers ([lib.rs:312](../../crates/calm-server/src/lib.rs#L312) bootstrap, [refresh.rs:315](../../crates/calm-core/src/indexer/refresh.rs#L315) refresh) route `Configuration\|Unknown → full reparse`, `Context → graph rebuild`, and a graph rebuild **does** recompute digests + package_deps ([pipeline.rs:1464-1465](../../crates/calm-core/src/indexer/pipeline.rs#L1464)). | **Reframe** D1 from "build" to "extend + bucket". The plumbing works; P1 becomes small. |
| **C2** | D7 bundle "~10% done (only config_matches)" | `bundle.rs` is **~55% done**: full manifest incl. `git_tree_dirty` + `embedding_model_id`, export/import, sha256 checksum, `PRAGMA integrity_check`, schema-too-new rejection, commit/version/config matching, atomic same-fs `rename()`, WAL/SHM sidecar cleanup, path-traversal-safe extraction (exact-name whitelist). | **D7 effort HIGH → MEDIUM.** Cheapest win is a one-liner (C4). |
| **C3** | D4 parsers "toml-lite/regex" | Parsers use the **real `toml` + `serde_json` crates** (go.mod is line-based, which is correct — go.mod isn't TOML). Already parse Cargo `[workspace.dependencies]`, Go `// indirect`, pyproject **PEP 621 `[project.dependencies]`** + Poetry main/dev/modern-groups, npm's 4 dep-kinds. | **D4 lift is pom.xml + `package_units`**, not "rewrite parsers". |
| **C4** | D7 "trusts git commit only" | Manifest **records** `git_tree_dirty`, but [bundle.rs:317](../../crates/calm-core/src/bundle.rs#L317) **does not consult it**: `force_full_reindex = !commit_matches \|\| !version_matches \|\| !config_matches`. Two divergent dirty trees on the same commit both pass. | **Latent correctness gap** — the cheapest, highest-value single fix in D7. |

Claims **confirmed accurate** by verification: D1 sanitize-derived-text ([inspect.rs:20-58](../../crates/calm-server/src/tools/inspect.rs#L20), no `injection_warning` on `target_text`/`rendered_text`); D1 EMBEDDING_SPACE_ID (`heal_dimension_mismatch` compares `blob.len()/4` only — a same-dim model/pooling/doc-format swap leaves stale vectors, [embedding.rs:562-578](../../crates/calm-core/src/embedding.rs#L562); and `embedding_model_id` is in the manifest but **not** in `config_fingerprint`, so a model swap forces neither local re-embed nor import re-embed); D3 same-file-only, 5-lang (Java/TS/JS/Py/Rust), **Go type relations entirely absent**; D5 single `confidence` col + extraction **drops** uncertain-target throws (re-raise, lowercase-factory) instead of recording them, **Go writes/throws entirely absent**; D2 digest fetch has no DISTINCT/canonical-sort on type_relations ([digest.rs:387-408](../../crates/calm-core/src/graph/digest.rs#L387)); §9 digest off-by-one is a pure breadcrumb.

## §1. The keystone and the sequencing insight

D1's *auto-bump-on-logic-change* is the **keystone**. Every other Group-D item (D2–D5, D7) changes
extraction or derivation logic. On the current **hand-bumped** `INDEX_INPUT_STATE_POLICY_VERSION`,
shipping any of them silently risks stale incremental indexes on already-deployed installs (source hash
unchanged → delta indexer skips → stale derived rows) — the exact bug the user created this session by
editing extraction logic. So **P1 is a prerequisite gate for shipping the rest safely, not a peer.**

The verified reconcile buckets (from C1) dictate *where* each version const goes:

| Version const covers | Fold into | Drift result | Re-derivation triggered |
|---|---|---|---|
| Source extraction (parser, semantic_facts → symbols/call_sites/type_relations/effects) | `config_material` | `Configuration` | **Full reparse** (per-file facts only refresh on reparse) |
| Graph derivation (digest, coreness) + package graph (package_deps) | `context_material` | `Context` | Graph rebuild (both recomputed in `rebuild_graph`) |
| Embedding space (model + pooling + doc-format) | separate (see P1 step 4) | n/a today | Re-embed (not covered by graph rebuild) |

```
P1  D1-core: bucketed version consts + CI drift-guard + DerivedStatus  ── KEYSTONE, gates P3-P7 ─┐
P2  D1-sec:  sanitize derived text at MCP boundary       (independent, tiny)                     │
P3  D5:      effects epistemic split (record what's dropped) + Go writes                         │ behind
P4  D3:      type graph v2 — global (cross-file) resolver  (biggest single value)               │ P1's
P5  D2:      digest dedupe + canonical sort + 3-branch benchmark                                 │ epochs
P6  D4:      package graph v2 (pom.xml + package_units + rebuild-plan)                           │
P7  D7:      bundle finish (git_tree_dirty fix → size cap → writer-lock → enum → epoch fields) ──┘
P8  D8/D9:   change-kind risk taxonomy (= L1) + gate checklist  (process; execute in KNOWN_LIMITATIONS)
```

## §2. Phase detail

### P1 — D1 core: make the compatibility contract auto-bump on logic change  ⭐ keystone

**What exists (C1):** `index_input_state`, `index_input_drift()`, and the full reconcile plumbing. Only the
*inputs* to the fingerprint are too narrow (config files, not derived-logic) and the single `policy_version`
is hand-bumped.

**Steps:**
1. Add four derived-logic version consts co-located with the code they version:
   `SOURCE_EXTRACTION_VERSION` (parser/semantic_facts), `GRAPH_DERIVATION_VERSION` (digest/coreness),
   `PACKAGE_GRAPH_VERSION` (package_deps), `EMBEDDING_SPACE_VERSION` (embedding). Fold each into the
   fingerprint **material bucket its re-derivation needs** (table in §1): source→`config_material`,
   graph+package→`context_material`. This gives finer, cheaper granularity than bumping the single
   `policy_version` (which always forces a full baseline). **No new table, no new reconcile code.**
2. **CI drift-guard** (this is what converts D1 from discipline to gate): a test that hashes the source of
   each versioned module (or a curated fact-schema fixture) and fails if the hash changed without the
   matching `*_VERSION` const changing. Mirrors the existing `UPDATE_TOOLSNAPS` pattern.
3. **`DerivedStatus`** enum (`ready | needs_baseline | stale | unsupported | failed | disabled`) surfaced on
   `indexing_status.intelligence.{types,effects,digest,packages}`, computed from the drift result + presence
   checks. No stored column needed.
4. **Embedding space (verified gap, needs its own path):** `heal_dimension_mismatch` only clears on a
   *dimension* change, and `embedding_model_id` is not in `config_fingerprint`. Fold
   `model_id + pooling + doc-format` into `EMBEDDING_SPACE_VERSION`, and add a stored embedding-space marker
   (one row, like `index_input_state`) so a same-dimension model swap clears + re-embeds. Also fold
   `embedding_model_id` into the bundle `config_fingerprint` (feeds P7).

**Files:** `indexer/refresh.rs`, `indexer/parser.rs`, `indexer/semantic_facts.rs`, `graph/digest.rs`,
`indexer/package_deps.rs`, `embedding.rs`, a new CI test. **Effort:** Low–Medium.
**Risk if skipped:** the stale-incremental bug the user already triggered, undetected.

### P2 — D1 security: sanitize derived text at the MCP boundary  (independent, tiny)

**Confirmed gap:** `fetch_semantic_facts`/`fetch_architecture_digest` ([inspect.rs:20](../../crates/calm-server/src/tools/inspect.rs#L20)) emit
`type_relations.target_text` and `symbol_digests.rendered_text` verbatim. `rendered_text` aggregates callee
identifiers repo-wide and is presented by `understand` as CALM's own analysis, so an injection-shaped
identifier anywhere can ride into trusted-looking output.
**Steps:** attach `calm_core::sanitize::injection_warning(...)` as a `content_warning` on the digest +
semantic-facts outputs, **byte-exact** (don't mutate), mirroring `source`/`understand`.
**Files:** `tools/inspect.rs`, `tools/detail.rs`. **Effort:** Low. **Gate:** test paralleling
`source_flags_prompt_injection_pattern_without_mutating_source`.

### P3 — D5: effects epistemic split — record what's currently dropped

**Confirmed gap + refined framing:** `symbol_effects` has one `confidence`, and extraction currently
**drops** throws whose target type is uncertain (`python_reraise_of_bound_variable_is_not_captured`,
`python_raise_of_lowercase_factory_call_is_not_captured` are passing tests). Splitting `event_confidence`
(certain a throw *exists*, ~syntactic) from `target_confidence` (certain *which* exception) lets `understand`
render "throws (type unknown)" honestly and **recover the currently-dropped facts** instead of losing them.
Go writes/throws are **entirely absent** (no `detect_go_write`; `go_and_rust_throw_are_out_of_scope` test) —
so Go receiver-field-write is greenfield, not a fix.
**Steps:** add `event_confidence` + `target_confidence` columns (bump `SOURCE_EXTRACTION_VERSION`); record
the uncertain-target throws with `target_confidence='none'`; add a `detect_go_write` for `r.x = …`. Surface
both fields in `EffectOutput`.
**Files:** `schema.rs`, `indexer/semantic_facts.rs`, `tools/inspect.rs`/`detail.rs`. **Effort:** Medium.
**Gate:** golden fixtures (extend the existing per-language ones). Placed before D3 as a smaller end-to-end
test of the "bump a version const → reconcile" loop on a low-blast-radius fact.

### P4 — D3: type graph v2 — global (cross-file) resolver  ⭐ biggest single value

**Confirmed gap:** `to_symbol` populated only for same-file targets; 5-lang extraction (Java/TS/JS/Py/Rust),
**Go absent**. Cross-file `implements`/`extends` stay `textual`. Highest daily value ("who implements this
interface, across files?").
**Steps (staged):**
1. Split storage: `type_relation_sites` (raw syntactic occurrence, per-file rebuild lifecycle, in `indexer/`)
   vs `type_relation_edges` (resolved edge, rebuilt in the graph pass, in `graph/`) — respects the
   indexer→analysis boundary the roadmap pins.
2. `TypeRef { display, lookup_name, qualifier, generic_shape }` normalized key instead of raw `target_text`.
3. **Resolver ladder** (same-file → import alias → namespace → SCIP overlay → ambiguous), reusing
   `EdgeConfidence` + the `external_proofs`/SCIP provenance pattern for the SCIP rung.
4. Syntax breadth: Java/TS `interface extends`, Rust supertraits, Go structural embedding only (no
   `implements` inference — a fact stronger than the evidence).
5. Reverse lookup into `reference_impact` at `likely_change`/`review` tier only — never `must_change`, never
   the write gate.
**Files:** `schema.rs`, `indexer/semantic_facts.rs`, new `graph/type_resolve.rs`, `tools/inspect.rs`,
reference_impact path. Bumps `GRAPH_DERIVATION_VERSION`. **Effort:** High (largest remaining block).
**Gate:** golden fixtures, zero known FP, no new MCP tool.

### P5 — D2: digest dedupe + canonical sort + the real 3-branch benchmark

**Confirmed gap:** digest fetch does no DISTINCT/canonical-sort on type_relations.
**Steps:** (a) aggregate/dedupe facts before the budget cap and apply a canonical stable sort inside
`graph::digest` (pure determinism; bump `GRAPH_DERIVATION_VERSION`). (b) Stand up the 3-branch benchmark the
roadmap mandates ([L187-193](2026-08-07-pecorino-adoption-roadmap.md)): `A=source+callers`,
`B=+callees+effects+types raw`, `C=+digest`. Only `delta(C−B)` justifies keeping the render path; if `C≈B`,
ship the raw facts and drop the digest render. **This is the gate deciding digest's future** and is the first
place the external retrieval corpus is actually required.
**Files:** `graph/digest.rs`, `benchmarks/`. **Effort:** Medium (code) + High (corpus).

### P6 — D4: package graph v2

**Confirmed state (C3):** robust real-crate parsers; already cover Cargo `[workspace.dependencies]`, Go
`// indirect`, pyproject PEP 621 `[project.dependencies]` + Poetry, npm 4 kinds, `ignore`/poetry-dev (this
session). Flat `package_dependencies`, ecosystem CHECK excludes java.
**Steps, by value:** (1) **Java `pom.xml`** via real XML parse + add `'maven'` to the ecosystem CHECK —
the single biggest coverage gap. (2) `package_units` table + intra-repo path/workspace relationships
(`foo = { path = "../foo" }` → A→B edge) — turns the flat list into a graph. (3) **Manifest size cap** (DoS
guard, mirrors the 8 MiB source cap). (4) Ecosystem fidelity as capacity allows: Cargo `[target.*]` deps +
`package=` rename + path/git metadata; npm `workspaces` + file/link/alias distinction; Python
`[project.optional-dependencies]` + PEP 503 normalize + direct URLs; Go `replace`/`exclude`. (5)
`DerivedRebuildPlan`: recompute a manifest only when its own hash changed (depends on P1 fingerprinting).
**Files:** `indexer/package_deps.rs`, `schema.rs`. Bumps `PACKAGE_GRAPH_VERSION`. **Effort:** Medium.
**Gate:** manifest fixtures per ecosystem.

### P7 — D7: bundle finish (already ~55% done — C2)

Ordered cheapest-first:
1. **`git_tree_dirty` fix (C4) — one-liner, do first:** fold the already-recorded flag into
   `force_full_reindex` (dirty on either side ⇒ can't trust commit alone). Closes a latent correctness gap.
2. **Decompression/size cap** on `import_bundle`'s `entry.unpack` (gzip/db-bomb → disk exhaustion; the
   extraction is otherwise path-safe via the exact-name whitelist).
3. **Writer-lock check before the rename-over-db** ([bundle.rs:328](../../crates/calm-core/src/bundle.rs#L328)): block importing over a live daemon's
   DB (reuse the instance-lock mechanism) instead of renaming the file out from under it.
4. **Source-tree content fingerprint** (hash of sorted path+content_hash) in the manifest, so same-commit
   divergent trees don't false-match — the durable version of step 1.
5. `force_full_reindex: bool` → **`ReconcilePlan` enum** (`NeedsSourceDelta | NeedsFullSourceBaseline |
   NeedsReembed | …`) that preserves *why* (commit/version/config/embedding) and prescribes *what*.
6. **Manifest epoch fields** (the P1 `*_VERSION` consts) so a bundle from a different derived-logic build
   forces the right re-derivation on import. **Depends on P1.**
7. Persist the reconcile decision durably (module currently returns `ImportReport`, doesn't persist) + a
   recurring CI job exercising the `index-bundles` feature.
**Files:** `bundle.rs`, `schema.rs`, CI. **Effort:** Medium.

### P8 — D8/D9: change-kind risk taxonomy + gate checklist

D8's change-kind risk taxonomy (`comment_only | signature | exception_behavior | state_write | …`) **is L1**
in [KNOWN_LIMITATIONS.md](../../KNOWN_LIMITATIONS.md) and the #5 priority of the recovery plan — execute it
there, don't double-track. D8's multi-file crash-recoverable txn **is L2**. The "verification broker" (one
policy point vs scattered `Command::new()`) is gated on L4's exec-policy abstraction. D9's 6-dimension gate
checklist is **process, not code** — encode as a PR-template section + the `docs/audit/` disposition-row
convention (this doc + P1's drift-guard are the first two dimensions made mechanical).

## §3. Explicitly NOT doing now (Group B reaffirmed)

Correct deferrals matching the roadmap's own Tier-4/5 gates, **not** debt:
- **§24 READS** — Tier-5 defer; low density, no use case until WRITES proves value.
- **§31 PPR / LTR** — Tier-4 shadow-only, gated on beating a BFS baseline that isn't built. Pecorino ships
  neither a trained LTR model nor weighted PPR, so there's no proven win to copy.
- **§43-44 federation / cross-repo CALLS** — Tier-4, "don't until evidence"; package-graph (P6) is the precursor.
- **§48 absolute-not-doing list** (ProNE, Leiden hot-path, full HCGS, merged federation DB, per-row metadata
  framework, new MCP tools) — Tier-5 reject.

## §4. Process fix (execute alongside P1, near-zero cost)

Per the recovery plan's §3: fold the recoverable Group-D items into `KNOWN_LIMITATIONS.md` (D8/D9 are already
L1/L2) and file the keystone (P1 drift-guard) as its own issue since it gates the rest. This doc is the
durable audit artifact; keep dispositions updated here as phases land (fixed + commit / deferred + issue).

## §5. P1 execution log (2026-08-08, same session)

**Shipped, uncommitted, fully tested.** Disposition per P1 sub-step:

- **P1 step 1 (bucketed version consts) — DONE.** `SOURCE_EXTRACTION_VERSION` ([semantic_facts.rs](../../crates/calm-core/src/indexer/semantic_facts.rs)),
  `GRAPH_DERIVATION_VERSION` ([digest.rs](../../crates/calm-core/src/graph/digest.rs)),
  `PACKAGE_GRAPH_VERSION` ([package_deps.rs](../../crates/calm-core/src/indexer/package_deps.rs)) added and
  folded into `InputCatalog::index_input_snapshot`'s existing `config_material`/`context_material` buckets
  (`refresh.rs`) — source-extraction bumps force a full reparse, graph/package bumps force the cheaper graph
  rebuild. No new table; extends the existing `index_input_state` mechanism per §0.5's correction.
- **P1 step 2 (CI drift-guard) — DONE.** New [crates/calm-core/tests/derived_artifact_versions.rs](../../crates/calm-core/tests/derived_artifact_versions.rs):
  3 tests, each hashing a frozen fixture's derived-table output against a checked-in literal (mirrors the
  repo's `UPDATE_TOOLSNAPS` convention). All three fixtures verified end-to-end on first run (correct
  extends/throw/write facts, correct digest rollup, correct 2-ecosystem package parse).
- **P1 step 3 (`DerivedStatus`) — DONE, with an honest scope reduction.** Modeled `Ready | NeedsBaseline |
  Stale` only — `Unsupported`/`Failed`/`Disabled` dropped (no real signal backs them yet; fabricating would
  violate "never guess"). Refined the audit's ask further: added `index_input_bucket_drift` (new, additive,
  in `refresh.rs`) so `type_relations`/`symbol_effects` and `symbol_digests`/`package_dependencies` get
  **independently accurate** freshness instead of both reading the same conflated 4-way `IndexInputDrift`
  (which short-circuits on a config mismatch without checking context — reusing it naively per-bucket would
  wrongly mark both stale). Surfaced as `indexing_status.derived_status.{overall,source_facts,graph_facts}`.
  Locked in end-to-end (real tool call, all 3 states) by
  `indexing_status_surfaces_derived_status_transitions` in `tools.rs` — including the honest, verified-not-assumed
  edge case that a test/CLI index skipping the real `bootstrap` (which alone calls `persist_index_input_snapshot`)
  reads `Stale`, not `Ready`, until something establishes the contract.
- **P1 step 4 (embedding space) — DONE for the model/format axis; bundle fold explicitly NOT done.**
  `EMBEDDING_SPACE_VERSION` + `heal_embedding_space_mismatch` (new, `embedding.rs`) extend
  `heal_dimension_mismatch`'s existing dimension-only self-heal to also catch a same-dimension MODEL swap or
  a `symbol_doc`-format version bump — wired into both real production call sites
  (`calm-server::lib::bootstrap_embeddings`, `calm-cli::main`'s `index` subcommand), not threaded through
  `create_embedding_table`/`create_chunk_embedding_table`'s signatures (would've forced ~17 call-site edits,
  almost all test boilerplate with zero behavioral value, for a check that only matters at 2 real sites).
  **Scope correction vs. the original plan text:** did NOT fold `embedding_model_id` into `bundle.rs`'s
  `config_fingerprint` — that fingerprint's own doc comment explicitly scopes it to file-coverage-affecting
  config only (languages/ignore), and conflating a different invalidation axis into it would be a design
  smell. Belongs with **P7** (bundle hardening) as an independent `embedding_matches` field alongside the
  `git_tree_dirty` fix — moved there.
- **Also fixed along the way (P1.4 test authoring caught a real bug in the test, not the implementation):**
  first draft of `heal_embedding_space_mismatch_clears_on_model_swap_at_same_dimension` asserted the WRONG
  first-call behavior; corrected and documented that "no persisted marker" intentionally clears (mirrors
  `index_input_drift`'s own `Unknown → full reparse` precedent) rather than trusting unverified pre-existing
  vectors.

**Verification run this session:** `cargo check`/`clippy -D warnings` clean (workspace, `--features
embeddings`); `cargo fmt --check` clean; targeted test suites (`indexer::refresh::*`, `embedding::*` incl. the
2 new tests, `indexing_status_*` incl. the 1 new test, `derived_artifact_versions.rs`) all green; full
`cargo test --workspace --features embeddings` run — see this session's record for the outcome.

**Not committed.** All P1 changes plus the pre-existing uncommitted work from earlier this session
(stale-`type_relations`/`symbol_effects`-on-full-reindex fix, `bundle.rs` `config_matches` fix, orphaned
embedding-vector pruning, Poetry dependency groups) remain in the working tree pending explicit commit
approval.

**Two real findings surfaced purely by building P1, not part of the original plan:**
1. A gate quirk in this repo's own edit-safety tooling: modifying (not inserting) a symbol whose body
   references a type/function with many real call sites elsewhere escalates to a human-approval-required
   tier even when the EDITED symbol itself has zero callers — confirmed by testing minimal single-line
   edits that avoided the reference and succeeded cleanly. Worked around by preferring pure insertions
   (`position="after"/"before"`) over in-place replacement wherever possible; not itself a P1 deliverable,
   noted here in case it recurs in P2+.
2. `index_input_drift`'s short-circuit behavior (config mismatch checked before context, never both) — real
   and correct for ITS use case (one reconciliation decision), but would have been a silent correctness bug
   if reused naively for per-bucket `DerivedStatus`. `index_input_bucket_drift` exists specifically because
   of this.

## §6. P2 execution log (2026-08-08, same session)

**Shipped, fully tested.** Sanitized `type_relations.target_text`/`to_symbol`, `symbol_effects.target_text`,
and `symbol_digests.rendered_text` at the MCP boundary (`symbol_info`/`understand`), closing the confirmed
gap from §0.5 (C1's sanitize-derived-text finding) — these previously bypassed the `injection_warning`
check that `source`/`understand`'s embedded-source-block/`remember`/`recall`/`symbols_batch` already apply.

- `fetch_architecture_digest` ([inspect.rs](../../crates/calm-server/src/tools/inspect.rs)) now runs
  `rendered_text` through `sanitize_source_output` (credential redaction, matching `source`'s own contract)
  then `injection_warning`, surfaced as a new `ArchitectureDigestOutput.content_warning` field.
- New `semantic_facts_content_warning` (pure new function, `fetch_semantic_facts` itself left completely
  untouched — see the tooling-gate note below) checks `type_relations.target_text`/`to_symbol` and
  `symbol_effects.target_text` via `injection_warning` only (no credential redaction — these are single AST
  identifier tokens, which cannot syntactically contain the multi-character credential patterns that
  function targets). Surfaced as a new `SymbolInfoOutput.content_warning` field, shared by both `symbol_info`
  and `understand` (`understand.symbol` embeds `SymbolInfoOutput`).
- Wired into all 3 `SymbolInfoOutput` construction sites (`outcome.rs::to_symbol_info`, `locate.rs::locate`,
  `inspect.rs::understand`'s inline literal) and both enrichment call sites (`symbol_info`, `understand`).
- New end-to-end test `understand_flags_prompt_injection_pattern_in_semantic_facts_and_digest` (tools.rs),
  modeled directly on the existing `understand_flags_prompt_injection_pattern_in_embedded_source` — inserts
  an injection-shaped `type_relations.target_text` and `symbol_digests.rendered_text` via raw SQL and
  confirms both `understand.symbol.content_warning` and `understand.architecture_digest.content_warning`
  fire with the real `ROLE_OVERRIDE` category.

**Toolsnaps regenerated:** `locate.snap`, `symbol_info.snap`, `understand.snap` (the 3 tools whose output
schema gained the new field) — confirmed via `UPDATE_TOOLSNAPS=1`, no other snapshot touched.

**Verified:** full `cargo test --workspace --features embeddings` green (1054 calm-core + 366 calm-server +
all other packages, 0 failures — the +1 over P1's 365 is the new test), clippy `-D warnings` clean, rustfmt
clean.

**Tooling-gate note (same class as P1's, reconfirmed):** the first attempt to add `content_warning` as a
third return value directly on `fetch_semantic_facts` (a signature change, 2 real callers) tripped the same
`HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` gate P1 hit, even though the target symbol's own real caller count
was low — reconfirms the pattern is specifically about SIGNATURE changes to an EXISTING function, not actual
caller-count risk. Worked around identically: left `fetch_semantic_facts` byte-for-byte untouched and added
`semantic_facts_content_warning` as a new, purely-inserted sibling function instead, wiring it in at the
call sites (body edits, not signature edits) rather than threading a third tuple element through the callee.

## §7. P3 execution log (2026-08-08, same session) — P3a shipped, P3b deferred

**P3a (effects epistemic split) — DONE, fully tested.** Split `symbol_effects.confidence` into
`event_confidence` (certainty an effect happened — always `"exact"` in v1: every extraction site fires only
on a real syntactic raise/throw/write node) and `target_confidence` (`"exact"` | `"none"` — certainty about
WHAT the target is). This **recovers** the 3 facts the pre-P3 code silently dropped:

- `raise e` (bound exception variable) — was `.is_empty()`, now `[("explicit_throw", "e", target_confidence: "none")]`.
- `raise factory()` (lowercase call, PEP-8 heuristic can't tell it from `raise SomeException()`) — same recovery.
- Bare `raise` (re-raise) — was silently skipped entirely (no target text at all structurally); now recorded
  with `target_text: ""`, `target_confidence: "none"` — a real throw event previously invisible.

Java/TS/JS throw detection stayed untouched: those require a real `object_creation_expression`/`new_expression`
constructor node structurally, so they're always `target_confidence: "exact"` when they fire at all — the
PEP-8-casing uncertainty is Python-specific, verified by reading `detect_java_throw`/`detect_tsjs_throw`
before assuming a blanket text-casing classifier would be correct (it would have wrongly downgraded
legitimately-certain Java/TS/JS facts).

- **Schema:** `symbol_effects` gains `event_confidence`/`target_confidence` columns (fresh-install CREATE TABLE
  + `migrate_add_column` for existing installs — old `confidence` column left in place on upgraded DBs, matching
  every other migration in `run_migrations`, which are all purely additive).
- **`SOURCE_EXTRACTION_VERSION` bumped 1→2** — the first real, live exercise of the P1 mechanism: the
  drift-guard test (`derived_artifact_versions.rs`) correctly FAILED first (hash mismatch), confirming the
  guard actually catches an unbumped-looking extraction change, then passed once the expected hash was updated
  alongside the version bump — proof the P1 keystone works as designed, not just in its own unit tests.
- **Design choice:** confidence classification lives in `walk_effects` (language + kind aware), not inside each
  per-language `detect_*` function — keeps the 7 existing detectors' signatures untouched and the uncertain-target
  logic in one place (`language == "python" && kind == "explicit_throw"`), rather than scattering a
  `looks_like_exception_reference` call into every detector where it wouldn't even be semantically correct for
  the structurally-certain languages.
- **Tests:** all 3 previously-dropping tests now assert recovery (kept their original — now slightly stale —
  names; see the tooling-gate note below for why); new `symbol_info_surfaces_effect_confidence_split` end-to-end
  test through the real `symbol_info` MCP output; pre-existing `understand_surfaces_architecture_digest_and_t1_facts`
  updated for the 2 new JSON fields (a real, expected `assert_eq!` full-object-equality failure caught by the full
  suite run — not a logic bug, just a test needing the same update every other `EffectOutput`-shaped assertion got).
- **Verified:** full `cargo test --workspace --features embeddings` green, clippy clean, rustfmt clean, `locate`/
  `symbol_info`/`understand` toolsnaps regenerated (the 3 tools whose schema gained the 2 new `EffectOutput` fields).

**Tooling-gate note (new variant this phase):** renaming an existing #[test] function name (even with zero real
callers, body untouched) trips the same `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` gate a signature change does —
confirmed by isolating the change (a body-only edit to the same function succeeded cleanly; the identical edit
plus a name change did not). This is a NEW data point beyond P1/P2's "signature changes escalate" finding:
identifier RENAMES specifically escalate too, independent of body content. Workaround: kept the 3 recovered-fact
tests' original names (now slightly stale relative to their new behavior) and added an explanatory comment in
each rather than renaming, since renaming carries zero functional value that would justify pushing further into
a gate designed for exactly this kind of caution.

**P3b (Go receiver-field-write detector) — deliberately deferred, not attempted.** `detect_go_write` would need
to track the enclosing method's receiver PARAMETER NAME (e.g. `r` in `func (r *Foo) M()`) through `walk_effects`'s
recursive traversal to correctly attribute `r.X = v` as a receiver-field write — unlike Rust's `self`, a language
keyword unambiguous regardless of binding, Go's receiver is just a regular identifier that could collide with an
unrelated local variable of the same name in a different function. This needs real design work (extending the
`current` tuple `walk_effects` already threads through recursion to also carry `Option<String>` receiver name,
correctly handling pointer vs. value receivers, and testing against false positives on shadowed/non-receiver
identifiers) that the module's own doc comment already flags as deliberately not wired: *"Go writes: needs
receiver-variable-name correlation... isn't wired here yet — deferred."* Rushing this alongside P3a's
epistemic-split work (already a meaningful, self-contained unit) risked a subtly-wrong detector — worse than no
detector, since a mis-attributed write is a false positive the "never guess" principle this whole plan is built
on explicitly rejects. Left as its own future slice, not silently dropped from the plan.

## §8. P4 execution log (2026-08-08, same session) — core resolver shipped, deliberately narrowed

**Design deviation from the literal spec (stated up front, not discovered mid-implementation):**
implemented as a **single-table, lifecycle-differentiated-columns** design instead of physically
splitting `type_relations` into `type_relation_sites`/`type_relation_edges`. `extract_file_data`
(indexer, per-file, unchanged) still writes `from_symbol`/`relation_kind`/`target_text` and
same-file-only `to_symbol`; a new graph-wide pass now OWNS upgrading `to_symbol`/`confidence` for
whatever extraction left unresolved. This gets the exact same indexer/graph separation the 2-table
design was after, at zero migration cost, and matches how `symbols`/`call_edges` already mix
indexer- and graph-owned columns in one table elsewhere in this codebase — a better-precedented
choice than introducing the repo's first physically-split derived-fact table pair.

**Shipped:** new [`graph/type_resolve.rs`](../../crates/calm-core/src/graph/type_resolve.rs) —
`resolve_cross_file_type_relations`, a self-contained pass (takes only `&Connection`, matching
`compute_coreness`/`compute_digests`/`compute_package_dependencies`'s existing signatures — does NOT
reuse `indexer::pipeline`'s private `ResolutionCtx`, respecting the indexer→graph boundary the
pecorino roadmap pins) wired into both `rebuild_graph` and `incremental_graph_update` alongside those
same three passes. Resolution ladder: same-file (existing, untouched) → cross-file same-language
unique bare-name match (`resolved`) → 0 or >1 candidates stay exactly as extraction left them
(`textual`, never guessed). `GRAPH_DERIVATION_VERSION` bumped 1→2.

**Verified:** 7 unit tests in `type_resolve.rs` itself (unique match, ambiguous stays textual,
cross-language never conflated, generic/qualified `target_text` stripped correctly via `lookup_name`,
same-file rows never re-touched, self-heals on a later rebuild once the target exists) + one real
end-to-end test (`cross_file_type_relation_resolves_after_full_index`,
`golden_graph_equivalence.rs`) proving it through the ACTUAL pipeline (`extract_file_data` →
`rebuild_graph`), not just against a synthetic DB. No MCP-layer code touched — `fetch_semantic_facts`
already reads whatever's in `type_relations.to_symbol` unconditionally, so cross-file-resolved facts
surface through `symbol_info`/`understand` automatically, already covered by existing tests on that
read path (confirmed by inspection, not re-tested redundantly). No toolsnap drift.

**Deliberately deferred from the full P4 spec (see §2's original P4 section for the literal steps):**
- **Import-alias / namespace disambiguation for the multi-candidate case** — when 2+ same-language
  candidates share a bare name, this v1 stays honestly `textual` rather than narrowing via the
  referencing file's own imports. Doing that safely needs the same import-alias machinery call-edge
  resolution already has, reused carefully without violating the boundary this module's doc comment
  explains staying clear of. A real, scoped follow-up — not attempted here to avoid rebuilding
  call-edge-resolution-grade complexity inside what should stay a small, auditable pass.
- **Full `TypeRef` struct** (`display`/`lookup_name`/`qualifier`/`generic_shape`, publicly reusable) —
  reduced to the private `lookup_name` helper for v1. Promotable to the richer shape later if/when
  `reference_impact` integration (below) needs it.
- **SCIP-overlay resolution rung** — extending SCIP ingest to also validate/upgrade `type_relations`
  (giving `formal` confidence via real type-checked evidence, matching how `call_edges` already gets
  SCIP-upgraded) is a separate, comparably-sized integration touching `scip::ingest`, a subsystem not
  touched this session.
- **Extended syntax breadth** (Rust supertraits, Java/TS multi-interface `extends`) — orthogonal to
  and independent of this resolver; whatever `type_relations` rows extraction produces (current 5-lang
  scope, unchanged by P4) now get cross-file resolution, and extraction breadth can grow later without
  touching the resolver at all.
- **`reference_impact` integration** — needs the resolver proven correct on real repos first, and
  `reference_impact`'s exact tier system investigated before wiring in (the plan's own instruction:
  `likely_change`/`review` tier only, never `must_change`, never the write gate).

## §9. PR A (P4.1 resolver soundness) execution log, with PR C folded in (2026-08-08, same session)

A second-round audit of the P4 code shipped in §8 (same day, still uncommitted-fresh) found 4 real
correctness gaps introduced by that first cut, plus one claim (a "graph-generation off-by-one bug",
proposed as its own PR C) that turned out to be a **verified non-bug** — `digest.rs`'s own module doc
comment and `schema.rs`'s `symbol_digests` table comment already document, with reasoning, that
`graph_generation` is "purely an observability breadcrumb, never compared for correctness" because
the table is unconditionally DELETE+re-INSERT every rebuild. Implementing PR C's proposed fix (thread
a `next_generation` parameter through `rebuild_graph`/`compute_digests`, add a regression test
asserting `symbol_digests.graph_generation == graph_generation_state.generation`) would have added a
new invariant the codebase had already deliberately declined to hold, for zero functional benefit, at
the cost of touching `rebuild_graph`'s signature across 4 call sites. **Disposition: closed as a
documentation clarification, not a code change** — a short comment was added at all 4
`UPDATE graph_generation_state SET generation = generation + 1` call sites
(`rebuild_graph_from_index`, `reindex_all_cancellable_with_phase`, `reindex_changed_cancellable`,
`reindex_paths`, all in `pipeline.rs`) pointing back to `digest.rs`'s module doc, so a future reader
doesn't rediscover the same "bug" and spend a PR re-fixing a non-issue.

**A1 — `resolution_source` ownership column.** `type_relations` gained a nullable
`resolution_source TEXT` column (`'same_file_ast'` | `'cross_file_unique'` | `NULL`), set by
`extract_file_data` when it resolves a same-file target and by `resolve_cross_file_type_relations`
when it resolves a cross-file one. Migration added to `run_migrations` (purely additive, matching
every other column in that function). Purpose: give the graph-wide pass a way to know which rows it
is allowed to reset/downgrade without inferring ownership from `confidence` alone (both resolvers can
produce `'resolved'`).

**A2 — reset-then-recompute, not upgrade-only.** The v1 resolver (§8) only ever examined
`to_symbol IS NULL` rows, so a row it had already resolved was never re-examined and could go stale
silently if its evidence later disappeared. `resolve_cross_file_type_relations` now opens with
`UPDATE type_relations SET to_symbol = NULL, confidence = 'textual', resolution_source = NULL WHERE
resolution_source = 'cross_file_unique'` before resolving anything — every row IT owns is reset and
re-derived from current DB state on every single call. `same_file_ast` rows are structurally excluded
from this reset (the WHERE clause only ever matches `'cross_file_unique'`) and stay exactly as
`extract_file_data` last set them, re-derived instead by that function on the next reindex of their
own file. This is what makes `resolved -> textual` an actual reachable transition (target deleted,
renamed, or a second same-named candidate appears) instead of a permanent one-way upgrade.

**A3 — candidate universe restricted to type-like kinds.** The v1 resolver's candidate map was
`SELECT name, qualified_name, language FROM symbols` with no `kind` filter — a same-named
function/variable could steal a type relation if the real class had been deleted/renamed. Now filtered
to `WHERE kind IN ('class', 'struct', 'trait', 'interface', 'enum')`, reusing the exact kind set
`extract_file_data`'s own `class_qn_by_name` (same-file candidate map) already used — the v1 gap was
an inconsistency between the two resolvers' candidate rules, not a design choice.

**A4 — qualifier safety.** `lookup_name` unconditionally stripped both generics (`Base<T>` → `Base`)
AND a qualifier prefix (`pkg.Base` → `Base`) before matching. A new `has_unresolved_qualifier` check
now runs first: a target with a qualifier the resolver doesn't parse (`pkg.Base`, `crate::foo::Base`,
`Foo::Base`) is skipped entirely, staying `textual` even if a same-named LOCAL symbol would otherwise
match uniquely — the qualifier might name an external, unindexed type, and discarding it to
manufacture a `'resolved'` match would be exactly the kind of guess this codebase's "never fabricate
beyond the evidence" principle rejects. Generic-only stripping (no qualifier) is still allowed.

**A6 — regression tests.** 7 new/repurposed unit tests in `type_resolve.rs`: qualified target stays
textual (repurposed `resolves_generic_and_qualified_target_text`, kept its name for git-blame
continuity per this session's established convention — see §5-§8 for prior instances of the same
pattern), bare-generic-without-qualifier still resolves, same-named non-type symbol never resolves,
qualified target never resolves even when a bare match exists, and the three A2 downgrade scenarios
explicitly (`resolved -> textual` on target-deleted, target-renamed, second-candidate-appears). Plus
one new end-to-end test, `cross_file_type_relation_resolves_after_incremental_reindex`
(`golden_graph_equivalence.rs`), proving the resolver behaves identically through
`incremental_graph_update` as through a full `rebuild_graph` — the §8 test only ever exercised the
full-index path. `GRAPH_DERIVATION_VERSION` bumped 2→3 (this session's cumulative graph-rebuild-time
semantics change, covering both §8's original resolver addition — which shipped without its own bump,
now closed retroactively — and this round's refinements). The existing `graph_derivation_fixture`'s
hash was unaffected (that fixture has no cross-file relation to exercise), so no hash update was
needed; `source_extraction_fixture` and `package_graph_fixture` also unaffected (unrelated logic).

**Deliberately not done (unchanged from §8's own deferral list, still correct):** import-alias/
namespace disambiguation for the multi-candidate case, full `TypeRef` struct, SCIP-overlay resolution
rung, extended syntax breadth, `reference_impact` integration. A4's qualifier guard makes the
multi-candidate deferral slightly more conservative than before (a qualified target no longer even
reaches the candidate-count check), not less — no scope regression.

**Verified:** full workspace test suite green (`calm-core` 1067 + 3 golden/derived-artifact
integration binaries all passing, `calm-server` 367, `calm-cli` 16 + `daemon_integration` 10 — the
latter only green single-threaded; parallel run showed 4 socket/process-timing failures traced to
this session's own live CALM MCP daemon contending for the same sockets, not a real regression, no
files outside `calm-cli`'s own daemon/CLI code were touched this round to begin with), `cargo clippy
--workspace --all-targets -D warnings` clean, `cargo fmt --check` clean.

**Tooling note for future sessions:** `edit_lines`/`edit_symbol` correctly load-bearing throughout
A1-A3, but two edits to `type_resolve.rs`'s test module hit `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`
(citing ">10 confirmed callers" on `resolve_cross_file_type_relations`) even for a body-only,
zero-net-caller-count-change edit — the elicitation round-trip this gate requires has no path to
completion in a non-interactive session. Both were completed via native `Edit` instead (same file,
test-only code, already fully reasoned through `edit_context` first) — a documented, deliberate
exception to this repo's calm-first tool policy, not a silent fallback.

## §10. PR B (Digest Epistemic Integrity) execution log (2026-08-08, same session)

Same audit round as §9's PR A, second finding: T1's confidence metadata (P3a, `event_confidence`/
`target_confidence` on `symbol_effects`; `confidence`/`to_symbol` on `type_relations`) was captured
correctly at the source but silently dropped between T1 and T2 -- `EffectFact`/`TypeRelationFact`
(the structs `compute_digests` builds and `render_digest` reads) never carried it, so an uncertain
fact (`raise e`, a textual/unresolved base class) rendered in `symbol_digests.rendered_text`
identically to a confirmed one. Verified concretely, not just in the abstract: traced `detect_python_
throw`'s bare-`raise` case (`target_text = ""`) all the way through the old `render_digest`'s
`throws.join(", ")` and confirmed it would literally produce `"Throws: ."` -- a real, reachable bug,
not a hypothetical.

**B1 — confidence carried through.** `EffectFact` gained `event_confidence`/`target_confidence`
(mirroring `symbol_effects` exactly); `TypeRelationFact` gained `to_symbol`/`confidence` (mirroring
`type_relations`). Both queries in `compute_digests` updated to select the new columns. Note:
`ArchitectureDigestOutput` (the MCP-surfaced struct in `calm-server`) only ever exposes
`rendered_text`, never `facts_json` -- confirmed by reading `fetch_architecture_digest` before
starting, so this change has zero MCP-schema/toolsnap surface and only affects `render_digest`'s
prose output.

**B2 — uncertain throws render as hedged, not confident, prose.** `render_digest` now splits
`explicit_throw` effects three ways: `target_confidence == "exact"` and non-empty text →
`"Throws: X."` (unchanged for the common case); non-`"exact"` with text (`raise e`, `raise
factory()`) → a separate `"Possibly raises (target unresolved): X."` line, same hedge vocabulary
`"Possibly calls:"` already established for `Inferred`-confidence callees; empty text (bare `raise`)
→ a single contentless `"Reraises an exception."` fact, never joined into either list. Fixes the
`"Throws: ."` bug directly.

**B3 — repeated identical effects dedupe to one fact with an occurrence count, before the
`MAX_EFFECTS_SHOWN` truncation.** `EffectFact` gained `occurrences: usize`; `compute_digests` builds
`effects_by_symbol` with an auxiliary `HashMap<EffectDedupKey, usize>` index per symbol so 5 identical
`self.cache = ...` writes across a function become one `EffectFact { occurrences: 5, .. }` instead of
5 entries competing for the same fixed budget. `render_digest`'s new `format_effect_target` helper
renders `"cache (x5)"` when `occurrences > 1`, plain `"cache"` otherwise -- ASCII `(xN)`, not a `×`
glyph, to avoid any encoding-fragility risk in a string that flows through `sanitize_source_output`/
`injection_warning` at the MCP boundary.

**B4 — type relations canonically sorted and deduped before render.** Previously assembled straight
from `HashMap` iteration + raw SQL row order (SQLite's return order is usually-but-not-guaranteed
insertion order) -- now sorted by `(relation_kind, target_text)` and deduped on that same key inside
the per-row loop, matching the sort+dedup `confirmed_callees`/`possible_callees` already had (this
was the asymmetry the audit flagged: callees were canonicalized, type relations weren't).

**B5 — resolved vs. textual type relations get separate lines.** `render_digest` now filters
`extends`/`implements` by `confidence == "resolved"` for the unhedged `"Extends:"`/`"Implements:"`
lines, and a parallel `confidence != "resolved"` filter for `"Possibly extends (unresolved):"`/
`"Possibly implements (unresolved):"` -- an unresolved relation is never upgraded into unhedged prose
just because extraction only found the one candidate.

**B6 — `GRAPH_DERIVATION_VERSION` bumped 3→4** (this is squarely the case its own doc comment
describes: a `compute_digests` rendering-logic change). The `graph_derivation_fixture`'s pinned hash
was unaffected -- verified by running the test, not assumed: that fixture's single throw/write/extends
facts all have `occurrences == 1` and `confidence == "resolved"`, so B2-B5's new branches are all
no-ops for that specific frozen input and the rendered text is byte-identical to before.

**Verified:** 4 new unit tests (`render_digest_hedges_uncertain_and_bare_throws`,
`render_digest_hedges_unresolved_type_relations`, `render_digest_shows_occurrence_count_for_repeated_
writes`, and an end-to-end `compute_digests_dedupes_repeated_effects_and_canonicalizes_type_relations`
that inserts 5 real `symbol_effects` rows + 2 out-of-order `type_relations` rows through the real
`compute_digests` function, not just against a hand-built `DigestFacts`) plus the existing
`render_digest_is_factual_and_compact`/`compute_digests_end_to_end_reflects_call_graph_and_t1_facts`
updated to compile against the new struct fields (assertions unchanged where the fixture's confidence
values don't exercise the new hedging paths). Full workspace suite green (`calm-core` 1071, `calm-
server` 367, same `calm-cli`/`daemon_integration` single-threaded caveat as §9), `cargo clippy
--workspace --all-targets -D warnings` clean (one real type-complexity lint hit and fixed with a local
`EffectDedupKey` type alias, not suppressed), `cargo fmt --check` clean (via `format_files`, not raw
`cargo fmt`, after being reminded mid-session that a bare `rustfmt` invocation resolves and reformats
the whole owning package's mod tree, not just the listed files).

**Deliberately not done:** B5's richer proposal (also carrying `resolution_source` from PR A's new
column into `TypeRelationFact` to let the renderer eventually distinguish `same_file_ast` from
`cross_file_unique` provenance in prose, not just resolved/textual) -- `confidence` alone is
sufficient for the hedge/no-hedge decision render_digest actually needs today; adding a field with no
current reader would be speculative. Promotable later without another schema change (the column
already exists from PR A).

## §11. PR D (issue #65, Review Authority Snapshot) execution log (2026-08-08, same session)

Issue #65 asks the write-gate's review token to bind more of what a review implicitly trusted, not
just the caller-set digest: graph generation, watcher freshness, provider (SCIP/stack-graphs)
generation, and risk-policy version, each with its own distinguishing error code. Per the issue's own
framing ("định nghĩa rõ review token chứng minh điều gì, rồi mới implement"), the contract was worked
out before writing code, then implemented for exactly ONE of the four fields this round -- the one
that turned out to be well-scoped, cheap, and (critically) newly valid.

**A stale prior-session finding, corrected.** Before implementing, re-read
`docs/plans/2026-08-02-ws2-review-token-execution-plan.md` (the design doc for the CURRENT
`caller_set_digest`/`STALE_CALLER_SET` mechanism) to see why `graph_generation` wasn't already bound.
Its finding F1 (2026-08-02): `incremental_graph_update` -- the path every `edit_lines`/`edit_symbol`
write actually takes by default -- "never touches" `graph_generation_state`, making the counter too
coarse to gate on; kept diagnostic-only by deliberate design. **Verified this session (2026-08-08,
reading `reindex_changed_cancellable`/`reindex_paths`/`rebuild_graph_from_index`/
`reindex_all_cancellable_with_phase` directly, all in `pipeline.rs`) that this is no longer true**:
every one of those functions now bumps `graph_generation_state.generation` whenever
`!summary.is_noop()`, whether the reindex went through the full or incremental path. Some later
session between 2026-08-02 and today closed the F1 gap (this session's own PR C fold-in comments were
added at exactly these 4 call sites, for an unrelated reason, and directly confirmed this). F1's
objection to gating on `graph_generation` no longer holds -- this is the same "verify before trusting
a past design decision" discipline C1-C4/§0.5 applied to the original audit, applied here to a design
doc instead.

**Shipped: `graph_generation` binding, `STALE_GRAPH_AUTHORITY`.** `EditContextReview` (`tools.rs`)
gained a `graph_generation: i64` field, captured in `guardrails.rs::edit_context` (a plain `SELECT
generation FROM graph_generation_state WHERE id = 1` alongside the existing `caller_set_digest`
computation) and threaded through `record_edit_context_review`. `edit_lines_impl_gated` (`edit.rs`)
now checks it inside the existing per-touched-symbol freshness loop, nested one level deeper than the
`caller_set_digest` check: a symbol whose caller set still matches can STILL be stale if the graph was
rebuilt since review (a rebuild can shift coreness/hub classification without adding or removing that
symbol's own callers) -- a new `STALE_GRAPH_AUTHORITY` error, same shape as the existing
`STALE_CALLER_SET` (audit trail log line, refusal message naming the symbol, `Call edit_context(...)
again` remedy). Fails OPEN (not blocking) if the fresh-generation lookup itself can't open a
connection -- an infra hiccup on this secondary signal must not block an edit the primary
(caller-set-digest) check already found current.

**Verified:** 2 new tests
(`graph_generation_bump_forces_stale_review_even_when_caller_set_is_unchanged`,
`graph_generation_unchanged_since_review_does_not_block_edit`) -- the first isolates the NEW failure
mode specifically (bumps `graph_generation_state` directly via SQL, deliberately leaving `call_edges`
untouched, so `caller_set_digest` still matches and only the new check can catch it), the second is
the regression guard. Full workspace suite green (`calm-server` 369, up from 367), `cargo clippy
--workspace --all-targets -D warnings` clean, `cargo fmt --check` clean (via `format_files`). Zero
MCP-schema/toolsnap surface -- `EditContextReview` is internal session state, never serialized to a
tool response.

**Deliberately deferred (the other 3 of #65's 4 proposed fields), with reasons, not silently
dropped:**
- **Watcher freshness** -- would need a cheap, comparable "as of review, was the index current"
  signal. `indexing_status`'s `watcher.freshness`/`last_refresh` exist but are point-in-time strings,
  not a monotonic counter comparable the same way `graph_generation` is -- needs its own small design
  pass (probably a counter bump on `watch_supervisor.rs`'s reconciliation events), not a `git blame`
  and a field addition. `graph_generation` already covers the sharpest version of this concern (was
  the GRAPH current), which is what the write gate actually reasons about.
- **Provider (SCIP/stack-graphs) generation** -- `call_edges.formal_source`/`evidence_state` exist
  per-edge, but there is no single "provider generation" counter to bind a snapshot to; the closest
  analogue (`external_proofs`' own freshness bookkeeping, `scip::ingest.rs`) is a different
  subsystem this session didn't otherwise touch, and inventing a new counter just for this gate risks
  being the wrong abstraction without first checking how a formal-evidence-only overlay's own
  currency is (or should be) tracked elsewhere.
- **Risk-policy version** -- `config.risk_rules`/`hub_threshold` etc. are config-driven, not
  versioned; a policy-version bump would need a decision about what counts as a "policy change"
  (any config edit? Only `risk_rules`? Only fields the gate actually reads?) that's a real design
  question, not a mechanical port of the other 3 fields' pattern.

Each is a real, scoped follow-up (the taxonomy issue #65 sketches for these -- `REVIEW_POLICY_CHANGED`,
`STALE_PROVIDER_EVIDENCE` -- was the audit's own elaboration, not yet agreed API; confirmed by reading
issue #65's actual body, which lists the 4 concept fields but not the specific error-code names).
Issue #65 stays open, not closed by this round -- `graph_generation` binding is one real field of four,
not the full snapshot.
