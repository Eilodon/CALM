use crate::types::SymbolKind;

#[derive(Clone, Copy)]
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

/// Per-language descriptor consolidating every source-level dispatch point a
/// new language needs to touch (2026-07-10 25-language-expansion plan,
/// Phase A). Before this registry, `extensions`/`constants`/`ts_language`/
/// `branch_node_kinds`/`decorator_node_kinds`/`binding_kinds`/
/// `line_comment_prefixes`/`modifier_keywords`/`shallow_detect` were 8
/// separate `match language { ... }` blocks spread across this file and
/// `parser.rs`, each one a place a new language's entry could be silently
/// forgotten — this is exactly how the Ruby `call_node_types` bug and the
/// Kotlin/Swift `object_declaration`/`protocol_declaration` symbol-kind bug
/// both shipped (see
/// `docs/superskills/plans/2026-07-10-25-language-expansion.md` §1.2/§1.5).
/// `LANGUAGES` is now the single source of truth; every dispatch function
/// below is a thin lookup over it via `find_spec`.
///
/// Deliberately does NOT cover the Cargo `lang-*` feature-flag plumbing
/// (`crates/*/Cargo.toml`) — re-declaring a passthrough feature per
/// consuming crate is a structural limit of Cargo workspaces, not something
/// a Rust-side registry can fold in. See the plan §1.2's conclusion and
/// `tests/lang_feature_parity.rs`'s cross-file check for that other half of
/// the "adding a language touches N files" problem.
/// A regex/line-scan shallow-fallback detector: takes a stripped source line,
/// returns the symbol name + kind it found, if any.
type ShallowDetectFn = fn(&str) -> Option<(String, SymbolKind)>;

#[derive(Clone, Copy)]
pub struct LanguageSpec {
    /// Canonical language string stored in `symbols.language`/`file_index.language`.
    pub name: &'static str,
    /// Extra language-id strings that resolve to this same entry (currently
    /// only `"bash"` for `"shell"` — `language_for_extension` always
    /// normalizes to the canonical name, but the old per-function matches
    /// also defensively matched the alias directly, so `find_spec` keeps
    /// that behavior).
    pub aliases: &'static [&'static str],
    /// File extensions (without the leading dot) that map to this language.
    pub extensions: &'static [&'static str],
    /// AST shape constants driving symbol/call extraction from a real parse tree.
    pub constants: LangConstants,
    /// Loads the tree-sitter grammar. Returns `None` when the language's
    /// `lang-*` Cargo feature isn't compiled in — always `Some` for the six
    /// Tier-0 languages, which have no optional gate.
    pub ts_language: fn() -> Option<tree_sitter::Language>,
    /// Node kinds counted as a cyclomatic-complexity branch. Empty means
    /// complexity always stays at the baseline of 1 (a Tier-0.5 language
    /// without a real parse tree, or simply not mapped yet).
    pub branch_node_kinds: &'static [&'static str],
    /// Node kinds treated as decorators/annotations for entry-point and
    /// is_test detection.
    pub decorator_node_kinds: &'static [&'static str],
    /// Node kinds carrying a `name: type` binding, for tier-2 type
    /// inference in `extract_type_map_from_tree`. Empty means no
    /// static-annotation type inference for this language (its
    /// constructor-call inference, if any, is separate hardcoded logic in
    /// that function, not driven by this registry).
    pub binding_kinds: &'static [&'static str],
    /// Line-comment prefixes recognized by the regex/line-scan shallow
    /// fallback's `is_comment_line` check.
    pub line_comment_prefixes: &'static [&'static str],
    /// Modifier keywords (`"public "`, `"static "`, ...) stripped from the
    /// front of a line before the shallow fallback's per-language
    /// `detect_*` pattern match runs.
    pub modifier_keywords: &'static [&'static str],
    /// Regex/line-scan fallback used when the real grammar isn't compiled
    /// in. `None` for the six Tier-0 languages (no fallback path — a failed
    /// parse there just yields zero symbols, same as before this registry).
    pub shallow_detect: Option<ShallowDetectFn>,
}

