# B13 — CALM vs CodeGraph, Multi-Repo Real A/B (Phase 1 + 2)

Extends [B11](../b11_extended_competitor_ab/README.md) (real CALM-vs-CodeGraph
A/B with real oracles, but **self-repo/Rust-only**, and pinned CodeGraph at a
now-stale v1.2.0) and [B12](../b12_tier1_tier2_tool_correctness/README.md)
(6 pinned external OSS repos with an independent regex/git-grep oracle, but
**CALM-only** — no competitor was ever run against that corpus). This
benchmark adds a real CodeGraph arm to B12's corpus/oracle and one task
neither B10/B11/B12 measured: **freshness under a live, external edit**.

Design rationale, full pitfall catalogue, and why this composes existing
infra instead of inventing a new corpus:
[`docs/plans/2026-08-02-calm-vs-codegraph-fair-benchmark-research.md`](../../docs/plans/2026-08-02-calm-vs-codegraph-fair-benchmark-research.md).

## Corrected numbers (2026-08-03) — read this section first

The Phase 1+2 numbers below (80.6% CALM / 72.2% CodeGraph) were **published with two
bugs in this benchmark's own methodology**, found during a follow-up investigation
after the user asked why the margin looked thin. Both are fixed now; the sections
further down are kept for provenance/audit trail, not deleted, but they are
**superseded** by this one.

1. **Ground-truth oracle bug** (`benchmarks/b12_tier1_tier2_tool_correctness/
   ground_truth.py::git_grep_call_sites`, shared with B12/B7): `git grep` scanned
   *every* tracked file, not just the corpus's own source-file extension, and never
   excluded comment lines — so a markdown/`.rst` doc that merely *mentions* `name(` in
   prose, or a source-file comment like `// ... common::resolve_preset(...)`, counted
   as a real "caller." Verified directly (not assumed) on 4 already-published oracle
   entries: `docs/patterns/celery.rst`, `docs/logging.rst`, 3 non-code mentions inside a
   markdown design doc, and one Rust re-export comment. Neither CALM nor CodeGraph ever
   found any of these (correctly — none are real call sites), so both tools were being
   docked for "missing" something that was never really there. **Fixed**: the oracle
   now restricts `git grep` to the corpus's own language extension and skips
   comment-only lines.
2. **SCIP-overlay race condition** (`run_benchmark.py`): `wait_calm_indexed()` returns
   as soon as tree-sitter indexing reaches `phase=="ready"`, before the automatic
   *async* SCIP overlay pass (rust-analyzer/scip-python, upgrading edges to `formal`
   confidence) has run. Verified live against a fresh fd clone: `indexing_status.
   scip_overlays[rust].up_to_date` stayed `false` 15s past ready with no auto-trigger.
   **Fixed**: the harness now calls the `scip_refresh` MCP tool explicitly right after
   indexing, before any recall query. (Empirically, on the 3 symbols spot-checked before
   the fix, forcing the overlay only upgraded `edge_confidence` and ruled out a few
   phantom fan-out edges — it did not change which files came back. Still fixed for
   rigor: a skeptical reader shouldn't have to take "it happened not to matter" on
   faith.)

Because the oracle's eligibility filtering changed, the corrected re-run samples a
**different** (still deterministic: sorted + fixed stride) set of 24 symbols than
Phase 1+2 — this is not the same sample with cleaner labels, it's a fresh, methodologically
superior run. Same pins, same N=3 repeats, same `n=8` symbols/corpus.

### Combined (corrected, post-`Self::` fix) — fd + flask + self-repo, N=24 symbols, 31 real oracle files

| | CALM | CodeGraph |
|---|---|---|
| File-recall | **31/31 (100%)** | 27/31 (87.1%) |

| Corpus | CALM | CodeGraph |
|---|---|---|
| fd (Rust) | **9/9 (100%)** | 9/9 (100%) |
| flask (Python) | **12/12 (100%)** | 9/12 (75.0%) |
| self-repo (Rust) | **10/10 (100%)** | 9/10 (90.0%) |

### A real CALM parser bug, found, root-caused, and fixed in this same pass

