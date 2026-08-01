# ADR-0010: SCIP-primary CallSite provenance (D4)

## Status

Accepted.

## Context

Line-only external-resolution evidence is not stable enough to change the
call graph: several calls can share a line, source text can move without a
semantic change, and a later graph rebuild can make an earlier provider result
stale.  LSP results are useful corroboration but are asynchronous and must not
silently outrank exact SCIP evidence.

## Decision

- Persist each current call site with identity version 2, UTF-8 byte span, and
  source-file hash.  A provider observation without all three remains
  observation-only.
- Treat an external proof as fresh only when its provider/context fingerprints,
  definition snapshot, CallSite identity, and graph generation match the graph
  currently served.  Stale and legacy proofs remain diagnostic records and
  cannot leave an edge formal.
- Let an exact SCIP match upgrade or reconfirm an edge as `formal` with
  `formal_source = 'scip'`.  A non-call reference, line-only match, or stale
  proof cannot upgrade, rule out, or insert a graph edge.
- Keep LSP residual: it uses the same proof freshness fence and can only
  corroborate eligible candidates.  The coordinator coalesces equal baselines,
  cancels obsolete runs, and prevents an older generation from committing after
  a newer one has been scheduled.
- Rebuild legacy CallSite identity in one graph transaction.  Migration status,
  duration, row count, generation, and failure/cancellation state are exposed
  for diagnosis; cancellation leaves the previous graph intact.

## Consequences

The graph's confidence is more conservative: unavailable, malformed, or stale
provider output produces no upgrade rather than a plausible-but-wrong formal
edge.  Provider runtime status exposes binary/version/profile/context
fingerprints so operators can distinguish unavailable tooling from a run that
produced no candidates.  This adds durable schema and migration surface, so
full and incremental indexing must remain semantically equivalent even though
their absolute generation counters differ.

## Verification contract

The D4 regression suite covers same-line distinct targets, overload/member
spans, Unicode/CRLF positions, non-call references, stale proof rejection,
migration cancellation, full-vs-incremental deterministic fingerprints,
latest-wins LSP coordination, bounded mock-server shutdown/protocol handling,
and pinned nightly SCIP provider contracts.  Fingerprint comparison records
whether a proof is bound to the graph's current generation, not the absolute
generation number, because equivalent rebuild histories legitimately have
different counters.
