# Session Handoff — 2026-07-07 22:15

## Task Summary
Execute `docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md` (the 8-language
formal-tier SCIP plan). This session implemented and committed P0.1, P0.2, and P0.3 —
the three foundation fixes that had to land before any new-language SCIP provider
(Phase 2) would be worth building. Session paused at the P0.3→P0.4 boundary per user
direction (P0.4 is a pure refactor with no payoff until a second concrete provider
exists to validate the abstraction against).

## Current Status
STATUS: IN_PROGRESS (P0.1-P0.3 done and committed; P0.4 onward not started)

## Completed Steps
- ✅ P0.1 — wired `calm_core::scip::run_overlay` into the one-shot `calm index` CLI
  path (`crates/calm-cli/src/main.rs`), mirroring `calm-server`'s background-indexer
  call shape exactly. Commit `20f4265`. Evidence: new ignored integration test
  (`crates/calm-cli/tests/scip_overlay_cli.rs`) passes with real rust-analyzer (5
  edges upgraded); manual subprocess run with `rust.scip.enabled:false` confirms
  identical-to-before behavior when the overlay doesn't run.
- ✅ P0.2 — `parse_index`/`parse_scip_file` (`crates/calm-core/src/scip/parse.rs`) now
  take `rebase_prefix: &Path`, join+normalize occurrence paths, and handle an
  indexer-emitted absolute `relative_path` by stripping SCIP's own
  `Metadata.project_root` first (percent-decoded `file://` URI). Unknown project_root
  → path stays absolute (never silently degrades to a relative-looking string that
  could collide with a real file). Both production call sites pass an empty prefix
  (Rust always runs at repo root) — zero behavior change, confirmed by the existing
  real-rust-analyzer test still passing. Commit `40e6b40`.
- ✅ P0.3 (the plan's own "highest-leverage" item) — `call_edges.formal_source`
  migration (`'scip'|'stack_graphs'|NULL`); SCIP is now allowed to override a
  `stack_graphs`-sourced `formal` edge (never a prior `'scip'` verdict) via
  `mark_ruled_out_siblings`'s `is_formal` computation; gated-insert
  (`scip::ingest::insert_missing_edges`) creates a new `formal`/`'scip'` edge for a
  call site tree-sitter extracted (`call_sites` row exists) that
  `rebuild_graph`'s `MAX_CALLEE_CANDIDATES` cap dropped entirely. `IngestStats` gains
  `inserted`/`match_rate`, surfaced via `indexing_status`'s `scip_overlay` field
  through a new `.calm/scip-stats.json` sidecar. Config: `rust.scip.insert_missing:
  Option<bool>` (default auto-on). `types/mcp_types.ts`'s `EdgeConfidence` fixed to
  all 6 real variants. Commit `e0471f9`. **Verified on real data**: `calm index` with
  real rust-analyzer on the `rust_workspace` fixture → 5 upgraded, 1 ruled out, **3
  newly inserted** edges (the exact cap-dropped scenario this exists for),
  match_rate=0.28 (believable, not a suspicious 1.0).
- ✅ Bonus fix found while wiring P0.3's stats: all 3 `run_overlay` production call
  sites (`lib.rs`, `watcher.rs`, `main.rs`) previously only refreshed `caller_count`
  on `upgraded>0 || ruled_out>0` — missing `inserted>0`, which would have left
  newly-inserted edges' target `caller_count` stale immediately. Fixed in all three.
- ✅ Full workspace test suite green after every commit (494 passed at last check),
  clippy clean (`-D warnings`), fmt clean.
- ✅ Updated the plan doc itself (`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`)
  to mark P0.1-P0.3 as done with commit refs and actual-outcome summaries (including
  2 deliberate deviations from the original design — see "Open Decisions" below) so a
  future session doesn't redo this work or get confused about what's still open.

## Open Work
(Ordered — dependencies in plan §9. P0.1-P0.3 done; resume at P0.4.)
- [ ] P0.4 — generalize the Rust-specific runner into a data-driven `ScipProvider`
  table (new file `crates/calm-core/src/scip/provider.rs`; refactor
  `scip/mod.rs::run_overlay`, `runner.rs`, `config.rs`). Pure refactor, no behavior
  change per its own DoD. **See Open Decisions below before starting this** — the
  user was asked whether to do this now vs. defer.
- [ ] P0.5 — `multi_lang_workspace` fixture + nightly CI job — depends on: P0.4 in
  the plan's stated order, but is mostly independent busywork (fixture files + CI
  yaml) that could plausibly be done in parallel or first if that's ever useful.
