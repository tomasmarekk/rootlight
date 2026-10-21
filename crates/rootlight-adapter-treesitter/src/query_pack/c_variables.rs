//! Prove written scalar variables through native C lexical scopes.
//! Target windows carry declaration coordinates; mutation changes values, not
//! lexical ownership. Includes and runtime execution remain unevaluated.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

#[derive(Clone, Copy)]
struct Variable {
    declaration: (usize, usize),
    name: (usize, usize),
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    checked: Option<bool>,
    remaining: usize,
    exhausted: bool,
    variables: HashMap<usize, Variable>,
    bindings: HashMap<(usize, [u8; 32]), Option<usize>>,
    reads: HashMap<usize, Option<Variable>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            checked: None,
            remaining,
            exhausted: false,
            variables: HashMap::new(),
            bindings: HashMap::new(),
            reads: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    pub(super) const fn remaining_work(&self) -> usize {
        self.remaining
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(left) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = left;
        Ok(true)
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<[u8; 32]>, AdapterError> {
        if !self.step(cancel)?
            || node.kind() != "identifier"
            || node.has_error()
            || node.is_missing()
        {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        if !bytes
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return Ok(None);
        }
        let mut key = blake3::Hasher::new();
        for chunk in bytes.chunks(64) {
            if !self.step(cancel)?
                || !chunk
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                return Ok(None);
            }
            key.update(chunk);
        }
        Ok(Some(*key.finalize().as_bytes()))
    }

