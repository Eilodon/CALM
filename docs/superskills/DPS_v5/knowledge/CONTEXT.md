# CONTEXT.md — Domain Knowledge

## Ubiquitous Language

- **InputCatalog**: the shared inventory and fingerprint set for source files, coverage inputs, and non-source context/configuration that can change indexing semantics. <!-- from ADR: ADR-0001 -->
- **Full reconciliation**: a refresh that compares durable input state and chooses changed-file, context rebuild, or full-index work instead of trusting an event path list. <!-- from ADR: ADR-0001 -->
- **WatcherSupervisor**: the bounded-retry runtime that treats the OS file watcher as an accelerator and retains periodic reconciliation when observation is degraded. <!-- from ADR: ADR-0001 -->

## Architectural Decisions

- Filesystem events are an optimization; `calm-core` owns refresh classification and durable input fingerprints so missed events or input drift cannot silently establish freshness. <!-- from ADR: ADR-0001 -->
- Watcher lifecycle/freshness health is separate from the last completed indexing phase and is exposed independently through `indexing_status`. <!-- from ADR: ADR-0001 -->

## Domain Gotchas

- `indexing_phase == ready` only describes the last completed build; it does not prove that future filesystem changes are currently observed. <!-- from ADR: ADR-0001 -->
- Configuration and context files can change the meaning of unchanged source bytes; they must be included in refresh fingerprints and reconciliation decisions. <!-- from ADR: ADR-0001 -->

<!-- Version: 1 — populate via domain-alignment skill, then keep updated via knowledge-compound -->

## Ubiquitous Language

<!-- Add domain terms where the word means something more specific than common usage -->

## Architectural Decisions

<!-- Decisions with applicability beyond a single feature -->

## Domain Gotchas

<!-- Operational surprises that don't fit architectural decisions -->
