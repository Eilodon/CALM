---
title: "D4 — SCIP-primary call-site provenance and on-demand residual LSP"
date: 2026-07-31
author: Codex, with user-approved design direction
SPEC_APPROVED: true
SPEC_ESCALATION: false
ESCALATION_FINDING: ""
related_specs:
  - docs/superskills/specs/2026-07-30-stack-graphs-demotion-lever.md
related_adrs:
  - docs/adr/0002-formal-resolver-stack-graphs.md
  - docs/adr/0004-lsp-optional-confidence-upgrade.md
---

# CALM — D4: SCIP-primary call-site provenance and on-demand residual LSP

## 1. Decision and scope

This is a new D4 workstream following D1–D3 in
`2026-07-30-stack-graphs-demotion-lever.md`. It does not rename, replace, or
reopen that approved D1–D3 scope, and it is unrelated to the older "Phase D"
cache work in the architecture plan.

**Decision:** CALM will treat SCIP as the primary batch source of formal
evidence only when it can be mapped to one exact, fresh call-site. Stack Graphs
remain a stable formal fallback. LSP is an opt-in, on-demand verifier for
residual ambiguous/textual edges; it never overwrites a Stack Graphs or SCIP
formal verdict.

In this document, **"Stack Graphs frozen" means frozen against LSP only**.
Fresh, exact SCIP evidence may confirm or replace a Stack Graphs verdict.
SCIP absence, provider failure, stale cache, or an unmatched occurrence is not
evidence for replacing any existing fallback verdict.

**P0 safety boundary:** until a current-version CallSite byte span exists, SCIP
is observation-only. It may report a disagreement or unmatched occurrence, but
it must not upgrade an edge to `formal`, insert an edge, mark a sibling ruled
out, or override a Stack Graphs result. "One candidate on this line" is not
exact identity and is never sufficient to label evidence `formal`. This narrow,
fail-closed stop protects the shipped D3 ingest path while P1 establishes
byte-span identity.

The stakeholder is CALM maintainers and MCP clients that consume edge
confidence. The goal is to make provenance a trustworthy property of a single
call-site instead of a line-level approximation.

### In scope

- Canonical byte-span CallSite identity throughout parser, persistence, graph,
  SCIP, and LSP paths.
- Evidence provenance, freshness, invalidation, and status surfaces.
- Exact SCIP range ingestion and residual-only LSP verification contract.
- Controlled migration of line-derived graph and overlay state.
- Test fixtures, deterministic CI lanes, provider health qualification, and
  documentation needed to make the authority policy auditable.

### Explicitly out of scope

- Changing the public `EdgeConfidence` vocabulary or making LSP a default
  resolver.
- Claiming every SCIP provider or LSP server is production-ready merely because
  a binary is discoverable.
- Automatic package download, automatic provider upgrade, or an LSP call from
  an edit/watcher/on-save path.
- Broad language expansion, UI redesign, and a new general-purpose reference
  graph model.

## 2. Verified starting state

The present implementation is line-addressed end-to-end:

- `RawCall`, `CallSiteData`, `call_sites`, and `call_edges` retain a line but
  not a callee span.
- Resolver deduplication and the database unique index collapse identity at a
  caller/target/line granularity.
- SCIP parses an occurrence range but retains its first line, then joins by
  `(file, line)`.
- LSP selects the first whole-word occurrence of the target name on a line
  before converting to its negotiated position encoding.

Consequently, same-line calls, member calls with repeated names, and non-call
references on a call's line can receive the wrong formal verdict. This is a
correctness defect in the identity model, not a provider-ordering problem.

Current LSP SQL already limits candidates and updates to unresolved
ambiguous/textual rows with no `formal_source`. D4 preserves that residual
guard and makes it a documented, regression-tested authority invariant.

## 3. Canonical call-site and edge identity

### 3.1 CallSite

