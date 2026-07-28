//! Opt-in remote/HTTP transport (Phase 3, docs/superskills/plans/
//! 2026-07-28-otel-http-dynamic-toolsets.md). Built only under
//! `--features http`. Fail-closed launch policy (loopback default, preset
//! forced read-only when remote) lives in `calm-cli` (`resolve_http_launch`)
//! -- this module only knows how to serve, given an already-decided
//! address/preset; it makes no policy decisions of its own.

/// Serves `daemon_server` over Streamable-HTTP at `addr`. `daemon_server`
/// must already be the product of `crate::bootstrap()` (same as the
/// unix-socket daemon path, `daemon::serve_unix_daemon`) -- NOT a bare
/// `CalmServer::new_with_preset`, or the background indexer/embedder/
/// watcher never starts and the index never builds. `ct` is that same
/// bootstrap call's `CancellationToken`, wired into axum's graceful
/// shutdown so the SIGINT/SIGTERM handlers `bootstrap()` already installs
/// actually stop this server instead of it hanging forever ignoring them.
/// The service factory hands each new HTTP session a fresh
/// `for_connection()`, the same per-connection isolation the unix-socket
/// daemon's accept loop uses.
///
/// `require_bearer_token`: when `Some(token)`, every request must carry
/// `Authorization: Bearer <token>` or gets a 401 (Task 3.4b) -- `None` means
/// no auth layer is mounted at all, which is only safe for a loopback bind
/// (the CLI's `resolve_http_launch` is what actually enforces that a
/// non-loopback bind can't reach this function without a token already
/// resolved).
pub async fn serve_http(
    daemon_server: crate::tools::CalmServer,
    addr: std::net::SocketAddr,
    ct: tokio_util::sync::CancellationToken,
    require_bearer_token: Option<String>,
) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let factory = move || {
        // Audit trail (L6 finding) — the simplest seam available: this
        // closure runs once per new HTTP session (a fresh MCP `initialize`
        // handshake), same cardinality as a new unix-socket connection in
        // daemon.rs's accept loop. No per-request remote-IP plumbing here
        // (StreamableHttpService's factory signature doesn't carry the
        // request), so this logs session creation, not the peer address.
        tracing::info!(
            target: crate::telemetry::AUDIT_TARGET,
            decision = "http_session_accepted",
        );
        Ok(daemon_server.for_connection())
    };
    let service = StreamableHttpService::new(
        factory,
        std::sync::Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let mut app = axum::Router::new().nest_service("/mcp", service);
    if let Some(token) = require_bearer_token {
        app = app.layer(axum::middleware::from_fn(move |req, next| {
            let token = token.clone();
            async move { crate::http::require_bearer_token(token, req, next).await }
        }));
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Serving MCP over HTTP at http://{addr}/mcp");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;
    Ok(())
}

/// [Task 3.4b / audit FM2] Bearer-token auth middleware — only mounted by
/// `serve_http` when a token was resolved (i.e. only ever for a
/// non-loopback bind, per `resolve_http_launch`'s policy). Constant-time
/// comparison isn't used here deliberately: this is a coarse
/// remote-dev-only gate documented as "not a substitute for TLS"
/// (docs/http-transport.md), not a defense against a timing side-channel
/// attacker who can already observe response latency on the network path.
async fn require_bearer_token(
    token: String,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let provided = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match provided {
        Some(p) if p == token => next.run(req).await,
        _ => {
            tracing::info!(
                target: crate::telemetry::AUDIT_TARGET,
                decision = "denied",
                reason_code = "HTTP_BEARER_TOKEN_MISMATCH",
            );
            (
                axum::http::StatusCode::UNAUTHORIZED,
                "missing or invalid bearer token",
            )
                .into_response()
        }
    }
}
