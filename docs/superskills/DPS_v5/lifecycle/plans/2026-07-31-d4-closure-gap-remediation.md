# D4 Closure Gap Remediation Plan

Date: 2026-07-31

Status: Closure plan ready; implementation and acceptance closure are still open.

Related design/spec:

- docs/superskills/specs/2026-07-31-scip-primary-callsite-provenance-d4.md
- docs/superskills/DPS_v5/lifecycle/plans/2026-07-31-d4-completion.md
- docs/adr/0010-scip-primary-callsite-provenance-d4.md

## Goal

Close every remaining D4 gap and produce an auditable acceptance packet for all
15 D4 acceptance items, without treating a green local test run as proof of
hosted provider qualification or independent review.

The existing D4 implementation plan remains the source of truth for
implementation tasks 1-9. This document is the closure/remediation delta for:

1. The TypeScript mirror can drift from the Rust-generated MCP schema.
2. The nightly CI regression guard checks the wrong historical unpinned URL.
3. The D4 packet has not passed an Independent Judge, a hosted nightly run,
   the final diff-impact gate, or a real commit/ADR shipment update.
4. Several acceptance claims have tests, but their tests are not named or
   strong enough to prove the exact failure mode claimed.

No D4 completion claim is valid until the hard gates in the final section are
green.

## Design and audit boundary

The D4 architecture is unchanged:

- a fresh exact SCIP byte span is the only source allowed to formalize,
  rule-out, or insert a call edge;
- Stack Graphs remains the fallback authority;
- LSP remains explicit/on-demand residual verification and cannot rewrite
  SCIP or Stack Graphs authority;
- migration, provider, and proof writes remain generation/freshness guarded.

Audit gate from the approved spec: PASS WITH FLAGS, Tier 2 Production.
The spec records SELF_AUDIT=YES and IJ_STATUS=DEFERRED. The deferred IJ is a
release blocker, not an informational note.

The current worktree already contains D4 changes and untracked D4 artifacts.
The executor must preserve unrelated user changes, reconcile the full intended
file set before staging, and never infer the D4 file set from tracked diff
alone.

## Closure acceptance matrix

The matrix below is the starting evidence ledger. Every row must end with one
named test or deterministic command, a fresh result, and an evidence pointer.
Existing coverage means the implementation appears to cover the behavior; it
does not mean the acceptance row is closed before the final run.

| Acceptance | Current assessment | Closure action |
| --- | --- | --- |
| 1. P0 is observation-only | Existing P0 test checks zero call-edge mutations | Re-run and record upgraded, ruled_out, inserted, and Stack Graph override counters as zero |
| 2. Two same-target calls on one line remain distinct | Existing pipeline test covers two same-line calls | Re-run with exact byte spans and record distinct CallSite/edge identities |
| 3. Same-line non-call reference cannot formalize or rule out a call | No clearly named regression test for this scenario | Add same_line_non_call_reference_cannot_formalize_or_rule_out_call_edge |
| 4. Repeated member/overload names use exact spans | Exact-span tests exist, but the wording requires an explicit repeated-name case | Add or nominate a test with two same-line member/overload occurrences and distinct spans |
| 5. Unicode/CRLF positions are correct | Existing LSP position test covers UTF-8/UTF-16 and CRLF | Re-run and record both position encodings |
| 6. LSP cannot mutate SCIP formal or Stack Graph authority | Policy and stale-generation tests exist | Re-run authority-boundary tests and assert graph/proof counters remain unchanged |
| 7. Only fresh exact SCIP can replace Stack Graphs | Existing exact-span and stale/unmatched tests exist | Re-run fresh, stale, unmatched, and source-hash mismatch paths |
| 8. Unchanged-hash identity migration rebuilds safely | Existing legacy identity migration tests exist | Add proof/cache invariants to migration evidence and confirm no line-derived fresh proof survives |
| 9. Cancellation/failure is atomic | Cancellation test records failure/metrics but does not fully snapshot prior proofs | Snapshot prior graph, edge, proof, and cache state and compare after |
| 10. Stale async LSP completion has zero graph effect | Existing stale-generation test exists | Re-run with zero edge/proof delta assertions |
| 11. Full/incremental and clean deterministic SCIP fingerprints match | Golden tests exist; fingerprint completeness needs review | Canonicalize all acceptance-relevant call-site, edge, rule-out, and proof fields |
| 12. LSP same-baseline coalescing/latest-wins is proof-safe | Coordinator tests cover scheduling, but latest-wins proof persistence is not fully asserted | Add database-level proof assertions for predecessor cancellation and newest-only completion |
| 13. Mock protocol fast lane and live-level evidence align | Mock protocol test exists; hosted evidence is absent | Keep mock tests local and attach a green hosted nightly run for every advertised live provider level |
| 14. Go/Ruby/Clang reject latest and verify exact acquisition | Static test currently checks the wrong URL pattern | Fix per-provider block assertions, then attach hosted checksum/version evidence |
| 15. Provider manifest/lock/build/config changes invalidate proof | Fingerprint code/tests exist, but the full declared-input matrix is not recorded | Test every declared input for Rust, Go, and Clang plus missing-to-present transitions |

