---
title: "Master Change-Control Kernel — execution blueprint (audited & re-sequenced against live source)"
date: 2026-08-08
status: >
  Audit + rewrite of a large forward-looking "Change Control Kernel" blueprint. Every 'current
  state' claim in the source blueprint was re-verified against live code this session (file:line
  cited in §1). Two claims were OUTDATED by the PR A–E train that shipped the same day
  (INT-02 digest hardening, #66 guarantee-contract mapping); one is PARTIAL (#65 graph_generation
  binding). All other primitive-level claims CONFIRMED. This doc supersedes the raw blueprint as the
  execution-ready record. No feature code written here.
verified_against: HEAD 5fca13e (main), this session.
naming: >
  Uses the CCK-NN namespace (Change-Control Kernel) to avoid colliding with the in-flight PR A–E /
  P1–P9 lettering of 2026-08-08-derived-artifact-hardening-execution-plan.md (the "derived plan"),
  which owns the indexer/evidence train this doc only references.
inputs:
  - "(source blueprint, session-local — this doc is its durable, corrected form)"
  - docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md   # owns INT / P5–P9 evidence work
  - docs/plans/2026-08-07-pecorino-adoption-roadmap.md                   # design source for Tier1–4
  - docs/plans/2026-08-05-state-db-rewiring-execution-plan.md            # the incident PR-CCK-01 hardens
---

# Master Change-Control Kernel — execution blueprint

## §0. Executive verdict

The source blueprint's **architecture is sound and its factual grounding is unusually accurate** —
17 of 19 load-bearing "current state" claims verified byte-for-byte against live source. It is *not*
a rewrite proposal; it is an evolution of primitives CALM already has. Adopt it, with three
corrections and one structural change:

1. **Two claims are outdated** (both closed by the PR A–E train that landed 2026-08-08, same day):
   - `#66` "catalog checks presence/doc drift but doesn't map enforced→behavior test" — **PR E already
     maps every `level="enforced"` entry to a `test` fn and asserts the fn exists**
     (`crates/calm-core/tests/guarantee_contract_coverage.rs`, commit 5fca13e). The blueprint's PR-02
     must be re-scoped from *build the mapping* to *deepen the mapping from fn-exists to a real
     behavior contract*.
   - INT-02 "digest P5: dedupe facts, canonical stable sort, bump `GRAPH_DERIVATION_VERSION`" — **PR B
     already did exactly this**; `GRAPH_DERIVATION_VERSION = 4` with dedup + canonical sort
     (`crates/calm-core/src/graph/digest.rs:415,607,624`). INT-02 is effectively done; only its
     benchmark kill-criterion (C≈B) remains.

2. **One claim is partial:** `#65` — PR D already bound `graph_generation` into the review as a hard
   gate (`STALE_GRAPH_AUTHORITY`, `crates/calm-server/src/tools/edit.rs:400,1391`). That is **1 of the
   ~9 staleness fields** the blueprint's §12 lists. The authority *snapshot* (the other 8 fields, plus
   persistence, single-use, signature) is still greenfield. Keep PR-CCK-09, minus graph_generation.

3. **Structural change:** the blueprint reuses the `PR-00..PR-31` numbering that visually collides with
   the derived plan's `PR A–E`. This doc renames to `CCK-00..CCK-31` and hands the entire
   indexer/evidence train (INT-01..04, Parts XIV/XV/XVI) back to the derived plan, which already owns
   it and is more precisely scoped.

**The one thing to internalize:** most of the *runtime enforcement* the blueprint wants already exists
and is correct — the gate error codes (`EDIT_CONTEXT_REQUIRED`, `STALE_CALLER_SET`,
`STALE_GRAPH_AUTHORITY`, `CONFIRM_REQUIRED`, `REASON_NOT_GROUNDED`, `UNCERTAIN_ZERO_CALLER`,
`HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`, `TRANSACTION_INIT_FAILED`) are all live in `edit.rs`. What is
missing is **durability** (authority is session-local, in a `HashMap`, not persisted) and
**structure** (authority is not a signed, single-use, snapshot-bound object). That framing tightens the
whole train: Phase 0–1 is not "add safety", it is "make the safety we already enforce *durable and
reproducible*."

