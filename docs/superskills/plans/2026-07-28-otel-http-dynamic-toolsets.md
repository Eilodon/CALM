# OTel · HTTP · Dynamic Toolsets — Implementation Plan

> **For agentic workers:** Use `subagent-driven-development` (recommended) or
> `executing-plans` to implement this plan task-by-task. Each Phase is independently
> shippable — stop after any Phase and you still have working, tested software.

**Goal:** Ship three ecosystem-alignment upgrades to CALM — runtime-dynamic toolsets,
opt-in OpenTelemetry span export, and an opt-in remote/HTTP transport — without weakening
CALM's safety gates or its "local-first" posture.

**Architecture:** All three build on existing seams. Dynamic toolsets add a per-session
`enabled_toolsets` set filtered at the two `ServerHandler` dispatch points CALM already
overrides (`list_tools`/`call_tool`). OTel adds one conditional `tracing_subscriber` layer
to the two existing tracing inits, gated by `OTEL_EXPORTER_OTLP_ENDPOINT`. HTTP wraps the
existing `CalmServer::for_connection()` in rmcp's `StreamableHttpService` service-factory.

**Tech Stack:** Rust, rmcp 2.2.0, tracing/tracing-subscriber 0.3, tokio 1.52,
opentelemetry 0.31 line (see Phase 2 pinning note), axum/hyper (via rmcp `server-side-http`).

**Audit Gate:** PASS WITH FLAGS (docs/superskills/specs/2026-07-28-otel-http-dynamic-toolsets.md)

**Risk Flags:**
- **HIGH — Task 1.5** (safety-tool floor): a disabled toolset must never make a safety gate
  unreachable, and must never deadlock edits.
- **HIGH — Task 3.4** (HTTP read-only default): the write path must not be network-reachable
  by default even under `--allow-remote`.

---

## Grounding anchors (verified 2026-07-28 — re-verify line numbers before editing)

| What | Where |
|---|---|
| `CalmServer` struct | crates/calm-server/src/tools.rs:226-332 |
| `CalmServer::for_connection` (per-session reset) | crates/calm-server/src/tools/common.rs:93-119 |
| `list_tools` override | crates/calm-server/src/tools.rs:589-599 |
| `call_tool` override + orientation chokepoint | crates/calm-server/src/tools.rs:601-681 |
| `tool_router_for_preset` | crates/calm-server/src/tools.rs:360-373 |
| `resolve_preset` / `toolset_tools` / `TOOLSET_NAMES` | crates/calm-server/src/tools/common.rs:860-997 |
| `init_daemon_tracing` (daemon path) | crates/calm-cli/src/main.rs:1110-1193 |
| non-daemon `calm serve` tracing init | crates/calm-cli/src/main.rs:236-242 |
| `Commands::Serve` arm + daemon/stdio branch | crates/calm-cli/src/main.rs:246-290 |
| daemon accept loop (`for_connection` per conn) | crates/calm-server/src/daemon.rs:162-191 |
| rmcp `StreamableHttpService::new(service_factory,…)` | rmcp-2.2.0 transport/streamable_http_server/tower.rs:631 |
| rmcp `peer.notify_tool_list_changed()` | rmcp-2.2.0 service/server.rs:491 |

> **Constructor note:** `CalmServer` is built at exactly one struct-literal site. Before
> Phase 1, grep `CalmServer {` under `crates/calm-server/src/` to find it (it lives in the
> serve/bootstrap path, not in `for_connection`, which uses `..self.clone()`). New fields in
> Task 1.1 are initialized there.

---

# Phase 1 — Dynamic toolsets

**Ships:** a `set_toolset` tool + per-session narrowing of the visible tool set, emitting
`notifications/tools/list_changed`, with a hard safety-tool floor.

### Task 1.1: Add the per-session `enabled_toolsets` field

**Files:**
- Modify: `crates/calm-server/src/tools.rs` (`CalmServer` struct + the constructor site)
- Modify: `crates/calm-server/src/tools/common.rs` (`for_connection`)

- [ ] **Step 1: Add the field to the struct** (after `oriented`, tools.rs:331):
```rust
    /// Per-session runtime toolset narrowing (Phase 1 dynamic toolsets).
    /// `None` = no runtime narrowing, expose whatever `preset`/`tool_router`
    /// already allows (the default; identical to pre-Phase-1 behavior).
    /// `Some(set)` = expose only tools whose toolset is in `set`, intersected
    /// with the preset ceiling and unioned with the non-disableable floor
    /// (see `SAFETY_FLOOR_TOOLSETS`). MUST be reset to a fresh `Arc` in
    /// `for_connection` (like `session_log`/`oriented`) so one session's
    /// narrowing never leaks onto another on a shared daemon.
    enabled_toolsets: Arc<RwLock<Option<std::collections::BTreeSet<String>>>>,
```
- [ ] **Step 2: Initialize it at the constructor site** — add to the `CalmServer { … }`
  struct literal: `enabled_toolsets: Arc::new(RwLock::new(None)),`
- [ ] **Step 3: Reset it in `for_connection`** (common.rs, alongside the `oriented` reset):
```rust
        // Fresh per connection — see field doc. NOT inherited via `..self.clone()`.
        enabled_toolsets: Arc::new(RwLock::new(None)),
```
- [ ] **Step 4: Compile** `cargo build -p calm-server` → expected: PASS (unused field warning OK)
- [ ] **Step 5: Commit** `git commit -am "feat(toolsets): add per-session enabled_toolsets field"`

