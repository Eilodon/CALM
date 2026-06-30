//! Import-statement extraction for the six tier-0 languages.
//!
//! For each import we recover the module/path being imported and the names it
//! binds into the file. This feeds two things:
//!   * `import_edges` (powering the `dependencies` tool), and
//!   * the resolver's per-file `import_map` (so a call to an imported name is
//!     labelled `resolved` rather than `textual`).
//!
//! Extraction is text-based per import node: tree-sitter locates the import
//! constructs, then lightweight string parsing pulls out module + names. This is
//! markedly simpler than bespoke AST walks across six grammars and degrades
//! gracefully — an unrecognised form yields no binding rather than a crash.

use crate::indexer::parser::parse_tree;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImport {
    /// The module / path string as written (`a.b`, `a::b::c`, `./foo`, `"pkg/x"`).
    pub module_name: String,
    /// Names bound into the importing file (callable without a module prefix).
    pub imported_names: Vec<String>,
}

/// Tree-sitter node kinds that introduce an import, per language.
fn import_node_types(language: &str) -> &'static [&'static str] {
    match language {
        "python" => &["import_statement", "import_from_statement"],
        "rust" => &["use_declaration"],
        "go" => &["import_spec"],
        "javascript" | "typescript" => &["import_statement"],
        "java" => &["import_declaration"],
        _ => &[],
    }
}

pub fn extract_imports(source: &str, language: &str) -> Vec<ParsedImport> {
    let types = import_node_types(language);
    if types.is_empty() {
        return Vec::new();
    }
    let Some(tree) = parse_tree(source, language) else {
        return Vec::new();
    };
    extract_imports_from_tree(&tree, source, language)
}

