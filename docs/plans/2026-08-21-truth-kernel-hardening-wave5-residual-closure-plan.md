---
title: "Truth-kernel hardening — Wave 5, closing the 7 residuals left after Waves 0-4"
date: 2026-08-21
status: "Planning only, no code written yet. Follows a fresh re-verification
  (2026-08-21) of docs/plans/2026-08-20-truth-kernel-hardening-execution-plan.md
  against HEAD 36eafa4 (Waves 0-4, all committed). That re-verification found
  every wave's own self-reported residual/deferred item still accurately
  described -- no silent gap beyond what the parent doc already disclosed. This
  doc turns those 7 residuals into a prioritized, file-level plan the same way
  the parent doc's Part B did for the original 12 audit findings."
scope: >
  Not a new audit. The parent doc (Wave 0-4) closed all 12 original findings at
  the mechanism level but explicitly left 7 items deferred or partially scoped,
  each with its own documented reason. This doc researches each one down to a
  concrete design (function signatures, call sites, code sketch, DoD) so the
  next implementation session can execute without re-deriving context.
verified_against: "HEAD 36eafa430a8040c1e90043ba3626de520a863f0b"
related:
  - docs/plans/2026-08-20-truth-kernel-hardening-execution-plan.md   # parent plan, Waves 0-4
---

# Truth-kernel hardening — Wave 5 (residual closure)

## The 7 residuals, prioritized

