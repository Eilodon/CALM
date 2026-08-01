# Architecture Decision Records

## ADR-0001 — Core refresh catalog and supervised watcher recovery

**Status:** ✅ ACCEPTED
**Date:** 2026-08-01
**Deciders:** @Eilodon
**Tags:** `calm-core` `scip` `watcher` `indexing`
**Change Classification:** `IMPLEMENTATION BUG`
**Review date:** 2026-11-01 — re-evaluate when watcher-health telemetry has at least 30 days of production observations
**Supersedes:** —
**Superseded by:** —

**DECISION TYPE:** `EXPERIENCE-DRIVEN`
**CONFIDENCE:** `HIGH`
**LAST CONFIRMED:** 2026-08-01 — `IMPLEMENTATION`
**VOLATILITY:** `WATCHFUL` — watch reconciliation failures and stale-index reports

### Context

Incremental indexing depended too heavily on filesystem events and did not persist the non-source inputs that affect extraction and resolution. Configuration drift, missed events, unsafe renames, or a dead OS watcher could therefore leave the graph stale while the last completed index still appeared healthy. The VHEATM Tier-2 remediation plan identified this as a correctness and observability gap across SCIP overlays, the indexer, and the server watcher lifecycle.

### Decision

Make refresh decisions in `calm-core` through a shared `InputCatalog` and durable `index_input_state`, so configuration/context drift and unsafe event batches can fall back to a full reconciliation. Add a `WatchSupervisor` that treats the OS watcher as an accelerator, exposes explicit lifecycle/freshness/error state, retries with bounded backoff, and preserves a safe reconciliation path when watcher creation or refresh fails. Surface that health separately through `indexing_status` and use one catalog for overlay status checks.

### Options Considered

- Full filesystem scans after every debounce were rejected because they impose unnecessary work on normal source edits.
- Watcher-only path updates were rejected because missed events and non-source input drift can invalidate otherwise unchanged source files.
- The selected design keeps incremental refresh for normal events and escalates only when the catalog or supervisor detects unsafe or stale conditions.

### Impact

Schemas changed: `index_input_state`
Components changed: `calm-core` refresh/indexing/SCIP overlay pipeline, server watcher supervision, indexing-status schema, CLI/server integration
Breaking change: NO

IMPACT RADIUS:
BLAST RADIUS: WIDE
Cascades: filesystem events → refresh catalog/reconciliation → graph and SCIP overlay state → watcher health exposed by `indexing_status`
Cascade Review: ✅ Done

### Consequences

The index can recover from missed events and input drift without silently claiming freshness, and operators can distinguish a healthy completed index from a watcher that is stale or degraded. The implementation adds durable fingerprints, reconciliation work, and a larger observable status payload; production behavior must be watched for unnecessary full reconciliations and repeated watcher backoff.

### Evidence

- [verified 2026-08-01] `cargo test --workspace --all-targets`: 1246 passed, 0 failed, 14 ignored; exit code 0.
- [verified 2026-08-01] `cargo fmt --all -- --check`: exit code 0.
- [verified 2026-08-01] `git diff --check`: exit code 0.
- [verified 2026-08-01] CALM `diff_impact` found all changed high-risk signatures and confirmed the primary `run_and_refresh` callers are formal edges; manual caller review completed.

### Owner

@Eilodon

### Known Debts (PATTERN-DEBT)

PATTERN-DEBT entries introduced or affected by this change:
  - none

### Next Cycle Trigger

When production telemetry records either 3 watcher reconciliations caused by stale/degraded state in a 24-hour window or any refresh failure that leaves `indexing_status.watcher.freshness` stale for more than 10 minutes.

### Cycle Retrospective

- The last completed index is not sufficient evidence that future filesystem changes are being observed.
- Non-source inputs need a durable contract because identical source bytes can still produce different extraction or resolution results.
- Watcher lifecycle and index freshness are separate signals and should not share one status field.
- The safety fallback is useful only when it is exercised by tests for startup drift, unsafe events, retry exhaustion, and add/modify/delete flows.
