# What external users get from CALM

This page describes exactly what you get from **installing CALM via `npx @eilodon/calm-mcp`, npm, the MCP Registry, or a GitHub Release** — as distinct from what only exists in a developer checkout of this repo (internal dev scripts, this repo's own dogfooding hooks, its pattern-debt data, and so on).

---

## 1. Install & distribution

| What | Details |
|---|---|
| How to install | `npx -y @eilodon/calm-mcp serve`, or `calm setup --npx`, which writes that entry into `.mcp.json`/`.cursor/mcp.json`/`.vscode/mcp.json` automatically |
| npm mechanism | `npm/calm-mcp/package.json` is a thin wrapper — `optionalDependencies` points at 5 per-platform packages (`linux-x64`, `linux-arm64`, `darwin-arm64`, `darwin-x64`, `win32-x64`), and `bin/calm-mcp.js` spawns the matching real binary and forwards SIGINT/SIGTERM/SIGHUP. Nothing is compiled from source on install, the same mechanism tools like esbuild/swc use. |
| Binary release matrix | `.github/workflows/release.yml` cross-compiles 5 targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-pc-windows-msvc`. Linux glibc (non-musl) isn't in the matrix. |
| Binary verification | Every release ships `SHA256SUMS` plus a GitHub build-provenance attestation (Sigstore/Fulcio-backed), checkable with `gh attestation verify calm-<target>.tar.gz -R Eilodon/CALM`. The container image on GHCR is signed separately via keyless `cosign` — two distinct mechanisms, kept separate on purpose (see the comment in `release.yml` for the reasoning). |
| MCP Registry | Published as `io.github.Eilodon/calm-mcp` via `mcp-publisher` and GitHub OIDC, so MCP-aware clients (VS Code, Cursor, Claude Code registry search) can discover CALM by name. |
| `calm init` with no flags | Only creates `.calm/config.json` — doesn't touch AGENTS.md, doesn't touch hooks. |
| First-run bootstrap | Automatically adds `.calm/` to `.gitignore`, spawns a background indexing thread, and creates the index DB at `.calm/index.db`. |
| Embedding model | `minishlab/potion-code-16M` (distilled from CodeRankEmbed), embedded into the binary via `include_bytes!`. Not distributed via Git LFS — `build.rs` fetches and SHA256-verifies it from Hugging Face once at compile time (on the machine building the release), then embeds it directly. A user's runtime is 100% offline for this; `allow_network_fallback` (default `true`) permits a one-time re-download only if the embedded copy turns out to be unusable. |
| Shared daemon | On by default on Linux/macOS: when the launcher is invoked with no extra arguments (how npm and the Claude Code plugin both call it), it automatically uses `calm connect`, so multiple clients on the same project share one indexing/watching process instead of running their own. Explicit opt-out via `CI_MCP_LAUNCHER_NO_DAEMON=1`. |

---

## 2. 29 MCP tools across an 8-stage workflow

**Navigation**: `search`, `locate`, `file_overview`, `symbol_info`, `source`, `understand`, `symbols_batch`
**Repo health**: `repo_overview`, `hotspots`, `fitness_report`
**Code edits**: `edit_lines`, `edit_symbol`, `format_files`
**Safety gates**: `edit_context`, `diff_impact`
**Graph**: `callers`, `callees`, `dependencies`, `path`
**Security/test**: `scan_text`, `test_gap_hotspots`
**Pattern debt**: `pattern_debt_register`, `pattern_debt_status`
**Memory**: `remember`, `recall`
**Recovery**: `indexing_status`, `session_context`
**Overlay refresh**: `scip_refresh`, `lsp_refresh`

### Toolsets and presets — two layers, not one flat list

- **13 fine-grained toolsets** (module-domain): `trace`, `locate`, `orient`, `memory`, `guardrails`, `recover`, `scip`, `lsp`, `security`, `testgap`, `inspect`, `edit`, `patterndebt`
- **5 cross-cutting presets**: `full` (default, all 29 tools), `orient`, `trace`, `edit`, `compound`
- Composable syntax: `--preset "trace,security"` unions toolsets, `--preset "full,-edit"` subtracts one — an unrecognized token is a hard error, never a silent grant of full access

---

## 3. The safety layer around letting an agent edit code — CALM's core differentiator

- **Blocks stale overwrites**: `edit_lines`/`edit_symbol` require an `expected_hash` from a prior read; a hash mismatch is rejected.
- **Real cross-process locking**: a `flock` on `.calm/edit.lock` (the `fs4` crate) stops two different CALM processes (e.g. Cursor and Claude Code open on the same repo) from both passing the hash check and silently overwriting each other.
- **Blocks symlink/path traversal**: `resolve_repo_path` canonicalizes the target and checks it against the project root, exercised by real filesystem tests including a real symlink.
- **Two independent sanitization systems**, both in `sanitize.rs`:
  1. **Credential redaction** — PEM keys, `sk-`/`rk-` tokens, GitHub PATs, AWS keys, JWTs, Slack tokens, `Authorization: Bearer` headers, URL-embedded credentials, env-style assignments.
  2. **Prompt-injection detection** (flags via `content_warning`, never silently modifies content) — fake ChatML (`<|im_start|>`), `[INST]`/`[SYS]` markers, fake role markers like `system:`, fake `</tool_result>` tags, "ignore previous instructions" phrasing, jailbreak/persona-override attempts, exfiltration phrasing, zero-width Unicode, and several non-English variants.
- **`scan_text`** runs the same injection heuristics against content that never goes through the index — a WebFetch/WebSearch result, a subagent's report — closing a blind spot `source`'s `content_warning` doesn't cover.
- **SIGTERM watchdog**: a kernel-level `libc::alarm()` (10 seconds), not an async timer — an async timer can't fire reliably here because the MCP transport's stdio-reading thread blocks the async runtime.
- **Restrictive file permissions (0600)** on the daemon socket, daemon log, audit log, and memory key file.

---

## 4. Language coverage — 24 languages parsed

| Tier | Languages | In the release binary? |
|---|---|---|
| Tier-0 (always on) | Python, Rust, Go, JavaScript, TypeScript, Java | Yes |
| Tier-0.5, on by default | C, C++, Ruby, PHP, C#, Shell, R | Yes |
| Tier-0.5, opt-in (its own feature flag each) | Kotlin, Swift, Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy | No — the grammar exists in the source but isn't compiled into the release binary; an npx/npm install only gets a regex/line-scan fallback (symbols, no call graph) for these |

**13 languages get a full call graph by default** (6 Tier-0 + 7 Tier-0.5); **11 more parse but need a rebuild with `--features lang-X`**; 24 total. Markdown and SQL are handled separately and aren't part of this count:

- **Markdown** gets its own ATX-heading line-scan, not real symbol extraction.
- **SQL** gets its own `sqlparser`-based parser (a real grammar, not tree-sitter) — tables/views/procedures are extracted, but there's no call graph.
- **`.txt`** files aren't recognized at all.
- **Solidity, Circom, Move, Cairo, Vyper, TOML** are "recognized but unparsed" — tracked in the DB by path/hash but with zero symbols extracted.

### SCIP overlay (formal, compiler-grade edges)
9 providers covering 12 languages: `rust-analyzer` (Rust), `scip-go` (Go, including multi-module `go.work` workspaces), `scip-python` (Python), `scip-typescript` (JS+TS), `scip-java` (Java + Kotlin in the same pass), `scip-dotnet` (C#), `scip-php` (PHP), `scip-ruby`/Sorbet (Ruby), `scip-clang` (C+C++). Each provider auto-detects its own toolchain binary and silently sits out if it isn't installed — zero behavior change on a machine without it.

### LSP overlay (additive, not a replacement for SCIP)
Three providers: `rust-analyzer` (a live-session path distinct from its batch SCIP export), `gopls` (Go), `clangd` (C/C++).

### Measured, not estimated
`benchmarks/resolution/` tracks real per-language resolution quality: Kotlin lands at 89.6% in the `ambiguous` tier, OCaml similarly at 86.3%; Dart produces symbols but zero call edges (a tree-sitter grammar limitation, not a bug); `inferred%` is 0.0% across all 11 Phase B/C languages, since Tier-2 type inference is only wired for Tier-0 languages so far.

---

## 5. Workflow guidance built in

- **The `calm_workflow` MCP Prompt** is available in every binary and callable anytime with no flags — it returns the condensed 8-stage workflow.
- **`get_info().with_instructions()`** is pushed automatically during the MCP `initialize` handshake, so every MCP client sees a "call `calm_workflow` first" pointer with zero setup on the user's part.
- **`calm init --agents-md`** (opt-in, off by default) writes a condensed version (~700 characters, 8 lines) into the target project's AGENTS.md, wrapped in `<!-- calm:workflow:start/end -->` markers and safe to re-run — not the full AGENTS.md this repo uses for its own development.

---

## 6. `calm init --hooks` — hard-gate hooks for any project

`calm init --hooks[=nudge|enforce|off]` scaffolds a generic Claude Code hook (`.claude/hooks/calm-hooks.sh` — a separate, leaner mechanism from this repo's own internal `calm-nudge.sh`) into any other project, with:

- **`nudge` by default** — only reminds, never blocks. `--hooks=enforce` is required to upgrade to an actual block (`exit 2`).
- **Honest best-effort framing** — the install output states the concrete ways the hook can be bypassed (overwriting `.calm/hooks.mode`, deleting the script, editing `settings.json`) rather than overclaiming it's unbypassable.
- **`calm doctor` reports real status** by cross-checking the mode file, the `settings.json` wiring, and whether the script itself still exists — not trusting any single source.
- **Downgrading the mode is never silent** — it leaves a trace in `.calm/audit.log` plus a one-time notice.

Shipped in `v0.3.0`; see `docs/superskills/specs/2026-07-15-calm-hooks-transparent-reactivation.md` for the full design.

---

## 7. What never ships to other users

- **`.claude/hooks/calm-nudge.sh`** — the internal enforcement mechanism this repo uses to develop CALM itself. It never ships, and no flag exposes it externally.
- **`docs/pattern-debt-registry.yaml`** — this repo's own debt-tracking data. The `pattern_debt_register`/`pattern_debt_status` tools work generically on any repo, but this specific file doesn't ship with them.
- **`.claude/skills/`** — the CALM dev team's own development methodology (VHEATM, adr-commit, tdd-verified, and similar internal skills). Not part of the CALM product, and unrelated to installing the CALM MCP server.

---

## Related reading

- [`../README.md`](../README.md) — quick start, the full tool list, and the CLI reference.
- [`architecture.md`](architecture.md) — the technical deep-dive behind everything summarized here.
- [`comparison.md`](comparison.md) — how CALM's design compares to other code-intelligence MCP servers.