## Work sequence

### Task 0 — Freeze the closure baseline and evidence ledger

Risk: HIGH, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owner boundary: single closure owner, with reviewer sign-off.

Files and artifacts:

- docs/superskills/DPS_v5/lifecycle/plans/2026-07-31-d4-completion.md
- docs/superskills/specs/2026-07-31-scip-primary-callsite-provenance-d4.md
- docs/adr/0010-scip-primary-callsite-provenance-d4.md
- a closure evidence ledger kept with the D4 review packet

Actions:

1. Record the starting commit, branch, dirty paths, and all untracked D4
   artifacts:

       git rev-parse HEAD
       git status --short
       git diff --check

2. Reconcile the intended D4 file list from the spec, existing plan, current
   diff, and untracked files. Classify each path as D4, unrelated user work,
   or generated snapshot.
3. Create an evidence ledger with these columns:
   acceptance ID, claim, source/test symbol, exact command, local result,
   hosted result, evidence tier, commit SHA, run URL, reviewer, and remaining
   risk.
4. Do not check off an existing plan item merely because its code is present.
   A checkbox becomes complete only when its ledger row has a fresh result.
5. Treat missing hosted or IJ evidence as OPEN, not as deferred complete.

Exit criteria:

- all 15 acceptance rows exist in the ledger;
- every current gap is mapped to a task below;
- unrelated dirty work is not included in the intended D4 commit set.

### Task 1 — Repair and guard the TypeScript MCP schema mirror

Risk: HIGH, QBR 6 (Severity 2 x Blast Radius 3 / Detectability 1).
Owner boundary: Rust MCP schema owner plus TypeScript consumer owner.

Files:

- types/mcp_types.ts
- crates/calm-server/src/tools/recover.rs
- crates/calm-server/src/tools.rs
- crates/calm-server/src/__toolsnaps__/indexing_status.snap

Required change:

1. Add the four missing fields to the identity_migration TypeScript shape:

       duration_ms?: number
       rows_rebuilt?: number
       busy_retries: number
       graph_generation?: number

   Preserve Rust serde optionality: the three conditionally serialized fields
   remain optional, while busy_retries is required.
2. Add a contract test that compares the identity_migration schema in the
   committed toolsnap with the identity_migration block in the TypeScript
   mirror. Compare at least property names, required versus optional
   properties, and primitive type/nullability for all ten fields.
3. Reuse the existing committed-snapshot loading path where possible. The
   test must read the committed snapshot, not regenerate it and silently
   accept a changed schema. It must fail if the TypeScript block loses a field
   or changes requiredness.
4. Keep the parser scope narrow and deterministic. Do not introduce a general
   TypeScript compiler dependency just for this contract. The test may extract
   the known interface/block and normalize whitespace, but it must reject
   duplicate field declarations and unknown fields in this contract.
5. Keep the existing Rust toolsnap equality test. The new test closes the
   separate Rust-schema-to-TypeScript-mirror gap; it does not replace the
   snapshot test.

RED evidence:

- Run the new contract test against the current six-field mirror and capture
  the missing-field failure before adding the four fields.

GREEN evidence:

       cargo test -p calm-server tool_schemas_match_committed_snapshots
       cargo test -p calm-server -- identity_migration

Acceptance:

- the four fields are present with the exact optionality above;
- a future Rust toolsnap field addition causes the mirror contract test to
  fail before merge;
- no generated snapshot update bypasses the mirror check.

### Task 2 — Add the missing same-line non-call SCIP regression guard

Risk: MEDIUM/HIGH, QBR 4.5 (Severity 3 x Blast Radius 3 / Detectability 2).
Owner boundary: SCIP ingestion and parser test owner.

Files:

