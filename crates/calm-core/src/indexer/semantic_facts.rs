//! Tier 1 semantic facts (2026-08-07 roadmap,
//! docs/plans/2026-08-07-pecorino-adoption-roadmap.md T1): type relations
//! (`extends`/`implements`) and symbol effects (explicit throws, field
//! writes) extracted directly from tree-sitter syntax.
//!
//! Deliberately conservative, same posture as `resolver::conservative`:
//! only facts with unambiguous syntax are extracted. Grammar shapes below
//! were verified against real parsed output (not assumed from memory) via
//! a throwaway `cargo run --example dump_grammar` S-expression dump —
//! notably that Rust's `impl_item` never gets its own `symbols` row (see
//! `parser::node_kind_to_symbol_kind`'s `impl_item` arm), so a `impl Trait
//! for Type` relation cannot be resolved the same (bare_name, def_line) way
//! the other four languages are — see `RawTypeRelation`'s doc comment.
//!
//! Scope cuts, all deliberate (not oversights):
//! - Go: no `extends`/`implements` syntax exists; inferring `implements`
//!   from structural method-set matching would fabricate a fact stronger
//!   than the syntax evidence (Go's typing is structural) — deferred.
//! - Rust/Go throws: neither has exception syntax (`Result`/`panic!` are a
//!   different semantic shape than a typed `raise`/`throw`) — deferred.
//! - Go writes: needs receiver-variable-name correlation (`r.X = v` inside
//!   `func (r *Foo) M()`) which isn't wired here yet — deferred.
//! - `self.cache.put(x)`-style receiver-method calls are never treated as
//!   a mutation of `self` — only a direct `self.field = ...`/`this.field =
//!   ...` assignment is syntactically exact enough to call a write.
//! - Rust trait supertraits (`trait A: B + C`) are never emitted as a type
//!   relation — `extract_rust_impl` only ever matches `impl_item` nodes, so
//!   a `trait_item`'s `B + C` bound list is structurally never visited.
//!   Only `impl Trait for Type` is in scope for v1.
//!
//! No `SemanticFactExtractor` trait/enum abstraction here (unlike the
//! Phase-0 roadmap sketch) — with exactly two fact shapes (type relations,
//! effects) and one call site each (`pipeline::extract_file_data`), a
//! trait-object dispatch layer would be pure ceremony over these two free
//! functions. Revisit if/when a third fact shape needs the same walk-and-
//! resolve lifecycle.

use tree_sitter::{Node, Tree};

/// One `extends`/`implements` fact, still keyed by the BARE class name and
/// its own definition line — resolved to a real `symbols.qualified_name`
/// later, in `pipeline::extract_file_data`, the same two-phase shape
/// `RawCall::enclosing_name`/`enclosing_line` already uses for call sites.
///
/// For every language except Rust, `class_line` is the exact line of the
/// class/interface node itself, so resolution there is an exact
/// `(class_name, class_line)` lookup against the file's own `qn_by_loc` —
/// guaranteed to hit, since that's the very node that put the class into
/// `symbols` in the first place. For Rust, `class_line` is the *impl
/// block's* line (impl blocks never get their own symbol row — see the
/// module doc comment), so resolution there falls back to a same-file
/// bare-name lookup against this file's own class-like symbols instead.
pub struct RawTypeRelation {
    pub class_name: String,
    pub class_line: usize,
    pub relation_kind: &'static str, // "extends" | "implements"
    pub target_text: String,
}

/// One `explicit_throw`/`write_field` fact, keyed by the BARE enclosing
/// function/method name and its own definition line — resolved the same
/// way `RawCall::enclosing_name`/`enclosing_line` already is.
pub struct RawEffect {
    pub enclosing_name: String,
    pub enclosing_line: usize,
    pub effect_kind: &'static str, // "explicit_throw" | "write_field"
    pub target_text: String,
    pub line: usize,
}

pub fn extract_type_relations_from_tree(
    tree: &Tree,
    source: &str,
    language: &str,
) -> Vec<RawTypeRelation> {
    // Only languages with real extends/implements-shaped syntax are
    // dispatched — every other language is a guaranteed no-op walk, same
    // early-out shape as `extract_file_aliases_from_tree`'s language gate.
    if !matches!(
        language,
        "java" | "typescript" | "javascript" | "python" | "rust"
    ) {
        return Vec::new();
    }
    let mut out = Vec::new();
    walk_type_relations(tree.root_node(), source, language, &mut out);
    out
}

