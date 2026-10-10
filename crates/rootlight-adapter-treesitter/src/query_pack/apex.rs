//! Apex captures retain direct written declarations and unresolved source uses.
//! Shared field types remain separate source evidence; owner proofs are bounded
//! to one immutable scan and never turn parser recovery into class membership.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    declarators: usize,
}

/// Per-scan native owner evidence shares the existing query capture ceiling.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    /// Creates an empty bounded cache without reserving input-sized storage.
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    /// Reports a bound that the caller must propagate as a capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self.owners.get(&node.id()) {
            return Ok(proof.clone());
        }
        if self.owners.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(None);
        }
        let proof = owner_proof(node, source, cancellation)?;
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), proof.clone());
        Ok(proof)
    }
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Option<Node<'tree>> {
    node.child_by_field_name(name)
        .filter(|child| child.parent() == Some(node))
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn owner_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "constructor_declaration"
            | "method_declaration"
            | "field_declaration"
            | "formal_parameter"
            | "enum_constant"
    )
}

fn written_name(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() != "identifier" || !clean(node, source) {
        return false;
    }
    let Some(text) = source
        .get(node.byte_range())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return false;
    };
    !text.is_empty()
        && structural_captured_name_for_language("apex", text, text.len()).as_deref() == Some(text)
}

fn boundary(
    node: Node<'_>,
    source: &[u8],
    opening: &[u8],
    closing: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !clean(node, source) {
        return Ok(false);
    }
    let mut first = None;
    let mut last = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() {
            first.get_or_insert(child);
            last = Some(child);
        }
    }
    Ok(matches!((first, last), (Some(first), Some(last))
        if clean(first, source) && clean(last, source)
            && first.start_byte() == node.start_byte()
            && last.end_byte() == node.end_byte()
            && source.get(first.byte_range()) == Some(opening)
            && source.get(last.byte_range()) == Some(closing)))
}

fn terminal_semicolon<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut last = None;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() {
            last = Some(child);
        }
    }
    Ok(last.filter(|child| {
        clean(*child, source)
            && child.end_byte() == node.end_byte()
            && source.get(child.byte_range()) == Some(b";")
    }))
}

fn header_before(
    node: Node<'_>,
    delimiter: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    let mut end = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.start_byte() >= delimiter.start_byte() {
            break;
        }
        if !child.is_extra() && child.end_byte() <= delimiter.start_byte() {
            end = Some(child.end_byte());
        }
    }
    Ok(end
        .filter(|end| *end > node.start_byte())
        .map(|end| node.start_byte()..end))
}