- crates/calm-core/src/scip/ingest.rs
- crates/calm-core/src/scip/parse.rs if the fixture requires an occurrence-kind
  distinction

Test name:

    same_line_non_call_reference_cannot_formalize_or_rule_out_call_edge

Test construction:

1. Build one source file containing a real call and a same-line non-call
   reference to the same textual symbol.
2. Give the real call an exact callee_start_byte, callee_end_byte, and source
   file hash.
3. Give the non-call reference a different exact span on that same line.
4. Seed a Stack Graph candidate edge and ingest SCIP occurrences.
5. Assert all of the following:

   - upgraded count is zero for the non-call span;
   - ruled_out count is zero for the non-call span;
   - inserted count is zero for the non-call span;
   - the original edge remains unchanged;
   - no SCIP formal proof is written for the non-call span;
   - no Stack Graph override counter increments because of the non-call span.

6. Re-run the existing exact-span tests:

   - exact_span_reference_upgrades_only_its_matching_same_line_call_site
   - exact_span_reference_rules_out_only_other_targets_of_the_same_call_site
   - exact_span_reference_inserts_for_an_uncandidated_call_site
   - p0_observation_only_leaves_scip_proof_out_of_call_graph

Design constraint:

The test must prove the current safety boundary: a line-only or different-span
reference cannot become evidence for a call. If the occurrence model cannot
distinguish a non-call occurrence that maliciously reuses the exact call span,
record that as a separate model limitation; do not claim the test proves a
stronger property than the data model can represent.

### Task 3 — Strengthen migration atomicity evidence

Risk: HIGH, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owner boundary: indexer/database migration owner.

Files:

- crates/calm-core/src/indexer/pipeline.rs
- crates/calm-core/src/db/schema.rs if the assertion needs a shared query
- existing migration test module

Required evidence:

1. Before forcing legacy identity migration, snapshot canonical fingerprints
   and counts for call_sites, call_edges, external_proofs, proof/rule-out rows,
   overlay cache rows, and graph generation.
2. Cancel the migration at the existing deterministic cancellation point.
3. After cancellation assert:

   - old graph/call-site identity remains byte-for-byte equivalent;
   - no partial current-version call site is visible;
   - no partial formal proof or rule-out is visible;
   - overlay cache state is unchanged;
   - failure status is recorded;
   - duration_ms and graph_generation are recorded;
   - rows_rebuilt and busy_retries match the actual cancelled work.

4. Preserve and re-run:

   - legacy_call_site_identity_forces_a_full_rebuild_even_when_hashes_match
   - reindex_paths_repairs_legacy_call_site_identity_even_when_hashes_match
   - cancelled_identity_baseline_preserves_legacy_graph_and_records_failure

5. Add a deterministic injected-failure case only if an existing failure hook
   can fail before commit. Otherwise, cancellation is the explicit acceptance
   path and the ledger must state that a separate non-cancellation failure is
   not exercised.

### Task 4 — Make deterministic graph/proof fingerprints complete

Risk: MEDIUM, QBR 4.5 (Severity 3 x Blast Radius 3 / Detectability 2).
Owner boundary: graph-equivalence test owner.

Files:

- crates/calm-core/tests/golden_graph_equivalence.rs
- the shared fingerprint helper if it is currently private to the test

Required canonical fields:

- call-site path, enclosing symbol, call-site identity version;
- exact callee start/end byte span and source file hash;
- edge target, edge kind, authority/source, evidence state, and ruled-out
  state;
- proof provider, provider version/profile fingerprint, context fingerprint,
  source hash, definition snapshot, graph generation, and proof status;
- deterministic rule-out target identity.

Exclude database IDs, timestamps, insertion order, and other non-semantic
values.

Test sequence:

1. Run a clean full build and an incremental rebuild over identical source.
2. Apply the same deterministic SCIP proof input to both graphs.
3. Compare the canonical fingerprint.
4. Run the clean tree twice and compare fingerprints across runs.
5. Mutate a proof-generation or provider-context input and assert that the
   fingerprint changes when the proof becomes stale.

Required existing tests:

    golden_equivalence_incremental_vs_fresh_across_mutation_rounds
    fresh_index_is_deterministic_on_identical_tree
    deterministic_scip_proofs_have_identical_fresh_fingerprints

### Task 5 — Close LSP stale/latest-wins proof persistence gaps

Risk: HIGH, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owner boundary: LSP coordinator, overlay, and persistence owners.

Files:

