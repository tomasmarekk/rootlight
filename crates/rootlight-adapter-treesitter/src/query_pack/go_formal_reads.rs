//! Certify only direct formal values in one clean returned Go composite literal.
//! Keys and all wider scopes retain their previous labels; this proof establishes
//! lexical binding identity without type, value or runtime interpretation.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

mod call_arguments;

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    functions: HashMap<usize, Option<HashSet<(usize, usize)>>>,
    arguments: HashMap<usize, Option<HashSet<(usize, usize)>>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining,
            exhausted: false,
            functions: HashMap::new(),
            arguments: HashMap::new(),
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

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.visit(cancel)?
            || node.kind() != "identifier"
            || node.is_missing()
            || node.has_error()
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
        if raw == b"_"
            || !(first.is_ascii_alphabetic() || first == b'_')
            || !rest
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return Ok(None);
        }
        Ok(std::str::from_utf8(raw).ok())
    }

    fn only<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            if found.is_some() {
                return Ok(None);
            }
            found = Some(child);
        }
        Ok(found)
    }

    fn scalar(node: Node<'_>) -> bool {
        matches!(
            node.kind(),
            "int_literal"
                | "float_literal"
                | "imaginary_literal"
                | "rune_literal"
                | "interpreted_string_literal"
                | "raw_string_literal"
                | "true"
                | "false"
                | "nil"
        )
    }

    fn inspect(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<HashSet<(usize, usize)>>, AdapterError> {
        if !self.complete
            || !self.visit(cancel)?
            || function.kind() != "function_declaration"
            || function.is_missing()
            || function.has_error()
            || function.child_by_field_name("type_parameters").is_some()
            || function
                .child_by_field_name("result")
                .is_some_and(|result| result.kind() == "parameter_list")
            || function
                .parent()
                .is_none_or(|parent| parent.kind() != "source_file")
        {
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
            let mut count = 0usize;
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
            .filter(|body| body.kind() == "block")
        else {
            return Ok(None);
        };
        let Some(statements) = self
            .only(body, cancel)?
            .filter(|node| node.kind() == "statement_list")
        else {
            return Ok(None);
        };
        let Some(statement) = self
            .only(statements, cancel)?
            .filter(|node| node.kind() == "return_statement")
        else {
            return Ok(None);
        };
        let Some(expressions) = self
            .only(statement, cancel)?
            .filter(|node| node.kind() == "expression_list")
        else {
            return Ok(None);
        };
        let Some(mut literal) = self.only(expressions, cancel)? else {
            return Ok(None);
        };
        if literal.kind() == "unary_expression" {
            if literal
                .child_by_field_name("operator")
                .is_none_or(|operator| operator.kind() != "&")
            {
                return Ok(None);
            }
            let Some(operand) = literal.child_by_field_name("operand") else {
                return Ok(None);
            };
            literal = operand;
        }
        if literal.kind() != "composite_literal" {
            return Ok(None);
        }
        let Some(values) = literal
            .child_by_field_name("body")
            .filter(|node| node.kind() == "literal_value")
        else {
            return Ok(None);
        };
        let mut proofs = HashSet::new();
        let mut cursor = values.walk();
        for element in values.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if element.kind() == "comment" {
                continue;
            }
            let value = match element.kind() {
                "literal_element" => element,
                "keyed_element" => {
                    let Some(key) = element.child_by_field_name("key") else {
                        return Ok(None);
                    };
                    let Some(key) = self.only(key, cancel)? else {
                        return Ok(None);
                    };
                    if key.kind() == "identifier" {
                        if self.name(key, cancel)?.is_none() {
                            return Ok(None);
                        }
                    } else if !Self::scalar(key) {
                        return Ok(None);
                    }
                    let Some(value) = element.child_by_field_name("value") else {
                        return Ok(None);
                    };
                    value
                }
                _ => return Ok(None),
            };
            let Some(value) = self.only(value, cancel)? else {
                return Ok(None);
            };
            if value.kind() == "identifier" {
                let Some(name) = self.name(value, cancel)? else {
                    return Ok(None);
                };
                if names.contains(name) {
                    proofs
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    proofs.insert((value.start_byte(), value.end_byte()));
                }
            } else if !Self::scalar(value) {
                return Ok(None);
            }
        }
        Ok(Some(proofs))
    }

    /// Refine only spans certified by the complete owning function census.
    /// The caller checks work exhaustion before retaining a capture.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if let Some(keep) = self.refine_arguments(node, role, candidate, cancel)? {
            return Ok(keep);
        }
        if role == StructuralRole::Signature
            && matches!(node.kind(), "function_declaration" | "method_declaration")
        {
            candidate.syntax = "go.composite_formal_context";
            candidate.required = true;
            return Ok(true);
        }
        if role != StructuralRole::Reference || node.kind() != "identifier" {
            return Ok(true);
        }
        let mut current = node;
        let function = loop {
            if !self.visit(cancel)? {
                return Ok(true);
            }
            let Some(parent) = current.parent() else {
                return Ok(true);
            };
            if parent.kind() == "func_literal" {
                return Ok(true);
            }
            if matches!(parent.kind(), "function_declaration" | "method_declaration") {
                break parent;
            }
            current = parent;
        };
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(true);
        };
        if node.start_byte() < body.start_byte() || node.end_byte() > body.end_byte() {
            return Ok(true);
        }
        if !self.functions.contains_key(&function.id()) {
            let proof = self.inspect(function, cancel)?;
            self.functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.functions.insert(function.id(), proof);
        }
        if self
            .functions
            .get(&function.id())
            .and_then(Option::as_ref)
            .is_some_and(|proofs| proofs.contains(&(node.start_byte(), node.end_byte())))
        {
            candidate.syntax = "go.composite_formal_value";
        }
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn at<'tree>(
        tree: &'tree tree_sitter::Tree,
        source: &[u8],
        needle: &[u8],
        offset: usize,
        length: usize,
    ) -> Node<'tree> {
        let start = source
            .windows(needle.len())
            .position(|part| part == needle)
            .unwrap()
            + offset;
        tree.root_node()
            .descendant_for_byte_range(start, start + length)
            .unwrap()
    }

    fn candidate(node: Node<'_>) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role: StructuralRole::Reference,
            syntax: "go.identifier",
            required: false,
            native_depth: 0,
        }
    }

    #[test]
    fn native_composite_values_select_the_formal_rhs_and_leave_keys_unknown() {
        let source = b"package sample\nfunc owned(data []byte) *Box { return &Box{data: data, position: 0} }\n";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let mut cache = CaptureCache::new(source, 4096, true);
        for (offset, syntax) in [(0, "go.identifier"), (6, "go.composite_formal_value")] {
            let node = at(&tree, source, b"data: data", offset, 4);
            let mut output = candidate(node);
            cache
                .refine(
                    node,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(output.syntax, syntax);
        }
    }

    #[test]
    fn duplicate_receiver_results_closure_recovery_and_dynamic_values_refuse_the_proof() {
        for source in [
            "package sample\nfunc owned(data, data []byte) *Box { return &Box{data: data} }\n",
            "package sample\nfunc (receiver Box) owned(data []byte) *Box { return &Box{data: data} }\n",
            "package sample\nfunc owned(data []byte) (result *Box) { return &Box{data: data} }\n",
            "package sample\nfunc owned(data []byte) *Box { copy := data; return &Box{data: data} }\n",
            "package sample\nfunc owned(data []byte) *Box { return &Box{data: func() []byte { return data }()} }\n",
            "package sample\nfunc owned(data []byte) *Box { return &Box{data: external(data)} }\n",
            "package sample\nfunc owned(data []byte,, other int) *Box { return &Box{data: data} }\n",
        ] {
            let tree = parse(source.as_bytes());
            let start = source.rfind("data").unwrap();
            let node = tree
                .root_node()
                .descendant_for_byte_range(start, start + 4)
                .unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), 4096, true);
            let mut output = candidate(node);
            cache
                .refine(
                    node,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(output.syntax, "go.identifier", "{source}");
        }
    }

    #[test]
    fn source_partition_work_and_typed_cancellation_remain_visible() {
        let source = b"package sample\nfunc owned(data []byte) *Box { return &Box{data: data} }\n";
        let tree = parse(source);
        let node = at(&tree, source, b"data: data", 6, 4);
        let mut output = candidate(node);
        let mut cache = CaptureCache::new(source, 0, true);
        cache
            .refine(
                node,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(cache.exhausted());
        assert_eq!(output.syntax, "go.identifier");
        let mut partial = CaptureCache::new(source, 4096, false);
        partial
            .refine(
                node,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.syntax, "go.identifier");
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            partial.refine(node, StructuralRole::Reference, &mut output, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn a_later_function_work_refusal_precedes_candidate_retention() {
        let source = b"package sample\nfunc first(left int) Box { return Box{value: left} }\nfunc second(right int) Box { return Box{value: right} }\n";
        let tree = parse(source);
        let first = at(&tree, source, b"value: left", 7, 4);
        let second = at(&tree, source, b"value: right", 7, 5);
        let mut calibration = CaptureCache::new(source, 4096, true);
        let mut output = candidate(first);
        calibration
            .refine(
                first,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.syntax, "go.composite_formal_value");
        let used = 4096 - calibration.remaining;
        let mut bounded = CaptureCache::new(source, used, true);
        let mut output = candidate(first);
        bounded
            .refine(
                first,
                StructuralRole::Reference,
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.syntax, "go.composite_formal_value");
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
        assert_eq!(refused.syntax, "go.identifier");
    }
}
