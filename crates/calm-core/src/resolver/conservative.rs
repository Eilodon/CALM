use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::lang_constants::assignment_nodes;
use super::{FileContext, ResolveResult};
use crate::types::EdgeConfidence;

pub struct ConservativeResolver {
    assignment_node_types: HashMap<&'static str, &'static [&'static str]>,
}

impl ConservativeResolver {
    pub fn new() -> Self {
        Self {
            assignment_node_types: assignment_nodes(),
        }
    }

    pub fn extract_aliases(
        &self,
        root: Node,
        source: &[u8],
        language: &str,
        ctx: &FileContext,
    ) -> HashMap<String, String> {
        let Some(node_types) = self.assignment_node_types.get(language) else {
            return HashMap::new();
        };

        // Pre-pass: detect multiply-assigned LHS [F7]
        let mut lhs_seen: HashSet<String> = HashSet::new();
        let mut multi_assigned: HashSet<String> = HashSet::new();
        walk_nodes(root, node_types, &mut |node| {
            if let Some(lhs) = get_assignment_lhs(node, source, language)
                && !lhs_seen.insert(lhs.clone())
            {
                multi_assigned.insert(lhs);
            }
        });

        // Main pass: build alias_map
        let mut alias_map: HashMap<String, String> = HashMap::new();
        walk_nodes(root, node_types, &mut |node| {
            let Some((lhs, rhs)) = get_assignment_lhs_rhs(node, source, language) else {
                return;
            };
            if !multi_assigned.contains(&lhs)
                && !ctx.file_symbols.contains(&lhs)
                && !ctx.import_map.contains_key(&lhs)
                && !ctx.type_map.contains_key(&lhs)
                && (ctx.file_symbols.contains(&rhs) || ctx.import_map.contains_key(&rhs))
            {
                alias_map.insert(lhs, rhs);
            }
        });

        alias_map
    }

    pub fn resolve_tier1(
        &self,
        callee_name: &str,
        ctx: &FileContext,
        alias_map: &HashMap<String, String>,
    ) -> ResolveResult {
        if ctx.file_symbols.contains(callee_name) {
            return ResolveResult {
                confidence: EdgeConfidence::Resolved,
                resolved_path: None,
            };
        }
        if let Some(path) = ctx.import_map.get(callee_name) {
            return ResolveResult {
                confidence: EdgeConfidence::Resolved,
                resolved_path: Some(path.clone()),
            };
        }
        if let Some(alias_target) = alias_map.get(callee_name) {
            return ResolveResult {
                confidence: EdgeConfidence::Resolved,
                resolved_path: ctx.import_map.get(alias_target).cloned(),
            };
        }
        ResolveResult {
            confidence: EdgeConfidence::Textual,
            resolved_path: None,
        }
    }

