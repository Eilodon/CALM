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
        // R has no import statement: `library(pkg)`/`require(pkg)` are ordinary
        // calls loading an installed CRAN package (never a repo file, so this
        // never resolves to a cross-file edge the way Python/JS imports can —
        // it's recorded purely as external-dependency metadata). Every `call`
        // node is walked and `parse_r_library` rejects the vast majority that
        // aren't library/require — same cost shape as JS's `variable_declarator`
        // firing on every declaration to catch the rare `require()` among them.
        "r" => &["call"],
        // C/C++ `#include "x.h"` — see `parse_c_include` for why `<...>`
        // system headers never even produce a `ParsedImport` (skipped there,
        // not here, so both languages share one node-kind list).
        "c" | "cpp" => &["preproc_include"],
        // PHP: 4 distinct node kinds, one per keyword (confirmed via the
        // real grammar — no single "require-like" wrapper node exists), plus
        // `use App\Service\Foo;` (`namespace_use_declaration`).
        "php" => &[
            "require_expression",
            "require_once_expression",
            "include_expression",
            "include_once_expression",
            "namespace_use_declaration",
        ],
        // `using MultiLang;` — brings a namespace's types into scope. Unlike
        // every other language's import here, it binds no single name (see
        // `parse_csharp_using`), so its only job is feeding `import_edges`
        // and — via `csharp_namespace::NamespaceMap` (8-language plan P1.5's
        // "using -> namespace" remainder) — the caller's active-namespace
        // set consulted at `rebuild_graph` time.
        "csharp" => &["using_directive"],
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
        "r" => parse_r_library(text),
        "c" | "cpp" => parse_c_include(text),
        "php" => parse_php_import(text),
        "csharp" => parse_csharp_using(text),
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

fn parse_r_library(text: &str) -> Option<ParsedImport> {
    // `library(pkg)`, `require(pkg)`, `requireNamespace("pkg")` — argument may
    // be a bare identifier (NSE convention) or a quoted string; either way
    // take everything up to the first `,`/`)` as the package name.
    let rest = text
        .strip_prefix("library(")
        .or_else(|| text.strip_prefix("require("))
        .or_else(|| text.strip_prefix("requireNamespace("))?
        .trim_start();
    let end = rest.find([',', ')'])?;
    let module = rest[..end].trim().trim_matches(['"', '\'']).to_string();
    if module.is_empty()
        || !module
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '.')
    {
        return None;
    }
    Some(ParsedImport {
        module_name: module,
        imported_names: Vec::new(),
    })
}

/// C/C++ `#include "x.h"` — quoted (repo-local) includes only; `#include
/// <sys/header.h>` system/library headers are deliberately skipped (never a
/// repo file, and there's no compiler include-path to search here anyway).
/// `imported_names` stays empty: unlike Python/JS's per-name imports, a C
/// `#include` doesn't bind any single identifier into scope — it's a raw
/// textual paste of the whole header, so every name it declares becomes
/// available, not just one. That means this doesn't help Tier-1's
/// `import_map` lookup (bare C function calls were never scoped by an import
/// name to begin with — see 8-language plan P1.3's same-dir tier for the
/// actual fix there); its value is the file-level `import_edges` row itself
/// (`dependencies` tool, hub/coreness graph) once `resolve_import_targets`
/// resolves `to_path`.
fn parse_c_include(text: &str) -> Option<ParsedImport> {
    let rest = text.trim().strip_prefix("#include")?.trim();
    let path = rest.strip_prefix('"')?.split('"').next()?;
    if path.is_empty() {
        return None;
    }
    // Route through the relative-import branch of `resolve_module_to_path`
    // (pipeline.rs) rather than its dotted/scoped-module branch — the latter
    // would mangle a literal '.' in "helper.h" into a fake path segment
    // ("helper/h"). A leading "./"/"../" is preserved as written (a header
    // can legitimately `#include "../shared/x.h"`); a bare filename gets one
    // synthesized so it still resolves relative to the including file's own
    // directory, the C convention for quoted includes.
    let module_name = if path.starts_with("./") || path.starts_with("../") {
        path.to_string()
    } else {
        format!("./{path}")
    };
    Some(ParsedImport {
        module_name,
        imported_names: Vec::new(),
    })
}

fn parse_php_import(text: &str) -> Option<ParsedImport> {
    if text.starts_with("use") {
        parse_php_use(text)
    } else {
        parse_php_require(text)
    }
}

