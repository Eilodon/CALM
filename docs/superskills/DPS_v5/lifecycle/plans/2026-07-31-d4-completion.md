# D4 Completion Implementation Plan

> **For agentic workers:** execute inline with `tdd-verified`; do not commit or
> push unless the user explicitly requests it.

**Goal:** satisfy every D4 acceptance item with durable, current provenance and
fresh verification evidence.

**Architecture:** CallSite byte spans remain the only proof identity. A graph
generation is committed atomically with every graph rebuild and copied into an
external proof; SCIP/LSP must compare it, source hash, span, provider profile,
and context immediately before graph mutation. LSP is explicit-only and has one
latest-wins coordinator per workspace/provider.

**Tech stack:** Rust, rusqlite, Tokio/LSP JSON-RPC, Tree-sitter, GitHub Actions.
**Audit gate:** PASS WITH FLAGS (spec audit); implementation audit blockers are
explicit tasks below.

## File responsibilities

- `crates/calm-core/src/db/schema.rs`: durable proof and graph-generation schema.
- `crates/calm-core/src/indexer/pipeline.rs`: atomic generation advance,
  migration measurements, full/incremental equivalence tests.
- `crates/calm-core/src/scip/ingest.rs`: exact proof insert/recheck and SCIP
  stale-result rejection.
- `crates/calm-core/src/lsp/client.rs`: bounded framing, allowlisted server
  requests, mock JSON-RPC harness.
- `crates/calm-core/src/lsp/overlay.rs`: latest-wins coordination and final
  LSP proof-write fence.
- `crates/calm-core/src/lsp/provider.rs`: reviewed provider profile data.
- `crates/calm-server/src/tools/lsp.rs`, `tools/recover.rs`, `tools.rs`,
  `types/mcp_types.ts`: runtime status contract and snapshots.
- `.github/workflows/scip-nightly.yml`, ADR/fixture/provider docs: reproducible
  live evidence and release boundary.

### Task 1: Add atomic graph-generation persistence

**Files:**
- Modify: `crates/calm-core/src/db/schema.rs`
- Modify: `crates/calm-core/src/indexer/pipeline.rs`
- Test: schema and pipeline unit tests in those files

- [ ] Write a failing test that initializes an in-memory DB, runs two complete
  pipelines, and asserts the generation is monotonically incremented only after
  each successful transaction.
- [ ] Run: `cargo test -p calm-core --lib graph_generation_advances_only_after_commit` → FAIL.
- [ ] Add:
```sql
CREATE TABLE IF NOT EXISTS graph_generation_state (
  id INTEGER PRIMARY KEY CHECK (id = 1), generation INTEGER NOT NULL
);
INSERT OR IGNORE INTO graph_generation_state(id, generation) VALUES (1, 0);
```
  Increment `generation` through the existing `tx` immediately before
  `tx.commit()`, never on cancellation/error.
- [ ] Re-run the focused test → PASS, then `cargo test -p calm-core --lib`.

### Task 2: Make proofs generation-bound and definition-snapshot-aware

**Files:**
- Modify: `crates/calm-core/src/db/schema.rs`
- Modify: `crates/calm-core/src/scip/ingest.rs`
- Modify: `crates/calm-core/src/lsp/overlay.rs`
- Test: `schema.rs`, `ingest.rs`, and `overlay.rs` unit tests

- [ ] Write failing tests proving a proof stores `graph_generation`,
  `identity_version`, provider argv/profile fingerprint, and definition snapshot;
  change the generation between resolution and write and assert zero edge/proof
  mutations.
- [ ] Run the focused tests → FAIL.
- [ ] Add nullable `definition_snapshot`, non-null `graph_generation`, and
  non-null `identity_version` columns plus migrations. Extend the exact
  `INSERT ... SELECT` proof writer to select CallSite identity and current graph
  generation in the same statement. Extend SCIP/LSP final `UPDATE` predicates
  with `graph_generation_state.generation = expected_generation`.
- [ ] Re-run focused tests → PASS; run `cargo test -p calm-core --lib`.

### Task 3: Record complete migration observability

