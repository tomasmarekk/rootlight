//! Written direct Lean headers and own-formal body identifiers, without elaboration.
//! Native fields select syntax; equal spelling alone never creates a declaration.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};

struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    label: &'static str,
}

struct Formal<'tree> {
    binder: Node<'tree>,
    name: Node<'tree>,
}

struct Function<'tree> {
    node: Node<'tree>,
    name: Node<'tree>,
    body: Node<'tree>,
    binders: Node<'tree>,
    formals: Vec<Formal<'tree>>,
}

pub(super) struct CaptureCache<'tree, 'source> {
    root: Node<'tree>,
    source: &'source [u8],
    complete: bool,
    closed: bool,
    initialized: bool,
    remaining: usize,
    exhausted: bool,
    declarations: HashMap<usize, Declaration>,
    definitions: HashMap<usize, ()>,
    reads: HashMap<usize, Range<usize>>,
    contexts: HashMap<usize, Range<usize>>,
    depths: HashMap<usize, usize>,
}

impl<'tree, 'source> CaptureCache<'tree, 'source> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            root,
            source,
            complete,
            closed: false,
            initialized: false,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(64),
            exhausted: false,
            declarations: HashMap::new(),
            definitions: HashMap::new(),
            reads: HashMap::new(),
            contexts: HashMap::new(),
            depths: HashMap::new(),
        }
    }

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

    fn clean(&self, node: Node<'_>) -> bool {
        !node.is_error()
            && !node.is_missing()
            && !node.has_error()
            && self.source.get(node.byte_range()).is_some()
    }

    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<[u8; 32]>, AdapterError> {
        if !self.step(cancel)?
            || node.kind() != "identifier"
            || node.named_child_count() != 0
            || !self.clean(node)
        {
            return Ok(None);
        }
        let Some(bytes) = self
            .source
            .get(node.byte_range())
            .filter(|bytes| !bytes.is_empty() && *bytes != b"_")
        else {
            return Ok(None);
        };
        let mut hash = blake3::Hasher::new();
        for (index, chunk) in bytes.chunks(64).enumerate() {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if !chunk.iter().enumerate().all(|(offset, byte)| {
                if index == 0 && offset == 0 {
                    byte.is_ascii_alphabetic() || *byte == b'_'
                } else {
                    byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'\'')
                }
            }) {
                return Ok(None);
            }
            hash.update(chunk);
        }
        Ok(Some(*hash.finalize().as_bytes()))
    }

    fn children(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        if node.child_count() > self.remaining {
            self.exhausted = true;
            return Ok(Vec::new());
        }
        let mut children = Vec::new();
        children
            .try_reserve_exact(node.child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(Vec::new());
            }
            children.push(child);
        }
        Ok(children)
    }

    fn named_field(
        &mut self,
        node: Node<'tree>,
        field: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut result = None;
        let mut cursor = node.walk();
        for child in node.children_by_field_name(field, &mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if child.is_named() && result.replace(child).is_some() {
                return Ok(None);
            }
        }
        Ok(result)
    }

    fn formal(
        &mut self,
        binder: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Formal<'tree>>, AdapterError> {
        if binder.kind() != "explicit_binder"
            || !self.clean(binder)
            || binder.child_by_field_name("default").is_some()
        {
            return Ok(None);
        }
        let Some(name) = self.named_field(binder, "name", cancel)? else {
            return Ok(None);
        };
        if self.word(name, cancel)?.is_none() || self.named_field(binder, "type", cancel)?.is_none()
        {
            return Ok(None);
        }
        let parts = self.children(binder, cancel)?;
        if parts.first().map(Node::kind) != Some("(") || parts.last().map(Node::kind) != Some(")") {
            return Ok(None);
        }
        Ok(Some(Formal { binder, name }))
    }

    fn function(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Function<'tree>>, AdapterError> {
        if node.kind() != "def" || !node.is_named() || !self.clean(node) {
            return Ok(None);
        }
        let Some(name) = self.named_field(node, "name", cancel)? else {
            return Ok(None);
        };
        if self.word(name, cancel)?.is_none() {
            return Ok(None);
        }
        // The grammar assigns body/type fields to punctuation too. Select the
        // unique named field rather than child_by_field_name's first token.
        let Some(body) = self.named_field(node, "body", cancel)? else {
            return Ok(None);
        };
        let parts = self.children(node, cancel)?;
        let mut binder_list = None;
        for part in parts {
            if part.kind() == "binders" && binder_list.replace(part).is_some() {
                return Ok(None);
            }
        }
        let Some(binder_list) = binder_list else {
            return Ok(None);
        };
        let mut formals = Vec::new();
        let mut names = HashMap::new();
        for binder in self.children(binder_list, cancel)? {
            if !binder.is_named() {
                return Ok(None);
            }
            let Some(formal) = self.formal(binder, cancel)? else {
                return Ok(None);
            };
            let Some(word) = self.word(formal.name, cancel)? else {
                return Ok(None);
            };
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if names.insert(word, ()).is_some() {
                return Ok(None);
            }
            formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            formals.push(formal);
        }
        Ok((!formals.is_empty()).then_some(Function {
            node,
            name,
            body,
            binders: binder_list,
            formals,
        }))
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        let root = self.root;
        let prefix = self.source.get(..root.start_byte()).unwrap_or_default();
        let prefix = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix);
        for chunk in prefix.chunks(64) {
            if !self.step(cancel)? {
                return Ok(());
            }
            self.complete &= chunk.iter().all(u8::is_ascii_whitespace);
        }
        self.complete &= root.parent().is_none()
            && root.kind() == "module"
            && root.end_byte() == self.source.len()
            && self.clean(root);
        let mut cursor = root.walk();
        let mut depth = 0usize;
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            self.depths
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.depths.insert(cursor.node().id(), depth);
            if cursor.goto_first_child() {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| query_failure("lean-native-depth"))?;
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break;
                }
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| query_failure("lean-native-depth"))?;
            }
            if cursor.node() == root {
                break;
            }
        }
        if !self.complete {
            return Ok(());
        }
        self.closed = true;
        let mut functions = Vec::new();
        let mut heads = HashMap::new();
        for command in self.children(root, cancel)? {
            if matches!(
                command.kind(),
                "line_comment"
                    | "block_comment"
                    | "doc_comment"
                    | "module_doc_comment"
                    | "set_option"
                    | "universe"
            ) {
                continue;
            }
            if command.kind() != "declaration" {
                self.closed = false;
                continue;
            }
            let parts = self.children(command, cancel)?;
            let Some(definition) = parts
                .first()
                .copied()
                .filter(|part| part.is_named() && part.kind() == "def")
            else {
                self.closed = false;
                continue;
            };
            if parts.len() != 1 || command.byte_range() != definition.byte_range() {
                self.closed = false;
                continue;
            }
            let Some(name) = self.named_field(definition, "name", cancel)? else {
                self.closed = false;
                continue;
            };
            let Some(word) = self.word(name, cancel)? else {
                self.closed = false;
                continue;
            };
            heads
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if heads.insert(word, ()).is_some() {
                self.closed = false;
            }
            let Some(function) = self.function(definition, cancel)? else {
                continue;
            };
            self.declarations
                .try_reserve(function.formals.len() + 1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.definitions
                .try_reserve(function.formals.len() + 1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.declarations.insert(
                definition.id(),
                Declaration {
                    name: function.name.byte_range(),
                    header: definition.start_byte()..function.body.start_byte(),
                    label: "lean.function",
                },
            );
            self.definitions.insert(function.name.id(), ());
            for formal in &function.formals {
                self.declarations.insert(
                    formal.binder.id(),
                    Declaration {
                        name: formal.name.byte_range(),
                        header: formal.binder.byte_range(),
                        label: "lean.parameter",
                    },
                );
                self.definitions.insert(formal.name.id(), ());
            }
            functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            functions.push(function);
        }
        if !self.closed || self.exhausted {
            return Ok(());
        }
        for function in functions {
            if function.body.end_byte() != function.node.end_byte() {
                continue;
            }
            let Some(body) = self.word(function.body, cancel)? else {
                continue;
            };
            for formal in function.formals {
                if self.word(formal.name, cancel)? == Some(body) {
                    self.reads
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.reads.insert(
                        function.body.id(),
                        formal.binder.start_byte()..function.body.end_byte(),
                    );
                    self.contexts
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.contexts
                        .insert(function.binders.id(), function.node.byte_range());
                }
            }
        }
        Ok(())
    }
}

