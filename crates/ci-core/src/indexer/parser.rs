use crate::types::SymbolKind;

pub struct ParsedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub kind: SymbolKind,
    pub language: String,
    pub path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub signature: String,
    pub docstring: String,
    pub name_tokens: String,
    pub is_entry_point: bool,
    /// Enclosing class/impl type name for methods (`None` for free functions).
    /// Drives tier-2 method resolution.
    pub class_context: Option<String>,
}

use crate::graph::tokenize::tokenize_identifier;
use crate::indexer::lang_constants::get_lang_constants;

/// Parse `source` for a tier-0 `language` into a tree-sitter tree, or `None` if
/// the language is unsupported or parsing fails. Single source of the per-language
/// grammar mapping.
pub fn parse_tree(source: &str, language: &str) -> Option<tree_sitter::Tree> {
    let lang: tree_sitter::Language = match language {
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        _ => return None,
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).ok()?;
    parser.parse(source, None)
}

pub fn extract_symbols(
    source: &str,
    language: &str,
    path: &str,
) -> Result<Vec<ParsedSymbol>, String> {
    let lang_consts = get_lang_constants(language).ok_or("No lang constants")?;
    let tree = parse_tree(source, language).ok_or("Failed to parse")?;
    let mut symbols = Vec::new();
    walk_symbols(
        tree.root_node(),
        source,
        &lang_consts,
        language,
        path,
        None,
        &mut symbols,
    );
    Ok(symbols)
}

/// Recursive symbol walk tracking the enclosing class/impl so methods record
/// their `class_context`.
fn walk_symbols(
    node: tree_sitter::Node,
    source: &str,
    lc: &crate::indexer::lang_constants::LangConstants,
    language: &str,
    path: &str,
    enclosing_class: Option<String>,
    out: &mut Vec<ParsedSymbol>,
) {
    // A symbol defined here belongs to the class we are currently inside.
    if lc.function_node_types.contains(&node.kind())
        && let Some(name_node) = node.child_by_field_name(lc.name_field)
    {
        let name = source[name_node.byte_range()].to_string();

        let mut docstring = String::new();
        if language == "python" {
            if let Some(body) = node.child_by_field_name("body")
                && body.kind() == "block"
                && let Some(expr) = body.child(0)
                && expr.kind() == "expression_statement"
            {
                let raw_doc = source[expr.byte_range()].trim();
                docstring = raw_doc.trim_matches(|c| c == '"' || c == '\'').to_string();
            }
        } else if let Some(prev) = node.prev_named_sibling()
            && let Some(doc_type) = lc.docstring_type
            && prev.kind() == doc_type
        {
            docstring = source[prev.byte_range()].trim().to_string();
        }

        let sig_end = source[node.start_byte()..]
            .find('{')
            .or_else(|| source[node.start_byte()..].find(':'))
            .map(|pos| node.start_byte() + pos + 1)
            .unwrap_or(node.end_byte());
        let signature = source[node.start_byte()..sig_end].trim().to_string();
        let name_tokens = tokenize_identifier(&name);

        // Go has no class scope: a method's "class" is its receiver type.
        let class_context = if language == "go" && node.kind() == "method_declaration" {
            go_receiver_type(node, source)
        } else {
            enclosing_class.clone()
        };

        let is_entry_point = detect_entry_point(node, source, language, &name, &signature);

        out.push(ParsedSymbol {
            qualified_name: name.clone(),
            name,
            kind: SymbolKind::Function,
            language: language.to_string(),
            path: path.to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature,
            docstring,
            name_tokens,
            is_entry_point,
            class_context,
        });
    }

    // Entering a class/impl sets the context for its descendants.
    let child_class = if lc.class_node_types.contains(&node.kind()) {
        node.child_by_field_name(lc.class_name_field)
            .map(|n| source[n.byte_range()].to_string())
            .or_else(|| enclosing_class.clone())
    } else {
        enclosing_class.clone()
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_symbols(child, source, lc, language, path, child_class.clone(), out);
    }
}

/// The receiver type of a Go `method_declaration` (`func (s *Service) M()` → `Service`).
fn go_receiver_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration"
            && let Some(ty) = child.child_by_field_name("type")
        {
            // Strip pointer/qualifier; keep the trailing bare type identifier.
            let bare: String = source[ty.byte_range()]
                .rsplit(['.', '*', ' '])
                .next()
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !bare.is_empty() {
                return Some(bare);
            }
        }
    }
    None
}

