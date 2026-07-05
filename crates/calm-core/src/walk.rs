use std::path::Path;

/// Built-in directories never descended into during a project scan,
/// regardless of user config or `.gitignore`.
pub const IGNORE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    "venv",
    "legacy",
];

/// Return true if `name` matches any pattern in `patterns`.
/// Supports `*.ext` glob (file extension matching) and exact name matching.
pub fn matches_ignore_pattern(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(ext) = p.strip_prefix("*.") {
            name.ends_with(&format!(".{ext}"))
        } else {
            p == name
        }
    })
}

/// True if `name` (a single path *segment*, not a full path) is a directory
/// name the walker categorically refuses to descend into — dot-prefixed, or
/// a literal `IGNORE_DIRS` entry. Deliberately does not consult
/// `ignore_patterns` (user/project config) — that's checked separately by
/// callers that have it, same as `build_walker`'s closure below. This is
/// just the built-in, unconditional part of the rule, factored out so
/// `build_walker` and `path_has_ignored_dir_component` can't drift apart.
pub fn is_ignored_dir_component(name: &str) -> bool {
    name.starts_with('.') || IGNORE_DIRS.contains(&name)
}

/// True if any *directory* component of `path` is one `is_ignored_dir_component`
/// would refuse to descend into — i.e. `ci`'s indexer will never scan this
/// path, no matter how long you wait, regardless of its own extension. Only
/// checks `path.parent()`'s components, not `path`'s own leaf name: the
/// walker only ever filters directory *names* (see `build_walker`'s
/// "dot-files were never filtered" note) — a leaf dotfile like a top-level
/// `.eslintrc.js` can still be legitimately indexed, so treating the leaf
/// itself as disqualifying would be a false exclusion. Used by
/// `diff_impact` to tell "genuinely out of scope" apart from "recognized
/// source file the indexer just hasn't scanned yet" for a path with no
/// `file_index` row.
pub fn path_has_ignored_dir_component(path: &Path) -> bool {
    path.parent().is_some_and(|parent| {
        parent
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .any(is_ignored_dir_component)
    })
}

/// Shared, gitignore-aware directory walker used by both the indexer
/// (`indexer::pipeline::collect_source_files`, which adds its own
/// extension gate on top) and `search(kind="grep")` (which does not gate by
/// extension, so it can search files the indexer never parses — Cargo.toml,
/// docs/*.md, etc.). Honors: built-in `IGNORE_DIRS`, dot-directories, any
/// user-configured `ignore` patterns (applied to both file and directory
/// names), and real `.gitignore` / `.git/info/exclude` rules — the indexer
/// previously never consulted `.gitignore` at all.
pub fn build_walker(root: &Path, ignore_patterns: &[String]) -> ignore::Walk {
    let patterns = ignore_patterns.to_vec();
    ignore::WalkBuilder::new(root)
        .hidden(false) // dot-dir skipping is replicated explicitly below; dot-files were never filtered
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .parents(false)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            let name = entry.file_name().to_str().unwrap_or("");
            if is_dir {
                !(is_ignored_dir_component(name) || matches_ignore_pattern(name, &patterns))
            } else {
                !matches_ignore_pattern(name, &patterns)
            }
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_ignore_pattern() {
        let patterns = vec!["vendor".to_string(), "*.min.js".to_string()];
        assert!(matches_ignore_pattern("vendor", &patterns));
        assert!(matches_ignore_pattern("app.min.js", &patterns));
        assert!(!matches_ignore_pattern("vendors", &patterns));
        assert!(!matches_ignore_pattern("app.js", &patterns));
    }

    #[test]
    fn test_is_ignored_dir_component() {
        assert!(is_ignored_dir_component(".claude"));
        assert!(is_ignored_dir_component(".git"));
        assert!(is_ignored_dir_component("target"));
        assert!(is_ignored_dir_component("node_modules"));
        assert!(!is_ignored_dir_component("crates"));
        assert!(!is_ignored_dir_component("src"));
    }

    #[test]
    fn test_path_has_ignored_dir_component_dotdir() {
        assert!(path_has_ignored_dir_component(Path::new(
            ".claude/hooks/ci-nudge.sh"
        )));
        assert!(path_has_ignored_dir_component(Path::new(".git/HEAD")));
    }

    #[test]
    fn test_path_has_ignored_dir_component_build_artifact_dir() {
        assert!(path_has_ignored_dir_component(Path::new(
            "target/debug/build/foo.rs"
        )));
        assert!(path_has_ignored_dir_component(Path::new(
            "frontend/node_modules/pkg/index.js"
        )));
    }

    #[test]
    fn test_path_has_ignored_dir_component_false_for_indexed_paths() {
        assert!(!path_has_ignored_dir_component(Path::new(
            "crates/calm-core/src/walk.rs"
        )));
        assert!(!path_has_ignored_dir_component(Path::new("README.md")));
    }

    /// A leaf dotfile is not itself a directory, and the walker only ever
    /// filters directory *names* — so a top-level dotfile must not be
    /// treated as excluded just because its own name starts with a dot.
    #[test]
    fn test_path_has_ignored_dir_component_leaf_dotfile_is_not_excluded() {
        assert!(!path_has_ignored_dir_component(Path::new(".eslintrc.js")));
    }
}
