# CALM Evidence-Architecture Upgrade — Verified Execution Plan

> **Status:** Wave 0 SHIPPED (PR#1-5) · PR#6-10 specs execution-ready (Part E) · **Date:** 2026-08-19 · **Base commit:** `903d3ef` + Wave 0
> **Method:** Every claim in the source audit was cross-checked against real code
> (file:line below), not docs/comments. This plan keeps what verified, corrects
> what was overstated, and sequences the work by real dependency, not wishlist order.

---

## Part A — Verification verdict

The audit is **unusually accurate**. Its central thesis (evidence is collapsed to a
scalar too early, discarding target identity the provider already knew) is confirmed
at the exact lines it cites. Nearly as important: the audit's top recommendation (WS7
overlay/static reconciliation) is **already the repo's own documented next step**, so
this is prioritizing an existing direction, not inventing one.

| # | Audit claim | Verdict | Evidence (real code) |
|---|---|---|---|
| P0 | Formal upgrade keyed on **bare callee name**; upgrades scalar to `Formal` | ✅ **Confirmed** | `pipeline.rs:773` `formally_resolved.contains(callee.as_str())` → `= EdgeConfidence::Formal` |
| P0 | `FormalEdge` knows **both** `reference_symbol` + `definition_symbol` | ✅ **Confirmed** | `resolver/formal.rs:47-50`, `resolver/mod.rs:22-25` |
| P0 | `definition_symbol` (target identity) **thrown away** before graph | ✅ **Confirmed** | `pipeline.rs:406` `edges.into_iter().map(\|e\| e.reference_symbol).collect()` |
| P0 | D8 = stack-graphs overlay *adds* a wrong `external.py::name @ formal` edge | ✅ **Confirmed** | plan `2026-08-18-context-intelligence-upgrade-plan.md:404-416` |
| P0 | Gating today: recall 0.875, FCR 0.25, false-confident-site-rate 0.125 | ✅ **Confirmed** | same plan `:508,:523-524`; metric computed `resolution_precision/run_benchmark.py:286,297` |
| P0 | Repo's own plan already recommends **WS7 reconciliation** for D8 | ✅ **Confirmed** | same plan `:415-416` |
| 3 | `EdgeConfidence` is a 6-variant scalar ladder via `rank()` | ✅ **Confirmed** | `types.rs` (Formal/Resolved/Inferred/Textual/Ambiguous/Unresolved) |
| 3 | `Unresolved` has **no producer** | ✅ **Confirmed** | only parsed/ranked/consumed (`inspect.rs:1039`, tests); no emit site found |
| 8 | Issue #72 (external-qualified refs) **still open** | ✅ **Confirmed** | `gh issue #72` open: "…1 documented as follow-up" |
| 9 | `import_path` reduced to **basename**, fail-open filter, can collide | ✅ **Confirmed** | `parser.rs:1554 module_path_last_segment`, used `pipeline.rs:1610` |
| 10 | `ambiguity_groups` keyed by **bare name**, not symbol identity | ✅ **Confirmed** | `trace.rs:70` `WHERE candidate_group_key = ?1` (`?1 = c.name`); `pipeline.rs:1292` comment: "not an identity" |
| 16 | `pipeline.rs` is the largest remaining hotspot | ✅ **Confirmed** | `gh issue #67` open |
| 17 | Semantic KNN = brute-force score-all → full sort → truncate | ✅ **Confirmed** | `embedding.rs:880-889 top_k_by_cosine` |
| 17 | All decoded vectors cached in-memory | ✅ **Confirmed** | `embedding.rs:707-719 knn` → `symbol_cache()` global map |
| 19 | ONNX `embed_batch` loops per text | ⚠️ **Confirmed but low-priority** | `embedding.rs:288` — **opt-in** `onnx-embeddings` only; default `Static` path batches (`:281 model.encode(texts)`) |
| 23 | Injection scan on tool output **logged, not surfaced to agent** | ✅ **Confirmed** | `telemetry.rs:28-36` → `tracing::warn!` only; returned `T` untouched |
| 24-28 | verify unsandboxed / no multi-file ChangeSet / risk = signature-only / reason default lexical | ✅ **Confirmed** | `KNOWN_LIMITATIONS.md` (all four entries verbatim) |
| 29 | `kernel_enforced_writes` defaults **false** | ✅ **Confirmed** | `config.rs:256` |
| 33 | No single command mirrors CI (local ≠ CI) | ✅ **Confirmed** | `scripts/` has per-check scripts, **no `xtask`/aggregate**; caused the `0a7a8aa→b1fd2a6→903d3ef` churn |
| 34 | B2 oracle has coverage gaps; gate failing since creation | ✅ **Confirmed** | `gh #72`; `scripts/check-b2-thresholds.sh` exists |

### Corrections / adjustments to the audit

1. **WS0 already exists and is committed** — `benchmarks/resolution_precision/run_benchmark.py`
   already computes `false_confidence_rate`, `false_confident_site_rate`, and a
   `unique_resolution_coverage` scaffold. Wave 0's job is **not "build WS0"** but
   "**grow its corpus** to ≥50 gating sites (only ~2 uniquely-resolved today, so
   `unique_resolution_coverage` is statistically meaningless) **and add D8-class
   regression fixtures**." Adjust wording accordingly.

2. **ONNX batching (audit §19) is genuinely low-priority** — it is only the opt-in
   `onnx-embeddings` backend; the default hot path (`model2vec-rs`) already batches.
   Keep it, but drop it to a "nice-to-have" in Wave 6, not a headline win.

