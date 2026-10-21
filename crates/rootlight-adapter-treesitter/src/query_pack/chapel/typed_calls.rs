//! Native proof for a bare call through an explicitly typed local unsigned scalar.
//! Only a unique exact-width same-module overload is selected. This proves no
//! value, external symbol, generic instantiation, or runtime implementation.

use super::*;
use rootlight_adapter_sdk::SinkError;

#[derive(Clone)]
pub(super) struct Proof {
    pub(super) owner: Range<usize>,
    pub(super) call: Range<usize>,
}

impl CaptureCache<'_> {
    fn typed_width(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<u8>, AdapterError> {
        if !self.clean(node) || node.kind() != "type_expression" {
            return Ok(None);
        }
        let children = self.children(node, cancel)?;
        let [call] = children.as_slice() else {
            return Ok(None);
        };
        if call.kind() != "call_expression" {
            return Ok(None);
        }
        let Some(function) = self.field(*call, "function", cancel)? else {
            return Ok(None);
        };
        let Some(arguments) = self.field(*call, "arguments", cancel)? else {
            return Ok(None);
        };
        if function.kind() != "primitive_type"
            || self.source.get(function.byte_range()) != Some(b"uint")
        {
            return Ok(None);
        }
        let arguments = self.children(arguments, cancel)?;
        let [literal] = arguments.as_slice() else {
            return Ok(None);
        };
        let values = self.children(*literal, cancel)?;
        let [integer] = values.as_slice() else {
            return Ok(None);
        };
        if integer.kind() != "integer_literal" {
            return Ok(None);
        }
        Ok(match self.source.get(integer.byte_range()) {
            Some(b"8") => Some(8),
            Some(b"16") => Some(16),
            Some(b"32") => Some(32),
            Some(b"64") => Some(64),
            _ => None,
        })
    }

    fn scalar_formal(
        &mut self,
        procedure: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Option<u8>>, AdapterError> {
        let Some(parameters) = self.field(procedure, "parameters", cancel)? else {
            return Ok(None);
        };
        let parameters = self.children(parameters, cancel)?;
        let [formal] = parameters.as_slice() else {
            return Ok(None);
        };
        if formal.kind() != "formal_parameter" {
            return Ok(None);
        }
        let Some(name) = self.field(*formal, "name", cancel)? else {
            return Ok(None);
        };
        let Some(ty) = self.field(*formal, "type", cancel)? else {
            return Ok(None);
        };
        if !self.name(name, cancel)? || self.children(*formal, cancel)?.len() != 2 {
            return Ok(None);
        }
        if let Some(width) = self.typed_width(ty, cancel)? {
            return Ok(Some(Some(width)));
        }
        // These two built-in string families cannot be the exact unsigned type.
        // Other named types might be aliases and therefore invalidate the group.
        let children = self.children(ty, cancel)?;
        let [value] = children.as_slice() else {
            return Ok(None);
        };
        let bytes = self.source.get(value.byte_range());
        Ok(matches!(
            (value.kind(), bytes),
            ("primitive_type", Some(b"string")) | ("variable_expression", Some(b"c_string"))
        )
        .then_some(None))
    }

    fn direct_statement(
        &mut self,
        node: Node<'_>,
        body: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut selected = node;
        while let Some(parent) = selected.parent() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if parent == body {
                return Ok(true);
            }
            if parent.kind() != "statements" {
                return Ok(false);
            }
            selected = parent;
        }
        Ok(false)
    }

    fn typed_call_proof(
        &mut self,
        call: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Proof>, AdapterError> {
        if !self.complete || !self.clean(call) || call.kind() != "call_expression" {
            return Ok(None);
        }
        let Some(function) = self.field(call, "function", cancel)? else {
            return Ok(None);
        };
        if function.kind() != "variable_expression" {
            return Ok(None);
        }
        let Some(callee) = self.direct_identifier(function, cancel)? else {
            return Ok(None);
        };
        let Some(arguments) = self.field(call, "arguments", cancel)? else {
            return Ok(None);
        };
        let arguments = self.children(arguments, cancel)?;
        let [argument] = arguments.as_slice() else {
            return Ok(None);
        };
        if argument.kind() != "variable_expression" {
            return Ok(None);
        }
        let Some(argument) = self.direct_identifier(*argument, cancel)? else {
            return Ok(None);
        };
        let Some(callee_text) = self.word(callee, cancel)? else {
            return Ok(None);
        };
        let Some(argument_text) = self.word(argument, cancel)? else {
            return Ok(None);
        };
        if self.same_word(callee_text, argument_text, cancel)? {
            return Ok(None);
        }
        let Some(statement) = call
            .parent()
            .filter(|n| matches!(n.kind(), "return_statement" | "expression_statement"))
        else {
            return Ok(None);
        };
        if self.children(statement, cancel)?.as_slice() != [call] {
            return Ok(None);
        }
        let mut body = statement;
        while let Some(parent) = body.parent() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            body = parent;
            if body.kind() == "block_statement" {
                break;
            }
        }
        let Some(owner) = body
            .parent()
            .filter(|n| n.kind() == "procedure_declaration")
        else {
            return Ok(None);
        };
        if !self.clean(owner)
            || !self.source_owned(owner, cancel)?
            || !self.direct_statement(statement, body, cancel)?
        {
            return Ok(None);
        }
        let Some((_, checked_body)) = self.header(owner, cancel)? else {
            return Ok(None);
        };
        if checked_body != body {
            return Ok(None);
        }
        let Some(parameters) = self.field(owner, "parameters", cancel)? else {
            return Ok(None);
        };
        for formal in self.children(parameters, cancel)? {
            let Some(name) = self.field(formal, "name", cancel)? else {
                return Ok(None);
            };
            let Some(name) = self.word(name, cancel)? else {
                return Ok(None);
            };
            if self.same_word(name, argument_text, cancel)?
                || self.same_word(name, callee_text, cancel)?
            {
                return Ok(None);
            }
        }
        let mut declaration = None;
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? || !self.clean(node) {
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "procedure_declaration" | "unsupported_keyword_statement"
            ) {
                return Ok(None);
            }
            if node.kind() == "for_statement" {
                let Some(index) = self.field(node, "index", cancel)? else {
                    return Ok(None);
                };
                let Some(name) = self.word(index, cancel)? else {
                    return Ok(None);
                };
                if self.same_word(name, argument_text, cancel)?
                    || self.same_word(name, callee_text, cancel)?
                {
                    return Ok(None);
                }
            }
            if node.kind() == "variable_declaration" {
                let Some(name) = self.local_name(node, cancel)? else {
                    return Ok(None);
                };
                let Some(text) = self.word(name, cancel)? else {
                    return Ok(None);
                };
                if self.same_word(text, callee_text, cancel)? {
                    return Ok(None);
                }
                if self.same_word(text, argument_text, cancel)? {
                    if declaration.replace(node).is_some() {
                        return Ok(None);
                    }
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
                    break;
                }
            }
            if cursor.node() == body {
                break;
            }
        }
        let Some(local) = declaration else {
            return Ok(None);
        };
        let Some(list) = local
            .parent()
            .filter(|n| n.kind() == "variable_declaration_list")
        else {
            return Ok(None);
        };
        if self.children(list, cancel)?.as_slice() != [local] {
            return Ok(None);
        }
        let Some(local_statement) = list
            .parent()
            .filter(|n| n.kind() == "variable_declaration_statement")
        else {
            return Ok(None);
        };
        if !self.direct_statement(local_statement, body, cancel)?
            || local.end_byte() >= callee.start_byte()
        {
            return Ok(None);
        }
        let children = self.children(local_statement, cancel)?;
        let [kind, checked_list] = children.as_slice() else {
            return Ok(None);
        };
        if *checked_list != list
            || !matches!(self.source.get(kind.byte_range()), Some(b"var" | b"const"))
        {
            return Ok(None);
        }
        let children = self.children(local, cancel)?;
        let [_, ty, initial] = children.as_slice() else {
            return Ok(None);
        };
        if ty.kind() != "type_part" || initial.kind() != "initialization_part" {
            return Ok(None);
        }
        let types = self.children(*ty, cancel)?;
        let [ty] = types.as_slice() else {
            return Ok(None);
        };
        let Some(width) = self.typed_width(*ty, cancel)? else {
            return Ok(None);
        };
        let values = self.children(*initial, cancel)?;
        let [value] = values.as_slice() else {
            return Ok(None);
        };
        let integers = self.children(*value, cancel)?;
        let [integer] = integers.as_slice() else {
            return Ok(None);
        };
        if integer.kind() != "integer_literal" {
            return Ok(None);
        }
        let bytes = self.source.get(integer.byte_range()).unwrap_or_default();
        if bytes.is_empty() || bytes.len() > 20 || !bytes.iter().all(u8::is_ascii_digit) {
            return Ok(None);
        }
        let Some(value) = bytes.iter().try_fold(0_u64, |n, digit| {
            n.checked_mul(10)?.checked_add(u64::from(*digit - b'0'))
        }) else {
            return Ok(None);
        };
        if width < 64 && value >= 1_u64 << width {
            return Ok(None);
        }
        let mut module_body = owner;
        while let Some(parent) = module_body.parent() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            module_body = parent;
            if module_body.kind() == "block_statement" {
                break;
            }
        }
        let Some(module) = module_body
            .parent()
            .filter(|n| n.kind() == "module_declaration")
        else {
            return Ok(None);
        };
        if !self.clean(module) || self.header(module, cancel)?.is_none() {
            return Ok(None);
        }
        let mut matches = 0_usize;
        let mut cursor = module_body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? || !self.clean(node) {
                return Ok(None);
            }
            if node.kind() == "unsupported_keyword_statement" {
                return Ok(None);
            }
            let skip = matches!(node.kind(), "procedure_declaration" | "module_declaration");
            if node.kind() == "procedure_declaration" {
                let Some((name, _)) = self.header(node, cancel)? else {
                    return Ok(None);
                };
                let Some(name) = self.word(name, cancel)? else {
                    return Ok(None);
                };
                if self.same_word(name, callee_text, cancel)? {
                    let Some(ty) = self.scalar_formal(node, cancel)? else {
                        return Ok(None);
                    };
                    matches += usize::from(ty == Some(width));
                }
            } else if node.kind() == "variable_declaration" {
                let Some(name) = self.local_name(node, cancel)? else {
                    return Ok(None);
                };
                let Some(name) = self.word(name, cancel)? else {
                    return Ok(None);
                };
                if self.same_word(name, callee_text, cancel)? {
                    return Ok(None);
                }
            }
            if !skip && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node() == module_body {
                break;
            }
        }
        Ok((matches == 1).then(|| Proof {
            owner: owner.byte_range(),
            call: call.byte_range(),
        }))
    }

    pub(super) fn typed_call(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Proof>, AdapterError> {
        if !self.visit(cancel)? {
            return Ok(None);
        }
        let call = if node.kind() == "call_expression" {
            node
        } else {
            let Some(variable) = node.parent().filter(|n| n.kind() == "variable_expression") else {
                return Ok(None);
            };
            let Some(call) = variable.parent().filter(|n| n.kind() == "call_expression") else {
                return Ok(None);
            };
            if self.field(call, "function", cancel)? != Some(variable) {
                return Ok(None);
            }
            call
        };
        if let Some(proof) = self.typed_calls.get(&call.id()) {
            return Ok(proof.clone());
        }
        let proof = self.typed_call_proof(call, cancel)?;
        self.typed_calls
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.typed_calls.insert(call.id(), proof.clone());
        Ok(proof)
    }

    pub(super) fn typed_call_owner(
        &mut self,
        body: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Range<usize>>, AdapterError> {
        let Some(owner) = body
            .parent()
            .filter(|n| n.kind() == "procedure_declaration")
        else {
            return Ok(None);
        };
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if node.kind() == "call_expression"
                && let Some(proof) = self.typed_call(node, cancel)?
                && proof.owner == owner.byte_range()
            {
                return Ok(Some(proof.owner));
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_chapel::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn selected(source: &[u8], complete: bool, maximum: usize) -> (Vec<Range<usize>>, bool) {
        let tree = parse(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut cache = CaptureCache::new(source, maximum, complete);
        let mut result = Vec::new();
        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            if node.kind() == "identifier"
                && cache
                    .typed_call(node, &Cancellation::new())
                    .unwrap()
                    .is_some()
            {
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
    fn complete_original_selects_only_the_explicit_unsigned_local_overload() {
        let source = include_bytes!("../../../tests/fixtures/chapel/genhashkey32.chpl");
        assert_eq!(selected(source, true, 65536).0, vec![637..649]);
        let source = b"module Scalar { proc route(arg: uint(16)) {} proc route(arg: string) {} proc run() { var value: uint(16) = 1; return route(value); } }";
        let at = source
            .windows(b"route(value)".len())
            .position(|s| s == b"route(value)")
            .unwrap();
        assert_eq!(selected(source, true, 65536).0, vec![at..at + 5]);
    }

    #[test]
    fn duplicates_shadowing_unknown_types_nested_and_literal_calls_have_no_certificate() {
        for source in [
            "module W { proc route(arg: uint(32)) {} proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value = 1; return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value: uint(16) = 1; return route(value); } }",
            "module W { proc route(arg) {} proc run() { var value: uint(32) = 1; return route(value); } }",
            "module W { proc route(arg: Alias) {} proc run() { var value: uint(32) = 1; return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; var route = 1; return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; for value in 1..2 {} return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; { var value: uint(32) = 2; } return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run(value: uint(32)) { return route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; return W.route(value); } }",
            "module W { proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; for step in 1..2 { route(value); } } }",
            "module W { proc route(arg: uint(32)) {} proc run() { route(1); } }",
        ] {
            assert!(
                selected(source.as_bytes(), true, 65536).0.is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn partial_work_and_cancelled_cached_requests_never_donate_call_proof() {
        let source = b"module W { proc route(arg: uint(32)) {} proc run() { var value: uint(32) = 1; return route(value); } }";
        assert_eq!(selected(source, true, 65536).0.len(), 1);
        assert!(selected(source, false, 65536).0.is_empty());
        let bounded = selected(source, true, 0);
        assert!(bounded.0.is_empty() && bounded.1);
        let tree = parse(source);
        let mut cache = CaptureCache::new(source, 65536, true);
        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            if node.kind() == "call_expression"
                && cache
                    .typed_call(node, &Cancellation::new())
                    .unwrap()
                    .is_some()
            {
                let stopped = Cancellation::new();
                assert!(stopped.cancel(CancellationReason::ClientRequest));
                assert!(matches!(
                    cache.typed_call(node, &stopped),
                    Err(AdapterError::Cancelled {
                        reason: CancellationReason::ClientRequest
                    })
                ));
                break;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                assert!(cursor.goto_parent());
            }
        }
    }
}
