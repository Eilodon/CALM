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

    let root = tree.root_node();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if lang_consts.function_node_types.contains(&kind)
            && let Some(name_node) = node.child_by_field_name(lang_consts.name_field)
        {
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
                    && let Some(doc_type) = lang_consts.docstring_type
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

                symbols.push(ParsedSymbol {
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
                    is_entry_point: false,
                });
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    Ok(symbols)
}

/// A raw call site discovered in source, attributed to its enclosing function.
///
/// `enclosing_name`/`enclosing_line` identify the caller symbol (resolved to a
/// `qualified_name` later by the pipeline); `callee` is the bare called name.
pub struct RawCall {
    pub enclosing_name: String,
    pub enclosing_line: usize,
    pub callee: String,
    pub line: usize,
}

/// Reduce a callee expression's text to a bare identifier for name-based matching.
///
/// `obj.method` → `method`, `mod::func` → `func`, `Type::<T>::new(` → `new`.
fn callee_bare_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['.', ':']).next().unwrap_or(raw);
    let ident: String = last
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Walk the AST collecting call sites, tracking the nearest enclosing function.
fn walk_calls(
    node: tree_sitter::Node,
    source: &str,
    consts: &crate::indexer::lang_constants::LangConstants,
    enclosing: Option<(String, usize)>,
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

    if consts.call_node_types.contains(&node.kind())
        && let Some((enc_name, enc_line)) = &current
        && let Some(fn_node) = node.child_by_field_name(consts.call_function_field)
        && let Some(callee) = callee_bare_name(&source[fn_node.byte_range()])
    {
        out.push(RawCall {
            enclosing_name: enc_name.clone(),
            enclosing_line: *enc_line,
            callee,
            line: node.start_position().row + 1,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, source, consts, current.clone(), out);
    }
}

/// Extract call sites from a source file, each attributed to its enclosing function.
/// Top-level calls (outside any function) are skipped — they have no caller symbol.
pub fn extract_calls(source: &str, language: &str, _path: &str) -> Result<Vec<RawCall>, String> {
    let consts = get_lang_constants(language).ok_or("No lang constants")?;
    let tree = parse_tree(source, language).ok_or("Failed to parse")?;

    let mut out = Vec::new();
    walk_calls(tree.root_node(), source, &consts, None, &mut out);
    Ok(out)
}

/// File-local alias map (`x = helper` → `x` ↦ `helper`) via the conservative
/// resolver, so calls through simple aliases resolve to the real target.
///
/// Only `file_symbols` is supplied as context (no import/type map yet), so this
/// covers aliases to symbols defined in the same file — the common local case.
pub fn extract_file_aliases(
    source: &str,
    language: &str,
    file_symbols: &std::collections::HashSet<String>,
) -> std::collections::HashMap<String, String> {
    let Some(tree) = parse_tree(source, language) else {
        return std::collections::HashMap::new();
    };
    let ctx = crate::resolver::FileContext {
        file_symbols: file_symbols.clone(),
        import_map: std::collections::HashMap::new(),
        type_map: std::collections::HashMap::new(),
    };
    crate::resolver::conservative::ConservativeResolver::new().extract_aliases(
        tree.root_node(),
        source.as_bytes(),
        language,
        &ctx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
