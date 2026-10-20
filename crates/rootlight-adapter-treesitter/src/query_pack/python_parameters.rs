//! Certify lexical formal references with a complete native function census.
//! This is lexical binding evidence, not execution, value or decorator analysis.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    remaining: usize,
    complete: bool,
    exhausted: bool,
    parameters: HashMap<usize, Option<HashSet<&'source str>>>,
}

impl<'source> CaptureCache<'source> {
    /// Create a per-scan cache with a shared finite work allowance.
    /// A partial native source context cannot certify a function binding.
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            remaining,
            complete,
            exhausted: false,
            parameters: HashMap::new(),
        }
    }

    /// Report exhausted native proof work before the query loop retains output.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let mut bytes = raw.iter().copied();
        let Some(first) = bytes.next() else {
            return Ok(None);
        };
        // Non-ASCII names retain the old unknown path until Python NFKC symbol
        // equivalence is modeled. Raw spelling equality is insufficient there.
        if !(first.is_ascii_alphabetic() || first == b'_')
            || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Ok(None);
        }
        Ok(std::str::from_utf8(raw).ok())
    }

    fn parameter_name<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.visit(cancel)? || node.is_missing() || node.has_error() {
            return Ok(None);
        }
        match node.kind() {
            "identifier" => Ok(Some(node)),
            "default_parameter" | "typed_default_parameter" => Ok(node
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")),
            "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {
                let mut found = None;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if !self.visit(cancel)? {
                        return Ok(None);
                    }
                    if child.kind() == "type" || child.kind() == "comment" {
                        continue;
                    }
                    if found.is_some() {
                        return Ok(None);
                    }
                    found = self.parameter_name(child, cancel)?;
                    if found.is_none() {
                        return Ok(None);
                    }
                }
                Ok(found)
            }
            _ => Ok(None),
        }
    }

    fn inspect(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<HashSet<&'source str>>, AdapterError> {
        if !self.complete
            || function.has_error()
            || function.is_missing()
            || function.child_by_field_name("type_parameters").is_some()
        {
            return Ok(None);
        }
        let Some(parent) = function.parent() else {
            return Ok(None);
        };
        // Decorators and nested lexical functions are outside this first slice.
        if parent.kind() != "module"
            && !(parent.kind() == "block"
                && parent
                    .parent()
                    .is_some_and(|p| p.kind() == "class_definition"))
        {
            return Ok(None);
        }
        let method = parent.kind() == "block";
        let private_name = |name: &str| method && name.starts_with("__") && !name.ends_with("__");
        let mut cursor = function.walk();
        for _ in function.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
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
            if matches!(
                parameter.kind(),
                "comment" | "positional_separator" | "keyword_separator"
            ) {
                continue;
            }
            let Some(node) = self.parameter_name(parameter, cancel)? else {
                return Ok(None);
            };
            let Some(name) = self.name(node, cancel)? else {
                return Ok(None);
            };
            // Python mangles class-private names before binding; raw ASCII
            // equality alone cannot detect collisions with the expanded spelling.
            if private_name(name) {
                return Ok(None);
            }
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !names.insert(name) {
                return Ok(None);
            }
        }
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(None);
        };
        let mut cursor = body.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let node = cursor.node();
            if node.is_missing()
                || node.is_error()
                || (node.is_named()
                    && !matches!(
                        node.kind(),
                        "block"
                            | "assignment"
                            | "augmented_assignment"
                            | "if_statement"
                            | "elif_clause"
                            | "else_clause"
                            | "for_statement"
                            | "call"
                            | "argument_list"
                            | "keyword_argument"
                            | "attribute"
                            | "lambda"
                            | "yield"
                            | "await"
                            | "return_statement"
                            | "raise_statement"
                            | "expression_statement"
                            | "pass_statement"
                            | "identifier"
                            | "integer"
                            | "float"
                            | "true"
                            | "false"
                            | "none"
                            | "binary_operator"
                            | "unary_operator"
                            | "boolean_operator"
                            | "not_operator"
                            | "comparison_operator"
                            | "conditional_expression"
                            | "parenthesized_expression"
                            | "expression_list"
                            | "tuple"
                            | "list"
                            | "set"
                            | "dictionary"
                            | "pair"
                            | "string"
                            | "string_start"
                            | "string_content"
                            | "string_end"
                            | "escape_sequence"
                            | "comment"
                    ))
            {
                // Unmodeled binders and scopes keep the entire formal proof
                // unavailable. Plain local targets are checked separately below.
                return Ok(None);
            }
            let skip_children = node.kind() == "lambda";
            if skip_children
                && node
                    .child_by_field_name("parameters")
                    .is_some_and(|parameters| parameters.named_child_count() != 0)
            {
                // Defaults can evaluate in the outer scope. Only an empty
                // parameter list has no unreviewed outer binding operation.
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "assignment" | "augmented_assignment" | "for_statement"
            ) {
                let Some(target) = node.child_by_field_name("left") else {
                    return Ok(None);
                };
                if node.child_by_field_name("type").is_some() {
                    return Ok(None);
                }
                if node.kind() == "assignment" && target.kind() == "attribute" {
                    let mut object = target;
                    while object.kind() == "attribute" {
                        if !self.visit(cancel)? {
                            return Ok(None);
                        }
                        let Some(parent) = object.child_by_field_name("object") else {
                            return Ok(None);
                        };
                        object = parent;
                    }
                    if object.kind() != "identifier" || self.name(object, cancel)?.is_none() {
                        return Ok(None);
                    }
                } else {
                    if target.kind() != "identifier" {
                        return Ok(None);
                    }
                    let Some(written) = self.name(target, cancel)? else {
                        return Ok(None);
                    };
                    if names.contains(written) || private_name(written) {
                        return Ok(None);
                    }
                }
            }
            if node.kind() == "call" {
                let Some(head) = node.child_by_field_name("function") else {
                    return Ok(None);
                };
                if !matches!(head.kind(), "identifier" | "attribute") {
                    return Ok(None);
                }
                if head.kind() == "identifier"
                    && self
                        .name(head, cancel)?
                        .is_some_and(|name| names.contains(name))
                {
                    // Calling a formal is outside this reference-only slice.
                    return Ok(None);
                }
            }
            if node.kind() == "identifier" {
                let Some(name) = self.name(node, cancel)? else {
                    return Ok(None);
                };
                if private_name(name)
                    || matches!(name, "exec" | "eval" | "locals" | "globals" | "vars")
                {
                    return Ok(None);
                }
            }
            // Empty lambdas own a separate lexical body. Their contents stay
            // opaque; refine() already refuses every identifier inside them.
            if !skip_children && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(Some(names));
                }
            }
        }
    }

    /// Refine only complete native body reads; all other captures keep old roles.
    /// Work exhaustion is reported to the query loop before any candidate is kept.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "function_definition" {
            candidate.syntax = "python.parameter_binding_context";
            candidate.required = true;
            return Ok(());
        }
        if role != StructuralRole::Reference || node.kind() != "identifier" {
            return Ok(());
        }
        if node.parent().is_some_and(|parent| {
            let field = match parent.kind() {
                "attribute" => "attribute",
                "keyword_argument" => "name",
                _ => return false,
            };
            parent
                .child_by_field_name(field)
                .is_some_and(|selected| selected.id() == node.id())
        }) {
            return Ok(());
        }
        let mut current = node;
        let function = loop {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let Some(parent) = current.parent() else {
                return Ok(());
            };
            if matches!(
                parent.kind(),
                "class_definition"
                    | "lambda"
                    | "list_comprehension"
                    | "set_comprehension"
                    | "dictionary_comprehension"
                    | "generator_expression"
            ) {
                return Ok(());
            }
            if parent.kind() == "function_definition" {
                break parent;
            }
            current = parent;
        };
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(());
        };
        if node.start_byte() < body.start_byte() || node.end_byte() > body.end_byte() {
            return Ok(());
        }
        if !self.parameters.contains_key(&function.id()) {
            let inspected = self.inspect(function, cancel)?;
            self.parameters
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.parameters.insert(function.id(), inspected);
        }
        let Some(name) = self.name(node, cancel)? else {
            return Ok(());
        };
        if self
            .parameters
            .get(&function.id())
            .and_then(Option::as_ref)
            .is_some_and(|names| names.contains(name))
        {
            candidate.syntax = "python.parameter_body";
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_parameter_proof_obeys_work_and_typed_cancellation() {
        let source = b"def written(value: int):\n    return value\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let function = tree.root_node().named_child(0).unwrap();
        let mut bounded = CaptureCache::new(source, 0, true);
        assert!(
            bounded
                .inspect(function, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(bounded.exhausted());
        let mut complete = CaptureCache::new(source, 256, true);
        assert!(
            complete
                .inspect(function, &Cancellation::new())
                .unwrap()
                .unwrap()
                .contains("value")
        );
        let mut partition = CaptureCache::new(source, 256, false);
        assert!(
            partition
                .inspect(function, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            complete.inspect(function, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}

#[cfg(test)]
mod late_work_qualification_tests {
    use super::*;

    #[test]
    fn a_later_native_work_refusal_is_visible_before_candidate_retention() {
        let source = b"def owned(value):\n    return value + value\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let first_offset = source
            .windows(6)
            .rposition(|part| part == b"value ")
            .unwrap();
        let second_offset = source
            .windows(6)
            .rposition(|part| part == b"value\n")
            .unwrap();
        let first = tree
            .root_node()
            .descendant_for_byte_range(first_offset, first_offset + 5)
            .unwrap();
        let second = tree
            .root_node()
            .descendant_for_byte_range(second_offset, second_offset + 5)
            .unwrap();
        assert_eq!(first.kind(), "identifier");
        assert_eq!(second.kind(), "identifier");
        let candidate = |node: Node<'_>| QueryCandidate {
            role: StructuralRole::Reference,
            syntax: "identifier",
            start: node.start_byte(),
            end: node.end_byte(),
            native_depth: 0,
            required: false,
        };
        let mut calibration = CaptureCache::new(source, 4096, true);
        let mut reference = candidate(first);
        calibration
            .refine(
                first,
                StructuralRole::Reference,
                &mut reference,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(reference.syntax, "python.parameter_body");
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
        assert_eq!(accepted.syntax, "python.parameter_body");
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
        assert_eq!(refused.syntax, "identifier");
    }
}

#[cfg(test)]
mod expanded_body_work_tests {
    use super::*;

    #[test]
    fn expanded_async_native_census_obeys_small_work_and_partial_context() {
        let source = b"async def written(value):\n    local = external(value)\n    for item in values:\n        if item:\n            yield value\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let function = tree.root_node().named_child(0).unwrap();
        let mut accepted = CaptureCache::new(source, 4096, true);
        assert!(
            accepted
                .inspect(function, &Cancellation::new())
                .unwrap()
                .unwrap()
                .contains("value")
        );
        let used = 4096 - accepted.remaining;
        assert!(used > 1);
        let mut limited = CaptureCache::new(source, used - 1, true);
        assert!(
            limited
                .inspect(function, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(limited.exhausted());
        let mut partition = CaptureCache::new(source, 4096, false);
        assert!(
            partition
                .inspect(function, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            accepted.inspect(function, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