    fn scope(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        loop {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "translation_unit" | "compound_statement" | "for_statement"
            ) {
                return Ok(Some(node.id()));
            }
            let Some(parent) = node.parent() else {
                return Ok(None);
            };
            node = parent;
        }
    }

    fn bind(
        &mut self,
        scope: usize,
        name: [u8; 32],
        variable: Option<usize>,
    ) -> Result<(), AdapterError> {
        self.bindings
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.bindings
            .entry((scope, name))
            .and_modify(|entry| *entry = None)
            .or_insert(variable);
        Ok(())
    }

    fn census(&mut self, mut root: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if let Some(complete) = self.checked {
            return Ok(complete);
        }
        self.checked = Some(false);
        while let Some(parent) = root.parent() {
            if !self.step(cancel)? {
                return Ok(false);
            }
            root = parent;
        }
        if !self.complete
            || root.kind() != "translation_unit"
            || root.has_error()
            || root.is_missing()
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
            || root.end_byte() != self.source.len()
        {
            return Ok(false);
        }
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if !self.step(cancel)?
                || node.is_error()
                || node.is_missing()
                || (node.kind().starts_with("preproc_")
                    && node.kind() != "preproc_include"
                    && !node.parent().is_some_and(|p| p.kind() == "preproc_include"))
                || matches!(
                    node.kind(),
                    "type_definition" | "struct_specifier" | "union_specifier" | "enum_specifier"
                )
            {
                return Ok(false);
            }
            if node.kind() == "function_definition" {
                if node.parent() != Some(root) {
                    return Ok(false);
                }
                let Some(declarator) = node.child_by_field_name("declarator") else {
                    return Ok(false);
                };
                if declarator.kind() != "function_declarator" {
                    return Ok(false);
                }
                let Some(name) = declarator.child_by_field_name("declarator") else {
                    return Ok(false);
                };
                let Some(name) = self.name(name, cancel)? else {
                    return Ok(false);
                };
                self.bind(root.id(), name, None)?;
            }
            if node.kind() == "parameter_declaration"
                && let Some(name) = node.child_by_field_name("declarator")
            {
                let Some(name) = self.name(name, cancel)? else {
                    return Ok(false);
                };
                let mut owner = node.parent();
                let body = loop {
                    let Some(parent) = owner else {
                        return Ok(false);
                    };
                    if !self.step(cancel)? {
                        return Ok(false);
                    }
                    if parent.kind() == "function_definition" {
                        break parent.child_by_field_name("body");
                    }
                    owner = parent.parent();
                };
                let Some(body) = body else { return Ok(false) };
                self.bind(body.id(), name, None)?;
            }
            if node.kind() == "declaration" {
                let Some(parent) = node.parent() else {
                    return Ok(false);
                };
                if !matches!(
                    parent.kind(),
                    "translation_unit" | "compound_statement" | "for_statement"
                ) || (parent.kind() == "for_statement"
                    && parent.child_by_field_name("initializer") != Some(node))
                {
                    return Ok(false);
                }
                let mut declarator = None;
                let mut children = node.walk();
                for child in node.named_children(&mut children) {
                    if !self.step(cancel)? {
                        return Ok(false);
                    }
                    match child.kind() {
                        "primitive_type" | "type_identifier" | "type_qualifier" | "comment" => {}
                        "init_declarator" if declarator.is_none() => declarator = Some(child),
                        _ => return Ok(false),
                    }
                }
                let Some(declarator) = declarator else {
                    return Ok(false);
                };
                let Some(name_node) = declarator.child_by_field_name("declarator") else {
                    return Ok(false);
                };
                let Some(name) = self.name(name_node, cancel)? else {
                    return Ok(false);
                };
                if declarator.child_by_field_name("value").is_none() {
                    return Ok(false);
                }
                let Some(scope) = self.scope(parent, cancel)? else {
                    return Ok(false);
                };
                self.variables
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.variables.insert(
                    declarator.id(),
                    Variable {
                        declaration: (declarator.start_byte(), declarator.end_byte()),
                        name: (name_node.start_byte(), name_node.end_byte()),
                    },
                );
                self.bind(scope, name, Some(declarator.id()))?;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.checked = Some(true);
                    return Ok(true);
                }
            }
        }
    }

    fn read_operand(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut child = node;
        let mut value = false;
        while let Some(parent) = child.parent() {
            if !self.step(cancel)? {
                return Ok(false);
            }
            match parent.kind() {
                "binary_expression" | "parenthesized_expression" => value = true,
                "assignment_expression" => {
                    if parent.child_by_field_name("right") != Some(child) {
                        return Ok(false);
                    }
                    value = true;
                }
                "argument_list" => {
                    if child != node {
                        return Ok(false);
                    }
                    value = true;
                }
                "call_expression" => {
                    if parent.child_by_field_name("arguments") != Some(child) {
                        return Ok(false);
                    }
                }
                "return_statement" => value = true,
                "expression_statement" => {
                    if !value {
                        return Ok(false);
                    }
                }
                "for_statement" => {
                    if parent.child_by_field_name("condition") == Some(child) {
                        value = true;
                    } else if !value {
                        return Ok(false);
                    }
                }
                "compound_statement" | "if_statement" | "while_statement" | "do_statement"
                | "switch_statement" | "case_statement" | "labeled_statement" => {}
                "function_definition" => {
                    return Ok(value && parent.child_by_field_name("body") == Some(child));
                }
                // Unknown wrappers include sizeof/alignof/generic/type contexts;
                // an inner RHS or call cannot bypass their unevaluated boundary.
                _ => return Ok(false),
            }
            child = parent;
        }
        Ok(false)
    }

    fn target(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Variable>, AdapterError> {
        if let Some(target) = self.reads.get(&node.id()) {
            return Ok(*target);
        }
        let mut selected = None;
        if self.census(node, cancel)?
            && self.read_operand(node, cancel)?
            && let Some(name) = self.name(node, cancel)?
        {
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                if !self.step(cancel)? {
                    return Ok(None);
                }
                if matches!(
                    parent.kind(),
                    "translation_unit" | "compound_statement" | "for_statement"
                ) && let Some(binding) = self.bindings.get(&(parent.id(), name))
                {
                    selected = binding
                        .and_then(|id| self.variables.get(&id))
                        .copied()
                        .filter(|v| v.declaration.1 <= node.start_byte());
                    break;
                }
                ancestor = parent.parent();
            }
        }
        self.reads
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.reads.insert(node.id(), selected);
        Ok(selected)
    }

    fn declared(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(variable) = self.variables.get(&node.id()).copied() else {
            return Ok(false);
        };
        let Some(name) = node.child_by_field_name("declarator") else {
            return Ok(false);
        };
        if (name.start_byte(), name.end_byte()) != variable.name {
            return Ok(false);
        }
        let Some(name) = self.name(name, cancel)? else {
            return Ok(false);
        };
        let Some(scope) = node.parent().and_then(|p| p.parent()) else {
            return Ok(false);
        };
        let Some(scope) = self.scope(scope, cancel)? else {
            return Ok(false);
        };
        Ok(self.bindings.get(&(scope, name)) == Some(&Some(node.id())))
    }

    /// Return Some only for this protocol's captures; preserve existing C facts.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        if role == StructuralRole::Declaration && node.kind() == "declaration" {
            let mut supported = false;
            if self.census(node, cancel)?
                && let Some(declarator) = node.child_by_field_name("declarator")
            {
                supported = self.declared(declarator, cancel)?;
            }
            if !supported {
                candidate.syntax = "c.unproven_declarator";
                return Ok(Some(true));
            }
            return Ok(None);
        }
        if role == StructuralRole::Signature && node.kind() == "translation_unit" {
            if !self.census(node, cancel)? {
                return Ok(Some(false));
            }
            candidate.start = 0;
            candidate.end = self.source.len();
            candidate.syntax = "c.variable_file_context";
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Scope
            && matches!(node.kind(), "compound_statement" | "for_statement")
        {
            candidate.syntax = if node.kind() == "for_statement" {
                "c.for"
            } else {
                "c.block"
            };
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Declaration && node.kind() == "init_declarator" {
            if !self.census(node, cancel)? || !self.declared(node, cancel)? {
                return Ok(Some(false));
            }
            candidate.syntax = "c.variable";
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Reference && node.kind() == "field_identifier" {
            candidate.syntax = "c.unproven_variable";
            return Ok(Some(true));
        }
        if node.kind() != "identifier" {
            return Ok(None);
        }
        if role == StructuralRole::Definition
            && node.parent().is_some_and(|p| p.kind() == "init_declarator")
        {
            if !self.census(node, cancel)? {
                return Ok(Some(false));
            }
            let Some(parent) = node.parent() else {
                return Ok(Some(false));
            };
            if !self.declared(parent, cancel)? {
                return Ok(Some(false));
            }
            candidate.syntax = "c.variable_name";
            candidate.required = true;
            return Ok(Some(true));
        }
        if !matches!(
            role,
            StructuralRole::Reference | StructuralRole::Signature | StructuralRole::Scope
        ) {
            return Ok(None);
        }
        if let Some(target) = self.target(node, cancel)? {
            if role == StructuralRole::Scope {
                candidate.role = StructuralRole::Signature;
                candidate.start = target.declaration.0;
                candidate.syntax = "c.variable_target";
            } else {
                candidate.role = StructuralRole::Reference;
                candidate.syntax = "c.written_variable";
            }
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Scope {
            return Ok(Some(false));
        }
        if role == StructuralRole::Reference
            && !node.parent().is_some_and(|p| p.kind() == "argument_list")
        {
            candidate.syntax = "c.unproven_variable";
            return Ok(Some(true));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn at<'tree>(tree: &'tree tree_sitter::Tree, source: &[u8], word: &[u8]) -> Node<'tree> {
        let offset = source
            .windows(word.len())
            .rposition(|part| part == word)
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset + 5)
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        node
    }

    fn candidate(node: Node<'_>, role: StructuralRole) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "c.identifier",
            required: false,
            native_depth: 0,
        }
    }

    #[test]
    fn native_target_window_starts_at_the_nearest_written_shadow_declarator() {
        let source = b"int value=1; int owned(void){ { int value=2; return value; } }\n";
        let tree = tree(source);
        assert!(!tree.root_node().has_error());
        let node = at(&tree, source, b"value;");
        let mut cache = CaptureCache::new(source, 4096, true);
        let mut output = candidate(node, StructuralRole::Scope);
        assert_eq!(
            cache
                .refine(
                    node,
                    StructuralRole::Scope,
                    &mut output,
                    &Cancellation::new()
                )
                .unwrap(),
            Some(true)
        );
        assert_eq!(output.role, StructuralRole::Signature);
        assert_eq!(output.syntax, "c.variable_target");
        assert_eq!(
            output.start,
            source.windows(7).position(|s| s == b"value=2").unwrap()
        );
        assert_eq!(output.end, node.end_byte());
        assert!(output.required);
        assert!(!cache.exhausted());
    }

    #[test]
    fn native_recovered_partial_and_duplicate_contexts_withhold_target_windows() {
        for (source, complete) in [
            (
                b"int value=1; int value=2; int owned(void){ return value; }\n".as_slice(),
                true,
            ),
            (b"int value=1; int owned(void){ return value; }\n", false),
            (
                b"int value=1; int owned(void){ return value; } broken @\n",
                true,
            ),
        ] {
            let tree = tree(source);
            let node = at(&tree, source, b"value;");
            let mut cache = CaptureCache::new(source, 4096, complete);
            let mut output = candidate(node, StructuralRole::Scope);
            assert_eq!(
                cache
                    .refine(
                        node,
                        StructuralRole::Scope,
                        &mut output,
                        &Cancellation::new()
                    )
                    .unwrap(),
                Some(false)
            );
        }
    }

    #[test]
    fn native_work_and_cancel_refuse_without_a_partial_certificate() {
        let source = b"int value=1; int owned(void){ return value; }\n";
        let tree = tree(source);
        let node = at(&tree, source, b"value;");
        let mut cache = CaptureCache::new(source, 0, true);
        let mut output = candidate(node, StructuralRole::Scope);
        assert_eq!(
            cache
                .refine(
                    node,
                    StructuralRole::Scope,
                    &mut output,
                    &Cancellation::new()
                )
                .unwrap(),
            Some(false)
        );
        assert!(cache.exhausted());
        assert_eq!(cache.remaining_work(), 0);
        let stop = Cancellation::new();
        stop.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, 4096, true);
        assert!(matches!(
            cache.refine(node, StructuralRole::Scope, &mut output, &stop),
            Err(AdapterError::Cancelled { .. })
        ));
    }
    #[test]
    fn unevaluated_ancestor_never_receives_an_ordinary_value_read_window() {
        for source in [
            b"int state=1; int owned(void){ return sizeof(state = state); }\n".as_slice(),
            b"int state=1; int owned(void){ return sizeof(external(state)); }\n",
        ] {
            let tree = tree(source);
            assert!(!tree.root_node().has_error());
            let node = at(&tree, source, b"state");
            let mut cache = CaptureCache::new(source, 4096, true);
            let mut output = candidate(node, StructuralRole::Scope);
            assert_eq!(
                cache
                    .refine(
                        node,
                        StructuralRole::Scope,
                        &mut output,
                        &Cancellation::new()
                    )
                    .unwrap(),
                Some(false)
            );
            assert!(!cache.exhausted());
        }
    }
}
