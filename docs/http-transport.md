# Remote/HTTP transport (advanced, opt-in)

By default CALM talks to its client over stdio (or a local unix socket in
shared-daemon mode — see `docs/mcp-client-setup.md`). `calm serve --http`
adds a third, opt-in transport: Streamable-HTTP, the same protocol
Claude Code and other clients speak to remote MCP servers.

This exists for one narrow use case: **a devcontainer or GitHub Codespace**,
where the editor/agent runs on your machine but the project (and therefore
the index) lives inside the container, and stdio/unix-socket forwarding
isn't available across that boundary. It is not a general-purpose "expose
CALM to the internet" feature, and the defaults below are deliberately
conservative about being used that way.

## Requires the `http` build feature

```
cargo build --features http
```

A binary built without this feature still accepts `--http` on the command
line (so `--help` output doesn't silently omit it) but refuses to start:
`--http requires building with --features http (this binary wasn't)`.

## Loopback by default

```
calm serve --project-root . --http
# binds 127.0.0.1:8787 by default (--addr to override)
```

A bind address whose IP is not loopback (`127.0.0.1`/`::1`) is **refused**
unless `--allow-remote` is also passed:

```
$ calm serve --http --addr 0.0.0.0:8787
error: refusing non-loopback HTTP bind 0.0.0.0:8787 without --allow-remote
       (fail-closed default -- see docs/http-transport.md)
```

This is intentional and fail-closed: the common devcontainer/Codespace case
only ever needs a loopback bind (the port is forwarded to your local
machine by the container tooling itself, not exposed on the container's own
network interface), so that's what works with zero extra flags. Reaching
for `--allow-remote` should be a deliberate, informed decision, not an
accident of binding `0.0.0.0` out of habit.

## `--allow-remote` requires a token, and forces read-only

```
CALM_HTTP_TOKEN=$(openssl rand -hex 32) \
  calm serve --http --addr 0.0.0.0:8787 --allow-remote
```

Two things happen the moment a bind is non-loopback, and neither is
optional:

1. **`CALM_HTTP_TOKEN` must be set to a non-empty value**, or the server
   refuses to start at all — an unauthenticated remote bind is refused
   before it ever opens a socket, not left to a middleware layer to catch
   after the fact. Every request then needs
   `Authorization: Bearer <that token>` or gets `401 Unauthorized`.
2. **The effective preset is forced to `full,-edit`** (every tool except
   the edit toolset), *regardless of what `--preset` requested*. The
   write path — `edit_lines`, `edit_symbol`, `format_files` — is never
   network-reachable via this transport by default. There is currently no
   flag to override this; if you need remote edit access, you're outside
   this feature's intended scope and should reconsider the setup instead
   (e.g. run CALM inside the same trust boundary as the client).

A loopback bind needs neither: no token check, and whatever `--preset` you
asked for.

## TLS: bring your own

The bearer-token check is a coarse gate against an unauthenticated client
reaching the tool surface — it is **not** a substitute for TLS. The token
travels in plaintext over whatever transport carries the HTTP request; if
`--allow-remote` traffic crosses a network you don't fully trust, put a
real reverse proxy (nginx, Caddy, your cloud provider's load balancer) in
front that terminates TLS, and bind CALM itself to loopback behind it —
the proxy talks to `127.0.0.1:8787`, only the proxy's TLS-terminated
endpoint is actually remote-facing. CALM does not attempt to be its own
TLS-terminating edge server.

## What isn't covered here

- **Rate limiting / DoS protection** — none. This is a single-tenant
  dev-loop tool, not a public service.
- **Per-request audit detail** — `serve_http`'s session-accept audit log
  (`.calm/audit.log` in daemon mode) doesn't currently carry the remote
  peer's IP; see `crates/calm-server/src/http.rs`'s doc comment for why
  (the service-factory seam it hooks doesn't have per-request access).
- **Token rotation** — restart the process with a new `CALM_HTTP_TOKEN` to
  rotate; there's no live-reload for it.
