# Cloud environment setup — why the "calm" MCP server needs a Setup Script

This repo dogfoods its own MCP server: `.mcp.json` wires up a `calm` stdio
server pointed at this workspace's own `calm-cli` binary. On Claude Code on
the web, that binary does not exist until something compiles it — and
where that compile happens determines whether the server ever connects.

## The failure this document exists to prevent

`calm-cli` is a Rust binary with a nontrivial dependency tree (tree-sitter
grammars, stack-graphs, bundled SQLite). A cold `cargo build -p calm-cli` on
a fresh checkout measures **~59s** in this environment. Claude Code's MCP
client dials configured servers **concurrently with, not after,**
`SessionStart` hooks — confirmed against the official docs:

- [code.claude.com/docs/en/mcp](https://code.claude.com/docs/en/mcp) —
  MCP server startup is "non-blocking by default"; a failed initial
  connection gets at most 3 retries on transient errors (~7s of total
  backoff, as of v2.1.121), then the server is marked failed **for the
  rest of the session**, with no further retry.
- [code.claude.com/docs/en/claude-code-on-the-web](https://code.claude.com/docs/en/claude-code-on-the-web) —
  `SessionStart` hooks "run after Claude Code launches, on every session,"
  which is a different (and unordered, relative to MCP dialing) point in
  startup than a Setup Script, which runs "before Claude Code launches."

A `SessionStart` hook that runs `cargo build` (this repo has one —
`.claude/hooks/session-start-build-calm.sh`) can therefore **win** the race
against a cold connection attempt, but cannot **guarantee** winning it — a
59s build has no trouble outlasting a ~7s retry budget. A previous version
of this repo's setup relied on that hook alone and believed it had fixed
the problem; it hadn't — it had just usually won the race, until a session
where it didn't.

## The primary fix: a binary already fetchable the instant a checkout happens

**UPDATE (2026-07-12):** this section used to describe `.calm-bin/`, a
binary committed straight into the repo via Git LFS. That approach
exhausted the GitHub account's Git LFS bandwidth budget (confirmed via
GitHub Actions job logs: `git lfs fetch` failing repo-wide with "This
repository exceeded its LFS budget", breaking CI, this binary's own
prebuild workflow, and the nightly SCIP overlay simultaneously on the same
day), and indirectly caused a prior AI session to mistake an unresolved LFS
pointer stub for a stale artifact and delete the safety net from `main`
entirely. See
[`docs/superskills/specs/2026-07-12-edge-release-binary-distribution.md`](superskills/specs/2026-07-12-edge-release-binary-distribution.md)
for the full incident writeup and design audit.

The binary now lives on a rolling `edge` GitHub Release instead — published
by [`.github/workflows/prebuild-mcp-binary.yml`](../.github/workflows/prebuild-mcp-binary.yml)
on every push to `main` that touches `crates/**`, `Cargo.toml`, or
`Cargo.lock`. `scripts/mcp-launcher.sh`'s tier 1.5 fetches it, verifies it
against the current source tree by exact commit match (`git diff --quiet`
against the published `EDGE_SHA` for the build-relevant paths — not an
approximate version string), and execs it — all with **no manual
configuration in any environment**, unlike everything below this point,
which requires a per-environment UI step. GitHub Release assets have no
bandwidth/storage quota (confirmed against docs.github.com: "no limit on
the total size of a release, nor bandwidth usage"), so this is a permanent
removal of the original failure mode, not a bigger version of the same
constraint. Cached locally after first verification, so steady-state
sessions in an already-used environment need no network call at all.

**This closes the race for every MCP client (Claude Code local or on the
web, Cursor, VS Code, anything else pointed at `mcp-launcher.sh`) without
anyone needing to know a Setup Script UI exists** — directly unlike a
Setup Script, which is cloud-environment-specific, manually configured,
and undiscoverable unless someone already knows to look for it. This is
why the Setup Script below is now optional defense-in-depth rather than
the primary fix.

**Two residual gaps, both fail safe (fall through to compiling, not to a
broken binary):**

- A commit newer than the last published `edge` release (build still in
  flight, or the triggering push didn't touch `crates/**`) has no matching
  edge asset yet — falls through to tiers below, same as before.
- It only covers `x86_64-unknown-linux-musl` today. A different sandbox
  architecture falls through to the tiers below, unaffected either way.

## Optional extra defense-in-depth: a Cloud environment Setup Script

With the `edge` release tier above, this is **no longer required** for the
common case — kept here for environments that want a zero-network-dependency
guarantee (the Setup Script runs before Claude Code launches at all, so it
never has to race anything, network included) or as a second layer in case
the `edge` tier's assumptions don't hold for a given sandbox (e.g. outbound
HTTPS to `objects.githubusercontent.com` specifically — the actual redirect
target for release asset downloads, not `github.com` itself — is blocked by
a restrictive network policy).

Setup Scripts and `SessionStart` hooks look similar but solve different
problems:

| | Setup Script | `SessionStart` hook |
|---|---|---|
| Runs | Once, **before** Claude Code (and MCP dialing) launches at all | Every session, **after** Claude Code launches, concurrently with MCP dialing |
| Configured in | Cloud environment settings UI (not in this repo) | `.claude/settings.json` (this repo, `.claude/hooks/session-start-build-calm.sh`) |
| Output persistence | Filesystem snapshotted and reused for ~7 days, or until the script/network config changes | None — runs fresh every session |

Only the Setup Script runs early enough to structurally rule out the race.
Because it's environment-level config, it is **not stored in this repo** —
that's exactly why it needs to be written down here, or the next person to
hit this failure has no way to discover it.

### What to paste in

Open the environment settings dialog (cloud icon → environment selector →
settings icon) for the environment used to run sessions against this repo,
and put this in the **Setup script** field:

**Do not put backticks (`` ` ``) anywhere in the script below**, comments
included. Confirmed incident: Claude Code's cloud Setup Script field runs
this text through something that evaluates backtick command substitution
even inside a `#` comment, silently corrupting the script — exit 127,
with `command not found` errors for words that only ever appeared between
backticks in a comment. Plain quotes (`'`/`"`) are fine; backticks are not.

```bash
#!/bin/bash
# Ensure cargo is in PATH — setup scripts run as non-login root shells,
# so ~/.cargo/env is not sourced automatically.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# No Git LFS resolution step needed (2026-07-12 update): this repo has
# zero LFS-tracked content any more. The vendored embedding model's
# weights are fetched from HuggingFace Hub and checksum-verified by
# crates/calm-core/build.rs::ensure_embedding_weights as part of the
# cargo build below — same || true-style non-fatal degrade (a fetch
# failure there writes a placeholder instead of failing the build;
# Embedder::load's own runtime fallback and
# embeddings_status: "offline_unavailable" messaging are still the
# ultimate safety net if that doesn't fully resolve it).

# Build the calm-cli binary. The || true is CRITICAL: setup scripts that
# exit non-zero prevent the session from starting entirely (confirmed in
# the official docs). A failed build here is non-fatal — the MCP server
# simply won't connect, which is recoverable; a dead session is not.
cargo build --quiet -p calm-cli 2>&1 || true

# Install rust-analyzer if missing. Unrelated to the MCP connection race
# above -- this is for CALM's own Rust SCIP overlay (audit-design
# 2026-07-20): RustConfig::default() already auto-detects rust-analyzer
# and auto-enables on every reindex that touches a .rs file (policy:
# OnSave), so the only thing an environment without it is missing is the
# binary itself. Same || true safety rule as the build above: this must
# never be the thing that kills session startup. Idempotent (a no-op if
# already installed), so safe to run unconditionally every time.
rustup component add rust-analyzer 2>&1 || true
```

**Why `|| true`:** Claude Code's cloud docs state: *"If the script exits
non-zero, the session fails to start."* Without it, any build failure
(transient network error downloading crates, a compile error on a dev
branch, `cargo` not in `PATH`) kills the session outright — you get a
generic "session failed to start" error with no way to debug. With
`|| true`, a failed build degrades gracefully: the session starts, the
MCP server just won't connect (same as before the Setup Script existed),
and you can inspect the failure interactively.

**Why `source ~/.cargo/env`:** Setup scripts run as **non-login, non-
interactive** root shells. `cargo` is pre-installed at
`/root/.cargo/bin/cargo`, but that path comes from `~/.cargo/env` which
is only sourced by login shells. Without the explicit source, `cargo:
command not found` → exit 127 → session dead (without `|| true`).

This must build to the **same path** `.mcp.json` expects:
`target/debug/calm` (debug, not `--release` — release compiles slower for no
benefit here, since this binary is a local dev/dogfood tool, not a
distributed artifact; keep this in sync with
`.claude/hooks/session-start-build-calm.sh` and
`scripts/mcp-launcher.sh` if that ever changes).

Keep it under Claude Code's ~5-minute Setup Script budget — 59s measured
leaves a wide margin.

### Optional extra margin: `MCP_TIMEOUT`

`MCP_TIMEOUT` (milliseconds) controls the MCP client's own initial
connection timeout. It is a process-level environment variable for the
`claude` process itself (e.g. `MCP_TIMEOUT=10000 claude` locally) — **not**
a per-server `.mcp.json` field, and **not** the same as `.mcp.json`'s
per-server `timeout` field (that one bounds individual tool *calls* after
connection, not the initial handshake). For cloud sessions, the
equivalent lever is adding `MCP_TIMEOUT=120000` as an **environment
variable** in the same environment settings dialog as the Setup Script.

**Format:** In the environment variables field, add one line (no quotes):
```
MCP_TIMEOUT=120000
```

This is optional defense-in-depth (matters mainly for the very first
session before any cache exists, or right after the ~7-day cache expiry) —
the `edge` release tier above is what fixes the steady state without any
manual configuration at all; this and the Setup Script are both extra
margin on top of it.

## What's already handled in this repo (defense in depth, not a substitute)

- The `edge` GitHub Release tier in `scripts/mcp-launcher.sh` — see the
  section above. The layer that eliminates the race outright rather than
  just narrowing it, when it applies, with zero manual configuration
  required in any environment.
- `scripts/mcp-launcher.sh` — `.mcp.json`'s actual entrypoint now (shared
  across every MCP client, not just Claude Code; see
  `docs/mcp-client-setup.md`). Execs an already-cached binary directly if
  present (no `cargo` involved, no risk of an unexpected rebuild reopening
  this exact race);
  only builds inline if the binary is missing outright.
- `.claude/hooks/session-start-build-calm.sh` — still runs every session.
  Redundant with the Setup Script in the common case (no-op if `target/`
  is already warm), but it's what keeps the binary from going *stale*
  (e.g. after editing `calm`'s own source), and it's the only mechanism at
  all for local/non-cloud Claude Code, which has no Setup Script concept.
  Same layering for rust-analyzer (2026-07-20): backgrounded, `|| true`,
  only fires when `rustup which rust-analyzer` already fails (a no-op
  once the Setup Script above or a prior session installed it).
- `crates/calm-core/build.rs::ensure_embedding_weights` — fetches and
  checksum-verifies the vendored embedding model from HuggingFace Hub at
  compile time (2026-07-12; used to be a Git LFS asset resolved by a
  best-effort `git lfs pull` step here and in the Setup Script snippet
  above — see
  [`docs/superskills/specs/2026-07-12-edge-release-binary-distribution.md`](superskills/specs/2026-07-12-edge-release-binary-distribution.md)).
  Never fails the build: on any fetch failure it writes a placeholder
  shaped exactly like an unresolved LFS pointer stub, so the runtime
  safety net below still applies unchanged.
- `Embedder::load`'s network fallback + `embeddings_status:
  "offline_unavailable"` (`crates/calm-core/src/embedding.rs`,
  `crates/calm-server/src/lib.rs::bootstrap_embeddings`) — the last line of
  defense if `build.rs` above still leaves the vendored embedding model
  asset as a placeholder (offline environment, no `curl`, etc.):
  `Embedder::load` detects the stub and falls back to a one-time
  HuggingFace Hub download of the same default model instead of failing
  permanently, unless `semantic_search.allow_network_fallback` is
  explicitly set to `false` — in which case `embeddings_status` reports
  `"offline_unavailable"` (a known policy outcome) instead of the more
  generic `"failed"`. `indexing_status(retry_embeddings: true)` re-checks
  this, so fixing the asset or flipping the config recovers without a
  restart. This only covers the *embedding model*, not the `calm` binary
  itself — that's what the two layers above are for.

None of this replaces the Setup Script for cloud sessions — it narrows the
window and covers the cases the Setup Script can't (local dev, staleness),
but only the Setup Script removes the compile step from the connection
race entirely.

## How to verify it worked

At the start of a session, once indexing has had a moment to run:

```
mcp__calm__repo_overview()
```

If this resolves (rather than the tool being entirely absent from your
tool list), the connection succeeded. If it's missing, check for a `calm-cli
pre-build failed` message in the session's `SessionStart` hook output —
that means the fallback hook itself failed (e.g. a real compile error),
which is a different problem than the timing race this document covers.
