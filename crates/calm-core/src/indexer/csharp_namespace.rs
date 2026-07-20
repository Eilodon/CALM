//! C# namespace scan (8-language plan P1.5's "using -> namespace" remainder):
//! reads every real `.cs` file's `namespace`/`file_scoped_namespace_declaration`
//! and records which files declare types inside which namespace — mirrors
//! `crate_map::CrateMap::build`'s "read real files, build an in-memory map"
//! pattern, so `using X;` can be checked against real namespace declarations
//! instead of a directory-matches-namespace convention (C# namespaces don't
//! reliably mirror directory structure the way Go packages do).
//!
//! Unlike `CrateMap`/`Psr4Map`, this is threaded only into `rebuild_graph`
//! (same call site those two already use), not into `extract_file_data` —
//! doing the latter would require building the whole-project map *before*
//! the per-file, parallel `extract_file_data` pass starts, a bigger pipeline
//! reorder for no extra correctness: `rebuild_graph` already runs after every
//! file is parsed and already has `call_sites`/`import_edges` to work from.

use std::collections::HashMap;
use std::path::Path;

use crate::indexer::lang_constants::{LangConstants, get_lang_constants};
use crate::indexer::parser::parse_tree;

#[derive(Clone)]
pub struct NamespaceMap {
    /// namespace (dotted, exactly as written in a `namespace X.Y { }` /
    /// `namespace X.Y;` declaration) -> every project-root-relative,
    /// forward-slashed `.cs` file that declares at least one type inside it.
    /// Deduped; a namespace can legitimately span many files (it's not a
    /// 1:1 crate/src-root relationship like `CrateMap`'s).
    files_by_namespace: HashMap<String, Vec<String>>,
}

impl NamespaceMap {
    /// Never fails — an empty map just means the "using -> namespace"
    /// upgrades below are skipped, same silent-degrade philosophy as
    /// `CrateMap`/`Psr4Map`.
    pub fn build(project_root: &Path) -> Self {
        let mut files_by_namespace: HashMap<String, Vec<String>> = HashMap::new();
        let Some(consts) = get_lang_constants("csharp") else {
            return Self { files_by_namespace };
        };
        for entry in crate::walk::build_walker(project_root, &[], false) {
            let Ok(entry) = entry else { continue };
            if entry.path().extension().and_then(|e| e.to_str()) != Some("cs") {
                continue;
            }
            let path = entry.path();
            let Ok(source) = std::fs::read_to_string(path) else {
                continue;
            };
            let Some(rel) = rel_file(project_root, path) else {
                continue;
            };
            for ns in namespaces_declared_in(&source, &consts) {
                let files = files_by_namespace.entry(ns).or_default();
                if !files.contains(&rel) {
                    files.push(rel.clone());
                }
            }
        }
        Self { files_by_namespace }
    }

    pub fn is_empty(&self) -> bool {
        self.files_by_namespace.is_empty()
    }

    /// The single file that declares `namespace` — `None` if no file does,
    /// or if 2+ do. `import_edges.to_path` is single-valued (one target per
    /// row), so a namespace spanning multiple files resolves to no target
    /// rather than an arbitrary guess — same "don't fabricate a wrong
    /// answer" rule every other silent-degrade path in this indexer follows.
    pub fn resolve(&self, namespace: &str) -> Option<&str> {
        match self.files_by_namespace.get(namespace) {
            Some(files) if files.len() == 1 => Some(files[0].as_str()),
            _ => None,
        }
    }

    /// Does `path` declare at least one type inside `namespace`? Used by
    /// `rebuild_graph`'s same-namespace candidate narrowing to check whether
    /// a `by_name_class` candidate lives in one of the caller's active
    /// `using` namespaces.
    pub fn contains(&self, namespace: &str, path: &str) -> bool {
        self.files_by_namespace
            .get(namespace)
            .is_some_and(|files| files.iter().any(|f| f == path))
    }
}

