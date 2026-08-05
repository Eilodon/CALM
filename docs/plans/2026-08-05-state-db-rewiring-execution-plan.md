---
title: "state.db rewiring — wiring calm-server's real call sites to the already-built durable-state split"
date: 2026-08-05
status: "RESEARCH + DESIGN COMPLETE, not yet implemented. Follow-up execution doc to the
  storage-layer foundation landed in PR #58 (\"Claude/durability verification gaps jwav0c\").
  A functional regression was found during this pass (see §0) that raises the priority above
  what KNOWN_LIMITATIONS.md's framing (\"a separate follow-up\") suggests."
scope: rewires every real calm-server/calm-cli call site that reads or writes a durable table
  (edit_transactions, tx_events, audit_ledger, maintenance_jobs, project_memory,
  project_memory_refs) to go through state.db (db::conn::open_state_writer,
  PRAGMA synchronous=FULL) instead of index.db (db::conn::open_writer,
  PRAGMA synchronous=NORMAL) — closing KNOWN_LIMITATIONS.md "Durable state and the rebuildable
  index share one SQLite file at runtime".
inputs:
  - KNOWN_LIMITATIONS.md                                                    # names this gap explicitly, scopes it by open_writer's ~320-file blast radius
  - crates/calm-core/src/db/schema.rs                                       # SCHEMA_SQL/init_db (index.db) vs STATE_SCHEMA_SQL/init_state_db/migrate_legacy_durable_tables (state.db)
  - crates/calm-core/src/db/conn.rs                                         # open_writer (NORMAL) vs open_state_writer (FULL)
  - crates/calm-core/src/txn.rs, ledger.rs, maintenance.rs, memory.rs       # durable-write core logic, all Connection-agnostic
  - crates/calm-server/src/tools/{common,edit,txn,memory,orient}.rs, lib.rs # real runtime call sites
verified_against: HEAD (9773d41, branch claude/calm-server-rewiring-plan-no80rd), this pass —
  every open_writer/open_state_writer/init_db/init_state_db call site in crates/calm-server,
  crates/calm-core, crates/calm-cli read fresh via grep + targeted Read; PR #58's diff
  (git show 9773d41 -- crates/calm-core/src/db/schema.rs) inspected directly to confirm §0's
  finding rather than inferred from comments.
---

# state.db rewiring — execution plan

## §0. Key finding: this is a functional fix, not just a durability upgrade

Before scoping the rewiring, I verified a claim CALM's own hub-symbol ranking flagged
(`txn::get` — coreness 16, `caller_count: 283`, `is_hub: true` in `repo_overview`'s
`core_symbols` — the single most central symbol in the whole 5238-symbol index) against the
actual schema on disk, not just the doc comments describing it.

**The durable tables (`edit_transactions`, `tx_events`, `audit_ledger`, `maintenance_jobs`,
`project_memory`, `project_memory_refs`) exist ONLY inside `STATE_SCHEMA_SQL`
(`schema.rs:214-333`).** `SCHEMA_SQL` (the string `init_db` executes against `index.db`) ends at
line ~206 with `pattern_debt` — none of the six durable tables appear in it. `run_migrations`
(`schema.rs:517-655`, also run by `init_db`) only ever calls `migrate_add_column` against
pre-existing index tables (`symbols`, `call_sites`, `call_edges`, `file_index`) — it creates no
tables. The comment at `schema.rs:644-647` says so directly: *"project_memory's own
content_mac/quarantined columns are handled by STATE_SCHEMA_SQL/init_state_db now (project_memory
lives in state.db, not here)"*.

Cross-checked against `git show 9773d41 -- crates/calm-core/src/db/schema.rs`: PR #58 added
`STATE_SCHEMA_SQL`/`init_state_db`/`migrate_legacy_durable_tables` as new code: it did not move
any `CREATE TABLE` out of `SCHEMA_SQL` (no removal diff lines for any durable table there),
because — per `git log --oneline -- crates/calm-core/src/db/schema.rs` — the durable tables
were never in `SCHEMA_SQL` to begin with; they were added straight into what's now
`STATE_SCHEMA_SQL` in this same PR.

