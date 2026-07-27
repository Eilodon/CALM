---
title: Martin/OOD metrics, ownership-entropy risk signal, and churn-aware search ranking
date: 2026-07-27
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Scope

Three upgrades, ordered by dependency (not by the originally proposed order):

- **#1** Martin/OOD metrics (Ca/Ce/I/A/D) → `fitness_report` / `thresholds.toml`
- **#2** Ownership-entropy signal → `edit_context.risk_assessment`
- **#3** Route coreness/hotspot/churn into `search` ranking

Item #4 (cross-encoder rerank) is explicitly out of scope — see the prior
research note; it stays last and needs its own spec.

---

## Verified ground truth

Every claim below was checked against the working tree at `d1fd271`
(v0.3.6) with CALM's own tools or a direct measurement — not inferred.

### Data layer

| Fact | Evidence |
|---|---|
| `import_edges(id, from_path, to_path NULL-able, module_name, symbols_used)`; indexed on both `from_path` and `to_path` | `crates/calm-core/src/db/schema.rs:47-56` |
| No unique constraint on `(from_path, to_path)` — **13 duplicate pairs exist today** | measured: `GROUP BY from_path,to_path HAVING COUNT(*)>1` → 13 |
| `symbols` has `caller_count`, `coreness`, `is_hub`, `is_test`, `cyclomatic_complexity` — **no churn/hotspot column anywhere in the schema** | `schema.rs:4-25` |
| Column-add migration pattern: `migrate_add_column(conn, "symbols", "coreness", "INTEGER")` | `schema.rs:229-237` |

### Git layer — the finding that reshapes #2 and #3

`analysis::git_log::commits_with_files(project_root, since)` already runs
**one repo-wide** `git log --since=<since> --name-only --format='|||%ae|||%aI'`
and returns `(Vec<GitCommit{author, date, files}>, git_available)`
(`crates/calm-core/src/analysis/git_log.rs:20-55`). It is a hub with two
production callers:

- `analysis::cochange::compute_co_changes` (cochange.rs:41) — cached in the
  server layer
- `analysis::hotspot::collect_git_churn` (hotspot.rs:307) — **uncached**

`ChurnInfo` already carries `authors: HashSet<String>` — the author identity
per file is collected today, but as a *set*, so per-author commit counts
(what Shannon entropy needs) are discarded.

**Measured on this repo (482 commits in a 90-day window):**

| Approach | Time | Scaling |
|---|---|---|
| One repo-wide `git log` (what `commits_with_files` runs) | **208 ms** | O(1) in file count — covers every file |
| Per-file `git log -- <path>` × 20 files | **801 ms** | O(N) in file count |

The per-file design in the original #2 plan is ~4× slower at 20 files and
gets linearly worse; the repo-wide pass already in the tree is strictly
better and needs no `--follow` risk analysis at all.

### Cache layer

`CalmServer::co_changes_cached` (`tools/common.rs:193-222`, TTL 60s) is a
**single-slot** cache — `Option<(key, Instant, result)>`, compared with
`*cached_key == key`, not a map. Alternating between two files thrashes it
100%. Copying it verbatim for a *per-file* signal inherits that flaw; for a
*repo-wide* signal keyed only by `since`, single-slot is exactly right.

### Ranking layer

`noise_multiplier(path, is_test)` (`search.rs:119-125`) is applied as a
score multiplier at **two** sites, not one:

- `search.rs:213` — inside `search_symbol` (`kind="symbol"`, `"text"`)
- `search.rs:986` — inside `rrf_merge_n` (`kind="hybrid"`)

`rrf_merge_n` uses `BTreeMap` specifically so ties break deterministically
(comment at `search.rs:965-971`).

### Risk layer

`risk_level_from_caller_count` (`tools/common.rs:2175-2183`) is shared by
**three** consumers, and one of them blocks writes:

