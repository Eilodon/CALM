# Pattern Debt Registry

Schema: see shared/pattern-debt-schema.md
Auto-populated by: pattern-globalize skill
Queried by: kb-query skill

<!-- ENTRIES BELOW — do not delete, update status field instead -->

PATTERN-DEBT-ts-mcp-schema-mirror-drift:
  pattern: "Consumer-facing TypeScript MCP output types drift from the committed Rust toolsnap schema because no explicit parity contract owns each legacy mirror"
  grep_cmd: "mcp__calm__search(query=\"mcp_types\\\\.ts\", kind=\"grep\", glob=\"**/*\")"
  found: 44
  fixed_now: ["types/mcp_types.ts:510", "crates/calm-server/src/tools.rs:1090"]
  remaining: 44
  priority: HIGH
  owner: maintainers
  created_date: 2026-07-31
  created_sprint: D4
  review_interval: "before every change to types/mcp_types.ts or crates/calm-server/src/__toolsnaps__"
  resolution_trigger: "Before any non-D4 change modifies a TypeScript MCP output interface or a toolsnap"
  status: OPEN
  resolved_date: null
  actual_outcome: NEAR_MISS
