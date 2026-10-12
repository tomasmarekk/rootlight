//! Pascal captures retain direct written owners without compiler resolution.
//! Shared groups have independent name spans; qualified implementations do not
//! acquire remote type owners, and preprocessor alternatives remain source-only.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
    names: usize,
    forward: bool,
}

/// Shares the query scan's existing capture bound and immutable source lifetime.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

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

fn child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind && !child.is_missing() {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn identifier(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"&").unwrap_or(bytes);
    bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn written_name(node: Node<'_>, source: &[u8]) -> bool {
    clean(node, source)
        && matches!(node.kind(), "identifier" | "genericDot" | "moduleName")
        && source
            .get(node.byte_range())
            .is_some_and(|bytes| bytes.split(|byte| *byte == b'.').all(identifier))
}

fn terminal_name<'tree>(
    mut node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if !written_name(node, source) {
        return Ok(None);
    }
    while node.kind() == "genericDot" {
        cancellation.check()?;
        let Some(rhs) = field(node, "rhs") else {
            return Ok(None);
        };
        node = rhs;
    }
    Ok((node.kind() == "identifier").then_some(node))
}

fn is_module(node: Node<'_>) -> bool {
    matches!(node.kind(), "unit" | "program" | "library")
}
fn is_group(node: Node<'_>) -> bool {
    matches!(node.kind(), "declField" | "declVar" | "declArg")
}
fn is_owner(node: Node<'_>) -> bool {
    is_module(node)
        || is_group(node)
        || matches!(
            node.kind(),
            "declType" | "declProc" | "defProc" | "declProp"
        )
}
fn implementation_header(node: Node<'_>) -> bool {
    node.kind() == "declProc"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "defProc" && field(parent, "header") == Some(node)
        })
}

fn semicolon(node: Node<'_>, source: &[u8]) -> bool {
    source
        .get(node.byte_range())
        .is_some_and(|bytes| bytes.last() == Some(&b';'))
}

fn has_direct_pp(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    Ok(child(node, "pp", cancellation)?.is_some())
}

fn routine_syntax(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    for (kind, syntax) in [
        ("kProcedure", "pascal.procedure"),
        ("kFunction", "pascal.function"),
        ("kConstructor", "pascal.constructor"),
        ("kDestructor", "pascal.destructor"),
    ] {
        if child(node, kind, cancellation)?.is_some() {
            return Ok(Some(syntax));
        }
    }
    Ok(None)
}

