# ADR: CALM agent-experience upgrade — symbol-boundary integrity, small-text-match edit mode, per-file native-Edit gate

## 1. Title

Ship the Tier 1/2/4 items from the CALM agent-experience upgrade spec: a
proactive symbol-boundary-integrity check with an `edit_symbol` refusal
gate, an `old_text`-based small-text-match edit mode, a per-file (not
per-session) re-arm of the native-Edit-block hook, two `edit_symbol`
position modes for brand-new module content, and a proptest invariant
guarding `apply_hunks` against line-fusion regressions.

## 2. Context

Earlier in this session, a real bug was found and fixed: `apply_hunks`
could fuse two adjacent symbols onto one physical source line when a
mid-file replace hunk's `new_text` lacked a trailing newline — the root
cause of two live `PARSE_ERROR` landmines (`orient.rs:251`, `trace.rs:539`).
Reflecting on that bug (and comparing this session's clean CALM usage
against a prior Claude session that progressively lost trust in CALM and
partially reverted to native file tools) surfaced a sharper question: which
of CALM's safety properties depend on *this specific agent's* care,
memory, or just-read source familiarity, versus being structurally
guaranteed regardless of which agent, model, or repo is driving? That
reframing — "strip away agent memory/model/repo history, does the
guarantee still hold?" — produced a 4-tier proposal list, which
`audit-design` then FAST-audited (PASS WITH FLAGS, 3 failure modes, 2
abductive hypotheses) before `writing-plans` + `task-risk-score` turned it
into the 13-task plan this ADR closes out.

## 3. Decision

**Phase A — symbol-boundary integrity** (`crates/calm-core/src/graph/
boundary.rs`, new): a `boundary_ambiguous` column on `symbols`, computed by
`update_boundary_ambiguous_flags` (a whole-DB post-process pass, same
pattern as `graph::hub::update_is_hub_flags`, called from the identical
site in `rebuild_graph` so it inherits the same per-reindex invalidation
guarantee already trusted for `hub_kind`). Surfaced as a new
`fitness_report` health check (`boundary_ambiguous_count`); `edit_symbol`'s
replace path checks the flag and refuses with `BOUNDARY_AMBIGUOUS` before
attempting any write, regardless of what caused the ambiguity.

**Phase B — small-text-match edit mode**: `EditSymbolParams` gains an
optional `old_text` field; when set, `find_and_replace_hunk` (new,
`calm-core/src/edit.rs`) searches for it within the resolved symbol's
range, requires exactly one match, and reuses `SymbolResolution::Ambiguous`'s
reporting shape (`AMBIGUOUS_MATCH` with per-occurrence line numbers) when
it isn't unique. Explicitly gated behind Phase A's flag — a
`boundary_ambiguous` symbol refuses `old_text` mode too, since its own
range can't be trusted as a search scope (the abductive hypothesis 1
cross-reference from the design's risk assessment).

**Phase C — per-file native-Edit gate** (`.claude/hooks/calm-nudge.sh`):
`edit_context_called` (session-wide bool) became `edit_context_files` (a
per-session set of file paths). A prior `edit_context` call for file A no
longer silently unlocks native `Edit` on an unrelated, never-reviewed file
B for the rest of the session. Still per-file, not per-symbol —
correlating individual edits to a specific `edit_context(symbol)` call
remains unreliable from a shell hook (the original author's own
documented reasoning, left intact). Shipped together with an upgraded deny
message in the same commit, per the audit's explicit requirement never to
land the stricter gate before the clearer one.

**Phase D — cheap UX wins**: `edit_symbol` gained `position="top_of_file"`/
`"end_of_file"` (no symbol resolution, `path` required, reuses the
already-hash-safe `insertion_hunk` primitive with a line-1/last-line
anchor) for brand-new module-level content with no sibling to anchor on.
`edit_lines_impl` gained a `position_anchored` bool that suppresses the
"content also appears elsewhere" hash-ambiguity note for insertion modes,
which re-anchor via a fresh live parse and were never actually at risk of
the hash-collision the note warns about.

**Phase E — regression-proofing**: a `proptest` invariant
(`apply_hunks_never_fuses_two_untouched_lines`) fuzzing `apply_hunks`
against the exact bug class that started this whole investigation.