**Files:**
- Modify: `crates/calm-core/src/db/schema.rs`
- Modify: `crates/calm-core/src/indexer/pipeline.rs`
- Modify: `crates/calm-server/src/tools/recover.rs`
- Modify: `crates/calm-server/src/tools.rs`
- Modify: `types/mcp_types.ts`

- [ ] Write a failing migration-cancel test asserting `failed`, duration,
  rebuilt-file count, graph generation, and failure reason are visible while
  old graph/proofs remain committed.
- [ ] Run focused test → FAIL.
- [ ] Persist `duration_ms`, `rows_rebuilt`, and `busy_retries` in
  `identity_migration_state`; bracket the full transaction with monotonic time,
  count reparsed files, and report SQLite busy failures without changing graph
  authority. Serialize overlay writers with the existing writer lock.
- [ ] Re-run focused and server status tests → PASS.

### Task 4: Prove rebuild equivalence

**Files:**
- Modify: `crates/calm-core/src/indexer/pipeline.rs`
- Modify: `crates/calm-core/src/scip/ingest.rs`

- [ ] Write a failing fixture test that runs clean full rebuild + deterministic
  SCIP twice, then incremental rebuild + deterministic SCIP, and compares a
  sorted fingerprint of current CallSites, edges, proofs, and rule-outs.
- [ ] Run focused test → FAIL.
- [ ] Extract one test-only fingerprint query ordered by canonical CallSite
  identity, target, proof provider, and generation; fix any order-dependent
  persistence until the fingerprints match.
- [ ] Re-run focused test → PASS and retain it as acceptance #11.

### Task 5: Finish latest-wins LSP execution and stale-write rejection

**Files:**
- Modify: `crates/calm-core/src/lsp/overlay.rs`
- Test: `crates/calm-core/src/lsp/overlay.rs`

- [ ] Write failing tests for A→B→C generations: A and B produce no proof,
  only C runs; a resolved A completion after generation advance updates zero
  rows. Use a deterministic barrier, not sleeps.
- [ ] Run focused tests → FAIL.
- [ ] Keep one pending latest generation with waiter accounting; propagate the
  cancellation token through the resolver, shutdown the child before return,
  and invoke the generation-bound phase-3 helper only after final recheck.
- [ ] Re-run focused tests → PASS and run all `lsp::overlay` tests.

### Task 6: Build deterministic LSP mock protocol coverage

**Files:**
- Modify: `crates/calm-core/src/lsp/client.rs`
- Create: `crates/calm-core/tests/lsp_mock_harness.rs`
- Modify: `crates/calm-core/src/lsp/provider.rs`

- [ ] Write failing mock-server tests for provider argv, initialize language ID,
  UTF-8/UTF-16 + CRLF positions, root-confined workspace folders,
  `workspace/configuration`, capability registration, unsupported request,
  timeout, cancellation, and stale result.
- [ ] Run: `cargo test -p calm-core --features lsp-overlay lsp_mock_harness` → FAIL.
- [ ] Implement a framed stdio fixture server and provider profile fields for
  canonical language ID, initialization options, workspace-folder policy, and
  reviewed request policy. Reject external file URIs after normalized root check.
- [ ] Re-run focused tests → PASS; run all LSP tests. This is fast-PR-lane
  evidence and must not download/install anything.

### Task 7: Expose provider runtime and proof status

**Files:**
- Modify: `crates/calm-core/src/lsp/overlay.rs`
- Modify: `crates/calm-core/src/lsp/mod.rs`
- Modify: `crates/calm-server/src/tools/lsp.rs`
- Modify: `crates/calm-server/src/tools/recover.rs`
- Modify: `crates/calm-server/src/tools.rs`
- Modify: `types/mcp_types.ts`

- [ ] Write failing MCP-schema tests requiring per-provider support level
  (`detected|version-probed|fixture-tested|nightly-verified`), binary/version,
  profile/context fingerprint, candidate count, queue/run/result status, and
  skip/reject reason.
- [ ] Run server focused test → FAIL.
- [ ] Add a typed `LspProviderRuntimeStatus`; populate it at probe, queue,
  completion, cancellation, and rejection points. Return it from `lsp_refresh`
  and `indexing_status`; regenerate only the affected tool snapshot.
