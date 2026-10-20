//! Cap'n Proto captures prove direct written names in the pinned fieldless CST.
//! Named returns are flattened siblings, so each return-name leaf owns one fact.
//! Aliases, generic binding and parser recovery remain explicit source gaps.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn declaration_kind(node: Node<'_>) -> Option<&'static str> {
    Some(match node.kind() {
        "struct" => "capnp.struct",
        "enum" => "capnp.enum",
        "interface" => "capnp.interface",
        "field" | "union_field" => "capnp.field",
        "group" => "capnp.group",
        "union" => "capnp.union",
        "method" => "capnp.method",
        "parameter" => "capnp.parameter",
        "return_identifier" => "capnp.return_parameter",
        "enum_field" => "capnp.enum_value",
        "const" => "capnp.const",
        "annotation" => "capnp.annotation",
        _ => return None,
    })
}

fn name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let kind = match node.kind() {
        "return_identifier" => return Ok(Some(node)),
        "struct" | "interface" | "group" | "union" => "type_identifier",
        "enum" => "enum_identifier",
        "field" | "union_field" => "field_identifier",
        "method" => "method_identifier",
        "parameter" => "param_identifier",
        "enum_field" => "enum_member",
        "const" => "const_identifier",
        "annotation" => "annotation_definition_identifier",
        _ => return Ok(None),
    };
    direct(node, kind, cancellation)
}

fn identifier(bytes: &[u8]) -> bool {
    bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn path(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing()
        && !node.has_error()
        && source
            .get(node.byte_range())
            .is_some_and(|bytes| bytes.split(|byte| *byte == b'.').all(identifier))
}

fn visible(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.kind() == "ERROR"
            || owner.is_missing()
            || (declaration_kind(owner).is_some() && owner.has_error())
        {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

fn valid_declaration(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    Ok(visible(node, cancellation)?
        && name(node, cancellation)?
            .is_some_and(|name| source.get(name.byte_range()).is_some_and(identifier)))
}

fn type_position(node: Node<'_>) -> bool {
    matches!(node.kind(), "custom_type" | "extend_type")
        || (node.kind() == "type_identifier"
            && node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    "custom_type" | "method" | "named_return_type" | "unnamed_return_type"
                )
            }))
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    Ok(match role {
        StructuralRole::Root | StructuralRole::Module => node.parent().is_none(),
        StructuralRole::Declaration | StructuralRole::Definition => {
            valid_declaration(node, source, cancellation)?
        }
        StructuralRole::Scope => {
            node.kind() == "union"
                && visible(node, cancellation)?
                && !node.has_error()
                && name(node, cancellation)?.is_none()
        }
        StructuralRole::Signature if node.start_byte() == node.end_byte() => false,
        StructuralRole::Signature
            if matches!(
                node.kind(),
                "ERROR" | "using_directive" | "import" | "generics" | "implicit_generics"
            ) =>
        {
            true
        }
        StructuralRole::Signature if node.kind() == "custom_type" => !path(node, source),
        StructuralRole::Signature
            if node.kind() == "union"
                && !node.has_error()
                && name(node, cancellation)?.is_none() =>
        {
            false
        }
        StructuralRole::Signature
            if matches!(node.kind(), "field" | "union_field")
                && !node.has_error()
                && name(node, cancellation)?.is_none() =>
        {
            false
        }
        StructuralRole::Signature => declaration_kind(node).is_some(),
        StructuralRole::Reference => {
            if !visible(node, cancellation)? || !path(node, source) {
                return Ok(false);
            }
            if node.kind() == "custom_type" {
                true
            } else if node
                .parent()
                .is_some_and(|parent| parent.kind() == "custom_type" && path(parent, source))
            {
                false
            } else if let Some(owner) = node.parent() {
                name(owner, cancellation)? != Some(node)
            } else {
                false
            }
        }
        StructuralRole::Comment | StructuralRole::StringLiteral => true,
        _ => false,
    })
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => "capnp.file",
        StructuralRole::Definition => "capnp.name",
        StructuralRole::Reference if !type_position(node) => "capnp.written_name",
        StructuralRole::Reference
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "custom_type" && !path(parent, source)) =>
        {
            "capnp.unavailable_type"
        }
        StructuralRole::Reference => "capnp.named_type",
        StructuralRole::Comment => "capnp.comment",
        StructuralRole::StringLiteral => "capnp.string",
        StructuralRole::Scope => "capnp.anonymous_union",
        StructuralRole::Signature
            if matches!(
                node.kind(),
                "using_directive" | "import" | "generics" | "implicit_generics"
            ) =>
        {
            "capnp.binding_unavailable"
        }
        StructuralRole::Signature if node.kind() == "custom_type" => "capnp.type_unavailable",
        StructuralRole::Signature
            if node.kind() == "ERROR" || !valid_declaration(node, source, cancellation)? =>
        {
            "capnp.declaration_unavailable"
        }
        _ => declaration_kind(node).ok_or_else(|| query_failure("query-capnp-declaration"))?,
    };
    let mut range = node.byte_range();
    if role == StructuralRole::Definition {
        range = name(node, cancellation)?
            .ok_or_else(|| query_failure("query-capnp-name"))?
            .byte_range();
    } else if role == StructuralRole::Signature && !syntax.ends_with("_unavailable") {
        let bytes = source
            .get(range.clone())
            .ok_or_else(|| query_failure("query-capnp-range"))?;
        if let Some(offset) = bytes.iter().position(|byte| *byte == b'{') {
            range.end = range.start + offset;
            while range.end > range.start
                && source
                    .get(range.end - 1)
                    .is_some_and(u8::is_ascii_whitespace)
            {
                range.end -= 1;
            }
        }
    }
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    })
}
