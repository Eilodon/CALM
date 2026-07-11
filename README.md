# CALM — Coding Agent Liveness Map

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Eilodon/CALM/actions/workflows/ci.yml/badge.svg)](https://github.com/Eilodon/CALM/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/%40eilodon%2Fcalm-mcp?label=npm)](https://www.npmjs.com/package/@eilodon/calm-mcp)
![Languages](https://img.shields.io/badge/languages-24%20parsed%20%C2%B7%206%20call--graph%20%C2%B7%2012%20formal--verified-informational)

**A live, graph-verified map of your codebase — so an AI coding agent can edit with its eyes open instead of grepping in the dark.**

Real call graphs, not vector-similarity guesses · hard safety gates before risky edits · memory that survives a restart — and, below, proven head-to-head against real competitor MCP servers instead of just asserted.

---

## The problem

An AI agent that edits code without knowing who calls the function it's about to change will, sooner or later:

- Delete "dead code" that a dozen other files still call.
- Change a signature and miss half its call sites.
- Refactor a symbol it assumed was minor — and discover, after breaking the build, that it was the hub the whole module leaned on.

None of that is a reasoning failure. It's a *visibility* failure: the agent never had a map. Give it one, and the guessing stops.

## Why "CALM"

Most coding agents operate the way anyone would in an unfamiliar codebase with only `grep`: no sense of what's wired to what, no way to know if touching this function ripples into fourteen others, no memory of the gotcha it worked out an hour ago. That's not confidence — it's fast guessing.

CALM stands for **Coding Agent Liveness Map**. *Liveness*, because the map is never a stale snapshot — it watches the filesystem, reindexes incrementally as files change, and is honest in every response about how fresh it currently is (`scanning → parsing → building_edges → ready`). *Map*, because it's an actual graph — call edges, import edges, hub/coreness metrics — not a flat text index pretending to be one. Hand an agent a live, trustworthy map of the terrain, and it stops flailing. It gets calm.

## What you get

- **A real call graph, not a vector-similarity guess.** `callers`/`callees`/`edit_context` tell an agent exactly who depends on the code it's about to touch — full `tree-sitter` call graphs for 6 languages (Python, TypeScript, JavaScript, Java, Rust, Go), plus call-graph coverage for 17 more languages behind opt-in grammar features (24 languages parsed in total; see [Multi-tier indexing](#multi-tier-indexing)).
- **Edits that can't silently break things.** Every write is hash-verified against the exact line range, syntax-checked before it ever touches disk, and hub/high-fan-in symbols hard-refuse without an explicit `confirm:true` — a policy only a tool with a real dependency graph can enforce. Proven, not just claimed: in a head-to-head run against a real competitor MCP server, that gate refused an unconfirmed edit to a verified hub symbol — the competitor had no such gate, and rewrote the file. See [Proven against real competitors](#proven-against-real-competitors-not-just-claimed).
- **Compiler-grade ground truth, on demand.** SCIP overlays (`rust-analyzer`, `scip-go`, `scip-python`, `scip-ruby`, and more) and live LSP overlays (`gopls`, `clangd`) upgrade "best guess" edges to formally verified ones across 12 languages, with zero behavior change on a machine that doesn't have the toolchain installed.
- **Memory that survives a restart.** `remember`/`recall` keep architecture decisions and gotchas around across sessions instead of making the agent re-derive them from scratch every time.
- **A codebase that grades itself.** `fitness_report` turns hub concentration, dead code, and architecture-boundary violations into a queryable, CI-enforceable signal instead of a one-off audit.
- **Safe under concurrent sessions.** A cross-process edit lock and single-writer indexing model mean two editor sessions on the same repo don't corrupt each other's writes or double-index — see [Concurrency & reliability](#concurrency--reliability).
- **Local-first.** No code leaves your machine for indexing, search, or editing; the one narrow exception (a default embedding model download) is opt-out-able. MIT-licensed.

## Where CALM sits in this space

"Code intelligence for AI agents" is now a real product category, not a niche — several tools in it have tens of thousands of GitHub stars. A 2026 independent survey of that category landed on two blunt conclusions:

> "No tools [in this category] implement pre-edit safety gates or impact warnings before structural changes."
>
> "Memory integration [is] notably absent across all tools — a gap that remains."

Those are the two things CALM is built around: **hard safety gates before risky edits**, and **memory that survives a session restart**. We didn't just take that survey on faith, either — [`benchmarks/b11_extended_competitor_ab/`](benchmarks/b11_extended_competitor_ab/) installs real competitor MCP servers and calls them, and the live results are more nuanced than the quote: one competitor (Serena) *does* have working cross-session memory, so that part of the "gap" isn't universal — but the pre-edit safety gate gap held up exactly as described, and CALM demonstrated it isn't hypothetical (see below).

The trade-off is honest, not hidden: CALM's full-call-graph tier is still 6 languages, not the 40+ some pure-LSP tools reach out of the box — but tree-sitter parsing itself now spans 24 languages, and 12 of those have a formal- or LSP-verified upgrade path wired, so that gap is narrower than it used to be. What doesn't change is the differentiation underneath: confidence-graded edges, hard pre-edit gates, durable memory — and, as of the latest head-to-head run, those aren't just claimed.

That's a claim worth checking rather than taking on faith. [`docs/comparison.md`](docs/comparison.md) has the detailed, methodology-first write-up against other tools in this category, and [`benchmarks/`](benchmarks/) goes further than docs-based comparison — it actually installs and calls real competitor MCP servers against the same corpus, reports bad numbers alongside good ones by policy, and corrects its own earlier mistakes in the open when a re-test finds one.

### Is CALM the right fit?

**Good fit:** agents that edit code directly, not just answer questions about it · single-repo codebases in a Tier-0/Tier-0.5 language · teams that want cross-session memory instead of re-deriving context every run · projects running multiple MCP clients (Claude Code, Cursor, VS Code, Windsurf, JetBrains) against the same repo · local-first users who don't want to depend on an embedding API.

**Not the fit today:** multi-repo/cross-repo enterprise search (Sourcegraph/Cody are built for that scale) · a language nowhere in CALM's current 24-language tree-sitter set.

CALM is under continuous, active development — the language matrix, the concurrency model, and the benchmark suite below all shipped or grew within the current week, not a one-time launch.

## Philosophy

CALM isn't a pile of MCP tools bolted together — it's designed as a **map and an active co-pilot for the agent actually holding the wheel**, not a dashboard for a human watching from the sidelines.

- **Every response carries `suggested_next`.** The agent is rarely left guessing what step comes next — the tool that just ran tells it.
- **The genuinely risky steps are hard-gated, not just recommended.** `edit_context` before any edit, `diff_impact` before any commit — these are enforced, not suggested. Everything lower-stakes just nudges; the agent keeps its own judgment where the cost of being wrong is low.
- **The signals are proactive, not something the agent has to ask for.** `fitness_report`, `session_context`'s `pending_diff_impact` / `possibly_stuck`, `repo_overview`'s `memory_notes_count` — the agent never has to remember "did I already check impact?" or notice on its own "am I going in circles?". CALM answers before it's asked.

The end goal is reduced cognitive load: the agent spends its budget on the work that actually creates value, not on managing its own navigational bookkeeping.

## Proof, not promises

Numbers are cheap to claim and easy to fake. These are measured, today (2026-07-11), by pointing CALM's own `fitness_report`/`repo_overview` at its own codebase — not aspirational, and reproducible by running the same two tool calls yourself:

| Metric | Measured value |
|---|---|
| Codebase indexed | **183 files, 2,590 symbols** — 15 languages present in this repo alone |
| Hub concentration (`hub_pct`) | 9.2% — 164 hub symbols (gate: ≤ 20%) |
| Self dead-code rate (`dead_code_pct`, coverage-aware) | 6.5% (gate: ≤ 10%) |
| Edge coverage (`edge_coverage_pct`) | 69.6% of symbols have at least one call edge (gate: ≥ 60%) |
| High-complexity functions (`high_complexity_pct`) | 2.4% (gate: ≤ 15%) |
| Architecture boundary violations | 0 (declared rules actively enforced, not aspirational) |
| Full test suite (default features) | **781 passed**, 0 failed (11 ignored — live-binary integration tests for external tools, e.g. `rust-analyzer`/`scip-go`/`scip-java`, not installed in every environment) — see [`Testing`](#testing) |

For context on the SCIP overlay's actual lift: an earlier measurement found 1,619 / 2,096 Rust call edges (77.2%) upgraded to `formal` (rust-analyzer ground truth) on a smaller snapshot of this graph, up from 0% before the overlay existed — not re-measured at the current graph size, but the mechanism hasn't changed. A separate, stricter Rust-only measurement against a full `rust-analyzer` SCIP oracle (precision/recall, not just "% upgraded") found precision 0.795 / recall 0.193 for the pre-overlay syntactic resolver alone — i.e. what it claims is usually right, but it was missing most of the oracle's edges before the SCIP overlay closes that gap; that number predates the overlay and hasn't been re-run since (see [`benchmarks/b2_call_graph_quality/`](benchmarks/b2_call_graph_quality/)). Reported here, unflattering parts included, because that's this project's own stated benchmark policy.

### Proven against real competitors, not just claimed

[`benchmarks/b11_extended_competitor_ab/`](benchmarks/b11_extended_competitor_ab/) installs and calls **CodeGraph, Semble, grepai, and Serena** — real MCP servers, not simulated — against an isolated git worktree of this repo, 5 repeats per task, with a correctness oracle for every task:

| Task | CALM | Best competitor | Worst competitor |
|---|---|---|---|
| `find_callers` (recall) | **2/2** | Serena / grepai / Semble — 2/2 | CodeGraph — **1/2** (misses a cross-crate caller) |
| `pre_edit_blast_radius` (recall) | **5/5** | grepai / Serena — 5/5 | CodeGraph — **1/5** (same miss, larger radius) |
| `risk_gate_refusal` | **refused** the unconfirmed edit | — | Serena had no `confirm`/`force` field — **it rewrote the file** |
| `memory_recall` (survives restart) | **yes** | Serena — also yes | CodeGraph / grepai / Semble — no persistent memory |

Reported honestly, including where CALM isn't the cheapest: on `locate_and_inspect`, CALM's token-compression ratio (7.7x) was the lowest of the five tools tested (Serena reached 238.5x); on `find_callers`, CALM used more tokens than a naive grep baseline (0.6x ratio). The pattern across all four tasks: CALM is not always the cheapest, but its correctness stayed at or near the ceiling every time, and it's the only tool tested whose safety gate actually stopped a bad edit instead of just being able to answer questions about one. Full methodology, all tasks, and raw numbers in the benchmark's own README.

### Language coverage, measured not asserted

[`benchmarks/resolution/`](benchmarks/resolution/) runs the tier-distribution baseline (resolved / inferred / textual / ambiguous split — no oracle, one real OSS repo per language) across all **19** newly-added or Tier-0.5 languages, most recently extended to cover the 11 languages from the 25-language expansion's Phase B/C batch. Headline findings reported as-is, including the unflattering ones: Kotlin (89.6%) and OCaml (86.3%) land mostly in the `ambiguous` tier from common short method-name collisions (the same pattern already seen on C++); Dart produces symbols but **zero** call edges, a documented grammar limitation (no call-expression node in that tree-sitter grammar), not a bug; `inferred%` is 0.0% across all 11 because Tier-2 type inference is only wired for the original Tier-0 languages so far. Full per-language table in the benchmark's own README.

## Quick start

**Using CALM on your own project** — no clone, no Rust toolchain:

```json
{
  "mcpServers": {
    "calm": {
      "command": "npx",
      "args": ["-y", "@eilodon/calm-mcp", "serve"]
    }
  }
}
```

Drop that into `.mcp.json` (Claude Code/Cursor) or `.vscode/mcp.json` (VS Code uses a top-level `"servers"` key instead of `"mcpServers"`, same shape otherwise) at your project root. Or from a shell:

```bash
claude mcp add --transport stdio calm -- npx -y @eilodon/calm-mcp serve
```

**[Add to Cursor →](cursor://anysphere.cursor-deeplink/mcp/install?name=calm&config=eyJjb21tYW5kIjoibnB4IiwiYXJncyI6WyIteSIsIkBlaWxvZG9uL2NhbG0tbWNwIiwic2VydmUiXX0=)** · Claude Code plugin: `/plugin marketplace add Eilodon/CALM` then `/plugin install calm@CALM`.

Prefer a native binary over npx? `curl -fsSL https://raw.githubusercontent.com/Eilodon/CALM/main/scripts/install.sh | sh`, then run `calm setup` from inside your project — it writes the same MCP config automatically, pointing at the binary you just installed.

See [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) (Vietnamese) for Windsurf/JetBrains (global config, can't be checked into a repo) and every install option in detail.

**Developing on CALM itself** (this repo):

```bash
# 1. Build the binary
cargo build --release -p calm-cli

# 2. Initialize config for your project
calm init --project-root .

# 3. Build the index (embeds symbols too, if semantic search is enabled in config.json)
calm index --project-root .

# 4. Run the MCP server over stdio — incremental reindex kicks in automatically if an index already exists
calm serve --project-root .
```

This repo ships ready-made config for Claude Code (`.mcp.json`), Cursor (`.cursor/mcp.json`), and VS Code (`.vscode/mcp.json`) — all three point at `scripts/mcp-launcher.sh`, a shared launcher that finds an already-built binary, downloads a checksum-verified prebuilt release if you're on a matching git tag, or builds from source if nothing is available yet. Clone the repo and it just works — no manual build step required first. See [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) (Vietnamese) for how the launcher decides what to do.

> **Note:** `calm serve` automatically adds `.calm/` to `.gitignore` on startup so the index database never gets committed.

## Example: an agent's actual workflow

```
agent: repo_overview()
  → 183 files, 2,590 symbols, 164 hub symbols, indexing_phase=ready

agent: "I need to change getUserByEmail"
  → locate("getUserByEmail")        # find the file + symbol metadata
  → source("getUserByEmail")        # read just the function body, not the whole file
  → edit_context("getUserByEmail")  # MANDATORY before any edit
      → 12 callers, risk_assessment=high → agent reviews each caller before touching the signature
  → edit_symbol("getUserByEmail", expected_hash=..., new_text=...)
      → risk_assessment=high, is_hub=true, no confirm:true → refused, with an explanation
  → edit_symbol(..., confirm=true)  # confirms the review is done — writes for real, reindexes immediately
  → diff_impact(staged=true)        # verifies blast radius before commit
```

## How CALM works

### Multi-tier indexing
- **6 Tier-0 languages** — Python, TypeScript, JavaScript, Java, Rust, Go — get full `tree-sitter` AST parsing, a real call graph, an import graph, and multi-tier resolution, always on, no feature flag required.
- **18 Tier-0.5 languages** — full `tree-sitter` AST parsing with call-graph and import resolution, gated behind Cargo features:
  - **On by default** (`tier0-5` feature bundle): C, C++, C#, Ruby, PHP, Shell, R.
  - **Opt-in, one feature flag each**: Kotlin, Swift (`lang-kotlin`/`lang-swift`), plus Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy — the 9 languages added in the 25-language expansion's Phase C batch. Falls back to regex/line-scan symbol extraction (no call graph) only when the matching grammar feature isn't compiled in.
  - Dart is the one exception worth calling out by name: it parses symbols cleanly but produces zero call edges, because its tree-sitter grammar has no call-expression node — a documented limitation, not a bug.
- **SQL gets its own standalone indexer** (`sqlparser`, real grammar parsing, not regex) — extracts tables/views/procedures accurately across Postgres/MySQL/SQL Server dialects, but stops short of a call graph, since "calls" isn't a coherent concept across SQL dialects the way it is for the languages above.
- **Incremental watcher** — only changed files get re-parsed (FNV-1a content hash diff); the call graph rebuilds incrementally, parallelized with `rayon`. `calm serve` picks incremental reindex automatically whenever an index already exists.

### A call graph you can actually trust
- **Every edge carries a confidence label** — `resolved` / `inferred` / `formal` / `textual` (plus `ambiguous`/`unresolved` fallback tiers when a call site's target genuinely can't be pinned down) — so an agent knows when it's looking at a sure thing versus a best guess.
- **SCIP overlay — formal, compiler-grade ground truth, 9 providers spanning 12 languages**: Rust (`rust-analyzer`), Go (`scip-go`), Python (`scip-python`), JavaScript + TypeScript (`scip-typescript`), Java (`scip-java` — also indexes Kotlin in the same pass, since `scip-java` bundles a `kotlinc` plugin for mixed Gradle/Maven modules), C# (`scip-dotnet`), PHP (`scip-php`), Ruby (`scip-ruby`/Sorbet, the newest addition), and C/C++ (`scip-clang`, shipped and unit-tested but not yet live-verified end-to-end). Each language is a data-driven `ScipProvider` entry (`crates/calm-core/src/scip/provider.rs`), not a copy-pasted module — adding another language is one table row. Every provider auto-detects its own binary and silently sits out if it isn't there (zero behavior change on a machine missing that toolchain), runs under a hard timeout, and caches against a per-language fingerprint (lockfile/build-file hash + toolchain + dirty source keys) so an unchanged project never re-pays the cost. Verification maturity varies honestly by language: Rust runs green in nightly CI continuously; Go/Java/C#/Ruby were verified live at implementation time; C/C++'s `scip-clang` path has never been live-verified (sandbox/network blockers) — treat it as wired, not proven, until it has been.
- **LSP overlay — a second, complementary path to formal edges**: for Go (`gopls`) and C/C++ (`clangd`) — plus Rust's `rust-analyzer` on a live-session path, distinct from its batch SCIP export — a real language server resolves ambiguous/textual call sites interactively, on demand. This doesn't run automatically (`policy` defaults to `on_demand`); trigger it explicitly with the `lsp_refresh` tool or `calm scip-run`/equivalent CLI. It matters most for C/C++ today, where it's the only live-verified formal path since the SCIP one isn't yet.
- Trigger either overlay on demand with `calm scip-run --lang <rust|go|python|javascript|java|csharp|php|ruby|c|all>` or the `scip_refresh`/`lsp_refresh` MCP tools; `calm index --scip-file <path.scip> --sub-root <dir>` ingests a pre-built SCIP index instead, for CI/sandboxed runs with no network access to install an indexer.
- **Graph metrics — `coreness` (k-core) and `is_hub`** — flag the symbols central enough that touching them is inherently higher-risk. `repo_overview.core_symbols` reuses the same metric to sketch the architecture's "skeleton" on the very first call (inspired by Aider's PageRank repo-map, but built on a metric CALM already computes rather than a separate pass).

### Search that actually finds things
- **Full-text + semantic search, fused** — FTS5 (BM25) combined with semantic embeddings (`model2vec-rs`, pure Rust, no ONNX) via a 3-way Reciprocal Rank Fusion (text + symbol-identity vector + code-body-chunk vector) — finds relevant code even when the query doesn't share a token with the symbol name. KNN is a brute-force cosine scan in pure Rust with an in-RAM cache — no C vector-search extension, so it behaves identically on every release platform (the previous `sqlite-vec` dependency didn't compile on musl libc, which silently killed semantic search on Linux/Docker builds). The default model (`minishlab/potion-code-16M`, MIT-licensed) is vendored straight into the binary at compile time via Git LFS — no network needed for the default case; a broken LFS checkout falls back to downloading it once from Hugging Face and caching it locally, unless you explicitly opt out to keep a strict zero-network guarantee.
- **Real grep/glob, straight off disk** — `search(kind="grep")` uses actual regex + glob filtering through a `.gitignore`-respecting walker, bypassing the index entirely — so it reaches files the indexer never parses (`Cargo.toml`, `docs/*.md`) too, each match enriched with its surrounding symbol when one exists.
- **Noise-penalty ranking** — results living in test/generated/example files are scored down when an equivalent real-implementation result exists, so the actual code surfaces first instead of getting buried under a same-named test fixture.

### Editing with an actual safety net
- **`edit_lines`/`edit_symbol`** — the one write path, working on any tracked file (not just parsed symbols). A content-hash conflict guard (FNV-1a) on the exact line range rejects stale writes and hands back the current hash/content to re-read; multiple hunks in one call apply bottom-up so offsets never drift between them.
- **Syntax-validated before it ever touches disk** — `tree-sitter` checks the result parses cleanly; a write that would introduce a syntax error is refused outright, nothing gets written.
- **Hub and high-fan-in symbols require an explicit `confirm:true`** — a policy only a tool with a real call graph can enforce.
- **Atomic writes, immediate reindex** — temp file + fsync + rename, then reindexed synchronously (not waiting on the file watcher); the response comes back with post-edit risk/callers, like a miniature `diff_impact`.
- **Hook-enforced, not just documented** — under Claude Code, `.claude/hooks/calm-nudge.sh` actually blocks the first `Edit` of a session until `edit_context` has been called, and blocks `git commit`/`git push` if files changed since the last `diff_impact`. `session_context`'s `pending_diff_impact` gives the same signal on any other MCP client.

### Concurrency & reliability
Running CALM from more than one editor session on the same repo used to mean N independent indexers, N SQLite connections, and N copies of the embedding model in RAM — real pain found by pointing CALM's own instrumentation at this repo mid-session. That's been closed from multiple directions:
- **Cross-process edit lock** — an OS `flock` on `.calm/edit.lock`, layered underneath the in-process hash check, closes a narrow TOCTOU window where two separate processes could both pass a stale-hash check and the second write would silently discard the first.
- **Single-instance indexing lock, with promotion** — only one `calm serve` process per project root ever runs the background indexer/watcher; a losing process auto-promotes to owner if the current owner exits mid-session, instead of a second editor session being stuck read-only forever.
- **A real SIGTERM watchdog** — a raw kernel `alarm()` guarantees the process exits even when the MCP transport's stdio-read thread is blocked in an uncancellable OS read, a bug that a purely async watchdog silently never caught.
- **An opt-in shared-daemon model** — `calm serve --listen unix:PATH` runs one owning daemon; `calm connect` gives every other session a lightweight forwarder over the same socket instead of its own full process, with automatic stale-build detection (`daemon.meta`) that respawns the daemon after a rebuild instead of silently serving an old binary. This is implemented and dogfooded internally, but not yet the default entry point for the npm/plugin distribution — worth knowing about if you're running many concurrent sessions today and want to opt in early.

### The codebase grading itself
- **`calm fitness-check` / `fitness_report`** — 9 metrics (hub concentration, dead code, hotspot risk, edge coverage, cyclomatic complexity, architecture-boundary violations, doc-drift) checked against thresholds in `thresholds.toml`, queryable mid-session or as a CI gate.
- **Coverage-aware dead-code detection** — auto-detects lcov / `.coverage` / Go `coverage.out` / Cobertura XML at startup and folds real runtime coverage into `dead_code_confidence`, so code a test actually exercises at runtime doesn't get flagged just because the static call graph missed the call site. `scripts/gen-coverage.sh` generates one on demand for this repo itself.
- **Architecture boundaries — `[[boundaries]]`** — declare "module A must not import module B" directly in `thresholds.toml`, matched by path prefix against the real import graph; every violation is reported with the actual offending file pair, not just a count.
- **Doc-drift detection — `[config_drift]`** — flags file-path references inside declared docs that no longer point at anything real, so a design doc doesn't quietly keep describing a file that was deleted three refactors ago.

### An agent that remembers, and knows when it's stuck
- **`remember`/`recall`** — durable, interpretive notes (an architecture decision, a gotcha) keyed by topic, surviving restarts — distinct from `session_context`, which only tracks in-session navigation and resets on restart.
- **Git co-change mining** — `edit_context` mines `git log` for files that historically change alongside the one being edited despite no import/call relationship (a model and its migration, say) — a coupling signal the static graph can't see on its own.
- **Session progress signal** — `session_context.possibly_stuck` flags 10+ tool calls with no new file/symbol touched; informational only, the decision to break the loop stays with the host (e.g. Claude Code's `/goal`).
- **MCP Prompts** — `review_symbol`, `debug_symbol`, `onboard_area` package a full multi-step workflow into one slash-command-style call.

### Honest about its own freshness
- **Index state machine surfaced everywhere** — `scanning → parsing → building_edges → ready`, so an agent never mistakes stale data for current.
- **Build-freshness check** — `calm doctor` compares the commit the running binary was built from against the repo's current `HEAD`; `scripts/mcp-launcher.sh` checks source mtimes before trusting an existing `target/{debug,release}/calm`, rebuilding rather than silently serving a stale binary.

### Safe by default
- **Output sanitization** — `source`/`understand` redact credential-shaped text (PEM keys, GitHub/AWS/Slack tokens, JWTs, password assignments) before it's ever returned, and flag a `content_warning` when code contains prompt-injection-shaped text — flagged, never silently altered, since a false positive there would corrupt real code. The heuristic (`calm_core::sanitize`, 19 labeled patterns) covers plain-English phrasing (`"ignore previous instructions"`), chat-template role-marker spoofing (ChatML `<|im_start|>`, `[INST]`/`[SYS]` brackets, fake `system:`/Markdown-heading role markers), fake tool/turn-boundary tags (`</tool_result>`, `<system>`), jailbreak/persona-override phrasing, exfiltration phrasing (prompt/secret-reveal requests), zero-width Unicode obfuscation, and a first pass at Vietnamese-language equivalents — deliberately excludes anything with real false-positive risk (e.g. generic tag-density scoring, homoglyph detection) rather than guess.
- **`scan_text` — the same detection, on demand, for anything that didn't come through the index.** `source`/`understand`'s `content_warning` only covers indexed source; every tool's own output is separately scanned (advisory-logged, not surfaced) at one central choke point (`timed_tool`). Neither covers content an agent fetches itself — a WebFetch/WebSearch result, a subagent's report. `scan_text` closes that gap: point it at any text and get the same labeled hits back, entirely local and regex-based — no dependency on a hosted LLM safety classifier being available, so it keeps working even when that classifier isn't.
- **Local-only** — no outbound calls for the code/data path. The one narrow exception is the semantic-search default model download, which is a single public, static file fetch, opt-out-able, and unrelated to your repo's contents ever leaving the machine.

## Crate layout

- `crates/calm-core/` — the index engine: `tree-sitter` parsing, SQLite schema, the multi-tier resolver (conservative → inferred → formal/Stack-Graphs, SCIP, or LSP), graph algorithms (coreness, hub detection), FTS5/semantic search, analysis (hotspots, coverage, codeowners, diff-impact, dead-code), fitness metrics, gitignore management.
- `crates/calm-server/` — the MCP server (`rmcp` over stdio or a unix-socket daemon), exposing 25 tools plus the incremental file watcher.
- `crates/calm-cli/` — the CLI: `calm init`, `calm index`, `calm serve`, `calm connect`, `calm setup`, `calm fitness-check`, `calm doctor`.

## CLI reference

```bash
calm init     --project-root .    # writes .calm/config.json with defaults
calm index    --project-root .    # one-shot full index (Scanning → Parsing → BuildingEdges → Ready)
                                 # also embeds symbols+chunks if semantic_search.enabled=true
calm serve    --project-root .    # MCP server over stdio + incremental reindex + file watcher
calm serve    --project-root . --listen unix:/path/to/daemon.sock   # run as a shared daemon (opt-in)
calm connect  --project-root .    # lightweight forwarder to an already-running daemon (opt-in, Unix)
calm serve    --project-root /project --db-path /data/index.db   # separate DB path (container deployment)
calm serve    --project-root . --preset orient   # register only the "orient" phase's tools
calm doctor   --project-root .    # validates config, DB (symbols/files/metrics history), git
calm setup    --project-root .    # writes/merges MCP config (.mcp.json/.cursor/.vscode) pointing at this binary
calm fitness-check --project-root .                             # CI gate, exits 1 on failure
calm fitness-check --project-root . --json                      # JSON output
calm fitness-check --project-root . --config thresholds.toml    # custom thresholds
calm scip-run --project-root . --lang go        # force one SCIP provider to run now, bypassing refresh policy
calm scip-run --project-root .                  # --lang omitted = run every provider ("rust,go,python,javascript,java,csharp,php,ruby,c")
calm index    --project-root . --scip-file build/index.scip --sub-root services/api   # ingest a pre-built SCIP index (CI/sandboxed, no external indexer install needed)
```

## 25 MCP tools for AI agents

CLI presets filter tools by workflow phase: `orient`, `trace`, `edit`, `compound`, `full` (default) via `calm serve --preset` or the `preset` field in `config.json`. Every response carries `suggested_next` to point at the next step — full detail on each tool and the complete workflow lives in [AGENTS.md](AGENTS.md).

| Group | Tools |
|---|---|
| Orient | `repo_overview`, `hotspots`, `fitness_report` (health snapshot — same metrics as `calm fitness-check`, queryable mid-session), `indexing_status` |
| Locate | `locate`, `search`, `file_overview` |
| Inspect | `source`, `symbol_info`, `understand`, `symbols_batch` (source + callers/callees for several exact `qualified_name`s in one round trip) |
| Trace | `callers`, `callees`, `path`, `dependencies` |
| Edit | `edit_context` (mandatory before any edit), `edit_lines`/`edit_symbol` (the one write tool — hash-verified, risk-gated), `diff_impact` (mandatory before commit) — the first and last are hook-enforced under Claude Code (see `.claude/hooks/calm-nudge.sh`); `session_context`'s `pending_diff_impact` is the equivalent signal on any other MCP client |
| Recover | `session_context`, `remember`, `recall` |
| Advanced | `scip_refresh`, `lsp_refresh` — force one or every SCIP/LSP provider to run now, bypassing the automatic refresh policy. `scan_text` — run the same prompt-injection/credential heuristics `source`/`understand` use against *any* text you supply (a WebFetch/WebSearch result, a subagent's report, pasted content) — local and offline, independent of any hosted LLM safety classifier. All three: `full` preset only, not in the four workflow-phase presets above — deliberate manual/rare-use escape hatches, not steps in the default flow |

### MCP Prompts — workflows packaged as slash-commands

Distinct from the `tools` above — MCP Prompts (`prompts/list`, `prompts/get`) return a single ready-made instruction message for a workflow you repeat often; MCP clients surface them as slash-commands:

| Prompt | Argument | Packaged workflow |
|---|---|---|
| `review_symbol` | `symbol` | `locate` → `source` → `edit_context` (mandatory) → risk summary before touching anything |
| `debug_symbol` | `symbol` | `understand` → `callers(max_depth=3)` → check `test_files`/`dead_code_confidence` |
| `onboard_area` | `path` | `repo_overview` → `file_overview`/`dependencies` → `hotspots` scoped to that path |

## Fitness check — the CI gate

`calm fitness-check` measures 9 metrics against thresholds declared in `thresholds.toml`:

| Metric | What it measures | Default threshold |
|---|---|---|
| `hub_count` | Count of symbols classified as hubs | ≤ 1000 |
| `hub_pct` | % of symbols that are hubs (scale-invariant) | ≤ 20.0% |
| `avg_coreness` | Average k-core coreness across the graph | ≤ 15.0 |
| `dead_code_pct` | % of symbols with "high" dead-code confidence | ≤ 10% |
| `hotspot_risk` | Highest hotspot score in the codebase | ≤ 0.75 |
| `edge_coverage_pct` | % of symbols with at least one call edge | ≥ 60% |
| `high_complexity_pct` | % of functions/methods with McCabe cyclomatic complexity > 10 (AST-based; Tier-0.5 languages always report complexity 1) | ≤ 15.0% |
| `boundary_violations` | Count of `import_edges` violating a declared `[[boundaries]]` rule | ≤ 0 |
| `config_drift_count` | Count of doc file-path references (declared via `[config_drift].doc_paths`) pointing at nothing real | ≤ 0 |

Every `calm fitness-check` run also snapshots metrics to the DB so `edit_context` can show a trend (delta versus the previous day).

### Architecture boundaries — `[[boundaries]]`

Declare "module A must not import module B" directly in `thresholds.toml` (same file as `[thresholds]`), matched by path prefix (not glob/regex). Note this is for layering Rust's own crate/module boundaries *don't* already enforce — declaring "calm-core must not import calm-server" would be a no-op, since Cargo's dependency graph makes that structurally impossible already:

```toml
[[boundaries]]
from = "crates/calm-core/src/indexer/"
to = "crates/calm-core/src/analysis/"
reason = "indexer (extraction) must stay upstream of analysis (dead-code, hotspots, fitness) — not the other way around"
```

`calm fitness-check` reports each violation concretely (the real from/to path, the rule, and the reason) outside `--json` mode; the default `max_boundary_violations = 0` means a rule you bothered to declare is one you actually keep.

## Deployment

- `cargo build --release` → static musl binaries via `.github/workflows/release.yml`, matrix: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (with `SHA256SUMS`), `aarch64-apple-darwin`. `scripts/mcp-launcher.sh` downloads and checksum-verifies the right platform's build automatically when checkout is on a matching git tag — see [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) (Vietnamese).
- `Containerfile`, multi-stage (`rust:alpine` → `scratch`) — a single static binary, no runtime image needed, published to `ghcr.io/eilodon/calm-mcp` (tagged by version + `latest`) on every git tag push.
- `compose.yaml` ships a hardened example (`read_only`, `cap_drop: ALL`, `no-new-privileges`, `pids_limit: 64`, `mem_limit: 256m`).
- The repo uses Git LFS for `crates/calm-core/assets/potion-code-16m/*.safetensors` (~61MB) — run `git lfs install && git lfs pull` to get the real weight file. Without LFS, `git clone`/`cargo build` still **compiles successfully** (`include_bytes!` just embeds raw bytes without parsing them) — but that file is a ~130-byte LFS pointer instead of the real model, so loading it **at runtime** fails ("failed to parse safetensors"), `indexing_status` reports `embeddings_status: "failed"`, and `search(kind="semantic"/"hybrid")` automatically degrades to FTS-only — no crash, just no semantic search until you run `git lfs pull` and rebuild.

## Testing

```bash
cargo test --workspace                        # unit + integration (default features)
cargo test -p calm-core --features embeddings   # includes the semantic/vector path (brute-force cosine KNN)
cargo test --test parity_test test_formal_edges   # Stack Graphs regression corpus
```

Three CI jobs run on every PR: `verify` (fmt/clippy/test/audit), `stack-graphs-corpus` (formal-resolver parity), `embeddings` (clippy + test with the `embeddings` feature).

> **Note:** `crates/calm-server/tests/watcher_integration.rs` waits on real filesystem-event delivery under a hard timeout. These tests are environment-sensitive and may fail in constrained containers (inotify/I/O limitations). If you hit failures, re-run in isolation (`cargo test -p calm-server --test watcher_integration`) on an unconstrained machine and treat it as a real regression only if it still fails there.

## Further reading

Resolver internals, ADRs, and migration plans live in [`docs/`](docs/) (mostly Vietnamese) — start with [`docs/comparison.md`](docs/comparison.md) for positioning or [`docs/legacy/architecture-design.md`](docs/legacy/architecture-design.md) for the original technical design. [`docs/adr/`](docs/adr/) holds the individual architecture decision records (Stack Graphs scope, the formal-resolver approach, the LSP-optional confidence upgrade, the daemon+forwarder concurrency model); [`docs/superskills/plans/`](docs/superskills/plans/) holds the working plans behind the language-expansion and formal-tier work, including what was cut from scope and why.

[`benchmarks/`](benchmarks/) has the measurement suite behind every number in this README — B2 (call-graph precision/recall vs. a SCIP oracle), B11 (real tool calls against 4 live competitor MCP servers, not self-reported numbers), and `resolution/` (tier-distribution baseline across 19 real OSS repos, one per language). Every benchmark's README reports bad numbers alongside good ones on purpose — see `benchmarks/README.md`'s own stated policy on that.

## License

[MIT](LICENSE)