/// Project-root-relative, forward-slashed file path, or `None` if `abs_file`
/// isn't under `project_root` (shouldn't happen — `build_walker` only ever
/// yields entries under the root it was given).
fn rel_file(project_root: &Path, abs_file: &Path) -> Option<String> {
    let rel = abs_file.strip_prefix(project_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Every namespace with at least one type declared directly in `source`,
/// deduped. Handles both block-scoped (`namespace X { ... }`, nestable) and
/// C# 10's file-scoped (`namespace X;`, applies to the rest of the file)
/// forms — confirmed via the real grammar that both use a `name` field
/// (holding either a plain `identifier` or a dotted `qualified_name`; taking
/// the field's raw source text handles either shape without unwrapping).
fn namespaces_declared_in(source: &str, consts: &LangConstants) -> Vec<String> {
    let Some(tree) = parse_tree(source, "csharp") else {
        return Vec::new();
    };
    let root = tree.root_node();
    // A file-scoped namespace declaration has no body — it sets the default
    // namespace for every subsequent top-level declaration in the file, so
    // it's read once, up front, rather than threaded through the recursive
    // walk below (which only needs to react to block-scoped declarations
    // that Do have a body to descend into).
    let mut file_ns = String::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "file_scoped_namespace_declaration"
            && let Some(n) = child.child_by_field_name("name")
        {
            file_ns = source[n.byte_range()].to_string();
            break;
        }
    }
    let mut out = Vec::new();
    walk_namespaces(root, source, &file_ns, consts, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk_namespaces(
    node: tree_sitter::Node,
    source: &str,
    ns: &str,
    consts: &LangConstants,
    out: &mut Vec<String>,
) {
    let child_ns: String = if node.kind() == "namespace_declaration" {
        node.child_by_field_name("name")
            .map(|n| source[n.byte_range()].to_string())
            .unwrap_or_else(|| ns.to_string())
    } else {
        ns.to_string()
    };
    // The global (unnamed) namespace is never a valid `using` target — skip
    // recording it so an unnamespaced file (or type) doesn't pollute the map
    // with a bogus `""` key.
    if !child_ns.is_empty() && consts.class_node_types.contains(&node.kind()) {
        out.push(child_ns.clone());
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        walk_namespaces(c, source, &child_ns, consts, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ci_ns_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn maps_block_scoped_namespace_to_its_file() {
        let dir = tmp("block");
        write(
            &dir,
            "helper.cs",
            "namespace MultiLang\n{\n    public static class Helper {}\n}\n",
        );
        let map = NamespaceMap::build(&dir);
        assert_eq!(map.resolve("MultiLang"), Some("helper.cs"));
        assert!(map.contains("MultiLang", "helper.cs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maps_file_scoped_namespace_to_its_file() {
        let dir = tmp("filescoped");
        write(
            &dir,
            "helper.cs",
            "namespace MultiLang.Sub;\n\npublic static class Helper {}\n",
        );
        let map = NamespaceMap::build(&dir);
        assert_eq!(map.resolve("MultiLang.Sub"), Some("helper.cs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn namespace_spanning_two_files_is_ambiguous_for_resolve_but_contains_both() {
        let dir = tmp("twofiles");
        write(
            &dir,
            "a.cs",
            "namespace MultiLang\n{\n    public class A {}\n}\n",
        );
        write(
            &dir,
            "b.cs",
            "namespace MultiLang\n{\n    public class B {}\n}\n",
        );
        let map = NamespaceMap::build(&dir);
        assert_eq!(
            map.resolve("MultiLang"),
            None,
            "2 files share the namespace — no single winner, no schema for multi-valued to_path"
        );
        assert!(map.contains("MultiLang", "a.cs"));
        assert!(map.contains("MultiLang", "b.cs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_class_name_in_different_namespaces_is_distinguishable() {
        let dir = tmp("collision");
        write(
            &dir,
            "a.cs",
            "namespace MultiLang\n{\n    public class Helper {}\n}\n",
        );
        write(
            &dir,
            "b.cs",
            "namespace Elsewhere\n{\n    public class Helper {}\n}\n",
        );
        let map = NamespaceMap::build(&dir);
        assert!(map.contains("MultiLang", "a.cs"));
        assert!(!map.contains("MultiLang", "b.cs"));
        assert!(map.contains("Elsewhere", "b.cs"));
        assert!(!map.contains("Elsewhere", "a.cs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_namespace_yields_empty_map() {
        let dir = tmp("none");
        write(&dir, "program.cs", "class Program {}\n");
        let map = NamespaceMap::build(&dir);
        assert!(map.is_empty());
        assert_eq!(map.resolve("MultiLang"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_cs_files_yields_empty_map() {
        let dir = tmp("nocs");
        let map = NamespaceMap::build(&dir);
        assert!(map.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
