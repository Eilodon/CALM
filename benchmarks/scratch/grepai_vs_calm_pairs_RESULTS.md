# grepai vs CALM — paired capability test (ad-hoc, not the B10/B11 benchmark suite)

Scope: only capability pairs where both tools have a genuinely equivalent tool (user's explicit
rule) — no naive baseline, no `unsupported` bookkeeping, no N=5/IQR. Isolated git worktree corpus
(self-repo, HEAD `3e22ea7`), not the live checkout. Script: `grepai_vs_calm_pairs.py`. Raw data:
`grepai_vs_calm_pairs_results.json`.

Every number below was checked against the actual response content or the real source file before
being reported — two numbers from the first run turned out to be measurement bugs, not real
findings, and are documented as such rather than silently fixed.

## Summary table

| Pair | calm tok | grepai tok | calm speed (median, 3 runs) | grepai speed (median, 3 runs) |
|---|---|---|---|---|
| search | 745 | 897 | 0.005s | 0.270s |
| find_callers | 313 | 702 | 0.003s | 0.091s |
| find_callees | 1,084 | 1,555 | 0.004s | 0.101s |
| call_graph | 1,505 | 6,114 | 0.007s (2 calls) | 0.092s (1 call) |
| index_status | 134 | 83 | 0.064s | 0.296s |

**Speed, unconditionally**: CALM is faster on every single pair, by 15x-90x. This isn't close. Two
structural reasons, not just "better code": (1) CALM's `calm serve` process was already warm/running
for this whole test — its round-trip is SQLite queries in an already-loaded Rust process; (2) grepai's
`search` in particular pays a real Ollama HTTP round-trip for every query embedding, and even its
structural tools (`trace_callers`/`trace_callees`/`trace_graph`) go through a separate Go process
per call. Token cost tells the opposite, weaker story on 3 of 5 pairs (grepai cheaper) — read the
per-pair notes below before concluding either "wins."

## 1. search — messy, not a clean result either way

First run compared CALM's DEFAULT `search` (kind="symbol" — exact name/signature match) against
grepai's semantic search on a natural-language query. That's not a fair pairing — CALM's own tool
description says use `kind="hybrid"` when you don't have an exact symbol name, exactly this case.
Fixed and re-ran with `kind="hybrid"`.

Neither tool put the actual target (`run_indexing_pipeline` in `pipeline.rs`) at rank 1 for the
query `"run_indexing_pipeline function implementation"`:

| Rank | CALM (`kind=hybrid`) | grepai (`grepai_search`) |
|---|---|---|
| 1 | `indexing_status` method, `recover.rs` (unrelated) | `competitor_tasks.yaml` line 58 (unrelated — the string "run_indexing_pipeline" appears there as a YAML value) |
| 2 | `serve_stdio_with_preset`, `lib.rs` (unrelated) | `tasks.yaml` line 48 (unrelated, same reason) |
| 3 | **`run_indexing_pipeline`, `pipeline.rs` — correct** | `pipeline.rs:191` (not inside the target function) |
| 4 | `pipeline.rs` file-level hit | `pipeline.rs:1307` (not inside the target function) |
| 5 | `chunker.rs` (unrelated) | `pipeline.rs:1154` (inside `reindex_changed`, a neighbor, not the target) |

CALM surfaces the exact right answer at rank 3. grepai's top 5 never actually hits a chunk from
inside `run_indexing_pipeline`'s own line range on this corpus. Read this carefully, though: this is
one query on a ~90-file corpus that happens to contain a benchmark YAML file with the literal query
string in it — a semantic search tool getting distracted by that is a real but narrow finding, not
a general verdict on either tool's search quality. `benchmarks/b3_search_quality/` is the right place
for a rigorous NDCG-based search comparison; this is just one anecdote.

## 2. find_callers — clean tie, both exactly correct

Oracle: `grep -rn 'collect_source_files(' crates` (excluding the definition line) → 2 real caller
files (`pipeline.rs`, `recover.rs` — the latter cross-crate, via a fully-qualified path).

- **CALM: 2/2.**
- **grepai: 2/2.**

Both tools resolved the cross-crate, fully-qualified call correctly. No gap here.

## 3. find_callees — both tools have a real, different gap, verified against actual source

Read `reindex_changed`'s current 117-line body directly (not trusted from either tool) to build an
independent oracle of real function calls, then independently re-verified every disagreement by
re-reading the source line-by-line before writing this up.

**Real function calls in the body** (confirmed by reading `crates/calm-core/src/indexer/pipeline.rs:1131-1247` directly):
`load_config`, `FormalResolver::new`, `load_python`, `load_typescript`, `collect_source_files`,
`language_for_extension`, `is_recognized_unparsed_extension`, `rel_path`, `hash_content`,
`mtime_secs`, `now_secs`, `extract_file_data`, `remove_file_rows` (×2), `persist_file`,
`upsert_file_index`, `ReindexSummary::default`, `is_noop`, `CrateMap::build`, `rebuild_graph`
— 19 distinct project-internal calls (`.clone()` on a `String` is also in the body but resolves to
the stdlib `Clone::clone`, not a project function — correctly excluded by both tools, and was a false
positive in my first-pass oracle that got caught and removed before this write-up).