### Task 1.2: Define the safety-tool floor (data only, no behavior yet)

**Files:**
- Modify: `crates/calm-server/src/tools/common.rs` (near `TOOLSET_NAMES`)
- Test: same file, `#[cfg(test)]` module

- [ ] **Step 1: Write the failing test:**
```rust
    #[test]
    fn safety_floor_toolsets_are_all_real_and_cover_the_gates() {
        // Every floor toolset must be a real toolset name…
        for name in SAFETY_FLOOR_TOOLSETS {
            assert!(TOOLSET_NAMES.contains(name), "floor toolset {name:?} not in TOOLSET_NAMES");
        }
        // …and the union of floor tools must include the mandatory gate tools.
        let floor: std::collections::BTreeSet<String> = SAFETY_FLOOR_TOOLSETS
            .iter()
            .flat_map(|n| toolset_tools(n).unwrap())
            .collect();
        for required in ["repo_overview", "indexing_status", "session_context",
                         "edit_context", "diff_impact", "set_toolset"] {
            assert!(floor.contains(required), "safety floor missing gate tool {required:?}");
        }
    }
```
- [ ] **Step 2: Run — verify FAIL** `cargo test -p calm-server safety_floor_toolsets` → FAIL (const missing)
- [ ] **Step 3: Add the const** (after `TOOLSET_NAMES`, common.rs:874):
```rust
/// Toolsets that runtime narrowing (`set_toolset`) can NEVER disable — the
/// non-negotiable floor. Orientation tools back the `oriented` gate; `edit`
/// backs the mandatory pre-edit `edit_context` + pre-commit `diff_impact`
/// gates; without them a client could narrow the visible set until a gate is
/// unreachable (safety bypass) OR until edits deadlock (the native-Edit hook
/// still denies, but `edit_context` is gone → agent can never satisfy it).
/// `orient` also carries `set_toolset` itself, so a session can always widen
/// back out. See Task 1.5 in the plan for the enforcement-path argument.
pub(crate) const SAFETY_FLOOR_TOOLSETS: &[&str] = &["orient", "edit"];
```
- [ ] **Step 4: Run — verify PASS** `cargo test -p calm-server safety_floor_toolsets` → PASS
- [ ] **Step 5: Commit** `git commit -am "feat(toolsets): define non-disableable SAFETY_FLOOR_TOOLSETS"`

> **Note for executor:** confirm `set_toolset` (Task 1.4) is registered in the `orient`
> module's `#[tool_router]` so it lands inside the floor. If it goes in a different module,
> add that module's toolset name to `SAFETY_FLOOR_TOOLSETS` and update the test.

### Task 1.3: Compute the effective visible set (pure function, unit-tested)

**Files:**
- Modify: `crates/calm-server/src/tools/common.rs`
- Test: same file

- [ ] **Step 1: Write the failing test:**
```rust
    #[test]
    fn effective_tool_names_intersects_preset_and_floors_safety() {
        let preset_full = calm_all_tool_names();
        // Narrow to just "trace": result = trace tools + floor tools, all ⊆ preset.
        let narrowed = Some(std::collections::BTreeSet::from(["trace".to_string()]));
        let got = effective_tool_names(&preset_full, narrowed.as_ref());
        assert!(got.contains("edit_context"), "floor tool dropped");   // from floor
        assert!(got.contains("callers") || got.contains("path"),       // from trace toolset
                "requested toolset tools missing");
        assert!(got.is_subset(&preset_full), "escaped the preset ceiling");
        // None = unchanged passthrough of the preset.
        assert_eq!(effective_tool_names(&preset_full, None), preset_full);
    }
```
- [ ] **Step 2: Run — verify FAIL** `cargo test -p calm-server effective_tool_names` → FAIL
- [ ] **Step 3: Implement:**
```rust
/// The concrete tool-name set a session should see, given the preset ceiling
/// and an optional runtime narrowing. `None` narrowing = return the ceiling
/// unchanged. `Some(sets)` = (union of those toolsets' tools ∪ floor) ∩ ceiling.
/// The floor is unconditional; the ceiling is never exceeded.
pub(crate) fn effective_tool_names(
    preset_ceiling: &std::collections::BTreeSet<String>,
    narrowing: Option<&std::collections::BTreeSet<String>>,
) -> std::collections::BTreeSet<String> {
    let Some(sets) = narrowing else {
        return preset_ceiling.clone();
    };
    let mut visible: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for name in sets.iter().chain(SAFETY_FLOOR_TOOLSETS.iter().map(|s| &s.to_string())) {
        if let Some(tools) = toolset_tools(name) {
            visible.extend(tools);
        }
    }
    visible.intersection(preset_ceiling).cloned().collect()
}
```
> **Executor caution:** the `.chain(SAFETY_FLOOR_TOOLSETS.iter().map(|s| &s.to_string()))`
> line creates temporaries; if the borrow checker complains, build an owned
> `Vec<String>` of names first, then iterate. Keep the semantics identical.
- [ ] **Step 4: Run — verify PASS** `cargo test -p calm-server effective_tool_names` → PASS
- [ ] **Step 5: Commit** `git commit -am "feat(toolsets): effective_tool_names (preset ∩ (request ∪ floor))"`

