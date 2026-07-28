---
title: "OTel span export, remote/HTTP transport, and dynamic toolsets"
SPEC_APPROVED: true
SPEC_ESCALATION: false
stakeholder: CALM maintainer (@Eilodon) + agents/teams running a shared CALM daemon
date: 2026-07-28
---

# Spec: Three ecosystem-alignment upgrades for CALM

Three independently-shippable proposals, ordered by fit-to-CALM (best first).
Each is grounded in the current code, not aspiration.

## Grounding facts (verified in code this session)

- `CalmServer.tool_router: ToolRouter<CalmServer>` (crates/calm-server/src/tools.rs:320)
  is built **once** at construction by `tool_router_for_preset` (tools.rs:360) — full
  router minus whatever the `preset` string excludes via `disable_route`. Every
  `for_connection` clone (tools/common.rs:93) inherits the SAME router via
  `..self.clone()`; the router is **frozen at daemon spawn**, first-writer-wins per
  ADR-0005. The `preset` field is `#[allow(dead_code)]` — dispatch reads `tool_router`.
- `for_connection` already establishes a real per-session boundary: fresh `session_id`,
  fresh `session_log`, fresh `oriented` gate. `active_sessions`, `edit_lock`,
  `next_session_id` stay shared. So "per-session mutable state" is an existing,
  tested pattern — but `tool_router` is explicitly NOT in the per-session-reset list.
- `init_daemon_tracing` (crates/calm-cli/src/main.rs:1110) already composes a
  `tracing_subscriber::registry().with(human_layer).with(audit_layer).init()`. A
  third `.with(otel_layer)` is the entire wiring surface for the daemon path. The
  non-daemon `calm serve` uses a **separate** tracing init (main.rs:236).
- Transport today: `serve_stdio` → `rmcp::transport::stdio` (lib.rs:71); daemon →
  Unix-socket accept loop `run_accept_loop` (daemon.rs:162), each accepted connection
  gets `server.for_connection()`. OS file perms (0700 `.calm/`, socket) are the only
  access control — there is no in-band auth anywhere.
- `Cargo.toml:173`: `rmcp = { features = ["server","transport-io","macros","elicitation","schemars"] }`.
  No `transport-streamable-http-server`. `tracing-subscriber` has `json` + `env-filter`.
- Session model is **stateful**: `oriented` gate enforces "orient before you act" per
  connection; `session_context` reports "who else is on this daemon". The MCP
  2026-07-28 spec RC pushes HTTP toward **stateless** (no `Mcp-Session-Id`).

---

## Proposal 1 — Dynamic toolsets (best fit)

### Intent
Let a client narrow the exposed tool set at runtime (e.g. start in a read/explore
set, unlock the edit set later), instead of the fixed `preset` frozen at daemon
spawn. Directly serves CALM's headline USP (token efficiency): today all ~30 tool
schemas sit in context even during pure read/explore phases.

### Design
- Add a per-session `enabled_toolsets: Arc<RwLock<BTreeSet<String>>>` to `CalmServer`,
  reset in `for_connection` (like `session_log`/`oriented`) so one session's toolset
  choice never leaks to another on a shared daemon.
- Keep the single frozen `tool_router` as the **universe** of routes. Filter at
  `list_tools` and gate at `call_tool` against `enabled_toolsets` ∩ preset. The
  `preset` remains the hard ceiling; dynamic toolsets can only ever be a subset of it.
- New tool `set_toolset(names: [..])` mutates the per-session set and returns the new
  visible tool list. Emit `notifications/tools/list_changed` so the client re-fetches.
- **Safety-tool floor:** a fixed set of tools can never be disabled — at minimum the
  orientation tools (`repo_overview`/`indexing_status`/`session_context`), the
  mandatory edit-gate tools (`edit_context`/`diff_impact`), and `set_toolset` itself.
  Otherwise a client could disable `edit_context` and then the `oriented`/edit gates
  become unreachable or vacuous.

