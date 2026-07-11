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
    /// Best-effort "this is a test function" signal (rust `#[test]`/
    /// `#[tokio::test]`, python `test_*` under a test path, go `Test*` with a
    /// `*testing.T` param, java `@Test`). Used to exempt tests from dead-code
    /// analysis — they have no in-repo callers by design, invoked externally
    /// by the test harness just like `is_entry_point` symbols.
    pub is_test: bool,
    /// Enclosing class/impl type name for methods (`None` for free functions).
    /// Drives tier-2 method resolution.
    pub class_context: Option<String>,
    /// McCabe cyclomatic complexity (1 = no branches). Always 1 for
    /// languages without a real parse tree — see `branch_node_kinds`.
    pub complexity: i64,
}

use crate::graph::tokenize::tokenize_identifier;
use crate::indexer::lang_constants::get_lang_constants;

/// Map a tree-sitter node kind and context to the correct `SymbolKind`.
/// `in_class` is true when the node is a direct child of a class/impl scope,
/// which upgrades plain function definitions to Method.
fn node_kind_to_symbol_kind(node_kind: &str, in_class: bool) -> SymbolKind {
    match node_kind {
        "class_definition" | "class_declaration" => SymbolKind::Class,
        // Ruby's grammar uses the bare node kinds "class"/"module" (not
        // "class_declaration") — without this arm they fell through to the
        // generic Function/Method default below. Haskell's typeclass node is
        // ALSO literally named "class" (`class Named a where ...`) — since
        // this function takes no language parameter, that string collides
        // with Ruby's here and a typeclass maps to Class too (not the more
        // semantically-apt Trait) as a result. Accepted as a minor labeling
        // rough edge rather than threading a language parameter through
        // this whole function for one cosmetic distinction.
        "class" | "module" => SymbolKind::Class,
        "struct_item" => SymbolKind::Struct,
        // Java records and C#/C++/C struct declarations are all fixed-field
        // data carriers — the closest existing kind to a dedicated "record".
        "record_declaration" => SymbolKind::Struct,
        // C++ `class_specifier`/`struct_specifier` and C's `struct_specifier`
        // have direct "name" fields (see lang_constants.rs) but previously had
        // no arm here, so they silently fell through to Function/Method.
        "class_specifier" => SymbolKind::Class,
        "class_statement" => SymbolKind::Class, // PowerShell
        "struct_specifier" => SymbolKind::Struct,
        "struct_declaration" => SymbolKind::Struct, // C#
        "trait_item" => SymbolKind::Trait,
        "trait_declaration" => SymbolKind::Trait, // PHP
        // Scala: `trait Named { ... }` — confirmed via real grammar
        // (tree-sitter-scala 0.24.1) node-types.json, distinct node kind
        // from `class_definition`/`object_definition`. Same category as
        // PHP's trait_declaration above.
        "trait_definition" => SymbolKind::Trait,
        // Reachable only if `resolve_name_node` ever grows an `impl_item` case (it
        // currently doesn't: `impl_item` has no `name` field, only `type`/`trait`).
        // `impl_item` stays in Rust's `function_node_types` purely so its children
        // get `class_context` via `class_node_types` below — emitting impl blocks
        // themselves as symbols would add one noisy duplicate-named entry per
        // `impl`/`impl Trait for` block without a corresponding consumer.
        "impl_item" => SymbolKind::Impl,
        "interface_declaration" => SymbolKind::Interface,
        // Kotlin's `object` (singleton declaration) — closest existing kind
        // is Class (it has a body, can hold methods/properties, no
        // dedicated Object/Singleton variant exists). Found via a real
        // end-to-end index of a fixture file: without this arm, a
        // top-level `object Foo {}` silently fell through to the generic
        // Function/Method default below and was labeled `SymbolKind::Function`
        // — plausible-looking but wrong, the exact kind of bug the shallow
        // unit tests (which never call this function on kotlin/swift input)
        // can't catch.
        "object_declaration" => SymbolKind::Class,
        // Scala's `object Main { ... }` — same singleton concept as
        // Kotlin's `object_declaration` above, same treatment. Verified via
        // real grammar AST dump, not guessed — see this bug class's history
        // right above.
        "object_definition" => SymbolKind::Class,
        // Swift's `protocol` is exactly Swift's version of an interface —
        // same category of gap as `object_declaration` above, found the
        // same way (`Named` reported as `SymbolKind::Function` on a real
        // indexed fixture instead of Interface).
        "protocol_declaration" => SymbolKind::Interface,
        "enum_item" => SymbolKind::Enum, // Rust
        // TS `enum_declaration`, Java `enum_declaration`, C# `enum_declaration`,
        // C/C++ `enum_specifier` — one shared arm since the mapping only sees
        // the bare node-kind string, not which language produced it.
        "enum_declaration" | "enum_specifier" => SymbolKind::Enum,
        "constructor_declaration" => SymbolKind::Constructor, // Java
        // TS's `type_alias_declaration`, Go's `type_spec`/`type_alias` (each
        // spec inside a `type (...)` block — see lang_constants.rs's Go entry
        // and resolve_name_node), Rust's `type_item`/`union_item`, and C#'s
        // `delegate_declaration` are all "this name refers to a type" —
        // distinct grammar node kinds for the same concept, so all map here.
        // Haskell's `data`/`newtype` declaration — verified via node-types.json,
        // covers both sum types and single-constructor records; no separate
        // struct/enum distinction attempted (documented scope cut, same
        // category as Swift's class_declaration unification).
        // OCaml's `type_binding` (`type greeter = { name : string }`) joins
        // this same group.
        "data_type"
        | "type_binding"
        | "type_alias_declaration"
        | "type_spec"
        | "type_alias"
        | "type_item"
        | "union_item"
        | "union_specifier"
        | "delegate_declaration" => SymbolKind::Type,
        // Explicit method nodes are always Method regardless of scope.
        // PowerShell's `class_method_definition` is unambiguous by node
        // kind alone (distinct from `function_statement`) — joins this
        // group even though it gets no class_context (see lang_constants.rs's
        // PowerShell entry).
        "method_declaration" | "method_definition" | "class_method_definition" => {
            SymbolKind::Method
        }
        // JS/TS `const foo = () => {}` — treated as a variable holding a function.
        "lexical_declaration" => SymbolKind::Variable,
        // Zig top-level `const`/`var` (`const std = @import(...)`,
        // `const Greeter = struct {...}`) — same "variable holding
        // anything, including a type" treatment as JS/TS above; a
        // struct/enum-holding const doesn't get upgraded to Class/Enum
        // since node_kind_to_symbol_kind only ever sees this node's own
        // kind, never its RHS's kind (documented imprecision, not a bug).
        "variable_declaration" => SymbolKind::Variable,
        // R `foo <- function(x) {...}` — unlike JS's `const`, assignment is R's
        // *only* function-definition syntax (no separate `function foo() {}`
        // declaration form exists), so the bound name is treated as a real
        // Function rather than a Variable-holding-a-closure.
        "binary_operator" => SymbolKind::Function,
        // Plain function nodes: Method when inside a class/impl, Function otherwise.
        // Also covers TS/JS `generator_function_declaration` (see
        // lang_constants.rs) — a generator is still a plain function/method
        // for this index's purposes, no dedicated kind needed. Also covers
        // Scala's `function_definition`/`function_declaration` (abstract
        // trait member) — no dedicated kind needed there either, same
        // reasoning.
        _ => {
            if in_class {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            }
        }
    }
}
/// Parse `source` for a tier-0 `language` into a tree-sitter tree, or `None` if
/// the language is unsupported or parsing fails. Single source of the per-language
/// grammar mapping.
pub fn parse_tree(source: &str, language: &str) -> Option<tree_sitter::Tree> {
    let lang = (crate::indexer::lang_constants::find_spec(language)?.ts_language)()?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).ok()?;
    parser.parse(source, None)
}
pub fn extract_symbols(
    source: &str,
    language: &str,
    path: &str,
) -> Result<Vec<ParsedSymbol>, String> {
    let tree = parse_tree(source, language).ok_or("Failed to parse")?;
    Ok(extract_symbols_from_tree(&tree, source, language, path))
}

/// Same as [`extract_symbols`] but against an already-parsed tree, so callers
/// that need multiple extractions from one file (symbols, calls, imports,
/// types, aliases) can share a single tree-sitter parse instead of re-parsing
/// the same source once per extraction.
pub fn extract_symbols_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
    path: &str,
) -> Vec<ParsedSymbol> {
    let Some(lang_consts) = get_lang_constants(language) else {
        return Vec::new();
    };
    let mut symbols = Vec::new();
    walk_symbols(
        tree.root_node(),
        source,
        &lang_consts,
        language,
        path,
        None,
        Vec::new(),
        &mut symbols,
    );
    symbols
}

/// Resolve the name node for `node`. Most `function_node_types` expose `name`
/// directly via `lc.name_field` — this includes Go's `type_spec`/`type_alias`
/// (each spec inside a `type (...)` block has its own direct "name" field, so
/// walking them individually rather than the enclosing `type_declaration`
/// handles grouped blocks with N specs, not just the first). A few kinds wrap
/// the name on a nested child instead: JS/TS `lexical_declaration` holds it on
/// a `variable_declarator` child (only followed when the declarator's value is
/// itself a function literal, so plain `const x = 5` is not treated as a
/// symbol).
/// Whether `node` is a call this language's grammar can't structurally
/// distinguish from a real definition (Elixir: `def foo(x) do...end` is
/// literally a macro *call* to `def`) — checked by callee TEXT via
/// `definition_macro_names`, since no node-kind-level signal exists. Empty
/// `definition_macro_names` (every language but Elixir) means this is
/// always `false` — zero behavior change for everyone else.
fn is_definition_macro_call(
    node: tree_sitter::Node,
    source: &str,
    lc: &crate::indexer::lang_constants::LangConstants,
) -> bool {
    if lc.definition_macro_names.is_empty() {
        return false;
    }
    let Some(target) = node.child_by_field_name("target") else {
        return false;
    };
    let target_text = &source[target.byte_range()];
    lc.definition_macro_names.contains(&target_text)
}

fn resolve_name_node<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    lc: &crate::indexer::lang_constants::LangConstants,
) -> Option<tree_sitter::Node<'a>> {
    // Haskell's "bind" (`name = expr`, no parameters — e.g. `main = ...`)
    // needs a parent check before accepting its otherwise-direct "name"
    // field (see the "bind" arm below: the identical node shape also
    // represents a *local* `let`/`where` binding, which must NOT become a
    // symbol), so it's excluded from this generic fast path and handled
    // entirely in the match instead. No other language's function-defining
    // node kind is ever literally "bind", so this exclusion is inert
    // everywhere else.
    if node.kind() != "bind"
        && let Some(n) = node.child_by_field_name(lc.name_field)
    {
        return Some(n);
    }
    match node.kind() {
        "lexical_declaration" => {
            let mut cursor = node.walk();
            node.children(&mut cursor).find_map(|decl| {
                if decl.kind() != "variable_declarator" {
                    return None;
                }
                let value = decl.child_by_field_name("value")?;
                if matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "function"
                ) {
                    decl.child_by_field_name("name")
                } else {
                    None
                }
            })
        }
        // C and C++ function_definition: the function name is not in a direct
        // "name" field but nested inside a declarator chain:
        //   function_definition
        //     declarator: function_declarator | pointer_declarator | ...
        //       declarator: ... (recursively) → identifier
        // Walk the chain until we reach an identifier node.
        "function_definition" => {
            fn find_ident_in_declarator(n: tree_sitter::Node) -> Option<tree_sitter::Node> {
                if n.kind() == "identifier" || n.kind() == "field_identifier" {
                    return Some(n);
                }
                if let Some(inner) = n.child_by_field_name("declarator") {
                    return find_ident_in_declarator(inner);
                }
                None
            }
            node.child_by_field_name("declarator")
                .and_then(find_ident_in_declarator)
        }
        // R has no named-function syntax at all: `foo <- function(x) {...}`
        // binds an anonymous `function_definition` r-value to `foo` via a
        // `binary_operator` (fields lhs/operator/rhs). Only proceed when the
        // value side really is a function literal (optionally parenthesized),
        // so `x <- 5` or `a <- b` are never mistaken for symbols — same guard
        // shape as the `lexical_declaration` arm above.
        //
        // Bare right-assign, `function(x) {...} -> foo`, deliberately is NOT
        // recognized: verified against tree-sitter-r's actual parse that a
        // function body extends as far right as possible, so `-> foo` binds
        // *inside* the body (making `foo` part of the function, not its name)
        // rather than wrapping the definition — a real R precedence gotcha,
        // not a parser bug. Only the explicitly-parenthesized form,
        // `(function(x) {...}) -> foo`, is unambiguous and handled here.
        "binary_operator" => {
            let op = node.child_by_field_name("operator")?;
            let (name_side, value_side) = match op.kind() {
                "<-" | "<<-" | "=" | ":=" => ("lhs", "rhs"),
                "->" | "->>" => ("rhs", "lhs"),
                _ => return None,
            };
            let mut value = node.child_by_field_name(value_side)?;
            if value.kind() == "parenthesized_expression" {
                value = value.child_by_field_name("body")?;
            }
            if value.kind() != "function_definition" {
                return None;
            }
            let name = node.child_by_field_name(name_side)?;
            if name.kind() == "identifier" {
                Some(name)
            } else {
                None
            }
        }
        // Dart (tree-sitter-dart 0.0.4 — an older, hand-written grammar; the
        // only ABI-14-compatible published version, see lang_constants.rs's
        // Dart entry): a method/constructor's name sits 2 levels deep with
        // no field path all the way down. `class_member_definition` wraps
        // either `method_signature` (a method with a body) or `declaration`
        // (an abstract/interface method with no body, a constructor, OR a
        // plain field — the last of which correctly falls through to `None`
        // below since it has neither a `function_signature` nor
        // `constructor_signature` child). Verified via a real AST dump, not
        // guessed: both `function_signature` and `constructor_signature` are
        // *unnamed* positional children of that wrapper (no field to jump
        // straight to), but each carries a real `name` field of its own once
        // reached.
        "class_member_definition" => {
            let mut cursor = node.walk();
            node.children(&mut cursor).find_map(|wrapper| {
                if !matches!(wrapper.kind(), "method_signature" | "declaration") {
                    return None;
                }
                let mut inner_cursor = wrapper.walk();
                wrapper.children(&mut inner_cursor).find_map(|inner| {
                    if matches!(inner.kind(), "function_signature" | "constructor_signature") {
                        inner.child_by_field_name("name")
                    } else {
                        None
                    }
                })
            })
        }
        // Dart top-level function declarations (`void main() {...}`) parse
        // as a bare `lambda_expression` at the compilation-unit level in
        // this grammar — the same node kind a true anonymous callback
        // (`(x) => x + 1`) also produces. The two are told apart by whether
        // `parameters` (a `function_signature`) itself has a `name` field:
        // present for a real top-level declaration, absent for an anonymous
        // lambda — so this safely returns `None` for the latter instead of
        // manufacturing a fake symbol, same guard shape as the R arm above.
        "lambda_expression" => node
            .child_by_field_name("parameters")
            .and_then(|p| p.child_by_field_name("name")),
        // Elixir: homoiconic grammar, `def`/`defp` are macro calls (node
        // kind "call"), not a distinct definition node — see
        // `is_definition_macro_call` above and `LangConstants::
        // definition_macro_names`'s doc comment for the full story. Once
        // recognized as a real definition (not an ordinary call, which
        // falls through to `None` here, same as this arm never firing for
        // any other language), the defined name is the macro's own first
        // argument: a nested `call` (`greet(name)`, even with zero args —
        // `greet()` is still a `call`) when parens are present, or a bare
        // `identifier` when parens are omitted entirely (`def foo do
        // ...end` — valid Elixir). Verified via a real AST dump, not
        // guessed.
        "call" if is_definition_macro_call(node, source, lc) => {
            let mut cursor = node.walk();
            let arguments = node
                .children(&mut cursor)
                .find(|c| c.kind() == "arguments")?;
            let mut arg_cursor = arguments.walk();
            let first_arg = arguments.children(&mut arg_cursor).next()?;
            match first_arg.kind() {
                "call" => first_arg.child_by_field_name("target"),
                "identifier" => Some(first_arg),
                _ => None,
            }
        }
        // Haskell: `bind` is a zero-parameter definition (`main = ...`) —
        // confirmed via a real AST dump to be the EXACT SAME node kind for
        // both a real top-level definition and a local `let`/`where`
        // binding (`let g = Greeter {...}` inside a do-block). Only the
        // immediate parent tells them apart: a top-level bind's parent is
        // the module's `declarations` node; a local one's parent is
        // `local_binds` instead. Returning `None` for the local case (not
        // just "less precise" — genuinely wrong to index a local variable
        // as a module-level symbol) is what `test_haskell_real_grammar_
        // symbols_and_calls_are_accurate`'s `let g = ...` fixture locks in.
        "bind" => {
            if node.parent().is_some_and(|p| p.kind() == "declarations") {
                node.child_by_field_name("name")
            } else {
                None
            }
        }
        // OCaml: `let_binding` (direct "pattern" field, itself a bare
        // `value_name` for a simple non-destructuring binding) is the SAME
        // node kind for a real top-level definition (`let greet g = ...`)
        // and a local `let x = ... in ...` expression — same ambiguity
        // class as Haskell's "bind" above, but distinguished one level up:
        // every let_binding's immediate parent is always a
        // `value_definition` wrapper (for `and`-chains); THAT wrapper's own
        // parent is what actually tells them apart —
        // "structure"/"compilation_unit" for a real module-level
        // definition, vs "let_expression" for a local one. A destructuring
        // pattern (`let (a, b) = ...`, `pattern` kind != "value_name")
        // correctly returns `None` too — there's no single name to extract.
        "let_binding" => {
            let pattern = node.child_by_field_name("pattern")?;
            if pattern.kind() != "value_name" {
                return None;
            }
            let grandparent_kind = node.parent().and_then(|p| p.parent()).map(|gp| gp.kind());
            if matches!(
                grandparent_kind,
                Some("structure") | Some("compilation_unit")
            ) {
                Some(pattern)
            } else {
                None
            }
        }
        // Zig: `variable_declaration` (`const Greeter = struct {...}`,
        // `const std = @import(...)`) has NO "name" field at all (only
        // "type") — the identifier is a bare positional child. Same
        // ambiguity class as Haskell's "bind"/OCaml's "let_binding" once
        // again: a LOCAL `const`/`var` inside a function body produces the
        // identical node shape. This time only the IMMEDIATE parent
        // disambiguates ("source_file" for a real top-level declaration,
        // typically "block" for a local one) — one level shallower than
        // OCaml needed, confirmed via a real AST dump, not assumed by
        // analogy.
        "variable_declaration" => {
            if node.parent().is_some_and(|p| p.kind() == "source_file") {
                let mut cursor = node.walk();
                node.children(&mut cursor)
                    .find(|c| c.kind() == "identifier")
            } else {
                None
            }
        }
        // PowerShell: `class_statement`, `class_method_definition`, and
        // `function_statement` all have ZERO fields (confirmed via
        // node-types.json) — every name is a bare positional child, found
        // by kind rather than field name. `class_statement`'s own name and
        // `class_method_definition`'s method name are BOTH kind
        // "simple_name" (confirmed via a real AST dump: a method's
        // optional `[ReturnType]` sits before it, so "find the first
        // simple_name child" is what actually locates the name, not a
        // fixed child index). `function_statement` uses a different kind,
        // "function_name", for the same purpose.
        "class_statement" | "class_method_definition" => {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|c| c.kind() == "simple_name")
        }
        "function_statement" => {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|c| c.kind() == "function_name")
        }
        _ => None,
    }
}

/// Walks backward through contiguous same-kind comment siblings immediately
/// preceding `node` — no blank-line gap between any two, nor between the
/// last one and `node` itself — and joins them in source order. Line-
/// comment doc conventions (Rust `///`, Go `//`, Shell/C `#`) parse each
/// line as its *own* tree-sitter node, so taking only the single immediate
/// `prev_named_sibling()` (the old behavior) silently captured just the
/// *last* line of a multi-line doc comment and dropped the rest. Block-
/// comment conventions (`/** */`) are already one node spanning every line,
/// so they pass through unaffected — the loop just finds nothing above them
/// to merge, same net result as before.
///
/// Adjacency check: a `line_comment` node's `end_position().row` already
/// lands on the *following* line (tree-sitter's rust grammar folds the
/// terminating newline into the token), so two nodes are immediately
/// adjacent when `earlier.end_position().row == later.start_position().row`
/// — no `+ 1` needed (confirmed by walking the real parse tree; an earlier
/// version of this got that off by one and matched nothing).
fn collect_doc_comment_lines(node: tree_sitter::Node, source: &str, doc_type: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut current = node.prev_named_sibling();
    let mut expected_row = node.start_position().row;
    while let Some(n) = current {
        if n.kind() != doc_type || n.end_position().row != expected_row {
            break;
        }
        lines.push(source[n.byte_range()].trim().to_string());
        expected_row = n.start_position().row;
        current = n.prev_named_sibling();
    }
    lines.reverse();
    lines.join("\n")
}