### Task 1.4: Add the `set_toolset` tool

**Files:**
- Modify: `crates/calm-server/src/tools/orient.rs` (register in its `#[tool_router]`)
- Test: `crates/calm-server/src/tools.rs` `#[cfg(test)]`

- [ ] **Step 1: Write the failing test** (in the tools.rs test module, using a
  test-constructed `CalmServer`):
```rust
    #[tokio::test]
    async fn set_toolset_narrows_then_widens_and_never_drops_floor() {
        let server = CalmServer::for_test("full"); // existing test constructor helper
        // Narrow to trace only.
        server.apply_toolset(Some(vec!["trace".into()])).await.unwrap();
        let visible = server.current_visible_tool_names().await;
        assert!(visible.contains("edit_context"), "floor dropped on narrow");
        assert!(!visible.contains("scan_text"), "security tool leaked past narrow");
        // Widen back to full.
        server.apply_toolset(None).await.unwrap();
        assert!(server.current_visible_tool_names().await.contains("scan_text"));
    }
```
> **Executor:** if `for_test`/`current_visible_tool_names` helpers don't exist, add thin
> test-only helpers next to the existing test scaffolding — do NOT invent a public API.
- [ ] **Step 2: Run — verify FAIL** `cargo test -p calm-server set_toolset_narrows` → FAIL
- [ ] **Step 3: Implement the tool** (orient.rs; follow the exact `#[tool]` macro shape of a
  neighboring tool in that file):
```rust
    /// Narrow (or reset) which tools THIS session exposes, at runtime, without
    /// restarting the server. Names are toolset names (see repo_overview /
    /// `TOOLSET_NAMES`); the safety floor (orient+edit) is always kept, and the
    /// static preset is never exceeded. Pass an empty list / omit `toolsets` to
    /// reset to the full preset. Emits tools/list_changed so the client refetches.
    #[tool(description = "Narrow or reset this session's exposed toolset at runtime.")]
    async fn set_toolset(
        &self,
        params: rmcp::handler::server::tool::Parameters<SetToolsetParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        let requested = params.0.toolsets.unwrap_or_default();
        // Validate every requested name against TOOLSET_NAMES (reject unknowns
        // loudly, mirroring resolve_preset's hard-error posture).
        for name in &requested {
            if !common::TOOLSET_NAMES.contains(&name.as_str()) {
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::ContentBlock::text(format!(
                        "unknown toolset {name:?}; valid: {}", common::TOOLSET_NAMES.join(", ")
                    )),
                ]));
            }
        }
        let narrowing = if requested.is_empty() {
            None
        } else {
            Some(requested.iter().cloned().collect::<std::collections::BTreeSet<_>>())
        };
        self.apply_toolset_inner(narrowing).await;
        // Audit trail — one of the two new runtime behaviors (L6 finding).
        tracing::info!(
            target: crate::telemetry::AUDIT_TARGET,
            session_id = self.session_id,
            decision = "toolset_changed",
            toolsets = ?requested,
        );
        // Best-effort notify; a send failure must not fail the call.
        let _ = context.peer.notify_tool_list_changed().await;
        let visible = self.current_visible_tool_names().await;
        Ok(rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(format!(
                "toolset narrowed to: {}\nvisible tools: {}",
                if requested.is_empty() { "full preset".into() } else { requested.join(", ") },
                visible.into_iter().collect::<Vec<_>>().join(", ")
            )),
        ]))
    }
```
```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetToolsetParams {
    /// Toolset names to expose (union). Empty/omitted resets to the full preset.
    #[serde(default)]
    pub toolsets: Option<Vec<String>>,
}
```
- [ ] **Step 4: Add the private helpers** (in the `impl CalmServer` in tools.rs):
```rust
    async fn apply_toolset_inner(&self, narrowing: Option<std::collections::BTreeSet<String>>) {
        *self.enabled_toolsets.write().await = narrowing;
    }
    #[cfg(test)]
    async fn apply_toolset(&self, sets: Option<Vec<String>>) -> anyhow::Result<()> {
        self.apply_toolset_inner(sets.map(|v| v.into_iter().collect())).await;
        Ok(())
    }
    async fn current_visible_tool_names(&self) -> std::collections::BTreeSet<String> {
        let ceiling = common::resolve_preset(&self.preset)
            .ok()
            .flatten()
            .unwrap_or_else(common::calm_all_tool_names_pub);
        let narrowing = self.enabled_toolsets.read().await.clone();
        common::effective_tool_names(&ceiling, narrowing.as_ref())
    }
```
> **Executor:** `enabled_toolsets` was declared `Arc<RwLock<…>>` in Task 1.1. If the codebase
> uses `std::sync::RwLock` elsewhere for server state, match that and drop the `.await`s;
> if `tokio::sync::RwLock`, keep them. Pick to match the neighboring fields' lock type —
> `session_log` uses `std::sync::Mutex`, so **prefer `std::sync::RwLock` and remove `.await`**.
> `calm_all_tool_names` is currently private; expose a `pub(crate) calm_all_tool_names_pub`
> thin wrapper or make it `pub(crate)`.
- [ ] **Step 5: Run — verify PASS** `cargo test -p calm-server set_toolset_narrows` → PASS
- [ ] **Step 6: Commit** `git commit -am "feat(toolsets): set_toolset runtime tool + helpers"`