- crates/calm-core/src/lsp/overlay.rs
- crates/calm-core/src/lsp/client.rs
- existing LSP coordinator/overlay tests

Required tests:

1. Use deterministic barriers, not sleeps, to start predecessor A, queue
   predecessor B on the same baseline, and submit newer baseline C.
2. Assert same-baseline requests coalesce to one execution.
3. Assert C cancels or fences A/B and only C can write a proof.
4. After A/B cancellation or stale completion, query the database and assert
   zero new edge/proof/rule-out mutations.
5. After C completes, assert exactly the expected current-generation proof
   exists.
6. Re-run:

   - stale_lsp_generation_cannot_mutate_edges_or_proofs
   - changed_lsp_context_stales_its_proof_and_reopens_the_edge
   - coordinator_coalesces_same_baseline_and_fences_an_older_generation
   - coordinator_runs_only_the_latest_queued_baseline

Authority checks:

- automatic-policy configuration cannot start an LSP server;
- LSP cannot modify Stack Graphs authority;
- LSP cannot overwrite a SCIP formal edge;
- stale LSP completion cannot increment graph mutation counters.

Protocol fast lane:

Verify the deterministic mock server still exercises initialize,
workspace/configuration, client/registerCapability, positionEncoding,
workspace folders, initialization options, definition, unknown-request
rejection, and oversized-frame rejection. Add timeout/cancellation coverage
only through the existing mock protocol harness and deterministic barriers.

### Task 6 — Qualify every provider context input and status meaning

Risk: HIGH, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owner boundary: provider runtime/status owner and LSP overlay owner.

Files:

- crates/calm-core/src/lsp/provider.rs
- crates/calm-core/src/lsp/overlay.rs
- MCP status/tool snapshots only if the public status shape changes

Provider input matrix:

- rust-analyzer: Cargo.toml, Cargo.lock, rust-toolchain.toml,
  .cargo/config.toml;
- gopls: go.mod, go.sum, go.work;
- clangd: compile_commands.json, CMakeLists.txt.

Test every input:

1. Establish a proof with a fixed source hash.
2. Mutate exactly one declared input.
3. Assert the context fingerprint changes and the old proof is stale.
4. Assert unrelated provider fingerprints do not change.
5. Test missing-to-present and present-to-changed transitions.
6. Assert profile/argv/version changes also invalidate the relevant proof.

Status contract:

- fixture-tested means the deterministic mock contract passed;
- nightly-verified means pinned hosted acquisition, checksum, version probe,
  and live provider test passed;
- unavailable/version-probe failure must remain visible in reason/status and
  must not be represented as nightly-verified.

Record this distinction in the evidence ledger and fixture README.

### Task 7 — Fix the nightly CI regression guard at provider-block scope

Risk: HIGH/CROSS, QBR 6 (Severity 3 x Blast Radius 2 / Detectability 1).
Owners: core test owner, workflow maintainer, release maintainer.

Files:

- crates/calm-core/tests/nightly_ci_contract.rs
- .github/workflows/scip-nightly.yml

The current assertion is invalid because it checks:

    releases/download/latest

The historical unpinned forms that must be rejected are:

    releases/latest/download
    go install ...@latest

Do not add a whole-workflow assertion for the word latest. Valid occurrences
include ubuntu-latest, comments, and unrelated PHP/Docker :latest examples.

Replace the current test with named provider-block checks:

1. Extract the acquisition block for the Go, Ruby, and Clang installation
   steps by their stable step names.
2. In each block assert:

   - a provider-specific VERSION variable exists;
   - a provider-specific 64-hex SHA256 variable exists;
   - the download URL contains the pinned VERSION variable;
   - the downloaded artifact is checked with sha256sum --check --status;
   - the extracted binary is probed with its version command;
   - the block contains no releases/latest/download;
   - the Go block contains no @latest acquisition;
   - the Ruby/Clang blocks contain no unversioned release asset path.

3. Ensure the checksum is applied to the exact downloaded artifact before the
   provider is put on PATH or executed.
4. Keep the test deterministic and independent of network access.
5. Fail if a provider block disappears or is renamed without the test being
   updated; silently skipping a missing block is not allowed.

RED evidence:

- Seed each historical bad pattern in a temporary test string or fixture and
  prove that the contract rejects it.

GREEN command:

    cargo test -p calm-core --test nightly_ci_contract

This Rust contract test is the canonical local static CI validation. Run
actionlint against the workflow as an additional check when the command is
available, but do not substitute a network run or a blanket grep for the
provider contract.

