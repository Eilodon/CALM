---
title: "Product-uplift proof & evidence/policy roadmap — audited & corrected"
date: 2026-08-20
status: >
  Independent report ("Báo cáo độc lập: Lý do và kế hoạch nâng cấp CALM") audited against live
  source this session. Verdict: unusually accurate — every checkable factual claim (snapshot
  counts, tool existence, verify_change/batch_status/risk-taxonomy limits, the classify_gate vs
  RiskVector split, B7's Express/Zod failure numbers, reference_impact's origin) matched live code
  or KNOWN_LIMITATIONS.md/CHANGELOG.md verbatim. No factual errors found. Two corrections applied
  below (§1): the report's Phase 1 overlaps ~1:1 with an existing, partially-shipped plan it didn't
  cite; its Phase 2's "change lifecycle" already has a plan/review half shipped in 0.7.0. This doc
  keeps the original's structure and priority order, folds in the corrections, and adds the actual
  execution record for Phase 0 item 1 (B7-v2) below. **B7-v2 executed and run across all 6 tasks
  this session (§4-§6): a third `calm_v2` arm wiring `reference_impact` into the CALM rename
  workflow shipped, but the result is negative — it did NOT close either of B7's known Express/Zod
  gaps. Root-caused empirically, not assumed, in two full passes (§7 same-day, §8 next-day deeper
  audit): Zod's miss is a real, previously-undocumented indexer gap (`import_node_types` never
  walks JS/TS `export_statement`, plus a deeper multi-hop wildcard-barrel-reexport problem, §7.1).
  Express's miss was INITIALLY misdiagnosed in §7 as "the already-known parser.rs call-site gap" —
  §8 found the real, more severe root cause: `resolve_sites_to_edges`' same-language filter reads
  `ctx.path_lang`, which is built ONLY from files that have at least one extracted `symbols` row —
  a source file consisting entirely of calls inside anonymous callbacks (no top-level named
  function/class anywhere, e.g. any Mocha/Jest/Vitest test file using bare `describe`/`it`/`test`
  nesting) never gets a `path_lang` entry, so EVERY outgoing call from that file silently resolves
  to zero candidates — independent of confidence tier, receiver shape, or nesting depth. No
  regression on the other 4 tasks. `reference_impact`'s own source comment overclaims coverage for
  both cases — flagged for correction.
verified_against: HEAD 13bc2c6 (main), this session.
inputs:
  - "(source report, pasted by user this session — Vietnamese, no filename; this doc is its
     durable, corrected form, following the audit-and-supersede convention of
     2026-08-08-master-change-control-execution-blueprint.md)"
  - docs/plans/2026-08-19-evidence-architecture-execution-plan.md   # Phase 1 overlap — see §1.1
  - KNOWN_LIMITATIONS.md                                            # source of truth for §3.4-3.5's gaps
  - benchmarks/b7_task_correctness/README.md                        # B7 baseline this doc extends
---

# Product-uplift proof & evidence/policy roadmap

## §0. Executive verdict

The source report's central thesis — *CALM has a good technical foundation but hasn't proven its
largest claim (agent+CALM produces better patches than agent alone), and the fix is to close the
evidence→understanding→decision→change→verification loop before adding more tools/languages* — is
sound and matches what CALM's own internal docs already conclude independently (see §1.1).

Every factual claim I could check against live source was accurate, several to the exact line:

- Live snapshot numbers (459 files / 6,315 symbols / 14,708 edges / 8,170 fresh external proofs,
  version 0.7.0) — exact match to `indexing_status()` and `Cargo.toml`.
- `verify_change` = `cargo check` only, unsandboxed — matches `KNOWN_LIMITATIONS.md` §1 near-verbatim.
- `batch_status` = pure observability, no server-side ChangeSet grouping — matches
  `KNOWN_LIMITATIONS.md` §2 near-verbatim.
- Risk taxonomy = signature-change only, no comment-only/deletion/security-sensitive classes —
  matches `KNOWN_LIMITATIONS.md` §3 near-verbatim.
- `RiskVector` struct fields (`caller_count_level`, `is_hub`, `hub_kind`, `signature_changed`,
  `uncertain_zero_caller`, `risk_rule_floor`, `kind_mismatch`, `touches_manifest`,
  `touches_uncovered_code`) — exact match to `crates/calm-core/src/policy/model.rs:57-89`.
- `classify_gate` (calm-server/tools/edit.rs) is still the live write-gate for `edit_lines`/
  `edit_symbol`; `policy::evaluate()`/`RiskVector`/`PolicyDecision` is a parallel path used for
  `review_change`'s Human-tier refusal and for digest recording, kept in sync with `classify_gate`
  by parity tests, not by sharing one function. CALM's own code literally calls `classify_gate`
  "legacy" (`change.rs:598`) while still routing real blocking through it — the report's "two
  engines" framing is exactly right, down to that word.
- B7's Express (recall 0.667) / Zod (recall 0.5) failures, the CALM arm's reliance on
  `edit_context`'s `callers()` alone, and the 6-language corpus — exact match to
  `benchmarks/b7_task_correctness/README.md`'s results table.
- `reference_impact` was built specifically to close the Express/Zod gap — confirmed by the
  function's own source comment (`trace.rs:780-784`), which names both benchmark tasks. Git blame
  shows `reference_impact` landed 2026-08-04 (`60ff9c9`), **after** B7's last results run
  (2026-07-30, `dcef3d5`) — so the report's inference ("B7 is historical evidence, not yet a valid
  measurement of the current build") is correct, not overreach.

No claim was found to be fabricated, stale-but-presented-as-current, or internally inconsistent.

