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
    // VCS internals and CALM's own state/index — unlike other dot-prefixed
    // directories (`.github/`, `.claude/`, etc., which can hold
    // human-authored config/docs a `search(kind="grep")` caller may
    // legitimately want to reach, see `allow_dotdirs` below), these two are
    // never useful to walk regardless of caller, so they're excluded
    // unconditionally rather than folded into the dotdir toggle.
    ".git",
    ".calm",
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
/// name the walker refuses to descend into: always for a literal
/// `IGNORE_DIRS` entry, and for any other dot-prefixed name unless
/// `allow_dotdirs` is `true`. Deliberately does not consult
/// `ignore_patterns` (user/project config) — that's checked separately by
/// callers that have it, same as `build_walker`'s closure below. This is
/// just the built-in part of the rule, factored out so `build_walker` and
/// `path_has_ignored_dir_component` can't drift apart.
///
/// `allow_dotdirs`: the indexer (via `path_has_ignored_dir_component` and
/// `build_walker`'s own indexer callers) always passes `false` — dotdirs
/// categorically hold nothing it should ever parse as source. `search
/// (kind="grep")` passes `true` so it can reach the human-authored
/// config/docs (`.github/workflows/*.yml`, `.claude/hooks/*.sh`, etc.) that
/// commonly live under a dotdir and that this search kind's own contract
/// promises to cover ("files the indexer never parses") — `.git`/`.calm`
/// stay excluded regardless via `IGNORE_DIRS` above, since neither is ever
/// useful to grep either.
pub fn is_ignored_dir_component(name: &str, allow_dotdirs: bool) -> bool {
    (!allow_dotdirs && name.starts_with('.')) || IGNORE_DIRS.contains(&name)
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
            .any(|seg| is_ignored_dir_component(seg, false))
    })
}

/// Shared, gitignore-aware directory walker used by both the indexer
/// (`indexer::pipeline::collect_source_files`, which adds its own
/// extension gate on top) and `search(kind="grep")` (which does not gate by
/// extension, so it can search files the indexer never parses — Cargo.toml,
/// docs/*.md, etc.). Honors: built-in `IGNORE_DIRS` (always), dot-directories
/// (unless `allow_dotdirs` is `true` — see `is_ignored_dir_component`), any
/// user-configured `ignore` patterns (applied to both file and directory
/// names), and real `.gitignore` / `.git/info/exclude` rules — the indexer
/// previously never consulted `.gitignore` at all.
///
/// Every caller except `search_grep` passes `allow_dotdirs: false`,
/// preserving the walker's original dotdir-exclusion behavior exactly.
pub fn build_walker(root: &Path, ignore_patterns: &[String], allow_dotdirs: bool) -> ignore::Walk {
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
                !(is_ignored_dir_component(name, allow_dotdirs)
                    || matches_ignore_pattern(name, &patterns))
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
        assert!(is_ignored_dir_component(".claude", false));
        assert!(is_ignored_dir_component(".git", false));
        assert!(is_ignored_dir_component("target", false));
        assert!(is_ignored_dir_component("node_modules", false));
        assert!(!is_ignored_dir_component("crates", false));
        assert!(!is_ignored_dir_component("src", false));
    }

    /// `search(kind="grep")`'s contract: dotdirs open up (e.g. `.github`,
    /// `.claude`) except the two that are never useful to grep regardless —
    /// VCS internals and CALM's own state/index, both hard-excluded via
    /// `IGNORE_DIRS` rather than the dotdir toggle.
    #[test]
    fn test_is_ignored_dir_component_allow_dotdirs_still_excludes_git_and_calm() {
        assert!(!is_ignored_dir_component(".github", true));
        assert!(!is_ignored_dir_component(".claude", true));
        assert!(is_ignored_dir_component(".git", true));
        assert!(is_ignored_dir_component(".calm", true));
        assert!(is_ignored_dir_component("target", true));
        assert!(!is_ignored_dir_component("crates", true));
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
