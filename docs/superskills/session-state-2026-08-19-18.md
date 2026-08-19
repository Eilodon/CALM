# Session Handoff — 2026-08-19 18:xx (Asia/Ho_Chi_Minh)

## Task Summary
Executing `docs/plans/2026-08-19-evidence-architecture-execution-plan.md` end to end — "tiến hành thực thi phần còn lại của kế hoạch theo cách tối ưu nhất, hiệu quả nhất, chính xác nhất và triệt để nhất". The plan has 10 PRs; PR#1-6 are done. **We are mid-way through PR#7**, which splits the ~7,200-line hotspot `crates/calm-core/src/indexer/pipeline.rs` (GitHub issue #67) into 9 move-only sub-modules under `crates/calm-core/src/indexer/pipeline/`. 6 of 9 slices are shipped+pushed. Slice 7 is fully researched (exact line ranges, exact visibility decisions) but **not yet written or edited** — that is the very next action.

## Current Status
STATUS: IN_PROGRESS — mid PR#7 (slice 7/9)

## Completed Steps

### PR#1-6 (Wave 0)
- ✅ PR#1-5: shipped+pushed (see plan doc for detail — not part of this handoff's scope)
- ✅ PR#6: `AmbiguityGroup` target-aware — shipped+pushed `c67439f`

### PR#7 (pipeline.rs hotspot split) — 6/9 slices done, all shipped+pushed to `origin/main`
| Slice | New module | Moved symbols | Commit |
|---|---|---|---|
| 1 | `pipeline/discovery.rs` | `collect_source_files`, `hash_content` (pub, re-exported), `mtime_secs`, `read_source_capped`, `rel_path`, `upsert_file_index` (private) | `d264ead` |
| 2 | `pipeline/extraction.rs` | `formal_resolution_timeout_count` (pub, re-exported), `extract_file_data`, `persist_file` (private), `formally_resolved_names` (`#[cfg(test)]`) | `d8e7e36` |
| 3 | `pipeline/context.rs` | `build_resolution_context`, `resolve_via_inheritance_closure` (private) | `2769dc7` |
| 4 | `pipeline/reconcile.rs` (largest/riskiest slice — ~620 lines) | `AmbiguityGroup` (`pub(super)`), `resolve_sites_to_edges` (`pub(super)`), `insert_ambiguity_groups_batch` (`pub(super)`) | `1cf7047` |
| 5 | `pipeline/modules.rs` | `resolve_import_targets` (`pub(super)`), + 9 private helpers (`resolve_rust_module`, `resolve_candidates`, `resolve_module_to_path`, `strip_js_emit_extension`, `own_module_dir`, `python_package_root`, `parent_of`, `join_rel`, `normalize_rel`) | `29a8fc9` |
| 6 | `pipeline/graph.rs` | `rebuild_graph` (`pub(super)`), `rebuild_graph_from_index` (pub, re-exported), `IncrementalOutcome` enum (`pub(super)`), `incremental_graph_update` (`pub(super)`), `refresh_caller_counts` (pub, re-exported) | `5047185` |

Local `main` is fully in sync with `origin/main` at `5047185` (verified via `git fetch` + `git log origin/main..HEAD` / `HEAD..origin/main`, both empty) as of this handoff. Nothing pending to commit/push right now.

### pipeline.rs current shape (as of `5047185`, 6032 lines total)
Top-of-file module wiring (this is the pattern every future slice must follow):
```rust
mod discovery;
pub use discovery::{collect_source_files, hash_content};
use discovery::{mtime_secs, read_source_capped, rel_path, upsert_file_index};

mod extraction;
pub use extraction::formal_resolution_timeout_count;
#[cfg(test)]
use extraction::formally_resolved_names;
use extraction::{extract_file_data, persist_file};

mod context;
use context::{build_resolution_context, resolve_via_inheritance_closure};

mod reconcile;
use reconcile::{insert_ambiguity_groups_batch, resolve_sites_to_edges};

mod modules;
use modules::resolve_import_targets;

mod graph;
use graph::{IncrementalOutcome, incremental_graph_update, rebuild_graph};
pub use graph::{rebuild_graph_from_index, refresh_caller_counts};
```
Then consts (`MAX_CALLEE_CANDIDATES`, `MAX_INCREMENTAL_DELTA_PATHS`, `DELTA_QUERY_CHUNK_SIZE`, `MAX_INDEXABLE_FILE_BYTES`, `PARSE_BATCH_SIZE` at line 128), `signature_returns_option_or_result`, `CallSiteRow` type, `now_secs`, `GraphMode` enum+`label()`, `ReindexSummary` struct+`is_noop()`, `remove_file_rows`, `names_for_path`, `CallSiteData` struct, `ExtractedFile` struct, `ExtractedBatchRow` type, `SymbolCandidate` type, `ResolutionCtx` struct — **all of these still live in pipeline.rs, deliberately not moved yet** (shared-type-stays-at-ancestor-until-every-consumer-has-moved pattern). Then `PipelineOutcome` enum through `reindex_paths` (the slice-7 target, see below). Then `#[cfg(test)] mod tests { use super::*; ... }` from line 1267 to EOF (~3500 lines) — **the test module is never split out**, it stays in pipeline.rs for all 9 slices.