A CallSite is the exact selected callee token in a source snapshot. Its
canonical coordinate system is zero-based, half-open **UTF-8 source bytes**:

```text
CallSiteIdentity = (
  from_path,
  from_symbol,
  callee_start_byte,
  callee_end_byte,
  edge_kind,
  identity_version
)
```

`from_path` is mandatory because byte offsets are meaningful only within one source
file. `source_file_hash` is a proof-freshness precondition, not part of the
logical CallSite key. `call_line` remains a display and query convenience only;
it is never used to establish evidence identity. The parser must select `method` in
`receiver.method()`, not the whole expression or the receiver. Tree-sitter byte
ranges are the source of truth for parser-derived spans.

`to_symbol` is intentionally absent from CallSite identity: a call-site can
have several candidate targets while its intrinsic source location remains one
site. A `call_edges` row represents a candidate or resolved target of one
CallSite and has a unique key:

```text
(call_site_id, to_symbol, edge_kind)
```

All current-version CallSite span columns are non-null. Legacy rows may not
participate in a current identity unique index; SQLite NULL uniqueness must not
be used as a migration escape hatch.

### 3.2 Evidence state

`formal_source` remains the origin label (`stack_graphs`, `scip`, `lsp`). D4
adds an orthogonal evidence state:

```text
fresh | stale | legacy | unverified
```

Only a `formal` edge with `fresh` evidence may be presented as current formal
proof. Existing line-only overlay verdicts become `legacy` during migration;
they are not silently represented as fresh exact evidence.

## 4. Authority and proof policy

The authority lattice is intentionally narrow:

1. **SCIP exact, fresh proof** may confirm or replace a Stack Graphs verdict.
2. **Stack Graphs** remains the fallback when no eligible SCIP proof exists.
3. **LSP** may promote only a residual `ambiguous` or `textual` edge with no
   formal source and no SCIP rule-out marker.
4. An LSP result never updates a `stack_graphs`, `scip`, or any other already
   formal edge. A later exact SCIP result may supersede a residual LSP result
   or rule it out.
5. Missing, timed-out, malformed, stale, or unmatched external evidence leaves
   the existing edge unchanged and records a non-success outcome.

An external result is eligible only if all of the following are true:

- its input file hash equals the indexed source snapshot;
- its reference range normalizes to exactly one current CallSite span;
- its target maps to a current symbol/definition under the provider contract;
- its index generation is current at commit time;
- its provider profile, version probe, and command fingerprint match the
  persisted evidence record.
- its provider-declared resolution-context fingerprint (workspace manifest,
  lock/build/config inputs) matches the snapshot used for the result.

SCIP's `typed_range` takes precedence over legacy `range`; each is converted
from the document's declared position encoding to canonical UTF-8 byte offsets.
LSP receives a position converted from the stored canonical byte offset into
the negotiated LSP encoding only at request time.

`insert_missing_edges` must not create a `call` edge solely from a SCIP
reference occurrence. An occurrence that cannot prove an AST call-site is
stored or reported as an unmatched reference, not fabricated into the call
graph.

### 4.1 P0 observation-only enforcement

P0 is a temporary authority restriction, not a new confidence vocabulary. The
SCIP importer may retain diagnostics needed to compare SCIP with Stack Graphs,
but its graph-mutating paths (`formal` upgrade, insert, rule-out, and
Stack-Graphs override) are disabled until the P1 exact-span acceptance evidence
passes. P0 ends only with byte-span matching; it must not be relaxed through a
line-level occurrence-count heuristic.

## 5. Migration, invalidation, and persistence

### 5.1 Controlled identity migration

This change requires an explicit identity-version migration, not an ordinary
file-watcher reindex. Normal indexing may skip unchanged hashes and an overlay
cache may skip provider work; either behavior would preserve line-derived
evidence after the schema changes.

