//! Maps YAML native nodes to source-preserving data and serialization roles.
//! Tags and directives remain required context; scalar construction belongs to
//! lowering, where it can use the complete source rather than a truncated node.

use super::{AdapterError, QueryCandidate, StructuralRole, query_failure};
use tree_sitter::Node;

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
) -> Result<QueryCandidate, AdapterError> {
    let mut start = node.start_byte();
    let mut end = node.end_byte();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => "yaml.file",
        StructuralRole::Scope => scope(node)?,
        StructuralRole::Declaration => match node.kind() {
            "block_mapping_pair" | "flow_pair" | "flow_node" => "yaml.property",
            "anchor" => {
                let owner = node
                    .parent()
                    .ok_or_else(|| query_failure("query-yaml-anchor"))?;
                start = owner.start_byte();
                end = owner.end_byte();
                "yaml.anchor"
            }
            _ => return Err(query_failure("query-yaml-declaration")),
        },
        StructuralRole::Definition => match node.kind() {
            "anchor_name" => "yaml.anchor",
            "flow_node" | "block_node" => "yaml.node_key",
            "block_mapping_pair" | "flow_pair" => {
                // An omitted key is an empty scalar at the mapping indicator,
                // not the complete pair or its value's text.
                let mut cursor = node.walk();
                let indicator = node
                    .children(&mut cursor)
                    .find(|child| matches!(child.kind(), ":" | "?"))
                    .ok_or_else(|| query_failure("query-yaml-empty-key"))?;
                start = indicator.end_byte();
                end = start;
                "yaml.empty_key"
            }
            _ => return Err(query_failure("query-yaml-definition")),
        },
        StructuralRole::Reference => "yaml.alias",
        StructuralRole::Signature => match node.kind() {
            "tag" => "yaml.tag",
            "yaml_directive" => "yaml.version",
            "tag_directive" => "yaml.tag_directive",
            "reserved_directive" => "yaml.reserved_directive",
            _ => return Err(query_failure("query-yaml-context")),
        },
        StructuralRole::StringLiteral => "yaml.string",
        StructuralRole::Comment | StructuralRole::Documentation => "yaml.comment",
        _ => return Err(query_failure("query-yaml-role")),
    };
    let mut native_depth = 0_usize;
    let mut parent = node.parent();
    while let Some(node) = parent {
        native_depth = native_depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-yaml-depth"))?;
        parent = node.parent();
    }
    Ok(QueryCandidate {
        start,
        end,
        role,
        syntax,
        required: false,
        native_depth,
    })
}

fn scope(node: Node<'_>) -> Result<&'static str, AdapterError> {
    if let Some(parent) = node.parent() {
        if parent.kind() == "flow_sequence" {
            return Ok("yaml.sequence_element");
        }
        if parent.child_by_field_name("key") == Some(node) {
            return Ok("yaml.key");
        }
    }
    Ok(match node.kind() {
        "document" => "yaml.document",
        "block_sequence_item" => "yaml.sequence_element",
        "block_node" | "flow_node" => "yaml.node",
        "block_mapping" | "flow_mapping" => "yaml.mapping",
        "block_sequence" | "flow_sequence" => "yaml.sequence",
        _ => return Err(query_failure("query-yaml-scope")),
    })
}
