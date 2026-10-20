//! GraphQL captures keep physical declaration names distinct from query selections.
//! The retained grammar has no fields; direct native-child context proves names.
//! Extensions and uncertain escape syntax retain ranged gaps instead of new symbols.

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
        "object_type_definition" => "graphql.object",
        "interface_type_definition" => "graphql.interface",
        "input_object_type_definition" => "graphql.input",
        "scalar_type_definition" => "graphql.scalar",
        "union_type_definition" => "graphql.union",
        "enum_type_definition" => "graphql.enum",
        "directive_definition" => "graphql.directive",
        "field_definition" => "graphql.field",
        "input_value_definition"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "arguments_definition") =>
        {
            "graphql.argument"
        }
        "input_value_definition" => "graphql.input_field",
        "enum_value_definition" => "graphql.enum_value",
        "operation_definition" => "graphql.operation",
        "fragment_definition" => "graphql.fragment",
        "variable_definition" => "graphql.variable",
        _ => return None,
    })
}

fn name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let wrapper = match node.kind() {
        "fragment_definition" => direct(node, "fragment_name", cancellation)?,
        "enum_value_definition" => direct(node, "enum_value", cancellation)?,
        "variable_definition" => direct(node, "variable", cancellation)?,
        _ => return direct(node, "name", cancellation),
    };
    match wrapper {
        Some(wrapper) => direct(wrapper, "name", cancellation),
        None => Ok(None),
    }
}

fn ordinary_name(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing()
        && !node.has_error()
        && source.get(node.byte_range()).is_some_and(|bytes| {
            bytes
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        })
}

fn visible(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.kind() == "type_system_extension"
            || owner.kind() == "ERROR"
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
        && name(node, cancellation)?.is_some_and(|name| ordinary_name(name, source)))
}

fn definition_name(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut current = node.parent();
    for _ in 0..3 {
        let Some(owner) = current else {
            break;
        };
        cancellation.check()?;
        if declaration_kind(owner).is_some() {
            return Ok(name(owner, cancellation)? == Some(node));
        }
        if !matches!(owner.kind(), "fragment_name" | "enum_value" | "variable") {
            break;
        }
        current = owner.parent();
    }
    Ok(false)
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
        StructuralRole::Signature if node.end_byte() == node.start_byte() => false,
        StructuralRole::Signature if node.kind() == "string_value" => source
            .get(node.byte_range())
            .is_some_and(|text| text.contains(&b'\\') || text.starts_with(b"\"\"\"")),
        StructuralRole::Signature
            if node.kind() == "type_system_extension" || node.kind() == "ERROR" =>
        {
            true
        }
        StructuralRole::Signature
            if node.kind() == "operation_definition"
                && !node.has_error()
                && visible(node, cancellation)?
                && name(node, cancellation)?.is_none() =>
        {
            false
        }
        StructuralRole::Signature => match node.parent() {
            Some(owner) if !visible(owner, cancellation)? => false,
            _ => declaration_kind(node).is_some(),
        },
        StructuralRole::Scope => {
            visible(node, cancellation)?
                && !node.has_error()
                && (node.kind() == "schema_definition" || name(node, cancellation)?.is_none())
        }
        StructuralRole::Reference => {
            visible(node, cancellation)?
                && ordinary_name(node, source)
                && !definition_name(node, cancellation)?
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
        StructuralRole::Root | StructuralRole::Module => "graphql.file",
        StructuralRole::Definition => "graphql.name",
        StructuralRole::Reference
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "named_type") =>
        {
            "graphql.named_type"
        }
        StructuralRole::Reference => "graphql.written_name",
        StructuralRole::Comment => "graphql.comment",
        StructuralRole::StringLiteral => "graphql.string",
        StructuralRole::Scope => "graphql.anonymous_owner",
        StructuralRole::Signature if node.kind() == "string_value" => {
            "graphql.string_validation_unavailable"
        }
        StructuralRole::Signature if node.kind() == "type_system_extension" => {
            "graphql.extension_unavailable"
        }
        StructuralRole::Signature
            if node.kind() == "ERROR" || !valid_declaration(node, source, cancellation)? =>
        {
            "graphql.declaration_unavailable"
        }
        _ => declaration_kind(node).ok_or_else(|| query_failure("query-graphql-declaration"))?,
    };
    let mut range = node.byte_range();
    if role == StructuralRole::Definition {
        range = name(node, cancellation)?
            .ok_or_else(|| query_failure("query-graphql-name"))?
            .byte_range();
    } else if role == StructuralRole::Signature && !syntax.ends_with("_unavailable") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            cancellation.check()?;
            if matches!(
                child.kind(),
                "fields_definition"
                    | "input_fields_definition"
                    | "enum_values_definition"
                    | "selection_set"
            ) {
                range.end = child.start_byte();
                while range.end > range.start
                    && source
                        .get(range.end - 1)
                        .is_some_and(u8::is_ascii_whitespace)
                {
                    range.end -= 1;
                }
                break;
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
