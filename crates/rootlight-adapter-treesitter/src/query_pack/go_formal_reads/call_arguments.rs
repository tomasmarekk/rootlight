//! Certify direct call arguments only when no body scope rebinds a formal.
//! The complete clean free-function census rejects closures and unmodeled
//! binders; callee identity, types, values and execution remain unavailable.

use super::*;

impl CaptureCache<'_> {
    fn inspect_arguments(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<HashSet<(usize, usize)>>, AdapterError> {
        if !self.complete
            || !self.visit(cancel)?
            || function.kind() != "function_declaration"
            || function.has_error()
            || function.is_missing()
            || function.child_by_field_name("type_parameters").is_some()
            || function
                .child_by_field_name("result")
                .is_some_and(|n| n.kind() == "parameter_list")
        {
            return Ok(None);
        }
        let Some(root) = function.parent().filter(|n| n.kind() == "source_file") else {
            return Ok(None);
        };
        if root.has_error()
            || root.end_byte() != self.source.len()
            || !(root.start_byte() == 0
                || root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf"))
        {
            return Ok(None);
        }
        let Some(function_name) = function.child_by_field_name("name") else {
            return Ok(None);
        };
        let Some(function_name) = self.name(function_name, cancel)? else {
            return Ok(None);
        };
        let mut same_name = 0_usize;
        let mut cursor = root.walk();
        for sibling in root.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if sibling.kind() == "function_declaration"
                && let Some(name) = sibling.child_by_field_name("name")
                && self.name(name, cancel)? == Some(function_name)
            {
                same_name += 1;
            }
        }
        if same_name != 1 {
            return Ok(None);
        }
        let Some(parameters) = function.child_by_field_name("parameters") else {
            return Ok(None);
        };
        let mut names = HashSet::new();
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if parameter.kind() == "comment" {
                continue;
            }
            if !matches!(
                parameter.kind(),
                "parameter_declaration" | "variadic_parameter_declaration"
            ) {
                return Ok(None);
            }
            let mut fields = parameter.walk();
            let mut count = 0_usize;
            for field in parameter.children_by_field_name("name", &mut fields) {
                let Some(name) = self.name(field, cancel)? else {
                    return Ok(None);
                };
                names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                if !names.insert(name) {
                    return Ok(None);
                }
                count += 1;
            }
            if count == 0 {
                return Ok(None);
            }
        }
        let Some(body) = function
            .child_by_field_name("body")
            .filter(|n| n.kind() == "block")
        else {
            return Ok(None);
        };
        let Some(statements) = self
            .only(body, cancel)?
            .filter(|n| n.kind() == "statement_list")
        else {
            return Ok(None);
        };
        // Keep the existing returned-composite proof and its limitations separate.
        if self
            .only(statements, cancel)?
            .is_some_and(|n| n.kind() == "return_statement")
        {
            return Ok(None);
        }
        let mut proof = HashSet::new();
        let mut cursor = body.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let node = cursor.node();
            if node.is_missing()
                || node.is_error()
                || matches!(
                    node.kind(),
                    "func_literal"
                        | "function_declaration"
                        | "method_declaration"
                        | "composite_literal"
                        | "var_declaration"
                        | "const_declaration"
                        | "type_declaration"
                        | "type_switch_statement"
                        | "expression_switch_statement"
                        | "select_statement"
                        | "receive_statement"
                        | "labeled_statement"
                        | "goto_statement"
                )
            {
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "short_var_declaration" | "assignment_statement" | "range_clause"
            ) && let Some(left) = node.child_by_field_name("left")
            {
                if left.kind() != "expression_list" {
                    return Ok(None);
                }
                let mut targets = left.walk();
                for target in left.named_children(&mut targets) {
                    if !self.visit(cancel)? {
                        return Ok(None);
                    }
                    if target.kind() == "comment" {
                        continue;
                    }
                    if target.kind() != "identifier" {
                        return Ok(None);
                    }
                    if self
                        .source
                        .get(target.byte_range())
                        .is_some_and(|raw| raw == b"_")
                    {
                        continue;
                    }
                    let Some(name) = self.name(target, cancel)? else {
                        return Ok(None);
                    };
                    if names.contains(name) {
                        return Ok(None);
                    }
                }
            }
            if matches!(node.kind(), "inc_statement" | "dec_statement") {
                let Some(target) = self.only(node, cancel)? else {
                    return Ok(None);
                };
                let Some(name) = self.name(target, cancel)? else {
                    return Ok(None);
                };
                if names.contains(name) {
                    return Ok(None);
                }
            }
            if node.kind() == "identifier"
                && node.parent().is_some_and(|n| n.kind() == "argument_list")
            {
                let Some(name) = self.name(node, cancel)? else {
                    return Ok(None);
                };
                if names.contains(name) {
                    let Some(call) = node
                        .parent()
                        .and_then(|n| n.parent())
                        .filter(|n| n.kind() == "call_expression")
                    else {
                        return Ok(None);
                    };
                    let Some(head) = call.child_by_field_name("function") else {
                        return Ok(None);
                    };
                    let head = if head.kind() == "selector_expression" {
                        let Some(operand) = head.child_by_field_name("operand") else {
                            return Ok(None);
                        };
                        operand
                    } else {
                        head
                    };
                    let Some(head_name) = self.name(head, cancel)? else {
                        return Ok(None);
                    };
                    if names.contains(head_name) {
                        return Ok(None);
                    }
                    proof
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    proof.insert((node.start_byte(), node.end_byte()));
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(Some(proof));
                }
            }
        }
    }

    fn ensure_arguments(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if !self.arguments.contains_key(&function.id()) {
            let proof = self.inspect_arguments(function, cancel)?;
            self.arguments
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.arguments.insert(function.id(), proof);
        }
        Ok(())
    }

    pub(super) fn refine_arguments(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "function_declaration" {
            self.ensure_arguments(node, cancel)?;
            if self
                .arguments
                .get(&node.id())
                .and_then(Option::as_ref)
                .is_some_and(|set| !set.is_empty())
            {
                candidate.syntax = "go.call_formal_context";
                candidate.required = true;
                return Ok(Some(true));
            }
            return Ok(None);
        }
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature)
            || node.kind() != "identifier"
        {
            return Ok(None);
        }
        if node.parent().is_none_or(|n| n.kind() != "argument_list") {
            return Ok((role == StructuralRole::Signature).then_some(false));
        }
        let mut current = node;
        let mut owner = None;
        while let Some(parent) = current.parent() {
            if !self.visit(cancel)? {
                return Ok(Some(false));
            }
            if parent.kind() == "composite_literal" {
                return Ok((role == StructuralRole::Signature).then_some(false));
            }
            if parent.kind() == "func_literal" {
                break;
            }
            if matches!(parent.kind(), "function_declaration" | "method_declaration") {
                owner = Some(parent);
                break;
            }
            current = parent;
        }
        if let Some(function) = owner {
            self.ensure_arguments(function, cancel)?;
            if self
                .arguments
                .get(&function.id())
                .and_then(Option::as_ref)
                .is_some_and(|set| set.contains(&(node.start_byte(), node.end_byte())))
            {
                candidate.role = StructuralRole::Reference;
                candidate.syntax = "go.call_formal";
                candidate.required = true;
                return Ok(Some(true));
            }
        }
        if role == StructuralRole::Signature {
            return Ok(Some(false));
        }
        candidate.syntax = "go.unproven_call_formal";
        Ok(Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_argument_census_uses_both_actual_query_roles_and_same_work_guard() {
        let source = b"package sample\nfunc owned(value int) { external(value) }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let start = source
            .windows(5)
            .rposition(|text| text == b"value")
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let mut candidate = QueryCandidate {
                start,
                end: start + 5,
                role,
                syntax: "go.identifier",
                required: false,
                native_depth: 0,
            };
            let mut cache = CaptureCache::new(source, 4096, true);
            assert!(
                cache
                    .refine(node, role, &mut candidate, &Cancellation::new())
                    .unwrap()
            );
            assert_eq!(candidate.syntax, "go.call_formal");
            assert_eq!(candidate.role, StructuralRole::Reference);
            assert!(candidate.required);
        }
        let mut candidate = QueryCandidate {
            start,
            end: start + 5,
            role: StructuralRole::Reference,
            syntax: "go.identifier",
            required: false,
            native_depth: 0,
        };
        let mut partial = CaptureCache::new(source, 4096, false);
        partial
            .refine(
                node,
                StructuralRole::Reference,
                &mut candidate,
                &Cancellation::new(),
            )
            .unwrap();
        assert_ne!(candidate.syntax, "go.call_formal");
        let mut bounded = CaptureCache::new(source, 0, true);
        bounded
            .refine(
                node,
                StructuralRole::Reference,
                &mut candidate,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(bounded.exhausted());
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            partial.refine(node, StructuralRole::Reference, &mut candidate, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
