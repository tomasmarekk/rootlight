//! Prove written C calls only to unique earlier same-file function definitions.
//! Native binders, writes and preprocessor names fence this deliberately partial
//! proof; it does not evaluate includes, preprocessing or runtime dispatch.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

#[derive(Default)]
struct Binding {
    definition: Option<usize>,
    duplicate: bool,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    definitions: HashMap<&'source str, Binding>,
    global_barriers: HashSet<&'source str>,
    local_barriers: HashSet<(usize, &'source str)>,
    macro_names: HashSet<&'source str>,
    macro_owners: HashSet<usize>,
    maximum: usize,
    initialized: bool,
    allowed: bool,
    exhausted: bool,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize) -> Self {
        Self {
            source,
            definitions: HashMap::new(),
            global_barriers: HashSet::new(),
            local_barriers: HashSet::new(),
            macro_names: HashSet::new(),
            macro_owners: HashSet::new(),
            maximum,
            initialized: false,
            allowed: false,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn name(&self, node: Node<'_>) -> Option<&'source str> {
        let text = std::str::from_utf8(self.source.get(node.byte_range())?).ok()?;
        let mut bytes = text.bytes();
        let first = bytes.next()?;
        ((first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(text)
    }

    fn barrier(
        &mut self,
        node: Node<'_>,
        name: &'source str,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        if let Some(owner) = enclosing_function(node, false, cancellation)? {
            self.local_barriers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.local_barriers.insert((owner.id(), name));
        } else {
            self.global_barriers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.global_barriers.insert(name);
        }
        Ok(())
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
        if root.kind() != "translation_unit" || root.has_error() {
            return Ok(());
        }
        let mut cursor = root.walk();
        let mut visited = 0_usize;
        loop {
            cancellation.check()?;
            if visited >= self.maximum {
                self.exhausted = true;
                return Ok(());
            }
            visited += 1;
            let current = cursor.node();
            if current.kind() == "preproc_call" {
                // Opaque directives can change macro bindings. Their effect is
                // unavailable, so they cannot establish a completed name census.
                return Ok(());
            }
            if matches!(current.kind(), "identifier" | "type_identifier") {
                if let Some(declaration) = ordinary_binder(current, cancellation)? {
                    let Some(name) = self.name(current) else {
                        return Ok(());
                    };
                    if declaration.kind() == "function_definition"
                        && declaration.parent() == Some(root)
                        && function_name(declaration, cancellation)? == Some(current)
                    {
                        self.definitions
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        let binding = self.definitions.entry(name).or_default();
                        if binding
                            .definition
                            .replace(declaration.start_byte())
                            .is_some()
                        {
                            binding.duplicate = true;
                        }
                    } else if matches!(
                        declaration.kind(),
                        "preproc_def" | "preproc_function_def" | "function_definition"
                    ) {
                        self.global_barriers
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.global_barriers.insert(name);
                        if declaration.kind() != "function_definition" {
                            self.macro_names
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.macro_names.insert(name);
                        }
                    } else {
                        self.barrier(declaration, name, cancellation)?;
                    }
                }
                if written_target(current, cancellation)? {
                    let Some(name) = self.name(current) else {
                        return Ok(());
                    };
                    self.barrier(current, name, cancellation)?;
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
                    self.mark_macro_owners(root, cancellation)?;
                    self.allowed = true;
                    return Ok(());
                }
            }
        }
    }

    fn mark_macro_owners(
        &mut self,
        root: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        if self.macro_names.is_empty() {
            return Ok(());
        }
        // A macro may emit declarations whose binders do not exist in the raw
        // tree. Inspect uses only after the complete macro-name census, including
        // object-like macros and definitions appearing later in the source.
        let mut cursor = root.walk();
        loop {
            cancellation.check()?;
            let node = cursor.node();
            if matches!(
                node.kind(),
                "identifier" | "type_identifier" | "field_identifier"
            ) && self
                .name(node)
                .is_some_and(|name| self.macro_names.contains(name))
                && let Some(owner) = enclosing_function(node, false, cancellation)?
            {
                self.macro_owners
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.macro_owners.insert(owner.id());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(());
                }
            }
        }
    }

    /// Admit a written direct identifier call after the complete native census.
    /// Cancellation and allocation failures propagate; exhausted work is explicit.
    pub(super) fn direct_call(
        &mut self,
        call: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.initialized {
            self.initialize(call, cancellation)?;
        }
        if !self.allowed || call.kind() != "call_expression" || call.has_error() {
            return Ok(false);
        }
        let Some(callee) = call
            .child_by_field_name("function")
            .filter(|callee| callee.kind() == "identifier")
        else {
            return Ok(false);
        };
        let Some(name) = self.name(callee) else {
            return Ok(false);
        };
        let Some(owner) = enclosing_function(call, true, cancellation)? else {
            return Ok(false);
        };
        let Some(owner_name) = function_name(owner, cancellation)?.and_then(|node| self.name(node))
        else {
            return Ok(false);
        };
        if owner
            .parent()
            .is_none_or(|parent| parent.kind() != "translation_unit")
            || self.global_barriers.contains(owner_name)
            || self.global_barriers.contains(name)
            || self.local_barriers.contains(&(owner.id(), name))
            || self.macro_owners.contains(&owner.id())
        {
            return Ok(false);
        }
        if !self.definitions.get(owner_name).is_some_and(|binding| {
            !binding.duplicate && binding.definition == Some(owner.start_byte())
        }) {
            return Ok(false);
        }
        Ok(self.definitions.get(name).is_some_and(|binding| {
            !binding.duplicate
                && binding
                    .definition
                    .is_some_and(|start| start < call.start_byte())
        }))
    }
}

fn function_name<'tree>(
    function: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(mut declarator) = function.child_by_field_name("declarator") else {
        return Ok(None);
    };
    while declarator.kind() == "pointer_declarator" {
        cancellation.check()?;
        let Some(inner) = declarator.child_by_field_name("declarator") else {
            return Ok(None);
        };
        declarator = inner;
    }
    Ok((declarator.kind() == "function_declarator")
        .then(|| declarator.child_by_field_name("declarator"))
        .flatten()
        .filter(|name| name.kind() == "identifier"))
}

fn enclosing_function<'tree>(
    node: Node<'tree>,
    reject_conditional: bool,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        cancellation.check()?;
        if reject_conditional && parent.kind().starts_with("preproc_") {
            return Ok(None);
        }
        if parent.kind() == "function_definition" {
            return Ok(Some(parent));
        }
        ancestor = parent.parent();
    }
    Ok(None)
}

