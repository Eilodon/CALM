# ADR: Dynamic toolsets, opt-in OTel span export, opt-in remote/HTTP transport

## 1. Title
Ship runtime-narrowable toolsets, opt-in OpenTelemetry span export, and an opt-in
Streamable-HTTP transport, without weakening CALM's safety gates or local-first posture.

## 2. Context
Three ecosystem-alignment gaps were identified via a comparison against SOTA MCP
servers (2026-07-19/20 research, `docs/superskills/adrs/` history): no runtime
toolset narrowing (agents get a static preset for the whole session), no
observability integration, and no remote-dev transport (stdio/unix-socket only,
unusable across a devcontainer/Codespace boundary). A spec
(`docs/superskills/specs/2026-07-28-otel-http-dynamic-toolsets.md`) and audit-design
pass (PASS WITH FLAGS, 2 HIGH findings: FM1 toolset-bypass, FM2 HTTP write-path
exposure) preceded a detailed implementation plan
(`docs/superskills/plans/2026-07-28-otel-http-dynamic-toolsets.md`), executed here
task-by-task under TDD.

## 3. Decision
**Phase 1 — dynamic toolsets:** a new `set_toolset` MCP tool lets a session narrow
its visible tool set at runtime (`enabled_toolsets: Arc<RwLock<Option<BTreeSet<String>>>>`
on `CalmServer`, reset fresh per connection). `effective_tool_names` computes
`(preset ∩ (requested ∪ floor))`. `SAFETY_FLOOR_TOOLSETS` is non-disableable —
verified against the REAL `#[tool_router]` groupings, not tool-name assumptions
(the plan's own guess, `["orient", "edit"]`, was wrong: `edit_context`/`diff_impact`
live in `guardrails`, `indexing_status`/`session_context` live in `recover`; the
correct floor is `["orient", "edit", "guardrails", "recover"]`). Enforced at BOTH
`list_tools` (hides) and `call_tool` (refuses dispatch by name) — filtering only
`list_tools` is cosmetic. `notifications/tools/list_changed` is debounced (only
fires when the narrowing actually changes).