/// Recursive symbol walk tracking the enclosing class/impl so methods record
/// their `class_context`.
#[allow(clippy::too_many_arguments)]
fn walk_symbols(
    node: tree_sitter::Node,
    source: &str,
    lc: &crate::indexer::lang_constants::LangConstants,
    language: &str,
    path: &str,
    enclosing_class: Option<String>,
    enclosing_decorators: Vec<String>,
    out: &mut Vec<ParsedSymbol>,
) {
    // A symbol defined here belongs to the class we are currently inside.
    if lc.function_node_types.contains(&node.kind())
        && let Some(name_node) = resolve_name_node(node, source, lc)
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
        } else if let Some(doc_type) = lc.docstring_type {
            docstring = collect_doc_comment_lines(node, source, doc_type);
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

        let is_entry_point = detect_entry_point(
            node,
            source,
            language,
            &name,
            &signature,
            &enclosing_decorators,
        );
        let is_test = detect_is_test(node, source, language, &name, &signature, path);
        let complexity = compute_cyclomatic_complexity(node, language);

        out.push(ParsedSymbol {
            qualified_name: name.clone(),
            name,
            kind: node_kind_to_symbol_kind(node.kind(), enclosing_class.is_some()),
            language: language.to_string(),
            path: path.to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature,
            docstring,
            name_tokens,
            is_entry_point,
            is_test,
            complexity,
            class_context,
        });
    }

    // Entering a class/impl sets the context for its descendants. Rust's
    // `trait_item` names itself via field `name` (a `type_identifier`) — it
    // does not share `impl_item`'s `class_name_field` ("type", the Self
    // type) — so the field to read can't come from the single
    // per-language `class_name_field` constant alone for this node kind.
    let child_class = if lc.class_node_types.contains(&node.kind()) {
        let name_field = if node.kind() == "trait_item" {
            "name"
        } else {
            lc.class_name_field
        };
        node.child_by_field_name(name_field)
            .map(|n| source[n.byte_range()].to_string())
            .or_else(|| enclosing_class.clone())
    } else {
        enclosing_class.clone()
    };

    // Container-decorator inheritance: a class/impl/etc. container's OWN
    // decorators/annotations (e.g. Python `@app.route`-style class
    // decorator, Rust `#[wasm_bindgen] impl Foo {..}`, TS/JS
    // `@Controller()`/`@Injectable()`, Java `@RestController` on the class)
    // are collected fresh for each container node reached — language-
    // agnostic, same recursive-propagation shape as `child_class` above —
    // and threaded down so `detect_entry_point` can let a public member
    // inherit "reachable via framework" from its container instead of only
    // ever checking the member's own decorators. Deliberately does NOT fall
    // back to `enclosing_decorators` the way `child_class` falls back to
    // `enclosing_class`: a nested container with no decorators of its own
    // should not inherit an ancestor's, keeping the signal scoped to direct
    // members of the decorated container, not arbitrarily deep nesting.
    let child_decorators = if lc.class_node_types.contains(&node.kind()) {
        container_decorators_of(node, source, language)
    } else {
        enclosing_decorators.clone()
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_symbols(
            child,
            source,
            lc,
            language,
            path,
            child_class.clone(),
            child_decorators.clone(),
            out,
        );
    }
}

/// A node's own decorators/annotations, language-agnostic entry point for
/// both `walk_symbols`'s container-decorator propagation and (indirectly, via
/// the member-level call in `detect_entry_point`) a single symbol's own
/// decorators. Three distinct grammar shapes, verified against each real
/// grammar rather than assumed:
/// - Java's `@Foo` parses as a CHILD of the declaration
///   (`collect_java_annotations`).
/// - TS/JS's decorator on a CLASS is a `decorator` FIELD directly on the
///   `class_declaration` node itself (`class_declaration decorator: (...)
///   name: ... body: ...`) — NOT a preceding sibling the way a class
///   MEMBER's own decorator is (a member's decorator sits as a sibling
///   inside `class_body`, which `collect_decorators`'s sibling walk already
///   handles correctly — only the container/class case needed this
///   field-based special-casing).
/// - Every other supported language's decorator/attribute (Python, Rust,
///   and TS/JS members) is a preceding SIBLING (`collect_decorators`).
fn container_decorators_of(node: tree_sitter::Node, source: &str, language: &str) -> Vec<String> {
    match language {
        "java" => collect_java_annotations(node, source)
            .into_iter()
            .map(String::from)
            .collect(),
        "javascript" | "typescript" if node.kind() == "class_declaration" => {
            let mut cursor = node.walk();
            node.children_by_field_name("decorator", &mut cursor)
                .map(|d| source[d.byte_range()].trim().to_string())
                .collect()
        }
        _ => collect_decorators(node, source, decorator_node_kinds(language))
            .into_iter()
            .map(String::from)
            .collect(),
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

/// Decorator/attribute sibling node kind(s) that may precede a definition, per
/// language. Java needs two: `@Foo` parses as `marker_annotation`, `@Foo(...)`
/// as `annotation`. TS/JS decorators (`@Controller()`, `@Injectable()`, ...)
/// parse as a single `decorator` node, same shape as Python's.
fn decorator_node_kinds(language: &str) -> &'static [&'static str] {
    crate::indexer::lang_constants::find_spec(language)
        .map(|spec| spec.decorator_node_kinds)
        .unwrap_or(&[])
}

/// Source text of every decorator/attribute immediately preceding `node`
/// (innermost first). Comment trivia (`line_comment`/`block_comment`/
/// `comment`) sitting between two attributes — or trailing the last one
/// right before `node` — is skipped rather than treated as a stop condition:
/// `#[test]\n// why\n#[should_panic]\nfn foo() {}` must still see both
/// attributes. Only a real, non-comment, non-matching sibling (i.e. actual
/// code) legitimately ends the chain.
fn collect_decorators<'a>(
    node: tree_sitter::Node,
    source: &'a str,
    kinds: &[&str],
) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut sib = node.prev_named_sibling();
    while let Some(s) = sib {
        if matches!(s.kind(), "line_comment" | "block_comment" | "comment") {
            sib = s.prev_named_sibling();
            continue;
        }
        if !kinds.contains(&s.kind()) {
            break;
        }
        out.push(source[s.byte_range()].trim());
        sib = s.prev_named_sibling();
    }
    out
}

/// Whether `d` (a Rust `#[attr]`/`#[attr(...)]`/`#[path::attr]` source
/// string) is a strong, general signal that something other than an
/// ordinary call site invokes or registers whatever it's attached to —
/// route/tool/RPC registration, FFI export, a plugin/handler framework,
/// etc. Shared between member-level and container-level (inherited)
/// attribute checks in `detect_entry_point`'s `"rust"` arm so both apply the
/// exact same classification.
fn rust_attr_is_dispatch_signal(d: &str) -> bool {
    // The small set of modifier attributes that don't imply this.
    const NON_DISPATCH_ATTRS: &[&str] = &[
        "allow",
        "deny",
        "warn",
        "forbid",
        "must_use",
        "deprecated",
        "inline",
        "cold",
        "cfg",
        "cfg_attr",
        "doc",
        "track_caller",
        "non_exhaustive",
        "repr",
        "should_panic",
        "ignore",
        "test",
        "derive",
        "automatically_derived",
    ];
    let inner = d.trim_start_matches("#[").trim_end_matches(']');
    let path = inner.split('(').next().unwrap_or(inner).trim();
    path == "main" || path.ends_with("::main") || !NON_DISPATCH_ATTRS.contains(&path)
}

/// Whether a Java annotation (`@Foo`/`@Foo(...)`/`@pkg.Foo`, as returned by
/// `collect_java_annotations`) is a well-known framework annotation implying
/// the annotated class/method is invoked/registered by something other than
/// an ordinary in-repo call — Spring-style component/route annotations and
/// JAX-RS resource annotations. Deliberately an allowlist (unlike Rust's
/// exclusion-list approach): Java has a much larger and less predictable
/// universe of annotations used for purposes that do NOT imply reachability
/// (validation, `@Override`, `@Deprecated`, `@SuppressWarnings`, ...), so
/// guessing "anything not obviously harmless" would be unsound here. `@Test`
/// is intentionally absent — that's `detect_is_test`'s signal, not an entry
/// point. Generic over `S: AsRef<str>` so it accepts both the member-level
/// `Vec<&str>` `collect_java_annotations` returns and the container-level
/// `&[String]` `walk_symbols` threads through.
fn java_annotation_signals_entry_point<S: AsRef<str>>(annotations: &[S]) -> bool {
    const FRAMEWORK_ANNOTATIONS: &[&str] = &[
        "RestController",
        "Controller",
        "Service",
        "Component",
        "Repository",
        "Configuration",
        "Bean",
        "RequestMapping",
        "GetMapping",
        "PostMapping",
        "PutMapping",
        "DeleteMapping",
        "PatchMapping",
        "EventListener",
        "Scheduled",
        "PostConstruct",
        "PreDestroy",
        "Path",
        "GET",
        "POST",
        "PUT",
        "DELETE",
    ];
    annotations.iter().any(|ann| {
        let ann = ann.as_ref();
        let inner = ann.trim_start_matches('@');
        let base = inner.split('(').next().unwrap_or(inner).trim();
        let base = base.rsplit('.').next().unwrap_or(base);
        FRAMEWORK_ANNOTATIONS.contains(&base)
    })
}

/// Whether a class/impl MEMBER is public enough to inherit an entry-point
/// signal from its enclosing container's own decorator/annotation (see
/// `detect_entry_point`'s `container_decorators` parameter). NOT the same
/// check as `analysis::dead_code::is_private_symbol`: that function's
/// TypeScript/JavaScript case checks for the `export` keyword, which is
/// right for top-level declarations but would always read a class METHOD as
/// private (individual methods are never themselves marked `export` — only
/// top-level declarations are), silently defeating the NestJS/Angular case
/// this parameter exists for. Bare/`protected` TS/JS members count as
/// public-enough here — only an explicit `private` modifier excludes them —
/// erring toward not missing a real entry point rather than tightening the
/// gate.
fn member_is_pub_for_container_inheritance(language: &str, signature: &str) -> bool {
    match language {
        "rust" => signature.contains("pub "),
        "java" => !signature.contains("private "),
        "typescript" | "javascript" => !signature.contains("private "),
        _ => false,
    }
}

/// Per-language entry-point convention: known framework decorators/attributes,
/// `main`/`init` functions, `export default`, and — for Rust/Java/TS/JS — a
/// public member inheriting the same signal from its enclosing container's
/// own decorator/annotation (`container_decorators`, propagated by
/// `walk_symbols`; empty for Python/Go, which don't have this gap — Python
/// already checks a class-level-vs-member-level distinction differently via
/// dunder methods, and Go has no class/decorator concept at all).
fn detect_entry_point(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    name: &str,
    signature: &str,
    container_decorators: &[String],
) -> bool {
    let decorators = collect_decorators(node, source, decorator_node_kinds(language));

    match language {
        "python" => {
            const HOOKS: &[&str] = &[
                ".route(",
                ".command(",
                ".get(",
                ".post(",
                ".put(",
                ".delete(",
                ".patch(",
            ];
            // Dunder methods (`__init__`, `__str__`, `__eq__`, `__iter__`, ...)
            // are invoked by Python's data model via protocol dispatch
            // (`str(x)` calls `__str__`, `for v in x` calls `__iter__`, a
            // constructor call `X()` calls `__init__`) — never by their
            // literal name at a call site, so a name-based call-graph can
            // never see a "caller" for them regardless of real usage.
            (name.len() > 4 && name.starts_with("__") && name.ends_with("__"))
                || decorators
                    .iter()
                    .any(|d| HOOKS.iter().any(|h| d.contains(h)))
        }
        "rust" => {
            // Common std/core trait methods dispatched via operator or
            // protocol syntax (`x.into()`, `x == y`, `for v in x`,
            // `x.clone()`, ...) rather than by their literal name at a call
            // site — invisible to a name-based call-graph regardless of how
            // many places genuinely invoke them through the trait.
            const TRAIT_DISPATCH_NAMES: &[&str] = &[
                "from",
                "try_from",
                "fmt",
                "drop",
                "deref",
                "deref_mut",
                "default",
                "clone",
                "eq",
                "ne",
                "partial_cmp",
                "cmp",
                "hash",
                "next",
                "into_iter",
                "index",
                "index_mut",
                "add",
                "sub",
                "mul",
                "div",
                "rem",
                "neg",
                "not",
                "bitand",
                "bitor",
                "bitxor",
                "as_ref",
                "as_mut",
                "borrow",
                "borrow_mut",
                "deserialize",
                "serialize",
            ];
            name == "main"
                || TRAIT_DISPATCH_NAMES.contains(&name)
                || decorators.iter().any(|d| rust_attr_is_dispatch_signal(d))
                || (member_is_pub_for_container_inheritance(language, signature)
                    && container_decorators
                        .iter()
                        .any(|d| rust_attr_is_dispatch_signal(d)))
        }
        "go" => node.kind() == "function_declaration" && (name == "main" || name == "init"),
        "java" => {
            signature.contains("public static void main")
                || (member_is_pub_for_container_inheritance(language, signature)
                    && (java_annotation_signals_entry_point(&collect_java_annotations(
                        node, source,
                    )) || java_annotation_signals_entry_point(container_decorators)))
        }
        "javascript" | "typescript" => {
            name == "main"
                || node
                    .parent()
                    .map(|p| {
                        p.kind() == "export_statement"
                            && source[p.byte_range()]
                                .trim_start()
                                .starts_with("export default")
                    })
                    .unwrap_or(false)
                || (member_is_pub_for_container_inheritance(language, signature)
                    && (!decorators.is_empty() || !container_decorators.is_empty()))
        }
        _ => false,
    }
}

/// Source text of every `annotation`/`marker_annotation` on `node`. Unlike
/// Rust's `#[attr]` (a preceding sibling of the item), tree-sitter-java parses
/// `@Foo` as a direct CHILD of the declaration node — either directly, or
/// nested one level inside a `modifiers` child — so this walks children
/// instead of `collect_decorators`'s prev-sibling walk.
fn collect_java_annotations<'a>(node: tree_sitter::Node, source: &'a str) -> Vec<&'a str> {
    fn is_annotation(kind: &str) -> bool {
        kind == "annotation" || kind == "marker_annotation"
    }
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_annotation(child.kind()) {
            out.push(source[child.byte_range()].trim());
        } else if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for grandchild in child.children(&mut mod_cursor) {
                if is_annotation(grandchild.kind()) {
                    out.push(source[grandchild.byte_range()].trim());
                }
            }
        }
    }
    out
}

/// McCabe cyclomatic complexity: 1 (baseline path) + 1 per decision-point
/// node in `node`'s subtree (see `branch_node_kinds`). Walks the full
/// subtree, so a nested function/closure defined inside `node` contributes
/// to the enclosing symbol's count too, in addition to getting its own
/// separate `ParsedSymbol` entry — this over-counts relative to some
/// stricter McCabe implementations that stop at nested function boundaries,
/// but keeps the walk simple and still gives a useful relative signal
/// ("this symbol's body, including anything defined inline in it, branches
/// a lot").
fn compute_cyclomatic_complexity(node: tree_sitter::Node, language: &str) -> i64 {
    let branch_kinds = crate::indexer::lang_constants::branch_node_kinds(language);
    if branch_kinds.is_empty() {
        return 1;
    }
    let mut complexity = 1i64;
    count_branch_nodes(node, branch_kinds, &mut complexity);
    complexity
}

fn count_branch_nodes(node: tree_sitter::Node, branch_kinds: &[&str], complexity: &mut i64) {
    if branch_kinds.contains(&node.kind()) {
        *complexity += 1;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count_branch_nodes(child, branch_kinds, complexity);
    }
}

/// Best-effort "is this a test function" signal, per language. Feeds
/// `is_test` on `ParsedSymbol` so dead-code analysis can exempt tests —
/// they have no in-repo callers by design (invoked by the test harness),
/// which otherwise makes every test function look like high-confidence
/// dead code. Javascript/typescript are not covered: Jest/Mocha tests are
/// anonymous callbacks passed to `it(`/`test(`, not named top-level symbols
/// the extractor would see here.
fn detect_is_test(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    name: &str,
    signature: &str,
    path: &str,
) -> bool {
    match language {
        "rust" => {
            let decorators = collect_decorators(node, source, decorator_node_kinds(language));
            decorators.iter().any(|d| {
                let inner = d.trim_start_matches("#[").trim_end_matches(']');
                let attr_path = inner.split('(').next().unwrap_or(inner).trim();
                attr_path == "test" || attr_path == "rstest" || attr_path.ends_with("::test")
            })
        }
        "python" => {
            let file_name = path.rsplit('/').next().unwrap_or(path);
            let test_path = file_name.starts_with("test_")
                || file_name.ends_with("_test.py")
                || path.contains("/tests/")
                || path.contains("/test/");
            name.starts_with("test_") && test_path
        }
        "go" => {
            path.ends_with("_test.go")
                && name.starts_with("Test")
                && name.len() > 4
                && name[4..5].chars().next().is_some_and(|c| c.is_uppercase())
                && signature.contains("*testing.T")
        }
        "java" => {
            let decorators = collect_java_annotations(node, source);
            decorators.iter().any(|d| {
                let inner = d.trim_start_matches('@');
                let ann_path = inner.split('(').next().unwrap_or(inner).trim();
                ann_path == "Test" || ann_path.ends_with(".Test")
            })
        }
        _ => false,
    }
}

/// A raw call site discovered in source, attributed to its enclosing function.
///
/// `enclosing_name`/`enclosing_line` identify the caller symbol; `enclosing_class`
/// is the class it lives in (for `self`/`this` resolution); `receiver` is the
/// object of a method call (`recv.method()`), enabling tier-2 type resolution.
#[derive(Debug)]
pub struct RawCall {
    pub enclosing_name: String,
    pub enclosing_line: usize,
    pub enclosing_class: Option<String>,
    pub callee: String,
    pub receiver: Option<String>,
    /// True when `receiver` came from a `Type::method()` scoped-path call
    /// (the path segment immediately before the last `::`) rather than a
    /// `recv.method()` field access. `receiver` is then already the type
    /// name itself — resolution must scope directly to that class, not go
    /// through the variable→type lookup a `.`-receiver needs.
    pub receiver_is_type_path: bool,
    /// True when this whole call expression is immediately `?`-tried
    /// (`foo.bar()?`) or immediately `.unwrap()`/`.expect(..)`-chained
    /// (`foo.bar().unwrap()`) — a cheap, sound (not heuristic-guessy)
    /// syntactic signal that the call's return type is `Option<_>`/`Result<_,_>`.
    /// Rust won't compile otherwise, so this is provable from the parse tree
    /// alone, no type inference needed. Used by `rebuild_graph` to drop
    /// bare-name fan-out candidates whose own signature can't possibly be
    /// `Option`/`Result` — see its `MAX_CALLEE_CANDIDATES` fallback.
    pub looks_option_or_result_chained: bool,
    /// The immediate module-path segment just before the callee, when the
    /// whole callee expression is a lowercase-qualified `::`-path
    /// (`crate::telemetry::timed_tool` → `Some("telemetry")`) with no `use`
    /// bringing the name into `file_symbols`/`import_map` — see
    /// `module_hint_of`. `None` for a `.`-receiver call, a type-qualified
    /// `Type::method()` call (already carried via `receiver`), or a bare
    /// unqualified name.
    pub module_hint: Option<String>,
    pub line: usize,
}

