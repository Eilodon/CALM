# Truth-kernel hardening Wave 8 — Wave 7 audit closure plan

**Status:** SHIPPED (uncommitted). All items — P0-A through P0-E, P1-A/B, P1-C (confirmed
moot) — implemented and verified 2026-08-22: full workspace suite green (450+5+3 tests, 0
failed), `cargo fmt --check` clean, `cargo clippy --workspace --all-targets -- -D warnings`
clean, `diff_impact` run on the full cumulative diff. **Source:** external audit (Vietnamese) received
2026-08-22, verified line-by-line against `92d7f44` (Wave 7 HEAD, current `main`) before
this plan was written. **Verification method:** every cited file/line was read live via
`mcp__calm__source`/`callers`/`search`, cross-checked against the actual control flow (not
just the cited line ranges) — no claim in this plan is taken on the audit's word alone.

## Verification summary

Every specific, checkable claim in the audit was confirmed accurate against live code — an
unusually high-precision audit, zero false claims found. Two findings are in fact **more
severe** than the audit itself stated, confirmed by tracing control flow the audit's line
citations didn't fully unwind:

- **Regression 1 (Strict symbol-less lockout):** the audit says the "review a different
  symbol, retry" remedy doesn't work. Verified it's worse than that — **no success path
  exists at all**, including through the full `ReviewAuthority` mint+spend route. Traced
  `anchor_qualified_name` (edit.rs:969) to its only producer, `edit_symbol_flow`, which sets
  it exclusively for a resolved symbol's doc-comment-anchored insertion — `edit_lines_flow`
  (the path this whole scenario runs on: plain line edits to a comment/module-constant/gap
  region) always passes `None`. So `current_targets` stays empty even when `change_id`+
  `authority_id` are supplied, `target_scope_digest` of an empty set can never match a real
  authority's committed scope, and `ReviewAuthority::verify_only` fails closed with
  `AUTHORITY_WRONG_TARGET_SCOPE` — a different error code than the structural gate's
  `EDIT_CONTEXT_REQUIRED`, but still a hard reject. Strict mode cannot edit a symbol-less
  region via `edit_lines`, period, confirmed via both code paths.
- **The final-spend regression test doesn't just under-test the fix — the fix it's meant to
  test never runs in it.** `edit_lines_impl_gated` calls the identical `observe_spend_snapshot`
  + `Degraded`-reject logic **twice**: once at edit.rs:1461 (labeled "Wave 6" in its own
  comment — this is the *pre-existing* check) and again at edit.rs:2288 (Wave 7's new "final"
  site, right before `authorize_and_begin_edit`). The Wave 7 test
  (`spend_time_freshness_rejects_when_file_index_mtime_has_drifted_since_mint`,
  tools.rs:13283) tampers `file_index.mtime` once, *before* calling `edit_lines_flow` — so the
  L1461 pre-existing check fires and returns early. Execution never reaches L2288. The test
  would pass identically if Wave 7's actual fix were fully reverted.