// ts_language loaders. The six Tier-0 grammars are always-on dependencies
// (no Cargo feature gate); the rest are optional `dep:tree-sitter-X` crates
// behind a `lang-X` feature, so each gets a `#[cfg]`/`#[cfg(not)]` pair that
// returns `None` when the grammar isn't compiled in — same effect as the
// old `parse_tree` match's per-arm `#[cfg(feature = "lang-X")]`, just
// reshaped as a function so it can sit in a `LanguageSpec` field.
fn ts_lang_python() -> Option<tree_sitter::Language> {
    Some(tree_sitter_python::LANGUAGE.into())
}
fn ts_lang_rust() -> Option<tree_sitter::Language> {
    Some(tree_sitter_rust::LANGUAGE.into())
}
fn ts_lang_go() -> Option<tree_sitter::Language> {
    Some(tree_sitter_go::LANGUAGE.into())
}
fn ts_lang_javascript() -> Option<tree_sitter::Language> {
    Some(tree_sitter_javascript::LANGUAGE.into())
}
fn ts_lang_typescript() -> Option<tree_sitter::Language> {
    Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
}
fn ts_lang_java() -> Option<tree_sitter::Language> {
    Some(tree_sitter_java::LANGUAGE.into())
}

#[cfg(feature = "lang-ruby")]
fn ts_lang_ruby() -> Option<tree_sitter::Language> {
    Some(tree_sitter_ruby::LANGUAGE.into())
}
#[cfg(not(feature = "lang-ruby"))]
fn ts_lang_ruby() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-php")]
fn ts_lang_php() -> Option<tree_sitter::Language> {
    Some(tree_sitter_php::LANGUAGE_PHP.into())
}
#[cfg(not(feature = "lang-php"))]
fn ts_lang_php() -> Option<tree_sitter::Language> {
    None
}

// kotlin/swift (2026-07-10): real grammars, not the old no-op stubs — see
// the "kotlin"/"swift" LangConstants entries below for the AST-dump
// verification that corrected several never-tested field guesses.
#[cfg(feature = "lang-kotlin")]
fn ts_lang_kotlin() -> Option<tree_sitter::Language> {
    Some(tree_sitter_kotlin_ng::LANGUAGE.into())
}
#[cfg(not(feature = "lang-kotlin"))]
fn ts_lang_kotlin() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-swift")]
fn ts_lang_swift() -> Option<tree_sitter::Language> {
    Some(tree_sitter_swift::LANGUAGE.into())
}
#[cfg(not(feature = "lang-swift"))]
fn ts_lang_swift() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-csharp")]
fn ts_lang_csharp() -> Option<tree_sitter::Language> {
    Some(tree_sitter_c_sharp::LANGUAGE.into())
}
#[cfg(not(feature = "lang-csharp"))]
fn ts_lang_csharp() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-shell")]
fn ts_lang_shell() -> Option<tree_sitter::Language> {
    Some(tree_sitter_bash::LANGUAGE.into())
}
#[cfg(not(feature = "lang-shell"))]
fn ts_lang_shell() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-c")]
fn ts_lang_c() -> Option<tree_sitter::Language> {
    Some(tree_sitter_c::LANGUAGE.into())
}
#[cfg(not(feature = "lang-c"))]
fn ts_lang_c() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-cpp")]
fn ts_lang_cpp() -> Option<tree_sitter::Language> {
    Some(tree_sitter_cpp::LANGUAGE.into())
}
#[cfg(not(feature = "lang-cpp"))]
fn ts_lang_cpp() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-r")]
fn ts_lang_r() -> Option<tree_sitter::Language> {
    Some(tree_sitter_r::LANGUAGE.into())
}
#[cfg(not(feature = "lang-r"))]
fn ts_lang_r() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-scala")]
fn ts_lang_scala() -> Option<tree_sitter::Language> {
    Some(tree_sitter_scala::LANGUAGE.into())
}
#[cfg(not(feature = "lang-scala"))]
fn ts_lang_scala() -> Option<tree_sitter::Language> {
    None
}