---

## §1. Audit table — every claim, verified against live source

Legend: ✅ CONFIRMED · ⚠️ OUTDATED · 🟡 PARTIAL/NEEDS-ADJUST

| # | Blueprint claim | Verdict | Evidence (file:line) |
|---|---|---|---|
| 1 | state.db is schema **v1**; no ALTER migration path; forward = re-run idempotent `IF NOT EXISTS` DDL | ✅ | `db/schema.rs:24,34,69-74` |
| 2 | `init_state_db_versioned` refuses newer DB, else stamps version | ✅ | `db/schema.rs:35-53,69-74` |
| 3 | `EditContextReview` is **session-local**, binds caller-set digest + call-count freshness | ✅ | in-mem `Arc<Mutex<HashMap<u64,SessionSummary>>>` `tools/common.rs:230,451`; digest `tools/guardrails.rs:396`; no state.db INSERT anywhere |
| 4 | `#65` open: review binds only caller-set digest, not full authority snapshot | 🟡 | Issue **OPEN**; but PR D bound `graph_generation` too → `STALE_GRAPH_AUTHORITY` `tools/edit.rs:400,1391`. 1/9 §12 fields done |
| 5 | `#66` open: catalog checks presence/doc drift, doesn't map enforced→behavior test | ⚠️ | Issue **OPEN**, but PR E maps every enforced entry → `test` fn + asserts fn exists `crates/calm-core/tests/guarantee_contract_coverage.rs`; `docs/guarantee-levels.toml` uses `test=` not `contract_test=` |
| 6 | `sanitize_source_output(&str) -> String`; `source()` etags raw, redacts body, can still suggest whole-symbol edit on raw etag | ✅ | `sanitize.rs:82` |
| 7 | `path_policy.rs` symlink check is **textual, not TOCTOU-safe**; `openat2(RESOLVE_BENEATH)` is a documented follow-up | ✅ | `path_policy.rs:28-34` |
| 8 | atomic writer: random nonce + `create_new(true)` + temp `sync_all` + `rename` + Unix parent-dir fsync; durability after rename **best-effort** | ✅ | `edit.rs:556,581-597,619-670`; `fsync_parent_dir` Err ignored `edit.rs:670-675` |
| 9 | `verify.rs`: `cargo check` runs directly, can execute `build.rs`/proc-macros/fetches; has timeout, output caps, Unix process-group kill | ✅ | `verify.rs:18,70-120,157` |
| 10 | `verify_change` == cargo-check-of-a-tx | ✅ | `tools/txn.rs:411-424,554` `run_cargo_check` |
| 11 | `batch_status` aggregates caller-provided tx IDs, no server-side logical change-set | ✅ | `tools/txn.rs:97-110` |
| 12 | `EdgeConfidence` ladder = Formal/Resolved/Inferred/Textual/Ambiguous/Unresolved (mixes dimensions) | ✅ | `types.rs:32-81` |
| 13 | `external_proofs`, `call_sites`, `call_edges` tables exist; `call_edges` is materialized answer | ✅ | `db/schema.rs:109,161,184` |
| 14 | `SessionSummary.reviewing_symbol` is advisory-only | ✅ | `tools/common.rs:571-578` |
| 15 | `#67` open: `indexer/pipeline.rs` is the largest remaining hotspot | ✅ | **7244 lines**; issue OPEN |
| 16 | `FRESHNESS_WINDOW_CALLS = 200`; UX TTL, not an authorization proof | ✅ | `tools/edit.rs:1254` |
| 17 | `config.risk_rules` exist and compile to policy floors | ✅ | `config.rs:38,131,986` |
| 18 | topology risk has caller/hub/signature/path-rule signals; change-**kind** is the gap; B14 exists | ✅ | B14 bench commit `b5b91ab`; `config.rs` path rules |
| 19 | INT-03 Maven `pom.xml` + `package_units` is genuinely next (not done) | ✅ | `indexer/jvm_package.rs` is *import resolution* only (reads `package` decls); no pom dep parse; T4a shipped Cargo/npm/go/py only |
| 20 | INT-04 bundle: replace boolean `force_full_reindex` with typed `ReconcilePlan` | ✅ | `bundle.rs:144` boolean field |
| 21 | facade tools (`plan_change`/`review_change`/`apply_change`/`run_verification`/`change_status`) | ✅ absent | greenfield; none present in `crates/calm-server/src` |

