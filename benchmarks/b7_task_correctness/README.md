# B7 — Task-Correctness benchmark (Phase 1: fd/Rust, flask/Python; Phase 2: express/JS, zod/TS, gin/Go; Phase 3: spring-petclinic/Java)

Measures whether the CALM-scripted refactor workflow (`edit_context` →
rename at each real reference → `diff_impact`) completes a real rename task
more correctly than a naive grep-and-edit workflow. **Deterministic oracle
only — no LLM judge**: (1) the corpus's own real build/test gate, and (2) an
independent reference-recall check against ground truth computed once,
outside either arm.

Differs from B11/B12 (**tool-surface** correctness: given a fixed query, is
the *answer* right?) — B7 measures **task** correctness: given a refactor,
does the whole edit loop complete it without breaking the corpus's own tests
or missing a real reference. This is the Serena-style claim ("8–12 manual
steps → 1 call, fewer errors") `benchmarks/README.md` names as B7's origin.

Full design rationale: `docs/superskills/specs/2026-07-30-calm-dfb-levers-design.md`
(§1 + its audit-design Risk Assessment).

## Corpora

Reuses B12's pinned registry (`benchmarks/b12_tier1_tier2_tool_correctness/corpora.py`)
directly:

| lang | corpus | build_cmd | test_cmd |
|---|---|---|---|
| rust | fd (sharkdp/fd) | *(none — `cargo test` builds+tests in one step)* | `cargo test --quiet` |
| python | flask (pallets/flask) | `uv sync --frozen` | `uv run pytest -q` |
| javascript | express (expressjs/express) | `npm install` | `npm test` |
| typescript | zod (colinhacks/zod) | `pnpm install` | `pnpm test` |
| go | gin (gin-gonic/gin) | `go build ./...` | `go test ./...` |
| java | spring-petclinic (spring-projects) | *(none — `mvn test` compiles+tests in one step)* | `./mvnw -q test -Dtest='!*IntegrationTests' -DfailIfNoTests=false` |

**Go/gin was initially skipped** in this session's first Phase-2 pass — `go`
wasn't installed and passwordless `sudo` was unavailable (verified live:
`apt-cache policy golang-go` showed a candidate package, but `sudo -n true`
failed). The user then provided sudo access specifically to unblock this
(one-time, not persisted anywhere), Go 1.22.2 was installed via
`apt-get install golang-go`, and gin was added the same day.

**Verified live, not assumed — every build/test command below was confirmed
correct by actually running it, not by reading a package.json/pyproject.toml/
go.mod and guessing:**
- flask needs `uv sync --frozen` + `uv run pytest`, **not** bare
  `pip install pytest` — a first attempt with bare pip grabbed the latest
  pytest, whose internal `_pytest.monkeypatch.notset` API (removed upstream)
  broke flask's own `tests/test_cli.py` at collection time. flask pins test
  dependencies via `uv.lock` (`[tool.uv] default-groups` includes `"tests"`)
  and sets `filterwarnings = ["error"]`, so a version-mismatched pytest fails
  outright, not just warns.
- express has no committed lockfile — plain `npm install` is correct.
- zod uses `pnpm` (`pnpm-lock.yaml` present, not `package-lock.json`) — a
  bare `npm install` would not respect its lockfile.
