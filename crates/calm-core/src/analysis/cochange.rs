use std::collections::HashMap;
use std::path::Path;

use super::git_log::commits_with_files;

/// A file that historically changed alongside the symbol/file under
/// inspection, in the same commit — a "coupling" signal a static call/import
/// graph cannot see at all (e.g. a model file and its migration, or a
/// component and its snapshot test, with no import edge between them).
#[derive(Debug, Clone, PartialEq)]
pub struct CoChangeEntry {
    pub path: String,
    pub co_change_count: usize,
    /// ISO-8601 date of the most recent commit where both files changed
    /// together — `None` only if `commits_with_files` returned a commit
    /// with no parseable author date, which should not happen in practice.
    pub last_co_changed: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CoChangeResult {
    pub entries: Vec<CoChangeEntry>,
    pub git_available: bool,
    /// How many commits in the `since` window touched `target_path` at all
    /// — 0 doesn't necessarily mean "no coupling exists", it may mean the
    /// file is simply new or wasn't touched in the window.
    pub commits_with_target: usize,
}

/// Mines `git log` for files that changed in the same commit as
/// `target_path`, ranked by how often that happened. `min_co_changes`
/// filters out one-off coincidences (e.g. a repo-wide reformat commit);
/// `top_n` bounds the response size the same way `hotspots`/`callers` do.
pub fn compute_co_changes(
    project_root: &Path,
    target_path: &str,
    since: &str,
    min_co_changes: usize,
    top_n: usize,
) -> CoChangeResult {
    let (commits, git_available) = commits_with_files(project_root, since);
    if !git_available {
        return CoChangeResult {
            entries: Vec::new(),
            git_available: false,
            commits_with_target: 0,
        };
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut last_seen: HashMap<String, String> = HashMap::new();
    let mut commits_with_target = 0usize;

    for commit in &commits {
        if !commit.files.iter().any(|f| f == target_path) {
            continue;
        }
        commits_with_target += 1;
        for path in &commit.files {
            if path == target_path {
                continue;
            }
            *counts.entry(path.clone()).or_insert(0) += 1;
            // Commits are newest-first, so the first time we see `path` here
            // is already its most recent co-change with `target_path`.
            last_seen
                .entry(path.clone())
                .or_insert_with(|| commit.date.clone().unwrap_or_default());
        }
    }

    let mut entries: Vec<CoChangeEntry> = counts
        .into_iter()
        .filter(|(_, count)| *count >= min_co_changes)
        .map(|(path, co_change_count)| {
            let last_co_changed = last_seen.get(&path).filter(|d| !d.is_empty()).cloned();
            CoChangeEntry {
                path,
                co_change_count,
                last_co_changed,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.co_change_count
            .cmp(&a.co_change_count)
            .then_with(|| a.path.cmp(&b.path))
    });
    entries.truncate(top_n);

    CoChangeResult {
        entries,
        git_available: true,
        commits_with_target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    // Each test gets its own `tempfile::tempdir()` (unique random path) —
    // NOT a `std::process::id()`-based name, which collides across the
    // several tests in this module since they all share one test-binary
    // process id and cargo test runs them in parallel by default.
    fn setup_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        dir
    }

    #[test]
    fn test_no_git_repo_returns_unavailable() {
        let dir = tempfile::tempdir().unwrap();

        let result = compute_co_changes(dir.path(), "model.py", "1 year", 1, 10);
        assert!(!result.git_available);
        assert!(result.entries.is_empty());
    }

    #[test]
    fn test_finds_files_that_change_together() {
        let dir = setup_repo();
        // unrelated.py gets its own separate initial commit so it NEVER
        // shares a commit with model.py at all.
        std::fs::write(dir.path().join("unrelated.py"), "1").unwrap();
        run_git(dir.path(), &["add", "unrelated.py"]);
        run_git(dir.path(), &["commit", "-q", "-m", "unrelated init"]);

        std::fs::write(dir.path().join("model.py"), "1").unwrap();
        std::fs::write(dir.path().join("migration.py"), "1").unwrap();
        run_git(dir.path(), &["add", "model.py", "migration.py"]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);

        // model.py + migration.py change together twice more.
        std::fs::write(dir.path().join("model.py"), "2").unwrap();
        std::fs::write(dir.path().join("migration.py"), "2").unwrap();
        run_git(dir.path(), &["commit", "-q", "-am", "second"]);

        std::fs::write(dir.path().join("model.py"), "3").unwrap();
        std::fs::write(dir.path().join("migration.py"), "3").unwrap();
        run_git(dir.path(), &["commit", "-q", "-am", "third"]);

        std::fs::write(dir.path().join("unrelated.py"), "2").unwrap();
        run_git(
            dir.path(),
            &["commit", "-q", "-am", "fourth, unrelated only"],
        );

        let result = compute_co_changes(dir.path(), "model.py", "1 year", 1, 10);
        assert!(result.git_available);
        // init + second + third all touch model.py.
        assert_eq!(result.commits_with_target, 3);
        assert_eq!(
            result.entries.len(),
            1,
            "only migration.py co-changed with model.py"
        );
        assert_eq!(result.entries[0].path, "migration.py");
        assert_eq!(result.entries[0].co_change_count, 3);
        assert!(result.entries[0].last_co_changed.is_some());
    }

    #[test]
    fn test_min_co_changes_filters_one_off_coincidence() {
        let dir = setup_repo();
        std::fs::write(dir.path().join("a.py"), "1").unwrap();
        std::fs::write(dir.path().join("b.py"), "1").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(
            dir.path(),
            &["commit", "-q", "-m", "one-off repo-wide commit"],
        );

        let result = compute_co_changes(dir.path(), "a.py", "1 year", 2, 10);
        assert!(
            result.entries.is_empty(),
            "single co-occurrence must be filtered by min_co_changes=2"
        );
    }

    #[test]
    fn test_top_n_truncates_and_sorts_by_count_desc() {
        let dir = setup_repo();
        // a.py/b.py/c.py exist from commit 1, WITHOUT target.py, so none of
        // the co-change counts below are contaminated by an implicit
        // "everything in the initial commit" co-occurrence.
        for f in ["a.py", "b.py", "c.py"] {
            std::fs::write(dir.path().join(f), "1").unwrap();
        }
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);

        // b.py co-changes with target.py exactly 3x, a.py exactly 1x, c.py
        // never. Content must change each iteration or `commit -am` is a
        // no-op ("nothing to commit") — verified by hand, not assumed.
        for i in 0..3 {
            std::fs::write(dir.path().join("target.py"), format!("x{i}")).unwrap();
            std::fs::write(dir.path().join("b.py"), format!("x{i}")).unwrap();
            run_git(dir.path(), &["add", "target.py", "b.py"]);
            run_git(dir.path(), &["commit", "-q", "-m", "bump"]);
        }
        std::fs::write(dir.path().join("target.py"), "y").unwrap();
        std::fs::write(dir.path().join("a.py"), "y").unwrap();
        run_git(dir.path(), &["commit", "-q", "-am", "a bump"]);

        let result = compute_co_changes(dir.path(), "target.py", "1 year", 1, 1);
        assert_eq!(result.entries.len(), 1, "top_n=1 must truncate");
        assert_eq!(result.entries[0].path, "b.py");
        assert_eq!(result.entries[0].co_change_count, 3);
    }

    #[test]
    fn test_target_itself_never_appears_in_results() {
        let dir = setup_repo();
        std::fs::write(dir.path().join("solo.py"), "1").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.path().join("solo.py"), "2").unwrap();
        run_git(dir.path(), &["commit", "-q", "-am", "second"]);

        let result = compute_co_changes(dir.path(), "solo.py", "1 year", 1, 10);
        assert!(result.entries.iter().all(|e| e.path != "solo.py"));
    }
}