The migration reuses and extends the existing full
`run_indexing_pipeline_cancellable` transaction contract. It is **one baseline
SQLite transaction**, not a dual-generation graph, inactive copy, or
reader-visible active-generation pointer.

The migration contract is:

1. Serialize the migration against overlay writers and record a small,
   diagnostic-only `identity_migration` status (`pending`, `running`,
   `baseline_ready`, or `failed`) with target version and timestamps.
2. Begin one full-index transaction. Within it, apply schema changes; invalidate
   or mark line-derived proof/rule-out state `legacy`; invalidate provider cache
   state; and force parsing of every indexed source file even if its hash is
   unchanged.
3. Persist current-version CallSites, rebuild the baseline graph, and validate
   that current rows have non-null spans, no current proof is line-derived, and
   the current identity uniqueness constraints hold.
4. Commit the transaction once. A reader sees its prior committed snapshot until
   this commit, then the complete new baseline; it never reads a mixed graph.
5. Only after commit may a SCIP batch overlay run. LSP remains explicit. Each
   external result must carry and recheck the committed identity/baseline version
   before it can write.

Cancellation or any baseline error drops the transaction, preserving the prior
committed graph and proof state. The failure status is then recorded separately
for diagnosis; it is not a rollback subsystem and does not alter graph
authority. D4 must measure migration duration, lock/busy behavior, rows rebuilt,
and failure reason. If those measurements show that a one-transaction baseline
is not operationally viable, that is a new design decision requiring a new
specification—not permission to introduce dual generations silently.

Full and incremental rebuilds must produce equivalent current-version CallSite
and edge fingerprints. A reindex must never attach legacy proof to a new edge
only because file path and line still happen to match.

### 5.2 Durable proof records

Proof is persisted in SQLite, not a provider sidecar and not a mutable
`edge_id`. A proof record is keyed by stable CallSite identity plus target and
contains:

- provider ID, probed version, profile hash, and command/argv fingerprint;
- source file hash, canonical span, target identity, and definition snapshot
  identifier when supplied;
- a provider-declared resolution-context fingerprint covering the workspace
  inputs that can alter target resolution without changing the call-site file;
- graph/index generation, observation time, status, and failure reason.

Reindex, file-content change, provider-profile change, target disappearance,
resolution-context change, and a failed identity migration invalidate or reject
proof according to this record. An asynchronously completed LSP run rechecks
snapshot and generation immediately before writing; a stale completion updates
zero edges.

## 6. LSP runtime contract

LSP remains behind the existing opt-in feature and is invoked only by an
explicit `lsp_refresh` action in D4. No watcher, edit, on-save, or periodic
path may invoke an LSP resolution run. Existing configuration parsing may stay
backward compatible, but no automatic policy may promote graph confidence in
this scope.

The existing SCIP `run_all_coalesced` gate is not an LSP refresh coordinator: it
is process-global, runs batch SCIP overlays, and has neither provider profile
nor baseline ownership. D4 must add an LSP-specific coordinator (or extract a
tested common primitive without changing SCIP behavior). It permits at most one
in-flight run per `(workspace_root, provider_id)`. Requests for the same
baseline/version/profile join that run. A request after a newer baseline marks
the older run stale, requests cancellation, and queues at most one rerun after
the old child terminates; different generations must not run concurrently for
the same workspace/provider. Every run has a bounded candidate set and
deadline, propagates cancellation to its child process, performs no implicit
retry, and writes nothing until the final freshness check succeeds.

Each supported provider is a data-driven profile containing:

- binary resolver, deterministic version probe, argv, timeout, and stats key;
- canonical language ID;
- initialization options and workspace-folder policy;
- the manifest, lock, build, and configuration inputs included in its
  resolution-context fingerprint;
- a bounded `workspace/configuration` response;
- an allowlisted server-request/capability-registration policy; and
- typed spawn, framing, protocol, cancellation, and timeout errors.

