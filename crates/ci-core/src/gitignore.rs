use std::fs;
use std::path::Path;

use anyhow::Result;

pub fn ensure_gitignore(project_root: &Path) -> Result<()> {
    let path = project_root.join(".gitignore");
    if path.exists() {
        let content = fs::read_to_string(&path)?;
        if content.contains(".codeindex") {
            return Ok(());
        }
        // Append with a leading newline if file doesn't already end with one
        let suffix = if content.ends_with('\n') { ".codeindex/\n" } else { "\n.codeindex/\n" };
        fs::write(&path, format!("{content}{suffix}"))?;
    } else {
        fs::write(&path, ".codeindex/\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ci_gi_{}_{}", suffix, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_gitignore_when_missing() {
        let dir = tmp_dir("create");
        ensure_gitignore(&dir).unwrap();
        let content = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(content.contains(".codeindex"), "must contain .codeindex, got: {content}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_when_gitignore_exists_without_entry() {
        let dir = tmp_dir("append");
        fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        ensure_gitignore(&dir).unwrap();
        let content = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(content.contains("target/"), "must preserve existing entries");
        assert!(content.contains(".codeindex"), "must add .codeindex entry");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_when_entry_already_present() {
        let dir = tmp_dir("idem");
        fs::write(dir.join(".gitignore"), "target/\n.codeindex/\n").unwrap();
        ensure_gitignore(&dir).unwrap();
        let content = fs::read_to_string(dir.join(".gitignore")).unwrap();
        // Must NOT have duplicate entries
        assert_eq!(content.matches(".codeindex").count(), 1, "must not duplicate entry");
        let _ = fs::remove_dir_all(&dir);
    }
}
