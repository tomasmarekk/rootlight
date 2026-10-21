//! Certify a direct local initializer's read of its own written procedure formal.
//! A complete clean owner census excludes competing binders, formal writes and
//! executable calls. Other expressions and overload selection remain unproved.

use super::*;
use rootlight_adapter_sdk::SinkError;

pub(super) struct Proof {
    sites: Vec<Range<usize>>,
}

impl<'s> CaptureCache<'s> {
    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if !self.name(node, cancel)? {
            return Ok(None);
        }
        Ok(self
            .source
            .get(node.byte_range())
            .and_then(|bytes| std::str::from_utf8(bytes).ok()))
    }

    fn same_word(
        &mut self,
        left: &str,
        right: &str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.visit(cancel)? || left.len() != right.len() {
            return Ok(false);
        }
        for (left, right) in left.as_bytes().chunks(64).zip(right.as_bytes().chunks(64)) {
            if !self.visit(cancel)? || left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn has_word(
        &mut self,
        values: &[&str],
        word: &str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for value in values {
            if self.same_word(value, word, cancel)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn children<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'t>>, AdapterError> {
        let mut children = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? || !self.clean(child) {
                return Ok(Vec::new());
            }
            if matches!(child.kind(), "inline_comment" | "block_comment") {
                continue;
            }
            children
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            children.push(child);
        }
        Ok(children)
    }

    fn direct_identifier<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let children = self.children(node, cancel)?;
        Ok(match children.as_slice() {
            [name] if name.kind() == "identifier" && self.name(*name, cancel)? => Some(*name),
            _ => None,
        })
    }

    fn local_name<'t>(
        &mut self,
        declaration: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let children = self.children(declaration, cancel)?;
        let mut found = None;
        for child in children {
            match child.kind() {
                "identifier_list" if found.is_none() => {
                    found = self.direct_identifier(child, cancel)?
                }
                "type_part" | "initialization_part" => {}
                _ => return Ok(None),
            }
        }
        Ok(found)
    }

    fn primitive_type_call(
        &mut self,
        call: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(function) = self.field(call, "function", cancel)? else {
            return Ok(false);
        };
        let Some(arguments) = self.field(call, "arguments", cancel)? else {
            return Ok(false);
        };
        if function.kind() != "primitive_type" || arguments.kind() != "argument_list" {
            return Ok(false);
        }
        let Some(parent) = call.parent() else {
            return Ok(false);
        };
        if parent.kind() != "type_expression" {
            return Ok(false);
        }
        let children = self.children(arguments, cancel)?;
        let [literal] = children.as_slice() else {
            return Ok(false);
        };
        let children = self.children(*literal, cancel)?;
        Ok(literal.kind() == "literal_expression"
            && matches!(children.as_slice(), [integer] if integer.kind() == "integer_literal"))
    }

    fn initializer_proof(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Proof>, AdapterError> {
        if !self.complete
            || !self.clean(owner)
            || owner.kind() != "procedure_declaration"
            || !self.source_owned(owner, cancel)?
        {
            return Ok(None);
        }
        let Some((_, body)) = self.header(owner, cancel)? else {
            return Ok(None);
        };
        let Some(parameters) = self.field(owner, "parameters", cancel)? else {
            return Ok(None);
        };
        let mut formals = Vec::new();
        for formal in self.children(parameters, cancel)? {
            if formal.kind() != "formal_parameter" {
                return Ok(None);
            }
            let Some(name) = self.field(formal, "name", cancel)? else {
                return Ok(None);
            };
            let Some(ty) = self.field(formal, "type", cancel)? else {
                return Ok(None);
            };
            if ty.kind() != "type_expression" || self.children(formal, cancel)?.len() != 2 {
                return Ok(None);
            }
            let Some(name) = self.word(name, cancel)? else {
                return Ok(None);
            };
            if self.has_word(&formals, name, cancel)? {
                return Ok(None);
            }
            formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            formals.push(name);
        }
        if formals.is_empty() {
            return Ok(None);
        }
        let mut locals = Vec::<(&str, usize)>::new();
        let mut sites = Vec::new();
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? || !self.clean(node) {
                return Ok(None);
            }
            let opaque = matches!(
                node.kind(),
                "inline_comment" | "block_comment" | "string_literal"
            );
            if opaque {
                let Some(bytes) = self.source.get(node.byte_range()) else {
                    return Ok(None);
                };
                for _ in bytes.chunks(64) {
                    if !self.visit(cancel)? {
                        return Ok(None);
                    }
                }
            } else if node.is_named() {
                match node.kind() {
                    "block_statement" if node == body => {}
                    "statements"
                    | "type_part"
                    | "type_expression"
                    | "initialization_part"
                    | "variable_expression"
                    | "identifier"
                    | "identifier_list"
                    | "variable_declaration_list"
                    | "variable_declaration_statement"
                    | "assignment_operator"
                    | "lvalue_expression"
                    | "binary_expression"
                    | "literal_expression"
                    | "integer_literal"
                    | "real_literal"
                    | "bool_literal"
                    | "primitive_type"
                    | "argument_list"
                    | "return_statement" => {}
                    "variable_kind" => {
                        if !matches!(self.source.get(node.byte_range()), Some(b"var" | b"const")) {
                            return Ok(None);
                        }
                    }
                    "variable_declaration" => {
                        let Some(identifier) = self.local_name(node, cancel)? else {
                            return Ok(None);
                        };
                        let Some(name) = self.word(identifier, cancel)? else {
                            return Ok(None);
                        };
                        if self.has_word(&formals, name, cancel)? {
                            return Ok(None);
                        }
                        for (other, _) in &locals {
                            if self.same_word(other, name, cancel)? {
                                return Ok(None);
                            }
                        }
                        locals
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        locals.push((name, node.start_byte()));
                        for child in self.children(node, cancel)? {
                            if child.kind() != "initialization_part" {
                                continue;
                            }
                            let values = self.children(child, cancel)?;
                            let [value] = values.as_slice() else {
                                return Ok(None);
                            };
                            if value.kind() == "variable_expression"
                                && let Some(identifier) = self.direct_identifier(*value, cancel)?
                                && let Some(name) = self.word(identifier, cancel)?
                                && self.has_word(&formals, name, cancel)?
                            {
                                sites
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                sites.push(identifier.byte_range());
                            }
                        }
                    }
                    "assignment_statement" => {
                        let children = self.children(node, cancel)?;
                        let Some(lvalue) = children
                            .first()
                            .filter(|child| child.kind() == "lvalue_expression")
                        else {
                            return Ok(None);
                        };
                        let children = self.children(*lvalue, cancel)?;
                        let [value] = children.as_slice() else {
                            return Ok(None);
                        };
                        if value.kind() != "variable_expression" {
                            return Ok(None);
                        }
                        let Some(identifier) = self.direct_identifier(*value, cancel)? else {
                            return Ok(None);
                        };
                        let Some(name) = self.word(identifier, cancel)? else {
                            return Ok(None);
                        };
                        let mut local = false;
                        for (other, start) in &locals {
                            if self.same_word(other, name, cancel)? && *start < node.start_byte() {
                                local = true;
                            }
                        }
                        if !local || self.has_word(&formals, name, cancel)? {
                            return Ok(None);
                        }
                    }
                    "call_expression" if self.primitive_type_call(node, cancel)? => {}
                    _ => return Ok(None),
                }
            }
            if !opaque && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok((!self.exhausted && !sites.is_empty()).then_some(Proof { sites }));
                }
            }
        }
    }

    fn ensure_initializer_proof(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.initializer_proofs.contains_key(&owner.id()) {
            let proof = self.initializer_proof(owner, cancel)?;
            self.initializer_proofs
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.initializer_proofs.insert(owner.id(), proof);
        }
        Ok(!self.exhausted
            && self
                .initializer_proofs
                .get(&owner.id())
                .is_some_and(Option::is_some))
    }

    pub(super) fn initializer_site(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "identifier" {
            return Ok(false);
        }
        let mut current = node;
        while let Some(parent) = current.parent() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if parent.kind() == "procedure_declaration" {
                if !self.ensure_initializer_proof(parent, cancel)? {
                    return Ok(false);
                }
                let sites = self
                    .initializer_proofs
                    .get(&parent.id())
                    .and_then(Option::as_ref)
                    .map(|proof| proof.sites.len())
                    .unwrap_or(0);
                for index in 0..sites {
                    if !self.visit(cancel)? {
                        return Ok(false);
                    }
                    if self
                        .initializer_proofs
                        .get(&parent.id())
                        .and_then(Option::as_ref)
                        .and_then(|proof| proof.sites.get(index))
                        == Some(&node.byte_range())
                    {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            if matches!(parent.kind(), "module_declaration" | "source_file") {
                return Ok(false);
            }
            current = parent;
        }
        Ok(false)
    }

    pub(super) fn initializer_context(
        &mut self,
        body: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Range<usize>>, AdapterError> {
        let Some(owner) = body
            .parent()
            .filter(|node| node.kind() == "procedure_declaration")
        else {
            return Ok(None);
        };
        if body.kind() != "block_statement"
            || self.field(owner, "body", cancel)? != Some(body)
            || !self.ensure_initializer_proof(owner, cancel)?
        {
            return Ok(None);
        }
        Ok(Some(owner.byte_range()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_chapel::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn selected(source: &[u8], complete: bool, maximum: usize) -> (Vec<Range<usize>>, bool) {
        let tree = tree(source);
        assert!(
            !tree.root_node().has_error(),
            "native prerequisite: {}",
            tree.root_node().to_sexp()
        );
        let mut cache = CaptureCache::new(source, maximum, complete);
        let cancel = Cancellation::new();
        let mut result = Vec::new();
        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            if node.kind() == "identifier" && cache.initializer_site(node, &cancel).unwrap() {
                result.push(node.byte_range());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return (result, cache.exhausted());
                }
            }
        }
    }

    #[test]
    fn original_initializer_is_the_only_certified_formal_value_site() {
        let source = include_bytes!("../../../tests/fixtures/chapel/genhashkey32.chpl");
        assert_eq!(selected(source, true, 65536).0, vec![320..321]);
        let source = b"module Written { proc measure(value: int) { var saved: int = value; saved += 1; return saved; } }";
        let start = std::str::from_utf8(source)
            .unwrap()
            .find("= value")
            .unwrap()
            + 2;
        assert_eq!(selected(source, true, 65536).0, vec![start..start + 5]);
    }

    #[test]
    fn complete_owner_census_rejects_competitors_writes_calls_and_nested_scopes() {
        for source in [
            "module Written { proc measure(value: int, value: int) { var saved = value; } }",
            "module Written { proc measure(value: int) { var value = 1; var saved = value; } }",
            "module Written { proc measure(value: int) { var saved = value; value = 2; } }",
            "module Written { proc measure(value: int) { var saved = value; external(); } }",
            "module Written { proc measure(value: int) { var saved = value; { var other = 1; } } }",
            "module Written { proc measure(value: int) { var saved = value; proc nested() {} } }",
            "module Written { proc measure(value: int) { ref saved = value; } }",
            "module Written { proc measure(value: int) { var saved = value; begin; } }",
        ] {
            assert!(
                selected(source.as_bytes(), true, 65536).0.is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn partial_owner_work_and_cancellation_never_leave_a_cached_read_proof() {
        let source = b"module Written { proc measure(value: int) { var saved = value; } }";
        let start = std::str::from_utf8(source)
            .unwrap()
            .find("= value")
            .unwrap()
            + 2;
        assert_eq!(selected(source, true, 65536).0, vec![start..start + 5]);
        assert!(selected(source, false, 65536).0.is_empty());
        let bounded = selected(source, true, 0);
        assert!(bounded.0.is_empty());
        assert!(bounded.1);
        let tree = tree(source);
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source, 65536, true);
        let mut cursor = tree.walk();
        while cursor.node().kind() != "identifier" {
            assert!(cursor.goto_first_child() || cursor.goto_next_sibling());
        }
        assert!(matches!(
            cache.initializer_site(cursor.node(), &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