/// Decorator/attribute sibling node kind that may precede a definition, per language.
fn decorator_node_kind(language: &str) -> Option<&'static str> {
    match language {
        "python" => Some("decorator"),
        "rust" => Some("attribute_item"),
        _ => None,
    }
}

/// Source text of every decorator/attribute immediately preceding `node` (innermost first).
fn collect_decorators<'a>(node: tree_sitter::Node, source: &'a str, kind: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut sib = node.prev_named_sibling();
    while let Some(s) = sib {
        if s.kind() != kind {
            break;
        }
        out.push(source[s.byte_range()].trim());
        sib = s.prev_named_sibling();
    }
    out
}

/// Per-language entry-point convention: known framework decorators/attributes,
/// `main`/`init` functions, and `export default`.
fn detect_entry_point(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    name: &str,
    signature: &str,
) -> bool {
    let decorators = decorator_node_kind(language)
        .map(|k| collect_decorators(node, source, k))
        .unwrap_or_default();

    match language {
        "python" => {
            const HOOKS: &[&str] = &[
                ".route(", ".command(", ".get(", ".post(", ".put(", ".delete(", ".patch(",
            ];
            decorators
                .iter()
                .any(|d| HOOKS.iter().any(|h| d.contains(h)))
        }
        "rust" => {
            name == "main"
                || decorators.iter().any(|d| {
                    let inner = d.trim_start_matches("#[").trim_end_matches(']');
                    let path = inner.split('(').next().unwrap_or(inner).trim();
                    path == "main" || path.ends_with("::main")
                })
        }
        "go" => node.kind() == "function_declaration" && (name == "main" || name == "init"),
        "java" => signature.contains("public static void main"),
        "javascript" | "typescript" => {
            name == "main"
                || node
                    .parent()
                    .map(|p| {
                        p.kind() == "export_statement"
                            && source[p.byte_range()].trim_start().starts_with("export default")
                    })
                    .unwrap_or(false)
        }
        _ => false,
    }
}

/// A raw call site discovered in source, attributed to its enclosing function.
///
/// `enclosing_name`/`enclosing_line` identify the caller symbol; `enclosing_class`
/// is the class it lives in (for `self`/`this` resolution); `receiver` is the
/// object of a method call (`recv.method()`), enabling tier-2 type resolution.
pub struct RawCall {
    pub enclosing_name: String,
    pub enclosing_line: usize,
    pub enclosing_class: Option<String>,
    pub callee: String,
    pub receiver: Option<String>,
    pub line: usize,
}