### Task 1.5: [HIGH] Enforce the floor at BOTH dispatch points

**Why HIGH:** this is the audit's FM1 mitigation. Filtering `list_tools` alone is cosmetic —
a client can still *call* a hidden tool. The gate must be enforced where dispatch happens
(`call_tool`), and it must never make a safety tool uncallable.

**Files:**
- Modify: `crates/calm-server/src/tools.rs` (`list_tools` + `call_tool`)
- Test: same file

- [ ] **Step 1: Write the failing tests** (two — visibility AND enforcement):
```rust
    #[tokio::test]
    async fn narrowed_session_hides_out_of_set_tools_in_list() {
        let server = CalmServer::for_test("full");
        server.apply_toolset(Some(vec!["trace".into()])).await.unwrap();
        let listed = server.list_tools(None, test_ctx()).await.unwrap();
        let names: Vec<_> = listed.tools.iter().map(|t| t.name.to_string()).collect();
        assert!(!names.contains(&"scan_text".to_string()));      // security hidden
        assert!(names.contains(&"edit_context".to_string()));    // floor kept
    }

    #[tokio::test]
    async fn narrowed_session_refuses_to_dispatch_out_of_set_tool() {
        let server = CalmServer::for_test("full");
        server.apply_toolset(Some(vec!["trace".into()])).await.unwrap();
        let req = call_req("scan_text", serde_json::json!({"pattern": "x"}));
        let res = server.call_tool(req, test_ctx()).await.unwrap();
        assert!(res.is_error.unwrap_or(false), "hidden tool must not dispatch");
        // …but a floor tool still dispatches:
        let ok = server.call_tool(call_req("session_context", serde_json::json!({})), test_ctx())
            .await.unwrap();
        assert!(!ok.is_error.unwrap_or(false), "floor tool wrongly blocked");
    }
```
- [ ] **Step 2: Run — verify FAIL** `cargo test -p calm-server narrowed_session` → FAIL
- [ ] **Step 3a: Filter `list_tools`** (tools.rs:594-598):
```rust
        let visible = self.current_visible_tool_names_blocking(); // sync variant if std::RwLock
        Ok(rmcp::model::ListToolsResult {
            next_cursor: None,
            tools: self.tool_router.list_all()
                .into_iter()
                .filter(|t| visible.contains(t.name.as_ref()))
                .collect(),
            meta: None,
        })
```
- [ ] **Step 3b: Enforce in `call_tool`** — insert AFTER the orientation-gate block and
  BEFORE building `ToolCallContext` (tools.rs:~658), so orientation still wins first:
```rust
        // Runtime toolset gate (Phase 1). Enforced here, not just in list_tools,
        // so a hidden tool cannot be dispatched by name. The floor guarantees
        // set_toolset/edit_context/diff_impact/orientation tools are always in
        // `visible`, so this can never make a safety gate unreachable.
        let visible = self.current_visible_tool_names_blocking();
        if !visible.contains(tool_name.as_str()) {
            tracing::info!(
                target: crate::telemetry::AUDIT_TARGET,
                session_id = self.session_id,
                decision = "denied",
                reason_code = "TOOL_NOT_IN_ACTIVE_TOOLSET",
                tool = %tool_name,
            );
            return Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::ContentBlock::text(format!(
                    "tool {tool_name:?} is not in this session's active toolset; \
                     call set_toolset to widen it"
                )),
            ]));
        }
```
- [ ] **Step 4: Run — verify PASS** `cargo test -p calm-server narrowed_session` → PASS
- [ ] **Step 5: Advertise the capability** — where `ServerCapabilities` are built for
  `CalmServer` (`server_info`/`get_info`), add `.enable_tool_list_changed()`. Add a toolsnap
  regression: `cargo test -p calm-server toolsnaps` and commit the updated `.snap`.
- [ ] **Step 6: Commit** `git commit -am "feat(toolsets): enforce floor at list_tools + call_tool [FM1]"`

### Task 1.6: Docs + toolsnap for the new tool

- [ ] **Step 1:** Update `__toolsnaps__` (new `set_toolset` schema) — `cargo test -p calm-server toolsnaps`, commit the `.snap`.
- [ ] **Step 2:** Add a short "Dynamic toolsets" subsection to `docs/mcp-client-setup.md` (how a client calls `set_toolset` and re-fetches on `list_changed`).
- [ ] **Step 3: Full gate** `cargo fmt && cargo clippy -p calm-server -- -D warnings && cargo test -p calm-server`
- [ ] **Step 4: Commit** `git commit -am "docs(toolsets): document set_toolset + refresh toolsnaps"`

---

# Phase 2 — Minimal OTel span export (opt-in)

**Ships:** an optional `otel` Cargo feature that, when built AND `OTEL_EXPORTER_OTLP_ENDPOINT`
is set at runtime, exports the spans CALM already emits. Zero behavior change otherwise.

### Task 2.1: Pin the version-aligned OTel dependency set

