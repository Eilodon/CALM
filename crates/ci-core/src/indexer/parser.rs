use crate::types::{SymbolKind, IndexingPhase};

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

use crate::indexer::lang_constants::get_lang_constants;
use crate::graph::tokenize::tokenize_identifier;

pub fn extract_symbols(source: &str, language: &str, path: &str) -> Result<Vec<ParsedSymbol>, String> {
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = match language {
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        _ => return Err(format!("Unsupported language: {}", language)),
    };
    parser.set_language(&lang).map_err(|e| e.to_string())?;

    let lang_consts = get_lang_constants(language).ok_or("No lang constants")?;
    let tree = parser.parse(source, None).ok_or("Failed to parse")?;
    let mut symbols = Vec::new();

    let root = tree.root_node();
    let mut stack = vec![root];
    
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if lang_consts.function_node_types.contains(&kind) {
            if let Some(name_node) = node.child_by_field_name(lang_consts.name_field) {
                let name = source[name_node.byte_range()].to_string();
                
                let mut docstring = String::new();
                if language == "python" {
                    if let Some(body) = node.child_by_field_name("body") {
                        if body.kind() == "block" {
                            if let Some(expr) = body.child(0) {
                                if expr.kind() == "expression_statement" {
                                    let raw_doc = source[expr.byte_range()].trim();
                                    docstring = raw_doc.trim_matches(|c| c == '"' || c == '\'').to_string();
                                }
                            }
                        }
                    }
                } else if let Some(prev) = node.prev_named_sibling() {
                    if let Some(doc_type) = lang_consts.docstring_type {
                        if prev.kind() == doc_type {
                            docstring = source[prev.byte_range()].trim().to_string();
                        }
                    }
                }

                let sig_end = source[node.start_byte()..].find('{')
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

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
