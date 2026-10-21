//! Certify a written function formal used as a literal hashtable value.
//! The whole clean function is checked for rebinding and nested execution;
//! keys, commands, method targets and runtime values remain unproved.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

#[derive(Default)]
struct FunctionProof<'source> {
    formals: Vec<&'source [u8]>,
    values: HashSet<(usize, usize)>,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete_input: bool,
    initial: usize,
    remaining: usize,
    exhausted: bool,
    functions: HashMap<usize, FunctionProof<'source>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete_input: bool) -> Self {
        let initial = maximum
            .min(super::HARD_MAX_QUERY_CAPTURES)
            .saturating_mul(64);
        Self {
            source,
            complete_input,
            initial,
            remaining: initial,
            exhausted: false,
            functions: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(super) const fn initial_work(&self) -> usize {
        self.initial
    }

    pub(super) const fn remaining_work(&self) -> usize {
        self.remaining
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

    fn variable(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        if !self.step(cancel)? || node.kind() != "variable" {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        if raw.len() < 2 || raw[0] != b'$' {
            return Ok(None);
        }
        for (index, chunk) in raw.chunks(64).enumerate() {
            if !self.step(cancel)? {
                return Ok(None);
            }
            for (offset, byte) in chunk.iter().enumerate() {
                let valid = match (index, offset) {
                    (0, 0) => *byte == b'$',
                    (0, 1) => *byte == b'_' || byte.is_ascii_alphabetic(),
                    _ => *byte == b'_' || byte.is_ascii_alphanumeric(),
                };
                if !valid {
                    return Ok(None);
                }
            }
        }
        Ok(Some(raw))
    }

    fn matches_formal(
        &mut self,
        names: &[&[u8]],
        name: &[u8],
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for written in names {
            if written.len() != name.len() {
                if !self.step(cancel)? {
                    return Ok(false);
                }
                continue;
            }
            let mut equal = true;
            for (written, value) in written.chunks(64).zip(name.chunks(64)) {
                if !self.step(cancel)? {
                    return Ok(false);
                }
                equal &= written.eq_ignore_ascii_case(value);
            }
            if equal {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn owner<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut current = Some(node);
        while let Some(parent) = current {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if parent.kind() == "function_statement" {
                return Ok(Some(parent));
            }
            current = parent.parent();
        }
        Ok(None)
    }

    fn clean_context(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.complete_input || owner.has_error() || owner.is_missing() {
            return Ok(false);
        }
        let Some(list) = owner
            .parent()
            .filter(|node| node.kind() == "statement_list")
        else {
            return Ok(false);
        };
        let Some(root) = list.parent().filter(|node| node.kind() == "program") else {
            return Ok(false);
        };
        if root.parent().is_some() || root.has_error() || root.is_missing() {
            return Ok(false);
        }
        let Some(prefix) = self.source.get(..root.start_byte()) else {
            return Ok(false);
        };
        let Some(suffix) = self.source.get(root.end_byte()..) else {
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
        let mut children = owner.walk();
        for child in owner.children(&mut children) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if !child.is_named()
                && self.source.get(child.byte_range()).is_some_and(|raw| {
                    raw.eq_ignore_ascii_case(b"filter") || raw.eq_ignore_ascii_case(b"workflow")
                })
            {
                return Ok(false);
            }
            if child.kind() == "function_name" {
                let Some(raw) = self.source.get(child.byte_range()) else {
                    return Ok(false);
                };
                for chunk in raw.chunks(64) {
                    if !self.step(cancel)? || chunk.contains(&b':') {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    fn value_position(
        &mut self,
        leaf: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = leaf;
        while let Some(parent) = current.parent() {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if parent.kind() == "hash_entry" {
                let Some(key) = parent.named_child(0) else {
                    return Ok(false);
                };
                let mut literal = key;
                loop {
                    if !self.step(cancel)? {
                        return Ok(false);
                    }
                    if matches!(
                        literal.kind(),
                        "key_expression" | "unary_expression" | "string_literal"
                    ) && literal.named_child_count() == 1
                    {
                        literal = literal.named_child(0).unwrap();
                        continue;
                    }
                    return Ok(parent.named_child(1) == Some(current)
                        && super::powershell::is_literal_key(literal));
                }
            }
            if !matches!(
                parent.kind(),
                "unary_expression" | "array_literal_expression" | "pipeline_chain" | "pipeline"
            ) || parent.byte_range() != current.byte_range()
            {
                return Ok(false);
            }
            current = parent;
        }
        Ok(false)
    }

    fn assignment_target(
        &mut self,
        leaf: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = leaf;
        while let Some(parent) = current.parent() {
            if !self.step(cancel)? {
                return Ok(true);
            }
            if parent.kind() == "left_assignment_expression" {
                return Ok(true);
            }
            if parent.kind() != "array_literal_expression"
                && !(parent.kind() == "unary_expression"
                    && parent.byte_range() == current.byte_range())
            {
                return Ok(false);
            }
            current = parent;
        }
        Ok(false)
    }

    fn inspect(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<FunctionProof<'source>, AdapterError> {
        let mut proof = FunctionProof::default();
        if !self.clean_context(owner, cancel)? {
            return Ok(proof);
        }
        let mut children = owner.walk();
        let Some(header) = owner
            .named_children(&mut children)
            .find(|child| child.kind() == "function_parameter_declaration")
        else {
            return Ok(proof);
        };
        let Some(parameters) = header
            .named_child(0)
            .filter(|node| node.kind() == "parameter_list")
        else {
            return Ok(proof);
        };
        let mut children = parameters.walk();
        for parameter in parameters.named_children(&mut children) {
            if !self.step(cancel)? || parameter.kind() != "script_parameter" {
                return Ok(proof);
            }
            let mut parts = parameter.walk();
            let mut name = None;
            for part in parameter.named_children(&mut parts) {
                if !self.step(cancel)? {
                    return Ok(proof);
                }
                match part.kind() {
                    "variable" if name.is_none() => name = self.variable(part, cancel)?,
                    "attribute_list" => {
                        let mut attributes = part.walk();
                        for attribute in part.named_children(&mut attributes) {
                            if !self.step(cancel)?
                                || attribute.kind() != "attribute"
                                || attribute.named_child_count() != 1
                                || !attribute
                                    .named_child(0)
                                    .is_some_and(|node| node.kind() == "type_literal")
                            {
                                return Ok(proof);
                            }
                        }
                    }
                    _ => return Ok(proof),
                }
            }
            let Some(name) = name else { return Ok(proof) };
            if self.matches_formal(&proof.formals, name, cancel)? {
                return Ok(proof);
            }
            proof
                .formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            proof.formals.push(name);
        }
        if proof.formals.is_empty() {
            return Ok(proof);
        }
        let mut children = owner.walk();
        let Some(body) = owner
            .named_children(&mut children)
            .find(|node| node.kind() == "script_block")
        else {
            return Ok(proof);
        };
        let mut values = HashSet::new();
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.step(cancel)? || node.has_error() || node.is_missing() || node.is_error() {
                return Ok(proof);
            }
            if node.is_named() {
                match node.kind() {
                    "script_block"
                    | "script_block_body"
                    | "statement_list"
                    | "pipeline"
                    | "assignment_expression"
                    | "left_assignment_expression"
                    | "assignement_operator"
                    | "pipeline_chain"
                    | "array_literal_expression"
                    | "unary_expression"
                    | "invokation_expression"
                    | "argument_list"
                    | "argument_expression_list"
                    | "argument_expression"
                    | "logical_argument_expression"
                    | "bitwise_argument_expression"
                    | "comparison_argument_expression"
                    | "additive_argument_expression"
                    | "multiplicative_argument_expression"
                    | "format_argument_expression"
                    | "range_argument_expression"
                    | "type_literal"
                    | "type_spec"
                    | "type_name"
                    | "type_identifier"
                    | "simple_name"
                    | "integer_literal"
                    | "decimal_integer_literal"
                    | "string_literal"
                    | "expandable_string_literal"
                    | "verbatim_string_characters"
                    | "hash_literal_expression"
                    | "hash_literal_body"
                    | "hash_entry"
                    | "key_expression"
                    | "format_expression"
                    | "format_operator"
                    | "comment"
                    | "flow_control_statement" => {}
                    "expression_with_unary_operator" => {
                        if node.named_child_count() != 1
                            || !node
                                .named_child(0)
                                .is_some_and(|child| child.kind() == "cast_expression")
                        {
                            return Ok(proof);
                        }
                    }
                    "cast_expression" => {
                        let Some(value) = node
                            .named_child(1)
                            .filter(|child| child.kind() == "unary_expression")
                        else {
                            return Ok(proof);
                        };
                        if !value
                            .named_child(0)
                            .is_some_and(|child| child.kind() == "hash_literal_expression")
                        {
                            return Ok(proof);
                        }
                    }
                    "member_name" => {
                        if node.named_child_count() != 1
                            || !node
                                .named_child(0)
                                .is_some_and(|child| child.kind() == "simple_name")
                        {
                            return Ok(proof);
                        }
                    }
                    "variable" => {
                        let Some(name) = self.variable(node, cancel)? else {
                            return Ok(proof);
                        };
                        if self.matches_formal(&proof.formals, name, cancel)? {
                            if self.assignment_target(node, cancel)? {
                                return Ok(proof);
                            }
                            if self.value_position(node, cancel)? {
                                values
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                values.insert((node.start_byte(), node.end_byte()));
                            }
                        }
                    }
                    _ => return Ok(proof),
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
                    proof.values = values;
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
        let context = role == StructuralRole::Expression && node.kind() == "function_statement";
        if !context
            && !(node.kind() == "variable"
                && matches!(role, StructuralRole::Reference | StructuralRole::Signature))
        {
            return Ok(true);
        }
        let Some(owner) = self.owner(node, cancel)? else {
            return Ok(role != StructuralRole::Signature && !context);
        };
        if !self.functions.contains_key(&owner.id()) {
            let proof = self.inspect(owner, cancel)?;
            self.functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.functions.insert(owner.id(), proof);
        }
        let proven = self.functions.get(&owner.id()).is_some_and(|proof| {
            if context {
                !proof.values.is_empty()
            } else {
                proof.values.contains(&(node.start_byte(), node.end_byte()))
            }
        });
        if proven {
            candidate.role = if context {
                StructuralRole::Signature
            } else {
                StructuralRole::Reference
            };
            candidate.syntax = if context {
                "powershell.formal_context"
            } else {
                "powershell.formal_value"
            };
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature || context {
            return Ok(false);
        }
        candidate.syntax = "powershell.unproven_formal";
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_powershell::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn candidate(node: Node<'_>, role: StructuralRole) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "powershell.identifier",
            required: false,
            native_depth: 0,
        }
    }

    #[test]
    fn original_literal_value_uses_separate_required_reference_and_context() {
        let source = include_bytes!(
            "../../tests/fixtures/powershell_formal/Get-StrategyReportCsvStatistics.ps1"
        );
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let value = tree
            .root_node()
            .descendant_for_byte_range(927, 937)
            .unwrap();
        assert_eq!(value.kind(), "variable");
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, 16_384, true);
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let mut output = candidate(value, role);
            assert!(cache.refine(value, role, &mut output, &cancel).unwrap());
            assert!(output.required);
            assert_eq!(output.role, StructuralRole::Reference);
            assert_eq!(output.syntax, "powershell.formal_value");
            assert_eq!((output.start, output.end), (927, 937));
        }
        let owner = cache.owner(value, &cancel).unwrap().unwrap();
        let mut context = candidate(owner, StructuralRole::Expression);
        assert!(
            cache
                .refine(owner, StructuralRole::Expression, &mut context, &cancel)
                .unwrap()
        );
        assert_eq!(context.role, StructuralRole::Signature);
        assert_eq!(context.syntax, "powershell.formal_context");
        assert!(context.required);
        assert_eq!((context.start, context.end), (668, 1022));
    }

    #[test]
    fn rebinding_defaults_dynamic_execution_and_nested_scope_keep_optional_evidence() {
        for source in [
            "function Owned($value) { $VALUE = 0; return @{ Key = $value } }\n",
            "function Owned($value) { $value,$other = 1,2; return @{ Key = $value } }\n",
            "function Owned($value) { ++$value; return @{ Key = $value } }\n",
            "function Owned($value) { Set-Variable value 0; return @{ Key = $value } }\n",
            "function Owned($value) { . ./other.ps1; return @{ Key = $value } }\n",
            "function Owned($value) { return { param($other) @{ Key = $value } } }\n",
            "function Owned($value = 1) { return @{ Key = $value } }\n",
            "function Owned($value,$VALUE) { return @{ Key = $value } }\n",
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "{source}");
            let start = source.rfind("$value").unwrap();
            let leaf = tree
                .root_node()
                .descendant_for_byte_range(start, start + 6)
                .unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            let mut optional = candidate(leaf, StructuralRole::Reference);
            assert!(
                cache
                    .refine(
                        leaf,
                        StructuralRole::Reference,
                        &mut optional,
                        &Cancellation::new()
                    )
                    .unwrap()
            );
            assert_eq!(optional.syntax, "powershell.unproven_formal");
            assert!(!optional.required);
            let mut carrier = candidate(leaf, StructuralRole::Signature);
            assert!(
                !cache
                    .refine(
                        leaf,
                        StructuralRole::Signature,
                        &mut carrier,
                        &Cancellation::new()
                    )
                    .unwrap()
            );
        }
    }

    #[test]
    fn partial_input_exact_work_exhaustion_and_cancellation_cannot_certify() {
        let source = b"function Owned($value) { return @{ Key = $value } }\n";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let start = source
            .windows(6)
            .rposition(|part| part == b"$value")
            .unwrap();
        let leaf = tree
            .root_node()
            .descendant_for_byte_range(start, start + 6)
            .unwrap();
        for (maximum, complete) in [(16_384, false), (0, true)] {
            let mut cache = CaptureCache::new(source, maximum, complete);
            let mut output = candidate(leaf, StructuralRole::Reference);
            cache
                .refine(
                    leaf,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert!(!output.required);
            assert_eq!(cache.exhausted(), maximum == 0);
            if maximum == 0 {
                assert_eq!((cache.initial_work(), cache.remaining_work()), (0, 0));
            }
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut output = candidate(leaf, StructuralRole::Reference);
        assert!(matches!(
            CaptureCache::new(source, 16_384, true).refine(
                leaf,
                StructuralRole::Reference,
                &mut output,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(!output.required);
    }
}