- `edit_context` (`guardrails.rs:206`) — advisory string
- `diff_impact` (`guardrails.rs:529`) — advisory struct
- `compute_touch_risk` (`edit.rs:1732`) → `edit_lines_impl_gated`
  (`edit.rs:848`, `edit.rs:1291`) — **the write gate**

`EditContextOutput.risk_assessment` is `Option<String>` (`guardrails.rs:737`)
— a bare level, no reasons. `diff_impact` already uses the richer
`RiskAssessmentOutput { level, reasons }` (`guardrails.rs:554`).

Toolsnaps capture **both** `inputSchema` and `outputSchema`
(`crates/calm-server/src/__toolsnaps__/*.snap`, 29 files), so any output
shape change is a reviewed snapshot diff.

### Fitness layer

`FitnessMetrics` has 8 fields (`fitness.rs:170-188`); `boundary_violations`
and `config_drift_count` are added at the `run_fitness_check` layer
(`fitness.rs:441-452`), giving the 10 numeric metrics observed in
`fitness_report`.

Every `FitnessCheckItem` carries `passed: bool` (`fitness.rs:420-426`) —
**there is no report-only concept**. `thresholds.toml` states outright that
numeric thresholds keep `FitnessThresholds::default()`; only `[[boundaries]]`
and `[config_drift]` are declared in the file.

### Architecture boundary — hard constraint on #3

`thresholds.toml` declares, and `max_boundary_violations: 0` enforces:

```toml
[[boundaries]]
from = "crates/calm-core/src/indexer/"
to   = "crates/calm-core/src/analysis/"
```

`compute_coreness` is written into `symbols` from `indexer/pipeline.rs`
(`rebuild_graph:1054`, `incremental_graph_update:1211`) and lives in
`crates/calm-core/src/graph/coreness.rs` — under `graph/`, **not**
`analysis/`. That is precisely why it does not trip the rule.

`check_boundaries` reads `import_edges` directly, so it flags **direct**
imports only, not transitive paths.

### Coverage — the measurement that reshapes #1

| Population | Files | With ≥1 resolved import edge | Isolated |
|---|---|---|---|
| All indexed files | 214 | 96 (45%) | **118 (55%)** |
| `crates/**` Rust only | 97 | **85 (88%)** | 12 |

`import_edges`: 564 rows, 198 with non-NULL `to_path`, 119 distinct
`from_path`, 47 distinct `to_path`.

A repo-wide average of I or D over the full 214-file population would be
computed over 55% structurally-undefined zeros. That is not a threshold
tuning problem — it is a wrong-denominator problem that produces a
confident, meaningless number.

---

## Corrections to the input plan

| # | Input plan said | Verified reality | Correction |
|---|---|---|---|
| 1 | #2: per-file `git log --format='%an' -- <path>` | `commits_with_files` already does one repo-wide pass with `%ae` | Derive entropy from the shared repo-wide pass; never shell out per file |
| 2 | #2 and #3 need a shared cache module, "decide at plan time" | Both already funnel into `commits_with_files` | Cache **at `commits_with_files`** in calm-core — one change serves cochange, hotspot, entropy, and churn |
| 3 | #3: cache in `tools/common.rs`, blend in `rrf_merge_n` | `common.rs` is calm-server; `search.rs` is calm-core; calm-core cannot depend on calm-server | **Structurally impossible as written.** Persist churn as a `symbols` column instead |
| 4 | #3: blend into `rrf_merge_n` | `noise_multiplier` is applied at `search.rs:213` **and** `:986` | Patch both, or `kind="symbol"` and `kind="hybrid"` rank inconsistently |
| 5 | #2: "nâng risk level" | `risk_level_from_caller_count` also feeds the blocking write gate | Adjust **only** `edit_context`'s local string; never the shared function |
| 6 | #2: add a reason string | `risk_assessment` is `Option<String>` | Requires an output-schema change + `edit_context.snap` update; align to the existing `RiskAssessmentOutput` shape |
| 7 | #1: "ship report-only first" | No report-only mechanism exists | D ∈ [0,1] by construction → default threshold `1.0` is un-failable by definition. Zero new machinery |
| 8 | #1: I = Ce/(Ce+Ca), "guard chia 0 (file cô lập)" | 55% of indexed files are isolated | Division-by-zero is the **majority case**, not an edge case. Scope the population and report coverage |
| 9 | #1: module at `analysis/martin.rs` | Read-only from DB, consumed by `fitness.rs` (crate root) | Correct as proposed — no boundary issue, since nothing under `indexer/` imports it |
| 10 | #3: churn pass placement | `indexer/` → `analysis/` is a declared, zero-tolerance violation | Churn-persistence pass must live under `graph/`, mirroring `graph/coreness.rs` |