The first corrected re-run (before this fix) was **30/31 (96.8%)**, with exactly one row
where CodeGraph won: fd's `replace_separator` (`src/fmt/mod.rs`) — CodeGraph found it
(1/1), CALM found **nothing** (0/1, not even an ambiguous edge). Checked directly against
fd's source at the pinned commit (`raw.githubusercontent.com`, no new clone needed): the
function is called 5 times in the same file, every time as `Self::replace_separator(...)`
— an associated-function call through Rust's `Self` shorthand, not `TypeName::method(...)`.

**Reproduced independently on CALM's own codebase**, not just fd — `ConservativeResolver`'s
`Default::default()` (`crates/calm-core/src/resolver/conservative.rs:123`) calls
`Self::new()`, and `callers` on `ConservativeResolver::new` used to return 13 real callers
but **not** `default`, even though every other `ConservativeResolver::new()` (fully
qualified) call site in the same file *was* found.

**Root cause** (chased through 4 functions in `crates/calm-core/src/indexer/`, all read
directly, not guessed): `is_type_like()` (`parser.rs`) flags any receiver starting with an
uppercase letter as a type path — true for `"Self"`. `split_receiver_callee`'s `::` branch
then sets `receiver_is_type_path=true`. `extract_file_data` (`pipeline.rs`) used this to
set `target_class = Some("Self")` — the **literal keyword text**, never substituted for the
enclosing `impl` block's real type name, even though that name was already correctly
tracked (`enclosing_class`, via Rust's `class_name_field: "type"` on `impl_item`).
`resolve_sites_to_edges` then looks up `by_name_class[(callee, "Self")]`, which can never
match — no symbol is ever actually named `"Self"` — so the call site silently resolved to
zero edges. (`resolve_tier2` already does the equivalent substitution for lowercase
`self`/`this`, but this branch short-circuits before ever reaching it.) **Real-world
magnitude on CALM's own repo**: 42 real `Self::method()` call sites across 15 files — a
common, encouraged Rust idiom (safe under type renames), not a rare edge case.

**Fixed**: `extract_file_data` now substitutes `enclosing_class` for a Rust `"Self"`
receiver before setting `target_class`, mirroring `resolve_tier2`'s existing `self`/`this`
handling. New regression test (`rust_self_colon_colon_call_resolves_to_the_enclosing_impl_
type`, full-pipeline style matching the existing `test_formal_tier_upgrades_textual_
python_call`) passes; broader suite (`cargo test -p calm-core --lib -- indexer::pipeline
indexer::parser resolver:: rust`) — **255 passed, 0 failed**, no regressions. Re-running
this exact benchmark after the fix moved `replace_separator` from 0/1 to 1/1 and the
combined total from 30/31 to **31/31 — CALM has zero misses left in this sample**.

**Swept for similar bugs, not just this one — and fixed the one it found.** First pass
flagged (but didn't fix) a related gap: `walk_calls`'s `child_class` tracking used the same
`class_name_field` unconditionally for every `class_node_types` entry, but Rust's
`trait_item` has no `"type"` field (only `"name"`) — `walk_symbols` already special-cased
this for *symbol* extraction but the equivalent fix was never applied to *call* extraction.
Initial call: skip it, reasoning `Self` inside a trait default method is inherently
"unbound" to one concrete type so a fix would just be a guess. **That reasoning was wrong**
— caught on direct follow-up. A live characterization test (`trait Greeter { fn helper()
{..} fn greet() { Self::helper() } }`) showed the actual mechanism was identical to the
`impl_item` bug (`target_class` left as literal `"Self"`, zero edges), not a "which
concrete type" ambiguity at all — `enclosing_class` was simply `None` for anything inside
a `trait_item`, same broken shape, different root cause. Fixed the same way: `walk_calls`
now reads `trait_item`'s `"name"` field, mirroring `walk_symbols`'s existing special-case.
`Self::helper()` inside a trait default method now correctly resolves to that trait's own
declared `helper` — the same defensible "point at the declaration" choice this call-graph
already makes elsewhere when a concrete type can't be narrowed further. New regression test
(`rust_self_colon_colon_call_inside_a_trait_default_method_resolves_to_the_trait`) passes.
Full `cargo test -p calm-core --lib -- indexer:: resolver:: rust`: **356 passed, 0 failed**.

Also unconfirmed (found by reading code, not live-tested): Swift's `Self.method()`
(dot-based, not `::`) may hit a parallel gap via `resolve_tier2`'s case-sensitive match
arm. Ruled out: PHP's `self::`/`static::`/`parent::` are conventionally lowercase so
`is_type_like` never flags them (the reason PHP doesn't share this bug despite using `::`
the same way); no other supported language pairs a capitalized self-referencing keyword
with `::`-call syntax.

## Scope run so far

**Phase 1**: fd (external, Rust) + CALM self-repo (isolated worktree), N=1.
**Phase 2** (same day, immediately after): added **flask** (external,
Python) + repeated every query **N=3 times** per symbol per tool to check
determinism (B11's exact repeat rationale — catch transient MCP/process
hiccups, not resample different symbols). fmt/C++ remains descoped — still
needs a `scip-clang` oracle setup heavier than this pass's time budget.

Both phases hit real, disclosed constraints, not silent scope-narrowing:
this machine ran at **97-98% disk (5.3-7.9GB free)** throughout — each
corpus needs a throwaway clone plus a `.codegraph/` index (215MB observed on
CALM's own repo) — mitigated by cleaning up (`.codegraph`/`.calm` deleted,
worktree/clone removed) immediately after every corpus's pass.

## A real CALM bug found (and fixed) while building this benchmark

Indexing the **flask** corpus with a freshly-built CALM permanently failed:
`indexing_phase: "failed"`, `indexing_error: "UNIQUE constraint failed:
call_sites.from_path, call_sites.enclosing_qn, call_sites.callee_start_byte,
call_sites.callee_end_byte, call_sites.edge_kind, call_sites.identity_version"`
— 0/93 files indexed, every time, reproduced 3 times independently before
concluding it wasn't a fluke.

Root cause (`crates/calm-core/src/indexer/pipeline.rs::persist_file`): the
tree-sitter Python extractor emitted two `CallSiteData` entries whose full
identity tuple was byte-for-byte identical. `idx_call_sites_current_identity`
(added later, in `schema.rs::migrate_call_site_identity_v2`) correctly
rejects that as a duplicate — but the original `INSERT INTO call_sites` had
no `OR IGNORE`, so the UNIQUE-constraint error aborted the **whole
transaction**, failing indexing for every file in the batch, not just the
one duplicate row. Every sibling table with an identity constraint
(`call_edges`, `import_edges` — `indexer/edges.rs`) already used
`INSERT OR IGNORE` for exactly this reason; this was the one insert that
predated the constraint and was never updated to match.

**Fixed**: `INSERT OR IGNORE INTO call_sites`, skip-count logged via
`tracing::debug!` (fail-soft, not silently swallowed) — commit-ready in
`crates/calm-core/src/indexer/pipeline.rs`, plus a new regression test
(`persist_file_ignores_a_call_site_that_collides_on_the_full_identity_tuple`)
that constructs the exact duplicate-identity scenario directly against
`persist_file` and asserts it dedupes instead of erroring. Verified live:
flask now indexes cleanly (93/93 files, 1627 symbols, 5655 edges, ~21s cold).
**Not yet found**: the exact upstream Python tree-sitter condition that
produces the duplicate emission in the first place (the persistence-layer
fix is correct and sufficient regardless, but the extractor-side "why" is
still open for a follow-up look — flagged, not hidden).

This is worth stating plainly for anyone using these numbers: **CodeGraph
was run against a version of CALM that could not even complete an index of
this exact corpus** until this fix landed. The recall numbers below are
therefore only meaningful post-fix — reported that way throughout.

## Version pinning — the single most-violated discipline in this suite's own history

| Tool | Pin | How verified |
|---|---|---|
| CALM (Phase 1+2, superseded) | git SHA `aba60aa86b7215cdce755d12835083329f4c7172` **plus** the `persist_file` fix above, layered on top the same session (not yet a separate commit at benchmark time) | `git rev-parse HEAD`, recorded independently of `calm --version` — that only ever prints the Cargo.toml package version ("0.4.0"), identical whether you're on the tag or N commits past it. Binary: `target/debug/calm`, rebuilt fresh immediately before each run. |
| CALM (corrected re-run, canonical) | git SHA `52d1abe682069e018d00d09f2d9ab07b820a557c` (the `b13`-commit itself) plus the oracle + `scip_refresh`-race fixes, uncommitted at benchmark time | Same method: `git rev-parse HEAD`, `target/debug/calm` rebuilt fresh after `cargo clean` (disk pressure forced a full rebuild) immediately before this run. |
| CodeGraph | `@colbymchenry/codegraph@1.5.0`, pinned **explicitly** in every spawn command | `npm view @colbymchenry/codegraph version` → `1.5.0` (published 2026-07-21). **New pitfall found live during this benchmark's own setup**: a bare `npx -y @colbymchenry/codegraph --version` (no version pin) returned **1.4.1** from local npx cache — silently stale by one minor version despite `-y`. Always pin the exact version string, never trust a bare `npx -y <pkg>` to mean "latest." Added to the design doc's pitfall catalogue. |

## Methodology

- **Oracle**: reuses B12's `ground_truth.py` verbatim — regex definition
  extraction + word-bounded `git grep` call-site extraction, independent of
  either tool's own parser (same principle as B11's grep-based oracle).
- **Sampling**: `unique_definitions` (excludes names redefined in multiple
  scopes / dunder methods) + an occurrence-count filter (`total_occurrences`
  between 1 and 25) before sampling 8 symbols per corpus, deterministically
  (sorted + fixed stride) — same filters B12's own dry-run history required
  to avoid sampling degenerate names like `index`/`add`/`__init__`.
- **Task A — callers recall**: for each sampled symbol, call CALM's
  `callers` and CodeGraph's `codegraph_callers` **3 times each** (same live
  server, no re-index between repeats), extract the file set each returns,
  and compute recall against the oracle's file set. Every single repeat
  agreed with itself across all 24 symbol/corpus cells and both tools — full
  determinism, matching B11's own finding ("every repeat call was
  identical... robustness against transient hiccups, not variance in the
  underlying tools").
