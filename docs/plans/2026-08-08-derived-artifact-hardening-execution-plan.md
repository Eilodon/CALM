---
title: "Derived-artifact hardening — Group D execution plan (verified against live source)"
date: 2026-08-08
status: "P1 (keystone) SHIPPED same day, uncommitted — see §5. P2-P9 not started. Every 'current
  state' claim below was read from live source this session (file:line cited), and a second
  verification pass (§0.5) corrected four audit claims that turned out inaccurate once traced
  through the code. This is the durable record the session-local '§4-48 derived-artifact audit'
  should have produced (executes the §3 meta-fix of
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