| # | Item | Parent doc ref | Effort | Priority | Why this order |
|---|---|---|---|---|---|
| 5.1 | `gate_prediction` still can't predict the uncovered-code floor | P0-6 / "0.6" | ~M | **P0** | Breaks `edit_context`'s own advertised contract on the single most common real trigger; a live-reproduced, self-inflicted bug in a safety tool |
| 5.2 | DB-`Ambiguous` candidates not live-re-verified | Wave 1 residual | ~S-M | **P1** | Last real gap in the truth kernel's core promise; small, self-contained, reuses existing `verify_live` |
| 5.3 | Write-path tools (`edit_symbol`, `edit_context`) don't accept `qualified_name` | Wave 3.4 scope-narrow | ~S | **P2** | Low risk, direct ambiguity-avoidance value, mechanical once 5.2 lands (shares the same resolver) |
| 5.4 | `understand()`'s own `kind` fallback still defaults to `"symbol"` | Wave 3 follow-up note | ~XS | **P2** | Trivial one-liner, bundle with 5.3 |
| 5.5 | No adversarial test for drift injected *during* reconciliation | Wave 2.2 DoD gap | ~M | **P3** | Real coverage gap but the underlying mechanism (2.1's live-mtime check) already runs at the right point — this closes proof, not a live bug |
| 5.6 | `coreness.rs` hub-detection bucket left at `rank() > 0` | Wave 2.3 deferred | measurement, not code | **P3** | Needs an empirical false-hub-rate pass before any code decision — sequence this as a data-gathering task, not a sprint item |
| 5.7 | `parse_tree` collapses 3 failure causes into one `None` | Wave 4 deferred | ~L | **P4** | Correct but lowest ROI: 9 production call sites, no live-reproduced bug behind it (unlike 5.1) |

Suggested sequencing: **5.1 → 5.2 → (5.3 + 5.4 together) → 5.5 → 5.6 (measurement) → 5.7**.
5.1-5.4 are independent of each other and could be parallelized across sessions if desired;
5.5-5.7 are each standalone and don't block anything else.

---

## 5.1 — Fix `gate_prediction`'s blindness to the uncovered-code floor (P0-6)

### Root cause, precisely

`compute_touch_risk` (`crates/calm-server/src/tools/edit.rs:3704-3939`) computes
`touches_uncovered_code` at line 3906:

```rust
let touches_uncovered_code = !proposed_hunks.is_empty() && coverage.source != "none" && { ... };
```

`edit_context`'s `gate_prediction` block (`guardrails.rs:332-346`) calls this function with
`proposed_hunks = &[]` (no real edit exists yet at pre-edit exploration time) — so the check
can **never** fire from that call site, regardless of the target's real coverage state. The
real gate (`edit_lines_impl_gated`) always has real hunks, so it fires correctly — meaning
`gate_prediction` and the real gate can disagree, exactly the bug P0-6 documents. There is
already a test that hard-codes this as expected behavior instead of fixing it:
`compute_touch_risk_uncovered_code_never_fires_with_no_proposed_hunks` (`tools.rs:11162`).

A second, broader form of the same root cause (found during the parent doc's own Wave 0
execution, not yet acted on): `touches_uncovered_code`'s coverage probe doesn't check
whether any *touched* symbol is even executable code. A struct/enum/interface/heading-only
range trips the same floor, because coverage tooling has nothing to report for
non-instrumentable lines and `is_covered` reads that absence as `false` — same as genuinely
untested code.

### Design (two independent, additive fixes — do both, in this order)

**5.1a — Gate the coverage check on executable-kind rows (fixes the "any struct edit gets
maximum gate severity" bug, zero call-site changes needed)**

Inside `compute_touch_risk`'s existing `for row in rows` loop (edit.rs:3723), track a new
flag alongside `hub_hit`/`uncertain_zero_caller`:

```rust
let mut any_executable_kind = false;
// ...inside the loop, next to the existing `hub_hit |= row.is_hub;`:
any_executable_kind |= matches!(row.kind.as_str(), "function" | "method");
```

Then change line 3906 to:

```rust
let touches_uncovered_code = any_executable_kind
    && !proposed_hunks.is_empty()
    && coverage.source != "none"
    && { ... };
```

This mirrors the exact `kind == "function" | "method"` guard the signature-touch and
dead-code checks already use a few lines above (line 3736, 3769) — same convention, not a
new one. **No signature change, no call-site migration** — purely internal to
`compute_touch_risk`. This alone closes the "struct edits shouldn't hit the top gate" half
of P0-6.

**5.1b — Let `gate_prediction` actually probe coverage, without corrupting the
signature-change check**

The naive fix (pass `&[(c.line_start, c.line_end, "")]` instead of `&[]`) is unsafe as-is:
`compute_touch_risk`'s signature-escalation block (edit.rs:3756-3767) would read the
synthetic empty `new_text` as "the new signature is empty," a real semantic change, and
falsely escalate. Using `new_text.is_empty()` as a sentinel to suppress that check is
**also unsafe** — a real `edit_lines` call deleting a function signature (replacing it with
literally empty text) is a legitimate, real signature change that must still escalate; an
empty-text sentinel would silently defeat that detection for the real gate too, since both
paths share this one function.

The correct fix needs an explicit flag distinguishing "this call has real proposed content"
from "this call is speculative." Add one new parameter:

```rust
pub(crate) fn compute_touch_risk(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    path: &str,
    ranges: &[(i64, i64)],
    coverage: &calm_core::analysis::coverage::CoverageData,
    risk_rules: &[calm_core::config::RiskRule],
    proposed_hunks: &[(i64, i64, &str)],
    policy: &calm_core::policy::Policy,
    real_hunks: bool,   // NEW — false only at edit_context's speculative
                         // gate_prediction call site; true everywhere else.
                         // Gates the signature-change check ONLY (5.1a's
                         // executable-kind guard already handles the
                         // coverage-check side safely for both cases,
                         // since that check ignores hunk *text* entirely —
                         // only (start, end) matter there).
) -> TouchRiskResult { ... }
```

Inside the loop, guard the existing signature-touch block:

```rust
if real_hunks
    && signature_touch.is_none()
    && !row.signature.is_empty()
    && matches!(row.kind.as_str(), "function" | "method")
{
    // ...unchanged existing logic...
}
```

At `edit_context`'s call site (`guardrails.rs:332`), pass a synthetic full-range placeholder
hunk and `real_hunks: false`:

```rust
) = edit::compute_touch_risk(
    &conn,
    &self.project_root,
    &c.path,
    &[(c.line_start, c.line_end)],
    &self.coverage.read_ok(),
    &config.risk_rules,
    &[(c.line_start, c.line_end, "")],  // synthetic: only (start,end) are read
                                          // when real_hunks=false, see below
    &gate_policy,
    false,
);
```

All other call sites (`edit_lines_impl_gated` ×2 at edit.rs:1201, edit.rs:2745,
`review_change` at change.rs:658, and ~10 test call sites in `tools.rs`) pass `true` —
mechanical, compiler-enforced-complete, the same migration shape Wave 1's `ReadFailed` arm
and Wave 3.4's `qualified_name` param already established as this codebase's convention for
threading a new signal through a small, enumerable call-site set.

### DoD

- New test mirroring `edit_context_gate_prediction_matches_real_gate_for_hub_symbol`
  (`tools.rs:11408`), but for coverage: seed `CoverageData` with an *uncovered* range on a
  `function`-kind symbol with no test, call `edit_context`, assert
  `gate_prediction.will_block == true` / `requires` reflects `policy.uncovered_code_floor`
  — then perform the real write with `confirm:false` on the same range and assert it is
  blocked for the *same* reason. This is the parity assertion the existing hub-symbol test
  already establishes as this repo's pattern for `gate_prediction` correctness; today it
  would fail (predicts `will_block:false`, real gate blocks) — that failing-then-passing
  transition is the actual proof this fix works.
- New test: same setup but the symbol is `kind="struct"` with a doc comment only (no
  executable lines) — assert `gate_prediction.will_block == false` even with `coverage`
  showing the range as uncovered (5.1a's fix).
- Existing `compute_touch_risk_uncovered_code_never_fires_with_no_proposed_hunks` test
  needs updating: with `real_hunks` now threaded, decide whether to keep it as a
  regression test for the *old*, now-superseded call shape (rename to something like
  `..._synthetic_hunks_still_skip_signature_check`) or delete it — it currently locks in
  the bug this item fixes, so it cannot survive unchanged.
- `cargo test --workspace` green, `cargo clippy --workspace --all-targets -- -D warnings`
  clean, toolsnap regen not expected (no `#[tool]`-visible schema changes — `real_hunks` is
  an internal-only parameter).

---

## 5.2 — Live-re-verify DB-`Ambiguous` candidates (Wave 1 residual)

### Root cause, precisely

`resolve_symbol` (`outcome.rs:614-639`):

```rust
if candidates.len() > 1 {
    return Ok(SymbolResolution::Ambiguous(candidates));
}
Ok(verify_live(conn, project_root, candidates.remove(0)))
```

Only the single-candidate case reaches `verify_live`. A symbol that was genuinely ambiguous
in the DB (two same-named methods in different classes, say) and has since had **one** of
those two deleted from disk still reports `Ambiguous` with both candidates today — the
caller has no way to know one of them no longer exists. This is a strictly smaller-blast-
radius gap than the original P0-1 (a *confidently wrong single answer*) but it's still a
real staleness leak the parent doc explicitly flagged as open.

### Design

`verify_live` (`outcome.rs:648-715`) already does exactly the per-candidate check needed —
it takes one `CandidateRow` by value and returns `Found` / `NotFound` / `ReadFailed` (never
`Ambiguous`, confirmed by reading its full body). Reuse it directly, once per candidate,
instead of writing a new helper:

```rust
pub(crate) fn resolve_symbol(...) -> rusqlite::Result<SymbolResolution> {
    let mut candidates = resolve_symbol_candidates(conn, name, path, qualified_name)?;
    if let Some(line) = line { /* unchanged */ }
    if candidates.is_empty() {
        return Ok(SymbolResolution::NotFound);
    }
    if candidates.len() == 1 {
        return Ok(verify_live(conn, project_root, candidates.remove(0)));
    }

    // 5.2 (Wave 5, Wave-1 residual): a DB-ambiguous result is no longer
    // trusted as-is -- each candidate is live-verified the same way a
    // lone Found candidate always has been. A candidate that's vanished
    // from disk since indexing no longer poisons the ambiguity; if
    // verification narrows the set to exactly one survivor, this now
    // returns Found instead of a stale Ambiguous.
    const MAX_LIVE_VERIFIED_CANDIDATES: usize = 20; // matches this crate's
        // existing cap convention (callers/callees/skipped_files) -- an
        // unqualified bare-name query with more DB matches than this
        // degrades to today's un-reverified Ambiguous rather than doing
        // O(hundreds) file reads per call; documented, not silent.
    if candidates.len() > MAX_LIVE_VERIFIED_CANDIDATES {
        return Ok(SymbolResolution::Ambiguous(candidates));
    }
    let mut still_live = Vec::with_capacity(candidates.len());
    for c in candidates {
        match verify_live(conn, project_root, c) {
            SymbolResolution::Found(c) => still_live.push(*c),
            SymbolResolution::NotFound => {}
            SymbolResolution::ReadFailed(e) => return Ok(SymbolResolution::ReadFailed(e)),
            SymbolResolution::Ambiguous(_) => unreachable!("verify_live never returns Ambiguous"),
        }
    }
    match still_live.len() {
        0 => Ok(SymbolResolution::NotFound),
        1 => Ok(SymbolResolution::Found(Box::new(still_live.remove(0)))),
        _ => Ok(SymbolResolution::Ambiguous(still_live)),
    }
}
```

No call-site changes needed anywhere — this is purely internal to `resolve_symbol`, same as
how Wave 1 itself was framed ("a non-breaking internal change for every existing 3-arm
match"). All 10+ callers already handle all 4 `SymbolResolution` variants exhaustively
(compiler already enforces this).

**Cost note to flag in the PR description, not hide:** worst case is a bare name shared by
exactly `MAX_LIVE_VERIFIED_CANDIDATES` symbols across the repo, each in a different file —
up to 20 extra file reads + hashes per `resolve_symbol` call in that specific case. Every
one of the 10 call sites already reads at least one file on the `Found` path today, so this
only changes the *ambiguous* path's cost profile, which was previously free (DB-only). A
`qualified_name`-bearing caller (Wave 3.4's read-only tools, the common repeat-call path)
never hits this branch at all, since `qualified_name` narrows to exactly one DB row by
construction.

### DoD

- New test: two same-named methods in different classes (the existing Wave 1 duplicate-
  class_context fixture shape), delete one from disk without reindexing, call through a
  bare-name resolution (no `qualified_name`) — assert the result is `Found` (not
  `Ambiguous`) and it's the surviving one.
- New test: same setup, delete *both* methods (rename the whole file) — assert `NotFound`,
  not a read of unrelated bytes.
- New test: genuinely-still-ambiguous case (both candidates present and unchanged) — assert
  `Ambiguous` with exactly 2 entries, unchanged from today's behavior (regression guard).
- New test: >`MAX_LIVE_VERIFIED_CANDIDATES` DB matches — assert the function returns
  `Ambiguous` with all candidates and does **not** attempt to read `MAX+1` files (can be
  asserted indirectly via a candidate whose path doesn't exist on disk at all — if the cap
  didn't apply, that would surface as `ReadFailed`, not a silently-passed-through
  candidate).

---

## 5.3 — Wire real `qualified_name` narrowing into the write-path tools

### Root cause

`EditContextParams` (`guardrails.rs:935-959`) and `EditSymbolParams` (`edit.rs:4587-4653`)
have **no** `qualified_name` field at all — their `resolve_symbol` calls
(`guardrails.rs:38`, `edit.rs:399-413`) pass a literal `None`, not a param-derived value.
This was an intentional scope-narrowing in Wave 3.4 (deferred as "lower-value given heavier
gate/authority machinery"), not an oversight — but the actual risk that justified deferring
it (interaction with the gate/authority pipeline) doesn't really apply: `qualified_name`
only changes **which symbol** `resolve_symbol` resolves to before any gate logic runs; it
doesn't touch `classify_gate`, `ReviewAuthority`, or anything downstream.

### Design

Add one field to each struct, following the exact doc-comment convention Wave 3.4 already
used for the 9 read-only tools (e.g. `symbol_info`'s):

```rust
// EditContextParams (guardrails.rs) and EditSymbolParams (edit.rs), same shape:
/// Exact `qualified_name` from a prior `search`/`locate` result — when set,
/// resolves directly by identity and `path`/`line` are ignored, so this can
/// never come back ambiguous even for a globally-common bare `symbol` name.
/// Still flows through the same live-verification every resolution does.
#[serde(skip_serializing_if = "Option::is_none")]
pub(crate) qualified_name: Option<String>,
```

Then at each call site, replace the literal `None` with `p.qualified_name.as_deref()`:

- `guardrails.rs:38`: `resolve_symbol(&conn, &self.project_root, &p.symbol, p.path.as_deref(), p.line, p.qualified_name.as_deref())`
- `edit.rs:412`: same substitution, in `edit_symbol_flow`.

### DoD

- Both DoD assertions from Wave 3.4's original 3.4 item, re-run against these two tools:
  a `qualified_name` lookup of a globally-common bare name resolves uniquely; a
  `qualified_name` lookup of a since-deleted symbol reports `NotFound`/`ReadFailed`, never a
  stale read — for `edit_context` specifically (the exact tool P0-6/5.1 also touches, so
  sequencing this after 5.1/5.2 avoids stacking unrelated diffs in the same review).
- Toolsnap regen needed: `edit_context` and `edit_symbol`'s own input schemas gain a new
  optional field — `UPDATE_TOOLSNAPS=1` regenerates `edit_context.snap` and (if one exists)
  `edit_symbol`'s.

---

## 5.4 — Fix `understand()`'s own independent `kind` fallback

Trivial, bundle with 5.3 in the same PR (both touch tool-input defaults, same review
surface). `inspect.rs:487`:

```rust
let kind_str = p.kind.as_deref().unwrap_or("symbol");   // today
let kind_str = p.kind.as_deref().unwrap_or("hybrid");   // fix — matches Wave 3.1's
                                                           // search default, closes the
                                                           // "small, separate follow-up"
                                                           // the parent doc's 3.1 research
                                                           // pass explicitly left open
```

**DoD:** the exact natural-language query Wave 3.1's own DoD used
("how does calm decide whether an edit is high risk") via `understand()` with no `kind`
override returns a relevant result, mirroring the existing `search` regression test for the
same query.

---

## 5.5 — Adversarial test: drift injected *during* reconciliation (Wave 2.2 DoD gap)

### What already exists vs. what's missing

The reconciliation-fence mechanism (2.1's `live_mtime_drift` check reused inside
`EvidenceSnapshot::compute`, called at `watch_supervisor.rs:629` right before persisting)
already runs at the correct point in `WatchSupervisor::refresh` (`watch_supervisor.rs:
511-705`) — after the reindex/graph-rebuild/SCIP-overlay work (lines ~500-594) and
immediately before the `Reconciled` snapshot is persisted (line 629). Existing tests
(`rearm_reconciliation_catches_drift_created_while_the_watcher_was_down`,
`startup_reconciliation_catches_drift_before_the_first_observed_event`) prove drift
*before* a reconciliation run is caught. None prove drift injected *during* the run itself
— i.e., a file that reads fine at the reindex step but is mutated on disk before `compute()`
re-scans mtimes at the tail end.

### Design: a `#[cfg(test)]`-only hook, zero runtime cost outside tests

`refresh()` is one long synchronous function with no natural async yield point a test can
race against without real sleeps (flaky). The idiomatic fix is a test-only injection point
invoked exactly between "reindex/overlay work is done" and "the fence's `compute()` call" —
i.e., right before line 629:

```rust
// watch_supervisor.rs, near WatchSupervisor's other #[cfg(test)] test-support fields
#[cfg(test)]
type ReconciliationTestHook = std::sync::Arc<dyn Fn() + Send + Sync>;

// on WatchSupervisor (or threaded as an Option<&dyn Fn()> parameter into whichever
// inner function directly precedes the `if let Some(reason) = outcome.full_reconciliation`
// block, if refresh() itself isn't easily given a new test-only field):
#[cfg(test)]
reconciliation_test_hook: Option<ReconciliationTestHook>,
```

Call it once, right before the `EvidenceSnapshot::compute` call at line 629:

```rust
#[cfg(test)]
if let Some(hook) = &self.reconciliation_test_hook {
    hook();
}
```

A test constructs a `WatchSupervisor` with this hook set to a closure that mutates a
tracked file's content and mtime (e.g. via `filetime::set_file_mtime` — already a workspace
dependency per the existing `compute_after_reconciliation_is_always_reconciled_regardless_
of_drift`-style tests), then runs a real `refresh()` end to end and asserts the persisted
snapshot is `Degraded` (or `Current`, never `Reconciled`) for that run — the literal DoD
the parent doc's 2.2 section specified. This compiles away entirely in non-test builds (the
field and the call both behind `#[cfg(test)]`), so it carries no production cost or
attack-surface concern, and it tests the *real* `refresh()` code path rather than a
mocked-out tail (which the parent doc's other reconciliation tests also do, so this matches
existing convention rather than introducing a new one).

### DoD

- `reconciliation_catches_drift_injected_during_the_reconciliation_reindex_window` (or
  similar name): real reindex runs, hook mutates a file mid-flight, resulting snapshot is
  never `Reconciled` for that run.
- Existing `full_reconciliation_persists_a_reconciled_evidence_snapshot_when_no_drift`
  continues to pass unchanged (hook absent/no-op by default).

---

## 5.6 — `coreness.rs` hub-detection bucket: a measurement task, not a code change

This is **not** a coding task to schedule directly — the parent doc already correctly
identified that tightening `compute_coreness`'s `rank() > 0` to `is_verified()`-only would
*loosen* the `confirm:true` edit gate (shrinks the hub set), a safety-relevant tradeoff in
the permissive direction. `coreness.rs:51-52` is unchanged and should stay that way until
this measurement exists.

**Concrete next step:** write a one-off analysis script (not a permanent tool) that, against
this repo's own `index.db` (and ideally 2-3 of the other benchmark repos already used
elsewhere in this codebase's benchmark suite — see `calm-benchmark-suite-structure` for
where those live), computes:

- today's hub set size (`is_hub=true` count) under the current `rank() > 0` bucket,
- the hypothetical hub set size under `is_verified()`-only (`Formal`/`Resolved`),
- the *delta* set (symbols that would lose `is_hub` under the stricter bucket) — for a
  sample of those, manually check whether they're real high-fan-in symbols that
  `textual`/`inferred` edges were the only signal for (a true hub CALM would then
  under-protect) vs. noise (edges that were never real calls, e.g. common-name collisions
  the audit's own P0-3g′ finding already flagged as inflating textual-edge counts).

This produces the false-hub-rate number the parent doc says is the actual prerequisite —
only after that exists does 2.3's original code sketch (already written in the parent doc,
lines 686-691: `is_verified`/`is_probable`/`is_lexical_lead`, already landed) get applied to
`coreness.rs` too, or not, based on real evidence rather than a judgment call.

---

## 5.7 — `parse_tree` skip-reason tracking (lowest priority, largest blast radius)

Deferred correctly in the parent doc; re-confirmed unchanged at `parser.rs:197-203`. Full
scope if/when picked up: 9 production call sites (`edit.rs` ×2 —
`validate_syntax`/`validate_syntax_diff`; `csharp_namespace.rs`; `imports.rs`; 4 inside
`parser.rs` itself — `extract_symbols`, `extract_calls`, `extract_file_aliases`,
`extract_type_map`; `pipeline/extraction.rs::extract_file_data`), each consuming the
`Option<Tree>` differently (`?`, `.map()`, `let Some() else`) per the parent doc's own
research pass. Recommended shape when scheduled: mirror 4.1b's `Result<String, String>`
precedent on `read_source_capped` — change `parse_tree`'s return to
`Result<tree_sitter::Tree, ParseFailure>` where `ParseFailure` is a small enum
(`UnsupportedLanguage | AbiLoadFailed | Timeout`), and thread the reason into the same
`file_index.skip_reason` column 4.1b already added rather than inventing a second column —
one skip-reason surface for both read-failures and parse-failures, not two parallel ones.
Not designed further here; this item's ROI doesn't justify more research time until 5.1-5.6
are closed, per the priority table above.
