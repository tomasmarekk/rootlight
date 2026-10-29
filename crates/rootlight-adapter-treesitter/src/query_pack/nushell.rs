//! Preserve native Nu declaration extents and exact written name endpoints.
//! Runtime values and command dispatch remain references without guessed targets.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    _source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    let parameter = if node.kind() == "parameter" {
        Some(node)
    } else if role == StructuralRole::Definition {
        node.parent().filter(|parent| parent.kind() == "parameter")
    } else {
        None
    };
    if let Some(parameter) = parameter {
        let formal = parameter.parent().is_some_and(|wrapper| {
            matches!(wrapper.kind(), "parameter_bracks" | "parameter_parens")
                && wrapper.parent().is_some_and(|owner| {
                    owner.kind() == "decl_def"
                        && owner.child_by_field_name("parameters") == Some(wrapper)
                })
        });
        if !formal {
            return Ok(None);
        }
    }
    let mut owner = Some(node);
    while let Some(current) = owner {
        cancel.check()?;
        if matches!(current.kind(), "decl_def" | "decl_module") {
            if current.has_error() || current.is_missing() {
                return Ok(None);
            }
            break;
        }
        owner = current.parent();
    }

    let syntax = match role {
        StructuralRole::Root => "nu.file",
        StructuralRole::Module => "nu.file",
        StructuralRole::Scope => "nu.scope",
        StructuralRole::Declaration => match node.kind() {
            "decl_def" => "nu.function",
            "decl_module" => "nu.namespace",
            "parameter" => "nu.parameter",
            "stmt_let" | "stmt_mut" | "stmt_const" => "nu.variable",
            _ => return Err(query_failure("query-nu-declaration-kind")),
        },
        StructuralRole::Definition => "nu.identifier",
        StructuralRole::Reference => {
            if node.parent().is_some_and(|p| p.kind() == "val_variable") {
                "nu.value"
            } else {
                "nu.completer"
            }
        }
        StructuralRole::Signature => "nu.runtime_semantics_unavailable",
        StructuralRole::Expression => "nu.runtime_semantics_unavailable",
        StructuralRole::Comment => "nu.comment",
        StructuralRole::StringLiteral => "nu.string",
        _ => return Err(query_failure("query-nu-role")),
    };
    let mut depth = native_depth(node, cancel)?;
    if role == StructuralRole::Scope && matches!(node.kind(), "decl_def" | "decl_module") {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-native-depth"))?;
    }
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    }))
}