3. **Rating numbers (8.66 → 9.85) are aspirational, not measurable** — treat them as
   direction, never as acceptance criteria. Every gate below is a concrete,
   observable metric or test, never "reach rating X."

4. **The WS0 benchmark path is `resolution_precision/`**, distinct from the older
   `benchmarks/resolution/` tier-histogram benchmark. Don't conflate them.

5. **"Stack Graphs upstream archived" (audit §5) is an external claim** I can't verify
   from the repo. What *is* verified: the repo already moved Python to exact SCIP
   (health shows `python` SCIP available/up-to-date). Demoting SG to a fallback
   *provider* is a sound strategy, but it is a **strategy decision**, not a bug fix —
   gate it on WS0 data (audit §5/§18 already say this), don't front-load it.

**Net:** proceed. The P0 evidence work is real, well-scoped, and low-risk because the
measurement harness to prove non-regression already exists.

---

## Part B — Execution plan

Design invariant threaded through every wave (the audit's "constitution", adopted):

> **Discovery may rank. Evidence may justify. Only reconciled evidence may establish
> graph truth. Context budgets may hide information but never change truth. No
> natural-language explanation may grant write authority.**

Each wave lists: **Goal · Concrete tasks (real files) · Definition of Done (observable)**.
No wave merges without a `resolution_precision` before/after run showing `call_recall`
non-decreasing **and** `false_confidence_rate`/`false_confident_site_rate` non-increasing —
the repo's existing merge rule (`2026-08-18…plan.md:129-130`), kept.

---

### Wave 0 — Stop confidently-wrong truth + stop CI churn (do first)

Cheapest, highest-leverage. Two of these five are process fixes that pay for themselves
immediately.

**0.1 — CI-parity one command** *(process; unblocks everything; ~S)*
- Add `scripts/ci-local.sh` (or a `cargo xtask ci` if a `xtask` crate is introduced)
  that runs, in CI's exact order: `cargo fmt --all -- --check`; the three clippy
  feature-sets CI uses (`verify`, `all-languages`, `otel`+`http`); `cargo test
  --workspace`; `gen-status.sh --check`; `check-doc-truth.sh`; `check-claims-registry.sh`;
  `check-adr-staleness.sh`; `check-b2-thresholds.sh`.
- **`.github/workflows/ci.yml` must invoke the same script** so local ≡ CI by
  construction — this is the actual fix for the `0a7a8aa→b1fd2a6→903d3ef` sequence.
- **DoD:** one command reproduces every blocking CI check; a second, deliberately-stale
  doc count is caught locally before push.

**0.2 — Fix the B2 oracle before tuning anything** *(measurement integrity; ~M)*
- Per issue #72: the rust-analyzer SCIP oracle lacks occurrences for ~13 feature-gated
  source files (visibility mismatch). Fix the oracle build/features so it covers the
  real translation units, **then** re-measure an honest baseline, **then** freeze
  corpus + context fingerprint, **then** set `thresholds.toml` floors.
- **Rule (from `feedback-audit-oracle-before-publishing-benchmark`):** do **not** tune
  the resolver against B2 until the instrument is fixed — otherwise you Goodhart the
  resolver against a broken ruler.
- **DoD:** `check-b2-thresholds.sh` passes on a corpus whose oracle coverage is
  measured and documented; thresholds reflect the honest baseline, not aspirational floors.

**0.3 — WS7A: reconcile provider proof against confident static resolution** *(the P0 core; ~M)* — **SHIPPED 2026-08-19**
> **Mechanism correction (verified live, not from the audit/fixture comment):** the D8
> false-confidence edge is inserted by the **SCIP overlay**
> (`scip/ingest.rs::insert_missing_exact_edges`, `formal_source='scip'`), *not* the
> stack-graphs `formally_resolved` bare-name upgrade the audit and fixture I's own
> oracle note blamed — the call site is already `resolved`, so `extract_file_data`'s
> upgrade (gated on `!= Resolved`) never fires. Fix landed there instead:
> `insert_missing_exact_edges` skips a competing `formal` insert when the same call
> site already has a confident static edge (`resolved`/non-scip `formal`) to a different
> target (`has_conflicting_confident_static_edge`). The stack-graphs bare-name upgrade
> is a *separate*, currently-undemonstrated latent risk — follow-up, not fixed here.
- ~~Change `formally_resolved_names` to return `(reference_symbol, definition_symbol)` pairs~~
  (superseded — that path is not the D8 cause; see correction above).
- At the upgrade site (`pipeline.rs:771-776`), map `definition_symbol` → the target
  `SymbolId` the static resolver *actually chose* for this call site, and compare:
  - **agree** → confirm `Formal` (today's happy path, now *justified*);
  - **static-only** (no formal edge for this exact target) → keep `resolved`/`structural`,
    do not upgrade;
  - **formal-only** (formal proved a target static didn't pick) → provider-supported
    candidate, surfaced as candidate, **not** silently substituted;
  - **disagree** (formal target ≠ static target) → **`EVIDENCE_CONFLICT`**, never a
    silent confident edge.
- Keep the ADR-A1 timeout/`Err`→empty-set semantics intact (that behavior is orthogonal
  and already correct).
- **DoD:** the exact D8 fixture (`from external import name` shadowing a local `def name`)
  produces **zero** confidently-wrong edges; `call_recall` unchanged; no existing
  hard-scoped edge silently replaced.

**0.4 — WS7B: conflict is a first-class, measurable state** *(~S, rides on 0.3)*
- Persist conflicts (minimal schema: `call_site_id`, `target_a`, `target_b`,
  `conflict_kind`) so `EVIDENCE_CONFLICT` is inspectable, and add a
  `provider_conflict_rate` to the `resolution_precision` report.
- **DoD:** provider disagreement is countable in the benchmark; a conflict never renders
  as `formal`/`resolved` to `callers`/`callees`/`reference_impact`.

**0.5 — Issue #72: qualified external roots (universal, not `if root=="std"`)** *(~M)* — **SHIPPED 2026-08-19 (Rust/C++ stdlib slice)**
> Landed as a parser-level `external_crate_root()` helper (`parser.rs`, single set
> `{std, core, alloc}`, not scattered `== "std"` checks) that makes `walk_calls` DROP a
> call rooted at an external crate instead of letting its bare last segment (`write` from
> `std::fs::write`) fall through the unscoped by-name fallback to an unrelated local
> `write` (verified live: `main.rs`'s `std::fs::write` bound to `txn.rs::write`). Chosen
> over the full `QualifiedReference` schema plumbing below because it's parser-only (no
> high-risk `resolve_sites_to_edges` surgery, no schema change) and matches what SCIP
> already produces (no local edge). Regression test
> `test_external_crate_root_call_does_not_bind_to_local_same_named_fn` (syntactic pipeline,
> no overlay). npm/Python/Go/JVM analogues remain follow-ups — each needs its own fixture
> before extending the set (discipline: no change without a demonstrating case). The
> `QualifiedReference` representation below is still the Wave 2 direction.
- Per `feedback-prefer-universal-over-hardcoded-local`: introduce a `QualifiedReference`
  representation `{ root: ExternalCrate(name) | Local, path, name }` in the candidate
  planner. When a call's receiver/root is a known external module (`std`, an npm
  package, a Python external module, a Go external module, a JVM package, a C/C++
  external header namespace), **local candidates are structurally impossible** — the
  planner must not fall through to local by-name matching.
- Reuse the language-specific stdlib knowledge that already exists (`maps.go.is_stdlib`,
  `pipeline.rs:2577`) behind this one representation rather than adding more per-call guards.
- **DoD:** `std::fs::write` (and the npm/Python/Go/JVM analogues as fixtures) never
  binds to a same-named local symbol; the guard is one representation, exercised by a
  cross-language fixture matrix.

**0.6 — AmbiguityGroup target-aware membership** *(~M)*
- Today `ambiguity_groups.candidate_group_key = bare name`, and `callers()`
  (`trace.rs:70`) matches `WHERE candidate_group_key = c.name` — a Python `helper` can
  inherit a caveat from an unrelated Rust `helper` group. Split into
  `ambiguity_group_candidates(group_id, symbol_id, rank_hint)` and query uncertainty by
  `symbol_id`, keeping `candidate_group_key` only as a grouping label.
- **DoD:** a cross-language same-bare-name fixture shows `callers(python::helper)` gets
  **no** caveat from the Rust `helper` group; existing WS3 "dropped site is recorded"
  tests still pass.

**Wave 0 acceptance gate (whole wave):**
`D8 confidently-wrong edges = 0` · `call_recall ≥ current (0.875)` ·
`false_confidence_rate ≤ current (0.25)` · `false_confident_site_rate ≤ current (0.125)` ·
`provider_conflict_rate` now measurable · no semantic change from 0.1/0.2 ·
`scripts/ci-local.sh` green.

---

### Wave 1 — Behavior-preserving `pipeline.rs` split (issue #67)

Do the extraction **before** heavier schema work, so later intelligence doesn't land in
a 10k-line God-module — but **strictly no semantics change**.

- Sequential extraction (one PR each): `pipeline/{discovery,extraction,persistence}.rs`;
  `resolver/{context,candidates,evidence,reconcile}.rs`;
  `incremental/{delta,invalidation,materialize}.rs`; `publish/{metrics,semantic}.rs`.
- **DoD (every extraction PR):** canonical index-DB digest identical before/after ·
  `resolution_precision` identical · B2 identical · incremental≡full equivalence
  unchanged · zero net logic diff (moves only).

---

### Wave 2 — Unified identity foundation

> **See Part E (PR#8, PR#9) for build-ready specs. ⚠ Reading real code corrected this wave: call-site identity already exists at v2 (byte-span) — PR#9 evolves it to a position-independent v3, it does not build it.**

Ship stable identity so evidence and ANN have something durable to bind to.

- `TypeId { language, package_or_module, lexical_scope, qualified_name,
  declaration_symbol_id }`; replace `target_class: Option<String>` with
  `target_type: Option<TypeRef>` where `TypeRef = Resolved(TypeId) | External(ExternalTypeId)
  | Unresolved(TypeShape)`. **External is a real state**, not "local lookup missed."
- `SymbolRevision` (declaration/signature identity), `CallSiteKey` (structural identity)
  + `CallSiteRevision` (normalized local-subtree identity), `ImportBindingId`.
- Migrate gradually; ambiguous identity must **fail closed**.
- **DoD:** same-bare-name-different-package adversarial fixtures pass (`com.foo.User` vs
  `com.bar.User`; `foo.Base` vs `bar.Base`); inserting a comment above a call site does
  **not** invalidate its `CallSiteKey` (kills the proof-churn the audit §37 describes);
  ambiguous identity resolves to conflict, never a guess.

---

### Wave 3 — Full evidence ledger + reconciler (the strategic core)

> **See Part E (PR#10) for a build-ready first slice. ⚠ `external_proofs` + `evidence_conflicts` already provide 2 of the ledger tables — PR#10 generalizes them, it does not start from zero.**

Turn "providers mutate `call_edges` directly" into "providers append immutable evidence;
a reconciler materializes verdicts." SCIP already binds occurrence→byte-span→source-hash
with a graph-generation fence and durable proof re-check — **keep all of that machinery**,
only change its *output target*.

- New tables (audit §3 schema, adapted to current schema version — do **not** copy old
  version numbers): `reference_candidates`, `reference_evidence` (with `disposition
  supports|excludes`, `authority_class`, `provider`, `observed_revision`), `provider_runs`
  (with `coverage_status complete|partial|timeout|failed`), `evidence_conflicts`,
  derived `reference_verdicts` (`verified|structural|probable|possible`, `dispatch_kind`,
  `freshness`).
- **Rule A:** providers no longer mutate `call_edges`; they append evidence. `call_edges`
  becomes a **materialized compatibility projection** of `reference_verdicts` so the 40
  MCP tools keep working through the migration.
- **Rule B:** authority ≠ "wins everything." Compiler/SCIP proof outranks a same-dir
  heuristic, but if it contradicts a local lexical binding / explicit import / language
  package rule / explicit receiver type, the result is **conflict**, not silent overwrite.
- Move StackGraph, SCIP, LSP, and the static resolver behind **one evidence interface**.
  StackGraph demotes from "formal authority" to a fallback *provider* (audit §5) — gated
  on WS0 data showing SG-only languages still benefit.
- **DoD:** no provider mutates the final graph directly · every `verified` edge is
  explainable (which evidence, which provider, which revision) · every conflict
  inspectable · provider completeness (`coverage_status`) visible to tools · WS0
  false-confidence non-increasing.

---

### Wave 4 — Complete reference / type intelligence

- Package-aware type resolution (built on Wave 2 `TypeId`); **polymorphic dispatch as a
  first-class cardinality** (`ResolutionCardinality { Unique, Polymorphic, Ambiguous,
  Unresolved }`) so interface dispatch (`Handler h; h.handle()` with N implementors) is
  represented as a dispatch *set*, distinct from indexer ambiguity (unknown receiver).
- Unify `Call/Construct/Import/Reexport/TypeRef/Inherits/Implements/Read/Write/…` under
  one `ReferenceKind`; `callers()` becomes `references(kind=Call, direction=Incoming)`;
  `reference_impact` becomes a graph-native projection, not a late tool-layer fusion.
- Replace scalar `blast_radius = N` with an **Impact Envelope** (verified/structural
  lower bound, uncertain middle band, non-call refs, mentions, per-provider coverage).
- **DoD:** no same-name global-singleton confident guesses · dynamic dispatch distinct
  from ambiguity in tool output · `reference_impact` reads from the central graph.

---

### Wave 5 — Incremental derivation engine

Keep correctness-first, raise the scalability ceiling.

- `DerivationDependencyIndex`: source changes emit typed delta facts
  (`NameChanged`, `SignatureChanged`, `ImportBindingChanged`, `TypeRelationChanged`,
  `CallSiteChanged`, `VisibilityChanged`, `ModuleChanged`, `BuildContextChanged`,
  `ProviderContextChanged`); each derived subsystem declares its dependencies; a small
  edit recomputes only the dirty derivations instead of rerunning every global pass
  (`resolve_cross_file_type_relations`, coreness, hub/boundary flags, churn, digests,
  package deps).
- Track `graph_ready / metrics_ready / semantic_ready / provider_ready` as distinct states.
- **DoD:** the inviolable law — **`Incremental(G₀, mutations) == FullRebuild(final_source)`**
  by canonical digest — holds over a **randomized state-machine benchmark**
  (create/modify/rename/delete/move/import±/signature/superclass-change/manifest/feature-toggle),
  hundreds of seeds, in nightly. Small edit provably avoids unrelated global work.
  Full-fallback rate explicit + measured.

---

### Wave 6 — Semantic scale + context composer

Order matters: identity/evidence first (Waves 2-3), ANN last, because unstable ids make
ANN invalidation intractable.

- **Cheap, do early (can land in Wave 0-time, no dependency):** replace
  `top_k_by_cosine`'s full `sort` (`embedding.rs:888`) with a bounded top-k heap
  (O(N log k) not O(N log N)); exact oracle unchanged.
- After stable identity: versioned **HNSW sidecar** in `.calm/vector/` (manifest:
  `embedding_space_version`, `model_digest`, `dimension`, `graph_generation`,
  `vector_count`, `index_checksum`) — no SQLite C-extension (keeps musl portability).
  Exact scan stays as oracle/fallback (`if vectors < threshold: exact else ANN`).
- Batch embedding writes + true ONNX batching (low-priority, opt-in backend only).
- **Task-aware Context Composer under `understand`/`edit_context`/`plan_change`** — **no
  41st tool** (the audit §22 is right; the repo just spent a commit fixing 39→40 doc
  drift). Progressive disclosure L0-L4, utility = relevance × evidence × task × novelty ÷
  token cost, and output **must** carry `omitted_verified_callers`,
  `omitted_possible_callers`, `omitted_symbols`, `token_estimate`, `coverage`.
- **DoD:** ANN `recall@10` ≥ predefined floor vs exact KNN at 10k/100k/1M vectors ·
  p50/p95/p99 latency + RSS floors met · **no graph truth depends on semantic
  similarity** · token/task benchmark improves.

---

### Parallel lane — Change-control plane (runs alongside Waves 1-6)

Rebased onto the current state-schema version (do **not** copy the blueprint's old
version numbers). Blueprint: `docs/plans/2026-08-08-master-change-control-execution-blueprint.md`.

- **Multi-file ChangeSet (P0 of this lane):** `ChangeSet / ChangeSetFile / StagedArtifact
  / CommitAttempt / RecoveryAction`; state machine
  `PLANNED→STAGED→COMMITTING→{APPLIED, PARTIALLY_APPLIED, NOT_APPLIED}`; **"unknown whether
  written" is a forbidden state.** One reconcile + one semantic update + one verification
  per ChangeSet, not per file.
- **ExecutionBroker for verification:** network deny-by-default, repo read-only + scratch
  read-write, env allowlist, wall/CPU/memory/process-count caps, process-tree kill; cold
  dependency fetch is an **explicit prefetch stage**, never implicit network in the
  verifier. Per-language adapters (Rust/Go/TS/Python) behind the one policy.
- **VerificationReceipt** binding `source_tree_digest` + `staged_tree_digest` +
  toolchain/command/policy digests; invalid if source changed mid-verify. A change is
  `DONE` only when committed **AND** index reconciled **AND** required verification
  complete **AND** attestation emitted.
- After dogfood: `kernel_enforced_writes` **default-on where supported** (Linux x86_64 +
  primitive present), else explicit `write_containment: "textual_fallback"` +
  `warning: weaker_containment` surfaced to the agent — never treat the two postures as equal.
- **Full change-kind `ObservedChangeKind`** taxonomy (CommentOnly/FormattingOnly/LocalBody/
  ControlFlow/DataFlow/Visibility/Signature/Deletion/PublicApi/DependencyManifest/
  SecuritySensitive/TestOnly/Generated). `SecuritySensitive` from path policy + known
  auth/config/CI/migration paths + sensitive-API/effect touches. **No taint engine yet.**
- **Retire lexical `reason` grounding** in phases: A) `cites` preferred, `reason`
  deprecated → B) structured authority OR exact `cites` required → C) `reason` is
  explanation-only, never permission.
- **Keep agent-relay downgraded** (`review_decide_via_agent_relay` stays default-off,
  weaker than independent TTY review); attestation records the review mechanism.
- **DoD:** crash-injection matrix proves no `PARTIALLY_APPLIED` is undetectable; verifier
  runs network-denied by default; receipt invalidates on mid-verify source change.

---

### Cross-cutting — Agent-truthfulness seam (cheap, do opportunistically in Wave 0-1)

- Standardize `ToolMeta { snapshot_id, graph_generation, coverage, warnings, omitted,
  degraded }` and `ToolOutcome<T> { data, meta }`. Route `telemetry.rs:28-36`'s injection
  hits into `meta.warnings` so the **consuming agent** sees `warning: untrusted_content`,
  not just the operator's logs. No mutation of source text required.

---

## Part C — First-10-PR critical path (refined from audit §45)

Sequenced so each PR unblocks the next and none builds on identity/evidence debt:

1. **`ci-local.sh` + CI invokes it** — kills the local≠CI churn (Wave 0.1).
2. **B2 oracle correctness fix** — fix the ruler before measuring (Wave 0.2).
3. **WS7A: preserve `definition_symbol` target identity** (Wave 0.3) — the P0 core.
4. **WS7B: evidence conflict as measurable state + D8 regression** (Wave 0.4).
5. **Issue #72: `QualifiedReference` external roots** (Wave 0.5).
6. **AmbiguityGroup target-aware membership** (Wave 0.6).
7. **`pipeline.rs` resolver extraction — behavior-preserving** (Wave 1, first slice).
8. **`TypeId` / qualified type identity** (Wave 2).
9. **`CallSiteRevision` + `SymbolRevision`** (Wave 2).
10. **Evidence ledger v1 + `call_edges` compatibility projection** (Wave 3, first slice).

HNSW is deliberately **not** in the top 10 (audit §18 is right: ANN before stable
identity makes invalidation intractable). The bounded-heap KNN win *may* piggyback early
since it needs nothing.

---

## Part D — What NOT to do (adopted from audit §44, verified as still-correct discipline)

No full rewrite · no Neo4j/generic graph DB (SQLite stays) · no HNSW before identity ·
no semantic similarity in edge truth · no new same-dir/same-file heuristic without a
fixture · no arity generalization to unverified-grammar languages · no taint engine yet ·
no cross-repo heuristic call graph yet · no cloud control plane · agent-relay stays
non-default · **no 41st MCP tool** unless telemetry proves need · **no resolver tuning
against B2 until the oracle is fixed.**

---

## Appendix — Files that will carry the most change

- `crates/calm-core/src/indexer/pipeline.rs` — WS7A/B (0.3/0.4), #72 (0.5), the Wave-1
  split, and most of Wave 2-3. Currently the #67 hotspot; split it (Wave 1) before piling on.
- `crates/calm-core/src/resolver/{formal.rs,mod.rs}` — `FormalEdge` producer; Wave 0.3
  changes what it hands to the pipeline.
- `crates/calm-core/src/db/schema.rs` — evidence-ledger tables (Wave 3),
  `ambiguity_group_candidates` (0.6), identity columns (Wave 2).
- `crates/calm-server/src/tools/trace.rs` — `callers`/`callees`/`reference_impact` become
  projections over verdicts (Wave 3/4); ambiguity query by `symbol_id` (0.6).
- `crates/calm-core/src/embedding.rs` — bounded-heap KNN (cheap), HNSW sidecar (Wave 6).
- `crates/calm-server/src/telemetry.rs` + `tools/common.rs` — `ToolMeta`/`ToolOutcome`
  warning surface (cross-cutting).
- `benchmarks/resolution_precision/` — grow corpus to ≥50 gating sites, add D8-class
  fixtures, add `provider_conflict_rate` (Wave 0).
- `scripts/ci-local.sh` (new) + `.github/workflows/ci.yml` — Wave 0.1.

---

## Part E — Execution-ready specs for PR#6–10 (verified against real code 2026-08-19)

> Wave 0 (PR#1–5) is **SHIPPED**. This part turns the remaining critical-path PRs from
> Part B outlines into build-ready specs, each anchored to real `file:line` and — most
> importantly — each stating **what already exists** so we extend rather than rebuild.
> Reading the real code surfaced three facts that change the plan (flagged **⚠ CORRECTION**
> below). Verified at base `903d3ef` + Wave 0.

### PR#6 — AmbiguityGroup target-aware membership (Wave 0.6) · size **M** · gate: **high-risk** (`trace.rs` read queries)

**The bug (confirmed).** An overflow "too many same-named candidates" group is keyed by the
**bare callee name** and both consumer queries match on that bare name, so the caveat leaks
across unrelated symbols:
- Construction: [`resolve_sites_to_edges`](../../crates/calm-core/src/indexer/pipeline.rs#L1866) pushes
  `AmbiguityGroup { candidate_group_key: callee.clone(), .. }` — bare name, explicitly documented
  "not an identity" ([pipeline.rs:1292-1295](../../crates/calm-core/src/indexer/pipeline.rs#L1292-L1295)).
- Consumers: `callers()` [trace.rs:68-70](../../crates/calm-server/src/tools/trace.rs#L68-L70) and
  `reference_impact` [trace.rs:887-888](../../crates/calm-server/src/tools/trace.rs#L887-L888) both do
  `WHERE candidate_group_key = ?1` bound to `c.name` (bare). A Python `helper` inherits a Rust
  `helper` group's caveat.

**⚠ CORRECTION to Part B ("query by `symbol_id`").** The overflow branch cannot key by a resolved
`symbol_id` because *by definition none of the candidates was picked*. But the candidate **set is
known and then thrown away** — the same "identity collapsed to a scalar" pattern as the P0 bug:
[pipeline.rs:1834](../../crates/calm-core/src/indexer/pipeline.rs#L1834) returns
`(Vec::new(), false, false, Some(t.len()), None)`, discarding `t` (the surviving candidate
`Vec<(qualified_name, path)>` from [pipeline.rs:1559](../../crates/calm-core/src/indexer/pipeline.rs#L1559))
and keeping only its length. So the correct fix is **persist the candidate members**, then match a
queried symbol by exact `qualified_name`, not bare name.

**Two implementable options (recommend A):**

- **A — member table (fully target-aware; matches the P0 thesis).**
  - Schema: `CREATE TABLE ambiguity_group_candidates (group_id INTEGER NOT NULL REFERENCES
    ambiguity_groups(id) ON DELETE CASCADE, candidate_qn TEXT NOT NULL, candidate_path TEXT,
    rank_hint INTEGER)` + `INDEX(candidate_qn)`. Additive `IF NOT EXISTS`, no version bump
    (same policy as `evidence_conflicts` in PR#4).
  - `AmbiguityGroup` struct ([pipeline.rs:1296](../../crates/calm-core/src/indexer/pipeline.rs#L1296))
    gains `candidates: Vec<(String, Option<String>)>`; thread `t` through the overflow branch
    instead of dropping it at :1834; `insert_ambiguity_groups_batch`
    ([pipeline.rs:1930](../../crates/calm-core/src/indexer/pipeline.rs#L1930)) inserts children.
    CASCADE + the two existing DELETE scopes (`rebuild_graph`
    [:2012](../../crates/calm-core/src/indexer/pipeline.rs#L2012), `incremental_graph_update`
    [:2233](../../crates/calm-core/src/indexer/pipeline.rs#L2233)) already clean children for free.
  - Queries: join and match `WHERE agc.candidate_qn = ?1` bound to `c.qualified_name`.
- **B — language column (minimal; kills only the cross-language collision).** Add `language TEXT`
  to `ambiguity_groups` (derive from `from_path` via `language_for_extension` at insert), filter
  `AND language = ?2`. Cheaper, but a same-language different-module `helper`↔`helper` still leaks.
  Satisfies the *stated* DoD (cross-language) but not the thesis. Fallback only if scope must shrink.

**Test:** cross-language fixture — a Python file with a >`MAX_CALLEE_CANDIDATES` `helper` overflow
and an *unrelated* Rust `helper` symbol; assert `callers(rust::helper).caveat` is `None`
(option A also: `callers(python::helper)` still gets it). Keep green:
`test_overflow_candidates_recorded_as_ambiguity_group_not_dropped_silently`
([pipeline.rs:5246](../../crates/calm-core/src/indexer/pipeline.rs#L5246)) and
`callers_reports_unresolved_ambiguity_groups` ([tools.rs:6172](../../crates/calm-server/src/tools.rs#L6172)).

**DoD:** cross-language leak = 0; WS3 "dropped site is recorded" tests still pass;
`resolution_precision` unchanged (this touches only the caveat surface, never `call_edges`).

**Risk/rollback:** read-path + additive schema only; no edge semantics change. Revert = drop table +
restore bare-name query. **Gate:** `trace.rs` is risk-high → needs one out-of-band TTY
`calm review approve <id>` for the query edits.

---

### PR#7 — `pipeline.rs` behavior-preserving split (Wave 1, issue #67) · size **L** · own session

**Reality of the file.** ~7,200 lines: **non-test logic ends ≈ line 3687**, tests run 3696→7202.
So the "6k-line" figure is ~3.7k lines of logic + ~3.5k lines of in-file tests. Two functions
dominate and are the real work:
- `extract_file_data` [423–903](../../crates/calm-core/src/indexer/pipeline.rs#L423) (~480 lines)
- `resolve_sites_to_edges` [1304–1924](../../crates/calm-core/src/indexer/pipeline.rs#L1304) (~620 lines)

**Verified extraction seams (already cleanly grouped by concern — move-only, one PR each):**

| New module | Functions (line ranges) |
|---|---|
| `pipeline/discovery.rs` | `read_source_capped` 49, `collect_source_files` 133, `hash_content`/`mtime_secs`/`rel_path`/`upsert_file_index` 154–291 |
| `pipeline/extraction.rs` | `formally_resolved_names` 400, `extract_file_data` 423–903, `persist_file` 920 |
| `resolver/context.rs` | `build_resolution_context` 1033, `build_inheritance_closure` 1157, `resolve_via_inheritance_closure` 1246 |
| `resolver/reconcile.rs` | `resolve_sites_to_edges` 1304–1924, `insert_ambiguity_groups_batch` 1930 |
| `resolver/modules.rs` | `resolve_import_targets` 2300 … `normalize_rel` 2761 (module/path resolution cluster) |
| `pipeline/graph.rs` | `rebuild_graph` 1950, `rebuild_graph_from_index` 2052, `incremental_graph_update` 2109, `refresh_caller_counts` 2285 |
| `pipeline/driver.rs` | `run_indexing_pipeline*` 2799–2824, `reindex_*` 2834–2975, 2990, 3335, 3530 |
| `resolver/cache.rs` | `cached_formal_resolver` 3024, `cached_resolution_maps` 3092, `invalidate_resolution_maps_cache` 3157 |
| `pipeline/identity_migration.rs` | `needs_call_site_identity_baseline` 3169, `record_call_site_identity_migration_status` 3186, `rebuild_call_site_identity_baseline` 3247 |

Extract in **dependency-leaf order** (discovery → extraction → modules/context → reconcile →
graph → driver) so each PR compiles against already-moved deps. Keep every function `pub(crate)`
at first; tighten visibility only in a final no-op pass. Tests stay in `pipeline.rs` until logic
is fully split, then move alongside.

**Safety oracle (already built).** [`tests/golden_graph_equivalence.rs`](../../crates/calm-core/tests/golden_graph_equivalence.rs)
compares a *continued* vs a *fresh* index by **semantic keys only** (never DB id/timestamp/order),
keyed on full call-site identity + SCIP proof provenance. **DoD per extraction PR:** golden
equivalence green · `resolution_precision` byte-identical before/after · B2 identical · zero net
logic diff (`git diff` shows moves + `use` only). **No `resolve_sites_to_edges` behavior change
lands in this wave** — TypeId (PR#8) is the first semantic change and rides *on top of* the split.

**Gate:** each slice touches risk-high `pipeline.rs` → one TTY approval per PR. Keep PRs small
(one module each) so each approval covers a reviewable move.

---

### PR#8 — `TypeId` / qualified type identity (Wave 2) · size **L** · depends on PR#7

**What exists.** Receiver type is a **bare string**: `target_class: Option<String>` on
`CallSiteData` ([pipeline.rs:302](../../crates/calm-core/src/indexer/pipeline.rs#L302)), column
`target_class TEXT` ([schema.rs:253](../../crates/calm-core/src/db/schema.rs#L253), migrated
[:1038](../../crates/calm-core/src/db/schema.rs#L1038)), persisted via `persist_file`
([:941/:956](../../crates/calm-core/src/indexer/pipeline.rs#L941)), and **consumed at ~15 sites
inside `resolve_sites_to_edges`** (1404, 1460, 1546, 1559, 1681, 1704, 1722, 1727, 1736, 1742).
A bare `User` cannot distinguish `com.foo.User` from `com.bar.User`.

**Why it must follow PR#7.** All ~15 consumers live in the 620-line `resolve_sites_to_edges`.
Changing `target_class` before that function is extracted into `resolver/reconcile.rs` means
editing the single riskiest span in the repo. Split first, then evolve the isolated module.

**Mechanism.** Introduce `TypeRef = Resolved(TypeId) | External(ExternalTypeId) | Unresolved(TypeShape)`
where `TypeId { language, package_or_module, lexical_scope, qualified_name, declaration_symbol_id }`.
**`External` is a first-class state**, not "local lookup missed" — this is the type-level twin of
PR#5's `external_crate_root`. Add `target_type` **alongside** `target_class` first (dual-write),
migrate consumers one branch at a time behind the reconcile module, then drop the string. Ambiguous
identity (two packages, same bare name) **fails closed to conflict**, never a guess.

**Test:** `com.foo.User` vs `com.bar.User` (Java) and `foo.Base` vs `bar.Base` (Python) adversarial
fixtures resolve to distinct targets or conflict — never silently to one. **DoD:** no same-bare-name
different-package confident edge; `resolution_precision` `false_confidence_rate` non-increasing.

---

### PR#9 — Position-independent call-site identity (Wave 2) · size **L** · depends on PR#7

**⚠ CORRECTION to Part B ("add `CallSiteKey`/`CallSiteRevision`").** Call-site identity is **not
missing — it already exists at v2** and is byte-span based:
- `call_sites` carries `callee_start_byte`, `callee_end_byte`, `identity_version`
  ([migrate_call_site_identity_v2, schema.rs:1268](../../crates/calm-core/src/db/schema.rs#L1268));
  unique identity = `(from_path, enclosing_qn, callee_start_byte, callee_end_byte, edge_kind,
  identity_version)` ([schema.rs:1292-1303](../../crates/calm-core/src/db/schema.rs#L1292-L1303)).
- `external_proofs` anchors durable proof to that identity + `source_file_hash`
  ([schema.rs:266-283](../../crates/calm-core/src/db/schema.rs#L266-L283)), CASCADE-deleted when the
  `call_sites` row is.
- A full migration harness already exists: `needs_call_site_identity_baseline`
  ([pipeline.rs:3169](../../crates/calm-core/src/indexer/pipeline.rs#L3169)) →
  `rebuild_call_site_identity_baseline` ([:3247](../../crates/calm-core/src/indexer/pipeline.rs#L3247))
  → `record_call_site_identity_migration_status` ([:3186](../../crates/calm-core/src/indexer/pipeline.rs#L3186)),
  emitting `GraphMode::FullFallback("call_site_identity_v2")`.

**The real, narrower gap (audit §37, now precise).** The v2 key is **byte-ABSOLUTE**. Inserting a
comment *above* a call site (same function, unchanged call) shifts `callee_start/end_byte` → identity
changes → `external_proofs` CASCADE-deleted → **the proof re-verifies from scratch though nothing
about the call changed** (proof churn). So PR#9 is not "build identity"; it is "make the existing
identity position-independent": add a **v3** identity whose offsets are **relative to the enclosing
symbol's start byte** (or an AST-path/normalized-subtree key), so edits *outside* the enclosing
symbol don't perturb it.

**Reuse, don't rebuild.** Bump `identity_version` 2→3, add the relative-offset columns, and drive
the reparse through the **existing** `needs_call_site_identity_baseline`/`rebuild_call_site_identity_baseline`
path — it already handles "detect stale identity version → forced full baseline → status rows →
`FullFallback` graph mode." Update `tests/golden_graph_equivalence.rs`'s `CallSiteKey`/`ProofKey`
([golden_graph_equivalence.rs:22-46](../../crates/calm-core/tests/golden_graph_equivalence.rs#L22-L46))
in lockstep (they key on the byte offsets today).

**Test:** index a fixture; snapshot `external_proofs`; insert a comment line *above* a proven call
site; reindex; assert the proof row for that call site is **unchanged** (`status='fresh'`, same
identity) — today it churns. **DoD:** upstream-insertion proof churn = 0; golden equivalence green;
`SymbolRevision` (declaration/signature hash) added for signature-vs-body change discrimination.

---

### PR#10 — Evidence ledger v1 + `call_edges` compatibility projection (Wave 3, first slice) · size **L**

**What exists (2 of the ledger's tables are already seeded).**
- `external_proofs` ([schema.rs:266-283](../../crates/calm-core/src/db/schema.rs#L266-L283)) already
  is a per-provider evidence row: `provider`, `provider_fingerprint`, `context_fingerprint`,
  `source_file_hash`, byte span, `graph_generation` fence, `definition_snapshot`, and a
  `status IN (fresh|stale|legacy|unverified|rejected)` lifecycle. This **is** the "SCIP machinery to
  keep" — evidence ledger v1 is a **generalization of this table into `reference_evidence`**
  (add `disposition supports|excludes`, `authority_class`, `observed_revision`), not a greenfield build.
- `evidence_conflicts` (shipped PR#4) is already the ledger's conflict table.

**The one behavioral flip.** Today providers **mutate `call_edges` directly** — the concrete site is
`scip/ingest.rs::insert_missing_exact_edges` (the function PR#3 guarded). Rule A: providers **append
evidence**; a reconciler materializes `reference_verdicts`; `call_edges` becomes a **materialized
projection** so the 40 MCP tools keep working unchanged through the migration. First slice = stand up
`reference_evidence` (migrate `external_proofs` inserts to also write it) + `reference_verdicts` +
the projection that reproduces today's `call_edges` byte-for-byte under golden equivalence. Authority
(Rule B): SCIP proof outranks a same-dir heuristic, but contradicting a local lexical binding /
explicit import / package rule / explicit receiver type is **conflict**, not silent overwrite.

**DoD:** with the projection active, `resolution_precision` + B2 + golden equivalence are all
identical to pre-PR#10 (proves the projection is faithful) · every `verified` edge is explainable
(which evidence/provider/revision) · no provider writes `call_edges` directly anymore.

---

### Revised sequencing note

PR#8 and PR#9 both depend on PR#7 (they edit code that lives inside the not-yet-split
`resolve_sites_to_edges` / `pipeline.rs`), and both are independent of each other — so after the
split they can proceed in either order or in parallel. PR#10 depends on PR#9's stable revision
(evidence must bind to a churn-free identity). Net critical path is unchanged from Part C; the
correction is that **#9 is "evolve v2→v3", not "introduce", and #10 "generalizes `external_proofs`",
not "creates a ledger"** — both materially smaller than the Part B wording implied.