- Every other cited claim — the CAS being best-effort not atomic, the authority-consumed-
  before-stale-base-check UX gap, `verified_bytes` falling back to an unverified reread on
  `None`, `locate`'s dead gap-chunk branch (confirmed **unreachable**: `qualified_name:
  Some(r.qualified_name.clone())` at locate.rs:344 is unconditional, so `results[0]
  .qualified_name.is_none()` at locate.rs:461 can never be true) plus its incomplete args
  (missing `end_line`, which `source_range` requires — confirmed at inspect.rs:394-402), the
  `callers→source` `suggested_next` value bug (`direct[0].symbol` is `call_edges.from_symbol`,
  confirmed a qualified name via the `LEFT JOIN symbols s ON s.qualified_name = ce.from_symbol`
  it's selected alongside — passed into `source`'s `symbol` param, which resolves via bare
  `symbols.name = ?1` when `qualified_name` is unset, confirmed at outcome.rs:525-530) — all
  confirmed exactly as described, several corroborated by the code's own inline comments
  (e.g. outcome.rs:723-740's "DOCUMENTED RESIDUAL... which an external audit correctly
  flagged" for the `file_index`-missing fail-open path, and graph.rs:386's own "no existing
  consumer reads `verified_caller_count` yet").
- **New discovery beyond the audit, directly relevant to the P0-C fix below:** `source`'s
  range-vs-symbol-mode gate (inspect.rs:222-225) branches purely on `p.symbol` being
  non-empty — `qualified_name` is confirmed to be a narrowing hint usable only *within*
  symbol mode, never a standalone selector. Any `suggested_next` fix that supplies
  `qualified_name` alone without a non-empty `symbol` will silently fall into range mode and
  then fail on missing `line`/`end_line`. The two fixes below account for this.
- Test shape claim confirmed: `git diff 49c2638 92d7f44` shows exactly 4 new test fns +
  1 rewritten (matches the audit's "5 regression tests" tally, matches Wave 7's own commit
  message "4 new regression tests + 1 rewritten test").

This plan orders remediation by blast radius: **P0 = usability/correctness regressions Wave 7
itself introduced or left provably unverified** (ship first, small diffs, each with a
deny→retry→succeed or fail→pass test proving the fix, not just the deny); **P1 = real safety
gaps Wave 6/7 already knew about and explicitly scoped out** (typed verification state,
Option→Result error propagation); **P2 = pre-existing, already-documented backlog Wave 7 did
not touch** (unchanged, listed for completeness, not re-litigated here).

---

## P0-A — Strict-mode symbol-less edit has no success path (regression, edit.rs:1443-1888)

**Root cause:** `pre_touched` is derived purely from which indexed symbols a hunk's line
range overlaps (`compute_touch_risk`). For a pure-comment/whitespace/module-level-constant/
between-symbols edit via `edit_lines`, it is always empty — by construction, not by mistake.
Two independent gates both key off it and both fail closed with no escape:
1. No-authority structural gate (edit.rs:1868): unconditional `EDIT_CONTEXT_REQUIRED` when
   `pre_touched.is_empty()`, regardless of what was reviewed this session.
2. Full-authority path (edit.rs:1443-1600, mirrored at the final spend site): `current_targets`
   built from `pre_touched` ∪ `anchor_qualified_name`; the latter is `None` on this code path
   (`edit_lines_flow` never resolves one), so `current_targets` stays empty and
   `ReviewAuthority::verify_only`'s target-scope digest check rejects with
   `AUTHORITY_WRONG_TARGET_SCOPE` against literally any authority.

**Fix — give `edit_context`/`edit_lines` a real range-scoped review, mirroring `source`'s
existing range mode:**

1. Extend `edit_context`'s params to accept the same `path` + `line` + `end_line` range mode
   `source` already has (`symbol` omitted) — mint a review/authority scoped to
   `(path, line_start, end_line, range_checksum)` instead of a `qualified_name` set. Compute a
   `target_scope_digest` over this range tuple the same way the existing code computes one
   over a `Vec<ChangeIntentTarget>` (add a `ChangeIntentTarget::Range` variant, or a parallel
   `path`-keyed digest input — whichever keeps `authority::CurrentState`'s existing shape
   least disturbed; needs a design read of `calm_core::authority`/`calm_core::change::intent`
   before committing to the exact enum shape).
2. `edit_context_review` (the session-local, no-authority structural-gate store) gets the same
   range key alongside its existing qualified_name key, so the no-authority path
   (edit.rs:1868) can check "was this exact path+range reviewed this session, still fresh"
   before rejecting, instead of unconditionally failing when `pre_touched.is_empty()`.
3. `edit_lines_impl_gated`'s `current_targets` construction (both the pre-check at ~1490 and
   the final spend at ~2311) gets a third source besides `pre_touched`/`anchor_qualified_name`:
   when both are empty, build a single range target from the hunk span actually being written,
   for the authority-verify branch to match against a range-scoped authority from (1).
4. Error message at edit.rs:1878-1886 updated to name the real remedy (call `edit_context`
   with `path`+`line`+`end_line`, not "a symbol in this path") once (1)-(3) exist.

**Test requirement (non-negotiable, per the audit's own critique of the Wave 7 test):** a
*deny → mint range authority → retry → succeed* end-to-end test, not just a deny test. Confirm
it fails against pre-fix code (unconditional reject) before confirming it passes post-fix —
same discipline as every prior wave's regression tests in this repo.

**Sequencing note:** touches `calm_core::authority`/`calm_core::change::intent` (shared with
the full-authority Strict path generally) — read `ReviewAuthority::verify_only`/
`authorize_and_begin_edit` and `ChangeIntentTarget`'s full definition first; this is the
highest-blast-radius item in this plan and should not be rushed opposite P0-B/C/D below.

---

## P0-B — `locate`'s gap-chunk suggestion is dead code with wrong args (regression, locate.rs)

**Root cause, part 1 (dead branch):** locate.rs:344 unconditionally wraps
`r.qualified_name.clone()` in `Some(...)` for every result, so the `results[0]
.qualified_name.is_none()` check at locate.rs:461 (added this wave specifically to catch gap
chunks) can never be true.

**Root cause, part 2 (wrong args even if reached):** the branch's suggestion only sets
`{"path", "line"}` (locate.rs:473-477); `source`'s range mode requires **both** `line` and
`end_line` (inspect.rs:394-402) — confirmed it would still return `INVALID_PARAMS`.

**Fix — confirmed correct against `chunk_hit_to_result`'s real semantics (search.rs:919-995):**
`SearchResult.kind` is `None` precisely for gap chunks (locate.rs:960, no `symbols` row to
join) and for the degenerate DB-inconsistency case (search.rs:952) — the same signal the
audit's own suggested fix uses, and it generalizes correctly to `kind="file"`/`kind="grep"`
hits too (also not symbol-backed, also `kind: None`), not just semantic chunks:

```rust
// locate.rs:344
qualified_name: r.kind.is_some().then(|| r.qualified_name.clone()),
```

```rust
// locate.rs:472-477 — add end_line, using the field already copied onto SearchResultItem
match (results[0].line_start, results[0].line_end) {
    (Some(line), Some(end_line)) => suggested_with_args(
        "source",
        "Read implementation (body match, not a named symbol)",
        serde_json::json!({"path": results[0].path, "line": line, "end_line": end_line}),
    ),
    (Some(line), None) => suggested_with_args(
        "source",
        "Read implementation (body match, not a named symbol)",
        serde_json::json!({"path": results[0].path, "line": line, "end_line": line}),
    ),
    _ => suggested_with_args(
        "file_overview",
        "No symbol or line to anchor on — see the file's structure",
        serde_json::json!({"path": results[0].path}),
    ),
}
```

**Test requirement:** an end-to-end test that gets a real gap-chunk/file/grep hit back from
`locate`, follows the emitted `suggested_next.args` into an actual `source` call, and asserts
success (not `INVALID_PARAMS`, not `NotFound`) — proving the chain, not just the branch
selection. Confirm it fails pre-fix (branch unreachable) before confirming post-fix pass.

---

## P0-C — `callers`'s `suggested_next` → `source` is schema-valid but value-wrong (trace.rs:228-233)

**Root cause:** `direct[0].symbol` holds `call_edges.from_symbol`, confirmed a qualified name
(joined against `symbols.qualified_name` in the same query, trace.rs:95). Passed as `source`'s
`symbol` param alone; `resolve_symbol_candidates` with no `qualified_name` and no `path` runs
`WHERE name = ?1` (outcome.rs:530) — a bare-name match a full qualified-name string will never
satisfy. Confirmed via `source`'s own dispatch (inspect.rs:222-225) that `qualified_name`
cannot be supplied *instead of* `symbol` either — the mode gate keys on `symbol` alone, so a
fix must supply both.

**Fix:**

```rust
// trace.rs:228-233
} else if count > 0 {
    let qn = direct[0].symbol.as_str();
    let bare = qn.rsplit("::").next().unwrap_or(qn);
    suggested_with_args(
        "source",
        "Read top caller implementation",
        serde_json::json!({"symbol": bare, "qualified_name": qn}),
    )
}
```

**Test requirement:** call `callers` on a real fixture symbol with ≥1 direct caller, follow
the emitted `suggested_next.args` into `source`, assert it resolves to the correct caller (not
`NotFound`, not ambiguous). Grep the rest of `trace.rs`/`locate.rs`/`inspect.rs` for other
`suggested_with_args("source", ..., json!({"symbol": <something already known to be a
qualified_name>}))` call sites while in this file — the audit's broader point (below) is that
this exact shape of bug is easy to reintroduce one call site at a time.

---

## P0-D — Schema-validation meta-test for every `suggested_next.args`

**Root cause:** P0-B and P0-C are two independent instances of the same class of bug (args
that are schema-valid JSON but don't satisfy the target tool's actual resolution semantics),
each caught by hand, one wave apart. The audit's own recommendation: stop finding these one at
a time.

**Fix:** a single test (or small harness) that, for every `suggested_next` emitted across the
existing test suite's tool-call assertions (or a dedicated fixture sweep), (a) validates
`args` deserializes into the target tool's actual `Params` struct, and (b) where feasible,
actually invokes the target tool with those args and asserts a non-error `ToolOutcome`. (b) is
the stronger guarantee and the one that would have caught both P0-B and P0-C; (a) alone would
not have caught P0-C (schema-valid, semantically wrong). Scope this as a real test-suite
addition, not a doc note — it's the difference between this being the last wave that finds
this bug class and not.

---

## P1-A — `observe_spend_snapshot` swallows errors into `None` (edit.rs:2865-2874)

**Root cause:** confirmed — `self.make_state_read_conn().ok()?`, `self.make_read_conn().ok()?`,
and `.ok()` on the final `compute_with_recorded_freshness` call all discard the real error.
Downstream, a `None` snapshot produces `snapshot_id: ""` / `graph_generation: 0`
(edit.rs:2300-2307), which safety-wise still fails closed (an authority minted against a real
snapshot won't match an empty one), but surfaces the wrong reason code — a DB-open failure
looks identical to "no snapshot available."

**Fix:** change `observe_spend_snapshot`'s return type to
`Result<EvidenceSnapshot, SpendObservationError>` (new small enum: `StateConn`, `ReadConn`,
`Compute` variants wrapping the underlying errors), let both call sites (edit.rs:1461, 2288)
match on it and emit a distinct `AUTHORITY_DB_ERROR`-class reason code instead of silently
falling through the `Degraded`-check into the empty-snapshot-id path. Low risk, small diff,
pure error-plumbing — safe to ship independently of P0-A.

**Test requirement:** inject a state-DB-open failure (e.g. point `state_db_path` at an
unwritable/missing location mid-test) and assert the emitted reason code names the DB error,
not a generic staleness message.

---

## P1-B — Authority-consumed-before-stale-base-check UX gap (edit.rs:2347-2472)

**Root cause:** confirmed — `authorize_and_begin_edit` (edit.rs:2347) atomically verifies +
consumes the authority *before* `write_via_configured_backend`'s stale-base digest check
(edit.rs:2439) can still reject with `STALE_FILE`. The code's own comment (edit.rs:2362-2363)
states this is intentional ("the authority was already atomically verified and consumed
above"). The returned message (`"failed to write {path}: file changed on disk since it was
read for this edit -- re-read and retry"`, edit.rs:4293-4296) never says the *authority* is
now spent — a caller naively retrying with the same `authority_id` will additionally hit
`AUTHORITY_ALREADY_CONSUMED` and have to debug two errors instead of one.

