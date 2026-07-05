# Rename checklist — `ci` / Code-Intelligence → CALM

Status (2026-07-06): **Tier 1 and Tier 3 are done.** Tier 2 (published npm packages, GHCR image)
is the only remaining work, and it needs a deliberate release, not just file edits.

## Tier 1 — internal, reversible, no external impact — DONE

Crate names (`calm-core`/`calm-server`/`calm-cli`), directory names, binary name (`calm`), the
`.mcp.json`/`.cursor/mcp.json`/`.vscode/mcp.json` server key (`"calm"`, tool prefix
`mcp__calm__*`), and the runtime index dir (`.codeindex/` → `.calm/`) were all renamed in a prior
session. An audit on 2026-07-06 found and fixed the rough edges left behind:

- `.calm-bin/x86_64-unknown-linux-musl/ci` → renamed to `.../calm` (dir had been renamed but not
  the LFS-tracked binary inside it; `mcp-launcher.sh`/`.gitattributes` both expected `calm`).
- `crates/calm-cli/src/main.rs` — clap `name = "ci"` → `"calm"`; the `calm setup` subcommand's
  `write_mcp_config`/`manual_mcp_config_snippet` were still hardcoding `"ci"` as the JSON server
  key it writes into a *target* project's `.mcp.json` — a real bug, since it would've written a
  key that doesn't match this server's actual name.
- `crates/calm-server/src/tools.rs` — `ServerInfo.instructions` string and one `mcp__ci__*` prompt
  example in a doc comment.
- `benchmarks/b2_call_graph_quality/run_benchmark.py` — `--ci-bin` flag vs. `args.calm_bin` access
  was already broken (`AttributeError`) from a partial rename; fixed flag name + stale
  `target/release/ci` default path.
- Doc-comment/test-literal `ci-core`/`ci-server`/`ci-cli` references across live `crates/*.rs`,
  `thresholds.toml`, `.github/workflows/ci.yml`, `docs/pattern-debt-registry.yaml`, and the active
  benchmark scripts (`benchmarks/**`) — cosmetic but now consistent.
- `docs/cloud-environment-setup.md` — this one had a *runnable* Setup Script snippet with
  `cargo build --quiet -p ci-cli` (would silently fail post-rename since `|| true` swallows the
  error) and several other stale paths — fully re-swept.
- Hook files renamed `ci-nudge.sh`→`calm-nudge.sh`, `session-start-build-ci.sh`→
  `session-start-build-calm.sh`, with every reference in `AGENTS.md`/`README.md`/
  `.claude/settings.json`/`docs/*.md` updated to match.
- Cleanup: orphaned pre-rename copies (`crates/ci-core/assets/.../model.safetensors`, 62M;
  `.ci-bin/x86_64-unknown-linux-musl/ci`, 98M) and the stale `.codeindex/` runtime dir (superseded
  by `.calm/`, no longer gitignored so it cluttered `git status`) were deleted.

**Deliberately left alone:** dated historical records — `docs/adr/*.md`, `docs/superskills/**`,
`docs/migration-plan-v2.md`, `docs/migration-plan-v3.md`, `docs/architecture-design.md`,
`docs/superskills/specs/CONTRACTS.md` — and benchmark-report prose describing past runs (numbers
measured under the old name). Rewriting the brand name in a point-in-time decision record or a
dated benchmark write-up is revisionist, not a rename fix; treat these the same as git history.

## Tier 2 — published artifacts, need a coordinated release — NOT DONE

Each of these has already been distributed to whoever installed this tool before today. A rename
here means "the old thing keeps existing whether you touch it or not" — the only choice is whether
to add a deprecation pointer or just let it go stale silently.

- **npm packages** — local `npm/calm-mcp*` dirs and `package.json` `name` fields already say
  `@eilodon/calm-mcp*` (done in the same prior session as Tier 1). Confirmed via the npm registry
  on 2026-07-06 that **`@eilodon/calm-mcp` is not published yet** — only the old `@eilodon/ci-mcp*`
  (still live, not deprecated). Actually publishing the new scope requires a real release (built
  platform binaries via `npm/stage-release.sh` from a tagged GitHub Release), not just files in
  this checkout — needs the user to drive a version bump + tag + publish, then decide whether to
  `npm deprecate` the old `@eilodon/ci-mcp*` packages pointing at the new name.
- **Docker/GHCR image** — `Containerfile`/`compose.yaml`/`.github/workflows/release.yml` already
  reference `ghcr.io/eilodon/calm-mcp` (done). Not verified whether anything has actually been
  pushed there yet — that only happens on the next tagged release run.
- **Release binaries** — `.github/workflows/release.yml` artifact naming (`calm-${target}.tar.gz`)
  and `scripts/install.sh`'s expected asset/download name are already consistent with each other;
  just needs an actual tagged release to produce them under the new name.

## Tier 3 — outside this repo's control, needs the user directly — DONE

- **GitHub repository rename** — user renamed `Eilodon/Code-Intelligence` → `Eilodon/CALM` (and
  the local checkout directory) directly, 2026-07-06. Verified: `github.com/Eilodon/CALM` → 200,
  old URL → 301 redirect. Local `git remote origin` updated to point at the new URL directly rather
  than relying on the redirect indefinitely.
