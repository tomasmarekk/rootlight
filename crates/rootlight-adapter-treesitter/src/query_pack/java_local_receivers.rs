//! Bind a direct Java invocation receiver to an earlier immutable written local.
//! Complete owner and raw-source scans exclude rebinding and lexical escapes;
//! initializer values, imported types and invoked methods remain unevaluated.

use super::{QueryCandidate, StructuralRole};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    direct: Option<bool>,
    methods: HashMap<usize, Option<HashSet<(usize, usize)>>>,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            remaining,
            complete,
            exhausted: false,
            direct: None,
            methods: HashMap::new(),
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(left) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = left;
        Ok(true)
    }
    fn clean(&self, node: Node<'_>) -> bool {
        !node.is_missing()
            && !node.is_error()
            && !node.has_error()
            && node.start_byte() < node.end_byte()
            && node.end_byte() <= self.source.len()
    }
    fn direct_source(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if let Some(direct) = self.direct {
            return Ok(direct);
        }
        if !self.complete {
            self.direct = Some(false);
            return Ok(false);
        }
        // Java translates eligible Unicode escapes before parsing, including in
        // comments. A raw Tree-sitter tree cannot certify their lexical effect.
        for start in (0..self.source.len()).step_by(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            let end = start.saturating_add(65).min(self.source.len());
            if self
                .source
                .get(start..end)
                .is_none_or(|bytes| bytes.windows(2).any(|pair| pair == b"\\u"))
            {
                self.direct = Some(false);
                return Ok(false);
            }
        }
        self.direct = Some(true);
        Ok(true)
    }
    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if !self.visit(cancel)? || !self.clean(node) || node.kind() != "identifier" {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, rest)) = bytes.split_first() else {
            return Ok(None);
        };
        Ok((bytes != b"_"
            && (first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
            && rest
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')))
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten())
    }
    fn field<'t>(
        &mut self,
        node: Node<'t>,
        field: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let index = u32::try_from(index).map_err(|_| SinkError::InvalidIr)?;
            if node.field_name_for_child(index) == Some(field) {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(child);
            }
        }
        Ok(found)
    }
    fn children<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'t>>, AdapterError> {
        let mut values = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? || !self.clean(child) {
                return Ok(Vec::new());
            }
            if matches!(child.kind(), "line_comment" | "block_comment") {
                continue;
            }
            values
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            values.push(child);
        }
        Ok(values)
    }
    fn unique_name(
        &mut self,
        names: &mut Vec<&'s str>,
        name: &'s str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for other in names.iter() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if other.len() != name.len() {
                continue;
            }
            let mut equal = true;
            for (a, b) in other.as_bytes().chunks(64).zip(name.as_bytes().chunks(64)) {
                if !self.visit(cancel)? {
                    return Ok(false);
                }
                equal &= a == b;
            }
            if equal {
                return Ok(false);
            }
        }
        names
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        names.push(name);
        Ok(true)
    }
    fn source_owner(
        &mut self,
        method: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = method;
        while let Some(parent) = current.parent() {
            if !self.visit(cancel)? || parent.is_error() || parent.is_missing() {
                return Ok(false);
            }
            match parent.kind() {
                "program" if parent.parent().is_none() => return Ok(true),
                "class_body" | "interface_body" | "enum_body" => {}
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration" => {
                    let Some(name) = self.field(parent, "name", cancel)? else {
                        return Ok(false);
                    };
                    if self.word(name, cancel)?.is_none()
                        || self.field(parent, "body", cancel)? != Some(current)
                    {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
            current = parent;
        }
        Ok(false)
    }
    fn inspect(
        &mut self,
        method: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<HashSet<(usize, usize)>>, AdapterError> {
        if !self.direct_source(cancel)?
            || !self.clean(method)
            || method.kind() != "method_declaration"
            || method.child_by_field_name("type_parameters").is_some()
            || !self.source_owner(method, cancel)?
        {
            return Ok(None);
        }
        let Some(name) = self.field(method, "name", cancel)? else {
            return Ok(None);
        };
        if self.word(name, cancel)?.is_none() {
            return Ok(None);
        }
        let Some(body) = self
            .field(method, "body", cancel)?
            .filter(|node| node.kind() == "block")
        else {
            return Ok(None);
        };
        let Some(parameters) = self.field(method, "parameters", cancel)? else {
            return Ok(None);
        };
        let mut names = Vec::new();
        for parameter in self.children(parameters, cancel)? {
            if parameter.kind() != "formal_parameter" {
                return Ok(None);
            }
            let Some(name) = self.field(parameter, "name", cancel)? else {
                return Ok(None);
            };
            let Some(name) = self.word(name, cancel)? else {
                return Ok(None);
            };
            if !self.unique_name(&mut names, name, cancel)? {
                return Ok(None);
            }
        }
        let mut locals = HashMap::<&str, usize>::new();
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? || !self.clean(node) {
                return Ok(None);
            }
            let opaque = matches!(
                node.kind(),
                "line_comment" | "block_comment" | "string_literal" | "character_literal"
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
                    "block" if node == body => {}
                    "local_variable_declaration" => {
                        if node.parent() != Some(body) {
                            return Ok(None);
                        }
                        let Some(declarator) = self.field(node, "declarator", cancel)? else {
                            return Ok(None);
                        };
                        if declarator.kind() != "variable_declarator"
                            || self.field(declarator, "value", cancel)?.is_none()
                        {
                            return Ok(None);
                        }
                        let Some(ty) = self.field(node, "type", cancel)? else {
                            return Ok(None);
                        };
                        if !matches!(
                            ty.kind(),
                            "type_identifier"
                                | "integral_type"
                                | "floating_point_type"
                                | "boolean_type"
                        ) {
                            return Ok(None);
                        }
                        for child in self.children(node, cancel)? {
                            if child != declarator
                                && child != ty
                                && !(child.kind() == "modifiers"
                                    && self.source.get(child.byte_range()) == Some(b"final"))
                            {
                                return Ok(None);
                            }
                        }
                        let Some(name) = self.field(declarator, "name", cancel)? else {
                            return Ok(None);
                        };
                        let Some(name) = self.word(name, cancel)? else {
                            return Ok(None);
                        };
                        if !self.unique_name(&mut names, name, cancel)? {
                            return Ok(None);
                        }
                        locals
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        locals.insert(name, node.end_byte());
                    }
                    "variable_declarator"
                    | "modifiers"
                    | "type_identifier"
                    | "integral_type"
                    | "floating_point_type"
                    | "boolean_type"
                    | "identifier"
                    | "method_invocation"
                    | "argument_list"
                    | "expression_statement"
                    | "return_statement"
                    | "array_creation_expression"
                    | "dimensions"
                    | "dimensions_expr"
                    | "array_initializer"
                    | "decimal_integer_literal"
                    | "hex_integer_literal"
                    | "decimal_floating_point_literal"
                    | "true"
                    | "false"
                    | "null_literal"
                    | "field_access"
                    | "this"
                    | "super"
                    | "parenthesized_expression"
                    | "binary_expression" => {}
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
                    break;
                }
            }
            if cursor.node() == body {
                break;
            }
        }
        let mut sites = HashSet::new();
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if node.kind() == "method_invocation"
                && let Some(receiver) = self.field(node, "object", cancel)?
                && let Some(name) = self.word(receiver, cancel)?
                && locals
                    .get(name)
                    .is_some_and(|end| *end <= receiver.start_byte())
            {
                sites
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                sites.insert((receiver.start_byte(), receiver.end_byte()));
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok((!self.exhausted && !sites.is_empty()).then_some(sites));
                }
            }
        }
    }
    fn method<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut current = Some(node);
        while let Some(node) = current {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if node.kind() == "method_declaration" {
                return Ok(Some(node));
            }
            if matches!(node.kind(), "lambda_expression" | "class_body" | "program") {
                return Ok(None);
            }
            current = node.parent();
        }
        Ok(None)
    }
    fn proof(&mut self, method: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.methods.contains_key(&method.id()) {
            let proof = self.inspect(method, cancel)?;
            self.methods
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.methods.insert(method.id(), proof);
        }
        Ok(!self.exhausted && self.methods.get(&method.id()).is_some_and(Option::is_some))
    }
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "block" {
            let Some(method) = node.parent().filter(|parent| {
                parent.kind() == "method_declaration"
                    && parent.child_by_field_name("body") == Some(node)
            }) else {
                return Ok(false);
            };
            if !self.proof(method, cancel)? {
                return Ok(false);
            }
            candidate.start = method.start_byte();
            candidate.end = method.end_byte();
            candidate.syntax = "java.local_receiver_context";
            candidate.required = true;
            return Ok(true);
        }
        let receiver = node.kind() == "identifier"
            && node.parent().is_some_and(|parent| {
                parent.kind() == "method_invocation"
                    && parent.child_by_field_name("object") == Some(node)
            });
        if !receiver || !matches!(role, StructuralRole::Reference | StructuralRole::Signature) {
            return Ok(true);
        }
        let Some(method) = self.method(node, cancel)? else {
            if role == StructuralRole::Reference {
                candidate.syntax = "java.unproven_local_receiver";
            }
            return Ok(role != StructuralRole::Signature);
        };
        let proven = self.proof(method, cancel)?
            && self
                .methods
                .get(&method.id())
                .and_then(Option::as_ref)
                .is_some_and(|sites| sites.contains(&(node.start_byte(), node.end_byte())));
        if role == StructuralRole::Signature {
            if !proven {
                return Ok(false);
            }
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "java.local_receiver";
            candidate.required = true;
            return Ok(true);
        }
        if proven {
            return Ok(false);
        }
        candidate.syntax = "java.unproven_local_receiver";
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }
    fn receiver<'t>(tree: &'t tree_sitter::Tree, source: &[u8], needle: &[u8]) -> Node<'t> {
        let start = source
            .windows(needle.len())
            .rposition(|bytes| bytes == needle)
            .unwrap();
        tree.root_node()
            .descendant_for_byte_range(start, start + needle.len())
            .unwrap()
    }
    fn candidate(node: Node<'_>) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role: StructuralRole::Signature,
            syntax: "java.identifier",
            required: true,
            native_depth: 0,
        }
    }
    #[test]
    fn original_native_local_receiver_has_a_required_carrier_and_single_optional_leaf() {
        let source =
            include_bytes!("../../tests/fixtures/java_local_receiver/TestPipelineJava.java");
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let node = tree
            .root_node()
            .descendant_for_byte_range(1082, 1088)
            .unwrap();
        let mut cache = CaptureCache::new(source, 65536, true);
        let cancel = Cancellation::new();
        let mut required = candidate(node);
        assert!(
            cache
                .refine(node, StructuralRole::Signature, &mut required, &cancel)
                .unwrap()
        );
        assert_eq!((required.start, required.end), (1082, 1088));
        assert_eq!(required.syntax, "java.local_receiver");
        assert!(required.required);
        assert_eq!(required.role, StructuralRole::Reference);
        let mut optional = candidate(node);
        assert!(
            !cache
                .refine(node, StructuralRole::Reference, &mut optional, &cancel)
                .unwrap()
        );
        let owner = cache.method(node, &cancel).unwrap().unwrap();
        let body = owner.child_by_field_name("body").unwrap();
        let mut context = candidate(body);
        assert!(
            cache
                .refine(body, StructuralRole::Signature, &mut context, &cancel)
                .unwrap()
        );
        assert_eq!(context.syntax, "java.local_receiver_context");
        assert_eq!((context.start, context.end), (959, 1204));
    }
    #[test]
    fn native_receiver_proof_refuses_writes_nested_scopes_duplicate_binders_and_escaped_source() {
        for source in [
            "class Written { void owned() { Object value = first(); value = second(); value.run(); } }",
            "class Written { void owned() { int value = 1; value++; value.run(); } }",
            "class Written { void owned(Object value) { Object value = first(); value.run(); } }",
            "class Written { void owned() { Object value = first(); Object value = second(); value.run(); } }",
            "class Written { void owned() { Object value = first(); Runnable r = () -> value.run(); value.run(); } }",
            "class Written { void owned() { Object value = first(); { value.run(); } value.run(); } }",
            "class Written { void owned() { value.run(); Object value = first(); } }",
            "class Written { void donor() { Object value = first(); } void owned() { value.run(); } }",
            "// \\u000a lexical effect\nclass Written { void owned() { Object value = first(); value.run(); } }",
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "{source}");
            let node = receiver(&tree, source.as_bytes(), b"value.run");
            let node = tree
                .root_node()
                .descendant_for_byte_range(node.start_byte(), node.start_byte() + 5)
                .unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), 65536, true);
            let mut output = candidate(node);
            assert!(
                !cache
                    .refine(
                        node,
                        StructuralRole::Signature,
                        &mut output,
                        &Cancellation::new()
                    )
                    .unwrap(),
                "{source}"
            );
            assert!(
                cache
                    .refine(
                        node,
                        StructuralRole::Reference,
                        &mut output,
                        &Cancellation::new()
                    )
                    .unwrap()
            );
            assert_eq!(output.syntax, "java.unproven_local_receiver");
        }
    }
    #[test]
    fn native_local_receiver_partial_work_and_cancellation_are_authoritative() {
        let source = b"class Written { void owned() { Object value = first(); value.run(); } }";
        let tree = parse(source);
        let start = source
            .windows(10)
            .position(|bytes| bytes == b"value.run(")
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        for (maximum, complete) in [(0, true), (65536, false)] {
            let mut cache = CaptureCache::new(source, maximum, complete);
            let mut output = candidate(node);
            assert!(
                !cache
                    .refine(
                        node,
                        StructuralRole::Signature,
                        &mut output,
                        &Cancellation::new()
                    )
                    .unwrap()
            );
            assert_eq!(cache.exhausted(), maximum == 0);
        }
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source, 65536, true);
        let mut output = candidate(node);
        assert!(matches!(
            cache.refine(node, StructuralRole::Signature, &mut output, &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