## §1. Corrections (additive, not contradicting the source report)

### §1.1 Phase 1 already has a partially-shipped plan — cite it, don't restart it

The report's Phase 1 ("Evidence, Visibility, Policy") independently arrives at the same diagnosis
as `docs/plans/2026-08-19-evidence-architecture-execution-plan.md`, which:

- Names `EdgeConfidence` a "6-variant scalar ladder" and confirms target identity
  (`FormalEdge.definition_symbol`) was discarded before reaching the graph — same P0 the report's
  §3.2 describes in different words.
- Has **already shipped** PR#6 (target-aware `ambiguity_groups`), PR#7 (definition_symbol
  threading), PR#8 (`target_type_qn`), PR#9 (call-site identity v3 + upsert-by-identity) — as of
  this session's HEAD (`13bc2c6`).
- Has PR#10 ("Evidence ledger v1") fully spec'd and **build-ready**, explicitly building on two
  ledger tables that **already exist** (`external_proofs`, `evidence_conflicts`) rather than
  starting from zero. It is currently deferred — not for lack of design, but because a scope
  correction was found live: `call_edges` has three separate writer modules (`edges.rs`,
  `scip/ingest.rs`, `lsp/overlay.rs`), not the one the original plan assumed, which changes the
  blast radius of the upsert work.

**Action:** treat Phase 1 as "resume PR#10" plus "surface the ledger through the API projection the
report describes (identity/provenance/strength/freshness/coverage/conflict)", not as new-from-zero
work. This changes effort estimate, not direction.

### §1.2 Phase 2 / Mục tiêu D — split what's shipped from what's missing

`plan_change` + `review_change` (shipped in 0.7.0, `CHANGELOG.md`) already deliver the
**intent → impact → plan → review** half of the lifecycle the report asks for in Phase 2A:
`ChangeIntent` is a durable, reviewable record; `ReviewAuthority` binds target scope + source/graph/
config/provider-state snapshot + caller-set digest + the `PolicyDecision` it was reviewed against.

What's genuinely missing — confirmed by `KNOWN_LIMITATIONS.md` §2, which the original report also
cites correctly — is strictly the **apply → partial recovery → verify → attestation** half: every
`edit_lines`/`edit_symbol`/`format_files` call is still its own independent `EditTransaction`, no
`PARTIALLY_APPLIED` status, no cross-file rollback plan. `batch_status` only aggregates counts over
caller-supplied `tx_id`s after the fact.

**Action:** no change to the report's Phase 2 design — just note in the plan that half the pipe
already exists, so ChangeSet work is "extend `plan_change`/`review_change`'s output into an apply
lifecycle", not a green-field object model.

### §1.3 Not checkable, immaterial

The report's "build lúc 02:19:45" timestamp is a runtime/CLI banner value at the time the source
report was written, not repo state — no git/DB artifact to verify it against, and it affects none
of the report's conclusions.

## §2. Re-sequenced priority (unchanged from the source report, corrections folded in)

```text
1. B7-v2 + real-agent pilot                         (Phase 0 — this doc executes it below)
2. Resume PR#10 (evidence ledger v1) + verdict/VisibilityHealth projection   (was: "build Phase 1")
3. Canonical PolicyDecision (retire classify_gate as the sole real gate, or
   formally promote it — currently two parity-tested paths)
4. ChangeSet apply/partial-recovery half (plan/review half already shipped)
5. VerificationBroker
6. LSP semantic refactor adapter
7. Cognitive router + search economics
8. Typed obligations memory (Fact/Decision/Hypothesis/Obligation/Risk/Debt —
   confirmed gap: `RememberOutput` today is flat topic/content + staleness/quarantine only)
9. Framework providers
10. Federation and language expansion
```

---

# Execution record — Phase 0, item 1: B7-v2

The source report's own answer to "if you can only start one thing": *build B7-v2 using
`reference_impact` and the full current write lifecycle, then run a real-agent pilot with a clean
control arm.* This section is that work, started this session.

## §3. Scope for this pass

