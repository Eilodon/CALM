# Contributing to CALM

CALM (Coding Agent Liveness Map) is early and solo-maintained right now — which means a single contribution can actually matter. This file is deliberately biased toward *non-code* ways to help first, because a healthy project needs more than pull requests.

## You don't have to write code to contribute

- **Proofread the just-translated docs.** `docs/comparison.md` and `docs/mcp-client-setup.md` were Vietnamese-only for a while and have now been translated to English (matching the main README and `docs/architecture.md`) — a native-English pass to catch anything a machine-assisted translation got stiff or unclear is still valuable.
- **Try CALM on a codebase in a Tier-0.5 language** — 7 are on by default (C, C++, C#, Ruby, PHP, Shell, R), 11 more are opt-in behind one Cargo feature flag each (Kotlin, Swift, Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy) — and report where the regex/line-scan symbol extraction (or, once a language has a SCIP provider wired, the cross-reference resolution) gives a wrong or missing answer. `benchmarks/resolution/` tracks known gaps (e.g. Dart parses symbols but produces zero call edges — a tree-sitter grammar limitation, not a bug) — new reports directly inform what gets prioritized next.
- **Write up your own workflow.** A blog post or short thread showing CALM catching something before it caused a breakage is worth more than most code contributions, and we'll link to it from the README.
- **Add yourself to "Who's using CALM."** Even a one-line entry ("we use it to review agent-generated PRs before merge") helps the next person trust the project. (`ADOPTERS.md` doesn't exist yet — open a PR creating it if you're the first!)
- **Answer questions in Issues/Discussions.** Solo maintainer plus a growing user base is exactly the gap where a knowledgeable early user helps the most.

## Ways to help with code

Roadmap items currently open, roughly in priority order:

1. **Hook adapters for other MCP hosts.** `calm init --hooks[=nudge|enforce]` now scaffolds the PreToolUse/PostToolUse deny-hook wiring generically (backed by `calm-core::hooks`/`hooks_check`), but it only targets Claude Code's hook schema so far. VS Code/GitHub Copilot uses the *same* `PreToolUse` / `permissionDecision: deny` shape, so that port should be close to a direct translation. Cursor (`beforeMCPExecution`) and Windsurf (Cascade Hooks) use different shapes and need their own adapters.
2. **Resolver accuracy.** False positives/negatives in `coreness`/`is_hub` classification, edge cases in the SCIP-overlay caching key. Also useful: getting more of the optional SCIP cross-reference providers (Go, Java, C#, PHP, C, Ruby — see `install_hint` in `repo_overview`'s `health_summary`) easier to install, since call-graph precision for those languages is capped without one installed.
3. **New Tier-0.5 → Tier-0 promotions.** SQL and Dart both already ship (SQL has its own standalone `sqlparser`-based indexer — real grammar, not regex — but deliberately stops short of a call graph, since "calls" isn't a coherent concept across SQL dialects; Dart is Tier-0.5 with the known zero-call-edge grammar limit noted above) — the next candidates are whichever Tier-0.5 language your own report from the "try CALM on a Tier-0.5 language" bullet above turns up as highest-friction.

Please open a GitHub Issue before starting on anything larger than a small fix, so effort doesn't collide with what's already in progress.

## Development setup

```bash
git clone https://github.com/Eilodon/CALM.git
cd CALM
cargo build --release   # build.rs fetches + checksum-verifies the embedding model from Hugging Face Hub once, then vendors it into the binary — no Git LFS involved
cargo test --workspace  # embeddings is on by default, no extra --features flag needed
```

## Before opening a PR

- `cargo fmt` and `cargo clippy` clean
- `cargo test --workspace` passes
- `calm fitness-check --project-root .` passes against this repo's own thresholds — CALM reviewing itself is part of the point
- Commit messages explain *why*, not just *what* — the git-co-change mining feature literally mines this history later

## Code of conduct

Be the kind of contributor you'd want reviewing your own PR. (A formal `CODE_OF_CONDUCT.md` is on the list — flag it if you'd like to help draft one.)

## License

By contributing, you agree your contribution is licensed under the project's [MIT License](LICENSE).
