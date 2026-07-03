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
                !(name.starts_with('.')
                    || IGNORE_DIRS.contains(&name)
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
}