- **Task B — freshness under a live external edit** (new, not in any prior
  benchmark in this suite): append a brand-new call site to one sampled
  symbol via a **plain file write**, bypassing both tools' own edit
  mechanisms entirely. Query both immediately (t=0s) and after a fixed 3s
  grace window. Directly tests CodeGraph's "auto syncs on code changes"
  marketing claim against CALM's documented (`docs/architecture.md`)
  hash-diff-triggered incremental watcher.
- Every tool runs against a **throwaway clone** (fd, flask) or **isolated
  `git worktree --detach`** (self-repo) — never the shared pinned corpus or
  the live self-repo daemon/index — cleaned up immediately after each pass.
- A found-bug worth stating plainly (self-audit, per this suite's own
  "reproduce before reporting" norm): the first run of this harness scored
  CALM **0/N on every single sample**. Before writing that down as a
  finding, the raw JSON was inspected directly — CALM's real `callers`
  response has no separate `path` field; the file lives inside a qualified
  `symbol` string (`path/to/file.rs::Type::method`), which an earlier draft
  of `extract_paths_from_calm_callers` didn't know to split on. Fixed and
  re-run before any number below was recorded.

## Results — Phase 1+2 (2026-08-02, SUPERSEDED by the corrected numbers above)

Kept below for provenance/audit trail only — see "Corrected numbers" at the top of this
file for the canonical, oracle-fixed results. These tables reflect the 2 methodology bugs
described above (doc/comment oracle noise + the SCIP-overlay race condition) and should
not be cited going forward.

