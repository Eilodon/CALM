---
title: CALM remaining accuracy/efficiency levers — F/B7, D, (B) design
date: 2026-07-30
author: gokuderafight (via Claude Sonnet 5)
SPEC_APPROVED: true
SPEC_ESCALATION: false
ESCALATION_FINDING: ""
supersedes_research: docs/superskills/plans/2026-07-29-calm-accuracy-efficiency-research.md
---

# CALM — Detailed design for the remaining open levers (F/B7 → D; B deferred)

> Follow-on to the 2026-07-29 research doc. That doc's §7 listed A′/C/D/F/B as
> open. Since then commit **`dfe6ef4` (PR #48)** shipped **C** (coreness folded
> into hybrid RRF, all 3 legs) and **A′ for Go** (generalized the Elixir-only
> arity gate). **E** was already fully closed. This spec covers what is *still*
> open, re-grounded against the current code, and picks the build order the
> user approved: **F/B7 first → D (local-ONNX backend) → (B deferred to a
> feasibility note)**.

## 0. Ground-truth corrections to the research doc (verified this session)

Every claim below was re-checked against the code at HEAD `dfe6ef4`, not the
research doc's prose:

| # | Doc claimed | Reality (file:line) | Impact on plan |
|---|---|---|---|
| G1 | B is "write TSG rules per language" (high effort, but doable per a template) | `load_java` loads rules from the **external crate** `tree_sitter_stack_graphs_java` ([formal.rs:583](../../../crates/calm-core/src/resolver/formal.rs)). `Cargo.toml` has exactly 4: python/typescript/javascript/java ([Cargo.toml:32-35](../../../crates/calm-core/Cargo.toml)) — the only languages the stack-graphs ecosystem publishes. | **B is ecosystem-blocked**, not just long-term. Demoted to a feasibility note (§4). |
| G2 | Formal tier covers "Rust/Python/TS/JS/Java via `resolver/formal.rs`" | `FormalResolver` has only `load_python/typescript/javascript/java` — **no `load_rust`**. Rust's `formal` edges come from the rust-analyzer **SCIP overlay**, not stack-graphs. | Corrects the mental model; doesn't change B's verdict. |
| G3 | D "already has a config knob `semantic_search.model`" — implying low effort | True, but `load()` sends any custom `model_id` through `StaticModel::from_pretrained` ([embedding.rs:192](../../../crates/calm-core/src/embedding.rs)); the Cargo comment states the knob "still resolves through model2vec-rs's hf-hub fallback" ([Cargo.toml:78-79](../../../crates/calm-core/Cargo.toml)). It can only load **model2vec-format** models. | D genuinely needs a new backend abstraction (§3), confirmed. |
| G4 | B7/B8 need infra built | B7/B8 both `Planned` ([README.md:16-17](../../../benchmarks/README.md)). `MCPClient`, `naive_workflow`, `tasks.yaml`, B11's worktree isolation + live oracles already exist. **No** agent-loop/LLM driver anywhere. | B7-scripted is cheap (§1); B8 + B7-LLM need one new harness (§2). |

**Net ROI re-ranking:** F/B7 (cheapest, strategic, and the only way to *measure*
whether A′/C helped) > D (real "recall now" lever) > B (blocked).

---

## 0.1 Decisions locked (2026-07-30, user)

| Q | Decision | Rationale given |
|---|---|---|
| B7 corpus | **Per-language**: each task runs on a real corpus of its own language (Python task → Python corpus, Rust task → Rust corpus, etc.), not self-repo-only | Self-repo-only would only ever exercise Rust; the whole point of B7 is measuring the workflow across the languages CALM claims to support |
| D runtime | **Universal** — must not be glibc-only | Matches CALM's existing local-first/musl guarantee; a glibc-only "high-accuracy" path would be a second-class citizen |
| B8 model pair | **Haiku vs Sonnet** | — |
| tasks.yaml vs new file | **Researched below (§1.3a)** — evidence-based, not a guess | User explicitly asked for research, not a coin flip |

