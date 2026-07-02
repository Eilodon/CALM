# Cloud environment setup — why the "ci" MCP server needs a Setup Script

This repo dogfoods its own MCP server: `.mcp.json` wires up a `ci` stdio
server pointed at this workspace's own `ci-cli` binary. On Claude Code on
the web, that binary does not exist until something compiles it — and
where that compile happens determines whether the server ever connects.

## The failure this document exists to prevent

`ci-cli` is a Rust binary with a nontrivial dependency tree (tree-sitter
grammars, stack-graphs, bundled SQLite). A cold `cargo build -p ci-cli` on
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
`.claude/hooks/session-start-build-ci.sh`) can therefore **win** the race
against a cold connection attempt, but cannot **guarantee** winning it — a
59s build has no trouble outlasting a ~7s retry budget. A previous version
of this repo's setup relied on that hook alone and believed it had fixed
the problem; it hadn't — it had just usually won the race, until a session
where it didn't.

## The actual fix: a Cloud environment Setup Script

Setup Scripts and `SessionStart` hooks look similar but solve different
problems:

| | Setup Script | `SessionStart` hook |
|---|---|---|
| Runs | Once, **before** Claude Code (and MCP dialing) launches at all | Every session, **after** Claude Code launches, concurrently with MCP dialing |
| Configured in | Cloud environment settings UI (not in this repo) | `.claude/settings.json` (this repo, `.claude/hooks/session-start-build-ci.sh`) |
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
cd "$(dirname "$0")" 2>/dev/null || true
cargo build --quiet -p ci-cli
```

(Setup scripts run with the repo checked out as the working directory, so
a plain `cargo build --quiet -p ci-cli` without the `cd` is equally fine —
the `cd` guard above is only there in case that assumption ever changes.)

This must build to the **same path** `.mcp.json` expects:
`target/debug/ci` (debug, not `--release` — release compiles slower for no
benefit here, since this binary is a local dev/dogfood tool, not a
distributed artifact; keep this in sync with
`.claude/hooks/session-start-build-ci.sh` and
`.claude/hooks/ci-mcp-entrypoint.sh` if that ever changes).

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
This is optional defense-in-depth (matters mainly for the very first
session before any cache exists, or right after the ~7-day cache expiry)
— the Setup Script is what actually fixes the steady state.

## What's already handled in this repo (defense in depth, not a substitute)

- `.claude/hooks/ci-mcp-entrypoint.sh` — `.mcp.json`'s actual entrypoint
  now. Execs the pre-built binary directly if present (no `cargo`
  involved, no risk of an unexpected rebuild reopening this exact race);
  only builds inline if the binary is missing outright.
- `.claude/hooks/session-start-build-ci.sh` — still runs every session.
  Redundant with the Setup Script in the common case (no-op if `target/`
  is already warm), but it's what keeps the binary from going *stale*
  (e.g. after editing `ci`'s own source), and it's the only mechanism at
  all for local/non-cloud Claude Code, which has no Setup Script concept.

None of this replaces the Setup Script for cloud sessions — it narrows the
window and covers the cases the Setup Script can't (local dev, staleness),
but only the Setup Script removes the compile step from the connection
race entirely.

## How to verify it worked

At the start of a session, once indexing has had a moment to run:

```
mcp__ci__repo_overview()
```

If this resolves (rather than the tool being entirely absent from your
tool list), the connection succeeded. If it's missing, check for a `ci-cli
pre-build failed` message in the session's `SessionStart` hook output —
that means the fallback hook itself failed (e.g. a real compile error),
which is a different problem than the timing race this document covers.