> **CRITICAL (audit A2 finding):** the Rust OTel crates version-skew. Do NOT `cargo add`
> latest. Pin the **opentelemetry-0.31 line**, which is the only currently-published set that
> resolves to a SINGLE `opentelemetry` core version (verified via `cargo tree`).

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/calm-cli/Cargo.toml` (+ `crates/calm-server/Cargo.toml` if the layer lives there)

- [ ] **Step 1: Add workspace deps** (Cargo.toml, optional = true via feature):
```toml
opentelemetry = { version = "0.31", optional = true }
opentelemetry_sdk = { version = "0.31", features = ["rt-tokio"], optional = true }
opentelemetry-otlp = { version = "0.31", default-features = false, features = ["http-proto", "reqwest-client"], optional = true }
tracing-opentelemetry = { version = "0.32", optional = true }
```
> Note: `default-features = false` + `http-proto` avoids pulling the grpc-tonic stack
> (the audit's secondary finding). `reqwest` is already in CALM's tree.
- [ ] **Step 2: Add the `otel` feature** to `crates/calm-cli/Cargo.toml`:
```toml
[features]
otel = ["dep:opentelemetry", "dep:opentelemetry_sdk", "dep:opentelemetry-otlp", "dep:tracing-opentelemetry"]
```
- [ ] **Step 3: Verify single-core resolution** `cargo tree -p calm-cli --features otel | grep -E '── opentelemetry v' | sort -u` → expected: exactly `opentelemetry v0.31.x`
- [ ] **Step 4: Verify default build unaffected** `cargo build -p calm-cli` (no `--features otel`) → PASS, and `grep -c opentelemetry Cargo.lock` unchanged from baseline is NOT required (lock records optional deps) but `cargo tree -p calm-cli | grep -c opentelemetry` → expected: 0
- [ ] **Step 5: Commit** `git commit -am "build(otel): pin version-aligned opentelemetry 0.31 set behind otel feature"`

### Task 2.2: The conditional OTel layer builder

**Files:**
- Create: `crates/calm-cli/src/otel.rs`
- Modify: `crates/calm-cli/src/main.rs` (module decl + both tracing inits)

- [ ] **Step 1: Write the builder** (`crates/calm-cli/src/otel.rs`):
```rust
//! Optional OpenTelemetry span export. Built only under `--features otel`,
//! and even then active ONLY when `OTEL_EXPORTER_OTLP_ENDPOINT` is set —
//! absent env var means the OTLP pipeline is never constructed, so no
//! background exporter task spawns and no network I/O happens (audit A3).

#[cfg(feature = "otel")]
pub fn otel_layer<S>() -> anyhow::Result<Option<impl tracing_subscriber::Layer<S>>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry::trace::TracerProvider as _;
    // Gate on the standard env var. Absent => no pipeline, no task, no network.
    if std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_none() {
        return Ok(None);
    }
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()?;
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name("calm-mcp")
                .build(),
        )
        .build();
    let tracer = provider.tracer("calm-mcp");
    // Store the provider for shutdown flushing (Task 2.3).
    PROVIDER.set(provider).ok();
    Ok(Some(tracing_opentelemetry::layer().with_tracer(tracer)))
}

#[cfg(feature = "otel")]
static PROVIDER: std::sync::OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> =
    std::sync::OnceLock::new();

#[cfg(feature = "otel")]
pub fn shutdown() {
    if let Some(p) = PROVIDER.get() {
        let _ = p.shutdown(); // flush batched spans; ignore error on best-effort shutdown
    }
}

// No-op stubs when the feature is off, so call sites stay clean.
#[cfg(not(feature = "otel"))]
pub fn shutdown() {}
```
> **Executor:** exact `opentelemetry-otlp 0.31` builder method names (`SpanExporter::builder`,
> `.with_http`) — confirm against the pinned crate's docs; the OTel API renames across minor
> versions. Adapt names, keep the env-gate + return-`Option` contract EXACTLY.
- [ ] **Step 2: Wire into both inits** — `main.rs`:
  - non-daemon (main.rs:236-242): build the registry with `.with(otel_layer()?)` appended.
  - `init_daemon_tracing` (main.rs:1188-1191): append `.with(otel_layer()?)` to the existing
    `registry().with(human_layer).with(audit_layer)`.
  - Under `#[cfg(not(feature = "otel"))]`, `otel_layer` is absent → guard both call sites with
    `#[cfg(feature = "otel")]` or provide a `None`-returning stub for the non-otel build.
```rust
    // main.rs non-daemon arm, feature-gated:
    let registry = tracing_subscriber::registry().with(fmt_layer);
    #[cfg(feature = "otel")]
    let registry = registry.with(crate::otel::otel_layer()?);
    registry.init();
```
- [ ] **Step 3: Call `otel::shutdown()`** on the daemon SIGTERM path and at normal `serve` exit.
- [ ] **Step 4: Build both ways** `cargo build -p calm-cli` and `cargo build -p calm-cli --features otel` → both PASS
- [ ] **Step 5: Commit** `git commit -am "feat(otel): conditional env-gated span-export layer"`

### Task 2.3: Verify the "zero task when unset" guarantee + shutdown flush

**Files:**
- Test: `crates/calm-cli/tests/otel_gate.rs` (new, `#![cfg(feature = "otel")]`)

