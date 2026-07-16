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
    ensure_embedding_weights();

    let sha = run_git(&["rev-parse", "--short=12", "HEAD"]);
    let dirty = run_git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());

    let info = match sha {
        Some(sha) if dirty => format!("{sha}-dirty"),
        Some(sha) => sha,
        None => "unknown".to_string(),
    };

    println!("cargo:rustc-env=CI_BUILD_INFO={info}");

    // Stamps the absolute path this crate was compiled from, so a locally
    // built dev binary can recognize "the project I'm serving right now IS
    // the exact checkout I was compiled from" (see
    // `is_own_running_binary_source` in `src/lib.rs`) — used to warn a
    // dogfooding agent that editing crates/ Rust source won't reach this
    // session's already-running daemon until it's rebuilt and reconnected.
    // For a downloaded release binary this is a CI runner path that won't
    // exist on the user's machine, so the comparison fails closed (never a
    // false positive for a normal user).
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let source_root = std::path::Path::new(&manifest_dir)
        .parent() // crates/
        .and_then(std::path::Path::parent) // repo root
        .map(|p| p.display().to_string())
        .unwrap_or(manifest_dir);
    println!("cargo:rustc-env=CI_BUILD_SOURCE_ROOT={source_root}");
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

/// Ensures `assets/potion-code-16m/model.safetensors` exists and is valid
/// before `include_bytes!` (in `src/embedding.rs`, gated on the
/// `embeddings` feature) reads it at compile time — `include_bytes!`
/// requires the file to exist on disk, and only a build script is
/// guaranteed to run before every possible build invocation (plain `cargo
/// build`, CI, rust-analyzer, ...); a shell hook like
/// `.claude/hooks/session-start-build-calm.sh` only runs for its own
/// caller, so it can't provide that guarantee on its own.
///
/// CORRECTION (2026-07-12): this file used to be committed via Git LFS.
/// That exhausted the GitHub account's Git LFS bandwidth budget (same
/// incident as `.calm-bin`'s — see
/// `docs/superskills/specs/2026-07-12-edge-release-binary-distribution.md`)
/// — `model.safetensors` is no longer git-tracked at all. Fetched here
/// instead, directly from the public HuggingFace Hub repo
/// `minishlab/potion-code-16M` — the exact same public source
/// `Embedder::load`'s existing runtime fallback already uses via
/// model2vec-rs's `from_pretrained`, and the same `huggingface.co/<repo>/
/// resolve/main/<file>` URL pattern that fallback already relies on, so
/// this isn't a new trust boundary, just an earlier use of the same one.
///
/// Never fails the build: on any error (no network, no `curl`, checksum
/// mismatch, ...) this writes a placeholder byte-shaped exactly like an
/// unresolved Git LFS pointer stub instead of real weights.
/// `embedding::is_lfs_pointer_stub` already detects that exact shape, and
/// `Embedder::load` already falls back to a one-time HuggingFace Hub
/// download at runtime for it — reusing that existing, already-tested
/// path instead of inventing a second fallback mechanism means a
/// build-time fetch failure degrades to exactly today's "git-lfs wasn't
/// installed" runtime behavior, never to a compile error.
fn ensure_embedding_weights() {
    if std::env::var_os("CARGO_FEATURE_EMBEDDINGS").is_none() {
        return; // not building with the embeddings feature — nothing to do
    }

    let path = std::path::Path::new("assets/potion-code-16m/model.safetensors");
    const EXPECTED_SHA256: &str =
        "ca6159081a6e96cebe4ad878e5e8437bfccc761e8db16223370149cd2faa6c0b";

    if path.is_file() && sha256_of_file(path).as_deref() == Ok(EXPECTED_SHA256) {
        return; // already present and verified from a prior build — no network needed
    }

    println!(
        "cargo:warning=fetching vendored embedding model weights \
         (minishlab/potion-code-16M, ~64MB, one-time, cached in the working tree afterward)"
    );

    if let Err(reason) = fetch_and_install(path, EXPECTED_SHA256) {
        println!(
            "cargo:warning=could not fetch vendored embedding model weights ({reason}) \
             — using a placeholder; semantic search falls back to a one-time HuggingFace \
             Hub download at runtime instead (see embedding::Embedder::load)"
        );
        write_placeholder(path);
    }
}

/// Downloads to a temp file and verifies its checksum BEFORE ever touching
/// `dest`, then installs via an atomic rename — a concurrent `cargo build`
/// (e.g. rust-analyzer running alongside a manual terminal build) can
/// never observe a partially-written `dest`, and a corrupt/incomplete
/// download can never get `include_bytes!`'d into a binary unverified.
fn fetch_and_install(dest: &std::path::Path, expected_sha256: &str) -> Result<(), String> {
    let url = "https://huggingface.co/minishlab/potion-code-16M/resolve/main/model.safetensors";
    let tmp = std::env::temp_dir().join(format!("calm-potion-code-16m-{}.tmp", std::process::id()));

    let status = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "5", "--max-time", "120", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .map_err(|e| format!("curl not runnable: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("curl exited with {status}"));
    }

    let actual = match sha256_of_file(&tmp) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err("downloaded file's SHA256 doesn't match the expected checksum".to_string());
    }

    std::fs::rename(&tmp, dest).map_err(|e| format!("install downloaded file: {e}"))
}

/// Shells out rather than adding a crate dependency for one checksum
/// comparison — mirrors how `scripts/mcp-launcher.sh` already verifies its
/// own downloads (`sha256sum -c`). Tries `sha256sum` (coreutils, universal
/// on Linux and GitHub's macOS runners) first, falls back to `shasum -a
/// 256` (present on macOS even without coreutils) — the same GNU-vs-BSD
/// portability concern already documented in `release.yml`'s `sed -i.bak`
/// comment for this workspace's other cross-platform tooling.
fn sha256_of_file(path: &std::path::Path) -> Result<String, String> {
    let try_cmd = |cmd: &str, args: &[&str]| -> Option<String> {
        let output = Command::new(cmd).args(args).arg(path).output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout)
            .ok()?
            .split_whitespace()
            .next()
            .map(str::to_string)
    };
    try_cmd("sha256sum", &[])
        .or_else(|| try_cmd("shasum", &["-a", "256"]))
        .ok_or_else(|| "neither sha256sum nor shasum -a 256 is available".to_string())
}

fn write_placeholder(path: &std::path::Path) {
    let _ = std::fs::write(
        path,
        b"version https://git-lfs.github.com/spec/v1\n\
          oid sha256:0000000000000000000000000000000000000000000000000000000000000000\n\
          size 0\n",
    );
}
