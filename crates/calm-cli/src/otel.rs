//! Optional OpenTelemetry span export. Built only under `--features otel`,
//! and even then active ONLY when `OTEL_EXPORTER_OTLP_ENDPOINT` is set —
//! absent env var means the OTLP pipeline is never constructed, so no
//! background exporter task spawns and no network I/O happens (audit A3).
//!
//! Version note (re-verify before bumping): pinned to opentelemetry/
//! opentelemetry_sdk/opentelemetry-otlp 0.31 + tracing-opentelemetry 0.32,
//! the only combination that resolves to a single `opentelemetry` core as
//! of 2026-07-28 (see root Cargo.toml's comment on these deps for the
//! live-verified failure mode a bare `cargo add` to latest hits instead).
//! Check with `cargo tree -p opentelemetry` (expect exactly one line).

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
    // Store the provider for shutdown flushing (`shutdown` below) — the
    // caller (main.rs) doesn't hold onto the provider itself, only calls
    // `otel::shutdown()` on its own exit paths (daemon SIGTERM, normal
    // `serve` return).
    PROVIDER.set(provider).ok();
    Ok(Some(tracing_opentelemetry::layer().with_tracer(tracer)))
}

#[cfg(feature = "otel")]
static PROVIDER: std::sync::OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> =
    std::sync::OnceLock::new();

/// Flush any batched-but-unsent spans before the process exits. Best-effort
/// (a flush failure here shouldn't block or fail shutdown) — a no-op if
/// `otel_layer` was never called or returned `None` (endpoint unset).
#[cfg(feature = "otel")]
pub fn shutdown() {
    if let Some(p) = PROVIDER.get() {
        let _ = p.shutdown();
    }
}

// No-op stub when the feature is off, so call sites in main.rs stay clean
// (unconditional `otel::shutdown()`, no `#[cfg(feature = "otel")]` needed
// at every call site).
#[cfg(not(feature = "otel"))]
pub fn shutdown() {}