## Results — fd (sharkdp/fd, Rust, external, commit `41532d1`)

calm index: ~17s cold. codegraph init: ~2.3s cold. All N=3 repeats agreed.

| Symbol | Oracle files | CALM recall | CodeGraph recall |
|---|---|---|---|
| `absolute_path` | 2 | 2/2 | 2/2 |
| `build_walker` | 1 | 1/1 | 1/1 |
| `drop` | 1 | 1/1 | 1/1 |
| `flush` | 1 | 1/1 | 1/1 |
| `in_batch_mode` | 1 | 1/1 | 1/1 |
| `merge_exitcodes` | 4 | 3/4 | 3/4 |
| `pattern_matches_strings_with_leading_dot` | 2 | 1/2 | 1/2 |
| `replace_path_separator` | 1 | 1/1 | 1/1 |
| **Total** | **13** | **11/13 (84.6%)** | **11/13 (84.6%)** |

**Tied, and on the exact same two misses** (both are the symbol's *own
definition file* showing up in the grep oracle — a shared oracle artifact,
not a tool-specific miss).

## Results — flask (pallets/flask, Python, external, commit `36e4a82`)

calm index: ~21s cold. codegraph init: ~2.5s cold. All N=3 repeats agreed.

| Symbol | Oracle files | CALM recall | CodeGraph recall |
|---|---|---|---|
| `_called_with_wrong_args` | 1 | 1/1 | 1/1 |
| `_prepare_send_file_kwargs` | 1 | 1/1 | 1/1 |
| `celery_init_app` | 2 | 1/2 | 1/2 |
| `find_app_by_string` | 1 | 1/1 | 1/1 |
| `get_cookie_samesite` | 1 | 1/1 | 1/1 |
| `has_request_context` | 3 | **1/3** | **0/3** |
| `make_setup_state` | 1 | 1/1 | 1/1 |
| `report_error` | 1 | 1/1 | 1/1 |
| **Total** | **11** | **8/11 (72.7%)** | **7/11 (63.6%)** |