- gin's `go.mod` requires Go 1.25.0, newer than the installed 1.22.2 —
  `GOTOOLCHAIN=auto` (Go's default since 1.21) transparently downloaded
  1.25.0 on first `go build`, no manual intervention needed. Verified by
  actually running the build, not assumed from the version mismatch alone.
- spring-petclinic's Maven Wrapper + `~/.m2` cache were already warm from an
  earlier session's language-support benchmarking, so no network/toolchain
  install was needed for Java. The task's `test_cmd` excludes the 4
  Testcontainers-backed `*IntegrationTests` classes (they spin up real Docker
  MySQL/Postgres containers): a timed run including them took 2m04s on a cold
  image cache alone, real risk against `oracle.py::run_cmd`'s fixed 300s
  timeout given B7 runs build/test up to 3× per task (baseline + naive + calm
  arms). Excluding them costs zero oracle coverage — verified none of the 8
  real reference sites fall in those 4 files. `-DfailIfNoTests=false` is
  needed because `-Dtest` exclusion-only patterns otherwise make surefire
  treat "0 explicitly-matched classes" as a failure.

## Isolation

Every corpus is a **fresh, throwaway clone per (task, arm)** — mirrors B12's
`prepare_worktree`/`cleanup_worktree` pattern (crash-safe: a crash mid-run
just leaves a clone to garbage-collect), not B11's mutate-then-reset pattern.

**Work copies live OUTSIDE the CALM repo tree** (`../calm-b7-work/`, a sibling
directory), unlike B12's own `.work/` (nested inside `benchmarks/`). This
matters specifically for Rust: cargo auto-discovers an ancestor `[workspace]`
if a crate is nested inside CALM's own cargo workspace — reproduced live this
session ("current package believes it's in a workspace when it's not") when
Phase 1's work copies were first placed inside the repo tree. B12 never hit
this because it only ever runs read-only `calm serve` queries against its
corpus copies, never a real `cargo test` inside them; B7 is the first
benchmark that does, so this is a genuinely new failure mode, not something
B12's own docstring warning (about the *pinned source* location) fully
covered.

## Oracle

Ground truth for "which files need touching for a **complete rename**" is
`oracle.py::real_references` — a **bare-identifier**, word-bounded, extension-
filtered `git grep`, built on top of (but broader than) B12's
`ground_truth.git_grep_call_sites`. Two real gaps were found and fixed while
building this, in two separate iterations:

1. **No file-extension filter** (found on flask's `init_db`): B12's raw
   oracle counted 5 `.rst` documentation-prose mentions as Python "call
   sites" alongside 3 real `.py` files. Fixed by restricting results to the
   corpus's own source extension.
2. **Call-shaped-only pattern misses non-call references** (found on zod's
   `prettifyError`): a first version matched only `NAME\(` (calls), which
   misses a bare re-export statement like `export { prettifyError }` (no
   trailing paren). A **rename** needs every bare-identifier reference —
   calls, re-exports, imports, type positions — not just calls, which is a
   genuinely different question from "who calls this" (what B12's oracle,
   and CALM's own call-graph-based `callers()`, are built to answer). Fixed
   by widening the pattern to match the bare identifier regardless of what
   follows it.

## Results (all 6 tasks, current methodology)

| task | baseline | naive build_pass | naive recall | naive tool_calls | calm build_pass | calm recall | calm tool_calls |
|---|---|---|---|---|---|---|---|
| rename_fd_pattern_matches_leading_dot | green | True | 1.0 | 3 | True | 1.0 | 4 |
| rename_flask_from_prefixed_env | green | True | 1.0 | 4 | True | 1.0 | 5 |
| rename_gin_clean_path | green | True | 1.0 | 3 | True | 1.0 | 4 |
| rename_express_set_charset | green | True | 1.0 | 4 | **False** | **0.667** | 4 |
| rename_zod_prettify_error | green | True | 1.0 | 5 | **False** | **0.5** | 4 |
| rename_petclinic_find_pet_types | green | True | 1.0 | 7 | True | 1.0 | 4 |

Java's row reflects the state **after** the parser fix below — see Phase 3
for the real bug this run found and fixed live, before this number was
reachable.

### Phase 1 + gin (fd, flask, gin): an honest tie

All three arms hit perfect recall + a passing build, and naive uses fewer or
equal tool calls. Reported as measured, not hidden (project policy — cf.
B6's `find_callers`=0% precedent): all three symbols are distinctive enough
(unique corpus-wide, picked via B12's `sample_distinctive` filter) that a
repo-wide `git grep` already finds every reference. gin's case additionally
confirms *why*: Go has no equivalent of JS's `require('./mod').fn(...)`
property-access call shape for same-package calls (Go's intra-package calls
are always bare identifiers), so there's no structural opportunity for the
property-access gap seen in JS to appear here at all. This doesn't mean CALM
has no advantage on these three — it means these symbols don't exercise the
case where an advantage (or the JS/TS gap) would show up.

### Phase 2 (express, zod): a real, reproducible finding — `edit_context`'s
`callers()` alone is not sufficient for a complete rename

Naive (bare-identifier grep) hits 100% recall + a passing build on **both**
Phase-2 tasks. The CALM arm, which relies purely on `edit_context`'s
`callers` list, **fails the build on both** — but for two structurally
different reasons, precisely distinguished by checking what was actually
missed:

**express/`setCharset` — a genuine call-graph gap.** `lib/response.js` binds
the import with a destructured bare name
(`var setCharset = require('./utils').setCharset;`) and calls it as
`setCharset(type, 'utf-8')` — CALM finds this fine (`edge_confidence:
"resolved"`). `test/utils.js` instead does `var utils = require('../lib/utils')`
and calls it as `utils.setCharset(...)` — a **property-access call through the
module namespace object**. This is a real invocation (5 real test assertions
depend on it), yet it's **completely absent** from `edit_context`'s response —
not in `callers`, not even in `blast_radius.files_affected` (29 files listed,
this isn't one of them). Verified directly against the raw `edit_context`
JSON, not inferred from the benchmark's summary. Renaming only what
`callers()` reports leaves `test/utils.js` calling a function that no longer
exists → `TypeError: utils.setCharset is not a function` (5 failing tests).

**zod/`prettifyError` — a task-oracle scope issue, not (clearly) a CALM bug.**
CALM's `callers()` *did* find the one real call
(`z.prettifyError(...)` in `error-utils.test.ts`) — so qualified/property-
access calls **can** resolve correctly in this codebase (an important
counter-example to reading the express finding as "property access never
resolves"). What it didn't report were `packages/zod/src/v4/classic/external.ts`
and `mini/external.ts`'s **bare re-export statements**
(`export { prettifyError }`) — which aren't calls at all, so a call-graph
tool correctly has no reason to surface them via `callers()`. A complete
rename needs the import/re-export graph too, which is a different CALM tool
(`dependencies()`) or a plain text search (`search(kind="grep")`) — exactly
what `AGENTS.md`'s own documented workflow already treats as a *separate*
stage from `callers()`, not something `edit_context` claims to cover on its
own.

**What this means for a real CALM-based rename workflow:** don't rely on
`edit_context`/`callers()` alone for a rename — it models the *call* graph,
not the full reference/import graph. A safe rename workflow should follow it
with a `search(kind="grep")` sweep for the bare identifier (or check
`dependencies()`) before considering the blast radius complete. This is
existing, documented CALM workflow guidance being *validated* by a concrete
failure case, not a new gap to file — except for the express finding, which
*is* a genuine, reproducible call-graph resolution gap worth investigating
in `parser.rs`'s JS/TS call-site extraction (property-access calls on a
required module's bare identifier vs. a destructured bare-name call to the
same export).

### Phase 3 (spring-petclinic/Java): a real call-graph bug found and fixed
live — `this.field.method()` calls were completely invisible

`findPetTypes` (`PetTypeRepository` interface method) was picked for the same
reasons as gin's `cleanPath`: a single, unambiguous definition, called from 2
production files and 3 test files with no name collision (ruled out
`Owner.getPet`, 3 overloads, and `VetRepository.findAll`, which collides with
`JpaRepository`'s own inherited `findAll`).

**First run: naive=1.0 recall/green build, calm=0.333 recall/`False` build —
CALM's own arm broke its own rename.** `edit_context("findPetTypes")` (raw
JSON) reported only 2 caller edges, both in `PetTypeFormatterTests.java`,
missing `PetController.java` and `PetTypeFormatter.java` (**production code**)
and `PetControllerTests.java`/`ClinicServiceTests.java`. The naive build
failure output pinpointed exactly what didn't get renamed:
`cannot find symbol: method findPetTypes()`.

**Root cause, found by reading the actual call sites, not guessing:** every
missed site calls `this.types.findPetTypes()` (`this.`-qualified field
access); the one call CALM *did* find is the only site written as a bare
`types.findPetTypes()` (no `this.`). That 100%-correlated split led straight
to `crates/calm-core/src/indexer/parser.rs::last_ident_segment` — the
function that extracts a call's receiver from Java's `method_invocation`
"object" field text. It split on `->` (PHP) and `::` (Rust/PHP scope) but
**never on a plain `.`** — so for the object text `"this.types"`,
`leading_ident` walked from byte 0 and stopped at the first `.`, returning
`"this"` instead of `"types"`. Tier-2 resolution then looked up a declared
type for the fake pseudo-variable `"this"`, found nothing, and silently
dropped the call edge — no fallback, no low-confidence edge, nothing. This
makes CALM's call graph (and `edit_context`/`callers()` built on it) blind to
**every** `this.field.method()` call in Java — one of the two idiomatic
field-access styles, used specifically to disambiguate a field from a
same-named constructor parameter (`this.types = types;`, exactly
`PetTypeRepository`'s own consumers' pattern, and a very common Spring/
enterprise-Java convention).

**Fixed** by adding `.` as a third segment separator (alongside `->`/`::`),
taking whichever separator occurs last in the text — the same "rightmost
identifier segment" contract the function already documented, just extended
to the one separator style it was missing. Verified additive, not a behavior
change: PHP's `$this->helper` (no `.` in that text) and every existing
`this`-only receiver test (`this.logIt()`, no `.` after `this`) are
unaffected — confirmed by the full workspace test suite staying green (868
passed) plus a new regression test
(`test_java_this_qualified_field_call_produces_receiver_not_this`). Re-running
the benchmark after the fix (and a `cargo build --release -p calm-cli` to
pick it up): calm build_pass=`True`, recall=1.0, matching naive — see the
Results table above.

### A rejected candidate, kept for the audit trail

`slugify` (`packages/zod/src/v4/core/util.ts:347`) was tried first and
discarded: it passed B12's `unique_definitions` filter (which only scans
free `function`/`class`/`interface`/`type` regex patterns) but is actually a
**3-way name collision** — `util.ts`'s free function is called by
`api.ts::_slugify`, re-exported as `_slugify as slugify` in `checks.ts`, and
there's *also* an unrelated `_ZodString.slugify()` fluent builder method
(`schemas.ts`) that calls `checks.slugify()`, never touching `util.ts`'s
function at all. Plain-text rename can't tell these apart, so neither arm's
result on this symbol would have measured anything real. This is itself a
genuine finding: B12's `unique_definitions` doesn't see object-literal/
class-method definitions, only top-level declarations — worth knowing before
reusing it as a candidate-picker for an *edit* task (as opposed to its
original read-only tool-correctness use in B12).

## Bugs found and fixed while building this (report, not hide)

1. **WORK_ROOT nested-workspace gotcha** — `cargo test` failed with "current
   package believes it's in a workspace when it's not" until work copies
   moved outside the CALM repo tree (see Isolation above).
2. **Double-escaped regex in the naive arm** — an early draft's grep pattern
   was built inside an `rf"..."` raw string as `\\(` (two literal backslashes
   + `(`, an unbalanced/invalid ERE). `git grep` exited non-zero and printed
   nothing, so the naive arm silently "renamed" 0 files, trivially passed
   build/test, and scored recall=0.0. Caught by actually running the harness
   end-to-end, not by reading the code.
3. **Oracle extension-filter gap** (flask's `init_db`) and **4. call-shaped-
   only oracle gap** (zod's `prettifyError`) — see Oracle above.
5. **`edit_context`'s `CallerEntry` has no `path` field** — verified against
   the real `edit_context.snap` schema before writing `run_calm_arm`: the file
   path is the substring of `symbol` before the first `::` (removed as a
   duplicate field — see the doc comment in
   `crates/calm-server/src/tools/guardrails.rs`). A naive `c.get("path")`
   would have silently returned `None` for every caller.
6. **`slugify` name-collision task pick** — see "A rejected candidate" above.
7. **`last_ident_segment` never split on a plain `.`** (Phase 3, spring-
   petclinic's `findPetTypes`) — a real, previously-undiscovered call-graph
   gap in `crates/calm-core/src/indexer/parser.rs`, not a benchmark-harness
   bug like 1/2 above. See "Phase 3" above for the full root-cause. Fixed in
   `last_ident_segment` + a new regression test
   (`test_java_this_qualified_field_call_produces_receiver_not_this`).

## Running it

```bash
cargo build --release -p calm-cli   # calm-cli must already be built
benchmarks/.venv/bin/python benchmarks/b7_task_correctness/run_benchmark.py
# or a single task:
benchmarks/.venv/bin/python benchmarks/b7_task_correctness/run_benchmark.py --task rename_fd_pattern_matches_leading_dot
```

Preconditions: network access (fresh `git clone --local` of the pinned
sources, plus `cargo`'s crates.io fetch, `uv sync`'s, `npm`/`pnpm install`'s,
and `go build`'s module resolution on first run); `uv` on PATH for the flask
task, `pnpm` on PATH for the zod task, a Go toolchain (`GOTOOLCHAIN=auto`
handles a version mismatch against `go.mod`) for the gin task.

`benchmarks/b7_task_correctness/.work` (B12's own convention) is **not** used
here — see Isolation above; work copies land in `../calm-b7-work/` instead,
which is not committed and safe to delete between runs.

## Next steps

## v2 (2026-08-20): a third `calm_v2` arm, and why it did NOT close the gap

## Update (same day): Express's real bug found and FIXED — `path_lang` gap in `context.rs`

## Update (same day): Zod's real bug found and FIXED too — transitive re-export walk

Per an explicit follow-up ask, the zod gap (§7.1/§6.1's "materially bigger feature") was fixed in
the same session. Two changes: (1) `crates/calm-core/src/indexer/imports.rs` now walks
`export_statement` for JS/TS and parses `export {...} from '...'` / `export * from '...'` into
`import_edges` (previously zero export syntax was ever indexed, for any JS/TS project); (2)
`reference_impact`'s import-edge lookup (`crates/calm-server/src/tools/trace.rs`) is now a bounded
BFS through wildcard re-export chains instead of a single-hop query, closing zod's actual two-hop
barrel-file case. Full detail, including a real fallback-chain bug caught and fixed before it
shipped: `docs/plans/2026-08-20-product-uplift-and-b7v2-roadmap.md` §10.

`cargo test --workspace --release`: 0 failed. Re-ran all 6 B7 tasks end-to-end:

| task | calm_v2, before | calm_v2, after |
|---|---|---|
| rename_zod_prettify_error | 0.5/False | **1.0/True** |
| the other 5 tasks (incl. express) | 1.0/True | 1.0/True (unchanged) |

**B7 now passes 6/6 via the `calm_v2` arm.** `calm` v1 stays at 0.5/False for zod by design (v1
never calls `reference_impact`, the tool this fix lives in). Not covered, documented as a known
scoped-out gap: `export * as ns from` namespace re-exports, and an `as`-aliased re-export changing
the name partway through a chain.

The deeper audit above (triggered by a user follow-up: "is there truly no way to fix this?") found
that the actual root cause was neither `reference_impact`'s coverage nor the parser — it was
`build_resolution_context`'s `path_lang` map being derived only from the `symbols` table, so a
file with zero top-level named declarations anywhere (any Mocha/Jest/Vitest-style `describe`/`it`/
`test` file, including the real `test/utils.js`) never got a language entry, causing
`resolve_sites_to_edges`'s same-language safety filter to empty out every outgoing call that file
made — regardless of confidence tier. Full root-cause trace: `docs/plans/2026-08-20-product-uplift-
and-b7v2-roadmap.md` §8.

**Fixed** in `crates/calm-core/src/indexer/pipeline/context.rs` (`path_lang` now seeded from
`file_index`, which already tracks every indexed file's language regardless of symbol count) —
same doc's §9 has the full verification chain. Re-ran all 6 B7 tasks end-to-end against the fixed
binary:

| task | calm / calm_v2, before | calm / calm_v2, after |
|---|---|---|
| rename_express_set_charset | 0.667/False · 0.667/False | **1.0/True · 1.0/True** |
| rename_zod_prettify_error | 0.5/False · 0.5/False | 0.5/False · 0.5/False (unchanged — separate bug, §7.1) |
| the other 4 tasks | 1.0/True | 1.0/True (unchanged, no regression) |

`cargo test --workspace --release`: 0 failed. New permanent regression test:
`test_call_from_a_file_with_no_named_symbols_still_gets_a_call_edge` in
`crates/calm-core/src/indexer/pipeline.rs`. Fix is currently uncommitted, pending the user's
go-ahead to commit/push.

Full rationale and audit trail: `docs/plans/2026-08-20-product-uplift-and-b7v2-roadmap.md`.

A third arm, `calm_v2`, was added (`run_calm_arm_v2` in `run_benchmark.py`, alongside `naive`/
`calm` — `calm` v1 kept unchanged for comparison). It supplements `edit_context`'s `callers()`
with `reference_impact`'s `must_change`/`likely_change` hits, on the hypothesis (from
`reference_impact`'s own source comment, `crates/calm-server/src/tools/trace.rs:780-784`, which
names both tasks below) that this tool — built specifically to close this benchmark's own
documented gap — would fix Express and Zod.

**Result: it did not.** Both tasks scored identically to v1:

| task | v1 (`calm`) recall/build | v2 (`calm_v2`) recall/build |
|---|---|---|
| rename_express_set_charset | 0.667 / False | 0.667 / False |
| rename_zod_prettify_error | 0.5 / False | 0.5 / False |

The other four tasks (fd, flask, gin, spring-petclinic) stayed at 1.0/True in `calm_v2` too — no
regression from the wider `reference_impact` surface.

**Root-caused, not assumed:**

- **Zod**: `reference_impact` returned `must_change_count: 0` for `prettifyError`. Both missing
  files re-export it via `export { …, prettifyError, … } from "../core/index.js";`. Traced to
  `crates/calm-core/src/indexer/imports.rs::import_node_types`, which for `"javascript" |
  "typescript"` only walks `["import_statement", "variable_declarator"]` — **`export_statement` is
  never walked at all**, so no `import_edges` row is ever created for a re-export, for any JS/TS
  project. This is a real, previously-undocumented gap in the import extraction, not something
  `reference_impact`'s existing import-edge tier can see. (The two real `z.prettifyError(...)` call
  sites did get a call_edge, but at `"ambiguous"` confidence, correctly bucketed as `review` —
  `edit_context.callers()` already covered those, so this was never where the miss came from.)
- **Express**: `reference_impact` produced 0 `review` hits and 7 `textual_only` hits for
  `setCharset` — `test/utils.js`'s `utils.setCharset(...)` (property access on a bare
  `require('../lib/utils')`) still produces no call_edge at all. Confirms this is exactly the
  pre-existing `parser.rs` call-site-extraction gap already named in this section's own "Next
  steps" above — orthogonal to what an import/export-edge fix could address.

**Correction to `reference_impact`'s own source comment**: its claim to catch "the exact gap
behind" both tasks holds for a plain-`import`-side reference, but not for Zod's actual `export {
X } from 'y'` re-export shape, and not for Express's case at all (a different bug class — call-site
extraction, not import/export tracking). Worth narrowing that comment once the `export_statement`
gap below is fixed.

**Follow-up, not done in this pass — revised after a deeper audit** (full detail:
`docs/plans/2026-08-20-product-uplift-and-b7v2-roadmap.md` §6-§7): walking `export_statement` in
`import_node_types`/`extract_imports_from_tree` is real and necessary, but **not sufficient on its
own** for Zod — its actual re-export is a two-hop barrel chain (`external.ts` names `prettifyError`
re-exporting from `core/index.ts`, which itself re-exports `errors.ts` via a *wildcard*
`export * from './errors.js'` naming no symbols at all). A single-hop `import_edges` fix would
point `external.ts` at `core/index.ts`, not at `errors.ts` where the symbol is actually defined —
`reference_impact`'s direct `to_path` lookup still wouldn't find it. Closing this needs transitive
import-edge resolution through wildcard re-export chains, not a single-function patch — re-run
`rename_zod_prettify_error` after landing whichever shape of fix to confirm empirically, not
assume (this file's own history is a live example of why: the original `reference_impact` source
comment claimed to close this gap and, empirically re-run here, did not). Express's gap needs a
separate `parser.rs` call-site-extraction fix (property-access call through a required module's
bare identifier), already scoped above — unaffected by anything import/export-related.

Also independently re-verified this session (adversarial self-audit, not just re-reading the
numbers): both failures reproduce identically outside the benchmark harness (`npm test` /
`pnpm test` run directly against the renamed corpus copies — same `TypeError`/`TypeCheckError` as
above); both symbols are collision-free and false-positive-free in the pinned corpora (manual grep
matches `oracle_callsite_files` exactly for both); and naive's "1.0 recall" on every task is close
to tautological by construction (`run_naive_arm`'s file-selection regex and `oracle.py`'s
ground-truth regex are near-identical) — the informative, independent signal is `build_pass` on
both arms, which is real compiler/test ground truth and was the first thing re-verified. See the
roadmap doc's §7 for the full adversarial audit trail.

1. Investigate the express `setCharset` call-graph gap directly in
   `parser.rs`'s JS/TS call-site extraction (property-access call through a
   required module's bare identifier vs. a destructured bare-name call to
   the same export) — a candidate root-cause worth its own session, not
   folded into this benchmark's scope. (Phase 3's Java finding was this same
   shape of bug — receiver misattribution in `parser.rs` — so it's worth
   checking whether the `.`-split fix incidentally helps here too before
   assuming a separate root cause; not verified either way yet.)
