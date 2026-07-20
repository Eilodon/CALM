use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use super::doc_refs::extract_path_refs;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConfigDriftFinding {
    /// Repo-relative path of the doc that made the reference (as declared in
    /// `[config_drift].doc_paths`).
    pub doc_path: String,
    /// The file-path-like token found in that doc that doesn't resolve to
    /// any real file in the project tree.
    pub reference: String,
}

/// Scans each declared doc file for file-path-like references (`server.py`,
/// `tools/search.py`, `crates/calm-core/src/fitness.rs`) and reports any that
/// don't resolve to a real file on disk — the same signal that would catch a
/// `CONTRACTS.md` still describing a pre-rewrite Python layout days after the
/// codebase moved to Rust.
///
/// Existence is checked against the real project tree (gitignore-aware
/// walk), not the symbol index — a reference to `Cargo.toml` or `README.md`
/// is legitimate even though neither is a parsed source file the indexer
/// puts in `file_index`. A reference resolves if it exactly matches a
/// repo-relative path, or matches the tail of one after a `/` boundary (so a
/// doc can write the short form `fitness.rs` instead of the full
/// `crates/calm-core/src/fitness.rs`).
///
/// Returns an empty vec (not an error) when `doc_paths` is empty or a
/// declared doc doesn't exist — this check only judges references *inside*
/// docs that are present, mirroring `check_boundaries`' "no rules declared"
/// pass-by-default behavior.
/// Every repo-relative file path under `project_root`, gitignore-aware —
/// shared groundwork for both `check_config_drift` (does this reference
/// resolve at all?) and `crate::memory`'s ref-capture (which real path did a
/// short-form reference resolve to?), so both pay the walk cost once and
/// agree on what "real" means.
pub fn build_real_path_index(project_root: &Path, ignore_patterns: &[String]) -> HashSet<String> {
    let mut real_paths = HashSet::new();
    for entry in crate::walk::build_walker(project_root, ignore_patterns, false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        real_paths.insert(rel);
    }
    real_paths
}

/// Resolves a file-path-like reference to the repo-relative path it names,
/// or `None` if it doesn't correspond to any real file. Tries, in order: (1)
/// a direct filesystem check — covers dot-directories
/// (`.github/workflows/x.yml`, `.claude/hooks/y.sh`) that `real_paths`
/// deliberately excludes, same exclusion `search`'s grep walker relies on to
/// skip `.git`; (2) exact match in `real_paths`; (3) suffix match after a
/// `/` boundary, so a doc can write the short form `fitness.rs` instead of
/// the full `crates/calm-core/src/fitness.rs`.
pub fn resolve_reference(
    project_root: &Path,
    real_paths: &HashSet<String>,
    reference: &str,
) -> Option<String> {
    if project_root.join(reference).exists() {
        return Some(reference.to_string());
    }
    if real_paths.contains(reference) {
        return Some(reference.to_string());
    }
    real_paths
        .iter()
        .find(|p| p.ends_with(&format!("/{reference}")))
        .cloned()
}

pub fn check_config_drift(
    project_root: &Path,
    doc_paths: &[String],
    ignore_patterns: &[String],
) -> Vec<ConfigDriftFinding> {
    if doc_paths.is_empty() {
        return Vec::new();
    }

    let real_paths = build_real_path_index(project_root, ignore_patterns);

    let mut findings = Vec::new();
    for doc_path in doc_paths {
        let full = project_root.join(doc_path);
        let Ok(text) = std::fs::read_to_string(&full) else {
            continue;
        };
        let mut refs = extract_path_refs(&text);
        refs.sort();
        refs.dedup();
        for r in refs {
            if resolve_reference(project_root, &real_paths, &r).is_none() {
                findings.push(ConfigDriftFinding {
                    doc_path: doc_path.clone(),
                    reference: r,
                });
            }
        }
    }
    findings.sort_by(|a, b| {
        a.doc_path
            .cmp(&b.doc_path)
            .then(a.reference.cmp(&b.reference))
    });
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ci_config_drift_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_doc_paths_returns_no_findings() {
        let dir = temp_project("empty");
        let findings = check_config_drift(&dir, &[], &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn missing_doc_file_is_skipped_not_errored() {
        let dir = temp_project("missing_doc");
        let findings = check_config_drift(&dir, &["NOPE.md".into()], &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_reference_to_nonexistent_file() {
        let dir = temp_project("flags_missing");
        write(&dir, "CONTRACTS.md", "> **Owner:** server.py\n");
        let findings = check_config_drift(&dir, &["CONTRACTS.md".into()], &[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].doc_path, "CONTRACTS.md");
        assert_eq!(findings[0].reference, "server.py");
    }

    #[test]
    fn does_not_flag_reference_to_real_file_exact_path() {
        let dir = temp_project("real_exact");
        write(&dir, "Cargo.toml", "[package]\n");
        write(&dir, "README.md", "See `Cargo.toml` for deps.\n");
        let findings = check_config_drift(&dir, &["README.md".into()], &[]);
        assert!(findings.is_empty(), "got {findings:?}");
    }

    #[test]
    fn does_not_flag_reference_to_real_file_short_suffix_form() {
        let dir = temp_project("real_suffix");
        write(&dir, "crates/calm-core/src/fitness.rs", "// stub\n");
        write(
            &dir,
            "AGENTS.md",
            "See `fitness.rs` for the fitness gate.\n",
        );
        let findings = check_config_drift(&dir, &["AGENTS.md".into()], &[]);
        assert!(findings.is_empty(), "got {findings:?}");
    }

    #[test]
    fn does_not_flag_reference_to_real_file_in_dot_directory() {
        let dir = temp_project("dotdir");
        write(&dir, ".github/workflows/release.yml", "name: Release\n");
        write(
            &dir,
            "README.md",
            "see `.github/workflows/release.yml` for the release matrix\n",
        );
        let findings = check_config_drift(&dir, &["README.md".into()], &[]);
        assert!(findings.is_empty(), "got {findings:?}");
    }

    #[test]
    fn dedups_repeated_reference_within_one_doc() {
        let dir = temp_project("dedup");
        write(
            &dir,
            "CONTRACTS.md",
            "server.py owns this.\nAlso see server.py again.\n",
        );
        let findings = check_config_drift(&dir, &["CONTRACTS.md".into()], &[]);
        assert_eq!(findings.len(), 1);
    }
}