/// Keep the leading identifier of a segment (drop generics/parens/whitespace).
fn leading_ident(seg: &str) -> Option<String> {
    let ident: String = seg
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Split a callee expression into (immediate receiver, method/callee name).
///
/// `self.method` → (Some("self"), "method"); `a.b.method` → (Some("b"), "method");
/// `mod::func` / `func` → (None, "func").
fn split_receiver_callee(raw: &str) -> Option<(Option<String>, String)> {
    if let Some(dot) = raw.rfind('.') {
        let (left, right) = raw.split_at(dot);
        let callee = leading_ident(&right[1..])?;
        // Immediate receiver = last segment of the left side.
        let recv = left.rsplit(['.', ':']).next().and_then(leading_ident);
        Some((recv, callee))
    } else {
        let last = raw.rsplit("::").next().unwrap_or(raw);
        Some((None, leading_ident(last)?))
    }
}

/// Walk the AST collecting call sites, tracking the nearest enclosing function
/// and class.
fn walk_calls(
    node: tree_sitter::Node,
    source: &str,
    consts: &crate::indexer::lang_constants::LangConstants,
    enclosing: Option<(String, usize)>,
    enclosing_class: Option<String>,
    out: &mut Vec<RawCall>,
) {
    let mut current = enclosing;
    if consts.function_node_types.contains(&node.kind())
        && let Some(name_node) = node.child_by_field_name(consts.name_field)
    {
        current = Some((
            source[name_node.byte_range()].to_string(),
            node.start_position().row + 1,
        ));
    }
    let child_class = if consts.class_node_types.contains(&node.kind()) {
        node.child_by_field_name(consts.class_name_field)
            .map(|n| source[n.byte_range()].to_string())
            .or_else(|| enclosing_class.clone())
    } else {
        enclosing_class.clone()
    };

    if consts.call_node_types.contains(&node.kind())
        && let Some((enc_name, enc_line)) = &current
        && let Some(fn_node) = node.child_by_field_name(consts.call_function_field)
        && let Some((receiver, callee)) = split_receiver_callee(&source[fn_node.byte_range()])
    {
        out.push(RawCall {
            enclosing_name: enc_name.clone(),
            enclosing_line: *enc_line,
            enclosing_class: child_class.clone(),
            callee,
            receiver,
            line: node.start_position().row + 1,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(
            child,
            source,
            consts,
            current.clone(),
            child_class.clone(),
            out,
        );
    }
}

/// Extract call sites from a source file, each attributed to its enclosing function.
/// Top-level calls (outside any function) are skipped — they have no caller symbol.
pub fn extract_calls(source: &str, language: &str, _path: &str) -> Result<Vec<RawCall>, String> {
    let consts = get_lang_constants(language).ok_or("No lang constants")?;
    let tree = parse_tree(source, language).ok_or("Failed to parse")?;

    let mut out = Vec::new();
    walk_calls(tree.root_node(), source, &consts, None, None, &mut out);
    Ok(out)
}

/// File-local alias map (`x = helper` → `x` ↦ `helper`) via the conservative
/// resolver, so calls through simple aliases resolve to the real target.
///
/// The full `FileContext` (file_symbols + import_map + type_map) is supplied so
/// the resolver's multi-assignment and symbol/import/type guards apply.
pub fn extract_file_aliases(
    source: &str,
    language: &str,
    ctx: &crate::resolver::FileContext,
) -> std::collections::HashMap<String, String> {
    let Some(tree) = parse_tree(source, language) else {
        return std::collections::HashMap::new();
    };
    crate::resolver::conservative::ConservativeResolver::new().extract_aliases(
        tree.root_node(),
        source.as_bytes(),
        language,
        ctx,
    )
}

/// Best-effort `name → type` map from explicit annotations: typed parameters
/// plus Rust `let` and Go `var` bindings, across every tier-0 language that has
/// them. Used by the resolver as an alias guard and for tier-2 method resolution.
///
/// JavaScript (dynamic) yields an empty map. Go shares one type across several
/// names (`x, y Foo`), so each name is mapped.
pub fn extract_type_map(source: &str, language: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(tree) = parse_tree(source, language) else {
        return map;
    };
    // Node kinds carrying a `name(s): type` (or `name(s) type`) binding.
    let binding_kinds: &[&str] = match language {
        "python" => &["typed_parameter"],
        "typescript" => &["required_parameter", "optional_parameter"],
        "rust" => &["parameter", "let_declaration"],
        "go" => &["parameter_declaration", "var_spec"],
        "java" => &["formal_parameter"],
        _ => return map, // javascript: no static annotations
    };

    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if binding_kinds.contains(&node.kind()) {
            for (name, ty) in binding_names_and_type(node, source, language) {
                map.insert(name, ty);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    map
}

/// `(name, type)` pairs from one typed binding node. Most languages bind a single
/// name; Go shares one type across several names.
fn binding_names_and_type(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
) -> Vec<(String, String)> {
    let Some(type_node) = node.child_by_field_name("type") else {
        return Vec::new();
    };
    // Strip a leading `:` (TS type_annotation) and surrounding whitespace.
    let ty = source[type_node.byte_range()]
        .trim_start_matches(':')
        .trim()
        .to_string();
    if ty.is_empty() {
        return Vec::new();
    }

    let names: Vec<String> = match language {
        // `x, y Foo`: every identifier child shares the type.
        "go" => {
            let mut cur = node.walk();
            node.children(&mut cur)
                .filter(|c| c.kind() == "identifier")
                .map(|c| source[c.byte_range()].to_string())
                .collect()
        }
        // Python's typed_parameter names its identifier as the first child.
        "python" => node
            .named_child(0)
            .map(|n| source[n.byte_range()].to_string())
            .into_iter()
            .collect(),
        // TS/Rust use a `pattern` field; Java uses `name`.
        _ => node
            .child_by_field_name("pattern")
            .or_else(|| node.child_by_field_name("name"))
            .map(|n| source[n.byte_range()].to_string())
            .into_iter()
            .collect(),
    };

    names
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .map(|n| (n, ty.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_map_all_languages() {
        let cases = [
            ("def f(x: Foo):\n    pass\n", "python"),
            ("fn f(x: Foo) {}\n", "rust"),
            ("package p\nfunc f(x Foo) {}\n", "go"),
            ("class C { void m(Foo x) {} }\n", "java"),
            ("function f(x: Foo) {}\n", "typescript"),
        ];
        for (src, lang) in cases {
            let m = extract_type_map(src, lang);
            assert_eq!(
                m.get("x"),
                Some(&"Foo".to_string()),
                "type_map for {lang} should bind x: Foo"
            );
        }
        // Go shares one type across several names.
        let m = extract_type_map("package p\nfunc f(x, y Foo) {}\n", "go");
        assert_eq!(m.get("x"), Some(&"Foo".to_string()));
        assert_eq!(m.get("y"), Some(&"Foo".to_string()));
        // JavaScript has no static types.
        assert!(extract_type_map("function f(x) {}\n", "javascript").is_empty());
    }

    #[test]
    fn test_python_symbol_extraction() {
        let code = r#"
def hello(a, b):
    """This is a docstring"""
    pass
"#;
        let symbols = extract_symbols(code, "python", "test.py").unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert_eq!(symbols[0].signature, "def hello(a, b):");
        assert_eq!(symbols[0].docstring, "This is a docstring");
        assert_eq!(symbols[0].name_tokens, "hello");
    }

    #[test]
    fn test_rust_symbol_extraction() {
        let code = r#"
/// This is a docstring
pub fn hello(a: i32, b: i32) -> i32 {
    a + b
}
"#;
        let symbols = extract_symbols(code, "rust", "test.rs").unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert!(symbols[0].signature.contains("fn hello"));
        assert_eq!(symbols[0].docstring.trim(), "/// This is a docstring");
    }

    fn find<'a>(symbols: &'a [ParsedSymbol], name: &str) -> &'a ParsedSymbol {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found"))
    }

    #[test]
    fn test_python_entry_point_decorator() {
        let code = r#"
@app.route("/")
def index():
    pass

def helper():
    pass
"#;
        let symbols = extract_symbols(code, "python", "app.py").unwrap();
        assert!(find(&symbols, "index").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
    }

    #[test]
    fn test_python_entry_point_cli_command() {
        let code = r#"
@cli.command()
def run():
    pass
"#;
        let symbols = extract_symbols(code, "python", "cli.py").unwrap();
        assert!(find(&symbols, "run").is_entry_point);
    }

    #[test]
    fn test_rust_entry_point_main_name() {
        let code = "fn main() {}\nfn helper() {}\n";
        let symbols = extract_symbols(code, "rust", "main.rs").unwrap();
        assert!(find(&symbols, "main").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
    }

    #[test]
    fn test_rust_entry_point_tokio_main_attribute() {
        let code = "#[tokio::main]\nasync fn main() {}\n";
        let symbols = extract_symbols(code, "rust", "main.rs").unwrap();
        assert!(find(&symbols, "main").is_entry_point);
    }

    #[test]
    fn test_go_entry_point_main_and_init() {
        let code = "package p\nfunc main() {}\nfunc init() {}\nfunc helper() {}\n";
        let symbols = extract_symbols(code, "go", "main.go").unwrap();
        assert!(find(&symbols, "main").is_entry_point);
        assert!(find(&symbols, "init").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
    }

    #[test]
    fn test_go_entry_point_excludes_method_receiver() {
        let code = "package p\ntype T struct{}\nfunc (t T) main() {}\n";
        let symbols = extract_symbols(code, "go", "main.go").unwrap();
        assert!(!find(&symbols, "main").is_entry_point);
    }

    #[test]
    fn test_java_entry_point_public_static_void_main() {
        let code = r#"
public class Main {
    public static void main(String[] args) {}
    void helper() {}
}
"#;
        let symbols = extract_symbols(code, "java", "Main.java").unwrap();
        assert!(find(&symbols, "main").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
    }

    #[test]
    fn test_typescript_entry_point_export_default() {
        let code = "export default function run() {}\nfunction helper() {}\n";
        let symbols = extract_symbols(code, "typescript", "index.ts").unwrap();
        assert!(find(&symbols, "run").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
    }

    #[test]
    fn test_typescript_entry_point_main_name() {
        let code = "function main() {}\n";
        let symbols = extract_symbols(code, "typescript", "index.ts").unwrap();
        assert!(find(&symbols, "main").is_entry_point);
    }
}