/// Keep the leading identifier of a segment (drop generics/parens/whitespace).
fn leading_ident(seg: &str) -> Option<String> {
    // '-' is included for PowerShell's Verb-Noun command convention
    // (`Write-Host`, `Get-ChildItem`) — a single token in that grammar, but
    // otherwise never part of a real identifier in any currently supported
    // language (all of which use '-' only as the subtraction operator,
    // never inside a bare identifier), so this is safe to extend globally:
    // confirmed via the full test suite staying green after this change.
    // Found via a real test failure (`Write-Host` resolving to callee
    // "Write" instead of "Write-Host"), not guessed.
    let ident: String = seg
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Split a callee expression into (immediate receiver, method/callee name,
/// whether that receiver is a type-path segment rather than a variable).
///
/// `self.method` → (Some("self"), "method", false);
/// `a.b.method` → (Some("b"), "method", false);
/// `HashMap::new` → (Some("HashMap"), "new", true) — the segment before the
/// last `::` is kept (not discarded) when it looks like a type name, since
/// that's the class an associated-function call like `Type::method()` must
/// resolve against; `mod::func` → (None, "func", false) — a lowercase
/// segment reads as a module, not a type (see `is_type_like`).
fn split_receiver_callee(raw: &str) -> Option<(Option<String>, String, bool)> {
    // `.` and `->` (C/C++ pointer member access) are treated as the same
    // priority tier — whichever is rightmost when both appear (e.g. a
    // mixed chain like `a.b->c()`) is the outermost/immediate access.
    // `->` is safe to add generically here (this function has no language
    // parameter, it's shared by every `walk_calls` caller): no currently
    // supported language's *call-expression* text ever contains a literal
    // `->` for any other reason — Rust's `->` only appears in a function
    // signature's return-type position, never inside a call's callee
    // expression. `::` stays strictly lower-priority than `.`/`->`, exactly
    // as before this change, so it still only fires when neither is present
    // — this preserves e.g. Rust turbofish `foo.bar::<T>()` resolving via
    // the dot branch (as it always has), not being hijacked by the `::`.
    let dot = raw.rfind('.').map(|i| (i, i + 1));
    let arrow = raw.rfind("->").map(|i| (i, i + 2));
    // Lua's method-call `:` (`self:foo()`, `g:greet()`) joins this same
    // priority tier — same reasoning as `->`: no other supported language's
    // callee text ever contains a genuine single `:`. Deliberately EXCLUDES
    // any `:` adjacent to another `:` so it never fires on the `::` pair
    // Rust turbofish/PHP static calls/Ruby namespaces use (those keep
    // falling through to the unchanged `::` tier below) — found via a real
    // test failure (`self:formatGreeting()` resolving to callee "self",
    // receiver None) when Lua support was added, not guessed.
    let colon = raw
        .rfind(':')
        .filter(|&i| {
            raw.as_bytes().get(i.wrapping_sub(1)) != Some(&b':')
                && raw.as_bytes().get(i + 1) != Some(&b':')
        })
        .map(|i| (i, i + 1));
    let dot_or_arrow_or_colon = [dot, arrow, colon]
        .into_iter()
        .flatten()
        .max_by_key(|&(i, _)| i);
    if let Some((idx, end)) = dot_or_arrow_or_colon {
        let (left, right) = raw.split_at(idx);
        let callee = leading_ident(&right[end - idx..])?;
        // Immediate receiver = last segment of the left side.
        let recv = left.rsplit(['.', ':']).next().and_then(leading_ident);
        Some((recv, callee, false))
    } else if let Some(idx) = raw.rfind("::") {
        let (left, right) = raw.split_at(idx);
        let callee = leading_ident(&right[2..])?;
        let recv = left.rsplit("::").next().and_then(leading_ident);
        let is_type = recv.as_deref().is_some_and(is_type_like);
        Some((if is_type { recv } else { None }, callee, is_type))
    } else {
        Some((None, leading_ident(raw)?, false))
    }
}
/// The module-path segment `split_receiver_callee` discards for a lowercase
/// (non-type-like) `::`-qualified callee, e.g. `crate::telemetry::timed_tool`
/// or `telemetry::timed_tool` → `Some("telemetry")`.
///
/// Without this, a fully-qualified call to a module-level function with no
/// `use` importing it is textually indistinguishable from a bare unqualified
/// call of the same name — `resolve_tier1` matches purely on bare name, and
/// `rebuild_graph`'s same-file preference (see its doc comment) can then bind
/// to an unrelated same-named symbol that merely happens to share the
/// caller's own file, silently misresolving the edge (in the worst case, to
/// a phantom self-recursive edge on the caller itself) instead of the module
/// actually named in the source. Preserved separately from `receiver` so
/// tier-2 method resolution (which expects a variable/type name, not a
/// module) is unaffected — this is consumed only by `rebuild_graph`'s
/// candidate selection, as a same-strength-as-`same_file` tiebreak that
/// takes priority when present, since an explicit qualifier in the source
/// text is stronger evidence than incidental file collocation.
fn module_hint_of(raw: &str) -> Option<String> {
    if raw.contains('.') {
        return None; // dot-form's own receiver already covers this call
    }
    let idx = raw.rfind("::")?;
    let (left, _) = raw.split_at(idx);
    let seg = left.rsplit("::").next().and_then(leading_ident)?;
    if is_type_like(&seg) {
        None // type-qualified — already carried via `receiver`/`receiver_is_type_path`
    } else {
        Some(seg)
    }
}

/// Heuristic: a path segment is "type-like" when it starts with an uppercase
/// letter, matching Rust/C#/Java/Kotlin/Swift convention for types/classes
/// (vs. snake_case modules or lowerCamelCase namespaces/packages). Not
/// perfect — code that doesn't follow the convention won't benefit — but a
/// false negative here just falls back to the pre-existing unscoped
/// resolution behavior, so the cost of missing one is low; a false positive
/// (treating a module as a type) just means a class-scoped lookup that
/// finds nothing, same as today.
pub(crate) fn is_type_like(segment: &str) -> bool {
    segment.chars().next().is_some_and(|c| c.is_uppercase())
}

/// True if `call_node` (a whole `recv.method(..)`/`Type::method(..)` call
/// expression) is immediately `?`-tried, or immediately followed by
/// `.unwrap()`/`.expect(..)`/`.unwrap_or*(..)`. Only Rust's grammar defines
/// `try_expression`; other languages' `call_node` never has one as a parent,
/// so this is always `false` there — harmless, not a Rust-only code path.
///
/// This is deliberately narrow (direct parent only, not an arbitrary walk up
/// the chain) — it only needs to be *sound* (no false "yes"), not complete.
/// A missed case just falls back to today's behavior; a wrong "yes" would
/// incorrectly drop a real candidate in `rebuild_graph`.
fn looks_option_or_result_chained(call_node: tree_sitter::Node, source: &str) -> bool {
    const UNWRAP_LIKE: &[&str] = &[
        "unwrap",
        "expect",
        "unwrap_or",
        "unwrap_or_default",
        "unwrap_or_else",
    ];
    let Some(parent) = call_node.parent() else {
        return false;
    };
    if parent.kind() == "try_expression" {
        return true;
    }
    if parent.kind() == "field_expression" {
        // Confirm `call_node` is the receiver (`value`) of this field access, not
        // some unrelated sibling — and that the field name is one of the
        // unwrap-like methods, and that the field access is itself being called
        // (`.unwrap()`, not just referenced as `.unwrap`).
        let is_value = parent
            .child_by_field_name("value")
            .is_some_and(|v| v.id() == call_node.id());
        let field_name = parent
            .child_by_field_name("field")
            .map(|f| &source[f.byte_range()]);
        let is_invoked = parent.parent().is_some_and(|gp| {
            gp.kind() == "call_expression"
                && gp
                    .child_by_field_name("function")
                    .is_some_and(|f| f.id() == parent.id())
        });
        if is_value && matches!(field_name, Some(f) if UNWRAP_LIKE.contains(&f)) && is_invoked {
            return true;
        }
    }
    // `.and_then(|x| EXPR)` — sound (not heuristic) one-level closure peel:
    // `Option::and_then`/`Result::and_then` require the closure's return type
    // to equal the *outer* Option/Result's own inner type, so if the whole
    // `.and_then(..)` call is itself provably Option/Result-chained (recurse),
    // then a single-expression closure body passed to it must be too — the
    // code wouldn't type-check otherwise. Restricted to `and_then` specifically
    // (not `.map(..)`, whose closure returns a plain value, not an `Option`/
    // `Result` — recursing there would be unsound) and to a closure whose body
    // *is* the call (single-expression closures only, not a `{ .. }` block
    // that merely contains it somewhere).
    if parent.kind() == "closure_expression"
        && parent
            .child_by_field_name("body")
            .is_some_and(|b| b.id() == call_node.id())
        && let Some(and_then_call) = parent.parent().and_then(|args| args.parent())
        && and_then_call.kind() == "call_expression"
        && let Some(fn_node) = and_then_call.child_by_field_name("function")
        && fn_node.kind() == "field_expression"
        && fn_node
            .child_by_field_name("field")
            .is_some_and(|f| &source[f.byte_range()] == "and_then")
    {
        return looks_option_or_result_chained(and_then_call, source);
    }
    false
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
    // `resolve_name_node` (not a raw `name_field` lookup) so the enclosing
    // symbol is found the same way `walk_symbols` finds it — e.g. JS/TS
    // `const foo = () => {...}` (name lives on a `variable_declarator` child,
    // not directly on `lexical_declaration`) or R `foo <- function(x) {...}`
    // (name lives on the `binary_operator`'s `lhs`, not a "name" field at
    // all). Without this, a call made *inside* one of those forms silently
    // got attributed to no enclosing symbol (or the wrong one).
    if consts.function_node_types.contains(&node.kind())
        && let Some(name_node) = resolve_name_node(node, source, consts)
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
        && !is_definition_macro_call(node, source, consts)
        && let Some((enc_name, enc_line)) = &current
        && let field_name = consts
            .call_function_field_by_kind
            .iter()
            .find(|(kind, _)| *kind == node.kind())
            .map_or(consts.call_function_field, |(_, f)| f)
        && let Some(fn_node) = node
            .child_by_field_name(field_name)
            .or_else(|| {
                // Fallback for a callee that isn't a named field at all — PHP's
                // `object_creation_expression` (`new Foo(...)`) names its callee
                // as a plain positional child of node-KIND "name" (confirmed via
                // the real grammar: no "type"/"name" field exists on this node),
                // not a field. Reuses `field_name` as the node-kind to search
                // for, so this only ever changes behavior for a
                // `call_function_field_by_kind` entry that's already known not
                // to resolve as a field — every pre-existing (call kind, field)
                // pairing still resolves via the field lookup above, unchanged.
                let mut cur = node.walk();
                node.children(&mut cur).find(|c| c.kind() == field_name)
            })
            .or_else(|| {
                // Sentinel for grammars where the call node has NO fields at all
                // and the callee can't be pinned to one fixed child kind either
                // — confirmed via real `node-types.json` for both Kotlin's and
                // Swift's `call_expression` (`fields: []`): the callee is
                // whichever expression comes first — a bare identifier for
                // `println(...)`, or a `navigation_expression` for `this.foo()`/
                // `Repo.save()`. Rather than teach this function a new node kind
                // per language, just take the call node's own first child
                // verbatim — its raw text ("println", or "this.foo") is exactly
                // what `split_receiver_callee` below already knows how to split
                // on the last `.`/`->`/`::`, so no new receiver-parsing logic is
                // needed either. Only ever reached when `field_name` is this
                // exact sentinel string, which no other language's
                // `call_function_field`/`call_function_field_by_kind` uses —
                // provably zero behavior change for every other language.
                if field_name == "$first_child" {
                    let mut cur = node.walk();
                    node.children(&mut cur).next()
                } else {
                    None
                }
            })
        && let Some((mut receiver, callee, mut receiver_is_type_path)) =
            split_receiver_callee(&source[fn_node.byte_range()])
    {
        // Some grammars keep the receiver and the bare callee name as two
        // SEPARATE fields on the call node itself ("object"/"scope" + the
        // `call_function_field`) rather than one combined dotted-path field
        // the way Rust's `field_expression`/Go's `selector_expression` do —
        // confirmed via the real grammar for PHP's
        // `member_call_expression`/`nullsafe_member_call_expression`
        // ("object" + "name") and `scoped_call_expression` ("scope" +
        // "name"), and for Java's `method_invocation` ("object" + "name",
        // same latent gap — `helper.run()` never carried a receiver before
        // this, only ever a bare "run"). `call_function_field`'s own text is
        // then just the bare callee, so `split_receiver_callee` above finds
        // no separator and returns `receiver: None` even though there
        // really is one. Fall back to the call node's own "object"/"scope"
        // field in that case: "object"'s last identifier segment is the
        // immediate receiver (mirrors the "receiver = last segment" rule
        // `split_receiver_callee` already uses for chained access); "scope"
        // additionally marks a type-path call (`Foo::bar()`), like Rust's
        // `::`. Ruby's `call` node names this same slot "receiver" instead
        // (confirmed via the real grammar's `node-types.json` — no other
        // language's call node in this table uses that field name, so this
        // arm is Ruby-only in practice) — `self.log_it()`/`Rails.logger.info`
        // would otherwise carry a callee with no receiver at all.
        if receiver.is_none() {
            if let Some(obj) = node.child_by_field_name("object") {
                receiver = last_ident_segment(&source[obj.byte_range()]);
            } else if let Some(scope) = node.child_by_field_name("scope") {
                let seg = last_ident_segment(&source[scope.byte_range()]);
                receiver_is_type_path = seg.as_deref().is_some_and(is_type_like);
                receiver = if receiver_is_type_path { seg } else { None };
            } else if let Some(recv) = node.child_by_field_name("receiver") {
                receiver = last_ident_segment(&source[recv.byte_range()]);
            }
        }
        out.push(RawCall {
            enclosing_name: enc_name.clone(),
            enclosing_line: *enc_line,
            enclosing_class: child_class.clone(),
            callee,
            receiver,
            receiver_is_type_path,
            module_hint: module_hint_of(&source[fn_node.byte_range()]),
            looks_option_or_result_chained: looks_option_or_result_chained(node, source),
            line: node.start_position().row + 1,
        });
    }

    // Elixir: a definition-macro call's own "arguments" child is its
    // SIGNATURE (the name+params wrapper `resolve_name_node` above just
    // extracted the name from, e.g. "greet" in `def greet(name) do`), not
    // real code — recursing into it would visit that same nested
    // `call(target: "greet")` node again as an ordinary call site and
    // record a phantom self-call (`greet` "calling" itself). Skipped only
    // for a `is_definition_macro_call` node, which is only ever true when
    // `definition_macro_names` is non-empty (Elixir only) — every other
    // language keeps recursing into every child exactly as before. Found
    // via a real failing assertion (`extract_calls` returning a bogus
    // `RawCall { enclosing_name: "greet", callee: "greet" }` edge), not
    // guessed.
    let skip_definition_signature = is_definition_macro_call(node, source, consts);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if skip_definition_signature && child.kind() == "arguments" {
            continue;
        }
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

/// The immediate (rightmost) identifier segment of a receiver/scope
/// expression's raw text — e.g. "helper" from PHP's "$this->helper" (a
/// nested property access used as another call's receiver), "Foo" from a
/// bare "Foo" scope. Strips a leading `$` (PHP variable sigil) since
/// tier-2 type_map lookup keys on the plain variable name either way.
fn last_ident_segment(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let arrow_end = raw.rfind("->").map(|i| i + 2);
    let colon_end = raw.rfind("::").map(|i| i + 2);
    let start = arrow_end.into_iter().chain(colon_end).max().unwrap_or(0);
    leading_ident(raw[start..].trim_start_matches('$'))
}
/// Extract call sites from a source file, each attributed to its enclosing function.
/// Top-level calls (outside any function) are skipped — they have no caller symbol.
pub fn extract_calls(source: &str, language: &str, _path: &str) -> Result<Vec<RawCall>, String> {
    let tree = parse_tree(source, language).ok_or("Failed to parse")?;
    Ok(extract_calls_from_tree(&tree, source, language))
}

/// Same as [`extract_calls`] but against an already-parsed tree (see
/// [`extract_symbols_from_tree`]).
pub fn extract_calls_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
) -> Vec<RawCall> {
    let Some(consts) = get_lang_constants(language) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_calls(tree.root_node(), source, &consts, None, None, &mut out);
    out
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
    extract_file_aliases_from_tree(&tree, source, language, ctx)
}

/// Same as [`extract_file_aliases`] but against an already-parsed tree (see
/// [`extract_symbols_from_tree`]).
pub fn extract_file_aliases_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
    ctx: &crate::resolver::FileContext,
) -> std::collections::HashMap<String, String> {
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
    let Some(tree) = parse_tree(source, language) else {
        return std::collections::HashMap::new();
    };
    extract_type_map_from_tree(&tree, source, language)
}

/// Same as [`extract_type_map`] but against an already-parsed tree (see
/// [`extract_symbols_from_tree`]).
pub fn extract_type_map_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    // Node kinds carrying a `name(s): type` (or `name(s) type`) binding.
    // Includes struct/class FIELD declarations (not just params/locals) so
    // tier-2 method resolution can disambiguate `x.foo()` by `x`'s real
    // field type across modules — without this, two unrelated types in
    // different modules sharing both a field name and a method name could
    // only be told apart by same-file/same-language heuristics, not by the
    // field's actual declared type.
    // Per-language binding node kinds now live in `LanguageSpec::binding_kinds`
    // (crates/calm-core/src/indexer/lang_constants.rs) — see each entry's own
    // doc comment there for why a given node kind was chosen per language.
    let Some(binding_kinds) = crate::indexer::lang_constants::find_spec(language)
        .map(|spec| spec.binding_kinds)
        .filter(|kinds| !kinds.is_empty())
    else {
        return map; // no static-annotation binding kinds for this language (e.g. javascript), or unrecognized
    };

    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if binding_kinds.contains(&node.kind()) {
            for (name, ty) in binding_names_and_type(node, source, language) {
                map.insert(name, ty);
            }
        }
        // Rust constructor inference: `let x = Foo::new(...)`, `Foo::default()`,
        // or `Foo { .. }` binds x to type Foo even without a type annotation.
        if language == "rust"
            && node.kind() == "let_declaration"
            && node.child_by_field_name("type").is_none()
            && let Some(pat) = node.child_by_field_name("pattern")
            && pat.kind() == "identifier"
            && let Some(value) = node.child_by_field_name("value")
            && let Some(ty) = rust_constructor_type(value, source)
        {
            map.insert(source[pat.byte_range()].to_string(), ty);
        }
        // Rust reassignment inference: `let mut x;` declares `x` with no
        // initializer (so the `let_declaration` case above never fires), and
        // its type only becomes apparent at a later `x = Foo::Variant;` /
        // `x = Foo::new(..);` assignment — typically one arm of an `if`/`match`
        // that builds up a state-machine-style value across branches (e.g.
        // this crate's own `EdgeConfidence` confidence variable in
        // `pipeline.rs::extract_file_data`). Every matching assignment for the
        // same `x` inserts the same type, which is what makes this safe across
        // multiple branches: whichever one the walk visits first or last,
        // `map[x]` ends up the same value.
        if language == "rust"
            && node.kind() == "assignment_expression"
            && let Some(lhs) = node.child_by_field_name("left")
            && lhs.kind() == "identifier"
            && let Some(rhs) = node.child_by_field_name("right")
            && let Some(ty) = rust_constructor_type(rhs, source)
        {
            map.insert(source[lhs.byte_range()].to_string(), ty);
        }
        // C# constructor inference: `var x = new Foo(...);` — `var`'s
        // "type" field is the literal identifier text "var" (not a real
        // type), so `binding_names_and_type`'s generic path above
        // deliberately treats it as absent (empty `ty`, see there) and
        // contributes nothing for this node; every declarator's real type
        // comes from its own initializer instead. Multiple declarators per
        // statement (`var a = new Foo(); `-style is one declarator, but the
        // grammar still allows `Foo a = ..., b = ...;` sharing one
        // `variable_declaration`) are each handled independently.
        if language == "csharp"
            && node.kind() == "variable_declaration"
            && node
                .child_by_field_name("type")
                .is_some_and(|t| source[t.byte_range()].trim() == "var")
        {
            let mut cur = node.walk();
            let declarators: Vec<_> = node
                .children(&mut cur)
                .filter(|c| c.kind() == "variable_declarator")
                .collect();
            for declarator in declarators {
                if let Some(name_node) = declarator.child_by_field_name("name")
                    && let Some(init) = csharp_declarator_initializer(declarator)
                    && let Some(ty) = csharp_constructor_type(init, source)
                {
                    map.insert(source[name_node.byte_range()].to_string(), ty);
                }
            }
        }
        // PHP constructor inference: `$x = new Foo();` — an untyped local
        // has no typed-property/parameter annotation to read, so this is
        // the only way its type becomes known for tier-2. Every PHP
        // assignment (there's no `let`/`var` declaration keyword the way
        // Rust/JS have) goes through `assignment_expression`, so one check
        // covers both a fresh binding and a later reassignment.
        if language == "php"
            && node.kind() == "assignment_expression"
            && let Some(lhs) = node.child_by_field_name("left")
            && lhs.kind() == "variable_name"
            && let Some(rhs) = node.child_by_field_name("right")
            && let Some(ty) = php_constructor_type(rhs, source)
        {
            let var = source[lhs.byte_range()].trim_start_matches('$').to_string();
            map.insert(var, ty);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    map
}

/// The type constructed by a Rust expression used as a `let` initializer (or,
/// via `assignment_expression` in `extract_type_map_from_tree`, a later
/// reassignment): `Foo::new(..)` / `Foo::default()` / `Foo::with_x(..)` ->
/// `Foo`; `Foo { .. }` (struct literal) -> `Foo`; a bare enum-variant path
/// `Foo::Variant` (no call parens) -> `Foo`. Returns `None` for anything else.
fn rust_constructor_type(value: tree_sitter::Node, source: &str) -> Option<String> {
    match value.kind() {
        // Foo::new(...) -- a call whose function is a scoped identifier.
        "call_expression" => {
            let func = value.child_by_field_name("function")?;
            if func.kind() != "scoped_identifier" {
                return None;
            }
            let path = func.child_by_field_name("path")?;
            // The type is the last path segment before the associated fn name.
            let seg = source[path.byte_range()].rsplit("::").next()?;
            first_type_ident(seg)
        }
        // Foo { .. } -- struct literal names its type directly.
        "struct_expression" => {
            let name = value.child_by_field_name("name")?;
            first_type_ident(&source[name.byte_range()])
        }
        // Foo::Variant -- a bare enum-variant path used as a value (no call
        // parens), e.g. `confidence = EdgeConfidence::Inferred;`. Same
        // last-segment-before-`::` extraction as the call case above, just
        // without a `function`/`call_expression` wrapper to unwrap first.
        "scoped_identifier" => {
            let path = value.child_by_field_name("path")?;
            let seg = source[path.byte_range()].rsplit("::").next()?;
            first_type_ident(seg)
        }
        _ => None,
    }
}

/// Keep a leading UpperCamelCase type identifier from `seg` (drop generics etc.);
/// returns None if it doesn't look type-like (avoids treating `foo::bar()` module
/// calls as constructors).
fn first_type_ident(seg: &str) -> Option<String> {
    let ident: String = seg
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.chars().next()?.is_uppercase() {
        Some(ident)
    } else {
        None
    }
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
    // C's `struct Shape *s;` / C++'s `class Foo { ... };` field types are a
    // whole `struct_specifier`/`class_specifier`/`union_specifier`/
    // `enum_specifier` node — its raw text is "struct Shape", not just
    // "Shape" — so pull the tag's own `name` field instead of using the
    // whole node's text (which would then never match a symbol's
    // `class_context`, itself just the bare tag name). Falls back to the
    // whole text for an anonymous struct/union (no `name` child), which
    // simply won't match any real class_context — harmless.
    let ty = if matches!(
        type_node.kind(),
        "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier"
    ) {
        type_node
            .child_by_field_name("name")
            .map(|n| source[n.byte_range()].trim().to_string())
            .unwrap_or_else(|| source[type_node.byte_range()].trim().to_string())
    } else {
        let raw = source[type_node.byte_range()]
            .trim_start_matches(':')
            .trim()
            .to_string();
        // C#'s `var` is implicit typing, not a real type name — its "type"
        // field is just the literal identifier text "var". Treat as absent
        // so this node contributes nothing here; `extract_type_map_from_tree`
        // has a separate constructor-inference block (mirroring Rust's own)
        // that infers the real type from `var x = new Foo(...)`'s
        // initializer instead.
        if language == "csharp" && raw == "var" {
            String::new()
        } else {
            raw
        }
    };
    if ty.is_empty() {
        return Vec::new();
    }

    let names: Vec<String> = match language {
        // `x, y Foo`: every identifier child shares the type. Go's struct
        // `field_declaration` exposes each repeated name as its own
        // `field_identifier` child (confirmed via the real grammar: e.g.
        // `X, Y int` emits two sibling `name: (field_identifier)` nodes on
        // one `field_declaration`), rather than the bare `identifier`
        // children `var_spec`/`parameter_declaration` use — same multi-name-
        // one-type shape, different node kind for the name itself.
        "go" => {
            let mut cur = node.walk();
            node.children(&mut cur)
                .filter(|c| c.kind() == "identifier" || c.kind() == "field_identifier")
                .map(|c| source[c.byte_range()].to_string())
                .collect()
        }
        // Python's typed_parameter names its identifier as the first child.
        "python" => node
            .named_child(0)
            .map(|n| source[n.byte_range()].to_string())
            .into_iter()
            .collect(),
        // Java's `field_declaration` shares one type across one-or-more
        // `variable_declarator` children (`int x, y;`) — the name lives on
        // each declarator, not on `field_declaration` itself (unlike
        // `formal_parameter`, which names itself directly and falls through
        // to the generic arm below unchanged).
        "java" if node.kind() == "field_declaration" => {
            let mut cur = node.walk();
            node.children(&mut cur)
                .filter(|c| c.kind() == "variable_declarator")
                .filter_map(|d| d.child_by_field_name("name"))
                .map(|n| source[n.byte_range()].to_string())
                .collect()
        }
        // C#'s `variable_declaration` (shared by both `field_declaration`
        // and `local_declaration_statement` — see `extract_type_map_from_tree`'s
        // "csharp" comment) shares one type across one-or-more
        // `variable_declarator` children, same multi-declarator shape as
        // Java's field_declaration above.
        "csharp" if node.kind() == "variable_declaration" => {
            let mut cur = node.walk();
            node.children(&mut cur)
                .filter(|c| c.kind() == "variable_declarator")
                .filter_map(|d| d.child_by_field_name("name"))
                .map(|n| source[n.byte_range()].to_string())
                .collect()
        }
        // PHP `simple_parameter`: "name" field is a `variable_name` node
        // whose own text includes the `$` sigil ("$x") — strip it since
        // tier-2 type_map lookup keys on the bare variable name.
        "php" if node.kind() == "simple_parameter" => node
            .child_by_field_name("name")
            .map(|n| source[n.byte_range()].trim_start_matches('$').to_string())
            .into_iter()
            .collect(),
        // PHP `property_declaration` shares one type across one-or-more
        // `property_element` children (`private Foo $a, $b;`) — each
        // element's own "name" field is again a `$`-prefixed `variable_name`.
        "php" if node.kind() == "property_declaration" => {
            let mut cur = node.walk();
            node.children(&mut cur)
                .filter(|c| c.kind() == "property_element")
                .filter_map(|el| el.child_by_field_name("name"))
                .map(|n| source[n.byte_range()].trim_start_matches('$').to_string())
                .collect()
        }
        // C/C++: the name lives inside the `declarator` field, which for a
        // pointer/reference/array declaration wraps another `declarator`
        // around the actual `identifier`/`field_identifier` (`Circle *c` is
        // a `pointer_declarator` around `identifier "c"`) — unwrap down to
        // it. `children_by_field_name` (not `child_by_field_name`) so a
        // multi-declarator statement (`Circle *a, *b;`) yields every name,
        // not just the first.
        "c" | "cpp" => {
            let mut cur = node.walk();
            node.children_by_field_name("declarator", &mut cur)
                .filter_map(|d| innermost_c_declarator_identifier(d, source))
                .collect()
        }
        // TS/Rust use a `pattern` field for parameters; Java's
        // `formal_parameter` and every field-declaration kind added above
        // (TS `public_field_definition`/`property_signature`, Rust/Go
        // `field_declaration`) name themselves directly via `name`.
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

/// Unwrap a C/C++ declarator chain (`pointer_declarator`, `reference_declarator`,
/// `array_declarator`, `parenthesized_declarator`, `init_declarator` — any
/// wrapper that itself has a `declarator` field) down to the innermost
/// `identifier`/`field_identifier`, returning its text. `None` for a
/// declarator shape with no plain identifier at its core (e.g. a function
/// pointer declarator) — those simply don't contribute a type_map entry.
fn innermost_c_declarator_identifier(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(source[node.byte_range()].to_string()),
        _ => node
            .child_by_field_name("declarator")
            .and_then(|d| innermost_c_declarator_identifier(d, source)),
    }
}

/// The initializer expression of a C# `variable_declarator` (`= <expr>`),
/// e.g. the `new Foo(...)` in `var x = new Foo(...);`. The grammar doesn't
/// name this field (confirmed via the real grammar — `=` and the value are
/// both plain positional children), so it's found by position: the node
/// right after the literal `=` token.
fn csharp_declarator_initializer<'a>(
    declarator: tree_sitter::Node<'a>,
) -> Option<tree_sitter::Node<'a>> {
    let mut cur = declarator.walk();
    let children: Vec<_> = declarator.children(&mut cur).collect();
    let eq_idx = children.iter().position(|c| c.kind() == "=")?;
    children.get(eq_idx + 1).copied()
}