Full real-agent pilot (24-40 tasks, Phase 0 §2-4 of the source report) is out of scope for one
sitting. What's in scope now, matching the report's own exit condition ("don't scale before the
harness is proven"):

1. Give B7's CALM arm a `reference_impact`-aware rename step, closing the exact gap B7 found.
2. Re-run the two known-regression tasks (Express `setCharset`, Zod `prettifyError`) first —
   these are the falsifiable claim.
3. Re-run the full 6-task suite once the regressions are addressed, to check for new regressions
   the wider `reference_impact` surface (textual-tier hits) might introduce (over-renaming risk).
4. Report the result honestly either way — this repo's own stated benchmark policy
   (`benchmarks/README.md`: "khi số đo ra ngoài kỳ vọng... báo cáo trung thực thay vì ẩn đi").

Full `plan_change`/`review_change`/`verify_change` lifecycle wiring (the report's "full write
lifecycle" ask) is noted as a follow-up in §6 — `verify_change` is Rust-only today (§1 of
`KNOWN_LIMITATIONS.md`), so it cannot gate the Python/JS/TS/Go/Java arms yet without the
cross-language verification abstraction that's already a separate, larger open item.

## §4. What was built

A third arm, `calm_v2` (`run_calm_arm_v2` in `benchmarks/b7_task_correctness/run_benchmark.py`),
added alongside the existing `naive`/`calm` arms (not replacing `calm`, so the original v1 numbers
stay reproducible for comparison). It calls `edit_context` exactly as v1 does, then additionally
calls `reference_impact` on the same symbol and unions its `must_change`/`likely_change` hits into
the rename set; `review`/`textual_only` hits are counted in the result row but deliberately not
auto-renamed (same caution the tool's own design and `AGENTS.md` already recommend — an
unreviewed textual match can be an unrelated same-named symbol, as this benchmark's own `slugify`
3-way-collision finding shows).

## §5. Results — honest, including the negative one

| task | lang | v1 (`calm`) recall/build | v2 (`calm_v2`) recall/build | regression? |
|---|---|---|---|---|
| rename_fd_pattern_matches_leading_dot | rust | 1.0 / True | 1.0 / True | no change |
| rename_flask_from_prefixed_env | python | 1.0 / True | 1.0 / True | no change |
| rename_gin_clean_path | go | 1.0 / True | 1.0 / True | no change |
| rename_petclinic_find_pet_types | java | 1.0 / True | 1.0 / True | no change |
| **rename_express_set_charset** | javascript | 0.667 / False | **0.667 / False** | **gap NOT closed** |
| **rename_zod_prettify_error** | typescript | 0.5 / False | **0.5 / False** | **gap NOT closed** |

The four previously-100%-recall tasks are unaffected — wiring in `reference_impact` introduced no
new over-renaming regression on this sample. But the headline result is negative: **adding
`reference_impact` to the CALM arm did not fix either of B7's two known failures**, contradicting
the optimistic reading in §0 above (and in `reference_impact`'s own source comment, which names
both tasks as gaps it closes). Root-caused both, not assumed:

**Zod (`prettifyError`)** — `reference_impact` reported `must_change_count: 0`. The two missing
files (`packages/zod/src/v4/classic/external.ts`, `.../mini/external.ts`) each re-export the
symbol via `export { …, prettifyError, … } from "../core/index.js";` — a real ES-module re-export
with a `from` clause. `crates/calm-core/src/indexer/imports.rs::import_node_types` (the tree-sitter
node-kind allowlist `extract_imports_from_tree` walks) lists only `["import_statement",
"variable_declarator"]` for `"javascript" | "typescript"` — **`export_statement` is never walked
at all**, for any JS/TS project, not just this one. So no `import_edges` row is ever created for
an `export { X } from 'y'` re-export, regardless of `reference_impact`. The two real call sites in
`error-utils.test.ts` (`z.prettifyError(...)`) *did* get a call_edge, but at `"ambiguous"`
confidence — `reference_impact` correctly buckets that as `review`, not `must_change`, so it's
counted but not auto-applied either. (`edit_context.callers()` still surfaced those same two call
sites for the v1/v2 base rename — that part of the recall was never the problem; both missing
files are exclusively the re-export gap.) This is a genuine, previously-undocumented indexer gap
distinct from the `KNOWN_LIMITATIONS.md`/plan items already tracked — filed as a follow-up, not
fixed in this pass (real fix = walk `export_statement` too, extend `parse_import`/`ParsedImport`
to a re-export shape, thread through `symbols_used`; non-trivial enough to be its own slice).

**Express (`setCharset`)** — `reference_impact` reported 0 `review` hits and 7 `textual_only` hits
for this symbol; `test/utils.js`'s `utils.setCharset(...)` (property access on a bare
`require('../lib/utils')` result) produced no call_edge at all, confirming the B7 README's original
finding stands unchanged: this is the pre-existing, already-flagged call-site-extraction gap in
`parser.rs` for a property-access call through a required module's bare identifier — orthogonal to
what `reference_impact`'s import-edge tier can address, since there's no import/re-export
statement here, just a call shape the parser doesn't extract an edge for yet.

**Correction to §0 and to `reference_impact`'s own source comment**
(`crates/calm-server/src/tools/trace.rs:780-784`): the claim that `reference_impact` "catches...the
exact gap behind" the Express/Zod misses is accurate for *some* bare re-export shapes (a plain
`import`-side reference) but not for the specific `export { X } from 'y'` shape zod actually uses,
and not for Express's case at all (a different bug class entirely — call-site extraction, not
import/export tracking). Recommend narrowing or correcting that comment in a follow-up commit once
the `export_statement` gap is fixed, so it stops overclaiming coverage it doesn't yet have.

## §6. Follow-ups (not done in this pass) — revised after §7's deeper audit

1. **The `export_statement` fix alone is NOT sufficient for Zod — see §7.1.** Walking
   `export_statement` in `crates/calm-core/src/indexer/imports.rs` is still necessary (it's a real,
   zero-coverage gap for any JS/TS project), but Zod's actual re-export is a **two-hop barrel
   chain** (`external.ts` → `core/index.ts` via a *named* re-export, then `core/index.ts` →
   `errors.ts` via a *wildcard* `export * from './errors.js'` that names no symbols at all). A
   single-hop `import_edges` row from the fix above would point `external.ts` at `core/index.ts`,
   not at `errors.ts` — `reference_impact`'s `WHERE to_path = ?1` (bound to the definition file)
   would still return nothing. Closing this needs *either* transitive resolution when building
   `import_edges` (propagate a wildcard re-export's target set through the chain to the real
   definition file) *or* a multi-hop traversal in `reference_impact`'s own query — a materially
   bigger indexer feature than the single-function fix originally scoped here. Re-run
   `rename_zod_prettify_error` after landing whichever shape of fix to confirm empirically, not
   assume — this file's own history (§7) is a live example of why.
2. **Express's property-access call-site gap** is a separate, pre-existing `parser.rs` issue (B7
   README's own "Next steps" already names it) — out of scope for the `reference_impact` wiring
   done here.