    /// Tier-2: infer the type a method call's receiver resolves to.
    ///
    /// `self`/`this` resolve to the enclosing class; any other receiver resolves
    /// through the `type_map` (explicit annotations). Returns the bare type name
    /// — the class in which to look the method up — or `None` when the receiver's
    /// type is unknown (the call then stays `textual`). A hit is `"inferred"`
    /// confidence: reliable, but not as certain as a direct binding.
    pub fn resolve_tier2(
        &self,
        receiver: &str,
        ctx: &FileContext,
        enclosing_class: Option<&str>,
    ) -> Option<String> {
        let ty = match receiver {
            "self" | "this" => enclosing_class?.to_string(),
            other => ctx.type_map.get(other)?.clone(),
        };
        // Reduce `*Service` / `&Foo` / `pkg.Foo` / `Foo<T>` / `List[int]` to the
        // bare type name: drop leading ref/pointer sigils, any module qualifier,
        // and trailing generics.
        let trimmed = ty.trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        let seg = trimmed.rsplit(['.', ':']).next().unwrap_or(trimmed);
        let bare = seg
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>();
        if bare.is_empty() { None } else { Some(bare) }
    }
}

impl Default for ConservativeResolver {
    fn default() -> Self {
        Self::new()
    }
}

fn walk_nodes<F>(node: Node, target_types: &[&str], callback: &mut F)
where
    F: FnMut(Node),
{
    if target_types.contains(&node.kind()) {
        callback(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_nodes(child, target_types, callback);
    }
}

fn get_assignment_lhs(node: Node, source: &[u8], language: &str) -> Option<String> {
    get_assignment_lhs_rhs(node, source, language).map(|(lhs, _)| lhs)
}

fn get_assignment_lhs_rhs(node: Node, source: &[u8], language: &str) -> Option<(String, String)> {
    match language {
        "python" => get_python_assignment(node, source),
        "typescript" | "javascript" => get_ts_js_assignment(node, source),
        "java" => get_java_assignment(node, source),
        "rust" => get_rust_assignment(node, source),
        "go" => get_go_assignment(node, source),
        _ => None,
    }
}

fn get_python_assignment(node: Node, source: &[u8]) -> Option<(String, String)> {
    if node.kind() == "augmented_assignment" {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;
    if left.kind() != "identifier" || right.kind() != "identifier" {
        return None;
    }
    Some((node_text(left, source)?, node_text(right, source)?))
}

fn get_ts_js_assignment(node: Node, source: &[u8]) -> Option<(String, String)> {
    let name = node.child_by_field_name("name")?;
    let value = node.child_by_field_name("value")?;
    if name.kind() != "identifier" || value.kind() != "identifier" {
        return None;
    }
    Some((node_text(name, source)?, node_text(value, source)?))
}

fn get_java_assignment(node: Node, source: &[u8]) -> Option<(String, String)> {
    let mut declarators = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            declarators.push(child);
        }
    }
    if declarators.len() != 1 {
        return None;
    }
    let decl = declarators[0];
    let name = decl.child_by_field_name("name")?;
    let value = decl.child_by_field_name("value")?;
    if name.kind() != "identifier" || value.kind() != "identifier" {
        return None;
    }
    Some((node_text(name, source)?, node_text(value, source)?))
}

fn get_rust_assignment(node: Node, source: &[u8]) -> Option<(String, String)> {
    if node.kind() != "let_declaration" {
        return None;
    }
    let pattern = node.child_by_field_name("pattern")?;
    let value = node.child_by_field_name("value")?;
    if pattern.kind() != "identifier" || value.kind() != "identifier" {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "mutable_specifier" {
            return None;
        }
    }
    if node.child_by_field_name("type").is_some() {
        return None;
    }
    Some((node_text(pattern, source)?, node_text(value, source)?))
}

fn get_go_assignment(node: Node, source: &[u8]) -> Option<(String, String)> {
    if node.kind() == "short_var_declaration" {
        let left = node.child_by_field_name("left")?;
        let right = node.child_by_field_name("right")?;
        let left_idents = collect_ident_children(left);
        let right_idents = collect_ident_children(right);
        if left_idents.len() != 1 || right_idents.len() != 1 {
            return None;
        }
        Some((
            node_text(left_idents[0], source)?,
            node_text(right_idents[0], source)?,
        ))
    } else if node.kind() == "var_declaration" {
        let mut specs = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "var_spec" {
                specs.push(child);
            }
        }
        if specs.len() != 1 {
            return None;
        }
        let spec = specs[0];
        let name_node = spec.child_by_field_name("name")?;
        let value_list = spec.child_by_field_name("value")?;
        if spec.child_by_field_name("type").is_some() {
            return None;
        }
        let val_idents = collect_ident_children(value_list);
        if val_idents.len() != 1 {
            return None;
        }
        if name_node.kind() != "identifier" {
            return None;
        }
        Some((
            node_text(name_node, source)?,
            node_text(val_idents[0], source)?,
        ))
    } else {
        None
    }
}

fn collect_ident_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "identifier")
        .collect()
}