**Explicitly deferred, not built here**: Tier 2 item 6 (`tools/
list_changed` on daemon respawn) — investigation found the `calm connect`
forwarder relays stdin↔socket byte-verbatim with zero MCP/JSON-RPC
awareness, so a server-side notify call alone isn't proven sufficient;
needs its own spec once the forwarder's reconnect path is traced. Tier 3
item 7 (scoping `REASON_NOT_GROUNDED` down for behavior-preserving edits)
— requires explicit user sign-off on the structural-equivalence mechanism
per the audit's L5 (security-adjacent gate-loosening) flag; not started.

## 4. Status

ACCEPTED

## 5. Consequences

**Improved**: the fused-line landmine class is now caught proactively
(index-time flag + `fitness_report` visibility) and pre-emptively (refused
before any write, not just reactively after `validate_syntax` catches
corrupted output) — for any cause, not just the one root-caused earlier
this session. Small, surgical edits no longer require a
grep-then-preview-then-submit round trip. The native-Edit safety net no
longer has a session-wide blind spot. Brand-new module-level content
(a new `use`, a new top-level function) no longer needs an unrelated
sibling symbol as an anchoring hack. `apply_hunks`' core invariant is now
fuzzed, not just spot-checked by hand-written cases.

**Worsened / debt knowingly created**: `find_and_replace_hunk` computes its
own `expected_hash` from the same window its own line-arithmetic derives —
a boundary bug there would be self-consistent and NOT caught by
`apply_hunks`' downstream hash check. Mitigated with 2 targeted tests
(old_text spanning a line boundary, multi-byte UTF-8 old_text) found during
`task-risk-score`, not by a structural proof. The per-file hook gate adds a
small, real friction cost (one more `edit_context` call per file touched in
a multi-file session) in exchange for closing the session-wide blind spot —
accepted as the correct trade per the audit, not free. Tier 2 item 6 and
Tier 3 item 7 remain open; their own scoping work (forwarder protocol
trace; structural-equivalence mechanism design) is not started.

## 6. Alternatives Considered

**Per-symbol re-arm of the native-Edit hook** (matching each `Edit` call to
the specific prior `edit_context(symbol)` call) was considered and
rejected — the hook's own header comment, left intact, documents why:
correlating individual edits to a specific symbol-level review isn't
reliable from a shell hook with only `tool_input.file_path`/`command`
available. Per-file is the coarsest granularity that's both reliably
trackable and strictly tighter than the prior session-wide behavior.

**`tools/list_changed` notification for the daemon-respawn schema-staleness
gap** (the original Tier 2 item 6 proposal) was not implemented once
investigation showed the `calm connect` forwarder has zero MCP protocol
awareness (pure byte-verbatim relay) — the fix's real location (server
notify call vs. forwarder reconnect-then-resynthesize-initialize logic) is
still an open question, so no code was written against an unverified
premise.

**Naive whitespace-stripped comparison for loosening `REASON_NOT_GROUNDED`**
(Tier 3 item 7) was considered and rejected in the design phase — unsound
for indentation-significant languages (a Python reindent that changes
block membership would text-diff as "whitespace only" while being a real
behavior change). A true AST-shape-ignoring-trivia comparison is needed
instead, and that mechanism needs explicit user sign-off before building,
per the audit's L5 flag (this is the same *family* of decision as
"never auto-apply a security-gate loosening," even though it isn't a
CVE/advisory bypass specifically).

## 7. Evidence

Full workspace suite green throughout: `cargo test --workspace` — 696
tests in calm-core (was 689 at session start of this plan; +7: 4
`find_and_replace_hunk` tests, 2 extra edge-case tests found during
`task-risk-score`, 1 proptest), 201 in calm-server (was 194; +7 across
Phases A/B/D), plus calm-cli and integration suites, zero failures on the
final run [verified 2026-07-13 — `cargo test -p calm-core --lib` reports
696 passed, `cargo test -p calm-server --lib` reports 201 passed, both
re-run immediately before this ADR was written]. New `.claude/hooks/
test-calm-nudge.sh` (no test framework exists for this project's shell
hooks) passes [verified 2026-07-13, same session], asserting: per-file
allow/deny, that the deny message names the specific unreviewed file, that
the message explicitly distinguishes "reviewed elsewhere, not here," and
that per-file state is additive across multiple files.

