//! Integration tests for `.calm/` permission hardening: `calm init` must
//! create `.calm/` at 0700 (not the umask-derived default a plain
//! `create_dir_all` would leave it at), and `calm doctor --fix` must be
//! able to retroactively tighten a `.calm/` that ended up loose anyway
//! (an old checkout, a directory created by something else, a permissive
//! umask on a platform without the atomic-0700 helper at the time).
//!
//! Spawns the real built `calm` binary, matching `hooks_doctor_fix.rs`'s
//! posture: the guarantee under test is the actual CLI wiring end to end,
//! not just the in-process helper functions.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
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

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn calm_init_creates_calm_dir_at_0700_regardless_of_umask() {
    let dir = fresh_project();
    let root = dir.path();

    let out = run_calm(root, &["init"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let calm_dir = root.join(".calm");
    assert!(calm_dir.is_dir(), ".calm/ must exist after init");
    assert_eq!(
        mode_of(&calm_dir),
        0o700,
        ".calm/ must be created 0700 by `calm init`, not left at the umask default"
    );
}

#[test]
fn doctor_without_fix_reports_loose_permissions_but_does_not_change_them() {
    let dir = fresh_project();
    let root = dir.path();
    run_calm(root, &["init"]);

    let calm_dir = root.join(".calm");
    std::fs::set_permissions(&calm_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = run_calm(root, &["doctor"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("WANT 700"),
        "expected doctor to flag the loosened .calm/ mode, got: {text}"
    );

    assert_eq!(
        mode_of(&calm_dir),
        0o755,
        "plain `calm doctor` (no --fix) must never itself change permissions"
    );
}

#[test]
fn doctor_fix_tightens_a_loosened_calm_dir_and_reports_fixed() {
    let dir = fresh_project();
    let root = dir.path();
    run_calm(root, &["init"]);

    let calm_dir = root.join(".calm");
    std::fs::set_permissions(&calm_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(mode_of(&calm_dir), 0o755, "sanity: loosening took effect");

    let out = run_calm(root, &["doctor", "--fix"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("FIXED"),
        "expected doctor --fix to report the repair, got: {text}"
    );

    assert_eq!(
        mode_of(&calm_dir),
        0o700,
        "calm doctor --fix must tighten .calm/ back to 0700"
    );
}

#[test]
fn doctor_fix_is_a_noop_on_already_correct_permissions() {
    let dir = fresh_project();
    let root = dir.path();
    run_calm(root, &["init"]);
    let calm_dir = root.join(".calm");
    assert_eq!(mode_of(&calm_dir), 0o700, "sanity: init already hardens");

    // First --fix run also lazily creates .calm/index.db (a pre-existing,
    // unrelated `doctor()` behavior when no index has been built yet) at
    // the umask default and fixes it in the same pass -- not the "already
    // healthy" state this test wants. Settle that here so the SECOND run
    // below is the actual no-op under test.
    let settle = run_calm(root, &["doctor", "--fix"]);
    assert!(settle.status.success());

    let out = run_calm(root, &["doctor", "--fix"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("OK"),
        "expected an already-correct .calm/ to report OK, not FIXED: {text}"
    );
    assert!(
        !text.contains("FIXED"),
        "nothing should need fixing: {text}"
    );
    assert_eq!(mode_of(&calm_dir), 0o700);
}