### Non-goals
- Not per-request toolsets (per-session only).
- Not removing the static `preset` (dynamic is a subset-narrowing on top of it).

---

## Proposal 2 — Minimal OTel span export (cheap, opt-in only)

### Intent
Give the maintainer (or a team on a shared daemon) a real field signal — which phase
of a session eats latency (scanning/parsing/building_edges, hook wait, tool latency)
— by exporting the spans that already exist. Fills the self-acknowledged L6 gap
(specs/2026-07-14-...adoption-proposal.md:113) and aligns with the 2026-07-28 spec RC's
W3C Trace Context propagation in `_meta`.

### Design
- Add `tracing-opentelemetry` + `opentelemetry-otlp` behind a **non-default** Cargo
  feature `otel`.
- Wire one extra layer into BOTH tracing inits (`init_daemon_tracing` and the
  non-daemon `calm serve` init). The layer is only added when
  `OTEL_EXPORTER_OTLP_ENDPOINT` is set — absent env var = zero exporter, zero network,
  identical current behavior.
- Destination is **always** user-configured via standard `OTEL_*` env vars. CALM never
  ships a default endpoint and never phones home.
- Honor W3C `traceparent` from inbound `_meta` when the rmcp version in use surfaces it;
  otherwise root the trace locally. (rmcp support for the same-day RC is a watch item,
  not a blocker — local rooting works regardless.)
- On shutdown, flush/close the exporter (batch spans must not be lost on SIGTERM — the
  daemon already has a SIGTERM path).

### Non-goals
- No metrics/logs export in v1 (spans only).
- No content capture of source bodies in spans — span attributes limited to
  operation-level fields (tool name, phase, durations, counts), never file contents.

---

## Proposal 3 — Remote/HTTP transport (narrowest scope, most risk)

### Intent
Serve the devcontainer/Codespace/remote-SSH case: code lives on a remote host, the
chat client runs locally, and the agent talks to a CALM daemon co-located with the
real files instead of syncing a filesystem.

### Design
- Add `rmcp` feature `transport-streamable-http-server` behind a **non-default** Cargo
  feature `http`.
- New subcommand `calm serve --http --addr <ADDR>`. **Binds `127.0.0.1` by default.**
  A non-loopback bind requires an explicit `--allow-remote` flag AND a bearer token
  (`CALM_HTTP_TOKEN`); refuse to start a non-loopback bind with no token (fail-closed).
- Reuse `for_connection()` per HTTP session to preserve CALM's stateful session model
  (orientation gate, session_log, session_context). Do NOT adopt the RC's stateless
  model in v1 — it would defeat the `oriented` gate and "who else is here".
- Document explicitly that HTTP mode widens the trust boundary beyond "same-machine
  process" and is opt-in for remote-dev only, not a hosting posture.

### Non-goals
- No multi-tenant auth, no OAuth, no public hosting story.
- No stateless-HTTP rework in v1 (revisit only if/when rmcp stabilizes the RC shape).

---

## Rollout order
1. Dynamic toolsets (self-contained, extends an existing per-session pattern).
2. OTel (isolated behind a feature + env gate; near-zero blast radius).
3. HTTP (largest new trust surface; ship last, smallest scope).

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-28 | trigger: NORMAL -->

**Tier:** 2 (Production — no PII/payments, but the edit tools are a write path and
HTTP mode widens a trust boundary) | **Date:** 2026-07-28

### Context
```
CONTEXT_MODE:      DESIGN
STAKEHOLDER:       CALM maintainer + teams on a shared daemon
GOAL:              pre-mortem before implementing 3 ecosystem-alignment upgrades
AUDIT_TARGET_TIER: 2
```

