# B15 — Cross-Language Competitor A/B (`calm` vs CodeGraph vs Ctxo vs Context+)

Extends [B13](../b13_codegraph_multirepo_ab/README.md) (real CALM-vs-CodeGraph A/B, but only
2-3 corpora) to **all 6 of CALM's Tier-0 languages** — reusing
[B12](../b12_tier1_tier2_tool_correctness/README.md)'s corpus registry verbatim (fd/Rust,
flask/Python, gin/Go, express/JS, zod/TypeScript, spring-petclinic/Java) — and adds **2 new real
competitors**, both live-verified against a real spawned MCP server in this repo's own sandbox
before being wired in (schemas, tool names, and setup quirks below were discovered by calling the
tools, not by reading their READMEs):

- **[Ctxo](https://github.com/alperhankendi/Ctxo)** (MIT, npm) — has its own PreToolUse safety
  gate (`ctxo gate`), directly contesting CALM's "only tool with a pre-edit gate" framing.
- **[Context+](https://github.com/ForLoopCodes/contextplus)** (MIT, npx, 1971★) — has its own
  memory/RAG tools, directly contesting CALM's "only tool with cross-session memory" framing, and
  opens its own README with an unqualified **"99% accuracy"** claim with zero methodology — exactly
  the marketing anti-pattern the "Nghiên cứu competitor" section of the top-level
  [`benchmarks/README.md`](../README.md) already warns against repeating.

CodeGraph's adapter (`codegraph_callers`, version pin, `CODEGRAPH_MCP_TOOLS` env) is reused
verbatim from B13 — unchanged, still on npm latest `1.5.0` as of this run.

## Task measured

File-recall on "who calls this symbol" — the same shape B13 already used, now driven through 4
different real tool schemas:

| Tool | Call | Notes |
|---|---|---|
| `calm` | `callers(symbol, path)` | structured JSON; file lives inside a qualified `symbol` string, not a separate field (B13 already found this the hard way) |
| CodeGraph | `codegraph_callers(symbol, file)` | free-text response, paths extracted via regex |
| Ctxo | `search_symbols(pattern)` → `find_importers(symbolId, edgeKinds=["calls"])` | **two** real MCP calls — `find_importers` needs a `symbolId` (`file::name::kind`), not a bare name |
| Context+ | `get_blast_radius(symbol_name, file_context)` | free-text response (`"  path.ext:\n    L<n>: ..."`), paths extracted via regex |

Oracle: B12's `ground_truth.py`, reused verbatim — word-bounded `git grep` + independent
definition-regex extraction, **including 3 real oracle bugs found and fixed while building this
exact benchmark** (see "Bugs found building this" below) on top of the 2 found auditing CALM on
2026-08-18 (symbol-collision in B2's SCIP oracle, string-literal false positives in this same
`git_grep_call_sites`).

## Scope limits — read before citing any number from this benchmark

- **Ctxo has no plugin for python or rust.** Verified live, not assumed from its docs: `ctxo
  install python` and `ctxo install rust` both **404 on the real npm registry**
  (`@ctxo/lang-python`, `@ctxo/lang-rust` don't exist). Silently skipped on those 2 languages —
  an absent row is not the same claim as a losing row, and is recorded as
  `arms_skipped_unsupported` in `results.json`, not hidden.
- **Every tool answers a slightly different question under the shared "file-recall" label.**
  CodeGraph free-texts an impact summary; Ctxo's `find_importers` is edge-typed (asked for
  `edgeKinds: ["calls"]` specifically, but its underlying resolver may still have its own notion
  of what counts); Context+'s `get_blast_radius` explicitly documents itself as "usages," not
  "calls" — and (see below) was caught doing plain substring text search, not symbol-aware
  matching, on at least one real query. Read the raw per-symbol rows in `results.json` before
  treating any aggregate percentage as a clean ranking — same caveat B11's own README gives for
  its raw token-ratio numbers.
- **Single pass** (`--n-repeats 1` by default) per symbol per corpus in this run. `--n-repeats 3`
  is available and recommended before treating any one row as final, matching B13's discipline.

## Bugs found building this (verified live, not asserted)

Three real, previously-undocumented ground-truth bugs were found and fixed *while building this
benchmark*, before any published number — same "audit the oracle before trusting a miss" discipline
as the 2026-08-02 and 2026-08-18 fixes to this same file:

1. **Java method-definition pattern required an explicit access modifier.** Package-private
   methods — the standard JUnit 5 convention for test methods (`void testFoo() { ... }`, no
   `public`/`private`/`protected`) — were never recognized as *definitions*, so a test method named
   after the production method it exercises (e.g. `void initUpdateOwnerForm() throws Exception`
   testing `OwnerController.initUpdateOwnerForm()`) got miscounted as a real *call site* of the
   production method. Verified live on spring-petclinic: CALM, CodeGraph, **and** Ctxo all scored
   0/1 "missing" a call that was never real — the oracle's sole "hit" was the test method's own
   declaration line. Fixed: the modifier group is now optional, same as the pre-existing
   class/interface patterns in the same file already treat it.
2. **Ctxo's plugin installer hard-requires a `package.json`, even for non-npm languages.**
   Verified live on spring-petclinic (pure Maven, zero npm anywhere in the repo): `ctxo install
   java` refused outright with "No package.json in the current project" — its plugin-install
   mechanism always shells out through npm, unconditionally, regardless of which language plugin
   is being installed. Worked around with a throwaway `package.json` written before setup and
   removed immediately after (exactly what a real user hitting this on a Java-only repo would do).
3. **`sample_symbols` returned 0 candidates on one otherwise-healthy run**, never reliably
   reproduced (the same corpus produced 8 real candidates seconds later when queried by hand) —
   not disk pressure (21GB free at the time), smells like a transient subprocess/IO hiccup. Added
   a one-shot retry rather than silently reporting a corpus as sample-less.

## Real finding, not a harness bug: Context+'s `get_blast_radius` false-positived on plain substring text

Live-reproduced, not inferred: querying `get_blast_radius(symbol_name="print", file_context=
".../PetTypeFormatter.java")` on spring-petclinic returned **16 usages in 3 files**, one of which
was `src/main/resources/static/resources/css/petclinic.css` — matching literal CSS text like
`print-color-adjust: exact;` and `.d-print-inline-block`. This is plain substring text matching
conflating a Java method name with unrelated CSS class names that happen to contain the same
letters, not symbol-aware call-graph analysis. It doesn't cost Context+ any *recall* in this
benchmark's scoring (the real oracle file was still present in its returned set, so it still scores
a hit) — but it's a real, disclosable precision problem, worth weighing against the "99% accuracy"
claim on Context+'s own README, and worth reading the raw `contextplus_files` column for, not just
the recall fraction.

## Results (2026-08-18, `calm` @ `5b61d55` + the `new`-expression fix below, N=8 symbols/corpus, single pass)

File-recall on "who calls this symbol", per language (hit/total oracle files):

| lang | calm | CodeGraph | Ctxo | Context+ |
|---|---:|---:|---:|---:|
| python | 9/9 (100%) | 9/9 (100%) | *(no plugin — see below)* | 9/9 (100%) |
| rust | 9/9 (100%) | 9/9 (100%) | *(no plugin — see below)* | 9/9 (100%) |
| go | 11/11 (100%) | 10/11 (91%) | **1/11 (9%) — see disclosure below, not a capability claim** | 11/11 (100%) |
| javascript | 8/8 (100%) | 8/8 (100%) | **0/8 (0%) — see disclosure below, not a capability claim** | 8/8 (100%) |
| typescript | 11/11 (100%) | 9/11 (82%) | **1/11 (9%) — see disclosure below, not a capability claim** | 11/11 (100%) |
| java | 25/25 (100%) | 24/25 (96%) | 23/25 (92%) | 25/25 (100%) |
| **aggregate (java only for Ctxo — see below)** | **73/73 (100.0%)** | **69/73 (94.5%)** | **23/25 (92%)** | **73/73 (100%)** |

**This table supersedes both earlier published runs**: run 1 (calm 68/72 = 94.4%, below CodeGraph's
69/72 = 95.8%) and run 2 (calm 72/73 = 98.6%, ahead of CodeGraph but with 1 known miss left) — see
"Root-cause investigation" below for both fixes involved. Note the java column's *sample itself*
changed between run 1 and run 2 (fixing the oracle bug changed which 8 symbols `sample_symbols`
deterministically picks — the symbol names `getName`/`isNew` that motivated the first investigation
are no longer in this particular sample, but the fix they drove is independently verified live in the
investigation section, not just inferred from a table moving). Run 2 → this run is a like-for-like
resample (same fixed oracle, only the resolver changed), so the javascript `7/8 → 8/8` move is
directly attributable to the `new`-expression fix, not a sampling artifact.

**Read the "Ctxo go/js/ts: a real, unresolved integration-reliability finding" section below before
citing the go/js/ts Ctxo numbers for anything** — they are published for transparency (raw data in
`results.json`), but this benchmark's own investigation could not confirm they measure Ctxo's real
call-graph quality, so the aggregate row above excludes them and counts only Ctxo's java result
(the one arm verified end-to-end with real, non-empty query results).

**Reading the rest of the table**: `calm` now has **zero misses across all 6 languages (48 sampled
symbols)** in this run — the javascript `User` case (invoked exclusively via `new User(...)`) that was
`calm`'s last remaining gap is fixed (see "Round 2" below). CodeGraph misses 4 files total (go/1,
typescript/2, java/1 — not investigated further here); Context+ misses none in this sample (see the
CSS false-positive section below for why "0 misses" doesn't mean "flawless" — it's a precision
question this recall-only task structurally can't see). Ctxo's java column is unchanged from the
previous run (23/25, 92%) — this run's fix doesn't touch anything Ctxo-relevant.

## Root-cause investigation (2026-08-18): why did `calm` initially score below CodeGraph?

The first published run above showed `calm` (94.4%) narrowly behind CodeGraph (95.8%) and well
behind Context+ (100%). Investigated end-to-end — not just re-read the numbers — by reproducing
`calm`'s exact misses against a live corpus, inspecting its `call_edges` table directly, and building
a minimal 2-class Java repro. Three distinct things were found, not one:

1. **A benchmark-design property, not a bug**: this task measures *recall only*, never precision.
   Context+ and CodeGraph both lean toward "return more, don't worry about false positives" —
   Context+'s `get_blast_radius` was caught doing plain substring text matching (see the CSS
   false-positive section below), which trivially maximizes recall at zero precision cost under this
   scoring. A recall-only metric structurally favors that strategy over a resolver that tries to stay
   precise. Not fixed here (would need a precision column, e.g. `len(tool_files - oracle_files)`, to
   fairly separate "found the real callers" from "returned everything and hoped").
