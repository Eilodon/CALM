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
19 of 21 load-bearing "current state" claims verified byte-for-byte against live source (1 outdated, 1 partial — both corrected below; see §1 for the full 21-row count). It is *not*
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
   gate (`STALE_GRAPH_AUTHORITY`, `crates/calm-server/src/tools/guardrails.rs:400`, `crates/calm-server/src/tools/edit.rs:1391`). That is **1 of the
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
| 4 | `#65` open: review binds only caller-set digest, not full authority snapshot | 🟡 | Issue **OPEN**; but PR D bound `graph_generation` too → `STALE_GRAPH_AUTHORITY` `tools/guardrails.rs:400`, `tools/edit.rs:1391`. 1/9 §12 fields done |
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
  reconcile (`crates/calm-server/src/lib.rs:292`, `refresh.rs:315`). Snapshot's `freshness_class` should *read* that state,
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
- Tests: `plan_change` on a declared/observed `ChangeIntentKind` mismatch surfaces the mismatch
  instead of silently accepting it; `review_change` refuses to mint `authority_id` without the
  required human/MRTR approval step; repeated `plan_change` calls for the same intent are idempotent
  (same `change_id`); the `change` toolset is absent from a preset that doesn't opt into it.
- Depends: CCK-10 (needs a live authority-minting path — `review_change` calls into it directly).

#### CCK-12 — Authority dogfood promotion · **Status: TODO (config, not architecture)**
- CALM self-repo runs `authority_mode = required`; external users keep compat during an observation
  window. Collect: stale-reject rate, human-veto rate, false-block reports, legacy-fallback usage.
  Flip external default to `structured` only after Phase-1 graduation. (Mirror the #64 shadow→enforce
  promotion pattern already used for calm-guard-dogfood.)

**Dependency graph — Phase 1:** CCK-06 (compute-only, no schema) and CCK-08 (shadow risk engine) have
no dependency on each other or on a schema bump — both can start right after CCK-00 and run in
parallel. CCK-07 (state.db v2) needs CCK-01's migration executor to exist, not CCK-06 — it can also
start as soon as CCK-01 lands, in parallel with CCK-06/08. CCK-09 (state.db v3) needs CCK-07 (a v2
schema to migrate from) and CCK-06's `EvidenceSnapshot` shape (the authority object binds snapshot
digests) — sequence CCK-07 → CCK-09, with CCK-06 ready before CCK-09 starts. CCK-10 (wire authority
into the edit flow) needs both CCK-09 (the authority object) and CCK-08 (the risk gate deciding which
touches require one). CCK-11 needs CCK-10's minting path. CCK-12 is a config flip that needs CCK-10
live in production long enough to collect dogfood metrics — last in sequence, not a code dependency.
Net ordering: {CCK-06, CCK-08} parallel first → CCK-07 once CCK-01 lands → CCK-09 after CCK-07 →
CCK-10 after CCK-09 and CCK-08 → CCK-11 → CCK-12.

**PHASE 1 BENCHMARK GATE — `benchmarks/b15_change_reliability/`** (3 arms: native / legacy CALM /
ChangeIntent+Authority). **Graduation:** 0 stale-authority unsafe accepts · 0 forged/replayed accepts ·
100% hard authority decisions reproducible · no task-correctness regression vs legacy · legacy-fallback
usage declining in dogfood. Only then is structured authority the default.

---

### PHASE 2 — Multi-file ChangeSet