**Fix:** on `WriteBackendError::StaleBase`, enrich the `STALE_FILE` error detail
(edit.rs:2468-2472) to state explicitly that the authority tied to this attempt was already
consumed and a fresh `edit_context` mint is required before retrying — not a behavior change,
a message-accuracy fix. Cheap, no design risk; bundle with P1-A in the same small PR.

**Test requirement:** assert the `STALE_FILE` error message/detail explicitly mentions
re-minting authority when `authority_id` was set on the failed request (message content
differs from the `change_id`/`authority_id`-absent case, which has nothing to re-mint).

---

## P1-C — `Strict` capability language: `lost_update_guard` claim accuracy

**Root cause:** confirmed — `write_via_configured_backend`'s own doc comment already says
"Best-effort, not a true atomic compare-and-swap" (edit.rs:4320). The audit's ask is about
*external-facing* language (tool descriptions / Strict-mode capability claims), not the code
comment, which is already honest.

**Fix:** grep `strict_tool_description`/mode-compiler description overrides (guardrails.rs,
per Wave 7's own item 6) for any claim using "atomic"/"CAS"/"compare-and-swap" in connection
with this guard, and replace with the accurate `lost_update_guard=best_effort` framing the
audit proposes. Small, text-only, low risk — verify first whether any such claim currently
exists (Wave 7's commit message suggests the 3 tool-description fixes it already made were
about *other* claims — repo_overview-first, diff_impact-before-commit, source-over-native-Read
— not this one, so this may already be a non-issue; confirm before writing code).

---

## P2 — Pre-existing backlog, unaffected by Wave 7 (spot-checked, confirmed unchanged, not re-scoped here)

Carried forward from Wave 6/7's own documented residuals — re-verify current accuracy before
picking any of these up, don't assume this plan's spot-check is still fresh by the time work
starts:

- **`file_index`-missing fail-open in `verify_live`** (outcome.rs:707-744): confirmed still
  returns untyped `Found(c, Option<bytes>)` for a never-hash-verified DB row; code's own
  comment documents the typed-state redesign (`Verified`/`UnverifiedMetadata`/`NotFound`/
  `ReadFailed`) as scoped-out, ~16 call sites, and notes the prior attempt broke 39+ tests
  whose fixtures insert `symbols` rows with no backing file. Real fix, real scope — a
  dedicated wave, not a P0/P1 item here.
- **`verified_caller_count` has no consumer** (graph.rs:386, confirmed via the code's own
  comment): Wave 6's investigation already found this isn't a live functional gap (`callers()`
  computes its own live confidence breakdown); the one real gap (`symbol_info`/search results
  showing bare `caller_count` with no breakdown) needs a wider struct change — still correctly
  scoped as backlog, not touched by Wave 7, not touched here.
- **Coreness ranking boost uses unverified `coreness`, not a verified-only variant**
  (search.rs:217-225, confirmed via `coreness_boost`'s own comment): real schema addition
  (new column + k-core pass), correctly scoped as backlog.
- **New source files invisible to freshness checks** (`live_mtime_drift` / `file_index` scan
  scope): Wave 6 attempted and reverted a fix for the identical reason as the `file_index`
  item above (shared root cause: test fixtures without matching `file_index` rows) — same
  disposition, same reason, not re-attempted here.
- **`understand()`'s `> 0.0` confidence floor** and **coverage exact/shallow/path-only/stale
  granularity**: not touched by Wave 7, not re-verified line-by-line this pass (lower
  confidence than the P0/P1 items above, which were read in full); take at face value from the
  audit and re-confirm before scoping actual work.

---

## Suggested execution order

1. **P0-C, P0-B** first — smallest diffs, fully self-contained (`trace.rs`, `locate.rs`), zero
   interaction with authority/CAS internals, each independently testable and shippable same
   day.
2. **P0-D** immediately after — the schema-validation meta-test is cheap to add once P0-B/C's
   fixes exist as reference-correct examples, and it's the highest-leverage single item for
   preventing a Wave 9 rediscovering the same bug class a third time.
3. **P1-A, P1-B** together — both touch the same `edit.rs` neighborhood (`observe_spend_
   snapshot` / stale-base error path), small and low-risk, worth bundling.
4. **P1-C** — quick grep-and-confirm; may already be a non-issue.
5. **P0-A last** — by far the largest design surface (touches `calm_core::authority`,
   `calm_core::change::intent`, both the no-authority and full-authority gate paths, plus
   `edit_context`'s own param surface). Do not rush this opposite the smaller items above;
   read `ReviewAuthority`/`ChangeIntentTarget` in full before committing to the range-scope
   enum shape, the same discipline every prior wave in this repo applied before touching
   authority internals.

Full workspace suite (`calm-core` + `calm-server` + `calm-cli`), fmt/clippy `-D warnings`, and
`diff_impact` on the final diff are all required before calling any item SHIPPED — consistent
with every prior wave's own closure bar in this repo.
