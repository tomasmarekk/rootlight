//! Native OCaml witnesses for direct match scrutinees and preceding file functions.
//! A complete clean file census precedes every certificate; later patterns never
//! supply lexical binding from spelling, interfaces or imported module contents.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use super::{CaptureCache, QueryCandidate, StructuralRole, native_depth};
use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

struct Function<'tree> {
    node: Node<'tree>,
    name: Node<'tree>,
    body: Node<'tree>,
    parameters: Vec<Node<'tree>>,
}

#[derive(Default)]
pub(super) struct Proofs {
    complete: bool,
    reads: HashMap<usize, (Range<usize>, Range<usize>)>,
    calls: HashMap<usize, (Range<usize>, Range<usize>)>,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.has_error()
        && !node.is_missing()
        && !node.is_error()
        && node.start_byte() < node.end_byte()
        && node.end_byte() <= source.len()
}

fn name<'source>(
    node: Node<'_>,
    source: &'source [u8],
    cache: &mut CaptureCache,
    cancel: &Cancellation,
) -> Result<Option<&'source [u8]>, AdapterError> {
    if !clean(node, source)
        || node.named_child_count() != 0
        || !matches!(node.kind(), "value_name" | "value_pattern")
    {
        return Ok(None);
    }
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    for _ in raw.chunks(64) {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
    }
    let Ok(text) = std::str::from_utf8(raw) else {
        return Ok(None);
    };
    Ok((text != "_"
        && structural_captured_name_for_language("ocaml", text, text.len())
            .is_some_and(|selected| selected.as_ref() == text))
    .then_some(raw))
}

fn only<'tree>(
    node: Node<'tree>,
    kind: &str,
    cache: &mut CaptureCache,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        if child.kind() == "comment" {
            continue;
        }
        if child.kind() != kind || result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

fn plain_path<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if node.kind() != "value_path" || !clean(node, source) {
        return Ok(None);
    }
    let Some(leaf) = only(node, "value_name", cache, cancel)? else {
        return Ok(None);
    };
    Ok(
        (leaf.byte_range() == node.byte_range() && name(leaf, source, cache, cancel)?.is_some())
            .then_some(leaf),
    )
}

fn function<'tree>(
    item: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache,
    cancel: &Cancellation,
) -> Result<Option<Function<'tree>>, AdapterError> {
    if item.kind() != "value_definition" || !clean(item, source) {
        return Ok(None);
    }
    let Some(node) = only(item, "let_binding", cache, cancel)? else {
        return Ok(None);
    };
    let (Some(selected), Some(body)) = (
        node.child_by_field_name("pattern"),
        node.child_by_field_name("body"),
    ) else {
        return Ok(None);
    };
    if selected.kind() != "value_name"
        || name(selected, source, cache, cancel)?.is_none()
        || node.start_byte() != selected.start_byte()
        || !clean(body, source)
    {
        return Ok(None);
    }
    let mut parameters = Vec::new();
    let mut names = HashSet::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        if child == selected || child == body || child.kind() == "comment" {
            continue;
        }
        if child.kind() != "parameter" || child.child_by_field_name("default").is_some() {
            return Ok(None);
        }
        let Some(pattern) = child.child_by_field_name("pattern") else {
            return Ok(None);
        };
        if pattern.kind() != "value_pattern"
            || child.byte_range() != pattern.byte_range()
            || only(child, "value_pattern", cache, cancel)? != Some(pattern)
        {
            return Ok(None);
        }
        let Some(raw) = name(pattern, source, cache, cancel)? else {
            return Ok(None);
        };
        names
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        if !names.insert(raw) {
            return Ok(None);
        }
        parameters
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        parameters.push(pattern);
    }
    Ok((!parameters.is_empty()).then_some(Function {
        node,
        name: selected,
        body,
        parameters,
    }))
}

