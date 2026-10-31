//! Retain native TriG graph owners and their source-written labels.
//! RDF graph names are resource terms, so no label is promoted to a Definition.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) fn candidate(
    context: &super::turtle::Context,
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let syntax = match (node.kind(), role) {
        ("document", StructuralRole::Root | StructuralRole::Module) => "trig.file",
        ("graph", StructuralRole::Module) => {
            if node.has_error() || node.is_missing() {
                return Ok(None);
            }
            let mut cursor = node.walk();
            let named = node
                .children_by_field_name("label", &mut cursor)
                .any(|term| term.is_named());
            if named {
                "trig.named_graph"
            } else {
                "trig.default_graph"
            }
        }
        (
            "iri_reference" | "prefixed_name" | "blank_node_label" | "anon",
            StructuralRole::Signature,
        ) => {
            let Some(owner) = node.parent().filter(|parent| {
                parent.kind() == "graph" && !parent.has_error() && !parent.is_missing()
            }) else {
                return Ok(None);
            };
            let mut cursor = owner.walk();
            if !owner
                .children_by_field_name("label", &mut cursor)
                .any(|label| label.id() == node.id())
            {
                return Ok(None);
            }
            "trig.graph_label"
        }
        _ => {
            let Some(mut candidate) =
                super::turtle::candidate(context, node, role, source, cancellation)?
            else {
                return Ok(None);
            };
            candidate.syntax = match candidate.syntax {
                "turtle.file" => "trig.file",
                "turtle.prefix" => "trig.prefix",
                "turtle.prefix_binding" => "trig.prefix_binding",
                "turtle.class" => "trig.class",
                "turtle.property" => "trig.property",
                _ => return Err(query_failure("query-trig-rdf-role")),
            };
            return Ok(Some(candidate));
        }
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    }))
}