/// Same as [`extract_imports`] but against an already-parsed tree, so callers
/// that need multiple extractions from one file can share a single
/// tree-sitter parse instead of re-parsing the same source once per extraction.
pub fn extract_imports_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
) -> Vec<ParsedImport> {
    let types = import_node_types(language);
    if types.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if types.contains(&node.kind()) {
            let text = &source[node.byte_range()];
            if let Some(imp) = parse_import(text, language) {
                out.push(imp);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn parse_import(text: &str, language: &str) -> Option<ParsedImport> {
    let text = text.trim().trim_end_matches(';').trim();
    match language {
        "python" => parse_python_import(text),
        "rust" => parse_rust_import(text),
        "go" => parse_go_import(text),
        "javascript" | "typescript" => parse_js_import(text),
        "java" => parse_java_import(text),
        _ => None,
    }
}

/// `name as alias` / `name` → the bound identifier (alias wins).
fn bound_name(segment: &str) -> Option<String> {
    let seg = segment.trim();
    if seg.is_empty() || seg == "*" {
        return None;
    }
    if let Some((_, alias)) = seg.split_once(" as ") {
        return ident(alias);
    }
    // For a dotted/path module, the bound name is the last segment.
    let last = seg.rsplit(['.', ':', '/']).next().unwrap_or(seg);
    ident(last)
}

/// Keep a leading identifier (drop generics, parens, quotes, whitespace).
fn ident(s: &str) -> Option<String> {
    let t: String = s
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '{' || c == '}')
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if t.is_empty() { None } else { Some(t) }
}

fn parse_python_import(text: &str) -> Option<ParsedImport> {
    if let Some(rest) = text.strip_prefix("from ") {
        // from <module> import a, b as c, *
        let (module, names) = rest.split_once(" import ")?;
        let module = module.trim().to_string();
        let imported = names
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split(',')
            .filter_map(bound_name)
            .collect();
        Some(ParsedImport {
            module_name: module,
            imported_names: imported,
        })
    } else if let Some(rest) = text.strip_prefix("import ") {
        // import a.b.c, x as y
        let first = rest.split(',').next()?.trim();
        let module = first
            .split_once(" as ")
            .map(|(m, _)| m.trim())
            .unwrap_or(first)
            .to_string();
        let names = rest.split(',').filter_map(bound_name).collect();
        Some(ParsedImport {
            module_name: module,
            imported_names: names,
        })
    } else {
        None
    }
}

fn parse_rust_import(text: &str) -> Option<ParsedImport> {
    // use a::b::c;  use a::b::{c, d};  use a::b as x;  use a::b::*;
    let rest = text.strip_prefix("use ")?.trim().trim_start_matches("::");
    if let Some((prefix, list)) = rest.split_once("::{") {
        let list = list.trim_end_matches('}');
        let names = list.split(',').filter_map(bound_name).collect();
        Some(ParsedImport {
            module_name: prefix.trim().to_string(),
            imported_names: names,
        })
    } else {
        let module = rest
            .split_once(" as ")
            .map(|(m, _)| m.trim())
            .unwrap_or(rest)
            .trim_end_matches("::*")
            .to_string();
        let names = bound_name(rest).into_iter().collect();
        Some(ParsedImport {
            module_name: module,
            imported_names: names,
        })
    }
}

fn parse_go_import(text: &str) -> Option<ParsedImport> {
    // import_spec: optional alias then a quoted path, e.g. `m "fmt"` or `"a/b"`.
    let text = text.trim();
    let (alias, path) = match text.split_once('"') {
        Some((before, after)) => {
            let path = after.split('"').next().unwrap_or("");
            let alias = before.trim();
            (
                if alias.is_empty() {
                    None
                } else {
                    Some(alias.to_string())
                },
                path.to_string(),
            )
        }
        None => return None,
    };
    if path.is_empty() {
        return None;
    }
    let name = alias.or_else(|| ident(path.rsplit('/').next().unwrap_or(&path)));
    Some(ParsedImport {
        module_name: path,
        imported_names: name.into_iter().collect(),
    })
}

fn parse_js_import(text: &str) -> Option<ParsedImport> {
    // import { a, b as c } from 'mod';  import x from 'mod';  import * as ns from 'mod';
    let (clause, module) = text.split_once(" from ")?;
    let module = module
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    let clause = clause.strip_prefix("import").unwrap_or(clause).trim();
    let mut names = Vec::new();
    if let Some(start) = clause.find('{')
        && let Some(end) = clause.find('}')
    {
        for seg in clause[start + 1..end].split(',') {
            names.extend(bound_name(seg));
        }
    }
    // default / namespace import: leading bare identifier or `* as ns`
    let head = clause.split(['{', ',']).next().unwrap_or("").trim();
    if let Some(ns) = head.strip_prefix("* as ") {
        names.extend(ident(ns));
    } else if !head.is_empty() && !head.starts_with('{') && !head.starts_with('*') {
        names.extend(ident(head));
    }
    Some(ParsedImport {
        module_name: module,
        imported_names: names,
    })
}

fn parse_java_import(text: &str) -> Option<ParsedImport> {
    // import a.b.C;  import static a.b.C.m;  import a.b.*;
    let rest = text
        .strip_prefix("import ")?
        .trim()
        .strip_prefix("static ")
        .unwrap_or_else(|| text.strip_prefix("import ").unwrap().trim())
        .trim();
    let module = rest.to_string();
    let names = if rest.ends_with(".*") {
        Vec::new()
    } else {
        bound_name(rest).into_iter().collect()
    };
    Some(ParsedImport {
        module_name: module,
        imported_names: names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str, lang: &str) -> ParsedImport {
        let v = extract_imports(source, lang);
        assert_eq!(v.len(), 1, "expected exactly one import in: {source}");
        v.into_iter().next().unwrap()
    }

    #[test]
    fn python_from_import() {
        let i = one("from a.b import c, d as e\n", "python");
        assert_eq!(i.module_name, "a.b");
        assert_eq!(i.imported_names, vec!["c", "e"]);
    }

    #[test]
    fn python_plain_import() {
        let i = one("import os.path as p\n", "python");
        assert_eq!(i.module_name, "os.path");
        assert_eq!(i.imported_names, vec!["p"]);
    }

    #[test]
    fn rust_use_group() {
        let i = one("use crate::a::{b, c};\n", "rust");
        assert_eq!(i.module_name, "crate::a");
        assert_eq!(i.imported_names, vec!["b", "c"]);
    }

    #[test]
    fn rust_use_single() {
        let i = one("use std::collections::HashMap;\n", "rust");
        assert_eq!(i.module_name, "std::collections::HashMap");
        assert_eq!(i.imported_names, vec!["HashMap"]);
    }

    #[test]
    fn go_import() {
        let i = one("package m\nimport \"fmt\"\n", "go");
        assert_eq!(i.module_name, "fmt");
        assert_eq!(i.imported_names, vec!["fmt"]);
    }

    #[test]
    fn js_named_import() {
        let i = one("import { a, b as c } from './mod';\n", "javascript");
        assert_eq!(i.module_name, "./mod");
        assert_eq!(i.imported_names, vec!["a", "c"]);
    }

    #[test]
    fn java_import() {
        let i = one("import a.b.C;\n", "java");
        assert_eq!(i.module_name, "a.b.C");
        assert_eq!(i.imported_names, vec!["C"]);
    }
}
