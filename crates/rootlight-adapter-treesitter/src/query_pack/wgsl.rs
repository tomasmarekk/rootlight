//! WGSL captures prove direct physical owners and raw Unicode 16 names.
//! Type, member and call spellings remain unresolved source references;
//! hidden comments are available only through the immutable original source.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    name: Option<Range<usize>>,
    syntax: &'static str,
}

#[derive(Default)]
struct CacheEntry {
    owner: Option<Option<OwnerProof>>,
    chain: Option<bool>,
    excluded: Option<bool>,
}

/// Shares the caller's existing capture/body ceiling across all native proofs.
pub(super) struct CaptureCache {
    entries: HashMap<usize, CacheEntry>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    /// Creates a bounded proof cache without source-sized preallocation.
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    /// Requires the caller to report exhaustion as its existing capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn admit_entry(&mut self, id: usize) -> Result<bool, AdapterError> {
        if self.entries.contains_key(&id) {
            return Ok(true);
        }
        if self.entries.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(false);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.entries.insert(id, CacheEntry::default());
        Ok(true)
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if !self.admit_entry(node.id())? {
            return Ok(None);
        }
        if let Some(proof) = self
            .entries
            .get(&node.id())
            .and_then(|entry| entry.owner.as_ref())
        {
            return Ok(proof.clone());
        }
        let proof = owner_proof(node, source, cancellation)?;
        if let Some(entry) = self.entries.get_mut(&node.id()) {
            entry.owner = Some(proof.clone());
        }
        Ok(proof)
    }

    fn chain(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = Some(node);
        let mut pending = Vec::new();
        let mut valid = true;
        while let Some(parent) = current {
            cancellation.check()?;
            if !self.admit_entry(parent.id())? {
                return Ok(false);
            }
            if let Some(cached) = self.entries.get(&parent.id()).and_then(|entry| entry.chain) {
                valid = cached;
                break;
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(parent.id());
            if parent.is_missing()
                || parent.kind() == "ERROR"
                || (owner_kind(parent) && self.owner(parent, source, cancellation)?.is_none())
            {
                valid = false;
                break;
            }
            current = parent.parent();
        }
        for id in pending {
            cancellation.check()?;
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.chain = Some(valid);
            }
        }
        Ok(valid)
    }

    fn excluded(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = Some(node);
        let mut pending = Vec::new();
        let mut excluded = false;
        while let Some(parent) = current {
            cancellation.check()?;
            if !self.admit_entry(parent.id())? {
                return Ok(true);
            }
            if let Some(cached) = self
                .entries
                .get(&parent.id())
                .and_then(|entry| entry.excluded)
            {
                excluded = cached;
                break;
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(parent);
            current = parent.parent();
        }
        for child in pending.into_iter().rev() {
            cancellation.check()?;
            if let Some(parent) = child.parent() {
                excluded |= excluded_child(parent, child);
            }
            if let Some(entry) = self.entries.get_mut(&child.id()) {
                entry.excluded = Some(excluded);
            }
        }
        Ok(excluded)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn text<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn direct_named<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut selected = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child.kind() == kind {
            if selected.is_some() || child.parent() != Some(node) {
                return Ok(None);
            }
            selected = Some(child);
        }
    }
    Ok(selected)
}