fn node_text(node: Node, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_python(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    fn make_ctx(symbols: &[&str], imports: &[(&str, &str)]) -> FileContext {
        FileContext {
            file_symbols: symbols.iter().map(|s| s.to_string()).collect(),
            import_map: imports
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            type_map: HashMap::new(),
        }
    }

    #[test]
    fn test_resolve_file_symbol() {
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["my_func"], &[]);
        let alias_map = HashMap::new();
        let result = resolver.resolve_tier1("my_func", &ctx, &alias_map);
        assert_eq!(result.confidence, EdgeConfidence::Resolved);
        assert!(result.resolved_path.is_none());
    }

    #[test]
    fn test_resolve_import() {
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&[], &[("requests", "requests")]);
        let alias_map = HashMap::new();
        let result = resolver.resolve_tier1("requests", &ctx, &alias_map);
        assert_eq!(result.confidence, EdgeConfidence::Resolved);
        assert_eq!(result.resolved_path, Some("requests".to_string()));
    }

    #[test]
    fn test_resolve_alias() {
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&[], &[("original", "mod.original")]);
        let mut alias_map = HashMap::new();
        alias_map.insert("alias".to_string(), "original".to_string());
        let result = resolver.resolve_tier1("alias", &ctx, &alias_map);
        assert_eq!(result.confidence, EdgeConfidence::Resolved);
        assert_eq!(result.resolved_path, Some("mod.original".to_string()));
    }

    #[test]
    fn test_resolve_unknown_is_textual() {
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&[], &[]);
        let alias_map = HashMap::new();
        let result = resolver.resolve_tier1("unknown_func", &ctx, &alias_map);
        assert_eq!(result.confidence, EdgeConfidence::Textual);
        assert!(result.resolved_path.is_none());
    }

    #[test]
    fn test_extract_alias_python_simple() {
        let source = "x = original_func\n";
        let tree = parse_python(source);
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["original_func"], &[]);
        let aliases = resolver.extract_aliases(tree.root_node(), source.as_bytes(), "python", &ctx);
        assert_eq!(aliases.get("x"), Some(&"original_func".to_string()));
    }

    #[test]
    fn test_extract_alias_python_skip_multi_assigned() {
        let source = "x = foo\nx = bar\n";
        let tree = parse_python(source);
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["foo", "bar"], &[]);
        let aliases = resolver.extract_aliases(tree.root_node(), source.as_bytes(), "python", &ctx);
        assert!(
            !aliases.contains_key("x"),
            "F7: multiply-assigned variables must be skipped"
        );
    }

    #[test]
    fn test_extract_alias_python_skip_augmented() {
        let source = "x += y\n";
        let tree = parse_python(source);
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["y"], &[]);
        let aliases = resolver.extract_aliases(tree.root_node(), source.as_bytes(), "python", &ctx);
        assert!(aliases.is_empty());
    }

    #[test]
    fn test_extract_alias_python_skip_complex_rhs() {
        let source = "x = func()\n";
        let tree = parse_python(source);
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["func"], &[]);
        let aliases = resolver.extract_aliases(tree.root_node(), source.as_bytes(), "python", &ctx);
        assert!(
            aliases.is_empty(),
            "Only bare identifier RHS should be tracked"
        );
    }

    #[test]
    fn test_extract_alias_python_skip_if_lhs_is_symbol() {
        let source = "existing_sym = other\n";
        let tree = parse_python(source);
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["existing_sym", "other"], &[]);
        let aliases = resolver.extract_aliases(tree.root_node(), source.as_bytes(), "python", &ctx);
        assert!(
            !aliases.contains_key("existing_sym"),
            "LHS that is already a file symbol must not be aliased"
        );
    }

    #[test]
    fn test_resolve_priority_file_symbol_over_import() {
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["name"], &[("name", "external.name")]);
        let alias_map = HashMap::new();
        let result = resolver.resolve_tier1("name", &ctx, &alias_map);
        assert_eq!(result.confidence, EdgeConfidence::Resolved);
        assert!(
            result.resolved_path.is_none(),
            "File symbol match takes priority — no resolved_path"
        );
    }

    #[test]
    fn test_unsupported_language_returns_empty_aliases() {
        let source = "x = y\n";
        let tree = parse_python(source);
        let resolver = ConservativeResolver::new();
        let ctx = make_ctx(&["y"], &[]);
        let aliases = resolver.extract_aliases(tree.root_node(), source.as_bytes(), "csharp", &ctx);
        assert!(aliases.is_empty());
    }
}