fn type_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    let Some(name) = field(node, "name") else {
        return Ok(None);
    };
    if name.kind() != "identifier" || !written_name(name, source) || !semicolon(node, source) {
        return Ok(None);
    }
    let mut cursor = node.walk();
    let mut selected = None;
    for ty in node.children_by_field_name("type", &mut cursor) {
        cancellation.check()?;
        if matches!(ty.kind(), "type" | "declClass" | "declIntf") {
            if selected.replace(ty).is_some() || !clean(ty, source) {
                return Ok(None);
            }
        } else if ty.kind() != "kType" {
            return Ok(None);
        }
    }
    let Some(ty) = selected else {
        return Ok(None);
    };
    if ty.kind() == "type" {
        return Ok(Some(OwnerProof {
            extent: node.byte_range(),
            header: node.byte_range(),
            syntax: "pascal.type_alias",
            names: 1,
            forward: false,
        }));
    }
    let (syntax, forward_syntax) = if ty.kind() == "declIntf" {
        ("pascal.interface", "pascal.forward_interface")
    } else if child(ty, "kClass", cancellation)?.is_some() {
        ("pascal.class", "pascal.forward_class")
    } else if child(ty, "kRecord", cancellation)?.is_some() {
        ("pascal.record", "pascal.unsupported_owner")
    } else if child(ty, "kObject", cancellation)?.is_some() {
        ("pascal.object", "pascal.unsupported_owner")
    } else {
        return Ok(None);
    };
    let mut cursor = ty.walk();
    let mut boundary = None;
    let mut complete = false;
    for member in ty.named_children(&mut cursor) {
        cancellation.check()?;
        if member.kind() == "kEnd" {
            complete = true;
        }
        if boundary.is_none()
            && matches!(
                member.kind(),
                "declField"
                    | "declProc"
                    | "declProp"
                    | "declSection"
                    | "declTypes"
                    | "declVars"
                    | "declConsts"
                    | "declVariant"
                    | "kEnd"
            )
        {
            boundary = Some(member.start_byte());
        }
    }
    if !complete && forward_syntax == "pascal.unsupported_owner" {
        return Ok(None);
    }
    Ok(Some(OwnerProof {
        extent: node.byte_range(),
        header: node.start_byte()..boundary.unwrap_or(node.end_byte()),
        syntax: if complete { syntax } else { forward_syntax },
        names: 1,
        forward: !complete,
    }))
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if is_module(node) {
        // Module header identity is local: an unrelated damaged sibling does not
        // erase clean declarations. Full compiler acceptance stays unqualified.
        if node.is_missing() || source.get(node.byte_range()).is_none() {
            return Ok(None);
        }
        let keyword = match node.kind() {
            "unit" => "kUnit",
            "program" => "kProgram",
            _ => "kLibrary",
        };
        let (Some(opening), Some(name), Some(end)) = (
            child(node, keyword, cancellation)?,
            child(node, "moduleName", cancellation)?,
            child(node, "kEndDot", cancellation)?,
        ) else {
            return Ok(None);
        };
        if !clean(opening, source)
            || !written_name(name, source)
            || !clean(end, source)
            || opening.start_byte() != node.start_byte()
            || end.end_byte() != node.end_byte()
        {
            return Ok(None);
        }
        let mut cursor = node.walk();
        let mut terminator = None;
        for next in node.children(&mut cursor) {
            cancellation.check()?;
            if next.kind() == ";" && next.start_byte() >= name.end_byte() {
                terminator = Some(next);
                break;
            }
        }
        let Some(terminator) = terminator else {
            return Ok(None);
        };
        if !clean(terminator, source) {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            extent: node.byte_range(),
            header: opening.start_byte()..terminator.end_byte(),
            syntax: match node.kind() {
                "unit" => "pascal.unit",
                "program" => "pascal.program",
                _ => "pascal.library",
            },
            names: 1,
            forward: false,
        }));
    }
    if !clean(node, source) || has_direct_pp(node, cancellation)? {
        return Ok(None);
    }
    if node.kind() == "declType" {
        return type_proof(node, source, cancellation);
    }
    if node.kind() == "defProc" {
        let Some(header) = field(node, "header") else {
            return Ok(None);
        };
        let Some(proof) = owner_proof(header, source, cancellation)? else {
            return Ok(None);
        };
        let mut cursor = node.walk();
        let mut body = None;
        for next in node.children_by_field_name("body", &mut cursor) {
            cancellation.check()?;
            if body.replace(next).is_some()
                || !clean(next, source)
                || !matches!(next.kind(), "block" | "asm")
            {
                return Ok(None);
            }
        }
        let Some(body) = body else {
            return Ok(None);
        };
        if header.start_byte() != node.start_byte()
            || header.end_byte() > body.start_byte()
            || body.end_byte() > node.end_byte()
            || !semicolon(node, source)
        {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            extent: node.byte_range(),
            header: header.byte_range(),
            ..proof
        }));
    }
    if node.kind() == "declProc" {
        let Some(name) = field(node, "name") else {
            return Ok(None);
        };
        let Some(syntax) = routine_syntax(node, cancellation)? else {
            return Ok(None);
        };
        if !written_name(name, source) || !semicolon(node, source) {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            extent: node.byte_range(),
            header: node.byte_range(),
            syntax,
            names: 1,
            forward: false,
        }));
    }
    if node.kind() == "declProp" {
        if field(node, "name").is_none_or(|name| !written_name(name, source))
            || field(node, "type").is_none_or(|ty| !clean(ty, source))
            || !semicolon(node, source)
        {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            extent: node.byte_range(),
            header: node.byte_range(),
            syntax: "pascal.property",
            names: 1,
            forward: false,
        }));
    }
    if is_group(node) {
        if field(node, "type").is_some_and(|ty| !clean(ty, source))
            || (node.kind() != "declArg"
                && (!semicolon(node, source) || field(node, "type").is_none()))
        {
            return Ok(None);
        }
        let mut cursor = node.walk();
        let mut names = 0usize;
        for name in node.children_by_field_name("name", &mut cursor) {
            cancellation.check()?;
            if name.kind() == "," {
                continue;
            }
            if name.kind() != "identifier" || !written_name(name, source) {
                return Ok(None);
            }
            names = names
                .checked_add(1)
                .ok_or_else(|| query_failure("query-pascal-name-accounting"))?;
        }
        if names == 0 {
            return Ok(None);
        }
        let syntax = match node.kind() {
            "declField" => "pascal.field",
            "declVar" => "pascal.variable",
            _ => "pascal.parameter",
        };
        return Ok(Some(OwnerProof {
            extent: node.byte_range(),
            header: node.byte_range(),
            syntax,
            names,
            forward: false,
        }));
    }
    Ok(None)
}