The client must not acknowledge every server request with a generic successful
`null` response. Unknown server requests receive a safe protocol error or a
documented no-op response; they never trigger shell commands, package installs,
or arbitrary configuration reads.

Provider configuration is a static, reviewed profile plus explicit CALM config.
All file URIs and workspace folders must remain under the configured workspace
root after normalization; an LSP request cannot cause CALM to read a path,
secret, or configuration file outside that boundary.

Live profiles are not wired directly into production logic. A mock LSP harness
first proves argv, language ID, UTF-8/UTF-16 position conversion, workspace
folders, initialization options, `workspace/configuration`, capability
registration, cancellation, timeout, and stale-result rejection.

## 7. Provider qualification and CI

Provider support is reported by level, not one boolean:

```text
detected | version-probed | fixture-tested | nightly-verified
```

The implementation must surface the binary/version/profile used, availability,
freshness, candidate count, queued/running/success/failure status, and the
reason for any skipped or rejected proof. `lsp_refresh` output and
`indexing_status` need this distinction; a count of attempted/upgraded edges is
not sufficient.

CI has two distinct lanes:

- **Fast PR lane:** deterministic AST/SCIP/LSP mock tests; no network, no
  package installation, no live binary dependency.
- **Nightly lane:** version-pinned real providers, buildable fixtures, explicit
  live test selection, recorded provider version, binary smoke probe, and
  cache/offline behavior. A `latest` release download is not acceptable.

The current Go module install, Ruby release download, and Clang release download
must each move from `latest` to an explicit version and platform-appropriate
checksum before their lane can claim `nightly-verified`. Every new network-fetched
provider follows the same version-and-checksum rule.

Java/Kotlin is operational hardening and compatibility-canary work, not an LSP
rollout. Its legacy pinned integration must retain coverage while a current
upstream canary validates drift. C# is a strong normal-install SCIP candidate,
while C# LSP remains late because it needs a real project-loading environment.
PHP remains source-pin constrained; PHP LSP is considered only if measured SCIP
residual uncertainty or distribution fragility justifies it. Ruby must pin a
release/checksum and state that precision depends on available type metadata;
Ruby LSP is not enabled by default.

The multi-language workspace is made buildable for every live-provider fixture
claimed by CI, and its README is updated to match executable reality.

## 8. Required acceptance evidence

The design is satisfied only when all evidence below exists:

1. P0 SCIP observation telemetry causes zero `call_edges` mutations: no formal
   upgrade, insert, rule-out, or Stack Graphs override.
2. Two same-target calls on one line create two distinct CallSites and edges.
3. Same-line non-call references cannot SCIP-formal or SCIP-rule-out a call.
4. Repeated member/overload names on one line resolve only their exact spans.
5. Unicode and CRLF text before a callee yield correct UTF-8 and UTF-16 LSP
   positions.
6. LSP cannot alter a Stack Graphs or SCIP formal edge, including under an
   automatic-policy configuration value.
7. Exact, fresh SCIP may replace Stack Graphs; stale or unmatched SCIP cannot.
8. A migration with unchanged file hashes enters the full baseline path, forces
   reparse, invalidates overlay cache state, and leaves no line-derived
   SCIP/LSP verdict marked fresh.
9. Cancellation or failure during that one transaction preserves the prior
   committed graph, records `identity_migration=failed`, and publishes no
   partially migrated CallSite or proof.
10. A stale asynchronous LSP completion writes zero graph changes.
11. Full and incremental rebuild fingerprints, followed by deterministic SCIP,
    are identical across two clean runs.
12. Same-baseline LSP requests coalesce; a newer baseline cancels/queues exactly
    one rerun and both its stale predecessor and any cancellation leave no proof.
13. Mock protocol tests pass in the fast lane; each advertised live-provider
    level has its matching pinned nightly evidence.
14. Go, Ruby, and Clang nightly acquisition reject `latest`, record their exact
    version, and verify the expected checksum before invoking the provider.
