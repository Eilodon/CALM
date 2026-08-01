# Changelog

All notable changes to CALM are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions match the
git tags in [Releases](https://github.com/Eilodon/CALM/releases).

## [Unreleased]

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
