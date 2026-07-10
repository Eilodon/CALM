pub struct LangConstants {
    pub function_node_types: &'static [&'static str],
    pub name_field: &'static str,
    pub docstring_type: Option<&'static str>,
    /// Node kinds that represent a call / invocation site.
    pub call_node_types: &'static [&'static str],
    /// Field name on a call node that holds the callee expression (the called function).
    /// Default for any `call_node_types` entry not present in
    /// `call_function_field_by_kind`.
    pub call_function_field: &'static str,
    /// Per-node-kind override of `call_function_field`, for languages where
    /// different call node kinds name their callee field differently. PHP
    /// needs this: `function_call_expression` (bare `foo()`) uses field
    /// `"function"` (the language's overall default, above), but
    /// `member_call_expression`/`nullsafe_member_call_expression`/
    /// `scoped_call_expression`/`object_creation_expression` all use
    /// `"name"` instead (confirmed via the real grammar) — one shared
    /// `call_function_field` string per language can't express both at
    /// once. Empty for every other language (no override needed).
    pub call_function_field_by_kind: &'static [(&'static str, &'static str)],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
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
        // Ruby: call-graph extraction was silently 100% broken until
        // 2026-07-10 — this arm declared `call_node_types: &["method_call"]`,
        // but the vendored grammar (`tree-sitter-ruby`) has no node kind
        // named "method_call" at all; confirmed by reading its own
        // `node-types.json` directly. The real call node is named `"call"`
        // (it does carry a `"method"` field, so `call_function_field` below
        // was already correct — only the node-kind string was wrong).
        // Verified via empirical re-index of a real fixture before/after:
        // `call_edges`/`call_sites` were 0 rows for every Ruby call, with or
        // without parens/an explicit receiver, before this fix.
        "ruby" => Some(LangConstants {
            function_node_types: &["method", "singleton_method", "class", "module"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call"],
            call_function_field: "method",
            call_function_field_by_kind: &[],
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
            call_node_types: &[
                "function_call_expression",
                "member_call_expression",
                "nullsafe_member_call_expression",
                "scoped_call_expression",
                "object_creation_expression",
            ],
            call_function_field: "function",
            // member_call_expression/nullsafe_member_call_expression/
            // scoped_call_expression/object_creation_expression all name
            // their callee via "name" instead (confirmed via the real
            // grammar) — function_call_expression above keeps the language
            // default ("function").
            call_function_field_by_kind: &[
                ("member_call_expression", "name"),
                ("nullsafe_member_call_expression", "name"),
                ("scoped_call_expression", "name"),
                ("object_creation_expression", "name"),
            ],
            class_node_types: &[
                "class_declaration",
                "interface_declaration",
                "trait_declaration",
            ],
            class_name_field: "name",
        }),
        // Kotlin: `get_lang_constants` had a full entry for this language
        // long before any tree-sitter-kotlin dependency existed at all
        // (`parse_tree` had zero match arm for it — always fell back to
        // shallow line-scan) — every field below was an untested guess.
        // Verified 2026-07-10 against the real grammar
        // (tree-sitter-kotlin-ng 1.1.0) via a throwaway AST-dump test:
        // - No node kind named "interface_declaration" exists at all
        //   (confirmed via node-types.json) — `interface Foo {}` parses as a
        //   plain `class_declaration`, already covered without a separate arm.
        // - `call_expression` has ZERO fields (`fields: []` in
        //   node-types.json, not `"callee"` as previously guessed) — the
        //   callee is just whichever expression comes first: a bare
        //   `identifier` for `println(...)`, or a `navigation_expression`
        //   for `this.foo()`/`Repo.save()`. `call_function_field:
        //   "$first_child"` is a sentinel `walk_calls` (parser.rs)
        //   special-cases to grab the call node's own first child
        //   regardless of kind, then let `split_receiver_callee` do its
        //   normal dot-splitting on that node's raw text — see
        //   `walk_calls`'s doc comment for why this is safe for every other
        //   language (the sentinel string is never used as a real
        //   field/kind name elsewhere).
        "kotlin" => Some(LangConstants {
            function_node_types: &[
                "function_declaration",
                "class_declaration",
                "object_declaration",
            ],
            name_field: "name",
            // KDoc block comments (`/** ... */`); no node kind literally
            // named "comment" exists in this grammar (confirmed via
            // node-types.json — only "line_comment"/"block_comment").
            docstring_type: Some("block_comment"),
            call_node_types: &["call_expression"],
            call_function_field: "$first_child",
            call_function_field_by_kind: &[],
            class_node_types: &["class_declaration", "object_declaration"],
            class_name_field: "name",
        }),
        // Swift: same history as Kotlin above — every field was an untested
        // guess until verified 2026-07-10 against the real grammar
        // (tree-sitter-swift =0.7.0, pinned below its published latest for an
        // unrelated ABI reason — see the workspace root Cargo.toml comment).
        // - No node kind named "struct_declaration" or "enum_declaration"
        //   exists at all (confirmed via node-types.json) — `struct Foo {}`/
        //   `enum Foo {}` both parse as plain `class_declaration`, same
        //   unification pattern as Kotlin's `interface` above. This loses
        //   the struct/enum/class distinction in `SymbolKind` (all become
        //   Class) — a documented scope cut, not a bug: distinguishing them
        //   would require inspecting a keyword child's text, not just the
        //   node kind.
        // - `call_expression` has ZERO fields, same shape as Kotlin — same
        //   `"$first_child"` sentinel applies (see Kotlin's comment above
        //   and `walk_calls`'s doc comment in parser.rs).
        "swift" => Some(LangConstants {
            function_node_types: &[
                "function_declaration",
                "class_declaration",
                "protocol_declaration",
            ],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call_expression"],
            call_function_field: "$first_child",
            call_function_field_by_kind: &[],
            class_node_types: &["class_declaration", "protocol_declaration"],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
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
            call_function_field_by_kind: &[],
            class_node_types: &["class_specifier", "struct_specifier", "enum_specifier"],
            class_name_field: "name",
        }),
        // R: functions are anonymous r-values — `foo <- function(x) {...}` is a
        // `binary_operator` node (lhs=name, operator="<-"/"<<-"/"="/":=", rhs=
        // function_definition), or the parenthesized right-assign form
        // `(function(x) {...}) -> foo` (operator="->"/"->>", name/value sides
        // swapped; the *unparenthesized* form doesn't work here — see
        // resolve_name_node's comment for why). Matching bare
        // `function_definition` would misfire on `resolve_name_node`'s generic
        // `name_field` lookup below: that node's own "name" field is the
        // `function`/`\` keyword token, not an identifier, so every anonymous
        // callback would otherwise mint a fake symbol literally named "function".
        // See resolve_name_node's "binary_operator" arm for the real name walk.
        // No class syntax exists (S3/S4/R6/RefClasses are all `setClass()`/
        // `R6::R6Class()` calls, not grammar nodes) — class_node_types is empty,
        // same as Go.
        "r" => Some(LangConstants {
            function_node_types: &["binary_operator"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call"],
            call_function_field: "function",
            call_function_field_by_kind: &[],
            class_node_types: &[],
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
        // `switch()` in R is an ordinary call, not grammar-level branching, so
        // it's invisible here (same class of gap as R having no class syntax).
        "r" => &[
            "if_statement",
            "for_statement",
            "while_statement",
            "repeat_statement",
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
        // Community convention is capital ".R"; lowercase ".r" also occurs.
        "r" | "R" => Some("r"),

        // Standalone module (8-language plan P3.3) — not tree-sitter, see
        // `indexer::sql`'s module doc comment for why.
        "sql" => Some("sql"),

        // Standalone module, same shape as `sql` above — not tree-sitter,
        // dedicated fence-aware line-scan for ATX headings only (see
        // `indexer::parser::extract_markdown_symbols`).
        "md" | "markdown" => Some("markdown"),

        _ => None,
    }
}

/// File extensions recognized as meaningful but not source-parsed: either a
/// language this indexer has no extraction support for at all (no
/// tree-sitter grammar, no shallow regex fallback — `language_for_extension`
/// returns `None` for all of these), or a config/data format like TOML that
/// isn't a programming language and never will have one. A match still earns
/// the file a `file_index` row (path, hash, mtime, `language = NULL`,
/// `symbol_count = 0`) via `collect_source_files`, so it's visible to
/// `dependencies`/`repo_overview`/`diff_impact`/`search` as "recognized but
/// unparsed" rather than being indistinguishable from a truly invisible file
/// (an image, a lockfile, a doc) that still gets no row at all. Deliberately
/// narrow — this is not a general "track every file" catch-all, just enough to
/// give diff_impact an honest, non-misleading signal (e.g. a `Cargo.toml` edit
/// no longer reads as "out of scope") until real extraction support exists
/// for the language entries.
pub fn is_recognized_unparsed_extension(ext: &str) -> bool {
    matches!(ext, "sol" | "circom" | "move" | "cairo" | "vy" | "toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_recognized_unparsed_extension_matches_known_registry() {
        for ext in ["sol", "circom", "move", "cairo", "vy", "toml"] {
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
        for ext in ["sol", "circom", "move", "cairo", "vy", "toml"] {
            assert!(
                language_for_extension(ext).is_none(),
                "{ext} must not also be a language_for_extension entry"
            );
        }
    }
}
