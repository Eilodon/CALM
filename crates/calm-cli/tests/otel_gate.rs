//! [Task 2.3] Proves the audit A3 guarantee: even when built with the
//! `otel` feature, no OTLP pipeline (no exporter, no tracer provider, no
//! background export task) is ever constructed unless
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set at runtime. This is the one
//! observable, testable half of "no code leaves your machine unless you
//! opted in" -- the other half (spans actually reaching a collector when
//! the endpoint IS set) is a manual smoke test, not something a unit test
//! can verify without a real network listener.
#![cfg(feature = "otel")]

#[test]
fn otel_layer_is_none_without_endpoint_env() {
    // SAFETY: this process/test binary is single-threaded for this env var
    // (no other test in this file or thread reads/writes it concurrently);
    // `cargo test` on rust 2024 edition requires `unsafe` here since
    // std::env::remove_var can race other threads mutating the process
    // environment in general, not specific to this test.
    unsafe {
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }
    let layer = calm_cli::otel::otel_layer::<tracing_subscriber::Registry>().unwrap();
    assert!(
        layer.is_none(),
        "layer must be None (no pipeline built) when OTEL_EXPORTER_OTLP_ENDPOINT is unset"
    );
}
