# Changelog

All notable changes to CALM are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions match the
git tags in [Releases](https://github.com/Eilodon/CALM/releases).

## [Unreleased]

## [0.6.0] - 2026-08-06

### Added
- Opt-in WS-6 first-slice verification (`docs/plans/2026-08-03-ws6-verification-pipeline-execution-plan.md`): `[verification] rust_check_on_write` (default off) routes a `.rs` write through the durable transaction's `VERIFY_PENDING` state instead of straight to `Done`; new `verify_change(tx_id)` tool runs `cargo check` scoped to the nearest Cargo package and advances the transaction to `Done`/`Failed` -- a failed check does not revert the file already written to disk
- `plugins/calm/.claude-plugin/plugin.json`'s `version` is now checked against `Cargo.toml`'s (`scripts/check-doc-truth.sh`) so the Claude Code plugin manifest can't silently drift from the release it bundles again
- `verify_change` now binds to the transaction's `proposed_digest`, checked both immediately before and immediately after `cargo check` runs -- a concurrent write can no longer get bound to someone else's verification receipt (`VERIFICATION_SNAPSHOT_CHANGED`)
- `[verification] timeout_secs` (default 120s): `cargo check` is killed if it hangs (a stuck `build.rs`/proc-macro/registry fetch) instead of blocking the tool call indefinitely
- `calm init` now creates `.calm/` atomically at `0700` (matching the daemon's own posture) instead of a plain `create_dir_all` at the umask default; `calm doctor --fix` additionally retightens an already-loose `.calm/` and its sensitive files (`index.db`, `memory.key`, `daemon.log`, `audit.log`, `daemon.sock`)
- Non-loopback `calm serve --http` now forces a capability-derived `remote-safe` preset (every tool declaring `read_only_hint = true`, computed live off the tool router) instead of the old `full,-edit` toolset exclusion, which only ever disabled `edit_lines`/`edit_symbol`/`format_files` -- `remember`, `verify_change`, `retry_maintenance`, `scip_refresh`, `lsp_refresh`, `set_toolset`, and `pattern_debt_register` are now also excluded by default over an unauthenticated-by-default remote transport
- The audit ledger (`audit_ledger`) is now HMAC-SHA256-signed (keyed by a new 0600 `.calm/audit.key`, separate from `memory.key`) instead of a plain unkeyed SHA-256 chain -- an actor with only SQLite file write access can no longer forge a chain that still passes `verify_chain`
- `calm setup --npx` now pins the written entry to `@eilodon/calm-mcp@<this binary's own version>` by default instead of an unpinned `npx -y @eilodon/calm-mcp`, so a cold `npx` invocation always resolves to the same release; `--track latest` opts back into the old unpinned behavior
- `.calm/config.json` `risk_rules` (default empty): a path-glob-to-minimum-risk floor (e.g. `{glob: "**/auth/**", minimum: "high"}`) that the write gate can never classify below, closing the gap where a low-fan-in but security-sensitive file read as low risk regardless of caller count
- `remember` now quarantines a note whose content trips the prompt-injection heuristic (still saved, same detection-only philosophy) and `recall` excludes quarantined notes from its ambient/broad paths (FTS `query`, no-args list-all) by default -- an exact `topic` lookup still always returns it, mirroring `edit_context`'s existing `related_notes` ambient-surfacing gate
- `KNOWN_LIMITATIONS.md`: an honest catalog of what CALM doesn't do yet and why each gap is deliberately deferred rather than half-built
- Indexing now skips any file over 8 MiB (`read_source_capped`, checked via a cheap `metadata()` stat before ever reading the file) and bounds a single tree-sitter parse to 5s (`Parser::set_timeout_micros`) -- a pathologically huge or deeply-nested file can no longer hang or balloon the indexer's memory
- `compute_touch_risk` now escalates risk to `"high"` when an edit's own proposed content actually changes a touched function/method's signature TEXT (not just overlaps its line range -- a whole-body replace that leaves the signature byte-for-byte identical does not escalate), reusing `diff_impact`'s own `is_signature_semantically_changed`/`escalate_risk_if_signature_changed`
- `calm serve --http` now caps request body size (16 MiB, `axum::extract::DefaultBodyLimit`) and concurrent in-flight requests (64, `tower::limit::ConcurrencyLimitLayer`) as defense-in-depth against the unbounded-resource gap a bare `axum::Router` had; still not a substitute for a reverse proxy's real rate limiting
- New `reference_impact` tool: merges call edges, import edges naming a symbol, and a repo-wide textual grep into one classified reference list (`must_change`/`likely_change`/`review`/`textual_only`) for rename/removal planning -- closes the exact gap behind two real `benchmarks/b7_task_correctness` misses (a bare re-export statement invisible to the call graph alone)
- `edit_lines`/`edit_symbol` gained an optional `cites` param: the EXACT `qualified_name` of a caller `edit_context` returned this session, checked by equality rather than the existing `reason` field's word-boundary substring search -- closes the "paste a real caller name into an unrelated sentence" gaming path for callers that opt in; the free-text `reason` path remains for backward compatibility
- A real on-disk audit ledger connection whose `audit.key` can't be read or created (e.g. a read-only `.calm/`) now fails the write closed (`LedgerError::KeyUnavailable`) instead of silently falling back to the old unkeyed, forgeable SHA-256 chain -- the existing `append_ledger_in_savepoint` savepoint rollback (P0-4: a ledger failure must never block the write it's auditing) already does the right thing once `append` actually signals failure, now with a warn-level log so the gap is observable
- `calm connect --preset` now takes effect even when attaching to an already-live daemon, not just when this connection is the one that spawns it: a one-line handshake preamble ahead of the raw MCP byte stream lets each connection narrow its own effective tool ceiling (`CalmServer::narrow_connection_preset`), reusing the same `resolve_preset`/`current_visible_tool_names` machinery `set_toolset` already enforces -- a too-wide request is a no-op, never a privilege escalation, since the daemon's own `tool_router` (built once at spawn time) stays the hard ceiling
- New `batch_status` tool: takes a caller-supplied list of `tx_id`s (the ones a set of `edit_lines`/`edit_symbol`/`format_files` calls already returned) and reports one aggregate view -- counts by state, which are missing, whether any failed -- instead of requiring a separate `edit_transaction_status` call per file for a multi-file change. Observability only: doesn't group transactions server-side or change what those write tools do (see `KNOWN_LIMITATIONS.md` "No multi-file change-set / transaction")
- New `calm guard` CLI command: runs the exact `diff_impact` tool an MCP agent's own Stage-7 pre-commit gate uses against the staged diff (`git diff --cached`) and exits non-zero when `aggregate_risk` is at or above `--fail-on` (default `high`) -- a first Git/CI-native integration point for changes made outside any MCP session (a teammate's native editor, a bot PR), usable directly as a pre-commit hook or CI step
- Durable state (`project_memory`, `project_memory_refs`, `edit_transactions`, `tx_events`, `maintenance_jobs`, `audit_ledger`) now lives in a separate `state.db` (`PRAGMA synchronous=FULL`, `db::conn::open_state_writer`) instead of sharing the rebuildable index's `index.db` (`synchronous=NORMAL`) -- every real call site (`remember`/`recall`, `edit_transaction_status`/`batch_status`/`maintenance_status`/`retry_maintenance`/`repair_consistency`/`verify_change`, the shadow-tx paths inside `edit_lines`/`edit_symbol`/`format_files`, and the OS-level crash-injection harness) now reads and writes through it; `db::schema::migrate_legacy_durable_tables` copies any pre-split `index.db`'s durable rows into `state.db` once, idempotently, on first startup after upgrading. Closes `KNOWN_LIMITATIONS.md` "Durable state and the rebuildable index share one SQLite file at runtime"

