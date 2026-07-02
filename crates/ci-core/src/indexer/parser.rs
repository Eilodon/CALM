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
        "struct_item" => SymbolKind::Struct,
        "trait_item" => SymbolKind::Trait,
        // Reachable only if `resolve_name_node` ever grows an `impl_item` case (it
        // currently doesn't: `impl_item` has no `name` field, only `type`/`trait`).
        // `impl_item` stays in Rust's `function_node_types` purely so its children
        // get `class_context` via `class_node_types` below — emitting impl blocks
        // themselves as symbols would add one noisy duplicate-named entry per
        // `impl`/`impl Trait for` block without a corresponding consumer.
        "impl_item" => SymbolKind::Impl,
        "interface_declaration" => SymbolKind::Interface,
        "type_declaration" => SymbolKind::Type,
        // Explicit method nodes are always Method regardless of scope.
        "method_declaration" | "method_definition" => SymbolKind::Method,
        // JS/TS `const foo = () => {}` — treated as a variable holding a function.
        "lexical_declaration" => SymbolKind::Variable,
        // Plain function nodes: Method when inside a class/impl, Function otherwise.
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
    let lang: tree_sitter::Language = match language {
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        // Tier-0.5: optional grammar crates, each gated by a Cargo feature.
        #[cfg(feature = "lang-ruby")]
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        #[cfg(feature = "lang-php")]
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        // kotlin and swift: lang-kotlin / lang-swift are no-op feature stubs;
        // their tree-sitter crates use incompatible API versions. These languages
        // always fall back to regex extraction in extract_file_data.
        #[cfg(feature = "lang-csharp")]
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        #[cfg(feature = "lang-shell")]
        "shell" | "bash" => tree_sitter_bash::LANGUAGE.into(),
        #[cfg(feature = "lang-c")]
        "c" => tree_sitter_c::LANGUAGE.into(),
        #[cfg(feature = "lang-cpp")]
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
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
        &mut symbols,
    );
    symbols
}

/// Resolve the name node for `node`. Most `function_node_types` expose `name`
/// directly via `lc.name_field`, but a few wrap the name on a nested child:
/// Go `type_declaration` holds it on a `type_spec` child, and JS/TS
/// `lexical_declaration` holds it on a `variable_declarator` child (only
/// followed when the declarator's value is itself a function literal, so
/// plain `const x = 5` is not treated as a symbol).
fn resolve_name_node<'a>(
    node: tree_sitter::Node<'a>,
    lc: &crate::indexer::lang_constants::LangConstants,
) -> Option<tree_sitter::Node<'a>> {
    if let Some(n) = node.child_by_field_name(lc.name_field) {
        return Some(n);
    }
    match node.kind() {
        "type_declaration" => {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|c| c.kind() == "type_spec")
                .and_then(|spec| spec.child_by_field_name("name"))
        }
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
        _ => None,
    }
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
        && let Some(name_node) = resolve_name_node(node, lc)
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

/// Decorator/attribute sibling node kind(s) that may precede a definition, per
/// language. Java needs two: `@Foo` parses as `marker_annotation`, `@Foo(...)`
/// as `annotation`.
fn decorator_node_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "python" => &["decorator"],
        "rust" => &["attribute_item"],
        "java" => &["marker_annotation", "annotation"],
        _ => &[],
    }
}

/// Source text of every decorator/attribute immediately preceding `node` (innermost first).
fn collect_decorators<'a>(
    node: tree_sitter::Node,
    source: &'a str,
    kinds: &[&str],
) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut sib = node.prev_named_sibling();
    while let Some(s) = sib {
        if !kinds.contains(&s.kind()) {
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
                            && source[p.byte_range()]
                                .trim_start()
                                .starts_with("export default")
                    })
                    .unwrap_or(false)
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
fn strip_modifiers<'a>(s: &'a str, language: &str) -> &'a str {
    let modifiers: &[&str] = match language {
        "csharp" => &[
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
        "kotlin" => &[
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
        "swift" => &[
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
        "php" => &[
            "public ",
            "private ",
            "protected ",
            "static ",
            "abstract ",
            "final ",
        ],
        "cpp" | "c" => &[
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
        _ => &[],
    };
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

fn detect_c_cpp(s: &str) -> Option<(String, SymbolKind)> {
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

fn detect_csharp(s: &str) -> Option<(String, SymbolKind)> {
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

fn detect_ruby(s: &str) -> Option<(String, SymbolKind)> {
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

fn detect_shell(s: &str) -> Option<(String, SymbolKind)> {
    // `function name` or `function name()` or `name()` at line start
    if let Some(rest) = s.strip_prefix("function ") {
        let name = ident_at_start(rest.trim_start())?;
        return Some((name, SymbolKind::Function));
    }
    // `name() {` or `name () {`
    if let Some(paren_pos) = s.find('(') {
        let before = s[..paren_pos].trim_end();
        if !before.contains(' ') && !before.is_empty() {
            let name = ident_at_start(before)?;
            return Some((name, SymbolKind::Function));
        }
    }
    None
}

fn detect_kotlin(s: &str) -> Option<(String, SymbolKind)> {
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

fn detect_swift(s: &str) -> Option<(String, SymbolKind)> {
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

fn detect_php(s: &str) -> Option<(String, SymbolKind)> {
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
    match language {
        "ruby" | "shell" | "bash" | "php" => {
            trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.starts_with('*')
        }
        _ => {
            trimmed.starts_with("//")
                || trimmed.starts_with('*')
                || trimmed.starts_with("/*")
                || trimmed.starts_with('#')
        }
    }
}

fn detect_shallow(trimmed: &str, language: &str) -> Option<(String, SymbolKind)> {
    let s = strip_modifiers(trimmed, language);
    match language {
        "c" | "cpp" => detect_c_cpp(s),
        "csharp" => detect_csharp(s),
        "ruby" => detect_ruby(s),
        "shell" | "bash" => detect_shell(s),
        "kotlin" => detect_kotlin(s),
        "swift" => detect_swift(s),
        "php" => detect_php(s),
        _ => None,
    }
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
}