/// `require`/`require_once`/`include`/`include_once` — confirmed via the
/// real grammar that these are 4 distinct node kinds (no shared wrapper),
/// so `import_node_types` lists all 4 and this dispatches on whichever
/// keyword prefix matched.
fn parse_php_require(text: &str) -> Option<ParsedImport> {
    let rest = text
        .strip_prefix("require_once")
        .or_else(|| text.strip_prefix("include_once"))
        .or_else(|| text.strip_prefix("require"))
        .or_else(|| text.strip_prefix("include"))?
        .trim();
    // The require/include target is always a quoted string literal
    // somewhere in `rest` — either standalone ('x.php') or the tail of a
    // `__DIR__ . '/x.php'` concatenation (confirmed the most common real
    // pattern via the real grammar). Take the first quoted string's content
    // either way; `__DIR__` (or its absence) doesn't change the module path
    // text itself, only anchors resolution to the including file's own
    // directory — which the "./" prefix below already does via
    // `resolve_module_to_path`'s relative-import branch, mirroring
    // `parse_c_include`.
    let quote_idx = rest.find(['\'', '"'])?;
    let quote_char = rest.as_bytes()[quote_idx] as char;
    let path = rest[quote_idx + 1..].split(quote_char).next()?;
    if path.is_empty() {
        return None;
    }
    // `__DIR__ . '/x.php'`-style concatenation's string half conventionally
    // starts with '/' (it's just the tail after __DIR__, not an absolute
    // filesystem path) — strip it before prefixing "./" so this doesn't
    // produce a doubled ".//x.php".
    let module_name = if path.starts_with("./") || path.starts_with("../") {
        path.to_string()
    } else if let Some(rest) = path.strip_prefix('/') {
        format!("./{rest}")
    } else {
        format!("./{path}")
    };
    Some(ParsedImport {
        module_name,
        imported_names: Vec::new(), // pastes the whole file, binds no single name
    })
}
/// `use App\Service\Foo;` / `use App\Service\Foo as Bar;` /
/// `use function App\helper;` / `use const App\FOO;`. `module_name` keeps
/// the raw backslash-separated namespace path as written — resolving it to
/// a real file needs PSR-4 (composer.json's `autoload.psr-4`), not the
/// generic dotted-module convention scan `resolve_module_to_path` already
/// does for other languages (that scan only replaces `::`/`.`, not PHP's
/// `\`, and has no PSR-4 prefix→dir table to consult) — see the PSR-4
/// resolver this module's `imported_names` feeds into.
fn parse_php_use(text: &str) -> Option<ParsedImport> {
    let rest = text.strip_prefix("use")?.trim();
    let rest = rest
        .strip_prefix("function ")
        .or_else(|| rest.strip_prefix("const "))
        .unwrap_or(rest)
        .trim();
    let (path, alias) = match rest.split_once(" as ") {
        Some((p, a)) => (p.trim(), Some(a.trim())),
        None => (rest, None),
    };
    let path = path.trim_start_matches('\\'); // a leading \ is a no-op fully-qualified marker
    if path.is_empty() {
        return None;
    }
    let name = alias
        .map(str::to_string)
        .or_else(|| ident(path.rsplit('\\').next().unwrap_or(path)));
    Some(ParsedImport {
        module_name: path.to_string(),
        imported_names: name.into_iter().collect(),
    })
}