fn walk_type_relations(node: Node, source: &str, language: &str, out: &mut Vec<RawTypeRelation>) {
    match language {
        "java" => extract_java_class(node, source, out),
        "typescript" => extract_ts_class(node, source, out),
        "javascript" => extract_js_class(node, source, out),
        "python" => extract_python_class(node, source, out),
        "rust" => extract_rust_impl(node, source, out),
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_type_relations(child, source, language, out);
    }
}

fn first_named_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

/// Java `class_declaration`: `superclass: (superclass (type_identifier))`
/// and `interfaces: (super_interfaces (type_list (type_identifier)...))` —
/// both real FIELDS on the node (unlike TS/JS's unnamed `class_heritage`
/// child), verified via `dump_grammar`.
fn extract_java_class(node: Node, source: &str, out: &mut Vec<RawTypeRelation>) {
    if node.kind() != "class_declaration" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let class_name = source[name_node.byte_range()].to_string();
    let class_line = node.start_position().row + 1;

    if let Some(superclass) = node.child_by_field_name("superclass")
        && let Some(type_node) = first_named_child(superclass)
    {
        out.push(RawTypeRelation {
            class_name: class_name.clone(),
            class_line,
            relation_kind: "extends",
            target_text: source[type_node.byte_range()].trim().to_string(),
        });
    }
    if let Some(interfaces) = node.child_by_field_name("interfaces")
        && let Some(type_list) = first_named_child(interfaces)
    {
        let mut cursor = type_list.walk();
        for t in type_list.named_children(&mut cursor) {
            out.push(RawTypeRelation {
                class_name: class_name.clone(),
                class_line,
                relation_kind: "implements",
                target_text: source[t.byte_range()].trim().to_string(),
            });
        }
    }
}

/// TypeScript `class_declaration`: heritage sits as an unnamed `class_heritage`
/// CHILD (not a field), itself wrapping an `extends_clause` (field `value`)
/// and/or an `implements_clause` (bare named children) — verified via
/// `dump_grammar`, distinct shape from both Java (real fields) and JS (no
/// `extends_clause` wrapper at all, see `extract_js_class`).
fn extract_ts_class(node: Node, source: &str, out: &mut Vec<RawTypeRelation>) {
    if node.kind() != "class_declaration" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let class_name = source[name_node.byte_range()].to_string();
    let class_line = node.start_position().row + 1;

    let mut cursor = node.walk();
    let Some(heritage) = node
        .children(&mut cursor)
        .find(|c| c.kind() == "class_heritage")
    else {
        return;
    };

    let mut hc = heritage.walk();
    for child in heritage.children(&mut hc) {
        match child.kind() {
            "extends_clause" => {
                if let Some(value) = child.child_by_field_name("value") {
                    out.push(RawTypeRelation {
                        class_name: class_name.clone(),
                        class_line,
                        relation_kind: "extends",
                        target_text: source[value.byte_range()].trim().to_string(),
                    });
                }
            }
            "implements_clause" => {
                let mut ic = child.walk();
                for t in child.named_children(&mut ic) {
                    out.push(RawTypeRelation {
                        class_name: class_name.clone(),
                        class_line,
                        relation_kind: "implements",
                        target_text: source[t.byte_range()].trim().to_string(),
                    });
                }
            }
            _ => {}
        }
    }
}

/// JavaScript `class_declaration`: heritage is also an unnamed `class_heritage`
/// child, but — unlike TypeScript — has NO `extends_clause` wrapper: its
/// first named child IS the base expression directly (an `identifier` or
/// `member_expression`). JS has no `implements` keyword at all. Verified
/// via `dump_grammar` (`(class_heritage (identifier))`, no nested clause).
fn extract_js_class(node: Node, source: &str, out: &mut Vec<RawTypeRelation>) {
    if node.kind() != "class_declaration" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let class_name = source[name_node.byte_range()].to_string();
    let class_line = node.start_position().row + 1;

    let mut cursor = node.walk();
    let Some(heritage) = node
        .children(&mut cursor)
        .find(|c| c.kind() == "class_heritage")
    else {
        return;
    };
    if let Some(base) = first_named_child(heritage) {
        out.push(RawTypeRelation {
            class_name,
            class_line,
            relation_kind: "extends",
            target_text: source[base.byte_range()].trim().to_string(),
        });
    }
}