fn member_owner(node: Node<'_>) -> Option<Node<'_>> {
    let body = node.parent()?;
    let owner = body.parent()?;
    let kind_matches = matches!(
        (body.kind(), owner.kind()),
        ("class_body", "class_declaration") | ("interface_body", "interface_declaration")
    );
    (kind_matches && field(owner, "body") == Some(body)).then_some(owner)
}

fn declaration_position(node: Node<'_>) -> bool {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            node.parent()
                .is_some_and(|parent| parent.kind() == "parser_output")
                || member_owner(node).is_some()
        }
        "constructor_declaration" | "field_declaration" => {
            member_owner(node).is_some_and(|owner| owner.kind() == "class_declaration")
        }
        "method_declaration" => member_owner(node).is_some(),
        "enum_constant" => node.parent().is_some_and(|body| {
            body.kind() == "enum_body"
                && body.parent().is_some_and(|owner| {
                    owner.kind() == "enum_declaration" && field(owner, "body") == Some(body)
                })
        }),
        "formal_parameter" => node.parent().is_some_and(|parameters| {
            parameters.kind() == "formal_parameters"
                && parameters.parent().is_some_and(|owner| {
                    matches!(
                        owner.kind(),
                        "constructor_declaration" | "method_declaration"
                    ) && field(owner, "parameters") == Some(parameters)
                })
        }),
        _ => false,
    }
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if !clean(node, source) || !declaration_position(node) {
        return Ok(None);
    }
    let mut declarators = 0usize;
    let header = match node.kind() {
        "field_declaration" => {
            if field(node, "type").is_none_or(|ty| !clean(ty, source))
                || terminal_semicolon(node, source, cancellation)?.is_none()
            {
                return Ok(None);
            }
            let mut cursor = node.walk();
            for child in node.children_by_field_name("declarator", &mut cursor) {
                cancellation.check()?;
                if child.parent() != Some(node)
                    || child.kind() != "variable_declarator"
                    || !clean(child, source)
                    || field(child, "name").is_none_or(|name| !written_name(name, source))
                {
                    return Ok(None);
                }
                declarators = declarators
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-apex-declarator-accounting"))?;
            }
            if declarators == 0 {
                return Ok(None);
            }
            node.byte_range()
        }
        "formal_parameter" => {
            if field(node, "type").is_none_or(|ty| !clean(ty, source))
                || field(node, "name").is_none_or(|name| !written_name(name, source))
            {
                return Ok(None);
            }
            node.byte_range()
        }
        "enum_constant" => {
            if field(node, "name").is_none_or(|name| !written_name(name, source)) {
                return Ok(None);
            }
            node.byte_range()
        }
        _ => {
            if field(node, "name").is_none_or(|name| !written_name(name, source)) {
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "constructor_declaration" | "method_declaration"
            ) {
                let Some(parameters) = field(node, "parameters") else {
                    return Ok(None);
                };
                if !boundary(parameters, source, b"(", b")", cancellation)?
                    || (node.kind() == "method_declaration"
                        && field(node, "type").is_none_or(|ty| !clean(ty, source)))
                {
                    return Ok(None);
                }
            }
            let delimiter = if let Some(body) = field(node, "body") {
                if !boundary(body, source, b"{", b"}", cancellation)? {
                    return Ok(None);
                }
                body
            } else if node.kind() == "method_declaration" {
                let Some(semicolon) = terminal_semicolon(node, source, cancellation)? else {
                    return Ok(None);
                };
                semicolon
            } else {
                return Ok(None);
            };
            let Some(header) = header_before(node, delimiter, cancellation)? else {
                return Ok(None);
            };
            header
        }
    };
    Ok(Some(OwnerProof {
        extent: node.byte_range(),
        header,
        declarators,
    }))
}

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let target = node.byte_range();
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.is_missing() || owner.kind() == "ERROR" {
            return Ok(false);
        }
        if owner_kind(owner) {
            let Some(proof) = cache.owner(owner, source, cancellation)? else {
                return Ok(false);
            };
            if target.start < proof.extent.start || target.end > proof.extent.end {
                return Ok(false);
            }
        }
        // Root error state may describe an unrelated malformed sibling.
        current = owner.parent();
    }
    Ok(true)
}

fn field_owner(node: Node<'_>) -> Option<Node<'_>> {
    node.parent().filter(|parent| {
        parent.kind() == "field_declaration" && node.kind() == "variable_declarator"
    })
}

fn definition_node(node: Node<'_>) -> Option<Node<'_>> {
    field(node, "name")
}

fn nonlexical_identifier(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let target = node.byte_range();
    let mut current = Some(node);
    while let Some(child) = current {
        cancellation.check()?;
        let Some(parent) = child.parent() else {
            break;
        };
        if matches!(
            parent.kind(),
            "annotation"
                | "type_arguments"
                | "type_parameters"
                | "superclass"
                | "interfaces"
                | "extends_interfaces"
                | "scoped_type_identifier"
                | "generic_type"
        ) || (field(parent, "type")
            .is_some_and(|ty| ty.start_byte() <= target.start && target.end <= ty.end_byte()))
        {
            return Ok(true);
        }
        if (owner_kind(parent) || matches!(parent.kind(), "variable_declarator" | "type_parameter"))
            && field(parent, "name") == Some(child)
        {
            return Ok(true);
        }
        current = Some(parent);
    }
    Ok(false)
}

/// Admits source identities only under clean, direct native declaration owners.
/// Cancellation/allocation errors propagate; exhausted cache entries are exposed
/// to the caller so a partial identity scan cannot masquerade as complete.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "parser_output" || role == StructuralRole::Expression {
        return Ok(true);
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::Documentation | StructuralRole::StringLiteral
    ) {
        return Ok(clean(node, source));
    }
    if !valid_owner_chain(node, source, cancellation, cache)? {
        return Ok(false);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        if let Some(owner) = field_owner(node) {
            let Some(proof) = cache.owner(owner, source, cancellation)? else {
                return Ok(false);
            };
            return Ok(clean(node, source)
                && definition_node(node).is_some_and(|name| written_name(name, source))
                && (role != StructuralRole::Signature || proof.declarators == 1));
        }
        return Ok(owner_kind(node) && cache.owner(node, source, cancellation)?.is_some());
    }
    if role == StructuralRole::Reference {
        return Ok(written_name(node, source) && !nonlexical_identifier(node, cancellation)?);
    }
    Ok(clean(node, source))
}