## [0.5.0] - 2026-08-03

### Added
- Durable edit-transaction journal (`txn.rs`) and maintenance outbox, wired into `edit_lines`/`format_files`, with a startup recovery hook and 4 new admin tools (`edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`) exposed under a new `txn` toolset
- Append-only, hash-chained audit ledger (SHA-256 evidence digests) as a durable channel alongside tracing
- Caller-set-digest TOCTOU guard on the edit gate: an unrelated edit that changes a symbol's caller set since `edit_context` reviewed it now rejects the stale review (`STALE_CALLER_SET`) instead of trusting it
- Write-safety enforce-transition: no write path can bypass `EditTransaction`; critical-risk edits without an approver are blocked
- 3-mode symlink containment (`path_policy.rs`) wired into repo-path resolution
- OS-level crash-injection test suite (`txn_crash_injection`): self-raised SIGKILL after every reachable transaction-state transition, verified against disk/ledger consistency, 100 iterations/transition
- `release.yml` `qualify-release` gate (fmt/clippy/test/audit/stack-graphs corpus/fitness-check/doc-drift/cross-SDK interop) that binary and container publish jobs now depend on — a tag push can no longer reach a release without it
- Refresh reconciliation and bounded watcher supervision: shared input catalog, durable input fingerprints, explicit health reporting distinguishing completed-index state from live filesystem observation

