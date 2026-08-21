---
title: "Truth-kernel hardening — identity/freshness audit verification + execution plan"
date: 2026-08-20
status: "Verification complete (12/12 findings traced to live code). Wave 0 (0.1-0.5)
  SHIPPED same day, uncommitted: source_range path containment, suggested_next arg
  fixes + regression tests, kind=text doc-truth fix, edit_context annotation fix,
  defensive clamps in source()/understand()/symbols_batch. P0-6 (new finding, see
  below) surfaced during Wave 0 execution -- edit_context's gate_prediction
  structurally cannot predict the uncovered-code risk floor, confirmed live 4
  separate times this session; fix not yet implemented (candidate documented,
  optional 0.6).

  Wave 1 (Live Truth Kernel) SHIPPED 2026-08-20 same day, uncommitted, same session
  as Wave 0: live-verification folded directly into `resolve_symbol` itself (design
  decision, user-confirmed -- see Wave 1 section for the full rationale), not a
  separate opt-in function. New `SymbolResolution::ReadFailed` variant; new
  `verify_live`/`match_live_symbol` functions in outcome.rs; `resolve_symbol` gained
  a `project_root` parameter, threaded through all 9 pre-existing call sites (10
  call-sites counting `path()`'s two) plus a compile-time-caught 10th (missing match
  arm on `path()`'s `to` block). `best_live_range` (edit.rs, the insertion-anchor
  path) upgraded to share `match_live_symbol`'s (name, kind, class_context) key,
  fixing P0-1f there too; `insertion_hunk_for`'s fresh-parse failure no longer falls
  back to stale coordinates (P0-1g). `understand()` and `symbols_batch()` -- the two
  call sites that bypassed `resolve_symbol`/`resolve_symbol_candidates` entirely --
  refactored to route through it (P0-1d). 5 new adversarial tests added (stale/moved,
  duplicate-class_context, genuinely-ambiguous, deleted, concurrent-drift), all
  passing on first run. cargo test -p calm-server --lib: 413/413 green (408 pre-
  existing + 5 new). fmt clean (format_files), clippy clean (0 warnings). No toolsnap
  changes needed (Wave 1 touched internal resolution logic only, no #[tool] schema
  changed). diff_impact confirms the full change surface matches expectations (10
  files, all `resolve_symbol`-downstream tools, no unexpected touches).

  One documented design deviation from this doc's original Wave 1 draft: the
  'AmbiguousLive' outcome (2+ ties after a live re-parse) is surfaced as
  `SymbolResolution::ReadFailed` with code `STALE_AMBIGUOUS`, not as a fabricated
  `Ambiguous(candidates)` list -- building full CandidateRow instances from bare
  ParsedSymbol data would require inventing caller_count/coreness/etc. that a fresh
  parse cannot know, which risked exactly the kind of confidently-wrong metadata this
  wave exists to eliminate. The DoD's actual intent (never guess, never panic, non-
  silent) is preserved; only the specific error shape differs from the original
  sketch. Also documented as an explicit known residual (not a regression): the
  DB-`Ambiguous` (2+ candidates before any live check) path is not itself
  live-re-verified per-candidate in this pass -- only the already-narrowed single
  `Found` candidate is.

  Wave 2 (Evidence/confidence semantics) SHIPPED 2026-08-20, uncommitted, same
  session: 2.1 -- `EvidenceSnapshot::compute` gained a mtime-only live-disk spot-check
  (`live_mtime_drift`, `authority/snapshot.rs`) that downgrades `Current` to
  `Degraded` when any `file_index` row's live mtime disagrees with its stored value
  (fail-closed on missing file / NULL mtime), reusing `indexer::pipeline::mtime_secs`
  (widened `pub(super)` -> `pub(crate)`, re-exported) rather than a second conversion.
  Design-decided mtime-only (not mtime+size or full rehash) because `compute` runs on
  every gated edit (`edit_lines_impl_gated`), not just `edit_context` -- confirmed via
  its real production callers -- so a full rehash there would double I/O across the
  whole catalog on every edit; documented residual: same-mtime-different-content is
  not caught by this signal alone. Doc comment at `compute_with_recorded_freshness`
  corrected (the old "no TOCTOU window" claim overstated what `snapshot_id` alone
  covers). 2.2 -- the reconciliation-fence: `watch_supervisor.rs`'s post-reconciliation
  persist switched from unconditional `compute_after_reconciliation` to plain
  `compute()` + a `Current -> Reconciled` promotion, so live-mtime drift caught during
  the reconciliation's own reindex/overlay window blocks the `Reconciled` claim instead
  of being silently overwritten (`compute`'s output space is exactly
  `{Current, Degraded}`, never `Reconciled` on its own, so the promotion only ever
  strengthens what it verified). 2.3 -- narrowed scope per the research pass below:
  landed `EdgeConfidence::is_verified`/`is_probable`/`is_lexical_lead` (canonical
  predicates, `types.rs`), a new additive `symbols.verified_caller_count` column
  (schema migration + `refresh_caller_counts` populating it alongside the unchanged
  `caller_count`, zero consumers yet -- mirrors the existing `coreness`/
  `possible_coreness` dual-column precedent), `trace.rs`'s `path()` `certain` flag
  tightened from `rank() > 0` to `is_verified()` (a documented correctness fix, not a
  gate-loosening change -- confirmed zero server-side readers of `certain` exist
  beyond the JSON output), and `edit.rs`'s bridge-gate SQL cross-referenced to
  `is_verified()` in a comment (can't literally share code across the Rust/SQL
  boundary). The `coreness.rs`/`refresh_caller_counts`-bucket safety-relevant question
  (tightening hub-detection would loosen the `confirm:true` edit gate for
  weakly-resolved symbols) was explicitly deferred, not attempted -- needs a real
  false-hub-rate measurement pass first. 9 new tests added (3 for 2.1's drift-detect
  + 2 fail-closed paths, 1 for 2.2's happy-path regression, 2 for 2.3's
  verified_caller_count bucket split); one documented test-coverage gap: 2.2's
  "drift injected mid-reconciliation" adversarial case (the plan's own original DoD)
  needs a delay-hook test-infra investment not built in this pass. Full
  `cargo test --workspace` green (calm-server 414, calm-core 1264, plus schema-
  migration/watcher-integration suites), fmt clean, clippy clean (0 warnings),
  `diff_impact` confirms the full change surface (Wave 0+1+2 combined, still
  uncommitted) matches expectations.

  Wave 3 (Assistant-grade retrieval) SHIPPED 2026-08-21, uncommitted, same session:
  3.1 -- `search`'s default `kind` flipped from `"symbol"` to `"hybrid"`
  (`default_symbol` renamed `default_search_kind`, both the serde default and the
  `unwrap_or` fallback in `search()`'s own dispatch updated together). 3.2 -- new
  `fts_chunks` FTS5 table over `code_chunks.chunk_text` (`content='code_chunks'`,
  `content_rowid='id'`) with `code_chunks_ai`/`code_chunks_ad` triggers mirroring
  `symbols_ai`/`symbols_ad` exactly (insert/delete only, confirmed no in-place
  update path), backfilled for pre-existing rows via `migrate_fts_chunks`'s
  `INSERT INTO fts_chunks(fts_chunks) VALUES ('rebuild')`; `search_text` rewritten
  to merge `fts_exact` hits with the new `chunk_text_results` via `rrf_merge_n`
  (gained a `rrf_k` parameter); `kind="text"`'s tool-schema description updated to
  drop Wave 0's 0.3 "does NOT search function bodies" disclaimer now that it's
  true. 3.3 -- `understand()`'s internal search call raised `limit=1` -> `limit=2`;
  new `resolution_confidence`/`alternatives` fields on `UnderstandOutput`, computed
  from a top-1/top-2 score-margin check against a new named constant
  `UNDERSTAND_AMBIGUOUS_MARGIN_RATIO = 0.9`; when ambiguous, `top` is shadowed to
  `None` (naturally empties every downstream field) and `suggested_next` points at
  `symbol_info` with the original query. 3.4 -- the corrected design from the
  research pass (qualified_name as a narrowing SQL filter feeding the *same*
  `verify_live` call, never a bypass): `resolve_symbol`/`resolve_symbol_candidates`
  gained an optional `qualified_name` parameter (sole `WHERE` filter when present,
  `line`-narrowing still applies afterward as a no-op once already unique); threaded
  through all 9 Wave-1 call sites plus `understand()` itself (using the search hit's
  own already-known `qualified_name` directly -- closes the DB-Ambiguous residual
  documented in Wave 1 too, for this one call site). Real `qualified_name` wiring
  scoped to the 7 read-only tools + `understand`; the 2 write-path tools
  (`edit_symbol_flow`, `edit_context`) got only the mechanical arity fix (`None`),
  judged lower-value given their heavier gate/authority machinery -- an inline
  scope-narrowing call, not confirmed via a separate question. `SearchResultItem`
  gained the `qualified_name` field back, gated on `kind.is_some()` (not
  unconditional) after a self-caught bug: `kind="file"` path hits and gap-chunk
  semantic hits without a backing symbol carry a synthetic placeholder identity
  (the raw path, or `"path#chunk:N-M"`) that doesn't resolve against `symbols` --
  surfacing those as if real would send a caller into a guaranteed `NotFound` round
  trip, contradicting the field's own doc comment.

  Both 3.4 DoD assertions landed as tests: `qualified_name_resolves_uniquely_even_for_a_globally_common_bare_name`
  (a real name-tie on "new" across two files -- ambiguous without `qualified_name`,
  resolves uniquely with it) and `qualified_name_lookup_of_a_since_deleted_symbol_still_live_verifies`
  (mirrors Wave 1's `wave1_deleted_symbol_reports_not_found_not_stale_bytes`, driven
  by `qualified_name` instead of a bare name -- proves the new path still reports
  `NotFound`, never a stale read, for a since-deleted symbol).

  Two real bugs caught and fixed during this wave's own implementation, not part of
  the original plan text: (1) the `SearchResultItem.qualified_name` gating bug above,
  caught by `cargo check` immediately after the first landing attempt, fixed in the
  same sitting before it ever shipped un-gated. (2) a **pre-existing** gap from
  earlier struct-field additions this same session (Wave 3's own 3.4 work adding
  `qualified_name`/`from_qualified_name`/`to_qualified_name` fields to 7 `Params`
  structs): `cargo check -p calm-server --lib` had been used throughout as the fast
  compile-check, but `--lib` alone does not compile the `#[cfg(test)]` module --
  `cargo check --lib --tests` (first run as part of this wave's closing verification
  pass) surfaced 65 `E0063` missing-field errors across every pre-existing test call
  site constructing those 7 structs as Rust literals. Fixed as one mechanical batch
  (`qualified_name: None,` / `from_qualified_name: None, to_qualified_name: None,`
  added to each, purely additive, zero existing assertion touched) -- confirmed via
  `cargo check --workspace --all-targets` returning clean afterward. Lesson for
  future waves: run the `--tests` variant of the fast compile-check periodically
  during a struct-shape-changing wave, not only at the very end, since the narrower
  `--lib`-only check can stay green for many edits while a real (if mechanical)
  break accumulates silently in the test module.

  `cargo test --workspace`: every crate green (calm-server 419/419, calm-core
  1265/1265, plus schema-migration/watcher-integration/doctest suites, 0 failures
  workspace-wide). `cargo fmt --check` clean (via `format_files`, not raw
  `cargo fmt --write`, to keep the index in sync). `cargo clippy --workspace
  --all-targets -- -D warnings` clean after one fix (`bool::then(|| ...)` ->
  `then_some(...)` at the three `SearchResultItem`-construction sites, clippy's own
  suggested fix, zero behavior change). Toolsnaps regenerated
  (`UPDATE_TOOLSNAPS=1`) for the 11 tools whose output schema changed
  (`qualified_name` added to `search`/`locate`'s results; `resolution_confidence`/
  `alternatives` added to `understand`; the new `qualified_name` param added to
  `source`/`symbol_info`/`callers`/`callees`/`reference_impact`/`path`/
  `pattern_debt_register`; `edit_context`'s snapshot moved incidentally). `diff_impact`
  confirms the full change surface (Wave 0+1+2+3 combined, still uncommitted) matches
  expectations -- every flagged "high risk, signature modified" symbol is an
  intentional change from this plan, no unexpected touches.

  Wave 4 (Coverage/noise honesty) SHIPPED 2026-08-21, uncommitted, same session:
  4.1a -- `reindex_changed_cancellable`'s `?`-in-closure/`.flatten()` pattern
  (`driver.rs`) now distinguishes a walked-but-transiently-unreadable file (kept
  in `seen_paths` via a new `WalkOutcome::Unreadable` variant) from a genuinely
  unrecognized-extension file (never a candidate, correctly excluded) -- closes
  the real correctness bug found in this wave's own research pass: before the
  fix, a file that momentarily failed `read_source_capped` (transient permission
  hiccup, briefly over `MAX_INDEXABLE_FILE_BYTES`, a non-UTF-8 write mid-flight)
  had its indexed symbols/call_sites actually deleted on the very next
  incremental reindex, even though the file still existed and would read fine
  moments later. Landed first, as its own isolated step, per the user-confirmed
  design decision. 4.1b -- `read_source_capped`'s return type changed from
  `Option<String>` to `Result<String, String>` (`discovery.rs`), where `Err`
  carries a ready-to-persist skip reason (`"too_large:<bytes>"` /
  `"unreadable:<io::ErrorKind Debug repr>"`, the latter folding in invalid-UTF-8
  too since `read_to_string` itself reports that as `InvalidData` -- no separate
  enum/variant needed, simpler than the plan's original sketch); a new
  `mark_file_index_skip_reason` helper persists it to a new nullable
  `file_index.skip_reason TEXT` column (`schema.rs`, `CREATE TABLE` +
  `migrate_add_column`, same low-risk pattern as `mtime`/`symbols.is_test`) while
  leaving a successfully-read file's existing `hash`/`symbol_count` untouched --
  a successful `upsert_file_index` afterwards clears `skip_reason` back to
  `NULL` on its own (`INSERT OR REPLACE` never lists that column). All 3 of
  `read_source_capped`'s callers (`reindex_paths`, `reindex_all_cancellable_
  with_phase`, `reindex_changed_cancellable`) updated to record the reason
  instead of silently dropping the file. Scoped to `read_source_capped` only,
  per the user-confirmed design decision -- `parse_tree`'s analogous
  `UnsupportedLanguage | AbiLoadFailed | Timeout` distinction (9 production call
  sites, a materially wider blast radius) is explicitly deferred, not silently
  dropped. DoD closed beyond the minimum: `indexing_status` (`recover.rs`)
  gained a new `skipped_files: {total, entries[]}` field (capped at 20 entries,
  true count preserved when truncated -- same contract `callers`/`callees`
  already use) so an agent can actually enumerate *which* files are skipped and
  *why*, not just notice `files_indexed` falling short of `files_total` with no
  explanation. 4.2 -- `enclosing_symbol` (`search.rs`) now also selects
  `symbols.is_test` (already computed/stored at index time by
  `detect_is_test`, needed no new heuristic) instead of only
  `name`/`qualified_name`/`kind`; `search_grep` threads the real value through
  instead of hardcoding `is_test: false` for every result, so
  `search(kind="grep", include_tests=false)` -- previously a silent no-op for
  grep specifically, since `apply_include_tests_filter`'s `retain(|r|
  !r.is_test)` had nothing to ever retain against -- now actually excludes
  matches inside test code, matching the behavior `kind="symbol"`/`"hybrid"`
  already had. A match with no enclosing symbol still defaults to
  `is_test: false` (today's behavior, unchanged for that case). One clippy
  fixup mid-wave: the new 4-tuple return type on `enclosing_symbol` tripped
  `type_complexity`, factored into a named `EnclosingSymbolRow` type alias
  (same convention this crate already uses for `ExtractedBatchRow`), zero
  behavior change.

  Research pass (2026-08-21, post-Wave-3) preceded implementation and is
  recorded in full at the top of the Wave 4 section below, including the two
  user-confirmed design decisions (fix the deletion bug first as its own step;
  scope 4.1b to `read_source_capped` only). 4 new regression tests added:
  `test_reindex_changed_does_not_delete_a_transiently_unreadable_file_row`
  (4.1a, the core correctness assertion -- proves symbols/call_sites survive a
  transient read failure), `indexing_status_surfaces_skipped_files_with_their_
  reason` (4.1b DoD), plus updates to the two pre-existing oversized-file tests
  (`read_source_capped_skips_a_file_over_the_byte_cap` and `run_indexing_
  pipeline_skips_an_oversized_file_but_still_indexes_the_rest`) for the new
  `Result` signature and the new intentional "skipped file still earns a
  placeholder row" behavior. `search_handler_grep_include_tests_false_excludes_
  a_match_inside_a_test_symbol` (4.2 DoD, mirrors the pre-existing `kind=symbol`
  version of the same regression to prove grep now behaves the same way).

  `cargo test --workspace`: every crate green (calm-server 421/421, calm-core
  1266/1266, plus schema-migration/watcher-integration/doctest suites, 0
  failures workspace-wide -- one `daemon::tests::bind_or_yield_recovers_a_
  stale_socket` failure during a full parallel run reproduced as a pass in
  isolation, confirmed environmental/flaky, not a regression from this wave's
  changes, which never touch `daemon.rs`). `cargo fmt --check` clean (via
  `format_files`). `cargo clippy --workspace --all-targets -- -D warnings`
  clean after the one `type_complexity` fixup above. Toolsnap regenerated
  (`UPDATE_TOOLSNAPS=1`) for `indexing_status` (new `skipped_files` field) --
  the only tool output schema this wave changed; `search`/`search_grep`'s own
  schema is unaffected since `is_test` was already a documented field, only its
  internal computation changed. `diff_impact` confirms the full change surface
  (Wave 0+1+2+3+4 combined, still uncommitted) matches expectations -- every
  flagged "high risk, signature modified" symbol is an intentional change from
  this plan, no unexpected touches.

  Not committed to git. All 4 waves of this plan are now shipped.
scope: >
  A large incoming audit claimed CALM's read/edit path can silently act on stale
  index coordinates, that its freshness/evidence attestations don't actually bind to
  live disk, that "confirmed" means different things to different consumers, and that
  several tool-surface contracts (suggested_next args, tool annotations, search scope
  claims) don't match implementation. Every claim was cross-checked against real code
  at HEAD (file:line below, several via live tool-call reproduction, one via direct
  SQLite query against the dogfood index), not re-derived from the audit's own text.
  This doc records the verified verdict (Part A) and turns the confirmed debt into a
  prioritized, file-level execution plan (Part B).
verified_against: "HEAD 0d8d1f2d6fa862c058e7f6f0d5fbbe145cc95ca7"
related:
  - docs/plans/2026-08-19-evidence-architecture-execution-plan.md   # sibling truth-chain hardening (resolver → graph), P0-3/P0-4 overlap
  - docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md  # index_input_state/index_input_drift prior analysis (different consumer, epoch-bump not disk-freshness)
  - gh issue #72   # external-qualified-reference roots — P0-4 is the same open issue, Rust/C++ stdlib slice only
---

# Truth-kernel hardening — verification + execution plan

## Part A — Verification verdict

Method: every claim traced to file:line at HEAD `0d8d1f2`; five re-run live against the
running MCP session as of this verification (marked **[live]**); one cross-checked
against the dogfood `index.db` directly via `sqlite3 -readonly` (marked **[db]**).

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| P0-1a | `source()` slices **live** disk bytes at **indexed** (possibly stale) `line_start/line_end` | ✅ Confirmed | `inspect.rs:243-254` — `std::fs::read_to_string` (live) sliced by `c.line_start/c.line_end` (DB), no `file_index.hash` comparison, no re-parse |
| P0-1b | `edit_symbol` replace-without-`old_text` reuses the **same** DB coordinates | ✅ Confirmed | `edit.rs:434-441` — `HunkRequest{ start_line: c.line_start, end_line: c.line_end, ... }` freshly re-resolved but from the same possibly-stale DB row |
| P0-1c | `apply_hunks` hash proves "range unchanged since read", not "range is the right symbol" | ✅ Confirmed | `edit.rs (calm-core):206-280` — hashes `lines[start..end]`, no identity check |
| P0-1d | `understand()` repeats the exact same stale-slice pattern independently | ✅ Confirmed (found during verification, not explicit in audit's cited lines) | `inspect.rs:558-560`, same asymmetric-clamp shape as `source()` |
| P0-1e | Asymmetric clamp can panic (`start` unclamped to `lines.len()`, `end` is) | ✅ Confirmed | `inspect.rs:247-248` — `lines[start..end]` panics if stale `line_start` > current file length |
| P0-1f | `best_live_range` matches **bare name only** — duplicate/overloaded symbols can re-anchor wrong | ✅ Confirmed | `edit.rs:4418-4429` — `symbols.iter().filter(\|s\| s.name == name).min_by_key(...)`, no kind/class_context/signature check |
| P0-1g | Fresh-parse failure falls back to **stale** coordinates instead of failing closed | ✅ Confirmed | `edit.rs:4259` — `Err(_) => (c.line_start as usize, c.line_end as usize)` |
| P0-2a | `EvidenceSnapshot`'s `source_catalog_digest` hashes **DB rows**, not live disk | ✅ Confirmed | `snapshot.rs:314-327` — `SELECT path, hash FROM file_index`, no disk read |
| P0-2b | `index_input_drift` explicitly excludes source hashes by design | ✅ Confirmed | `refresh.rs:682` doc comment: *"Source hashes are deliberately absent: the delta indexer already owns them"* |
| P0-2c | The "any disk change changes the id, no TOCTOU window" doc claim overclaims for the not-yet-indexed case | ✅ Confirmed | `snapshot.rs:173-175` claim vs. mechanism above; real, non-trivial lag window: 500ms debounce + 15min reconciliation interval (`watch_supervisor.rs:148,156`) |
| P0-2d | Full reconciliation persists `Reconciled` from **final DB state**, no re-scan fence to confirm disk didn't move again during the (potentially long) reindex/embed/overlay window | ✅ Confirmed | `watch_supervisor.rs:560-645` — single scan→index→snapshot, no second confirmatory scan |
| P0-3a | `refresh_caller_counts` treats everything except literal `'ambiguous'` as confirmed (includes `textual`) | ✅ Confirmed | `graph.rs:381-390` |
| P0-3b | `coreness` confirmed-bucket = `rank() > 0` (includes `inferred`/`textual`) | ✅ Confirmed | `coreness.rs:51-53`; `rank()`: Formal=4/Resolved=3/Inferred=2/Textual=1/Ambiguous=Unresolved=0 (`types.rs:72-84`) |
| P0-3c | `digest`'s recursive-symbol confirmed-bucket = `rank() >= Resolved` (formal/resolved only) | ✅ Confirmed, **and the code itself documents the inconsistency** | `digest.rs:70-73` comment: *"deliberately stricter than coreness's 'confirmed' bucket (which also includes Inferred/Textual)"* |
| P0-3d | `transitive_bfs` expands through everything except `ambiguous` (textual/inferred expand) | ✅ Confirmed, deliberate + ADR'd | `detail.rs:583-584`; `docs/adr/0009-transitive-bfs-ambiguous-not-expanded.md` |
| P0-3e | `path()`'s `certain` flag can be `true` for an all-`textual` route | ✅ Confirmed | `trace.rs:1105-1110` — `certain = exists && any(rank() > 0)`, textual rank=1 |
| P0-3f | Bridge-downgrade eligibility (edit gate) requires `formal`/`resolved` only | ✅ Confirmed | `edit.rs:4084-4086` — `edge_confidence IN ('resolved','formal')` |
| P0-3g | `CapturedLogs::write` caller_count=406, 403 textual, 3 resolved | ❌ **Stale at current HEAD** — `caller_count=8` now **[db]** | Illustrative number from the audit no longer reproduces; likely improved by intervening commits (PR#9/PR#10 identity work) |
| P0-3g′ | (replacement, verified live) the phenomenon still exists today | ✅ Confirmed with fresh evidence **[db]** | `init_db`: caller_count=219, 99 (45%) textual, 120 resolved. `CalmServer::db`: caller_count=156, **100%** `inferred`, 0% resolved/formal |
| P0-4 | `external_crate_root` only recognizes `std`/`core`/`alloc`; other external crates (e.g. `tokio::fs::write`) still fall through to unscoped bare-name resolution | ✅ Confirmed, and **already a tracked open item** | `parser.rs:1565-1571`; `parser.rs:1857-1868` comment documents the exact bug for `std`; `docs/plans/2026-08-19-...:149-172` already scopes npm/Python/Go/JVM analogues as follow-ups to issue #72 |
| P0-5 | `source_range` joins caller-supplied `path` with no canonicalize/containment check | ✅ Confirmed | `inspect.rs:380` — `self.project_root.join(path)`, vs. write path's `resolve_repo_path` → `path_policy::resolve_within_root` (`edit.rs:4198-4202`), which explicitly cites the GhostApproval (Wiz, 2026-07-08) precedent |
| P1-1a | `search`'s default `kind` (`"symbol"`) returns 0 hits for natural-language queries | ✅ Confirmed **[live]** | reran audit's exact query "how does calm decide whether an edit is high risk" with `kind` omitted → `results: []`; `kind="hybrid"` on the same query surfaces the right function top-1 |
| P1-1b | `search_symbol`/`search_text` wrap the whole query as one FTS5 phrase | ✅ Confirmed | `search.rs:191-204` `escape_fts5_query` wraps in `"..."`, shared by both `fts_exact` and `fts_tokens` statements |
| P1-1c | `kind="text"`'s tool-schema description ("FTS over code body") mismatches implementation (docstring+signature only) | ✅ Confirmed, **worse than the audit states** | Tool schema (loaded this session): `"text" (FTS over code body)`; `search.rs:371` own comment: *"Still doesn't cover function bodies, imports, or non-code files"* — a direct schema/implementation contradiction, not just an internal design gap |
| P1-2 | `understand` resolves with `limit=1`, no score floor, no margin check, no alternatives | ✅ Confirmed | `inspect.rs:472-483` |
| P1-3 | `SearchResultItem` drops `qualified_name` even though the core `SearchResult` carries it | ✅ Confirmed | `locate.rs:598-615` |
| P1-4 | Multiple `suggested_next.args` don't validate against their target tool's own param schema | ✅ Confirmed, reproduced **[live]** twice this session | `locate.rs:411,424` sends `{"target":...}` to `source` (needs `symbol`); `locate.rs:414,416,418,420` send `{"kind":...}` to `search` (missing required `query`) — both reproduced by literally calling the tools this session |
| P1-5a | `read_source_capped` collapses oversized/unreadable/non-UTF8/TOCTOU-delete into one `None` | ✅ Confirmed | `discovery.rs:27-33` |
| P1-5b | Targeted reindex silently `continue`s past a capped-read failure, no status row | ✅ Confirmed | `driver.rs:592-598`, comment lists all 4 causes as equivalent |
| P1-5c | `parse_tree` collapses unsupported-grammar / ABI-load-failure / parse-timeout into one `None` | ✅ Confirmed | `parser.rs:197-203` — 3 distinct `?`-shortcut points, same `None` |
| P1-6a | `include_tests` defaults to `true` | ✅ Confirmed | `search` tool schema (loaded this session): `"include_tests": {"default": true}` |
| P1-6b | `search_grep` hardcodes `is_test: false` for every result, so `include_tests=false` can't filter grep hits | ✅ Confirmed | `search.rs:715` |
| P1-7 | `edit_context` declares `read_only_hint=true, idempotent_hint=true` but its handler unconditionally opens a write transaction | ✅ Confirmed, strongly | `guardrails.rs:19-26` annotations vs. `guardrails.rs:418` → `mint_review_authority_for_edit_context` (`guardrails.rs:1242-1360`): `state_conn.transaction()`, `snapshot.persist`, `insert_change_intent`, `ReviewAuthority::mint` (3 more inserts), `tx.commit()` — two calls mint two distinct `authority_id`/`intent_id` pairs, the opposite of idempotent |

**Net: 12/12 findings confirmed at the mechanism level.** One illustrative number (P0-3g)
is stale relative to current HEAD and is replaced above with fresh evidence for the same
underlying architectural gap. No finding was found to be a false positive.

### New finding surfaced during Wave 0 execution (not in the original audit)

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| P0-6 | `edit_context`'s `gate_prediction` cannot predict a `policy.uncovered_code_floor` escalation (defaults to `"high"`), so it systematically under-reports gate severity for any symbol lacking test coverage — the single most common real-world trigger of `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` | ✅ Confirmed, reproduced **[live]** twice | `edit.rs:3880-3896` — `touches_uncovered_code = !proposed_hunks.is_empty() && ...`; `guardrails.rs:338-343`'s own comment: "No proposed edit content exists yet at this pre-edit exploration call... `edit_lines_impl_gated`'s own real gate call supplies real hunks once an edit is proposed." `edit_context(source_range)` reported `gate_prediction: {will_block: false, requires: "none"}` and `edit_context(locate)` reported `requires: "edit_context+confirm+grounded_reason"`; the real `edit_lines` write for both instead returned `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`, in both cases traced to `touches_uncovered_code` only evaluating true once real hunks existed |

**Why this matters:** `edit_context`'s own tool description says "ALWAYS CALL THIS before any code modification — mandatory, never skip," and its `gate_prediction` field exists specifically so an agent can learn *before* attempting a write whether it will be blocked. For any under-tested symbol (arguably where a safety tool's warning is most needed), that prediction is silently wrong in the unsafe direction (under-predicts severity), and the agent only discovers the real gate at write time — after already having committed to a plan around the (wrong) prediction.

**Candidate fix (not yet implemented):** pass a synthetic whole-range placeholder hunk
(`&[(c.line_start, c.line_end, "")]`) instead of `&[]` into `compute_touch_risk` from
`gate_prediction`'s call site — this activates `touches_uncovered_code`'s real check
(which only needs `(start, end)`, not real content) without needing to know the actual
edit yet. Must guard the signature-change check (`edit.rs:3741-3752`) so an empty
placeholder `new_text` is never treated as "the new signature is empty" — either skip
that specific check when hunks are known-synthetic, or gate it on a sentinel. Add to
Wave 0 as **0.6**.

**Second reproduction, broader than first suspected:** the same gate fired on
`crates/calm-server/src/tools/locate.rs::SearchParams` — a plain struct with only doc
comments and field declarations, zero callers, zero executable lines. This means
`touches_uncovered_code`'s `coverage.is_covered(...)` check doesn't distinguish
"genuinely untested logic" from "lines that were structurally never instrumentable in
the first place" (doc comments, struct field lists, type declarations) — coverage
tooling has nothing to report for non-executable lines, so `is_covered` reads as
`false` for them too, tripping the same `"high"` floor. Practical effect: **almost any
edit to a doc comment or struct definition in this codebase currently routes through
the top-tier independent-review gate**, not just genuinely risky untested logic. This
sharpens 0.6's fix: the placeholder-hunk approach should also skip the coverage check
entirely when every touched row's `kind` is a non-executable kind (struct/enum/type/
doc-only), mirroring the existing `kind == "function" | "method"` guard already used
elsewhere in `compute_touch_risk` for the signature/dead-code checks.

### Overlap with existing plans (do not re-litigate, just sequence against them)

- **P0-4** is issue #72, already partially shipped (`docs/plans/2026-08-19-...:149-172`,
  `std`/`core`/`alloc` slice only). This plan's P0-4 task is exactly the deferred
  "npm/Python/Go/JVM analogues" item from that doc — extend the *representation*
  (`QualifiedReference`), not re-discover the bug.
- **P0-2/P0-3**'s `index_input_state`/`index_input_drift`/`EdgeConfidence` machinery was
  independently analyzed in `2026-08-08-derived-artifact-hardening-execution-plan.md`
  for a *different* purpose (auto-bumping the epoch when extraction/derivation logic
  itself changes, not "does the snapshot reflect the live filesystem right now"). The
  two concerns are complementary, not duplicate — that doc's D1 keystone should land
  independently; this plan's P0-2 fix does not require it as a prerequisite.

---

## Part B — Execution plan

Design invariant for this plan (mirrors the sibling evidence-architecture doc's
constitution, specialized to the read/edit path):

> **An index-derived coordinate is a hint until it is verified against live disk in
> the same call that uses it. A hash may prove bytes are unchanged; only a fresh
> identity match may prove which symbol they belong to. A tool's declared annotations
> (read_only, idempotent) must describe what the handler actually does, not what it
> did when the annotation was written. No `suggested_next` may name a tool without
> arguments that tool would accept.**

No wave merges without: `cargo test --workspace` green, and for Wave 1 specifically, a
new adversarial fixture suite (stale/moved/duplicate/concurrent-edit) added to CI, not
just to a local session.

---

### Wave 0 — Stop the cheap, high-confidence leaks (do first, no architecture change) — **SHIPPED 2026-08-20, uncommitted**

Every item here is a small, self-contained diff. None depends on another. Total: ~1-2 days.

> All five items below shipped same session. Every single edit — including the
> pure-annotation change (0.4) and the new test-only additions — was independently
> classified `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` at write time (see P0-6 below for
> why `edit_context`'s own prediction couldn't see this coming). Each was reviewed via
> `mcp__calm__review_decide_via_agent_relay` after the actual diff was shown to and
> approved by a human this session (`.calm/config.json`'s `edit.elicit_via_agent_relay
> = true`) — 8 separate review approvals total for what a naive read of "5 low-risk
> fixes" would suggest needs none. Worth carrying forward: this friction is itself
> informative about how the uncovered-code floor behaves in practice on this repo
> (see P0-6).

**0.1 — Path containment for `source_range`** *(P0-5; ~S)* — **SHIPPED**
- `inspect.rs:380`: replace `self.project_root.join(path)` with
  `calm_core::path_policy::resolve_within_root(&self.project_root, path, SymlinkPolicy::FollowInternalSymlinks)`
  — the exact call `resolve_repo_path` (edit.rs:4198) already makes for the write path.
  Map `PathPolicyError` the same way `resolve_repo_path` does (reuse that function
  directly rather than duplicating the match arms, since `source_range` doesn't
  currently import `edit.rs`'s helper — either make `resolve_repo_path` `pub(crate)`
  at a shared location, or call it from `inspect.rs` if visibility already allows it).
- **DoD:** a `../../etc/passwd`-shaped `path` and a symlink pointing outside
  `project_root` both return `PATH_ESCAPES_PROJECT_ROOT`, not file content. Add
  `source_range_rejects_traversal` / `source_range_rejects_external_symlink` tests
  mirroring the existing `edit_lines`/`edit_symbol` containment tests.

**0.2 — Fix `suggested_next` args that don't validate against their target tool** *(P1-4; ~S)* — **SHIPPED (partial: locate.rs's 6 sites; source_range's `edit_lines` suggestion left args-free by design, per the "drop args when incomplete" convention below)**
- `locate.rs:411,424`: `{"target": results[0].name}` → `{"symbol": results[0].name, "path": results[0].path}`.
- `locate.rs:414,416,418,420`: add the original `query` back into the args —
  `{"query": p.query, "kind": "grep"}` etc.
- `inspect.rs:427` (`source_range`'s suggestion of `edit_lines`): `edit_lines` requires
  `hunks`, which can't be known ahead of a real edit intent — **don't** claim a fully
  executable call here. Either drop `args` entirely (keep `reason` as guidance-only)
  or add a documented convention (e.g. an `args_partial: bool` flag, or simply omit
  `args` whenever it would be incomplete) — pick ONE convention and apply it
  consistently, not per-callsite judgment calls.
- **New invariant test** (the actual fix — spot-patches alone will regress again):
  for every `suggested_next` emission site reachable in the test suite, when `args` is
  present, deserialize it against the named tool's own `Parameters<T>` schema and
  assert success. This can piggyback on the existing tool-schema/JsonSchema
  infrastructure already used for MCP registration — no new schema system needed.
- **DoD:** the new invariant test fails on the current code (proving it would have
  caught both bugs), then passes after the two spot-fixes above.

**0.3 — Fix the `kind="text"` schema/implementation mismatch** *(P1-1c; ~S — stopgap, not the real fix)* — **SHIPPED**
- Change the `search` tool's `kind` parameter doc string from `"text" (FTS over code
  body)` to something accurate, e.g. `"text" (FTS over docstring + signature — NOT
  function bodies or imports; use kind="grep" for that)`.
- This is a documentation-truth fix only. The real fix (index chunk bodies into FTS) is
  Wave 3 (3.2) — do not conflate the two; shipping 0.3 alone still leaves `kind="text"`
  weak, it just stops lying about it.
- **DoD:** `check-doc-truth.sh`-style discipline — tool description matches behavior;
  add a regression test asserting `kind="text"` does NOT match a query string that
  only appears in a function body and NOT in its docstring/signature (locks in the
  current, now-honestly-documented, scope).

**0.4 — Correct `edit_context`'s tool annotations** *(P1-7; ~S)* — **SHIPPED**
- `guardrails.rs:19-26`: `read_only_hint = true` → `false`; `idempotent_hint = true` →
  `false`. `edit_context` mints a new `ReviewAuthority`/`ChangeIntent` row pair on
  every call that clears the freshness bar — that's a real write with real identity,
  not a cacheable/replayable read.
- Audit whether any MCP client behavior (this session's own harness included) makes
  caching/dedup/parallel-safety assumptions based on these hints — if so, flag as a
  follow-up, don't silently change behavior for existing integrations without a note
  in CHANGELOG.md.
- **DoD:** annotation change ships with a CHANGELOG entry; existing
  `edit_context_mints_human_tier_authority_after_a_recorded_reconciliation_and_spend_persists_a_receipt`-style
  tests still pass (behavior unchanged, only metadata corrected).

**0.5 — Defensive clamp against the P0-1e panic** *(pure safety net, ships independently of Wave 1)* — **SHIPPED (all 3 sites: `source()`, `understand()`, `symbols_batch()`)**
- `inspect.rs:247-248` and `inspect.rs:558-559` (both `source()` and `understand()`):
  clamp `start = start.min(end)` (or equivalently `min(lines.len())`) before slicing,
  so a stale `line_start` past current EOF degrades to an empty/short read instead of
  a panic (which today would 500 the whole tool call / could be a cheap DoS vector by
  externally shrinking a watched file between index and read).
- This does **not** fix the wrong-target risk (that's Wave 1) — it only removes the
  crash. Land it now because it's a one-line diff with zero design risk, independent
  of everything else in this plan.
- **DoD:** a proptest/fuzz case over `(indexed_line_start, indexed_line_end,
  live_file_line_count)` triples never panics.

---

### Wave 1 — Live Truth Kernel (P0-1, the core architectural fix) — **SHIPPED 2026-08-20, uncommitted**

**Do not build a new `LiveTargetResolver`/`ReadReceipt` type system from scratch.**
Verification found the actual choke point already exists and is narrower than the
audit assumed: `resolve_symbol` → `resolve_symbol_candidates` (`outcome.rs:506-603`,
9 callers, `is_hub=true`) already centralizes symbol lookup for `source`, `edit_symbol`,
`edit_lines`, `path`, `callers`, `symbol_info`. `CandidateRow` already carries `kind`
and `class_context` — enough to build a real matcher without a new representation.

**Follow-up research pass (2026-08-20, same day, post-Wave-0) resolved all three
"verify during implementation" unknowns from the first draft, and changed the design:**
- `edit_context` (`guardrails.rs:38`) already calls `resolve_symbol` — **not** a third
  independent path. No special-case needed for it.
- `understand()` (`inspect.rs:505-547`) confirmed to bypass `resolve_symbol_candidates`
  entirely: resolves via `search()` then a hand-rolled `query_row` keyed on exact
  `qualified_name`. Real migration work, not a free ride.
- `symbols_batch()` (`inspect.rs:722-759`) also bypasses it: its own batched `IN (...)`
  SQL building `CandidateRow` by hand. Same category of real migration work.
- `hash_content(&source)` (`discovery.rs:62`, FNV-1a) is confirmed to be the *exact*
  function called at index time (`driver.rs:251,389,600`) on the *exact* full-file
  string — directly comparable, zero translation needed for 1.2's fast path.
- `ParsedSymbol.kind.as_str()` (`types.rs:199-214`) produces exactly the same lowercase
  strings `CandidateRow.kind`/`symbols.kind` store — the `(name, kind, class_context)`
  matcher key needs no new representation, just a direct field comparison.
- `edit_symbol_flow`'s `old_text: None` replace path (`edit.rs:436-441`) — the literal
  P0-1b site — takes `c.line_start`/`c.line_end` straight from `resolve_symbol`'s
  `Found` candidate with **zero** intermediate logic. Confirms: fixing `resolve_symbol`
  itself fixes this call site with no additional edit there at all.

**Design decision (user-confirmed 2026-08-20, superseding the original 1.2/1.3 split):**
bake live-verification directly into `resolve_symbol`, not a separate
`verify_or_reresolve_live` callers must remember to invoke. `SymbolResolution` already
has exactly the right shape for this to be a non-breaking internal change for every
*existing* 3-arm `match` (`NotFound`/`Ambiguous`/`Found`) at all 9 current call sites —
`Vanished → NotFound`, `Rebound → Found` (with corrected coordinates), `AmbiguousLive →
Ambiguous` (live-parsed candidates). Only a genuinely new failure mode
(`ReadFailed` — disk read or fresh-parse failed) needs a 4th match arm, added
uniformly at all 9 sites (compiler-enforced-complete, cannot be silently skipped).
Rejected alternative: a separate opt-in function, matching the original plan text —
cheaper for today's DB-only tools (`callers`, `symbol_info`, `path`) since they'd skip
the added disk-read+hash, but leaves future call sites able to silently forget to call
it, reintroducing exactly the bug class this wave exists to close. The added cost is
judged acceptable: one file read + one FNV hash per call (not a re-parse) in the common
case where the hash matches.

**1.1 — Add `file_index.hash` to the resolution query** *(~S, enables everything else)*
- `outcome.rs:511-517` (`resolve_symbol_candidates`'s SQL): add
  `JOIN file_index ON file_index.path = symbols.path`, select `file_index.hash AS
  indexed_file_hash` into `CandidateRow`.
- **DoD:** `CandidateRow.indexed_file_hash` is populated for every resolution; a symbol
  in a file with no `file_index` row (should not happen, but the outer join must not
  panic) degrades to `None` cleanly.

**1.2 — Fold live-verification into `resolve_symbol` itself** *(~M, the actual fix)*
```rust
// crates/calm-server/src/tools/outcome.rs
pub(crate) enum SymbolResolution {
    NotFound,
    Ambiguous(Vec<CandidateRow>),
    Found(Box<CandidateRow>),
    ReadFailed(ErrorDetail),   // NEW — disk read or fresh-parse failed; never
                               // silently falls back to stale coordinates (fixes P0-1g)
}

pub(crate) fn resolve_symbol(
    conn: &rusqlite::Connection,
    project_root: &Path,      // NEW param — needed to read the live file
    name: &str,
    path: Option<&str>,
    line: Option<i64>,
) -> rusqlite::Result<SymbolResolution> { ... }
```
- Existing DB-candidate lookup + optional `line`-narrowing (today's logic) unchanged.
- Once narrowed to exactly one DB candidate (today's `Found` case), before returning:
  read the file once, `hash_content()` it, compare to `c.indexed_file_hash` (from 1.1).
  Equal → return `Found(c)` unchanged, no re-parse — the common case stays cheap.
- Hash mismatch (slow path): `extract_symbols` the live content, match by
  `(name, kind, class_context)` — **not bare name alone**, fixing P0-1f — with
  nearest-`indexed.line_start` as the final tiebreak only among already-equal
  candidates. Share this matcher function with `best_live_range` (1.3) rather than
  reimplementing it there. 0 matches → `NotFound` (was `Vanished`). Exactly 1 →
  `Found(corrected candidates)`. 2+ still tied → `Ambiguous(live-parsed candidates)`
  (was `AmbiguousLive`) — never guessed.
- File read fails, or `extract_symbols` fails on the slow path → `ReadFailed`, **never**
  the current `Err(_) => (c.line_start, c.line_end)` stale fallback. This fixes P0-1g.
  DB-level errors (`conn.prepare`/`query_map` failure) stay in the outer
  `rusqlite::Result::Err` — `ReadFailed` is specifically for the disk-freshness check,
  a different failure axis with a different recovery (reindex vs. real DB corruption).
- **Known, documented residual scope for this first landing:** the DB-`Ambiguous`
  (2+ candidates before any live check) case is *not* live-re-verified per-candidate in
  this pass — only the already-narrowed single `Found` candidate is. A future symbol
  that was ambiguous in the DB and has since had one candidate deleted on disk will
  still report both as ambiguous rather than narrowing to one. Documented as a
  follow-up, not blocking, since it's a strictly smaller-blast-radius gap than P0-1's
  original "confidently wrong single answer" failure mode.
- **DoD:** unit tests for all 4 `SymbolResolution` outcomes stemming from live-checks,
  including a fixture with two same-named methods in different `impl` blocks (the exact
  P0-1f duplicate-name scenario) proving `class_context` disambiguates where bare-name
  matching would have picked wrong.

**1.3 — Migrate call sites** *(~M, mostly compiler-driven; real work only for 2 of them)*
- **9 existing `resolve_symbol` callers** (`edit_symbol_flow`, `edit_context`,
  `symbol_info`, `source`, `pattern_debt_register`, `callers`, `callees`,
  `reference_impact`, `path` ×2): each needs exactly one new match arm for
  `SymbolResolution::ReadFailed` plus threading `project_root` into the call —
  mechanical, compiler-enforced-complete (a missed site fails to compile, not a silent
  gap). `edit_symbol_flow`'s P0-1b site needs **no other change** — it already just
  uses whatever `c.line_start`/`c.line_end` `Found` carries.
- `understand()` (`inspect.rs:455-582`): **real refactor** — replace the `search()` +
  hand-rolled `query_row` (keyed on qualified_name) with a call through
  `resolve_symbol_candidates`/`resolve_symbol` (bare name + path from the search hit),
  so it inherits live-verification for free. Removes the independently-duplicated
  stale-slice logic at `inspect.rs:578-581` (P0-1d) in the same change.
- `symbols_batch()` (`inspect.rs:722-759`): **real refactor** — same shape, batch-load
  through the shared resolution path (or at minimum apply the same hash-check +
  fresh-reparse fallback to its own hand-rolled `CandidateRow` construction) instead of
  reimplementing raw-disk slicing independently. Explicit DoD item, not assumed covered
  by 1.2 alone.
- `insertion_hunk_for`/`best_live_range` (`edit.rs:4229-4314`, `4418-4429`): this path
  already always fresh-parses (good), it just needs `best_live_range`'s matcher
  upgraded to the same `(name, kind, class_context)` key as 1.2 (share the matcher
  function, don't reimplement it) and its `Err(_) => stale fallback` removed (already
  covered by reusing 1.2's fresh-parse error handling instead of its own).
- **DoD (the adversarial suite the audit asked for):** a new test module exercising,
  against a real temp project:
  - *stale*: index a file, mutate it on disk (insert lines above the target symbol)
    without reindexing, call `source` then `edit_symbol` with the returned
    `expected_hash` — assert the edit lands on the *correct* (post-mutation) symbol
    body, never on whatever now occupies the old line range.
  - *moved*: symbol relocated to a different line in the same file — same assertion.
  - *duplicate*: two same-named methods in different `impl`/`class` blocks, one
    stale-indexed — assert the correct one is targeted via `class_context`, and that
    an actually-ambiguous case (same name, same kind, same class_context — should not
    occur in valid code but must not panic) reports `AmbiguousLive`, not a guess.
  - *deleted*: symbol removed from the file since last index — assert `Vanished`,
    never a read of unrelated bytes.
  - *concurrent*: two sequential tool calls (source→edit_symbol) with a disk mutation
    injected between them — assert the second call's own live re-check catches it
    even though the first call already returned (defense in depth vs. the fast-path
    optimization in 1.2 potentially caching staleness across calls if implemented
    wrong).

---

### Wave 2 — Evidence/confidence semantics (P0-2, P0-3) — **SHIPPED 2026-08-20, uncommitted**

**Research pass (2026-08-20, same day, post-Wave-1) resolved the two "verify during
implementation" unknowns for 2.1/2.2 and sharpened 2.3's risk assessment:**
- `file_index` already has a nullable `mtime REAL` column, populated at index time
  (`db/schema.rs:224`, backfilled via `migrate_add_column`) — **no schema migration
  needed** for a live spot-check; there is no `size` column today, so mtime+size would
  require one and mtime-only would not.
- `EvidenceSnapshot::compute` (the non-reconciled path) is **not** edit_context-only —
  confirmed production callers are `plan_change`, `review_change`, and
  `edit_lines_impl_gated` (×2, `edit.rs:1427,2225`) — i.e. it already runs on every
  gated edit, not just the pre-edit exploration call. `source_catalog_digest` already
  scans every `file_index` row (`SELECT path, hash ... ORDER BY path`) on every one of
  those calls; adding a full-content rehash there means reading every indexed file's
  bytes on every edit, a real cost-profile change on this already-hot path, not a
  hypothetical one.
- **Design decision (user-confirmed 2026-08-20):** the live spot-check is
  **mtime-only**, reusing `file_index.mtime` — zero schema migration, one `fs::metadata`
  stat per row (no content read). Documented residual, honestly: a same-mtime,
  different-content edit within the filesystem's mtime granularity window is not
  caught by this signal alone (same class of gap the plan's original mtime+size sketch
  already flagged; size would only narrow, not close, that window). Integration point:
  the live-mtime-vs-`file_index.mtime` comparison feeds `freshness_class` (any
  disagreement forces `Degraded`, never silently stays `Current`), not
  `source_catalog_digest` — keeps this module's existing separation of concerns intact
  (`source_catalog_digest`/`snapshot_id` = content identity; `index_input_drift`/
  `freshness_class` = how much to trust that identity right now); folding it into the
  digest instead would just change `snapshot_id` on a benign `touch` with no way for a
  caller to know *why* trust should drop.
- `is_hub` (the flag that gates the `confirm:true` edit requirement,
  `graph/hub.rs:11-86`) is fed by **both** `caller_count` (`refresh_caller_counts`) and
  `coreness` (`compute_coreness`) — confirmed via direct read of `update_is_hub_flags`.
  Tightening either bucket to `is_verified`-only (Formal/Resolved) would **shrink** the
  hub set, i.e. *loosen* the edit gate for symbols currently protected only by
  textual/inferred fan-out — a real safety-relevant tradeoff (fewer false-hub
  over-gates, but also fewer true-hub protections for weakly-resolved code) that needs
  an empirical false-hub-rate measurement pass, not a mechanical migration. **Scoped
  out of this landing**, tracked as a follow-up (see 2.3 below) — proceeding with just
  the additive, non-behavior-changing part of 2.3 for now.

**2.1 — `EvidenceSnapshot` binds live disk, not just DB catalog** *(~M)* — mtime-only,
see design decision above.
- Add a `live_mtime_drift(conn, project_root) -> rusqlite::Result<bool>` (or equivalent)
  helper in `snapshot.rs`: for every `file_index` row, `fs::metadata(project_root.join
  (path)).modified()` vs. the stored `mtime` (handle `None` on either side — a file
  missing from disk, or a pre-migration row with `mtime IS NULL` — as drift, fail
  closed, same posture `index_input_drift` already takes on `Unknown`).
  Short-circuit on first mismatch — this is a boolean gate, not a digest, so no need to
  scan every remaining row once one drift is found.
- Wire into `build()`/`compute()`: if `live_mtime_drift` is `true`, treat
  `freshness_class` as `Degraded` regardless of what `index_input_drift` reported —
  `compute_after_reconciliation` is unaffected (it unconditionally sets `Reconciled`,
  by design, per its own doc comment).
- Correct the doc comment at `snapshot.rs:173-175` ("any disk change since the recorded
  snapshot changes the id, no TOCTOU window") to state the real guarantee: content
  changes are caught via `snapshot_id` (unaffected, still content-addressed); a live
  disk change **not yet reflected in any DB row** is caught via `freshness_class`
  degrading to `Degraded`, not via `snapshot_id` changing — two different mechanisms
  for two different lag windows, both now covered, but distinctly.
- **DoD:** a test that mutates a file's content *and* mtime after indexing (without
  reindexing) and asserts `EvidenceSnapshot::compute` reports `Degraded`, never
  `Current`; a second test asserting the known residual (content mutated in place with
  mtime pinned to the old value, e.g. `filetime::set_file_mtime`) is *not* caught by
  this signal alone — labeled explicitly as documented residual, not a silent gap.

**2.2 — Reconciliation fence** *(~M, rides on 2.1)*
- `watch_supervisor.rs:596-645`: after the existing full reindex + graph rebuild +
  overlay pass, reuse 2.1's `live_mtime_drift` as a second, cheap catalog scan
  immediately before persisting the `Reconciled` snapshot; if it reports drift, do not
  persist `Reconciled` — persist `Current` (via `EvidenceSnapshot::compute`, not
  `compute_after_reconciliation`) and let the already-running debounce pick up the new
  change on its own.
- **DoD:** a test that injects a disk mutation *during* a simulated long reindex
  (delay hook or a large-enough fixture) and asserts the resulting snapshot is
  `Current`, never `Reconciled`, for that run.

**2.3 — Canonical `EvidencePolicy`: one confirmed/probable/lexical/unresolved mapping** *(~M-L, cross-cutting)* — **scope narrowed 2026-08-20**: land only the additive, non-behavior-changing part below now; the `coreness.rs`/`refresh_caller_counts` bucket question (see research pass above) is deferred pending a false-hub-rate measurement, not attempted in this pass.
- Introduce a single source of truth (a method on `EdgeConfidence` or a small
  `EvidencePolicy` module) that every consumer imports instead of re-deriving its own
  `rank()` cutoff inline:
  ```rust
  impl EdgeConfidence {
      pub fn is_verified(&self) -> bool { matches!(self, Formal | Resolved) }
      pub fn is_probable(&self) -> bool { matches!(self, Inferred) }
      pub fn is_lexical_lead(&self) -> bool { matches!(self, Textual) }
  }
  ```
- Migrate call sites one at a time, **each with its own before/after behavior note**
  (some of today's inconsistencies are deliberate — `transitive_bfs`'s ADR-0009 choice
  to expand through textual/inferred is a considered tradeoff, not obviously a bug；
  changing it is a product decision, not a mechanical refactor):
  - **THIS LANDING** — `graph.rs:381-390` (`refresh_caller_counts`): split into
    `caller_count` (today's definition, unchanged, keep for compat) +
    new `verified_caller_count` (`is_verified` only) — purely additive column/field,
    expose both, let consumers (hub/risk gating) migrate to the stricter one
    deliberately later rather than silently changing `caller_count`'s meaning under
    existing callers today.
  - **DEFERRED** (research pass, 2026-08-20) — `coreness.rs:51-53`: hub-detection's
    `is_hub` (which gates the `confirm:true` edit requirement) is fed by both
    `caller_count` and `coreness` (confirmed via `graph/hub.rs::update_is_hub_flags`);
    tightening this bucket to `is_verified`-only would shrink the hub set and *loosen*
    the edit gate for symbols currently protected only by textual/inferred fan-out —
    a safety-relevant behavior change in the permissive direction, not a pure refactor.
    Needs its own before/after false-hub-rate measurement pass against a real corpus
    before any change lands here. Left on today's broader bucket (`rank() > 0`) for
    now — an explicitly permitted exception to this item's own DoD grep-check below,
    not silently missed.
  - **THIS LANDING** — `trace.rs:1105-1110` (`path`'s `certain`): tighten to
    `is_verified`-only, matching what `digest.rs` already considers the stricter bar —
    this one is a clear correctness fix (an all-textual "certain" path is misleading by
    the tool's own advertised contract: "terminated_by=null + exists=true/false →
    certain result"), not a gate-loosening concern like `coreness.rs` above (nothing
    downstream of `certain` currently drives an edit-permissiveness decision).
  - **THIS LANDING** — `edit.rs:4084-4086` (bridge-gate): already `is_verified`-
    equivalent, no behavior change — just point it at the shared helper for
    future-proofing.
- **DoD:** every direct `EdgeConfidence::rank()` comparison outside `types.rs` itself,
  the deliberately-independent `transitive_bfs`/`digest.rs` cases, and the deferred
  `coreness.rs`/`compute_coreness` bucket (documented exceptions, all three) is
  replaced by a call to the shared predicate; a grep-based CI check (`check-*.sh`
  style, matching this repo's existing convention) fails if a new `rank() >`/
  `rank() >=` comparison appears outside the allowed files.

---

### Wave 3 — Assistant-grade retrieval (P1-1, P1-2, P1-3) — **SHIPPED 2026-08-21, uncommitted**

**Research pass (2026-08-21, post-Wave-2) resolved all four items' "verify during
implementation" unknowns and corrected one real design trap in the original draft:**
- `search_hybrid` (`search.rs:1025-1091`) already gracefully degrades to exactly
  today's `search_symbol` output (wrapped with `degraded: true` + an explanatory
  `note`) whenever no embedder is configured or the query embeds to an empty vector —
  confirmed by direct read, not assumed. Flipping `search`'s default `kind` therefore
  cannot regress a no-embeddings project to worse-than-today; the only real cost is
  extra per-call embedding work on projects that DO have embeddings enabled, not a new
  failure mode. This closes 3.1's original "changes cost profile" hedge into a
  quantified, one-sided risk (latency only, never breakage), which is why the
  simpler "flip the literal default" option (no new `"auto"` classifier) was chosen —
  see design decision below.
- `search`'s default `kind` has exactly ONE control point: `SearchParams::kind`'s
  `#[serde(default = "default_symbol")]` (`locate.rs:508,543`) — `SearchParams` IS the
  `search` tool's own parameter struct (`locate.rs:18`), not a separate/compound type,
  so there is no second default to keep in sync for that tool. (`understand`'s own,
  independent `unwrap_or("symbol")` fallback in `inspect.rs` is a different struct and
  out of scope for 3.1 — noted as a small, separate follow-up, not blocking.)
- `code_chunks` (`chunk_text`, already populated for semantic/similar search) is
  cleared via `DELETE FROM code_chunks WHERE path = ?1` (per-file reindex,
  `driver.rs::remove_file_rows`) or `DELETE FROM code_chunks` (full reindex,
  `driver.rs::reindex_all_cancellable_with_phase`), then re-inserted
  (`edges.rs::insert_code_chunks_batch`) — a DELETE-then-INSERT pattern, never an
  in-place `UPDATE`. This is the *exact* shape `fts_exact`/`fts_tokens` already handle
  today (`symbols_ai`/`symbols_ad` triggers, no `symbols_au`) — a new `content='
  code_chunks'` FTS5 table only needs the same two trigger kinds, not a third. Real
  technical risk for 3.2 is low; it is direct, precedented mechanical work.
- `resolve_symbol_candidates` (`outcome.rs:506-561`) filters only by `name` (+
  optional `path`) — there is no `qualified_name` parameter today. **The original
  draft's "short-circuiting bare-name re-resolution when present" would, if
  implemented literally as bypassing `resolve_symbol` entirely, defeat Wave 1's live
  truth kernel** — skipping straight to a DB row by `qualified_name` would skip
  `verify_live` too, reintroducing the exact P0-1 staleness risk Wave 1 exists to
  close. **Corrected design (this is the actual 3.4 plan now, superseding the literal
  original wording):** thread `qualified_name` into `resolve_symbol`/
  `resolve_symbol_candidates` as an additional optional narrowing filter (`WHERE
  qualified_name = ?` when present, replacing the `name`/`path` filter rather than
  supplementing it, since a qualified_name is already unique) — the result still flows
  through `resolve_symbol`'s existing `verify_live` step unchanged. This still meets
  the DoD (a `qualified_name`-driven lookup can never land on `Ambiguous`, since the
  query is unique by construction) without bypassing live-verification. `SearchResultItem`
  (`locate.rs:600-617`) confirmed to have NO `qualified_name` field today (matches
  P1-3's claim exactly); the core `SearchResult`/`RawRow` type already carries it
  (`SELECT s.qualified_name ...` in `search_text` and siblings), so 3.4(a) (restoring
  it to the output) is a pure plumbing addition, not a new data source.

**Design decisions (user-confirmed 2026-08-21):**
- **3.1:** flip the literal default (`default_symbol()` → a `default_hybrid()`
  equivalent, or simply change what `default_symbol()` returns/is named) rather than
  building a separate `"auto"` query classifier — simpler, and the graceful-degrade
  finding above means there is no correctness downside to flipping outright.
- **3.2:** the new chunk-body FTS extends `kind="text"`'s existing scope (merged with
  `fts_exact` results, e.g. via the same `rrf_merge_n`/union-then-rank shape already
  used elsewhere in this module) rather than shipping under a new kind name — matches
  this doc's own framing of 3.2 as "the real fix behind 0.3's stopgap," not a
  parallel feature. `kind="text"`'s tool-schema description (already corrected once,
  Wave 0 item 0.3) needs a second pass once this lands, to honestly claim body
  coverage instead of disclaiming it.

**3.1 — `kind="hybrid"` as the real default** *(~S)* — **SHIPPED**
- Change `SearchParams::kind`'s default to `"hybrid"`.
- **DoD:** the exact query reproduced live earlier this session
  ("how does calm decide whether an edit is high risk") returns a relevant top-3
  result with `kind` omitted.

**3.2 — Real code-body full-text search, folded into `kind="text"`** *(~M)* — **SHIPPED**
- New `fts_chunks` FTS5 virtual table (`content='code_chunks', content_rowid='id'`,
  `chunk_text` column, `tokenize='unicode61'` matching `fts_exact`/`fts_tokens`), plus
  `code_chunks_ai`/`code_chunks_ad` triggers mirroring `symbols_ai`/`symbols_ad`
  exactly (insert/delete only — confirmed no in-place update path exists).
- `search_text` merges `fts_exact` (name/docstring/signature) and the new
  `fts_chunks` (body) hits into one result set instead of only ever querying
  `fts_exact`.
- Update `kind="text"`'s tool-schema description once this lands (drop the "does NOT
  search function bodies" disclaimer Wave 0's 0.3 added).
- **DoD:** a query string that appears only inside a function body (not
  docstring/signature/name) is found by `kind="text"`.

**3.3 — `understand` ambiguity surfacing** *(~S, rides on Wave 1)* — **SHIPPED**
- `understand`'s internal `calm_core::search::search(..., limit=1, ...)` call becomes
  `limit=2` so a top-1/top-2 score margin is available; add a `resolution_confidence`
  field (`"confident"` / `"ambiguous"`) to `UnderstandOutput` plus an `alternatives`
  list surfaced only when margin is low, instead of silently committing to top-1.
  Margin threshold is a judgment call (no existing precedent in this codebase to
  anchor it to) — implement with a named, documented constant so it's a one-line
  tuning knob, not a magic number buried in a conditional.
- **DoD:** a query matching two equally-scored symbols returns
  `resolution_confidence: "ambiguous"` + both as `alternatives`, not a confident
  single answer.

**3.4 — `qualified_name` identity-chaining (corrected design, see research pass above)** *(~M, larger than the original "~S" estimate once the live-verification interaction is accounted for)* — **SHIPPED**
- `locate.rs:600-617`: add `qualified_name: Option<String>` to `SearchResultItem`
  (skip-serializing when absent, e.g. `kind="file"` hits) — already present in the
  underlying `SearchResult`, pure plumbing.
- `outcome.rs`: add an optional `qualified_name` narrowing parameter to
  `resolve_symbol`/`resolve_symbol_candidates`, used as the sole `WHERE` filter when
  present (still flows through `verify_live` — this is the load-bearing correction
  from the research pass, not optional).
- Thread a new optional `qualified_name` param through the same 9 call sites Wave 1's
  1.3 already migrated (`source`, `symbol_info`, `callers`, `callees`,
  `reference_impact`, `path` ×2, `edit_symbol_flow`, `pattern_debt_register`) — each
  needs one new optional field on its own `Params` struct and one extra argument
  threaded into its existing `resolve_symbol` call, compiler-enforced-complete the
  same way Wave 1's `ReadFailed` arm was.
- **DoD:** a `search` → `source` round trip using `qualified_name` never hits
  `Ambiguous`, even for a globally-common bare name like `new` or `write`; a
  `qualified_name` for a symbol that has since been deleted/renamed on disk still
  goes through live-verification (`NotFound`/`ReadFailed`, never a stale read) — this
  second assertion is the actual regression test for the corrected design, proving it
  isn't just 3.4's old, unsafe literal reading with different plumbing.

---

### Wave 4 — Coverage/noise honesty (P1-5, P1-6) — **SHIPPED 2026-08-21, uncommitted**

**Research pass (2026-08-21, post-Wave-3) resolved all three "verify during
implementation" unknowns for 4.1 and surfaced one genuine correctness bug not in the
original audit:**
- `read_source_capped`'s 3 callers (`reindex_all_cancellable_with_phase`,
  `reindex_changed_cancellable`, `reindex_paths`) do NOT behave identically on a
  `None` the way the original plan text implied. Traced each:
  - `reindex_paths` (edit-tool/safe-watcher targeted path): the existing `continue`
    at `driver.rs:592-598` leaves the stale `file_index` row untouched — safe,
    matches the plan's framing exactly.
  - `reindex_all_cancellable_with_phase` (full reindex): the `?`-in-closure +
    `.flatten()` pattern (`driver.rs:249`) means the file gets **no** `file_index`
    row at all after the reindex, not merely "no reason recorded" — harmless (full
    rebuild has no prior state to corrupt) but a sharper form of the same gap.
  - **`reindex_changed_cancellable` (`driver.rs:387`) — the everyday incremental
    reindex path, reachable from `WatchSupervisor::run`/`run_armed_session`,
    `bootstrap`, `serve_unix_daemon`, and `calm-cli::main`, i.e. the primary
    reindex loop during normal operation, not an edge case — has a genuine
    correctness bug, not just a missing-reason gap.** Its `candidates` list is
    built by the same `?`-in-closure/`.flatten()` pattern, so a file that is
    walked (still exists) but transiently fails to read (permission hiccup,
    momentarily over `MAX_INDEXABLE_FILE_BYTES`, AV lock, a non-UTF8 write
    mid-flight) is silently absent from `candidates`, hence absent from
    `seen_paths`. The subsequent `for path in existing.keys() { if
    !seen_paths.contains(path) { ...delete... } }` loop then treats it exactly
    like a genuinely deleted file — **its indexed symbols and call edges are
    actually removed**, even though the file still exists on disk and will read
    fine on the very next pass. Confirmed via direct trace of `driver.rs:379-407`
    and `458-466`; not previously documented anywhere in this repo.
- `parse_tree` (`parser.rs:197-203`) has **9 production call sites**, not the
  narrow set the plan's original "~M" estimate implicitly assumed: 2 in `edit.rs`
  (`validate_syntax`/`validate_syntax_diff` — the same syntax-validation gate
  CALM's own edit-time compile-check leans on), plus `csharp_namespace.rs`,
  `imports.rs`, and 4 more inside `parser.rs` itself (`extract_symbols`,
  `extract_calls`, `extract_file_aliases`, `extract_type_map`), plus
  `pipeline/extraction.rs::extract_file_data`. Each consumes the `Option<Tree>`
  differently (`?`, `.map()`, `let Some() else`), so a signature change is a
  real multi-site migration, not a narrow one. (The 3 other callers inside
  `semantic_facts.rs` that `.expect("parse")` on it are confirmed
  `#[cfg(test)]`-only test helpers — safe, not a production concern either way.)
- `file_index` (`schema.rs:225-232`) has no status/reason column today
  (`path, hash, language, symbol_count, last_indexed, mtime` only) — the plan's
  "new or existing column" placeholder resolves to: genuinely new, via the same
  low-risk `migrate_add_column` pattern already used for `mtime`/`symbols.is_test`.
- `symbols.is_test` (`schema.rs:187`) already exists and is populated once at index
  time by `detect_is_test` (`parser.rs:1351-1393`, per-language decorator/annotation/
  path heuristics). `search_grep`'s `enclosing_symbol()` helper (`search.rs:547-561`)
  currently selects only `name, qualified_name, kind` from that same `symbols` row —
  it does not select `is_test`, which is already sitting right there. This makes
  4.2 simpler than the plan's "reuse whatever heuristic" wording suggested: no new
  heuristic needed for the common case (a grep match inside a known symbol), just
  select the column that's already computed and stored. Only a match with no
  enclosing symbol (blank line, comment, non-code file) needs any fallback at all.

**Design decisions (user-confirmed 2026-08-21):**
- The `reindex_changed_cancellable` deletion-on-transient-unreadable bug is real
  and independent of the observability gap the plan originally described — it gets
  fixed **first, as its own isolated step (4.1a)**, before the enum/reason-tracking
  work, not folded into the same landing and not merely flagged for later.
- `parse_tree`'s reason-tracking is **descoped from this wave** given its 9-site
  production blast radius — 4.1(b) below is scoped to `read_source_capped`'s 3
  callers only. `parse_tree`'s `UnsupportedLanguage | AbiLoadFailed | Timeout`
  distinction is deferred as an explicit, separate follow-up, not silently dropped.

**4.1a — Fix `reindex_changed_cancellable` treating a transiently-unreadable file as
deleted** *(~S, correctness fix, land before 4.1b)* — **SHIPPED**
- `driver.rs:379-407`: a file dropped by the `read_source_capped`-returns-`None`
  branch inside the `.par_iter().map(...)` closure must not be indistinguishable
  from a file that was never walked. Either keep it in `seen_paths` by tracking
  walked-but-unreadable paths separately from `candidates` (so the
  `!seen_paths.contains(path)` deletion loop at `driver.rs:458-466` never fires for
  it), or restructure the closure to return a 3-way outcome (`Candidate |
  UnrecognizedExtension | ReadFailed`) instead of collapsing the latter two into one
  `None`.
- **DoD:** a test that walks a file, indexes it successfully, then makes it
  transiently unreadable (e.g. write non-UTF-8 bytes, or a 0-byte-permission file
  where the test harness allows it) without deleting it, runs
  `reindex_changed_cancellable` again, and asserts the file's symbols are **still
  present** in `symbols`/`call_sites` — not deleted — even though the read failed.

**4.1b — Distinguish `read_source_capped`'s skip reasons** *(~M, scoped to
`read_source_capped` only — `parse_tree` deferred, see design decision above)* — **SHIPPED**
- `discovery.rs:27-33`: change `read_source_capped`'s return type from `Option<String>`
  to a small enum (`Ok(String) | TooLarge{len} | Unreadable{io_error_kind}`) so
  callers can log/record *why*, not just *that* a file was skipped. (A dedicated
  `NotUtf8` variant may or may not be distinguishable from other `Unreadable` cases
  via `io::ErrorKind` alone — confirm `read_to_string`'s actual error kind for
  invalid UTF-8 during implementation; fold into `Unreadable` if not cleanly
  separable rather than inventing an approximate signal.)
- `driver.rs:592-598` (`reindex_paths`) and the two `.par_iter().map()` closures in
  `reindex_all_cancellable_with_phase`/`reindex_changed_cancellable` (already
  restructured by 4.1a for the deletion bug): route the reason into a new
  `file_index` column (e.g. `skip_reason TEXT`, nullable, `migrate_add_column`) — a
  successfully-indexed file has `skip_reason IS NULL`; a skipped file gets a row
  with `skip_reason` set (and stale `symbol_count`/`hash` left as whatever they were
  before, since nothing was re-extracted).
- **DoD:** `fitness_report` (or a new dedicated field) can enumerate skipped files
  with their specific `skip_reason`, not just a count.

**4.2 — `include_tests` actually filters grep** *(~S)* — **SHIPPED**
- `search.rs:547-561` (`enclosing_symbol`): add `is_test` to the `SELECT` and the
  returned tuple — already computed and stored on the `symbols` row, no new
  heuristic needed for this case.
- `search.rs:715` (`search_grep`'s result-building loop): use the enclosing symbol's
  `is_test` when present; when there is no enclosing symbol, fall back to `false`
  (today's behavior) or a cheap path-based heuristic — a design/judgment call to
  make during implementation, not investigated further in this research pass since
  it's a small, low-risk decision either way.
- **DoD:** `search(kind="grep", include_tests=false)` excludes matches inside a
  `#[cfg(test)] mod tests { ... }` block.
- (Broader `production | direct_test | test_support | fixture | generated | docs |
  benchmark` classification from the original audit is a bigger taxonomy change —
  scope as a follow-up issue, not part of this wave, unless 4.2's minimal fix proves
  insufficient in practice.)

---

## Sequencing summary

```
Wave 0 (parallel, ~1-2 days) ──┐
                                 ├─▶ Wave 1 (Live Truth Kernel, the load-bearing fix)
                                 │        │
                                 │        ▼
                                 │   Wave 2 (Evidence/confidence semantics)
                                 │        │
                                 └────────┼─▶ Wave 3 (Assistant retrieval) ─┐
                                          │                                  ├─▶ Strict-mode posture
                                          └─▶ Wave 4 (Coverage honesty) ────┘    (out of scope here)
```

Wave 0 has no dependencies and should land immediately. Wave 1 is the prerequisite for
anything claiming CALM's edit path is safe against stale/concurrent index state — it
is the one wave that should not be parallelized away or descoped, per the original
audit's own framing (which this verification agrees with): everything else is quality-
of-life or defense-in-depth on top of a truth kernel that, until Wave 1 ships, can be
made to silently write to the wrong code under a specific, now-demonstrated (adversarial
test suite in 1.3) sequence of stale-index + edit calls.
