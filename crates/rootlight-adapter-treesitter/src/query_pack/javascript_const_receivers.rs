//! Certify ordinary lexical const receivers in closed module object assignments.
//! Keys and member selectors are nonbinding witnesses; no callable value is inferred.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

#[derive(Clone, Copy)]
struct Proof<'tree> {
    binding: Node<'tree>,
    definition: Node<'tree>,
    receiver: Node<'tree>,
    call: Node<'tree>,
    assignment: Node<'tree>,
}

/// Retains a bounded complete-program receiver partition for one native tree.
pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    remaining: usize,
    complete: bool,
    initialized: bool,
    exhausted: bool,
    proofs: HashMap<usize, Proof<'tree>>,
    nonbinding: Vec<Node<'tree>>,
}

fn identifier_name(raw: &[u8]) -> bool {
    raw.first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        && raw
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        && !matches!(raw, b"eval" | b"await" | b"yield")
}

fn identifier(raw: &[u8]) -> bool {
    identifier_name(raw) && !reserved(raw)
}

// TSX can represent reserved words as identifiers without enforcing early errors.
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
    /// Starts a source-bound census; partial input cannot supply certificates.
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
            nonbinding: Vec::new(),
        }
    }

    /// Reports genuine helper work exhaustion without inventing a capture count.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn raw(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
        }
        Ok(Some(raw))
    }

    fn atom(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(raw) = self.raw(node, cancel)? else {
            return Ok(false);
        };
        Ok(match node.kind() {
            "identifier" => identifier(raw),
            "true" | "false" | "null" => true,
            "number" => {
                !raw.is_empty()
                    && (raw.len() == 1 || raw.first() != Some(&b'0'))
                    && raw.iter().all(u8::is_ascii_digit)
            }
            "string" => {
                matches!(raw.first(), Some(b'\'' | b'"'))
                    && raw.len() >= 2
                    && raw.first() == raw.last()
                    && raw.get(1..raw.len() - 1).is_some_and(|inside| {
                        inside.iter().all(|byte| {
                            Some(byte) != raw.first() && !matches!(byte, b'\\' | b'\r' | b'\n' | 0)
                        })
                    })
            }
            _ => false,
        })
    }

    fn call(
        &mut self,
        node: Node<'tree>,
        member: bool,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.step(cancel)? || node.kind() != "call_expression" {
            return Ok(None);
        }
        let Some(function) = node.child_by_field_name("function") else {
            return Ok(None);
        };
        let receiver = if member {
            if function.kind() != "member_expression" || function.named_child_count() != 2 {
                return Ok(None);
            }
            let (Some(object), Some(property)) = (
                function
                    .child_by_field_name("object")
                    .filter(|value| value.kind() == "identifier"),
                function
                    .child_by_field_name("property")
                    .filter(|value| value.kind() == "property_identifier"),
            ) else {
                return Ok(None);
            };
            if !self.atom(object, cancel)?
                || !self.raw(property, cancel)?.is_some_and(identifier_name)
            {
                return Ok(None);
            }
            self.nonbinding
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.nonbinding.push(property);
            object
        } else {
            if function.kind() != "identifier" || !self.atom(function, cancel)? {
                return Ok(None);
            }
            function
        };
        let Some(arguments) = node
            .child_by_field_name("arguments")
            .filter(|value| value.kind() == "arguments")
        else {
            return Ok(None);
        };
        for item in [node, function] {
            let mut cursor = item.walk();
            for child in item.children(&mut cursor) {
                if !self.step(cancel)?
                    || matches!(child.kind(), "optional_chain" | "?." | "type_arguments")
                {
                    return Ok(None);
                }
            }
        }
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            if argument.kind() != "comment" && !self.atom(argument, cancel)? {
                return Ok(None);
            }
        }
        let Some(last) = arguments
            .child_count()
            .checked_sub(2)
            .and_then(|index| u32::try_from(index).ok())
            .and_then(|index| arguments.child(index))
        else {
            return Ok(None);
        };
        Ok((last.kind() != ",").then_some(receiver))
    }

    fn assignment_left(
        &mut self,
        mut node: Node<'tree>,
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
                || node.named_child_count() != 2
                || node.child_by_field_name("optional_chain").is_some()
            {
                return Ok(false);
            }
            let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property")
                    .filter(|value| value.kind() == "property_identifier"),
            ) else {
                return Ok(false);
            };
            if !self.raw(property, cancel)?.is_some_and(identifier_name) {
                return Ok(false);
            }
            self.nonbinding
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.nonbinding.push(property);
            node = object;
        }
    }

    fn closed_hashbang(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "hash_bang_line"
            || node.parent() != Some(self.root)
            || !matches!(
                self.source.get(..node.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(false);
        }
        let Some(raw) = self.raw(node, cancel)? else {
            return Ok(false);
        };
        let next = self.source.get(node.end_byte()).copied();
        if !raw.starts_with(b"#!") || !matches!(next, None | Some(b'\n')) {
            return Ok(false);
        }
        // This pinned carrier ends only at LF. A different line terminator can
        // hide executable source from the native owner's complete-file census.
        for (index, byte) in raw.iter().enumerate() {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if *byte == b'\n'
                || (*byte == b'\r' && (index + 1 != raw.len() || next != Some(b'\n')))
                || matches!(
                    raw.get(index..index.saturating_add(3)),
                    Some(b"\xe2\x80\xa8" | b"\xe2\x80\xa9")
                )
            {
                return Ok(false);
            }
        }
        Ok(true)
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
        let mut bindings = Vec::new();
        let mut selected = Vec::new();
        let mut cursor = root.walk();
        for statement in root.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(());
            }
            match statement.kind() {
                "comment" | "empty_statement" => {}
                "hash_bang_line" => {
                    if !self.closed_hashbang(statement, cancel)? {
                        return Ok(());
                    }
                }
                "lexical_declaration" => {
                    if statement
                        .child_by_field_name("kind")
                        .is_none_or(|kind| kind.kind() != "const")
                        || statement.named_child_count() != 1
                    {
                        return Ok(());
                    }
                    let Some(binding) = statement.named_child(0).filter(|node| {
                        node.kind() == "variable_declarator"
                            && node.child_by_field_name("type").is_none()
                    }) else {
                        return Ok(());
                    };
                    let (Some(name), Some(value)) = (
                        binding
                            .child_by_field_name("name")
                            .filter(|node| node.kind() == "identifier"),
                        binding.child_by_field_name("value"),
                    ) else {
                        return Ok(());
                    };
                    if !self.atom(name, cancel)?
                        || !(self.atom(value, cancel)?
                            || self.call(value, false, cancel)?.is_some())
                    {
                        return Ok(());
                    }
                    bindings
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    bindings.push((name, binding));
                }
                "expression_statement" => {
                    let Some(assignment) = statement.named_child(0).filter(|node| {
                        node.kind() == "assignment_expression" && statement.named_child_count() == 1
                    }) else {
                        return Ok(());
                    };
                    let (Some(left), Some(object)) = (
                        assignment.child_by_field_name("left"),
                        assignment
                            .child_by_field_name("right")
                            .filter(|node| node.kind() == "object"),
                    ) else {
                        return Ok(());
                    };
                    if !self.assignment_left(left, cancel)? {
                        return Ok(());
                    }
                    let mut objects = Vec::new();
                    objects
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    objects.push(object);
                    while let Some(object) = objects.pop() {
                        let mut cursor = object.walk();
                        for pair in object.named_children(&mut cursor) {
                            if !self.step(cancel)? {
                                return Ok(());
                            }
                            if pair.kind() == "comment" {
                                continue;
                            }
                            if pair.kind() != "pair" || pair.named_child_count() != 2 {
                                return Ok(());
                            }
                            let (Some(key), Some(value)) = (
                                pair.child_by_field_name("key"),
                                pair.child_by_field_name("value"),
                            ) else {
                                return Ok(());
                            };
                            if key.kind() == "property_identifier"
                                && self.raw(key, cancel)?.is_some_and(identifier_name)
                            {
                                self.nonbinding
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                self.nonbinding.push(key);
                            } else if key.kind() != "string" || !self.atom(key, cancel)? {
                                return Ok(());
                            }
                            if value.kind() == "object" {
                                objects
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                objects.push(value);
                            } else if value.kind() == "call_expression" {
                                let Some(receiver) = self.call(value, true, cancel)? else {
                                    return Ok(());
                                };
                                selected
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                selected.push((receiver, value, assignment));
                            } else if !self.atom(value, cancel)? {
                                return Ok(());
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
        let mut proofs = HashMap::new();
        for (name, binding) in bindings {
            if !self.step(cancel)? {
                return Ok(());
            }
            let Some(spelling) = self.raw(name, cancel)? else {
                return Ok(());
            };
            let mut allowed = HashSet::new();
            allowed
                .try_reserve(
                    self.nonbinding
                        .len()
                        .saturating_add(selected.len())
                        .saturating_add(1),
                )
                .map_err(|_| SinkError::AllocationFailed)?;
            allowed.insert(name.id());
            for index in 0..self.nonbinding.len() {
                if !self.step(cancel)? {
                    return Ok(());
                }
                allowed.insert(self.nonbinding[index].id());
            }
            for (receiver, _, _) in &selected {
                if self.raw(*receiver, cancel)? == Some(spelling)
                    && binding.end_byte() <= receiver.start_byte()
                {
                    allowed.insert(receiver.id());
                }
            }
            let mut valid = true;
            let mut cursor = root.walk();
            loop {
                if !self.step(cancel)? {
                    return Ok(());
                }
                let node = cursor.node();
                if matches!(
                    node.kind(),
                    "identifier"
                        | "property_identifier"
                        | "shorthand_property_identifier"
                        | "shorthand_property_identifier_pattern"
                ) && self.raw(node, cancel)? == Some(spelling)
                    && !allowed.contains(&node.id())
                {
                    valid = false;
                    break;
                }
                if cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break;
                    }
                }
                if cursor.node() == root {
                    break;
                }
            }
            if valid {
                for (receiver, call, assignment) in &selected {
                    if self.raw(*receiver, cancel)? == Some(spelling)
                        && binding.end_byte() <= receiver.start_byte()
                    {
                        proofs
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        proofs.insert(
                            receiver.end_byte(),
                            Proof {
                                binding,
                                definition: name,
                                receiver: *receiver,
                                call: *call,
                                assignment: *assignment,
                            },
                        );
                    }
                }
            }
        }
        if !self.exhausted {
            self.proofs = proofs;
        }
        Ok(())
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

    /// Emits the complete required certificate partition from the program carrier.
    ///
    /// # Errors
    /// Returns cancellation or fallible allocation/accounting errors.
    pub(super) fn candidates(
        &mut self,
        node: Node<'tree>,
        role: StructuralRole,
        cancel: &Cancellation,
    ) -> Result<Vec<QueryCandidate>, AdapterError> {
        if !self.initialized {
            self.initialize(cancel)?;
        }
        cancel.check()?;
        let mut result = Vec::new();
        if self.exhausted
            || self.proofs.is_empty()
            || node != self.root
            || role != StructuralRole::Signature
        {
            return Ok(result);
        }
        let count = self
            .proofs
            .len()
            .checked_mul(5)
            .and_then(|count| count.checked_add(self.nonbinding.len()))
            .and_then(|count| count.checked_add(1))
            .ok_or(SinkError::AccountingOverflow)?;
        result
            .try_reserve_exact(count)
            .map_err(|_| SinkError::AllocationFailed)?;
        result.push(QueryCandidate {
            start: 0,
            end: self.source.len(),
            role: StructuralRole::Signature,
            syntax: "javascript.const_receiver_clean_file",
            required: true,
            native_depth: 0,
        });
        let mut proofs = Vec::new();
        proofs
            .try_reserve_exact(self.proofs.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for proof in self.proofs.values() {
            cancel.check()?;
            proofs.push(*proof);
        }
        for proof in proofs {
            for (node, role, syntax) in [
                (
                    proof.receiver,
                    StructuralRole::Reference,
                    "javascript.const_receiver",
                ),
                (
                    proof.binding,
                    StructuralRole::Signature,
                    "javascript.const_receiver_binding",
                ),
                (
                    proof.call,
                    StructuralRole::Signature,
                    "javascript.const_receiver_member_call",
                ),
                (
                    proof.assignment,
                    StructuralRole::Signature,
                    "javascript.const_receiver_owner",
                ),
            ] {
                let depth = self.depth(node, cancel)?;
                result.push(QueryCandidate {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    role,
                    syntax,
                    required: true,
                    native_depth: depth,
                });
            }
            result.push(QueryCandidate {
                start: proof.definition.start_byte(),
                end: proof.receiver.end_byte(),
                role: StructuralRole::Signature,
                syntax: "javascript.const_receiver_target",
                required: true,
                native_depth: 0,
            });
        }
        for index in 0..self.nonbinding.len() {
            let node = self.nonbinding[index];
            let depth = self.depth(node, cancel)?;
            result.push(QueryCandidate {
                start: node.start_byte(),
                end: node.end_byte(),
                role: StructuralRole::Signature,
                syntax: "javascript.const_receiver_nonbinding",
                required: true,
                native_depth: depth,
            });
        }
        if self.exhausted {
            result.clear();
        }
        Ok(result)
    }

    /// Refines only the exact selected receiver, preserving its native dedup depth.
    ///
    /// # Errors
    /// Returns cancellation or fallible census allocation/accounting errors.
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
        if !self.exhausted
            && role == StructuralRole::Reference
            && self
                .proofs
                .get(&node.end_byte())
                .is_some_and(|proof| proof.receiver == node)
        {
            let depth = self.depth(node, cancel)?;
            if !self.exhausted {
                candidate.syntax = "javascript.const_receiver";
                candidate.required = true;
                candidate.native_depth = depth;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(source: &[u8], maximum: usize, complete: bool) -> (Vec<QueryCandidate>, bool) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(source, tree.root_node(), maximum, complete);
        let result = cache
            .candidates(
                tree.root_node(),
                StructuralRole::Signature,
                &Cancellation::new(),
            )
            .unwrap();
        (result, cache.exhausted())
    }

    #[test]
    fn actual_tsx_program_certifies_original_receiver_and_explicit_same_spelling_key() {
        let source =
            include_bytes!("../../tests/fixtures/javascript_const_receiver/webpack.common.js");
        let (rows, exhausted) = candidates(source, 16_384, true);
        assert!(!exhausted);
        assert_eq!(
            rows.iter()
                .filter(|row| row.syntax == "javascript.const_receiver"
                    && row.start == 112
                    && row.end == 116
                    && row.role == StructuralRole::Reference
                    && row.required)
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.syntax == "javascript.const_receiver_nonbinding"
                    && row.start == 106
                    && row.end == 110
                    && row.required)
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.syntax == "javascript.const_receiver_clean_file"
                    && row.start == 0
                    && row.end == 199)
                .count(),
            1
        );
        for source in [
            "const value=1; target.if={class:value.return()};",
            "const from=1; of={default:from.class()};",
            "const of=1; from={while:of.if()};",
        ] {
            let (rows, exhausted) = candidates(source.as_bytes(), 16_384, true);
            assert!(!exhausted, "{source}");
            assert_eq!(
                rows.iter()
                    .filter(|row| row.syntax == "javascript.const_receiver"
                        && row.role == StructuralRole::Reference
                        && row.required)
                    .count(),
                1,
                "{source}"
            );
        }
    }

    #[test]
    fn work_exhaustion_and_partial_context_never_publish_a_partial_receiver_partition() {
        let source = b"const value=1; target={key:value.use(),other:value.other()};";
        for maximum in 0..1024 {
            let (rows, exhausted) = candidates(source, maximum, true);
            if exhausted {
                assert!(rows.is_empty(), "maximum={maximum}");
            } else {
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.syntax == "javascript.const_receiver")
                        .count(),
                    2
                );
            }
        }
        assert!(candidates(source, 16_384, false).0.is_empty());
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, tree.root_node(), 16_384, true);
        assert!(matches!(
            cache.candidates(tree.root_node(), StructuralRole::Signature, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn optional_computed_shadowed_and_dynamic_owner_shapes_refuse_without_guessing() {
        for source in [
            "const value=1; target={value:value?.use()};",
            "const value=1; target={value:value['use']()};",
            "const value=1; target={value:value.inner.use()};",
            "const value=1; target={value:flag?value.use():other};",
            "const value=1; value: target={key:value.use()};",
            "const value=1; const value=2; target={key:value.use()};",
            "const value=1; target={key:value.use()}; ({value}=other);",
            "const value=1; target={key:value.use()}; const fn=()=>value;",
            "const value=eval('x'); target={key:value.use()};",
            "const value=1; target={key:<Tag value={value}/>};",
            "const value:number=1; target={key:value.use()};",
        ] {
            assert!(
                candidates(source.as_bytes(), 16_384, true).0.is_empty(),
                "{source}"
            );
        }
        for name in [
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "debugger",
            "default",
            "delete",
            "do",
            "else",
            "enum",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "let",
            "new",
            "null",
            "return",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "typeof",
            "var",
            "void",
            "while",
            "with",
            "yield",
            "implements",
            "interface",
            "package",
            "private",
            "protected",
            "public",
            "static",
        ] {
            let source = format!("const {name}=1; target={{key:{name}.use()}};");
            assert!(
                candidates(source.as_bytes(), 16_384, true).0.is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn identity_work_refusal_names_the_javascript_receiver_helper() {
        use crate::{GrammarFamily, query_pack::QueryPackRegistry};

        let source = b"const value=1; target={key:value.use()};";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let registry = QueryPackRegistry::audited().unwrap();
        let error = registry
            .get(GrammarFamily::JavaScript)
            .unwrap()
            .extract_identity(
                GrammarFamily::JavaScript,
                &tree,
                source,
                8,
                &Cancellation::new(),
            )
            .unwrap_err();
        assert!(
            matches!(error,AdapterError::ProviderFailed { code } if code.as_str()=="query-identity-javascript-const-receivers-limit")
        );
    }

    #[test]
    fn native_hashbang_receiver_partition_requires_the_actual_supported_carrier_boundary() {
        for separator in ["\n", "\r\n"] {
            let text = format!("#!tool{separator}const value=1; target={{key:value.use()}};\n");
            let (rows, exhausted) = candidates(text.as_bytes(), 16_384, true);
            assert!(!exhausted);
            assert_eq!(
                rows.iter()
                    .filter(|row| row.syntax == "javascript.const_receiver")
                    .count(),
                1,
                "{text:?}"
            );
        }
        for separator in ["\r", "\u{2028}", "\u{2029}"] {
            let text = format!(
                "#!tool{separator}with(other) {{}}\nconst value=1; target={{key:value.use()}};\n"
            );
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                .unwrap();
            let tree = parser.parse(text.as_bytes(), None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "carrier refusal must not rely on a parse error"
            );
            let (rows, exhausted) = candidates(text.as_bytes(), 16_384, true);
            assert!(!exhausted);
            assert!(rows.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn native_hashbang_carrier_work_and_cancellation_leave_no_partial_certificate() {
        let mut source = b"#!".to_vec();
        source.extend(std::iter::repeat_n(b'x', 2048));
        source.extend_from_slice(b"\nconst value=1; target={key:value.use()};\n");
        let (rows, exhausted) = candidates(&source, 64, true);
        assert!(exhausted);
        assert!(rows.is_empty());
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(&source, tree.root_node(), 16_384, true);
        assert!(matches!(
            cache.candidates(tree.root_node(), StructuralRole::Signature, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
