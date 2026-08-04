//! Integration tests for the `calm guard` CLI command (P2, KNOWN_LIMITATIONS.md
//! "No Git/CI-native integration path"): drives the real `calm` binary against
//! a real git repo + index, the same way a pre-commit hook or CI step would.

use std::path::Path;
use std::process::Command;

fn calm_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_calm"))
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("running git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

const INITIAL_SOURCE: &str = "pub fn helper(x: i32) -> i32 {\n    x + 1\n}\n\npub fn caller_one() -> i32 {\n    helper(1)\n}\n\npub fn caller_two() -> i32 {\n    helper(2)\n}\n\npub fn caller_three() -> i32 {\n    helper(3)\n}\n";

/// A tiny real Cargo package (own repo, own `.calm/`/`target/` gitignored)
/// with one function (`helper`) called from 3 others -- enough fan-in for
/// `diff_impact`'s own risk classification to matter.
fn init_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"guard_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), INITIAL_SOURCE).unwrap();
    std::fs::write(dir.path().join(".gitignore"), ".calm/\ntarget/\n").unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "t@t.com"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn run_index(root: &Path) {
    let out = Command::new(calm_bin())
        .args(["index", "--project-root"])
        .arg(root)
        .output()
        .expect("running calm index");
    assert!(
        out.status.success(),
        "calm index failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_guard(root: &Path, extra_args: &[&str]) -> std::process::Output {
    Command::new(calm_bin())
        .args(["guard", "--project-root"])
        .arg(root)
        .args(extra_args)
        .output()
        .expect("running calm guard")
}

#[test]
fn calm_guard_blocks_on_a_high_risk_signature_change() {
    let dir = init_fixture();
    run_index(dir.path());

    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn helper(x: i32, y: i32) -> i32 {\n    x + y\n}\n\npub fn caller_one() -> i32 {\n    helper(1, 0)\n}\n\npub fn caller_two() -> i32 {\n    helper(2, 0)\n}\n\npub fn caller_three() -> i32 {\n    helper(3, 0)\n}\n",
    )
    .unwrap();
    git(dir.path(), &["add", "-A"]);

    let out = run_guard(dir.path(), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected calm guard to exit 1 on a signature change to a 3-caller function: \
         stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("high") && stdout.contains("helper"),
        "expected the report to name the high-risk symbol: stdout={stdout}"
    );
}

#[test]
fn calm_guard_passes_on_a_non_breaking_change() {
    let dir = init_fixture();
    run_index(dir.path());

    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn helper(x: i32) -> i32 {\n    x + 1 // body-only change, same signature\n}\n\npub fn caller_one() -> i32 {\n    helper(1)\n}\n\npub fn caller_two() -> i32 {\n    helper(2)\n}\n\npub fn caller_three() -> i32 {\n    helper(3)\n}\n",
    )
    .unwrap();
    git(dir.path(), &["add", "-A"]);

    let out = run_guard(dir.path(), &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected calm guard (default --fail-on high) to pass a body-only, \
         non-signature change: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn calm_guard_json_flag_emits_parseable_output() {
    let dir = init_fixture();
    run_index(dir.path());

    std::fs::write(dir.path().join("src/extra.rs"), "// new file, no symbols\n").unwrap();
    git(dir.path(), &["add", "-A"]);

    let out = run_guard(dir.path(), &["--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json output was not valid JSON ({e}): {stdout}"));
    assert!(
        parsed.get("aggregate_risk").is_some(),
        "expected the raw diff_impact JSON shape: {parsed}"
    );
}

#[test]
fn calm_guard_rejects_an_invalid_fail_on_value() {
    let dir = init_fixture();
    run_index(dir.path());

    let out = run_guard(dir.path(), &["--fail-on", "critical"]);
    assert!(
        !out.status.success(),
        "expected an invalid --fail-on value to fail, not silently do something else"
    );
}
