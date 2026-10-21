//! Certify written method formals used as ordinary member or element receivers.
//! A complete native body census excludes hidden binders; member targets, calls,
//! runtime types and execution stay outside this finite lexical proof.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

struct MethodProof<'source> {
    names: HashSet<&'source str>,
    receivers: HashSet<(usize, usize)>,
    complete: bool,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete_input: bool,
    remaining: usize,
    exhausted: bool,
    methods: HashMap<usize, MethodProof<'source>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete_input: bool) -> Self {
        Self {
            source,
            complete_input,
            // Match the existing bounded native certificate allowance. Each
            // node, ancestry step and 64-byte name chunk consumes one step.
            remaining: maximum
                .min(super::HARD_MAX_QUERY_CAPTURES)
                .saturating_mul(64),
            exhausted: false,
            methods: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.step(cancel)?
            || node.kind() != "identifier"
            || node.has_error()
            || node.is_missing()
        {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, rest)) = raw.split_first() else {
            return Ok(None);
        };
        Ok(((*first == b'_' || first.is_ascii_alphabetic())
            && rest
                .iter()
                .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric()))
        .then(|| std::str::from_utf8(raw).ok())
        .flatten())
    }

    fn owner<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut current = Some(node);
        while let Some(ancestor) = current {
            if !self.step(cancel)? {
                return Ok(None);
            }
            match ancestor.kind() {
                "method_declaration" => return Ok(Some(ancestor)),
                // Keep walking through unsupported nested binders so their
                // names cannot fall back to a spelling join with new formals.
                _ => current = ancestor.parent(),
            }
        }
        Ok(None)
    }

    fn parameter<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.step(cancel)?
            || node.kind() != "parameter"
            || node.has_error()
            || node.is_missing()
        {
            return Ok(None);
        }
        let Some(list) = node
            .parent()
            .filter(|parent| parent.kind() == "parameter_list")
        else {
            return Ok(None);
        };
        let Some(owner) = list
            .parent()
            .filter(|parent| parent.kind() == "method_declaration")
        else {
            return Ok(None);
        };
        if owner.child_by_field_name("parameters") != Some(list) {
            return Ok(None);
        }
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        Ok(self.word(name, cancel)?.map(|_| name))
    }

    fn clean_context(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.complete_input || owner.has_error() || owner.is_missing() {
            return Ok(false);
        }
        let mut current = owner;
        let mut type_owner = false;
        while let Some(parent) = current.parent() {
            if !self.step(cancel)? || parent.has_error() || parent.is_missing() || parent.is_error()
            {
                return Ok(false);
            }
            match parent.kind() {
                "class_declaration"
                | "struct_declaration"
                | "record_declaration"
                | "interface_declaration" => {
                    type_owner = true;
                }
                "declaration_list"
                | "namespace_declaration"
                | "file_scoped_namespace_declaration" => {}
                "compilation_unit" => {
                    let Some(prefix) = self.source.get(..parent.start_byte()) else {
                        return Ok(false);
                    };
                    let Some(suffix) = self.source.get(parent.end_byte()..) else {
                        return Ok(false);
                    };
                    for raw in [
                        prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix),
                        suffix,
                    ] {
                        for chunk in raw.chunks(64) {
                            if !self.step(cancel)? || !chunk.iter().all(u8::is_ascii_whitespace) {
                                return Ok(false);
                            }
                        }
                    }
                    return Ok(type_owner && parent.parent().is_none());
                }
                _ => return Ok(false),
            }
            current = parent;
        }
        Ok(false)
    }

    fn inspect(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<MethodProof<'source>, AdapterError> {
        let mut proof = MethodProof {
            names: HashSet::new(),
            receivers: HashSet::new(),
            complete: false,
        };
        let Some(parameters) = owner.child_by_field_name("parameters") else {
            return Ok(proof);
        };
        let mut header_valid = true;
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(proof);
            }
            if parameter.kind() == "comment" {
                continue;
            }
            let Some(name) = self.parameter(parameter, cancel)? else {
                header_valid = false;
                continue;
            };
            let Some(word) = self.word(name, cancel)? else {
                header_valid = false;
                continue;
            };
            proof
                .names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !proof.names.insert(word) {
                header_valid = false;
            }
        }
        if !header_valid || proof.names.is_empty() || !self.clean_context(owner, cancel)? {
            return Ok(proof);
        }
        // Generic, explicit-interface and expression-bodied methods need their
        // own source qualification; their ordinary captures remain available.
        let mut header = owner.walk();
        for child in owner.named_children(&mut header) {
            if !self.step(cancel)? {
                return Ok(proof);
            }
            if matches!(
                child.kind(),
                "type_parameter_list" | "explicit_interface_specifier"
            ) {
                return Ok(proof);
            }
        }
        let Some(method_name) = owner.child_by_field_name("name") else {
            return Ok(proof);
        };
        if self.word(method_name, cancel)?.is_none() {
            return Ok(proof);
        }
        let Some(body) = owner
            .child_by_field_name("body")
            .filter(|node| node.kind() == "block")
        else {
            return Ok(proof);
        };
        let mut candidates = HashSet::new();
        let mut walk = body.walk();
        loop {
            let node = walk.node();
            if !self.step(cancel)? || node.has_error() || node.is_error() || node.is_missing() {
                return Ok(proof);
            }
            if node.is_named() {
                match node.kind() {
                    "block"
                    | "comment"
                    | "local_declaration_statement"
                    | "variable_declaration"
                    | "implicit_type"
                    | "predefined_type"
                    | "qualified_name"
                    | "generic_name"
                    | "type_argument_list"
                    | "argument"
                    | "argument_list"
                    | "bracketed_argument_list"
                    | "array_creation_expression"
                    | "array_rank_specifier"
                    | "array_type"
                    | "initializer_expression"
                    | "integer_literal"
                    | "real_literal"
                    | "boolean_literal"
                    | "null_literal"
                    | "string_literal"
                    | "character_literal"
                    | "invocation_expression"
                    | "member_access_expression"
                    | "element_access_expression"
                    | "yield_statement"
                    | "return_statement"
                    | "expression_statement"
                    | "parenthesized_expression" => {}
                    "variable_declarator" => {
                        let Some(name) = node.child_by_field_name("name") else {
                            return Ok(proof);
                        };
                        let Some(word) = self.word(name, cancel)? else {
                            return Ok(proof);
                        };
                        if proof.names.contains(word) {
                            return Ok(proof);
                        }
                    }
                    "foreach_statement" => {
                        let Some(left) = node.child_by_field_name("left") else {
                            return Ok(proof);
                        };
                        let Some(word) = self.word(left, cancel)? else {
                            return Ok(proof);
                        };
                        if proof.names.contains(word) {
                            return Ok(proof);
                        }
                    }
                    "identifier" => {
                        let Some(word) = self.word(node, cancel)? else {
                            return Ok(proof);
                        };
                        if proof.names.contains(word)
                            && node.parent().is_some_and(|parent| {
                                matches!(
                                    parent.kind(),
                                    "member_access_expression" | "element_access_expression"
                                ) && parent.child_by_field_name("expression") == Some(node)
                            })
                        {
                            candidates
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            candidates.insert((node.start_byte(), node.end_byte()));
                        }
                    }
                    // This closed set deliberately excludes local functions,
                    // lambdas, patterns, queries, deconstruction and mutations.
                    _ => return Ok(proof),
                }
            }
            if walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    proof.receivers = candidates;
                    proof.complete = true;
                    return Ok(proof);
                }
            }
        }
    }

    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.step(cancel)? {
            return Ok(false);
        }
        if role == StructuralRole::Declaration && node.kind() == "parameter" {
            if self.parameter(node, cancel)?.is_none() {
                return Ok(false);
            }
            candidate.syntax = "csharp.parameter";
            return Ok(true);
        }
        if role == StructuralRole::Definition
            && node.kind() == "identifier"
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "parameter")
        {
            return Ok(self.parameter(node.parent().unwrap(), cancel)? == Some(node));
        }
        let context = role == StructuralRole::Signature && node.kind() == "method_declaration";
        if !(context
            || node.kind() == "identifier"
                && matches!(role, StructuralRole::Reference | StructuralRole::Signature))
        {
            return Ok(true);
        }
        let Some(owner) = self.owner(node, cancel)? else {
            return Ok(role != StructuralRole::Signature);
        };
        if !self.methods.contains_key(&owner.id()) {
            let proof = self.inspect(owner, cancel)?;
            self.methods
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.methods.insert(owner.id(), proof);
        }
        if context {
            if self
                .methods
                .get(&owner.id())
                .is_none_or(|proof| !proof.complete)
            {
                return Ok(false);
            }
            candidate.role = StructuralRole::Signature;
            candidate.syntax = "csharp.formal_context";
            candidate.required = true;
            return Ok(true);
        }
        if self.methods.get(&owner.id()).is_some_and(|proof| {
            proof
                .receivers
                .contains(&(node.start_byte(), node.end_byte()))
        }) {
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "csharp.formal_receiver";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature {
            return Ok(false);
        }
        let word = self.word(node, cancel)?;
        if self.methods.get(&owner.id()).is_some_and(|proof| {
            !proof.names.is_empty() && word.is_none_or(|word| proof.names.contains(word))
        }) {
            candidate.syntax = "csharp.unproven_formal";
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn find<'tree>(root: Node<'tree>, kind: &str, start: usize) -> Node<'tree> {
        let mut nodes = vec![root];
        while let Some(node) = nodes.pop() {
            if node.kind() == kind && node.start_byte() == start {
                return node;
            }
            let mut cursor = node.walk();
            nodes.extend(node.named_children(&mut cursor));
        }
        panic!("missing {kind}@{start}")
    }

    #[test]
    fn original_clean_method_certifies_only_its_plain_formal_receiver_positions() {
        let source = include_str!("../../tests/fixtures/csharp_formal_observation/Extensions.cs");
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let method = find(parsed.root_node(), "method_declaration", 485);
        let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
        let proof = cache.inspect(method, &Cancellation::new()).unwrap();
        assert!(proof.complete);
        assert_eq!(proof.names, HashSet::from(["array", "axis"]));
        assert!(proof.receivers.contains(&(712, 717)));
        assert_eq!(proof.receivers.len(), 2);
        let leaf = find(parsed.root_node(), "identifier", 712);
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let mut candidate = QueryCandidate {
                start: leaf.start_byte(),
                end: leaf.end_byte(),
                role,
                syntax: "csharp.identifier",
                required: false,
                native_depth: 0,
            };
            assert!(
                cache
                    .refine(leaf, role, &mut candidate, &Cancellation::new())
                    .unwrap()
            );
            assert_eq!(candidate.role, StructuralRole::Reference);
            assert_eq!(candidate.syntax, "csharp.formal_receiver");
            assert!(candidate.required);
        }
    }

    #[test]
    fn unsupported_nested_binders_and_header_shapes_withhold_whole_method_proof() {
        for source in [
            "class C { object M(object value) { object value = Other; return value.Member; } }",
            "class C { object M(object value) { foreach (var value in Other) { External(value.Member); } return value.Member; } }",
            "class C { object M(object value) { object N(object value) { return value.Member; } return value.Member; } }",
            "class C { object M(object value) { var f = (object value) => value.Member; return value.Member; } }",
            "class C { object M<T>(object value) { return value.Member; } }",
            "class C { object M(object value, object value) { return value.Member; } }",
            "class C { object M(object @value) { return value.Member; } }",
            "class C { object M(object value) { return @value.Member; } }",
        ] {
            let parsed = tree(source);
            assert!(!parsed.root_node().has_error(), "native premise: {source}");
            let method = find(
                parsed.root_node(),
                "method_declaration",
                source.find("object M").unwrap(),
            );
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            assert!(
                !cache
                    .inspect(method, &Cancellation::new())
                    .unwrap()
                    .complete,
                "{source}"
            );
        }
    }

    #[test]
    fn partial_native_work_exhaustion_and_typed_cancellation_cannot_certify_receivers() {
        let source = "class C { object M(object value) { return value.Member; } }";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let method = find(
            parsed.root_node(),
            "method_declaration",
            source.find("object M").unwrap(),
        );
        let mut partial = CaptureCache::new(source.as_bytes(), 16_384, false);
        assert!(
            !partial
                .inspect(method, &Cancellation::new())
                .unwrap()
                .complete
        );
        let mut stopped = CaptureCache::new(source.as_bytes(), 1, true);
        stopped.remaining = 3;
        assert!(
            !stopped
                .inspect(method, &Cancellation::new())
                .unwrap()
                .complete
        );
        assert!(stopped.exhausted());
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source.as_bytes(), usize::MAX, true);
        assert_eq!(cache.remaining, super::super::HARD_MAX_QUERY_CAPTURES * 64);
        assert!(matches!(
            cache.inspect(method, &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest,
            })
        ));
    }
}
