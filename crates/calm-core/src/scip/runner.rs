//! Detect a `rust-analyzer` binary and drive its batch `scip` subcommand.
//! Detect-once, fail-silent (ADR-0004 §2): any failure returns None/Err and the
//! caller keeps the syntactic graph untouched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Total wall-clock budget for one `rust-analyzer scip` pass. Measured cost on
/// the `ci` workspace itself (~44k occurrences) was 21.5s; ripgrep 20s. 120s
/// leaves generous headroom; overrun → kill and keep whatever the syntactic
/// tier already produced.
pub const SCIP_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolve a usable rust-analyzer binary path. Tries, in order: an explicit
/// override, `PATH`, `rustup which`, and the VS Code extension bundle.
pub fn resolve_binary(override_bin: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("rust-analyzer")); // PATH lookup via which-style probe
    if let Some(path) = rustup_which() {
        candidates.push(path);
    }
    if let Some(home) = dirs_home()
        && let Some(p) = newest_vscode_ra(&home)
    {
        candidates.push(p);
    }
    candidates.into_iter().find(|c| binary_runs(c))
}

/// Run `<bin> scip <root> --output <out>` under the time budget. Returns the
/// output path on success. Never propagates a panic; a non-zero exit or timeout
/// is an `Err` the caller swallows.
pub fn run_scip(bin: &Path, root: &Path, out: &Path) -> anyhow::Result<()> {
    let mut child = Command::new(bin)
        .arg("scip")
        .arg(root)
        .arg("--output")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Poll with a deadline; kill on overrun (Command has no built-in timeout —
    // same pattern as analysis/diff_impact.rs's bounded wait).
    let deadline = std::time::Instant::now() + SCIP_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("rust-analyzer scip exited with {status}");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            anyhow::bail!(
                "rust-analyzer scip exceeded {}s budget",
                SCIP_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn binary_runs(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `<bin> --version` output, trimmed, or `""` if it can't be run. Used as part
/// of the overlay cache key — any version change invalidates the cache.
pub fn binary_version(bin: &Path) -> String {
    Command::new(bin)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `rustup which rust-analyzer` — resolves the toolchain-managed binary when
/// it's not directly on `PATH`. Absent/failing `rustup` is not an error here;
/// `binary_runs` is the real gate.
fn rustup_which() -> Option<PathBuf> {
    let out = Command::new("rustup")
        .args(["which", "--toolchain", "stable", "rust-analyzer"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn newest_vscode_ra(home: &Path) -> Option<PathBuf> {
    let ext_dir = home.join(".vscode/extensions");
    let mut hits: Vec<PathBuf> = std::fs::read_dir(&ext_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rust-lang.rust-analyzer-"))
        })
        .map(|p| p.join("server/rust-analyzer"))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    hits.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_none_when_binary_absent() {
        // `resolve_binary` cascades an override -> PATH -> rustup -> VS Code
        // fallback, so a fake override alone doesn't prove absence: on any dev
        // box with rust-analyzer actually installed (as this one is), a fake
        // override would still resolve via one of those fallbacks, making the
        // end-to-end assertion flaky based on what's installed. Pin down the
        // real invariant — an absent/non-executable binary path yields a
        // negative, non-panicking result — at the underlying probe instead.
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-ra-binary-xyz"
        )));
    }
}