### Failure Modes
1. **Dynamic toolsets silently defeat the safety USP** — a client disables the
   toolset holding `edit_context`/`diff_impact`/orientation tools; the mandatory
   pre-edit and pre-commit gates (the thing README sells as CALM's differentiator)
   become vacuous or unreachable at runtime. — **HIGH** — mitigation in plan: YES
   (safety-tool floor) — **FLAG:** the floor must gate the *enforcement path*
   (`call_tool` gate logic + `oriented`), not merely tool visibility; the plan must
   prove disabling a toolset cannot make a gate unreachable AND cannot deadlock edits.
2. **HTTP `--allow-remote` exposes the write path with weak auth** — edit tools reach
   the network; a bearer token in `CALM_HTTP_TOKEN` leaks via `/proc/<pid>/environ`,
   shell history, or CI logs; anyone on the segment who gets it can drive
   `edit_symbol`/`edit_lines` to write arbitrary code. The Unix-socket 0700 perms
   that are the *only* current access control simply don't exist over HTTP. — **HIGH**
   — mitigation in plan: PARTIAL (loopback default + fail-closed token) — **FLAG:**
   plan should default HTTP-remote to a **read-only preset** (edit tools loopback-only)
   and document TLS as required for any real remote use.
3. **OTel egress contradicts the "no code leaves your machine" USP** — span attributes
   carry file paths, symbol names, and query strings; pointing `OTEL_EXPORTER_OTLP_ENDPOINT`
   at a SaaS APM ships repo structure off-box, breaking the README headline claim. —
   **MED** — mitigation in plan: PARTIAL (no source bodies in spans) — **FLAG:** paths
   and symbol names are still identifiers; plan must (a) document the qualification to
   the "no code leaves" claim when `otel`+endpoint are active, (b) consider an
   attribute-redaction/opt-in-detail level.

### Layer Signals
- **L1 Logic:** `set_toolset` with an empty set, an unknown name, or a name outside the
  active preset — clamp to `preset ∩ floor`, reject unknown names via the existing
  `VALID_TOOLSET_NAMES` validation, never produce an empty visible set (floor guarantees
  non-empty).
- **L2 Concurrency:** `enabled_toolsets: Arc<RwLock<_>>` is read at dispatch while
  `set_toolset` may be writing it; rmcp dispatches tool calls concurrently. Dispatch must
  take a single atomic snapshot; a `list_changed` racing an in-flight call is acceptable
  but must not panic or half-apply.
- **L3 Data:** no DB schema change in any proposal. OTel batch exporter holds spans in a
  **bounded** in-memory queue (drops on overflow) — confirm it never grows unbounded when
  the endpoint is down.
- **L4 Integration:** OTLP endpoint down/slow/500 must be fire-and-forget — span export
  must never block or fail a tool call. HTTP client disconnect mid-call must reuse the
  existing `conn_ct` child-token cancellation pattern (daemon.rs:175), not orphan work.
- **L5 Security:** covered in FM2 (HTTP auth) + FM3 (egress). Additional: OTLP over
  plaintext `http://` leaks `traceparent`/attributes — document `https://` for remote
  collectors.
- **L6 Observability:** emit an `AUDIT_TARGET` audit.log event when a session changes its
  toolset and when an HTTP remote session is accepted — the two new runtime behaviors that
  otherwise leave no trace.
- **L7 Cross-cutting:** `set_toolset` is idempotent by construction (sets absolute state).
  No rate limits needed for local transports; see Abductive 2 for `list_changed`.

### Assumptions to Verify — RESOLVED 2026-07-28 (verified against real rmcp 2.2.0 source + cargo resolver)
- **VERIFIED — Assumption 1 (dynamic toolsets seam):** CALM already overrides both
  `list_tools` (tools.rs:589, returns `self.tool_router.list_all()`) and `call_tool`
  (tools.rs:601), and `call_tool` already has a per-request dispatch chokepoint (the
  orientation gate, tools.rs:642-661) BEFORE `self.tool_router.call(...)`. A per-session
  enabled-set filter drops in at exactly these two points — **no rmcp fork**. The
  `list_changed` emission is also supported: rmcp exposes `peer.notify_tool_list_changed()`
  (service/server.rs:491) reachable via `RequestContext.peer` (service.rs:865), plus
  `ServerCapabilities::enable_tool_list_changed()` to advertise it.
- **VERIFIED WITH A FINDING — Assumption 2 (OTel version compat):** the naive "latest"
  set is BROKEN — `tracing-opentelemetry` version-skips relative to `opentelemetry-otlp`:
  `tracing-opentelemetry 0.33 → opentelemetry 0.33`, but the latest published
  `opentelemetry-otlp` is `0.32 → opentelemetry 0.32` (no otlp 0.33 exists), and
  `tracing-opentelemetry 0.32.1 → opentelemetry 0.31`. A `cargo add` of the two latest
  produces **two coexisting opentelemetry core versions** (spans built with one, exported
  by the other — silently non-functional). **The one fully-aligned, currently-published
  set is the opentelemetry 0.31 line**, verified to resolve to a single core version via
  `cargo tree`: `opentelemetry 0.31` + `opentelemetry_sdk 0.31` + `opentelemetry-otlp 0.31.1`
  + `tracing-opentelemetry 0.32.1`. Plan MUST pin this co-tested set, never bare-`cargo add`
  latest. Secondary finding: `opentelemetry-otlp 0.31` defaults to the **grpc-tonic**
  exporter (pulls tonic/prost/hyper) — for a *minimal* footprint use the `http-proto`
  transport feature instead, avoiding the gRPC stack.
- **VERIFIED (by design) — Assumption 3 (no background task when unset):** holds as long as
  CALM builds the `TracerProvider`/OTLP pipeline ONLY when `OTEL_EXPORTER_OTLP_ENDPOINT` is
  set. The batch span processor spawns its background task at *provider-build* time, which
  CALM gates; absent the env var, the provider is never built → zero task, zero network.
  This is an actionable construction rule for the plan, not an external unknown.
- **VERIFIED — Assumption 4 (HTTP per-connection reuse):** rmcp
  `StreamableHttpService::new(service_factory: impl Fn() -> Result<S, io::Error>, ...)`
  (transport/streamable_http_server/tower.rs:631) calls the factory per session via
  `get_service()` and spawns each `S` in its own `serve_server` worker — so passing
  `|| Ok(daemon.for_connection())` gives every HTTP session a fresh per-connection
  `CalmServer` (session_id/oriented/session_log preserved). BONUS FINDING: rmcp 2.2.0's
  streamable-http server is **session-based** (`SessionManager` w/ `Mcp-Session-Id`) — the
  stateless MCP-2026-07-28-RC shape is NOT in the pinned rmcp yet, so the spec's "keep the
  stateful model in v1" is what rmcp actually implements, not merely a preference.

### Abductive Hypotheses
- **Abductive 1 (interaction of correct components):** OTel `traceparent` propagation + the
  shared daemon can **cross-correlate two clients' traces** — if the daemon honors inbound
  `_meta.traceparent` but roots spans on the shared server rather than per-`for_connection`,
  session A's trace tree absorbs session B's spans. Same lesson as `oriented`/`session_log`
  "reset per connection": trace context must root fresh per connection.
- **Abductive 2 (scale/adversarial):** a buggy or hostile client loops `set_toolset`,
  emitting a `tools/list_changed` flood; clients that re-fetch `tools/list` on every
  notification amplify it into a self-DoS on the daemon. Debounce/bound `list_changed`
  emission.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
**PASS WITH FLAGS** — proceed to writing-plans. The two HIGH findings (FM1 safety floor,
FM2 HTTP write-path auth) have mitigations in the spec, but writing-plans MUST include:
(1) proof the safety-tool floor gates the enforcement path, not just visibility;
(2) HTTP-remote defaulting to a read-only preset with edit tools loopback-only.
Recommended sequencing unchanged: dynamic toolsets → OTel → HTTP, so the highest-fit,
lowest-trust-surface work lands first and HTTP (largest new trust boundary) ships last
with the flags resolved.
