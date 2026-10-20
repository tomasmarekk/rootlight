//! Certify the RHS formal in a clean single-assignment Java setter body.
//! This bounded lexical proof does not resolve fields, general method bodies,
//! overloads or execution; every other reference keeps its prior syntax label.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

#[derive(Clone, Copy)]
struct ReadProof<'source> {
    start: usize,
    end: usize,
    name: &'source str,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    source_checked: Option<bool>,
    methods: HashMap<usize, Option<ReadProof<'source>>>,
}

impl<'source> CaptureCache<'source> {
    /// Share a finite proof allowance across one complete native source scan.
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining,
            exhausted: false,
            source_checked: None,
            methods: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn source_is_direct(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if let Some(direct) = self.source_checked {
            return Ok(direct);
        }
        if !self.complete {
            self.source_checked = Some(false);
            return Ok(false);
        }
        // Java Unicode escapes are translated before tokenization. A raw CST
        // cannot certify their lexical effect, including escapes in comments.
        for start in (0..self.source.len()).step_by(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            let end = start.saturating_add(65).min(self.source.len());
            if self.source[start..end]
                .windows(2)
                .any(|pair| pair == b"\\u")
            {
                self.source_checked = Some(false);
                return Ok(false);
            }
        }
        self.source_checked = Some(true);
        Ok(true)
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.visit(cancel)?
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
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Some((&first, rest)) = raw.split_first() else {
            return Ok(None);
        };
        if !(first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
            || !rest
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'$'))
        {
            return Ok(None);
        }
        Ok(std::str::from_utf8(raw).ok())
    }

    fn only_statement<'tree>(
        &mut self,
        body: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut statement = None;
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(child.kind(), "line_comment" | "block_comment") {
                continue;
            }
            if statement.is_some() || child.kind() != "expression_statement" {
                return Ok(None);
            }
            statement = Some(child);
        }
        Ok(statement)
    }

    fn inspect(
        &mut self,
        method: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<ReadProof<'source>>, AdapterError> {
        if !self.source_is_direct(cancel)?
            || !self.visit(cancel)?
            || method.kind() != "method_declaration"
            || method.has_error()
            || method.is_missing()
            || method.child_by_field_name("type_parameters").is_some()
        {
            return Ok(None);
        }
        let Some(body) = method
            .child_by_field_name("body")
            .filter(|node| node.kind() == "block")
        else {
            return Ok(None);
        };
        let Some(statement) = self.only_statement(body, cancel)? else {
            return Ok(None);
        };
        let Some(assignment) = statement.named_child(0).filter(|node| {
            statement.named_child_count() == 1 && node.kind() == "assignment_expression"
        }) else {
            return Ok(None);
        };
        if assignment
            .child_by_field_name("operator")
            .is_none_or(|node| node.kind() != "=")
        {
            return Ok(None);
        }
        let Some(left) = assignment
            .child_by_field_name("left")
            .filter(|node| node.kind() == "field_access")
        else {
            return Ok(None);
        };
        if left
            .child_by_field_name("object")
            .is_none_or(|node| node.kind() != "this")
            || left
                .child_by_field_name("field")
                .is_none_or(|node| node.kind() != "identifier")
        {
            return Ok(None);
        }
        let Some(right) = assignment.child_by_field_name("right") else {
            return Ok(None);
        };
        let Some(name) = self.name(right, cancel)? else {
            return Ok(None);
        };
        let Some(parameters) = method.child_by_field_name("parameters") else {
            return Ok(None);
        };
        let mut names = HashSet::new();
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(parameter.kind(), "line_comment" | "block_comment") {
                continue;
            }
            if parameter.kind() != "formal_parameter"
                || parameter.has_error()
                || parameter.is_missing()
            {
                return Ok(None);
            }
            let Some(identifier) = parameter.child_by_field_name("name") else {
                return Ok(None);
            };
            let Some(formal) = self.name(identifier, cancel)? else {
                return Ok(None);
            };
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !names.insert(formal) {
                return Ok(None);
            }
        }
        Ok(names.contains(name).then_some(ReadProof {
            start: right.start_byte(),
            end: right.end_byte(),
            name,
        }))
    }

    /// Refine only the certified RHS; required context markers retain scope limits.
    /// Exhaustion is checked by the caller before candidate retention.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "method_declaration" {
            candidate.syntax = "java.setter_binding_context";
            candidate.required = true;
            return Ok(());
        }
        if role != StructuralRole::Reference || node.kind() != "identifier" {
            return Ok(());
        }
        let Some(assignment) = node.parent().filter(|parent| {
            parent.kind() == "assignment_expression"
                && parent.child_by_field_name("right") == Some(node)
        }) else {
            return Ok(());
        };
        let Some(statement) = assignment
            .parent()
            .filter(|node| node.kind() == "expression_statement")
        else {
            return Ok(());
        };
        let Some(body) = statement.parent().filter(|node| node.kind() == "block") else {
            return Ok(());
        };
        let Some(method) = body.parent().filter(|node| {
            node.kind() == "method_declaration" && node.child_by_field_name("body") == Some(body)
        }) else {
            return Ok(());
        };
        if !self.methods.contains_key(&method.id()) {
            let proof = self.inspect(method, cancel)?;
            self.methods
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.methods.insert(method.id(), proof);
        }
        if self
            .methods
            .get(&method.id())
            .copied()
            .flatten()
            .is_some_and(|proof| {
                proof.start == node.start_byte()
                    && proof.end == node.end_byte()
                    && self.source.get(node.byte_range()) == Some(proof.name.as_bytes())
            })
        {
            candidate.syntax = "java.setter_parameter";
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn at<'tree>(
        tree: &'tree tree_sitter::Tree,
        source: &[u8],
        needle: &[u8],
    ) -> Node<'tree> {
        let start = source
            .windows(needle.len())
            .rposition(|part| part == needle)
            .unwrap();
        let name = needle
            .iter()
            .rposition(|byte| *byte == b' ')
            .map_or(0, |index| index + 1);
        tree.root_node()
            .descendant_for_byte_range(start + name, start + needle.len())
            .unwrap()
    }

    pub(super) fn candidate(node: Node<'_>) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role: StructuralRole::Reference,
            syntax: "java.identifier",
            required: false,
            native_depth: 0,
        }
    }

    pub(super) fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn native_setter_certificate_selects_only_the_direct_formal_rhs() {
        let source =
            b"class Owner { String field; void set(String value) { this.field = value; } }";
        let tree = parse(source);
        let node = at(&tree, source, b"= value");
        assert_eq!(node.kind(), "identifier");
        let mut output = candidate(node);
        let mut cache = CaptureCache::new(source, 4096, true);
        cache
            .refine(
                node,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.syntax, "java.setter_parameter");
        let field_start = source
            .windows(b"this.field".len())
            .position(|part| part == b"this.field")
            .unwrap()
            + 5;
        let field = tree
            .root_node()
            .descendant_for_byte_range(field_start, field_start + 5)
            .unwrap();
        let mut field_output = candidate(field);
        cache
            .refine(
                field,
                StructuralRole::Reference,
                &mut field_output,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(field_output.syntax, "java.identifier");
    }

    #[test]
    fn native_setter_proof_rejects_other_scopes_recovery_duplicates_and_escaped_sources() {
        for source in [
            "class Owner { void set(String value) { String local = value; this.field = value; } }",
            "class Owner { void set(String value, String value) { this.field = value; } }",
            "class Owner { void first(String value) {} void second() { this.field = value; } }",
            "class Owner { String value; void set() { this.field = value; } }",
            "class Owner { void set(String value) { Runnable fn = () -> { this.field = value; }; } }",
            "class Owner { void set(String value) { class Nested { void inner() { this.field = value; } } } }",
            "class Owner { void set(String value,) { this.field = value; } }",
            "class Owner { void set(String value) { this.field = value; this.other = value; } }",
            "class Owner { void set(String value) { this.field += value; } }",
            "// \\u000a ambiguous preprocessing\nclass Owner { void set(String value) { this.field = value; } }",
        ] {
            let tree = parse(source.as_bytes());
            let node = at(&tree, source.as_bytes(), b" value");
            let mut output = candidate(node);
            let mut cache = CaptureCache::new(source.as_bytes(), 4096, true);
            cache
                .refine(
                    node,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(output.syntax, "java.identifier", "{source}");
        }
    }

    #[test]
    fn native_setter_proof_preserves_work_partition_and_typed_cancellation() {
        let source = b"class Owner { void set(String value) { this.field = value; } }";
        let tree = parse(source);
        let node = at(&tree, source, b"= value");
        let mut output = candidate(node);
        let mut bounded = CaptureCache::new(source, 0, true);
        bounded
            .refine(
                node,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(bounded.exhausted());
        assert_eq!(output.syntax, "java.identifier");
        let mut partial = CaptureCache::new(source, 4096, false);
        partial
            .refine(
                node,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.syntax, "java.identifier");
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            partial.refine(node, StructuralRole::Reference, &mut output, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}

#[cfg(test)]
mod late_work_qualification_tests {
    use super::{
        tests::{at, candidate, parse},
        *,
    };

    #[test]
    fn later_method_work_refusal_precedes_candidate_retention() {
        let source = b"class Owner { void one(String first) { this.field = first; } void two(String second) { this.field = second; } }";
        let tree = parse(source);
        let first = at(&tree, source, b"= first");
        let second = at(&tree, source, b"= second");
        let mut calibration = CaptureCache::new(source, 4096, true);
        let mut accepted = candidate(first);
        calibration
            .refine(
                first,
                StructuralRole::Reference,
                &mut accepted,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(accepted.syntax, "java.setter_parameter");
        let used = 4096 - calibration.remaining;
        assert!(used > 0);
        let mut bounded = CaptureCache::new(source, used, true);
        let mut accepted = candidate(first);
        bounded
            .refine(
                first,
                StructuralRole::Reference,
                &mut accepted,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(accepted.syntax, "java.setter_parameter");
        assert!(!bounded.exhausted());
        let mut refused = candidate(second);
        bounded
            .refine(
                second,
                StructuralRole::Reference,
                &mut refused,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(bounded.exhausted());
        assert_eq!(refused.syntax, "java.identifier");
    }
}
