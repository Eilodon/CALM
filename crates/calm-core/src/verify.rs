//! Fast/Semantic verification -- WS-6's first slice (docs/plans/2026-08-03-
//! ws6-verification-pipeline-execution-plan.md): today this is exactly one
//! check, `cargo check` scoped to the nearest ancestor Cargo package,
//! gating `crates/calm-core/src/txn.rs`'s `TxState::VerifyPending`
//! transition for real for the first time. Deliberately narrow -- no
//! Semgrep/CodeQL/test-selection tier, no other language yet, no async job
//! queue. See that plan doc's anti-goals for why, and its rollout section
//! for why no shadow/enforce staging is needed here (the feature is gated
//! behind `config::VerificationConfig::rust_check_on_write`, default
//! `false`, so it starts in an off state rather than a shadow one).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Only language this module can actually verify today -- checked before
/// ever routing a transaction through `VerifyPending`
/// (`VerificationConfig::rust_check_on_write` gates whether verification is
/// attempted at all; this gates *which* files it applies to once the flag
/// is on).
pub fn is_verifiable_rust_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("rs")
}

/// Walks upward from `file_path`'s parent directory looking for the
/// nearest `Cargo.toml` -- same ancestor-walk strategy as
/// `format::detect_rust_edition` (see that function's doc comment for why
/// a heuristic walk, not a full workspace-graph query, is sufficient: a
/// miss here only costs a wrong verification scope, never a wrong file
/// write). Stops at `project_root`.
pub fn find_nearest_cargo_toml(file_path: &Path, project_root: &Path) -> Option<PathBuf> {
    let mut dir = file_path.parent();
    while let Some(d) = dir {
        let candidate = d.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if d == project_root {
            break;
        }
        dir = d.parent();
    }
    None
}

pub struct CargoCheckResult {
    pub command: String,
    pub passed: bool,
    /// `cargo check`'s diagnostics land on stderr regardless of
    /// `--message-format`; truncated so a pathological error flood can't
    /// balloon a tool response.
    pub diagnostics: Vec<String>,
}

const MAX_DIAGNOSTIC_LINES: usize = 40;

/// Runs `cargo check --manifest-path <manifest_path>` and classifies the
/// result. Inline, not backgrounded -- same posture as `retry_maintenance`
/// (`crates/calm-server/src/tools/txn.rs`): an explicit, on-demand
/// verification action, not something in a hot write path, so blocking for
/// however long a real `cargo check` takes is the accepted cost.
pub fn run_cargo_check(manifest_path: &Path) -> Result<CargoCheckResult, String> {
    let command = format!(
        "cargo check --manifest-path {} --message-format=short",
        manifest_path.display()
    );
    let output = Command::new("cargo")
        .arg("check")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--message-format=short")
        .output()
        .map_err(|e| format!("failed to spawn cargo: {e} (is it installed and on PATH?)"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostics: Vec<String> = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(MAX_DIAGNOSTIC_LINES)
        .map(|l| l.to_string())
        .collect();

    Ok(CargoCheckResult {
        command,
        passed: output.status.success(),
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_verifiable_rust_file_matches_only_rs_extension() {
        assert!(is_verifiable_rust_file(Path::new("src/lib.rs")));
        assert!(!is_verifiable_rust_file(Path::new("README.md")));
        assert!(!is_verifiable_rust_file(Path::new("src/lib")));
    }

    #[test]
    fn find_nearest_cargo_toml_finds_immediate_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        assert_eq!(
            find_nearest_cargo_toml(&src, dir.path()),
            Some(dir.path().join("Cargo.toml"))
        );
    }

    #[test]
    fn find_nearest_cargo_toml_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        assert_eq!(find_nearest_cargo_toml(&src, dir.path()), None);
    }

    // Both cargo-spawning tests below build an isolated, standalone crate
    // under its own tempdir (own Cargo.toml, no [workspace] parent) so cargo
    // gives it its own target/ directory -- never the real calm-core
    // workspace's, which would otherwise contend for the same build lock a
    // concurrently running `cargo test` already holds.

    #[test]
    fn run_cargo_check_passes_on_valid_isolated_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"verify_fixture_ok\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn ok() -> i32 { 1 }\n").unwrap();

        let result = run_cargo_check(&dir.path().join("Cargo.toml")).expect("cargo on PATH");
        assert!(result.passed, "diagnostics: {:?}", result.diagnostics);
    }

    #[test]
    fn run_cargo_check_fails_on_broken_isolated_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"verify_fixture_broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "fn broken( { not rust\n").unwrap();

        let result = run_cargo_check(&dir.path().join("Cargo.toml")).expect("cargo on PATH");
        assert!(!result.passed);
        assert!(!result.diagnostics.is_empty());
    }
}