Additionally, the user flagged (correctly, and consistent with this repo's own
history — see memory entries on the `resolution`/`b2` benchmarks' past silent
bugs) that **the existing benchmark infra F plans to reuse must itself be
rigorously audited before being trusted as a foundation**. §0.2 below is that
audit, done inline against the actual code (no subagent, per this project's
verification-before-completion norm).

## 0.2 Audit of the benchmark infrastructure F/B7 and D would reuse

Read in full: `benchmarks/lib/mcp_client.py`, `benchmarks/lib/naive_workflow.py`,
`benchmarks/lib/tasks.yaml`, `benchmarks/b11_extended_competitor_ab/run_benchmark.py`,
`benchmarks/b12_tier1_tier2_tool_correctness/{ground_truth.py,corpora.py,FINDINGS_ROOTCAUSE.md,UPGRADE_PLAN.md}`,
`benchmarks/b3_search_quality/run_benchmark.py`, `benchmarks/resolution/README.md`.

**Finding A1 (HIGH, changes B7's design) — B11's `grep_oracle_callers` is the
un-hardened predecessor of B12's oracle; B7 must use B12's, not B11's.**
B12's `ground_truth.py` docstrings document three real ground-truth bugs its
author found empirically and fixed:
1. Bare substring match counted `create_global_jinja_loader(` as a call site
   for `jinja_loader` — fixed with a word-boundary regex (`(^|[^A-Za-z0-9_])NAME\(`).
2. A name redefined in many scopes (`my_reverse` as ~10 local test-helper
   closures) had every *other* definition line miscounted as a call site —
   fixed via `_looks_like_a_definition` filtering.
3. Generic/common names (`index`, `add`, `default`) produced hundreds of
   unrelated corpus-wide matches — fixed via `total_occurrences` filtering
   before sampling.

B11's `grep_oracle_callers` (which the original design in §1.2 proposed
reusing) has **none of these three fixes** — it does a bare `grep -rn f"{symbol}("`
with only a same-file-prefix filter. It works for B11 today only because B11's
4 anchors were hand-picked to avoid exactly these traps (documented inline:
"good enough for this small, single-symbol, single-language case; not a
general-purpose oracle"). Since B7 now spans multiple languages/corpora
(§0.1), reusing the *naive* oracle would silently reproduce bugs B12 already
found and fixed once. **Decision: B7's oracle is built on B12's
`git_grep_call_sites`/`unique_definitions`/`total_occurrences`, not B11's
`grep_oracle_callers`.**

**Finding A2 (MED, changes D's evaluation plan) — B3's `hybrid` mode silently
degrades to FTS-only when the `embeddings` feature/model isn't present, and
reports as if that were a real result.** From `run_benchmark.py`'s own
docstring: "most environments running this script … won't have that, so
hybrid falls back to FTS-only and its NDCG will equal kind=symbol's. That is
reported, not hidden." This is honest, but it is a **trap for D specifically**:
if D's "before/after" B3 run doesn't explicitly confirm `hybrid_degraded ==
false` for every query, a "no improvement" result could mean *the new backend
was never actually exercised*, not that it doesn't help. Also, B3 only scores
`kind=symbol` and `kind=hybrid` (RRF-fused) — there's no direct `kind=semantic`-only
measurement, so the new ONNX backend's raw contribution is diluted by fusion
with FTS in the existing scoring. **Decision: D's benchmarking step (i) asserts
`hybrid_degraded=false` before trusting any number, and (ii) adds a
`kind=semantic`-only score to `b3_search_quality` so the embedding backend's
own NDCG is visible pre-fusion, not just its post-RRF contribution.**

**Finding A3 (useful, de-risks B7) — B12's `corpora.py` is already the right
per-language corpus registry to build B7 on**, not something to invent fresh:
6 pinned real OSS repos across Tier-0 (flask/python, gin/go,
spring-petclinic/java, express/js, zod/typescript, fd/rust), with a
fresh-clone-per-run isolation pattern (`prepare_worktree`/`cleanup_worktree`)
that is *more* crash-safe than B11's mutate-then-`git checkout --`-reset
pattern (a crash mid-B11-run can leave a real mutation in the shared worktree;
a crash mid-B12-run just leaves a throwaway clone to garbage-collect). B7
should reuse `corpora.py`'s registry directly for the corpus↔language pairing
the user asked for in §0.1.

**New precondition surfaced by A3 (must be stated, not assumed):** B7 needs
more than B12 did from these corpora — B12 only *reads* them; B7 must **build
and run each corpus's real test suite** after a refactor (`cargo test` for fd,
`pytest` for flask, `go test` for gin, `npm test` for express/zod, `mvn test`
for spring-petclinic) to get its build/test-pass oracle. That means real
per-language toolchain availability becomes a hard precondition — Node+npm
(with network for `npm install`), a JVM+Maven, a Go toolchain, a Python env —
none of which B11/B12 needed (they only ever call `calm serve` + read-only
queries). **Recommendation: launch B7 with the 2 lowest-friction, most
build-hermetic corpora first — fd (Rust, `cargo test`, toolchain already
required for calm-cli itself) and flask (Python, `pytest`, no network needed
post-checkout) — and treat Go/JS-TS/Java as a fast-follow, Java last (Maven+JVM
is the heaviest, flakiest setup of the five).**

**Checked, not a concern:** B12's FINDINGS_ROOTCAUSE F1 (JS/TS call graph blind
to calls outside a named-function body) is **already fixed** — commit `34918c4`
("fix(indexer,edit,diff): close B12 upgrade-plan findings F1/F2+F2b/F4") is in
this repo's history prior to this session. Not a live risk for a JS/TS corpus
in B7.