**CALM ahead**, entirely on `has_request_context` — a symbol with 3 real
oracle callers (its own file, a docs `.rst` reference, and a test file).
CodeGraph returned **zero** of them (found only the symbol's own
`__init__.py` re-export, not an actual caller); CALM found the test-file
caller but also missed the doc reference (docs aren't callers CodeGraph or
CALM's `callers` tool are designed to find anyway — a `.rst` prose mention,
not code — so 1/3 is closer to a ceiling than it looks for either tool on
this specific oracle row).

## Results — CALM self-repo (isolated worktree, commit `aba60aa8`)

calm index: ~25-32s cold (larger multi-crate workspace). codegraph init:
~6s cold. All N=3 repeats agreed.

| Symbol | Oracle files | CALM recall | CodeGraph recall |
|---|---|---|---|
| `acquire_blocking` | 1 | 1/1 | 1/1 |
| `collect_git_churn` | 1 | 1/1 | 1/1 |
| `error_output` | 1 | 1/1 | 1/1 |
| `inherits_workspace_edition` | 1 | 1/1 | 1/1 |
| `looks_like_glob` | 1 | 1/1 | 1/1 |
| `parse_unified_diff` | 2 | **2/2** | 1/2 |
| `resolve_preset` | 4 | **2/4** | 1/4 |
| `slash_path` | 1 | 1/1 | 1/1 |
| **Total** | **12** | **10/12 (83.3%)** | **8/12 (66.7%)** |

**CALM ahead**, entirely on two multi-file symbols where CodeGraph found the
primary file but missed a secondary indirect-reference file — the same
shape of gap B11 found independently months earlier on an unrelated symbol
(`reindex_changed`/`recover.rs`), a recurring pattern, not a one-off.

## Combined (fd + flask + self-repo, N=24 symbols, 36 oracle files, 3 languages)

| | CALM | CodeGraph |
|---|---|---|
| File-recall | **29/36 (80.6%)** | 26/36 (72.2%) |

Consistent direction across all three independent corpora and two languages
(Rust ×2, Python ×1): tied on fd, CALM ahead on flask and self-repo, always
on multi-reference symbols where CodeGraph finds the primary/obvious file
but misses a secondary indirect one. Not cherry-picked — sampling was
deterministic (sorted, fixed stride) before any tool was queried, and every
corpus run is included, not a subset.

### Freshness probe — reproduced on all 3 corpora, 2 languages, identical result every time

| Corpus | CALM sees new caller @t0s | CALM @t3s | CodeGraph @t0s | CodeGraph @t3s |
|---|---|---|---|---|
| fd (Rust) | No | Yes | Yes | Yes |
| flask (Python) | No | Yes | Yes | Yes |
| self-repo (Rust) | No | Yes | Yes | Yes |