---

## Design

### Phase 0 — shared git-signal foundation (blocks #2 and #3)

**T0.1 — Relocate the git primitive.** Move `analysis/git_log.rs` to
`crates/calm-core/src/git.rs` (crate root, alongside `search.rs` and
`fitness.rs`). Rationale: it is a subprocess wrapper, not "derived
intelligence". After T0.2 the indexer pipeline needs it, and leaving it
under `analysis/` would make `indexer/ → analysis/` the honest description
of the dependency even though `check_boundaries` only sees direct edges.
Two production call sites to update.

**T0.2 — Cache it.**

```rust
// crates/calm-core/src/git.rs
pub fn commits_with_files_cached(project_root: &Path, since: &str)
    -> (Arc<Vec<GitCommit>>, bool)
```

Process-wide `RwLock<Option<((PathBuf, String), Instant, Arc<Vec<GitCommit>>)>>`,
TTL 60s, mirroring `co_changes_cached`'s discipline. Single-slot is correct
here: the key is `(project_root, since)` and `since` is effectively constant
within a session. Return `Arc` so cochange/hotspot/entropy share one
allocation instead of cloning a 482-commit vector three times.

**T0.3 — One derivation pass.**

```rust
// crates/calm-core/src/git.rs
pub struct FileGitSignals {
    pub commit_count: u32,
    pub author_commits: HashMap<String, u32>,  // %ae → count
    pub last_changed: Option<String>,
}
pub fn file_signals(commits: &[GitCommit]) -> HashMap<String, FileGitSignals>;
pub fn ownership_entropy(s: &FileGitSignals) -> Option<f64>;  // None if commit_count < 2
```

`ownership_entropy` = Shannon over `author_commits` normalised by
`ln(distinct_authors)` so the result is in `[0,1]` and comparable across
files with different author counts. `None` below `default_min_churn = 2`
(`config.rs:748`), reusing the floor `hotspot.rs` already applies.

