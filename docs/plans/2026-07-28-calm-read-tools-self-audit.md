# CALM self-audit: the codebase-understanding toolchain (search/locate/source/understand/callers/callees/path/dependencies/hotspots/repo_overview)

**VHEATM 16.1 — Standard mode, Tier 2, SELF-AUDIT=YES (QBR×1.20 applied).**
Date: 2026-07-28. Auditor: Claude Code session, using CALM's own MCP tools against CALM's own
running daemon (build `1d58967ce7aa`) — no subagents used for verification (standing preference:
[[feedback-no-subagents-inline-verification]]).

**Scope:** the *read/discovery* tool group only — `search`, `locate`, `source`, `understand`,
`file_overview`, `callers`, `callees`, `path`, `dependencies`, `hotspots`, `symbols_batch`,
`repo_overview`. Explicitly NOT in scope: `edit_lines`/`edit_symbol`/`edit_context` write-gate
logic (audited separately, extensively, in prior sessions — see memory index), resolver/indexer
internals beyond what's needed to explain a tool-surface symptom.

**[E.IJ] Independent Judge note:** Tier 2 + SELF_AUDIT=YES nominally requires this. No subagent
was spawned to perform it (standing user preference overrides the default protocol here). In lieu
of it, every finding below was subjected to a same-session adversarial re-check — 2 of my own
initial hypotheses were disproven before being written down (see "Hypotheses rejected" at the end).
This is a documented, honest gap, not a silent skip (`Debt-EJ-CLAUDEAI-001` in VHEATM's own
framework debt list applies here).

---

## Findings, most severe first

### F1 — [CONFIRMED, HIGH] `call_edges` has silently-duplicated rows for at least one real symbol, inflating caller counts ~19x

**Evidence anchor:** live tool calls this session.
- `understand(query="path")` → picked `PathMatcher` (boundaries.rs enum) as best match, then
  dumped a `callers_summary` array with ~110+ entries that are all just 6 distinct
  `(symbol, line)` pairs repeated over and over.
- `callers(symbol="PathMatcher", path="crates/calm-core/src/analysis/boundaries.rs")` — called
  directly, independent of `understand` — confirms it's a data problem, not an `understand`
  assembly bug: `direct_count: 113`, `direct_truncated: true`, and every one of the (capped) 25
  returned entries is the **exact same** row: `from_symbol=PathMatcher::new`,
  `line=49`, `edge_kind=call`, `to_symbol=PathMatcher`. The real source line
  (`crates/calm-core/src/analysis/boundaries.rs:49`, `return PathMatcher::Glob(glob.compile_matcher());`)
  appears exactly once in the file — it cannot legitimately produce 113 call_edges rows.

**Scope check (Pattern Globalization, partial):** tested a control symbol from the least-churned
file in the repo (`Embedder`, `crates/calm-core/src/embedding.rs`, `commit_count: 4` per
`hotspots`) — `callers(symbol="Embedder", line=130)` returns a clean `direct_count: 6` with 6
distinct real call sites, no duplication. **This rules out total `call_edges`-table corruption**
— the bug is localized to some subset of symbols/files, not universal. Full scope (which other
symbols/files are affected, and by how much) is **not yet determined** — this needs a direct
`SELECT from_symbol, to_symbol, call_site_line, COUNT(*) FROM call_edges GROUP BY 1,2,3 HAVING
COUNT(*) > 1` query against `.calm/index.db`, which no current MCP tool surfaces (see F7).

**Root cause: NOT YET VERIFIED — 2 competing hypotheses, un-adjudicated:**
1. **Stale-row accumulation across repeated incremental reindexes.** Phase B (F1 incremental
   graph update, 2026-07-13, [[calm-upgrade-plan-3-execution]]) replaced full-graph rebuild with
   incremental updates. If the delete-before-insert step for a changed file's `call_edges` rows
   has any gap (e.g. scoped by the wrong key, or skipped on some reindex path), a file edited/
   reindexed repeatedly during active development (which `boundaries.rs` — part of the fitness/
   boundaries feature — plausibly has been, across many sessions) would accumulate duplicate rows
   over time while a rarely-touched file like `embedding.rs` would not. This story fits the
   evidence (localized, and to a plausibly-more-reindexed file) but is **not confirmed** — I did
   not read the incremental-update code path this session.