- [ ] Phase 1 (parallel after P0, per plan §9): P1.1 JS stack-graphs key · P1.2 PHP
  (call_node_types FIRST) · P1.3 Tier-1.5 same-dir preference · P1.4 C/C++ · P1.5 C#
  namespace table
- [ ] Phase 2 providers (after P0.4): go, java, csharp, python, php + ops surface
- [ ] Phase 3: scip-clang · scip-typescript · **SQL module (P3.3) — independent of
  P0.4/P0.5, can start any time now** (only needs the `edge_kind` column from P0.3's
  migration, which already landed)
- [ ] Benchmark harness `benchmarks/resolution/` — build after P0.5, measure baseline
  before Phase 2

## Open Decisions
- ❓ **P0.4 timing** — user was asked (this session) whether to: (a) do P0.4+P0.5 now
  per the plan's original sequential order, (b) skip P0.4's abstraction and build one
  concrete Phase 2 provider (e.g. Go) directly against the current Rust-specific
  shape first, generalizing only once there are 2 real cases, or (c) stop here
  entirely for this session. User's answer was to update documentation for handoff
  (implying (c) for this session) — **next session should re-ask or use judgment
  based on what the user wants to tackle next**, since no explicit choice among
  (a)/(b) was made, only that this session should end cleanly.
- ❓ Gated-insert default (`insert_missing` auto-on) — shipped as auto-on per the
  plan's original lean (gates are strict: fresh cache key + unique def symbol + real
  `call_sites` row + dedup). Real-data run found 3 inserts with no apparent false
  positives, but this has only been observed on one small fixture — worth watching
  match_rate/inserted counts on a larger real repo before fully trusting the default
  at scale.
- ❓ P1.3 V2 (confidence upgrade to Resolved via package_symbols) — unchanged from
  original plan: do only if V1 measurably insufficient on benchmark repos.

## Active Context
SPEC: (none — plan doc serves as spec)
PLAN: `docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md` — now
self-annotated with ✅/⬜ status per task; read the top banner and §10 first.
BRANCH: main (uncommitted-elsewhere work note: this session found and separately
committed unrelated pre-existing work at session start — see commit `1dd4ba2`,
already landed before P0.1; not part of this plan, mentioned only so it isn't
mistaken for plan work)
CONSTITUTION_LAWS_ACTIVE: repo AGENTS.md mandatory rules (repo_overview first;
edit_context before edits; diff_impact before commit — both hook-enforced)

## Evidence Produced This Session
(Verified 2026-07-07 on working tree — supersedes the P0.1-P0.3 evidence anchors in
the plan doc's own §1, which now describe fixed-not-broken behavior)
- `crates/calm-cli/src/main.rs` — `Commands::Index` now calls `run_overlay` — T1
- `crates/calm-core/src/scip/parse.rs` — `parse_index`/`parse_scip_file` take
  `rebase_prefix`; absolute-path + `project_root` stripping; 6 new unit tests all
  passing — T1
- `crates/calm-core/src/scip/ingest.rs` — `formal_source` override logic,
  `insert_missing_edges`, `IngestStats.{inserted,match_rate}` — 5 new unit tests
  passing, all 5 pre-existing tests still passing — T1
- `crates/calm-core/src/db/schema.rs:255-268` (approx) — `formal_source` migration — T1
- Real rust-analyzer end-to-end run on `rust_workspace` fixture (copied to a tempdir,
  run via the built `calm` binary, not just `cargo test`): 5 upgraded, 1 ruled_out, 3
  inserted, match_rate=0.28, `.calm/scip-stats.json` sidecar written correctly — T1
  (this session, reproducible via the commands in commit `e0471f9`'s message)
- Full workspace test suite: 494 passed, 0 failed, 2 ignored (both real-rust-analyzer
  integration tests, both separately verified passing with `--ignored`) — T1
- `cargo clippy --workspace --all-targets --features scip-overlay -- -D warnings`:
  clean — T1
- `cargo fmt --all -- --check`: clean — T1

## Blockers
- 🚫 None. P0.4 (or a Phase 2 provider, or P3.3 SQL) can all start immediately —
  see Open Decisions for which the user should pick.

## Next Session Opening
"Read `docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`'s top banner and
§10 (both updated this session) plus this handoff file. P0.1-P0.3 are done and
committed (`20f4265`, `40e6b40`, `e0471f9`) — do not redo them. Ask the user which
of the three P0.4 options in this file's 'Open Decisions' they want (or whether
they'd rather start P3.3 SQL, which is fully independent), then proceed."

## Skills in Use
- session-handoff: this document
- (none else actively invoked this session beyond the CALM MCP tool workflow itself)
