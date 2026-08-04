//! Opt-in remote/HTTP transport CLI-side policy (Phase 3, docs/superskills/
//! plans/2026-07-28-otel-http-dynamic-toolsets.md). Built only under
//! `--features http`. This module owns *policy* (what may bind, with which
//! tools) -- the actual HTTP server (`serve_http`) lives in
//! `calm_server::http`, which just serves whatever `SocketAddr`/preset this
//! module hands it.

/// Resolved launch decision: the address to bind, and the preset the bound
/// server should actually run with.
#[derive(Debug, PartialEq, Eq)]
pub struct HttpLaunch {
    pub addr: std::net::SocketAddr,
    pub effective_preset: String,
}

/// [Task 3.4a / audit FM2] Fail-closed launch policy for `calm serve --http`.
/// Pure function, no I/O -- unit/integration-testable without a real socket
/// (see `tests/http_guard.rs`).
///
/// Two independent gates, both fail-closed (refuse by default, not allow):
/// - A non-loopback `addr` is refused unless `allow_remote` is `true`.
/// - `allow_remote` additionally requires a non-empty `token` (from
///   `CALM_HTTP_TOKEN`) -- an empty string is treated the same as absent,
///   never silently accepted as "no auth needed".
///
/// Independent of both gates: any non-loopback bind forces the
/// capability-derived `"remote-safe"` preset (every tool with
/// `read_only_hint = true` -- see `calm_server::tools::common::
/// remote_safe_tool_names`), overriding `requested_preset` entirely.
/// Previously forced `"full,-edit"`, which only excludes the `edit`
/// toolset's 3 tools (edit_lines/edit_symbol/format_files) -- a real gap
/// closed 2026-08: `remember`, `verify_change`/`retry_maintenance`
/// (the latter spawns `cargo check`), `scip_refresh`/`lsp_refresh`
/// (external provider processes), `set_toolset`, and
/// `pattern_debt_register` are all state-mutating or process-executing
/// but sit outside the `edit` toolset, so `"full,-edit"` left every one of
/// them reachable over an authenticated remote connection despite the
/// "read-only" framing. `"remote-safe"` closes all of them at once by
/// filtering on the one annotation every tool already declares correctly
/// (`every_tool_declares_annotations` enforces non-optional declaration)
/// instead of enumerating toolsets by hand.
pub fn resolve_http_launch(
    addr: &str,
    allow_remote: bool,
    token: Option<String>,
    requested_preset: &str,
) -> anyhow::Result<HttpLaunch> {
    let sock: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --addr {addr:?}: {e}"))?;
    let is_loopback = sock.ip().is_loopback();
    if !is_loopback && !allow_remote {
        anyhow::bail!(
            "refusing non-loopback HTTP bind {sock} without --allow-remote \
             (fail-closed default -- see docs/http-transport.md)"
        );
    }
    if allow_remote {
        let has_real_token = token.as_deref().is_some_and(|t| !t.is_empty());
        if !has_real_token {
            anyhow::bail!(
                "--allow-remote requires a non-empty CALM_HTTP_TOKEN env var \
                 (fail-closed -- refusing to bind an unauthenticated remote socket)"
            );
        }
    }
    // Remote exposure forces read-only, independent of the loopback/token
    // checks above -- even a correctly-authenticated remote client never
    // gets the edit toolset by default.
    let effective_preset = if is_loopback {
        requested_preset.to_string()
    } else {
        "remote-safe".to_string()
    };
    Ok(HttpLaunch {
        addr: sock,
        effective_preset,
    })
}
