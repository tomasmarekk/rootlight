//! Native DQL query roles retain direct field ownership and physical spelling.
//! Required expressions are data evidence, without software or database targets.

use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn field(node: Node<'_>, owner: &str, name: &str) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == owner
            && parent
                .child_by_field_name(name)
                .is_some_and(|child| child.id() == node.id())
    })
}

fn variable_predicate(node: Node<'_>, source: &[u8]) -> bool {
    let Some(name) = node
        .named_child(0)
        .and_then(|predicate| predicate.child_by_field_name("name"))
    else {
        return false;
    };
    if name.kind() != "identifier" || name.byte_range() != node.byte_range() {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if !matches!(parent.kind(), "root_function" | "nested_function") {
        return false;
    }
    let Some(name) = parent.child_by_field_name("name") else {
        return false;
    };
    source.get(name.byte_range()).is_some_and(|name| {
        name.eq_ignore_ascii_case(b"uid")
            || name.eq_ignore_ascii_case(b"val")
            || name.eq_ignore_ascii_case(b"len")
    })
}

fn name_label(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    let parent = node.parent()?;
    if !clean(parent, source) {
        return None;
    }
    for owner in [
        "root_function",
        "function_call",
        "nested_function",
        "count_selection",
        "value_selection",
        "aggregate_selection",
    ] {
        if field(node, owner, "name") {
            return Some("dql.call_name");
        }
    }
    if field(node, "query_operation", "name") {
        return Some("dql.operation_name");
    }
    if field(node, "root_query", "name") || field(node, "aggregation_root", "name") {
        return Some("dql.root_name");
    }
    if field(node, "variable_binding", "name") {
        return Some("dql.binding_name");
    }
    if field(node, "output_alias", "name") {
        return Some("dql.alias_name");
    }
    if field(node, "variable_definition", "type") {
        return Some("dql.parameter_type");
    }
    if field(node, "argument", "name") {
        return Some("dql.argument_name");
    }
    if field(node, "value_selection", "variable") {
        return Some("dql.variable_use");
    }
    if field(node, "unary_expression", "operator") || field(node, "binary_expression", "operator") {
        return Some("dql.operator_name");
    }
    if field(node, "predicate_name", "name") {
        let wrapper = parent.parent()?;
        if variable_predicate(wrapper, source) {
            return None;
        }
        return Some("dql.predicate_name");
    }
    None
}

/// Certifies only clean native roles; recovered owners cannot grant identity.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    if !matches!(role, StructuralRole::Root | StructuralRole::Module) {
        if !clean(node, source) {
            return Ok(None);
        }
        let mut parent = node.parent();
        while let Some(owner) = parent {
            cancellation.check()?;
            if owner.kind() == "ERROR" || owner.is_missing() {
                return Ok(None);
            }
            parent = owner.parent();
        }
    }
    let syntax = match (node.kind(), role) {
        ("document", StructuralRole::Root | StructuralRole::Module) if node.parent().is_none() => {
            "dql.file"
        }
        ("comment", StructuralRole::Comment) => "dql.comment",
        ("string", StructuralRole::StringLiteral) => "dql.string",
        (_, StructuralRole::Expression) => match node.kind() {
            "query_operation" => "dql.operation",
            "root_query" => "dql.root",
            "aggregation_root" => "dql.aggregate_root",
            "variable_definition" => "dql.parameter",
            "variable_reference" if field(node, "variable_definition", "name") => {
                "dql.parameter_name"
            }
            "variable_reference" => "dql.parameter_use",
            "variable_binding" => "dql.binding",
            "output_alias" => "dql.alias",
            "root_function" => "dql.generator",
            "field_selection" => "dql.selection",
            "count_selection" => "dql.count",
            "value_selection" => "dql.value",
            "aggregate_selection" => "dql.aggregate",
            "predicate" if variable_predicate(node, source) => "dql.variable_use",
            "predicate" | "selection_predicate" => "dql.predicate",
            "predicate_name"
                if node
                    .parent()
                    .is_some_and(|owner| owner.kind() == "count_selection") =>
            {
                "dql.predicate"
            }
            "predicate_name" => return Ok(None),
            "function_call" => "dql.filter_call",
            "nested_function" => "dql.nested_call",
            "filter_directive" => "dql.filter",
            "flag_directive" => "dql.flag",
            "unary_expression" => "dql.unary",
            "binary_expression" => "dql.binary",
            "parenthesized_expression" => "dql.group",
            "function_language" | "language_suffix" => "dql.language",
            "argument" => "dql.argument",
            "string" => "dql.string",
            "number" => "dql.number",
            "boolean" => "dql.boolean",
            "regex" => "dql.regex",
            "list" => "dql.list",
            "identifier" | "function_name" | "filter_operator" => {
                let Some(label) = name_label(node, source) else {
                    return Ok(None);
                };
                label
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure() || role == StructuralRole::Expression,
        native_depth: native_depth(node, cancellation)?,
    }))
}
