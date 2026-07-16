pub mod analysis;
pub mod config;
pub mod db;
pub mod edit;
pub mod embedding;
pub mod fitness;
pub mod format;
pub mod gitignore;
pub mod graph;
pub mod hooks;
pub mod hooks_check;
pub mod indexer;
#[cfg(feature = "lsp-overlay")]
pub mod lsp;
pub mod memory;
pub mod resolver;
pub mod sanitize;
#[cfg(feature = "scip-overlay")]
pub mod scip;
pub mod search;
pub mod types;
pub mod walk;
pub mod workflow;

/// Git commit this binary was built from (short SHA, `-dirty` suffix if the
/// working tree had uncommitted changes at build time), or `"unknown"` if
/// git wasn't available at build time. See `build.rs`.
pub const BUILD_INFO: &str = env!("CI_BUILD_INFO");

/// Absolute path this crate's source lived at when THIS binary was
/// compiled (see `build.rs`) — a downloaded release binary carries a CI
/// runner path here that won't exist locally, so comparisons against it
/// (see `is_own_running_binary_source`) fail closed for anyone not
/// running a locally-built dev binary.
pub const BUILD_SOURCE_ROOT: &str = env!("CI_BUILD_SOURCE_ROOT");

/// True when `project_root` is literally the same checkout this running
/// binary was compiled from AND `repo_relative_path` is Rust source under
/// `crates/` — i.e. this call is about to report on (or an edit just
/// wrote to) a file that IS part of the currently-running server's own
/// code. A live daemon (ADR-0005) keeps serving the binary it was
/// spawned with regardless of what happens to the source on disk
/// afterward, so a dogfooding agent editing CALM's own crates/ mid-
/// session can silently keep talking to pre-edit behavior until the
/// daemon is rebuilt and reconnected — this flags exactly that
/// situation so a caller (see `edit_lines_impl`'s `note` field) can warn
/// about it instead of the change appearing to have no effect for no
/// visible reason.
///
/// Always `false` for a released binary (npm, GitHub Release, container
/// image, ...): its `BUILD_SOURCE_ROOT` is a CI runner path that
/// `canonicalize()` fails to resolve on a user's machine, so this
/// returns `false` before ever comparing paths — no false positive for
/// the overwhelming majority of users who never build CALM themselves.
pub fn is_own_running_binary_source(
    project_root: &std::path::Path,
    repo_relative_path: &str,
) -> bool {
    let rel = std::path::Path::new(repo_relative_path);
    let starts_with_crates = matches!(
        rel.components().next(),
        Some(std::path::Component::Normal(c)) if c == "crates"
    );
    if !starts_with_crates || rel.extension().and_then(|e| e.to_str()) != Some("rs") {
        return false;
    }
    let Ok(build_root) = std::path::Path::new(BUILD_SOURCE_ROOT).canonicalize() else {
        return false;
    };
    let Ok(served_root) = project_root.canonicalize() else {
        return false;
    };
    build_root == served_root
}
#[cfg(test)]
mod tests {
    #[test]
    fn build_info_is_never_empty() {
        // build.rs always sets CI_BUILD_INFO to something — a short SHA
        // (optionally -dirty) or the literal "unknown" fallback — never "".
        assert!(!super::BUILD_INFO.is_empty());
    }


    #[test]
    fn build_source_root_is_never_empty() {
        assert!(!super::BUILD_SOURCE_ROOT.is_empty());
    }

    #[test]
    fn is_own_running_binary_source_true_for_this_repo_crates_rs_file() {
        // BUILD_SOURCE_ROOT is a real env!() baked in at compile time for
        // THIS binary (whatever ran this test) — on the machine that built
        // it (true for any `cargo test` run, dev or CI), it always
        // canonicalizes, so this is not conditional on being "the CALM
        // repo" specifically, just on running the test suite at all.
        let root = std::path::Path::new(super::BUILD_SOURCE_ROOT);
        assert!(
            root.canonicalize().is_ok(),
            "BUILD_SOURCE_ROOT should resolve on the machine that built this test binary: {root:?}"
        );
        assert!(super::is_own_running_binary_source(
            root,
            "crates/calm-core/src/lib.rs"
        ));
    }

    #[test]
    fn is_own_running_binary_source_false_for_a_different_project_root() {
        let elsewhere = std::env::temp_dir();
        assert!(!super::is_own_running_binary_source(
            &elsewhere,
            "crates/calm-core/src/lib.rs"
        ));
    }

    #[test]
    fn is_own_running_binary_source_false_outside_crates() {
        let root = std::path::Path::new(super::BUILD_SOURCE_ROOT);
        assert!(!super::is_own_running_binary_source(root, "docs/README.md"));
        assert!(!super::is_own_running_binary_source(
            root,
            "crates/calm-core/Cargo.toml"
        ));
    }
}