impl Proofs {
    pub(super) fn prepare(
        node: Node<'_>,
        source: &[u8],
        cache: &mut CaptureCache,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !cache.visit(cancel)? {
                return Ok(Self::default());
            }
            root = parent;
        }
        if !cache.complete_context
            || root.kind() != "compilation_unit"
            || !clean(root, source)
            || root.end_byte() != source.len()
            || !(root.start_byte() == 0
                || root.start_byte() == 3 && source.starts_with(b"\xef\xbb\xbf"))
        {
            return Ok(Self::default());
        }
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(root);
        while let Some(current) = pending.pop() {
            if !cache.visit(cancel)? {
                return Ok(Self::default());
            }
            if current.is_missing()
                || current.is_error()
                || matches!(
                    current.kind(),
                    "extension"
                        | "item_extension"
                        | "attribute"
                        | "item_attribute"
                        | "floating_attribute"
                        | "include_module"
                        | "include_module_type"
                )
            {
                return Ok(Self::default());
            }
            let mut cursor = current.walk();
            for child in current.children(&mut cursor) {
                if !cache.visit(cancel)? {
                    return Ok(Self::default());
                }
                pending
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                pending.push(child);
            }
        }
        let mut proofs = Self::default();
        let mut previous: Option<Function<'_>> = None;
        let mut cursor = root.walk();
        for item in root.named_children(&mut cursor) {
            if !cache.visit(cancel)? {
                return Ok(Self::default());
            }
            if item.kind() == "comment" {
                continue;
            }
            let current = function(item, source, cache, cancel)?;
            if let Some(owner) = current.as_ref() {
                if owner.body.kind() == "match_expression"
                    && let Some(scrutinee) = owner.body.child_by_field_name("expression")
                    && let Some(leaf) = plain_path(scrutinee, source, cache, cancel)?
                {
                    let raw = name(leaf, source, cache, cancel)?;
                    for parameter in &owner.parameters {
                        if !cache.visit(cancel)? {
                            return Ok(Self::default());
                        }
                        if name(*parameter, source, cache, cancel)? == raw {
                            proofs
                                .reads
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            proofs.reads.insert(
                                scrutinee.id(),
                                (parameter.byte_range(), scrutinee.byte_range()),
                            );
                        }
                    }
                }
                if let Some(target) = previous.as_ref()
                    && owner.body.kind() == "application_expression"
                    && let Some(callee) = owner.body.child_by_field_name("function")
                    && let Some(leaf) = plain_path(callee, source, cache, cancel)?
                    && name(leaf, source, cache, cancel)?
                        == name(target.name, source, cache, cancel)?
                {
                    let mut arguments = 0usize;
                    let mut admitted = true;
                    let mut children = owner.body.walk();
                    for (index, child) in owner.body.children(&mut children).enumerate() {
                        if !cache.visit(cancel)? {
                            return Ok(Self::default());
                        }
                        let index = u32::try_from(index)
                            .map_err(|_| super::query_failure("ocaml-native-child-index"))?;
                        if owner.body.field_name_for_child(index) == Some("argument") {
                            arguments = arguments
                                .checked_add(1)
                                .ok_or(SinkError::AllocationFailed)?;
                            admitted &= child.kind() == "number"
                                || plain_path(child, source, cache, cancel)?.is_some();
                        } else if child.is_named() && child != callee && child.kind() != "comment" {
                            admitted = false;
                        }
                    }
                    for parameter in &owner.parameters {
                        if !cache.visit(cancel)? {
                            return Ok(Self::default());
                        }
                        if name(*parameter, source, cache, cancel)?
                            == name(leaf, source, cache, cancel)?
                        {
                            admitted = false;
                        }
                    }
                    if admitted
                        && arguments == target.parameters.len()
                        && target.node.end_byte() < owner.node.start_byte()
                    {
                        proofs
                            .calls
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        proofs.calls.insert(
                            owner.body.id(),
                            (target.node.byte_range(), callee.byte_range()),
                        );
                    }
                }
            }
            // Any intervening root item, including open/include or a binding group,
            // removes adjacency. No imported or generic module lookup is performed.
            previous = current;
        }
        if cache.exhausted {
            return Ok(Self::default());
        }
        proofs.complete = true;
        Ok(proofs)
    }

    pub(super) fn candidates(
        &self,
        node: Node<'_>,
        role: StructuralRole,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<[Option<QueryCandidate>; 2], AdapterError> {
        cancel.check()?;
        if !self.complete {
            return Ok([None, None]);
        }
        let make = |range: Range<usize>, role, syntax| -> Result<QueryCandidate, AdapterError> {
            Ok(QueryCandidate {
                start: range.start,
                end: range.end,
                role,
                syntax,
                required: true,
                native_depth: native_depth(node, cancel)?,
            })
        };
        if node.kind() == "compilation_unit" && role == StructuralRole::Signature {
            return Ok([
                Some(make(
                    0..source.len(),
                    StructuralRole::Signature,
                    "ocaml.written_bindings_file",
                )?),
                None,
            ]);
        }
        if let Some((target, site)) = self.reads.get(&node.id()) {
            return Ok(match role {
                StructuralRole::Reference => {
                    [Some(make(site.clone(), role, "ocaml.match_formal")?), None]
                }
                StructuralRole::Signature => [
                    Some(make(
                        target.start..site.end,
                        role,
                        "ocaml.match_formal_target",
                    )?),
                    Some(make(
                        site.clone(),
                        StructuralRole::Reference,
                        "ocaml.match_formal",
                    )?),
                ],
                _ => [None, None],
            });
        }
        if let Some((target, site)) = self.calls.get(&node.id()) {
            return Ok(match role {
                StructuralRole::Call => [
                    Some(make(site.clone(), role, "ocaml.direct_file_call")?),
                    None,
                ],
                StructuralRole::Signature => [
                    Some(make(
                        target.start..site.end,
                        role,
                        "ocaml.direct_file_target",
                    )?),
                    Some(make(
                        site.clone(),
                        StructuralRole::Call,
                        "ocaml.direct_file_call",
                    )?),
                ],
                _ => [None, None],
            });
        }
        Ok([None, None])
    }
}

#[cfg(test)]
mod work_tests {
    use super::*;
    #[test]
    fn exhausted_ocaml_native_census_retains_no_prefix_certificate() {
        let source = include_bytes!("../../../tests/fixtures/ocaml_written/caml_bytestring.ml");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Ocaml))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        for work in [0, 8, 64] {
            let mut cache = CaptureCache::new(work).with_complete_context(true);
            let proof = Proofs::prepare(tree.root_node(), source, &mut cache, &Cancellation::new())
                .unwrap();
            assert!(cache.exhausted());
            assert!(!proof.complete);
            assert!(proof.reads.is_empty() && proof.calls.is_empty());
        }
    }
}
