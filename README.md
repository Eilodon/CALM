# CALM — Coding Agent Liveness Map

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Eilodon/CALM/actions/workflows/ci.yml/badge.svg)](https://github.com/Eilodon/CALM/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/%40eilodon%2Fcalm-mcp?label=npm)](https://www.npmjs.com/package/@eilodon/calm-mcp)
![Languages](https://img.shields.io/badge/languages-24%20parsed%20%C2%B7%2013%20call--graph%20by%20default%20%C2%B7%2012%20formal--verified-informational)

**A live, graph-verified map of your codebase — so an AI coding agent can edit with its eyes open instead of grepping in the dark.**

Real call graphs, not vector-similarity guesses · hard safety gates before risky edits · memory that survives a restart — every claim below is measured against CALM's own codebase and a reproducible benchmark suite, not just asserted.

**New here?** [Quick start](#quick-start) gets you running in under a minute — no clone, no Rust toolchain, works with [Claude Code, VS Code, Cursor, Windsurf/Devin Desktop, Codex, Antigravity, and JetBrains](#quick-start). **Comparing this against other agent-pipeline tools?** Jump straight to [Proof, not promises](#proof-not-promises). **Want the internals?** [`docs/architecture.md`](docs/architecture.md) covers multi-tier indexing, the SCIP/LSP overlay system, the concurrency model, and the sanitization layer in full.

| | |
|---|---|
| **Coverage** | 24 languages parsed · 6 with full call graphs · 12 with a formal/compiler-verified upgrade path |
| **Safety** | The only 1 of 5 real MCP servers tested that refused an unconfirmed edit to a verified hub symbol |
| **Self-graded** | 9.5% hub concentration · 5.5% dead code · 0 architecture-boundary violations, on CALM's own 2,689-symbol codebase |

Full methodology and more numbers (including a ~60% token-reduction result on repeat hub-symbol lookups) in [Proof, not promises](#proof-not-promises) below.

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

- **Your agent stops guessing who depends on what.** `callers`/`callees`/`edit_context` show every real caller before a change ships — full `tree-sitter` call graphs for **13 languages out of the box** (Python, TypeScript, JavaScript, Java, Rust, Go — zero-config Tier-0 — plus C, C++, C#, Ruby, PHP, Shell, and R, on by default via the `tier0-5` grammar bundle), plus call-graph coverage for 11 more (Kotlin, Swift, Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy) behind an opt-in `--features lang-X` flag not compiled into the shipped binary — those fall back to a regex/line-scan symbol extractor with no call graph until you build with that flag (24 languages parsed in total; see [multi-tier indexing](docs/architecture.md#multi-tier-indexing)).
- **Edits that can't silently break things.** Every write is hash-verified against the exact line range, syntax-checked before it ever touches disk, and hub/high-fan-in symbols hard-refuse without an explicit `confirm:true` — a policy only a tool with a real dependency graph can enforce. Proven, not just claimed — in benchmark runs against several established open-source MCP servers, that gate refused an unconfirmed edit to a verified hub symbol when not every tool tested had an equivalent one. See [Measured against the tools that came before it](#measured-against-the-tools-that-came-before-it).
- **When your compiler can double-check the graph, CALM asks it to.** SCIP overlays (`rust-analyzer`, `scip-go` — including multi-module `go.work` workspaces — `scip-python`, `scip-ruby`, and more) and live LSP overlays (`gopls`, `clangd`) upgrade "best guess" edges to formally verified ones across 12 languages, with zero behavior change on a machine that doesn't have the toolchain installed.
- **Memory that survives a restart.** `remember`/`recall` keep architecture decisions and gotchas around across sessions instead of making the agent re-derive them from scratch every time.
- **A codebase that grades itself.** `fitness_report` turns hub concentration, dead code, and architecture-boundary violations into a queryable, CI-enforceable signal instead of a one-off audit.
- **Plays well with others, and stays on your machine.** A cross-process edit lock and single-writer indexing model mean two editor sessions on the same repo don't corrupt each other's writes or double-index — under the shared daemon, sessions can even see each other coming. No code leaves your machine for indexing, search, or editing; the one narrow exception (a default embedding model download) is opt-out-able. MIT-licensed.

## Where CALM fits

"Code intelligence for AI agents" is a real product category now, not a niche — built up by open-source pioneers (Aider, Serena, Sourcegraph/Cody, and others) that first proved an AI agent works better with real code structure under it than with grep and good intentions. CALM owes its starting assumptions to that work; it exists to close the two gaps a 2026 independent survey of the category called out plainly:

> "No tools [in this category] implement pre-edit safety gates or impact warnings before structural changes."
>
> "Memory integration [is] notably absent across all tools — a gap that remains."

**Hard safety gates before risky edits**, and **memory that survives a session restart**, are the two things CALM is built around as a result. Reality turned out more nuanced than "notably absent" — at least one predecessor (Serena) already had working cross-session memory, which was a genuinely useful reference point while designing CALM's own `remember`/`recall` — but the pre-edit safety-gate gap held up in CALM's own testing, and closing it is the part of CALM's design most distinctly its own (see [Measured against the tools that came before it](#measured-against-the-tools-that-came-before-it) below).

The trade-off is honest, not hidden: CALM's full-call-graph tier out of the box is 13 languages, not the 40+ some pure-LSP tools reach — but tree-sitter parsing itself now spans 24 languages (11 more behind a build-time flag), and 12 of those have a formal- or LSP-verified upgrade path wired, so that gap is narrower than it used to be. What doesn't change is the differentiation underneath: confidence-graded edges, hard pre-edit gates, durable memory, and a codebase that grades its own health — each backed by a number you can reproduce yourself, not just a claim (see [Proof, not promises](#proof-not-promises) below).

### Is CALM the right fit?

**Good fit:** agents that edit code directly, not just answer questions about it · single-repo codebases in a Tier-0/Tier-0.5 language · teams that want cross-session memory instead of re-deriving context every run · projects running multiple MCP clients (see [supported clients](#quick-start) below) against the same repo · local-first users who don't want to depend on an embedding API.

**Not the fit today:** multi-repo/cross-repo enterprise search — tools purpose-built for that scale (Sourcegraph/Cody among them) will serve you better · a language nowhere in CALM's current 24-language tree-sitter set.

CALM is under continuous, active development — the language matrix, the concurrency model, and the benchmark suite below all shipped or grew within the current week, not a one-time launch.

## Quick start

**Supported clients** — CALM is any MCP client that speaks stdio; these are wired up or documented today:

| Client | Modes | Fastest install |
|---|---|---|
| **Claude Code** | CLI · Web · IDE | `claude mcp add --transport stdio calm -- npx -y @eilodon/calm-mcp serve` |
| **VS Code** | IDE (native MCP / Copilot Agent mode) | `code --add-mcp '{"name":"calm","command":"npx","args":["-y","@eilodon/calm-mcp","serve"]}'` |
| **Cursor** | IDE · Cloud (Background Agent) | [Add to Cursor →](cursor://anysphere.cursor-deeplink/mcp/install?name=calm&config=eyJjb21tYW5kIjoibnB4IiwiYXJncyI6WyIteSIsIkBlaWxvZG9uL2NhbG0tbWNwIiwic2VydmUiXX0=) |
| **Windsurf / Devin Desktop** | IDE · Cloud | edit `~/.codeium/windsurf/mcp_config.json` |
| **Codex** (OpenAI) | CLI · IDE | `codex mcp add calm -- npx -y @eilodon/calm-mcp serve` |
| **Antigravity** (Google) | CLI · IDE | edit `~/.gemini/config/mcp_config.json` |
| **JetBrains AI Assistant** | IDE | via UI settings |

Full walkthrough for every client above, including exact global-config snippets for the ones that need one — [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) (Vietnamese).

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

Drop that into `.mcp.json` (Claude Code/Cursor) or `.vscode/mcp.json` (VS Code uses a top-level `"servers"` key instead of `"mcpServers"`, same shape otherwise) at your project root. Claude Code plugin instead: `/plugin marketplace add Eilodon/CALM` then `/plugin install calm@CALM`.

Prefer a native binary over npx? `curl -fsSL https://raw.githubusercontent.com/Eilodon/CALM/main/scripts/install.sh | sh`, then run `calm setup` from inside your project — it writes the same MCP config automatically, pointing at the binary you just installed. Add `calm setup --npx` instead to write the portable `npx` entry (shareable/committable — teammates and CI don't need the binary, and it tracks the published release).

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

This repo ships ready-made config for Claude Code (`.mcp.json`), Cursor (`.cursor/mcp.json`), and VS Code (`.vscode/mcp.json`) — all three point at `scripts/mcp-launcher.sh`, a shared launcher that finds an already-built binary, downloads a checksum-verified prebuilt release if you're on a matching git tag, or builds from source if nothing is available yet. Clone the repo and it just works — no manual build step required first.

> **Note:** `calm serve` automatically adds `.calm/` to `.gitignore` on startup so the index database never gets committed.

## Example: an agent's actual workflow

```
agent: repo_overview()
  → 192 files, 2,689 symbols, 175 hub symbols, indexing_phase=ready

agent: "I need to change getUserByEmail"
  → locate("getUserByEmail")        # find the file + symbol metadata
  → source("getUserByEmail")        # read just the function body, not the whole file
  → edit_context("getUserByEmail")  # MANDATORY before any edit
      → 12 callers, risk_assessment=high → agent reviews each caller before touching the signature
  → edit_symbol("getUserByEmail", expected_hash=..., new_text=...)
      → risk_assessment=high, is_hub=true, no confirm:true → refused, with an explanation
  → edit_symbol(..., confirm=true, reason="checked getUserByToken, still returns the same shape")
      # reason must cite a real caller edit_context returned, not a generic phrase — writes for real, reindexes immediately  → diff_impact(staged=true)        # verifies blast radius before commit
```

## Proof, not promises

Numbers are cheap to claim and easy to fake. These are measured, today (2026-07-11), by pointing CALM's own `fitness_report`/`repo_overview` at its own codebase — not aspirational, and reproducible by running the same two tool calls yourself:

| Metric | Measured value |
|---|---|
| Codebase indexed | **192 files, 2,689 symbols** — 15 languages present in this repo alone |
| Hub concentration (`hub_pct`) | 9.5% — 175 hub symbols (gate: ≤ 20%) |
| Self dead-code rate (`dead_code_pct`, coverage-aware) | 5.5% (gate: ≤ 10%) |
| Edge coverage (`edge_coverage_pct`) | 74.7% of symbols have at least one call edge (gate: ≥ 60%) |
| High-complexity functions (`high_complexity_pct`) | 2.3% (gate: ≤ 15%) |
| Architecture boundary violations | 0 (declared rules actively enforced, not aspirational) |
| Token efficiency | ~60% fewer tokens on a repeat `callers()` call to a hub symbol (list capping + etag caching) |
| Full test suite (default features) | **826 passed**, 0 failed (12 ignored — live-binary integration tests for external tools, e.g. `rust-analyzer`/`scip-go`/`scip-java`, not installed in every environment) — see [`Testing`](#testing) for caveats on two environment-sensitive suites |

For context on the SCIP overlay's actual lift: an earlier measurement found 1,619 / 2,096 Rust call edges (77.2%) upgraded to `formal` (rust-analyzer ground truth) on a smaller snapshot of this graph, up from 0% before the overlay existed — not re-measured at the current graph size, but the mechanism hasn't changed. A separate, stricter Rust-only measurement against a full `rust-analyzer` SCIP oracle (precision/recall, not just "% upgraded") found precision 0.795 / recall 0.193 for the pre-overlay syntactic resolver alone — i.e. what it claims is usually right, but it was missing most of the oracle's edges before the SCIP overlay closes that gap; that number predates the overlay and hasn't been re-run since. Reported here, unflattering parts included, because that's this project's own stated benchmark policy.

<details>
<summary><strong>Full competitor-benchmark methodology, and per-language honest caveats</strong></summary>

### Measured against the tools that came before it

Rather than take the positioning above on faith, `benchmarks/b11_extended_competitor_ab/` installs and calls four established open-source code-intelligence MCP servers — CodeGraph, Semble, grepai, and Serena — against an isolated git worktree of this repo, 5 repeats per task, with a correctness oracle for every task. The goal isn't a leaderboard; it's checking CALM's own claims against real, running prior art instead of a marketing page.

What held up: CALM matched the best result on caller-recall and blast-radius tasks, and was the only one of the five servers whose pre-edit safety gate actually refused a risky, unconfirmed edit rather than just being able to describe the risk after the fact. On durable cross-session memory, CALM and Serena were the only two of the five with any at all — a useful data point rather than a surprise, since Serena's approach to memory was part of what shaped CALM's own `remember`/`recall`.

Reported honestly, including where CALM isn't the cheapest: on one token-efficiency task its compression ratio was the lowest of the five tools tested, and on another it used more tokens than a naive grep baseline. The pattern across all four tasks: CALM's correctness stayed at or near the ceiling every time, even on the tasks where its token efficiency didn't. Full methodology, every task, and the raw per-tool numbers live in the benchmark's own README.

### Language coverage, measured not asserted

`benchmarks/resolution/` runs the tier-distribution baseline (resolved / inferred / textual / ambiguous split — no oracle, one real OSS repo per language) across the 19 newly-added or Tier-0.5 languages. Headline findings reported as-is, including the unflattering ones: Kotlin (89.6%) and OCaml (86.3%) land mostly in the `ambiguous` tier from common short method-name collisions (the same pattern already seen on C++); Dart produces symbols but **zero** call edges, a documented grammar limitation (no call-expression node in that tree-sitter grammar), not a bug; `inferred%` is 0.0% across the 11 Phase B/C languages because Tier-2 type inference is only wired for the original Tier-0 languages so far. Full per-language table in the benchmark's own README.

</details>

## How CALM works

Full technical detail lives in [`docs/architecture.md`](docs/architecture.md) — including the design philosophy behind why every response carries `suggested_next` and why the risky steps are hard-gated instead of just recommended. Section-by-section summary:

- **[Multi-tier indexing](docs/architecture.md#multi-tier-indexing)** — 6 languages with full call graphs always on, 18 more behind opt-in grammar features, 24 parsed in total.
- **[A call graph you can actually trust](docs/architecture.md#a-call-graph-you-can-actually-trust)** — every edge is labeled by confidence (`resolved`/`inferred`/`formal`/`textual`); SCIP and LSP overlays upgrade edges to compiler-grade ground truth across 12 languages.
- **[Search that actually finds things](docs/architecture.md#search-that-actually-finds-things)** — FTS5 + semantic embeddings fused via Reciprocal Rank Fusion, plus real grep/glob straight off disk for files the indexer never parses.
- **[Editing with an actual safety net](docs/architecture.md#editing-with-an-actual-safety-net)** — hash-verified writes, syntax validation before anything touches disk, and a three-part gate (fresh `edit_context`, `confirm:true`, a grounded `reason`) on hub/high-risk symbols.
- **[Concurrency & reliability](docs/architecture.md#concurrency--reliability)** — a shared daemon, cross-process edit lock, and single-instance indexing lock mean multiple editor sessions on one repo don't corrupt or duplicate work.
- **[The codebase grading itself](docs/architecture.md#the-codebase-grading-itself)** — 9 fitness metrics, coverage-aware dead-code detection, declared architecture boundaries, doc-drift detection.
- **[An agent that remembers, and knows when it's stuck](docs/architecture.md#an-agent-that-remembers-and-knows-when-its-stuck)** — durable cross-session notes, git co-change mining, a stuck-loop signal.
- **[Safe by default](docs/architecture.md#safe-by-default)** — credential/prompt-injection redaction on every tool response, local-only by default.

## Crate layout

- `crates/calm-core/` — the index engine: `tree-sitter` parsing, SQLite schema, the multi-tier resolver (conservative → inferred → formal/Stack-Graphs, SCIP, or LSP), graph algorithms (coreness, hub detection), FTS5/semantic search, analysis (hotspots, coverage, codeowners, diff-impact, dead-code), fitness metrics, gitignore management.
- `crates/calm-server/` — the MCP server (`rmcp` over stdio or a unix-socket daemon), exposing 29 tools plus the incremental file watcher.
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

## 29 MCP tools for AI agents
CLI presets filter tools by workflow phase: `orient`, `trace`, `edit`, `compound`, `full` (default) via `calm serve --preset` or the `preset` field in `config.json` — or compose a custom set from toolset (module) names, e.g. `--preset "trace,security"` or `--preset "full,-edit"` (see AGENTS.md for the full toolset list). Every response carries `suggested_next` to point at the next step — full detail on each tool and the complete workflow lives in [AGENTS.md](AGENTS.md).

| Group | Tools |
|---|---|
| Orient | `repo_overview`, `hotspots`, `fitness_report` (health snapshot — same metrics as `calm fitness-check`, queryable mid-session), `indexing_status`, `test_gap_hotspots` (ranks symbols by coreness × dead-code/test-coverage confidence — where test-writing effort pays off most) |
| Locate | `locate`, `search`, `file_overview` |
| Inspect | `source`, `symbol_info`, `understand`, `symbols_batch` (source + callers/callees for several exact `qualified_name`s in one round trip) |
| Trace | `callers`, `callees` (ordered, capped, etag-cacheable on hub symbols), `path`, `dependencies` |
| Edit | `edit_context` (mandatory before any edit), `edit_lines`/`edit_symbol` (the one write tool for arbitrary content — hash-verified; a hub/high-risk touch is refused unless `edit_context` ran for that exact symbol this session, `confirm:true` is passed, and `reason` cites a real caller `edit_context` returned), `format_files` (rustfmt via stdin only — never a positional file arg, so it can't trigger rustfmt's own crate-wide `mod`-tree discovery and reformat files outside its own `paths` list; no confirm/edit_context gate since formatting can't change semantics), `pattern_debt_register`/`pattern_debt_status` (anchor a duplicated bug pattern by qualified_name via `search(kind="similar")`, re-check later for `open`/`resolved`/`anchor_lost`), `diff_impact` (mandatory before commit) — `edit_context` and `diff_impact` are hook-enforced under Claude Code (see `.claude/hooks/calm-nudge.sh`); `session_context`'s `pending_diff_impact` is the equivalent signal on any other MCP client |
| Recover | `session_context`, `remember`, `recall` |
| Advanced | `scip_refresh`, `lsp_refresh` — force one or every SCIP/LSP provider to run now, bypassing the automatic refresh policy. `scan_text` — run the same prompt-injection/credential heuristics `source`/`understand` use against *any* text you supply (a WebFetch/WebSearch result, a subagent's report, pasted content) — local and offline, independent of any hosted LLM safety classifier. All three: `full` preset only, not in the four workflow-phase presets above — deliberate manual/rare-use escape hatches, not steps in the default flow |

### MCP Prompts — workflows packaged as slash-commands

Distinct from the `tools` above — MCP Prompts (`prompts/list`, `prompts/get`) return a single ready-made instruction message for a workflow you repeat often; MCP clients surface them as slash-commands:

| Prompt | Argument | Packaged workflow |
|---|---|---|
| `review_symbol` | `symbol` | `locate` → `source` → `edit_context` (mandatory) → risk summary before touching anything |
| `debug_symbol` | `symbol` | `understand` → `callers(max_depth=3)` → check `test_files`/`dead_code_confidence` |
| `onboard_area` | `path` | `repo_overview` → `file_overview`/`dependencies` → `hotspots` scoped to that path |
| `review_pr` | `range` | `diff_impact(commits=range)` → `hotspots` (overlap check) → `fitness_report` → aggregate risk summary before merge |
| `calm_workflow` | *(none)* | No-argument orientation to the full Stage 1-8 tool workflow — for a client that never auto-loads AGENTS.md, or a mid-session refresher |

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

- `cargo build --release` → static musl binaries via `.github/workflows/release.yml`, matrix: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (with `SHA256SUMS`), `aarch64-apple-darwin`. `scripts/mcp-launcher.sh` downloads and checksum-verifies the right platform's build automatically when checkout is on a matching git tag.
- `Containerfile`, multi-stage (`rust:alpine` → `scratch`) — a single static binary, no runtime image needed, published to `ghcr.io/eilodon/calm-mcp` (tagged by version + `latest`) on every git tag push.
- `compose.yaml` ships a hardened example (`read_only`, `cap_drop: ALL`, `no-new-privileges`, `pids_limit: 64`, `mem_limit: 256m`).
- The repo uses Git LFS for `crates/calm-core/assets/potion-code-16m/*.safetensors` (~61MB) — run `git lfs install && git lfs pull` to get the real weight file.

<details>
<summary>What happens if you skip <code>git lfs pull</code></summary>

`git clone`/`cargo build` still **compiles successfully** (`include_bytes!` just embeds raw bytes without parsing them) — but that file is a ~130-byte LFS pointer instead of the real model, so loading it **at runtime** fails ("failed to parse safetensors"), `indexing_status` reports `embeddings_status: "failed"`, and `search(kind="semantic"/"hybrid")` automatically degrades to FTS-only — no crash, just no semantic search until you run `git lfs pull` and rebuild.

</details>

## Testing

```bash
cargo test --workspace                        # unit + integration (default features)
cargo test -p calm-core --features embeddings   # includes the semantic/vector path (brute-force cosine KNN)
cargo test --test parity_test test_formal_edges   # Stack Graphs regression corpus
```

Three CI jobs run on every PR: `verify` (fmt/clippy/test/audit), `stack-graphs-corpus` (formal-resolver parity), `embeddings` (clippy + test with the `embeddings` feature).

Full workspace run, today (2026-07-11): **826 passed**, 0 failed, 12 ignored (live-binary integration tests for external tools, e.g. `rust-analyzer`/`scip-go`/`scip-java`, not installed in every environment).

## Further reading

Everything below is more detail than this README needs to make its case — pointers for anyone who wants to go deeper, not required reading:

- [`docs/architecture.md`](docs/architecture.md) — the full technical deep-dive: multi-tier indexing, SCIP/LSP overlays, search internals, the edit safety net, concurrency, self-grading, memory, sanitization, and the design philosophy behind it all.
- [`docs/comparison.md`](docs/comparison.md) — methodology-first positioning write-up against other tools in this category.
- [`docs/`](docs/) — resolver internals, migration plans, and other design notes not covered by `docs/architecture.md` above.
- [`docs/adr/`](docs/adr/) — individual architecture decision records (Stack Graphs scope, the formal-resolver approach, the LSP-optional confidence upgrade, the daemon+forwarder concurrency model).
- [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) — every MCP client install path in detail, including Windsurf/Devin Desktop and Codex global config.
- [`AGENTS.md`](AGENTS.md) — the full tool-by-tool workflow guide this project's own agents follow.
- [`benchmarks/`](benchmarks/) — the measurement suite behind every number in this README: `b2_call_graph_quality/` (precision/recall vs. a SCIP oracle), `b11_extended_competitor_ab/` (real calls against 4 other live MCP servers, not self-reported numbers), `resolution/` (tier-distribution baseline across 19 real OSS repos, one per language). Every benchmark's own README reports bad numbers alongside good ones on purpose — see `benchmarks/README.md` for that policy.

## License

[MIT](LICENSE)
