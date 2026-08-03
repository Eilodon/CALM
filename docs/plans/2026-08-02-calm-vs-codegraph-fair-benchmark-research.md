# CALM vs CodeGraph — Fair Benchmark Research (2026-08-02)

Research-only deliverable (no code/build/run yet). User asked two things: (1) is the
CALM this session is running the latest version, and (2) design the most accurate, fair,
honest benchmark methodology to compare CALM against CodeGraph, for a public "if you use
CodeGraph, here's why to also try CALM" publication — install both MCPs, measure on 2-3
publicly re-clonable OSS repos plus CALM's own repo, and be careful because this suite's
own past benchmarks have a documented history of setup bugs producing wrong numbers.

## 1. Version check — verdict

| What | Version | Source |
|---|---|---|
| Latest published CALM release | **v0.4.0**, 2026-08-01 | `git ls-remote --tags`, `gh release list` (GitHub), matches `Cargo.toml` `version = "0.4.0"` and npm `@eilodon/calm-mcp@0.4.0` |
| This session's live MCP server | `target/debug/calm`, built from local HEAD | `ps aux` shows the daemon PID running `target/debug/calm serve` — per `scripts/mcp-launcher.sh`'s own fast-path rule, a fresh local dev build always wins over a downloaded release when it's newer than every source file |
| Local git HEAD vs v0.4.0 | **5 commits ahead**, unreleased | includes today's WS-1 (`1830328`) and WS-2 Phase 2 caller-set-digest (`aba60aa`) work — not yet tagged |
| Global CLI (`~/.local/bin/calm`, used by other projects on this machine) | **v0.3.6** (2026-07-22) | `calm --version` — **stale, 2 releases behind**, missing everything in v0.4.0's changelog (JS/TS call-graph blind-spot fixes, dynamic toolsets, HTTP transport, OTel, Martin/OOD metrics, `b7_task_correctness`) |

**Answer: this session's CALM is not just "latest," it's ahead of latest** (an unreleased
dev build newer than the v0.4.0 tag). The thing that **is** stale is `~/.local/bin/calm`,
the global binary other projects on this machine (KARMA, Code-Intelligence, HolySeed, etc.
per `~/.cache/claude-cli-nodejs/*/mcp-logs-calm`) would fall back to if their own
`mcp-launcher.sh` fast path doesn't find a fresher local dev build. Worth a
`cargo install` refresh separately from this research task if those other projects matter.

## 2. What already exists — don't rebuild, extend

This repo already runs a competitor-benchmark suite (`benchmarks/`) that has been through
multiple audit-and-fix cycles. Relevant pieces, verified by reading them directly (not
from memory):

- **`b10_real_competitor_ab/`** — first CALM-vs-CodeGraph-vs-Semble A/B. Superseded; its
  own successor's README lists 5 methodology holes it had (see §3).
- **`b11_extended_competitor_ab/`** — the current real A/B: CALM vs CodeGraph vs Semble vs
  grepai vs Serena, **real MCP tool calls**, real oracles for all 4 shared tasks (not just
  1 of 4 like B10), N=5 repeats, isolated `git worktree` corpus (never the live repo — one
  task does a real edit attempt), documented "how to read this data" caveats, and it
  already includes the two tasks that actually probe CALM's structural differentiators
  (`risk_gate_refusal`, `memory_recall`). **Limitation: self-repo only (CALM's own Rust
  code), and the CodeGraph version it pinned (`competitor_tasks.yaml`: "v1.2.0") is now 3
  minor releases stale** — npm's current latest is **1.5.0**, published 2026-07-21.
- **`b12_tier1_tier2_tool_correctness/`** — drives 9 real CALM MCP tools via JSON-RPC
  against **6 pinned, real external OSS repos**, one per Tier-0 language: flask (Python),
  gin (Go), spring-petclinic (Java), express (JS), zod (TS), fd (Rust, cloned outside the
  CALM workspace on purpose — nested-workspace cargo detection silently breaks
  rust-analyzer otherwise). Ground truth is an independent regex extractor + `git grep`,
  deliberately not CALM's own parser. **CALM-only — no competitor was ever run against
  this corpus.** This is exactly the multi-repo, publicly-reproducible corpus the user is
  asking for; it just needs a competitor arm added, not a new corpus built from scratch.
