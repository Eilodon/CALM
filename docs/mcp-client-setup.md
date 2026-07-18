# Using the "calm" MCP server with different agents/IDEs

`calm` isn't an MCP server built only for Claude Code — `scripts/mcp-launcher.sh`
is a shared entrypoint for **any** stdio-speaking MCP client (Claude Code, Cursor,
VS Code, Windsurf, JetBrains, Codex CLI, Antigravity, or any tool that can
spawn a command). This file explains how the launcher works and how to
point each client at it.

## Don't want to clone the whole repo? — install the `calm` binary directly

The "Launcher resolves a binary in 3 tiers" section below describes how to self-host
**from within a CALM checkout** (useful if you're developing `calm` itself, or your
project *is* this repo). If you just want `calm` as a regular MCP server for
**a different project**, no checkout needed, there are 2 ways:

### 1. Install script (no Node required)

```bash
curl -fsSL https://raw.githubusercontent.com/Eilodon/CALM/main/scripts/install.sh | sh
```

Downloads the right prebuilt binary for your current platform (Linux x86_64/aarch64,
macOS Apple Silicon — the same 3-platform matrix `release.yml` builds), verifies
SHA256 against the `SHA256SUMS` published with the release, and installs to
`~/.local/bin/calm` (override via the `CI_INSTALL_DIR` variable). There's no
build-from-source tier here — there's no source checkout to build from; an
unsupported platform means `git clone` + `cargo build --release --bin calm`
by hand per the README instead of an automatic fallback.

### 2. npm (`@eilodon/calm-mcp`)

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

A thin JS package that picks the right prebuilt binary for your platform via
`optionalDependencies` (no network fetch on postinstall — the binary already
ships inside the npm tarball). See [`../npm/README.md`](../npm/README.md) for
how to publish/verify this package.

### 3. One-line CLI add-server commands (client already has one built in, no manual file editing)

No need to know the config file path ahead of time — the three clients below
write the config to the right place with a single command, short enough that
the agent itself (Claude/Codex/VS Code, when it has shell access) can run it on
the user's behalf if asked something like "install Eilodon's CALM for me":

```bash
# Claude Code
claude mcp add --transport stdio calm -- npx -y @eilodon/calm-mcp serve

# Codex CLI
codex mcp add calm -- npx -y @eilodon/calm-mcp serve

# VS Code (writes straight to the user profile, no config file to open)
code --add-mcp '{"name":"calm","command":"npx","args":["-y","@eilodon/calm-mcp","serve"]}'
```

Cursor/Windsurf/Antigravity don't have an equivalent CLI command yet — but since
those tools' agents (in agent/agentic mode) have file-write permission anyway,
the agent can still edit the right config file itself
(`.cursor/mcp.json`, `.vscode/mcp.json`, `~/.codeium/windsurf/mcp_config.json`,
`~/.gemini/config/mcp_config.json`) when asked — there just isn't a single
built-in command to type for it.

### After installing via method 1 or 2 above (doesn't apply to method 3 — `codex`/`claude mcp add` already wrote the config, no `calm setup` needed): `calm setup`

From inside the project you want `calm` to analyze:

```bash
calm setup
```

Writes/merges a `"calm"` entry into `.mcp.json`, `.cursor/mcp.json`,
`.vscode/mcp.json` in that project — without touching any other entries already
there — pointing straight at the binary you just installed. If a `"calm"`
entry already points somewhere else (e.g. you were previously using the
launcher script), `calm setup` leaves it alone by default; use
`calm setup --force` if you really want to overwrite it. Windsurf/JetBrains
still need manual pasting (see their own sections below), since those are
global configs, not project-level.

Want a **portable/shareable** config (commit `.mcp.json` to the repo for the
whole team/CI, independent of the binary path on your machine)?
`calm setup --npx` writes the entry as `npx -y @eilodon/calm-mcp serve`
instead of an absolute path — it automatically tracks the published npm
version, and only needs Node wherever it runs.

## Launcher resolves a binary in 3 tiers

`scripts/mcp-launcher.sh` always tries these in order, and uses the first
binary it finds:

1. **Fast path** — an already-usable binary: `$CI_MCP_BIN` (manual override) →
   `~/.cache/calm-mcp/<tag>/calm` (downloaded-and-verified from a previous run) →
   `target/release/calm` → `target/debug/calm` (an existing local build).
