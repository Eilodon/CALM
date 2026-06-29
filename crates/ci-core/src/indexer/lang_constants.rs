pub struct LangConstants {
    pub function_node_types: &'static [&'static str],
    pub name_field: &'static str,
    pub docstring_type: Option<&'static str>,
}

pub fn get_lang_constants(lang: &str) -> Option<LangConstants> {
    match lang {
        "python" => Some(LangConstants {
            function_node_types: &["function_definition", "class_definition"],
            name_field: "name",
            docstring_type: Some("expression_statement"), // Python docstrings are expression_statements
        }),
        "rust" => Some(LangConstants {
            function_node_types: &["function_item", "struct_item", "trait_item", "impl_item"],
            name_field: "name",
            docstring_type: Some("line_comment"),
        }),
        "go" => Some(LangConstants {
            function_node_types: &["function_declaration", "method_declaration", "type_declaration"],
            name_field: "name",
            docstring_type: Some("comment"),
        }),
        "javascript" | "typescript" => Some(LangConstants {
            function_node_types: &["function_declaration", "class_declaration", "method_definition", "lexical_declaration"],
            name_field: "name",
            docstring_type: Some("comment"),
        }),
        "java" => Some(LangConstants {
            function_node_types: &["method_declaration", "class_declaration", "interface_declaration"],
            name_field: "name",
            docstring_type: Some("block_comment"),
        }),
        _ => None,
    }
}
