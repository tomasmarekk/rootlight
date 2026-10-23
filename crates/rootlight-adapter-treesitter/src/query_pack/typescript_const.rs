//! Certify a narrow ordinary const value use from the complete native program.
//! The proof selects lexical variables, never the callable value they may hold.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, ecmascript};

#[derive(Default)]
struct Binding<'tree> {
    count: usize,
    written: bool,
    declaration: Option<Node<'tree>>,
}

#[derive(Clone, Copy)]
struct Proof<'tree> {
    definition: Node<'tree>,
    target: Node<'tree>,
    reference: Node<'tree>,
    owner: Node<'tree>,
}

pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    remaining: usize,
    complete: bool,
    initialized: bool,
    exhausted: bool,
    proofs: HashMap<usize, Proof<'tree>>,
}

fn key(raw: &[u8], cancel: &Cancellation) -> Result<Option<ContentHash>, AdapterError> {
    cancel.check()?;
    if !raw
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
    {
        return Ok(None);
    }
    let mut hash = blake3::Hasher::new();
    for chunk in raw.chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            return Ok(None);
        }
        hash.update(chunk);
    }
    Ok(Some(ContentHash::from_bytes(*hash.finalize().as_bytes())))
}

fn same(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.chunks(64).zip(right.chunks(64)) {
        cancel.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn ordinary_const(node: Node<'_>) -> bool {
    node.kind() == "variable_declarator"
        && node
            .child_by_field_name("name")
            .is_some_and(|name| name.kind() == "identifier")
        && node.child_by_field_name("type").is_none()
        && node.child_by_field_name("value").is_some()
        && node.parent().is_some_and(|parent| {
            parent.kind() == "lexical_declaration"
                && parent
                    .child_by_field_name("kind")
                    .is_some_and(|kind| kind.kind() == "const")
                && parent.named_child_count() == 1
        })
}

fn reserved(raw: &[u8]) -> bool {
    matches!(
        raw,
        b"await"
            | b"break"
            | b"case"
            | b"catch"
            | b"class"
            | b"const"
            | b"continue"
            | b"debugger"
            | b"default"
            | b"delete"
            | b"do"
            | b"else"
            | b"enum"
            | b"export"
            | b"extends"
            | b"false"
            | b"finally"
            | b"for"
            | b"function"
            | b"if"
            | b"import"
            | b"in"
            | b"instanceof"
            | b"let"
            | b"new"
            | b"null"
            | b"return"
            | b"super"
            | b"switch"
            | b"this"
            | b"throw"
            | b"true"
            | b"try"
            | b"typeof"
            | b"var"
            | b"void"
            | b"while"
            | b"with"
            | b"yield"
            | b"implements"
            | b"interface"
            | b"package"
            | b"private"
            | b"protected"
            | b"public"
            | b"static"
    )
}

impl<'source, 'tree> CaptureCache<'source, 'tree> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            root,
            remaining: maximum,
            complete,
            initialized: false,
            exhausted: false,
            proofs: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        if self.remaining == 0 {
            self.exhausted = true;
            return Ok(false);
        }
        self.remaining -= 1;
        Ok(true)
    }

    fn depth(&mut self, mut node: Node<'_>, cancel: &Cancellation) -> Result<usize, AdapterError> {
        let mut depth = 0;
        while let Some(parent) = node.parent() {
            if !self.step(cancel)? {
                break;
            }
            depth += 1;
            node = parent;
        }
        Ok(depth)
    }

    fn atom(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.step(cancel)? {
            return Ok(false);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(false);
        };
        match node.kind() {
            "identifier" => Ok(!reserved(raw) && key(raw, cancel)?.is_some()),
            "true" | "false" | "null" => Ok(true),
            "number" => {
                if raw.is_empty() || raw.len() > 1 && raw.first() == Some(&b'0') {
                    return Ok(false);
                }
                for chunk in raw.chunks(64) {
                    cancel.check()?;
                    if !chunk.iter().all(u8::is_ascii_digit) {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            "string" => {
                let Some(quote @ (b'\'' | b'"')) = raw.first().copied() else {
                    return Ok(false);
                };
                if raw.len() < 2 || raw.last() != Some(&quote) {
                    return Ok(false);
                }
                for chunk in raw[1..raw.len() - 1].chunks(64) {
                    cancel.check()?;
                    if chunk
                        .iter()
                        .any(|byte| *byte == quote || matches!(byte, b'\\' | b'\r' | b'\n' | 0))
                    {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn plain_callee(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        loop {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if node.kind() == "identifier" {
                return self.atom(node, cancel);
            }
            if node.kind() != "member_expression"
                || node.child_by_field_name("optional_chain").is_some()
            {
                return Ok(false);
            }
            let Some(property) = node
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")
            else {
                return Ok(false);
            };
            let Some(raw) = self.source.get(property.byte_range()) else {
                return Ok(false);
            };
            if key(raw, cancel)?.is_none() {
                return Ok(false);
            }
            let Some(object) = node.child_by_field_name("object") else {
                return Ok(false);
            };
            node = object;
        }
    }

    fn target_initializer(
        &mut self,
        target: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.step(cancel)? {
            return Ok(false);
        }
        let Some(mut value) = target.child_by_field_name("value") else {
            return Ok(false);
        };
        let awaited = value.kind() == "await_expression";
        if awaited {
            if value.named_child_count() != 1 {
                return Ok(false);
            }
            let Some(inner) = value.named_child(0) else {
                return Ok(false);
            };
            value = inner;
        }
        if value.kind() != "call_expression" {
            return Ok(!awaited && self.atom(value, cancel)?);
        }
        let Some(callee) = value.child_by_field_name("function") else {
            return Ok(false);
        };
        if !self.plain_callee(callee, cancel)? {
            return Ok(false);
        }
        let Some(arguments) = value
            .child_by_field_name("arguments")
            .filter(|arguments| arguments.kind() == "arguments")
        else {
            return Ok(false);
        };
        let mut cursor = value.walk();
        for child in value.children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if matches!(child.kind(), "optional_chain" | "?." | "type_arguments") {
                return Ok(false);
            }
        }
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            if !self.atom(argument, cancel)? {
                return Ok(false);
            }
        }
        let Some(previous) = arguments
            .child_count()
            .checked_sub(2)
            .and_then(|index| u32::try_from(index).ok())
            .and_then(|index| arguments.child(index))
        else {
            return Ok(false);
        };
        Ok(previous.kind() != ",")
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        let root = self.root;
        if !self.complete
            || root.kind() != "program"
            || root.has_error()
            || root.is_missing()
            || root.end_byte() != self.source.len()
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
        {
            return Ok(());
        }
        let mut bindings: HashMap<ContentHash, Binding<'tree>> = HashMap::new();
        let mut references = Vec::new();
        let mut cursor = root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            if matches!(
                node.kind(),
                "with_statement"
                    | "internal_module"
                    | "module"
                    | "namespace_declaration"
                    | "ambient_declaration"
                    | "function_declaration"
                    | "function_expression"
                    | "arrow_function"
                    | "generator_function"
                    | "generator_function_declaration"
                    | "class_declaration"
                    | "class"
                    | "method_definition"
            ) {
                return Ok(());
            }
            if matches!(
                node.kind(),
                "identifier" | "type_identifier" | "shorthand_property_identifier_pattern"
            ) {
                let Some(raw) = self.source.get(node.byte_range()) else {
                    return Ok(());
                };
                let Some(key) = key(raw, cancel)? else {
                    return Ok(());
                };
                if raw == b"eval" {
                    return Ok(());
                }
                let binder = ecmascript::binding_kind(node, cancel)?.is_some()
                    || node
                        .parent()
                        .is_some_and(|parent| parent.child_by_field_name("name") == Some(node));
                let mut write = false;
                let mut ancestor = node.parent();
                while let Some(parent) = ancestor {
                    if !self.step(cancel)? {
                        return Ok(());
                    }
                    let left = match parent.kind() {
                        "assignment_expression"
                        | "augmented_assignment_expression"
                        | "for_in_statement" => parent.child_by_field_name("left"),
                        "update_expression" => parent.child_by_field_name("argument"),
                        _ => None,
                    };
                    write |= left.is_some_and(|left| {
                        left.start_byte() <= node.start_byte() && node.end_byte() <= left.end_byte()
                    });
                    ancestor = parent.parent();
                }
                if binder || write {
                    bindings
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    let binding = bindings.entry(key).or_default();
                    if binder {
                        binding.count = binding.count.saturating_add(1);
                        if node.parent().is_some_and(ordinary_const) {
                            binding.declaration = node.parent();
                        }
                    }
                    binding.written |= write;
                }
                if node.kind() == "identifier" {
                    references
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    references.push((node, key));
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
                    for (reference, key) in references {
                        if !self.step(cancel)? {
                            self.proofs.clear();
                            return Ok(());
                        }
                        let Some(binding) = bindings
                            .get(&key)
                            .filter(|binding| binding.count == 1 && !binding.written)
                        else {
                            continue;
                        };
                        let Some(target) = binding.declaration else {
                            continue;
                        };
                        if !self.target_initializer(target, cancel)? {
                            continue;
                        }
                        let Some(definition) = target.child_by_field_name("name") else {
                            continue;
                        };
                        if target.end_byte() > reference.start_byte()
                            || !same(
                                &self.source[definition.byte_range()],
                                &self.source[reference.byte_range()],
                                cancel,
                            )?
                        {
                            continue;
                        }
                        let Some(owner) = self.call_owner(reference, cancel)? else {
                            continue;
                        };
                        let target_scope = target.parent().and_then(|parent| parent.parent());
                        let mut ancestor = owner.parent().and_then(|parent| parent.parent());
                        let mut reaches = false;
                        while let Some(parent) = ancestor {
                            if !self.step(cancel)? {
                                self.proofs.clear();
                                return Ok(());
                            }
                            reaches |= Some(parent) == target_scope;
                            ancestor = parent.parent();
                        }
                        if !reaches {
                            continue;
                        }
                        self.proofs
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.proofs.insert(
                            reference.end_byte(),
                            Proof {
                                definition,
                                target,
                                reference,
                                owner,
                            },
                        );
                    }
                    return Ok(());
                }
            }
        }
    }

    fn call_owner(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.step(cancel)? {
            return Ok(None);
        }
        let Some(parent) = node.parent() else {
            return Ok(None);
        };
        let call = if parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(node)
        {
            parent
        } else if parent.kind() == "arguments" {
            let Some(call) = parent
                .parent()
                .filter(|call| call.kind() == "call_expression")
            else {
                return Ok(None);
            };
            call
        } else {
            return Ok(None);
        };
        let Some(callee) = call
            .child_by_field_name("function")
            .filter(|callee| callee.kind() == "identifier")
        else {
            return Ok(None);
        };
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Ok(None);
        };
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if argument.kind() != "identifier" {
                return Ok(None);
            }
        }
        let mut cursor = call.walk();
        for child in call.children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if matches!(child.kind(), "optional_chain" | "?." | "type_arguments") {
                return Ok(None);
            }
        }
        if callee.is_missing() {
            return Ok(None);
        }
        Ok(call.parent().filter(|owner| {
            ordinary_const(*owner) && owner.child_by_field_name("value") == Some(call)
        }))
    }

    /// One required program signature carries the complete certified partition.
    pub(super) fn candidates(
        &mut self,
        node: Node<'tree>,
        role: StructuralRole,
        cancel: &Cancellation,
    ) -> Result<Vec<QueryCandidate>, AdapterError> {
        cancel.check()?;
        if !self.initialized {
            self.initialize(cancel)?;
        }
        let mut output = Vec::new();
        if self.exhausted
            || self.proofs.is_empty()
            || role != StructuralRole::Signature
            || node != self.root
        {
            return Ok(output);
        }
        let proofs: Vec<_> = {
            let mut rows = Vec::new();
            rows.try_reserve_exact(self.proofs.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            for proof in self.proofs.values() {
                cancel.check()?;
                rows.push(*proof);
            }
            rows
        };
        let capacity = proofs
            .len()
            .checked_mul(4)
            .and_then(|count| count.checked_add(1))
            .ok_or(SinkError::AccountingOverflow)?;
        output
            .try_reserve_exact(capacity)
            .map_err(|_| SinkError::AllocationFailed)?;
        output.push(QueryCandidate {
            start: 0,
            end: self.source.len(),
            role: StructuralRole::Signature,
            syntax: "typescript.const_clean_file",
            required: true,
            native_depth: 0,
        });
        for proof in proofs {
            let depth = self.depth(proof.reference, cancel)?;
            output.push(QueryCandidate {
                start: proof.reference.start_byte(),
                end: proof.reference.end_byte(),
                role: StructuralRole::Reference,
                syntax: "typescript.const_value",
                required: true,
                native_depth: depth,
            });
            output.push(QueryCandidate {
                start: proof.definition.start_byte(),
                end: proof.reference.end_byte(),
                role: StructuralRole::Signature,
                syntax: "typescript.const_target",
                required: true,
                native_depth: 0,
            });
            let depth = self.depth(proof.owner, cancel)?;
            output.push(QueryCandidate {
                start: proof.owner.start_byte(),
                end: proof.owner.end_byte(),
                role: StructuralRole::Signature,
                syntax: "typescript.const_call_rhs",
                required: true,
                native_depth: depth,
            });
            let depth = self.depth(proof.target, cancel)?;
            output.push(QueryCandidate {
                start: proof.target.start_byte(),
                end: proof.target.end_byte(),
                role: StructuralRole::Signature,
                syntax: "typescript.const_binding",
                required: true,
                native_depth: depth,
            });
        }
        if self.exhausted {
            output.clear();
        }
        Ok(output)
    }

    pub(super) fn refine(
        &mut self,
        node: Node<'tree>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if !self.initialized {
            self.initialize(cancel)?;
        }
        cancel.check()?;
        if role == StructuralRole::Reference
            && self
                .proofs
                .get(&node.end_byte())
                .is_some_and(|proof| proof.reference == node)
            && !self.exhausted
        {
            let depth = self.depth(node, cancel)?;
            if self.exhausted {
                return Ok(());
            }
            candidate.syntax = "typescript.const_value";
            candidate.required = true;
            candidate.native_depth = depth;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_const_partition_requires_full_clean_context_and_pollable_work() {
        let source = b"const value = 1; const output = consume(value);";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        for (maximum, complete, admitted) in [
            (16_384, true, true),
            (0, true, false),
            (16_384, false, false),
        ] {
            let mut cache = CaptureCache::new(source, tree.root_node(), maximum, complete);
            let candidates = cache
                .candidates(
                    tree.root_node(),
                    StructuralRole::Signature,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                candidates
                    .iter()
                    .filter(|candidate| candidate.syntax == "typescript.const_value"
                        && candidate.role == StructuralRole::Reference
                        && candidate.required)
                    .count(),
                usize::from(admitted)
            );
            assert_eq!(cache.exhausted(), maximum == 0 && complete);
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, tree.root_node(), 16_384, true);
        assert!(matches!(
            cache.candidates(tree.root_node(), StructuralRole::Signature, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn native_const_proof_does_not_promote_shadow_writes_closures_or_computed_arguments() {
        for source in [
            "const value = 1; { const value = 2; const output = consume(value); }",
            "const value = 1; value += 1; const output = consume(value);",
            "const value = 1; const output = (() => value);",
            "const value = 1; const output = consume(value.member);",
            "const value = 1; const output = consume?.(value);",
            "let value = 1; const output = consume(value);",
            "const output = consume(value); const value = 1;",
            "const value = consume(value);",
            "{ const value = 1; } const output = consume(value);",
            "const value = 1, other = 2; const output = consume(value);",
            "const value: number = 1; const output = consume(value);",
            "const value = 1; const output = consume(value;",
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true);
            assert!(
                cache
                    .candidates(
                        tree.root_node(),
                        StructuralRole::Signature,
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn renamed_callee_and_argument_certificates_keep_exact_written_spans() {
        let source = "const apply = factory; const sample = payload; const report = apply(sample);";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true);
        let candidates = cache
            .candidates(
                tree.root_node(),
                StructuralRole::Signature,
                &Cancellation::new(),
            )
            .unwrap();
        let mut actual: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.syntax == "typescript.const_value")
            .map(|candidate| {
                assert!(candidate.required);
                assert_eq!(candidate.role, StructuralRole::Reference);
                candidate.start..candidate.end
            })
            .collect();
        actual.sort_by_key(|range| range.start);
        let callee = source.rfind("apply").unwrap();
        let argument = source.rfind("sample").unwrap();
        assert_eq!(actual, vec![callee..callee + 5, argument..argument + 6]);
        for generated in candidates
            .iter()
            .filter(|candidate| candidate.syntax == "typescript.const_value")
        {
            let proof = cache.proofs[&generated.end];
            let mut captured = QueryCandidate {
                start: generated.start,
                end: generated.end,
                role: StructuralRole::Reference,
                syntax: "typescript.identifier",
                required: false,
                native_depth: 0,
            };
            cache
                .refine(
                    proof.reference,
                    StructuralRole::Reference,
                    &mut captured,
                    &Cancellation::new(),
                )
                .unwrap();
            assert!(captured.required);
            assert!(
                captured.same_fact(*generated),
                "the optional capture must deduplicate with its required carrier"
            );
        }
    }

    #[test]
    fn target_initializers_have_closed_atomic_and_plain_call_shapes() {
        for (initializer, admitted) in [
            ("1", true),
            ("'plain'", true),
            ("factory", true),
            ("true", true),
            ("null", true),
            ("await validate('plain')", true),
            ("JSON.parse(json)", true),
            (".", false),
            ("..... validate('plain')", false),
            ("[1]", false),
            ("({item: 1})", false),
            ("1 + 2", false),
            ("1.5", false),
            ("-1", false),
            ("factory.member", false),
            ("factory?.(input)", false),
            ("factory<Type>(input)", false),
            ("factory(other())", false),
            (r"'escaped\n'", false),
        ] {
            let source = format!("const value = {initializer}; const output = consume(value);");
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
                .unwrap();
            let tree = parser.parse(&source, None).unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true);
            let candidates = cache
                .candidates(
                    tree.root_node(),
                    StructuralRole::Signature,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                candidates
                    .iter()
                    .filter(|candidate| candidate.syntax == "typescript.const_value")
                    .count(),
                usize::from(admitted),
                "{initializer}"
            );
        }
    }
}
