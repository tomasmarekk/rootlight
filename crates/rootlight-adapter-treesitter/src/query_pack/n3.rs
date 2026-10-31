//! Capture written N3 declarations without interpreting RDF or logical terms.
//! Explicit quantifiers belong to their containing formula, unlike quick variables.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) {
        // Recovery can place a malformed list tail outside its directive node.
        // Require a complete containing document before certifying written binders.
        let mut ancestor = Some(node);
        while let Some(current) = ancestor {
            cancel.check()?;
            if current.has_error() || current.is_missing() {
                return Ok(None);
            }
            ancestor = current.parent();
        }
    }
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => "n3.file",
        StructuralRole::Scope if node.kind() == "formula" => "n3.formula",
        StructuralRole::Declaration | StructuralRole::Definition => {
            let owner = if role == StructuralRole::Definition && node.kind() == "pn_prefix" {
                node.parent().and_then(|parent| parent.parent())
            } else if matches!(node.kind(), "iri_reference" | "prefixed_name") {
                node.parent()
            } else {
                Some(node)
            };
            let Some(owner) = owner.filter(|owner| !owner.has_error() && !owner.is_missing())
            else {
                return Ok(None);
            };
            match owner.kind() {
                "prefix_id" | "sparql_prefix" => "n3.prefix",
                "universal" => "n3.universal_variable",
                "existential" => "n3.existential_variable",
                _ => return Err(query_failure("query-n3-declaration-owner")),
            }
        }
        _ => return Err(query_failure("query-n3-written-role")),
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancel)?,
    }))
}
