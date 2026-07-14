---
title: CALM — VHEATM 16.1 Full-mode / Tier-3 deep audit
date: 2026-07-14
mode: Full
tier: 3 (Critical) — self-audit (auditor co-authored much of the code under review)
stakeholder: repo owner (ybao)
goal: (1) risks/blind spots, (2) optimization opportunities, (3) simplification opportunities without loss of power/accuracy
independent_judge: general-purpose subagent, blind to auditor reasoning — 5/5 sampled claims confirmed TRUE, 0 divergence
status: RE-AUDITED AND FIXED 2026-07-14 (second pass) — see "Re-audit outcome" at the bottom.
  First pass produced findings from evidence + one prior audit's own claims;
  second pass re-verified every finding against REAL CODE ONLY (rejecting
  docs/specs/comments as evidence), fixed what re-verification confirmed as
  real code bugs, and corrected this doc's own §1.2 which turned out wrong.
---

# CALM — VHEATM Full Audit (2026-07-14)

**Context declared:** CONTEXT_MODE=LIVE (dogfooded daemon, real multi-project usage) ·
SELF_AUDIT=YES (QBR ×1.20 applied) · AUDIT_TARGET_TIER=3 (hook-enforced edit/commit
gate — blast radius is the user's own workflow across 3 live projects) · AI_INTEGRATED=YES
(CALM's own tools shape another AI's actions) · LANGUAGE=rust (primary).

**Evidence base:** `fitness_report`, `hotspots`, `pattern_debt_status`, `test_gap_hotspots`
(CALM's own tools) + direct source reads (`memory.rs`, `edit.rs` core+server, `sanitize.rs`,
permission bits, `pattern-debt-registry.yaml`) + `cargo test` ground truth + one Independent
Judge subagent pass (fresh context, no shared reasoning, told to actively falsify).

---

## 1. Risks, dangers, blind spots — FIRST-PASS FINDINGS (see bottom for what survived re-audit)

### 1.1 [CONFIRMED, MANDATORY → FIXED] `test_gap_hotspots` misreports CALM's own best-tested code as untested
**Evidence:** all 20 top-coreness MCP handlers (`edit_context`, `remember`, `recall`,
`edit_symbol`, `diff_impact`, `session_context`, `source`, `locate`, `callers`, …) show
`test_files: []`. But `crates/calm-server/src/tools.rs:597` holds a centralized `mod tests`
with 171 functions that call these handlers directly (`edit_context`: 13/13 pass, `edit_symbol`:
18/18, `remember`/`recall`: 16 pass — independently re-run and confirmed).
**Root cause (confirmed against live production `index.db`, not just theory):** `build_health`
(`tools/inspect.rs`) determined "has a direct test" via `is_test_file(from_path)` — a **filename
substring heuristic** (`"test"`/`"spec"`/`"tests/"`) — instead of the parser's already-computed,
attribute-accurate `symbols.is_test` flag on the *calling* symbol. `tools.rs` has no test-ish
filename, so all 171 tests there were invisible to it, even though `call_edges` correctly
recorded the cross-file relationship and `symbols.is_test=1` was correctly set on every test
function.
**FIXED 2026-07-14:** `build_health`'s `test_files` query now joins `call_edges` to
`symbols.is_test`, OR-ed with the original filename heuristic as a fallback for callers with no
`symbols` row. Regression test added
(`test_gap_hotspots_recognizes_test_caller_in_a_non_test_named_file`). Full suite green.

### 1.2 [ORIGINAL FINDING WAS WRONG — corrected] `boundary_ambiguous_count = 52`
**Original claim (retracted):** "fix is spec'd (2026-07-13 spec doc Tier-1 item #1), not
implemented." **Re-verified against real code 2026-07-14 and found FALSE:** the mechanism is
fully implemented, tested, and running correctly. `symbols.boundary_ambiguous` is a real column,
written at index time by `graph::boundary::update_boundary_ambiguous_flags` (called from both
reindex paths in `indexer/pipeline.rs`); `fitness_report` surfaces `boundary_ambiguous_count`
with a threshold; `edit_symbol`'s replace path refuses when the flag is set
(`tools/edit.rs:139`), with dedicated tests
(`edit_symbol_replace_refuses_a_boundary_ambiguous_symbol`,
`edit_symbol_old_text_mode_refuses_on_boundary_ambiguous_symbol`). The "52" is real ambiguous
symbols the mechanism correctly caught — a fact about the codebase's current AST shape, not a
missing feature. **What was actually wrong: the spec doc**, which still read as a forward-looking
proposal. Fixed by adding a verified-status note to
`docs/superskills/specs/2026-07-13-calm-agent-experience-upgrade.md` pointing at the exact
files/tests proving items 1-3 are shipped, so a future reader doesn't re-investigate or
re-implement something that already exists.

### 1.3 [CONFIRMED, MEDIUM → FIXED] Doc-comment sandwich in `insertion_hunk_for(Before)`
Re-verified: `insertion_hunk` (calm-core) still anchors `Before` at the symbol's own raw
`line_start` (unchanged, by design — see below). The former mitigation only warned the caller.
**FIXED 2026-07-14, root cause (not just the symptom):** `insertion_hunk_for` (the server-side
live-parse caller, `tools/edit.rs`) now calls a new `leading_doc_comment_start` helper that scans
upward from the live file text for a contiguous leading doc-comment block — single-line markers
(`///`/`//!` Rust, `#` Python/Ruby, `//` C-family/JS/TS/Java/C#/Go/Kotlin/Swift/Scala) or a
`/* ... */` block — and moves the actual insertion anchor above it. No schema migration needed
(the previously-assumed-necessary `doc_start_line` column): this function already re-reads the
live file on every call, so the doc-comment position never needed to be persisted. **Verified
residual gap, not a regression:** an attribute (`#[derive(...)]` etc.) between the comment and
the symbol still defeats detection — confirmed directly against tree-sitter-rust 0.23.3 that
`attribute_item` is a separate top-level sibling, not folded into the item's span. The warning
still fires correctly in exactly that case. Two tests added (fix path + residual-gap path), both
green.

### 1.4 [CONFIRMED, LOW → FIXED] `.calm/audit.log`/`daemon.log` permissions
Both files inherited the umask-derived default (0664 observed) instead of an explicit mode,
inconsistent with `.calm/memory.key`'s deliberate 0600. Content is metadata-only (no secrets),
but reveals file paths/session activity to any other local user on a shared box. **FIXED
2026-07-14:** `init_daemon_tracing` (calm-cli) now opens both files with `OpenOptionsExt::mode
(0o600)`. Added to the existing `daemon_calm_dir_and_socket_have_restrictive_permissions`
integration test (which spawns a real daemon). This repo's own existing files were also
retroactively `chmod 600`'d (the code fix only affects file creation, not files that already
existed).

### 1.5 [CONFIRMED, MEDIUM-HIGH → FIXED] "Supported languages" overstated real cross-reference accuracy
`repo_overview` lists 15 languages; only 3 have any SCIP overlay attempted (rust 44.9% match,
python 8.3%, javascript 0.04% — effectively zero), and 6 more (go/java/csharp/php/c/ruby) have
none at all. This was only discoverable via a separate `indexing_status` call. **FIXED
2026-07-14:** added `health_summary.weak_cross_reference_languages` to `repo_overview` (the one
call every session is guaranteed to make) — raw per-language `available`/`last_match_rate` data
for the repo's own languages, not a verdict. Live-verified end-to-end against this repo's real
`index.db` after rebuild+reconnect: correctly surfaces all 9 SCIP-tracked languages given their
current match rates.

### 1.6 [RE-VERIFIED WITH FRESH EVIDENCE, no code change needed] Daemon/concurrency hardening
Originally flagged "not re-verified this cycle." Re-verified 2026-07-14 with fresh evidence, not
memory: `cargo test -p calm-core --lib edit_lock` (2/2 pass), `sigterm_shutdown` integration test
(pass), `signal_shutdown`/stale-build detection code confirmed present in `daemon.rs`, and this
exact mechanism was witnessed live twice this session (forced daemon reconnects during the fix
cycle). No regression found.

---

## 2. Optimization / upgrade opportunities — STATUS

| # | Opportunity | Status |
|---|---|---|
| A | Fix `test_gap_hotspots`'s cross-file test-attribution blind spot | **DONE** (§1.1) |
| B | Surface per-language SCIP overlay match-rate in `repo_overview` | **DONE** (§1.5) |
| C | Implement index-time `boundary_ambiguous` flag | **already done before this audit** (§1.2 — original finding was wrong) |
| D | `chmod 600 .calm/audit.log` | **DONE** (§1.4, code + retroactive) |
| E | Promote `sandwich_warning` to auto-correct | **DONE** (§1.3 — anchor now moves automatically; warning is now only a residual-gap signal) |

## 3. Simplification opportunities — UNCHANGED, not addressed this pass

**I.** `crates/calm-server/src/tools/common.rs` (now ~2100+ lines, still the top `hotspots`
entry) — splitting response-envelope / DB-plumbing / shared-formatting concerns into separate
files remains a valid, undone simplification. Not touched this session (out of scope: no root
cause to fix, a pure refactor).

**II.** `.claude/hooks/calm-nudge.sh` duplicate-policy-in-bash pattern — still valid, not
addressed. The CLI subcommand idea (`calm hook-check <path>`) remains a real but separate
project.

**III.** Language support-matrix scope-honesty — §1.5's fix addresses the *visibility* half
(agents can now see the gap); actually finishing SCIP integration for the 6 zero-overlay
languages, or demoting them from "supported," remains undone and out of scope for a bug-fix pass.

---

## Re-audit outcome (2026-07-14, second pass)

Per explicit instruction: re-audit this report against **real code only** (reject docs/specs/
comments as evidence), fix what's actually wrong in docs/specs when found, and root-cause + fix
what's actually wrong in code when found.

- **4 of 6 risk findings were real code bugs** — root-caused and fixed: §1.1, §1.3, §1.4, §1.5.
- **1 finding (§1.2) was itself wrong** — the code was already correct; the *spec doc* was stale.
  Fixed the doc, not the (nonexistent) code bug.
- **1 finding (§1.6) was re-verified as still correct**, this time with fresh test-run + live
  evidence instead of carried-forward memory.
- **1 bonus bug found and fixed while re-verifying #1**, not in the original report: `edit_lines`/
  `edit_symbol`'s `validate_syntax` re-parses the *whole* file and rejects on any tree-sitter
  error anywhere in it — including pre-existing ones the edit never touched. Concretely broken by
  `&raw const`/`&raw mut` (stable Rust; tree-sitter-rust 0.23.3 has no grammar rule for it),
  present in `inspect.rs`, which silently blocked *every* edit to that file — proven with a
  byte-identical no-op "edit" that was still rejected. Fixed with `validate_syntax_diff`: only
  rejects when the edit strictly *increases* the parse-error count relative to the original,
  never on pre-existing errors. This was found by attempting to apply the §1.1 fix and hitting a
  false `PARSE_ERROR` — i.e., discovered through real use, not searched for.
- All fixes have regression tests. Full workspace suite green (932+ tests across calm-core,
  calm-server, calm-cli, both with and without the `scip-overlay` feature). Verified live via a
  real daemon rebuild + reconnect + a real `repo_overview` call showing the new field populated
  with real data.
- CALM's own `project_memory` note (`2026-07-14-edit-hook-write-gap-and-doc-comment-sandwich`),
  which had independently logged the same §1.3 root cause and proposed the schema-migration
  approach this fix avoided needing, was updated to reflect the fix.