`collect_git_churn` is refactored to build `ChurnInfo` from `file_signals`
rather than its own loop — one derivation, two consumers, no possibility of
the two disagreeing. This closes the cross-constraint the earlier audit
flagged (#2 and #3 producing different answers for the same file) by
construction rather than by convention.

**T0.4 — Preserve graceful degrade.** `git_available: false` must propagate
unchanged. Copy the `hotspot.rs:127-135` pattern verbatim; `edit_context` is
a mandatory gate, and an error there blocks all editing.

### #1 — Martin/OOD metrics

**T1.1 — `crates/calm-core/src/analysis/martin.rs`.** Pure DB reads:

```sql
-- Ce (efferent): what this file depends on
SELECT from_path, COUNT(DISTINCT to_path) FROM import_edges
 WHERE to_path IS NOT NULL GROUP BY from_path;
-- Ca (afferent): what depends on this file
SELECT to_path,   COUNT(DISTINCT from_path) FROM import_edges
 WHERE to_path IS NOT NULL GROUP BY to_path;
```

`DISTINCT` is load-bearing — 13 duplicate pairs exist today.

**T1.2 — Population scoping (the fix for the 55%-isolated finding).**
A file enters the measured population only if `Ca + Ce > 0`. Report
`files_measured` and `files_total` alongside every aggregate so the
denominator is never invisible. `I = Ce / (Ce + Ca)` is then always defined;
no zero-guard branch is reachable.

**T1.3 — Abstractness `A`, language-gated.** `A` = share of a file's
symbols that are abstract. Ship exactly three tiers:

| Tier | Languages | `A` definition |
|---|---|---|
| Supported | Rust, Java, Kotlin, C#, TypeScript, Go, PHP, Scala, Swift | trait/interface/abstract-class symbol kinds ÷ total type symbols |
| Not applicable | Python, JavaScript, Ruby, Lua, Elixir, shell, … | `A = None` → **`D` is not emitted**, only `Ca`/`Ce`/`I` |
| Unknown | everything else | `A = None`, same as above |

`D = |A + I − 1|` is emitted only when `A.is_some()`. Reporting `D` for a
duck-typed language would be a fabricated number, not a conservative one.
This is the honest resolution of the MED-HIGH failure mode the earlier audit
raised, and it costs one `Option`.

**T1.4 — Wire into fitness.** Add to `FitnessMetrics`:
`avg_instability: f64`, `avg_distance: Option<f64>`,
`martin_files_measured: i64`, `martin_files_total: i64`.
Add to `FitnessThresholds`: `max_avg_distance: f64` with
**`default() = 1.0`**.

Because `D = |A+I−1| ∈ [0,1]` by construction, a threshold of `1.0` can
never fail — the metric reports its true value in `fitness_report` and in CI
output while being structurally incapable of turning CI red. Tightening to a
real bound after baselining is then a single-number change in
`FitnessThresholds::default()`, reviewable in isolation. No `report_only`
field, no new machinery, no toolsnap churn beyond the added output fields.

**T1.5 — Cross-language regression fixture.** Extend
`crates/calm-core/tests/fixtures/multi_lang_workspace` with a known
Ca/Ce/I shape and assert exact values. This is the only mechanism that
catches a silent cross-language resolution regression — the freshly-fixed
import resolution (86.8% → 99.6%, commit `d1fd271`) had accumulated four
defects precisely because nothing measured `import_edges.to_path`.

### #2 — Ownership entropy in `edit_context`

**T2.1 — Consume, don't recompute.** In `guardrails.rs::edit_context`, call
`CalmServer::git_signals_cached(since)` (thin server-side wrapper over
`git::commits_with_files_cached` + `file_signals`), look up the target
file, and take `ownership_entropy`. No new git subprocess; the same cached
pass `co_changed_files` already needs in the same handler.

**T2.2 — Escalate locally only.** Compute the base level via
`risk_level_from_caller_count` exactly as today, then apply a local
adjustment inside `edit_context`:

- low entropy (single dominant author) **and** low commit count →
  escalate `low` → `medium`, reason: `"single-author file (low bus factor)
  — no second reviewer has context here"`
- otherwise unchanged

`risk_level_from_caller_count` itself is **not** modified. Its third
consumer, `compute_touch_risk` → `edit_lines_impl_gated`, is the blocking
write gate; entropy is an advisory signal about reviewer coverage and must
not acquire the power to refuse an edit.

**T2.3 — Output shape.** Change `EditContextOutput.risk_assessment` from
`Option<String>` to the existing `Option<RiskAssessmentOutput>`
(`{ level, reasons }`) already used by `diff_impact`. This unifies two
divergent shapes rather than adding a third. Update `edit_context.snap`;
flag the change in the PR body as an intentional output-schema change, since
`outputSchema` is client-visible.

**T2.4 — Tests.** Zero commits; single author; `git_available: false`;
entropy stable across two calls within TTL; escalation fires only in the
intended quadrant; a `high`-risk symbol is never *de*-escalated by entropy.

### #3 — Churn in search ranking

**T3.1 — Persist churn, do not cache it (order matters — this is first).**

- `migrate_add_column(conn, "symbols", "churn_score", "REAL")` — nullable,
  same shape as the `coreness` precedent.
- New `crates/calm-core/src/graph/churn.rs::update_churn_scores(tx, project_root)`
  — under `graph/`, mirroring `graph/coreness.rs`, so the pipeline call
  does not trip the `indexer/ → analysis/` boundary.
- Call it from `rebuild_graph` (`pipeline.rs:1054` block) and
  `incremental_graph_update` (`pipeline.rs:1211` block), in the same
  "global metric passes" group as `compute_coreness` /
  `update_is_hub_flags` / `update_boundary_ambiguous_flags`. The comment at
  `pipeline.rs:1205-1208` requires these to stay identical in both paths —
  add to both or golden-parity breaks.
- Normalise to `[0,1]` at write time so the ranking read is a plain column
  read with no repo-wide max lookup.
- `git_available: false` → write `NULL`, never `0.0`. `NULL` means "unknown",
  `0.0` means "measured, never changed"; conflating them would silently
  de-rank an entire repo the moment git is unavailable.

Persisting rather than caching is what makes this work at all: `search.rs`
lives in calm-core and cannot reach a calm-server cache. It also makes
ranking **deterministic by construction** between reindexes — the HIGH
failure mode from the earlier audit stops being a thing to test around,
because there is no live git call in the query path to be non-deterministic.

**T3.2 — Blend at both multiplier sites.** Add `s.churn_score` to the two
`SELECT` lists feeding `RawRow`, then extend the existing multiplier:

```rust
fn rank_multiplier(path: &str, is_test: bool, churn: Option<f64>) -> f64 {
    if is_test || is_noisy_path(path) { return NOISE_PENALTY; }   // unchanged, first
    1.0 + CHURN_WEIGHT * churn.unwrap_or(0.0)
}
```

Applied at `search.rs:213` **and** `search.rs:986`. The `is_test` /
noisy-path branch returns **before** any churn boost — a test file can never
be lifted by churn. This is the non-negotiable guard: case **S3** in
`docs/test/calmrootcauseandfixes20260709.md` is a real, already-shipped bug
where a test outranked the implementation it tested, and `hotspot.rs` /
`orient.rs` already exclude `is_test` from architectural-importance signals
for exactly this reason.

`CHURN_WEIGHT` starts at `0.15` — bounded so churn can reorder near-ties
without overturning a clear relevance win, which contains the feedback-loop
failure mode (rank ↑ → edits ↑ → churn ↑ → rank ↑) to a range where
relevance still dominates.

**T3.3 — Tests.**
- Determinism: identical query twice with a simulated background reindex in
  between → identical ordering (the invariant
  `test_search_symbol_ties_break_deterministically_by_qualified_name`
  protects).
- **S3 as a permanent regression test**: the
  `is_lfs_pointer_stub_detects_pointer_text_not_real_weights` shape — a
  high-churn test file paraphrasing the query must not outrank the real
  implementation. Place in `search.rs` unit tests (fast, runs on every CI
  job) rather than `benchmarks/b3_search_quality/`.
- `churn_score IS NULL` for every row → ranking identical to today's
  baseline (proves the no-git path is a true no-op).

---

## Execution order

```
T0.1 → T0.2 → T0.3 → T0.4        (shared foundation; blocks #2 and #3)
  ├── #1  T1.1 → T1.2 → T1.3 → T1.4 → T1.5   (independent — can run in parallel with T0)
  ├── #2  T2.1 → T2.2 → T2.3 → T2.4
  └── #3  T3.1 → T3.2 → T3.3
```

#1 has no dependency on Phase 0 and can be built first or concurrently.
#2 and #3 must not start before T0.3, or they will re-introduce the
divergent-signal problem the foundation exists to prevent.

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-27 | trigger: NORMAL -->

**Tier:** 2 (Production) | **Date:** 2026-07-27

Not Tier 3: no PII, payments, multi-tenancy, or regulated data. Tier 2
because `edit_context` is a mandatory gate on every write path and `search`
is the primary navigation surface — both are load-bearing for correctness of
downstream agent behaviour.

### Failure Modes

1. **Churn column written by the indexer trips the repo's own boundary
   gate.** — HIGH — mitigation in plan: **YES** (T3.1). `thresholds.toml`
   forbids `indexer/ → analysis/` at `max_boundary_violations: 0`. Placing
   `update_churn_scores` in `analysis/` and calling it from
   `pipeline.rs` fails `ci fitness-check` on the first CI run. The design
   enables this because the natural home for a churn function *reads* like
   `analysis/`, and `analysis/hotspot.rs` already computes churn.
   Mitigated by placing it under `graph/`, mirroring the `graph/coreness.rs`
   precedent that already writes to `symbols` from the same pipeline slot.

2. **Entropy escalation silently acquires blocking power over edits.** —
   HIGH — mitigation in plan: **YES** (T2.2).
   `risk_level_from_caller_count` has three consumers and one of them
   (`compute_touch_risk` → `edit_lines_impl_gated`) refuses writes. The
   design enables this because "raise the risk level" reads as a single
   change, and the shared helper is the obvious place to make it. A
   single-author file would start being *refused* rather than *flagged* —
   a hard stop on a repo where most files have one author, which is most
   young repos. Mitigated by confining the adjustment to `edit_context`'s
   local string and asserting in T2.4 that the gate path is untouched.

3. **Martin aggregates computed over a 55%-undefined denominator.** —
   MED-HIGH — mitigation in plan: **YES** (T1.2). Measured: 118 of 214
   indexed files have no import edge at all. Averaging I or D across the
   full population yields a confident number dominated by structural zeros,
   which then gets a threshold attached to it. The design enables this
   because "guard against division by zero" frames isolation as an edge
   case, and the natural aggregate is `AVG()` over all files. Mitigated by
   restricting the population to `Ca + Ce > 0` and emitting
   `files_measured` / `files_total` next to every aggregate.

### Layer Signals

- **L1 Logic** — `D = |A + I − 1|` is emitted only when `A.is_some()`. The
  untested branch is the *dynamic-language* path, where `A` is `None` and
  `D` must be absent rather than defaulted. A `.unwrap_or(0.0)` here would
  silently report `D = |0 + I − 1|` — a plausible-looking number with no
  meaning. T1.3 must assert absence, not a value.

- **L2 Concurrency** — the Phase 0 cache is a process-wide `RwLock` read on
  `edit_context` (mandatory, per-edit) and on the background reindex
  (`update_churn_scores`). Under the shared daemon (ADR-0005) these are
  genuinely concurrent across sessions. The `Arc<Vec<GitCommit>>` return is
  what keeps a reader from holding the lock during derivation; returning
  `Vec` by clone under the read guard would serialise every `edit_context`
  in every attached session behind one git pass.

- **L3 Data** — three nullable column adds
  (`symbols.churn_score`, plus #1's fields on `FitnessMetrics`, which is
  in-memory only). `migrate_add_column` is idempotent and already proven for
  `coreness`. The real data risk is semantic, not structural:
  `churn_score = NULL` (git unavailable) vs `0.0` (measured, no churn) must
  stay distinct — T3.1 specifies this, and T3.3 tests the all-NULL case
  reproduces today's ranking exactly.

- **L4 Integration** — the external dependency is the `git` binary, not a
  network service. Failure modes are absence (`git_available: false`,
  already handled repo-wide) and *slowness on a large history*, which the
  208 ms measurement here does not bound. See Abductive 2.

- **L5 Security** — no new signal leaves the process. `%ae` (author email)
  is already collected by `commits_with_files` today and already surfaces in
  `hotspots` output via `ChurnInfo.authors`. Entropy is a scalar derived
  from it and is strictly less identifying than what already ships. No new
  exposure; no change to `sanitize_source_output`'s remit.

- **L6 Observability** — the metric most likely to break silently is #1's
  `avg_instability`, because a cross-language import-resolution regression
  moves it smoothly rather than breaking it. There is no alarm for "this
  number is now wrong". T1.5's fixture with exact asserted values is the
  only detector; without it, the four defects just fixed in `d1fd271` are
  the precedent for how long such a regression can live unnoticed.

- **L7 Cross-cutting** — no rate limits, no idempotency concern (all
  computations are pure functions of git state + DB state), no regulated
  data. Not flagging L7.11.

### Assumptions to Verify

- **ASSUMED** — "`CHURN_WEIGHT = 0.15` is bounded enough that relevance
  still dominates." Chosen by reasoning about the multiplier's range against
  `NOISE_PENALTY = 0.6`, not measured. The T3.3 S3 test is the falsifier;
  if it fails, lower the weight rather than special-casing the test.

- **ASSUMED** — "60s TTL is fresh enough for entropy." Inherited from
  `co_changes_cached`'s existing reasoning (git history only changes on a
  new commit). Holds for entropy identically. Stated rather than re-derived.