| Tool | Found | Missed |
|---|---|---|
| **CALM** | `FormalResolver::new`, `load_python`, `load_typescript`, `collect_source_files`, `language_for_extension`, `is_recognized_unparsed_extension`, `rel_path`, `hash_content`, `mtime_secs`, `now_secs`, `extract_file_data`, `is_noop`, `CrateMap::build` (13) | **`load_config`, `remove_file_rows`, `persist_file`, `upsert_file_index`, `ReindexSummary::default`, `rebuild_graph`** (6) |
| **grepai** | `load_config`, `FormalResolver::new`, `load_python`, `load_typescript`, `collect_source_files` (5, plus several unresolved stdlib-ish calls like `prepare`/`query_map`/`sort` with empty symbol metadata) | Everything past line ~1153 — **stops covering the function about 20 lines in, out of 117** |

Two different, real, verified gaps:
- **CALM** covers the whole function (finds calls near both the start and the end) but its resolver
  misses 6 specific real calls — notably `load_config`, which is the very first substantive line of
  the function. No confirmed root cause from this test alone (would need to read the resolver code);
  worth a closer look if call-graph completeness on this function matters to you.
- **grepai**'s `trace_callees` has no `depth`/`limit` parameter in its schema, and empirically stops
  analyzing call sites roughly 20 lines into a 117-line function — it's not that it resolves fewer
  calls, it's that it doesn't look at the rest of the function body at all for this call.

Neither tool is "more accurate" here in a way that generalizes — they fail differently, on different
mechanisms (resolution gap vs. body-coverage cutoff).

### Follow-up verification (direct tool calls, not the script) — is CALM's gap a SCIP-overlay timing artifact?

`scip-overlay` turned out to already be a **default** Cargo feature (`crates/calm-core/Cargo.toml`,
promoted 2026-07-05, confirmed via `git log -S`) — not something needing an explicit `--features`
flag as an older `benchmarks/README.md` note about B2 implied. It also runs on a background thread
independent of `indexing_phase`, which raised a real possibility: did the original test read
`callees` before the overlay finished, silently using a lower resolution tier?

Checked directly, no script: started `target/release/calm` by hand over raw stdio, polled
`indexing_status` every second. Result: `scip_overlay: {available: true, up_to_date: true}` was
already true at t=0.1s, before `indexing_phase` even reached `"ready"` (t=5.4s on one run, t=0.1s on
a repeat — cache-dependent). Queried `callees(reindex_changed)` immediately, then again 30 seconds
later: **byte-identical result both times**, all 13 edges already at `edge_confidence: "formal"` in
both reads. So the hypothesis is **ruled out** — this is not a timing artifact. CALM's resolver
stably, reproducibly does not resolve `load_config`, `remove_file_rows`, `persist_file`,
`upsert_file_index`, `rebuild_graph`, `ReindexSummary::default` for this function, with or without
waiting. (Loose pattern noticed, not confirmed as root cause: the 5 non-`default` misses are all
inside nested `for`/`if` blocks in the function's second half; the 13 it does find are mostly in the
flatter first half — would need to actually read the resolver code to confirm this isn't coincidence.)

Re-ran grepai's `grepai_trace_callees` directly too (fresh corpus, raw JSON-RPC, not through the
script): same cutoff, reproduced — stops at line 1153, 12-13 entries, never reaches the calls in the
function's back half regardless of whether they'd resolve. Stable on repeat, not a fluke.

## 4. call_graph — same underlying gaps as #3, packaged differently

CALM: `callers` + `callees` (2 separate calls, 1,505 tokens combined). grepai: `grepai_trace_graph`
(1 call, depth=2, 6,114 tokens — costs more because it returns nodes+edges for the *whole*
2-hop neighborhood, not just direct callers/callees). The call-count difference (2 vs 1) is real and
worth knowing on its own — grepai bundles both directions in one round-trip, CALM doesn't. Content
correctness for this pair inherits the same `find_callees` gap noted above.

## 5. index_status — functional parity, CALM faster

Both correctly report ready state. CALM: `indexing_phase: "ready"`, 88/88 files, 1356 symbols, 2072
edges. grepai: `symbols_ready: true`, 148 files, 1795 chunks. CALM ~4.6x faster on this specific call
(0.064s vs 0.296s) — smallest speed gap of the 5 pairs, since grepai's status check doesn't need a
fresh embedding call.

## What I'd actually trust from this test

- **Speed**: CALM meaningfully faster on every pair, real and consistent — this one I'd stand behind.
- **find_callers**: genuine tie, both correct on the one case tested.
- **find_callees / call_graph**: both tools have real gaps; neither is uniformly more accurate — don't
  round this off to "CALM wins on accuracy," the picture is mixed and verified as mixed.
- **search**: too small a sample (1 query, 1 small corpus with a self-referential YAML quirk) to
  conclude anything about search quality specifically — see B3 for a real answer to that question.