**Phase 2 — opt-in OTel:** an `otel` Cargo feature (opentelemetry/opentelemetry_sdk/
opentelemetry-otlp pinned to the 0.31 line, tracing-opentelemetry 0.32 — live
re-verified against crates.io on the day of implementation, not just trusted from
the morning's audit, since the ecosystem had already shifted mid-day) adds a
conditional `tracing_subscriber` layer. Even when compiled in, no OTLP pipeline is
built unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set at runtime — no exporter, no
provider, no background task, no network I/O.

**Phase 3 — opt-in remote/HTTP:** `calm serve --http` (needs the `http` feature)
wraps `CalmServer::for_connection()` in rmcp's `StreamableHttpService`. Loopback-only
by default; a non-loopback `--addr` is refused unless `--allow-remote`, which itself
requires a non-empty `CALM_HTTP_TOKEN` (fail-closed on both). Any non-loopback bind
forces the `full,-edit` preset regardless of `--preset` — the write path is never
network-reachable by default. A bearer-token axum middleware gates every request
when remote.

## 4. Status
ACCEPTED

## 5. Consequences

**Improved:**
- Agents can narrow their own tool surface mid-session without a restart, with a
  provably non-bypassable safety floor (real daemon+socket integration test, not
  a mocked one — rmcp 2.2.0 keeps `Peer::new`/`RequestContext::new` crate-private,
  so this could not be unit-tested in-process; confirmed by direct inspection of
  the pinned rmcp source).
- Optional, zero-cost-when-unused observability hook for operators who want it.
- A devcontainer/Codespace story that didn't exist before, with a real fail-closed
  security posture (not "documented as insecure," actually refuses to start
  insecurely).

**Worsened / new surface:**
- Two new optional Cargo features (`otel`, `http`) each add real transitive
  dependency weight when built (axum, tonic/tonic-prost as *compile-time-only*
  deps of `opentelemetry-otlp` even under `http-proto`, reqwest as a genuinely new
  workspace dependency). Mitigated: both features are off by default; CI now
  builds/tests/clippies both explicitly plus their combination.
- `calm-cli` gained a small lib target (`src/lib.rs`) alongside its existing bin —
  previously bin-only. Purely additive (existing bin behavior unchanged), exists
  only so `otel_layer`/`resolve_http_launch` are reachable from integration tests.
- The OTel dependency pin is a live, moving target (opentelemetry core published
  0.32.0 the same day this was implemented) — the CI guard (`otel-http-features`
  job) exists specifically to catch a future silent regression here.

**Debt knowingly created:**
- `serve_http`'s audit-log-on-accept doesn't carry the remote peer's IP (the
  `StreamableHttpService` factory seam doesn't expose per-request data) —
  documented in `docs/http-transport.md`, not silently omitted.
- No rate limiting / DoS protection on the HTTP transport — explicitly out of
  scope (single-tenant dev-loop tool, documented in the threat-model doc).

## 6. Alternatives Considered

- **Filtering only `list_tools` for the toolset floor** (matching a literal, narrower
  reading of the plan) — rejected: a client could still dispatch a "hidden" tool by
  name via `call_tool`, since MCP doesn't require a client to have called
  `tools/list` first. This was the audit's FM1 finding; enforcement had to be at
  the actual dispatch chokepoint.
- **`CalmServer::new_with_preset` directly in `serve_http`** (the plan's literal
  pseudocode signature) — rejected after checking `serve_unix_daemon`'s real
  implementation: it uses `calm_server::bootstrap()`, which also starts the
  background indexer/embedder/watcher and installs SIGINT/SIGTERM handlers. A bare
  constructor would have shipped an HTTP server whose index never builds.
- **The plan's literal `cargo tree | grep -c 'opentelemetry v0.3[^1]'` CI guard** —
  rejected after live-reproducing that it false-positive-matches the substring
  inside `tracing-opentelemetry v0.32.1`, which would make the guard permanently
  red from the day it merged. Replaced with `cargo metadata | jq` filtering on the
  exact package name field.

## 7. Evidence

- `cargo test --workspace --features otel,http` (the untested-in-plan combination):
  272 (calm-server lib) + 3 (watcher_integration) + all calm-cli integration
  suites passed, 0 failed — `[verified 2026-07-28]`.
- `cargo tree -p opentelemetry` in both a scratch probe and this real workspace:
  exactly one resolved `opentelemetry v0.31.0` — `[verified 2026-07-28]`.
- `narrowed_session_hides_tool_from_list_and_refuses_to_dispatch_it`
  (`crates/calm-cli/tests/daemon_integration.rs`): real daemon subprocess + unix
  socket + raw JSON-RPC — narrows to `trace`, confirms `scan_text` absent from
  `tools/list` AND refused with `isError:true` by `tools/call`, confirms
  `session_context` (floor) still dispatches — `[verified 2026-07-28]`.
- `resolve_http_launch`'s 4 scenarios (`crates/calm-cli/tests/http_guard.rs`):
  non-loopback-without-allow-remote refused, allow-remote-without-token refused
  (including empty-string token), allow-remote-with-token forces `full,-edit`,
  loopback needs neither — `[verified 2026-07-28]`.
- `otel_layer_is_none_without_endpoint_env`
  (`crates/calm-cli/tests/otel_gate.rs`): confirmed both under `--features otel`
  (1 test runs, passes) and without it (0 tests, file inert) —
  `[verified 2026-07-28]`.
- CI guard script (`cargo metadata | jq`) dry-run: reports count=1, version=0.31.0
  in the current state — `[verified 2026-07-28]`. Its correct-failure path (does
  it actually trip on a real 2-core skew) is `[assumed — verify post-deploy]`,
  since manufacturing a genuine second-core regression to test against was not
  attempted (would require a real crates.io publish or a local registry override).

## 8. Owner
Claude Sonnet 5 (executing agent), on behalf of the repository owner (@Eilodon) —
solo-maintained repo per the plan's own task-risk-score context.

## 8b. Known Debts (PATTERN-DEBT)
No `docs/superskills/pattern-debt.md` exists in this repo (checked — this repo
uses `docs/pattern-debt-registry.yaml` as its own real DEBT-NNN tracker, a
different mechanism from the Super Skills PATTERN-DEBT format). No DEBT-NNN entries
were created or affected by this change: this was net-new feature work, not a bug
fix, so `pattern-globalize` doesn't apply. The two "Debt knowingly created" items
in Section 5 (no per-request IP in the HTTP audit log; no HTTP rate limiting) are
tracked in prose in `docs/http-transport.md`, not as registry entries, since
neither is a *bug pattern* to globalize-check elsewhere in the codebase.

## 9. Next Cycle Trigger
When `opentelemetry` (exact package) resolves to more than one version under
`--features otel` (the `otel-http-features` CI job's guard step fails) OR when
`tracing-opentelemetry`'s published dependency requirement on `opentelemetry`
changes such that the 0.31/0.32 pairing this ADR pins no longer resolves at all.

## 10. Cycle Retrospective

- **A design-time audit finding can go stale within the same day.** The
  morning's audit-design pass verified the OTel 0.31-line pin resolved to a
  single core; by the time Phase 2 was implemented (afternoon, same day),
  `opentelemetry` core had published 0.32.0 on crates.io. Re-verify version-skew
  claims live at implementation time, not just at design time, for any dependency
  set from a fast-moving ecosystem — don't trust a morning's `cargo tree` output
  in the afternoon.
- **A plan's pseudocode can be subtly wrong about which layer owns setup.** The
  plan's `serve_http` signature took a bare `CalmServer`, implying the caller just
  constructs one — but the two existing serve paths (`serve_stdio_with_preset`,
  `serve_unix_daemon`) both route through `bootstrap()` for the indexer/embedder/
  signal-handler setup. Always check how existing sibling code paths actually wire
  a dependency before trusting a plan's simplified signature for a new one.
- **A `grep`-based CI guard needs an anchor or a structured-data alternative.**
  Both this session's own ad hoc dependency-tree checks AND the plan's proposed CI
  guard independently hit the same class of bug: an unanchored substring pattern
  matching `X` inside `prefix-X`. Default to `cargo metadata --format-version 1 |
  jq` (exact field match) over `cargo tree | grep` for any future dependency-graph
  CI assertion in this repo.
- **rmcp 2.2.0's `Peer::new`/`RequestContext::new` are crate-private.** Any future
  plan that proposes testing `list_tools`/`call_tool` (or anything needing a real
  `RequestContext<RoleServer>`) with a hand-built context in-process will hit this
  wall. The working pattern in this repo is a real daemon subprocess +
  socket + raw JSON-RPC (`daemon_integration.rs`), not a mock.
- **Debt knowingly created:** HTTP audit logging lacks per-request remote-IP
  (StreamableHttpService's factory seam has no per-request hook without deeper
  axum middleware work) — deferred, documented, not silently dropped.