- **`b7_task_correctness/`** (new in v0.4.0) — reuses B12's exact same 6-repo registry and
  adds the strongest oracle type in the whole suite: **the corpus's own real build+test
  suite** (`cargo test`, `pytest`, `npm test`, `go test`, `mvnw test`), pass/fail, no LLM
  judge. Compares CALM's scripted refactor loop (`edit_context` → rename → `diff_impact`)
  against a naive grep-and-edit baseline. CodeGraph has no edit tool (confirmed below), so
  it can't be a 3rd arm of *this exact* comparison as-is, but its read/impact tools could
  fairly be spliced in as "CodeGraph-informed manual edit" vs CALM's scripted loop vs naive
  grep — see §5.
- **`resolution/`** — 19-language tier-distribution sweep. **Has no oracle at all**
  (measures resolved/inferred/textual/ambiguous *proportions*, not whether any edge is
  *correct*) — a past session already got firmly corrected by the user for citing this as
  an accuracy number. Do not use it as evidence in the new benchmark; it answers a
  different question.

## 3. Catalogued pitfalls — every one of these is a real, previously-shipped bug in this
suite's own methodology, not a hypothetical. The new benchmark must design these out from
the start, not fix them after the fact:

1. **Unfiltered "ruled out" edges inflate error counts 2.5-3x.** An earlier accuracy script
   read `call_edges` without excluding `ruled_out_by_scip = 0`, double-counting edges every
   real agent-facing tool already suppresses. → always filter exactly like the production
   tools do; if in doubt, call the *same* MCP tool an agent would, not a raw DB query.
2. **Exact `(file,line)` oracle matching understates real correctness**, because a symbol's
   declaration line and a compiler's def-occurrence line legitimately differ by a few
   lines (worst on TS: a apparent 60.8% "accuracy" was actually 90.2% once a ±3-line/
   same-file tolerance was applied). → use a tolerant match, document the tolerance.
3. **Recall denominators must match what's being measured.** Counting every SCIP reference
   occurrence (var reads, type refs, imports) against a tool that only models *calls*
   manufactures a "low recall" that's really a scope mismatch. → restrict oracle
   edges to the same relation being tested before computing recall.
4. **A confidence tier's precision does not generalize across languages.** Measured
   directly: `resolved`/`inferred` tiers are ~93-97% correct on CALM's own Rust code, but
   only ~55-57% correct on fmt (C++) against a real `scip-clang` oracle — barely better
   than a coin flip on heavily-overloaded header-only C++. → validate precision per
   language actually included in the benchmark; never port a Rust number to another
   language's row.
5. **N=1, no repeats** (B10) hides variance and process/MCP hiccups. → N≥5 per task per
   tool, report median + spread, note when all repeats were identical (that's itself a
   finding: determinism, not just robustness).
6. **An unflagged tokenizer proxy misrepresents cost.** B11 uses GPT-4 BPE (`tiktoken`) as
   a stand-in for whatever model an agent actually runs on and says so explicitly in the
   README. → keep doing that, or use the real target model's tokenizer if the publication
   claims real cost, not just directional comparison.
7. **Correctness must be measured for every task, not just one of four** (B10's gap,
   fixed in B11). A token-efficiency ratio without a correctness column next to it is not
   evidence of quality — B11's own README shows CodeGraph's `pre_edit_blast_radius` ratio
   (577.8x) looks dominant purely because its recall was 1/5, not because it compresses
   the same answer better.
8. **Hard-refuse-gate test anchors must be independently verified as hubs** before
   claiming a refusal test exercised the gate (B10 never checked this rigorously; B11
   re-verified via `hotspots(include_symbols=true)`). → always verify the gate-triggering
   precondition out-of-band, don't assume the anchor qualifies.
9. **A bare `codegraph` on `PATH` can silently resolve to an unrelated tool** with the same
   name (found live during B11 setup — writes its own `.codegraph.db`/`CLAUDE.md`/hooks).
   → always invoke `npx -y @colbymchenry/codegraph`, exactly as this repo's own
   `.mcp.json` already does, never a bare `codegraph`.