fn named_shape(
    node: Node<'_>,
    allowed: &[&str],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && !allowed.contains(&child.kind()) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn token(
    node: Node<'_>,
    source: &[u8],
    spelling: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra()
            && !child.is_named()
            && clean(child, source)
            && source.get(child.byte_range()) == Some(spelling)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn written_name(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !clean(node, source) || !matches!(node.kind(), "ident" | "member_ident" | "swizzle_name") {
        return Ok(false);
    }
    if node.kind() != "swizzle_name" {
        let Some(leaf) = direct_named(node, "ident_pattern_token", cancellation)? else {
            return Ok(false);
        };
        if !clean(leaf, source) || leaf.byte_range() != node.byte_range() {
            return Ok(false);
        }
    }
    Ok(text(node, source).is_some_and(|raw| {
        structural_captured_name_for_language("wgsl", raw, raw.len()).as_deref() == Some(raw)
    }))
}

fn braced(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !clean(node, source) {
        return Ok(false);
    }
    let mut opening = None;
    let mut last = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !child.is_named() && source.get(child.byte_range()) == Some(b"{") {
            if opening.is_some() || !clean(child, source) {
                return Ok(false);
            }
            opening = Some(child);
        }
        last = Some(child);
    }
    Ok(opening.is_some()
        && last.is_some_and(|last| {
            clean(last, source)
                && !last.is_named()
                && source.get(last.byte_range()) == Some(b"}")
                && last.end_byte() == node.end_byte()
        }))
}

fn following_semicolon<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut next = node.next_sibling();
    while let Some(child) = next {
        cancellation.check()?;
        if child.is_extra() {
            next = child.next_sibling();
            continue;
        }
        return Ok((child.parent() == node.parent()
            && !child.is_named()
            && clean(child, source)
            && source.get(child.byte_range()) == Some(b";"))
        .then_some(child));
    }
    Ok(None)
}

fn scalar_extent(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    let Some(parent) = node.parent() else {
        return Ok(None);
    };
    let terminal = match parent.kind() {
        "translation_unit" => following_semicolon(node, source, cancellation)?,
        "statement" if clean(parent, source) => following_semicolon(node, source, cancellation)?,
        "for_init"
            if clean(parent, source)
                && parent
                    .parent()
                    .is_some_and(|header| header.kind() == "for_header") =>
        {
            following_semicolon(parent, source, cancellation)?
        }
        _ => None,
    };
    Ok(terminal.map(|terminal| node.start_byte()..terminal.end_byte()))
}

fn optionally_typed_name<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if !clean(node, source) || !named_shape(node, &["ident", "type_specifier"], cancellation)? {
        return Ok(None);
    }
    let Some(name) = direct_named(node, "ident", cancellation)? else {
        return Ok(None);
    };
    Ok(written_name(name, source, cancellation)?.then_some(name))
}

fn variable_name<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if !clean(node, source)
        || !token(node, source, b"var", cancellation)?
        || !named_shape(
            node,
            &["template_list", "optionally_typed_ident"],
            cancellation,
        )?
    {
        return Ok(None);
    }
    let Some(typed) = direct_named(node, "optionally_typed_ident", cancellation)? else {
        return Ok(None);
    };
    optionally_typed_name(typed, source, cancellation)
}

