# Workflow facade design — 6-9 high-level tools over the existing 37

Status: **design only, not execution-ready** — see "Why this is a design doc,
not a PR" below. Written as the P1.5 item of the 2026-08-05 CALM-improvements
review (the same review whose other four P1 items — commit-range `calm
guard`, the GitHub Action, `calm value-report`, and `b14_risk_calibration` —
shipped same-day; this is the one that didn't, deliberately).

## The complaint this responds to

> Public surface nên tập trung vào khoảng 6–9 tool cấp cao thay vì buộc model
> chọn trong hàng chục primitive.

Verified against `docs/status.generated.md`: **37 tools**, current as of this
write-up. `AGENTS.md` already needs an explicit 8-stage workflow guide and a
"Tool Quick Reference" table to make the 37 navigable — the complaint is
real, not imagined. This doc proposes a concrete facade shape, and separately
argues for *not* shipping it as code this session.

## Where the actual complexity is (and isn't)

Grouping the 37 by `docs/status.generated.md` + `README.md`'s own "37 MCP
tools" table:

| Group | Tools | Count | Decision-paralysis risk |
|---|---|---|---|
| Orient | `repo_overview`, `hotspots`, `fitness_report`, `indexing_status`, `test_gap_hotspots` | 5 | Low — names are self-explanatory, called once per session mostly |
| Locate/Inspect/Trace | `locate`, `search`, `file_overview`, `source`, `symbol_info`, `understand`, `symbols_batch`, `callers`, `callees`, `path`, `dependencies`, `reference_impact` | 12 | **High** — `locate` vs `search` vs `understand` vs `symbol_info` genuinely overlap; this session itself repeatedly reached for `search(kind="hybrid")` when `locate` or `understand` would have compounded the same round trip |
| Edit | `edit_context`, `edit_lines`, `edit_symbol`, `format_files`, `pattern_debt_register`, `pattern_debt_status`, `diff_impact` | 7 | Low — the two mandatory gates (`edit_context`, `diff_impact`) are hook-enforced, not a choice; `edit_lines`/`edit_symbol` are a real fork (line-range vs symbol-scoped) that shouldn't be hidden |
| Txn/admin | `edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`, `verify_change`, `batch_status` | 6 | Low — rare, diagnostic-only, already excluded from the default 4 workflow-phase presets |
| Recover | `session_context`, `remember`, `recall` | 3 | Low |
| Advanced | `scip_refresh`, `lsp_refresh`, `scan_text`, `set_toolset` | 4 | Low — deliberate escape hatches, `full` preset only |

**37 tools is not uniformly the problem.** 25 of the 37 (Edit/Txn/Recover/
Advanced) are either hook-enforced, rare, or already gated behind presets —
collapsing them into a facade would remove precision (`edit_lines` vs
`edit_symbol` genuinely differ in contract) for no real reduction in
decision-paralysis, since an agent rarely chooses among them anyway. The
**12-tool Locate/Inspect/Trace group is where a facade earns its keep** — 4
tools (`locate`, `search`, `understand`, `symbol_info`) already compound each
other internally (`locate` = search + file_overview + symbol_info in one
call; `understand` = locate + source + callers), so the primitives already
exist to build a further compound on top of.

## Proposed facade (if/when built)

Six new tools, **additive** — none of the 37 removed or renamed:

| Facade tool | Wraps | Dispatch signal |
|---|---|---|
| `orient` | `repo_overview`, `hotspots`, `fitness_report`, `indexing_status`, `test_gap_hotspots` | `mode` param (`overview`\|`hotspots`\|`fitness`\|`indexing`\|`test_gaps`), default `overview` |
| `explore` | `locate`, `search`, `file_overview`, `source`, `symbol_info`, `understand`, `symbols_batch`, `callers`, `callees`, `path`, `dependencies`, `reference_impact` | `query` + `action` param (`find`\|`read`\|`trace`\|`impact`), each maps to one or two of the 12 by the same rule `locate`/`understand` already use internally |
| `prepare_change` | `edit_context` (unchanged — already the mandatory gate) | n/a, thin passthrough for naming symmetry with `apply_change` |
| `apply_change` | `edit_lines`, `edit_symbol`, `format_files` | `mode` param (`lines`\|`symbol`\|`format`) — kept distinct because the write contracts differ, not merged into one shape |
| `verify_change` | `diff_impact` (already so named as an MCP tool — **name collision, see below**), `verify_change` (WS-6 cargo-check tool, same name today) | resolve before implementing — see Naming collisions |
| `status` | `session_context`, `edit_transaction_status`, `maintenance_status`, `batch_status` | `scope` param (`session`\|`transaction`\|`maintenance`\|`batch`) |

`remember`/`recall`/`retry_maintenance`/`repair_consistency`/
`pattern_debt_register`/`pattern_debt_status`/`scip_refresh`/`lsp_refresh`/
`scan_text`/`set_toolset` stay ungrouped — genuinely standalone actions a
facade dispatcher param would just rename, not simplify.

**Naming collision already found by writing this table down**: the existing
`verify_change` tool (WS-6's on-demand `cargo check`) and the proposed
facade's natural name for "check my diff" collide with `diff_impact`'s own
conceptual role. This is exactly the kind of thing a design pass is for —
catching it here cost one table row; catching it after shipping costs a
tool rename and a second migration.

## Why this is a design doc, not a PR

Three independent reasons, each sufficient on its own:

1. **No usage-frequency data.** Which of the 12 Locate/Inspect/Trace tools an
   agent actually reaches for, and how often it picks wrong, is exactly the
   kind of thing `[[calm-multiagent-lease-research-2026-08-02]]`-style prior
   work on this project has already flagged as a blocker for interface
   redesign — "design-only, NOT execution-ready, needs usage data first" was
   the same conclusion for a comparable interface question. `calm
   value-report` (shipped earlier in this same P1 batch) is the first piece
   of exactly that missing instrumentation — its `.calm/audit.log` mining
   currently covers edit-gate decisions only, not tool-call frequency by
   name; extending it to log every `tool_execution_completed` event's `tool`
   field into a queryable form is the natural prerequisite this design
   doc is recommending, not this design doc itself.

2. **Blast radius is real, not hypothetical, on THIS exact repo.**
   `benchmarks/b12_tier1_tier2_tool_correctness/run_benchmark.py` and
   `benchmarks/b13_codegraph_multirepo_ab/run_benchmark.py` both call
   `client.call_tool("edit_context", ...)`/`"diff_impact"`/etc. by exact
   string name. `crates/calm-server/src/__toolsnaps__/*.snap` golden-file
   tests (`tool_schemas_match_committed_snapshots`) pin every one of the 37
   schemas byte-for-byte. `AGENTS.md`'s entire 8-stage guide, hook wiring
   (`.claude/hooks/calm-nudge.sh`), and this session's own `calm-guide`
   skill all name these 37 tools directly. None of that breaks under the
   *additive* proposal above (new tools, nothing removed) — but a facade
   that's additive-only also only partially addresses the original
   complaint (the 37 still exist, still show up in `repo_overview`'s
   `workflow_guide`, still need documenting) unless a second phase hides
   them behind a new opt-in preset, which is a bigger, genuinely breaking-
   for-existing-sessions change deserving its own review.

3. **The two P0 gates already caught scope creep once this session** — the
   state-schema downgrade guard hit `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`
   on a 193-caller hub and had to be redesigned around it rather than pushed
   through. `explore`'s dispatcher would sit in front of `locate`/`search`/
   `understand`/etc. — all several of which are themselves hubs with
   double-digit caller counts in `crates/calm-server/src/tools.rs`. The
   honest estimate for implementing this facade properly (dispatcher logic,
   param-shape unification across 4-12 wrapped tools per facade entry,
   updated toolsnaps, updated `AGENTS.md`, a migration note, new tests) is
   comparable in size to this entire P1 batch (five shipped items) on its
   own — it deserves that same amount of dedicated session time, not a
   tail-end addition to one.

## Recommended next step, if greenlit

1. Extend `calm value-report` (or a sibling) to log per-tool call frequency
   from `tool_execution_completed` events into a queryable form — cheap,
   already has the log line, no new mechanism.
2. Run it for a few real weeks of dogfooding on this repo + at least one
   external user's project.
3. Revisit this table with actual "which of the 12 gets called when a
   different one of the 12 was more precise" data, not the "genuinely
   overlap" judgment call this doc made from reading tool descriptions.
4. Implement `explore` first (the one group with a real signal above),
   additive, behind nothing — measure adoption before touching the other
   five lower-value facade candidates or considering a hide-the-37 preset.
