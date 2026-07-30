# B7 — Task-Correctness benchmark (Phase 1: fd/Rust, flask/Python)

Measures whether the CALM-scripted refactor workflow (`edit_context` →
rename at each real call site → `diff_impact`) completes a real rename task
more correctly than a naive grep-and-edit workflow. **Deterministic oracle
only — no LLM judge**: (1) the corpus's own real `cargo test`/`pytest` build+
test gate, and (2) an independent callsite-recall check against ground truth
computed once, outside either arm.

Differs from B11/B12 (**tool-surface** correctness: given a fixed query, is
the *answer* right?) — B7 measures **task** correctness: given a refactor,
does the whole edit loop complete it without breaking the corpus's own tests
or missing a real call site. This is the Serena-style claim ("8–12 manual
steps → 1 call, fewer errors") `benchmarks/README.md` names as B7's origin.

Full design rationale: `docs/superskills/specs/2026-07-30-calm-dfb-levers-design.md`
(§1 + its audit-design Risk Assessment).

## Corpora

Reuses B12's pinned registry (`benchmarks/b12_tier1_tier2_tool_correctness/corpora.py`)
directly — Phase 1 picked the 2 lowest-friction languages:

| lang | corpus | build_cmd | test_cmd |
|---|---|---|---|
| rust | fd (sharkdp/fd) | *(none — `cargo test` builds+tests in one step)* | `cargo test --quiet` |
| python | flask (pallets/flask) | `uv sync --frozen` | `uv run pytest -q` |

**Verified live, not assumed:** flask's correct test setup is `uv sync --frozen`
+ `uv run pytest`, NOT a bare `pip install pytest` — a first attempt with bare
pip grabbed the latest pytest, whose internal `_pytest.monkeypatch.notset` API
(removed upstream) broke flask's own `tests/test_cli.py` at collection time.
flask pins its test dependencies via `uv.lock` (`[tool.uv] default-groups`
includes `"tests"`) and sets `filterwarnings = ["error"]`, so a
version-mismatched pytest can fail outright, not just warn.

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

Ground truth for "which files have a real call site" is B12's own
`ground_truth.git_grep_call_sites` (word-bounded, redefinition-filtered),
wrapped by `oracle.py::real_call_sites` with **one fix**: an extension filter.
**Found live, not in B12's own test suite:** B12's raw oracle has no
file-extension restriction — sampling flask's `init_db` for a Phase-1
candidate returned 9 "call sites" via plain `git grep`, 5 of which were
`.rst` documentation prose mentioning the function by name (`docs/appcontext.rst`,
`docs/patterns/sqlalchemy.rst`, etc.), not real Python call expressions. B12
itself doesn't need this fix (its own docstring says it "only needs to flag
gross zero-recall failures," not compute a precise recall fraction), but B7
does, so `real_call_sites` restricts results to the corpus's own source
extension before scoring.

## Results (Phase 1, both tasks)

| task | baseline | naive build_pass | naive recall | naive tool_calls | calm build_pass | calm recall | calm tool_calls |
|---|---|---|---|---|---|---|---|
| rename_fd_pattern_matches_leading_dot | green | True | 1.0 | 3 | True | 1.0 | 4 |
| rename_flask_from_prefixed_env | green | True | 1.0 | 4 | True | 1.0 | 5 |

**Honest finding, reported as measured (project policy — see main README's
"don't hide an unflattering number," precedent: B6's `find_callers`=0%):**
both arms tie at perfect recall on both Phase-1 tasks, and naive even uses
*fewer* tool calls. This is a real, non-manufactured result for these two
specific tasks — both target symbols are distinctive (unique corpus-wide
name, picked via B12's own `sample_distinctive` filter) and every real call
site literally spells the symbol's name, so a repo-wide `git grep -l`
(unrestricted by directory) finds exactly the same file set CALM's call graph
does. **This does not mean CALM has no advantage** — it means Phase 1's two
symbols don't exercise the case where the advantage should show up.

**Where CALM should actually differentiate (not yet tested — a concrete
recommendation for Phase 1b, not a vague TODO):** a symbol name that collides
with an unrelated same-named definition in a different scope/class (the
`ambiguous`-tier problem the parent research doc's arity-gate work targets),
or a call reached via dispatch that isn't a literal textual match of the
target's name (trait/interface method dispatch, a re-exported alias). A naive
`grep -l NAME(` either over-collects (edits an unrelated same-named function
elsewhere) or under-collects (misses a call that doesn't textually spell
`NAME`); CALM's structural resolution should get this right where grep
can't. Picking such a symbol is the natural next Phase-1b task, not a new
phase.

## Bugs found and fixed while building this (report, not hide)

1. **WORK_ROOT nested-workspace gotcha** (see Isolation above) — `cargo test`
   failed with "current package believes it's in a workspace when it's not"
   until work copies moved outside the CALM repo tree.
2. **Double-escaped regex in the naive arm** — an early draft's grep pattern
   was built inside an `rf"..."` raw string as `\\(` (two literal backslashes
   + `(`, an unbalanced/invalid ERE), not the single `\(` B12's own proven
   pattern uses. `git grep` exited non-zero on the bad regex and printed
   nothing to stdout, and the code didn't check the exit code — so the naive
   arm silently "renamed" 0 files, trivially passed build/test (nothing was
   touched to break), and scored recall=0.0. Caught by actually running the
   harness end-to-end, not by reading the code.
3. **Oracle extension-filter gap** (see Oracle above) — found while picking
   candidates, before it could contaminate a task definition.
4. **`edit_context`'s `CallerEntry` has no `path` field** — verified against
   the real `edit_context.snap` schema before writing `run_calm_arm`: the file
   path is the substring of `symbol` before the first `::` (the field was
   removed as a duplicate — see the doc comment in
   `crates/calm-server/src/tools/guardrails.rs`). A naive `c.get("path")`
   would have silently returned `None` for every caller.

## Running it

```bash
cargo build --release -p calm-cli   # calm-cli must already be built
benchmarks/.venv/bin/python benchmarks/b7_task_correctness/run_benchmark.py
# or a single task:
benchmarks/.venv/bin/python benchmarks/b7_task_correctness/run_benchmark.py --task rename_fd_pattern_matches_leading_dot
```

Preconditions: network access (fresh `git clone --local` of the pinned
sources, plus `cargo`'s crates.io fetch and `uv sync`'s package resolution on
first run); `uv` on PATH for the flask task.

`benchmarks/b7_task_correctness/.work` (B12's own convention) is **not** used
here — see Isolation above; work copies land in `../calm-b7-work/` instead,
which is not committed and safe to delete between runs.