2. **Verified download** — only applies on Linux x86_64/aarch64, and **only when
   `HEAD` is sitting exactly on a released git tag** (never guesses at a
   version). Downloads the right platform's asset from that tag's GitHub
   Release, verifies SHA256 against the published `SHA256SUMS`, then
   sanity-checks that `calm --version` matches the expected version — only
   after all of that does it cache the binary and exec it. Any step failing
   (download error, checksum mismatch, version mismatch) falls through to
   tier 3 — it **never** execs a binary that hasn't finished verification.
3. **Build from source** — `cargo build -p calm-cli`, always works as long as
   the Rust toolchain is present. This is the only path for macOS/Windows,
   for a checkout mid-development (not sitting on a tag), or an environment
   with no network access.

Why it doesn't default to fetching "latest release": if you're developing on
`main` between two releases, fetching "latest" would silently install a binary
**older** than the source already on your machine — a mismatch that's very
hard to notice. The launcher only downloads by default when the checkout is
sitting exactly on a tag (a matching tag is the only case where it's safe to
trust the download); to prioritize fast startup and accept that version-drift
risk instead, set `CI_MCP_LAUNCHER_ALLOW_LATEST=1`.

If the SHA256 doesn't match (suspected corrupted or tampered-with download),
the launcher **does not exec** that binary — it logs a clear error to stderr
and automatically builds from source instead of just stopping, so the server
can still always start.

## Shared daemon mode (default since 2026-07-11)

Whatever binary gets selected above, the launcher by default invokes it via
`calm connect` instead of `calm serve` when both conditions hold: running on
Unix (macOS/Linux), and no other arg was passed to the launcher. `calm connect`
connects to (or spawns, if none exists yet) one daemon shared across the whole
project — multiple clients/sessions opening the same project share one
indexer/watcher/embedder instead of each session running its own (see
`docs/adr/0005-daemon-forwarder-shared-process.md`). You'll see
`.calm/daemon.sock`/`daemon.meta`/`daemon.log` appear in the project directory
as a sign this is active.

Any custom arg (e.g. a client config that adds its own `--preset`) makes the
launcher fall back to `calm serve` as before, unchanged. To turn daemon mode
off entirely (e.g. an environment that shouldn't share a process across
sessions), set `CI_MCP_LAUNCHER_NO_DAEMON=1`.

## Clients with config already checked into this repo

The following three files all point at `scripts/mcp-launcher.sh`, differing
only in the top-level field name:

| Client | File (repo-level) | Top-level field |
|---|---|---|
| Claude Code | `.mcp.json` | `mcpServers` |
| Cursor | `.cursor/mcp.json` | `mcpServers` |
| VS Code | `.vscode/mcp.json` | `servers` (different name, same `command`/`args` shape) |

Clone the repo and all three work immediately — no further configuration needed.

## Windsurf / Devin Desktop (global config, can't be checked in)

Windsurf rebranded to **Devin Desktop** (Cognition, June 2026) — still the same
underlying Cascade platform, the config path below is unchanged.

Windsurf/Devin only reads config from `~/.codeium/windsurf/mcp_config.json`
(per-user, no project-level option) — it can't be checked out with the repo,
so it has to be pasted by hand. The simplest way, **no CALM clone needed**,
uses npx like the Quick Start section in the README:

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

If you're developing on the CALM repo itself (not a different project), point
at `scripts/mcp-launcher.sh` directly instead of npx — replace
`/absolute/path/to/CALM` with the real path where you cloned this repo
(unlike the 3 checked-in configs above, the path here **must be absolute**
since there's no "project root" concept for a single global config file):

**IMPORTANT:** Because Windsurf uses a global config, you **MUST pass
`--project-root`** explicitly so CALM knows which project to index. Without
it, CALM falls back to the Windsurf process's current working directory
(which could be a different directory, or your home directory), leading to
the wrong scope getting indexed.

```json
{
  "mcpServers": {
    "calm": {
      "command": "bash",
      "args": ["/absolute/path/to/CALM/scripts/mcp-launcher.sh", "--project-root", "/absolute/path/to/CALM"]
    }
  }
}
```

Replace `/absolute/path/to/CALM` with the real path where you cloned this repo.
Both paths (the launcher script and the project root) must be absolute.

Devin Desktop also has its own "MCP Marketplace" right inside the Cascade
panel (the MCPs icon at the top, or Settings → Cascade → MCP Servers), which
supports one-click install via a deeplink shaped like
`windsurf://windsurf-mcp-registry?serverName=<server-name>` — **CALM isn't
listed there as of this writing**, so that kind of deeplink doesn't work for
CALM yet; use one of the two manual-paste methods above while a marketplace
submission is pending.

## JetBrains AI Assistant

Configured through JetBrains's own UI settings (not a file checked into the
repo) — point command/args at the exact same snippet as the Windsurf section
above (absolute path to `scripts/mcp-launcher.sh`).

