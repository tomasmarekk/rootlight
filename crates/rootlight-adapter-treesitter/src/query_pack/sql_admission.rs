//! Cancel-aware SQL declaration admission before source-name projection.
//! Invalid routine owners must not reparent otherwise clean child declarations.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{StructuralRole, sql};

pub(super) fn retain(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if !matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        return Ok(true);
    }
    // A valid child cannot acquire a file owner when its authored routine or
    // table-variable owner was rejected. Ordinary top-level SQL stays independent.
    let mut ancestor = node.parent();
    while let Some(owner) = ancestor {
        cancellation.check()?;
        if owner.is_error()
            || owner.is_missing()
            || (owner.kind() == "tsql_table_variable" && owner.has_error())
            || (matches!(owner.kind(), "tsql_if" | "tsql_conditional_block") && owner.has_error())
        {
            return Ok(false);
        }
        if owner.kind() == "tsql_procedure" && sql::procedure_header(owner).is_none() {
            let header_identity = sql::recovered_procedure_header(owner, cancellation)?;
            let own_header = header_identity.is_some_and(|header| {
                node == header
                    || node.kind() == "tsql_parameter"
                        && node.parent().is_some_and(|parameters| {
                            parameters.kind() == "tsql_parameters"
                                && parameters.parent() == Some(header)
                        })
            });
            let body_metadata = role == StructuralRole::Signature
                && node.parent() == Some(owner)
                && sql::recovered_body(node, cancellation)?;
            if !own_header && !body_metadata {
                return Ok(false);
            }
        }
        ancestor = owner.parent();
    }
    Ok(match node.kind() {
        "tsql_procedure" => sql::declaration_header(node, cancellation)?.is_some(),
        "tsql_procedure_header" => {
            if let Some(parent) = node.parent() {
                sql::declaration_header(parent, cancellation)? == Some(node)
            } else {
                false
            }
        }
        "tsql_parameter" => {
            if node.has_error() || node.is_missing() {
                return Ok(false);
            }
            let Some(parameters) = node
                .parent()
                .filter(|parent| parent.kind() == "tsql_parameters")
            else {
                return Ok(false);
            };
            let Some(header) = parameters
                .parent()
                .filter(|parent| parent.kind() == "tsql_procedure_header")
            else {
                return Ok(false);
            };
            if let Some(parent) = header.parent() {
                sql::declaration_header(parent, cancellation)? == Some(header)
            } else {
                false
            }
        }
        "tsql_variable" | "tsql_table_variable" => !node.has_error() && !node.is_missing(),
        _ => true,
    })
}