3. Full `plan_change`/`review_change`/`verify_change` lifecycle wiring into the CALM arm, and the
   24-40 task real-agent pilot with a clean control arm, remain not started — correctly gated by
   this section's own finding: don't scale the pilot on top of a benchmark whose flagship fix
   didn't land yet.
4. `benchmarks/b7_task_correctness/README.md` needs a v2 section reflecting the above (added
   alongside this doc).

## §7. Deep audit: is the benchmark itself trustworthy? (requested follow-up, same session)

Before trusting §5's negative result, the harness and its ground truth were independently
re-verified — adversarially, assuming the benchmark itself could be wrong, not just CALM.

**§7.1 — Multi-hop re-export chain (see §6.1 above).** Read `packages/zod/src/v4/core/index.ts`
directly: `export * from "./errors.js";` among 15 other wildcard re-exports, confirming the two-hop
barrel shape. This *upgrades* the finding from "one missing function" to "the real fix is
transitive import resolution," which changes the effort estimate materially — worth knowing before
anyone picks up follow-up §6.1 expecting a one-function patch.

**§7.2 — Re-ran both failing suites directly (bypassing the benchmark harness entirely)**, against
the still-on-disk `calm_v2`-renamed corpus copies, to rule out a harness bug or test flake
producing a false failure:
- Express: `npm test` → **5 failing, all `TypeError: utils.setCharset is not a function` at
  `test/utils.js:50/54/58/62/66`** — exact match to the original B7 finding's own description.
  1251 passing, 4 pending, 5 failing — a clean, non-flaky signal.
- Zod: `pnpm test` → **`TypeCheckError: '"zod/v4/core"' has no exported member named
  'prettifyError'`** at `external.ts:26:3` and `mini/external.ts:18:3`, plus 4 downstream test
  failures from the same cause. Exact match to the predicted root cause.

Both failures are real compiler/runtime errors, independently reproduced outside the benchmark
script, not an artifact of `oracle.py` or `score_arm`'s arithmetic.

**§7.3 — Name-collision / oracle-precision check** on the ORIGINAL (unmodified) pinned corpora —
does the "unique symbol" assumption these two tasks were picked under actually hold, and does the
oracle's plain-regex approach over- or under-count?
- `setCharset` in the pinned express corpus (`benchmarks/resolution/corpus/js`, commit
  `a371447`): appears in exactly 3 files — `lib/utils.js` (def), `lib/response.js` (import+call),
  `test/utils.js` (test) — an **exact** match to `oracle_callsite_files`, zero collisions, zero
  false positives from comments/strings.
- `prettifyError` in the pinned zod corpus (`benchmarks/resolution/corpus/typescript`, commit
  `912f0f5`): appears in exactly 4 `.ts` files (matching oracle exactly) plus 2 `.mdx` doc files —
  correctly excluded by `oracle.py`'s own extension filter (the same fix already documented for
  flask's `.rst` false positives). Zero collisions, zero false positives within the filtered set.

Both oracles are precise for these two tasks specifically — the negative result isn't an oracle
artifact.

**§7.4 — A real methodology nuance, not a bug: naive's recall is close to tautological by
construction.** `run_naive_arm`'s file-selection regex
(`rf"(^|[^A-Za-z0-9_]){{re.escape(symbol)}}($|[^A-Za-z0-9_])"` via `git grep -l`) and
`oracle.py::real_references`'s ground-truth regex are **near-identical** (same bare-identifier,
word-bounded pattern; oracle additionally filters by extension and excludes the definition line).
This means naive's "1.0 recall" isn't independent evidence naive's *approach* is good — it's close
to true by construction, since both the tool being scored and the scorer use the same textual
signal. The genuinely independent, informative signals in this benchmark are (a) CALM's recall,
which *does* differ meaningfully because it comes from a structurally different signal (the call
graph), and (b) `build_pass` on both arms, which is real compiler/test ground truth unrelated to
either regex. §7.2 already cross-validated `build_pass` directly. Recommend documenting this in
`oracle.py`'s own module docstring so a future reader doesn't read "naive: recall 1.0" as a
finding about naive's quality rather than an artifact of the harness's own construction.

**§7.5 — Corpus pinning.** Both corpora are read-only local git checkouts
(`benchmarks/resolution/corpus/{js,typescript}`) with `corpora.py::pinned_commit()` available to
report their HEAD, but **`run_benchmark.py` never calls it or records the commit in
`results.json`** — so a results file on its own doesn't self-document which exact upstream commit
produced it (only this doc does, by hand: express `a371447` 2026-07-27, zod `912f0f5` 2026-06-10).
Low-severity (the checkouts are never mutated in normal operation, so in practice this doesn't
drift), but worth a one-line fix: add `"pinned_commit": pinned_commit(lang)` to each task row.

