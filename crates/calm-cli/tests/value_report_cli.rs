//! Integration tests for the `calm value-report` CLI command (P1 "value
//! report" ask from the 2026-08-05 CALM-improvements review): drives the
//! real `calm` binary against a synthetic `.calm/audit.log` in the exact
//! JSON-lines shape `init_daemon_tracing`'s `audit_layer` actually writes
//! (`{"fields":{"decision":...,"risk":...},"target":"calm_audit"}`).

use std::path::Path;
use std::process::Command;

fn calm_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_calm"))
}

fn init_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".calm")).unwrap();
    dir
}

fn audit_line(decision: &str, risk: Option<&str>) -> String {
    match risk {
        Some(r) => format!(
            r#"{{"timestamp":"2026-08-05T00:00:00Z","level":"INFO","fields":{{"session_id":1,"decision":"{decision}","risk":"{r}"}},"target":"calm_audit"}}"#
        ),
        None => format!(
            r#"{{"timestamp":"2026-08-05T00:00:00Z","level":"INFO","fields":{{"session_id":1,"decision":"{decision}"}},"target":"calm_audit"}}"#
        ),
    }
}

fn run_value_report(root: &Path, extra_args: &[&str]) -> std::process::Output {
    Command::new(calm_bin())
        .args(["value-report", "--project-root"])
        .arg(root)
        .args(extra_args)
        .output()
        .expect("running calm value-report")
}

#[test]
fn calm_value_report_reports_honestly_when_no_audit_log_exists() {
    let dir = init_fixture();
    let out = run_value_report(dir.path(), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "missing audit log must not be a hard error: {stdout}"
    );
    assert!(
        stdout.contains("no audit log found"),
        "expected an honest zero-data message, not a misleading empty report: {stdout}"
    );
}

#[test]
fn calm_value_report_counts_applied_denied_and_risk_from_a_synthetic_log() {
    let dir = init_fixture();
    let lines = [
        audit_line("applied", Some("low")),
        audit_line("applied", Some("high")),
        audit_line("denied", None),
        audit_line("elicit_asked", None),
        audit_line("elicit_declined", None),
    ]
    .join("\n");
    std::fs::write(dir.path().join(".calm/audit.log"), lines + "\n").unwrap();

    let out = run_value_report(dir.path(), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("applied: 2"), "stdout={stdout}");
    assert!(stdout.contains("denied"), "stdout={stdout}");
    assert!(
        stdout.contains("denied") && stdout.contains('1'),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains("1 asked, 0 approved, 1 declined"),
        "stdout={stdout}"
    );
}

#[test]
fn calm_value_report_json_flag_emits_parseable_json_with_exact_counts() {
    let dir = init_fixture();
    let lines = [
        audit_line("applied", Some("medium")),
        audit_line("applied", Some("medium")),
        audit_line("applied", Some("medium")),
        audit_line("denied", None),
        audit_line("denied", None),
    ]
    .join("\n");
    std::fs::write(dir.path().join(".calm/audit.log"), lines + "\n").unwrap();

    let out = run_value_report(dir.path(), &["--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json output was not valid JSON ({e}): {stdout}"));
    assert_eq!(parsed["applied"], 3);
    assert_eq!(parsed["denied"], 2);
    assert_eq!(parsed["risk_medium"], 3);
    assert_eq!(parsed["audit_log_found"], true);
}

#[test]
fn calm_value_report_ignores_lines_from_a_different_tracing_target() {
    let dir = init_fixture();
    let lines = [
        audit_line("applied", Some("low")),
        r#"{"timestamp":"2026-08-05T00:00:00Z","level":"INFO","fields":{"tool":"diff_impact","duration_ms":7},"target":"tool_execution_completed"}"#.to_string(),
    ]
    .join("\n");
    std::fs::write(dir.path().join(".calm/audit.log"), lines + "\n").unwrap();

    let out = run_value_report(dir.path(), &["--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        parsed["total_events"], 1,
        "only the calm_audit-target line should count: {stdout}"
    );
    assert_eq!(parsed["applied"], 1);
}

#[test]
fn calm_value_report_counts_malformed_lines_as_parse_errors_not_a_crash() {
    let dir = init_fixture();
    let lines = format!(
        "{}\nnot valid json at all\n",
        audit_line("applied", Some("low"))
    );
    std::fs::write(dir.path().join(".calm/audit.log"), lines).unwrap();

    let out = run_value_report(dir.path(), &["--json"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["parse_errors"], 1, "stdout={stdout}");
    assert_eq!(
        parsed["applied"], 1,
        "the one valid line before the bad one must still count: {stdout}"
    );
}
