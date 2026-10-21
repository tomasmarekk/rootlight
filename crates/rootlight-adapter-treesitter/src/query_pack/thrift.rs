//! Thrift declarations require complete direct native owners and written names.
//! Independent mandatory signatures retain unsupported syntax and semantic gaps.

use std::ops::Range;

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind && result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

fn text(range: Range<usize>, source: &[u8]) -> Option<&str> {
    std::str::from_utf8(source.get(range)?).ok()
}

fn declaration(node: Node<'_>) -> Option<&'static str> {
    Some(match node.kind() {
        "typedef_definition" => "thrift.typedef",
        "enum_definition" => "thrift.enum",
        "struct_definition" => "thrift.struct",
        "union_definition" => "thrift.union",
        "exception_definition" => "thrift.exception",
        "service_definition" => "thrift.service",
        "function_definition" => "thrift.function",
        "const_definition" => "thrift.constant",
        "namespace_declaration" => "thrift.namespace",
        "field" => "thrift.field",
        "parameter"
            if node
                .parent()
                .and_then(|owner| owner.parent())
                .is_some_and(|owner| owner.kind() == "throws") =>
        {
            "thrift.throws_parameter"
        }
        "parameter" => "thrift.parameter",
        "identifier"
            if node.parent().is_some_and(|owner| {
                owner.kind() == "enum_definition" && owner.child_by_field_name("type") != Some(node)
            }) =>
        {
            "thrift.enum_value"
        }
        _ => return None,
    })
}

fn name_range(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    if node.kind() == "namespace_declaration" {
        let Some(scope) = direct(node, "namespace_scope", cancellation)? else {
            return Ok(None);
        };
        let mut value: Option<Range<usize>> = None;
        let mut quoted = false;
        let mut cursor = node.walk();
        // The grammar aliases each dotted namespace component as a sibling;
        // the written identity must retain the selector and every component.
        for child in node.named_children(&mut cursor) {
            cancellation.check()?;
            match child.kind() {
                "namespace" if !quoted => match &mut value {
                    Some(range) => range.end = child.end_byte(),
                    None => value = Some(child.byte_range()),
                },
                "string" if value.is_none() => {
                    value = Some(child.byte_range());
                    quoted = true;
                }
                "namespace" | "string" => return Ok(None),
                _ => {}
            }
        }
        return Ok(value
            .filter(|value| scope.end_byte() < value.start)
            .map(|value| scope.start_byte()..value.end));
    }
    let name = match node.kind() {
        "identifier" if declaration(node).is_some() => Some(node),
        "struct_definition" | "union_definition" | "enum_definition" | "service_definition" => {
            node.child_by_field_name("type")
        }
        "typedef_definition" => direct(node, "typedef_identifier", cancellation)?,
        "exception_definition"
        | "function_definition"
        | "const_definition"
        | "field"
        | "parameter" => direct(node, "identifier", cancellation)?,
        _ => None,
    };
    // The upstream grammar accepts dotted declaration headers. A first leaf
    // cannot establish the identity of an incomplete or qualified owner name.
    let Some(name) = name.filter(|name| !name.is_missing() && !name.has_error()) else {
        return Ok(None);
    };
    let mut next = name.next_sibling();
    while let Some(comment) = next.filter(|node| node.kind() == "comment") {
        cancellation.check()?;
        next = comment.next_sibling();
    }
    Ok(next
        .is_none_or(|next| next.kind() != ".")
        .then(|| name.byte_range()))
}

fn complete(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if declaration(node).is_none() || node.is_missing() || node.has_error() {
        return Ok(false);
    }
    let parent = node.parent();
    let parent_kind = parent.map(|parent| parent.kind());
    let admitted = match node.kind() {
        "typedef_definition"
        | "enum_definition"
        | "struct_definition"
        | "union_definition"
        | "exception_definition"
        | "service_definition"
        | "const_definition"
        | "namespace_declaration" => parent_kind == Some("document"),
        "function_definition" => parent_kind == Some("service_definition"),
        "field" => matches!(
            parent_kind,
            Some("struct_definition" | "union_definition" | "exception_definition")
        ),
        "parameter" => {
            parent_kind == Some("parameters")
                && parent
                    .and_then(|owner| owner.parent())
                    .is_some_and(|owner| {
                        owner.kind() == "function_definition"
                            || (owner.kind() == "throws"
                                && owner
                                    .parent()
                                    .is_some_and(|method| method.kind() == "function_definition"))
                    })
        }
        "identifier" => parent_kind == Some("enum_definition"),
        _ => false,
    };
    let Some(range) = name_range(node, cancellation)? else {
        return Ok(false);
    };
    let Some(name) = text(range, source) else {
        return Ok(false);
    };
    let valid = if node.kind() == "namespace_declaration" {
        !name.is_empty() && !name.contains('\0')
    } else {
        name.as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    Ok(admitted && valid)
}

fn visible(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.is_missing()
            || matches!(
                owner.kind(),
                "ERROR" | "senum_definition" | "interaction_definition" | "xsd_attrs"
            )
            || (declaration(owner).is_some() && !complete(owner, source, cancellation)?)
        {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

fn type_range(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    if matches!(node.kind(), "type" | "definition_type") {
        let Some(first) = node.child_by_field_name("type") else {
            return Ok(None);
        };
        let mut end = first.end_byte();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            cancellation.check()?;
            if child.kind() == "identifier" {
                end = child.end_byte();
            }
        }
        return Ok(Some(first.start_byte()..end));
    }
    if node.kind() != "service_definition" {
        return Ok(None);
    }
    let mut started = false;
    let mut range: Option<Range<usize>> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == "{" {
            break;
        }
        if child.kind() == "extends" {
            started = true;
        }
        if started && child.kind() == "identifier" {
            match &mut range {
                Some(range) => range.end = child.end_byte(),
                None => range = Some(child.byte_range()),
            }
        }
    }
    Ok(range)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if matches!(
        role,
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
    ) {
        return Ok(node.kind() == "document" && node.parent().is_none());
    }
    if role == StructuralRole::Signature {
        if node.byte_range().is_empty() {
            return Ok(false);
        }
        if declaration(node).is_some()
            || matches!(
                node.kind(),
                "ERROR"
                    | "senum_definition"
                    | "interaction_definition"
                    | "package_declaration"
                    | "annotation_definition"
                    | "fb_annotation_definition"
                    | "xsd_attrs"
                    | "include_statement"
                    | "literal"
                    | "number"
            )
        {
            return Ok(true);
        }
        if node.kind() == "string" {
            return Ok(source
                .get(node.byte_range())
                .is_some_and(|raw| raw.iter().any(|byte| matches!(byte, b'\\' | b'\n' | b'\r'))));
        }
        if node.kind() == "container_type" {
            return Ok(node
                .named_child(0)
                .is_some_and(|child| matches!(child.kind(), "sink" | "stream")));
        }
        return Ok(type_range(node, cancellation)?
            .and_then(|range| text(range, source))
            .and_then(|name| structural_captured_name_for_language("thrift", name, name.len()))
            .is_some_and(|name| name == "uuid"));
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) {
        return Ok(!node.is_missing() && !node.has_error());
    }
    if !visible(node, source, cancellation)? {
        return Ok(false);
    }
    match role {
        StructuralRole::Declaration | StructuralRole::Definition => {
            complete(node, source, cancellation)
        }
        StructuralRole::Reference => Ok(type_range(node, cancellation)?
            .and_then(|range| text(range, source))
            .is_some_and(|name| {
                structural_captured_name_for_language("thrift", name, name.len()).is_some()
            })),
        StructuralRole::Import => Ok(node.kind() == "include_statement"
            && !node.has_error()
            && direct(node, "string", cancellation)?.is_some()),
        _ => Ok(false),
    }
}

fn performs(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == "performs" {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    let mut range = node.byte_range();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => {
            range = 0..source.len();
            "thrift.file"
        }
        StructuralRole::Declaration => {
            declaration(node).ok_or_else(|| query_failure("query-thrift-owner"))?
        }
        StructuralRole::Definition => {
            range = name_range(node, cancellation)?
                .ok_or_else(|| query_failure("query-thrift-name"))?;
            if node.kind() == "namespace_declaration" {
                "thrift.namespace_name"
            } else {
                "thrift.name"
            }
        }
        StructuralRole::Reference => {
            range = type_range(node, cancellation)?
                .ok_or_else(|| query_failure("query-thrift-type"))?;
            let uuid = text(range.clone(), source)
                .and_then(|name| structural_captured_name_for_language("thrift", name, name.len()))
                .is_some_and(|name| name == "uuid");
            if uuid {
                "thrift.uuid_type"
            } else if node.kind() == "service_definition" {
                "thrift.service_base"
            } else {
                "thrift.type"
            }
        }
        StructuralRole::Import => {
            range = direct(node, "string", cancellation)?
                .ok_or_else(|| query_failure("query-thrift-include"))?
                .byte_range();
            "thrift.written_include"
        }
        StructuralRole::Comment => "thrift.comment",
        StructuralRole::StringLiteral => "thrift.string",
        StructuralRole::Signature
            if matches!(
                node.kind(),
                "ERROR" | "senum_definition" | "interaction_definition" | "xsd_attrs"
            ) =>
        {
            "thrift.unsupported_construct"
        }
        StructuralRole::Signature if node.kind() == "string" => {
            "thrift.string_validation_unavailable"
        }
        StructuralRole::Signature if matches!(node.kind(), "type" | "definition_type") => {
            "thrift.uuid_unavailable"
        }
        StructuralRole::Signature
            if matches!(
                node.kind(),
                "namespace_declaration"
                    | "package_declaration"
                    | "annotation_definition"
                    | "fb_annotation_definition"
                    | "include_statement"
                    | "literal"
                    | "number"
                    | "container_type"
            ) =>
        {
            "thrift.source_semantics_unavailable"
        }
        StructuralRole::Signature
            if node.kind() == "service_definition" && performs(node, cancellation)? =>
        {
            "thrift.source_semantics_unavailable"
        }
        StructuralRole::Signature if !complete(node, source, cancellation)? => {
            "thrift.declaration_unavailable"
        }
        StructuralRole::Signature => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                cancellation.check()?;
                if matches!(child.kind(), "{" | "parameters") {
                    range.end = child.start_byte();
                    break;
                }
            }
            "thrift.written_header"
        }
        _ => return Err(query_failure("query-thrift-role")),
    };
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    })
}