**§7.6 — Statistical power of the "no regression" claim is weaker than §5 implies.** Checked
`calm_v2`'s reported touch-file set against v1's for `rename_petclinic_find_pet_types` (the one
previously-passing task where the raw JSON was still available, not yet overwritten by a later
run): **identical 6-file set — `reference_impact` contributed exactly zero additional files.**
The four "unaffected" tasks were selected (by B7's own original design) for being clean,
unambiguous cases specifically *because* they were the easy tier — so it's unsurprising
`reference_impact`'s extra signal was a no-op there rather than something that got exercised and
survived. "No regression observed on 4 tasks" is accurate, but should be read as "no counter-example
found in a small, easy-leaning sample," not as "the wider surface was stress-tested and passed."

**§7.7 — Verdict.** The benchmark's negative result (§5) holds up under adversarial re-audit — every
independently-checkable component (build/test ground truth, oracle precision, symbol uniqueness)
re-verified clean. The one thing that changed under scrutiny is the *depth* of the recommended fix
(§6.1: transitive re-export resolution, not a single-function patch) and the *strength* of the
"no regression" claim (§7.6: real but low-power on this sample). Recommend trusting §5's numbers;
do not yet trust "walking `export_statement` will fix Zod" as a scoped, one-PR follow-up — it needs
its own design pass first.

## §8. Express's REAL root cause (next-session deep dive — §7.2's diagnosis was itself incomplete)

User's follow-up question ("is there truly no way to fix Express — what limitation blocks it?")
prompted re-opening §7.2's Express conclusion instead of accepting it. §7.2 had re-confirmed the
*symptom* (`utils.setCharset(...)` invisible) but repeated the ORIGINAL B7 README's diagnosis
("parser.rs call-site extraction gap") without re-verifying it against current source. That
diagnosis turned out to be stale: a real fix for this *exact* case already exists and is committed
(`crates/calm-core/src/indexer/pipeline/extraction.rs`'s "whole-module require/import binding"
branch, git-blamed to `b96342b`, with its own regression test
`test_js_require_namespace_object_property_call_resolves_via_module_hint` in `pipeline.rs`, both
already on `main` before this session started). So the call-site *is* extracted and *is* correctly
resolved to `inferred` confidence with `module_hint: "utils"` — verified directly via
`.calm/index.db`'s `call_sites` table on a live reproduction. The bug is downstream of that,
and is more severe than the original diagnosis:

**Root cause, isolated via 8 controlled fixtures (single-variable changes each):**
`crates/calm-core/src/indexer/pipeline/reconcile.rs::resolve_sites_to_edges` filters every
candidate list by `Some(candidate_lang) == ctx.path_lang.get(caller_from_path)` (a same-language
correctness guard, `context.rs` line ~357). `ctx.path_lang` (`context.rs::build_resolution_context`)
is populated **only by iterating the `symbols` table** — `path_lang.entry(path).or_insert(lang)`
inside the loop over every symbol row. **A source file with zero top-level named
functions/classes anywhere in it — e.g. any JS/TS test file written in the standard
`describe(...)`/`it(...)`/`test(...)` nested-anonymous-callback style — contributes ZERO rows to
`symbols`, so it never gets a `path_lang` entry at all.** `ctx.path_lang.get(that_file)` then
returns `None`, `Some(candidate_lang) == None` is `false` for every candidate regardless of
correctness, `same_lang` ends up empty, and the function returns `Vec::new()` — **silently zero
edges for every single outgoing call that file makes, at any confidence tier, via any call
shape.** Confirmed this is not about nesting depth or receiver shape (both looked like plausible
explanations initially and were each ruled out): a same-file call from an identical anonymous
callback resolves fine (`repro7`, because the *callee* being in the same file incidentally gives
that file a `symbols`/`path_lang` entry); a cross-file call from a file with a real named function
elsewhere resolves fine (`repro2`/`repro4`); a cross-file call from a file with literally no named
declaration anywhere fails regardless of 1-level or 2-level anonymous nesting, bare call or
receiver call, `describe`/`it`/`test` (`repro5`/`repro6`/`repro8`, `callers()` reports zero direct
callers even at `resolved`-tier confidence). Express's real `test/utils.js` is exactly this shape
— 100% Mocha callback bodies, no named function anywhere in the file.

**This is a genuine bug, not an architectural ceiling — and it's concretely fixable.** The data
needed already exists: `file_index` (`crates/calm-core/src/indexer/pipeline/discovery.rs::
upsert_file_index`) records `(path, language, symbol_count, ...)` for **every** indexed file
regardless of symbol count — `build_resolution_context` just never reads it for this purpose.
Seeding `path_lang` from `file_index` instead of (or in addition to) `symbols` would very likely
close this whole class of gap in one change — not scoped/attempted this session, flagged as the
single highest-value follow-up, since it plausibly affects **every JS/TS/any-language test file
using anonymous-callback test frameworks project-wide**, not just this one B7 task. Not yet
verified whether Zod's `error-utils.test.ts` dodges this same bug because that file happens to
contain some other named declaration (giving it a `path_lang` entry) — plausible given zod's calls
did get an edge (`ambiguous` confidence) rather than zero, but not directly confirmed.

**Answer to "is this fixable, what's blocking it":** Yes, fixable, and now precisely scoped — not
a fundamental limit of CALM's textual/tree-sitter approach (unlike, say, the zod barrel-reexport
case in §7.1, which genuinely does need new transitive-resolution capability). This one is a data
plumbing gap: the right source of truth (`file_index`) already exists in the schema, it's just not
wired into `path_lang`. Re-run `rename_express_set_charset` after landing that fix to confirm
empirically — this file's own history (§7.2 asserting the fix already existed and closed the
question, when it hadn't) is itself the reason not to claim victory before re-measuring.

## §9. Fix SHIPPED and empirically confirmed (same session, following user's explicit request)

§8's diagnosis was implemented, not just written up — per the user's follow-up ask for "the most
optimal, effective, accurate, thorough fix," not another diagnosis.

**Change**: `crates/calm-core/src/indexer/pipeline/context.rs::build_resolution_context` — `path_lang`
now seeded from `SELECT path, language FROM file_index WHERE language IS NOT NULL` (one query,
covers every indexed file regardless of symbol count) instead of being incidentally derived inside
the `symbols`-table loop (`path_lang.entry(path).or_insert_with(...)`, removed). `file_index.language`
and `symbols.language` are guaranteed identical strings — both come from the same
`language_for_extension(ext)` call at the same call site in `driver.rs`, verified by reading that
code directly, not assumed. Both `rebuild_graph` (full reindex) and `incremental_graph_update`
(delta reindex) call this same function, so one change covers both paths — confirmed via `callers()`
before editing.