### Changed
- `edit.rs` and the transaction tool surface reuse a single writer connection per file instead of re-opening per step; independent transaction advances batch under one `BEGIN`/`COMMIT`

### Fixed
- Rust `Self::method()` calls (inside `impl`/`trait` blocks) resolved to zero call edges instead of the enclosing type — `target_class` now substitutes the real enclosing type/trait name instead of the literal `Self` keyword
- Duplicate `call_sites` inserts aborted the whole indexing transaction instead of being skipped, permanently failing indexing on affected repos
- `watcher_integration` tests could leak a background thread and temp directory for the process's life if a panic unwound past cleanup
- Indexer-to-analysis architecture boundary violation introduced by watcher-supervision work
- CI jobs had no `timeout-minutes`, letting a hung test silently occupy a runner for GitHub's 6h default instead of failing fast
- `Cargo.lock` internal package versions left stale after a workspace version bump
- `cargo fmt` violations in the indexer test module

## [0.4.0] - 2026-08-01

### Added
- Martin/OOD metrics, ownership-entropy risk signal, churn-aware search ranking
- Dart call-edge extraction
- Elixir bare-name calls gated by arity, not just name
- JVM and Go imports resolved from declarations instead of layout guesses
- `b7_task_correctness` benchmark: real rename refactors across 6 language corpora (Rust, Python, JS, TS, Go, Java), checked against an independent pass/fail oracle
- Optional local-ONNX embedding backend (`tract`) as an alternative to the vendored default model
- Per-session dynamic toolsets: `enabled_toolsets` field, `set_toolset` tool, safety-floor enforcement at `list_tools`/`call_tool`
- Opt-in OpenTelemetry span export behind the `otel` feature
- Opt-in Streamable-HTTP transport (`calm serve --http`) — loopback-only by default, fail-closed (`--allow-remote` + bearer token required for non-loopback), forces a read-only preset remotely
- `formal_source` per-edge provenance surfaced on 5 read tools; SCIP-vs-stack-graphs override disagreement observability
- `stack-graphs-formal` feature gate — the stack-graphs family is now default-on but opt-out, instead of hard-wired
- SCIP-primary `CallSite` byte-span provenance
- Issue templates, CODEOWNERS, and this CHANGELOG

### Changed
- The 6 resolution maps bundled into a single `ResolutionMaps` struct
- `tools/common.rs` split into toolset/outcome/detail modules to clear the `hotspot_risk` fitness gate
- Formal-resolution timeout ceilings made deterministic; a previously-silent timeout swallow now surfaces

### Fixed
- Deduplicated derived-edge tables; sharpened `search`/`edit_context` precision
- SCIP-disproven and ambiguous edges no longer corrupt graph traversal (`ruled_out_by_scip` filter applied across all remaining `call_edges` consumers)
- Vendor packages no longer counted as first-party imports in benchmarks
- Transitive `@hono/node-server` dependency bumped to patched 2.0.10+
- `knn`/`knn_chunks` embedding cache no longer collides across `:memory:` SQLite connections
- B1-B4 call-graph accuracy gaps found by the 2026-07-28 benchmark root-cause (indexer/SCIP)
- B12 upgrade-plan findings F1/F2+F2b/F4 (JS/TS call-graph blind spots outside named-function bodies; edit/diff-impact fixes)
- Java `this.field.method()` call-graph blind spot
- `opentelemetry_sdk` dependency alignment — pinned to a single resolved core version after a Dependabot bump broke the build

## [0.3.6] - 2026-07-22
- Repo overview timeout analysis and follow-up fixes.

## [0.3.5] - 2026-07-21

### Added
- Human-in-the-loop elicitation veto for hub/high-risk edits — escalates confirmation to the client UI instead of agent self-confirmation.