The Phase E proptest was verified to actually catch the bug class it
guards, not just pass trivially: the newline-normalization fix in
`apply_hunks` was temporarily reverted, the test failed immediately with a
minimal counterexample (`prefix=["c"], suffix=["d"]` → `"c\nxd\n"` instead
of `"c\nx\nd\n"`), then the fix was restored — net diff to `apply_hunks`
itself across that revert/restore cycle is zero (verified via `git diff`).

Two isolated test flakes were observed during full-workspace runs
(`db::instance_lock::first_acquirer_succeeds_second_fails_until_first_drops`,
`calm_connect_respawns_a_daemon_running_a_stale_build`) — both confirmed
[verified 2026-07-13] unrelated to this plan: each passes consistently in
isolation and on repeated full-workspace re-runs, and neither task in this
plan touches locking or daemon-spawn code. ASSUMED, not independently
re-verified in this session: that these two tests are *generally* flaky
under `cargo test --workspace`'s full parallel load (plausible given their
lock/process-contention nature) rather than something specific to this
session's machine load — worth a dedicated look if either recurs.

## 8. Owner

**ybao (bao.nt.1992@gmail.com)**

## 8b. Known Debts (PATTERN-DEBT)

No entries in `docs/pattern-debt-registry.yaml` were introduced or
affected by this change. The one existing `status: open` entry
(`DEBT-006-ty-subprocess-premise-invalid`, a `ty` type-checker resolver
premise-invalidation issue) is unrelated — not touched by this plan.

New, not-yet-registered debt from Section 5 worth tracking if it recurs:
`find_and_replace_hunk`'s self-consistent-hash blind spot (mitigated by 2
tests, not structurally proven) — candidate for a PATTERN-DEBT entry if a
real boundary bug is ever found there.

## 9. Next Cycle Trigger

When `find_and_replace_hunk`'s boundary-line-arithmetic tests
(`find_and_replace_hunk_old_text_spanning_a_line_boundary`,
`find_and_replace_hunk_multi_byte_utf8_old_text`) are joined by a third
distinct failure mode discovered in production usage, OR when Tier 2 item
6's forwarder investigation is separately scoped (a distinct, trackable
event: a new spec file under `docs/superskills/specs/` naming the
forwarder reconnect trace), OR when a user explicitly signs off on Tier 3
item 7's structural-equivalence mechanism.

## 10. Cycle Retrospective

- **Assumption that proved wrong**: adding one new required struct field
  (`EditSymbolParams.old_text`) looked like a small, contained change —
  it actually required updating 12 existing struct literals across 8 test
  functions in `tools.rs`, found only by trying to compile the test target
  (`cargo build` alone doesn't compile `#[cfg(test)]` modules — `cargo test
  --no-run` was needed to surface all 12 locations at once).
- **Surprise about the codebase**: `EditSymbolParams`/`CandidateRow`-shaped
  SQL `SELECT` column lists are duplicated verbatim across 3 files
  (`common.rs::resolve_symbol_candidates`, `inspect.rs::symbols_batch`,
  `testgap.rs::test_gap_hotspots`) with no shared constant — adding one
  column to the struct required finding and fixing all 3 independently;
  the compiler caught 2 of them only because `boundary_ambiguous` has no
  default value in a positional struct literal.
- **What we'd design differently**: a single `SYMBOL_ROW_COLUMNS: &str`
  constant (or a macro) shared by all 3 `SELECT` sites would make the next
  column addition a 1-line change instead of a 3-file hunt; not fixed here
  since it was out of this plan's scope, but worth doing before the next
  `symbols`-table column lands.
- **Debt knowingly created**: `find_and_replace_hunk`'s hash
  self-consistency gap (Section 5) — accepted with 2 targeted tests rather
  than blocked on a structural fix, since the risk is narrow (only affects
  the brand-new `old_text` mode, not the existing `replace`/insertion
  paths) and a proof would have meaningfully expanded this plan's scope.
- **Signal for the next cycle to watch**: if `edit_symbol` gains a 3rd
  small-text-match-shaped feature, extract the SELECT-column-list
  duplication fix (previous bullet) at that point rather than deferring
  again — two deferrals in a row on the same debt is the threshold worth
  actually stopping for.
