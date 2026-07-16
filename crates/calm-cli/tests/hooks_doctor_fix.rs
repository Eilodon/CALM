//! Integration test for `calm doctor --fix` — self-healing a stale hooks
//! entrypoint. Closes the "known follow-up" from
//! docs/superskills/specs/2026-07-16-calm-hooks-native-cli-subcommand.md's
//! Implementation status section: the plugin's SessionStart bootstrap only
//! scaffolds hooks once (`.calm/hooks.mode` existing gates it), so if the
//! binary path baked into `.claude/settings.json` at scaffold time later
//! goes stale (project directory moved/renamed, npm reinstall changing
//! `node_modules` layout, ...), nothing used to repair it automatically.
//!
//! Spawns the real built `calm` binary, not the in-process functions it
//! calls, since the whole point is verifying the actual CLI wiring —
//! `Commands::Doctor`'s `fix` flag dispatch — end to end, not just
//! `apply_hooks_flag`'s already-covered unit tests.

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

#[test]
fn doctor_fix_repairs_a_stale_entrypoint_path_without_changing_mode() {
    let dir = fresh_project();
    let root = dir.path();

    let init_out = run_calm(root, &["init", "--hooks=enforce"]);
    assert!(
        init_out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );

    let settings_path = root.join(".claude/settings.json");
    let before = std::fs::read_to_string(&settings_path).expect("settings.json scaffolded");
    let real_bin_str = calm_bin().to_str().expect("bin path is valid utf8");
    assert!(
        before.contains(real_bin_str),
        "expected the real binary path wired in, got: {before}"
    );

    // Simulate the project having moved (or the npm-resolved binary having
    // gone stale): corrupt only the wired command path, leaving
    // .calm/hooks.mode (mode=enforce) untouched — exactly what a real
    // relocation looks like from `calm doctor`'s point of view.
    let corrupted = before.replace(real_bin_str, "/no/such/path/this-does-not-exist/calm");
    assert_ne!(corrupted, before);
    std::fs::write(&settings_path, &corrupted).unwrap();

    let doctor_before = run_calm(root, &["doctor"]);
    let doctor_before_text = String::from_utf8_lossy(&doctor_before.stdout);
    assert!(
        doctor_before_text.contains("CONFIGURED BUT NOT ACTIVE")
            || doctor_before_text.contains("missing"),
        "expected doctor to notice the stale entrypoint before --fix, got: {doctor_before_text}"
    );

    let fix_out = run_calm(root, &["doctor", "--fix"]);
    assert!(
        fix_out.status.success(),
        "doctor --fix failed: {}",
        String::from_utf8_lossy(&fix_out.stderr)
    );
    let fix_text = String::from_utf8_lossy(&fix_out.stdout);
    assert!(
        !fix_text.contains("--fix failed"),
        "doctor --fix reported failure: {fix_text}"
    );

    let healed = std::fs::read_to_string(&settings_path).unwrap();
    assert!(
        healed.contains(real_bin_str),
        "entrypoint should point back at the real binary after --fix: {healed}"
    );
    assert!(
        !healed.contains("this-does-not-exist"),
        "corrupted path must be gone after --fix: {healed}"
    );

    let doctor_after = run_calm(root, &["doctor"]);
    let doctor_after_text = String::from_utf8_lossy(&doctor_after.stdout);
    assert!(
        doctor_after_text.contains("enforce mode, active"),
        "expected healed doctor output to report active, got: {doctor_after_text}"
    );
}

#[test]
fn doctor_fix_never_touches_an_explicit_off() {
    let dir = fresh_project();
    let root = dir.path();

    let init_out = run_calm(root, &["init", "--hooks=off"]);
    assert!(init_out.status.success());

    // --hooks=off on a never-installed project is already a no-op for the
    // scaffold itself — the real guarantee under test is the --fix
    // dispatch: it must read the configured mode and skip entirely when
    // it's Off, never silently re-enabling hooks a user turned off.
    let fix_out = run_calm(root, &["doctor", "--fix"]);
    assert!(fix_out.status.success());
    let fix_text = String::from_utf8_lossy(&fix_out.stdout);
    assert!(
        fix_text.contains("nothing to fix"),
        "expected --fix to no-op on Off mode, got: {fix_text}"
    );

    let settings_path = root.join(".claude/settings.json");
    assert!(
        !settings_path.exists(),
        "an explicit Off must never get scaffolded by --fix"
    );
}

#[test]
fn doctor_fix_is_a_noop_when_already_healthy() {
    let dir = fresh_project();
    let root = dir.path();

    run_calm(root, &["init", "--hooks=nudge"]);
    let settings_path = root.join(".claude/settings.json");
    let before = std::fs::read_to_string(&settings_path).unwrap();

    let fix_out = run_calm(root, &["doctor", "--fix"]);
    assert!(fix_out.status.success());

    let after = std::fs::read_to_string(&settings_path).unwrap();
    assert_eq!(
        before, after,
        "an already-healthy nudge-mode install must not be rewritten by --fix"
    );
}
