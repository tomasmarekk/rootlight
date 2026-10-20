//! Prove a deliberately narrow immutable module function binding from native syntax.
//! Every binder and write is inspected before any call receives the proof label;
//! missing capacity or an unknown spelling withholds proof rather than implying absence.

use std::{borrow::Cow, collections::HashMap};

use rootlight_adapter_sdk::{AdapterError, SinkError, canonical_ecmascript_identifier};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::ecmascript;

#[derive(Default)]
struct Binding {
    declarations: usize,
    module_function: bool,
    written: bool,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    bindings: HashMap<Cow<'source, str>, Binding>,
    maximum: usize,
    initialized: bool,
    allowed: bool,
    exhausted: bool,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize) -> Self {
        Self {
            source,
            bindings: HashMap::new(),
            maximum,
            initialized: false,
            allowed: false,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn name(&self, node: Node<'_>) -> Option<Cow<'source, str>> {
        let text = std::str::from_utf8(self.source.get(node.byte_range())?).ok()?;
        canonical_ecmascript_identifier(text, text.len())
    }

    fn initialize(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.initialized = true;
        let mut root = node;
        while let Some(parent) = root.parent() {
            cancellation.check()?;
            root = parent;
        }
        if root.kind() != "program"
            || root.has_error()
            || root.is_missing()
            || root.start_byte() != 0
            || root.end_byte() != self.source.len()
        {
            return Ok(());
        }
        let mut children = root.walk();
        if !root
            .named_children(&mut children)
            .any(|child| matches!(child.kind(), "import_statement" | "export_statement"))
        {
            return Ok(());
        }
        let mut cursor = root.walk();
        let mut visited = 0usize;
        loop {
            cancellation.check()?;
            if visited >= self.maximum {
                self.exhausted = true;
                return Ok(());
            }
            visited += 1;
            let current = cursor.node();
            if matches!(
                current.kind(),
                "with_statement" | "internal_module" | "module" | "namespace_declaration"
            ) {
                return Ok(());
            }
            if matches!(
                current.kind(),
                "identifier" | "type_identifier" | "shorthand_property_identifier_pattern"
            ) {
                let binding = ecmascript::binding_kind(current, cancellation)?.is_some()
                    || current
                        .parent()
                        .is_some_and(|parent| parent.child_by_field_name("name") == Some(current));
                let written = written_target(current, cancellation)?;
                if binding || written || current.kind() == "identifier" {
                    let Some(name) = self.name(current) else {
                        return Ok(());
                    };
                    // Even a shadowed eval name is conservatively a dynamic-scope barrier.
                    if name == "eval" {
                        return Ok(());
                    }
                    if binding || written {
                        self.bindings
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        let row = self.bindings.entry(name).or_default();
                        if binding {
                            row.declarations = row.declarations.saturating_add(1);
                            row.module_function |= module_function_name(current, root);
                        }
                        row.written |= written;
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
                    self.allowed = true;
                    return Ok(());
                }
            }
        }
    }

    /// Return true only after the entire module proves one unwritten function binding.
    pub(super) fn direct_call(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.initialized {
            self.initialize(node, cancellation)?;
        }
        if !self.allowed || node.kind() != "identifier" {
            return Ok(false);
        }
        let Some(call) = node.parent().filter(|parent| {
            parent.kind() == "call_expression"
                && parent.child_by_field_name("function") == Some(node)
                && parent
                    .child_by_field_name("arguments")
                    .is_some_and(|arguments| arguments.kind() == "arguments")
        }) else {
            return Ok(false);
        };
        let mut children = call.walk();
        if call
            .children(&mut children)
            .any(|child| matches!(child.kind(), "optional_chain" | "?."))
        {
            return Ok(false);
        }
        let Some(name) = self.name(node) else {
            return Ok(false);
        };
        Ok(self.bindings.get(name.as_ref()).is_some_and(|binding| {
            binding.declarations == 1 && binding.module_function && !binding.written
        }))
    }
}

fn module_function_name(node: Node<'_>, root: Node<'_>) -> bool {
    let Some(function) = node.parent().filter(|parent| {
        parent.kind() == "function_declaration"
            && parent.child_by_field_name("name") == Some(node)
            && parent.child_by_field_name("body").is_some()
    }) else {
        return false;
    };
    function.parent() == Some(root)
        || function.parent().is_some_and(|parent| {
            parent.kind() == "export_statement"
                && parent.parent() == Some(root)
                && parent.child_by_field_name("declaration") == Some(function)
        })
}

fn written_target(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        cancellation.check()?;
        let target = match parent.kind() {
            "assignment_expression" | "augmented_assignment_expression" | "for_in_statement" => {
                parent.child_by_field_name("left")
            }
            "update_expression" => parent.child_by_field_name("argument"),
            "unary_expression"
                if parent
                    .child_by_field_name("operator")
                    .is_some_and(|operator| operator.kind() == "delete") =>
            {
                parent.child_by_field_name("argument")
            }
            _ => None,
        };
        // Include every identifier in a write target conservatively, including
        // destructuring and writes in nested closures; no source-order guess is used.
        if target.is_some_and(|target| {
            target.start_byte() <= node.start_byte() && node.end_byte() <= target.end_byte()
        }) {
            return Ok(true);
        }
        ancestor = parent.parent();
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn complete_native_module_proof_obeys_capacity_and_typed_cancellation() {
        let source = b"export function target(){} export function caller(){target()}";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let mut call = None;
        loop {
            let node = cursor.node();
            if node.kind() == "identifier"
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "call_expression")
            {
                call = Some(node);
                break;
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
            if cursor.node() == root {
                break;
            }
        }
        let call = call.unwrap();
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(source, 1);
        assert!(!bounded.direct_call(call, &cancellation).unwrap());
        assert!(bounded.exhausted());
        let mut admitted = CaptureCache::new(source, 1024);
        assert!(admitted.direct_call(call, &cancellation).unwrap());
        assert!(!admitted.exhausted());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            admitted.direct_call(call, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        );
    }
}