2. **A live dedup bug in the current indexer**, specific to some AST shape `boundaries.rs:49`
   has (e.g. an enum-variant-constructor call inside a `.map()` closure, since
   `check_boundaries` at line 79 does exactly this pattern:
   `rules.iter().map(|rule| (PathMatcher::new(&rule.from), PathMatcher::new(&rule.to)))`) that
   would reproduce on a *fresh* full reindex, not just accumulate over time.

**Distinguishing the two is the necessary first diagnostic step** before any fix: run a clean
`calm index --project-root .` against a fresh `.calm/index.db` (or `DELETE FROM call_edges WHERE
from_path = 'crates/calm-core/src/analysis/boundaries.rs'` then reindex just that file) and see
if the duplication reappears. If it does, it's a live indexer bug (hypothesis 2). If it doesn't,
it's accumulated cruft (hypothesis 1), and the real fix is auditing the incremental-update
delete/insert transaction boundary — with a broader implication that **any repo that has been
running CALM's daemon across many incremental reindexes over time may be silently accumulating
duplicate call_edges**, not just this repo's `boundaries.rs`.

**Downstream impact (why this matters for an agent, not just a data-hygiene nit):**
- `callers`/`edit_context`'s `direct_count`/`confirmed_caller_count` are the primary blast-radius
  signal an agent reads before editing. A symbol with 113 phantom callers reads as high-risk/
  hub-like when it may have single-digit real usage — the opposite of F3/F4's under-counting risk,
  but equally capable of producing a wrong decision (an agent might refuse to touch, or demand
  excessive `reason` justification for, a symbol that's actually safe to change).
- `blast_radius.transitive` (via `transitive_bfs`) inherits the inflation on the first hop, before
  ADR-0009's ambiguous-non-expansion guard even applies (this bug is on `formal`-confidence edges,
  which BFS *does* expand through).
- Token cost: a single `understand`/`callers` call on an affected symbol burns 100+ lines of
  100%-redundant output — directly hostile to the "efficient for a coding agent" goal this audit
  was scoped around.