/// Maps admitted captures to original byte ranges without rewriting source.
/// Multi-field headers intentionally remain non-container expression evidence,
/// and unresolved member/call spellings never establish target bindings.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    if matches!(
        role,
        StructuralRole::Call | StructuralRole::CallName | StructuralRole::ScopedCall
    ) {
        return Err(query_failure("query-apex-unproven-call"));
    }
    let mut depth_node = node;
    let mut range = node.byte_range();
    let syntax = if node.kind() == "parser_output" {
        range = 0..source.len();
        "apex.file"
    } else if role == StructuralRole::Expression {
        if node.kind() == "field_declaration" && cache.owner(node, source, cancellation)?.is_none()
        {
            if clean(node, source) && member_owner(node).is_some() {
                "apex.accessor_semantics"
            } else {
                "apex.unsupported_owner"
            }
        } else if !valid_owner_chain(node, source, cancellation, cache)? {
            "apex.unsupported_owner"
        } else {
            match node.kind() {
                "field_declaration"
                    if cache
                        .owner(node, source, cancellation)?
                        .is_some_and(|proof| proof.declarators > 1) =>
                {
                    "apex.shared_field_header"
                }
                "method_invocation" => "apex.application",
                "field_access" => "apex.member_access",
                "annotation" => "apex.annotation_semantics",
                _ => "apex.body_semantics",
            }
        }
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        let syntax = if let Some(owner) = field_owner(node) {
            let proof = cache
                .owner(owner, source, cancellation)?
                .ok_or_else(|| query_failure("query-apex-field-owner"))?;
            if proof.declarators == 1 {
                depth_node = owner;
                range = if role == StructuralRole::Signature {
                    proof.header
                } else {
                    proof.extent
                };
                "apex.field"
            } else {
                "apex.field_fragment"
            }
        } else {
            if role == StructuralRole::Signature {
                range = cache
                    .owner(node, source, cancellation)?
                    .ok_or_else(|| query_failure("query-apex-header"))?
                    .header;
            }
            match node.kind() {
                "class_declaration" => "apex.class",
                "interface_declaration" => "apex.interface",
                "enum_declaration" => "apex.enum",
                "constructor_declaration" => "apex.constructor",
                "method_declaration" => "apex.method",
                "formal_parameter" => "apex.parameter",
                "enum_constant" => "apex.constant",
                _ => return Err(query_failure("query-apex-declaration-kind")),
            }
        };
        if role == StructuralRole::Definition {
            range = definition_node(node)
                .ok_or_else(|| query_failure("query-apex-definition-name"))?
                .byte_range();
        }
        syntax
    } else if role == StructuralRole::Reference {
        match node.parent() {
            Some(parent)
                if parent.kind() == "method_invocation" && field(parent, "name") == Some(node) =>
            {
                "apex.unproven_call"
            }
            Some(parent)
                if parent.kind() == "field_access" && field(parent, "field") == Some(node) =>
            {
                "apex.member"
            }
            _ => "apex.identifier",
        }
    } else {
        match role {
            StructuralRole::Comment | StructuralRole::Documentation => "apex.comment",
            StructuralRole::StringLiteral => "apex.string",
            StructuralRole::Scope => "apex.body",
            _ => return Err(query_failure("query-apex-unproven-role")),
        }
    };
    let mut depth = native_depth(depth_node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "parser_output" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-apex-depth"))?;
    }
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    })
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use tree_sitter::{Parser, Tree};

    use super::*;

    fn tree(source: &[u8]) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Apex))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn owner_cache_reuses_proofs_and_fails_closed_at_its_bound() {
        let source = b"class First {} class Second {}";
        let tree = tree(source);
        let first = tree.root_node().named_child(0).unwrap();
        let second = tree.root_node().named_child(1).unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1);
        for _ in 0..4 {
            assert!(valid_owner_chain(first, source, &cancellation, &mut cache).unwrap());
        }
        assert_eq!(cache.owners.len(), 1);
        assert!(!valid_owner_chain(second, source, &cancellation, &mut cache).unwrap());
        assert!(cache.exhausted());
        assert_eq!(cache.owners.len(), 1);
    }

    #[test]
    fn cancelled_cached_owner_proof_is_not_reused() {
        let source = b"class Written {}";
        let tree = tree(source);
        let owner = tree.root_node().named_child(0).unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1);
        assert!(valid_owner_chain(owner, source, &cancellation, &mut cache).unwrap());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(valid_owner_chain(owner, source, &cancellation, &mut cache).is_err());
        assert!(
            candidate(
                owner,
                StructuralRole::Signature,
                source,
                &cancellation,
                &mut cache
            )
            .is_err()
        );
    }
}