## Open Work — PR#7 slices 7, 8, 9 (fully specified below, in dependency order)

### Slice 7/9 — `pipeline/driver.rs` (NEXT ACTION — research complete, not yet written)

**Exact current line ranges to move** (verified via `grep -n` against `5047185`, will need re-verification if pipeline.rs changed since — it hasn't as of this handoff):
- **Region A: lines 229-249** — `remove_file_rows` (229-238), `names_for_path` (245-249)
- **Region B: lines 386-597** — `PipelineOutcome` enum (386-389), `run_indexing_pipeline` (396-402), `run_indexing_pipeline_cancellable` (414-421), `reindex_all_cancellable` (431-437), `reindex_all_cancellable_with_phase` (439-572), `ReindexOutcome` enum (579-582), `reindex_changed` (587-597)
- **Region C: lines 932-1266** — `reindex_changed_cancellable` (932-1107, contains a **nested** `struct Candidate` at 965-976 — moves together, stays nested inside the function, do not hoist it out), `reindex_paths` (1127-1266)

**Do NOT move** (confirmed via `callers()` before disconnect — all callers are still in pipeline.rs and not part of this slice): `ReindexSummary`, `GraphMode`, `ExtractedFile`, `CallSiteData`, `PARSE_BATCH_SIZE` const — these stay in pipeline.rs, pulled into driver.rs via `use super::{...}`.

**Visibility decisions** (verified via `mcp__calm__callers` on each symbol before the MCP disconnect — do not re-derive, just apply):
- `PipelineOutcome`, `ReindexOutcome`: **move as `pub enum`**, re-export via `pub use driver::{PipelineOutcome, ReindexOutcome};` at pipeline.rs top level. Both have real external callers by full path: `crate::indexer::pipeline::PipelineOutcome` from `calm-core/src/indexer/refresh.rs` and `calm-server/src/lib.rs::bootstrap`; `crate::indexer::pipeline::ReindexOutcome` from the same two files plus `calm-core/tests/golden_graph_equivalence.rs`. Also referenced internally by `rebuild_call_site_identity_baseline` (still in pipeline.rs, slice-9 territory) and by the `#[cfg(test)] mod tests` block (via its `use super::*;`) — both automatically resolve once the `pub use` re-export exists in pipeline.rs's own scope, no extra work needed.
- `run_indexing_pipeline`: **pub**, external callers: `calm-cli/src/main.rs`, `calm-core/src/indexer/refresh.rs`, `calm-core/tests/{derived_artifact_versions,golden_graph_equivalence,martin_cross_language,rust_indexing}.rs`, plus ~15 in-file test functions (stay in pipeline.rs's tests module).
- `run_indexing_pipeline_cancellable`: **pub**, external caller `calm-server/src/lib.rs::bootstrap` (×2), internal caller `rebuild_call_site_identity_baseline` (still in pipeline.rs).
- `reindex_all_cancellable`: **pub**, external caller `calm-core/src/indexer/refresh.rs::execute_refresh_cancellable`.
- `reindex_all_cancellable_with_phase`: **private** (plain `fn`, no `pub(super)` needed) — both its only callers (`run_indexing_pipeline_cancellable`, `reindex_all_cancellable`) are moving into driver.rs too.
- `reindex_changed`: **pub**, external caller `calm-server/tests/watcher_integration.rs`.
- `reindex_changed_cancellable`: **pub**, external callers `calm-core/src/indexer/refresh.rs` (×2), `calm-core/tests/golden_graph_equivalence.rs`, `calm-server/src/lib.rs::bootstrap`.
- `reindex_paths`: **pub**, external callers `calm-core/src/indexer/refresh.rs`, `calm-server/src/tools/edit.rs` (×2: `format_files_impl`, `edit_lines_impl_gated`), `calm-core/tests/golden_graph_equivalence.rs` (×6 call sites).
- `remove_file_rows`, `names_for_path`: **private**, move into driver.rs — all 4 call sites of `remove_file_rows` and all 4 of `names_for_path` are inside `reindex_changed_cancellable`/`reindex_paths`, both moving. Verified via `callers()` before disconnect and re-confirmed via native `grep -n` after.

**Imports driver.rs will need** (`use super::{...}` for anything staying in pipeline.rs, regardless of that item's own visibility — plain-private items ARE visible to descendant modules automatically, this is the established rule from slices 3-6):
```rust
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{
    ExtractedFile, GraphMode, IncrementalOutcome, PARSE_BATCH_SIZE, ReindexSummary,
    cached_formal_resolver, cached_resolution_maps, extract_file_data, hash_content,
    incremental_graph_update, invalidate_resolution_maps_cache, is_manifest_path,
    mtime_secs, needs_call_site_identity_baseline, now_secs, persist_file, read_source_capped,
    rebuild_call_site_identity_baseline, rebuild_graph, rel_path, upsert_file_index,
};
use crate::indexer::lang_constants::{is_recognized_unparsed_extension, language_for_extension};
use crate::indexer::parser::ParsedSymbol; // only if ExtractedFile's field types need it in scope — check when writing
```
(`collect_source_files` is already `pub use`d at pipeline.rs top level from `discovery` — pull via `use super::collect_source_files;` same as the rest.)

**Top-of-pipeline.rs wiring to add** (same position as the other 6 `mod` blocks, i.e. right after the `graph` block):
```rust
mod driver;
use driver::reindex_all_cancellable_with_phase; // only if something outside driver.rs still needs it — verify at write time, likely NOT needed (private, no outside caller)
pub use driver::{
    PipelineOutcome, ReindexOutcome, reindex_all_cancellable, reindex_changed,
    reindex_changed_cancellable, reindex_paths, run_indexing_pipeline,
    run_indexing_pipeline_cancellable,
};
```

**Execution steps** (apply the established 12-step process below — Region C first since it has the highest line numbers, then Region B, then Region A, so earlier deletions don't shift the line numbers of ones not yet processed):
1. `mcp__calm__callers` re-verification is optional (already done and recorded above) unless pipeline.rs has changed since `5047185` — check `git log` first.
2. `mcp__calm__source` read exact current byte content for regions A, B, C (or reuse content already known — byte-identical to what's quoted in this doc's prior research, re-verify with a fresh read since edit tools require current hashes anyway).
3. `Write` `crates/calm-core/src/indexer/pipeline/driver.rs` with all moved symbols in original relative order (Region A content first since it appears first in the file, then Region B, then Region C — the file's own internal order doesn't have to match pipeline.rs's, but keeping it in original order aids review).
4. Zero-hash preview-diff each region (`edit_lines` with no `expected_hash`) against the new file's corresponding slice, byte-for-byte.
5. Apply real `edit_lines` calls, **Region C first, then B, then A** (highest line numbers first so earlier edits don't invalidate later line numbers). Each call: delete the region, and only on the Region B edit (or wherever convenient) also insert the `mod driver; ... pub use driver::{...};` block.
6. Expect `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` — ask the user to run (full path, `calm` on $PATH is a stale install without `review` subcommand):
   ```
   /home/ybao/B.1/CALM/target/debug/calm review show <id>
   /home/ybao/B.1/CALM/target/debug/calm review approve <id>
   ```
   Wait for their real confirmation (they'll say something like "ok đã duyệt" / "duyệt rồi" in Vietnamese) before retrying the edit with `confirm: true`.
7. Expect `EDIT_CONTEXT_REQUIRED` cascades — call `mcp__calm__edit_context` for every named symbol in each deletion range before the `edit_lines` for that range will succeed (e.g. for Region C: `reindex_changed_cancellable`, `Candidate`, `reindex_paths`; for Region B: `PipelineOutcome`, `run_indexing_pipeline`, `run_indexing_pipeline_cancellable`, `reindex_all_cancellable`, `reindex_all_cancellable_with_phase`, `ReindexOutcome`, `reindex_changed`; for Region A: `remove_file_rows`, `names_for_path`).
8. `cargo build -p calm-core --all-features` — fix unused-import warnings (expect at least the imports pipeline.rs no longer needs directly, e.g. if `HashMap`/`HashSet` become solely driver.rs's concern check pipeline.rs's own top-level `use std::collections::{HashMap, HashSet};` is still needed elsewhere before removing).
9. `cargo clippy -p calm-core --all-features --all-targets` — must be 100% clean.
10. `cargo test -p calm-core --all-features` **in background** (foreground runs are slow — ~40s+ for the full 1306+ tests) — wait for the notification, confirm `test result: ok. 1306 passed; 0 failed; ...` (count may have grown slightly — check against the prior slice's baseline) AND both `golden_equivalence_incremental_vs_fresh_across_mutation_rounds`/`golden_equivalence_continued_vs_fresh_across_mutation_rounds` show `... ok`.
11. `mcp__calm__format_files` on `pipeline.rs` + `pipeline/driver.rs`; confirm `cargo fmt --all -- --check` clean.
12. Stage both files, `mcp__calm__diff_impact(staged=true)`, commit (message style: see the 6 prior slice commits, e.g. `git log --oneline -6` — follow the exact structure: what moved, visibility rationale citing the `callers()` evidence, zero-net-logic-diff claim, verification summary), `git push origin main`.

### Slice 8/9 — `resolver/cache.rs`
Move: `cached_formal_resolver` (line 621 as of `5047185`), `ResolutionMaps` struct stays put (already `pub`, shared — do NOT move), `CachedResolutionMaps` struct (656-662), `cached_resolution_maps` (689-732), `is_manifest_path` (740-742), `invalidate_resolution_maps_cache` (754-760).

**⚠️ New visibility wrinkle not seen in slices 1-6**: once slice 7 lands, **`driver.rs` (a sibling module, not an ancestor) calls all four of these functions**. Sibling modules do NOT automatically see each other's private items — only the ancestor/descendant relationship grants that. So this slice must make `cached_formal_resolver`, `cached_resolution_maps`, `invalidate_resolution_maps_cache`, `is_manifest_path` **`pub(super)`** in the new `cache.rs` (not plain private, unlike every slice-8-adjacent function in slices 1-6), and `driver.rs` must gain `use super::cache::{cached_formal_resolver, cached_resolution_maps, invalidate_resolution_maps_cache, is_manifest_path};` (replacing its slice-7-era `use super::{cached_formal_resolver, ...}` which relied on them still being plain pipeline.rs-level items). This is a two-file edit: `cache.rs` (new) + `driver.rs`'s import line (existing, from slice 7).

Re-run `callers()` on all four before moving to confirm no OTHER caller exists outside {driver.rs, cache.rs itself} — expected to be clean since `cached_resolution_maps`/`invalidate_resolution_maps_cache` are also called from `graph.rs`'s `rebuild_graph_from_index` (already `pub` there, already `use super::{cached_resolution_maps, invalidate_resolution_maps_cache}` from slice 6) — **graph.rs will need the SAME import fix** (`use super::cache::{cached_resolution_maps, invalidate_resolution_maps_cache};`) since it's also a sibling of the new cache.rs. Check `crates/calm-core/src/indexer/pipeline/graph.rs`'s current `use super::{...}` block for these two names before editing.

### Slice 9/9 — `pipeline/identity_migration.rs`
Move: `needs_call_site_identity_baseline` (766-778), `record_call_site_identity_migration_status` (783-839), `rebuild_call_site_identity_baseline` (844-930).

**Same sibling-visibility wrinkle as slice 8**: `driver.rs` calls `needs_call_site_identity_baseline` and `rebuild_call_site_identity_baseline` (from `reindex_changed_cancellable`/`reindex_paths`) — both must become `pub(super)` in the new `identity_migration.rs`, and `driver.rs`'s imports need `use super::identity_migration::{needs_call_site_identity_baseline, rebuild_call_site_identity_baseline};`.

**Reverse dependency**: `rebuild_call_site_identity_baseline` itself calls `run_indexing_pipeline_cancellable`, which by this point lives in `driver.rs` (`pub`) — so `identity_migration.rs` needs `use super::driver::run_indexing_pipeline_cancellable;`. Also uses `PipelineOutcome`/`ReindexOutcome` (both `pub` in driver.rs by now) and `ReindexSummary` (stays in pipeline.rs) — pull the former via `use super::driver::{PipelineOutcome, ReindexOutcome};`, the latter via `use super::ReindexSummary;`.

After slice 9 lands, **pipeline.rs's PR#7 split is complete**: the file should contain only shared struct/type/const definitions, the 9 `mod` + import/re-export blocks, `now_secs`/`signature_returns_option_or_result`, and the untouched `#[cfg(test)] mod tests` block. This is the target end-state — no further pipeline.rs slicing is planned beyond slice 9.

## Open Decisions
None outstanding — every visibility/move decision for slices 7-9 has already been derived from real `callers()` evidence (recorded above) or from the established Rust-privacy rules used consistently across slices 1-6. Nothing here requires a judgment call; it requires execution.

## Active Context
PLAN: `docs/plans/2026-08-19-evidence-architecture-execution-plan.md` (Part E, Wave 1 — governs all of PR#7's slice numbering/grouping/rationale)
BRANCH: `main` (this whole effort commits directly to `main`, one commit per slice, pushed immediately after each — no feature branches, established pattern for all 6 prior slices)
MEMORY: `/home/ybao/.claude/projects/-home-ybao-B-1-CALM/memory/calm-evidence-architecture-audit-verification-2026-08-19.md` — the cross-session progress log for this whole multi-day effort. **Last updated through slice 3-4; slices 5 and 6 were never recorded there** — the next session should update it after finishing slice 7 (covering 5, 6, AND 7 in one pass) to keep it from drifting further out of sync.

## Evidence Produced This Session
- All 6 completed slices (1-6): full build+clippy+test(1306 tests)+golden-equivalence-green verification, each individually confirmed before its commit — see each commit's message on `origin/main` for the specific verification summary (`git log --oneline -8` from `main`).
- Slice 7's exact line ranges, visibility table, and required imports (documented in full above) — derived from real `mcp__calm__callers()` calls made earlier this session, before the MCP connection dropped. Cross-checked against a fresh native `grep -n` after the disconnect (both agree exactly, pipeline.rs has not changed since `5047185`).
- Slice 8/9's sibling-module `pub(super)` requirement (documented above) — derived by reasoning about Rust's privacy model (ancestor/descendant visibility only, siblings excluded) applied to the fact that slice 7's `driver.rs` will call into not-yet-extracted functions that slices 8/9 will relocate. Not yet verified against real `callers()` output for slice 8/9's specific functions (do that first, per the process, before writing either module) — but the STRUCTURAL conclusion (siblings need `pub(super)` + cross-module `use super::X::{...}`) is sound regardless of exact caller list.

## Blockers
🚫 **CALM MCP tools disconnected for this session** (all `mcp__calm__*` tools unavailable via `ToolSearch`). Root cause fully diagnosed and fixed server-side: `.calm/daemon.meta` got deleted by a losing daemon-spawn candidate during a thundering-herd race (triggered by a window reload SIGTERM'ing the old daemon while ~3 long-abandoned `calm connect` processes — accumulated over days from other editor sessions, Cursor/Windsurf — simultaneously raced to reclaim the socket). Fixed by killing the affected daemon and letting a fresh one spawn with a clean `daemon.meta`. **Server-side is now confirmed healthy** (fresh `daemon.meta`, clean daemon.log, active `calm connect`/`calm serve` processes for a NEW session as of 18:10 local time) — but tools still hadn't repopulated in *this* session's `ToolSearch` as of this handoff, suggesting a client-side (VSCode extension) tool-list refresh issue independent of the server fix. **Next session should check `ToolSearch` for `mcp__calm__*` immediately on start** — if still missing, the user needs to fully reload/restart the client again; if present, proceed directly with slice 7 using this document's exact specs (no re-research needed).

If CALM tools are still down next session and the user wants to proceed anyway: the calm-first output style permits native Read/Grep/Edit as a fallback when CALM is genuinely unavailable, BUT this repo's pipeline.rs is a `HIGH_RISK` hotspot (issue #67) whose edit gate is specifically designed to require CALM's own `edit_context`/`edit_lines`/review flow — **do not bypass it with native Edit even under fallback justification**. Wait for CALM to reconnect before touching pipeline.rs; use the wait productively (re-verify line numbers via native grep, as this document already has).

## Next Session Opening
"Resuming PR#7 slice 7/9 (`pipeline/driver.rs`) from the 2026-08-19 handoff. First: confirm `mcp__calm__*` tools are available via `ToolSearch`; if not, tell the user plainly and wait. Once available: re-verify pipeline.rs hasn't changed since commit `5047185` (`git log --oneline -1` should show `5047185` as HEAD or a descendant with no pipeline.rs changes), then execute slice 7 exactly per this document's 'Open Work' section — no new research needed, the line ranges/visibility table/imports are already fully specified above."

## Skills in Use
None of this repo's own Super Skills scaffolding (`docs/superskills/specs`, ADRs) is actively gating this task — PR#7 is a mechanical move-only refactor executed directly against `docs/plans/2026-08-19-evidence-architecture-execution-plan.md`, not through the spec→audit-design→writing-plans pipeline. `session-handoff` itself is the only skill invoked this session.