#### CCK-13 — state.db **v4**: ChangeSet + staging schema · **Status: TODO**
- Create `change/changeset.rs`. Modify `state_migrations.rs` (add v3→v4), bump
  `STATE_DB_SCHEMA_VERSION` 3→4. Tables: `change_sets` (id, intent/authority refs, status),
  `changeset_files` (per-file target path, base digest, staged digest, status). Schema only — no
  repo mutation, no staging-directory writes yet (that's CCK-14).
- Tests: v3→v4 migration fixture round-trips existing `change_intents`/`review_authorities` rows
  unchanged; fresh v4 init is idempotent; `change_sets`/`changeset_files` reject an unknown
  `authority_id` foreign key.
- Depends: CCK-09 (state.db v3 must exist before a v4 migration can run against it).

#### CCK-14 — Prepare/stage a ChangeSet · **Status: TODO**
- Create `change/stage.rs`. For every file in a `ChangeSet`: resolve target path, read current
  content, hash it, syntax-check the proposed replacement, write staged content to
  `.calm/staging/<id>/<path>`, fsync. No write to the real source tree at this stage — `commit`
  (CCK-15) is the only step allowed to touch tracked files.
- Tests: a syntax error in any one file aborts staging for the whole set before any file is touched;
  staged digests match what `commit` later reads; re-staging an already-staged set is idempotent;
  a disk-full failure mid-stage leaves no partial `.calm/staging/<id>/` directory behind.
- Depends: CCK-13 (needs the `change_sets`/`changeset_files` schema to record staging state against).

#### CCK-15 — Commit coordinator · **Status: TODO**
- Create `change/commit.rs`. Composes the existing per-file `EditTransaction` (`txn.rs`) — does not
  replace it (see §2's kept invariant). Applies every staged file's `EditTransaction` in sequence,
  then triggers exactly one graph reindex after all files land, not one per file.
- Tests: an N-file ChangeSet produces exactly one reindex, not N; a mid-commit crash leaves the
  ChangeSet in a state CCK-16's matrix can classify (never silently "done"); a base-digest mismatch
  on any one staged file aborts the whole commit before any file is written, not partway through.
- Depends: CCK-14 (needs staged content to commit from).

#### CCK-16 — Crash-injection matrix · **Status: TODO**
- Create `crates/calm-core/tests/changeset_crash_injection.rs`. Kill the process at every state × file
  boundary in the commit sequence; canonical fixture is 10 files × every boundary × 100 runs. Add CI
  job `changeset-crash-injection` so this runs on every PR touching `change/`, not only at release.
- Tests: every crash point resolves to exactly one of `APPLIED` / `PARTIALLY_APPLIED` / `NOT_APPLIED`
  — never an undetectable or ambiguous state; a `PARTIALLY_APPLIED` result is always distinguishable
  from `APPLIED` via `change_status` (CCK-18).
- Depends: CCK-15 (needs a real commit coordinator to inject crashes into).

#### CCK-17 — Recovery API · **Status: TODO**
- Create `change/recovery.rs`: `RecoveryAction` enum + `repair_consistency` tool. Deliberately **no
  automatic recovery choice** — a state that isn't safe to auto-resolve returns
  `MANUAL_RECOVERY_REQUIRED` rather than guessing forward or rolling back on the caller's behalf.
- Tests: every `PARTIALLY_APPLIED` state CCK-16's matrix can produce maps to a defined
  `RecoveryAction` or an explicit `MANUAL_RECOVERY_REQUIRED`; `repair_consistency` is idempotent
  (calling it twice on an already-recovered ChangeSet is a no-op, not an error).
- Depends: CCK-16 (needs the crash taxonomy to know what recovery actions must cover).

#### CCK-18 — `apply_change`/`change_status` facade · **Status: TODO**
- Add to `tools/change.rs` (extends CCK-11's toolset). Supersedes `batch_status`/
  `edit_transaction_status` for the ChangeSet workflow; both old tools stay available, expert/compat,
  for the single-file path.
- Tests: `change_status` on a `PARTIALLY_APPLIED` ChangeSet surfaces it as such, not folded into a
  generic error; `apply_change` on an already-applied ChangeSet is idempotent; old
  `batch_status`/`edit_transaction_status` behavior is unchanged for callers not using ChangeSet.
- Depends: CCK-15 (commit coordinator) and CCK-17 (recovery surface `change_status` reports through).

**Dependency graph — Phase 2:** strictly sequential — CCK-13 → CCK-14 → CCK-15 → CCK-16 → CCK-17 →
CCK-18 — each stage produces the state the next one needs (schema → staging → commit → crash taxonomy
→ recovery → facade). No parallelization inside this phase; CCK-16's crash-injection matrix is the
phase's real gate and should not slip to the end of a sprint just because it "only" adds tests.

**Non-negotiable state-machine rule:** `PARTIALLY_APPLIED` is a *legitimate, explicit* state, not a
failed implementation. Forbidden outcomes anywhere: unknown / maybe-written / assume-rolled-back.

**PHASE 2 GRADUATION:** 0 silent partial applications · 0 lost file commits after crash · 100%
`PARTIALLY_APPLIED` detectable · 100% ChangeSet replay deterministic · single-ChangeSet reindex ==
full-reindex graph · task-correctness ≥ legacy.

---

### PHASE 3 — Verification infrastructure

#### CCK-19 — `ExecutionBroker` · **Status: TODO (may start in parallel with Phase 1/2, right after CCK-05)**
- Create `crates/calm-core/src/verification/{mod,broker}.rs`. Single chokepoint every verification
  command must route through — no adapter (CCK-20/21) may call `Command::new` outside it.
- Tests (malicious-fixture matrix, each must be *contained*, not just eventually killed): fork-bomb,
  stdout/stderr flood, network attempt (denied under default policy), out-of-scratch-directory write,
  forbidden read (outside repo/scratch), timeout.
- Depends: CCK-05 (`RootedFilesystem`/containment primitives the broker's sandboxing builds on) —
  does not need CCK-06..18, so it can be built in parallel with all of Phase 1/2.

#### CCK-20 — state.db **v5** + Rust adapter · **Status: TODO**
- Modify `state_migrations.rs` (v4→v5, bump `STATE_DB_SCHEMA_VERSION`). Move `verify.rs` →
  `verification/adapters/rust.rs`, routed through the CCK-19 broker; keep the old `verify.rs` path as
  a re-export compat shim, hard-deleted only in a later cleanup PR. The `verification.timeout_secs`
  and `verification.bound_to_proposed_digest` guarantee-contract tests (`docs/guarantee-levels.toml`)
  must keep resolving through the move — the file move must not silently orphan their `test =`
  pointers.
- Tests: CCK-02's guarantee-coverage check still resolves both contracts to the moved fn; the old
  `verify_change(tx_id)` compat alias still round-trips through the broker end-to-end.
- Depends: CCK-19 (broker must exist to route the Rust adapter through) and a v4 schema (end of
  Phase 2) to migrate from — CCK-20 is the first Phase-3 PR that needs Phase 2 finished.

#### CCK-21A/B/C — Go/TS/Python adapters · **Status: TODO**
- Create `verification/adapters/{go,typescript,python}.rs`, one PR per language (21A/21B/21C), all
  routed through the CCK-19 broker — no adapter gets its own `Command::new`. "Unsupported/unavailable"
  (toolchain not installed, no test command configured) is a valid, explicit result — never invent or
  guess a command to force one.
- Tests per adapter: a CI lint asserting no `Command::new` outside `verification/broker.rs` in that
  adapter's module; the unsupported-toolchain case returns the explicit unavailable state, not an
  error and not a false pass.
- Depends: CCK-19 (broker) and CCK-20 (establishes the adapter module layout the other three follow).
  21A/21B/21C are parallel with each other once CCK-20 lands.

#### CCK-22 — Test-impact evidence · **Status: TODO**
- Coverage-derived selected-test evidence. Unknown coverage must *broaden* the selected set, never
  silently narrow it — an adapter that can't report coverage falls back to "run everything relevant,"
  not "run nothing and call it clean." Rename `verify_change(tx_id)` → `run_verification(change_id,
  profile?)` at this point (once ChangeSet's `change_id` exists from Phase 2); keep `verify_change` as
  a deprecated alias removed only before a v1 release.
- Tests: an adapter with unknown coverage selects a superset, verified against a fixture where the
  "true" minimal set is known; `run_verification` accepts a Phase-2 `change_id` and a bare single-file
  `tx_id` both.
- Depends: CCK-20/21 (needs adapters producing real coverage data) and CCK-18 (needs `change_id` to
  exist for the rename to make sense).

**Dependency graph — Phase 3:** CCK-19 is the only Phase-3 PR with no Phase-1/2 dependency — start it
right after CCK-05, in parallel with everything else. CCK-20 needs both CCK-19 and Phase 2's v4
schema, so Phase 3 cannot fully land before Phase 2 finishes even though CCK-19 itself starts much
earlier. CCK-21A/B/C run in parallel with each other after CCK-20. CCK-22 is last, needing real
coverage data out of the adapters.

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

- **Accuracy of the source blueprint: excellent** — 19/21 primitive claims byte-exact; the 2 misses (1 outdated, 1 partial)
  are same-day races with the PR A–E train, not analytical errors.
- **Architecture: adopt as-is.** The evolution-not-rewrite thesis is correct and the invariants are
  worth enforcing via the PR template today.
- **Sequencing correction:** CCK-01 (migration executor) is the true unblocker; CCK-02/#66 and INT-02
  are partly pre-done; CCK-03/#63 is a free open-issue close; graph_generation is already a live gate
  so CCK-09 shrinks. The North-Star workflow (`plan_change → review_change → apply_change →
  run_verification → change_status → attestation`, survivable across restart at any step) remains the
  correct first product milestone, and nothing above requires rewriting a primitive CALM already got
  right.
