# ADR: Fix a parallel-test flake caused by empty-string DB paths colliding in the embedding cache

## 1. Title
Stop `knn`/`knn_chunks`' process-wide embedding cache from treating every `:memory:`
connection as the same database, which was silently contaminating parallel test runs.

## 2. Context
`calm-core::search::tests::search_similar_truncated_flag_is_accurate` flaked intermittently
during `cargo test --workspace` (first observed and logged as "pre-existing, unrelated" in
the `split-common-hotspot` ADR/session on this same date). Investigated per explicit user
request ("điều tra, nghiên cứu, phân tích" — investigate/research/analyze) after the
unrelated `split-common-hotspot` branch was merged to `main`.

Root cause traced to `crates/calm-core/src/embedding.rs`'s `knn`/`knn_chunks`: both cache
decoded embedding vectors in a process-wide `OnceLock<Mutex<HashMap<String, Vec<...>>>>`
keyed by `conn.path()`. The existing code comment stated the designer's intent plainly:
in-memory `:memory:` connections were assumed to make `conn.path()` return `None`, bypassing
the cache entirely so tests could never leak state into each other. That assumption is false
on this project's pinned `rusqlite = "0.34"`: `Connection::path()` wraps
`sqlite3_db_filename`, which returns a valid (non-null) pointer to an **empty string** for an
in-memory main database, not a null pointer — confirmed by reading rusqlite 0.34.0's actual
`inner_connection::db_filename` source, not assumed from docs. So `conn.path()` returns
`Some("")` for every `Connection::open_in_memory()` in the process, and every in-memory-DB
test in the crate collided on the single cache key `""`.

## 3. Decision
Added `stable_db_path(conn: &Connection) -> Option<&str>` (`crates/calm-core/src/embedding.rs`,
next to `PathCache`/`symbol_cache`/`chunk_cache`): `conn.path().filter(|p| !p.is_empty())`.
Routed `knn`, `knn_chunks`, and `invalidate` (shared by both `symbol_cache()` and
`chunk_cache()`, called from `store_embedding`/`store_chunk_embedding`/
`prune_orphaned_chunk_vecs`/`heal_dimension_mismatch`) through it instead of matching
`conn.path()` directly. An anonymous/in-memory database has no stable identity, so
path-keyed process-wide caching for it is unsound by construction regardless of what
`sqlite3_db_filename` happens to return — this makes that explicit rather than relying on a
false premise about `None`.

Added a deterministic regression test,
`knn_chunks_does_not_leak_across_distinct_in_memory_connections`
(`crates/calm-core/src/embedding.rs`, after `knn_chunks_with_synthetic_vectors`): two separate
`Connection::open_in_memory()` connections, each with distinct chunk data, queried
back-to-back in the same process (no thread races needed to prove the bug — before the fix,
`conn_b`'s query would read back `conn_a`'s cached vectors because both mapped to key `""`).

## 4. Status
ACCEPTED

## 5. Consequences

**Improved:**
- Eliminates a real, reproduced flake: looping the `calm-core` unit-test binary
  (`embedding::`/`search::` filter, 8 threads, 60 iterations) went from 4/60 failures
  pre-fix to 0/60 post-fix.
- The fix is a pure test-isolation correctness fix, not a behavior change for production:
  grepped the whole workspace — `Connection::open_in_memory()` is used only in `#[test]`
  functions, never in any `CalmServer`/daemon/CLI code path, so real on-disk-path caching
  (never empty) is untouched.
- `invalidate` fixing both `symbol_cache()` and `chunk_cache()` in one place means the
  equivalent bug in the symbol-level `knn` (used by `search_semantic`'s symbol path, exercised
  by `knn_with_synthetic_vectors`) is fixed by the same one-function change, not just the
  chunk-level path that happened to be the one observed flaking.

**Worsened / new surface:**
- None identified. `stable_db_path` is a strict narrowing of what already counted as "has a
  cache-able path" — no real on-disk path is ever the empty string, so no existing
  non-test caching behavior changes.

## 6. Alternatives Considered
- **Mark the flaky test `#[ignore]` or serialize it with a mutex.** Rejected: treats the
  symptom, not the cause — every other in-memory-DB test touching `knn`/`knn_chunks` (at
  least 3 more in `embedding.rs` alone) remains exposed to the same race, and a future test
  would silently inherit it again.
- **Give every in-memory connection a unique synthetic cache key (e.g. a counter or the
  `Connection`'s pointer address).** Rejected: more moving parts for no benefit — the
  simplest fix is "don't cache what has no stable identity," which is also what the original
  code's own comment already claimed to do.

## 7. Evidence
- Reproduced pre-fix: looped the compiled `calm-core` test binary 60x with
  `--test-threads=8` filtering `embedding::`/`search::` — 4 failures, both observed assertion
  shapes (`left: 2, right: 3` at `search.rs:2127`, and the missing-`truncated`-flag case at
  `search.rs:2136`) fully consistent with cache contamination from another in-memory test's
  vectors — `[verified 2026-07-28]`.
- Post-fix: identical 60-iteration loop — 0 failures — `[verified 2026-07-28]`.
- New regression test `knn_chunks_does_not_leak_across_distinct_in_memory_connections` passes
  — `[verified 2026-07-28]`.
- `cargo fmt --all -- --check`: exit 0 — `[verified 2026-07-28]`.
- `cargo test --workspace` (default features): exit 0 across the whole workspace (calm-server
  lib 272/272, watcher_integration 3/3, doc-tests 0/0 across all three crates) —
  `[verified 2026-07-28]`.

## 8. Owner
Your Name

## 8b. Known Debts (PATTERN-DEBT)
No new PATTERN-DEBT entries introduced. No open entry in `docs/pattern-debt-registry.yaml`
(checked: `DEBT-006-ty-subprocess-premise-invalid`, `DEBT-011-martin-avg-distance-unfailable-threshold`)
relates to this change.

## 9. Next Cycle Trigger
When any future `cargo test --workspace` run flakes on a test that opens a
`Connection::open_in_memory()` and touches `knn`/`knn_chunks`/`symbol_cache`/`chunk_cache` —
that would mean either a new code path bypasses `stable_db_path` (a review miss, since there
are now only 3 call sites total) or rusqlite's `Connection::path()` semantics changed again on
a future version bump.

## 10. Cycle Retrospective
- Assumption that proved wrong: the *existing* code's own doc comment ("`Connection::path()`
  returns `None`" for `:memory:`) was taken at face value by whoever wrote it, and never
  verified against the actual pinned rusqlite/SQLite behavior. Any future cache keyed on
  `Connection::path()` in this codebase should re-check this rather than trust the comment.
- Surprise: the bug was reproducible in well under a minute once the exact mechanism was
  understood (60 iterations, ~1s each) — parallel-test flakes that look like "rare, ignore
  it" are often deterministic races that just need the right repro loop, not a fundamentally
  hard-to-catch heisenbug.
- What we'd design differently: `PathCache`'s doc comment should have stated the actual
  guarantee ("never cache a database with no stable on-disk identity") instead of an
  implementation detail ("`path()` returns `None`") that happened to be false — a
  guarantee-level comment would have made this bug visible on read instead of requiring a
  live reproduction.
- Debt knowingly left alone: `knn`'s companion `bench_knn_latency_100k_256dim` benchmark uses
  a `fresh` real (non-in-memory) connection deliberately to test cold-cache behavior — not
  touched here since it was never affected (real paths are never empty).
- Signal to watch: any new `#[test]` in this crate that opens more than one
  `Connection::open_in_memory()` in the same test process and expects isolated results is now
  safe by construction — no per-test workaround needed going forward.
