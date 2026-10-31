//! Capture complete written prefixes and explicit AS aliases, never variable uses.
//! Native query/subselect scopes provide identity without inventing query entities.

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
        // A recovered containing request cannot certify a clean-looking alias.
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
        StructuralRole::Root | StructuralRole::Module => "sparql.file",
        StructuralRole::Scope if query_scope(node) => "sparql.query",
        StructuralRole::Declaration if node.kind() == "prefix_declaration" => "sparql.prefix",
        StructuralRole::Definition if node.kind() == "pn_prefix" => {
            if !node
                .parent()
                .and_then(|parent| parent.parent())
                .is_some_and(|parent| parent.kind() == "prefix_declaration")
            {
                return Err(query_failure("query-sparql-prefix-owner"));
            }
            "sparql.prefix"
        }
        StructuralRole::Declaration | StructuralRole::Definition if node.kind() == "var" => {
            if !node
                .parent()
                .is_some_and(|parent| matches!(parent.kind(), "select_clause" | "bind"))
            {
                return Err(query_failure("query-sparql-alias-owner"));
            }
            let mut previous = node.prev_sibling();
            while previous.is_some_and(|node| node.is_extra()) {
                cancel.check()?;
                previous = previous.and_then(|node| node.prev_sibling());
            }
            if !previous.is_some_and(|node| node.kind() == "AS") {
                return Ok(None);
            }
            let mut owner = node.parent();
            while owner.is_some_and(|node| !query_scope(node)) {
                cancel.check()?;
                owner = owner.and_then(|node| node.parent());
            }
            if owner.is_none() {
                return Ok(None);
            }
            "sparql.alias"
        }
        _ => return Err(query_failure("query-sparql-written-role")),
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

fn query_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "select_query"
            | "sub_select"
            | "construct_query"
            | "describe_query"
            | "ask_query"
            | "modify"
    )
}