**CodeGraph's file watcher sees a plain external edit immediately in every
run; CALM's incremental watcher takes until somewhere between 0 and 3
seconds, every time.** This is CALM's clearest, most consistent loss across
the whole benchmark — reported plainly, not spun, per this suite's "don't
hide the bad number" policy. Not a correctness gap (CALM does catch up), and
specific to edits CALM itself did not make (its own `edit_lines`/
`edit_symbol` reindex synchronously) — but real, and exactly the scenario
CodeGraph's "auto-sync" pitch targets, so it's a fair axis, not a rigged one.

## How to read this data

Same warning B11's README gives: don't take 80.6% vs 72.2% as a final
verdict on 24 symbols across 3 repos. It's directionally consistent with
what B11 found independently (CodeGraph's cross-file/cross-crate recall
gap) on a completely different symbol, run weeks earlier — that consistency
across independent runs is more meaningful than either single number. All
N=3 repeats were fully deterministic (not one disagreement, either tool,
any corpus) — robustness against transient MCP hiccups, not evidence of
low variance in the underlying recall itself at higher N or on different
symbol samples.

## Limitations (read before citing this anywhere)

- **N=8 symbols per corpus** (repeated 3x each for determinism, not
  resampled) — a real number, not a statistically powered one. A different
  deterministic sample (different stride, different `MAX_OCCURRENCES`
  cutoff) would pick different symbols and could move the percentage.
- **CALM built in `dev` (debug) profile, not `--release`** — chosen to fit
  this run inside a tight, repeatedly-monitored disk budget (down to
  5.3GB free system-wide at one point during Phase 2). Affects wall-clock
  timing (not reported here as a headline number for this reason) but not
  correctness — `callers` returns the same graph data either way.
- **File-path extraction from CodeGraph's free-text responses is
  regex-based and deliberately permissive** — over-matching would only
  ever inflate CodeGraph's recall, never deflate it, so this cannot be the
  reason CALM ties/leads rather than loses.
- **fmt/C++ still not run** — needs a heavier `scip-clang` oracle setup;
  remains the honest gap in language/paradigm diversity (all 3 corpora run
  so far are dynamically-typed-friendly or Rust; no statically-compiled,
  heavily-overloaded C++-style corpus, which `benchmarks/resolution/`'s own
  prior audit found CALM's *own* confidence tiers don't generalize well to).
- **This suite's own past benchmarks have a documented history of setup
  bugs producing wrong numbers** — this run found two more, in itself: the
  harness's own `path`-field parsing bug (fixed before any number was
  recorded) and a genuine CALM indexing crash on the flask corpus (found,
  root-caused, fixed, and regression-tested in the same session — see
  above). Treat every number here as provisional until independently
  reproduced; that's the point of publishing the exact pins and the runner
  script, not just a table.
- **Two more found on 2026-08-03, after publishing this file**: the shared
  ground-truth oracle counted doc/comment mentions as call sites, and the
  harness didn't wait for CALM's SCIP overlay before querying. Both fixed —
  see "Corrected numbers" at the top of this file. Left in this list rather
  than quietly removed, per this same bullet's own point.
- **CALM did not resolve `Self::method()` associated-function calls** in
  Rust (found during the 2026-08-03 correction pass, verified on both fd and
  CALM's own codebase). **Fixed the same day**, along with a related gap in
  the same area (`Self::` inside a `trait` default method) found by the
  same sweep and fixed right after — see "A real CALM parser bug, found,
  root-caused, and fixed in this same pass" above. Corrected combined
  result: 31/31 (100%), not 30/31.

## Reproduce this

```bash
cargo build -p calm-cli   # matches whatever HEAD you're on; record `git rev-parse HEAD` separately
python3 benchmarks/b13_codegraph_multirepo_ab/run_benchmark.py --corpora rust,python,self --n-repeats 3
```

`results.json` is gitignored (suite-wide convention — see `benchmarks/README.md`);
this README's tables are the citable, hand-verified snapshot for this run's
exact pins (above), same convention B11/B12's own READMEs use.

## Files

- `run_benchmark.py` — the runner (corpus setup/cleanup, both MCP clients,
  callers-recall task with repeat/determinism checking, freshness probe).
- Reuses `../b12_tier1_tier2_tool_correctness/{corpora.py,ground_truth.py}`
  and `../lib/generic_mcp_client.py` directly — no forked copies.