15. A provider-declared manifest, lock, build, or configuration change
    invalidates relevant proof even when the call-site source file is unchanged.

## 9. Documentation and release boundary

Before a release that includes D4, update ADR-0004, provenance-facing tool
contracts, provider installation guidance, fixture README, and status
documentation so that they distinguish source, freshness, and support level.
The release remains opt-in for LSP and never auto-installs an external tool.

This specification authorizes design validation only. It does not authorize
implementation, provider installation, migration execution, feature enablement,
or a release. A separate audited implementation plan is required after this
audit gate.

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-31 | trigger: UPDATE -->

**Tier:** 2 (Production) | **Date:** 2026-07-31

**Context declaration:** `CONTEXT_MODE=DESIGN`; stakeholder = CALM maintainers
and MCP consumers; goal = prevent incorrect provenance authority before any D4
implementation; `MODE=FAST`; `SELF_AUDIT=YES`; `IJ_STATUS=DEFERRED` because no
separate judge was authorized for this turn. This is an UPDATE audit after
correcting the migration and LSP-coalescing design assumptions.

### Failure Modes

1. **A migration accidentally takes the dirty-path route, skips unchanged
   hashes, and leaves line-derived proof eligible** — **HIGH** — mitigation in
   spec: **YES**. D4 requires one forced full-baseline transaction, non-null
   current spans, explicit legacy invalidation, integrity checks before commit,
   and rollback-by-transaction rather than a dual-generation pointer.
2. **A valid external provider range maps to the wrong source byte span and
   formalizes/rules-out the wrong call** — **HIGH** — mitigation in spec:
   **YES**. P0 makes SCIP observation-only until exact spans exist; then
   typed/legacy range normalization, exact AST CallSite matching, no synthetic
   call from an unmatched occurrence, UTF-8/UTF-16 plus CRLF tests, and the
   residual-only LSP guard become mandatory.
3. **An older LSP run or drifting live binary appears successful and writes
   stale/unsafe proof** — **HIGH** — mitigation in spec: **YES**. D4 requires
   one LSP coordinator per workspace/provider, cancellation plus a single queued
   newer-baseline rerun, final freshness checks, root-confined protocol handling,
   static profiles, and Go/Ruby/Clang version-and-checksum nightly evidence.

### Layer Signals

- **L1 Logic:** the old line-only key is a non-injective identity. Required
  acceptance evidence 1–6 tests both correct spans and authority precedence.
- **L2 Concurrency:** migration, reindex, and LSP completion can race. One
  baseline transaction, overlay-writer serialization, a per-workspace/provider
  LSP coordinator, and zero-write stale completion address this; acceptance
  evidence 9, 10, and 12 is required.
- **L3 Data:** nullable legacy spans, a missing `from_path`, and reused `edge_id`s
  can silently bridge identities. Current-version spans and `from_path` are
  non-null; proof is keyed by CallSite identity/target and invalidated by
  baseline version and resolution context.
- **L4 Integration:** SCIP uses document position encodings while LSP negotiates
  another encoding; provider binaries, project manifests, and workspace loading
  are external dependencies. Normalization, version/profile probes, manifest
  fingerprints, mock protocol tests, and pinned nightly tests are required.
- **L5 Security:** an LSP server can request configuration or URI-scoped work.
  D4 bounds requests, rejects unknown requests safely, and confines paths to
  the normalized workspace root. No credential or regulated-data signal is in
  scope; Tier 3 is not triggered.
- **L6 Observability:** attempted/upgraded counters can mask unavailable,
  rejected, stale, or failed evidence. Provider support level, status, version,
  freshness, and failure reason are required release surfaces.
- **L7 Cross-cutting:** idempotency and resource control are triggered by repeat
  `lsp_refresh`; coalescing is per workspace/provider, newer baselines are
  serialized rather than run in parallel, and automatic execution is forbidden.