fn group_owner<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    node.parent()
        .filter(|parent| is_group(*parent) && node.kind() == "identifier")
}

fn group_name(node: Node<'_>, group: Node<'_>) -> bool {
    // The native grammar puts name fields before the direct type. This also
    // excludes a direct ABSOLUTE reference without rescanning every group name.
    node.parent() == Some(group)
        && node.kind() == "identifier"
        && field(group, "type").is_none_or(|ty| node.end_byte() <= ty.start_byte())
}

fn type_owner<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        if parent.kind() == "declType" {
            return Ok(Some(parent));
        }
        if matches!(
            parent.kind(),
            "defProc" | "declProc" | "unit" | "program" | "library"
        ) {
            return Ok(None);
        }
        node = parent;
    }
    Ok(None)
}

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.kind() == "ERROR" || owner.is_missing() {
            return Ok(false);
        }
        if is_owner(owner) && cache.owner(owner, source, cancellation)?.is_none() {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

fn name_field_contains(
    node: Node<'_>,
    parent: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = parent.walk();
    for name in parent.children_by_field_name("name", &mut cursor) {
        cancellation.check()?;
        if name == node {
            return Ok(true);
        }
    }
    Ok(false)
}

fn declaration_position(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    if let Some(group) = group_owner(node) {
        if !group_name(node, group) {
            return Ok(false);
        }
        return Ok(match group.kind() {
            "declArg" => group.parent().is_some_and(|args| {
                matches!(args.kind(), "declArgs" | "declPropArgs")
                    && args.parent().is_some_and(|owner| {
                        matches!(owner.kind(), "declProc" | "declProp")
                            && field(owner, "args") == Some(args)
                    })
            }),
            "declField" => {
                group.parent().is_some_and(|parent| {
                    matches!(parent.kind(), "declClass" | "declIntf" | "declSection")
                }) && type_owner(group, cancellation)?.is_some()
            }
            "declVar" => group
                .parent()
                .is_some_and(|parent| parent.kind() == "declVars"),
            _ => false,
        });
    }
    Ok(is_owner(node) && !is_group(node) && !implementation_header(node))
}

fn name_node<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if group_owner(node).is_some() {
        return Ok(Some(node));
    }
    if is_module(node) {
        return child(node, "moduleName", cancellation);
    }
    let owner = if node.kind() == "defProc" {
        field(node, "header")
    } else {
        Some(node)
    };
    let Some(name) = owner.and_then(|owner| field(owner, "name")) else {
        return Ok(None);
    };
    terminal_name(name, source, cancellation)
}

fn declaration_name_ancestor(
    mut node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        if is_group(parent) {
            return Ok(group_name(node, parent));
        }
        if name_field_contains(node, parent, cancellation)?
            && (is_owner(parent) || parent.kind() == "declConst")
        {
            return Ok(true);
        }
        if !matches!(parent.kind(), "genericDot" | "genericTpl" | "moduleName") {
            break;
        }
        if parent.kind() == "moduleName" {
            return Ok(true);
        }
        node = parent;
    }
    Ok(false)
}

/// Admits source-owned identities and unresolved reads within intact native owners.
/// Cancellation and owner-cache exhaustion propagate without increasing limits.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "root" {
        return Ok(true);
    }
    if implementation_header(node) {
        return Ok(false);
    }
    if role == StructuralRole::Expression {
        return Ok(true);
    }
    if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        if !declaration_position(node, cancellation)?
            || !valid_owner_chain(node, source, cancellation, cache)?
        {
            return Ok(false);
        }
        let owner = group_owner(node).unwrap_or(node);
        let Some(proof) = cache.owner(owner, source, cancellation)? else {
            return Ok(false);
        };
        if role == StructuralRole::Signature && proof.names > 1 {
            return Ok(false);
        }
        if role == StructuralRole::Scope && proof.forward {
            return Ok(false);
        }
        return Ok(true);
    }
    if !clean(node, source) || !valid_owner_chain(node, source, cancellation, cache)? {
        return Ok(false);
    }
    if role == StructuralRole::Reference {
        if node.kind() == "moduleName" {
            return Ok(node
                .parent()
                .is_some_and(|parent| parent.kind() == "declUses")
                && written_name(node, source));
        }
        if node.kind() == "identifier" {
            return Ok(!declaration_name_ancestor(node, cancellation)?);
        }
        return Ok(node.kind() == "exprDot"
            && source
                .get(node.byte_range())
                .is_some_and(|bytes| bytes.split(|byte| *byte == b'.').all(identifier)));
    }
    Ok(true)
}