/// C# constructor inference for `var x = new Foo(...);` — `object_creation_expression`
/// names its type directly via a `type` field, so unlike Rust's
/// `rust_constructor_type` this needs no path-segment unwrapping.
fn csharp_constructor_type(value: tree_sitter::Node, source: &str) -> Option<String> {
    if value.kind() != "object_creation_expression" {
        return None;
    }
    let ty = value.child_by_field_name("type")?;
    Some(source[ty.byte_range()].trim().to_string())
}

/// PHP constructor inference for `$x = new Foo();` — `object_creation_expression`
/// names its type as a positional child of node-kind "name" (confirmed via
/// the real grammar: no "type"/"name" *field* exists on this node, same gap
/// `walk_calls`' `object_creation_expression` fallback works around), not a
/// field lookup.
fn php_constructor_type(value: tree_sitter::Node, source: &str) -> Option<String> {
    if value.kind() != "object_creation_expression" {
        return None;
    }
    let mut cur = value.walk();
    let name_node = value.children(&mut cur).find(|c| c.kind() == "name")?;
    Some(source[name_node.byte_range()].to_string())
}
// ---------------------------------------------------------------------------
// Tier-0.5: lightweight regex/heuristic symbol extraction for languages that
// have no tree-sitter grammar registered in this build. Returns function and
// class-like names from a line-by-line scan. No call-site or import extraction
// — callers supply empty Vecs for those and skip resolver tiers entirely.
// ---------------------------------------------------------------------------

/// Leading identifier in `s` (alpha or `_` start, alphanumeric + `_` body).
fn ident_at_start(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(s.len());
    let name = &s[..end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
}

/// Identifier immediately before the first `(` on a line.
fn ident_before_paren(s: &str) -> Option<String> {
    let before = s.split('(').next()?.trim_end();
    let start = before
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    let name = &before[start..];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
}

/// Strip common visibility / storage modifier prefixes so the structural
/// keyword (`class`, `func`, `def`, …) appears at position 0.
fn strip_modifiers<'a>(s: &'a str, modifiers: &[&str]) -> &'a str {
    let mut s = s;
    loop {
        let prev = s;
        for &m in modifiers {
            s = s.strip_prefix(m).unwrap_or(s);
        }
        if s == prev {
            break;
        }
    }
    s.trim_start()
}

const C_SKIP: &[&str] = &[
    "if",
    "while",
    "for",
    "switch",
    "return",
    "else",
    "do",
    "assert",
    "static_assert",
    "sizeof",
    "decltype",
    "typeof",
    "alignof",
    "new",
    "delete",
    "throw",
    "catch",
    "using",
    "typedef",
    "case",
    "default",
];

pub(crate) fn detect_c_cpp(s: &str) -> Option<(String, SymbolKind)> {
    // Struct/class/namespace/union/enum
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("struct ", SymbolKind::Struct),
        ("namespace ", SymbolKind::Type),
        ("union ", SymbolKind::Struct),
        ("enum class ", SymbolKind::Enum),
        ("enum ", SymbolKind::Enum),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    // Function: has `(`, first word is not a control-flow keyword,
    // no `=` / `<<` to the left of `(` (would be a call in an expression).
    if !s.contains('(') {
        return None;
    }
    let first_word = s
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");
    if C_SKIP.contains(&first_word) {
        return None;
    }
    let before_paren = s.split('(').next().unwrap_or("");
    if before_paren.contains('=') || before_paren.contains('<') {
        return None;
    }
    let name = ident_before_paren(s)?;
    if C_SKIP.contains(&name.as_str()) {
        return None;
    }
    Some((name, SymbolKind::Function))
}

pub(crate) fn detect_csharp(s: &str) -> Option<(String, SymbolKind)> {
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("interface ", SymbolKind::Interface),
        ("struct ", SymbolKind::Struct),
        ("enum ", SymbolKind::Enum),
        ("record class ", SymbolKind::Class),
        ("record struct ", SymbolKind::Struct),
        ("record ", SymbolKind::Class),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    // Method: `ReturnType Name(` — must have a type then an ident before `(`
    if s.contains('(') {
        let before = s.split('(').next().unwrap_or("").trim_end();
        // There must be a space (separating return type from name)
        if before.contains(' ') {
            let name = ident_before_paren(s)?;
            // Skip known non-method keywords
            if !matches!(
                name.as_str(),
                "if" | "while"
                    | "for"
                    | "switch"
                    | "foreach"
                    | "using"
                    | "return"
                    | "catch"
                    | "throw"
                    | "new"
                    | "base"
                    | "this"
            ) {
                return Some((name, SymbolKind::Function));
            }
        }
    }
    None
}

pub(crate) fn detect_ruby(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("def ") {
        // `def name` or `def self.name` or `def ClassName.name`
        let name_part = rest.trim_start();
        let name = if let Some(after_dot) = name_part.find('.') {
            ident_at_start(&name_part[after_dot + 1..])
        } else {
            ident_at_start(name_part)
        }?;
        return Some((name, SymbolKind::Function));
    }
    for kw in &["class ", "module "] {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, SymbolKind::Class));
        }
    }
    None
}

pub(crate) fn detect_shell(s: &str) -> Option<(String, SymbolKind)> {
    // `function name` or `function name()` or `name()` at line start
    if let Some(rest) = s.strip_prefix("function ") {
        let name = ident_at_start(rest.trim_start())?;
        return Some((name, SymbolKind::Function));
    }
    // `name() {` or `name () {` — but not `name=(...)` / `name=$(...)`
    // (array or command-substitution assignment), which also has a `(`
    // with no space before it. Same guard `detect_c_cpp` already applies.
    if let Some(paren_pos) = s.find('(') {
        let before = s[..paren_pos].trim_end();
        if !before.contains(' ') && !before.contains('=') && !before.is_empty() {
            let name = ident_at_start(before)?;
            return Some((name, SymbolKind::Function));
        }
    }
    None
}

pub(crate) fn detect_kotlin(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("fun ") {
        let name = ident_at_start(rest.trim_start())?;
        return Some((name, SymbolKind::Function));
    }
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("object ", SymbolKind::Class),
        ("interface ", SymbolKind::Interface),
        ("enum class ", SymbolKind::Enum),
        ("sealed class ", SymbolKind::Class),
        ("sealed interface ", SymbolKind::Interface),
        ("annotation class ", SymbolKind::Class),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    None
}

pub(crate) fn detect_swift(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("func ") {
        let name = ident_at_start(rest.trim_start())?;
        return Some((name, SymbolKind::Function));
    }
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("struct ", SymbolKind::Struct),
        ("enum ", SymbolKind::Enum),
        ("protocol ", SymbolKind::Interface),
        ("extension ", SymbolKind::Impl),
        ("actor ", SymbolKind::Class),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    None
}

pub(crate) fn detect_php(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("function ") {
        // `function &name(` — strip leading `&`
        let rest = rest.trim_start().trim_start_matches('&');
        let name = ident_at_start(rest)?;
        return Some((name, SymbolKind::Function));
    }
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("interface ", SymbolKind::Interface),
        ("trait ", SymbolKind::Trait),
        ("enum ", SymbolKind::Enum),
        ("abstract class ", SymbolKind::Class),
        ("final class ", SymbolKind::Class),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    None
}

fn is_comment_line(trimmed: &str, language: &str) -> bool {
    const DEFAULT_PREFIXES: &[&str] = &["//", "*", "/*", "#"];
    let prefixes = crate::indexer::lang_constants::find_spec(language)
        .map(|spec| spec.line_comment_prefixes)
        .unwrap_or(DEFAULT_PREFIXES);
    prefixes.iter().any(|p| trimmed.starts_with(p))
}

// R has no dedicated function-declaration keyword: `name <- function(...)`
// is an ordinary assignment. Real-grammar mode (the `lang-r` feature)
// resolves this precisely via `resolve_name_node`'s "binary_operator" arm;
// this line-scan fallback only approximates the common left-assign forms
// (no `->` right-assign support — rare enough in practice to skip here).
pub(crate) fn detect_r(s: &str) -> Option<(String, SymbolKind)> {
    for sep in ["<<-", "<-", "="] {
        let Some(idx) = s.find(sep) else { continue };
        let lhs = s[..idx].trim();
        let rhs = s[idx + sep.len()..].trim_start();
        if !(rhs.starts_with("function(") || rhs.starts_with("function (")) {
            continue;
        }
        let Some(name) = r_ident_at_start(lhs) else {
            continue;
        };
        if name == lhs {
            return Some((name, SymbolKind::Function));
        }
    }
    None
}

// Like `ident_at_start`, but also allows `.` — common in idiomatic R names
// (S3 method dispatch like `print.myclass`, or the `.hidden` "private by
// convention" prefix), which the shared helper deliberately excludes for
// the other shallow-mode languages.
fn r_ident_at_start(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
        .unwrap_or(s.len());
    let name = &s[..end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
}

// Scala: modifiers ("private "/"sealed "/"case "/etc.) are stripped by
// `strip_modifiers` (via LanguageSpec::modifier_keywords) before this runs,
// so "sealed trait X"/"case class Y" both reduce to a plain "trait X"/
// "class Y" the class_kws loop below can match directly — verified against
// the real grammar (tree-sitter-scala 0.24.1) node-types.json, same as the
// tree-sitter path's LangConstants entry in lang_constants.rs.
pub(crate) fn detect_scala(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("def ") {
        let name = ident_at_start(rest.trim_start())?;
        return Some((name, SymbolKind::Function));
    }
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("object ", SymbolKind::Class),
        ("trait ", SymbolKind::Trait),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    None
}

// Dart shallow fallback: class-only, deliberately no function detection.
// Unlike every other Tier-0.5 language here, Dart has no keyword
// ("def"/"fun"/"function") preceding a top-level or method declaration —
// it's just `ReturnType name(params) {`, indistinguishable by a one-line
// regex/prefix scan from a field declaration or a call continuation
// without risking real false positives. The real tree-sitter path (when
// the `lang-dart` grammar is compiled in) handles this correctly via
// `resolve_name_node`'s dedicated arms; this fallback only ever runs when
// that grammar isn't available, so a class-only shallow index is a
// documented, honest scope cut rather than a guess that could misfire.
pub(crate) fn detect_dart(s: &str) -> Option<(String, SymbolKind)> {
    let class_kws: &[(&str, SymbolKind)] = &[
        ("class ", SymbolKind::Class),
        ("abstract class ", SymbolKind::Class),
        ("mixin ", SymbolKind::Class),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    None
}

// Lua identifiers in shallow mode may carry a table/method qualifier
// (`Greeter.new`, `Greeter:greet`) since Lua has no separate class name
// syntax — matches the real tree-sitter path's behavior for the same forms
// (see lang_constants.rs's Lua LangConstants comment).
fn lua_ident_at_start(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '.' && c != ':')
        .unwrap_or(s.len());
    let name = &s[..end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
}

pub(crate) fn detect_lua(s: &str) -> Option<(String, SymbolKind)> {
    let rest = s.strip_prefix("function ")?;
    let name = lua_ident_at_start(rest)?;
    Some((name, SymbolKind::Function))
}

pub(crate) fn detect_elixir(s: &str) -> Option<(String, SymbolKind)> {
    for kw in ["def ", "defp "] {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, SymbolKind::Function));
        }
    }
    None
}

// Haskell shallow fallback: data/newtype/class declarations only,
// deliberately no function-equation detection. Unlike every keyword-led
// language here, a Haskell function definition (`greet g = ...`) has no
// distinguishing keyword at all — a one-line prefix scan can't tell it
// apart from a type signature (`greet :: ...`) or plain expression without
// real parsing. The real tree-sitter path (see lang_constants.rs's Haskell
// entry) handles this correctly; this fallback only runs when that grammar
// isn't compiled in, so a declarations-only shallow index is a documented,
// honest scope cut, same reasoning as Dart's shallow fallback.
pub(crate) fn detect_haskell(s: &str) -> Option<(String, SymbolKind)> {
    let class_kws: &[(&str, SymbolKind)] = &[
        ("data ", SymbolKind::Type),
        ("newtype ", SymbolKind::Type),
        ("class ", SymbolKind::Trait),
    ];
    for (kw, kind) in class_kws {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, *kind));
        }
    }
    None
}

// OCaml shallow fallback: "let rec " checked before the plainer "let " (a
// prefix of it) so `let rec foo x = ...` doesn't misdetect "rec" as the
// name. Like Haskell, no real way to tell a top-level `let` from a nested
// `let ... in` by a one-line prefix scan alone (the real tree-sitter path
// uses a grandparent-node check instead — see lang_constants.rs's OCaml
// entry); this fallback only runs when that grammar isn't compiled in, so
// accepting some over-detection here (unlike Haskell, which just skips
// function detection entirely) is a documented, honest tradeoff for this
// safety-net path specifically.
pub(crate) fn detect_ocaml(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("type ")
        && let Some(name) = ident_at_start(rest)
    {
        return Some((name, SymbolKind::Type));
    }
    for kw in ["let rec ", "let "] {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, SymbolKind::Function));
        }
    }
    None
}