### Assumptions to Verify

- **ASSUMED:** every provider's reported reference span is convertible against
  the exact indexed source bytes. Each provider must prove this in a Unicode and
  CRLF fixture before earning `fixture-tested`.
- **ASSUMED:** the declared manifest/lock/build/config inputs are complete for
  a provider's resolution semantics. The profile test matrix must mutate each
  declared input and prove invalidation; undisclosed inputs keep the proof
  `unverified`.
- **ASSUMED:** one full SQLite transaction can finish within the deployment's
  lock/busy and time budget. A production-like migration test must report
  duration, rows rebuilt, busy/lock behavior, cancellation result, and failure
  reason before D4 is released.
- **ASSUMED:** a provider version probe identifies the executable that actually
  served the request. The live test must capture the probe result and the argv
  fingerprint together.

### Abductive Hypotheses

1. A call-site file is unchanged but a generated project model or transitive
   dependency changes target resolution; source-hash-only freshness would pass.
   D4 now requires a provider-declared resolution-context fingerprint and an
   unchanged-call-site invalidation test.
2. A server's cached open document differs from CALM's indexed disk snapshot;
   both byte conversion and target lookup could appear valid while describing
   different text. The mock/live contract must prove CALM opens the exact
   snapshot and rejects a result when its file/context fingerprint differs.

### Pattern Globalization

- `search(kind="grep", query="call_site_line", glob="crates/**")` returned
  **85** matches across parser/pipeline/schema, SCIP, LSP, MCP tools, and
  graph/parity tests. D4 migration scope is therefore repository-wide, not a
  local SCIP or LSP patch.
- `search(kind="grep", query="formal_source IS NULL|formal_source = 'lsp'|lsp-stats", glob="crates/**")`
  returned **10** matches across pipeline and LSP provider/overlay paths. The
  authority predicate and persisted status must be centralized rather than
  copied.
- `search(kind="grep", query="latest|SCIP_JAVA_VERSION|scip-ruby", glob=".github/workflows/scip-nightly.yml")`
  found a Java pin plus `latest` acquisition for Go, Ruby, and Clang. D4's
  nightly pin/checksum gate applies to every provider it advertises.
- The existing `run_all_coalesced` gate is SCIP-only and process-global, while
  `lsp_refresh` calls providers directly. D4 therefore adds a distinct LSP
  coordinator rather than treating the SCIP gate as an 80% implementation.

### Independent Judge

```yaml
IJ_STATUS: DEFERRED
deferred_reason: "SELF_AUDIT is true; no separate judge context was authorized in this turn."
judge_input:
  claim: "D4's P0 fail-closed stop, byte-span identity, one-transaction baseline, and residual LSP freshness controls prevent false formal provenance."
  evidence:
    - file: "docs/superskills/specs/2026-07-31-scip-primary-callsite-provenance-d4.md"
      line: 91
      excerpt_purpose: "Canonical CallSite identity and non-null span contract."
    - file: "docs/superskills/specs/2026-07-31-scip-primary-callsite-provenance-d4.md"
      line: 184
      excerpt_purpose: "One-transaction migration and invalidation contract."
  reproduction_or_trace: "Confirm P0 cannot mutate an edge; then compare two same-line calls and a same-line non-call reference before and after a forced unchanged-hash migration; attempt an LSP completion from an obsolete baseline."
expected_judge_output:
  verdict: real_bug | not_bug | insufficient_evidence
  severity: critical | high | medium | low
  rationale: "short"
```

### Gate Result

<!-- PASS | PASS WITH FLAGS | HOLD -->
**PASS WITH FLAGS.** All three HIGH failure modes now have a concrete,
testable mitigation in the revised design. Before a future `writing-plans` step,
an independent judge must review the deferred packet, and the implementation
plan must make every acceptance item, migration metric, provider pin, and
mock/live test a concrete task. This audit does not authorize implementation.