**Verification chain, each step actually run, not assumed:**
1. `cargo check -p calm-core` — clean.
2. Re-ran all 8 §8 repro fixtures with the rebuilt `calm-cli`: every previously-zero case
   (`repro3`/`repro5`/`repro6`/`repro8` — 2-level and 1-level nesting, bare and receiver-based calls,
   all cross-file) now reports `direct_count: 1` with the correct confidence (`resolved` for bare
   tier-1 calls, `inferred` for the whole-module-require/`module_hint` branch) — exactly the
   confidence levels predicted from reading the resolution code, not just "some edge appeared."
   Previously-working cases (`repro2`/`repro4`/`repro7`, same-file or named-caller shapes)
   unaffected — verified, not assumed.
3. Real pinned express corpus, fresh clone: `reference_impact("setCharset")` now classifies all 5
   `test/utils.js` occurrences as `must_change`/`likely_change` (was `textual_only`).
4. New permanent regression test added right after the existing (insufficient) one in
   `crates/calm-core/src/indexer/pipeline.rs`:
   `test_call_from_a_file_with_no_named_symbols_still_gets_a_call_edge` — a two-level
   `describe(){ it(){ ... } }` fixture with zero named declarations, asserting a real `call_edges`
   row exists. Passes.
5. `cargo test --workspace --release` — **entire workspace, 0 failed** (calm-core's own 1,252-test
   lib suite, calm-server's 400+, every other crate) — the same-language filter this touches is a
   hard correctness guard load-bearing across every language CALM indexes, so a full-workspace run
   was the right bar, not just the touched crate.
6. Re-ran all 6 B7 tasks end-to-end against the fixed `calm-cli` (not just the unit-level DB checks
   above):

| task | before fix (calm / calm_v2) | after fix (calm / calm_v2) |
|---|---|---|
| rename_fd_pattern_matches_leading_dot | 1.0/True · 1.0/True | 1.0/True · 1.0/True (unchanged) |
| rename_flask_from_prefixed_env | 1.0/True · 1.0/True | 1.0/True · 1.0/True (unchanged) |
| rename_gin_clean_path | 1.0/True · 1.0/True | 1.0/True · 1.0/True (unchanged) |
| rename_petclinic_find_pet_types | 1.0/True · 1.0/True | 1.0/True · 1.0/True (unchanged) |
| **rename_express_set_charset** | 0.667/False · 0.667/False | **1.0/True · 1.0/True — FIXED** |
| rename_zod_prettify_error | 0.5/False · 0.5/False | 0.5/False · 0.5/False (unchanged, expected — §7.1's separate barrel-reexport bug) |

Express is fixed on **both** arms — `calm` (v1, plain `edit_context.callers()`) now passes too,
not just `calm_v2`, because the fix repairs the underlying call graph `callers()` itself reads from,
not something specific to `reference_impact`. Zero regressions across all 6 tasks. Zod staying
exactly at 0.5/False is the expected, predicted outcome (a structurally different bug), not a gap
in this fix.

**Not done in this pass**: the zod barrel/wildcard-reexport fix (§7.1/§6.1 — transitive import
resolution, a materially larger feature); confirming whether zod's `error-utils.test.ts` would have
independently hit this same `path_lang` bug had it lacked a named declaration (moot now that both
are fixed/scoped separately, low priority to chase further); this fix is uncommitted, awaiting the
user's go-ahead to commit/push.

## §10. Zod's fix SHIPPED too — the transitive re-export walk (§7.1/§6.1's "materially bigger feature")

Per the user's explicit follow-up ask ("nghiên cứu fix luôn bug Zod đi fen"), implemented the
transitive-resolution fix §9 deferred. Two changes, both in the JS/TS-specific extraction/lookup
path, not the shared resolver:

**1. `crates/calm-core/src/indexer/imports.rs`** — `export_statement` added to `import_node_types`
for javascript/typescript (previously walked zero export nodes at all, for any JS/TS project). New
`parse_js_export_from` parses `export { a, b as c } from '...'` (named re-export) and `export *
from '...'` (wildcard) into `ParsedImport`. The wildcard branch reuses `imported_names: Vec::new()`
— the exact same "no specific names" convention `parse_rust_import`'s `use x::*` branch already
established for `symbols_used = '[]'`, not a new sentinel, so it's recognized for free by both
`dependencies()`'s existing one-hop glob-chain SQL and the new BFS below. `export * as ns from
'...'` (namespace re-export — `ns.symbol` access, a different reference shape) is deliberately left
unparsed, and so is an `as`-aliased named re-export (`imported_names` would need to carry (original,
alias) pairs to trace an alias chain, a bigger data-model change; zod's own real chain has no
aliasing).

**Real bug caught while wiring this up, not shipped**: routing `export`-shaped text through the
existing `parse_js_esm_import(text).or_else(...)` fallback chain let a namespace re-export
(`export * as ns from '...'`, which `parse_js_export_from` correctly refuses) fall through into
`parse_js_esm_import`'s default-import-fallback branch, which misparsed the literal word `"export"`
out of the text as a bogus imported name. Fixed by branching on `text.starts_with("export")` in
`parse_js_import` *before* trying the import-shaped parsers at all — caught by
`js_namespace_export_from_yields_no_import`, a test written before this guard existed, per this
session's stated verification discipline.

**2. `crates/calm-server/src/tools/trace.rs::reference_impact`** — the import-edge section (was a
single `WHERE to_path = ?1` query) is now a bounded BFS (`REFERENCE_IMPACT_MAX_REEXPORT_HOPS = 8`,
generous headroom over the 2-hop depth zod's real chain needs) outward from the definition's file:
a direct-name hit at any hop is `must_change` AND a new frontier node (could itself be re-exported
further up under the same name); a wildcard hit (`symbols_used = '[]'`) is not itself a hit but
continues the search. A `visited` set guards a circular re-export chain from looping forever.

**Verification, same discipline as §9:**
- `cargo check` clean on both crates.
- 3 new extraction-level tests (`js_named_export_from`, `js_wildcard_export_from_yields_no_names`,
  `js_namespace_export_from_yields_no_import`) — the third caught the fallback-chain bug above
  before it shipped.
- 2 new `reference_impact` tests: `reference_impact_follows_a_two_hop_wildcard_reexport_chain`
  (zod's exact shape: `external.js` names the symbol from `barrel.js`, which only wildcard-
  re-exports it from `errors.js` where it's actually defined — asserts `external.js` surfaces as
  `must_change`, and that `barrel.js` itself does NOT, since its own text never names the symbol)
  and `reference_impact_reexport_walk_is_immune_to_a_circular_chain` (a↔b wildcard cycle — asserts
  the call returns promptly instead of hanging). All pass.
- `cargo test --workspace --release`: **0 failed** (calm-core 1,255 tests — was 1,252 before both
  fixes, +4 net across §9/§10's additions minus none removed — actually +3 here on top of §9's +1;
  calm-server 402 — was 400 before §9/§10, +2 here).
- Re-ran all 6 B7 tasks end-to-end against the fixed `calm-cli`:

| task | before this pass | after |
|---|---|---|
| fd / flask / gin / petclinic | 1.0/True | 1.0/True (unchanged) |
| express (calm / calm_v2) | 1.0/True · 1.0/True (already fixed, §9) | 1.0/True · 1.0/True (unchanged) |
| **zod (calm / calm_v2)** | 0.5/False · 0.5/False | 0.5/False (unchanged, expected — v1 never calls `reference_impact`) · **1.0/True — FIXED** |

**B7 now passes 100% (6/6) via the `calm_v2` arm — every task originally in scope, zero
regressions.** `calm` v1 stays at zod's original 0.5/False by design (v1 is deliberately the
`edit_context.callers()`-only baseline this whole B7-v2 effort exists to compare against, not
something this fix touches).

**Still not covered, by design, documented above**: `export * as ns from` namespace re-exports;
`as`-aliased named re-exports changing the traced name partway through a chain. Both are real,
scoped-out gaps, not silent — worth a dedicated follow-up if a real task ever needs them, not
assumed to be rare enough to ignore forever.

All of §9 and §10's changes remain uncommitted, awaiting the user's go-ahead to commit/push.

---

# Execution record — roadmap item 3: Canonical PolicyDecision

Per the user's explicit choice (offered a menu after B7-v2 reached 6/6) to work item 3 next
instead of resuming PR#10 — §2's "currently two parity-tested paths" note is what this section
resolves, one concrete gap at a time rather than a full architecture rewrite.

## §11. The real gap found, and why it's narrower (and more concrete) than "merge two engines"

Read `classify_gate` (calm-server/tools/edit.rs) and `policy::evaluate()`
(calm-core/policy/evaluate.rs) side by side, plus every real (non-test) call site of both.
Finding: they are not two competing implementations of the same decision — they're used for
different purposes at different points in the same request (`edit_lines_impl_gated` calls
`evaluate()` at line ~1373 only to compute an authority-spend digest for the CCK-10
`change_id`+`authority_id` path, and calls `classify_gate` separately at line ~1398 for the
actual will-block decision — both from the SAME function, over the SAME edit). The real defect
is narrower and concrete: `classify_gate`'s `risk` input comes entirely from
`compute_touch_risk`, which folds in caller-count, signature-change, and `risk_rules` path
floors — but **never** `touches_manifest` or `touches_uncovered_code`, two axes
`policy::evaluate()` already escalates to `Policy::default()`'s "maximally conservative" `high`
floor (confirmed via its own test `defaults_are_all_high_the_maximally_conservative_setting`,
policy/loader.rs). Those two axes were only ever computed on the CCK-10 authority-digest branch
(gated behind a caller supplying `change_id`+`authority_id`) and inside `review_change` — **never**
on the plain `confirm`+`reason` path, which is the one every `edit_lines`/`edit_symbol` call takes
by default. Concrete failure scenario this closes: an agent runs `edit_lines` on `Cargo.toml` (or
any file with zero indexed symbols and no test coverage), passes `confirm: true` with a generic
reason, and the edit sails through with no independent-review requirement at all — despite this
project's own policy defaults treating a manifest edit as maximally conservative risk everywhere
else in the codebase.

`kind_mismatch` (the third axis `evaluate()` has that `classify_gate` doesn't) was checked and
correctly excluded from this fix: it compares a *declared* `ChangeIntent.kind` against the
observed diff, and the plain confirm/reason path has no `ChangeIntent` at all — there is nothing
to mismatch. Confirmed this isn't silently missing, it structurally doesn't apply outside the
authority lifecycle.

## §12. Fix shipped: fold both floors into `compute_touch_risk` itself, at the source both real gates read

Rather than restructure `edit_lines_impl_gated`/`edit_context`/`review_change`'s authority
plumbing (large, high-risk blast radius across the CCK-tagged authority system), the fix targets
`compute_touch_risk` (calm-server/tools/edit.rs) directly — the one function that already feeds
**both** real gate call sites (`edit_lines_impl_gated`'s actual block decision, and
`edit_context`'s `gate_prediction`, which its own doc comment already claims is "single source of
truth with the real gate" via reusing this exact function). `compute_touch_risk` already receives
100% of the raw data needed (`path`, `coverage`, `proposed_hunks`) — it just never called the two
already-existing helpers (`is_manifest_path`, the logic `hunks_touch_uncovered_code` already
encapsulates) that `edit_lines_impl_gated`'s authority branch calls separately. Two new blocks
added after the existing `risk_rules` floor escalation, using the exact same severity-max pattern
already there (so a project that configures a lower floor in `.calm/policy.toml` is honored, not
overridden by a hardcoded "always high"):

- `touches_manifest = is_manifest_path(path)` → escalates to `policy.manifest_floor`.
- `touches_uncovered_code` (mirrors `hunks_touch_uncovered_code`'s exact logic, inlined rather than
  called directly since the tuple-shaped `proposed_hunks` here isn't the `&[HunkRequest]` that
  helper expects) → escalates to `policy.uncovered_code_floor`.

Two new params: `project_root: &Path` (needed for `touches_uncovered_code`'s absolute-path
resolution) and `policy: &calm_core::policy::Policy` (needed so both floors are read from the
project's real config, not hardcoded). `TouchRiskResult`'s 6-tuple return shape is **unchanged** —
this is pure additive escalation logic, so none of the 12 existing callers needed their
destructuring pattern touched, only 2 new arguments threaded through:

- `edit_lines_impl_gated`'s real gate call (edit.rs) — now the plain confirm/reason path sees
  both axes for the first time.
- `edit_lines_impl_gated`'s post-edit touched-symbols re-query (edit.rs) — result already
  discarded here, passed `Policy::default()` (unused) rather than a real load, matching this
  call site's existing "discard risk" precedent.
- `edit_context`'s `gate_prediction` (guardrails.rs) — kept as a true mirror of the real gate.
  `touches_manifest` predicts correctly pre-edit (path-only); `touches_uncovered_code` cannot
  (always empty `proposed_hunks` pre-edit, the same structural limitation the existing
  signature-change escalation already has, documented in the same style).
- `review_change`'s `uncertain_zero_caller` extraction (change.rs) — only that one field was ever
  consumed from the tuple; passed `Policy::default()` (unused), same precedent as above.
- 6 existing unit tests in `tools.rs` — mechanically threaded a tempdir + `Policy::default()`.

Also fixed a stale module doc comment found while reading the surrounding code
(`crates/calm-core/src/policy/mod.rs`): it still said "**Shadow only** ... nothing in
`calm-server`'s write gate reads it yet (that wiring ... is CCK-10)" — true when written, false
since CCK-10 actually landed (3 real production call sites already existed before this session,
now a 4th via `compute_touch_risk`) and never updated. Corrected to name the real call sites.

## §13. Verification chain

1. `cargo check --workspace --all-features` — clean after wiring all 12 call sites.
2. 4 new regression tests added in `tools.rs` (after the existing signature-escalation tests):
   - `compute_touch_risk_escalates_for_a_manifest_path` — `Cargo.toml`, zero indexed symbols
     (isolates the manifest axis from caller-count risk entirely), asserts `high` +
     a reason naming the path.
   - `compute_touch_risk_manifest_floor_honors_project_policy_config` — a `Policy` with
     `manifest_floor: Medium` asserts `medium`, not the default `high`, proving the floor is read
     from config rather than hardcoded.
   - `compute_touch_risk_escalates_for_uncovered_code_when_hunks_are_proposed` — a hunk over an
     uncovered line with real `lcov`-shaped `CoverageData`, asserts `high` even though
     caller_count=2 alone is structurally only `low`.
   - `compute_touch_risk_uncovered_code_never_fires_with_no_proposed_hunks` — same uncovered
     file/range but empty `proposed_hunks` (`edit_context`'s pre-edit shape), asserts `low` —
     proves the documented prediction-time limitation is real, not just claimed in a comment.
3. `cargo test --workspace --release` — **entire workspace, 0 failed**: calm-core 1,255 (unchanged,
   this fix didn't touch calm-core), calm-server 406 (was 402 before this session's Zod work, +4
   here), schema_version_migration 5, watcher_integration 3 — all green.
4. `diff_impact` on the full working-tree diff: confirms `compute_touch_risk`
   `signature_changed: true`, 12 direct callers, `aggregate_risk: "high"` — the expected shape for
   a function-signature change with this many callers, not a surprise; all 12 already covered by
   the full-suite pass above.

## §14. Scope note — what this fix is, and isn't

This is the narrower of the two options roadmap item 3 named ("retire classify_gate... or
formally promote it"): `classify_gate` stays the real-time gate, but the signal it's fed is now
honest about 2 of the 3 axes it was previously blind to (`kind_mismatch` structurally doesn't
apply outside the authority lifecycle, confirmed above, not overlooked). A full architectural
merge — making `evaluate()`/`PolicyDecision` the single canonical decision object that
`classify_gate` derives from, rather than two independently-computed paths kept in sync by
parity tests — remains a larger, separate follow-up if ever wanted: it would mean restructuring
`edit_lines_impl_gated`/`edit_context`/`review_change`'s authority plumbing, not just their risk
inputs, and touches the CCK-tagged authority system directly. Not attempted here; this session's
fix closes the one concrete, demonstrated failure scenario (manifest/uncovered-code edits sailing
through ungated) without that larger blast radius.

Uncommitted, same as §9/§10, awaiting the user's go-ahead to commit/push.
