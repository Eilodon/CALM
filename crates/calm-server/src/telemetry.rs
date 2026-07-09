pub fn timed_tool<T: serde::Serialize>(name: &str, body: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let result = body();
    let elapsed = start.elapsed();
    let result_size = serde_json::to_string(&result).map(|s| s.len()).unwrap_or(0);
    tracing::info!(
        tool = name,
        duration_ms = elapsed.as_millis(),
        result_size,
        "tool_execution_completed"
    );
    result
}