fn ordinary_binder<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "pointer_declarator"
            | "array_declarator"
            | "function_declarator"
            | "init_declarator"
            | "attributed_declarator" => {
                if parent.child_by_field_name("declarator") != Some(current) {
                    return Ok(None);
                }
            }
            "parenthesized_declarator" => {}
            "declaration" | "type_definition" | "parameter_declaration" => {
                return Ok((parent.child_by_field_name("type") != Some(current)).then_some(parent));
            }
            "function_definition" => {
                return Ok(
                    (parent.child_by_field_name("declarator") == Some(current)).then_some(parent)
                );
            }
            "preproc_def" | "preproc_function_def" | "enumerator" => {
                return Ok((parent.child_by_field_name("name") == Some(current)).then_some(parent));
            }
            // Old-style C formals are native identifiers directly in this list.
            "parameter_list" if current.kind() == "identifier" => return Ok(Some(parent)),
            _ => return Ok(None),
        }
        current = parent;
    }
    Ok(None)
}

fn written_target(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        let target = match parent.kind() {
            "assignment_expression" => parent.child_by_field_name("left"),
            "update_expression" => parent.child_by_field_name("argument"),
            _ => None,
        };
        if target.is_some_and(|target| {
            target.start_byte() <= node.start_byte() && node.end_byte() <= target.end_byte()
        }) {
            return Ok(true);
        }
        if matches!(
            parent.kind(),
            "compound_statement" | "function_definition" | "translation_unit"
        ) {
            break;
        }
        current = parent;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_native_census_obeys_capacity_and_typed_cancellation() {
        let source = b"int target(void){return 1;} int caller(void){return target();}";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.walk();
        let call = loop {
            if cursor.node().kind() == "call_expression" {
                break cursor.node();
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                assert!(cursor.goto_parent(), "native fixture must contain its call");
            }
        };
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(source, 0);
        assert!(!bounded.direct_call(call, &cancellation).unwrap());
        assert!(bounded.exhausted());
        let mut complete = CaptureCache::new(source, 128);
        assert!(complete.direct_call(call, &cancellation).unwrap());
        assert!(!complete.exhausted());
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            complete.direct_call(call, &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }
}
