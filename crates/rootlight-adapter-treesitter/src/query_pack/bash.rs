//! Certify simple parameter reads in a written module environment.
//! Function dynamic scope, namerefs and executable shell mutations are barriers;
//! this census proves source binding, not command execution or expanded values.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

#[derive(Default)]
struct Binding {
    end: Option<usize>,
    duplicate: bool,
    barrier: Option<usize>,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    bindings: HashMap<&'source str, Binding>,
    functions: HashSet<&'source str>,
    commands: Vec<(usize, &'source str)>,
    barrier: Option<usize>,
    maximum: usize,
    initialized: bool,
    complete: bool,
    exhausted: bool,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize) -> Self {
        Self {
            source,
            bindings: HashMap::new(),
            functions: HashSet::new(),
            commands: Vec::new(),
            barrier: None,
            maximum,
            initialized: false,
            complete: false,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn text(&self, node: Node<'_>) -> Option<&'source str> {
        std::str::from_utf8(self.source.get(node.byte_range())?).ok()
    }

    fn name(&self, node: Node<'_>) -> Option<&'source str> {
        let text = self.text(node)?;
        let mut bytes = text.bytes();
        let first = bytes.next()?;
        ((first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|value| value.is_ascii_alphanumeric() || value == b'_'))
        .then_some(text)
    }

    fn block(&mut self, position: usize) {
        self.barrier = Some(self.barrier.map_or(position, |old| old.min(position)));
    }

    fn initialize(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        let mut root = node;
        while let Some(parent) = root.parent() {
            cancel.check()?;
            root = parent;
        }
        if root.kind() != "program" || root.has_error() {
            return Ok(());
        }
        let mut cursor = root.walk();
        let mut visited = 0;
        loop {
            cancel.check()?;
            if visited >= self.maximum {
                self.exhausted = true;
                return Ok(());
            }
            visited += 1;
            let current = cursor.node();
            if current.kind() == "&" {
                // The sibling terminator makes even a program-owned assignment
                // asynchronous. This census does not model that environment.
                self.block(0);
            }
            let function = current.kind() == "function_definition";
            if function {
                if let Some(name) = current
                    .child_by_field_name("name")
                    .and_then(|name| self.name(name))
                {
                    self.functions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.functions.insert(name);
                } else {
                    self.block(current.start_byte());
                }
            } else if current.kind() == "variable_assignment" {
                if let Some(name_node) = current.child_by_field_name("name")
                    && let Some(name) = self.name(name_node)
                {
                    let ordinary = current.parent() == Some(root)
                        && current.child_by_field_name("value").is_some_and(|value| {
                            value.kind() != "array"
                                && self.source.get(name_node.end_byte()..value.start_byte())
                                    == Some(b"=")
                        });
                    self.bindings
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    let binding = self.bindings.entry(name).or_default();
                    if ordinary {
                        if binding.end.replace(current.end_byte()).is_some() {
                            binding.duplicate = true;
                        }
                    } else {
                        let start = current.start_byte();
                        binding.barrier = Some(binding.barrier.map_or(start, |old| old.min(start)));
                    }
                } else {
                    self.block(current.start_byte());
                }
            } else if matches!(
                current.kind(),
                "declaration_command"
                    | "unset_command"
                    | "arithmetic_expansion"
                    | "postfix_expression"
                    | "unary_expression"
            ) || (current.kind() == "expansion"
                && current
                    .child_by_field_name("operator")
                    .and_then(|operator| self.text(operator))
                    .is_some_and(|operator| matches!(operator, "=" | ":=")))
            {
                self.block(current.start_byte());
            } else if current.kind() == "command" {
                let name = current
                    .child_by_field_name("name")
                    .filter(|name| name.named_child_count() == 1)
                    .and_then(|name| name.named_child(0))
                    .filter(|name| name.kind() == "word")
                    .and_then(|name| self.text(name));
                if let Some(name) = name.filter(|name| {
                    !name.is_empty()
                        && !name
                            .bytes()
                            .any(|c| c.is_ascii_whitespace() || b"$`\\'\"".contains(&c))
                }) {
                    if matches!(
                        name,
                        "eval"
                            | "."
                            | "source"
                            | "read"
                            | "mapfile"
                            | "readarray"
                            | "let"
                            | "alias"
                            | "unalias"
                            | "trap"
                            | "enable"
                            | "builtin"
                            | "command"
                            | "exec"
                    ) {
                        self.block(current.start_byte());
                    }
                    self.commands
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.commands.push((current.start_byte(), name));
                } else {
                    self.block(current.start_byte());
                }
            }
            // Uncalled function bodies cannot change module bindings. Any written
            // invocation is fenced after the complete function-name census.
            if !function && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    for index in 0..self.commands.len() {
                        cancel.check()?;
                        let (position, name) = self.commands[index];
                        if self.functions.contains(name) {
                            self.block(position);
                        }
                    }
                    self.complete = true;
                    return Ok(());
                }
            }
        }
    }

    /// Return an exact read classification; unavailable binding remains explicit.
    /// Cancellation and failed fallible census allocations propagate to the caller.
    pub(super) fn syntax(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'static str>, AdapterError> {
        cancel.check()?;
        if node.kind() != "variable_name" {
            return Ok(None);
        }
        let Some(parent) = node.parent() else {
            return Ok(None);
        };
        let simple = parent.kind() == "simple_expansion"
            || (parent.kind() == "expansion"
                && parent.named_child_count() == 1
                && self.source.get(parent.byte_range()).is_some_and(|raw| {
                    raw.starts_with(b"${")
                        && raw.ends_with(b"}")
                        && raw.len() == node.byte_range().len() + 3
                }));
        if !simple {
            return Ok((parent.kind() == "expansion").then_some("bash.complex_parameter"));
        }
        if self.name(node).is_none() {
            return Ok(Some("bash.complex_parameter"));
        }
        let mut ancestor = parent.parent();
        while let Some(owner) = ancestor {
            cancel.check()?;
            if owner.kind() == "heredoc_body" {
                // Delimiter quoting controls heredoc expansion. Its text alone
                // does not certify an ordinary parameter read in this slice.
                return Ok(Some("bash.complex_parameter"));
            }
            ancestor = owner.parent();
        }
        if !self.initialized {
            self.initialize(node, cancel)?;
        }
        let proven = self.complete
            && module_context(node, cancel)?
            && self
                .barrier
                .is_none_or(|position| position > node.start_byte())
            && self
                .name(node)
                .and_then(|name| self.bindings.get(name))
                .is_some_and(|binding| {
                    !binding.duplicate
                        && binding.end.is_some_and(|end| end <= node.start_byte())
                        && binding
                            .barrier
                            .is_none_or(|position| position > node.start_byte())
                });
        Ok(Some(if proven {
            "bash.module_read"
        } else {
            "bash.unproven_read"
        }))
    }
}

fn module_context(mut node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        cancel.check()?;
        match parent.kind() {
            "program" => return Ok(true),
            "simple_expansion"
            | "expansion"
            | "string"
            | "concatenation"
            | "command"
            | "command_name"
            | "command_substitution" => {}
            "variable_assignment"
                if parent
                    .parent()
                    .is_some_and(|owner| owner.kind() == "program")
                    && parent.child_by_field_name("value").is_some_and(|value| {
                        value.start_byte() <= node.start_byte()
                            && node.end_byte() <= value.end_byte()
                    }) => {}
            _ => return Ok(false),
        }
        node = parent;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn native_binding_census_obeys_cancellation_and_work_admission() {
        let source = b"REPO=outer\nprs=$REPO\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let read = tree
            .root_node()
            .named_child(1)
            .unwrap()
            .child_by_field_name("value")
            .unwrap()
            .named_child(0)
            .unwrap();
        assert_eq!(read.kind(), "variable_name");
        let mut cache = CaptureCache::new(source, 1);
        assert_eq!(
            cache.syntax(read, &Cancellation::new()).unwrap(),
            Some("bash.unproven_read")
        );
        assert!(cache.exhausted());
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            cache.syntax(read, &cancelled),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