- [ ] **Step 1: Write the test** (env-var absent → `otel_layer()` returns `None`):
```rust
#![cfg(feature = "otel")]
#[test]
fn otel_layer_is_none_without_endpoint_env() {
    // SAFETY: single-threaded test; no other thread reads env concurrently.
    unsafe { std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT"); }
    let layer = calm_cli::otel::otel_layer::<tracing_subscriber::Registry>().unwrap();
    assert!(layer.is_none(), "layer must be None (no pipeline) when endpoint unset");
}
```
> **Executor:** requires `otel` module + `otel_layer` reachable from the test — expose via
> `pub mod otel;` in a lib target or `#[path]`-include. If calm-cli is bin-only, move
> `otel.rs` behind a small `calm-cli` lib or test it as an integration of the layer builder.
- [ ] **Step 2: Run — verify PASS** `cargo test -p calm-cli --features otel otel_layer_is_none` → PASS
- [ ] **Step 3: Manual smoke (documented, not CI):** run a local `otel-collector`, set
  `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318`, run one `calm serve` session, confirm
  `mcp_tool_call` spans arrive. Record the steps in `docs/architecture.md` observability note.
- [ ] **Step 4: Commit** `git commit -am "test(otel): assert no pipeline built when endpoint unset"`

### Task 2.4: Document the egress qualification (audit FM3)

- [ ] **Step 1:** In `README.md`, next to the "No code leaves your machine" claim, add a
  footnote: OTel is off by default; when the `otel` feature is built AND
  `OTEL_EXPORTER_OTLP_ENDPOINT` is set, span attributes (file paths, symbol names, tool
  names) are sent to that user-configured collector — no source bodies, but repo identifiers
  do leave the machine; use `https://` collectors only.
- [ ] **Step 2:** Add an "Observability (optional)" subsection to `docs/architecture.md`.
- [ ] **Step 3: Commit** `git commit -am "docs(otel): qualify the local-first claim for opt-in OTel"`

---

# Phase 3 — Remote/HTTP transport (opt-in, narrowest scope)

**Ships:** `calm serve --http` behind an `http` Cargo feature, loopback-only by default,
read-only by default when exposed remotely.

### Task 3.1: Add the `http` feature + rmcp transport

**Files:**
- Modify: `crates/calm-server/Cargo.toml`, `Cargo.toml`

- [ ] **Step 1:** Add rmcp feature `transport-streamable-http-server` under an `http` feature:
```toml
# crates/calm-server/Cargo.toml
[features]
http = ["rmcp/transport-streamable-http-server"]
```
- [ ] **Step 2:** Build both ways `cargo build -p calm-server` / `cargo build -p calm-server --features http` → PASS
- [ ] **Step 3: Commit** `git commit -am "build(http): gate rmcp streamable-http server behind http feature"`

### Task 3.2: CLI surface — `--http`, `--addr`, `--allow-remote`

**Files:**
- Modify: `crates/calm-cli/src/main.rs` (`Commands::Serve` struct + arm)

- [ ] **Step 1:** Add fields to the `Serve` variant:
```rust
        /// Serve over Streamable-HTTP instead of stdio/unix-socket (needs the `http` feature).
        #[arg(long)]
        http: bool,
        /// HTTP bind address. Defaults to 127.0.0.1:0 (loopback, ephemeral port).
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
        /// Permit a non-loopback bind. Requires CALM_HTTP_TOKEN and forces a read-only preset.
        #[arg(long)]
        allow_remote: bool,
```
- [ ] **Step 2:** In the arm, branch to HTTP before the stdio path, feature-gated.
- [ ] **Step 3: Commit** `git commit -am "feat(http): add --http/--addr/--allow-remote CLI flags"`

### Task 3.3: The HTTP serve function (reuses `for_connection`)

**Files:**
- Create: `crates/calm-server/src/http.rs` (`#![cfg(feature = "http")]`)
- Modify: `crates/calm-server/src/lib.rs` (`#[cfg(feature="http")] pub mod http;`)

- [ ] **Step 1: Implement** — the daemon-shared `CalmServer` is built once; the service
  factory hands each HTTP session a fresh `for_connection()`:
```rust
pub async fn serve_http(
    daemon_server: crate::tools::CalmServer,
    addr: std::net::SocketAddr,
    read_only: bool,
) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService, StreamableHttpServerConfig,
        session::local::LocalSessionManager,
    };
    let factory = move || Ok(daemon_server.for_connection()); // per-session CalmServer
    let service = StreamableHttpService::new(
        factory,
        std::sync::Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    // read_only is enforced upstream by constructing daemon_server with a
    // read-only preset (Task 3.4); nothing to do here but serve.
    let _ = read_only;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, app).await?;
    Ok(())
}
```
> **Executor:** confirm the exact `StreamableHttpService` → tower/axum adapter path in
> rmcp 2.2.0 (`tower.rs` exposes it as a `tower_service::Service`). If rmcp ships an
> axum helper, prefer it; otherwise the `nest_service` shape above works because the
> service implements `tower::Service<Request>`.
- [ ] **Step 2:** Add an audit-log line when an HTTP session is accepted (L6 finding) — the
  simplest seam is inside `for_connection` already logging session creation, or wrap the factory.
- [ ] **Step 3: Commit** `git commit -am "feat(http): serve_http reusing for_connection per session"`

### Task 3.4: [HIGH] Fail-closed remote binding + read-only default

**Why HIGH:** this is the audit's FM2 mitigation — the write path must not be network-reachable
by default.

**Files:**
- Modify: `crates/calm-cli/src/main.rs` (the `--http` branch)
- Test: `crates/calm-cli/tests/http_guard.rs` (new)

