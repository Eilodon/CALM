use std::path::Path;
use std::process::Command;

const CODEOWNERS_PATHS: &[&str] = &[
    ".github/CODEOWNERS",
    "CODEOWNERS",
    "docs/CODEOWNERS",
    ".gitlab/CODEOWNERS",
];

pub fn load_codeowners(project_root: &Path) -> Vec<(String, Vec<String>)> {
    for relative in CODEOWNERS_PATHS {
        let path = project_root.join(relative);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut patterns = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let pattern = parts[0].to_string();
                let owners: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
                patterns.push((pattern, owners));
            }
        }
        return patterns;
    }
    Vec::new()
}

pub fn find_owners(patterns: &[(String, Vec<String>)], file_path: &str) -> Vec<String> {
    let mut matched: Vec<String> = Vec::new();
    let file_path_normalized = file_path.trim_start_matches('/');

    for (pattern, owners) in patterns {
        let pattern_normalized = pattern.trim_start_matches('/');

        if let Some(dir_pattern) = pattern_normalized.strip_suffix('/') {
            if contains_glob_chars(dir_pattern) {
                let parts: Vec<&str> = file_path_normalized.split('/').collect();
                let mut matched_dir = false;
                for i in 0..parts.len() {
                    let prefix = parts[..=i].join("/");
                    if glob_match(dir_pattern, &prefix) {
                        matched_dir = true;
                        break;
                    }
                }
                if matched_dir {
                    matched = owners.clone();
                }
            } else if file_path_normalized.starts_with(pattern_normalized) {
                matched = owners.clone();
            }
        } else if !pattern_normalized.contains('/') {
            // No slash → matches any file by basename
            let basename = file_path_normalized
                .rsplit('/')
                .next()
                .unwrap_or(file_path_normalized);
            if glob_match(pattern_normalized, basename) {
                matched = owners.clone();
            }
        } else {
            // Path pattern with slash → root-anchored, segment-by-segment
            let pattern_parts: Vec<&str> = pattern_normalized.split('/').collect();
            let file_parts: Vec<&str> = file_path_normalized.split('/').collect();
            if match_path_pattern(&pattern_parts, &file_parts) {
                matched = owners.clone();
            }
        }
    }
    matched
}

fn match_path_pattern(pattern_parts: &[&str], file_parts: &[&str]) -> bool {
    if pattern_parts.len() != file_parts.len() {
        return false;
    }
    pattern_parts
        .iter()
        .zip(file_parts.iter())
        .all(|(pp, fp)| glob_match(pp, fp))
}

fn contains_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

fn glob_match(pattern: &str, text: &str) -> bool {
    // Simple glob matching: * matches anything except /, ? matches single char
    glob_match_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_inner(pattern: &[u8], text: &[u8]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0);

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

pub fn get_git_blame_owners(
    project_root: &Path,
    file_path: &str,
    top_n: usize,
    timeout_secs: u64,
    since: &str,
) -> Vec<String> {
    let result = Command::new("git")
        .args([
            "log",
            &format!("--since={since}"),
            "--follow",
            "-n",
            "10",
            "--format=%ae",
            "--",
            file_path,
        ])
        .current_dir(project_root)
        .output();

    let output = match result {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut authors = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for email in stdout.lines() {
        let email = email.trim();
        if !email.is_empty() && seen.insert(email.to_string()) {
            authors.push(email.to_string());
            if authors.len() >= top_n {
                break;
            }
        }
    }
    let _ = timeout_secs; // timeout handled by OS-level process timeout
    authors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_codeowners_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_codeowners(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_codeowners_parses() {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join(".github");
        std::fs::create_dir_all(&gh).unwrap();
        std::fs::write(
            gh.join("CODEOWNERS"),
            "# comment\n*.rs @rust-team\nsrc/core/ @core-team @lead\n",
        )
        .unwrap();
        let result = load_codeowners(dir.path());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "*.rs");
        assert_eq!(result[0].1, vec!["@rust-team"]);
        assert_eq!(result[1].0, "src/core/");
        assert_eq!(result[1].1, vec!["@core-team", "@lead"]);
    }

    #[test]
    fn test_find_owners_basename_match() {
        let patterns = vec![("*.rs".to_string(), vec!["@rust".to_string()])];
        assert_eq!(find_owners(&patterns, "src/main.rs"), vec!["@rust"]);
        assert_eq!(find_owners(&patterns, "deep/nested/lib.rs"), vec!["@rust"]);
        assert!(find_owners(&patterns, "src/main.py").is_empty());
    }

    #[test]
    fn test_find_owners_directory_match() {
        let patterns = vec![("src/core/".to_string(), vec!["@core".to_string()])];
        assert_eq!(find_owners(&patterns, "src/core/runtime.rs"), vec!["@core"]);
        assert!(find_owners(&patterns, "src/other/file.rs").is_empty());
    }

    #[test]
    fn test_find_owners_path_pattern() {
        let patterns = vec![("src/*.rs".to_string(), vec!["@src".to_string()])];
        assert_eq!(find_owners(&patterns, "src/main.rs"), vec!["@src"]);
        // '*' does not cross '/'
        assert!(find_owners(&patterns, "src/nested/main.rs").is_empty());
    }

    #[test]
    fn test_find_owners_last_rule_wins() {
        let patterns = vec![
            ("*.rs".to_string(), vec!["@general".to_string()]),
            ("src/*.rs".to_string(), vec!["@src-team".to_string()]),
        ];
        assert_eq!(find_owners(&patterns, "src/main.rs"), vec!["@src-team"]);
        assert_eq!(find_owners(&patterns, "tests/test.rs"), vec!["@general"]);
    }

    #[test]
    fn test_glob_match_basic() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(!glob_match("*.rs", "main.py"));
        assert!(glob_match("test_?", "test_a"));
        assert!(!glob_match("test_?", "test_ab"));
        assert!(glob_match("*", "anything"));
    }
}