fn owner_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_decl"
            | "struct_decl"
            | "struct_member"
            | "param"
            | "type_alias_decl"
            | "global_variable_decl"
            | "global_value_decl"
            | "variable_or_value_statement"
            | "compound_statement"
            | "struct_body_decl"
            | "loop_statement"
            | "for_statement"
            | "continuing_compound_statement"
    )
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if !clean(node, source) {
        return Ok(None);
    }
    let mut proof = OwnerProof {
        extent: node.byte_range(),
        header: node.byte_range(),
        name: None,
        syntax: "wgsl.body",
    };
    match node.kind() {
        "function_decl" => {
            if node
                .parent()
                .is_none_or(|parent| parent.kind() != "translation_unit")
                || !named_shape(
                    node,
                    &["attribute", "function_header", "compound_statement"],
                    cancellation,
                )?
            {
                return Ok(None);
            }
            let (Some(header), Some(body)) = (
                direct_named(node, "function_header", cancellation)?,
                direct_named(node, "compound_statement", cancellation)?,
            ) else {
                return Ok(None);
            };
            let Some(name) = direct_named(header, "ident", cancellation)? else {
                return Ok(None);
            };
            if !written_name(name, source, cancellation)?
                || !token(header, source, b"fn", cancellation)?
                || !token(header, source, b"(", cancellation)?
                || !token(header, source, b")", cancellation)?
                || !braced(body, source, cancellation)?
                || header.end_byte() > body.start_byte()
                || body.end_byte() != node.end_byte()
            {
                return Ok(None);
            }
            proof.header = node.start_byte()..header.end_byte();
            proof.name = Some(name.byte_range());
            proof.syntax = "wgsl.function";
        }
        "struct_decl" => {
            let (Some(name), Some(body)) = (
                direct_named(node, "ident", cancellation)?,
                direct_named(node, "struct_body_decl", cancellation)?,
            ) else {
                return Ok(None);
            };
            if node
                .parent()
                .is_none_or(|parent| parent.kind() != "translation_unit")
                || !token(node, source, b"struct", cancellation)?
                || !written_name(name, source, cancellation)?
                || !braced(body, source, cancellation)?
                || name.end_byte() > body.start_byte()
                || body.end_byte() != node.end_byte()
            {
                return Ok(None);
            }
            proof.header = node.start_byte()..name.end_byte();
            proof.name = Some(name.byte_range());
            proof.syntax = "wgsl.struct";
        }
        "struct_member" | "param" => {
            let (name_kind, parent_kind, syntax) = if node.kind() == "param" {
                ("ident", "param_list", "wgsl.parameter")
            } else {
                ("member_ident", "struct_body_decl", "wgsl.field")
            };
            let (Some(name), Some(ty)) = (
                direct_named(node, name_kind, cancellation)?,
                direct_named(node, "type_specifier", cancellation)?,
            ) else {
                return Ok(None);
            };
            if node
                .parent()
                .is_none_or(|parent| parent.kind() != parent_kind)
                || !written_name(name, source, cancellation)?
                || !token(node, source, b":", cancellation)?
                || name.end_byte() > ty.start_byte()
                || ty.end_byte() != node.end_byte()
            {
                return Ok(None);
            }
            proof.name = Some(name.byte_range());
            proof.syntax = syntax;
        }
        "type_alias_decl" => {
            let (Some(name), Some(ty)) = (
                direct_named(node, "ident", cancellation)?,
                direct_named(node, "type_specifier", cancellation)?,
            ) else {
                return Ok(None);
            };
            let Some(extent) = scalar_extent(node, source, cancellation)? else {
                return Ok(None);
            };
            if node
                .parent()
                .is_none_or(|parent| parent.kind() != "translation_unit")
                || !token(node, source, b"alias", cancellation)?
                || !token(node, source, b"=", cancellation)?
                || !written_name(name, source, cancellation)?
                || name.end_byte() > ty.start_byte()
            {
                return Ok(None);
            }
            proof.extent = extent;
            proof.name = Some(name.byte_range());
            proof.syntax = "wgsl.type_alias";
        }
        "global_variable_decl" | "global_value_decl" | "variable_or_value_statement" => {
            let Some(extent) = scalar_extent(node, source, cancellation)? else {
                return Ok(None);
            };
            if node.kind() != "variable_or_value_statement"
                && node
                    .parent()
                    .is_none_or(|parent| parent.kind() != "translation_unit")
            {
                return Ok(None);
            }
            let variable = direct_named(node, "variable_decl", cancellation)?;
            let typed = direct_named(node, "optionally_typed_ident", cancellation)?;
            let (name, syntax) = match (variable, typed) {
                (Some(variable), None) if node.kind() != "global_value_decl" => (
                    variable_name(variable, source, cancellation)?,
                    "wgsl.variable",
                ),
                (None, Some(typed)) if node.kind() != "global_variable_decl" => {
                    let syntax = if token(node, source, b"const", cancellation)? {
                        "wgsl.constant"
                    } else if node.kind() == "global_value_decl"
                        && token(node, source, b"override", cancellation)?
                    {
                        "wgsl.override"
                    } else if node.kind() == "variable_or_value_statement"
                        && token(node, source, b"let", cancellation)?
                    {
                        "wgsl.let"
                    } else {
                        return Ok(None);
                    };
                    (optionally_typed_name(typed, source, cancellation)?, syntax)
                }
                _ => return Ok(None),
            };
            let Some(name) = name else {
                return Ok(None);
            };
            proof.extent = extent;
            proof.name = Some(name.byte_range());
            proof.syntax = syntax;
        }
        "compound_statement"
        | "struct_body_decl"
        | "loop_statement"
        | "continuing_compound_statement" => {
            if !braced(node, source, cancellation)? {
                return Ok(None);
            }
        }
        "for_statement" => {
            let (Some(header), Some(body)) = (
                direct_named(node, "for_header", cancellation)?,
                direct_named(node, "compound_statement", cancellation)?,
            ) else {
                return Ok(None);
            };
            if !clean(header, source)
                || !token(node, source, b"for", cancellation)?
                || !token(node, source, b"(", cancellation)?
                || !token(node, source, b")", cancellation)?
                || !braced(body, source, cancellation)?
                || header.end_byte() > body.start_byte()
                || body.end_byte() != node.end_byte()
            {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    }
    Ok(Some(proof))
}

fn excluded_child(parent: Node<'_>, child: Node<'_>) -> bool {
    matches!(parent.kind(), "attribute" | "global_directive")
        || (parent.kind() == "variable_decl" && child.kind() == "template_list")
        || (child.kind() == "ident"
            && matches!(
                parent.kind(),
                "function_header"
                    | "struct_decl"
                    | "type_alias_decl"
                    | "param"
                    | "optionally_typed_ident"
            ))
        || (child.kind() == "member_ident" && parent.kind() == "struct_member")
}

/// Retains identities only with clean complete owners and direct written names.
///
/// Allocation/cancellation failures propagate. Exhaustion must become the caller's
/// capture-limit evidence, never silently accepted complete structural coverage.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "translation_unit" {
        return Ok(matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Scope
                | StructuralRole::Expression
        ));
    }
    if role == StructuralRole::Expression {
        return Ok(!node.byte_range().is_empty() && text(node, source).is_some());
    }
    if !matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Reference
            | StructuralRole::Scope
    ) || !cache.chain(node, source, cancellation)?
    {
        return Ok(false);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        return Ok(cache
            .owner(node, source, cancellation)?
            .is_some_and(|proof| proof.name.is_some()));
    }
    if role == StructuralRole::Reference {
        return Ok(
            written_name(node, source, cancellation)? && !cache.excluded(node, cancellation)?
        );
    }
    Ok(cache.owner(node, source, cancellation)?.is_some())
}