pub(crate) fn detect_zig(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("fn ")
        && let Some(name) = ident_at_start(rest)
    {
        return Some((name, SymbolKind::Function));
    }
    for kw in ["const ", "var "] {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident_at_start(rest)
        {
            return Some((name, SymbolKind::Variable));
        }
    }
    None
}

// PowerShell function names commonly use the Verb-Noun convention
// (`Get-Something`) — the plain `ident_at_start` (alnum/underscore only)
// would truncate at the hyphen, so this needs its own hyphen-tolerant scan.
fn powershell_ident_at_start(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .unwrap_or(s.len());
    let name = &s[..end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
}

pub(crate) fn detect_powershell(s: &str) -> Option<(String, SymbolKind)> {
    if let Some(rest) = s.strip_prefix("function ")
        && let Some(name) = powershell_ident_at_start(rest)
    {
        return Some((name, SymbolKind::Function));
    }
    if let Some(rest) = s.strip_prefix("class ")
        && let Some(name) = ident_at_start(rest)
    {
        return Some((name, SymbolKind::Class));
    }
    None
}
fn detect_shallow(trimmed: &str, language: &str) -> Option<(String, SymbolKind)> {
    let spec = crate::indexer::lang_constants::find_spec(language)?;
    let s = strip_modifiers(trimmed, spec.modifier_keywords);
    spec.shallow_detect.and_then(|f| f(s))
}

/// Lightweight line-scan symbol extraction for Tier-0.5 languages (those
/// without a tree-sitter grammar registered in `parse_tree`). Returns function
/// and class-like names with path-qualified names and FTS-ready tokens.
/// No call-site or import extraction — callers skip resolver tiers entirely.
pub fn extract_symbols_shallow(source: &str, language: &str, path: &str) -> Vec<ParsedSymbol> {
    let mut out: Vec<ParsedSymbol> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();

    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_comment_line(trimmed, language) {
            continue;
        }
        let Some((name, kind)) = detect_shallow(trimmed, language) else {
            continue;
        };
        let mut qn = format!("{}::{}", path, name);
        if !seen.insert(qn.clone()) {
            qn = format!("{}#{}", qn, idx + 1);
            seen.insert(qn.clone());
        }
        out.push(ParsedSymbol {
            qualified_name: qn,
            name: name.clone(),
            kind,
            language: language.to_string(),
            path: path.to_string(),
            line_start: idx + 1,
            line_end: idx + 1,
            signature: trimmed.chars().take(120).collect(),
            docstring: String::new(),
            name_tokens: tokenize_identifier(&name),
            is_entry_point: false,
            is_test: false,
            class_context: None,
            complexity: 1,
        });
    }
    out
}

