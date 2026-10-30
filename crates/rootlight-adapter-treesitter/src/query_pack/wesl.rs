//! Capture WESL function owners and direct formal names from their native CST.
//! Other written identifiers and imports retain unresolved source evidence.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

fn function(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if node.kind() == "function_decl" {
            return Some(node);
        }
        node = node.parent()?;
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        let Some(owner) = function(node) else {
            return Ok(None);
        };
        if owner.has_error() || owner.is_missing() {
            return Ok(None);
        }
    }
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => "wesl.file",
        StructuralRole::Scope => match node.kind() {
            "translation_unit" => "wesl.file_scope",
            "function_decl" => "wesl.function_scope",
            "param_list" => "wesl.parameters_scope",
            "compound_statement" => "wesl.block_scope",
            _ => return Err(query_failure("query-wesl-scope")),
        },
        StructuralRole::Declaration => match node.kind() {
            "function_decl" => "wesl.function",
            "param" => {
                let direct = node.parent().is_some_and(|params| {
                    params.kind() == "param_list"
                        && params.parent().is_some_and(|header| {
                            header.kind() == "function_header"
                                && header.child_by_field_name("parameters") == Some(params)
                        })
                });
                if !direct {
                    return Ok(None);
                }
                "wesl.parameter"
            }
            _ => return Err(query_failure("query-wesl-declaration")),
        },
        StructuralRole::Definition => "wesl.name",
        StructuralRole::Reference => {
            if matches!(node.kind(), "import_statement" | "preproc_bevy_import") {
                "wesl.import"
            } else {
                if node.parent().is_some_and(|parent| {
                    matches!(parent.kind(), "function_header" | "param")
                        && parent.child_by_field_name("name") == Some(node)
                }) {
                    return Ok(None);
                }
                "wesl.identifier"
            }
        }
        StructuralRole::Signature => "wesl.header",
        StructuralRole::Expression => "wesl.context_unavailable",
        StructuralRole::Comment => "wesl.comment",
        _ => return Err(query_failure("query-wesl-role")),
    };
    let captured = if role == StructuralRole::Signature {
        node.named_child(0)
            .filter(|header| header.kind() == "function_header")
            .ok_or_else(|| query_failure("query-wesl-header"))?
    } else {
        node
    };
    let mut depth = native_depth(captured, cancel)?;
    if role == StructuralRole::Scope && node.kind() == "function_decl" {
        depth = depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
    }
    Ok(Some(QueryCandidate {
        start: captured.start_byte(),
        end: captured.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    }))
}
