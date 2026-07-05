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
        // `variable_declarator` also catches CommonJS `require()` — see
        // `parse_js_require`. It's the same node kind `assignment_nodes()`
        // (resolver/lang_constants.rs) already walks for alias tracking; the
        // two extractions look for different shapes in the same nodes and
        // don't conflict (alias tracking wants a bare-identifier RHS,
        // `parse_js_require` wants a `require(...)` call RHS).
        "javascript" | "typescript" => &["import_statement", "variable_declarator"],
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

/// Strip an optional leading Rust visibility modifier from `text`, returning the
/// remainder. Handles `pub`, `pub(crate)`, `pub(super)`, `pub(self)`, and
/// `pub(in a::b)` — the parenthesized form may contain spaces, so we skip to the
/// matching `)` rather than splitting on whitespace.
fn strip_rust_visibility(text: &str) -> &str {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix("pub") else {
        return t;
    };
    let rest = rest.trim_start();
    match rest.strip_prefix('(') {
        Some(after) => match after.split_once(')') {
            Some((_, tail)) => tail.trim_start(),
            None => rest, // malformed; leave as-is
        },
        None => rest,
    }
}

fn parse_rust_import(text: &str) -> Option<ParsedImport> {
    // use a::b::c;  use a::b::{c, d};  use a::b as x;  use a::b::*;
    // and the pub/pub(...) re-export forms of each.
    let rest = strip_rust_visibility(text)
        .strip_prefix("use ")?
        .trim()
        .trim_start_matches("::");
    if let Some((prefix, list)) = rest.split_once("::{") {
        let list = list.trim_end_matches('}');
        let names = list.split(',').filter_map(bound_name).collect();
        Some(ParsedImport {
            module_name: prefix.trim().to_string(),
            imported_names: names,
        })
    } else {
        let after_as = rest
            .split_once(" as ")
            .map(|(m, _)| m.trim())
            .unwrap_or(rest);
        let is_glob = after_as.ends_with("::*");
        let path = after_as.trim_end_matches("::*");
        // Split the trailing item off the module path (`a::b::Item` -> module
        // `a::b`, item `Item`), matching the `use a::{b, c}` group branch above.
        // A glob (`a::b::*`) has no item to split off — `path` is already the
        // whole module. A bare single-segment `use foo;` also has nothing to
        // split — module and item are both `foo`.
        let module = if is_glob {
            path.to_string()
        } else {
            path.rsplit_once("::")
                .map(|(prefix, _)| prefix)
                .unwrap_or(path)
                .to_string()
        };
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
    parse_js_esm_import(text).or_else(|| parse_js_require(text))
}

fn parse_js_esm_import(text: &str) -> Option<ParsedImport> {
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

/// CommonJS `require()`, still common in real Node.js code (older packages,
/// TypeScript compiled to CommonJS) but structurally a call expression, not
/// an `import_statement` — this is fed `variable_declarator` text instead
/// (`NAME = require(...)` or `{ a, b as c } = require(...)`, no trailing
/// `;`, no `const`/`let`/`var` keyword — that's the parent node).
///
/// Only a literal string argument resolves to a module — `require(path)`
/// with a computed argument can't be statically attributed, so it's left
/// unresolved (`None`) rather than guessed at.
fn parse_js_require(text: &str) -> Option<ParsedImport> {
    let (lhs, rhs) = text.split_once('=')?;
    let after_require = rhs.trim().strip_prefix("require(")?.trim_start();
    let quote = after_require.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after_require[quote.len_utf8()..];
    let end = rest.find(quote)?;
    let module = rest[..end].to_string();
    if module.is_empty() {
        return None;
    }

    let lhs = lhs.trim();
    let mut names = Vec::new();
    if let Some(start) = lhs.find('{')
        && let Some(end) = lhs.find('}')
    {
        for seg in lhs[start + 1..end].split(',') {
            names.extend(bound_name(seg));
        }
    } else {
        names.extend(ident(lhs));
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
    fn rust_pub_use_reexport() {
        let i = one("pub use engine::Engine;\n", "rust");
        assert_eq!(i.module_name, "engine");
        assert_eq!(i.imported_names, vec!["Engine"]);
    }

    #[test]
    fn rust_pub_crate_use() {
        let i = one("pub(crate) use crate::a::b;\n", "rust");
        assert_eq!(i.module_name, "crate::a");
        assert_eq!(i.imported_names, vec!["b"]);
    }

    #[test]
    fn rust_use_single() {
        let i = one("use std::collections::HashMap;\n", "rust");
        // Corrected alongside the pub-use fix above: the module/item split now
        // matches the group-import branch (`a::b::{c}` -> module `a::b`) instead
        // of folding the item into the module path.
        assert_eq!(i.module_name, "std::collections");
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
    fn js_require_default() {
        let i = one("const foo = require('./foo');\n", "javascript");
        assert_eq!(i.module_name, "./foo");
        assert_eq!(i.imported_names, vec!["foo"]);
    }

    #[test]
    fn ts_require_double_quoted() {
        let i = one("const foo = require(\"./foo\");\n", "typescript");
        assert_eq!(i.module_name, "./foo");
        assert_eq!(i.imported_names, vec!["foo"]);
    }

    #[test]
    fn js_require_destructure() {
        let i = one("const { a, b: c } = require('./mod');\n", "javascript");
        assert_eq!(i.module_name, "./mod");
        assert_eq!(i.imported_names, vec!["a", "c"]);
    }

    /// A computed argument can't be statically attributed to a module —
    /// must not guess (see `parse_js_require`'s literal-only contract).
    #[test]
    fn js_require_with_computed_path_yields_no_import() {
        let v = extract_imports("const x = require(somePath);\n", "javascript");
        assert!(v.is_empty(), "expected no import, got {v:?}");
    }

    /// A plain (non-`require`) assignment must not be mistaken for an
    /// import now that `variable_declarator` is walked for JS/TS.
    #[test]
    fn js_plain_assignment_yields_no_import() {
        let v = extract_imports("const x = 5;\n", "javascript");
        assert!(v.is_empty(), "expected no import, got {v:?}");
    }

    #[test]
    fn java_import() {
        let i = one("import a.b.C;\n", "java");
        assert_eq!(i.module_name, "a.b.C");
        assert_eq!(i.imported_names, vec!["C"]);
    }
}
