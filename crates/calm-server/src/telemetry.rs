pub fn timed_tool(name: &str, body: impl FnOnce() -> String) -> String {
    let start = std::time::Instant::now();
    let result = body();
    let elapsed = start.elapsed();
    tracing::info!(
        tool = name,
        duration_ms = elapsed.as_millis(),
        result_size = result.len(),
        "tool_execution_completed"
    );
    result
}
