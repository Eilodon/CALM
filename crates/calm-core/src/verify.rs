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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default wall-clock budget for one `run_cargo_check` call --
/// `VerificationConfig::timeout_secs`'s default when `.calm/config.json`
/// doesn't override it. `cargo check` can run `build.rs`/proc-macros/
/// registry or git-dependency fetches; 120s is generous for a `check`
/// (not `build`) on an already-fetched, already-built-once workspace
/// while still bounding a genuinely hung child process.
pub const DEFAULT_VERIFY_TIMEOUT_SECS: u64 = 120;

/// How often the timeout loop polls `Child::try_wait` -- short enough that
/// the reported wall-clock overrun past `timeout` is negligible, long
/// enough not to busy-loop.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
/// however long a real `cargo check` takes is the accepted cost -- but only
/// up to `timeout`. A `build.rs`/proc-macro/registry fetch that hangs would
/// otherwise wedge the tool call (and, on the stdio transport, the whole
/// session) forever; exceeding `timeout` kills the child and reports a
/// failed check instead.
///
/// Doesn't use `Command::output()` (which has no timeout support) --
/// stdout/stderr are drained on background threads while the main thread
/// polls `Child::try_wait` against `timeout`, the same "avoid a full pipe
/// buffer deadlocking the wait" trick `output()` uses internally, just with
/// a deadline added.
pub fn run_cargo_check(
    manifest_path: &Path,
    timeout: Duration,
) -> Result<CargoCheckResult, String> {
    let command = format!(
        "cargo check --manifest-path {} --message-format=short",
        manifest_path.display()
    );
    let mut child = Command::new("cargo")
        .arg("check")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--message-format=short")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn cargo: {e} (is it installed and on PATH?)"))?;

    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => return Err(format!("failed to poll cargo check: {e}")),
        }
    };

    let Some(status) = status else {
        // Timed out: kill and reap so the child never outlives this call as
        // a zombie, then discard whatever partial output the reader threads
        // collected -- diagnostics from a forcibly-killed, half-finished
        // run aren't a trustworthy pass/fail signal.
        let _ = child.kill();
        let _ = child.wait();
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        return Ok(CargoCheckResult {
            command,
            passed: false,
            diagnostics: vec![format!(
                "cargo check timed out after {}s and was killed",
                timeout.as_secs()
            )],
        });
    };

    let stderr_bytes = stderr_reader.join().unwrap_or_default();
    let _ = stdout_reader.join(); // drained only to prevent pipe-buffer deadlock; content unused
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    let diagnostics: Vec<String> = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(MAX_DIAGNOSTIC_LINES)
        .map(|l| l.to_string())
        .collect();

    Ok(CargoCheckResult {
        command,
        passed: status.success(),
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

        let result = run_cargo_check(&dir.path().join("Cargo.toml"), Duration::from_secs(60))
            .expect("cargo on PATH");
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

        let result = run_cargo_check(&dir.path().join("Cargo.toml"), Duration::from_secs(60))
            .expect("cargo on PATH");
        assert!(!result.passed);
        assert!(!result.diagnostics.is_empty());
    }

    #[test]
    fn run_cargo_check_kills_and_reports_failure_on_timeout() {
        // A near-zero timeout should trip almost immediately regardless of
        // how long the real `cargo check` would have taken, proving the
        // deadline -- not just the exit status -- controls the outcome.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"verify_fixture_timeout\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn ok() -> i32 { 1 }\n").unwrap();

        let start = Instant::now();
        let result = run_cargo_check(&dir.path().join("Cargo.toml"), Duration::from_millis(1))
            .expect("spawn should still succeed even though the run times out");
        assert!(!result.passed, "a timed-out run must never report passed");
        assert!(
            result.diagnostics.iter().any(|d| d.contains("timed out")),
            "diagnostics: {:?}",
            result.diagnostics
        );
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timeout enforcement should return promptly, not wait out a real cargo check"
        );
    }
}