**Additional accuracy notes discovered during audit (not in the blueprint):**

- **index.db already has an incremental ALTER-migration helper** — `migrate_add_column(conn, table,
  col, type)` (`db/schema.rs:878-906`, used for `external_proofs`). CCK-01 should *mirror this proven
  pattern* for state.db rather than invent a new one, and note that `run_migrations` is index.db's
  path while state.db has none — that asymmetry is the actual gap.
- **`b677a9e` already added `STATE_DB_SCHEMA_VERSION` + downgrade guard.** So CCK-01 is *not*
  "add versioning" (done); it is "add the *forward migration executor* the version marker implies."
  A retained fixture may be at `user_version = 0` (unstamped, pre-`b677a9e`) **or** `1` — the
  migrator must treat both as the v1 baseline. `refuse_if_schema_newer` already accepts `<= expected`.
- **`PRAGMA user_version` is transactional** in SQLite — setting it inside `BEGIN IMMEDIATE` and
  crashing before `COMMIT` rolls the stamp back with the DDL. The blueprint's "stamp only after
  success, all in one txn" contract is therefore *correct and cheap*; affirm it, don't second-guess it.

---

## §2. What survives unchanged (the constitution & target shape)

The 12 invariants (§ blueprint Part I) and the target module/service layout (Parts II) are **kept
verbatim** — they are good north stars and cost nothing to adopt as review criteria. The two most
load-bearing invariants for the near-term train:

- **#2 No stale evidence may grant authority** and **#3 Natural language is never a permission
  primitive.** These are the reason the authority object must become a signed, snapshot-bound,
  single-use record instead of a `HashMap` entry keyed by a free-text `reason`.
- **#9 Every `enforced` guarantee has ≥1 executable contract test.** PR E delivered the *shallow*
  form (fn-exists). CCK-02 delivers the *deep* form.

