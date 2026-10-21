//! Native Chapel module, ordinary procedure and formal declaration evidence.
//! Initializer Read certificates require the separate complete-owner census;
//! structural headers and ordinary names do not select overloads or bindings.

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::collections::HashMap;
use std::ops::Range;
use tree_sitter::Node;

mod formal_reads;
mod typed_calls;

struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    initializer_proofs: HashMap<usize, Option<formal_reads::Proof>>,
    typed_calls: HashMap<usize, Option<typed_calls::Proof>>,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
            initializer_proofs: HashMap::new(),
            typed_calls: HashMap::new(),
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(left) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = left;
        Ok(true)
    }
    fn clean(&self, node: Node<'_>) -> bool {
        !node.is_missing()
            && !node.is_error()
            && !node.has_error()
            && node.start_byte() < node.end_byte()
            && node.end_byte() <= self.source.len()
    }
    fn field<'t>(
        &mut self,
        node: Node<'t>,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let index = u32::try_from(index).map_err(|_| query_failure("chapel-child-index"))?;
            if node.field_name_for_child(index) == Some(name) {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(child);
            }
        }
        Ok(found)
    }
    fn name(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.clean(node) || node.kind() != "identifier" {
            return Ok(false);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(false);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Ok(false);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language(
                "chapel",
                text,
                text.len(),
            )
            .is_some_and(|name| name.as_ref() == text),
        )
    }
    fn header<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'t>, Node<'t>)>, AdapterError> {
        if !matches!(node.kind(), "module_declaration" | "procedure_declaration")
            || node.is_missing()
            || node.is_error()
        {
            return Ok(None);
        }
        let Some(name) = self.field(node, "name", cancel)? else {
            return Ok(None);
        };
        let Some(body) = self.field(node, "body", cancel)? else {
            return Ok(None);
        };
        if !self.name(name, cancel)?
            || body.kind() != "block_statement"
            || body.is_missing()
            || body.is_error()
            || body.start_byte() <= name.end_byte()
            || body.end_byte() > self.source.len()
        {
            return Ok(None);
        }
        let mut parameters = 0;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child == body {
                continue;
            }
            if child.start_byte() >= body.start_byte() || !self.clean(child) {
                return Ok(None);
            }
            if child.is_named()
                && !matches!(
                    child.kind(),
                    "identifier"
                        | "privacy_specifier"
                        | "formal_list"
                        | "type_expression"
                        | "inline_comment"
                        | "block_comment"
                )
            {
                return Ok(None);
            }
            if child.kind() == "formal_list" {
                parameters += 1;
            }
        }
        if parameters != usize::from(node.kind() == "procedure_declaration") {
            return Ok(None);
        }
        Ok(Some((name, body)))
    }
    fn source_owned(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut child = node;
        while let Some(parent) = child.parent() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            match parent.kind() {
                "source_file" if parent.parent().is_none() => return Ok(true),
                "statements" => {}
                "block_statement" => {
                    let Some(owner) = parent.parent() else {
                        return Ok(false);
                    };
                    if owner.kind() != "module_declaration"
                        || self.header(owner, cancel)?.is_none()
                        || self.field(owner, "body", cancel)? != Some(parent)
                    {
                        return Ok(false);
                    }
                }
                "module_declaration" => {
                    if self.header(parent, cancel)?.is_none()
                        || self.field(parent, "body", cancel)? != Some(child)
                    {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
            child = parent;
        }
        Ok(false)
    }
    fn declaration(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Declaration>, AdapterError> {
        if !self.complete {
            return Ok(None);
        }
        if matches!(node.kind(), "module_declaration" | "procedure_declaration") {
            let Some((name, body)) = self.header(node, cancel)? else {
                return Ok(None);
            };
            if !self.source_owned(node, cancel)? {
                return Ok(None);
            }
            return Ok(Some(Declaration {
                name: name.byte_range(),
                header: node.start_byte()..body.start_byte(),
                syntax: if node.kind() == "module_declaration" {
                    "chapel.module"
                } else {
                    "chapel.function"
                },
            }));
        }
        if node.kind() != "formal_parameter" || !self.clean(node) {
            return Ok(None);
        }
        let Some(list) = node
            .parent()
            .filter(|parent| parent.kind() == "formal_list")
        else {
            return Ok(None);
        };
        let Some(owner) = list
            .parent()
            .filter(|parent| parent.kind() == "procedure_declaration")
        else {
            return Ok(None);
        };
        if self.header(owner, cancel)?.is_none()
            || !self.source_owned(owner, cancel)?
            || self.field(owner, "parameters", cancel)? != Some(list)
        {
            return Ok(None);
        }
        let Some(name) = self.field(node, "name", cancel)? else {
            return Ok(None);
        };
        if !self.name(name, cancel)? {
            return Ok(None);
        }
        Ok(Some(Declaration {
            name: name.byte_range(),
            header: node.byte_range(),
            syntax: "chapel.parameter",
        }))
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let mut role = role;
    let mut required = role.belongs_to_identity_closure();
    let syntax = if node.parent().is_none()
        && matches!(node.kind(), "source_file" | "ERROR")
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        if cache.complete {
            range = 0..cache.source.len();
        }
        if role == StructuralRole::Root {
            "chapel.source_file"
        } else {
            "chapel.file"
        }
    } else if role == StructuralRole::Signature && node.kind() == "identifier" {
        if cache.typed_call(node, cancel)?.is_some() {
            role = StructuralRole::Reference;
            required = true;
            "chapel.typed_local_call"
        } else {
            if !cache.initializer_site(node, cancel)? {
                return Ok(None);
            }
            role = StructuralRole::Reference;
            required = true;
            "chapel.formal_initializer"
        }
    } else if role == StructuralRole::Signature && node.kind() == "block_statement" {
        if let Some(owner) = cache.typed_call_owner(node, cancel)? {
            range = owner;
            "chapel.typed_local_call_owner_context"
        } else {
            let Some(owner) = cache.initializer_context(node, cancel)? else {
                return Ok(None);
            };
            range = owner;
            "chapel.formal_initializer_context"
        }
    } else if role == StructuralRole::Signature && node.kind() == "call_expression" {
        let Some(proof) = cache.typed_call(node, cancel)? else {
            return Ok(None);
        };
        range = proof.call;
        "chapel.typed_local_call_context"
    } else if node.kind() == "unsupported_keyword_statement" && role == StructuralRole::Signature {
        "chapel.unsupported_context"
    } else if matches!(
        role,
        StructuralRole::Scope
            | StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
    ) {
        let Some(declaration) = cache.declaration(node, cancel)? else {
            return Ok(None);
        };
        match role {
            StructuralRole::Definition => {
                range = declaration.name;
                "chapel.name"
            }
            StructuralRole::Signature => {
                range = declaration.header;
                "chapel.header"
            }
            _ => declaration.syntax,
        }
    } else if role == StructuralRole::Reference {
        // Only the required native carrier owns a certified site's occurrence.
        // Uncertified names retain their ordinary optional Reference path.
        if cache.typed_call(node, cancel)?.is_some() || cache.initializer_site(node, cancel)? {
            return Ok(None);
        }
        if !cache.name(node, cancel)? {
            return Ok(None);
        }
        if let Some(parent) = node.parent()
            && cache
                .declaration(parent, cancel)?
                .is_some_and(|declaration| declaration.name == node.byte_range())
        {
            return Ok(None);
        }
        "chapel.unresolved_name"
    } else if role == StructuralRole::Expression {
        "chapel.construct_unavailable"
    } else if role == StructuralRole::Comment {
        "chapel.comment"
    } else if role == StructuralRole::StringLiteral {
        "chapel.string"
    } else {
        return Ok(None);
    };
    let mut depth = 0_usize;
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("chapel-depth"))?;
        ancestor = parent.parent();
    }
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
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_chapel::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }
    fn find<'t>(tree: &'t tree_sitter::Tree, kind: &str) -> Vec<Node<'t>> {
        let mut pending = vec![tree.root_node()];
        let mut found = Vec::new();
        while let Some(node) = pending.pop() {
            if node.kind() == kind {
                found.push(node);
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
            assert!(pending.len() + found.len() <= 10_000);
        }
        found.sort_by_key(Node::start_byte);
        found
    }
    #[test]
    fn original_native_module_procedure_and_formal_capture_roles_keep_exact_leaves() {
        let source = include_bytes!("../../tests/fixtures/chapel/genhashkey32.chpl");
        let tree = tree(source);
        assert!(!tree.root_node().has_error());
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(source, 16_384, true);
        let procedures = find(&tree, "procedure_declaration");
        assert_eq!(procedures.len(), 3);
        let first = procedures[0];
        for role in [
            StructuralRole::Scope,
            StructuralRole::Declaration,
            StructuralRole::Definition,
            StructuralRole::Signature,
        ] {
            let capture = candidate(first, role, &cancellation, &mut cache)
                .unwrap()
                .unwrap();
            assert!(capture.required);
            if role == StructuralRole::Definition {
                assert_eq!(capture.start..capture.end, 165..177);
            }
        }
        let formal = find(&tree, "formal_parameter")[0];
        let capture = candidate(
            formal,
            StructuralRole::Definition,
            &cancellation,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(capture.start..capture.end, 178..179);
        let modules = find(&tree, "module_declaration");
        let capture = candidate(
            modules[0],
            StructuralRole::Definition,
            &cancellation,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(capture.start..capture.end, 75..87);
    }
    #[test]
    fn partial_input_work_and_cancellation_cannot_donate_written_header_evidence() {
        let source = b"module Written { proc measure(value: int) { return value; } }";
        let tree = tree(source);
        let procedure = find(&tree, "procedure_declaration")[0];
        let cancel = Cancellation::new();
        let mut partial = CaptureCache::new(source, 128, false);
        assert!(
            candidate(
                procedure,
                StructuralRole::Declaration,
                &cancel,
                &mut partial
            )
            .unwrap()
            .is_none()
        );
        let mut bounded = CaptureCache::new(source, 0, true);
        assert!(
            candidate(
                procedure,
                StructuralRole::Declaration,
                &cancel,
                &mut bounded
            )
            .unwrap()
            .is_none()
        );
        assert!(bounded.exhausted());
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source, 128, true);
        assert!(matches!(
            candidate(procedure, StructuralRole::Declaration, &cancel, &mut cache),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
