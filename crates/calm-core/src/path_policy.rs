//! Path containment policy for repo-relative paths supplied by a caller
//! (an MCP client, a tool argument) — the check
//! `crates/calm-server/src/tools/edit.rs::resolve_repo_path` already
//! performed inline, pulled out into a standalone, testable module with an
//! explicit policy surface
//! (docs/plans/2026-08-02-phase1-p0-execution-plan.md §3.2c, WS-3 task
//! 3.3/3.4). This is a refactor of that existing logic, not a new check
//! layered on top of it: `resolve_repo_path` delegates here for the exact
//! same containment decision it always made under
//! `SymlinkPolicy::FollowInternalSymlinks` (this module's default and the
//! only mode wired into any server call site so far), so wiring this in
//! changes zero observable behavior.
//!
//! The other two modes (`RejectSymlinks`, `AllowExternalSymlinksWithApproval`)
//! are real and unit-tested here but not wired into any server call site
//! yet. `AllowExternalSymlinksWithApproval` in particular has no approval
//! mechanism to call into until WS-2 (review-token/elicitation) lands, so
//! it always surfaces `NeedsApproval` for an out-of-root target rather
//! than silently allowing an escape it cannot actually get consent for —
//! "not wired up" must fail closed, not open.
//!
//! `RejectSymlinks`'s component walk mirrors VHEATM's
//! `sandbox.py::SandboxExecutor.run()` (check each path component for
//! `is_symlink()`, don't just canonicalize-then-trust) rather than a
//! single `canonicalize()` call — canonicalize alone tells you where a
//! path *ends up*, not whether a symlink was involved in getting there,
//! which is exactly what distinguishes `RejectSymlinks` from
//! `FollowInternalSymlinks`. It is a textual (not physical/TOCTOU-safe)
//! walk: `..`/`.` components are normalized as a string stack, then each
//! resulting prefix is checked with `symlink_metadata`. That is enough to
//! catch the common case (a real symlink sitting somewhere between root
//! and target at check time) but is not a race-free guarantee against a
//! component being swapped for a symlink between this check and the
//! actual read/write — closing that gap needs `openat2(RESOLVE_BENEATH)`
//! on Linux, deliberately left as a separate follow-up PR (see the plan
//! doc), not bundled into this module.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// Reject if any path component from the root down to the target is a
    /// symlink, even one that resolves to somewhere inside the root.
    /// Strictest mode; not wired into any call site yet.
    RejectSymlinks,
    /// Follow symlinks, but reject if the fully resolved path lands
    /// outside the project root. Current/default behavior — what
    /// `resolve_repo_path` already did before this module existed.
    FollowInternalSymlinks,
    /// Follow symlinks even outside the root, but only with explicit
    /// approval. Phase 1 has no approval mechanism wired up, so this mode
    /// always returns `NeedsApproval` for an external target rather than
    /// silently allowing it. Not wired into any call site yet.
    AllowExternalSymlinksWithApproval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPolicyError {
    /// `path` could not be read/canonicalized at all (doesn't exist, a
    /// permission error, etc). `detail` is the underlying `io::Error`'s
    /// `Display` output.
    ReadFailed { path: String, detail: String },
    /// The fully resolved path lands outside `project_root`.
    EscapesRoot { path: String },
    /// `RejectSymlinks` found a symlink at `component` on the way from
    /// root to target, even though the final resolved location is inside
    /// the root.
    SymlinkRejected { path: String, component: PathBuf },
    /// `AllowExternalSymlinksWithApproval` saw a target outside the root
    /// and there is no approval mechanism yet to consult — fails closed.
    NeedsApproval { path: String },
}

/// Resolves `relative` (repo-relative, caller-supplied) against
/// `project_root` under `policy`, returning the canonicalized real path if
/// it's allowed. Both existing callers (`edit_lines_impl`,
/// `insertion_hunk_for`) require the target to already exist, so
/// canonicalizing the full path directly — rather than just its parent —
/// is enough to catch an escape via any component, including the leaf
/// itself being a symlink.
pub fn resolve_within_root(
    project_root: &Path,
    relative: &str,
    policy: SymlinkPolicy,
) -> Result<PathBuf, PathPolicyError> {
    let candidate = project_root.join(relative);
    let real = std::fs::canonicalize(&candidate).map_err(|e| PathPolicyError::ReadFailed {
        path: relative.to_string(),
        detail: e.to_string(),
    })?;
    // `project_root` isn't guaranteed canonical by every caller (tests in
    // particular construct a server directly from an uncanonicalized temp
    // dir) — canonicalize both sides rather than assume the caller
    // already did, so this check can't be defeated simply by an
    // un-canonicalized root.
    let root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());

    if !real.starts_with(&root) {
        return match policy {
            SymlinkPolicy::AllowExternalSymlinksWithApproval => {
                Err(PathPolicyError::NeedsApproval {
                    path: relative.to_string(),
                })
            }
            SymlinkPolicy::RejectSymlinks | SymlinkPolicy::FollowInternalSymlinks => {
                Err(PathPolicyError::EscapesRoot {
                    path: relative.to_string(),
                })
            }
        };
    }

    if policy == SymlinkPolicy::RejectSymlinks
        && let Some(component) = first_symlink_component(&root, relative)
    {
        return Err(PathPolicyError::SymlinkRejected {
            path: relative.to_string(),
            component,
        });
    }

    Ok(real)
}