### Fixed
- Transient re-acquire failure in the `instance_lock` CI test (flake, not a real race)
- `cargo fmt` CI check

## [0.3.4] - 2026-07-19

### Fixed
- `server.json` was missing the `serve` package argument, so registry-driven installs launched a dead server. Metadata-only release; no code changes.

## [0.3.3] - 2026-07-19

### Fixed
- Release job now only downloads `calm-*` artifacts — an unfiltered `download-artifact` step was racing Docker's `*.dockerbuild` artifact, which broke v0.3.2's release.

## [0.3.1] - 2026-07-18

### Added
- Windows and macOS Intel (x64) binary distribution
- Native hooks doctor-fix CLI subcommand and markdown semantics specs

### Fixed
- Daemon respawn race and flaky test timing assumptions
- Stale `"ci"` reference in `edit_lines` tool description

## [0.3.0] - 2026-07-15

### Added
- Composable toolset presets, tool schema snapshot tests (toolsnaps), cosign-signed release images, cross-SDK interop CI
- `calm init --hooks[=nudge|enforce|off]` generic hook scaffold (portable beyond Claude Code)
- `calm init --agents-md` scaffold + `get_info` instructions pointer for external onboarding
- `calm_workflow` MCP Prompt + `calm-guide` Skill
- SCIP toolchain sidecar Containerfile (Java/C#/PHP)
- Release binary provenance attestation (`actions/attest-build-provenance`)
- Install hint surfaced in `repo_overview` when a SCIP provider is unavailable

### Fixed
- DEBT-010 hook-state TOCTOU race
- Source-aware idempotent SessionStart injection
- `deny()` hook migrated to exit-2 (also fixed a real stderr-swallowing bug found along the way)
- `scip-nightly` restructured into per-language jobs; added Ruby + Clang; fixed a PHP crash

### Performance
- Session-state parse skipped and cleanup made probabilistic on the hot hook path

## [0.2.0] - 2026-07-13

### Added
- One-command install: `calm setup --npx`, automated release train
- Line-numbered, edit-ready `source()` reads with range mode
- `edit_symbol` `top_of_file`/`end_of_file` anchors and small-text-match mode
- Incremental graph update, on by default

### Fixed
- `edit_context` gate mishandling path-form arguments (false-denies)
- `calm-nudge` advisory hook redesigned for precision and a visible cost signal
- Local `config.json` overrides now surfaced in `repo_overview`'s health summary

## [0.1.4] - 2026-07-08

### Added
- Official MCP Registry listing, Claude Code plugin, Cursor deeplink
- SCIP providers: Java, JS/TS, Python, Go
- Standalone SQL indexer (`sqlparser`-based, deliberately no call graph — "calls" isn't coherent across SQL dialects)
- C#, C/C++, PHP heuristic indexers; JavaScript formal tier via Stack Graphs
- SCIP ops surface: `calm scip-run`, `--scip-file`, `scip_refresh`

### Fixed
- `crossbeam-epoch` bumped to 0.9.20 (RUSTSEC-2026-0204)

## [0.1.1] - 2026-07-05

### Added
- `install.sh` and npm package distribution
- Default embedding model vendored into the binary at build time (no Git LFS, no runtime network call)

### Fixed
- Several indexer accuracy gaps: dead-code/hub false positives, credential-redaction coverage, `super::` sibling-submodule import resolution, SCIP confidence-upgrade overlay re-running on incremental reindex (not just server startup)

## [0.1.0] - 2026-07-02

Initial public release — core MCP tool surface, resolver, search, `fitness-check` CLI, Layer-2 code-body chunk embeddings for semantic search.

[Unreleased]: https://github.com/Eilodon/CALM/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/Eilodon/CALM/compare/v0.3.6...v0.4.0
[0.3.6]: https://github.com/Eilodon/CALM/compare/v0.3.5...v0.3.6
[0.3.5]: https://github.com/Eilodon/CALM/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/Eilodon/CALM/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/Eilodon/CALM/compare/v0.3.1...v0.3.3
[0.3.1]: https://github.com/Eilodon/CALM/compare/v0.2.0...v0.3.1
[0.3.0]: https://github.com/Eilodon/CALM/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Eilodon/CALM/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/Eilodon/CALM/compare/v0.1.1...v0.1.4
[0.1.1]: https://github.com/Eilodon/CALM/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Eilodon/CALM/releases/tag/v0.1.0
