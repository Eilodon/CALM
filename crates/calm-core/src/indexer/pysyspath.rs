//! Python `sys.path` source-root discovery: recovers the statically-literal
//! `sys.path.insert(...)` / `sys.path.append(...)` calls a file makes before
//! its own imports, so `from mcp_client import MCPClient` in a script that
//! prepends a sibling `lib/` directory resolves to the real file instead of
//! staying `to_path = NULL`.
//!
//! Mirrors `psr4::Psr4Map`/`csharp_namespace::NamespaceMap`'s "read real files
//! once per indexing run, build an in-memory map, thread it through the
//! pipeline" pattern — like `NamespaceMap` (and unlike `CrateMap`/`Psr4Map`)
//! there is no single manifest whose mtime tracks this map, so
//! `cached_resolution_maps`' TTL is what bounds its staleness.
//!
//! Deliberately narrow: only insertions anchored at `__file__` are honoured.
//! A `sys.path.insert(0, os.environ["X"])` or an absolute `"/opt/foo"` cannot
//! be tied to a repo-relative directory without executing the program, so it
//! yields no root rather than a guess.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Only the head of each file is scanned: a `sys.path` mutation has to run
/// *before* the import that depends on it, and Python puts imports at the top
/// of the module. Bounds the read on machine-generated `.py` files.
const SCAN_BYTES: u64 = 64 * 1024;

#[derive(Clone, Default)]
pub struct PySysPathMap {
    /// importing file (repo-relative) → extra source roots (repo-relative, no
    /// trailing `/`), in the order they appear in the file. Only files that
    /// actually manipulate `sys.path` get an entry.
    roots_by_file: HashMap<String, Vec<String>>,
}

impl PySysPathMap {
    /// Never fails — an empty map just means no Python file in the project
    /// manipulates `sys.path` in a statically-recoverable way, and the other
    /// resolution branches carry on unchanged (same silent-degrade philosophy
    /// as `CrateMap`/`Psr4Map`/`NamespaceMap`).
    pub fn build(project_root: &Path) -> Self {
        let mut roots_by_file: HashMap<String, Vec<String>> = HashMap::new();
        for entry in crate::walk::build_walker(project_root, &[], false) {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("py") {
                continue;
            }
            let Some(rel) = rel_file(project_root, path) else {
                continue;
            };
            let Ok(file) = std::fs::File::open(path) else {
                continue;
            };
            let mut head = Vec::new();
            if std::io::BufReader::new(file)
                .take(SCAN_BYTES)
                .read_to_end(&mut head)
                .is_err()
            {
                continue;
            }
            // Lossy on purpose: a truncated multi-byte char at the SCAN_BYTES
            // boundary must not throw away the roots found before it.
            let source = String::from_utf8_lossy(&head);
            if !source.contains("sys.path") {
                continue;
            }
            let roots = extract_roots(&rel, &source);
            if !roots.is_empty() {
                roots_by_file.insert(rel, roots);
            }
        }
        Self { roots_by_file }
    }

    pub fn is_empty(&self) -> bool {
        self.roots_by_file.is_empty()
    }