Meanwhile `grep -rn "init_state_db\|open_state_writer"` across `crates/**/*.rs` shows **zero
production call sites** — every hit is `#[cfg(test)]`. Every real `calm-server`/`calm-cli` path
still calls `open_writer`/`init_db` (index.db) for durable work — `common.rs:46`
(`new_with_preset`'s startup `init_db`), `memory_write_conn` (`common.rs:319`), the shadow-txn
sites in `edit.rs`, all of `tools/txn.rs`.

**Consequence:** on a project whose `.calm/index.db` was created fresh under current HEAD, every
`INSERT INTO edit_transactions`/`project_memory`/`maintenance_jobs`/`tx_events`/`audit_ledger`
issued by the runtime targets a table that **does not exist** in the file that connection opened.
Most call sites swallow the error (best-effort shadow-txn writes in `edit.rs`, startup
`recover_incomplete`/`reconcile_stale_at_startup` behind `if let Ok(...)`), so nothing crashes —
but `remember` returns `WRITE_FAILED`, `recall` returns `QUERY_FAILED`, and the entire durable
edit-transaction journal / audit ledger / maintenance outbox that 0.5.0's CHANGELOG entry
describes as shipped is silently a no-op on any index built after PR #58. (Needs empirical
confirmation on a truly fresh `.calm/` before or during Phase 1 below — see §5 test plan item
(f) — since a locally-tested project's `index.db` may predate the split and still carry the old
`IF NOT EXISTS` tables from 0.5.0, masking the gap.)

This reframes the task: KNOWN_LIMITATIONS.md's own framing ("a separate follow-up... rather than
attempted alongside the schema/migration groundwork") reads as if durability today is merely
sub-optimal (NORMAL vs FULL). It is not — on a fresh index, the durable-state feature set is
currently broken outright, and this plan is the fix, not just the hardening pass.

## §1. Architecture decision: two connections, not ATTACH

| Option | Verdict |
|---|---|
| **A. Two separate connections** — `open_writer(index.db)` stays for rebuildable index/graph work, a new `open_state_writer(state.db)` handles every durable read/write | **Chosen.** |
| **B. `ATTACH DATABASE state.db AS state` onto the existing index connection**, qualify durable table names as `state.<table>` | Rejected |

