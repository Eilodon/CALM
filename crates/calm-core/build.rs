//! Stamps the git commit this binary was built from into `CI_BUILD_INFO`
//! (read via `env!()` in `src/lib.rs` as `calm_core::BUILD_INFO`), so `ci
//! doctor`/`ci --version` can report "what commit am I actually running",
//! not just the Cargo.toml version string (which doesn't change per-commit).
//! Root cause this exists for: a stale `target/debug/ci` binary silently
//! served an entire MCP session because nothing surfaced that it predated
//! the checked-out source — see `scripts/mcp-launcher.sh`'s freshness check
//! for the other half of the fix.
//!
//! No `cargo:rerun-if-changed` directives are emitted deliberately: with
//! none emitted, Cargo's default is "rerun if anything in this package
//! changed", which already recompiles whenever a change would plausibly
//! trigger a rebuild anyway — narrowing this to just `.git/HEAD` would miss
//! workspace-wide dependency changes and untracked/unstaged edits (the
//! exact kind of drift this is meant to catch).
//!
//! Best-effort: falls back to `"unknown"` if git isn't available or this
//! isn't a git checkout (e.g. a source tarball) — never fails the build.

use std::process::Command;

fn main() {
    let sha = run_git(&["rev-parse", "--short=12", "HEAD"]);
    let dirty = run_git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());

    let info = match sha {
        Some(sha) if dirty => format!("{sha}-dirty"),
        Some(sha) => sha,
        None => "unknown".to_string(),
    };

    println!("cargo:rustc-env=CI_BUILD_INFO={info}");
}

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