---

## 1. PART 1 — F/B7: Task-Correctness benchmark (PRIMARY deliverable)

### 1.1 What it measures — and how it differs from B11/B12
- **B11/B12 measure tool-surface correctness**: given a *fixed query*, does the
  tool return the right answer? (e.g. does `callers(x)` recall all caller files.)
- **B7 measures task correctness**: given a *refactor task*, does the whole
  workflow **complete it without breaking the code or missing a callsite?** The
  unit under test is the edit loop (`edit_context` → `edit_symbol` → `diff_impact`),
  not a single lookup. This is the Serena-style claim ("8–12 manual steps → 1
  call, fewer errors") the README names as B7's origin.

### 1.2 Design — two arms, one deterministic oracle
Each task is a **refactor with a machine-checkable definition of "done"**:

- **Arm A — naive (no call graph):** `grep` for the symbol, edit each hit. Reuses
  `naive_workflow._grep_files`. Deliberately has no way to see re-exports, trait
  impls, or macro-generated callsites.
- **Arm B — CALM-scripted:** `edit_context(symbol)` to enumerate the blast radius
  → `edit_symbol` at each site (hash-guarded) → `diff_impact` to verify. Reuses
  `MCPClient` from `benchmarks/lib/mcp_client.py`.

- **Oracle (deterministic, no LLM judge), built on B12's hardened primitives
  (§0.2 Finding A1), not B11's naive `grep_oracle_callers`:**
  1. **Build/test gate** — after the refactor, run **the corpus's own real
     toolchain** (`cargo test` for fd, `pytest` for flask, etc. — see §1.3 per
     language). Pass/fail is the headline. For a compiled corpus a missed
     callsite = a compile error = a clean, unarguable fail.
  2. **Callsite recall** — fraction of the ground-truth callsite set actually
     edited. Ground truth = `b12.ground_truth.git_grep_call_sites` (word-bounded,
     redefinition-filtered) computed independently of both arms, diffed against
     each arm's actual edited-file set via `git diff` against a pinned reference
     commit.

  Per the research doc's explicit constraint: **no LLM-judge as the primary gate**
  — it would inject a second model's noise into a measurement about the first.

### 1.3 Corpus & tasks — per-language (user decision, §0.1)
- **Corpus registry:** reuse `benchmarks/b12_tier1_tier2_tool_correctness/corpora.py`
  directly (§0.2 Finding A3) — it already pins one real OSS repo per Tier-0
  language. **Phase 1 (lowest-friction, ship first):** fd (Rust, `cargo test`,
  no extra toolchain beyond what calm-cli itself needs) and flask (Python,
  `pytest`, no network after checkout). **Phase 2 (fast-follow):** gin (Go,
  `go test`), express/zod (JS/TS, `npm test` — needs `npm install`, a real
  network precondition to state explicitly). **Phase 3 (heaviest, last):**
  spring-petclinic (Java, Maven+JVM — flag as the highest setup/flake risk of
  the five, matching B11's own experience with JVM-backed tooling cold starts).
- **Isolation:** B12's `prepare_worktree`/`cleanup_worktree` (fresh `git clone
  --local` per run from the pinned read-only source) — more crash-safe than
  B11's mutate-then-reset pattern (§0.2 Finding A3). A live/shared corpus run
  is forbidden (arms mutate files).
- **Task types** (start with 2–3 per language in Phase 1, mirrored per language
  in later phases — new schema, see §1.3a):
  - `rename_symbol` — rename a function with N real callsites across files in
    that corpus; done = builds/tests pass + all N sites renamed.
  - `change_signature` — add a parameter; done = every callsite updated + builds/tests pass.
  - `narrow_a_hub` (differentiator, Phase 1 Rust only initially) — attempt an
    unguarded edit on a real hub symbol in the corpus; done = CALM's risk gate
    **refuses** (reuse B11's `run_risk_gate_refusal` pattern), naive arm
    silently succeeds and breaks tests.
- **"Not a stub" acceptance criterion** (brainstorming gotcha): each task's oracle
  must be a *real* build/test invocation on disk against the actual corpus
  toolchain, never a hardcoded expected count.

### 1.3a tasks.yaml vs. a new file — researched, not guessed
`benchmarks/lib/tasks.yaml`'s schema (`naive: {type, path|pattern, globs}` +
`ci: {tool, arguments}`) is a **read-task** shape — it describes one lookup and
compares two answers. It's consumed today by **3 existing benchmarks** (B4, B6,
B11) all assuming self-repo-Rust-only anchors. B7 needs a structurally
different shape per task: a target corpus (`lang`, pinned commit), a mutation
spec (symbol, kind of refactor, new signature/name), and a build/test command
— none of which fit `tasks.yaml`'s two-key schema without either (a) adding
optional fields that B4/B6/B11 must now all tolerate/ignore, growing their
shared parsing surface for no benefit to them, or (b) branching parse logic by
task shape, which is exactly the kind of implicit coupling this project's own
benchmark audits (§0.2) have repeatedly found bugs in.
**Recommendation: a new `benchmarks/lib/refactor_tasks.yaml`** (separate file,
new schema: `{id, lang, corpus, symbol, refactor: {kind, ...}, build_cmd,
test_cmd}`), imported only by B7 (and later B8). Zero blast radius on B4/B6/B11;
the existing `tasks.yaml` stays exactly as every current consumer expects.

### 1.4 New pieces to build
- `benchmarks/b7_task_correctness/run_benchmark.py` — orchestrator (mirror B11's shape).
- `benchmarks/b7_task_correctness/oracle.py` — thin wrapper importing
  `b12_tier1_tier2_tool_correctness.ground_truth` for callsite ground truth,
  plus `build_test_gate(corpus, build_cmd, test_cmd)`.
- `benchmarks/lib/refactor_tasks.yaml` — new file, per §1.3a.
- `benchmarks/b7_task_correctness/README.md` — methodology + honest-reporting
  note + the per-language toolchain preconditions from §0.2 Finding A3.

### 1.5 Acceptance criteria
- Runs green on the self-repo worktree, leaves the worktree byte-identical afterwards.
- Produces `results.json` with, per task: `build_pass` (bool) + `callsite_recall`
  (`k/N`) for **both** arms.
- The headline delta (CALM build-pass / recall − naive) is reported even if it's
  small or unfavourable (README honest-reporting policy; cf. B6 `find_callers`=0%).

### 1.6 Risks
- **R1 — self-repo tasks are too easy / too few** to show a gap. Mitigation:
  pick symbols with re-exports or macro callsites where grep provably misses;
  add Phase-2 external corpora (b12's 6 pinned OSS repos) once methodology holds.
- **R2 — scripted arms don't reflect a real LLM's judgment.** Accepted: B7-scripted
  measures the *tool workflow's* ceiling deterministically. The LLM-in-the-loop
  question is B7-LLM-driven / B8 (§2), which is *why* the harness is built once.

---

## 2. PART 2 — F/B8 + the shared agent-loop harness

### 2.1 The harness (built once, used by B7-LLM-driven and B8)
No agent loop exists in `benchmarks/` today (only `tiktoken`, a token counter).
Build `benchmarks/lib/agent_loop.py`:
- Bridges the calm MCP tool list → the model's tool-definition schema, runs the
  agentic loop (model proposes `tool_use` → dispatch via `MCPClient.call_tool` →
  feed `tool_result` back until the model stops), and records transcript + token
  + tool-call counts.
- **Implementation note:** use the Anthropic SDK's tool-runner / a manual tool-use
  loop; **consult the `claude-api` skill at implementation time** for the current
  model IDs, `tool_runner` signature, and MCP-connector option — do not hardcode
  from memory.

### 2.2 B8 — model-tier leveling
- **Claim under test:** a cheap model + CALM tools ≈ an expensive model with no
  tools (or naive grep), on the same task set.
- **Cells:** {Haiku, Sonnet} × {CALM tools, naive/no-tools} over the B7 task
  set (model pair per user decision, §0.1). Same deterministic oracle (build/test
  pass + callsite recall) — reusing the B7-Phase-1 corpora (fd, flask) first.
- **Nondeterminism:** LLMs aren't deterministic → **N≥3–5 repeats/cell, report
  distribution (median/IQR, like B11), never a point estimate.** Use prompt
  caching to cut cost.
- **Cost:** ~tens of USD (few tasks × 4 cells × N repeats). Gate behind an
  explicit `--live-llm` flag so CI never spends money by accident.

### 2.3 Acceptance criteria & risks
- Harness: given a task + model + tool-mode, returns `{build_pass, recall, tokens,
  tool_calls, transcript}`; a tool error surfaces, never silently passes.
- **Environment preconditions** (brainstorming gotcha): `ANTHROPIC_API_KEY` present;
  network egress; the calm release binary already built. All three flagged in the
  README as run prerequisites, none assumed by CI.
- **Risk:** a model may "succeed" by luck on a trivial task → keep tasks non-trivial
  (R1 mitigation) and rely on the distribution across repeats, not a single win.

---

## 3. PART 3 — D: Optional local-ONNX embedding backend

### 3.1 The abstraction seam (small + contained)
`Embedder` is a concrete struct wrapping `StaticModel` today. Every consumer only
touches three methods — `embed_one` ([:212](../../../crates/calm-core/src/embedding.rs)),
`embed_batch` ([:216]), `dim` ([:208]) — plus `load` ([:158]). Callers
(`embed_pending`, `embed_pending_chunks`, `create_embedding_table(dim)`, `knn`) go
through those, so the blast radius is contained to `embedding.rs`.

**Design:** make `Embedder` an enum over a backend:
```
enum Backend { Static(StaticModel), Onnx(OnnxModel) }   // both behind #[cfg(feature="embeddings")]
```
`load(model_id, dim)` dispatches on config: `model_id == DEFAULT` or a model2vec id
→ `Static` (unchanged path, incl. vendored-bytes + LFS fallback); an `onnx:<id>`
prefix (or a new `semantic_search.backend` field) → `Onnx`. The `#[cfg(not(feature
= "embeddings"))]` stub mod must gain the mirrored no-op so non-embedding builds
still compile.

### 3.2 Runtime choice — THE load-bearing decision (must be universal, per §0.1)
The user requires the local backend to be **universal**, not glibc-only — so
this was verified as a fact, not assumed:

- **`ort` (ONNX Runtime bindings) — ruled out for the default/universal path.**
  Confirmed: ONNX Runtime's own build is tied to glibc — "its build process and
  some of its assumptions are closely tied to glibc… contrib ops and certain
  execution providers fail to compile under musl" (verified via web search this
  session, not assumed from memory). It could still exist as a separate,
  explicitly glibc-only extra feature later, but it cannot satisfy "universal."
- **`tract` — confirmed candidate.** Pure-Rust ONNX inference engine with a
  documented working pattern for **standalone `x86_64-unknown-linux-musl` static
  binaries** (verified via a real writeup on building musl-static Rust
  inference binaries this session). This is the same shape of guarantee
  model2vec-rs already gives CALM (pure-Rust, no C-extension musl breakage,
  same failure mode `sqlite-vec` hit before). **Recommended primary.**
- **`candle`** — HF's pure-Rust framework, capable of running BERT-family code
  embedders, but **no musl track record was found** in this session's search
  (absence of evidence, not evidence of absence — it may well work, just
  unverified). Demoted from "co-primary" to **not recommended until its own
  spike passes**; `tract` already has stronger evidence for the same job.
- **Decision gate (do first, before any other D work):** a small spike loading
  a real code-embedding model (ONNX-exported) under `tract` on an actual
  `x86_64-unknown-linux-musl` build of `calm-cli`, run and produce a real
  vector. This is now a *confirmation* spike (evidence already favors success),
  not an open bet — but it still must be run for real before shipping, since a
  blog post building a *different* model is not the same as this succeeding
  inside CALM's own `include_bytes!`/asset-loading conventions.

### 3.3 Model, feature flag, measurement
- **Feature:** new `onnx-embeddings` cargo feature (backend = `tract`), **off by
  default**; `embeddings` (model2vec) stays the default. Default binary +
  guarantees unchanged, and — unlike the `ort` path — this feature is safe to
  offer on the **same musl target** as the default build, not a separate glibc
  variant.
- **Model candidates (ONNX-exportable, code-specialized):** `nomic-ai/CodeRankEmbed`
  (the *teacher* potion-code-16M was distilled from — running the teacher is the
  direct recall uplift), `jinaai/jina-embeddings-v2-base-code`, or `BAAI/bge-*`
  — all need an ONNX export step (`optimum-cli export onnx` or similar) since
  `tract` consumes ONNX/NNEF, not raw safetensors+Python config.
- **Measurement — corrected per §0.2 Finding A2:** `benchmarks/b3_search_quality`
  needs two changes before it can be trusted for this comparison: (1) assert
  `hybrid_degraded == false` for every query in the run — a run where it's
  `true` measured nothing about the new backend and must be discarded/rerun,
  not reported; (2) add a `kind="semantic"`-only score alongside the existing
  `symbol`/`hybrid` scores, so the backend's own NDCG is visible pre-RRF-fusion,
  not diluted by the FTS leg. Run default vs `onnx-embeddings` (tract) and
  report both the semantic-only and hybrid deltas. D ships only if the delta is
  real under both.

### 3.4 Acceptance criteria & risks
- Default build/behaviour byte-identical (backend off by default; regression test:
  default path still loads vendored bytes).
- With `onnx-embeddings` on + a configured model: index builds, `search(kind=
  "semantic")` returns, B3 NDCG@10 improves measurably on **both** the
  semantic-only and hybrid scores (§3.3), with `hybrid_degraded` confirmed false.
- Musl build of `calm-cli` with `onnx-embeddings` enabled still produces a
  working static binary (the confirmation spike, §3.2, re-run as a CI check
  once merged — regressing this silently back to glibc-only would be exactly
  the kind of drift this project's own musl/LFS incident history (see memory:
  `sqlite-vec` musl break, LFS budget outage) warns about).
- **Risks:** binary/model size (mitigate: download at runtime + cache, don't
  `include_bytes!` a large model); dimension mismatch (already handled —
  `load` probes real dim + `heal_dimension_mismatch`); ONNX export step adds a
  Python-side dependency to the *model preparation* pipeline (not the Rust
  binary) — document as a one-time step per model, not a runtime dependency.

---

## 4. PART 4 — B (stack-graphs expansion): feasibility note, deferred

Not planned for build now. Why (from G1/G2):
- The 4 languages CALM supports are the only ones with a **published**
  `tree-sitter-stack-graphs-*` crate. Kotlin/Swift/Scala/Dart have **none**.
- Real options, none cheap: (a) find + security-vet a community crate per language
  (sparse, unmaintained); (b) author a full name-binding `.tsg` grammar from scratch
  per language (research-grade, weeks each, ongoing maintenance as grammars evolve).
- **Cheaper alternative for the same goal (no-SCIP language accuracy):** keep
  **generalizing A′ (arity gate)** — the Go pass in `dfe6ef4` is a proven template
  (widen `count_arguments_node`, add a `<lang>_def_arity`, populate `symbols.arity`,
  loosen the guard with per-language soundness). Covers a no-SCIP language in days,
  not weeks, and needs no external crate. **Recommendation: pursue A′-continuation
  before B.**

---

## 5. Sequencing & effort

| Order | Item | Effort | Risk | Gated on |
|---|---|---|---|---|
| 1 | **F/B7 Phase 1** (fd + flask, scripted arms) | Low–Med (reuses B11+B12 infra, but adds real per-corpus build/test) | Low | — |
| 2 | **F/B7 Phase 2** (gin, express/zod) | Low–Med | Low–Med (npm network precondition) | Phase 1 validated |
| 3 | **Agent-loop harness** | Medium | Medium (SDK specifics) | `claude-api` skill |
| 4 | **F/B7-LLM-driven + B8** (Haiku vs Sonnet) | Medium | Medium (cost, nondeterminism) | harness + API key |
| 5 | **D — local-ONNX backend (`tract`)** | Medium | Medium (confirmation spike, §3.2 — de-risked by evidence, not a blind bet) | tract musl confirmation spike |
| 6 | **F/B7 Phase 3** (spring-petclinic/Java) | Med–High | Med–High (Maven+JVM setup/flake) | Phases 1–2 validated |
| — | **B — stack-graphs** | High | High | ecosystem (deferred) |
| — | **A′-continuation** (alt to B) | Low–Med | Low | — (recommended over B) |

## 6. Decisions locked vs. still open

**Locked this session (§0.1, with evidence where requested):**
1. B7 corpus — per-language, phased (fd/flask → gin/express-zod → spring-petclinic).
2. D runtime — universal, `tract` (musl-confirmed) over `ort` (glibc-tied, ruled out) or `candle` (unverified).
3. B8 model pair — Haiku vs Sonnet.
4. tasks.yaml — new `refactor_tasks.yaml`, evidence: schema shape mismatch + zero blast-radius on B4/B6/B11's existing consumers (§1.3a).

**Genuinely still open (need a call before code, not researchable further from inside the repo):**
1. Exact refactor-task count/symbols per corpus for B7 Phase 1 (needs picking
   real symbols in fd/flask with the right shape — multi-callsite, not
   generic-named — mirroring B12's own `sample_distinctive` filter).
2. B8 budget ceiling (USD) and repeat count N (recommended N≥3–5 per §2.2, exact N is a cost/confidence tradeoff call).
3. Whether Phase 3 (Java/Maven) is worth the setup cost at all vs. stopping at 4 languages — revisit after Phase 1–2 results are in hand, not decided speculatively now.

> **Next gate:** on approval, set `SPEC_APPROVED: true` → triggers `audit-design`
> (risk audit) → `writing-plans` (task-level plan). No code before then.

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-30 | trigger: NORMAL -->

**Tier:** 2 (D ships an opt-in but production code path in `embedding.rs`; F/B7-B8 are internal benchmark tooling, Tier 1 on their own — Tier 2 taken as the ceiling for the combined spec) | **Date:** 2026-07-30

### Failure Modes
1. **B7 Phase 1's pinned corpora (fd/flask) may not build/test hermetically in a fresh clone** — Cargo needs a crates.io fetch (`cargo fetch`/`build`) and Python's flask test deps need `pip install` unless vendored; the spec explicitly calls out npm's network need (Phase 2) but never checks this for Phase 1's own Rust/Python corpora. — **MED-HIGH** — mitigation in plan: **NO** (must add: verify each corpus's test suite runs green, offline-or-not, before Phase 1 starts)
2. **Build+test-pass is necessary but not sufficient** — a picked symbol whose callsites aren't exercised by the corpus's own test suite would let both arms "pass" even with a genuinely missed callsite, making that task uninformative. — **MED** — mitigation in plan: **PARTIAL** (the dual-oracle design — build/test + independent callsite recall — catches this only if callsite recall is computed correctly and the symbol is chosen well; symbol selection itself is still an open question per §6)
3. **`tract`'s musl-compatibility evidence is being conflated with model-op-coverage evidence** — tract runs *on* musl, but whether it can execute the *specific* ONNX graph of a modern code-embedding transformer (attention variants, dynamic shapes, newer opset ops) is a completely different, unverified question. The spec's §3.2 "confirmation, not open bet" framing understates this. — **HIGH** — mitigation in plan: **NO** (must add: verify op-coverage for the actual candidate model — e.g. `nomic-ai/CodeRankEmbed` exported to ONNX — under `tract` specifically, before any further D work; if it fails, evaluate a smaller/simpler architecture or fall back to `candle` with its own spike)

### Layer Signals
- **L3 Data:** Switching `Static`→`Onnx` backend changes the embedding *space*, not just dimension. Existing `heal_dimension_mismatch` only catches a dimension change — two different models could coincidentally share a dimension and silently produce cross-space cosine comparisons against stale vectors during a partial reindex. **Needs explicit invalidate-all-on-backend-switch logic**, not just the dimension check.
- **L6 Observability:** No pre-refactor baseline test run is specified. Without one, a corpus's own pre-existing flaky/failing test gets misattributed to "the arm broke it." **Add: run each corpus's test suite once before any refactor, on the fresh clone, and require it green before counting a post-refactor failure as arm-caused.**
- **L2 Concurrency:** Running B7 Phases in parallel (not stated either way) risks the same class of shared-`target/`/shared-build contention this project has hit before in the *server* (see memory: multi-client WAL bloat) — now a risk for the *benchmark harness* instead. No signal that this was considered.
- L1/L4/L5/L7: no signal beyond what's already addressed in the spec (L4's HF-download error handling should just mirror the existing `Embedder::load` fallback idiom; L5's npm/registry supply-chain exposure is inherent to using real `npm test`, not new).

### Assumptions to Verify
- **ASSUMED:** B12's 6 pinned corpora build+test cleanly — B12 only ever *reads* them; this design is the first to require them to *build and pass tests*, and that was never checked.
- **ASSUMED:** `tract` can execute the chosen code-embedding model's actual ONNX graph (Failure Mode 3) — evidence found was musl-general, not model-specific.
- **ASSUMED:** B7's per-corpus target symbols are real hubs / have real multi-callsite shape — B11's own anchor was *live-verified* via `hotspots` before use (documented inline in B11); B7's §1.3 task list doesn't yet commit to the same verification for the new corpora.
- **ASSUMED:** N≥3–5 repeats is "enough" for B8 — a reasonable default, not derived from any power calculation; treat as a starting point to revisit if variance is high.

### Abductive Hypotheses
1. **Hub-gate task × fresh-clone-per-run interaction:** the `narrow_a_hub` task assumes CALM's risk gate fires on the picked symbol, but hub/coreness scores are graph-structural and computed fresh per clone with no prior history — the symbol's hub status was never independently re-verified for the *new* per-language corpora the way B11 verified its own self-repo anchor. Two individually-correct pieces (CALM's real hub-gate logic + B7's correct fresh-clone design) combine into an unverified assumption.
2. **Resource contention at full scale:** running B7 (multi-language builds) and B8 (concurrent LLM agent-loop calls) in the same environment/timing budget risks the exact resource-contention failure mode already seen in this project's *server* (multi-client WAL bloat, orphaned children — see memory) recurring in the *benchmark harness* instead, manifesting as spurious "arm A beat arm B" noise that's actually contention, not signal.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
**PASS WITH FLAGS** — the overall design (per-language B7, `tract`-based D, deferred B) is sound and proceeds. Execution must fold in, as concrete first steps rather than a separate planning doc:
1. Verify each Phase-1 corpus (fd, flask) builds+tests green on a fresh clone before writing any task against it (closes Failure Mode 1).
2. Run each corpus's test suite once pre-refactor to establish a green baseline (closes L6).
3. Verify the actual candidate embedding model's ONNX graph runs under `tract` — not just musl-general evidence — before any further D work (closes Failure Mode 3, the highest-severity open item).
4. Add embedding-space invalidation on backend switch, not just dimension-mismatch healing (closes L3).
5. Live-verify hub-status of any B7 `narrow_a_hub` target symbol via `hotspots`/`edit_context` before relying on it, mirroring B11's own discipline (closes Abductive 1).