**IMPORTANT:** Like Windsurf, JetBrains also uses a global config, so you
**MUST pass `--project-root`** explicitly so CALM knows which project to index:

```json
{
  "command": "bash",
  "args": ["/absolute/path/to/CALM/scripts/mcp-launcher.sh", "--project-root", "/absolute/path/to/CALM"]
}
```

Replace `/absolute/path/to/CALM` with the real path where you cloned this repo.

## Codex CLI (OpenAI)

**Fastest way — one command, no manual file editing:**

```bash
codex mcp add calm -- npx -y @eilodon/calm-mcp serve
```

This writes to the global config (`~/.codex/config.toml`) automatically. Check
it with `codex mcp list` or `/mcp` inside the Codex TUI.

**CORRECTION (2026-07-12):** an earlier version of this section said Codex was
"like Windsurf/JetBrains — no project-level option, global config only" — that
was wrong, corrected after re-checking current OpenAI documentation. Codex
**does support project-scoped config** via `.codex/config.toml` right inside
the repo, as long as that project is marked "trusted" (the exact trust
mechanism isn't documented in detail by OpenAI). A few sensitive keys
(`model_provider`, `model_providers`, `openai_base_url`, `notify`) are locked
and can't be overridden at the project level — but `mcp_servers.*` isn't on
that locked list, so CALM can still be declared here instead of only globally:

```toml
# .codex/config.toml (checked into the repo, requires the project be "trusted" by Codex)
[mcp_servers.calm]
command = "npx"
args = ["-y", "@eilodon/calm-mcp", "serve"]
```

Or, if you're developing on the CALM repo itself, point at
`scripts/mcp-launcher.sh` (absolute path, same reasoning as Windsurf) instead
of npx:

**IMPORTANT:** If you use the launcher script (instead of npx), you **MUST
pass `--project-root`** explicitly so CALM knows which project to index:

```toml
[mcp_servers.calm]
command = "bash"
args = ["/absolute/path/to/CALM/scripts/mcp-launcher.sh", "--project-root", "/absolute/path/to/CALM"]
```

Replace `/absolute/path/to/CALM` with the real path where you cloned this repo.

See details: [developers.openai.com/codex/mcp](https://developers.openai.com/codex/mcp).

**Codex Cloud (the hosted/async product, different from the ChatGPT web app):**
not yet confirmed whether it has an equivalent setup-script/environment-config
mechanism for pre-building a binary — OpenAI's public docs aren't detailed
enough here (unlike the ChatGPT web app, which is confirmed to *not* read local
Codex config, using its own separate plugin mechanism instead). Anyone who
actually needs Codex Cloud support should test it directly rather than guess
from the docs.

## Antigravity (Google)

Also a global config, shared between the Antigravity IDE and the Antigravity
CLI, at `~/.gemini/config/mcp_config.json` — same JSON `mcpServers` shape as
Claude Code/Cursor, just a different file location (global, not project-level):

**IMPORTANT:** Like Windsurf, Antigravity also uses a global config, so you
**MUST pass `--project-root`** explicitly so CALM knows which project to index:

```json
{
  "mcpServers": {
    "calm": {
      "command": "bash",
      "args": ["/absolute/path/to/CALM/scripts/mcp-launcher.sh", "--project-root", "/absolute/path/to/CALM"]
    }
  }
}
```

Replace `/absolute/path/to/CALM` with the real path where you cloned this repo.

After saving, Antigravity reloads automatically — no restart needed. It can
also be edited from inside the IDE via "..." on the agent panel → "Manage MCP
Servers" → "View raw config". The path to `mcp-launcher.sh` still must be
absolute, for the same reason as Windsurf.

## Related: a cold-start race condition on Claude Code on the web

`docs/cloud-environment-setup.md` covers a separate issue specific to Claude
Code on the web: the MCP client dials the server **in parallel** with the
SessionStart hook, with no ordering guarantee — so
`.claude/hooks/session-start-build-calm.sh` still exists independently of this
launcher. The launcher's fast path (tier 1) only checks "does a binary already
exist," not whether that binary is stale (e.g. mid-edit on `calm`'s own
source) — that's still that SessionStart hook's own job, not replaced by this
launcher.