pub(super) fn candidate<'tree>(
    node: Node<'tree>,
    mut role: StructuralRole,
    cancel: &Cancellation,
    cache: &mut CaptureCache<'tree, '_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cache.initialize(cancel)?;
    if !cache.step(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let mut required = role.belongs_to_identity_closure();
    let syntax = if node == cache.root {
        match role {
            StructuralRole::Root => {
                if cache.complete {
                    range = 0..cache.source.len();
                    "lean.file"
                } else {
                    "lean.unavailable_file"
                }
            }
            StructuralRole::Module | StructuralRole::Scope if cache.complete => {
                range = 0..cache.source.len();
                "lean.file_context"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Signature && cache.contexts.contains_key(&node.id()) {
        range = cache.contexts[&node.id()].clone();
        "lean.formal_binding_context"
    } else if let Some(declaration) = cache.declarations.get(&node.id()) {
        match role {
            StructuralRole::Declaration => declaration.label,
            StructuralRole::Definition => {
                range = declaration.name.clone();
                "lean.name"
            }
            StructuralRole::Signature => {
                range = declaration.header.clone();
                "lean.header"
            }
            _ => return Ok(None),
        }
    } else if node.kind() == "identifier"
        && matches!(role, StructuralRole::Reference | StructuralRole::Signature)
    {
        if cache.definitions.contains_key(&node.id()) || cache.word(node, cancel)?.is_none() {
            return Ok(None);
        }
        if let Some(window) = cache.reads.get(&node.id()) {
            required = true;
            if role == StructuralRole::Signature {
                range = window.clone();
                "lean.formal_body_target"
            } else {
                role = StructuralRole::Reference;
                "lean.formal_body"
            }
        } else if role == StructuralRole::Reference {
            "lean.unproved_name"
        } else {
            return Ok(None);
        }
    } else {
        match role {
            StructuralRole::Signature if node.kind() == "def" => "lean.unproved_definition",
            StructuralRole::Comment => "lean.comment",
            StructuralRole::StringLiteral => "lean.string",
            _ => return Ok(None),
        }
    };
    if cache.exhausted {
        return Ok(None);
    }
    let depth = cache
        .depths
        .get(&node.id())
        .copied()
        .ok_or_else(|| query_failure("lean-native-depth-node"))?;
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required,
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests;