### Task 8 — Obtain hosted nightly evidence

Risk: HIGH/CROSS, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owners: repository maintainer, GitHub Actions environment, provider owners.

This task cannot be proven by local Cargo tests. Trigger
.github/workflows/scip-nightly.yml from the exact commit containing the
workflow and static-contract fixes.

Capture in the evidence ledger:

- workflow URL and run ID;
- commit SHA;
- runner image;
- Go/Ruby/Clang version variables;
- each download URL and successful SHA256 verification;
- each provider version-probe output;
- selected fixture/live provider test results;
- cache/offline behavior and any ignored-test execution;
- timestamps and final job conclusion.

If any provider cannot download, checksum, execute, or produce the expected
version, the run is failed evidence. Fix the workflow or pin and rerun; do not
downgrade the claim to probably valid.

Acceptance 13 and 14 remain OPEN until this run is green and linked.

### Task 9 — Reconcile release documentation and ADR status

Risk: MEDIUM, QBR 3 (Severity 2 x Blast Radius 2 / Detectability 1).
Owner boundary: maintainers/release documentation.

Files:

- docs/adr/0004-lsp-optional-confidence-upgrade.md
- docs/adr/0010-scip-primary-callsite-provenance-d4.md
- crates/calm-core/tests/fixtures/multi_lang_workspace/README.md
- authoritative architecture/status/contract documents found by the final
  CALM search

Required documentation state:

1. D4 provenance contracts describe exact byte spans, source hashes,
   generation/freshness, authority precedence, and transaction semantics.
2. LSP is explicitly opt-in/on-demand and never auto-installed or activated
   by a watcher, edit, save, or legacy automatic policy.
3. Provider status clearly distinguishes fixture-tested, unavailable,
   version-probe failure, and nightly-verified.
4. Go/Ruby/Clang installation guidance names version and checksum pinning and
   does not recommend latest.
5. Fixture README matches what the workflow actually runs; remove claims that
   imply all providers are locally buildable or live-verified.
6. Historical ADR text remains historical; update current status/prose without
   rewriting the decision record.

Before commit, search for stale claims about automatic LSP, latest provider
installation, line-derived SCIP proof, and live provider support. Every
authoritative hit must either be updated or explicitly marked historical.

Do not change ADR-0010 to shipped before the commit exists. After the final
commit and all gates, update it to:

    Accepted & Implemented — shipped 2026-07-31. Commits: <actual commit SHA(s)>.

Use actual commit date and SHA values; do not insert placeholders into the
committed ADR.

### Task 10 — Run a real Independent Judge review

Risk: HIGH/CROSS, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owners: separate reviewer context and D4 maintainer.

The IJ must not be the same reasoning pass that wrote or self-audited this
plan. Provide a stripped packet containing:

- the D4 claim;
- the 15-item acceptance matrix;
- relevant source/test paths and exact commands;
- local test results;
- hosted nightly URL/run evidence;
- known risk flags;
- reproducibility steps for P0, same-line call/non-call, migration
  cancellation, stale LSP completion, and provider pin verification.

The IJ must explicitly decide:

- whether each acceptance item is actually proven;
- whether any test asserts an implementation detail instead of the claim;
- whether hosted provider evidence matches the advertised support level;
- whether migration and stale-result transactions preserve prior state;
- whether the static nightly guard catches the exact historical regressions.

Update the packet with reviewer identity, timestamp, findings, disposition,
and IJ_STATUS=COMPLETE only after independent review has occurred. If no
separate reviewer context is authorized or available, retain IJ_STATUS=DEFERRED
and leave D4 OPEN.

### Task 11 — Final verification, diff impact, commit, and shipment

Risk: HIGH, QBR 9 (Severity 3 x Blast Radius 3 / Detectability 1).
Owner boundary: D4 maintainer plus release reviewer.

Run these local gates after all edits:

    cargo fmt --all -- --check
    cargo check --workspace
    cargo test --workspace --all-features
    cargo test -p calm-server tool_schemas_match_committed_snapshots
    cargo test -p calm-core --test nightly_ci_contract
    cargo test -p calm-core --test golden_graph_equivalence
    cargo test -p calm-core exact_span_reference
    cargo test -p calm-core stale_lsp_generation
    cargo test -p calm-core coordinator_
    cargo test -p calm-core identity_
    git diff --check

Use exact test filters available in the final tree; if a filter matches zero
tests, treat that as a failed verification and run the concrete test binary.

