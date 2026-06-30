# ADR: ATAM Audit Fixes — Code Intelligence MCP v2.7.2

## 1. Title

Fix all 10 architectural compliance gaps found by ATAM audit against `docs/architecture-design.md` spec.

## 2. Context

A full ATAM audit of the v2.7.2 codebase against the architecture-design.md spec revealed 10 findings
(2 CRITICAL, 3 HIGH, 3 MEDIUM, 2 LOW). The codebase had diverged from spec in several areas: FTS5
search was not column-scoped, the indexing phase ladder was not advanced by the pipeline itself,
`session_context` provided no exploration frontier, the `compound` preset was unimplemented, and the
write connection was shared with read-only tool handlers (violating SINGLE_WRITER semantics).

All fixes were implemented on branch `atam-audit-fixes` using TDD (failing test first) with
3-stage review (spec compliance → code quality → specialist ATAM lens) per task.

## 3. Decision

Implemented all 10 findings in priority order on a dedicated worktree branch:

1. **FTS5 column filter** (`search.rs`): `kind=text` queries now use `{docstring} : <query>` to
   restrict FTS5 matching to the docstring column only — prevents false positives from name-only matches.

2. **Phase ladder** (`pipeline.rs`, `lib.rs`): `run_indexing_pipeline` now advances
   `Scanning → Parsing → BuildingEdges → Ready` internally; phase resets to `Scanning` on error to
   prevent permanently-stuck `BuildingEdges` state.

3. **Coreness in `symbol_info`** (`tools.rs`): `SymbolInfoOutput` gains `coreness: Option<i64>`,
   null when `edges_ready=false`, 0 for isolated nodes, positive for k-core depth.

4. **Compound preset** (`tools.rs`): Added `"compound"` arm to `preset_tools()` — 9-tool set for
   full workflow navigation without raw graph traversal tools.

5. **`locate` suggested_next** (`tools.rs`): Dead-code path (`caller_count == 0`) and
   ambiguous-name path (multiple candidates) now produce appropriate `suggested_next` hints.

6. **Config preset precedence** (`main.rs`, `config.rs`): CLI `--preset` > `config.json preset` >
   default `"full"`. Previously CLI default shadowed the config value.

7. **`session_context` frontier** (`tools.rs`): Computes frontier (unexplored files reachable via
   `import_edges` + `call_edges` from explored context). `frontier_degraded=true` when
   `edges_ready=false`. `suggested_next` now points to `file_overview` on top frontier file.

8. **Auto-gitignore** (`gitignore.rs`, `lib.rs`): `ensure_gitignore()` called at serve startup;
   idempotently appends `.codeindex/` to `.gitignore` if not already present.

9. **`session_started_at`** (`tools.rs`): ISO 8601 UTC timestamp set at session construction,
   stable across `session_context` calls, always serialized in output.

10. **SINGLE_WRITER** (`tools.rs`): `make_read_conn()` method opens a dedicated read-only connection
    with `PRAGMA query_only = ON`. 15/16 tool handlers converted; `session_context` deferred with TODO.

## 4. Status

ACCEPTED

## 5. Consequences

**Improved:**
- FTS5 search results are now docstring-scoped, eliminating false positives from symbol names
- Phase gate is atomically correct: `Ready` only after `tx.commit()` inside pipeline
- `session_context` provides actionable frontier navigation instead of always suggesting `repo_overview`
- Read-only tools no longer contend with the write connection (15 of 16 converted)
- Config preset hierarchy works as documented
- `.codeindex/` auto-gitignored on first serve — no accidental DB commits

**Worsened / Debt created:**
- `session_context` still uses the shared write connection for frontier DB queries (TODO deferred)
- `compute_frontier_entries` lives in `tools.rs` instead of `ci-core/src/db/queries.rs` (structural debt)
- Frontier IN-clause has no 999-variable SQLite limit guard (silent empty frontier at large scale)
- Phase TOCTOU gap between `edges_ready()` check and DB query is narrow but structurally non-zero

## 6. Alternatives Considered

**FTS5 full-document search (no column filter):** Rejected — violates spec which explicitly requires
docstring-only matching for `kind=text`; name matches are handled by `kind=symbol`.

**Eager frontier precomputation (cached at index time):** Rejected — adds invalidation complexity.
Query-on-demand is correct for session sizes (dozens of files); revisit if sessions grow beyond 500
files.

**Separate write crate for DB access:** Rejected for this cycle — would require a larger refactor.
`make_read_conn()` + `PRAGMA query_only` achieves the safety invariant at lower cost.

## 7. Evidence

- All 183 tests pass (150 `ci-core` + 26 `ci-server` + 6 `ci-cli` + 1 watcher integration)
  [verified 2026-06-30]
- `make_read_conn_opens_read_only_connection` test confirms `PRAGMA query_only = ON` rejects writes
  [verified 2026-06-30]
- `session_context_frontier_includes_import_and_call_edge_entries` test confirms correct path/reason
  tagging from real `import_edges` + `call_edges` rows [verified 2026-06-30]
- `test_search_text_does_not_match_name_only` confirms FTS5 column filter excludes name-only symbols
  [verified 2026-06-30]
- Phase ladder test `test_phase_advances_through_ladder` confirms `Ready` set only after commit
  [verified 2026-06-30]

## 8. Owner

**ybao (bao.nt.1992@gmail.com)**

## 8b. Known Debts (PATTERN-DEBT)

- **PATTERN-DEBT-session-context-read-conn**: `session_context` handler still uses shared write
  connection for `compute_frontier_entries`. Status: OPEN — 1 remaining handler.
- **PATTERN-DEBT-frontier-sql-in-tools**: `compute_frontier_entries` belongs in
  `ci-core/src/db/queries.rs`, not `tools.rs`. Status: OPEN — structural divergence from query layer.
- **PATTERN-DEBT-frontier-in-clause-limit**: No guard against SQLite 999-variable limit in frontier
  IN-clause; silent empty frontier above threshold. Status: OPEN.

## 9. Next Cycle Trigger

When ANY of the following occurs:
- `session_context` handler is touched again (convert the remaining TODO to `make_read_conn`)
- A session exceeds 200 explored files (SQLite 999-var limit becomes a real risk)
- `import_edges` or `call_edges` schema changes (migrate `compute_frontier_entries` to `queries.rs`)

## 10. Cycle Retrospective

- **Wrong assumption:** Thought `run_indexing_pipeline` already set the phase to `Ready` in `lib.rs`;
  it did, but AFTER the function returned, meaning phase could be observed as `BuildingEdges` during
  a slow commit. Moving the phase write inside the pipeline (after `tx.commit()`) was the correct fix.
- **Surprise:** The `cargo test` filter `session_context` missed `session_started_at_is_stable_across_calls`
  because the test name doesn't start with "session_context". Always verify with a broader filter or
  `--lib` when adding tests with different name prefixes.
- **Would design differently:** `compute_frontier_entries` should have gone into `ci-core/src/db/queries.rs`
  from day one — moving it post-hoc requires a public API change across the crate boundary.
- **Known debt created:** `session_context` TODO for `make_read_conn` was deferred to avoid restructuring
  the conditional branch that feeds `compute_frontier_entries`. Low risk (reads only), but violates
  the SINGLE_WRITER invariant the same task aimed to fix.
- **Signal to watch:** If the indexer becomes concurrent (e.g., incremental re-index while serving),
  the TOCTOU gap between `edges_ready()` and the frontier DB query becomes a real race. Monitor any
  change to `watcher.rs` or `pipeline.rs` that introduces parallelism.
