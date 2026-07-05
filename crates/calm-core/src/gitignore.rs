use std::fs;
use std::path::Path;

use anyhow::Result;

pub fn ensure_gitignore(project_root: &Path) -> Result<()> {
    if !project_root.join(".git").exists() {
        // Not a git repo (or git metadata not visible from this root) — a
        // .gitignore entry has no effect and would just be a stray file in
        // e.g. a tmp/scratch directory or a project intentionally not under git.
        return Ok(());
    }
    let path = project_root.join(".gitignore");
    if path.exists() {
        let content = fs::read_to_string(&path)?;
        if content.contains(".calm") {
            return Ok(());
        }
        // Append with a leading newline if file doesn't already end with one
        let suffix = if content.ends_with('\n') {
            ".calm/\n"
        } else {
            "\n.calm/\n"
        };
        fs::write(&path, format!("{content}{suffix}"))?;
    } else {
        fs::write(&path, ".calm/\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A temp dir that looks like a git repo (has `.git/`) — the common case
    /// `ensure_gitignore` is meant to act on.
    fn tmp_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ci_gi_{}_{}", suffix, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[test]
    fn creates_gitignore_when_missing() {
        let dir = tmp_dir("create");
        ensure_gitignore(&dir).unwrap();
        let content = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(
            content.contains(".calm"),
            "must contain .calm, got: {content}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_when_gitignore_exists_without_entry() {
        let dir = tmp_dir("append");
        fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        ensure_gitignore(&dir).unwrap();
        let content = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(
            content.contains("target/"),
            "must preserve existing entries"
        );
        assert!(content.contains(".calm"), "must add .calm entry");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_when_entry_already_present() {
        let dir = tmp_dir("idem");
        fs::write(dir.join(".gitignore"), "target/\n.calm/\n").unwrap();
        ensure_gitignore(&dir).unwrap();
        let content = fs::read_to_string(dir.join(".gitignore")).unwrap();
        // Must NOT have duplicate entries
        assert_eq!(
            content.matches(".calm").count(),
            1,
            "must not duplicate entry"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression for B8: `ensure_gitignore` used to write `.gitignore` even
    /// in a directory with no `.git/` — a stray file in e.g. a tmp/scratch
    /// dir or a project intentionally not under git.
    #[test]
    fn does_nothing_when_not_a_git_repo() {
        let dir = std::env::temp_dir().join(format!("ci_gi_nogit_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(!dir.join(".git").exists());

        ensure_gitignore(&dir).unwrap();

        assert!(
            !dir.join(".gitignore").exists(),
            "must not create .gitignore outside a git repo"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