fn grouped_syntax(
    owner: Node<'_>,
    proof: &OwnerProof,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if owner.kind() == "declField" {
        return Ok(if proof.names > 1 {
            "pascal.field_fragment"
        } else {
            "pascal.field"
        });
    }
    if owner.kind() == "declArg" {
        return Ok(if proof.names > 1 {
            "pascal.parameter_fragment"
        } else {
            "pascal.parameter"
        });
    }
    let class_variable = if let Some(section) = owner.parent() {
        type_owner(section, cancellation)?.is_some()
            && child(section, "kClass", cancellation)?.is_some()
    } else {
        false
    };
    Ok(match (class_variable, proof.names > 1) {
        (true, true) => "pascal.class_variable_fragment",
        (true, false) => "pascal.class_variable",
        (false, true) => "pascal.variable_fragment",
        (false, false) => "pascal.variable",
    })
}

/// Projects admitted captures without changing original spelling or proving targets.
/// Signatures belong to complete native headers, never a synthesized merged group.
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
        return Err(query_failure("query-pascal-unproven-call"));
    }
    let mut depth_node = node;
    let mut range = node.byte_range();
    let syntax = if node.kind() == "root" {
        range = 0..source.len();
        if role == StructuralRole::Expression {
            "pascal.file_semantics"
        } else {
            "pascal.file"
        }
    } else if role == StructuralRole::Expression {
        if has_direct_pp(node, cancellation)? || node.kind() == "pp" {
            "pascal.preprocessing"
        } else if is_owner(node) && !valid_owner_chain(node, source, cancellation, cache)? {
            "pascal.unsupported_owner"
        } else if is_group(node) {
            if cache
                .owner(node, source, cancellation)?
                .is_some_and(|proof| proof.names > 1)
            {
                "pascal.shared_declaration"
            } else {
                "pascal.declaration_semantics"
            }
        } else if node.kind() == "declType" {
            if cache
                .owner(node, source, cancellation)?
                .is_some_and(|proof| proof.forward)
            {
                "pascal.forward_type"
            } else {
                "pascal.type_semantics"
            }
        } else if node.kind() == "declProp" {
            "pascal.accessor_semantics"
        } else if node.kind() == "declProc" {
            if child(node, "procExternal", cancellation)?.is_some() {
                "pascal.external_implementation"
            } else {
                "pascal.prototype_linkage"
            }
        } else if node.kind() == "defProc" {
            if field(node, "header")
                .and_then(|header| field(header, "name"))
                .is_some_and(|name| name.kind() == "genericDot")
            {
                "pascal.qualified_routine_semantics"
            } else {
                "pascal.routine_semantics"
            }
        } else if is_module(node) {
            "pascal.unit_semantics"
        } else if node.kind() == "exprCall" {
            "pascal.call_semantics"
        } else {
            "pascal.unsupported_syntax"
        }
    } else if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        let owner = group_owner(node).unwrap_or(node);
        let proof = cache
            .owner(owner, source, cancellation)?
            .ok_or_else(|| query_failure("query-pascal-owner"))?;
        let syntax = if is_group(owner) {
            grouped_syntax(owner, &proof, cancellation)?
        } else {
            proof.syntax
        };
        if role == StructuralRole::Definition {
            range = name_node(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-pascal-name"))?
                .byte_range();
            depth_node = if is_group(owner) && proof.names > 1 {
                node
            } else {
                owner
            };
        } else if is_group(owner) && proof.names > 1 {
            range = node.byte_range();
        } else {
            depth_node = owner;
            range = if role == StructuralRole::Signature {
                proof.header
            } else {
                proof.extent
            };
        }
        syntax
    } else {
        match role {
            StructuralRole::Reference if node.kind() == "moduleName" => "pascal.unit_reference",
            StructuralRole::Reference if node.kind() == "exprDot" => "pascal.member_reference",
            StructuralRole::Reference => "pascal.identifier",
            StructuralRole::Comment | StructuralRole::Documentation => "pascal.comment",
            StructuralRole::StringLiteral => "pascal.string",
            _ => return Err(query_failure("query-pascal-role")),
        }
    };
    let mut depth = native_depth(depth_node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "root" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-pascal-depth"))?;
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
    use super::*;
    use rootlight_cancel::CancellationReason;
    use std::collections::BTreeSet;

    fn extract(source: &str) -> (tree_sitter::Tree, Vec<QueryCandidate>) {
        let family = crate::GrammarFamily::Pascal;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let pack =
            super::super::QueryPack::compile(family, include_str!("../../queries/pascal.scm"))
                .unwrap();
        let extraction = pack
            .extract(
                family,
                &tree,
                source.as_bytes(),
                10_000,
                1024,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(extraction.limit, None);
        let mut candidates = extraction.candidates;
        candidates.sort_unstable_by_key(|candidate| {
            (
                candidate.start,
                candidate.end,
                candidate.role,
                candidate.syntax,
                candidate.native_depth,
            )
        });
        candidates.dedup_by(|left, right| left.same_fact(*right));
        for candidate in &candidates {
            assert!(source.get(candidate.start..candidate.end).is_some());
            assert!(!matches!(
                candidate.role,
                StructuralRole::Call | StructuralRole::CallName | StructuralRole::ScopedCall
            ));
        }
        (tree, candidates)
    }

    fn written_range(source: &str, owner: &str, name: &str) -> Range<usize> {
        let start = source.find(owner).unwrap() + owner.find(name).unwrap();
        start..start + name.len()
    }

    fn capture_at<'a>(
        candidates: &'a [QueryCandidate],
        role: StructuralRole,
        syntax: &str,
        range: Range<usize>,
    ) -> &'a QueryCandidate {
        let matching = candidates
            .iter()
            .filter(|candidate| {
                candidate.role == role
                    && candidate.syntax == syntax
                    && candidate.start == range.start
                    && candidate.end == range.end
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "{role:?} {syntax} {range:?}: {candidates:?}"
        );
        matching[0]
    }

    #[test]
    fn capture_composition_preserves_group_names_and_written_qualified_headers() {
        let source = "unit CaptureStore;\r\ninterface\r\ntype TBox = class\r\n\
            Left, Right: Integer;\r\n\
            function Map(const Input, Scale: Integer): Integer;\r\n\
            end;\r\nimplementation\r\n\
            function TBox.Map(const Input, Scale: Integer): Integer;\r\n\
            begin Map := Input + Scale; end;\r\nend.";
        let prototype = "function Map(const Input, Scale: Integer): Integer;";
        let qualified = "function TBox.Map(const Input, Scale: Integer): Integer;";
        let (tree, candidates) = extract(source);
        assert!(!tree.root_node().has_error());
        let expected = [
            ("pascal.unit", "unit CaptureStore;", "CaptureStore"),
            ("pascal.class", "TBox = class", "TBox"),
            ("pascal.field_fragment", "Left, Right: Integer;", "Left"),
            ("pascal.field_fragment", "Left, Right: Integer;", "Right"),
            ("pascal.function", prototype, "Map"),
            ("pascal.function", qualified, "Map"),
            ("pascal.parameter_fragment", prototype, "Input"),
            ("pascal.parameter_fragment", prototype, "Scale"),
            ("pascal.parameter_fragment", qualified, "Input"),
            ("pascal.parameter_fragment", qualified, "Scale"),
        ]
        .map(|(syntax, owner, name)| {
            let range = written_range(source, owner, name);
            (syntax, range.start, range.end)
        })
        .into_iter()
        .collect::<BTreeSet<_>>();
        let definitions = candidates
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Definition)
            .map(|candidate| (candidate.syntax, candidate.start, candidate.end))
            .collect::<BTreeSet<_>>();
        assert_eq!(definitions, expected);
        assert_eq!(definitions.len(), 10);
        for (syntax, start, end) in &definitions {
            let definition = capture_at(
                &candidates,
                StructuralRole::Definition,
                syntax,
                *start..*end,
            );
            assert!(definition.required);
            if syntax.ends_with("_fragment") {
                let declaration = capture_at(
                    &candidates,
                    StructuralRole::Declaration,
                    syntax,
                    *start..*end,
                );
                assert!(declaration.required);
                assert_eq!(declaration.native_depth, definition.native_depth);
            }
        }
        assert!(candidates.iter().all(|candidate| {
            candidate.role != StructuralRole::Signature || !candidate.syntax.ends_with("_fragment")
        }));
        for header in ["unit CaptureStore;", prototype, qualified] {
            let start = source.find(header).unwrap();
            let syntax = if header.starts_with("unit ") {
                "pascal.unit"
            } else {
                "pascal.function"
            };
            let signature = capture_at(
                &candidates,
                StructuralRole::Signature,
                syntax,
                start..start + header.len(),
            );
            assert!(signature.required);
            assert_eq!(source.get(signature.start..signature.end), Some(header));
        }
        let class_start = source.find("TBox = class").unwrap();
        let class_end = source.find("end;\r\nimplementation").unwrap() + "end;".len();
        let class = capture_at(
            &candidates,
            StructuralRole::Declaration,
            "pascal.class",
            class_start..class_end,
        );
        let class_scope = capture_at(
            &candidates,
            StructuralRole::Scope,
            "pascal.class",
            class_start..class_end,
        );
        assert_eq!(class_scope.native_depth, class.native_depth + 1);
        for name in ["Left", "Right"] {
            let field = capture_at(
                &candidates,
                StructuralRole::Declaration,
                "pascal.field_fragment",
                written_range(source, "Left, Right: Integer;", name),
            );
            assert!(field.start > class.start && field.end < class.end);
            assert!(field.native_depth > class_scope.native_depth);
        }
        let implementation_start = source.find(qualified).unwrap();
        let implementation_end = source.find("end;\r\nend.").unwrap() + "end;".len();
        let implementation = capture_at(
            &candidates,
            StructuralRole::Declaration,
            "pascal.function",
            implementation_start..implementation_end,
        );
        assert!(implementation.start > class.end);
        assert!(candidates.iter().any(|candidate| {
            candidate.role == StructuralRole::Expression
                && candidate.syntax == "pascal.qualified_routine_semantics"
                && candidate.start == implementation.start
                && candidate.end == implementation.end
        }));
    }

    #[test]
    fn capture_composition_cannot_borrow_headers_from_a_clean_following_owner() {
        let source = "procedure Damaged(Value: );\nbegin end;\n\
            procedure Stable(Argument: Integer);\nbegin end;";
        let stable_header = "procedure Stable(Argument: Integer);";
        let stable_start = source.find(stable_header).unwrap();
        let (tree, candidates) = extract(source);
        assert!(tree.root_node().has_error());
        let definitions = candidates
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Definition)
            .map(|candidate| (candidate.syntax, candidate.start, candidate.end))
            .collect::<BTreeSet<_>>();
        let expected = [
            ("pascal.procedure", "Stable"),
            ("pascal.parameter", "Argument"),
        ]
        .map(|(syntax, name)| {
            let range = written_range(source, stable_header, name);
            (syntax, range.start, range.end)
        })
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(definitions, expected);
        capture_at(
            &candidates,
            StructuralRole::Declaration,
            "pascal.procedure",
            stable_start..source.len(),
        );
        capture_at(
            &candidates,
            StructuralRole::Signature,
            "pascal.procedure",
            stable_start..stable_start + stable_header.len(),
        );
        for candidate in candidates.iter().filter(|candidate| {
            candidate.syntax != "pascal.file"
                && matches!(
                    candidate.role,
                    StructuralRole::Declaration
                        | StructuralRole::Definition
                        | StructuralRole::Signature
                        | StructuralRole::Scope
                        | StructuralRole::Reference
                )
        }) {
            assert!(candidate.start >= stable_start, "{candidate:?}");
        }
        assert!(candidates.iter().any(|candidate| {
            candidate.role == StructuralRole::Expression
                && candidate.syntax == "pascal.unsupported_owner"
                && candidate.start < stable_start
                && candidate.end <= stable_start
        }));
    }

    #[test]
    fn owner_cache_reuses_proofs_and_fails_closed_at_its_existing_bound() {
        let source = b"procedure First; begin end; procedure Second; begin end;";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Pascal))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let owners = tree
            .root_node()
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "defProc")
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 2);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1);
        for _ in 0..8 {
            assert!(
                cache
                    .owner(owners[0], source, &cancellation)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(cache.owners.len(), 1);
        }
        assert!(
            cache
                .owner(owners[1], source, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        assert_eq!(cache.owners.len(), 1);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(cache.owner(owners[0], source, &cancellation).is_err());
    }
}
