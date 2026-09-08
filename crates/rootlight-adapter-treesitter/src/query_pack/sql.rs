//! Native SQL declaration ownership independent of query match ordering.
//! Extra comment nodes and optional clauses must not turn referenced objects or
//! authorization principals into definitions of the enclosing database object.

use tree_sitter::Node;

pub(super) fn is_declared_name(node: Node<'_>) -> bool {
    node.parent().and_then(definition_node) == Some(node)
}

pub(super) fn definition_node(declaration: Node<'_>) -> Option<Node<'_>> {
    match declaration.kind() {
        "create_index" => declaration.child_by_field_name("column"),
        "column_definition" => declaration.child_by_field_name("name"),
        "function_argument" => first_child(declaration, "identifier"),
        "create_table"
        | "create_view"
        | "create_materialized_view"
        | "create_function"
        | "create_sequence"
        | "create_trigger"
        | "create_type" => first_child(declaration, "object_reference"),
        "create_database" | "create_role" | "create_extension" => {
            first_child(declaration, "identifier")
        }
        "create_schema" => {
            let mut cursor = declaration.walk();
            for child in declaration.named_children(&mut cursor) {
                match child.kind() {
                    "keyword_authorization" => return None,
                    "identifier" => return Some(child),
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

fn first_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}