/// Markdown ATX headings (`#`..`######`) as searchable symbols — not a
/// tree-sitter grammar, standalone line-scan same shape as
/// `indexer::sql`'s extractor (see `extract_file_data`'s `lang ==
/// "markdown"` branch). Deliberately NOT routed through the shared
/// `extract_symbols_shallow`/`detect_shallow`/`is_comment_line`: their
/// default `#`-as-comment rule would eat every heading, and that dispatch
/// is stateless per-line so it can't track fenced-code-block state —
/// needed here so a `# comment` inside a ```python/```bash doc example
/// isn't mistaken for a heading.
pub fn extract_markdown_symbols(source: &str, path: &str) -> Vec<ParsedSymbol> {
    let mut out: Vec<ParsedSymbol> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    let mut in_fence = false;
    let mut fence_marker: Option<char> = None;

    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let fence_len = trimmed
            .chars()
            .take_while(|&c| c == '`' || c == '~')
            .count();
        if fence_len >= 3 {
            let marker = trimmed.chars().next();
            if in_fence {
                if marker == fence_marker {
                    in_fence = false;
                    fence_marker = None;
                }
            } else {
                in_fence = true;
                fence_marker = marker;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        // CommonMark: 4+ leading spaces makes this a code block, not a
        // heading — checked against the untrimmed line, not `trimmed`.
        if line.len() - trimmed.len() >= 4 {
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if hashes == 0 || hashes > 6 {
            continue;
        }
        let rest = &trimmed[hashes..];
        if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
            // e.g. `#!/usr/bin/env` in a fence-less snippet, or a bare
            // hashtag-like token — not a heading per CommonMark.
            continue;
        }
        let text = rest.trim().trim_end_matches('#').trim();
        if text.is_empty() {
            continue;
        }
        let mut qn = format!("{}::{}", path, text);
        if !seen.insert(qn.clone()) {
            qn = format!("{}#{}", qn, idx + 1);
            seen.insert(qn.clone());
        }
        out.push(ParsedSymbol {
            qualified_name: qn,
            name: text.to_string(),
            kind: SymbolKind::Heading,
            language: "markdown".to_string(),
            path: path.to_string(),
            line_start: idx + 1,
            line_end: idx + 1,
            signature: trimmed.chars().take(120).collect(),
            docstring: String::new(),
            name_tokens: tokenize_identifier(text),
            is_entry_point: false,
            is_test: false,
            class_context: None,
            complexity: 1,
        });
    }
    out
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
            ("class C { void M(Foo x) {} }\n", "csharp"),
            ("<?php\nfunction f(Foo $x) {}\n", "php"),
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
    fn php_property_declaration_shares_one_type_across_elements() {
        let m = extract_type_map("<?php\nclass C { private Foo $a, $b; }\n", "php");
        assert_eq!(m.get("a"), Some(&"Foo".to_string()));
        assert_eq!(m.get("b"), Some(&"Foo".to_string()));
    }

    #[test]
    fn php_assignment_infers_type_from_constructor() {
        let m = extract_type_map(
            "<?php\nclass C { function m() { $x = new Foo(); } }\n",
            "php",
        );
        assert_eq!(
            m.get("x"),
            Some(&"Foo".to_string()),
            "$x = new Foo() must infer x: Foo from the constructor"
        );
    }

    #[test]
    fn csharp_field_and_local_share_one_type_across_declarators() {
        // field_declaration
        let m = extract_type_map("class C { Foo a, b; }\n", "csharp");
        assert_eq!(m.get("a"), Some(&"Foo".to_string()));
        assert_eq!(m.get("b"), Some(&"Foo".to_string()));
        // local_declaration_statement (both wrap the same variable_declaration shape)
        let m = extract_type_map("class C { void M() { Foo local; } }\n", "csharp");
        assert_eq!(m.get("local"), Some(&"Foo".to_string()));
    }

    #[test]
    fn csharp_var_infers_type_from_constructor() {
        let m = extract_type_map("class C { void M() { var x = new Foo(); } }\n", "csharp");
        assert_eq!(
            m.get("x"),
            Some(&"Foo".to_string()),
            "var x = new Foo() must infer x: Foo from the constructor, not bind x: \"var\""
        );
    }

    #[test]
    // P1.2 step 1: PHP's member_call_expression/nullsafe_member_call_expression/
    // scoped_call_expression all name their callee via a "name" field
    // separate from the receiver ("object"/"scope") — confirmed via the real
    // grammar. Checks RawCall directly (cheaper/more precise than a full
    // pipeline test for this specific concern) that receiver+callee both
    // come through correctly for all three shapes, plus object_creation_expression.
    fn php_call_extraction_captures_receiver_for_all_call_kinds() {
        let src = "<?php\n\
            class C {\n\
                function m() {\n\
                    $this->helper->run();\n\
                    $obj?->run();\n\
                    Foo::bar();\n\
                    $y = new Foo();\n\
                }\n\
            }\n";
        let calls = extract_calls(src, "php", "c.php").unwrap();
        let find = |callee: &str, receiver: Option<&str>| {
            calls
                .iter()
                .find(|c| c.callee == callee && c.receiver.as_deref() == receiver)
        };

        let run1 = find("run", Some("helper"));
        assert!(
            run1.is_some(),
            "$this->helper->run() must extract receiver=helper (last segment), callee=run; got {calls:?}"
        );
        assert!(!run1.unwrap().receiver_is_type_path);

        let run2 = find("run", Some("obj"));
        assert!(
            run2.is_some(),
            "$obj?->run() (nullsafe) must extract receiver=obj, callee=run; got {calls:?}"
        );

        let bar = find("bar", Some("Foo"));
        assert!(
            bar.is_some(),
            "Foo::bar() must extract receiver=Foo, callee=bar; got {calls:?}"
        );
        assert!(
            bar.unwrap().receiver_is_type_path,
            "Foo::bar()'s receiver must be marked a type-path (scoped_call_expression), \
             like Rust's Type::method()"
        );

        let ctor = calls
            .iter()
            .find(|c| c.callee == "Foo" && c.receiver.is_none());
        assert!(
            ctor.is_some(),
            "new Foo() must extract as a call to bare name Foo (no receiver); got {calls:?}"
        );
    }

    /// Regression: struct/class FIELD types (as opposed to params/locals,
    /// which already worked) used to be entirely absent from `type_map` for
    /// every one of these four languages — the resolver could only
    /// disambiguate `x.foo()` by a locally-typed variable `x`, never by a
    /// field `self.x.foo()`/`this.x.foo()`. This is the same-language,
    /// cross-module false-callee gap (two unrelated types in different
    /// modules sharing a field name AND a method name resolve to the wrong
    /// one without the field's real declared type to disambiguate).
    #[test]
    fn test_type_map_covers_struct_class_field_declarations() {
        let rust = "struct Foo {\n    bar: BarType,\n}\n";
        assert_eq!(
            extract_type_map(rust, "rust").get("bar"),
            Some(&"BarType".to_string())
        );

        let go = "package p\ntype Foo struct {\n\tBar BarType\n}\n";
        assert_eq!(
            extract_type_map(go, "go").get("Bar"),
            Some(&"BarType".to_string())
        );
        // Go field declarations also share one type across multiple names.
        let go_multi = "package p\ntype Foo struct {\n\tX, Y int\n}\n";
        let m = extract_type_map(go_multi, "go");
        assert_eq!(m.get("X"), Some(&"int".to_string()));
        assert_eq!(m.get("Y"), Some(&"int".to_string()));

        let java = "class Foo {\n    BarType bar;\n}\n";
        assert_eq!(
            extract_type_map(java, "java").get("bar"),
            Some(&"BarType".to_string())
        );
        // Java field declarations also share one type across multiple names.
        let java_multi = "class Foo {\n    int x, y;\n}\n";
        let m = extract_type_map(java_multi, "java");
        assert_eq!(m.get("x"), Some(&"int".to_string()));
        assert_eq!(m.get("y"), Some(&"int".to_string()));

        let ts_class = "class Foo {\n    bar: BarType;\n}\n";
        assert_eq!(
            extract_type_map(ts_class, "typescript").get("bar"),
            Some(&"BarType".to_string())
        );
        let ts_interface = "interface Foo {\n    bar: BarType;\n}\n";
        assert_eq!(
            extract_type_map(ts_interface, "typescript").get("bar"),
            Some(&"BarType".to_string())
        );
    }

    /// Regression for the `confidence.as_str()`-style gap: a `let mut x;`
    /// with no initializer (so the `let_declaration` constructor-inference
    /// branch never fires) only reveals its type at a later `x = Foo::Variant;`
    /// assignment, often on more than one `if`/`else` branch. `assignment_expression`
    /// must be walked the same way `let_declaration` already is, and a bare
    /// enum-variant path (no call parens) must resolve via `rust_constructor_type`.
    #[test]
    fn test_type_map_infers_type_from_branch_reassignment_to_enum_variant() {
        let code = r#"
fn f(cond: bool) {
    let mut confidence;
    if cond {
        confidence = EdgeConfidence::Inferred;
    } else {
        confidence = EdgeConfidence::Textual;
    }
    confidence.as_str();
}
"#;
        let m = extract_type_map(code, "rust");
        assert_eq!(m.get("confidence"), Some(&"EdgeConfidence".to_string()));
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

    /// Regression: each `///` line parses as its own tree-sitter node, so
    /// taking only the single immediate `prev_named_sibling()` silently
    /// captured just the *last* line of a multi-line doc comment. Reproduces
    /// the exact shape discovered live: `understand()`'s output for a real
    /// symbol here (`apply_personalization_boost` in common.rs) returned
    /// only its doc comment's last line, dropping the actual explanation.
    #[test]
    fn test_rust_multiline_docstring_captures_all_lines() {
        let code = r#"
/// First line of explanation.
/// Second line with more detail.
///
/// A blank `///` line inside the block must still be included.
pub fn hello(a: i32, b: i32) -> i32 {
    a + b
}
"#;
        let symbols = extract_symbols(code, "rust", "test.rs").unwrap();
        assert_eq!(symbols.len(), 1);
        let doc = &symbols[0].docstring;
        assert!(doc.contains("First line of explanation."), "got: {doc:?}");
        assert!(
            doc.contains("Second line with more detail."),
            "got: {doc:?}"
        );
        assert!(
            doc.contains("A blank `///` line inside the block must still be included."),
            "got: {doc:?}"
        );
    }

    /// A doc comment block separated from an *unrelated* comment above it by
    /// a blank line must not merge the two — only the contiguous block
    /// immediately touching the function belongs to its docstring.
    #[test]
    fn test_rust_docstring_stops_at_blank_line_gap() {
        let code = r#"
// Unrelated comment far above, not part of the docstring.

/// Actual doc comment.
pub fn hello() {}
"#;
        let symbols = extract_symbols(code, "rust", "test.rs").unwrap();
        assert_eq!(symbols.len(), 1);
        let doc = &symbols[0].docstring;
        assert!(doc.contains("Actual doc comment."), "got: {doc:?}");
        assert!(
            !doc.contains("Unrelated comment far above"),
            "must not merge across a blank-line gap, got: {doc:?}"
        );
    }

    fn find<'a>(symbols: &'a [ParsedSymbol], name: &str) -> &'a ParsedSymbol {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found"))
    }

    #[test]
    fn test_python_symbol_kinds() {
        let code = r#"
class Greeter:
    def hello(self):
        pass

def standalone():
    pass
"#;
        let symbols = extract_symbols(code, "python", "test.py").unwrap();
        assert_eq!(find(&symbols, "Greeter").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "hello").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "standalone").kind, SymbolKind::Function);
    }

    #[test]
    fn test_rust_symbol_kinds() {
        let code = r#"
struct Point { x: i32, y: i32 }
trait Shape {}
impl Point {
    fn area(&self) -> i32 { 0 }
}
fn standalone() {}
"#;
        let symbols = extract_symbols(code, "rust", "test.rs").unwrap();
        assert_eq!(find(&symbols, "Point").kind, SymbolKind::Struct);
        assert_eq!(find(&symbols, "Shape").kind, SymbolKind::Trait);
        assert_eq!(find(&symbols, "area").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "standalone").kind, SymbolKind::Function);
    }

    #[test]
    fn test_rust_enum_union_type_alias_kinds() {
        let code = r#"
enum Status { Active, Inactive }
type Alias = i32;
union Word { i: i32, f: f32 }
"#;
        let symbols = extract_symbols(code, "rust", "test.rs").unwrap();
        assert_eq!(find(&symbols, "Status").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "Alias").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "Word").kind, SymbolKind::Type);
    }

    #[test]
    fn test_java_symbol_kinds() {
        let code = r#"
class Greeter {
    void hello() {}
}
interface Shape {}
"#;
        let symbols = extract_symbols(code, "java", "Test.java").unwrap();
        assert_eq!(find(&symbols, "Greeter").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "hello").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "Shape").kind, SymbolKind::Interface);
    }

    #[test]
    fn test_java_enum_record_constructor_kinds() {
        let code = r#"
class Box {
    Box() {}
}
enum Status { ACTIVE, INACTIVE }
record Point(int x, int y) {}
"#;
        let symbols = extract_symbols(code, "java", "Test.java").unwrap();
        let box_kinds: Vec<SymbolKind> = symbols
            .iter()
            .filter(|s| s.name == "Box")
            .map(|s| s.kind)
            .collect();
        assert!(box_kinds.contains(&SymbolKind::Class));
        assert!(box_kinds.contains(&SymbolKind::Constructor));
        assert_eq!(find(&symbols, "Status").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "Point").kind, SymbolKind::Struct);
    }

    #[test]
    fn test_go_symbol_kinds() {
        let code = r#"
package p

type Service struct{}

func (s *Service) Hello() {}

func Standalone() {}
"#;
        let symbols = extract_symbols(code, "go", "test.go").unwrap();
        assert_eq!(find(&symbols, "Service").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "Hello").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "Standalone").kind, SymbolKind::Function);
    }

    /// Regression: a grouped `type (...)` block used to collapse to a
    /// single symbol (whichever `type_spec` `resolve_name_node`'s `.find()`
    /// happened to hit first) — every other spec in the block silently
    /// vanished from the index. Each spec/alias is now walked individually.
    #[test]
    fn test_go_grouped_type_block_extracts_all_specs() {
        let code = "package p\n\ntype (\n\tA struct{ X int }\n\tB int\n\tC = int\n)\n";
        let symbols = extract_symbols(code, "go", "test.go").unwrap();
        assert_eq!(find(&symbols, "A").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "B").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "C").kind, SymbolKind::Type);
    }

    #[test]
    fn test_typescript_symbol_kinds() {
        let code = r#"
class Greeter {
    hello() {}
}
const arrow = () => {};
function standalone() {}
"#;
        let symbols = extract_symbols(code, "typescript", "test.ts").unwrap();
        assert_eq!(find(&symbols, "Greeter").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "hello").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "arrow").kind, SymbolKind::Variable);
        assert_eq!(find(&symbols, "standalone").kind, SymbolKind::Function);
    }

    #[test]
    fn test_typescript_enum_and_generator_function_kinds() {
        let code = r#"
enum Color { Red, Green, Blue }
function* gen() { yield 1; }
"#;
        let symbols = extract_symbols(code, "typescript", "test.ts").unwrap();
        assert_eq!(find(&symbols, "Color").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "gen").kind, SymbolKind::Function);
    }

    /// Regression: a TS/DTO-only file (all `export interface`/`export type`,
    /// no functions/classes) used to extract 0 symbols — `interface_declaration`
    /// and `type_alias_declaration` were missing from `function_node_types`,
    /// making such a file entirely invisible to `file_overview`/`search`.
    #[test]
    fn test_typescript_interface_and_type_alias_extracted() {
        let code = r#"
export interface FooRequest {
    id: string;
    count: number;
}

export type BarResponse = {
    ok: boolean;
};

export type Baz = string | number;
"#;
        let symbols = extract_symbols(code, "typescript", "mcp_types.ts").unwrap();
        assert_eq!(
            symbols.len(),
            3,
            "all three type-level declarations must be extracted"
        );
        assert_eq!(find(&symbols, "FooRequest").kind, SymbolKind::Interface);
        assert_eq!(find(&symbols, "BarResponse").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "Baz").kind, SymbolKind::Type);
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

    /// Regression: dunder methods are invoked by Python's data model via
    /// protocol dispatch (`X()` calls `__init__`, `str(x)` calls `__str__`),
    /// never by their literal name — must not be flagged as dead code just
    /// because a name-based call-graph never sees a "caller" for them.
    /// Single-underscore-prefixed names (the usual "private" convention)
    /// are a different thing entirely and must not match.
    #[test]
    fn test_python_entry_point_dunder_methods() {
        let code = "class C:\n    def __init__(self):\n        pass\n    def __str__(self):\n        return \"\"\n    def _private_helper(self):\n        pass\n    def helper(self):\n        pass\n";
        let symbols = extract_symbols(code, "python", "c.py").unwrap();
        assert!(find(&symbols, "__init__").is_entry_point);
        assert!(find(&symbols, "__str__").is_entry_point);
        assert!(!find(&symbols, "_private_helper").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
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

    /// Regression: an attribute-macro-registered handler (route/RPC/tool
    /// framework, etc.) is invoked externally via the macro's generated
    /// dispatch, never by a literal call site — a name-based call-graph can
    /// never give it a nonzero caller_count, so it must be treated as an
    /// entry point (like `main`) rather than flagged as dead code.
    #[test]
    fn test_rust_entry_point_attribute_macro() {
        let code = "struct S;\nimpl S {\n    #[tool(name = \"repo_overview\")]\n    fn repo_overview(&self) {}\n}\nfn helper() {}\n";
        let symbols = extract_symbols(code, "rust", "server.rs").unwrap();
        assert!(find(&symbols, "repo_overview").is_entry_point);
        assert!(!find(&symbols, "helper").is_entry_point);
    }

    /// Regression: a small set of purely-cosmetic/compiler-directive
    /// attributes must NOT trigger the broad "has an attribute macro"
    /// entry-point signal — they don't imply anything invokes the function
    /// other than an ordinary call site.
    #[test]
    fn test_rust_non_dispatch_attributes_dont_trigger_entry_point() {
        let code = "#[allow(dead_code)]\nfn helper() {}\n\n#[inline]\nfn also_helper() {}\n";
        let symbols = extract_symbols(code, "rust", "lib.rs").unwrap();
        assert!(!find(&symbols, "helper").is_entry_point);
        assert!(!find(&symbols, "also_helper").is_entry_point);
    }

    /// Regression: common std/core trait methods (`from`, `clone`, `fmt`,
    /// `default`, `next`, ...) are invoked via operator/trait/protocol
    /// syntax (`.into()`, `==`, `for` loops, ...), never by their literal
    /// name at a call site — a name-based call-graph can never see a real
    /// "caller" for them, so they must not be flagged as dead code just
    /// because they have zero recorded callers.
    #[test]
    fn test_rust_entry_point_trait_dispatch_names() {
        let code = "struct S;\nimpl S {\n    fn from(x: i32) -> Self {\n        S\n    }\n    fn clone(&self) -> Self {\n        S\n    }\n    fn process(&self) {}\n}\n";
        let symbols = extract_symbols(code, "rust", "types.rs").unwrap();
        assert!(find(&symbols, "from").is_entry_point);
        assert!(find(&symbols, "clone").is_entry_point);
        assert!(
            !find(&symbols, "process").is_entry_point,
            "an ordinary method name not in the trait-dispatch list is unaffected"
        );
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

    /// Container-decorator inheritance: an attribute macro on the `impl`
    /// BLOCK (not the method itself) is a strong entry-point signal for
    /// every pub method inside — e.g. `#[wasm_bindgen] impl Widget { pub
    /// fn tick(&self) {} }` exports `tick` to JS even though nothing in this
    /// repo calls it by name. Gated on `member_is_pub_for_container_
    /// inheritance`: a private helper in the same impl block must NOT
    /// inherit the signal just because its container happens to be
    /// decorated.
    #[test]
    fn test_rust_container_decorator_inherited_by_pub_method_only() {
        let code = "#[wasm_bindgen]\nimpl Widget {\n    pub fn tick(&self) {}\n    fn helper(&self) {}\n}\n";
        let symbols = extract_symbols(code, "rust", "widget.rs").unwrap();
        assert!(
            find(&symbols, "tick").is_entry_point,
            "pub method inside a decorated impl block inherits the container's entry-point signal"
        );
        assert!(
            !find(&symbols, "helper").is_entry_point,
            "a private method in the same impl must NOT inherit the signal"
        );
    }

    /// Same container-decorator inheritance for TS/JS: NestJS/Angular-style
    /// class decorators (`@Controller`, `@Injectable`, ...) previously left
    /// `decorator_node_kinds` returning an empty list for "typescript"/
    /// "javascript" entirely, so plain methods on a `@Controller` class were
    /// invisible to entry-point detection no matter what.
    #[test]
    fn test_typescript_container_decorator_inherited_by_method_only() {
        let code = "@Controller('/users')\nclass UserController {\n    findAll() {}\n    private helper() {}\n}\n";
        let symbols = extract_symbols(code, "typescript", "user.controller.ts").unwrap();
        assert!(
            find(&symbols, "findAll").is_entry_point,
            "a plain method inside a @Controller-decorated class inherits the container's entry-point signal"
        );
        assert!(
            !find(&symbols, "helper").is_entry_point,
            "an explicitly private TS method must NOT inherit the signal"
        );
    }

    /// A method's OWN decorator (no class-level decorator at all) is a
    /// separate code path from container inheritance — a member's decorator
    /// sits as a sibling inside `class_body` (verified via the real
    /// grammar), which `collect_decorators`'s existing sibling walk already
    /// finds correctly once `decorator_node_kinds` covers TS/JS at all.
    #[test]
    fn test_typescript_member_own_decorator_wired_into_entry_point() {
        let code = "class Api {\n    @Get()\n    findAll() {}\n    plain() {}\n}\n";
        let symbols = extract_symbols(code, "typescript", "api.ts").unwrap();
        assert!(
            find(&symbols, "findAll").is_entry_point,
            "a method decorated directly (no class decorator needed) is an entry point"
        );
        assert!(
            !find(&symbols, "plain").is_entry_point,
            "a plain undecorated method in an undecorated class is not"
        );
    }

    /// Same for Java: `collect_java_annotations` already correctly extracted
    /// class-level annotations, but nothing ever fed them into
    /// `detect_entry_point` — only `detect_is_test` used them. A
    /// `@RestController` class's plain public methods must be reachable via
    /// the framework's dispatch, not flagged dead just because nothing in
    /// this repo calls them directly.
    #[test]
    fn test_java_container_annotation_inherited_by_public_method_only() {
        let code = "@RestController\nclass UserController {\n    public void findAll() {}\n    private void helper() {}\n}\n";
        let symbols = extract_symbols(code, "java", "UserController.java").unwrap();
        assert!(
            find(&symbols, "findAll").is_entry_point,
            "public method inside a @RestController class inherits the container's entry-point signal"
        );
        assert!(
            !find(&symbols, "helper").is_entry_point,
            "a private Java method must NOT inherit the signal"
        );
    }

    /// Java member-level annotations (`@GetMapping`, ...) must also be
    /// checked directly by `detect_entry_point` — not only inherited from a
    /// container-level annotation — since route methods are commonly
    /// annotated individually rather than only at the class level.
    #[test]
    fn test_java_member_annotation_wired_into_entry_point() {
        let code = "class Api {\n    @GetMapping(\"/users\")\n    public void getUsers() {}\n}\n";
        let symbols = extract_symbols(code, "java", "Api.java").unwrap();
        assert!(
            find(&symbols, "getUsers").is_entry_point,
            "collect_java_annotations was already correct but never wired into detect_entry_point until this fix"
        );
    }

    // ---------------------------------------------------------------
    // DEBT-008: is_test detection (dead-code false-positive fix)
    // ---------------------------------------------------------------

    #[test]
    fn test_rust_is_test_attribute() {
        let code = r#"
#[test]
fn test_addition() {}

#[tokio::test]
async fn test_async_thing() {}

fn helper() {}
"#;
        let symbols = extract_symbols(code, "rust", "src/lib.rs").unwrap();
        assert!(find(&symbols, "test_addition").is_test);
        assert!(find(&symbols, "test_async_thing").is_test);
        assert!(!find(&symbols, "helper").is_test);
    }

    /// Regression: a comment sitting between two attributes — or trailing
    /// the last one right before the function — used to hard-stop
    /// `collect_decorators`'s backward walk, silently dropping every
    /// attribute above the comment. `#[test]` here is separated from
    /// `fn test_thing` by `#[should_panic]` AND a comment between the two
    /// attributes; both must still be seen.
    #[test]
    fn test_rust_collect_decorators_skips_comment_between_attributes() {
        let code = "#[test]\n// explanatory comment\n#[should_panic]\nfn test_thing() {\n    panic!(\"boom\");\n}\n";
        let symbols = extract_symbols(code, "rust", "src/lib.rs").unwrap();
        assert!(
            find(&symbols, "test_thing").is_test,
            "a comment between #[test] and #[should_panic] must not break the attribute chain"
        );
    }

    #[test]
    fn test_rust_is_test_does_not_flag_non_test_attributes() {
        let code = "#[allow(dead_code)]\nfn helper() {}\n";
        let symbols = extract_symbols(code, "rust", "src/lib.rs").unwrap();
        assert!(!find(&symbols, "helper").is_test);
    }

    #[test]
    fn test_python_is_test_requires_name_and_path_convention() {
        let code = "def test_login():\n    pass\n\ndef test_helper_for_prod():\n    pass\n";
        // Under tests/: pytest convention -> is_test.
        let symbols = extract_symbols(code, "python", "tests/test_auth.py").unwrap();
        assert!(find(&symbols, "test_login").is_test);

        // Same name prefix, but NOT under a test path/filename -> not flagged,
        // avoiding false positives on prod helpers that happen to start with
        // "test_" (e.g. a health-check `test_connection`).
        let symbols2 = extract_symbols(code, "python", "app/handlers.py").unwrap();
        assert!(!find(&symbols2, "test_helper_for_prod").is_test);
    }

    #[test]
    fn test_go_is_test_function() {
        let code =
            "package p\nimport \"testing\"\nfunc TestFoo(t *testing.T) {}\nfunc Helper() {}\n";
        let symbols = extract_symbols(code, "go", "foo_test.go").unwrap();
        assert!(find(&symbols, "TestFoo").is_test);
        assert!(!find(&symbols, "Helper").is_test);

        // Same source, non-_test.go path -> not flagged (go test discovery
        // itself requires the _test.go filename convention).
        let symbols2 = extract_symbols(code, "go", "foo.go").unwrap();
        assert!(!find(&symbols2, "TestFoo").is_test);
    }

    #[test]
    fn test_java_is_test_annotation() {
        let code = r#"
public class FooTest {
    @Test
    public void testBar() {}

    @org.junit.jupiter.api.Test
    public void testBaz() {}

    public void helper() {}
}
"#;
        let symbols = extract_symbols(code, "java", "FooTest.java").unwrap();
        assert!(find(&symbols, "testBar").is_test);
        assert!(find(&symbols, "testBaz").is_test);
        assert!(!find(&symbols, "helper").is_test);
    }

    // ---------------------------------------------------------------
    // Tier-0.5 grammar ABI guards — each optional tree-sitter grammar's
    // "language version" (ABI) must stay within this workspace's pinned
    // `tree-sitter` core's supported range (MIN_COMPATIBLE_LANGUAGE_VERSION
    // ..= LANGUAGE_VERSION), or `parser.set_language()` fails at runtime —
    // silently, since `parse_tree()` swallows the error via `.ok()?` and
    // `extract_file_data` treats a `None` tree as "no grammar for this
    // language", falling back to the much weaker shallow line-scan
    // extractor with zero calls/imports. This previously went unnoticed for
    // shell/php/csharp/c (all pinned to a version whose grammar reported
    // ABI 15 against a core that only supports up to 14) because no test
    // exercised `parse_tree`/`extract_symbols` for these languages — every
    // existing Tier-0.5 test called `extract_symbols_shallow` directly,
    // bypassing the real path entirely. These guards fail loudly the moment
    // a `cargo update` (or a well-meaning version-constraint relaxation)
    // reintroduces an ABI-incompatible grammar version.
    #[test]
    #[cfg(feature = "lang-ruby")]
    fn test_tier0_5_grammar_loads_ruby() {
        assert!(parse_tree("def foo\nend\n", "ruby").is_some());
    }

    #[test]
    #[cfg(feature = "lang-php")]
    fn test_tier0_5_grammar_loads_php() {
        assert!(parse_tree("<?php\nfunction foo() {}\n", "php").is_some());
    }

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn test_tier0_5_grammar_loads_csharp() {
        assert!(parse_tree("class Foo { void Bar() {} }\n", "csharp").is_some());
    }

    #[test]
    #[cfg(feature = "lang-shell")]
    fn test_tier0_5_grammar_loads_shell() {
        assert!(parse_tree("foo() {\n echo hi\n}\n", "shell").is_some());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn test_tier0_5_grammar_loads_c() {
        assert!(parse_tree("int foo() { return 0; }\n", "c").is_some());
    }

    #[test]
    #[cfg(feature = "lang-cpp")]
    fn test_tier0_5_grammar_loads_cpp() {
        assert!(parse_tree("int foo() { return 0; }\n", "cpp").is_some());
    }

    #[test]
    #[cfg(feature = "lang-r")]
    fn test_tier0_5_grammar_loads_r() {
        assert!(parse_tree("foo <- function(x) {\n  x\n}\n", "r").is_some());
    }

    /// Regression test for the shell-specific bug this guard suite was born
    /// from: with a working grammar, a `NAME=$(...)` / `NAME=(...)` variable
    /// assignment must never be reported as a function symbol, and a real
    /// call between two shell functions must produce a call edge (both were
    /// silently wrong under the Tier-0.5 shallow fallback, which never
    /// exercised the AST at all).
    #[test]
    #[cfg(feature = "lang-shell")]
    fn test_shell_real_grammar_symbols_and_calls_are_accurate() {
        let code = "os=$(uname -s)\narr=(a b c)\nfoo() {\n  echo hi\n}\nbar() {\n  if foo; then\n    echo ok\n  fi\n}\n";
        let symbols = extract_symbols(code, "shell", "a.sh").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            !names.contains(&"os"),
            "variable assignment must not be a symbol"
        );
        assert!(
            !names.contains(&"arr"),
            "array assignment must not be a symbol"
        );
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));

        let calls = extract_calls(code, "shell", "a.sh").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "bar" && c.callee == "foo"),
            "bar calling foo must produce a call edge"
        );
    }

    // Real-grammar SymbolKind coverage for the remaining Tier-0.5 languages
    // — without these, a mis-mapped node kind (falling through to the
    // default Function/Method arm) has no test that would ever catch it,
    // exactly the gap the ABI guards above were born from.
    #[test]
    #[cfg(feature = "lang-cpp")]
    fn test_cpp_real_grammar_symbol_kinds() {
        let code = "class Foo {};\nstruct Bar {};\nenum Color { Red, Green };\nint standalone() { return 0; }\n";
        let symbols = extract_symbols(code, "cpp", "test.cpp").unwrap();
        assert_eq!(find(&symbols, "Foo").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "Bar").kind, SymbolKind::Struct);
        assert_eq!(find(&symbols, "Color").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "standalone").kind, SymbolKind::Function);
    }

    #[test]
    #[cfg(feature = "lang-r")]
    fn test_r_real_grammar_symbols_and_calls_are_accurate() {
        let code = "os <- 5\nsquare <- function(x) {\n  x * x\n}\ncaller <- function(y) {\n  if (y > 0) {\n    square(y)\n  }\n}\n(function(x) x^3) -> cube\n";
        let symbols = extract_symbols(code, "r", "a.R").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            !names.contains(&"os"),
            "plain assignment must not be a symbol"
        );
        assert!(names.contains(&"square"));
        assert!(names.contains(&"caller"));
        assert!(
            names.contains(&"cube"),
            "parenthesized right-assign function definition must be a symbol"
        );

        let calls = extract_calls(code, "r", "a.R").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "caller" && c.callee == "square"),
            "caller calling square must produce a call edge"
        );
    }

    #[test]
    #[cfg(feature = "lang-r")]
    fn test_r_real_grammar_symbol_kinds() {
        let code = "standalone <- function() {\n  1\n}\n";
        let symbols = extract_symbols(code, "r", "a.R").unwrap();
        assert_eq!(find(&symbols, "standalone").kind, SymbolKind::Function);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn test_c_real_grammar_symbol_kinds() {
        let code = "struct Point { int x; int y; };\nunion Word { int i; float f; };\nenum Status { Active, Inactive };\nint standalone() { return 0; }\n";
        let symbols = extract_symbols(code, "c", "test.c").unwrap();
        assert_eq!(find(&symbols, "Point").kind, SymbolKind::Struct);
        assert_eq!(find(&symbols, "Word").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "Status").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "standalone").kind, SymbolKind::Function);
    }

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn test_csharp_real_grammar_symbol_kinds() {
        let code = "struct Point { }\nenum Status { Active, Inactive }\ndelegate void Handler();\nclass Foo { void Bar() {} }\n";
        let symbols = extract_symbols(code, "csharp", "test.cs").unwrap();
        assert_eq!(find(&symbols, "Point").kind, SymbolKind::Struct);
        assert_eq!(find(&symbols, "Status").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "Handler").kind, SymbolKind::Type);
        assert_eq!(find(&symbols, "Bar").kind, SymbolKind::Method);
    }

    #[test]
    #[cfg(feature = "lang-php")]
    fn test_php_real_grammar_trait_kind() {
        let code = "<?php\ntrait Greetable {\n    public function hello() {}\n}\n";
        let symbols = extract_symbols(code, "php", "test.php").unwrap();
        assert_eq!(find(&symbols, "Greetable").kind, SymbolKind::Trait);
    }

    #[test]
    #[cfg(feature = "lang-ruby")]
    fn test_ruby_real_grammar_class_module_kinds() {
        let code = "module Greetable\n  class Greeter\n    def hello\n    end\n  end\nend\n";
        let symbols = extract_symbols(code, "ruby", "test.rb").unwrap();
        assert_eq!(find(&symbols, "Greetable").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "Greeter").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "hello").kind, SymbolKind::Method);
    }

    /// Regression test for the 2026-07-10 fix: `call_node_types` used to say
    /// `"method_call"`, a node kind that does not exist in the vendored
    /// `tree-sitter-ruby` grammar (the real one is `"call"`) — so every
    /// single Ruby call, in every repo, silently produced zero call edges,
    /// with no error. This test asserts the actual behavior that regressed:
    /// a bare paren-less call (`puts "hi"`), and a receiver call
    /// (`self.log_it`) that must resolve to the sibling method in the same
    /// class, not just appear as an unresolved textual mention.
    #[test]
    #[cfg(feature = "lang-ruby")]
    fn test_ruby_real_grammar_symbols_and_calls_are_accurate() {
        let code = "class Greeter\n  def hello\n    puts \"hi\"\n    self.log_it\n  end\n\n  def log_it\n    puts(\"logged\")\n  end\nend\n";
        let symbols = extract_symbols(code, "ruby", "test.rb").unwrap();
        let names = shallow_names(&symbols);
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"log_it"));

        let calls = extract_calls(code, "ruby", "test.rb").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "hello" && c.callee == "puts"),
            "bare paren-less call must produce a call edge, got: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "hello"
                && c.callee == "log_it"
                && c.receiver.as_deref() == Some("self")),
            "self.log_it must produce a call edge with receiver captured, got: {calls:?}"
        );
    }

    // ---------------------------------------------------------------
    // Tier-0.5 shallow extraction tests
    // ---------------------------------------------------------------

    fn shallow_names(syms: &[ParsedSymbol]) -> Vec<&str> {
        syms.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn test_shallow_cpp() {
        let code = "class Foo {\nstruct Bar {};\nvoid init() {}\nint compute(int x) { return x; }\nif (true) {}\n";
        let syms = extract_symbols_shallow(code, "cpp", "a.cpp");
        let names = shallow_names(&syms);
        assert!(names.contains(&"Foo"), "should detect class");
        assert!(names.contains(&"Bar"), "should detect struct");
        assert!(names.contains(&"init"), "should detect function");
        assert!(
            names.contains(&"compute"),
            "should detect function with args"
        );
        assert!(!names.contains(&"if"), "should skip control flow");
    }

    #[test]
    fn test_shallow_csharp() {
        let code = "public class MyService {\npublic async Task<string> GetData(int id) {}\nenum Status { Active }\npublic interface IRepo {}\n";
        let syms = extract_symbols_shallow(code, "csharp", "a.cs");
        let names = shallow_names(&syms);
        assert!(names.contains(&"MyService"), "should detect class");
        assert!(names.contains(&"GetData"), "should detect method");
        assert!(names.contains(&"Status"), "should detect enum");
        assert!(names.contains(&"IRepo"), "should detect interface");
    }

    #[test]
    fn test_shallow_ruby() {
        let code = "class Animal\ndef speak\nend\nmodule Concerns\n";
        let syms = extract_symbols_shallow(code, "ruby", "a.rb");
        let names = shallow_names(&syms);
        assert!(names.contains(&"Animal"), "should detect class");
        assert!(names.contains(&"speak"), "should detect def");
        assert!(names.contains(&"Concerns"), "should detect module");
    }

    #[test]
    fn test_shallow_r() {
        let code = "os <- 5\nsquare <- function(x) {\n  x * x\n}\nprint.myclass = function(x, ...) {\n  cat(x)\n}\n";
        let syms = extract_symbols_shallow(code, "r", "a.R");
        let names = shallow_names(&syms);
        assert!(
            !names.contains(&"os"),
            "plain assignment should not be detected as a function"
        );
        assert!(names.contains(&"square"), "should detect <- function(...)");
        assert!(
            names.contains(&"print.myclass"),
            "should detect dotted S3-method name via = function(...)"
        );
    }

    #[test]
    fn test_markdown_headings_are_extracted_with_line_numbers_and_kind() {
        let code = "# Title\n\nsome text\n\n## Section One\n\ntext\n\n### Sub Section\n\n#Not A Heading (no space)\n";
        let syms = extract_markdown_symbols(code, "README.md");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Title", "Section One", "Sub Section"]);
        assert!(syms.iter().all(|s| s.kind == SymbolKind::Heading));

        let title = syms.iter().find(|s| s.name == "Title").unwrap();
        assert_eq!(title.line_start, 1);
        assert_eq!(title.line_end, 1);

        let section = syms.iter().find(|s| s.name == "Section One").unwrap();
        assert_eq!(section.line_start, 5);
        assert_eq!(section.qualified_name, "README.md::Section One");
    }

    #[test]
    fn test_markdown_headings_ignore_fenced_code_block_content() {
        let code = "# Real Heading\n\n```python\n# not a heading, this is a python comment\ndef foo():\n    pass\n```\n\n## Also Real\n";
        let syms = extract_markdown_symbols(code, "doc.md");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Real Heading", "Also Real"],
            "a '#'-prefixed line inside a fenced code block must not be treated as a heading"
        );
    }

    #[test]
    fn test_markdown_headings_ignore_shebang_and_hashtag_like_lines() {
        let code = "#!/usr/bin/env markdown\n#hashtag with no space\n####### too many hashes\n# \n## Real One\n";
        let syms = extract_markdown_symbols(code, "a.md");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Real One"]);
    }

    #[test]
    fn test_markdown_language_routes_through_extract_file_data_shape() {
        // language_for_extension must map .md to "markdown" so the indexer
        // pipeline actually reaches extract_markdown_symbols at all.
        assert_eq!(
            crate::indexer::lang_constants::language_for_extension("md"),
            Some("markdown")
        );
        assert_eq!(
            crate::indexer::lang_constants::language_for_extension("markdown"),
            Some("markdown")
        );
        // Markdown has no tree-sitter grammar, ever — extract_file_data's
        // dedicated branch is the only path that can produce symbols for it.
        assert!(parse_tree("# x", "markdown").is_none());
    }
    #[test]
    fn test_shallow_kotlin() {
        let code = "data class User(val name: String)\nfun greet(user: User) {}\ninterface Repository {}\n";
        let syms = extract_symbols_shallow(code, "kotlin", "a.kt");
        let names = shallow_names(&syms);
        assert!(names.contains(&"User"), "should detect data class");
        assert!(names.contains(&"greet"), "should detect fun");
        assert!(names.contains(&"Repository"), "should detect interface");
    }

    #[test]
    fn test_shallow_swift() {
        let code = "public class ViewModel {}\nfunc fetchData() {}\nstruct Config {}\nprotocol Updatable {}\n";
        let syms = extract_symbols_shallow(code, "swift", "a.swift");
        let names = shallow_names(&syms);
        assert!(names.contains(&"ViewModel"), "should detect class");
        assert!(names.contains(&"fetchData"), "should detect func");
        assert!(names.contains(&"Config"), "should detect struct");
        assert!(names.contains(&"Updatable"), "should detect protocol");
    }

    // Locks the ABI-14 grammar pin (Cargo.toml) the same way
    // `test_tier0_5_grammar_loads_ruby`/etc. already do for the other
    // Tier-0.5 languages — a future `cargo update` picking a newer,
    // ABI-incompatible patch would silently regress this to shallow
    // line-scan (`parse_tree`'s `.ok()?`) without this test catching it.
    #[test]
    #[cfg(feature = "lang-kotlin")]
    fn test_tier0_5_grammar_loads_kotlin() {
        assert!(
            parse_tree("fun main() {}", "kotlin").is_some(),
            "tree-sitter-kotlin-ng grammar should load and parse"
        );
    }

    #[test]
    #[cfg(feature = "lang-swift")]
    fn test_tier0_5_grammar_loads_swift() {
        assert!(
            parse_tree("func main() {}", "swift").is_some(),
            "tree-sitter-swift grammar should load and parse"
        );
    }

    // Real-grammar call-accuracy tests, same convention as
    // `test_ruby_real_grammar_symbols_and_calls_are_accurate` (born
    // `4439d3a` 2026-07-03) — asserts both a bare call and a
    // receiver-qualified call produce real call edges. This exact test
    // shape is what would have caught Kotlin/Swift's phantom
    // `interface_declaration`/`struct_declaration`/`enum_declaration` node
    // kinds and the wrong `"callee"` field guess, had they ever shipped
    // untested.
    #[test]
    #[cfg(feature = "lang-kotlin")]
    fn test_kotlin_real_grammar_symbols_and_calls_are_accurate() {
        let code = "class Greeter {\n    fun hello() {\n        println(\"hi\")\n        this.logIt()\n    }\n    fun logIt() {}\n}\nobject Singleton {\n    fun instance() {}\n}\n";
        let symbols = extract_symbols(code, "kotlin", "a.kt").unwrap();
        let names = shallow_names(&symbols);
        assert!(names.contains(&"Greeter"), "should detect class");
        assert!(names.contains(&"hello"), "should detect method");
        // Found via a real end-to-end index of a fixture file (unit tests
        // alone never exercise `node_kind_to_symbol_kind` on kotlin/swift
        // input): `object_declaration` had no arm there, so a top-level
        // `object Singleton {}` silently fell through to the generic
        // Function/Method default and was mislabeled `SymbolKind::Function`
        // instead of `Class`.
        let singleton = symbols
            .iter()
            .find(|s| s.name == "Singleton")
            .expect("should detect object declaration");
        assert_eq!(
            singleton.kind,
            SymbolKind::Class,
            "Kotlin `object` should map to SymbolKind::Class, not the generic Function/Method fallback"
        );
        let calls = extract_calls(code, "kotlin", "a.kt").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "hello" && c.callee == "println"),
            "bare call should produce a call edge: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "hello"
                && c.callee == "logIt"
                && c.receiver.as_deref() == Some("this")),
            "receiver-qualified call should produce a call edge with receiver: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-swift")]
    fn test_swift_real_grammar_symbols_and_calls_are_accurate() {
        let code = "class Greeter {\n    func hello() {\n        print(\"hi\")\n        self.logIt()\n    }\n    func logIt() {}\n}\nprotocol Named {\n    func getName() -> String\n}\n";
        let symbols = extract_symbols(code, "swift", "a.swift").unwrap();
        let names = shallow_names(&symbols);
        assert!(names.contains(&"Greeter"), "should detect class");
        assert!(names.contains(&"hello"), "should detect method");
        // Same category of gap as Kotlin's `object_declaration` above,
        // found the same way (real end-to-end index of a fixture file):
        // `protocol_declaration` had no arm in `node_kind_to_symbol_kind`,
        // so `protocol Named {}` was mislabeled `SymbolKind::Function`
        // instead of `Interface`.
        let named = symbols
            .iter()
            .find(|s| s.name == "Named")
            .expect("should detect protocol declaration");
        assert_eq!(
            named.kind,
            SymbolKind::Interface,
            "Swift `protocol` should map to SymbolKind::Interface, not the generic Function/Method fallback"
        );
        let calls = extract_calls(code, "swift", "a.swift").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "hello" && c.callee == "print"),
            "bare call should produce a call edge: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "hello"
                && c.callee == "logIt"
                && c.receiver.as_deref() == Some("self")),
            "receiver-qualified call should produce a call edge with receiver: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-scala")]
    fn test_tier0_5_grammar_loads_scala() {
        assert!(
            parse_tree("def main(): Unit = {}", "scala").is_some(),
            "tree-sitter-scala grammar should load and parse — locks the pinned ABI (=0.24.1, ABI 14); \
             a caret-range bump to 0.25.0+ (ABI 15) would silently regress this to shallow line-scan"
        );
    }

    #[test]
    #[cfg(feature = "lang-scala")]
    fn test_scala_real_grammar_symbols_and_calls_are_accurate() {
        let code = "class Greeter {\n  def hello(): Unit = {\n    println(\"hi\")\n    this.logIt()\n  }\n  def logIt(): Unit = {}\n}\nobject Singleton {\n  def instance(): Unit = {}\n}\ntrait Named {\n  def name: String\n}\n";
        let symbols = extract_symbols(code, "scala", "a.scala").unwrap();
        let names = shallow_names(&symbols);
        assert!(names.contains(&"Greeter"), "should detect class");
        assert!(names.contains(&"hello"), "should detect method");
        // Same bug class as Kotlin's object_declaration / Swift's
        // protocol_declaration above (see those tests' comments) — asserting
        // `kind`, not just presence-by-name, is what would have caught it.
        let singleton = symbols
            .iter()
            .find(|s| s.name == "Singleton")
            .expect("should detect object definition");
        assert_eq!(
            singleton.kind,
            SymbolKind::Class,
            "Scala `object` should map to SymbolKind::Class, not the generic Function/Method fallback"
        );
        let named = symbols
            .iter()
            .find(|s| s.name == "Named")
            .expect("should detect trait definition");
        assert_eq!(
            named.kind,
            SymbolKind::Trait,
            "Scala `trait` should map to SymbolKind::Trait, not the generic Function/Method fallback"
        );
        let calls = extract_calls(code, "scala", "a.scala").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "hello" && c.callee == "println"),
            "bare call should produce a call edge: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "hello"
                && c.callee == "logIt"
                && c.receiver.as_deref() == Some("this")),
            "receiver-qualified call should produce a call edge with receiver: {calls:?}"
        );
    }

    #[test]
    fn test_shallow_scala() {
        let code = "case class User(name: String)\ndef greet(user: User): Unit = {}\nsealed trait Repository\nobject Singleton\n";
        let syms = extract_symbols_shallow(code, "scala", "a.scala");
        let names = shallow_names(&syms);
        assert!(names.contains(&"User"), "should detect case class");
        assert!(names.contains(&"greet"), "should detect def");
        assert!(names.contains(&"Repository"), "should detect sealed trait");
        assert!(names.contains(&"Singleton"), "should detect object");
    }

    #[test]
    fn test_shallow_dart() {
        let code = "class Greeter {\nabstract class Named {\nmixin Loggable {\n";
        let syms = extract_symbols_shallow(code, "dart", "a.dart");
        let names = shallow_names(&syms);
        assert!(names.contains(&"Greeter"), "should detect class");
        assert!(names.contains(&"Named"), "should detect abstract class");
        assert!(names.contains(&"Loggable"), "should detect mixin");
    }

    #[test]
    fn test_shallow_lua() {
        let code = "local function main()\nfunction Greeter.new(name)\nfunction Greeter:greet()\n";
        let syms = extract_symbols_shallow(code, "lua", "a.lua");
        let names = shallow_names(&syms);
        assert!(names.contains(&"main"), "should detect local function");
        assert!(
            names.contains(&"Greeter.new"),
            "should detect dot-qualified function"
        );
        assert!(
            names.contains(&"Greeter:greet"),
            "should detect method-colon function"
        );
    }

    #[test]
    fn test_shallow_elixir() {
        let code = "def greet(name) do\ndefp format_greeting(name) do\n";
        let syms = extract_symbols_shallow(code, "elixir", "a.ex");
        let names = shallow_names(&syms);
        assert!(names.contains(&"greet"), "should detect def");
        assert!(names.contains(&"format_greeting"), "should detect defp");
    }

    #[test]
    fn test_shallow_haskell() {
        let code = "data Greeter = Greeter { name :: String }\nnewtype Age = Age Int\nclass Named a where\n";
        let syms = extract_symbols_shallow(code, "haskell", "a.hs");
        let names = shallow_names(&syms);
        assert!(names.contains(&"Greeter"), "should detect data");
        assert!(names.contains(&"Age"), "should detect newtype");
        assert!(names.contains(&"Named"), "should detect class");
    }

    #[test]
    fn test_shallow_ocaml() {
        let code =
            "type greeter = { name : string }\nlet greet g = g.name\nlet rec loop n = loop n\n";
        let syms = extract_symbols_shallow(code, "ocaml", "a.ml");
        let names = shallow_names(&syms);
        assert!(names.contains(&"greeter"), "should detect type");
        assert!(names.contains(&"greet"), "should detect let");
        assert!(names.contains(&"loop"), "should detect let rec");
    }

    #[test]
    fn test_shallow_zig() {
        let code = "pub fn main() void {\nconst std = @import(\"std\");\nvar counter: i32 = 0;\n";
        let syms = extract_symbols_shallow(code, "zig", "a.zig");
        let names = shallow_names(&syms);
        assert!(names.contains(&"main"), "should detect pub fn");
        assert!(names.contains(&"std"), "should detect const");
        assert!(names.contains(&"counter"), "should detect var");
    }

    #[test]
    fn test_shallow_powershell() {
        let code = "function Get-Greeting {\nclass Greeter {\n";
        let syms = extract_symbols_shallow(code, "powershell", "a.ps1");
        let names = shallow_names(&syms);
        assert!(
            names.contains(&"Get-Greeting"),
            "should detect hyphenated Verb-Noun function name"
        );
        assert!(names.contains(&"Greeter"), "should detect class");
    }
    #[test]
    fn test_shallow_shell() {
        let code = "function setup() {\nbuild() {\n  echo hi\n}\n";
        let syms = extract_symbols_shallow(code, "shell", "a.sh");
        let names = shallow_names(&syms);
        assert!(
            names.contains(&"setup"),
            "should detect function keyword style"
        );
        assert!(names.contains(&"build"), "should detect paren style");
    }

    #[test]
    fn test_shallow_shell_does_not_misdetect_assignments_as_functions() {
        let code = "os=$(uname -s)\nserve_args=(serve --project-root . \"$@\")\n";
        let syms = extract_symbols_shallow(code, "shell", "a.sh");
        let names = shallow_names(&syms);
        assert!(
            !names.contains(&"os"),
            "command-substitution assignment must not be detected as a function"
        );
        assert!(
            !names.contains(&"serve_args"),
            "array assignment must not be detected as a function"
        );
    }

    #[test]
    fn test_shallow_php() {
        let code = "<?php\nclass Controller {}\nfunction handle($req) {}\ninterface Handler {}\ntrait Logging {}\n";
        let syms = extract_symbols_shallow(code, "php", "a.php");
        let names = shallow_names(&syms);
        assert!(names.contains(&"Controller"), "should detect class");
        assert!(names.contains(&"handle"), "should detect function");
        assert!(names.contains(&"Handler"), "should detect interface");
        assert!(names.contains(&"Logging"), "should detect trait");
    }

    #[test]
    fn test_shallow_qualified_name_dedup() {
        // Two Ruby methods with the same name: second gets line suffix.
        let code = "def run\ndef run\n";
        let syms = extract_symbols_shallow(code, "ruby", "a.rb");
        assert_eq!(syms.len(), 2);
        assert!(syms[0].qualified_name.starts_with("a.rb::run"));
        assert_ne!(syms[0].qualified_name, syms[1].qualified_name);
    }

    // -----------------------------------------------------------------
    // Cyclomatic complexity
    // -----------------------------------------------------------------

    #[test]
    fn test_complexity_baseline_one_for_straight_line_function() {
        let code = "def hello(a, b):\n    return a + b\n";
        let symbols = extract_symbols(code, "python", "test.py").unwrap();
        assert_eq!(find(&symbols, "hello").complexity, 1);
    }

    #[test]
    fn test_complexity_python_counts_branches() {
        let code = "\
def classify(x):
    if x < 0:
        return 'neg'
    elif x == 0:
        return 'zero'
    else:
        return 'pos'
    for i in range(x):
        if i % 2 == 0 and i > 0:
            pass
    while x > 0:
        x -= 1
    return x
";
        let symbols = extract_symbols(code, "python", "test.py").unwrap();
        let c = find(&symbols, "classify").complexity;
        // baseline 1 + if + elif + for + if + and(boolean_operator) + while = 7
        assert_eq!(c, 7, "expected 7, got {c}");
    }

    #[test]
    fn test_complexity_rust_counts_branches() {
        let code = r#"
fn classify(x: i32) -> i32 {
    if x < 0 {
        return -1;
    } else if x == 0 {
        return 0;
    }
    match x {
        1 => 1,
        2 => 2,
        _ => 3,
    }
}
"#;
        let symbols = extract_symbols(code, "rust", "test.rs").unwrap();
        let c = find(&symbols, "classify").complexity;
        // baseline 1 + if + if(else-if) + 3 match_arm = 6
        assert_eq!(c, 6, "expected 6, got {c}");
    }

    #[test]
    fn test_complexity_go_counts_branches() {
        let code = r#"
package main

func classify(x int) int {
	if x < 0 {
		return -1
	}
	for i := 0; i < x; i++ {
		x--
	}
	switch x {
	case 1:
		return 1
	case 2:
		return 2
	}
	return x
}
"#;
        let symbols = extract_symbols(code, "go", "test.go").unwrap();
        let c = find(&symbols, "classify").complexity;
        // baseline 1 + if + for + 2 expression_case = 5
        assert_eq!(c, 5, "expected 5, got {c}");
    }

    #[test]
    fn test_complexity_javascript_counts_branches() {
        let code = "\
function classify(x) {
    if (x < 0) {
        return -1;
    } else if (x === 0) {
        return 0;
    }
    for (let i = 0; i < x; i++) {
        x--;
    }
    return x > 0 ? 1 : -1;
}
";
        let symbols = extract_symbols(code, "javascript", "test.js").unwrap();
        let c = find(&symbols, "classify").complexity;
        // baseline 1 + if + if(else-if) + for + ternary = 5
        assert_eq!(c, 5, "expected 5, got {c}");
    }

    #[test]
    fn test_complexity_java_counts_branches() {
        let code = r#"
class Foo {
    int classify(int x) {
        if (x < 0) {
            return -1;
        } else if (x == 0) {
            return 0;
        }
        for (int i = 0; i < x; i++) {
            x--;
        }
        return x;
    }
}
"#;
        let symbols = extract_symbols(code, "java", "test.java").unwrap();
        let c = find(&symbols, "classify").complexity;
        // baseline 1 + if + if(else-if) + for = 4
        assert_eq!(c, 4, "expected 4, got {c}");
    }

    #[test]
    fn test_complexity_shallow_extraction_defaults_to_one() {
        // Tier-0.5 (no real parse tree) always gets baseline complexity —
        // there's no AST to count branches in.
        let code = "def run\n  if true\n    puts 'x'\n  end\nend\n";
        let syms = extract_symbols_shallow(code, "ruby", "a.rb");
        assert!(syms.iter().all(|s| s.complexity == 1));
    }

    #[test]
    fn test_split_receiver_callee_dot_receiver() {
        assert_eq!(
            split_receiver_callee("self.method"),
            Some((Some("self".to_string()), "method".to_string(), false))
        );
        assert_eq!(
            split_receiver_callee("a.b.method"),
            Some((Some("b".to_string()), "method".to_string(), false))
        );
    }

    #[test]
    fn test_split_receiver_callee_type_path_is_scoped() {
        // Type::method() — uppercase segment before the last `::` is kept as
        // a receiver AND flagged as a type path (not a variable to look up).
        assert_eq!(
            split_receiver_callee("HashMap::new"),
            Some((Some("HashMap".to_string()), "new".to_string(), true))
        );
        // Module-qualified free function — lowercase segment is not type-like,
        // so it is dropped exactly like before this fix (no behavior change).
        assert_eq!(
            split_receiver_callee("mod::func"),
            Some((None, "func".to_string(), false))
        );
        // Multi-level module path to an associated function: only the segment
        // immediately before the last `::` is kept (the type, not the module).
        assert_eq!(
            split_receiver_callee("std::collections::HashMap::new"),
            Some((Some("HashMap".to_string()), "new".to_string(), true))
        );
    }

    #[test]
    fn test_split_receiver_callee_bare_function() {
        assert_eq!(
            split_receiver_callee("func"),
            Some((None, "func".to_string(), false))
        );
    }

    #[test]
    fn test_is_type_like() {
        assert!(is_type_like("HashMap"));
        assert!(is_type_like("StructA"));
        assert!(!is_type_like("mod"));
        assert!(!is_type_like("helper"));
        assert!(!is_type_like(""));
    }

    #[test]
    fn test_looks_option_or_result_chained_detects_try_operator() {
        let code = "fn caller() {\n    let _ = foo.bar()?;\n}\n";
        let calls = extract_calls(code, "rust", "a.rs").unwrap();
        let bar = calls.iter().find(|c| c.callee == "bar").unwrap();
        assert!(
            bar.looks_option_or_result_chained,
            "foo.bar()? must be detected as Option/Result-chained"
        );
    }

    #[test]
    fn test_looks_option_or_result_chained_detects_unwrap() {
        let code = "fn caller() {\n    let _ = foo.bar().unwrap();\n}\n";
        let calls = extract_calls(code, "rust", "a.rs").unwrap();
        let bar = calls.iter().find(|c| c.callee == "bar").unwrap();
        assert!(
            bar.looks_option_or_result_chained,
            "foo.bar().unwrap() must be detected as Option/Result-chained"
        );
    }

    #[test]
    fn test_looks_option_or_result_chained_false_for_plain_call() {
        let code = "fn caller() {\n    let _ = foo.bar();\n}\n";
        let calls = extract_calls(code, "rust", "a.rs").unwrap();
        let bar = calls.iter().find(|c| c.callee == "bar").unwrap();
        assert!(
            !bar.looks_option_or_result_chained,
            "a plain foo.bar() call is not provably Option/Result-returning"
        );
    }

    #[test]
    fn test_looks_option_or_result_chained_peels_and_then_closure() {
        // `.and_then` requires its closure to return Option/Result matching the
        // outer chain — provably sound to peel, not just heuristic.
        let code = "fn caller() {\n    let _ = foo.and_then(|p| p.as_str())?;\n}\n";
        let calls = extract_calls(code, "rust", "a.rs").unwrap();
        let as_str = calls.iter().find(|c| c.callee == "as_str").unwrap();
        assert!(
            as_str.looks_option_or_result_chained,
            ".and_then(|p| p.as_str())? must peel through and_then to detect chaining"
        );
    }

    #[test]
    fn test_looks_option_or_result_chained_does_not_peel_map_closure() {
        // Unlike `.and_then`, `.map`'s closure returns a plain value, not
        // Option/Result — peeling here would be unsound.
        let code = "fn caller() {\n    let _ = foo.map(|p| p.as_str())?;\n}\n";
        let calls = extract_calls(code, "rust", "a.rs").unwrap();
        let as_str = calls.iter().find(|c| c.callee == "as_str").unwrap();
        assert!(
            !as_str.looks_option_or_result_chained,
            ".map(..)'s closure isn't required to return Option/Result — must not peel"
        );
    }

    #[test]
    fn test_looks_option_or_result_chained_false_when_unwrap_is_on_a_different_receiver() {
        // `.unwrap()` here chains off `baz()`, not off `bar()` — must not be
        // misattributed to the unrelated inner call.
        let code = "fn caller() {\n    let _ = foo.bar();\n    let _ = baz().unwrap();\n}\n";
        let calls = extract_calls(code, "rust", "a.rs").unwrap();
        let bar = calls.iter().find(|c| c.callee == "bar").unwrap();
        assert!(!bar.looks_option_or_result_chained);
        let baz = calls.iter().find(|c| c.callee == "baz").unwrap();
        assert!(baz.looks_option_or_result_chained);
    }

    #[test]
    #[cfg(feature = "lang-dart")]
    fn test_tier0_5_grammar_loads_dart() {
        assert!(
            parse_tree("void main() {}", "dart").is_some(),
            "tree-sitter-dart grammar should load and parse — locks the pinned ABI \
             (=0.0.4, ABI 14, the newest ABI-14 release since this grammar has no git \
             tags — verified by downloading each published .crate tarball and grepping \
             src/parser.c directly); a caret-range bump to 0.1.0+ (ABI 15) would \
             silently regress this to shallow line-scan"
        );
    }

    #[test]
    #[cfg(feature = "lang-dart")]
    fn test_dart_real_grammar_symbols_are_accurate() {
        // tree-sitter-dart 0.0.4 (an older, hand-written grammar — see
        // lang_constants.rs's Dart entry) has no dedicated call-expression
        // node kind at all (calls are a generic member_access/selector/
        // argument_part postfix chain shared with plain field access) —
        // call-graph extraction is a documented scope cut, not an oversight.
        // This test locks that gap in place (asserts calls stay empty) so a
        // future accidental `call_node_types` entry that doesn't actually
        // work correctly gets caught instead of silently shipping partial
        // call edges.
        let code = "class Greeter {\n  String name;\n\n  Greeter(this.name);\n\n  String greet() {\n    print(\"hello\");\n    return this.formatGreeting(name);\n  }\n\n  String formatGreeting(String n) {\n    return \"Hello, \" + n;\n  }\n}\n\nabstract class Named {\n  String getName();\n}\n\nvoid main() {\n  var g = Greeter(\"world\");\n  g.greet();\n}\n";
        let symbols = extract_symbols(code, "dart", "a.dart").unwrap();
        let names = shallow_names(&symbols);
        assert!(names.contains(&"Greeter"), "should detect class");
        assert!(names.contains(&"greet"), "should detect method with a body");
        assert!(
            names.contains(&"formatGreeting"),
            "should detect second method with a body"
        );
        assert!(
            names.contains(&"getName"),
            "should detect abstract method (no body) inside an abstract class"
        );
        assert!(
            names.contains(&"main"),
            "should detect top-level function (parses as a named `lambda_expression`, \
             not a dedicated top-level-function node kind, in this grammar)"
        );

        let greeter = symbols
            .iter()
            .find(|s| s.name == "Greeter" && s.kind == SymbolKind::Class)
            .expect("Greeter should map to SymbolKind::Class");
        assert!(greeter.class_context.is_none());

        let greet = symbols
            .iter()
            .find(|s| s.name == "greet")
            .expect("greet method should be found");
        assert_eq!(
            greet.class_context.as_deref(),
            Some("Greeter"),
            "greet's class_context should be Greeter"
        );

        let main_fn = symbols
            .iter()
            .find(|s| s.name == "main")
            .expect("main should be found");
        assert!(
            main_fn.class_context.is_none(),
            "top-level main should have no class_context"
        );

        let calls = extract_calls(code, "dart", "a.dart").unwrap();
        assert!(
            calls.is_empty(),
            "Dart call-graph extraction is a documented scope cut (no clean \
             call-expression node in this grammar) — see this test's doc comment; \
             calls: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-lua")]
    fn test_tier0_5_grammar_loads_lua() {
        assert!(
            parse_tree("function main() end", "lua").is_some(),
            "tree-sitter-lua grammar should load and parse — locks the pinned ABI \
             (=0.2.0, ABI 14); a caret-range bump to 0.4.1+ (ABI 15) would silently \
             regress this to shallow line-scan"
        );
    }

    #[test]
    #[cfg(feature = "lang-lua")]
    fn test_lua_real_grammar_symbols_and_calls_are_accurate() {
        // Lua has no class syntax (OOP is convention over tables +
        // metatables, not a grammar-level construct) — `class_node_types`
        // is empty, so every function is SymbolKind::Function with no
        // class_context, even a `function Greeter:greet()` method-colon
        // definition. Its `name` field's raw text keeps the qualifier
        // ("Greeter:greet"/"Greeter.new") rather than splitting out a bare
        // name — a documented rough edge (still findable via `name_tokens`
        // tokenization splitting on `.`/`:`), not a bug.
        let code = "local Greeter = {}\n\nfunction Greeter.new(name)\n  return setmetatable({}, Greeter)\nend\n\nfunction Greeter:greet()\n  print(\"hello\")\n  self:formatGreeting()\nend\n\nlocal function main()\n  local g = Greeter.new(\"world\")\n  g:greet()\nend\n";
        let symbols = extract_symbols(code, "lua", "a.lua").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            names.contains(&"Greeter.new"),
            "should detect dot-qualified function: {names:?}"
        );
        assert!(
            names.contains(&"Greeter:greet"),
            "should detect method-colon function: {names:?}"
        );
        assert!(
            names.contains(&"main"),
            "should detect `local function` (same node kind as global \
             `function`, just referenced via a different field on its parent): {names:?}"
        );

        let calls = extract_calls(code, "lua", "a.lua").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "Greeter:greet" && c.callee == "print"),
            "bare call should produce a call edge: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "Greeter:greet"
                && c.callee == "formatGreeting"
                && c.receiver.as_deref() == Some("self")),
            "method-colon call should produce a call edge with receiver: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "main"
                && c.callee == "greet"
                && c.receiver.as_deref() == Some("g")),
            "method-colon call on a local var should produce a call edge with receiver: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-elixir")]
    fn test_tier0_5_grammar_loads_elixir() {
        assert!(
            parse_tree("def main do\nend\n", "elixir").is_some(),
            "tree-sitter-elixir grammar should load and parse — locks the pinned ABI \
             (=0.3.5, ABI 14)"
        );
    }

    #[test]
    #[cfg(feature = "lang-elixir")]
    fn test_elixir_real_grammar_symbols_and_calls_are_accurate() {
        // Elixir's grammar is homoiconic: `def`/`defp`/`defmodule`/etc are
        // ordinary macro *calls*, not a distinct node kind — structurally
        // indistinguishable from `IO.puts(...)` without checking the
        // callee's actual TEXT ("def" vs anything else). `definition_macro_names`
        // (LangConstants) is what makes that text check possible; see its
        // doc comment and the Elixir LanguageSpec entry in lang_constants.rs.
        // Scope cut for this pass: only "def"/"defp" are recognized as
        // definitions (not defmodule/defmacro/defprotocol/etc), and there is
        // no class_context (no module-nesting support) — every function is
        // a flat top-level SymbolKind::Function, same treatment as Lua/Go's
        // lack of class_node_types.
        let code = "defmodule Greeter do\n  def greet(name) do\n    IO.puts(\"hello\")\n    format_greeting(name)\n  end\n\n  defp format_greeting(name) do\n    \"Hello, \" <> name\n  end\nend\n";
        let symbols = extract_symbols(code, "elixir", "a.ex").unwrap();
        let names = shallow_names(&symbols);
        assert!(names.contains(&"greet"), "should detect def: {names:?}");
        assert!(
            names.contains(&"format_greeting"),
            "should detect defp: {names:?}"
        );
        assert!(
            !names.contains(&"defmodule") && !names.contains(&"Greeter"),
            "defmodule itself should NOT be extracted as a symbol in this pass: {names:?}"
        );

        let greet = symbols
            .iter()
            .find(|s| s.name == "greet")
            .expect("greet should be found");
        assert_eq!(greet.kind, SymbolKind::Function);

        let calls = extract_calls(code, "elixir", "a.ex").unwrap();
        assert!(
            calls.iter().any(|c| c.enclosing_name == "greet"
                && c.callee == "puts"
                && c.receiver.as_deref() == Some("IO")),
            "qualified call (IO.puts) should produce a call edge with receiver: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "greet" && c.callee == "format_greeting"),
            "bare call should produce a call edge: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.callee == "def" || c.callee == "defp"),
            "def/defp must never themselves be misread as an ordinary call site: {calls:?}"
        );
        assert_eq!(
            calls.len(),
            2,
            "exactly 2 real call edges expected (IO.puts and format_greeting from \
             greet, and <> from format_greeting counts as a binary_operator not \
             a call) — catches a phantom self-call regression (a def's own \
             name+params wrapper node, e.g. \"greet\" in `def greet(name) do`, \
             misread as an ordinary call to itself): {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-haskell")]
    fn test_tier0_5_grammar_loads_haskell() {
        assert!(
            parse_tree("main :: IO ()\nmain = putStrLn \"hi\"\n", "haskell").is_some(),
            "tree-sitter-haskell grammar should load and parse — locks the pinned ABI \
             (=0.23.1, ABI 14)"
        );
    }

    #[test]
    #[cfg(feature = "lang-haskell")]
    fn test_haskell_real_grammar_symbols_and_calls_are_accurate() {
        // Haskell has two distinct top-level definition node kinds
        // (verified via a real AST dump, not guessed): `function` (has
        // parameters, e.g. `greet g = ...`) and `bind` (zero-arg value
        // definitions like `main = ...`). Both have a direct "name" field,
        // but `bind` ALSO shows up for local `let`/`where` bindings (e.g.
        // `let g = Greeter {...}` inside a do-block) with the identical
        // shape — only the immediate PARENT tells top-level and local
        // bindings apart ("declarations" vs "local_binds"). This test's
        // `main` (top-level, must appear) and the nested `let g = ...`
        // (local, must NOT appear) both exist specifically to lock that
        // distinction in.
        let code = "data Greeter = Greeter { name :: String }\n\ngreet :: Greeter -> String\ngreet g = do\n  putStrLn \"hello\"\n  formatGreeting g\n\nformatGreeting :: Greeter -> String\nformatGreeting g = \"Hello, \" ++ name g\n\nmain :: IO ()\nmain = do\n  let g = Greeter { name = \"world\" }\n  putStrLn (greet g)\n";
        let symbols = extract_symbols(code, "haskell", "a.hs").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            names.contains(&"Greeter"),
            "should detect data_type: {names:?}"
        );
        assert!(
            names.contains(&"greet"),
            "should detect function: {names:?}"
        );
        assert!(
            names.contains(&"formatGreeting"),
            "should detect second function: {names:?}"
        );
        assert!(
            names.contains(&"main"),
            "should detect top-level zero-arg `bind`: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|&&n| n == "g").count(),
            0,
            "the local `let g = ...` bind inside main's do-block must NOT be \
             extracted as a symbol — catches the exact bug a naive `bind` \
             arm (no parent check) would produce: {names:?}"
        );

        let greeter = symbols
            .iter()
            .find(|s| s.name == "Greeter")
            .expect("Greeter should be found");
        assert_eq!(greeter.kind, SymbolKind::Type);

        let calls = extract_calls(code, "haskell", "a.hs").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "greet" && c.callee == "putStrLn"),
            "bare call inside greet should produce a call edge: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "greet" && c.callee == "formatGreeting"),
            "second call inside greet should produce a call edge: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "main" && c.callee == "putStrLn"),
            "call inside main should produce a call edge: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-ocaml")]
    fn test_tier0_5_grammar_loads_ocaml() {
        assert!(
            parse_tree("let main () = print_string \"hi\"\n", "ocaml").is_some(),
            "tree-sitter-ocaml grammar should load and parse — locks the pinned ABI \
             (=0.24.2, ABI 14); a caret-range bump to 0.25.0+ (ABI 15) would silently \
             regress this to shallow line-scan"
        );
    }

    #[test]
    #[cfg(feature = "lang-ocaml")]
    fn test_ocaml_real_grammar_symbols_and_calls_are_accurate() {
        // OCaml's `let_binding` (has a direct "pattern" field, itself a
        // bare `value_name` for a simple non-destructuring binding) is the
        // EXACT SAME node kind for a real top-level definition
        // (`let greet g = ...`) and a local `let x = ... in ...` expression
        // (e.g. `let g = {...} in ...` inside main) — confirmed via a real
        // AST dump, same ambiguity class as Haskell's "bind". Distinguished
        // by the GRANDPARENT this time (every let_binding's immediate
        // parent is always a `value_definition` wrapper, for `and`-chains):
        // "structure"/"compilation_unit" for a real definition, vs
        // "let_expression" for a local one. This fixture's local
        // `let g = ...` inside `main` must NOT appear as a symbol.
        let code = "type greeter = { name : string }\n\nlet format_greeting g =\n  \"Hello, \" ^ g.name\n\nlet greet g =\n  print_string \"hello\";\n  format_greeting g\n\nlet main () =\n  let g = { name = \"world\" } in\n  print_string (greet g)\n";
        let symbols = extract_symbols(code, "ocaml", "a.ml").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            names.contains(&"greeter"),
            "should detect type_binding: {names:?}"
        );
        assert!(
            names.contains(&"greet"),
            "should detect let_binding function: {names:?}"
        );
        assert!(
            names.contains(&"format_greeting"),
            "should detect second let_binding function: {names:?}"
        );
        assert!(
            names.contains(&"main"),
            "should detect top-level zero-arg let_binding: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|&&n| n == "g").count(),
            0,
            "the local `let g = ... in ...` inside main must NOT be extracted as \
             a symbol — catches the exact bug a naive let_binding arm (no \
             grandparent check) would produce: {names:?}"
        );

        let greeter = symbols
            .iter()
            .find(|s| s.name == "greeter")
            .expect("greeter should be found");
        assert_eq!(greeter.kind, SymbolKind::Type);

        let calls = extract_calls(code, "ocaml", "a.ml").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "greet" && c.callee == "print_string"),
            "bare call inside greet should produce a call edge: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "greet" && c.callee == "format_greeting"),
            "second call inside greet should produce a call edge: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-zig")]
    fn test_tier0_5_grammar_loads_zig() {
        assert!(
            parse_tree("pub fn main() void {}\n", "zig").is_some(),
            "tree-sitter-zig grammar should load and parse — locks the pinned ABI \
             (=1.1.2, ABI 14)"
        );
    }

    #[test]
    #[cfg(feature = "lang-zig")]
    fn test_zig_real_grammar_symbols_and_calls_are_accurate() {
        // Zig's `variable_declaration` (`const Greeter = struct {...}`,
        // `const std = @import(...)`) is the SAME node kind for a real
        // top-level declaration and a LOCAL `const`/`var` inside a function
        // body (e.g. `const g = Greeter{...}` inside main) — same ambiguity
        // class as Haskell's "bind"/OCaml's "let_binding", but only one
        // level: distinguished by the immediate parent ("source_file" for
        // real, anything else — typically "block" — for local). This
        // fixture's local `const g = ...` inside `main` must NOT appear.
        let code = "const std = @import(\"std\");\n\nconst Greeter = struct {\n    name: []const u8,\n\n    fn greet(self: Greeter) void {\n        std.debug.print(\"hello\", .{});\n        self.formatGreeting();\n    }\n\n    fn formatGreeting(self: Greeter) void {}\n};\n\npub fn main() void {\n    const g = Greeter{ .name = \"world\" };\n    g.greet();\n}\n";
        let symbols = extract_symbols(code, "zig", "a.zig").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            names.contains(&"std"),
            "should detect top-level const import: {names:?}"
        );
        assert!(
            names.contains(&"Greeter"),
            "should detect top-level const struct: {names:?}"
        );
        assert!(names.contains(&"greet"), "should detect method: {names:?}");
        assert!(
            names.contains(&"formatGreeting"),
            "should detect second method: {names:?}"
        );
        assert!(names.contains(&"main"), "should detect pub fn: {names:?}");
        assert_eq!(
            names.iter().filter(|&&n| n == "g").count(),
            0,
            "the local `const g = ...` inside main must NOT be extracted as a \
             symbol — catches the exact bug a naive variable_declaration arm \
             (no parent check) would produce: {names:?}"
        );

        let greet = symbols
            .iter()
            .find(|s| s.name == "greet")
            .expect("greet should be found");
        // Function, not Method: `struct_declaration` has no "name" field of
        // its own (its name only exists on the OUTER wrapping
        // variable_declaration) — documented scope cut, no class_context
        // for struct-nested methods this pass, same category as
        // OCaml's module_binding / Elixir's defmodule skip.
        assert_eq!(greet.kind, SymbolKind::Function);
        assert!(greet.class_context.is_none());

        let calls = extract_calls(code, "zig", "a.zig").unwrap();
        assert!(
            calls.iter().any(|c| c.enclosing_name == "greet"
                && c.callee == "print"
                && c.receiver.as_deref() == Some("debug")),
            "chained field-access call should produce a call edge: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "greet"
                && c.callee == "formatGreeting"
                && c.receiver.as_deref() == Some("self")),
            "method-style call should produce a call edge with receiver: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.enclosing_name == "main"
                && c.callee == "greet"
                && c.receiver.as_deref() == Some("g")),
            "call on a local var should produce a call edge with receiver: {calls:?}"
        );
    }

    #[test]
    #[cfg(feature = "lang-powershell")]
    fn test_tier0_5_grammar_loads_powershell() {
        assert!(
            parse_tree("function Main {\n    Write-Host \"hi\"\n}\n", "powershell").is_some(),
            "tree-sitter-powershell grammar should load and parse — locks the pinned ABI \
             (=0.25.9, ABI 14); a caret-range bump to 0.25.10+ (ABI 15) would silently \
             regress this to shallow line-scan"
        );
    }

    #[test]
    #[cfg(feature = "lang-powershell")]
    fn test_powershell_real_grammar_symbols_and_calls_are_accurate() {
        // PowerShell has TWO distinct call mechanisms, confirmed via a real
        // AST dump: `command` (bare shell-style, `Write-Host "hello"` or a
        // user function called command-style, `Greet "world"` — has a
        // direct "command_name" field) and `invokation_expression`
        // (`.Method()` calls — zero fields, receiver/member_name/args are
        // all separate positional children, not one combined dotted-text
        // span the existing "$first_child" sentinel could split). Only
        // `command` is wired for call-graph extraction this pass —
        // `.Method()` calls are a documented scope cut, same category as
        // Dart's full call-graph cut but narrower (command-style calls,
        // the more pervasive style in real PowerShell scripts, still work).
        //
        // `class_statement`/`class_method_definition`/`function_statement`
        // all have ZERO fields — every name is a positional child, found by
        // kind ("simple_name" for the former two, "function_name" for the
        // latter, confirmed distinct via the dump).
        let code = "class Greeter {\n    [string]$Name\n\n    [string] Greet() {\n        Write-Host \"hello\"\n        return $this.FormatGreeting()\n    }\n\n    [string] FormatGreeting() {\n        return \"Hello, \" + $this.Name\n    }\n}\n\nfunction Main {\n    $g = [Greeter]::new()\n    $g.Greet()\n}\n";
        let symbols = extract_symbols(code, "powershell", "a.ps1").unwrap();
        let names = shallow_names(&symbols);
        assert!(
            names.contains(&"Greeter"),
            "should detect class_statement: {names:?}"
        );
        assert!(
            names.contains(&"Greet"),
            "should detect class_method_definition: {names:?}"
        );
        assert!(
            names.contains(&"FormatGreeting"),
            "should detect second class_method_definition: {names:?}"
        );
        assert!(
            names.contains(&"Main"),
            "should detect function_statement: {names:?}"
        );

        let greeter = symbols
            .iter()
            .find(|s| s.name == "Greeter")
            .expect("Greeter should be found");
        assert_eq!(greeter.kind, SymbolKind::Class);
        let greet = symbols
            .iter()
            .find(|s| s.name == "Greet")
            .expect("Greet should be found");
        assert_eq!(
            greet.kind,
            SymbolKind::Method,
            "class_method_definition is unambiguous by node kind alone, no \
             class_context needed to tell it apart from a free function"
        );

        let calls = extract_calls(code, "powershell", "a.ps1").unwrap();
        assert!(
            calls
                .iter()
                .any(|c| c.enclosing_name == "Greet" && c.callee == "Write-Host"),
            "command-style call should produce a call edge: {calls:?}"
        );
    }
}
