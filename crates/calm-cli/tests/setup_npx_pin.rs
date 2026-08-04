//! Integration tests for `calm setup --npx`'s version-pinning: the
//! written MCP config entry must resolve to a specific npm version by
//! default (`@eilodon/calm-mcp@<this binary's own CARGO_PKG_VERSION>`),
//! not an unpinned `npx -y @eilodon/calm-mcp` that could silently resolve
//! to a different release on every cold `npx` invocation. `--track latest`
//! opts back into the old unpinned behavior.
//!
//! Spawns the real built `calm` binary, matching this test suite's other
//! CLI-wiring integration tests (`hooks_doctor_fix.rs`,
//! `permissions_doctor_fix.rs`).

use std::path::Path;
use std::process::Command;

fn calm_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_calm"))
}

fn fresh_project() -> tempfile::TempDir {
    tempfile::tempdir().expect("creating a tempdir for the test project")
}

fn run_calm(project_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(calm_bin())
        .args(args)
        .arg("--project-root")
        .arg(project_root)
        .output()
        .expect("spawning calm")
}

fn mcp_json_args(root: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(root.join(".mcp.json")).expect(".mcp.json written");
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["mcpServers"]["calm"]["args"]
        .as_array()
        .expect("args array present")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn setup_npx_default_pins_to_this_binarys_own_version() {
    let dir = fresh_project();
    let root = dir.path();

    let out = run_calm(root, &["setup", "--npx"]);
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let args = mcp_json_args(root);
    let expected_pkg = format!("@eilodon/calm-mcp@{}", env!("CARGO_PKG_VERSION"));
    assert!(
        args.contains(&expected_pkg),
        "expected default --npx to pin to {expected_pkg:?}, got args: {args:?}"
    );
}

#[test]
fn setup_npx_track_latest_writes_the_unpinned_package_name() {
    let dir = fresh_project();
    let root = dir.path();

    let out = run_calm(root, &["setup", "--npx", "--track", "latest"]);
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let args = mcp_json_args(root);
    assert!(
        args.contains(&"@eilodon/calm-mcp".to_string()),
        "expected --track latest to write the unpinned package name, got args: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a.starts_with("@eilodon/calm-mcp@")),
        "unpinned entry must not carry a version suffix, got args: {args:?}"
    );
}

#[test]
fn setup_npx_rejects_an_unknown_track_value() {
    let dir = fresh_project();
    let root = dir.path();

    let out = run_calm(root, &["setup", "--npx", "--track", "nonsense"]);
    assert!(
        !out.status.success(),
        "an unrecognized --track value must be rejected, not silently accepted"
    );
}