- **ASSUMED** — "`%ae` is a stable author identity." Email is more stable
  than `%an` but still splits on `user@laptop` vs `user@ci`. Entropy will
  read slightly high for contributors with multiple emails. Acceptable for
  an advisory signal; worth a one-line note in the reason string rather than
  a `.mailmap` integration.

- **DEFERRED (explicit)** — the tightened `max_avg_distance` value. The plan
  ships `1.0` (un-failable) deliberately; picking the real bound requires
  baselining across several real repos and is out of scope here. This is a
  deferral with a mechanism attached, not a TBD.

### Abductive Hypotheses

**Abductive 1 — the un-failable threshold becomes permanent by omission.**
Every component behaves correctly: `max_avg_distance = 1.0` reports the true
value and never fails CI, exactly as designed. But a threshold that has
never once gone red is indistinguishable from a threshold that is working,
and nothing in the plan schedules the tightening. The metric ships, appears
in `fitness_report`, is read by agents as a health signal, and quietly
guarantees nothing. This is not caught by the pre-mortem because no
individual step is wrong — the failure is the *absence* of a later step.
**Countermeasure:** record the tightening as a `PATTERN-DEBT` entry in
`docs/pattern-debt-registry.yaml` at the same commit that adds the
threshold, not afterward. The registry is the repo's existing mechanism for
exactly this, and it is already known to drift stale — so the entry must
carry the concrete baseline-then-tighten trigger, not "revisit later".

