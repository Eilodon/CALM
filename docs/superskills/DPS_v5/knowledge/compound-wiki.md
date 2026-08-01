# Compound Wiki

---
date: 2026-08-01
sprint: vheatm-tier2-refresh-supervision
adr: ADR-0001 in docs/superskills/DPS_v5/ADR.md
modules: [calm-core, calm-server, scip, watcher]
---

## Cycle: core-refresh-catalog-and-watcher-supervision

### New Domain Terms Added to CONTEXT.md

- `InputCatalog`: shared input inventory and fingerprints ✅
- `Full reconciliation`: durable-input-aware refresh escalation ✅
- `WatcherSupervisor`: bounded watcher retry and degraded fallback ✅

### Bug Patterns

- Missed filesystem events or non-source input drift can leave a completed index stale: observed and resolved in ADR-0001; no recurring PATTERN-DEBT entry created.

### Gotchas Captured

- A ready last build is not proof of future observation → added to `CONTEXT.md` Domain Gotchas ✅
- Context/configuration changes can alter unchanged source semantics → added to `CONTEXT.md` Domain Gotchas ✅

### Architectural Decisions Promoted

- Core owns refresh classification and durable input fingerprints; watcher health is a separate axis → added to `CONTEXT.md` Architectural Decisions ✅

### Nothing extracted

No new PATTERN-DEBT entry was warranted by this single cycle.

---

Auto-populated by: knowledge-compound skill (run after every adr-commit)
Queried by: kb-query skill
MCP-ready: YAML frontmatter per entry for future semantic KB import

<!-- ENTRIES BELOW — do not delete; each entry is a cycle's extracted learnings -->