10. **CodeGraph ships 7 of its 8 tools hidden by default** (`CODEGRAPH_MCP_TOOLS` env var
    required to expose `codegraph_node/search/callers/callees/impact/files/status` — only
    `codegraph_explore` is on by default). → an A/B claiming to compare full capability
    must set that env var explicitly and say so, or it's silently comparing against a
    crippled competitor config (this repo's `.mcp.json` already sets it correctly).
11. **Version drift invalidates old numbers without anyone noticing.** The existing
    `competitor_tasks.yaml` docstring pins "CodeGraph v1.2.0"; current npm latest is 1.5.0
    (2026-07-21). The JS/TS call-graph blind spots B11/B12 measured on CALM's side
    (`ac47e0a`, `34918c4`) are now fixed and shipped in v0.4.0 — a re-run today would
    likely show materially different numbers on **both** sides of the table. → every
    published number must carry the exact CALM git SHA (or tag) and exact CodeGraph npm
    version used, and a benchmark result should be treated as expired once either tool
    ships a release that touches the measured code path.
12. **Unconditional `results.json` overwrite with no merge/append** (every benchmark
    script in the suite, sharpest in `resolution/` which also has a `--lang` subset flag)
    can silently drop other languages'/tools' rows on a partial re-run. → the new
    benchmark's runner must append/merge by (tool, task, corpus) key, or write per-run
    timestamped files, never blindly overwrite a shared results file.
13. **Never run a benchmark against the live self-repo index/daemon** — `git worktree add
    --detach` (or a throwaway clone) is mandatory when the run does real edits or rebuilds
    an index a live daemon might be serving from; this has already caused near-misses.
14. **Self-scoring is not evidence to a skeptical outside audience.** Every number in this
    suite so far has been produced, checked, and published by the same project that built
    the tool being favorably measured. That is exactly the failure mode of CodeGraph's own
    published marketing numbers too (see §4) — the new benchmark should not repeat it if
    the goal is to convince existing CodeGraph users specifically, who have every reason to
    be skeptical of a vendor-authored comparison. See §5's reproducibility requirements.

## 4. What CodeGraph actually claims, and how rigorous that is

(`@colbymchenry/codegraph`, MIT-style OSS project + npm package + MCP server —
[github.com/colbymchenry/codegraph](https://github.com/colbymchenry/codegraph),
[colbymchenry.github.io/codegraph](https://colbymchenry.github.io/codegraph/))

- Local-first, pre-indexed code knowledge graph, auto-resyncs on file changes, exposed to
  Claude Code/Cursor/Codex/opencode/Gemini/Antigravity/Kiro/Hermes over MCP.
  30+ languages claimed (marketing copy) / "20+" (docs page) parsed incrementally.
  **Read-only by design** — no edit/write tool of any kind, no memory/notes concept
  (both independently confirmed live in this repo's own B11 run, not just from docs).
  8 MCP tools total, only `codegraph_explore` on by default.
- **Their own published numbers**: "89% fewer tool calls · 60% cheaper · 69% fewer tokens
  · file reads cut to zero" (per their docs site) / "59% fewer tokens, 49% faster, 70%
  fewer tool calls" (per a Medium post by the author) across "seven real-world codebases,"
  Claude Opus 4.8, "4 median runs per configuration." **No named repos, no raw data, no
  reproduction script, no third-party tool comparison anywhere in their public materials**
  — it's a with-vs-without-CodeGraph self A/B, not a vs-competitor benchmark, and none of
  it is independently reproducible today. CALM's own B11, despite its self-repo/staleness
  limitations, is already more rigorous than this by every axis in §3 (real oracles,
  documented limitations, negative findings kept in, N≥5).
- Practical implication for the new benchmark's credibility pitch: CALM doesn't need to
  match CodeGraph's marketing numbers, it needs to be **more reproducible than them** —
  publish the exact runner script, the exact pinned repo commits, and the raw JSON, so a
  skeptical CodeGraph user can rerun it themselves and get the same numbers. That
  reproducibility gap is itself part of the honest pitch, not just the measured deltas.

## 5. Proposed methodology for the new benchmark ("B13"?)

Don't start from zero — compose the three proven pieces already in this repo, apply every
lesson in §3, and close the two real gaps (single-corpus, no independent reproduction path).

**Corpora** — reuse B12/B7's already-pinned, already-verified-buildable registry
(`benchmarks/b12_tier1_tier2_tool_correctness/corpora.py`) rather than picking new repos:
run the self-repo dogfood pass (user explicitly wants this) **plus** at least 3 of the 6
external pinned repos, chosen for oracle strength and language spread — suggest
**fd (Rust)**, **flask (Python)**, and **fmt (C++, from the `resolution/` corpus, since
it's the one repo where CALM's own confidence-tier trust is already known to *not*
generalize from Rust — an honest low point is part of a credible pitch, not something to
hide). All are permissively licensed, small enough to clone/build in CI, and already have
a proven `scip-*`/compiler-grade oracle recipe documented in this repo.

**Tasks, in order of how hard they are to game:**
1. **Task-correctness with a real build+test oracle** (B7's method): a scripted rename/
   refactor, applied via (a) CALM's `edit_context`→edit→`diff_impact` loop, (b) a
   CodeGraph-informed manual edit (use `codegraph_callers`/`codegraph_impact` to find
   every site, then plain-write the edits — the fair framing given CodeGraph has no editor
   of its own), (c) naive grep-and-edit baseline. Pass/fail = corpus's own test suite,
   zero LLM judgment.
2. **Read/impact correctness** (B11/B12's method, extended past self-repo): callers/
   callees/impact-radius file recall against an independent oracle (regex+grep, or
   compiler-grade SCIP where available) on the 3+ external repos, not just self-repo.
3. **Freshness under a live edit — new, not in any existing benchmark**: make a real
   edit that adds/removes a caller, then immediately query both tools' call-graph/impact
   tools without waiting. CodeGraph markets "auto-sync on every file change" as a
   headline feature; CALM just shipped (`aba60aa`, this session) a mechanism specifically
   to detect this class of staleness (`STALE_CALLER_SET`). This is a sharp, honest,
   directly-adversarial test of a claim both projects actually make in public — exactly
   the kind of axis a skeptical reader will trust more than a token-cost ratio.
4. **Safety/persistence properties** (B11's `risk_gate_refusal`/`memory_recall`, ported
   as-is to the external corpora): CodeGraph is `unsupported` on both by design (read-only,
   no memory) — report that honestly as a structural difference in what each tool is
   *for*, not as a "CodeGraph loses," per §3 lesson 7's "different question" warning.
5. **Cost** (tokens/tool-calls, B4/B6/B11's method) — reported only alongside the
   correctness columns above, real target-model tokenizer if the publication makes a real
   cost claim, GPT-4 BPE proxy explicitly flagged otherwise.

**Fairness/reproducibility requirements (the part that actually earns trust with
CodeGraph's existing users):**
- Pin and publish the exact CALM git SHA/tag and exact `@colbymchenry/codegraph` npm
  version for every number reported; re-run and re-publish whenever either changes on a
  measured code path (§3 lesson 11 — both have moved since the last real run).
- Publish the runner script, the pinned repo commit SHAs, and the raw JSON output
  (a frozen dated snapshot, not the gitignored live-rerun file) so an outside CodeGraph
  user can `git clone` the same commits and reproduce the table themselves — this is the
  concrete answer to §3 lesson 14 (self-scoring isn't credible on its own).
- Pre-register the task list before running (this doc) rather than after seeing results,
  to pre-empt any "cherry-picked the tasks CALM wins" reading.
- Always isolate via `git worktree add --detach` for any corpus that gets edited or
  reindexed; never touch a live daemon's `.calm/index.db` or `.codegraph/`.
- Report every negative/structural result plainly (CodeGraph wins on raw token cost for
  read-only tasks where it returns less because it's answering a narrower question — say
  that outright, the way B11's README already does, not as a footnote).

## 6. What this document does NOT do

No code was written, nothing was installed or run. This is the design + fact-finding pass
the user asked for ("giờ nghiên cứu vấn đề sau"). Next decision point before spending real
time/compute: confirm the 3-external-repo shortlist (fd/flask/fmt proposed above) and
whether to build this as a new `benchmarks/b13_*` directory extending B11+B12+B7's code
directly, before actually installing CodeGraph 1.5.0 fresh and running anything.

## 7. UPDATE — Phase 1 built and run same session

User chose to proceed immediately rather than wait for review. Built
`benchmarks/b13_codegraph_multirepo_ab/` exactly per §5/§6 above, scoped to fd (external
Rust) + CALM self-repo (isolated worktree) — flask/fmt descoped to Phase 2, disclosed in
the new benchmark's own README, for two real reasons hit live: this machine dropped to
97%/7.5-7.9GB free disk mid-run, and turn/time budget. Full results, methodology, and
caveats: `benchmarks/b13_codegraph_multirepo_ab/README.md`.

Headline, combined across both corpora (N=16 symbols, 25 oracle files, single run, not yet
repeated): **CALM 21/25 (84.0%) file-recall vs CodeGraph 19/25 (76.0%)** on `callers`/
`codegraph_callers`, tied exactly on fd (11/13 each, same 2 misses — a shared oracle
artifact) and CALM ahead on self-repo (10/12 vs 8/12) on two multi-file symbols where
CodeGraph found the primary file but missed a secondary indirect-reference file — the same
shape of gap B11 found independently, months earlier, on an unrelated symbol
(`reindex_changed`/`recover.rs`). New freshness-under-live-edit task (proposed in §5,
never run before this session) found a real, disclosed CALM weakness in both corpora:
CodeGraph's file watcher sees a plain external file edit immediately; CALM's incremental
watcher took ~3s to catch up on the identical edit in both runs.

New pitfall found live during THIS benchmark's own build, added to §3's catalogue: a bare
`npx -y @colbymchenry/codegraph` (no version pin) silently resolved to a stale cached
**1.4.1** even with `-y`, while `npm view` confirmed true latest was **1.5.0** — every
spawn in the new benchmark now pins `@colbymchenry/codegraph@1.5.0` explicitly. Also
self-caught before publishing: the harness's first run scored CALM 0/N on literally every
sample — traced to `extract_paths_from_calm_callers` assuming a `path` field CALM's real
`callers` response doesn't have (the file lives inside a qualified `symbol` string,
`path::Type::method`) — fixed and re-run before any number above was recorded. Exactly the
"benchmark bug, not tool bug" failure mode this whole document exists to guard against,
caught this time before publication instead of after.

Not yet done: flask/fmt (Phase 2), N>1 repeats, pushing/committing this to the repo (held
for the user's sign-off given this is headed toward public use).

## 8. UPDATE — Phase 2 (flask + N=3 repeats), plus a real CALM bug found and fixed

User chose to continue immediately rather than pause for review. Added **flask**
(external, Python) and repeated every query **3x per symbol per tool** (B11's exact
rationale: catch transient MCP hiccups, not resample different symbols) — every single
repeat agreed with itself across all 24 symbol/corpus/tool cells, fully deterministic.

**Building this hit a real, load-bearing CALM bug, not a benchmark artifact**: a fresh
CALM could not index the flask corpus at all — `indexing_phase` went straight to
`"failed"` with `UNIQUE constraint failed: call_sites....identity_version`, 0/93 files,
reproduced 3 times independently before concluding it was real. Root-caused to
`crates/calm-core/src/indexer/pipeline.rs::persist_file`: a plain `INSERT INTO
call_sites` with no `OR IGNORE`, predating a UNIQUE index (`migrate_call_site_identity_v2`)
added later — every sibling identity-constrained table (`call_edges`, `import_edges`)
already used `INSERT OR IGNORE` for this exact reason; this insert was the one that never
got updated. Fixed (added `OR IGNORE` + a `tracing::debug!` skip-count, fail-soft not
silent), verified live (flask now indexes 93/93 files cleanly), and covered by a new
regression test in the same file. The upstream tree-sitter question — what Python
construct makes the extractor emit the exact-duplicate call site in the first place — is
still open; the persistence-layer fix is correct and sufficient regardless of that answer.

Also fixed the benchmark harness's own bug found while debugging this: `wait_calm_indexed`
looped until `indexing_phase=="ready"` with no check for `"failed"`, so a permanent
indexing failure looked identical to "still working" until the full timeout elapsed —
now fails fast with the real `indexing_error` instead.

**Updated combined result across all 3 corpora (fd, flask, self-repo — 24 symbols, 36
oracle files, N=3 repeats each)**: **CALM 29/36 (80.6%) vs CodeGraph 26/36 (72.2%)**.
Same direction as the Phase-1-only number (84.0/76.0), now confirmed on a 3rd corpus and
a 2nd language (Python), not just 2 Rust corpora. Freshness-under-live-edit result
reproduced identically on all 3 corpora/2 languages: CodeGraph sees an external plain-text
edit immediately every time; CALM's watcher takes until somewhere between 0-3s every time
— CALM's clearest, most consistent, most reproducible loss in the whole benchmark.

Full results/methodology/limitations: `benchmarks/b13_codegraph_multirepo_ab/README.md`
(fully rewritten for Phase 1+2 combined). Still not committed/pushed — held for the
user's review given this (and the CALM bugfix riding along with it) is headed toward
public use. fmt/C++ still open (needs `scip-clang` oracle setup).

## §9 (2026-08-03): oracle + rigor correction, requested after publishing the 80.6/72.2 number

User asked why CALM's margin looked thin given its extra complexity, and whether the
strongest CALM configuration was even used. Two real bugs in this benchmark's OWN
methodology were found and fixed (not a CALM-vs-CodeGraph tool question this time, an
oracle-and-harness-correctness question):

1. B12's shared `ground_truth.py::git_grep_call_sites` oracle counted markdown/`.rst`
   doc mentions and source-code comments as if they were real call sites (`git grep` had
   no pathspec restriction and no comment filter). Verified directly on 4 already-
   published oracle entries. Neither tool ever found any of them (correctly), so both
   were penalized for "missing" call sites that were never real. Fixed: pathspec-
   restricted to the corpus's own language extension, comment lines excluded.
2. `run_benchmark.py`'s `wait_calm_indexed` returned before CALM's automatic async SCIP
   overlay pass (rust-analyzer/scip-python) had run, confirmed live via a fresh fd clone
   (`scip_overlays[rust].up_to_date` stayed `false` 15s post-ready with no auto-trigger).
   Fixed: the harness now forces `scip_refresh` explicitly before scoring. Empirically
   this didn't change recall on the 3 symbols spot-checked pre-fix (only confidence tier
   + a few ruled-out phantom edges) — fixed anyway, for rigor rather than because it was
   shown to move the old numbers.

**Corrected, canonical result (fresh re-run, same pins, N=3 repeats, oracle-driven
sample necessarily differs from Phase 1+2's)**: **CALM 30/31 (96.8%) vs CodeGraph 27/31
(87.1%)**. The sole row CodeGraph now wins (fd's `replace_separator`) was independently
verified as a REAL, reproducible CALM gap, not oracle noise: CALM's Rust tree-sitter call
extractor doesn't resolve `Self::method()` associated-function calls, reproduced on
CALM's own codebase too (`ConservativeResolver::default`'s `Self::new()` call is invisible
to `callers`, even though every fully-qualified `ConservativeResolver::new()` call in the
same file is found). Not fixed as part of this pass — flagged as a concrete, high-value
follow-up in `crates/calm-core/src/indexer/parser.rs`.

Full writeup: `benchmarks/b13_codegraph_multirepo_ab/README.md`'s "Corrected numbers"
section (now the canonical entry point; Phase 1+2 sections kept below it, marked
superseded, not deleted). This section of this doc, like that README, replaces the
headline number everywhere it was cited above — do not cite 80.6/72.2 going forward.

**Same-day follow-up, per user's explicit ask to root-cause the `Self::` gap and sweep for
similar bugs before deciding whether to fix**: chased the exact chain through
`is_type_like`/`split_receiver_callee`/`extract_file_data`/`resolve_sites_to_edges`
(`crates/calm-core/src/indexer/{parser,pipeline}.rs`) — `target_class` was left as the
literal keyword text `"Self"` instead of the enclosing `impl` block's real type name,
even though that name was already tracked correctly (`enclosing_class`). 42 real
`Self::method()` call sites exist in CALM's own repo alone (15 files) — not a rare case.
Swept for parallels: found one more real gap in `walk_calls`'s handling of Rust
`trait_item` (no `"type"` field, unlike `impl_item`) for call extraction specifically.
First call was to skip it (reasoning: `Self` inside a trait default method seemed
inherently unbound to one concrete type, so a fix would be a guess) — **that reasoning was
wrong**, caught on direct follow-up when asked to justify it. A live characterization test
showed the real mechanism was identical to the `impl_item` bug (`enclosing_class` simply
`None` inside a trait, `target_class` left as literal `"Self"`), not a "which concrete
type" ambiguity at all. Fixed the same way (`walk_calls` now reads `trait_item`'s `"name"`
field, mirroring `walk_symbols`'s existing special-case). Both fixes + regression tests,
`cargo test -p calm-core --lib -- indexer:: resolver:: rust` → **356 passed, 0 failed**.
Rebuilt, re-ran b13 after the first fix: **31/31 (100%) CALM vs 27/31 (87.1%) CodeGraph**
— CALM has zero misses in this sample (the trait fix doesn't change this specific number,
none of the 24 sampled symbols involve a trait default method, but is real and tested).
Full detail in `benchmarks/b13_codegraph_multirepo_ab/README.md`'s "A real CALM parser
bug, found, root-caused, and fixed in this same pass".