**Abductive 2 — the foundation's own success hides its cost at scale.**
Phase 0 makes one repo-wide `git log` serve four consumers, so the obvious
metric (subprocess count) improves. But `commits_with_files` holds the
entire parsed history in memory: 482 commits here, and the parse is
`String::from_utf8_lossy` over the full `--name-only` output. On a repo with
100k commits and deep file-lists, that single call allocates hundreds of MB
and takes seconds — and now *four* code paths, including the mandatory
per-edit gate, block on the first one to trigger it after each TTL
expiry. The current design has each consumer paying its own smaller cost;
consolidation converts many small costs into one large synchronised stall.
The 208 ms figure measured here (482 commits, 13-day-old repo) says nothing
about this. **Countermeasure:** before shipping #2 or #3, run Phase 0
against a real large repo (Linux kernel or equivalent, ≥50k commits) and
measure both wall time and peak RSS of a single `commits_with_files_cached`
call. If it exceeds ~1 s or ~200 MB, cap the window (`--since` default
tightening) or switch to a streaming parse before wiring it into
`edit_context`. This is a **gating measurement, not a follow-up**.

### Gate Result

**PASS WITH FLAGS** — proceed to `writing-plans`.

Both HIGH findings have concrete mitigations already folded into the design
(T3.1 placement under `graph/`; T2.2 local-only escalation). The MED-HIGH
denominator finding is mitigated by T1.2.

Two conditions attach to the flags:

1. **Abductive 2 is a gate, not advice.** The large-repo measurement must
   run and pass before T2.1 or T3.2 land. #1 is unaffected and may proceed
   regardless.
2. **Abductive 1's PATTERN-DEBT entry lands in the same commit as T1.4**,
   not later.