fn reference_syntax(node: Node<'_>) -> &'static str {
    if matches!(node.kind(), "member_ident" | "swizzle_name") {
        return "wgsl.member";
    }
    if let Some(template) = node.parent()
        && template.kind() == "template_elaborated_ident"
        && let Some(parent) = template.parent()
    {
        return match parent.kind() {
            "call_phrase" => "wgsl.unproven_call",
            "type_specifier" | "function_header" => "wgsl.type",
            _ => "wgsl.identifier",
        };
    }
    "wgsl.identifier"
}

fn expression_syntax(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<&'static str, AdapterError> {
    if node.kind() == "attribute" {
        return Ok("wgsl.annotation_context");
    }
    if node.kind() == "global_directive" {
        return Ok("wgsl.directive_context");
    }
    if node.kind() == "variable_decl"
        && direct_named(node, "template_list", cancellation)?.is_some()
    {
        return Ok("wgsl.annotation_context");
    }
    Ok(if cache.chain(node, source, cancellation)? {
        "wgsl.body_semantics"
    } else {
        "wgsl.unsupported_owner"
    })
}

/// Projects exact original name, owner and header ranges without binding targets.
///
/// Hidden comment tokens have no query occurrence. The file's expression capture
/// marks that capability boundary while its original source remains available.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = if node.kind() == "translation_unit" {
        range = 0..source.len();
        match role {
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => "wgsl.file",
            StructuralRole::Expression => "wgsl.source_availability",
            _ => return Err(query_failure("query-wgsl-unproven-root-role")),
        }
    } else if role == StructuralRole::Expression {
        expression_syntax(node, source, cancellation, cache)?
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        let proof = cache
            .owner(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-wgsl-owner"))?;
        range = match role {
            StructuralRole::Definition => {
                proof.name.ok_or_else(|| query_failure("query-wgsl-name"))?
            }
            StructuralRole::Signature => proof.header,
            _ => proof.extent,
        };
        match role {
            StructuralRole::Definition => "wgsl.name",
            StructuralRole::Signature => "wgsl.header",
            _ => proof.syntax,
        }
    } else if role == StructuralRole::Reference {
        reference_syntax(node)
    } else if role == StructuralRole::Scope {
        "wgsl.body"
    } else {
        return Err(query_failure("query-wgsl-unproven-role"));
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "translation_unit" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-wgsl-depth"))?;
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
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator, Tree};

    use super::*;

    const QUERY: &str = include_str!("../../queries/wgsl.scm");

    fn tree(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_wgsl::LANGUAGE_WGSL.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn extract(source: &str) -> (Tree, Vec<QueryCandidate>) {
        let tree = tree(source);
        let query = Query::new(&tree_sitter_wgsl::LANGUAGE_WGSL.into(), QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(8192);
        let mut captures = Vec::new();
        while let Some(found) = matches.next() {
            for capture in found.captures {
                let index = usize::try_from(capture.index).unwrap();
                let role = StructuralRole::from_capture_name(query.capture_names()[index]).unwrap();
                if retain_capture(
                    capture.node,
                    role,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
                {
                    captures.push(
                        candidate(
                            capture.node,
                            role,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache,
                        )
                        .unwrap(),
                    );
                }
                assert!(!cache.exhausted());
            }
        }
        drop(matches);
        (tree, captures)
    }

    fn names<'source>(source: &'source str, captures: &[QueryCandidate]) -> Vec<&'source str> {
        let mut selected = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Definition)
            .collect::<Vec<_>>();
        selected.sort_by_key(|capture| (capture.start, capture.end));
        selected
            .into_iter()
            .map(|capture| &source[capture.start..capture.end])
            .collect()
    }

    fn has(captures: &[QueryCandidate], syntax: &str, role: StructuralRole) -> bool {
        captures
            .iter()
            .any(|capture| capture.syntax == syntax && capture.role == role)
    }

    #[test]
    fn original_written_owners_keep_exact_names_headers_and_extents() {
        let source = "struct Uniforms { scale: f32, }\nalias Δ = vec2<f32>;\nvar<private> counter: i32;\n@compute @workgroup_size(1)\nfn Main() { var local: i32 = 1; counter = local; }\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            names(source, &captures),
            ["Uniforms", "scale", "Δ", "counter", "Main", "local"]
        );
        for (syntax, raw) in [
            ("wgsl.struct", "struct Uniforms { scale: f32, }"),
            ("wgsl.type_alias", "alias Δ = vec2<f32>;"),
            ("wgsl.variable", "var<private> counter: i32;"),
            (
                "wgsl.function",
                "@compute @workgroup_size(1)\nfn Main() { var local: i32 = 1; counter = local; }",
            ),
            ("wgsl.variable", "var local: i32 = 1;"),
        ] {
            assert!(captures.iter().any(|capture| {
                capture.syntax == syntax
                    && capture.role == StructuralRole::Declaration
                    && &source[capture.start..capture.end] == raw
            }));
        }
        assert!(captures.iter().any(|capture| {
            capture.syntax == "wgsl.header"
                && capture.role == StructuralRole::Signature
                && &source[capture.start..capture.end] == "@compute @workgroup_size(1)\nfn Main()"
        }));
        for definition in captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Definition)
        {
            assert!(captures.iter().any(|owner| {
                owner.role == StructuralRole::Declaration
                    && owner.native_depth == definition.native_depth
                    && owner.start <= definition.start
                    && definition.end <= owner.end
            }));
            assert!(definition.required);
        }
    }

    #[test]
    fn constants_overrides_aliases_formals_and_local_values_keep_written_kinds() {
        let source = "const Count: u32 = 4u;\n@id(3) override Capacity = Count;\nalias Color = vec4<f32>;\nvar<private> Input: Color;\nstruct Vertex { @location(0) position: Color, weight: f32, }\nfn Shade(value: Color) -> Color { let result = value; const local = Count; var mutable: Color = result; return Color(mutable); }\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            names(source, &captures),
            [
                "Count", "Capacity", "Color", "Input", "Vertex", "position", "weight", "Shade",
                "value", "result", "local", "mutable"
            ]
        );
        for syntax in [
            "wgsl.constant",
            "wgsl.override",
            "wgsl.type_alias",
            "wgsl.parameter",
            "wgsl.field",
            "wgsl.let",
            "wgsl.variable",
        ] {
            assert!(has(&captures, syntax, StructuralRole::Declaration));
        }
        for definition in captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Definition)
        {
            assert!(!captures.iter().any(|capture| {
                capture.role == StructuralRole::Reference
                    && capture.start == definition.start
                    && capture.end == definition.end
            }));
        }
    }

    #[test]
    fn type_member_and_call_spellings_are_only_unresolved_source_references() {
        let source = "enable f16;\nstruct Written { field: f32, }\n@group(Group) @binding(Slot) var<storage, read> data: Written;\nfn Read(value: Written) -> f32 { return helper(value.field); }\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        for (syntax, raw) in [
            ("wgsl.type", "Written"),
            ("wgsl.member", "field"),
            ("wgsl.unproven_call", "helper"),
        ] {
            assert!(captures.iter().any(|capture| {
                capture.role == StructuralRole::Reference
                    && capture.syntax == syntax
                    && &source[capture.start..capture.end] == raw
            }));
        }
        for excluded in ["Group", "Slot", "storage", "read", "f16"] {
            assert!(!captures.iter().any(|capture| {
                capture.role == StructuralRole::Reference
                    && &source[capture.start..capture.end] == excluded
            }));
        }
        assert!(has(
            &captures,
            "wgsl.annotation_context",
            StructuralRole::Expression
        ));
        assert!(has(
            &captures,
            "wgsl.directive_context",
            StructuralRole::Expression
        ));
        assert!(captures.iter().all(|capture| !matches!(
            capture.role,
            StructuralRole::Call
                | StructuralRole::CallName
                | StructuralRole::ScopedCall
                | StructuralRole::Import
        )));
    }

    #[test]
    fn nested_blocks_and_for_initializers_keep_distinct_physical_owners() {
        let source = "fn First(value: i32) { var same: i32 = value; { let same = value; } for (var i: i32 = 0; i < 1; i++) { let inner = i; } }\nfn Second(value: i32) { var same: i32 = value; }\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            names(source, &captures),
            [
                "First", "value", "same", "same", "i", "inner", "Second", "value", "same"
            ]
        );
        let repeated = captures
            .iter()
            .filter(|capture| {
                capture.role == StructuralRole::Definition
                    && &source[capture.start..capture.end] == "same"
            })
            .collect::<Vec<_>>();
        assert_eq!(repeated.len(), 3);
        assert!(
            repeated
                .windows(2)
                .all(|pair| pair[0].start != pair[1].start)
        );
        assert!(captures.iter().any(|capture| {
            capture.role == StructuralRole::Scope
                && &source[capture.start..capture.end] == "{ let same = value; }"
        }));
        assert!(captures.iter().any(|capture| {
            capture.role == StructuralRole::Scope
                && &source[capture.start..capture.end]
                    == "for (var i: i32 = 0; i < 1; i++) { let inner = i; }"
        }));
    }

    #[test]
    fn original_recovery_cannot_borrow_clean_names_from_a_broken_owner() {
        let source = "alias Broken = array<vec2<f32>;\nalias After = i32;\n";
        let (tree, captures) = extract(source);
        assert!(tree.root_node().has_error());
        assert_eq!(names(source, &captures), ["After"]);
        assert!(has(
            &captures,
            "wgsl.unsupported_owner",
            StructuralRole::Expression
        ));
        assert!(!captures.iter().any(|capture| {
            capture.role == StructuralRole::Reference
                && &source[capture.start..capture.end] == "Broken"
        }));
    }

    #[test]
    fn hidden_original_comments_are_source_available_not_fabricated_captures() {
        let source = "/* **/ alias Next = i32;\n/* outer /* inner */ close */ fn Later() {}\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(names(source, &captures), ["Next", "Later"]);
        assert!(captures.iter().all(|capture| !matches!(
            capture.role,
            StructuralRole::Comment | StructuralRole::Documentation | StructuralRole::StringLiteral
        )));
        assert!(captures.iter().any(|capture| {
            capture.syntax == "wgsl.source_availability"
                && capture.role == StructuralRole::Expression
                && capture.start == 0
                && capture.end == source.len()
        }));
        let query = Query::new(&tree_sitter_wgsl::LANGUAGE_WGSL.into(), QUERY).unwrap();
        let mut names = query.capture_names().to_vec();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "declaration",
                "definition",
                "expression",
                "module",
                "reference",
                "root",
                "scope",
                "signature"
            ]
        );
    }

    #[test]
    fn unicode_names_keep_raw_spelling_and_exact_utf8_ranges() {
        let source = "struct Δ { e\u{301}: i32, }\nfn é(_value: Δ) { let e\u{301} = _value; }\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            names(source, &captures),
            ["Δ", "e\u{301}", "é", "_value", "e\u{301}"]
        );
        for capture in &captures {
            assert!(source.get(capture.start..capture.end).is_some());
        }
        assert!(!names(source, &captures).contains(&"e"));
    }

    #[test]
    fn proofs_are_reused_bounded_and_cancellable_without_accepting_unsupported_roles() {
        let source = "alias Written = i32;\n";
        let tree = tree(source);
        let owner = tree.root_node().named_child(0).unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(128);
        assert!(
            retain_capture(
                owner,
                StructuralRole::Declaration,
                source.as_bytes(),
                &cancellation,
                &mut cache,
            )
            .unwrap()
        );
        let entries = cache.entries.len();
        assert!(
            retain_capture(
                owner,
                StructuralRole::Definition,
                source.as_bytes(),
                &cancellation,
                &mut cache,
            )
            .unwrap()
        );
        assert_eq!(cache.entries.len(), entries);
        let mut bounded = CaptureCache::new(0);
        assert!(
            !retain_capture(
                owner,
                StructuralRole::Declaration,
                source.as_bytes(),
                &cancellation,
                &mut bounded,
            )
            .unwrap()
        );
        assert!(bounded.exhausted());
        assert!(bounded.entries.is_empty());
        for role in [
            StructuralRole::Call,
            StructuralRole::Import,
            StructuralRole::Comment,
            StructuralRole::Documentation,
            StructuralRole::StringLiteral,
        ] {
            assert!(
                !retain_capture(
                    tree.root_node(),
                    role,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
            );
            assert!(
                candidate(
                    tree.root_node(),
                    role,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .is_err()
            );
        }
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            retain_capture(
                owner,
                StructuralRole::Definition,
                source.as_bytes(),
                &cancellation,
                &mut cache,
            )
            .is_err()
        );
    }
}