    /// Extra source roots in effect for imports written in `from_path`.
    /// Empty for every file that doesn't touch `sys.path`.
    pub fn roots_for(&self, from_path: &str) -> &[String] {
        self.roots_by_file
            .get(from_path)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Project-root-relative, forward-slashed file path, or `None` if `abs_file`
/// isn't under `project_root` (twin of `csharp_namespace::rel_file` — both are
/// `build_walker` companions, kept private to their own module).
fn rel_file(project_root: &Path, abs_file: &Path) -> Option<String> {
    let rel = abs_file.strip_prefix(project_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// The argument text of every `sys.path.insert(`/`sys.path.append(` call in
/// `source`, paren-balanced so a nested `str(Path(...))` doesn't cut the
/// argument short.
fn syspath_call_args(source: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for marker in ["sys.path.insert(", "sys.path.append("] {
        let mut from = 0usize;
        while let Some(hit) = source[from..].find(marker) {
            let open = from + hit + marker.len();
            let mut depth = 1i32;
            let mut end = open;
            for (i, c) in source[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if depth == 0 && end > open {
                out.push(&source[open..end]);
                from = end;
            } else {
                // Unbalanced (truncated at SCAN_BYTES, or a `(` inside a
                // string literal) — stop scanning this marker rather than
                // rescanning the same position forever.
                break;
            }
        }
    }
    out
}

/// How many directory levels above the importing file's own directory the
/// argument points at, for the `__file__`-anchored forms:
/// `Path(__file__).resolve().parents[N]` → `N`;
/// `Path(__file__).parent` / `os.path.dirname(__file__)` → `0`;
/// each further `.parent` / `dirname(` → one more.
fn climb_levels(arg: &str) -> usize {
    if let Some(idx) = arg.find("parents[") {
        let digits: String = arg[idx + "parents[".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(n) = digits.parse::<usize>() {
            return n;
        }
    }
    // `.parents` must not also count as a `.parent` hit.
    let parents = arg
        .match_indices(".parent")
        .filter(|(i, _)| !arg[i + ".parent".len()..].starts_with('s'));
    let dirnames = arg.matches("dirname(").count();
    (parents.count() + dirnames).saturating_sub(1)
}

/// Quoted string literals in the argument, in order — the sub-directory
/// components joined onto the climbed directory (`parents[1] / "lib"`).
fn literal_segments(arg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = arg.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let q = bytes[i];
        if (q == b'"' || q == b'\'')
            && let Some(close) = arg[i + 1..].find(q as char)
        {
            let lit = &arg[i + 1..i + 1 + close];
            // An absolute path or a `..` escape can't be anchored at the
            // importing file's directory; drop the whole insertion.
            if !lit.is_empty() && !lit.starts_with('/') && !lit.contains("..") {
                out.push(lit.trim_matches('/').to_string());
            }
            i += close + 2;
            continue;
        }
        i += 1;
    }
    out
}

fn extract_roots(from_path: &str, source: &str) -> Vec<String> {
    let file_dir = from_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut out: Vec<String> = Vec::new();
    for arg in syspath_call_args(source) {
        if !arg.contains("__file__") {
            continue;
        }
        let mut dir = file_dir.to_string();
        for _ in 0..climb_levels(arg) {
            match dir.rsplit_once('/') {
                Some((parent, _)) => dir = parent.to_string(),
                // Climbing past the project root — the inserted path is
                // outside the indexed tree, so nothing can resolve under it.
                None if dir.is_empty() => return out,
                None => dir = String::new(),
            }
        }
        for seg in literal_segments(arg) {
            dir = if dir.is_empty() {
                seg
            } else {
                format!("{dir}/{seg}")
            };
        }
        if !dir.is_empty() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(from: &str, src: &str) -> Vec<String> {
        extract_roots(from, src)
    }

    #[test]
    fn parents_index_with_subdir() {
        // The exact form used by this repo's own benchmark runners.
        let src = r#"sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))"#;
        assert_eq!(
            roots("benchmarks/b10_real_competitor_ab/run_benchmark.py", src),
            vec!["benchmarks/lib".to_string()]
        );
    }

    #[test]
    fn single_parent_is_the_files_own_dir() {
        let src = r#"sys.path.insert(0, str(Path(__file__).parent / "lib"))"#;
        assert_eq!(roots("a/b/run.py", src), vec!["a/b/lib".to_string()]);
    }

    #[test]
    fn repeated_parent_climbs_one_level_each() {
        let src = r#"sys.path.append(str(Path(__file__).parent.parent / "lib"))"#;
        assert_eq!(roots("a/b/run.py", src), vec!["a/lib".to_string()]);
    }

    #[test]
    fn os_path_dirname_form() {
        let src = r#"sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "vendor"))"#;
        assert_eq!(roots("tools/run.py", src), vec!["tools/vendor".to_string()]);
    }

    #[test]
    fn parents_without_subdir() {
        let src = r#"sys.path.insert(0, str(Path(__file__).resolve().parents[2]))"#;
        assert_eq!(roots("a/b/c/run.py", src), vec!["a".to_string()]);
    }

    #[test]
    fn non_file_anchored_insertion_is_ignored() {
        // Nothing ties these to a repo-relative directory statically.
        assert!(roots("a/run.py", r#"sys.path.insert(0, "/opt/thing")"#).is_empty());
        assert!(roots("a/run.py", r#"sys.path.insert(0, os.environ["LIBS"])"#).is_empty());
    }

    #[test]
    fn dotdot_literal_is_refused() {
        let src = r#"sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../lib"))"#;
        // `..` is handled by the climb count, not by a literal escape — a
        // literal `..` would silently point outside the anchored directory.
        assert_eq!(roots("a/b/run.py", src), vec!["a/b".to_string()]);
    }

    #[test]
    fn climbing_past_project_root_yields_nothing() {
        let src = r#"sys.path.insert(0, str(Path(__file__).resolve().parents[5] / "lib"))"#;
        assert!(roots("a/run.py", src).is_empty());
    }

    #[test]
    fn multiple_insertions_preserved_in_order() {
        let src = r#"
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "vendor"))
"#;
        assert_eq!(
            roots("bench/x/run.py", src),
            vec!["bench/lib".to_string(), "bench/vendor".to_string()]
        );
    }

    #[test]
    fn build_finds_roots_and_skips_untouched_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("bench/x")).unwrap();
        std::fs::create_dir_all(root.join("bench/lib")).unwrap();
        std::fs::write(
            root.join("bench/x/run.py"),
            "import sys\nsys.path.insert(0, str(Path(__file__).resolve().parents[1] / \"lib\"))\nfrom helper import go\n",
        )
        .unwrap();
        std::fs::write(root.join("bench/lib/helper.py"), "def go():\n    pass\n").unwrap();

        let map = PySysPathMap::build(root);
        assert_eq!(map.roots_for("bench/x/run.py"), ["bench/lib".to_string()]);
        // A file that never touches sys.path gets no entry at all.
        assert!(map.roots_for("bench/lib/helper.py").is_empty());
    }

    #[test]
    fn empty_project_builds_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PySysPathMap::build(dir.path()).is_empty());
    }
}
