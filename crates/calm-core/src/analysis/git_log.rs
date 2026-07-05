use std::path::Path;
use std::process::Command;

/// One commit's author, ISO-8601 author date, and the files it touched
/// (`--name-only`) — the shared unit `hotspot::collect_git_churn` and
/// `cochange::compute_co_changes` both fold over, so the `git log` call and
/// its `|||author|||date` marker-line parsing exist in exactly one place.
#[derive(Debug, Clone)]
pub struct GitCommit {
    pub author: Option<String>,
    pub date: Option<String>,
    pub files: Vec<String>,
}

/// Runs `git log --since=<since> --name-only` and groups the output into
/// one `GitCommit` per commit. Returns `(commits, git_available)` —
/// `git_available: false` when git isn't present or this isn't a git repo
/// (not an error to propagate; callers degrade gracefully, same as
/// `hotspot`'s existing fallback).
pub fn commits_with_files(project_root: &Path, since: &str) -> (Vec<GitCommit>, bool) {
    let result = Command::new("git")
        .args([
            "log",
            &format!("--since={since}"),
            "--name-only",
            "--format=|||%ae|||%aI",
        ])
        .current_dir(project_root)
        .output();

    let output = match result {
        Ok(o) if o.status.success() => o,
        _ => return (Vec::new(), false),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits: Vec<GitCommit> = Vec::new();

    for line in stdout.lines() {
        if line.starts_with("|||") {
            let parts: Vec<&str> = line.split("|||").collect();
            commits.push(GitCommit {
                author: parts.get(1).map(|s| s.trim().to_string()),
                date: parts.get(2).map(|s| s.trim().to_string()),
                files: Vec::new(),
            });
        } else if !line.trim().is_empty()
            && let Some(commit) = commits.last_mut()
        {
            commit.files.push(line.trim().to_string());
        }
    }

    (commits, true)
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

    #[test]
    fn test_commits_with_files_groups_by_commit() {
        let dir = std::env::temp_dir().join(format!("ci_gitlog_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);

        std::fs::write(dir.join("a.py"), "1").unwrap();
        std::fs::write(dir.join("b.py"), "1").unwrap();
        run_git(&dir, &["add", "a.py", "b.py"]);
        run_git(&dir, &["commit", "-q", "-m", "first"]);

        std::fs::write(dir.join("a.py"), "2").unwrap();
        run_git(&dir, &["commit", "-q", "-am", "second"]);

        let (commits, available) = commits_with_files(&dir, "1 year");
        assert!(available);
        assert_eq!(commits.len(), 2);
        // git log lists most recent first.
        assert_eq!(commits[0].files, vec!["a.py"]);
        assert_eq!(commits[1].files, vec!["a.py", "b.py"]);
        assert!(commits[0].author.as_deref() == Some("test@example.com"));
        assert!(commits[0].date.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_commits_with_files_no_git_repo() {
        let dir = std::env::temp_dir().join(format!("ci_gitlog_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (commits, available) = commits_with_files(&dir, "1 year");
        assert!(!available);
        assert!(commits.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
