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

/// Per-stream (stdout, stderr) byte cap on `run_cargo_check`'s captured
/// output. A pathological/malicious `build.rs`/proc-macro can write output
/// far faster than any reasonable wall-clock timeout would catch; without
/// a cap the two reader threads below buffer everything in memory for the
/// whole `timeout` window, which can exhaust host memory well before that
/// deadline ever arrives. 8 MiB per stream is generous for even a very
/// verbose real compile-error dump.
const MAX_OUTPUT_BYTES_PER_STREAM: usize = 8 * 1024 * 1024;

/// Chunk size `read_capped` reads in -- small enough that hitting the cap
/// mid-chunk never overshoots it by more than this, large enough not to
/// thrash on syscall overhead for a normal, well-behaved `cargo check`.
const READ_CHUNK_SIZE: usize = 64 * 1024;

/// Drains `pipe` into a `Vec<u8>` capped at `max_bytes`, stopping the
/// instant the cap is hit rather than reading to EOF -- letting the
/// caller's poll loop kill the child immediately instead of waiting out
/// the rest of `timeout`. Sets `output_exceeded` (shared with the sibling
/// stream's reader thread and the poll loop) so the caller can tell "hit
/// the byte cap" apart from "hit EOF normally" after the fact.
fn read_capped(
    mut pipe: impl std::io::Read,
    max_bytes: usize,
    output_exceeded: &std::sync::atomic::AtomicBool,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_SIZE];
    loop {
        if buf.len() >= max_bytes {
            output_exceeded.store(true, std::sync::atomic::Ordering::SeqCst);
            break;
        }
        let want = READ_CHUNK_SIZE.min(max_bytes - buf.len());
        match pipe.read(&mut chunk[..want]) {
            Ok(0) => break, // EOF
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    buf
}

/// Kills `child` and, on Unix, its whole process group -- not just the
/// direct PID `Child::kill()` alone targets, which leaves any descendant
/// (rustc invocations, build scripts, proc-macro servers -- none of which
/// get their own process group by default, so they'd otherwise keep
/// running after a "successful" kill) still alive. Relies on the spawn
/// side having called `process_group(0)` (see `run_cargo_check`), which
/// makes the child its own group leader, so `-pid` targets that whole
/// group via the same `kill(-pgid, sig)` convention `daemon.rs` already
/// uses for its own child process groups. Falls back to `Child::kill()`
/// alone on non-Unix (no process-group story there in this fix) or if the
/// group kill itself is a no-op (e.g. the child already exited).
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: FFI call to `kill(2)` with a valid PID and signal number --
        // its behavior for every input (including "no such process", e.g. if
        // the child already exited between the caller noticing and this
        // call) is fully defined by POSIX; there's no caller-upheld
        // invariant beyond that.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Runs `cargo check --manifest-path <manifest_path>` and classifies the
/// result. Inline, not backgrounded -- same posture as `retry_maintenance`
/// (`crates/calm-server/src/tools/txn.rs`): an explicit, on-demand
/// verification action, not something in a hot write path, so blocking for
/// however long a real `cargo check` takes is the accepted cost -- but only
/// up to `timeout`, and only up to `MAX_OUTPUT_BYTES_PER_STREAM` of
/// captured output per stream. A `build.rs`/proc-macro/registry fetch that
/// hangs (or floods stdout/stderr) would otherwise wedge the tool call
/// (and, on the stdio transport, the whole session) forever, or exhaust
/// host memory well before `timeout` fires; exceeding either bound kills
/// the child (and, on Unix, its whole process group -- see
/// `kill_process_tree`) and reports a failed check instead.
///
/// Doesn't use `Command::output()` (which has no timeout support) --
/// stdout/stderr are drained on background threads while the main thread
/// polls `Child::try_wait` against `timeout`, the same "avoid a full pipe
/// buffer deadlocking the wait" trick `output()` uses internally, just with
/// a deadline (and a byte cap) added.
pub fn run_cargo_check(
    manifest_path: &Path,
    timeout: Duration,
) -> Result<CargoCheckResult, String> {
    let command = format!(
        "cargo check --manifest-path {} --message-format=short",
        manifest_path.display()
    );
    let mut cmd = Command::new("cargo");
    cmd.arg("check")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--message-format=short")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Put `cargo` (and every process it spawns -- rustc, build scripts,
    // proc-macro servers, all of which inherit the parent's process group
    // by default) in a NEW process group of its own, so a later kill can
    // target the whole tree via `kill_process_tree` instead of only the
    // direct `cargo` PID.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn cargo: {e} (is it installed and on PATH?)"))?;

    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let output_exceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_flag = std::sync::Arc::clone(&output_exceeded);
    let stderr_flag = std::sync::Arc::clone(&output_exceeded);
    let stdout_reader = std::thread::spawn(move || {
        read_capped(stdout_pipe, MAX_OUTPUT_BYTES_PER_STREAM, &stdout_flag)
    });
    let stderr_reader = std::thread::spawn(move || {
        read_capped(stderr_pipe, MAX_OUTPUT_BYTES_PER_STREAM, &stderr_flag)
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout
                    || output_exceeded.load(std::sync::atomic::Ordering::SeqCst)
                {
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => return Err(format!("failed to poll cargo check: {e}")),
        }
    };

    let Some(status) = status else {
        // Timed out, or exceeded the output cap on either stream: kill the
        // whole process group (not just the direct `cargo` PID) and reap so
        // nothing outlives this call as a zombie or an orphaned descendant,
        // then discard whatever partial output the reader threads
        // collected -- diagnostics from a forcibly-killed, half-finished
        // run aren't a trustworthy pass/fail signal.
        kill_process_tree(&mut child);
        let _ = child.wait();
        let exceeded_output = output_exceeded.load(std::sync::atomic::Ordering::SeqCst);
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        let diagnostic = if exceeded_output {
            format!(
                "cargo check produced more than {MAX_OUTPUT_BYTES_PER_STREAM} bytes of output \
                 on one stream and was killed"
            )
        } else {
            format!(
                "cargo check timed out after {}s and was killed",
                timeout.as_secs()
            )
        };
        return Ok(CargoCheckResult {
            command,
            passed: false,
            diagnostics: vec![diagnostic],
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

    #[test]
    fn read_capped_returns_everything_when_under_the_cap() {
        let data = b"short diagnostic output";
        let exceeded = std::sync::atomic::AtomicBool::new(false);
        let buf = read_capped(&data[..], 1024, &exceeded);
        assert_eq!(buf, data.to_vec());
        assert!(!exceeded.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn read_capped_stops_at_the_cap_and_flags_exceeded() {
        // Bigger than READ_CHUNK_SIZE so this also exercises the
        // multi-chunk loop, not just a single short read.
        let data = vec![b'x'; READ_CHUNK_SIZE * 3];
        let exceeded = std::sync::atomic::AtomicBool::new(false);
        let cap = READ_CHUNK_SIZE + 10;
        let buf = read_capped(&data[..], cap, &exceeded);
        assert_eq!(
            buf.len(),
            cap,
            "must stop exactly at the cap, not overshoot"
        );
        assert!(
            exceeded.load(std::sync::atomic::Ordering::SeqCst),
            "must flag that more data was available than the cap allowed"
        );
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
