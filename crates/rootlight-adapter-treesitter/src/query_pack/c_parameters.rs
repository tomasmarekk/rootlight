//! Prove named scalar/pointer formals in clean C function-definition headers.
//! Prototype and nested function-pointer binders cannot borrow an outer body's
//! owner; this structural evidence does not resolve parameter value reads.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

pub(super) fn is_parameter_capture(node: Node<'_>, role: StructuralRole) -> bool {
    (role == StructuralRole::Declaration && node.kind() == "parameter_declaration")
        || (role == StructuralRole::Definition
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "parameter_declaration"))
}

/// Share the same ordinary-definition binder boundary with native read proofs.
/// Unsupported declarators and cancellation retain their existing outcomes.
pub(super) fn written_name<'tree>(
    parameter: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if parameter.kind() != "parameter_declaration"
        || parameter.is_missing()
        || parameter.has_error()
    {
        return Ok(None);
    }
    let Some(parameters) = parameter
        .parent()
        .filter(|node| node.kind() == "parameter_list")
    else {
        return Ok(None);
    };
    let Some(callable) = parameters.parent().filter(|node| {
        node.kind() == "function_declarator"
            && node.child_by_field_name("parameters") == Some(parameters)
            && !node.has_error()
            && !node.is_missing()
    }) else {
        return Ok(None);
    };
    if callable
        .child_by_field_name("declarator")
        .is_none_or(|node| node.kind() != "identifier")
    {
        return Ok(None);
    }
    let mut header = callable;
    let function = loop {
        cancellation.check()?;
        let Some(parent) = header.parent() else {
            return Ok(None);
        };
        match parent.kind() {
            "pointer_declarator" if parent.child_by_field_name("declarator") == Some(header) => {
                header = parent;
            }
            "function_definition" if parent.child_by_field_name("declarator") == Some(header) => {
                break parent;
            }
            _ => return Ok(None),
        }
    };
    if function.is_missing() || function.has_error() {
        return Ok(None);
    }
    let Some(mut name) = parameter.child_by_field_name("declarator") else {
        return Ok(None);
    };
    loop {
        cancellation.check()?;
        if name.is_missing() || name.has_error() {
            return Ok(None);
        }
        match name.kind() {
            "identifier" => return Ok(Some(name)),
            "pointer_declarator" => {
                let Some(inner) = name.child_by_field_name("declarator") else {
                    return Ok(None);
                };
                name = inner;
            }
            _ => return Ok(None),
        }
    }
}

/// Return one existing-budget capture only for a proved ordinary formal header.
/// Cancellation propagates before native ancestry or declarator traversal.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let parameter = if role == StructuralRole::Declaration {
        Some(node)
    } else {
        node.parent()
    };
    let Some(parameter) = parameter else {
        return Ok(None);
    };
    let Some(name) = written_name(parameter, cancellation)? else {
        return Ok(None);
    };
    let selected = if role == StructuralRole::Definition {
        name
    } else {
        parameter
    };
    Ok(Some(QueryCandidate {
        start: selected.start_byte(),
        end: selected.end_byte(),
        role,
        syntax: if role == StructuralRole::Definition {
            "c.identifier"
        } else {
            "c.parameter"
        },
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(selected, cancellation)?,
    }))
}