// tree-sitter-dart's Rust binding predates the modern `LANGUAGE: LanguageFn`
// const convention every other grammar here uses — it's a bare
// `language() -> tree_sitter::Language` function instead (verified by
// downloading the actual published 0.0.4 .crate tarball and reading
// bindings/rust/lib.rs directly, not assumed from the newer 0.2.0's shape).
#[cfg(feature = "lang-dart")]
fn ts_lang_dart() -> Option<tree_sitter::Language> {
    Some(tree_sitter_dart::language())
}
#[cfg(not(feature = "lang-dart"))]
fn ts_lang_dart() -> Option<tree_sitter::Language> {
    None
}

#[cfg(feature = "lang-lua")]
fn ts_lang_lua() -> Option<tree_sitter::Language> {
    Some(tree_sitter_lua::LANGUAGE.into())
}
#[cfg(not(feature = "lang-lua"))]
fn ts_lang_lua() -> Option<tree_sitter::Language> {
    None
}

const JS_TS_CONSTANTS: LangConstants = LangConstants {
    function_node_types: &[
        "function_declaration",
        "generator_function_declaration",
        "class_declaration",
        "method_definition",
        "lexical_declaration",
        // TypeScript-only (never appear in the JS grammar, so no-op
        // there): interface/type-alias/enum declarations are otherwise
        // invisible to the extractor entirely — a TS/DTO-only file would
        // index as 0 symbols. See node_kind_to_symbol_kind for the
        // SymbolKind mapping.
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
};

const JS_TS_BRANCH_KINDS: &[&str] = &[
    "if_statement",
    "for_statement",
    "for_in_statement",
    "while_statement",
    "do_statement",
    "switch_case",
    "catch_clause",
    "ternary_expression",
];

const DEFAULT_COMMENT_PREFIXES: &[&str] = &["//", "*", "/*", "#"];
// Ruby/shell/php: deliberately 3 prefixes, no "/*" — see is_comment_line's
// doc comment in parser.rs for why block-comment-open isn't recognized
// for this group.
const HASH_STYLE_COMMENT_PREFIXES: &[&str] = &["#", "//", "*"];

/// Single source of truth for every language this indexer dispatches on by
/// name. Order is insertion order (Tier-0 first, then Tier-0.5); lookup is a
/// linear scan via `find_spec` — fine at this size (15 entries, a handful
/// of calls per indexed file), no need for a `phf`/hash map.
pub static LANGUAGES: &[LanguageSpec] = &[
    LanguageSpec {
        name: "python",
        aliases: &[],
        extensions: &["py"],
        constants: LangConstants {
            function_node_types: &["function_definition", "class_definition"],
            name_field: "name",
            docstring_type: Some("expression_statement"), // Python docstrings are expression_statements
            call_node_types: &["call"],
            call_function_field: "function",
            call_function_field_by_kind: &[],
            class_node_types: &["class_definition"],
            class_name_field: "name",
        },
        ts_language: ts_lang_python,
        branch_node_kinds: &[
            "if_statement",
            "elif_clause",
            "for_statement",
            "while_statement",
            "except_clause",
            "boolean_operator",
            "conditional_expression",
            "case_clause",
        ],
        decorator_node_kinds: &["decorator"],
        binding_kinds: &["typed_parameter"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: None,
    },
    LanguageSpec {
        name: "rust",
        aliases: &[],
        extensions: &["rs"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_rust,
        branch_node_kinds: &[
            "if_expression",
            "if_let_expression",
            "match_arm",
            "while_expression",
            "while_let_expression",
            "for_expression",
            "loop_expression",
        ],
        decorator_node_kinds: &["attribute_item"],
        binding_kinds: &["parameter", "let_declaration", "field_declaration"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: None,
    },
    LanguageSpec {
        name: "go",
        aliases: &[],
        extensions: &["go"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_go,
        branch_node_kinds: &[
            "if_statement",
            "for_statement",
            "expression_case",
            "communication_case",
            "type_case",
        ],
        decorator_node_kinds: &[],
        binding_kinds: &["parameter_declaration", "var_spec", "field_declaration"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: None,
    },
    LanguageSpec {
        name: "javascript",
        aliases: &[],
        extensions: &["js", "jsx", "mjs", "cjs"],
        constants: JS_TS_CONSTANTS,
        ts_language: ts_lang_javascript,
        branch_node_kinds: JS_TS_BRANCH_KINDS,
        decorator_node_kinds: &["decorator"],
        // no static annotations in JS — no binding_kinds entry.
        binding_kinds: &[],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: None,
    },
    LanguageSpec {
        name: "typescript",
        aliases: &[],
        extensions: &["ts", "tsx"],
        constants: JS_TS_CONSTANTS,
        ts_language: ts_lang_typescript,
        branch_node_kinds: JS_TS_BRANCH_KINDS,
        decorator_node_kinds: &["decorator"],
        binding_kinds: &[
            "required_parameter",
            "optional_parameter",
            "public_field_definition",
            "property_signature",
        ],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: None,
    },
    LanguageSpec {
        name: "java",
        aliases: &[],
        extensions: &["java"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_java,
        branch_node_kinds: &[
            "if_statement",
            "for_statement",
            "while_statement",
            "do_statement",
            "switch_label",
            "switch_rule",
            "catch_clause",
            "ternary_expression",
        ],
        decorator_node_kinds: &["marker_annotation", "annotation"],
        binding_kinds: &["formal_parameter", "field_declaration"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: None,
    },
    // Tier-0.5 — full tree-sitter parsing when the optional grammar feature is enabled.
    // LangConstants are always present; `ts_language` gates the actual grammar behind the flag.
    // Ruby: call-graph extraction was silently 100% broken until
    // 2026-07-10 — this entry's `call_node_types` used to say
    // `&["method_call"]`, but the vendored grammar (`tree-sitter-ruby`) has
    // no node kind named "method_call" at all; confirmed by reading its own
    // `node-types.json` directly. The real call node is named `"call"` (it
    // does carry a `"method"` field, so `call_function_field` below was
    // already correct — only the node-kind string was wrong). Verified via
    // empirical re-index of a real fixture before/after: `call_edges`/
    // `call_sites` were 0 rows for every Ruby call, with or without
    // parens/an explicit receiver, before this fix.
    LanguageSpec {
        name: "ruby",
        aliases: &[],
        extensions: &["rb"],
        constants: LangConstants {
            function_node_types: &["method", "singleton_method", "class", "module"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call"],
            call_function_field: "method",
            call_function_field_by_kind: &[],
            class_node_types: &["class", "module"],
            class_name_field: "name",
        },
        ts_language: ts_lang_ruby,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        line_comment_prefixes: HASH_STYLE_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: Some(crate::indexer::parser::detect_ruby),
    },
    LanguageSpec {
        name: "php",
        aliases: &[],
        extensions: &["php"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_php,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &["simple_parameter", "property_declaration"],
        line_comment_prefixes: HASH_STYLE_COMMENT_PREFIXES,
        modifier_keywords: &[
            "public ",
            "private ",
            "protected ",
            "static ",
            "abstract ",
            "final ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_php),
    },
    // Kotlin: `LangConstants` had a full entry for this language long
    // before any tree-sitter-kotlin dependency existed at all (`ts_language`
    // had no way to load a grammar — always fell back to shallow
    // line-scan) — every field below was an untested guess. Verified
    // 2026-07-10 against the real grammar (tree-sitter-kotlin-ng 1.1.0) via
    // a throwaway AST-dump test:
    // - No node kind named "interface_declaration" exists at all (confirmed
    //   via node-types.json) — `interface Foo {}` parses as a plain
    //   `class_declaration`, already covered without a separate arm.
    // - `call_expression` has ZERO fields (`fields: []` in node-types.json,
    //   not `"callee"` as previously guessed) — the callee is just
    //   whichever expression comes first: a bare `identifier` for
    //   `println(...)`, or a `navigation_expression` for
    //   `this.foo()`/`Repo.save()`. `call_function_field: "$first_child"`
    //   is a sentinel `walk_calls` (parser.rs) special-cases to grab the
    //   call node's own first child regardless of kind, then let
    //   `split_receiver_callee` do its normal dot-splitting on that node's
    //   raw text — see `walk_calls`'s doc comment for why this is safe for
    //   every other language (the sentinel string is never used as a real
    //   field/kind name elsewhere).
    LanguageSpec {
        name: "kotlin",
        aliases: &[],
        extensions: &["kt", "kts"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_kotlin,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[
            "public ",
            "private ",
            "protected ",
            "internal ",
            "open ",
            "abstract ",
            "final ",
            "override ",
            "data ",
            "inline ",
            "suspend ",
            "companion ",
            "tailrec ",
            "operator ",
            "infix ",
            "external ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_kotlin),
    },
    // Swift: same history as Kotlin above — every field was an untested
    // guess until verified 2026-07-10 against the real grammar
    // (tree-sitter-swift =0.7.0, pinned below its published latest for an
    // unrelated ABI reason — see the workspace root Cargo.toml comment).
    // - No node kind named "struct_declaration" or "enum_declaration"
    //   exists at all (confirmed via node-types.json) — `struct Foo {}`/
    //   `enum Foo {}` both parse as plain `class_declaration`, same
    //   unification pattern as Kotlin's `interface` above. This loses the
    //   struct/enum/class distinction in `SymbolKind` (all become Class) —
    //   a documented scope cut, not a bug: distinguishing them would
    //   require inspecting a keyword child's text, not just the node kind.
    // - `call_expression` has ZERO fields, same shape as Kotlin — same
    //   `"$first_child"` sentinel applies (see Kotlin's comment above and
    //   `walk_calls`'s doc comment in parser.rs).
    LanguageSpec {
        name: "swift",
        aliases: &[],
        extensions: &["swift"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_swift,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[
            "public ",
            "private ",
            "internal ",
            "fileprivate ",
            "open ",
            "static ",
            "override ",
            "mutating ",
            "nonmutating ",
            "lazy ",
            "final ",
            "required ",
            "convenience ",
            "dynamic ",
            "optional ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_swift),
    },
    LanguageSpec {
        name: "csharp",
        aliases: &[],
        extensions: &["cs"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_csharp,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &["parameter", "variable_declaration"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[
            "public ",
            "private ",
            "protected ",
            "internal ",
            "static ",
            "abstract ",
            "virtual ",
            "override ",
            "sealed ",
            "async ",
            "partial ",
            "readonly ",
            "new ",
            "extern ",
            "unsafe ",
            "volatile ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_csharp),
    },
    LanguageSpec {
        name: "shell",
        aliases: &["bash"],
        extensions: &["sh", "bash"],
        constants: LangConstants {
            function_node_types: &["function_definition"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["command"],
            call_function_field: "name",
            call_function_field_by_kind: &[],
            class_node_types: &[],
            class_name_field: "name",
        },
        ts_language: ts_lang_shell,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        line_comment_prefixes: HASH_STYLE_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: Some(crate::indexer::parser::detect_shell),
    },
    // C: function names live inside a declarator chain (no direct "name" field);
    // resolve_name_node() has a special case that walks declarator → identifier.
    // struct_specifier/union_specifier/enum_specifier DO have a direct
    // "name" field, same as C++ below — without these the real grammar
    // path recognized only functions, strictly less than the old regex
    // fallback (detect_c_cpp), which at least caught struct/union/enum
    // keywords textually.
    LanguageSpec {
        name: "c",
        aliases: &[],
        extensions: &["c"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_c,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &["declaration", "field_declaration"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[
            "static ",
            "inline ",
            "virtual ",
            "explicit ",
            "constexpr ",
            "const ",
            "extern ",
            "volatile ",
            "friend ",
            "override ",
            "final ",
            "noexcept ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_c_cpp),
    },
    // C++: same declarator quirk as C for function_definition; class_specifier /
    // struct_specifier / enum_specifier DO have a direct "name" field.
    LanguageSpec {
        name: "cpp",
        aliases: &[],
        extensions: &["cc", "cpp", "cxx", "h", "hpp", "hxx"],
        constants: LangConstants {
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
        },
        ts_language: ts_lang_cpp,
        branch_node_kinds: &[],
        decorator_node_kinds: &[],
        binding_kinds: &["declaration", "field_declaration"],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[
            "static ",
            "inline ",
            "virtual ",
            "explicit ",
            "constexpr ",
            "const ",
            "extern ",
            "volatile ",
            "friend ",
            "override ",
            "final ",
            "noexcept ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_c_cpp),
    },
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
    LanguageSpec {
        name: "r",
        aliases: &[],
        extensions: &["r", "R"],
        constants: LangConstants {
            function_node_types: &["binary_operator"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["call"],
            call_function_field: "function",
            call_function_field_by_kind: &[],
            class_node_types: &[],
            class_name_field: "name",
        },
        ts_language: ts_lang_r,
        // `switch()` in R is an ordinary call, not grammar-level branching, so
        // it's invisible here (same class of gap as R having no class syntax).
        branch_node_kinds: &[
            "if_statement",
            "for_statement",
            "while_statement",
            "repeat_statement",
        ],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: Some(crate::indexer::parser::detect_r),
    },
    // Scala (Phase C, 2026-07-11): verified against the real grammar
    // (tree-sitter-scala 0.24.1, pinned below its published latest 0.26.0
    // for an ABI-14-vs-15 reason — see the workspace root Cargo.toml
    // comment) via a throwaway AST-dump test on a fixture covering a class
    // with a constructor param, a bare call, a receiver call (`this.foo()`),
    // an `object` singleton, a `trait`, and a `case class`. Unlike
    // Kotlin/Swift, `call_expression` DOES carry a real "function" field
    // (confirmed via node-types.json) for both bare and receiver calls —
    // no `"$first_child"` sentinel needed here.
    LanguageSpec {
        name: "scala",
        aliases: &[],
        extensions: &["scala", "sc"],
        constants: LangConstants {
            function_node_types: &[
                "function_definition",
                // Abstract trait member (`def name: String` with no body) —
                // a distinct node kind from `function_definition`, confirmed
                // via node-types.json.
                "function_declaration",
                "class_definition",
                "object_definition",
                "trait_definition",
            ],
            name_field: "name",
            // ScalaDoc (`/** ... */`) — node-types.json confirms two comment
            // kinds exist, "comment" (line, `//`) and "block_comment" (`/*
            // ... */`), same split as Kotlin.
            docstring_type: Some("block_comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            call_function_field_by_kind: &[],
            class_node_types: &["class_definition", "object_definition", "trait_definition"],
            class_name_field: "name",
        },
        ts_language: ts_lang_scala,
        branch_node_kinds: &[
            "if_expression",
            "while_expression",
            "do_while_expression",
            "for_expression",
            "case_clause",
            "catch_clause",
        ],
        decorator_node_kinds: &["annotation"],
        // No binding_kinds entry (scope cut, same as kotlin/swift/ruby/shell/r
        // above) — Scala's `val x: Foo = ...` type inference would need its
        // own binding_names_and_type arm, not just a binding_kinds list entry.
        binding_kinds: &[],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        // "case " so `case class`/`case object` reduce to the plain
        // `class `/`object ` prefix `detect_scala`'s class_kws loop matches;
        // "sealed " so `sealed trait`/`sealed class` do too.
        modifier_keywords: &[
            "private ",
            "protected ",
            "override ",
            "final ",
            "sealed ",
            "abstract ",
            "implicit ",
            "lazy ",
            "case ",
        ],
        shallow_detect: Some(crate::indexer::parser::detect_scala),
    },
    // Dart (Phase C, 2026-07-11): tree-sitter-dart has no git tags on its
    // source repo (github.com/ast-grep/tree-sitter-dart) — ABI verified by
    // downloading each published .crate tarball directly from
    // static.crates.io and grepping the vendored src/parser.c. 0.0.4 is the
    // newest ABI-14 release; 0.1.0/0.2.0 jumped to ABI 15. This is also an
    // older, hand-written grammar with a structurally different shape from
    // every other language here — verified via a real AST dump, not
    // guessed:
    // - No dedicated call-expression node kind at all (calls are a generic
    //   member_access/selector/argument_part postfix chain shared with
    //   plain field access) — call-graph extraction is a DELIBERATE,
    //   documented scope cut for this first pass (`call_node_types` is
    //   empty), not an oversight. See `test_dart_real_grammar_symbols_are_
    //   accurate` in parser.rs, which locks this gap in place.
    // - A method/constructor's name sits 2 levels deep with no field path
    //   all the way down (`class_member_definition` wraps `method_signature`
    //   or `declaration`, which in turn wraps an unnamed `function_signature`/
    //   `constructor_signature` positional child that finally has a real
    //   `name` field) — handled by a dedicated `resolve_name_node` arm in
    //   parser.rs, same pattern as C's declarator-chain walk.
    // - A top-level function declaration (`void main() {...}`) parses as a
    //   bare `lambda_expression` — the same node kind a true anonymous
    //   callback produces — disambiguated by whether its `parameters`
    //   (`function_signature`) has a `name` field (see `resolve_name_node`).
    LanguageSpec {
        name: "dart",
        aliases: &[],
        extensions: &["dart"],
        constants: LangConstants {
            function_node_types: &[
                "class_definition",
                "class_member_definition",
                "lambda_expression",
            ],
            name_field: "name",
            // Dart doc comments (`/// ...`) are a distinct node kind in this
            // grammar, not folded into the generic "comment" kind (confirmed
            // via node-types.json).
            docstring_type: Some("documentation_comment"),
            call_node_types: &[],
            call_function_field: "",
            call_function_field_by_kind: &[],
            class_node_types: &["class_definition"],
            class_name_field: "name",
        },
        ts_language: ts_lang_dart,
        branch_node_kinds: &[
            "if_statement",
            "for_statement",
            "while_statement",
            "switch_label",
            "catch_clause",
        ],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        line_comment_prefixes: DEFAULT_COMMENT_PREFIXES,
        modifier_keywords: &[],
        shallow_detect: Some(crate::indexer::parser::detect_dart),
    },
    // Lua (Phase C, 2026-07-11): verified against the real grammar
    // (tree-sitter-lua 0.2.0, pinned below its published latest 0.5.0 for
    // the same ABI-14-vs-15 reason as every other Tier-0.5 grammar here) via
    // a throwaway AST-dump test on a fixture covering a dot-qualified
    // function (`function Greeter.new(...)`), a method-colon function
    // (`function Greeter:greet()`), a `local function`, and both a bare and
    // a method-colon call site.
    //
    // `function_declaration`'s "name" field and `function_call`'s "name"
    // field both work UNIFORMLY across every call/definition shape (bare
    // identifier, dot-qualified, or method-colon) — no `"$first_child"`
    // sentinel needed, unlike Kotlin/Swift. `local function foo()` produces
    // the exact same `function_declaration` node as a global `function
    // foo()`, just referenced via a different field ("local_declaration")
    // on its parent `chunk` — the generic node-kind walk sees it either way,
    // no special case needed.
    //
    // Lua has no class/OOP syntax at the grammar level (metatables are a
    // runtime convention, not a parseable construct) — `class_node_types`
    // is empty, so every function is `SymbolKind::Function` with no
    // `class_context`, and a method-colon function's `name` field text
    // keeps its qualifier ("Greeter:greet", not bare "greet") — a
    // documented rough edge, not a bug (still findable via `name_tokens`
    // tokenization splitting on `.`/`:`). See
    // `test_lua_real_grammar_symbols_and_calls_are_accurate` in parser.rs.
    LanguageSpec {
        name: "lua",
        aliases: &[],
        extensions: &["lua"],
        constants: LangConstants {
            function_node_types: &["function_declaration"],
            name_field: "name",
            docstring_type: Some("comment"),
            call_node_types: &["function_call"],
            call_function_field: "name",
            call_function_field_by_kind: &[],
            class_node_types: &[],
            class_name_field: "name",
        },
        ts_language: ts_lang_lua,
        branch_node_kinds: &[
            "if_statement",
            "elseif_statement",
            "while_statement",
            "for_statement",
            "repeat_statement",
        ],
        decorator_node_kinds: &[],
        binding_kinds: &[],
        // Lua line comments are `--`/`--[[ ... ]]`, not `//`/`#`-style —
        // neither of the two shared groups applies.
        line_comment_prefixes: &["--"],
        // "local " so `local function foo()` reduces to the plain
        // `function foo()` the shallow detector's prefix check matches.
        modifier_keywords: &["local "],
        shallow_detect: Some(crate::indexer::parser::detect_lua),
    },
];

/// Look up a language's full descriptor by its canonical name or a known
/// alias. Linear scan over `LANGUAGES` (15 entries) — cheap relative to the
/// tree-sitter parse it gates.
pub fn find_spec(name: &str) -> Option<&'static LanguageSpec> {
    LANGUAGES
        .iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name))
}

pub fn get_lang_constants(lang: &str) -> Option<LangConstants> {
    find_spec(lang).map(|spec| spec.constants)
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
    find_spec(language)
        .map(|spec| spec.branch_node_kinds)
        .unwrap_or(&[])
}

/// Map a file extension to a language id.
/// Returns a tier-0 language (full parse + call-graph) for the six main
/// languages, and a tier-0.5 language id (shallow symbol-only extraction)
/// for additional common languages. Returns `None` for unknown extensions.
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    for spec in LANGUAGES {
        if spec.extensions.contains(&ext) {
            return Some(spec.name);
        }
    }
    match ext {
        // Standalone module (8-language plan P3.3) — not tree-sitter, see
        // `indexer::sql`'s module doc comment for why. Not a LANGUAGES
        // entry: it has no LangConstants/tree-sitter grammar/shallow
        // fallback, so it doesn't fit this registry's shape.
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
/// (an image, a lockfile, a doc) that still gets no row at all. Deliberately narrow — this is not a general "track every file" catch-all, just enough to
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

    /// Guards against a copy-paste extension collision as Phase C adds 9
    /// more `LanguageSpec` entries — two languages silently fighting over
    /// the same extension would make `language_for_extension` always
    /// return whichever one happens to sit first in `LANGUAGES`.
    #[test]
    fn language_registry_extensions_are_disjoint() {
        let mut seen = std::collections::HashMap::<&str, &str>::new();
        for spec in LANGUAGES {
            for &ext in spec.extensions {
                if let Some(owner) = seen.insert(ext, spec.name) {
                    panic!(
                        "extension {ext:?} claimed by both {owner:?} and {:?}",
                        spec.name
                    );
                }
            }
        }
    }

    /// Every `LANGUAGES` entry name must be unique — a duplicate would make
    /// `find_spec` non-deterministic (first match wins) instead of erroring.
    #[test]
    fn language_registry_names_are_unique() {
        let mut seen = std::collections::HashSet::<&str>::new();
        for spec in LANGUAGES {
            assert!(
                seen.insert(spec.name),
                "duplicate LANGUAGES entry: {}",
                spec.name
            );
        }
    }

    #[test]
    fn find_spec_resolves_alias() {
        let by_name = find_spec("shell").expect("shell must be registered");
        let by_alias = find_spec("bash").expect("bash alias must resolve");
        assert_eq!(by_name.name, by_alias.name);
    }
}