Then:

1. Stage only the reconciled D4 files, including intended untracked files.
2. Inspect the staged name/status list and staged diff.
3. Run CALM diff impact on the staged diff:

       mcp__calm__diff_impact(staged=true)

4. Resolve every high/critical affected-symbol concern. The current
   ingest_occurrences hub has a broad blast radius and requires reviewer
   sign-off grounded in its real callers; low aggregate risk is not enough if
   the reviewer finding remains unresolved.
5. Confirm no pending_scan file remains. Permanent out_of_scope docs/config
   entries may remain, but must be listed and understood.
6. Run a final pattern-globalization search for:

   - call_site_line;
   - line-derived SCIP authority;
   - releases/latest/download;
   - provider @latest acquisition;
   - automatic LSP startup;
   - stale LSP proof writes without generation/context checks.

7. Run the adr-commit skill gate, verify ADR-0010 and PATTERN-DEBT lifecycle
   requirements, and commit using the repository convention, for example:

       feat(core,server): D4 -- close provenance acceptance gaps

   Split commits only when each commit remains reviewable and the ADR can list
   every actual SHA. Do not commit unrelated pre-existing user work.
8. After the commit, update ADR-0010 with actual shipped commit SHA values,
   rerun committed-range diff impact if required by the repository workflow,
   and attach the final evidence ledger.

## Risk and ownership summary

| Task | QBR | Level | Ownership boundary | Blocking evidence |
| --- | ---: | --- | --- | --- |
| 0. Baseline/ledger | 9 | HIGH | Single closure owner | 15 ledger rows and reconciled file set |
| 1. TypeScript mirror | 6 | HIGH | Rust MCP + TypeScript consumers | Rust snapshot and mirror contract both green |
| 2. SCIP non-call guard | 4.5 | MEDIUM/HIGH | SCIP ingestion owner | Named regression test and zero mutation assertions |
| 3. Migration atomicity | 9 | HIGH | Indexer/database owner | Before/after graph, proof, and cache equality |
| 4. Fingerprint completeness | 4.5 | MEDIUM | Graph-equivalence owner | Full/incremental and repeat-clean equality |
| 5. LSP latest-wins | 9 | HIGH | LSP coordinator + persistence owners | Database proof state proves stale writes are absent |
| 6. Provider context matrix | 9 | HIGH | Provider runtime + overlay owners | Every declared input invalidates only its affected proof |
| 7. Static nightly guard | 6 | HIGH/CROSS | Test + workflow maintainers | Exact historical bad patterns rejected per block |
| 8. Hosted nightly | 9 | HIGH/CROSS | GitHub Actions + provider owners | Green run URL with checksum/version evidence |
| 9. Docs/ADR | 3 | MEDIUM | Maintainers/release docs | Current claims match implementation and workflow |
| 10. Independent Judge | 9 | HIGH/CROSS | Separate reviewer context | IJ_STATUS=COMPLETE and findings dispositioned |
| 11. Final closure/commit | 9 | HIGH | Maintainer + release reviewer | Full tests, staged diff impact, ADR shipment |

QBR uses Severity x Blast Radius / Detectability. External hosted CI and
independent review deliberately remain high risk even when local tests are
green because detectability is low until the external gate runs.

## Definition of done

D4 is closed only when all conditions hold:

1. The TypeScript mirror contains all ten identity_migration fields with
   Rust-matching requiredness and the mirror contract test is green.
2. The nightly contract test rejects releases/latest/download and Go @latest
   in the correct provider blocks, while allowing unrelated valid latest text.
3. All 15 acceptance rows have named, fresh local evidence or a linked hosted
   result.
4. The same-line non-call regression test exists and proves zero formal,
   rule-out, insertion, and override mutations.
5. Migration cancellation/failure evidence proves prior graph/proof state is
   preserved with no partial writes.
6. LSP latest-wins evidence proves stale predecessors cannot write proofs.
7. Provider context fingerprint evidence covers every declared input.
8. Hosted nightly is green for every advertised live provider level.
9. An Independent Judge has reviewed the packet in a separate context and
   IJ_STATUS is COMPLETE.
10. Full local verification, git diff --check, staged CALM diff_impact, and
    adr-commit gates are green.
11. Actual D4 commit SHA values exist and ADR-0010 says
    Accepted & Implemented — shipped with those SHA values.

Until items 8-11 are true, the correct status is “D4 implementation present;
acceptance closure open”, not “D4 complete”.