**Recommended next action:** root-cause via the diagnostic above (I did not do this — it edits/
reindexes, appropriately left as a deliberate next step rather than folded into a read-only
audit), then either (a) add a `UNIQUE(from_symbol, to_symbol, call_site_line, edge_kind)`
constraint or ON CONFLICT dedup at insert time if it's hypothesis 2, or (b) audit/fix the
incremental-update delete scope if it's hypothesis 1. Either fix should ship with a regression
test seeded from this exact repro (`boundaries.rs::PathMatcher::new` line 49) and a one-off
cleanup migration/script for existing `.calm/index.db` files that may already have accumulated
duplicates (this repo's own DB included).

---

### F2 — [CONFIRMED, MEDIUM-HIGH] `understand`'s single-best-match silently mis-picks among ambiguous same-named candidates, with no signal that alternatives existed

**Evidence anchor:** `understand(query="path")` this session returned `PathMatcher`
(`crates/calm-core/src/analysis/boundaries.rs`, a 2-variant enum, `caller_count: 3` per its own
`symbol_info`-equivalent metadata) as *the* answer — not `CalmServer::path`
(`crates/calm-server/src/tools/trace.rs`), which is:
- one of `repo_overview`'s own top-15 `core_symbols` (coreness 8, `caller_count: 83`, `is_hub: true`)
- literally the MCP tool named `path` that this exact audit used minutes earlier
- the overwhelmingly more likely intended target for a bare query `"path"` from any real agent
  question ("how does the path tool work", "what does path do here")

`understand`'s own tool description says it does "locate + source + callers summary in 1 call...
only the single best match is used" — by design it collapses ambiguity into one silent pick, with
no `ambiguous: true`/candidate-list escape hatch the way `callers`/`edit_context`/`path` all have
(`SymbolResolution::Ambiguous(candidates) => return ResolvedOutcome::ambiguous(&candidates)` — a
pattern used consistently in `trace.rs`/`guardrails.rs`, confirmed by direct code read). `locate`
(which `understand` is explicitly built as a compound wrapper around) supports a `results` list
that would show both candidates; `understand`'s "convenience" collapse throws that away.

**Failure scenario:** an agent asks `understand("path")` intending to learn about the `path` MCP
tool (exactly what I did, mid-audit), silently gets `PathMatcher` instead, and — trusting the
compound tool's implicit claim of "this is the one relevant thing" — proceeds with a wrong mental
model of the codebase, with no indication a second, far more central candidate exists.

**Recommendation:** when the top-2 ranked candidates for `understand`'s underlying `locate` call
are within some closeness threshold of each other (or simply: whenever `locate`'s own `kind`
search would have returned >1 result for the bare name), `understand` should either (a) surface
a lightweight caveat/list of runner-up candidates the way `path`'s `NOT_FOUND` caveat already does
for a different failure mode, or (b) bias its own selection toward `is_hub`/`coreness` — CALM
already computes coreness and hub status universally, and defaulting a same-named-candidate tie
toward the structurally-central one would have picked `CalmServer::path` correctly here.

---

### F3 — [CONFIRMED, MEDIUM] `edit_context.blast_radius` doesn't confidence-filter `ambiguous` edges the way its own sibling field `risk_assessment` does

**Evidence anchor:** `crates/calm-server/src/tools/guardrails.rs::edit_context`, lines 157–173 vs
202–251 (read directly this session). `confirmed_caller_count` (feeding `risk_assessment`) is
explicitly computed as `callers.iter().filter(|c| c.edge_confidence != "ambiguous").count()`, with
a code comment describing a *real past incident* this exact filtering was added to prevent
("editing a real zero-caller `#[tool(name = "...")]` MCP handler... bypassed the mandatory
confirm/edit_context gate entirely"). But `blast_radius` (same function, same response object) is
computed via `transitive_bfs(...).entries.iter().map(|e| e.path.clone())` with **no confidence
filter at all** — `TransitiveEntry` carries an `edge_confidence` field (proven by `callers`'s own
`transitive: Option<Vec<TransitiveEntry>>` output, which *does* expose it) but `edit_context`
discards it before it reaches `BlastRadiusInfo`.

**Failure scenario:** for a commonly-named symbol in a language with high ambiguous fan-out
(verified via this repo's own official multi-language benchmark, `benchmarks/resolution/README.md`
— not self-measured: Kotlin 89.6% ambiguous, OCaml 86.3%, C++ 92.5% pre-SCIP, Go 54.3%), an agent
calling the *mandatory* pre-edit tool sees `risk_assessment.level: "low"` (correctly, since that
path filters ambiguous edges) alongside a `blast_radius.files_affected` list padded with files
reached only through name-collision noise — two fields of the same tool response disagreeing
about how big the blast radius is, with no comment or docs explaining the asymmetry.

**Recommendation:** either filter `ambiguous`-confidence entries out of the transitive BFS results
before computing `BlastRadiusInfo` (matching `risk_assessment`'s existing precedent and rationale),
or explicitly split `blast_radius` into `transitive_confirmed`/`transitive_including_ambiguous`
the same way `callers`' top-level response already splits `direct`/`ambiguous`. Either is a small,
well-precedented change — the codebase already has the exact right mental model in `callers`, it's
just not applied consistently to `edit_context`'s `blast_radius`.

---

### F4 — [CONFIRMED, MEDIUM] `path`'s `NOT_FOUND` caveat doesn't distinguish "typo/excluded-path" from "deliberately-unindexed builtin/stdlib/third-party symbol" — and its own suggested fallback produces confidently-wrong results for the latter

**Evidence anchor, live-verified 3-hop chain:**
1. `path(from_symbol="run_indexing_pipeline", to_symbol="println")` → `NOT_FOUND`, caveat: *"likely
   a typo, wrong case, or the file lives in an excluded path (target/, node_modules/, ...) — Run
   search(kind="hybrid") to find the exact name before concluding it doesn't exist."*
2. Following that exact suggestion: `search(query="println", kind="symbol")` → empty (correct —
   `println` is a Rust std macro, never indexed; confirms this is a *permanent*, not a
   typo/exclusion, non-match) — but its own `suggested_next` says *"Try hybrid for broader
   recall,"* nudging further down the same dead end.
3. `search(query="println", kind="hybrid")` → returns `main` (score 1.075) and `doctor`
   (score 0.9754) with no indication these are NOT `println`'s definition — they're merely
   functions whose *body* happens to call `println!`, surfaced via hybrid's semantic/chunk layer
   (verified: `search_symbol`'s FTS only indexes `name`/`docstring`/`signature`/`name_tokens`, per
   direct read of `crates/calm-core/src/db/schema.rs`'s FTS trigger definitions — the match is
   genuinely coming from the embedding layer matching on conceptual similarity, not a naming bug).

An agent that doesn't already know CALM never indexes stdlib/builtins/third-party dependencies
(a real, sensible, intentional scope boundary — see `DEBT-005` in `docs/pattern-debt-registry.yaml`,
same limitation for builtin call *sites*) will follow this chain exactly as designed and land on
two scored, plausible-looking "results" for a symbol that was never going to be found by any
in-index search, with nothing telling it why.

**Recommendation:** `path`/`callers`/`callees`/`edit_context`'s shared `NOT_FOUND` caveat-building
code should special-case common builtin/stdlib naming patterns per language (or, more generally,
whenever the queried name matches nothing in `symbols` *and* nothing in `call_sites` either —
i.e., it never appeared as a call target at all, vs. merely not being resolved to an edge) with
a distinct message: "not found, and never appears as a call target in this index — if this is a
standard-library/builtin/third-party-dependency symbol, CALM does not index those; this is
expected, not a search failure." This is a message-text/caveat-classification change, not a new
capability — cheap relative to the confusion it prevents.

---

### F5 — [CONFIRMED, LOW-MEDIUM] `repo_overview`'s `weak_cross_reference_languages` reports raw match-rate/inserted-count with no sample-size context, inviting misinterpretation on any repo with a small amount of code in that language

**Evidence anchor:** this session's own `repo_overview` call reported `javascript: {last_match_rate:
0.0011, last_inserted: 0}` and `python: {last_match_rate: 0.0857}` for *this* repo. Direct
follow-up (`search(query="*.py", kind="file")`) confirmed this repo has essentially zero real
Python source — 3 tiny test-fixture files plus benchmark-harness scripts — and effectively no
production JS. The near-zero match rates are a sample-size artifact, not a real signal about
CALM's JS/Python cross-reference quality (the official multi-repo benchmark in
`benchmarks/resolution/README.md`, measured on *real* external corpora, is the trustworthy source
for that — see F6). Nothing in the `weak_cross_reference_languages` field itself flags "N files
in this language" or "confidence: low (small sample)" — a maintainer or agent reading this field
on their own mixed-language repo (a Rust monorepo with a handful of incidental Python scripts,
say) could easily misjudge that language's real indexing quality from a statistically meaningless
number. I nearly did exactly this at the start of this audit, before checking.

**Recommendation:** add a file-count (or symbol-count) denominator alongside each language's
match-rate/inserted fields in `weak_cross_reference_languages`, and suppress or visibly flag
low-confidence entries below some minimum sample size (e.g. <10 files) rather than reporting a
percentage that reads as authoritative.

---

### F6 — [DOCUMENTED, not yet actioned — LOW-MEDIUM] `ambiguous` is the dominant resolution tier for most SCIP-less languages, and the one proven mitigation isn't packaged into onboarding

Not a new finding — already measured and documented in this repo's own
`benchmarks/resolution/README.md` (real external OSS corpora, not simulated) — but directly
relevant to this audit's scope since it's the single biggest lever on how useful
`callers`/`callees`/`path`/`dependencies` can be for an agent: C++ (fmt) 92.5% ambiguous with
tree-sitter alone, dropping to 56.6% (and 41.2% formal) once real `scip-clang` is wired in
(measured *today*, 2026-07-28, same day as this audit — see the README's own "Cập nhật 2026-07-28"
section); Kotlin 89.6%, OCaml 86.3%, Go 54.3%, JS 66.7% — all without any SCIP provider at all for
9 of those languages. The README's own conclusion explicitly **deprioritizes** making the
scip-clang recipe easier to adopt ("hạ ưu tiên xuống dưới việc làm cho recipe... dễ dùng hơn cho
user CALM"). Restating it here because this audit's lens (agent-facing accuracy, not resolver
internals) makes the priority call worth revisiting: the `install_hint` text
`repo_overview` surfaces for `c`/`cpp` mentions needing a `compile_commands.json` but not the
`FMT_TEST=ON`-style nuance (build coverage of translation units materially changes match rate —
40,191 fan-out sites ruled out in the real run) that determines whether a user's first attempt at
wiring SCIP actually helps much.

**Recommendation:** low-cost, high-leverage: expand the `c`/`cpp` `install_hint` strings (and/or
a `docs/`-level how-to) with the concrete "make sure your build covers the translation units you
care about" guidance the README already discovered empirically, so a user doesn't have to
independently rediscover it.

---

### F7 — [CONFIRMED via test_gap_hotspots + direct code read, LOW] the MCP-facing wiring layer for `search` has zero direct test coverage, unlike its underlying algorithm

**Evidence anchor:** `test_gap_hotspots` flags `crates/calm-server/src/tools/locate.rs::CalmServer::search`
— `coreness: 8` (max), `test_files: []`. Cross-checked: every `test_search_*` function in the repo
(20+, thoroughly covering `search_symbol`/`search_hybrid`/`search_grep`/`search_file`/
`search_semantic`/`rrf_merge_n`) lives in `crates/calm-core/src/search.rs` — the *algorithm* layer.
None exist in `crates/calm-server/src/tools/locate.rs`, the MCP *handler* layer, which (read
directly this session, lines 16–118) is not a thin passthrough: it does `kind`-string dispatch
with a silent-fallback default, calls `apply_include_tests_filter`/`apply_personalization_boost`,
and builds a 7-branch `suggested_next` fallback decision tree (symbol→hybrid→text→grep, plus
degraded-hybrid special-casing) — exactly the kind of glue logic where an off-by-one or
branch-ordering slip is easy and silent. This *exact seam* — server-layer glue around tool
handlers, as opposed to the well-tested core algorithms underneath — already produced one
confirmed real production bug in this codebase's history: `DEBT-007` (resolved), the rmcp
`#[tool(aggr)]` schema-advertisement bug where 14 of 16 tools advertised `EmptyObject` schemas
despite correct runtime behavior. Same class of risk, currently unguarded for `search`
specifically (and plausibly other handlers — this audit only checked `search`).

**Recommendation:** add a `#[cfg(test)]` suite in `locate.rs` directly exercising `CalmServer::search`
end-to-end (in-process, not just JSON-RPC schema tests) — at minimum: each `kind` value dispatches
to the right underlying function, the `suggested_next` DAG's branches are each hit by a fixture,
and `include_tests`/personalization don't silently misbehave when combined with each `kind`.

---

## What's already working well (don't regress these)

- `callers`/`edit_context`'s `direct`/`ambiguous` split and `confirmed_caller_count` computation
  is careful, well-reasoned, and has a real incident embedded in its own code comments — this is
  the correct model; F3 is really "apply this same model consistently," not "invent a new one."
- `transitive_bfs`'s ambiguous-non-expansion (ADR-0009, `crates/calm-server/src/tools/common.rs`)
  is correctly implemented and evidence-backed ("a depth-3 query returned up to 47% of a whole
  repo" pre-fix, per its own comment) — verified by direct code read, not just trusting the ADR
  doc (per VHEATM's "Documented ≠ Verified" principle).
- The benchmark culture underlying F6 is unusually rigorous for a project this size: real
  external corpora (not synthetic), an explicit "don't hide bad numbers" norm stated in
  `benchmarks/README.md`, and a same-day (2026-07-28) real (not simulated) scip-clang experiment
  run specifically to correct an earlier over-optimistic estimate.

## Hypotheses rejected during this audit (kept visible per Tikai Principle #2, "documented ≠ verified" — showing the check, not just the conclusion)

1. Initially suspected `search_symbol`'s FTS tables might index full code body (would contradict
   the tool's own "name/signature match" contract) — **disproven** by reading
   `crates/calm-core/src/db/schema.rs`'s FTS5 trigger definitions directly: `fts_exact` indexes
   `name, docstring, signature`; `fts_tokens` indexes `name_tokens` only. The misleading `println`
   result (F4) comes from hybrid's semantic/chunk layer, not a body-text leak in FTS.
2. Initially suspected the F1 duplicate-row bug might be global `call_edges` corruption — **disproven**
   by testing a control symbol (`Embedder`, least-churned file in the repo) and finding clean,
   non-duplicated results. Scope is real but currently confirmed-localized, not systemic.

## Suggested next step

F1 is the highest-severity, cheapest-to-triage finding (one diagnostic reindex distinguishes its
two root-cause hypotheses) and the one most directly harmful to agent decision-making (inflated
blast-radius numbers). Recommend it be root-caused and fixed first, with a `docs/pattern-debt-registry.yaml`
entry opened for it regardless of which hypothesis holds. F2–F4 are UX/precision fixes in
well-understood, already-well-patterned code (each has a sibling mechanism elsewhere in the same
file to copy). F5–F7 are lower-urgency but cheap.

---

# ROUND 2 — Meta-audit (auditing the round-1 audit above)

Re-ran deep verification on 2026-07-28, this time querying `.calm/index.db` directly with `sqlite3`
(a capability round 1 never used — it relied entirely on tool *output*, which is exactly why it got
some things wrong). Several round-1 conclusions were **incorrect or incomplete**. Corrections below,
most important first. This section supersedes the corresponding round-1 claims where they conflict.

## C1 — F1's root cause: CONFIRMED, and it was NEITHER of my two round-1 hypotheses

Round 1 left F1 as "2 competing, un-adjudicated hypotheses" (incremental-reindex delete gap; or a
live AST-shape dedup bug). **Both were wrong.** Direct DB + code investigation pins it exactly:

**The duplication is 100% in `formal_source='scip'` edges.** Measured on this repo's live DB:
- `call_edges`: 26,733 rows, only 10,103 distinct `(from,to,line,kind)` tuples → **~62% of the whole
  table is duplicate rows.**
- Broken down by confidence: `formal` 24,435 rows → 7,805 distinct (~68% dup). **Every other tier is
  perfectly clean**: ambiguous 1230=1230, textual 745=745, resolved 307=307, inferred 16=16. Max
  copies-per-tuple across all non-formal tiers = **1**.
- All 24,435 formal rows are `formal_source='scip'` (zero `stack_graphs`).
- Copies-per-tuple histogram is multi-modal, clustering at ~24–28, ~52–54, and ~75–81 — i.e. **dup
  factor ≈ number of SCIP overlay runs since that file's edges were last full-cleared.**

**Mechanism (code-confirmed):** the SCIP overlay (`crates/calm-server/src/scip_overlay.rs` →
`calm_core::scip::run_overlay` → `ingest_occurrences`, `insert_missing=true`) is a *separate
background pass* from the main index. It has **no pre-clear of prior scip edges**, and there is
**no UNIQUE constraint on `call_edges`** (confirmed via `sqlite_master`: only three non-unique
indexes `idx_call_edges_from/to/fpath`). Its only dup guard is `insert_missing_edges`'s within-run
`already_represented` set (`crates/calm-core/src/scip/ingest.rs:319-327`), keyed on
`(from_path, call_line, def_path, def_line)` where `def_path`/`def_line` come from a **JOIN to
`symbols.line_start`** (`ingest_occurrences` SELECT at line 107). But an inserted edge's
`to_symbol` is resolved via `resolve_unique_symbol_at(def_path, def_line)` = the *narrowest
enclosing symbol* at the SCIP def-occurrence line — whose `line_start` is **not** generally equal
to that occurrence line. So on the next overlay run the prior insert reloads under key
`(…, to_sym.line_start)` while the check looks it up under `(…, scip_occurrence_line)` — **key-space
mismatch → the edge is not recognized as already present → re-inserted.** One extra copy per overlay
run, for every edge whose target's `line_start` differs from its SCIP def-occurrence line. (Edges
where they coincide — 6,554 tuples with exactly 1 copy — never duplicate; that's the histogram's
n=1 bucket.)

The `incremental_graph_update` path I *suspected* in round 1 is, on reading it
(`pipeline.rs:1140-1268`), actually **correct** — it does a scoped `DELETE FROM call_edges WHERE
from_path IN (delta_paths)` before re-inserting. It's not the culprit. My round-1 hypotheses
pointed at the wrong subsystem entirely.

## C2 — F1's scope claim was WRONG: it is systemic, not localized. My "Hypothesis rejected #2" was a false rejection.

Round 1 explicitly concluded (in "Hypotheses rejected", item 2): *"suspected... global call_edges
corruption — disproven by testing a control symbol (Embedder)... Scope is real but currently
confirmed-localized, not systemic."* **This is false.** ~62% of the entire table is duplicated. The
`Embedder` control (`embedding.rs`, churn 4 — the least-changed file) came back clean precisely
*because* it's rarely reindexed and its scip edges were recently cleared — which, far from ruling
out the accumulation theory, is **direct positive evidence FOR it** (low-churn ⇒ few overlay
re-inserts ⇒ clean; high-churn hot files ⇒ 75–81 copies). I had the confirming datapoint in hand
in round 1 and misread it as a refutation. This is the single worst analytical error in the
round-1 audit — a control chosen without controlling for the actual causal variable (reindex
frequency).

## C3 — F1's impact statement was partly WRONG, in the code's favor: stored structural metrics are NOT corrupted

Round 1 implied `callers`/`edit_context` blast-radius signals are broadly corrupted. Precise truth:
- `refresh_caller_counts` (`pipeline.rs:1287`) uses `COUNT(DISTINCT from_symbol)` — so
  `symbols.caller_count` is **immune** to the row duplication. Verified live: `PathMatcher` has
  `caller_count=3, is_hub=0, coreness=3` stored, despite 113 raw caller rows. So **`hotspots`,
  `test_gap_hotspots`, `repo_overview.core_symbols`, `is_hub`/`coreness` rankings — all of which
  read the stored column — are NOT corrupted by this bug.** Round 1's worry that they were is
  withdrawn.
- BUT the corruption is real in the **live per-request tool responses that iterate raw
  `call_edges` rows**: `edit_context` computes `confirmed_caller_count = callers.iter().filter(non-
  ambiguous).count()` (a raw `.count()`, not distinct). Verified live: **`edit_context(PathMatcher)`
  reports `risk_assessment.level: "high"` for a symbol with 3 real callers** (true count → "low").
  The MANDATORY pre-edit safety tool actively misinforms. `callers.direct_count` (113 vs true 3) and
  its "high blast radius — verify before modifying" `suggested_next` are inflated the same way, plus
  the 100+-line all-identical output is pure token waste. This is F1's real, sharpened impact:
  **not the stored graph, but every live risk/caller signal an agent reads at edit time.**

## C4 — Pattern-Globalization MISS: `import_edges` has the same class of bug (round 1 never checked sibling tables)

VHEATM Principle #1 ("1 bug = grep globally") demands checking whether a found bug's *pattern*
recurs. Round 1 found the `call_edges` duplication and stopped there. Checking the sibling
derived-edge table: **`import_edges` also has fully-identical duplicate rows** — 584 total vs 533
distinct, 33 byte-identical duplicates (e.g. `embedding.rs → rusqlite` with `symbols_used=["Connection"]`
appears **9×**). This path doesn't go through SCIP at all, so it's an *independent* second instance
of the same root pattern: **CALM's derived-edge tables have no UNIQUE constraints and rely on
delete-before-insert discipline that is not uniformly enforced across every insert path.** (`call_sites`,
by contrast, is ~98% clean — 23,659 vs 23,186 distinct — so this isn't universal, but the two
*edge* tables both have it.) The correct framing is a single systemic finding, not two unrelated ones.

## C5 — F2 was UNDER-STATED: the real defect is in core search ranking, not `understand`'s convenience-collapse

Round 1 framed F2 as "`understand` silently mis-picks among ambiguous candidates." Direct test of the
layer beneath it shows the problem is deeper and worse. `locate("path", kind=symbol)` top-10 returns
`PathMatcher, PathCache, normalize_path, resolve_path, PathResult, PathStep`, and two Python
benchmark functions (matched via docstring/signature) — **and does NOT contain `CalmServer::path`
at all.** Verified via DB: `CalmServer::path` has `name='path'` (exact, and the **only** symbol in
the entire index whose name is exactly `path`), `name_tokens='path'`, `caller_count=83`,
`coreness=9` (**the highest coreness in the whole repo**). So the single exact-whole-name, unique,
maximally-central match is displaced off the first page by camelCase-token matches (`Path` inside a
longer name) and even unrelated docstring hits. `search_symbol`'s `rank_multiplier` scores on
`(path, is_test, churn_score)` and **ignores `coreness` entirely** — throwing away the one signal
that would have surfaced the right symbol. `understand` picking `PathMatcher` is just a *symptom*
of `locate`/`search` ranking it #1. **This is a precision failure at the most fundamental level of
the toolchain being audited: "find the symbol I named" doesn't reliably return an exact-name hub on
page one.** Recommended fix now has a concrete lever: fold `coreness` (already computed and stored)
and an exact-whole-name-match bonus into `rank_multiplier` / the BM25 weighting, and/or rank an
exact `name == query` match decisively above token-substring matches.

## What round 1 got RIGHT (re-verified, unchanged)

- **F3** (blast_radius doesn't filter `ambiguous` like risk_assessment does) — still valid by code
  read; narrow refinement: only depth-1 ambiguous neighbors leak in, since `transitive_bfs` reports
  but doesn't *expand* ambiguous edges (ADR-0009), so the pollution is bounded to the first hop.
- **F4** (builtin/stdlib `NOT_FOUND` caveat + misleading hybrid fallback) — still valid, was
  live-reproduced.
- **F5** (weak_cross_reference no sample-size context) — still valid.
- **F6** (ambiguous is the dominant tier; scip onboarding under-documented) — still valid; from the
  repo's own real-corpus benchmark.
- **F7** (search MCP handler has no direct test) — **re-confirmed**: the js-client-interop test
  (`tests/js_client_interop/client.mjs`) only drives `repo_overview`, not `search`; no other
  integration test exercises the `search` dispatch/`suggested_next` logic.

## Revised priority

1. **The derived-edge duplication pattern (C1–C4)** — highest severity. One fix covers the class:
   add `UNIQUE(from_symbol, to_symbol, call_site_line, edge_kind)` to `call_edges` and an analogous
   constraint to `import_edges` (with `INSERT OR IGNORE` / `ON CONFLICT`), as defense-in-depth that
   makes *every* insert path idempotent regardless of per-path delete discipline — plus fix
   `insert_missing_edges`'s dedup key mismatch as the specific root cause, plus a one-off cleanup
   migration for existing `.calm/index.db` files (this repo's own included — it currently carries
   ~16.6k redundant call_edges rows). Ship with a regression test seeded from the `boundaries.rs::
   PathMatcher` repro. Note: because the stored `caller_count` uses `COUNT(DISTINCT)`, the
   user-visible payoff is specifically correct `edit_context` risk + `callers` counts + big token
   savings, not a change to hub/coreness rankings (those were already correct).
2. **Search ranking (C5)** — high severity, core-precision. `rank_multiplier` should use `coreness`
   and reward exact whole-name matches.
3. F3, F4, F5, F6, F7 — unchanged from round 1.

## Meta-lesson for future CALM self-audits

Round 1's errors (C2 false rejection, C1 wrong root cause, C3 over-broad impact, C4 sibling miss)
share one cause: **it audited tool *output* without ever querying the underlying `.calm/index.db`.**
The tool output was itself distorted by the very bug under investigation (113 duplicated rows),
which is precisely how the duplication masqueraded as an `understand`-assembly issue and how the
control-symbol test misled. For any CALM self-audit touching data-layer correctness, direct SQL
against the index DB is not optional — the tools cannot be trusted as ground truth *about
themselves*.