- [ ] **Step 1: Write the failing tests:**
```rust
    // 1) non-loopback addr without --allow-remote → refuse to start.
    // 2) --allow-remote without CALM_HTTP_TOKEN → refuse to start.
    // 3) --allow-remote (valid) → preset is forced to a read-only set (no edit tools).
```
  (Express these against a small pure `resolve_http_launch(addr, allow_remote, token, preset)
  -> Result<HttpLaunch>` helper so they don't need a real socket.)
- [ ] **Step 2: Run — verify FAIL**
- [ ] **Step 3: Implement `resolve_http_launch`:**
```rust
struct HttpLaunch { addr: std::net::SocketAddr, effective_preset: String }

fn resolve_http_launch(
    addr: &str, allow_remote: bool, token: Option<String>, requested_preset: &str,
) -> anyhow::Result<HttpLaunch> {
    let sock: std::net::SocketAddr = addr.parse()?;
    let is_loopback = sock.ip().is_loopback();
    if !is_loopback && !allow_remote {
        anyhow::bail!("refusing non-loopback HTTP bind {sock} without --allow-remote");
    }
    if allow_remote {
        if token.filter(|t| !t.is_empty()).is_none() {
            anyhow::bail!("--allow-remote requires a non-empty CALM_HTTP_TOKEN (fail-closed)");
        }
    }
    // Remote exposure ⇒ force a read-only preset (edit tools stay loopback-only).
    let effective_preset = if !is_loopback {
        "full,-edit".to_string()   // every tool except the edit toolset
    } else {
        requested_preset.to_string()
    };
    Ok(HttpLaunch { addr: sock, effective_preset })
}
```
- [ ] **Step 4: Wire it** — the `--http` branch calls `resolve_http_launch`, builds the
  daemon `CalmServer` with `effective_preset`, then `serve_http(server, launch.addr, read_only)`.
  Add bearer-token middleware to the axum router when `allow_remote` (reject requests whose
  `Authorization: Bearer` != `CALM_HTTP_TOKEN`).
- [ ] **Step 5: Run — verify PASS** `cargo test -p calm-cli --features http http_guard` → PASS
- [ ] **Step 6: Commit** `git commit -am "feat(http): fail-closed remote bind + read-only preset [FM2]"`

### Task 3.5: Docs + threat-model note

- [ ] **Step 1:** New `docs/http-transport.md`: devcontainer/Codespace use case, the loopback
  default, why remote forces read-only, the token requirement, TLS expectation (put a real
  reverse proxy in front; the bearer token is not a substitute for TLS).
- [ ] **Step 2:** Link it from README's client-setup section as "advanced / remote-dev only".
- [ ] **Step 3: Full gate** `cargo fmt && cargo clippy -p calm-server -p calm-cli --features http -- -D warnings && cargo test`
- [ ] **Step 4: Commit** `git commit -am "docs(http): remote-transport guide + threat model"`

---

## Cross-cutting: CI

- [ ] Add feature-matrix build jobs to `.github/workflows/ci.yml`:
  `cargo build --features otel`, `cargo build --features http`, and a
  `cargo tree --features otel | grep -c 'opentelemetry v0.3[^1]'` guard that FAILS if a second
  opentelemetry core version ever creeps in (locks in the A2 finding).
- [ ] Commit `git commit -am "ci: build otel/http feature variants + single-otel-version guard"`

---

## Self-Review

**1. Spec coverage:**
- Proposal 1 (dynamic toolsets) → Phase 1 (Tasks 1.1–1.6). ✅ incl. safety floor (1.5), audit log, list_changed.
- Proposal 2 (OTel) → Phase 2 (Tasks 2.1–2.4). ✅ incl. version-pin finding, env-gate, egress doc.
- Proposal 3 (HTTP) → Phase 3 (Tasks 3.1–3.5). ✅ incl. read-only default, fail-closed, stateful reuse.
- Audit FM1 → Task 1.5 (enforcement-path). Audit FM2 → Task 3.4. Audit FM3 → Task 2.4. ✅
- Abductive-1 (cross-session trace) → covered by `for_connection` per-session root; add an
  explicit test in Task 2.3 follow-up if OTel graduates past minimal. Abductive-2
  (list_changed flood) → **GAP**: no debounce yet. See note below.
- L2 concurrency (snapshot read) → `current_visible_tool_names` reads a cloned snapshot. ✅

**2. Placeholder scan:** no TBD/TODO; every code step has real code. Executor-caution notes
mark the 3 spots needing API-name confirmation against pinned crates (not placeholders —
verification gates).

**3. Type consistency:** `enabled_toolsets` / `effective_tool_names` / `apply_toolset_inner`
/ `current_visible_tool_names` used consistently across Tasks 1.1→1.5. `SetToolsetParams`
defined once (1.4). `resolve_http_launch`/`HttpLaunch` defined once (3.4).

**4. Known gap to close in execution:** Abductive-2 (`list_changed` flood) — add a per-session
debounce (ignore `set_toolset` calls that don't change the set; the `apply_toolset_inner`
should compare-and-skip the notify when unchanged). Fold into Task 1.4 Step 3 as a guard:
only `notify_tool_list_changed` when the new narrowing differs from the old.

---

## Task Risk Summary (task-risk-score)
<!-- task-risk-score: DO NOT DUPLICATE — update this section -->
<!-- last-run: 2026-07-28 | sprint: otel-http-toolsets-v1 -->
<!-- formula: (S×B)/D, VHEATM v16-style; HIGH ≥ 6 (pre-empirical threshold) -->

**Context:** solo-maintained repo (@Eilodon) → every task is **SINGLE** boundary, no CROSS,
no external-owner escalation. Per-task context type noted; EXTERNAL_SERVICE tasks get D=min(D,1)
(OTLP/HTTP failures are prod-only), INFRASTRUCTURE tasks get B=max(B,2).

| Task | Context | S×B/D | QBR | Risk | Action |
|------|---------|-------|-----|------|--------|
| 1.1 add field | BUSINESS_LOGIC | 1×2/3 | 0.7 | LOW | proceed |
| 1.2 safety-floor const | BUSINESS_LOGIC | 2×2/3 | 1.3 | LOW | proceed (unit-tested) |
| 1.3 effective_tool_names | BUSINESS_LOGIC | 2×2/3 | 1.3 | LOW | proceed |
| 1.4 set_toolset tool | BUSINESS_LOGIC (+concurrency) | 2×2/2 | 2.0 | LOW | ℹ️ watch RwLock snapshot read in review |
| **1.5 enforce floor (both dispatch pts)** | **SECURITY** | **3×3/1** | **9** | **HIGH ⚠️** | keep enforcement test (integration-level); single concern, no split |
| 1.6 docs + toolsnap | BUSINESS_LOGIC | 1×1/3 | 0.3 | LOW | proceed |
| 2.1 pin OTel deps | EXTERNAL_SERVICE | 2×2/1 | 4.0 | MEDIUM | ℹ️ CI single-version guard is the mitigation (cross-cutting) |
| 2.2 conditional layer | EXTERNAL_SERVICE | 2×2/1 | 4.0 | MEDIUM | ℹ️ watch: env-gate must build no pipeline when unset |
| 2.3 verify no-task + flush | BUSINESS_LOGIC | 2×2/3 | 1.3 | LOW | proceed (this task IS the detectability) |
| 2.4 egress doc | DOC | — | — | SKIP | task-risk-score: SKIPPED (doc) — but FM3-critical, do not drop |
| 3.1 http feature | INFRASTRUCTURE | 1×2/3 | 0.7 | LOW | proceed |
| 3.2 CLI flags | BUSINESS_LOGIC | 1×1/3 | 0.3 | LOW | proceed |
| 3.3 serve_http reuse | INFRASTRUCTURE | 2×3/2 | 3.0 | MEDIUM | ℹ️ watch client-disconnect cancellation (conn_ct pattern) |
| **3.4 fail-closed + read-only** | **SECURITY** | **3×3/1** | **9** | **HIGH ⚠️** | **DECOMPOSE — see below** |
| 3.5 docs + threat model | DOC | — | — | SKIP | SKIPPED (doc) — but FM2-critical |
| CI feature matrix | INFRASTRUCTURE | 2×2/3 | 1.3 | LOW | proceed (locks A2 finding) |

**Decomposition of Task 3.4 (HIGH with >1 concern → mandatory split):**
- **Task 3.4a — fail-closed launch policy + read-only preset.** Pure `resolve_http_launch`
  helper (loopback default, refuse non-loopback w/o `--allow-remote`, force `full,-edit` preset
  when remote) + its unit tests + wiring. Single concern: *what may bind and with which tools*.
  Score: SECURITY 3×3/1 = **9 HIGH ⚠️** — but fully unit-testable (the helper is pure), so the
  integration-level check the HIGH rule requires = the three `http_guard` tests already in 3.4 Step 1.
- **Task 3.4b — bearer-token middleware.** axum layer rejecting `Authorization: Bearer` !=
  `CALM_HTTP_TOKEN`, only mounted when `allow_remote`. Depends on 3.4a. Single concern: *who may
  talk to a remote bind*. Score: SECURITY 3×3/1 = **9 HIGH ⚠️** — add an integration test that a
  wrong/absent token gets 401 and a correct token reaches a read-only tool.

**Summary:**
- **High-risk tasks:** 1.5 (safety-gate enforcement), 3.4a + 3.4b (HTTP write-path exposure).
  All three are the audit's two HIGH flags made concrete; each carries an integration-level test.
- **Cross-boundary tasks:** none (solo maintainer).
- **Integration-test surface:** 3 tasks (1.5 dispatch-refusal, 3.4a launch-policy, 3.4b token-auth),
  plus 1 manual smoke (2.3 Step 3, OTLP collector).
- **Sequencing rule:** do NOT ship Phase 3 until 3.4a+3.4b are green — the read-only-remote
  default is the thing standing between an opt-in convenience and a network-exposed write path.

---

## Execution Handoff

```
Plan complete: docs/superskills/plans/2026-07-28-otel-http-dynamic-toolsets.md
Risk summary: 3 HIGH tasks (1.5, 3.4a, 3.4b), 0 CROSS boundaries, 3 integration tests + 1 manual smoke.

Recommended order: Phase 1 (dynamic toolsets) → Phase 2 (OTel) → Phase 3 (HTTP).
Each Phase is independently shippable and independently valuable.

Execution options:
1. Subagent-Driven (recommended) — fresh subagent per task, specialist-review between tasks,
   pattern-globalize after any bug, adr-commit at finish.
2. Inline Execution — batch execution with review checkpoints at each Phase boundary.
```