2. **A real, freshly-introduced oracle bug** (same file this benchmark's own ground truth lives in):
   the 2026-08-18 fix that made Java's method-definition regex's modifier group optional (to catch
   package-private JUnit test methods, see `ground_truth.py`'s `PATTERNS["java"]` comment) turned that
   pattern into an accidental near-universal matcher for control-flow lines ending in a brace —
   `if (pet.isNew() && ...) {` matches "modifiers?, some text, NAME(...) {" with NAME captured as the
   keyword "if" itself. `_looks_like_a_definition` then wrongly excluded every such line from call-site
   ground truth. Verified live: `isNew`'s real oracle should have been 3 files
   (`Owner.java`/`PetController.java`/`PetValidator.java` — every OTHER tool in this benchmark found
   all 3) but had collapsed to 1. Fixed in `ground_truth.py` by reusing the same `_NOT_A_NAME` keyword
   set `extract_definitions` already filters against, applied to the definition pattern's own captured
   group instead of just "did any pattern match at all".
3. **A real, previously-undocumented bug in `calm`'s own core resolver** — the one that actually moved
   the table above. Root-caused to `crates/calm-core/src/indexer/pipeline.rs::resolve_sites_to_edges`:
   when a call site's receiver has a STATICALLY KNOWN type (`target_class` populated from a tracked
   binding — Java's `formal_parameter`, Go's `parameter_declaration`, etc.), the code looked up
   `ctx.by_name_class.get(&(callee, class))` — keyed on the method's *exact declaring class*, with no
   superclass walk — and on a miss returned **zero candidates**, dropping the call edge entirely,
   with none of the `ambiguous`-fan-out fallback an UNKNOWN-type receiver already gets. So a call to an
   *inherited* method (`getName()`/`isNew()` declared on `NamedEntity`/`BaseEntity`, invoked through a
   `Pet`-typed parameter) silently vanished — while the identical call through a same-type LOCAL
   variable (untracked, so genuinely "unknown" to the resolver) correctly fell back to `ambiguous`.
   **Knowing more about the receiver's type made `calm` strictly worse at finding the edge** — the
   opposite of the intended tiered-confidence design. Verified with a minimal, isolated 2-class Java
   repro (`Base`/`Sub`/3 receiver shapes) before touching any real code, confirmed as the root cause of
   3 of `calm`'s 4 original misses via direct `call_edges` inspection, and fixed by falling back to the
   unscoped `by_name` lookup — but ONLY when `cls` is itself a symbol this project actually declares
   (`ctx.by_name.contains_key(cls)`), so a receiver typed as an unmodeled external/stdlib type (Rust's
   `HashMap::new()` with no `HashMap` anywhere in the project) keeps the original "no candidates"
   behavior rather than wrongly fanning out project-wide — caught live by
   `test_type_path_call_resolves_scoped_not_fanned_out` regressing when the fallback was first tried
   unguarded. New regression test:
   `test_java_formal_parameter_resolves_inherited_superclass_method` (`pipeline.rs`). Full
   `cargo test -p calm-core --lib` (1234 tests) and `cargo test -p calm-server --lib` (395 tests): all
   green, 0 regressions. Re-verified live post-fix against the real spring-petclinic corpus: `getName`
   now correctly includes `Owner.java`/`PetController.java`, `isNew` now correctly includes all 3 real
   files.

**Net effect (run 1 → run 2)**: `calm` moved from 94.4% (behind CodeGraph) to 98.6% (ahead of
CodeGraph, within 1.4 points of Context+'s recall-maximizing ceiling) — with its one remaining miss
being the already-known, separately-tracked `new`-expression gap, not the inheritance bug. This is
also a broader finding than B15 itself: `by_name_class`'s inheritance blindness is generic (any
language populating `target_class` — Go, Rust, TS, C#, PHP, C/C++ — could hit the same shape), not
Java-specific; only Java's was live-confirmed here.

## Round 2 (2026-08-18, same day): fixing the `new`-expression gap too

After closing the inheritance bug above, the ONE remaining `calm` miss across all 6 languages was
javascript's `User` — invoked exclusively via `new User(...)`. Investigated whether this was worth
fixing and, if so, how — verified against the real vendored tree-sitter grammars CALM links (not
guessed from memory):

- **Root cause, confirmed via `node-types.json`** (`tree-sitter-javascript-0.23.1`,
  `tree-sitter-typescript-0.23.2`): `new Foo(...)` parses as its own `new_expression` node kind,
  entirely distinct from `call_expression` — `JS_TS_CONSTANTS.call_node_types` only listed
  `["call_expression"]`, so `walk_calls` never matched a `new_expression` at all, and no `RawCall` was
  ever emitted for it. `new_expression` has a required `constructor` field (the callee, same shape
  `split_receiver_callee` already handles) and an optional `arguments` field — same field NAME
  `call_expression` already uses, so `count_arguments_node` needed no change.
- **Fix**: reused the codebase's existing extension point for exactly this situation
  (`call_function_field_by_kind`, already used for PHP/Ruby/Kotlin/Swift/Dart's own non-standard call
  shapes) — a pure data change, zero new code paths:
  `call_node_types: &["call_expression", "new_expression"]`,
  `call_function_field_by_kind: &[("new_expression", "constructor")]`.
- **Bundled the same fix for Java** in the same pass, after verifying it's equally safe: Java's
  `new Foo(...)` is `object_creation_expression`, whose callee lives in a `type` field (not
  `constructor`) — confirmed via `tree-sitter-java-0.23.5`'s `node-types.json`. Its `arguments` field
  is actually named `argument_list`, but `count_arguments_node` already checks
  `"arguments" || "argument_list"` (pre-existing, unrelated to this fix). The one real risk checked
  before committing: does a *generic* constructor call (`new ArrayList<String>(...)`) corrupt the
  callee name with the `<String>` text? No — `leading_ident` (used by `split_receiver_callee`) already
  stops at the first non-identifier character, verified by reading its implementation, then confirmed
  live by a dedicated test. One genuine, pre-existing wrinkle surfaced by this test: Java indexes a
  class's own name (`Box`, kind=class) and its constructor's name (`Box`, kind=constructor) as two
  SEPARATE same-named symbols, so a bare-name constructor-call resolution correctly returns both as
  `ambiguous` candidates rather than picking one — not a new bug, a pre-existing characteristic of
  Java's symbol model this fix newly exercises, left as-is (disambiguating it further was out of
  scope).
- **Verification**: 3 new regression tests (`test_javascript_new_expression_resolves_as_a_call_to_the_
  constructed_class`, `test_typescript_new_expression_with_generics_resolves_as_a_call`,
  `test_java_object_creation_expression_resolves_as_a_call_to_the_constructed_class`, all in
  `pipeline.rs`). Full `cargo test -p calm-core --lib` (1237 tests, +3 from Round 1) and
  `cargo test -p calm-server --lib` (395 tests): all green, 0 regressions. Re-ran the full 6-language
  B15 sweep: javascript `User` now resolves correctly (`1/1`, `examples/view-locals/user.js`) —
  **`calm` reaches 73/73 (100%) aggregate, tied with Context+'s recall-maximizing ceiling and ahead of
  CodeGraph (94.5%), with zero misses across all 48 sampled symbols** — the difference being `calm`
  gets there via a real resolver, not substring text matching (see the Context+ CSS false-positive
  section below for what that strategy costs in precision, a dimension this recall-only task doesn't
  measure).
- **Deliberately NOT bundled this pass**: C++ (`new_expression`), C# (`object_creation_expression`),
  Kotlin/Swift's own construction syntax likely share the same gap, but their grammar crates aren't
  fetched under this repo's default build features (`lang-cpp`/`lang-csharp`) and weren't
  live-verified against a real `node-types.json` this session — flagged as a real, likely-present,
  unconfirmed gap for a future pass, not silently assumed fixed.

**The benchmark-design finding (point 1 above) also answers a broader question worth stating
plainly**: this task doesn't exercise `calm`'s actual differentiators at all. "File-recall on who
calls X" is the one axis where a tool that returns more, unfiltered, structurally cannot lose — it
says nothing about the pre-edit safety gate, cross-session memory, or token-efficiency tasks B11
already measures (and where CodeGraph/Context+ don't compete at all). A benchmark built to showcase
`calm`'s strengths specifically would weight those differently, not just recall on one call-graph
query shape.

## Ctxo go/js/ts: a real, unresolved integration-reliability finding

Not a harness bug (ruled out through 3 independent rounds of increasingly rigorous verification,
each one changing the setup code and re-running) and not (as far as this investigation could tell)
an oracle bug either. Documented in full because burying an inconvenient result is exactly what
this benchmark suite exists to not do:

1. **Round 1**: raw numbers were go 1/11, javascript 0/8, typescript 1/11, java 23/24. Hypothesis:
   `npm install -D @ctxo/lang-<x>` silently failing to materialize `node_modules` on larger
   real-dependency-tree corpora. Fixed (verify + retry) — numbers **did not change**.
2. **Round 2**: direct SQLite inspection of `.ctxo/.cache/symbols.db` after a run that printed
   "[ctxo] Building codebase index... Found 141 source files" / "Index complete: 141 files indexed"
   found **all 3 tables (`files`, `symbols`, `edges`) empty (0 rows)** — the CLI's own stated
   progress does not match what it actually persisted. Also found the CLI prints its real progress
   to **stderr, not stdout** (a genuine bug in the first fix's own verification logic, which only
   checked stdout). Fixed (read both streams, require a `package.json` marker file specifically —
   not just the containing directory — before treating the plugin install as real, retry up to 3x).
   Re-ran the full 6-language sweep: numbers **still did not change** — go 1/11, javascript 0/8,
   typescript 1/11, java 23/24, byte-for-byte identical to before the fix, despite setup metadata
   now unambiguously showing a real, complete, verified index build (`plugin_materialized: true`,
   `indexed_ok_marker: true`, real per-language file counts in the captured output).
3. **What this rules in/out**: the failure is reproducible, stable across 2 independent full runs
   with materially different (and progressively more careful) setup code, and specific to the
   `@ctxo/lang-typescript` (covers both javascript and typescript) and `@ctxo/lang-go` plugins —
   `@ctxo/lang-java` consistently works (23/24, and CALM/CodeGraph/Context+ all score normally on
   the exact same go/js/ts corpora in the exact same run, which rules out a corpus-level or
   MCP-transport-level problem). The pattern (small throwaway-package.json corpus works, larger
   real-dependency corpus doesn't) is consistent with an async persistence race in Ctxo's own
   indexer specific to those 2 plugins, but this investigation could not pin the exact mechanism
   within reasonable scope, and does not have access to Ctxo's own source to confirm.
4. **Why this isn't scored as "Ctxo fails on go/js/ts"**: a 0-9% recall number here would be
   measuring "did this specific CLI+MCP-server pipeline reliably persist an index in this sandbox,
   for these 2 plugins, on this run" — not "how good is Ctxo's call-graph resolution once it has a
   working index" (java's 23/24 answers that question much better). Publishing the low number as a
   capability claim would be exactly the kind of misleading, oracle-unaudited result this whole
   benchmark suite's own house rules (see `benchmarks/README.md`'s "Nghiên cứu competitor" section
   on the Semgrep 250%-vs-50-71% lesson) argue against repeating in the other direction.

## Run

```bash
cargo build --release -p calm-cli   # default features already full power, nothing extra needed
benchmarks/.venv/bin/python benchmarks/b15_cross_lang_competitor_ab/run_benchmark.py \
  --langs python,rust,go,javascript,typescript,java \
  --arms codegraph,ctxo,contextplus \
  --n-repeats 1
```

`--langs`/`--arms` accept comma-separated subsets for a faster partial run (e.g. `--langs java
--arms contextplus` for a single-corpus dry run). `.work/<lang>` corpora are thrown away after each
language's pass unless `--keep-corpus` is set; `results.json` is written incrementally, one
language at a time, so a crash partway through still leaves every completed language's data intact.

## Version pins

- CALM: whatever `--calm-bin` points at (default `target/release/calm`) — `results.json`'s
  `meta.calm_git_sha` records the exact commit independently of `calm --version` (which only prints
  the Cargo.toml package version, ambiguous across unreleased commits — B13 already learned this
  the hard way).
- CodeGraph: `@colbymchenry/codegraph@1.5.0`, pinned explicitly in every spawn (not a bare package
  name — B13 found the bare form can resolve to a stale npx cache).
- Ctxo: `@ctxo/cli@0.11.4` (latest on npm as of 2026-08-18).
- Context+: `contextplus@1.0.8` (latest on npm as of 2026-08-18).

Exact pins for the run in "Results" above (`calm_worktree_dirty_at_run: true` — the not-yet-committed
`new`-expression fix/tests/this README edit were themselves uncommitted working-tree changes at run
time, committed immediately after):

| | |
|---|---|
| calm | `5b61d55314e625c945b7e4c9a78c8f1e667f93f` (base commit; `new`-expression fix layered on top, see commit after this README's own) |
| python (flask) | `36e4a824f340fdee7ed50937ba8e7f6bc7d17f81` |
| rust (fd) | `41532d114e2ba565fb5367d606c111b29b96450c` |
| go (gin) | `34dac209ffb6ef85cc78c5d217bbb7ad001d68fd` |
| javascript (express) | `a3714473feb3d2908add734d340e7755fd85e0a3` |
| typescript (zod) | `912f0f51b0ced654d0069741e7160834dca742ee` |
| java (spring-petclinic) | `51045d1648dad955df586150c1a1a6e22ef400c2` |