Why B is rejected: every durable-write core function (`txn::begin/advance/advance_many`,
`ledger::append/verify_chain`, `maintenance::enqueue/mark_running/mark_completed/all_jobs`,
`memory::store_refs/check_staleness/ref_count/notes_for_path`) takes a bare `conn: &Connection`
and issues unqualified SQL (`INSERT INTO edit_transactions ...`, `SELECT ... FROM
project_memory`). These same functions are called from both write paths (need the table at
`main`) and read paths via `make_read_conn()` today. Making them ATTACH-aware would mean
threading a schema-qualifier through every one of ~15 core functions, or duplicating each query —
strictly worse than opening a second physical connection, for no benefit: SQLite's `synchronous`
pragma is per-connection, not per-attached-database, so an `ATTACH`ed `state.db` off the index
connection would silently inherit whatever `synchronous` level that connection was opened with —
defeating the entire point of the split (durable writes need `FULL`, unconditionally, not "FULL
if whoever opened the index connection happened to ask for it").

**Why splitting across two files/connections is safe** (not just convenient): `txn::begin` and
`txn::advance` already wrap themselves in their own `BEGIN IMMEDIATE`/`COMMIT` (`txn.rs:206`,
`txn.rs:343`), independent of whatever transaction the caller's index-side reindex is in — even
on today's *shared* connection, the journal write and the index rebuild are already two separate
SQLite transactions, not one atomic unit. `common.rs:63` (`recover_incomplete`) exists precisely
to reconcile the gap between "journal says X" and "index/disk says Y" after a crash between the
two. Moving the journal to a second connection changes nothing about this — it was never atomic
with the index write to begin with.

`ledger::append` is called from exactly one place, `txn::write_transition`
(`txn.rs:262`, `append_ledger_in_savepoint`) — so `audit_ledger` automatically follows
`edit_transactions`/`tx_events` onto `state.db` once `txn::*` calls are rewired. No separate
ledger call sites need to change.

## §2. Complete call-site inventory

### §2.1 Rewire to state.db — WRITE

| Site | Durable op | Notes |
|---|---|---|
| `tools/common.rs:46-85` (`new_with_preset`, startup) | `init_db` stays on index.db; **add** `open_state_writer`+`init_state_db`+`migrate_legacy_durable_tables`; move `recover_incomplete`(:63) and `reconcile_stale_at_startup`(:74) onto the new state connection | ordering matters — see §3 Phase 1 |
| `tools/common.rs:319` `memory_write_conn` | `remember`'s `INSERT INTO project_memory` | rename/redirect to `open_state_writer` |
| `tools/edit.rs:1555` `shared_conn` (edit_lines_impl_gated) | `txn::begin`(:1563), `advance`(:1576,:1593), `maintenance::enqueue`(:1705), `mark_running`(:1712), `mark_completed`(:1719) | reindex (`:1636 reindex_paths`) stays on `shared_conn`/index.db; add one `state_conn`, opened once, reused for every durable call in the critical section |
| `tools/edit.rs:668-772` (format_files_impl) | `txn::begin`(:689), `advance`(:713,:727), `advance_many`(:793,:821) | `reindex_conn`(:754) stays index.db |
| `tools/txn.rs:229,234,281` (`retry_maintenance`) | `maintenance::force_requeue`/`mark_running`/`mark_completed` | job **state** marks are durable; the actual scip/embed refresh those trigger still writes index.db separately |
| `tools/txn.rs:581,586` (`verify_change`/gate advance) | `txn::advance` | |

### §2.2 Rewire to state.db — READ

| Site | Durable read | Notes |
|---|---|---|
| `tools/memory.rs:109` (`recall`) | `project_memory`/`project_memory_fts`/`project_memory_refs` via `store_refs`/`check_staleness`(:215,:220)/`ref_count` | |
| `tools/orient.rs:205` (`repo_overview`) | `SELECT COUNT(*) FROM project_memory` for `memory_notes_count` | |
| `tools/txn.rs:52,111,179,323,405` | `txn::get`/`replay_state`/`maintenance::all_jobs` | |
| `tools/edit.rs:389` (`edit_context`, `related_notes`) | `notes_for_path` | this handler ALSO reads index-side callers in the same call — needs a second, state-side read connection alongside the existing index one, not a wholesale swap |

### §2.3 Explicitly NOT touched (stays on index.db)

`watch_supervisor.rs:519,925,1001,1074`; `scip_overlay.rs:319`; `tools/lsp.rs:26`;
`tools/scip.rs:32`; `tools/txn.rs:255` (embed bootstrap inside `retry_maintenance`);
`common.rs:758` (embeddings retry thread); `edit.rs`'s `reindex_paths` calls; `lib.rs:221,280,514,
758,779`; `common.rs:46`'s `init_db` call itself; CLI `main.rs:436,703,858` (`calm index`, `calm
fitness-check`, `calm scip-run` — all index/graph-only, no durable tables involved).

## §3. Sequenced implementation

**Phase 0 — plumbing, no behavior change.**
1. `lib.rs`: add `default_state_db_path(project_root) -> PathBuf` mirroring
   `default_db_path` (`lib.rs:722`) — `.calm/state.db`.
2. `tools.rs:239` `CalmServer` struct: add `state_db_path: PathBuf` field. `for_connection`
   (`common.rs:140`) uses `..self.clone()` for every field it doesn't explicitly override, so this
   propagates to every per-connection clone with no additional code there.
3. `common.rs`: add `state_write_conn(&self) -> Result<Connection>` (wraps `open_state_writer`,
   mirrors `memory_write_conn`) and `make_state_read_conn(&self) -> Result<Connection>` (mirrors
   whatever `make_read_conn` does today, `query_only` pragma, pointed at `state_db_path`).

**Phase 1 — startup init + migration, exact order matters.** Inside `new_with_preset`
(`common.rs:46`), after the existing index `init_db`:
```
state_conn = open_state_writer(&state_db_path)
init_state_db(&state_conn)
migrate_legacy_durable_tables(&state_conn, &db_path)   // copies rows from a pre-split index.db, idempotent, copy-only
recover_incomplete(&state_conn)                         // was: index conn
reconcile_stale_at_startup(&state_conn)                 // was: index conn
```
Migration must run before recovery/reconciliation so any pre-existing durable rows are already
present in `state.db` when those two scan it.

**Phase 2 — read sites** (§2.2 table): swap `make_read_conn()` for `make_state_read_conn()` at
each. `edit_context` (`edit.rs:389`) keeps its existing index-side connection for callers/callees
and opens a second state-side connection for `notes_for_path`.

**Phase 3 — write sites excluding edit.rs** (§2.1 minus the two `edit.rs` rows):
`memory_write_conn` → `state_write_conn`; `tools/txn.rs`'s four sites.

**Phase 4 — `edit.rs` (highest risk, do last).** Open one `state_conn` per call near where
`shared_conn`/`reindex_conn` is opened today, reuse it for every `txn::`/`maintenance::` call in
that function — matching the existing "one connection reused across the critical section" pattern
`docs/plans/2026-08-02-shadow-txn-connection-consolidation-plan.md` established for the index
side, just as a second connection alongside it rather than folded into it. Reindex itself stays on
`shared_conn`/`reindex_conn` untouched. Net cost: +1 `open_state_writer` per edit (~1.2ms per that
doc's own measurement of `open_writer`'s cost) — accepted, not optimized away, matching this doc's
own precedent of measuring before further micro-optimizing.

**Phase 5 — shutdown, cleanup, docs.** Extend `shutdown_and_checkpoint` (`lib.rs:513`) to also
checkpoint `state.db`. Delete the "Durable state and the rebuildable index share one SQLite file"
entry from `KNOWN_LIMITATIONS.md`. Add a CHANGELOG entry converting the existing Unreleased
"Not yet wired into any real calm-server call site" line into "now wired".

## §4. Risks and mitigations

- **Perf**: +1 connection open per edit in the hottest path. Mitigated by opening once per call
  and reusing (Phase 4), not per durable operation.
- **Contention**: `state.db` gets its own writer lock, separate from index.db's — this is a net
  *improvement*, not a new risk: `remember` no longer contends with the indexer/watcher's index
  writes the way `memory_write_conn`'s doc comment (`common.rs:311-317`) currently reasons about.
- **Legacy index.db rows**: `migrate_legacy_durable_tables` is copy-only (never `DROP`s from the
  source) — a pre-split `index.db` keeps its old durable rows as harmless dead weight after
  migration. Cleaning those up is a separate, explicit follow-up, not part of this plan.
- **Cross-file non-atomicity between index write and journal write**: pre-existing, not
  introduced by this change (see §1's atomicity argument) — `recover_incomplete` is the
  existing, tested mechanism for it and needs no new logic, just a new connection target.

## §5. Test plan

- Update `crates/calm-cli/tests/txn_crash_injection.rs` (`:111,:158`) and
  `crates/calm-cli/src/bin/txn_crash_harness.rs` (`:65`) to open/init `state.db` for the durable
  side; keep `index.db` for whatever indexing the harness does. This is the highest-value test to
  get right — it's the one place that already deliberately crashes the process mid-transition.
- Update the `open_writer`-based durable-row assertions in `tools.rs:3872-4110`'s test helpers to
  point at `state.db`.
- New tests:
  (a) after an edit, the resulting `edit_transactions` row exists in `state.db`, NOT `index.db`.
  (b) the live durable writer connection reports `PRAGMA synchronous` == 2 (FULL).
  (c) upgrade path: seed a pre-split `index.db` with durable rows, start a server, confirm
      `migrate_legacy_durable_tables` copied them into `state.db` and `ledger::verify_chain`
      still passes (seq preserved verbatim per `schema.rs`'s own doc comment).
  (d) `remember` → `recall` round-trip against a freshly split project (regression test for §0's
      finding — this must fail on pre-fix code and pass after).
  (e) `repo_overview.memory_notes_count` reflects a note saved via `remember` after the split.
  (f) **before writing any fix code**, confirm §0 empirically: fresh `.calm/`, call `remember`,
      observe today's actual failure mode (`WRITE_FAILED` or silent no-op) to validate the
      finding isn't a misread of the schema.

## §6. Rollout / verification

- `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`,
  plus the crash-injection suite specifically (not just default `cargo test` scope if it's
  feature-gated).
- Manual: fresh project → confirm `.calm/state.db` (+ `-wal`/`-shm`) is created; `remember`/
  `recall`, `edit_transaction_status`, `maintenance_status` all functional.
- Per this repo's own workflow (AGENTS.md Stage 7): `diff_impact(staged=true)` before every
  commit in the implementation phase — `txn::get`'s hub status (caller_count 283) means Stage 5's
  `edit_context` is mandatory before touching `txn.rs` itself, though this plan does not currently
  require changing `calm-core`'s `txn.rs`/`ledger.rs`/`maintenance.rs`/`memory.rs` bodies at all
  (they're already `Connection`-agnostic) — only their call sites in `calm-server`/`calm-cli`.

**Estimated effort**: M (roughly 8 non-test files touched per §2, plus 2 test/harness files and
CHANGELOG/KNOWN_LIMITATIONS updates) — one to two focused implementation sessions, phased per §3
so each phase is independently committable and `diff_impact`-verifiable.