/// Python `class_definition`: bases sit in field `superclasses`, an
/// `argument_list` node — each named child is a base class EXCEPT a
/// `keyword_argument` (e.g. `metaclass=Meta`), which is not a real base
/// and is filtered out. Verified via `dump_grammar`, including the
/// `metaclass=` exclusion case specifically.
fn extract_python_class(node: Node, source: &str, out: &mut Vec<RawTypeRelation>) {
    if node.kind() != "class_definition" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let class_name = source[name_node.byte_range()].to_string();
    let class_line = node.start_position().row + 1;

    let Some(superclasses) = node.child_by_field_name("superclasses") else {
        return;
    };
    let mut cursor = superclasses.walk();
    for child in superclasses.named_children(&mut cursor) {
        if child.kind() == "keyword_argument" {
            continue;
        }
        out.push(RawTypeRelation {
            class_name: class_name.clone(),
            class_line,
            relation_kind: "extends",
            target_text: source[child.byte_range()].trim().to_string(),
        });
    }
}

/// Rust `impl_item`: `impl Trait for Type { .. }` has both a `trait` field
/// and a `type` field (the Self type) — this is Rust's ONLY type-relation
/// shape (no struct inheritance exists). An inherent `impl Type { .. }`
/// (no `trait` field) carries no relation and is skipped. See the module
/// doc comment for why `class_name`/`class_line` here need the by-name
/// fallback resolution downstream instead of the exact-line lookup the
/// other four languages get.
fn extract_rust_impl(node: Node, source: &str, out: &mut Vec<RawTypeRelation>) {
    if node.kind() != "impl_item" {
        return;
    }
    let Some(trait_node) = node.child_by_field_name("trait") else {
        return;
    };
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    out.push(RawTypeRelation {
        class_name: source[type_node.byte_range()].trim().to_string(),
        class_line: node.start_position().row + 1,
        relation_kind: "implements",
        target_text: source[trait_node.byte_range()].trim().to_string(),
    });
}

pub fn extract_effects_from_tree(tree: &Tree, source: &str, language: &str) -> Vec<RawEffect> {
    let Some(spec) = crate::indexer::lang_constants::find_spec(language) else {
        return Vec::new();
    };
    if !matches!(
        language,
        "rust" | "python" | "java" | "typescript" | "javascript"
    ) {
        return Vec::new();
    }
    let mut out = Vec::new();
    walk_effects(
        tree.root_node(),
        source,
        language,
        &spec.constants,
        None,
        &mut out,
    );
    out
}

