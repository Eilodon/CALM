//! [Task 3.4a / audit FM2] `resolve_http_launch` is the fail-closed launch
//! policy gating CALM's opt-in HTTP transport: what may bind, and with
//! which tools. Pure function, no socket, no process -- these are the
//! integration-level checks the plan's own risk table calls for on this
//! HIGH task (task-risk-score QBR=9, SECURITY).
#![cfg(feature = "http")]

#[test]
fn refuses_non_loopback_bind_without_allow_remote() {
    let result = calm_cli::http::resolve_http_launch("0.0.0.0:8787", false, None, "full");
    assert!(
        result.is_err(),
        "a non-loopback bind without --allow-remote must be refused (fail-closed default)"
    );
}

#[test]
fn refuses_allow_remote_without_a_token() {
    let no_token = calm_cli::http::resolve_http_launch("0.0.0.0:8787", true, None, "full");
    assert!(
        no_token.is_err(),
        "--allow-remote with no CALM_HTTP_TOKEN must be refused"
    );

    let empty_token =
        calm_cli::http::resolve_http_launch("0.0.0.0:8787", true, Some(String::new()), "full");
    assert!(
        empty_token.is_err(),
        "--allow-remote with an EMPTY CALM_HTTP_TOKEN must also be refused, not treated as set"
    );
}

#[test]
fn allow_remote_with_valid_token_forces_a_read_only_preset() {
    let launch = calm_cli::http::resolve_http_launch(
        "0.0.0.0:8787",
        true,
        Some("a-real-secret-token".to_string()),
        "full",
    )
    .expect("non-loopback + --allow-remote + a real token should be allowed to launch");
    assert_eq!(
        launch.effective_preset, "full,-edit",
        "remote exposure must force a read-only preset (no edit toolset), regardless of --preset"
    );
}

#[test]
fn loopback_bind_needs_no_token_and_keeps_the_requested_preset() {
    let launch = calm_cli::http::resolve_http_launch("127.0.0.1:8787", false, None, "full")
        .expect("a loopback bind without --allow-remote should succeed with no token needed");
    assert_eq!(
        launch.effective_preset, "full",
        "a loopback-only bind should NOT be forced read-only -- it's not network-exposed"
    );
}
