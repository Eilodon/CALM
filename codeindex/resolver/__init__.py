from .lang_constants import ASSIGNMENT_NODES

class ConservativeResolver:
    def __init__(self):
        self.ASSIGNMENT_NODES = ASSIGNMENT_NODES

    def _walk(self, node, node_types):
        """Dummy generator for tree-sitter AST nodes."""
        pass

    def _extract_aliases(
        self,
        ast_root,
        file_symbols: set[str],
        import_map: dict[str, str],
        type_map: dict[str, str],
        language: str,
    ) -> dict[str, str]:
        """
        Returns alias_map: {alias_name → resolved_symbol_name}
        Conservative contract:
          - Chỉ track x = y với y là bare identifier đã resolvable
          - [F7] Skip variables được assign nhiều lần (ambiguous aliasing)
          - Unknown beats mis-classified — bất kỳ case phức tạp nào → skip
        """
        # Pre-pass: detect multiply-assigned LHS
        lhs_seen: set[str] = set()
        multi_assigned: set[str] = set()
        for node in self._walk(ast_root, self.ASSIGNMENT_NODES):
            lhs, _ = self._get_assignment_lhs_rhs(node, language)
            if lhs:
                if lhs in lhs_seen:
                    multi_assigned.add(lhs)
                lhs_seen.add(lhs)

        # Main pass: build alias_map
        alias_map: dict[str, str] = {}
        for node in self._walk(ast_root, self.ASSIGNMENT_NODES):
            lhs, rhs = self._get_assignment_lhs_rhs(node, language)

            if (lhs
                    and rhs
                    and lhs not in multi_assigned      # [F7] skip if re-assigned anywhere
                    and lhs not in file_symbols
                    and lhs not in import_map
                    and lhs not in type_map
                    and (rhs in file_symbols or rhs in import_map)):
                alias_map[lhs] = rhs

        return alias_map

    def _get_rust_assignment_lhs_rhs(self, node) -> tuple[str | None, str | None]:
        """
        Handle: let x = y;  (tree-sitter: let_declaration)
        Safe case ONLY: pattern is bare identifier, value is bare identifier.
        Skip: let mut x = ..., let (a, b) = ..., if let Some(x) = ...,
              let x: Type = ..., let x = func(), let x = obj.method()
        """
        if node.type != "let_declaration":
            return None, None
        pattern = node.child_by_field_name("pattern")
        value = node.child_by_field_name("value")
        if not pattern or not value:
            return None, None
        if pattern.type != "identifier" or value.type != "identifier":
            return None, None
        # mutable_specifier is not a named field — must iterate children
        if any(child.type == "mutable_specifier" for child in node.children):
            return None, None
        # type annotation IS a named field in tree-sitter-rust
        if node.child_by_field_name("type") is not None:
            return None, None
        return pattern.text.decode("utf-8"), value.text.decode("utf-8")

    def _get_go_assignment_lhs_rhs(self, node) -> tuple[str | None, str | None]:
        """
        Handle: x := y  (tree-sitter: short_var_declaration)
        Handle: var x = y  (tree-sitter: var_declaration → var_spec)
        Safe case ONLY: single LHS identifier, single RHS bare identifier.
        Skip: x, y := a, b  (multi-assign — F7 guard catches this too)
        """
        if node.type == "short_var_declaration":
            left = node.child_by_field_name("left")
            right = node.child_by_field_name("right")
            if not left or not right:
                return None, None
            # Explicitly filter for identifier nodes — robust against tree-sitter-go
            # changes to how multi-assign (x, y := a, b) is represented. A multi-assign
            # left-side may be an expression_list containing multiple identifiers; keeping
            # only identifier-type children and requiring exactly 1 rejects all such cases.
            left_idents = [c for c in left.children if c.type == "identifier"]
            right_idents = [c for c in right.children if c.type == "identifier"]
            if len(left_idents) != 1 or len(right_idents) != 1:
                return None, None
            return (left_idents[0].text.decode("utf-8"),
                    right_idents[0].text.decode("utf-8"))

        elif node.type == "var_declaration":
            # var x = y  →  var_declaration → var_spec (name, value)
            specs = [c for c in node.children if c.type == "var_spec"]
            if len(specs) != 1:
                return None, None
            spec = specs[0]
            name_node = spec.child_by_field_name("name")
            value_list = spec.child_by_field_name("value")
            if not name_node or not value_list:
                return None, None
            if spec.child_by_field_name("type") is not None:
                return None, None
            val_idents = [c for c in value_list.children if c.type == "identifier"]
            if len(val_idents) != 1:
                return None, None
            if name_node.type != "identifier":
                return None, None
            return (name_node.text.decode("utf-8"),
                    val_idents[0].text.decode("utf-8"))

        return None, None

    def _get_assignment_lhs_rhs(self, node, language: str) -> tuple[str | None, str | None]:
        if language in ("python",):
            return self._get_python_assignment_lhs_rhs(node)
        elif language in ("typescript", "javascript"):
            return self._get_ts_js_assignment_lhs_rhs(node)
        elif language == "java":
            return self._get_java_assignment_lhs_rhs(node)
        elif language == "rust":
            return self._get_rust_assignment_lhs_rhs(node)
        elif language == "go":
            return self._get_go_assignment_lhs_rhs(node)
        return None, None

    def _get_python_assignment_lhs_rhs(self, node) -> tuple[str | None, str | None]:
        """
        Handle: x = y  (tree-sitter: assignment)
        Skip: augmented_assignment (x += y), tuple/subscript/attribute LHS,
              chained assignment (a = b = c — right would be 'assignment' node),
              annotated assignment handled by separate 'annotated_assignment' node type.
        Safe case ONLY: bare identifier on both sides.
        """
        if node.type == "augmented_assignment":
            return None, None
        left = node.child_by_field_name("left")
        right = node.child_by_field_name("right")
        if not left or not right:
            return None, None
        if left.type != "identifier" or right.type != "identifier":
            return None, None
        return left.text.decode("utf-8"), right.text.decode("utf-8")

    def _get_ts_js_assignment_lhs_rhs(self, node) -> tuple[str | None, str | None]:
        """
        Handle: const x = y / let x = y / var x = y
                (tree-sitter: variable_declarator, child of lexical_declaration/variable_declaration)
        Skip: destructuring (array_pattern, object_pattern LHS), no initializer,
              RHS that is not a bare identifier.
        Safe case ONLY: name field is 'identifier', value field is 'identifier'.
        Note: type annotations (const x: T = y) are tolerated — the value check is
        the conservative gate, not the type annotation presence.
        """
        # node.type == "variable_declarator"
        name = node.child_by_field_name("name")
        value = node.child_by_field_name("value")
        if not name or not value:
            return None, None
        if name.type != "identifier" or value.type != "identifier":
            return None, None
        return name.text.decode("utf-8"), value.text.decode("utf-8")

    def _get_java_assignment_lhs_rhs(self, node) -> tuple[str | None, str | None]:
        """
        Handle: SomeType x = y  (tree-sitter: local_variable_declaration → variable_declarator)
        Skip: multiple declarators in one statement (int a = b, c = d),
              array/generic types (they don't affect alias detection but we stay conservative),
              no initializer, RHS not a bare identifier.
        Safe case ONLY: exactly one declarator, name is 'identifier', value is 'identifier'.
        """
        # node.type == "local_variable_declaration"
        declarators = [c for c in node.children if c.type == "variable_declarator"]
        if len(declarators) != 1:
            return None, None
        decl = declarators[0]
        name = decl.child_by_field_name("name")
        value = decl.child_by_field_name("value")
        if not name or not value:
            return None, None
        if name.type != "identifier" or value.type != "identifier":
            return None, None
        return name.text.decode("utf-8"), value.text.decode("utf-8")

    def _resolve_tier1(self, callee_name: str, file_symbols: set[str], import_map: dict[str, str], alias_map: dict[str, str]) -> tuple[str, str | None]:
        if callee_name in file_symbols:
            confidence = "resolved"
            resolved_path = None
        elif callee_name in import_map:
            confidence = "resolved"
            resolved_path = import_map[callee_name]
        elif callee_name in alias_map:
            confidence = "resolved"
            alias_target = alias_map[callee_name]
            resolved_path = import_map.get(alias_target)
        else:
            confidence = "textual"
            resolved_path = None
        return confidence, resolved_path

