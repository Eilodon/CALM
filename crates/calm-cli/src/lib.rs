//! Small lib target alongside the `calm` bin — exists only so modules like
//! `otel`/`http` are reachable from integration tests (`tests/*.rs`)
//! without a hand-built `RequestContext`/process-spawn workaround. The bin
//! (`src/main.rs`) owns all real CLI/serve logic; nothing here is meant to
//! be a public API for other crates.

#[cfg(feature = "http")]
pub mod http;
pub mod otel;
