# CALM — Coding Agent Liveness Map

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Eilodon/CALM/actions/workflows/ci.yml/badge.svg)](https://github.com/Eilodon/CALM/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/%40eilodon%2Fcalm-mcp?label=npm)](https://www.npmjs.com/package/@eilodon/calm-mcp)
![Languages](https://img.shields.io/badge/languages-24%20parsed%20%C2%B7%2013%20call--graph%20by%20default%20%C2%B7%2012%20formal--verified-informational)

**A live, graph-verified map of your codebase — so an AI coding agent can edit with its eyes open instead of grepping in the dark.**

Real call graphs instead of vector-similarity guesses. Compiler-verified edges wherever a compiler is available. Hard safety gates on the write path itself, not just warnings an agent is free to scroll past. Every number in this README is measured — by CALM's own tools, against CALM's own codebase, with a reproducible benchmark suite behind it.

**New here?** [Quick start](#quick-start) gets you running in under a minute — no clone, no Rust toolchain, works with [Claude Code, VS Code, Cursor, Windsurf/Devin Desktop, Codex, Antigravity, and JetBrains](#quick-start). **Comparing tools in this category?** Jump straight to [Proof, not promises](#proof-not-promises). **Want the internals?** [`docs/architecture.md`](docs/architecture.md) covers multi-tier indexing, the SCIP/LSP overlay system, the concurrency model, and the sanitization layer in full.

| | |
|---|---|
| **Coverage** | 24 languages parsed · 13 with full call graphs by default (6 zero-config + 7 more via the default `tier0-5` bundle) · 12 with a formal/compiler-verified upgrade path |
| **Safety** | the only one of 5 live MCP servers benchmarked that *refused* an unconfirmed edit to a verified hub symbol |
| **Efficiency** | 29x–241x fewer tokens than a naive read-the-files baseline on multi-file tasks ([benchmark](benchmarks/b4_token_efficiency/)) |

---

## The problem

An AI agent that edits code without knowing who calls the function it's about to change will, sooner or later:

- Delete "dead code" that a dozen other files still call.
- Change a signature and miss half its call sites.
- Refactor a symbol it assumed was minor — and discover, after breaking the build, that it was the hub the whole module leaned on.

None of that is a reasoning failure. It's a *visibility* failure: the agent never had a map. Give it one, and the guessing stops.

## Why "CALM"

Most coding agents operate the way anyone would in an unfamiliar codebase with only `grep`: no sense of what's wired to what, no way to know if touching this function ripples into fourteen others. That's not confidence — it's fast guessing.

CALM stands for **Coding Agent Liveness Map**. *Liveness*, because the map is never a stale snapshot — it watches the filesystem, reindexes incrementally as files change, and reports in every response how fresh it currently is (`scanning → parsing → building_edges → ready`). *Map*, because it's an actual graph — call edges, import edges, hub/coreness metrics — not a flat text index pretending to be one. Hand an agent a live, trustworthy map of the terrain, and it stops flailing. It gets calm.

## What you get

- **The agent stops guessing who depends on what.** `callers`/`callees`/`edit_context` show every known caller before a change ships. Full tree-sitter call graphs cover **13 languages out of the box**: Python, TypeScript, JavaScript, Java, Rust, and Go with zero configuration, plus C, C++, C#, Ruby, PHP, Shell, and R via the default `tier0-5` grammar bundle. Eleven more (Kotlin, Swift, Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy) parse behind opt-in `--features lang-X` build flags — 24 languages parsed in total (see [multi-tier indexing](docs/architecture.md#multi-tier-indexing)).
- **Edits that can't silently break things.** Every write is hash-verified against the exact line range and syntax-checked before it ever touches disk. Hub and high-fan-in symbols hard-refuse a write until the agent has reviewed the callers and explicitly confirmed — a policy only a tool with a real dependency graph can enforce, and one no other server in [CALM's competitor benchmark](#benchmarked-against-four-other-live-mcp-servers) enforced.
- **Every edge tells you how much to trust it.** Call edges are confidence-graded (`textual → inferred → resolved → formal`), and when your compiler can double-check the graph, CALM asks it to: SCIP overlays (`rust-analyzer`, `scip-go` — including multi-module `go.work` workspaces — `scip-python`, `scip-ruby`, and more) and live LSP overlays (`gopls`, `clangd`) upgrade best-guess edges to compiler-verified ground truth across 12 languages, with zero behavior change on a machine that doesn't have the toolchain installed.
- **A codebase that grades itself.** `fitness_report` turns hub concentration, dead code, complexity, and architecture-boundary violations into a queryable, CI-enforceable signal instead of a one-off audit — and `remember`/`recall` keep decisions and gotchas available across sessions.
- **Plays well with others, and stays on your machine.** A cross-process edit lock and single-writer indexing model mean two editor sessions on the same repo don't corrupt each other's writes or double-index — under the shared daemon, sessions can even see each other coming. No code leaves your machine for indexing, search, or editing; the default embedding model is vendored into the binary at build time (zero network at runtime), with a rare, opt-out-able fallback download only if that vendored copy is ever unusable. MIT-licensed.

## Where CALM fits

"Code intelligence for AI agents" is a real category now, built up by open-source pioneers — Aider, Serena, Sourcegraph/Cody, and others — that proved an agent works better with real code structure under it than with grep and good intentions. CALM builds on that foundation with a different center of gravity: most tools in the category **inform the read path** — better search, better navigation, better context. CALM also **guards the write path**. The same graph that answers "who calls this?" enforces "you don't change it until you've looked": pre-edit context is mandatory, hub edits demand an explicit confirmation grounded in a real caller, and every write is hash- and syntax-verified before it lands.

The trade-off is stated plainly: CALM's full-call-graph tier out of the box is 13 languages, not the 40+ some pure-LSP tools reach — though with 24 languages parsed and 12 carrying a compiler-verified upgrade path, the gap is narrower than it looks. What the trade buys is the part most distinctly CALM's own: confidence-graded edges, hard pre-edit gates, and a codebase that grades its own health — each backed by a number you can reproduce yourself ([Proof, not promises](#proof-not-promises)).

### Is CALM the right fit?

**Good fit:** agents that edit code directly, not just answer questions about it · single-repo codebases in a Tier-0/Tier-0.5 language · projects running multiple MCP clients (see [supported clients](#quick-start) below) against the same repo · local-first users who don't want to depend on an embedding API.

**Not the fit today:** multi-repo/cross-repo enterprise search — tools purpose-built for that scale (Sourcegraph/Cody among them) will serve you better · a language nowhere in CALM's current 24-language tree-sitter set.

## Quick start

**Supported clients** — CALM works with any MCP client that speaks stdio; these are wired up or documented today:

| Client | Modes | Fastest install |
|---|---|---|
| **Claude Code** | CLI · Web · IDE | `claude mcp add --transport stdio calm -- npx -y @eilodon/calm-mcp serve` |
| **VS Code** | IDE (native MCP / Copilot Agent mode) | `code --add-mcp '{"name":"calm","command":"npx","args":["-y","@eilodon/calm-mcp","serve"]}'` |
| **Cursor** | IDE · Cloud (Background Agent) | [Add to Cursor →](cursor://anysphere.cursor-deeplink/mcp/install?name=calm&config=eyJjb21tYW5kIjoibnB4IiwiYXJncyI6WyIteSIsIkBlaWxvZG9uL2NhbG0tbWNwIiwic2VydmUiXX0=) |
| **Windsurf / Devin Desktop** | IDE · Cloud | edit `~/.codeium/windsurf/mcp_config.json` |
| **Codex** (OpenAI) | CLI · IDE | `codex mcp add calm -- npx -y @eilodon/calm-mcp serve` |
| **Antigravity** (Google) | CLI · IDE | edit `~/.gemini/config/mcp_config.json` |
| **JetBrains AI Assistant** | IDE | via UI settings |

Full walkthrough for every client above, including exact global-config snippets for the ones that need one — [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md).

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
  → 233 files, 3,488 symbols, indexing_phase=ready

agent: "I need to change getUserByEmail"
  → locate("getUserByEmail")        # find the file + symbol metadata
  → source("getUserByEmail")        # read just the function body, not the whole file
  → edit_context("getUserByEmail")  # MANDATORY before any edit
      → 12 callers, risk_assessment=high → agent reviews each caller before touching the signature
  → edit_symbol("getUserByEmail", expected_hash=..., new_text=...)
      → risk_assessment=high, is_hub=true, no confirm:true → refused, with an explanation
  → edit_symbol(..., confirm=true, reason="checked getUserByToken, still returns the same shape")
      # reason must cite a real caller edit_context returned — writes for real, reindexes immediately
  → diff_impact(staged=true)        # verifies blast radius before commit
```

## Proof, not promises

Every number below is measured by pointing CALM's own `fitness_report`/`repo_overview` at its own codebase — reproducible with the same two tool calls on a fresh clone:

| Metric | Measured value |
|---|---|
| Codebase indexed | **233 files, 3,488 symbols** — 15 languages present in this repo alone |
| Hub concentration (`hub_pct`) | 7.5% — 169 hub symbols (gate: ≤ 20%) |
| Dead-code rate (`dead_code_pct`, coverage-aware) | 5.0% (gate: ≤ 10%) |
| Edge coverage (`edge_coverage_pct`) | 74.9% of symbols have at least one call edge (gate: ≥ 60%) |
| High-complexity functions (`high_complexity_pct`) | 2.9% (gate: ≤ 15%) |
| Ambiguous symbol boundaries (`boundary_ambiguous_count`) | 0 (gate: ≤ 0) |
| Architecture boundary violations (`boundary_violations`) | 0 (gate: ≤ 0) — the `watcher → tools` import previously flagged here was fixed by relocating the shared `RwLockExt`/`LockExt` traits it needed out of `tools/common.rs` into their own `sync_ext` module |
| Token efficiency vs. a naive read-the-files baseline | `source` **241x** · `edit_context` **193x** · `locate` **29x** · `callers` **1.0x** — median 111x across the four benchmark tasks ([methodology](benchmarks/b4_token_efficiency/)) |
| Full test suite (default features) | see [Testing](#testing) below |

<details>
<summary><strong>Competitor-benchmark methodology and per-language caveats</strong></summary>

### Benchmarked against four other live MCP servers

`benchmarks/b11_extended_competitor_ab/` installs and calls four established open-source code-intelligence MCP servers — CodeGraph, Semble, grepai, and Serena — against an isolated git worktree of this repo, 5 repeats per task, with a correctness oracle for every task. The goal isn't a leaderboard; it's checking CALM's claims against real, running prior art instead of a marketing page.

What the runs showed: CALM matched the best result on caller-recall and blast-radius tasks, and was the only one of the five servers whose pre-edit safety gate actually *refused* a risky, unconfirmed edit rather than merely being able to describe the risk after the fact. Not every number flatters: on one token-efficiency task CALM's compression ratio was the lowest of the five — correctness stayed at the ceiling there too, and the number is published as measured. That is this project's standing benchmark policy: unflattering results ship alongside good ones ([benchmarks/README.md](benchmarks/README.md)). Full methodology, every task, and the raw per-tool numbers live in the benchmark's own README.

### Language coverage, measured not asserted

`benchmarks/resolution/` runs a tier-distribution baseline (resolved / inferred / textual / ambiguous split — no oracle, one real OSS repo per language) across the 19 newly-added or Tier-0.5 languages, reported as-is: Kotlin (89.6%) and OCaml (86.3%) land mostly in the `ambiguous` tier from common short method-name collisions; Dart produces symbols but zero call edges — a documented limitation of that tree-sitter grammar, not a bug; Tier-2 type inference is wired only for the original Tier-0 languages so far. Full per-language table in the benchmark's own README.

</details>

## How CALM works

Full technical detail lives in [`docs/architecture.md`](docs/architecture.md) — including the design philosophy behind why every response carries `suggested_next` and why the risky steps are hard-gated instead of just recommended. Section-by-section summary:

- **[Multi-tier indexing](docs/architecture.md#multi-tier-indexing)** — 13 languages with full call graphs by default, 11 more parsed behind opt-in grammar features, 24 in total.
- **[A call graph you can actually trust](docs/architecture.md#a-call-graph-you-can-actually-trust)** — every edge is labeled by confidence (`resolved`/`inferred`/`formal`/`textual`); SCIP and LSP overlays upgrade edges to compiler-grade ground truth across 12 languages.
- **[Search that actually finds things](docs/architecture.md#search-that-actually-finds-things)** — FTS5 + semantic embeddings fused via Reciprocal Rank Fusion, plus real grep/glob straight off disk for files the indexer never parses.
- **[Editing with an actual safety net](docs/architecture.md#editing-with-an-actual-safety-net)** — hash-verified writes, syntax validation before anything touches disk, and a three-part gate (fresh `edit_context`, `confirm:true`, a grounded `reason`) on hub/high-risk symbols.
- **[Concurrency & reliability](docs/architecture.md#concurrency--reliability)** — a shared daemon, cross-process edit lock, and single-instance indexing lock mean multiple editor sessions on one repo don't corrupt or duplicate work.
- **[The codebase grading itself](docs/architecture.md#the-codebase-grading-itself)** — 10 fitness metrics, coverage-aware dead-code detection, declared architecture boundaries, doc-drift detection.
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

Run for real in `.github/workflows/ci.yml`'s `fitness-check` job on every push/PR — `calm index` first (a fresh checkout has no `.calm/index.db` yet), then `calm fitness-check --project-root . --config thresholds.toml`. That `--config` flag is not optional: without it, `[[boundaries]]` and `[config_drift]` are silently treated as "no rules declared" rather than erroring — only the numeric thresholds have a real default.

`calm fitness-check` measures 10 metrics against thresholds declared in `thresholds.toml`:

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
| `boundary_ambiguous_count` | Count of symbols with an ambiguous line boundary (shared with a neighbor) — `edit_symbol` replace on these is refused until resolved | ≤ 0 |
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

- `cargo build --release` → static (musl on Linux) binaries via `.github/workflows/release.yml`, 5-target matrix with `SHA256SUMS` + build-provenance attestation for every asset: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`. `scripts/mcp-launcher.sh`/`scripts/install.sh` download and checksum-verify the right platform's build automatically when checkout is on (or you're installing) a matching git tag.
- `Containerfile`, multi-stage (`rust:alpine` → `scratch`) — a single static binary, no runtime image needed, published to `ghcr.io/eilodon/calm-mcp` (tagged by version + `latest`) on every git tag push.
- `compose.yaml` ships a hardened example (`read_only`, `cap_drop: ALL`, `no-new-privileges`, `pids_limit: 64`, `mem_limit: 256m`).
- The default embedding model's weights are vendored into the binary via `include_bytes!` — `build.rs::ensure_embedding_weights` fetches `crates/calm-core/assets/potion-code-16m/*.safetensors` from Hugging Face Hub and checksum-verifies it once at *compile* time, so a normal `cargo build`/release binary loads it with zero network I/O at runtime. No Git LFS is involved (the repo carries zero LFS content).

<details>
<summary>What happens if the build-time fetch fails (offline build, etc.)</summary>

`cargo build` still **compiles successfully** — `build.rs` writes a small placeholder stub in place of the real weights instead of failing the build. Loading that stub **at runtime** fails ("failed to parse safetensors"), so `Embedder::load` automatically falls back to a one-time Hugging Face Hub download of the same model (cached locally afterward, gated by `semantic_search.allow_network_fallback`). If that fallback is disabled or also unavailable, `indexing_status` reports `embeddings_status: "failed"` and `search(kind="semantic"/"hybrid")` degrades to FTS-only — no crash, just no semantic search until the model is available and you rebuild or re-run.

</details>

## Testing

```bash
cargo test --workspace                        # unit + integration (embeddings is a default feature, included)
cargo test --test parity_test test_formal_edges   # Stack Graphs regression corpus
```

Five CI jobs run on every PR: `verify` (fmt/clippy/test/audit), `stack-graphs-corpus` (formal-resolver parity), `embeddings` (clippy + test with the `embeddings` feature), `all-languages` (fixture-repo indexing across all 24 parsed languages), `js-client-interop` (cross-checks the tool schema against a real JS MCP SDK client, not just Rust's own).

The full workspace suite — 1,000+ tests — passes clean, with a handful of `#[ignore]`d live-binary integration tests (e.g. `rust-analyzer`/`scip-go`/`scip-java`) that need external tools not installed in every environment.

## Further reading

- [`docs/architecture.md`](docs/architecture.md) — the full technical deep-dive: multi-tier indexing, SCIP/LSP overlays, search internals, the edit safety net, concurrency, self-grading, memory, sanitization, and the design philosophy behind it all.
- [`docs/comparison.md`](docs/comparison.md) — methodology-first positioning write-up against other tools in this category.
- [`docs/what-external-users-get.md`](docs/what-external-users-get.md) — exactly what an `npx`/npm/MCP-Registry install gives you, as distinct from this repo's own dev checkout: install/distribution mechanics, the full tool and toolset breakdown, the edit safety layer, language coverage, and what never ships externally.
- [`docs/`](docs/) — resolver internals, migration plans, and other design notes not covered by `docs/architecture.md` above.
- [`docs/adr/`](docs/adr/) — individual architecture decision records (Stack Graphs scope, the formal-resolver approach, the LSP-optional confidence upgrade, the daemon+forwarder concurrency model).
- [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) — every MCP client install path in detail, including Windsurf/Devin Desktop and Codex global config.
- [`AGENTS.md`](AGENTS.md) — the full tool-by-tool workflow guide this project's own agents follow.
- [`benchmarks/`](benchmarks/) — the measurement suite behind every number in this README: `b2_call_graph_quality/` (precision/recall vs. a SCIP oracle), `b4_token_efficiency/` (token cost vs. a naive baseline, per task), `b11_extended_competitor_ab/` (real calls against 4 other live MCP servers, not self-reported numbers), `resolution/` (tier-distribution baseline across 19 real OSS repos, one per language). Unflattering results are published alongside good ones on purpose — `benchmarks/README.md` states that policy.

## License

[MIT](LICENSE)