/// Mirrors `parser::walk_calls`'s enclosing-symbol tracking exactly (same
/// `resolve_name_node` call, same `(bare_name, def_line)` tuple shape) so
/// downstream resolution against `qn_by_loc` in `pipeline::extract_file_data`
/// is guaranteed to hit the identical symbol a call site would.
fn walk_effects(
    node: Node,
    source: &str,
    language: &str,
    lc: &crate::indexer::lang_constants::LangConstants,
    enclosing: Option<(String, usize)>,
    out: &mut Vec<RawEffect>,
) {
    let mut current = enclosing;
    if lc.function_node_types.contains(&node.kind())
        && let Some(name_node) = crate::indexer::parser::resolve_name_node(node, source, lc)
    {
        current = Some((
            source[name_node.byte_range()].to_string(),
            node.start_position().row + 1,
        ));
    }

    // No enclosing function/method known yet (module-level code) — skip.
    // `self`/`this` writes can't syntactically occur outside a method
    // anyway; this just keeps the gate uniform across languages instead of
    // special-casing a module-level sentinel nobody needs here.
    if let Some((enclosing_name, enclosing_line)) = &current
        && let Some((kind, text)) = detect_effect(node, source, language)
    {
        out.push(RawEffect {
            enclosing_name: enclosing_name.clone(),
            enclosing_line: *enclosing_line,
            effect_kind: kind,
            target_text: text,
            line: node.start_position().row + 1,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_effects(child, source, language, lc, current.clone(), out);
    }
}

fn detect_effect(node: Node, source: &str, language: &str) -> Option<(&'static str, String)> {
    match language {
        "rust" => detect_rust_write(node, source),
        "python" => detect_python_write(node, source).or_else(|| detect_python_throw(node, source)),
        "java" => detect_java_write(node, source).or_else(|| detect_java_throw(node, source)),
        "typescript" | "javascript" => {
            detect_tsjs_write(node, source).or_else(|| detect_tsjs_throw(node, source))
        }
        _ => None,
    }
}

/// `self.x = v;` / `self.x += 1;` inside `&mut self`/`mut self` methods —
/// `left: (field_expression value: (self) field: (field_identifier))` for
/// both `assignment_expression` and `compound_assignment_expr`. Guaranteed
/// to only ever parse this way inside a mutable-receiver method: a `&self`
/// (non-mut) method writing `self.x` is a compile error, so the Rust
/// compiler itself is the soundness guarantee here, not this extractor.
fn detect_rust_write(node: Node, source: &str) -> Option<(&'static str, String)> {
    if !matches!(
        node.kind(),
        "assignment_expression" | "compound_assignment_expr"
    ) {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    if left.kind() != "field_expression" {
        return None;
    }
    let value = left.child_by_field_name("value")?;
    if value.kind() != "self" {
        return None;
    }
    let field = left.child_by_field_name("field")?;
    Some(("write_field", source[field.byte_range()].to_string()))
}

/// `self.x = v` / `self.x += 1` — Python has no dedicated `self` node kind
/// (it's a plain `identifier` by convention), so the receiver is checked
/// by SOURCE TEXT, not node kind, unlike every other language here.
fn detect_python_write(node: Node, source: &str) -> Option<(&'static str, String)> {
    if !matches!(node.kind(), "assignment" | "augmented_assignment") {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    if left.kind() != "attribute" {
        return None;
    }
    let object = left.child_by_field_name("object")?;
    if source[object.byte_range()].trim() != "self" {
        return None;
    }
    let attr = left.child_by_field_name("attribute")?;
    Some(("write_field", source[attr.byte_range()].to_string()))
}

/// `raise InvalidToken(...)` / `raise InvalidToken` / `raise mod.Err(...)`.
/// Only the FIRST named child of `raise_statement` is inspected — a bare
/// `raise` (re-raise, no children) or `raise X from Y` (cause is a
/// separate later child) both correctly fall through to `None` rather than
/// risk misreading `from Y`'s cause as the raised exception. The target is
/// further gated by `looks_like_exception_reference` (see its own doc
/// comment) so `raise e` / `raise factory()` — a re-raised variable or an
/// arbitrary call, neither provably an exception type from AST alone —
/// don't get mislabeled as if their text were a resolved exception class.
fn detect_python_throw(node: Node, source: &str) -> Option<(&'static str, String)> {
    if node.kind() != "raise_statement" {
        return None;
    }
    let mut cursor = node.walk();
    let first = node.named_children(&mut cursor).next()?;
    match first.kind() {
        "call" => {
            let func = first.child_by_field_name("function")?;
            let text = source[func.byte_range()].trim().to_string();
            looks_like_exception_reference(&text).then_some(("explicit_throw", text))
        }
        "identifier" => {
            let text = source[first.byte_range()].trim().to_string();
            looks_like_exception_reference(&text).then_some(("explicit_throw", text))
        }
        _ => None,
    }
}

/// PEP 8 class-naming convention check (PascalCase), applied to the LAST
/// dotted segment (`mod.Err` -> checks `Err`, not `mod`). The only
/// naming-convention-based (not pure-AST) check in this module — used to
/// gate Python `raise` targets specifically, because Python's grammar
/// alone can't distinguish `raise SomeException(...)` (constructing an
/// exception) from `raise factory(...)` (an arbitrary call), nor a bare
/// class reference (`raise NotImplementedError`) from a bare re-raise of a
/// bound variable (`raise e`) — unlike Java/TS/JS's `new X(...)`, there is
/// no syntax marker for construction. Without this filter, `raise e`
/// would mislabel the caught-exception variable itself as if it were the
/// exception type ("Throws: e"), which is exactly the kind of fabricated-
/// beyond-the-evidence claim this module's own doc comment disavows.
fn looks_like_exception_reference(text: &str) -> bool {
    text.rsplit('.')
        .next()
        .and_then(|last| last.chars().next())
        .is_some_and(|c| c.is_uppercase())
}

/// `this.x = v;` — `left: (field_access object: (this) field: (identifier))`.
/// Deliberately does NOT match a bare `x = v;` (ambiguous: local variable
/// vs. implicit field access) — conservative, matches this module's stated
/// posture of missing data over fabricated data.
fn detect_java_write(node: Node, source: &str) -> Option<(&'static str, String)> {
    if node.kind() != "assignment_expression" {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    if left.kind() != "field_access" {
        return None;
    }
    let object = left.child_by_field_name("object")?;
    if object.kind() != "this" {
        return None;
    }
    let field = left.child_by_field_name("field")?;
    Some(("write_field", source[field.byte_range()].to_string()))
}

/// `throw new InvalidToken(...);` — only a direct `new X(...)` construction
/// is captured; `throw someVar;`/`throw e;` (rethrow, or a variable holding
/// an exception) is skipped, same conservative posture as the Python/TS/JS
/// throw detectors.
fn detect_java_throw(node: Node, source: &str) -> Option<(&'static str, String)> {
    if node.kind() != "throw_statement" {
        return None;
    }
    let mut cursor = node.walk();
    let ctor = node
        .children(&mut cursor)
        .find(|c| c.kind() == "object_creation_expression")?;
    let ty = ctor.child_by_field_name("type")?;
    Some(("explicit_throw", source[ty.byte_range()].trim().to_string()))
}

/// `this.x = v` / `this.x += 1` (TS and JS share this shape identically) —
/// `left: (member_expression object: (this) property: (property_identifier))`.
fn detect_tsjs_write(node: Node, source: &str) -> Option<(&'static str, String)> {
    if !matches!(
        node.kind(),
        "assignment_expression" | "augmented_assignment_expression"
    ) {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    if left.kind() != "member_expression" {
        return None;
    }
    let object = left.child_by_field_name("object")?;
    if object.kind() != "this" {
        return None;
    }
    let prop = left.child_by_field_name("property")?;
    Some(("write_field", source[prop.byte_range()].to_string()))
}

/// `throw new InvalidToken();` — only direct `new X(...)` construction.
fn detect_tsjs_throw(node: Node, source: &str) -> Option<(&'static str, String)> {
    if node.kind() != "throw_statement" {
        return None;
    }
    let mut cursor = node.walk();
    let ctor = node
        .children(&mut cursor)
        .find(|c| c.kind() == "new_expression")?;
    let target = ctor.child_by_field_name("constructor")?;
    Some((
        "explicit_throw",
        source[target.byte_range()].trim().to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::parser::parse_tree;

    fn relations(lang: &str, src: &str) -> Vec<(String, &'static str, String)> {
        let tree = parse_tree(src, lang).expect("parse");
        extract_type_relations_from_tree(&tree, src, lang)
            .into_iter()
            .map(|r| (r.class_name, r.relation_kind, r.target_text))
            .collect()
    }

    fn effects(lang: &str, src: &str) -> Vec<(String, &'static str, String)> {
        let tree = parse_tree(src, lang).expect("parse");
        extract_effects_from_tree(&tree, src, lang)
            .into_iter()
            .map(|e| (e.enclosing_name, e.effect_kind, e.target_text))
            .collect()
    }

    #[test]
    fn java_extends_and_implements() {
        let rs = relations(
            "java",
            "class Foo extends Bar implements Baz, Qux { void m() {} }",
        );
        assert_eq!(
            rs,
            vec![
                ("Foo".into(), "extends", "Bar".into()),
                ("Foo".into(), "implements", "Baz".into()),
                ("Foo".into(), "implements", "Qux".into()),
            ]
        );
    }

    #[test]
    fn java_no_heritage_is_empty() {
        assert!(relations("java", "class Foo { void m() {} }").is_empty());
    }

    #[test]
    fn typescript_extends_and_implements() {
        let rs = relations(
            "typescript",
            "class Foo extends Bar implements Baz, Qux { m() {} }",
        );
        assert_eq!(
            rs,
            vec![
                ("Foo".into(), "extends", "Bar".into()),
                ("Foo".into(), "implements", "Baz".into()),
                ("Foo".into(), "implements", "Qux".into()),
            ]
        );
    }

    #[test]
    fn typescript_implements_only() {
        let rs = relations(
            "typescript",
            "interface Baz {}\nclass Foo implements Baz {}\n",
        );
        assert_eq!(rs, vec![("Foo".into(), "implements", "Baz".into())]);
    }

    #[test]
    fn javascript_extends_only_no_implements_keyword() {
        let rs = relations("javascript", "class Foo extends Bar { m() {} }");
        assert_eq!(rs, vec![("Foo".into(), "extends", "Bar".into())]);
    }

    #[test]
    fn python_multiple_bases_and_metaclass_excluded() {
        let rs = relations("python", "class Foo(Bar, Baz, metaclass=Meta):\n    pass\n");
        assert_eq!(
            rs,
            vec![
                ("Foo".into(), "extends", "Bar".into()),
                ("Foo".into(), "extends", "Baz".into()),
            ]
        );
    }

    #[test]
    fn python_no_bases_is_empty() {
        assert!(relations("python", "class Foo:\n    pass\n").is_empty());
    }

    #[test]
    fn rust_impl_trait_for_type_is_implements() {
        let rs = relations(
            "rust",
            "trait Bar {}\nstruct Foo;\nimpl Bar for Foo { fn m(&self) {} }\n",
        );
        assert_eq!(rs, vec![("Foo".into(), "implements", "Bar".into())]);
    }

    #[test]
    fn rust_inherent_impl_has_no_relation() {
        assert!(relations("rust", "struct Foo;\nimpl Foo { fn m(&self) {} }\n").is_empty());
    }

    #[test]
    fn rust_write_field_via_mut_self() {
        let es = effects(
            "rust",
            "struct Foo { x: i32 }\nimpl Foo {\n    fn m(&mut self, v: i32) {\n        self.x = v;\n        self.x += 1;\n    }\n}\n",
        );
        assert_eq!(
            es,
            vec![
                ("m".into(), "write_field", "x".into()),
                ("m".into(), "write_field", "x".into()),
            ]
        );
    }

    #[test]
    fn python_write_field_and_throw() {
        let es = effects(
            "python",
            "class Foo:\n    def m(self, v):\n        self.x = v\n        self.y += 1\n        raise InvalidToken()\n",
        );
        assert_eq!(
            es,
            vec![
                ("m".into(), "write_field", "x".into()),
                ("m".into(), "write_field", "y".into()),
                ("m".into(), "explicit_throw", "InvalidToken".into()),
            ]
        );
    }

    #[test]
    fn python_bare_raise_identifier_and_qualified_call() {
        let es = effects(
            "python",
            "def f():\n    raise NotImplementedError\ndef g():\n    raise mod.Err('x')\n",
        );
        assert_eq!(
            es,
            vec![
                ("f".into(), "explicit_throw", "NotImplementedError".into()),
                ("g".into(), "explicit_throw", "mod.Err".into()),
            ]
        );
    }

    #[test]
    fn python_reraise_of_bound_variable_is_not_captured() {
        // `raise e` -- `e` is a caught-exception variable, not an
        // exception TYPE reference. Capturing it would mislabel the
        // variable's name as if it were a resolved exception class.
        assert!(
            effects(
                "python",
                "def f():\n    try:\n        pass\n    except Exception as e:\n        raise e\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn python_raise_of_lowercase_factory_call_is_not_captured() {
        // `raise factory()` -- syntactically identical to
        // `raise SomeException()`; only the PEP 8 casing convention lets
        // us tell them apart without full symbol resolution.
        assert!(effects("python", "def f():\n    raise factory()\n").is_empty());
    }

    #[test]
    fn python_bare_reraise_is_skipped() {
        assert!(
            effects(
                "python",
                "def f():\n    try:\n        pass\n    except Exception:\n        raise\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn java_write_field_via_this_only() {
        let es = effects(
            "java",
            "class Foo { int x; void m(int v) { this.x = v; x = v; } }",
        );
        // Only the `this.x = v` write is captured; bare `x = v` (ambiguous
        // local-vs-field) is deliberately not.
        assert_eq!(es, vec![("m".into(), "write_field", "x".into())]);
    }

    #[test]
    fn java_throw_new_is_captured_rethrow_is_not() {
        let es = effects(
            "java",
            "class Foo { void m() { throw new InvalidToken(); } void n(Exception e) { throw e; } }",
        );
        assert_eq!(
            es,
            vec![("m".into(), "explicit_throw", "InvalidToken".into())]
        );
    }

    #[test]
    fn typescript_write_field_and_throw() {
        let es = effects(
            "typescript",
            "class Foo {\n  x: number = 0;\n  m(v: number) {\n    this.x = v;\n    this.x += 1;\n    x = v;\n  }\n  n() {\n    throw new InvalidToken();\n  }\n}\n",
        );
        assert_eq!(
            es,
            vec![
                ("m".into(), "write_field", "x".into()),
                ("m".into(), "write_field", "x".into()),
                ("n".into(), "explicit_throw", "InvalidToken".into()),
            ]
        );
    }

    #[test]
    fn javascript_write_field() {
        let es = effects(
            "javascript",
            "class Foo {\n  m(v) {\n    this.x = v;\n  }\n}\n",
        );
        assert_eq!(es, vec![("m".into(), "write_field", "x".into())]);
    }

    #[test]
    fn go_and_rust_throw_are_out_of_scope() {
        assert!(effects("go", "package main\nfunc f() { panic(\"x\") }\n").is_empty());
        assert!(effects("rust", "fn f() { panic!(\"x\"); }\n").is_empty());
    }
}