/// Textually normalizes `relative`'s `.`/`..` components against `root`
/// (no filesystem access — a string-level stack, same normalization
/// `Path::components()` already exposes), then checks each resulting
/// prefix with `symlink_metadata` (which, unlike `metadata`, does not
/// itself follow a link) for the first one that is a symlink.
fn first_symlink_component(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut stack: Vec<OsString> = Vec::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(seg) => stack.push(seg.to_os_string()),
            Component::ParentDir => {
                stack.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }

    let mut probe = root.to_path_buf();
    for seg in stack {
        probe.push(seg);
        if let Ok(meta) = std::fs::symlink_metadata(&probe)
            && meta.file_type().is_symlink()
        {
            return Some(probe);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn follow_internal_symlinks_resolves_an_ordinary_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), "hi\n");
        let resolved =
            resolve_within_root(dir.path(), "a.txt", SymlinkPolicy::FollowInternalSymlinks)
                .unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path().join("a.txt")).unwrap()
        );
    }

    #[test]
    fn follow_internal_symlinks_rejects_dotdot_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_within_root(
            dir.path(),
            "../../etc/passwd",
            SymlinkPolicy::FollowInternalSymlinks,
        )
        .unwrap_err();
        match err {
            PathPolicyError::EscapesRoot { .. } | PathPolicyError::ReadFailed { .. } => {}
            other => panic!("expected an escape or read failure, got {other:?}"),
        }
    }

    #[test]
    fn read_failed_reports_a_nonexistent_target() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_within_root(
            dir.path(),
            "does_not_exist.txt",
            SymlinkPolicy::FollowInternalSymlinks,
        )
        .unwrap_err();
        assert!(matches!(err, PathPolicyError::ReadFailed { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn follow_internal_symlinks_allows_a_symlink_that_stays_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("real.txt"), "hi\n");
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
            .unwrap();
        let resolved = resolve_within_root(
            dir.path(),
            "link.txt",
            SymlinkPolicy::FollowInternalSymlinks,
        )
        .unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path().join("real.txt")).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn follow_internal_symlinks_rejects_a_symlink_escaping_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_file(&outside.path().join("secret.txt"), "top secret\n");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let err = resolve_within_root(
            dir.path(),
            "link.txt",
            SymlinkPolicy::FollowInternalSymlinks,
        )
        .unwrap_err();
        assert_eq!(
            err,
            PathPolicyError::EscapesRoot {
                path: "link.txt".to_string()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn reject_symlinks_refuses_even_a_symlink_that_stays_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("real.txt"), "hi\n");
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
            .unwrap();

        let err =
            resolve_within_root(dir.path(), "link.txt", SymlinkPolicy::RejectSymlinks).unwrap_err();
        assert!(
            matches!(err, PathPolicyError::SymlinkRejected { .. }),
            "RejectSymlinks must refuse an internal symlink that FollowInternalSymlinks allows, got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reject_symlinks_allows_an_ordinary_file_with_no_symlink_involved() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), "hi\n");
        let resolved =
            resolve_within_root(dir.path(), "a.txt", SymlinkPolicy::RejectSymlinks).unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path().join("a.txt")).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn allow_external_with_approval_fails_closed_when_no_approval_mechanism_exists() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_file(&outside.path().join("secret.txt"), "top secret\n");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let err = resolve_within_root(
            dir.path(),
            "link.txt",
            SymlinkPolicy::AllowExternalSymlinksWithApproval,
        )
        .unwrap_err();
        assert_eq!(
            err,
            PathPolicyError::NeedsApproval {
                path: "link.txt".to_string()
            },
            "with no approval mechanism wired up yet, an external target must never be silently allowed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn allow_external_with_approval_still_allows_an_internal_target_with_no_approval_needed() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), "hi\n");
        let resolved = resolve_within_root(
            dir.path(),
            "a.txt",
            SymlinkPolicy::AllowExternalSymlinksWithApproval,
        )
        .unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path().join("a.txt")).unwrap()
        );
    }
}
