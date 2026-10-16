//! Eiffel captures prove written owners without inferring feature or type binding.
//! Shared feature heads retain individual names and an explicit ownership gap.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct Feature {
    range: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
    names: usize,
}

pub(super) struct CaptureCache {
    features: HashMap<usize, Option<Feature>>,
    visibility: HashMap<usize, bool>,
    remaining: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(remaining: usize) -> Self {
        Self {
            features: HashMap::new(),
            visibility: HashMap::new(),
            remaining,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn visible(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut parent = node.parent();
        let mut pending = Vec::new();
        let mut visible = true;
        while let Some(node) = parent {
            if let Some(found) = self.visibility.get(&node.id()) {
                visible = *found;
                break;
            }
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(node.id());
            if matches!(
                node.kind(),
                "ERROR"
                    | "comment"
                    | "header_comment"
                    | "basic_manifest_string"
                    | "verbatim_string"
            ) {
                visible = false;
                break;
            }
            parent = node.parent();
        }
        self.visibility
            .try_reserve(pending.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for id in pending {
            self.visibility.insert(id, visible);
        }
        Ok(visible)
    }

    fn feature(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Feature>, AdapterError> {
        if let Some(found) = self.features.get(&node.id()) {
            return Ok(found.clone());
        }
        let found = feature(node, source, cancellation, self)?;
        self.features
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.features.insert(node.id(), found.clone());
        Ok(found)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn identifier(node: Node<'_>, source: &[u8]) -> bool {
    clean(node, source)
        && std::str::from_utf8(&source[node.byte_range()])
            .ok()
            .is_some_and(|text| {
                rootlight_adapter_sdk::structural_captured_name_for_language(
                    "eiffel",
                    text,
                    text.len(),
                )
                .is_some()
            })
}

fn closed(node: Node<'_>, source: &[u8]) -> bool {
    node.child_count()
        .checked_sub(1)
        .and_then(|index| u32::try_from(index).ok())
        .and_then(|index| node.child(index))
        .is_some_and(|last| {
            last.kind() == "end"
                && clean(last, source)
                && source
                    .get(last.byte_range())
                    .is_some_and(|text| text.eq_ignore_ascii_case(b"end"))
        })
}

fn feature(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Feature>, AdapterError> {
    if node.kind() != "feature_declaration" {
        return Ok(None);
    }
    let mut names = 0;
    let mut syntax = "eiffel.field";
    let mut header_end = node.end_byte();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == "attribute_or_routine" {
            if !closed(child, source) {
                return Ok(None);
            }
            let Some(body) = direct(child, "feature_body", cancellation, cache)? else {
                return Ok(None);
            };
            syntax = if direct(body, "attribute", cancellation, cache)?.is_some() {
                "eiffel.field"
            } else {
                "eiffel.routine"
            };
            header_end = child.start_byte();
            break;
        }
        // Body errors cannot invalidate a clean head; errors in its type or name can.
        if !clean(child, source) {
            return Ok(None);
        }
        match child.kind() {
            "new_feature" => names += 1,
            "explicit_value" => {
                syntax = "eiffel.constant";
                header_end = child.start_byte();
            }
            _ => {}
        }
    }
    Ok((names > 0).then_some(Feature {
        range: node.byte_range(),
        header: node.start_byte()..header_end,
        syntax,
        names,
    }))
}

fn declaration_identifier(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "entity_declaration_group" | "separate")
            || (parent.kind() == "extended_feature_name"
                && parent
                    .parent()
                    .is_some_and(|parent| parent.kind() == "new_feature"))
    })
}

fn binding_owner(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(owner) = parent {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
        if owner.kind() == "inline_agent" {
            return Ok(false);
        }
        if owner.kind() == "feature_declaration" {
            return Ok(cache
                .feature(owner, source, cancellation)?
                .is_some_and(|proof| proof.names == 1));
        }
        parent = owner.parent();
    }
    Ok(false)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancellation)? || !cache.visible(node, cancellation)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    if source.get(range.clone()).is_none() {
        return Err(query_failure("query-eiffel-source-range"));
    }
    let syntax = if node.kind() == "source_file"
        && matches!(role, StructuralRole::Root | StructuralRole::Module)
    {
        range = 0..source.len();
        "eiffel.file"
    } else if node.kind() == "class_declaration" {
        let name = direct(node, "class_name", cancellation, cache)?
            .filter(|name| identifier(*name, source));
        let Some(name) = name.filter(|_| closed(node, source)) else {
            return Ok(
                (role == StructuralRole::Signature).then_some(QueryCandidate {
                    start: range.start,
                    end: range.end,
                    role,
                    syntax: "eiffel.unsupported_owner",
                    required: true,
                    native_depth: native_depth(node, cancellation)?,
                }),
            );
        };
        match role {
            StructuralRole::Declaration => "eiffel.class",
            StructuralRole::Definition => {
                range = name.byte_range();
                "eiffel.name"
            }
            StructuralRole::Signature => {
                range.end = name.end_byte();
                if let Some(generics) = direct(node, "formal_generics", cancellation, cache)?
                    && clean(generics, source)
                {
                    range.end = generics.end_byte();
                }
                "eiffel.written_header"
            }
            _ => return Ok(None),
        }
    } else if node.kind() == "new_feature" {
        let Some(owner) = node.parent() else {
            return Ok(None);
        };
        let Some(proof) = cache.feature(owner, source, cancellation)? else {
            return Ok(
                (role == StructuralRole::Signature).then_some(QueryCandidate {
                    start: range.start,
                    end: range.end,
                    role,
                    syntax: "eiffel.unsupported_owner",
                    required: true,
                    native_depth: native_depth(node, cancellation)?,
                }),
            );
        };
        let Some(extended) = direct(node, "extended_feature_name", cancellation, cache)? else {
            return Ok(None);
        };
        let Some(name) = direct(extended, "identifier", cancellation, cache)?
            .filter(|name| identifier(*name, source))
        else {
            return Ok(None);
        };
        match role {
            StructuralRole::Declaration => {
                range = if proof.names == 1 {
                    proof.range
                } else {
                    name.byte_range()
                };
                proof.syntax
            }
            StructuralRole::Definition => {
                range = name.byte_range();
                "eiffel.name"
            }
            StructuralRole::Signature => {
                if proof.names == 1 {
                    range = proof.header;
                    "eiffel.written_header"
                } else {
                    "eiffel.shared_feature"
                }
            }
            _ => return Ok(None),
        }
    } else if node.kind() == "class_name"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "formal_generic")
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        )
        && identifier(node, source)
    {
        match role {
            StructuralRole::Declaration => "eiffel.type_parameter",
            StructuralRole::Definition => "eiffel.name",
            StructuralRole::Signature => "eiffel.written_parameter",
            _ => return Ok(None),
        }
    } else if node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "entity_declaration_group")
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        )
        && identifier(node, source)
    {
        let Some(group) = node.parent().filter(|group| clean(*group, source)) else {
            return Ok(None);
        };
        if !binding_owner(node, source, cancellation, cache)? {
            return Ok(None);
        }
        let kind = match group.parent().map(|parent| parent.kind()) {
            Some("formal_arguments") => "eiffel.parameter",
            Some("local_declarations") => "eiffel.local",
            _ => return Ok(None),
        };
        match role {
            StructuralRole::Declaration => kind,
            StructuralRole::Definition => "eiffel.name",
            StructuralRole::Signature => "eiffel.written_parameter",
            _ => return Ok(None),
        }
    } else if node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "separate")
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        )
        && identifier(node, source)
    {
        if !node
            .parent()
            .is_some_and(|parent| clean(parent, source) && closed(parent, source))
            || !binding_owner(node, source, cancellation, cache)?
        {
            return Ok(None);
        }
        match role {
            StructuralRole::Declaration => "eiffel.local",
            StructuralRole::Definition => "eiffel.name",
            StructuralRole::Signature => "eiffel.written_parameter",
            _ => return Ok(None),
        }
    } else if node.kind() == "free_operator"
        && clean(node, source)
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "unary" | "operator_expression"))
    {
        match role {
            StructuralRole::Call => "eiffel.written_operator",
            StructuralRole::Signature => "eiffel.operator_binding_semantics",
            _ => return Ok(None),
        }
    } else if node.kind() == "unqualified_call"
        && matches!(role, StructuralRole::Call | StructuralRole::Reference)
        && clean(node, source)
    {
        let Some(name) = direct(node, "identifier", cancellation, cache)?
            .filter(|name| identifier(*name, source))
        else {
            return Ok(None);
        };
        let has_actuals = direct(node, "actuals", cancellation, cache)?.is_some();
        let creation = node
            .parent()
            .is_some_and(|parent| parent.kind() == "creation");
        range = name.byte_range();
        if has_actuals || creation {
            if role != StructuralRole::Call {
                return Ok(None);
            }
            "eiffel.written_call"
        } else {
            if role != StructuralRole::Reference {
                return Ok(None);
            }
            "eiffel.identifier"
        }
    } else if role == StructuralRole::Reference
        && node.kind() == "class_name"
        && identifier(node, source)
    {
        if node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "class_declaration" | "formal_generic"))
        {
            return Ok(None);
        }
        "eiffel.type"
    } else if role == StructuralRole::Reference
        && node.kind() == "identifier"
        && identifier(node, source)
    {
        if declaration_identifier(node)
            || node
                .parent()
                .is_some_and(|parent| parent.kind() == "unqualified_call")
        {
            return Ok(None);
        }
        "eiffel.identifier"
    } else if role == StructuralRole::Comment
        && matches!(node.kind(), "comment" | "header_comment")
        && clean(node, source)
    {
        "eiffel.comment"
    } else if role == StructuralRole::StringLiteral
        && matches!(node.kind(), "basic_manifest_string" | "verbatim_string")
        && clean(node, source)
    {
        "eiffel.string"
    } else if role == StructuralRole::Signature && node.kind() == "separate" {
        "eiffel.separate_binding_semantics"
    } else if role == StructuralRole::Signature && node.kind() == "formal_generic" {
        "eiffel.generic_binding_semantics"
    } else if role == StructuralRole::Signature
        && node.kind() == "basic_manifest_string"
        && source[node.byte_range()].contains(&b'\n')
    {
        "eiffel.unproven_multiline_string"
    } else if role == StructuralRole::Signature
        && (matches!(
            node.kind(),
            "iteration" | "object_test" | "inline_agent" | "rename_pair"
        ) || (node.kind() == "extended_feature_name" && node.named_child_count() > 1))
    {
        "eiffel.binding_semantics"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
        ),
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrammarFamily, query_pack::QueryPack};

    #[test]
    fn eiffel_recovered_feature_heads_have_exact_native_provenance() {
        let source =
            "class RECOVERED feature stable: INTEGER = 42 broken: INTEGER = 0xFF tail: INTEGER end";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_eiffel::LANGUAGE_EIFFEL.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let pack = QueryPack::compile(
            GrammarFamily::Eiffel,
            include_str!("../../queries/eiffel.scm"),
        )
        .unwrap();
        let extraction = pack
            .extract(
                GrammarFamily::Eiffel,
                &tree,
                source.as_bytes(),
                4096,
                4096,
                &Cancellation::new(),
            )
            .unwrap();
        for expected in ["RECOVERED", "stable", "tail"] {
            assert!(
                extraction
                    .candidates
                    .iter()
                    .any(|candidate| candidate.role == StructuralRole::Definition
                        && &source[candidate.start..candidate.end] == expected),
                "missing {expected}; {}",
                tree.root_node().to_sexp()
            );
        }
    }
}
