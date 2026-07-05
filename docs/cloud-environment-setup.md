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

## A stronger layer: a binary already sitting in the checkout

Everything below this point (Setup Script, `MCP_TIMEOUT`) works by trying
to *win* a race against the MCP client's dial attempt. There's a way to
avoid the race entirely: `.calm-bin/x86_64-unknown-linux-musl/calm`, a
prebuilt binary committed to the repo via Git LFS and kept current by
[`.github/workflows/prebuild-mcp-binary.yml`](../.github/workflows/prebuild-mcp-binary.yml)
on every push to `main`. `scripts/mcp-launcher.sh` execs it directly
(subject to the same `is_binary_fresh` staleness check as a local
`target/debug/calm`) — if it's there, there is no compile step for the MCP
dial to race against, because the checkout itself (which necessarily
completes before Claude Code can even read `.mcp.json`) already contains a
working binary.

**This is not a certainty, only a strong improvement**, for two concrete
reasons:

- It depends on the cloud checkout mechanism actually resolving the Git
  LFS pointer to real content (running the smudge filter) rather than
  leaving a ~130-byte text stub in place. `scripts/mcp-launcher.sh` detects
  an unresolved pointer and falls through safely instead of crashing (see
  `is_lfs_pointer`'s comment for why that needed an explicit check — a
  plain `exec` on a pointer stub does *not* fail gracefully), but a
  fallthrough here still means you're back to racing the Setup Script
  against a cold build.
- It only covers `x86_64-unknown-linux-musl` today. A different sandbox
  architecture falls through to the tiers below, unaffected either way.

Keep the Setup Script below as defense-in-depth regardless — it's the only
one of the two that's a guaranteed fix rather than a "very likely" one.

## The actual fix: a Cloud environment Setup Script

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

```bash
#!/bin/bash
# Ensure cargo is in PATH — setup scripts run as non-login root shells,
# so ~/.cargo/env is not sourced automatically.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Resolve Git LFS assets BEFORE building — a checkout without git-lfs
# installed leaves ~130-byte pointer stubs in place of the vendored
# embedding model (crates/calm-core/assets/potion-code-16m/) and the prebuilt
# .calm-bin/ binary. The build still "succeeds" either way (`include_bytes!`
# just bakes whatever is on disk into the binary) — this is a real incident,
# not a hypothetical: it silently degrades semantic search to
# `embeddings_status: "failed"` at runtime instead of failing the build
# where it'd be noticed. Best-effort, same `|| true` philosophy as the build
# below — `Embedder::load`'s own network-fallback and
# `embeddings_status: "offline_unavailable"` messaging are the safety net
# if this doesn't fully resolve it.
if command -v git >/dev/null 2>&1 && grep -q 'filter=lfs' .gitattributes 2>/dev/null; then
  if ! git lfs version >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
    apt-get install -y git-lfs >/dev/null 2>&1 || true
  fi
  if git lfs version >/dev/null 2>&1; then
    git lfs pull >/dev/null 2>&1 || true
  fi
fi

# Build the calm-cli binary. The `|| true` is CRITICAL: setup scripts that
# exit non-zero prevent the session from starting entirely (confirmed in
# the official docs). A failed build here is non-fatal — the MCP server
# simply won't connect, which is recoverable; a dead session is not.
cargo build --quiet -p calm-cli 2>&1 || true
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
session before any cache exists, or right after the ~7-day cache expiry)
— the Setup Script is what actually fixes the steady state.

## What's already handled in this repo (defense in depth, not a substitute)

- `.calm-bin/x86_64-unknown-linux-musl/calm` — see the section above. The one
  layer that can eliminate the race outright rather than just narrowing it,
  when it applies.
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
  Also runs the same `git lfs pull` best-effort step as the Setup Script
  snippet above, before building — see the runtime safety net below for
  what happens when this doesn't fully resolve it.
- `Embedder::load`'s network fallback + `embeddings_status:
  "offline_unavailable"` (`crates/calm-core/src/embedding.rs`,
  `crates/calm-server/src/lib.rs::bootstrap_embeddings`) — the last line of
  defense if the LFS pull above still leaves the vendored embedding model
  asset as a pointer stub (offline environment, apt blocked, etc.):
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
