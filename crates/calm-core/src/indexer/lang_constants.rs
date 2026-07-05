pub struct LangConstants {
    pub function_node_types: &'static [&'static str],
    pub name_field: &'static str,
    pub docstring_type: Option<&'static str>,
    /// Node kinds that represent a call / invocation site.
    pub call_node_types: &'static [&'static str],
    /// Field name on a call node that holds the callee expression (the called function).
    pub call_function_field: &'static str,
    /// Node kinds that introduce a class / impl scope (for method `class_context`).
    pub class_node_types: &'static [&'static str],
    /// Field on a class node naming the type (Rust `impl` uses `type`, others `name`).
    pub class_name_field: &'static str,
}

pub fn get_lang_constants(lang: &str) -> Option<LangConstants> {
    match lang {
        "python" => Some(LangConstants {
            function_node_types: &["function_definition", "class_definition"],
            name_field: "name",
            docstring_type: Some("expression_statement"), // Python docstrings are expression_statements
            call_node_types: &["call"],
            call_function_field: "function",
            class_node_types: &["class_definition"],
            class_name_field: "name",
        }),
        "rust" => Some(LangConstants {
            function_node_types: &[
                "function_item",
                "function_signature_item",
                "struct_item",
                "trait_item",
                "impl_item",
                "enum_item",
                "type_item",
                "union_item",
            ],
            name_field: "name",
            docstring_type: Some("line_comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            class_node_types: &["impl_item", "trait_item"],
            class_name_field: "type",
        }),
        "go" => Some(LangConstants {
            function_node_types: &[
                "function_declaration",
                "method_declaration",
                // Each `type_spec`/`type_alias` is walked individually (not
                // the enclosing `type_declaration`) since a grouped
                // `type (\n A struct{}\n B int\n)` block is one
                // `type_declaration` node containing N sibling specs —
                // matching on the wrapper alone only ever surfaced the
                // first one. See resolve_name_node's doc comment.
                "type_spec",
                "type_alias",
            ],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            class_node_types: &[],
            class_name_field: "name",
        }),
        "javascript" | "typescript" => Some(LangConstants {
            function_node_types: &[
                "function_declaration",
                "generator_function_declaration",
                "class_declaration",
                "method_definition",
                "lexical_declaration",
                // TypeScript-only (never appear in the JS grammar, so no-op
                // there): interface/type-alias/enum declarations are
                // otherwise invisible to the extractor entirely — a
                // TS/DTO-only file would index as 0 symbols. See
                // node_kind_to_symbol_kind for the SymbolKind mapping.
                "interface_declaration",
                "type_alias_declaration",
                "enum_declaration",
            ],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            class_node_types: &["class_declaration"],
            class_name_field: "name",
        }),
        "java" => Some(LangConstants {
            function_node_types: &[
                "method_declaration",
                "class_declaration",
                "interface_declaration",
                "enum_declaration",
                "record_declaration",
                "constructor_declaration",
            ],
            name_field: "name",
            docstring_type: Some("block_comment"),
            call_node_types: &["method_invocation"],
            call_function_field: "name",
            class_node_types: &[
                "class_declaration",
                "interface_declaration",
                "enum_declaration",
                "record_declaration",
            ],
            class_name_field: "name",
        }),
        // Tier-0.5 — full tree-sitter parsing when the optional grammar feature is enabled.
        // LangConstants are always present; parse_tree gates the actual grammar behind the flag.
        "ruby" => Some(LangConstants {
            function_node_types: &["method", "singleton_method", "class", "module"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["method_call"],
            call_function_field: "method",
            class_node_types: &["class", "module"],
            class_name_field: "name",
        }),
        "php" => Some(LangConstants {
            function_node_types: &[
                "function_definition",
                "method_declaration",
                "class_declaration",
                "interface_declaration",
                "trait_declaration",
            ],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["function_call_expression"],
            call_function_field: "function",
            class_node_types: &[
                "class_declaration",
                "interface_declaration",
                "trait_declaration",
            ],
            class_name_field: "name",
        }),
        "kotlin" => Some(LangConstants {
            function_node_types: &[
                "function_declaration",
                "class_declaration",
                "interface_declaration",
                "object_declaration",
            ],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call_expression"],
            call_function_field: "callee", // Kotlin uses "callee", not "function"
            class_node_types: &[
                "class_declaration",
                "interface_declaration",
                "object_declaration",
            ],
            class_name_field: "name",
        }),
        "swift" => Some(LangConstants {
            function_node_types: &[
                "function_declaration",
                "class_declaration",
                "struct_declaration",
                "enum_declaration",
                "protocol_declaration",
            ],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["function_call_expression"],
            call_function_field: "function",
            class_node_types: &[
                "class_declaration",
                "struct_declaration",
                "enum_declaration",
                "protocol_declaration",
            ],
            class_name_field: "name",
        }),
        "csharp" => Some(LangConstants {
            function_node_types: &[
                "method_declaration",
                "class_declaration",
                "struct_declaration",
                "interface_declaration",
                "delegate_declaration",
                "enum_declaration",
            ],
            name_field: "name",
            docstring_type: Some("block_comment"),
            call_node_types: &["invocation_expression"],
            call_function_field: "function",
            class_node_types: &[
                "class_declaration",
                "struct_declaration",
                "interface_declaration",
                "enum_declaration",
            ],
            class_name_field: "name",
        }),
        "shell" | "bash" => Some(LangConstants {
            function_node_types: &["function_definition"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["command"],
            call_function_field: "name",
            class_node_types: &[],
            class_name_field: "name",
        }),
        // C: function names live inside a declarator chain (no direct "name" field);
        // resolve_name_node() has a special case that walks declarator → identifier.
        // struct_specifier/union_specifier/enum_specifier DO have a direct
        // "name" field, same as C++ below — without these the real grammar
        // path recognized only functions, strictly less than the old regex
        // fallback (detect_c_cpp), which at least caught struct/union/enum
        // keywords textually.
        "c" => Some(LangConstants {
            function_node_types: &[
                "function_definition",
                "struct_specifier",
                "union_specifier",
                "enum_specifier",
            ],
            name_field: "name", // no-op for function_definition; handled by resolve_name_node
            docstring_type: Some("comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            class_node_types: &[],
            class_name_field: "name",
        }),
        // C++: same declarator quirk as C for function_definition; class_specifier /
        // struct_specifier / enum_specifier DO have a direct "name" field.
        "cpp" => Some(LangConstants {
            function_node_types: &[
                "function_definition",
                "class_specifier",
                "struct_specifier",
                "enum_specifier",
            ],
            name_field: "name", // works for class/struct/enum; resolve_name_node handles function
            docstring_type: Some("comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            class_node_types: &["class_specifier", "struct_specifier", "enum_specifier"],
            class_name_field: "name",
        }),
        _ => None,
    }
}

/// Tree-sitter node kinds that count as a decision point for McCabe
/// cyclomatic complexity (baseline 1 + one per branch). Only defined for the
/// six Tier-0 languages, which get a real parse tree; every other language
/// (Tier-0.5 line-scan extraction, or an unrecognized language) returns an
/// empty slice, so `compute_cyclomatic_complexity` falls back to the
/// baseline 1 rather than guessing from unparsed text.
///
/// Short-circuit boolean operators (`&&`/`||`) are only counted where the
/// grammar exposes a dedicated node kind (Python's `boolean_operator`);
/// languages whose grammar folds them into a generic `binary_expression`
/// alongside arithmetic operators are not text-inspected to disambiguate, so
/// this undercounts those slightly rather than risk misclassifying
/// arithmetic as a branch.
pub fn branch_node_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "python" => &[
            "if_statement",
            "elif_clause",
            "for_statement",
            "while_statement",
            "except_clause",
            "boolean_operator",
            "conditional_expression",
            "case_clause",
        ],
        "rust" => &[
            "if_expression",
            "if_let_expression",
            "match_arm",
            "while_expression",
            "while_let_expression",
            "for_expression",
            "loop_expression",
        ],
        "go" => &[
            "if_statement",
            "for_statement",
            "expression_case",
            "communication_case",
            "type_case",
        ],
        "javascript" | "typescript" => &[
            "if_statement",
            "for_statement",
            "for_in_statement",
            "while_statement",
            "do_statement",
            "switch_case",
            "catch_clause",
            "ternary_expression",
        ],
        "java" => &[
            "if_statement",
            "for_statement",
            "while_statement",
            "do_statement",
            "switch_label",
            "switch_rule",
            "catch_clause",
            "ternary_expression",
        ],
        _ => &[],
    }
}

/// Map a file extension to a language id.
/// Returns a tier-0 language (full parse + call-graph) for the six main
/// languages, and a tier-0.5 language id (shallow symbol-only extraction)
/// for additional common languages. Returns `None` for unknown extensions.
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        // Tier-0: full parse + resolver + call-graph
        "py" => Some("python"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "java" => Some("java"),

        // Tier-0.5: lightweight line-scan symbol extraction only
        "c" => Some("c"),
        "cc" | "cpp" | "cxx" | "h" | "hpp" | "hxx" => Some("cpp"),
        "cs" => Some("csharp"),
        "rb" => Some("ruby"),
        "sh" | "bash" => Some("shell"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "php" => Some("php"),

        _ => None,
    }
}

/// File extensions recognized as meaningful source for a language this
/// indexer has no extraction support for at all — no tree-sitter grammar, no
/// shallow regex fallback (`language_for_extension` returns `None` for all of
/// these). A match still earns the file a `file_index` row (path, hash, mtime,
/// `language = NULL`, `symbol_count = 0`) via `collect_source_files`, so it's
/// visible to `dependencies`/`repo_overview`/`diff_impact` as "recognized but
/// unparsed" rather than being indistinguishable from a truly invisible file
/// (an image, a lockfile, a doc) that still gets no row at all. Deliberately
/// narrow — this is not a general "track every file" catch-all, just enough to
/// give diff_impact an honest, non-misleading signal for these languages
/// until real extraction support exists.
pub fn is_recognized_unparsed_extension(ext: &str) -> bool {
    matches!(ext, "sol" | "circom" | "move" | "cairo" | "vy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_recognized_unparsed_extension_matches_known_registry() {
        for ext in ["sol", "circom", "move", "cairo", "vy"] {
            assert!(
                is_recognized_unparsed_extension(ext),
                "{ext} should be recognized"
            );
        }
        assert!(!is_recognized_unparsed_extension("md"));
        assert!(!is_recognized_unparsed_extension("png"));
        assert!(!is_recognized_unparsed_extension("lock"));
    }

    /// The registry must stay disjoint from `language_for_extension` —
    /// otherwise a real tier-0/tier-0.5 language could be double-counted or
    /// (worse) accidentally downgraded to path-only tracking by a stray
    /// entry here.
    #[test]
    fn is_recognized_unparsed_extension_never_overlaps_language_for_extension() {
        for ext in ["sol", "circom", "move", "cairo", "vy"] {
            assert!(
                language_for_extension(ext).is_none(),
                "{ext} must not also be a language_for_extension entry"
            );
        }
    }
}