Kept design decisions worth re-affirming because they are cheap and correct:
- Module boundaries before crate boundaries (don't split Cargo crates speculatively).
- ChangeSet **composes** existing per-file `EditTransaction`; it does **not** replace `txn.rs`.
- Dedicated `.calm/control.key` (0600) with per-purpose HMAC domain separators, distinct from
  `memory.key`.
- Enrichments (embeddings, ranking, digest, hotspots, memory) may inform planning but must never be
  on the critical path of a mechanically safe edit (blueprint Part XXX). This is the single best
  safety-by-construction lens and should gate every future PR.

---

## §3. Corrected & re-sequenced PR train

Each PR carries: **Status** (TODO / PARTIAL / DONE-elsewhere), what to create/modify (file-level),
required tests, and graduation. PRs marked DONE/PARTIAL are annotated with what actually remains.

### PHASE 0 — Durable, reproducible foundation

#### CCK-00 — Land constitution + blueprint · **Status: TODO (this doc is the artifact)**
- Create `docs/architecture/change-control-kernel.md` (the 12 invariants + target diagram) and treat
  *this file* as the master execution blueprint (the blueprint's PR-00 target filename).
- Modify `CONTRIBUTING.md` + `.github/pull_request_template.md`: add the 6 review checkboxes (trust
  boundary / state schema / guarantee IDs / analyzer lifecycle / authority semantics / crash test).
- Tests: doc-drift check only (reuse `scripts/gen-status.sh --check` style).
- Depends: none.

#### CCK-01 — Real forward migrations for `state.db` · **Status: TODO (highest priority — blocks all durable features)**
- **Reframe:** versioning exists (`b677a9e`); the *executor* does not. Mirror index.db's
  `migrate_add_column` helper style.
- Create `crates/calm-core/src/db/state_migrations.rs` with:
  ```rust
  pub struct StateMigration { pub from: i64, pub to: i64, pub name: &'static str,
      pub apply: fn(&rusqlite::Connection) -> rusqlite::Result<()> }
  pub fn migrate_state_db_to_current(conn: &Connection) -> rusqlite::Result<()>;
  ```
  Contract per step: `BEGIN IMMEDIATE` → apply N→N+1 → verify postcondition → `PRAGMA
  user_version=N+1` → `COMMIT`. Stamp only on success (transactional, see §1 note).
- Modify `db/schema.rs` (`init_state_db_versioned` calls the migrator between refuse-guard and stamp),
  `db/mod.rs`.
- Create `crates/calm-core/tests/state_schema_migrations.rs` + fixtures
  `crates/calm-core/tests/fixtures/state_db/v1.sqlite` (**and** a `v0`-unstamped variant).
- Tests: v0/unversioned→v1; v1→current no-op; newer rejected; forced SQL failure leaves
  `user_version` unchanged; restart-after-crash idempotent; real v1 fixture retains
  memory/tx/events/ledger; FK/PRAGMAs preserved.
- **Graduation:** after this PR, *no* durable state change may bypass `state_migrations.rs` (enforce
  via a CCK-02 contract test that reads the migration list).

#### CCK-02 — Deepen guarantee contracts (#66) · **Status: PARTIAL — PR E shipped the shallow layer**
- **Do not** create `scripts/check-guarantee-contracts.py` or a parallel `calm-server/tests/
  guarantee_contracts.rs` — that duplicates PR E. Instead **extend the existing mechanism**:
  - `docs/guarantee-levels.toml` already carries `test =` per enforced entry;
    `guarantee_contract_coverage.rs` already asserts the fn exists.
  - Add the *missing* enforced→behavior contracts as real tests where the cited fn is only a proxy,
    starting with the blueprint's list: `txn.begin_before_write`, `high_risk_edit.independent_review`,
    `verification.bound_to_proposed_digest`, `verification.timeout_secs`,
    `ledger.hmac_signed_not_plain_hash`, memory quarantine, remote-HTTP restriction, edit-context gate.
  - Add a coverage assertion that `state_migrations.rs` is the only writer of `user_version` for
    state.db (closes CCK-01's graduation).
- **Closes:** the deep half of #66. Keep #66 open until every enforced entry's `test` is a behavior
  assertion, not a presence proxy.

#### CCK-03 — Public write-gate refusal E2E (#63) · **Status: TODO (enforcement exists, coverage doesn't)**
- All 8 error contracts already fire in `edit.rs` (verified §1 rows 3–4). The gap is tests that drive
  the **public** `edit_lines`/`edit_symbol`, not the internal `classify_gate`.
- Create `crates/calm-server/tests/write_gate_refusals.rs` asserting each of:
  `EDIT_CONTEXT_REQUIRED`, `STALE_CALLER_SET`, `STALE_GRAPH_AUTHORITY`, `CONFIRM_REQUIRED`,
  `REASON_NOT_GROUNDED`, `UNCERTAIN_ZERO_CALLER`, `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`,
  `TRANSACTION_INIT_FAILED` via a real server + temp repo.
- **Closes:** #63.

#### CCK-04 — Lossless/Protected `SourceView` · **Status: TODO**
- Replace `sanitize_source_output(&str)->String` (kept as a thin compat shim delegating to
  `.text`) with a structured `SanitizedView { text, redactions }` + `SourceView { view_id,
  raw_digest, rendered_text, kind, protected_regions }`.
- Protected token `⟪CALM_PROTECTED:v1:<sealed>⟫` binds {raw digest, byte range, class, expiry, HMAC
  under `.calm/control.key` domain sep}. Round-trip: verify token → reread raw → verify digest →
  substitute original bytes → *then* hash-check/write. Failure → `PROTECTED_SOURCE_VIEW_INVALID`.
- Modify `sanitize.rs`, `tools/inspect.rs`, `tools/edit.rs`, `tools.rs`, `__toolsnaps__/source.snap`;
  create `crates/calm-server/src/services/source_view.rs`.
- Tests: credential-free→lossless; credential→protected; token round-trips without exposing secret;
  stale/forged/partially-edited token rejected; edit outside secret preserves secret byte-for-byte;
  direct overwrite of a protected region requires escalation; CRLF survives; line-number rendering
  doesn't alter digest semantics.
- **Hard gate:** zero known lossy read→write regressions.

#### CCK-05 — `RootedFilesystem` + `WriteReceipt` · **Status: TODO (path_policy is textual today, §1 row 7)**
- Create `crates/calm-core/src/fs/{mod,rooted,atomic}.rs`. Linux backend: root `dirfd` + `openat2`
  (`RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS`, optional `RESOLVE_NO_SYMLINKS` strict). Other platforms:
  `containment_strength = strong | best_effort`, surfaced in status/attestation — never claim
  Linux-equivalence when the OS can't back it.
- `WriteReceipt { path, base_digest, result_digest, content_synced, directory_synced: Option<bool>,
  metadata_preserved, containment_strength }`. Dir-fsync failure *after* rename → `durability =
  uncertain`, **not** a generic "write failed" (fixes the best-effort gap at `edit.rs:670-675`).
- Modify `lib.rs`, `path_policy.rs` (keep as compat re-export), `edit.rs`, `tools/edit.rs`.
- Tests (Linux): symlink swap race, `..`, in-root symlink, external symlink, magic link, rename race,
  hardlink detection, parent-dir fsync failure injection. Cross-platform: containment report,
  permissions, read-only file, exec-bit preservation.
- **Graduation:** new safety-critical writes may no longer call `std::fs::write/rename` directly.

**PHASE 0 GRADUATION:** 100% state migrations from every retained fixture · 0 destructive lossy-source
round-trips · every enforced guarantee mapped to a *behavior* test · public refusal branches exercised
· no path-containment regression. No ChangeIntent hard enforcement yet.

**Parallelizable in Phase 0:** CCK-01, CCK-02, CCK-04, CCK-05 are independent after CCK-00. CCK-03
depends only on the existing gate. (Blueprint's Group A.)

---

### PHASE 1 — Evidence-bound, durable authority

#### CCK-06 — `EvidenceSnapshot` (compute-only) · **Status: TODO**
- Create `crates/calm-core/src/authority/{mod,snapshot.rs}`. Compute only; no schema yet.
  `snapshot_id = SNP-SHA256(canonical fields)`; `source_catalog_digest = SHA256(sorted(path\0hash))`
  from `file_index`. `freshness_class ∈ {reconciled, current, degraded}`.
- **Adjustment (from derived plan C1):** reconciliation plumbing already exists —
  `index_input_state` + `index_input_drift()` + `INDEX_INPUT_STATE_POLICY_VERSION` drive startup
  reconcile (`lib.rs:312`, `refresh.rs:315`). Snapshot's `freshness_class` should *read* that state,
  not reinvent it. High-risk authority must force reconciliation; watcher health alone is not proof.
- Tests: deterministic digest; row-order invariance; graph-generation / provider-state / config
  change each flips the digest; stale/degraded is explicit.

#### CCK-07 — state.db **v2**: Snapshot + ChangeIntent persistence · **Status: TODO (first schema bump through CCK-01)**
- Create `change/{mod,intent,store}.rs`. Modify `state_migrations.rs` (add v1→v2), bump
  `STATE_DB_SCHEMA_VERSION` 1→2. Tables: `evidence_snapshots`, `change_intents`,
  `change_intent_targets` (blueprint §5–6 SQL is well-formed; adopt as-is).
- Required migration fixture: real v1 state.db → v2, old tx replay unchanged, memory/ledger unchanged,
  new intent insert succeeds.

#### CCK-08 — ChangeKind + RiskVector + PolicyEngine (**shadow**) · **Status: TODO**
- Split declared `ChangeIntentKind` from `ObservedChangeKind` (diff/AST classifier). `mismatch` →
  escalation, never silent accept. `RiskVector` (9 axes) with `aggregate_risk` kept for back-compat
  but PolicyEngine reads the vector.
- Create `change/classify.rs`, `policy/{mod,model,loader,evaluate}.rs`, `.calm/policy.toml` loader.
  Existing `config.risk_rules` compile to policy floors (don't delete). Modify `config.rs`,
  `analysis/diff_impact.rs`, `tools/edit.rs`, `tools/guardrails.rs`.
- **Shadow only:** compute decisions, log disagreement, do not alter writes.
- Tests: metamorphic diff fixtures (whitespace/comment/body/signature/visibility/delete/add/manifest/
  test-only/declared-doc-vs-observed-code); determinism (same inputs+digest ⇒ byte-identical decision).

#### CCK-09 — state.db **v3**: `ReviewAuthority` (#65) · **Status: TODO minus graph_generation (PR D done)**
- Create `authority/{review,key}.rs`. Migrate v2→v3, bump 2→3. Tables `review_authorities`(+targets,
  +evidence). Signed (HMAC `.calm/control.key`), single-use nonce, `expires_at`, snapshot+intent+
  policy+risk digests bound. `ALTER TABLE edit_transactions ADD COLUMN authority_id`.
- **Adjustment:** §12's staleness set is 9 fields; `graph_generation` is already enforced live
  (`STALE_GRAPH_AUTHORITY`). Fold the existing check into the new object rather than re-implement, so
  there is exactly one authority-validation path.
- Tests: reject expired / forged HMAC / replayed single-use / wrong intent / changed target source /
  changed caller set / changed graph generation / changed provider state / changed analysis version /
  changed policy / wrong principal class.

#### CCK-10 — Integrate authority into current edit flow · **Status: TODO**
- `edit_context` becomes a compat wrapper: synthesize single-symbol ChangeIntent → capture Snapshot →
  compute Policy/Risk → mint ReviewAuthority; output gains `change_id`, `authority_id`,
  `authority_expires_at`. `edit_lines`/`edit_symbol` gain optional `change_id`/`authority_id`; new
  path validates authority. Old `edit_context+confirm+reason/cites` path stays byte-compatible.
- **Machine-authorization rule (invariant #3):** on the new path `reason` = explanation only,
  `authority_id` = permission. `reason` cannot grant authority.
- Tests: old path byte-compatible; new path rejects lexical reason as substitute; exact authority
  succeeds; consumed exactly once; stale refused.
- **Closes:** #65.

#### CCK-11 — `plan_change` + `review_change` tools · **Status: TODO (greenfield, §1 row 21)**
- Create `tools/change.rs`, `services/change_planner.rs`. `review_change` returns `authority_id` only
  after required human/MRTR approval. No write yet. New `change` toolset, not forced on all presets.

#### CCK-12 — Authority dogfood promotion · **Status: TODO (config, not architecture)**
- CALM self-repo runs `authority_mode = required`; external users keep compat during an observation
  window. Collect: stale-reject rate, human-veto rate, false-block reports, legacy-fallback usage.
  Flip external default to `structured` only after Phase-1 graduation. (Mirror the #64 shadow→enforce
  promotion pattern already used for calm-guard-dogfood.)

**PHASE 1 BENCHMARK GATE — `benchmarks/b15_change_reliability/`** (3 arms: native / legacy CALM /
ChangeIntent+Authority). **Graduation:** 0 stale-authority unsafe accepts · 0 forged/replayed accepts ·
100% hard authority decisions reproducible · no task-correctness regression vs legacy · legacy-fallback
usage declining in dogfood. Only then is structured authority the default.

---

### PHASE 2 — Multi-file ChangeSet

CCK-13 state.db **v4** (ChangeSet + staging schema; no repo mutation) → CCK-14 prepare/stage (resolve,
read, hash, syntax-check, stage to `.calm/staging/<id>/`, fsync; no source write) → CCK-15 commit
coordinator (composes existing per-file `EditTransaction`; one graph reindex after all files) →
CCK-16 crash-injection matrix (kill at every state × file boundary; canonical fixture 10 files × each
boundary × 100 runs; add CI job `changeset-crash-injection`) → CCK-17 recovery API (`RecoveryAction`,
`repair_consistency`; **no automatic recovery choice**; unsafe restore → `MANUAL_RECOVERY_REQUIRED`) →
CCK-18 `apply_change`/`change_status` facade (supersede `batch_status`/`edit_transaction_status` for the
new workflow; old tools stay expert/compat).

**Non-negotiable state-machine rule:** `PARTIALLY_APPLIED` is a *legitimate, explicit* state, not a
failed implementation. Forbidden outcomes anywhere: unknown / maybe-written / assume-rolled-back.

**PHASE 2 GRADUATION:** 0 silent partial applications · 0 lost file commits after crash · 100%
`PARTIALLY_APPLIED` detectable · 100% ChangeSet replay deterministic · single-ChangeSet reindex ==
full-reindex graph · task-correctness ≥ legacy.

---

### PHASE 3 — Verification infrastructure

CCK-19 `ExecutionBroker` (may run in parallel after CCK-05; malicious fixtures: fork-bomb, stdout/stderr
flood, network attempt, out-of-scratch write, forbidden read, timeout) → CCK-20 state.db **v5** + move
`verify.rs`→`verification/adapters/rust.rs` (delete `verify.rs` after a re-export transition; keep
`verification.timeout_secs` + `verification.bound_to_proposed_digest` guarantees passing) →
CCK-21A/B/C Go/TS/Python adapters (all via broker; no `Command::new` outside broker; "unsupported/
unavailable" is a valid state — do not invent a test command) → CCK-22 test-impact evidence
(coverage-derived; unknown coverage broadens, never silently narrows). Rename `verify_change(tx_id)` →
`run_verification(change_id, profile?)`, keep `verify_change` as a deprecated alias removed before v1.

- **Adjustment (broker × cargo network):** high-assurance default is `network=deny`, but a cold
  `cargo check` needs the registry. The Rust adapter must run against a pre-populated/vendored cargo
  cache (or a `prefetch` phase outside the deny-net profile). State this explicitly or the first
  high-assurance run fails spuriously.

**PHASE 3 GRADUATION — `benchmarks/b16_verification_selection/`:** 100% receipts bind exact result
tree · concurrent change during a run invalidates its receipt · high-assurance profile has no
untracked network exec · process-tree leak count = 0 · no verifier bypasses broker · selected-test
recall target set on external corpora *before* enforcement.

---

### PHASE 4 — Attestation

CCK-23 state.db **v6** + `ChangeAttestation` (`change/attestation.rs`, `services/attestation.rs`, CLI
`calm attest`/`calm verify-attestation`; local HMAC, optional external signing field — CALM core does
**not** implement cloud PKI). `change_status` returns the attestation ID; **do not** add a new MCP
tool unless telemetry proves need. Tests: change-after-verify → no attestation; missing receipt → no
attestation; stale authority → no attestation; tampered JSON → sig fail; cross-repo replay → fail;
deterministic payload digest.

**PHASE 4 GRADUATION:** a `DONE` ChangeSet ⟺ files committed + index reconciled + required verification
satisfied + attestation emitted. No other path may set logical `DONE`.

---

## §4. The indexer / evidence train is NOT re-owned here

Blueprint Parts XIV (INT-01..04), XV (pipeline split #67, LanguageAdapter, capability matrix), XVI
(provider evidence, prebuilt SCIP) are **handed back to
`2026-08-08-derived-artifact-hardening-execution-plan.md`**, which already owns them with precise,
source-verified scope. Corrections to fold into that plan:

| Blueprint item | Correction |
|---|---|
| INT-01 Go receiver field writes (P3b) | ✅ genuinely deferred — keep; needs correct receiver-identity tracking |
| **INT-02 digest P5 (dedup, canonical sort, version bump)** | ⚠️ **DONE by PR B** — `GRAPH_DERIVATION_VERSION=4`, dedup+canonical sort (`digest.rs:415,607,624`). Only the **A/B/C three-arm benchmark + kill-criterion (C≈B ⇒ simplify)** remains |
| INT-03 Maven `pom.xml` + `package_units` | ✅ next — `jvm_package.rs` is import-resolution only, no pom dep parse |
| INT-04 bundle typed `ReconcilePlan` | ✅ open — `bundle.rs:144` still boolean; derived plan (C2) rates bundle ~55% done, effort MEDIUM not HIGH |
| #67 pipeline split | ✅ real (7244 lines). Extract sequentially (discovery/extract/persist/invalidate/resolve/materialize/publish) with **golden-equivalence after every extraction PR** — no big-bang |

These may run in parallel with Phases 0–4 **as long as they never touch authority contracts**.

---

## §5. Corrected "first 10 PRs I would actually execute"

The blueprint's first-10 list is right in spirit but two items are wholly/partly pre-done. Corrected
critical path:

1. **CCK-00** Constitution + this blueprint.
2. **CCK-01** state.db forward-migration executor. *(highest leverage — unblocks every v2–v7 feature)*
3. **CCK-02** Deepen guarantee contracts to behavior (PR E did the shallow layer; finish the deep one).
4. **CCK-03** Public write-gate refusal E2E (#63) — pure test debt, closes an open issue cheaply.
5. **CCK-04** Protected SourceView.
6. **CCK-05** RootedFilesystem + WriteReceipt (durability-uncertain semantics).
7. **CCK-06** EvidenceSnapshot (reading existing `index_input_state`, not reinventing reconcile).
8. **CCK-07** state.db v2 (Snapshot + ChangeIntent).
9. **CCK-08** RiskVector + Policy (shadow).
10. **CCK-09** ReviewAuthority v3 (folding in the live `graph_generation` gate).

Only after CCK-09 does an agent touch `edit.rs`'s authority contract deeply (CCK-10). CCK-03 can be
pulled forward and merged immediately — it is the fastest open-issue close with zero architectural risk.

---

## §6. Risks, adjustments & additions the blueprint under-specifies

1. **PR-lettering collision (structural).** Fixed by the `CCK-NN` namespace here; the derived plan
   keeps `PR A–E / P1–P9`. Never run two branches that both bump a schema version without rebasing
   migrations (blueprint Part XXII — keep it).
2. **CCK-01 fixture reality.** Retained fixtures span `user_version` 0 (pre-`b677a9e`) and 1. The
   migrator and its generic test loop (blueprint Part XXIII) must start the loop at the lowest retained
   version, treating 0 and 1 as the same v1 baseline.
3. **ExecutionBroker × cargo network** (see Phase 3 adjustment) — the single most likely
   false-failure in the whole train. Design the vendored-cache/prefetch path up front.
4. **Two schema-version counters.** `INDEX_DB_SCHEMA_VERSION` and `STATE_DB_SCHEMA_VERSION` are
   independent; the CCK v2–v7 bumps are state.db only. index.db already has `migrate_add_column` +
   `run_migrations`. Don't conflate them; the crash/migration matrix (Part XXIV) must cover both DBs.
5. **#66 stays open after CCK-02** until every enforced `test` is a behavior assertion, not a
   presence proxy — otherwise a refactor can still hollow out a guarantee whose test only checks a
   function *exists*.
6. **Attestation determinism** depends on canonical JSON + a stable `analysis_versions` map. Reuse the
   canonicalization already proven in `graph/digest.rs` (PR B) rather than a second canonicalizer.
7. **Graduation framework (experimental→shadow→advisory→enforced)** and the **HARD "do not build yet"
   list** (Part XXXI: generic graph DB, PPR/LTR, taint engine, cross-repo heuristic call graph, cloud
   control plane) are correct and match the derived roadmap's deferrals — keep both verbatim as the
   scope-discipline backbone.

---

## §7. Bottom line

- **Accuracy of the source blueprint: excellent** — 17/19 primitive claims byte-exact; the 2 misses
  are same-day races with the PR A–E train, not analytical errors.
- **Architecture: adopt as-is.** The evolution-not-rewrite thesis is correct and the invariants are
  worth enforcing via the PR template today.
- **Sequencing correction:** CCK-01 (migration executor) is the true unblocker; CCK-02/#66 and INT-02
  are partly pre-done; CCK-03/#63 is a free open-issue close; graph_generation is already a live gate
  so CCK-09 shrinks. The North-Star workflow (`plan_change → review_change → apply_change →
  run_verification → change_status → attestation`, survivable across restart at any step) remains the
  correct first product milestone, and nothing above requires rewriting a primitive CALM already got
  right.