/// `using MultiLang;` / `using System.Collections.Generic;` — brings every
/// type in the named namespace into unqualified/qualified-shorthand scope
/// for the rest of the file. Unlike PHP's `use App\Service\Foo;` this binds
/// no single name (there's no "Foo" to extract — the whole namespace is in
/// play), so `imported_names` stays empty, same treatment as `#include`/
/// `require`. `module_name` keeps the dotted namespace text as written;
/// `csharp_namespace::NamespaceMap` (built from real `namespace`
/// declarations, not this text) is what actually resolves it to file(s).
///
/// `using static Type;` (imports a type's *static members*, not a
/// namespace) and `using Alias = Namespace.Type;` (a type alias) are
/// deliberately left unhandled (`None`) — neither is "this file can now see
/// namespace X", the only shape `NamespaceMap` resolves; confirmed via the
/// real grammar that both share `using_directive`'s node kind with the
/// plain form, so the distinction has to be made here, from the text.
fn parse_csharp_using(text: &str) -> Option<ParsedImport> {
    let rest = text.strip_prefix("using")?.trim();
    if rest.is_empty() || rest.starts_with("static ") || rest.contains('=') {
        return None;
    }
    Some(ParsedImport {
        module_name: rest.to_string(),
        imported_names: Vec::new(),
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

    #[test]
    #[cfg(feature = "lang-r")]
    fn r_library_call_import() {
        let i = one("library(dplyr)\n", "r");
        assert_eq!(i.module_name, "dplyr");
    }

    #[test]
    #[cfg(feature = "lang-r")]
    fn r_require_quoted_string_import() {
        let i = one("require(\"ggplot2\")\n", "r");
        assert_eq!(i.module_name, "ggplot2");
    }

    /// An ordinary call must not be mistaken for a `library()`/`require()`
    /// import now that every `call` node is walked for R.
    #[test]
    #[cfg(feature = "lang-r")]
    fn r_ordinary_call_yields_no_import() {
        let v = extract_imports("mean(x)\n", "r");
        assert!(v.is_empty(), "expected no import, got {v:?}");
    }

    #[test]
    fn c_include_quoted_header() {
        let i = one("#include \"helper.h\"\n", "c");
        assert_eq!(i.module_name, "./helper.h");
        assert!(
            i.imported_names.is_empty(),
            "a bare #include doesn't bind any single name into scope"
        );
    }

    #[test]
    fn cpp_include_quoted_header_preserves_subdir_and_dotdot() {
        let i = one("#include \"sub/x.h\"\n", "cpp");
        assert_eq!(i.module_name, "./sub/x.h");
        let i = one("#include \"../shared/y.h\"\n", "cpp");
        assert_eq!(i.module_name, "../shared/y.h");
    }

    #[test]
    fn c_include_system_header_yields_no_import() {
        let v = extract_imports("#include <stdio.h>\n", "c");
        assert!(
            v.is_empty(),
            "system/library headers are never a repo file — expected no import, got {v:?}"
        );
    }

    #[test]
    fn php_require_once_dir_concat() {
        let i = one("<?php\nrequire_once __DIR__ . '/x.php';\n", "php");
        assert_eq!(i.module_name, "./x.php");
        assert!(i.imported_names.is_empty());
    }
    #[test]
    fn php_require_bare_string() {
        let i = one("<?php\nrequire 'config.php';\n", "php");
        assert_eq!(i.module_name, "./config.php");
    }
    #[test]
    fn php_include_once_bare_string() {
        let i = one("<?php\ninclude_once 'y.php';\n", "php");
        assert_eq!(i.module_name, "./y.php");
    }
    #[test]
    fn php_use_qualified_name() {
        let i = one("<?php\nuse App\\Service\\Foo;\n", "php");
        assert_eq!(i.module_name, "App\\Service\\Foo");
        assert_eq!(i.imported_names, vec!["Foo".to_string()]);
    }
    #[test]
    fn php_use_with_alias() {
        let i = one("<?php\nuse App\\Service\\Foo as Bar;\n", "php");
        assert_eq!(i.module_name, "App\\Service\\Foo");
        assert_eq!(
            i.imported_names,
            vec!["Bar".to_string()],
            "an alias binds the alias name, not the original"
        );
    }

    #[test]
    fn csharp_using_plain_namespace() {
        let i = one("using MultiLang;\n", "csharp");
        assert_eq!(i.module_name, "MultiLang");
        assert!(
            i.imported_names.is_empty(),
            "using binds no single name, unlike PHP's use"
        );
    }
    #[test]
    fn csharp_using_dotted_namespace() {
        let i = one("using System.Collections.Generic;\n", "csharp");
        assert_eq!(i.module_name, "System.Collections.Generic");
    }
    #[test]
    fn csharp_using_static_yields_no_import() {
        let v = extract_imports("using static System.Console;\n", "csharp");
        assert!(
            v.is_empty(),
            "using static imports a type's members, not a namespace — expected no import, got {v:?}"
        );
    }
    #[test]
    fn csharp_using_alias_yields_no_import() {
        let v = extract_imports("using X = MultiLang.Helper;\n", "csharp");
        assert!(
            v.is_empty(),
            "a using alias is a type alias, not a namespace import — expected no import, got {v:?}"
        );
    }
}