- [ ] Re-run focused schema test, `cargo test -p calm-server`, and TypeScript
  contract check → PASS.

### Task 8: Complete pinned nightly and qualification evidence

**Files:**
- Modify: `.github/workflows/scip-nightly.yml`
- Create/modify: CI validation test or script under `scripts/`

- [ ] Write failing static CI test rejecting `latest`, unpinned `go install`, or
  a release download lacking a paired SHA-256 check.
- [ ] Run static test → FAIL.
- [ ] Keep Go/Ruby/Clang tags and hashes paired, add binary `--version` smoke
  output to the nightly artifact/log, and make each ignored live test explicit.
- [ ] Re-run static test → PASS. Nightly qualification is only marked verified
  after the hosted workflow succeeds; local tests must report it as pending.

### Task 9: Update release documentation

**Files:**
- Modify: `docs/adr/0004-lsp-optional-confidence-upgrade.md`
- Modify: `crates/calm-core/tests/fixtures/multi_lang_workspace/README.md`
- Modify: provider installation/status documentation discovered via CALM search

- [ ] Write a failing documentation assertion/search requiring source,
  freshness, support level, explicit-only LSP, and no-auto-install language.
- [ ] Run it → FAIL.
- [ ] Document canonical span identity, fresh/stale/legacy/unverified proof
  state, supported-provider qualification, nightly limits, and operator recovery.
- [ ] Re-run documentation assertion → PASS.

### Task 10: D4 acceptance closure

**Files:** all D4 files above; no new production files unless a failing test
requires one.

- [ ] Run every acceptance test by its numbered name and the complete core/server
  suites.
- [ ] Run `cargo check --workspace`, `git diff --check`, static CI validation,
  and CALM `diff_impact`.
- [ ] Re-read all Fix Anchors and run pattern-globalization searches for
  generation predicates, LSP request replies, and provider status surfaces.
- [ ] Run a final D4 audit against spec §8; mark the goal complete only when all
  15 items have T1/T2 evidence. Hosted nightly failures or a deferred independent
  judge keep the goal active, not “complete”.

## Spec coverage review

- #1–9: Tasks 1–4 cover mutation authority, spans, migration, and equivalence.
- #10–12: Tasks 2 and 5 cover stale completion and latest-wins coalescing.
- #13: Tasks 6–7 cover mock protocol, provider levels, and status surfaces.
- #14: Task 8 covers exact version/checksum enforcement.
- #15: Tasks 2 and 7 cover declared-context invalidation and visibility.
- Release boundary: Task 9.

## Task Risk Summary (task-risk-score)
<!-- task-risk-score: DO NOT DUPLICATE — update this section -->
<!-- last-run: 2026-07-31 | sprint: D4-completion -->

CONTEXT: EXTERNAL_SERVICE

| Task | S×B/D | QBR | Risk | Boundary | Action |
|------|-------|-----|------|----------|--------|
| 1 | 3×3/2 | 4.5 | MEDIUM | SINGLE | transaction test required |
| 2 | 3×3/1 | 9 | HIGH ⚠️ | SINGLE | split proof write/recheck tests |
| 3 | 3×2/2 | 3 | MEDIUM | SINGLE | migration rollback test |
| 4 | 3×2/2 | 3 | MEDIUM | SINGLE | deterministic fixture required |
| 5 | 3×3/1 | 9 | HIGH ⚠️ | SINGLE | barrier-based concurrency tests |
| 6 | 3×3/1 | 9 | HIGH ⚠️ | SINGLE | isolated mock protocol harness |
| 7 | 2×3/2 | 3 | MEDIUM | SINGLE | MCP snapshot + contract tests |
| 8 | 3×2/1 | 6 | HIGH ⚠️ | CROSS(teams=[maintainers, GitHub Actions], owner=maintainers, blocked-until=nightly-green) | static gate + hosted proof |
| 9 | 2×2/2 | 2 | LOW | SINGLE | docs assertion |
| 10 | 3×3/1 | 9 | HIGH ⚠️ | SINGLE | full acceptance audit |

**Summary:** high-risk tasks 2, 5, 6, 8, 10; one external CI boundary; five
tasks require integration-level evidence. No task has mixed implementation
concerns without a separate verification step.
