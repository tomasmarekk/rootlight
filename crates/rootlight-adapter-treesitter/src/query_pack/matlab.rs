//! Native MATLAB binding positions and body-independent declaration headers.
//! Applications retain uncertainty because MATLAB shares call and indexing syntax.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn binding(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let Some(parent) = node.parent() else {
        return Ok(None);
    };
    Ok(Some(match parent.kind() {
        "function_arguments" => "matlab.parameter",
        "arguments"
            if parent
                .parent()
                .is_some_and(|owner| owner.kind() == "lambda") =>
        {
            "matlab.parameter"
        }
        "function_output" => "matlab.output_variable",
        "multioutput_variable"
            if parent
                .parent()
                .is_some_and(|owner| owner.kind() == "function_output") =>
        {
            "matlab.output_variable"
        }
        "multioutput_variable"
            if parent
                .parent()
                .is_some_and(|owner| owner.kind() == "assignment") =>
        {
            "matlab.variable"
        }
        "assignment" if parent.child_by_field_name("left") == Some(node) => "matlab.variable",
        "iterator" if parent.named_child(0) == Some(node) => "matlab.variable",
        "catch_clause" if parent.named_child(0) == Some(node) => "matlab.variable",
        "global_operator" => "matlab.global_variable",
        "persistent_operator" => "matlab.persistent_variable",
        _ if assignment_root(node, cancellation)? => "matlab.variable",
        _ => return Ok(None),
    }))
}

fn assignment_root(mut node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        match parent.kind() {
            "assignment" => return Ok(parent.child_by_field_name("left") == Some(node)),
            "multioutput_variable" => {}
            "function_call" if parent.child_by_field_name("name") == Some(node) => {}
            "field_expression" if parent.child_by_field_name("object") == Some(node) => {}
            _ => return Ok(false),
        }
        node = parent;
    }
    Ok(false)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.kind() != "identifier" {
        return Ok(true);
    }
    Ok(match role {
        StructuralRole::Declaration => binding(node, cancellation)?.is_some(),
        StructuralRole::Reference if binding(node, cancellation)?.is_some() => false,
        StructuralRole::Reference
            if node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    "function_definition" | "function_signature" | "class_definition"
                ) && parent.child_by_field_name("name") == Some(node)
            }) =>
        {
            false
        }
        StructuralRole::Definition => {
            binding(node, cancellation)?.is_some()
                || node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "class_definition" | "function_definition" | "function_signature"
                    ) && parent.child_by_field_name("name") == Some(node)
                        || parent.kind() == "property"
                            && parent.child_by_field_name("name") == Some(node)
                            && parent
                                .parent()
                                .is_some_and(|owner| owner.kind() == "properties")
                })
        }
        _ => true,
    })
}

pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if role == StructuralRole::Declaration && node.kind() == "identifier" {
        return binding(node, cancellation);
    }
    if role == StructuralRole::Reference && node.kind() == "identifier" {
        return Ok(Some(reference_syntax(node)));
    }
    Ok(native_syntax(node, source))
}

fn native_syntax(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    Some(match node.kind() {
        "source_file" => "matlab.file",
        "function_definition"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "methods") =>
        {
            let class = node.parent()?.parent()?;
            let name = node.child_by_field_name("name")?.utf8_text(source).ok()?;
            let class_name = class.child_by_field_name("name")?.utf8_text(source).ok()?;
            if name == class_name {
                "matlab.constructor"
            } else {
                "matlab.method"
            }
        }
        "function_definition" => "matlab.function",
        "function_signature" => "matlab.method",
        "class_definition" => "matlab.class",
        "property" => "matlab.property",
        "identifier" | "property_name" => "matlab.identifier",
        "lambda" => "matlab.lambda",
        "function_call" => "matlab.application",
        "command" => "matlab.command",
        "comment" => "matlab.comment",
        "string" => "matlab.string",
        _ => return None,
    })
}

fn reference_syntax(node: Node<'_>) -> &'static str {
    let Some(parent) = node.parent() else {
        return "matlab.identifier";
    };
    match parent.kind() {
        "field_expression" if parent.child_by_field_name("object") != Some(node) => {
            "matlab.member_name"
        }
        "function_call"
            if parent.child_by_field_name("name") == Some(node)
                && parent.parent().is_some_and(|owner| {
                    owner.kind() == "field_expression"
                        && owner.child_by_field_name("object") != Some(parent)
                }) =>
        {
            "matlab.member_name"
        }
        "handle_operator" => "matlab.function_handle_name",
        "metaclass_operator" | "superclasses" | "superclass" | "property_name"
        | "class_property" | "attribute" => "matlab.type_or_attribute_name",
        "property" if parent.child_by_field_name("name") != Some(node) => {
            "matlab.type_or_attribute_name"
        }
        "property"
            if parent
                .parent()
                .is_some_and(|owner| owner.kind() == "properties") =>
        {
            "matlab.member_name"
        }
        _ => "matlab.identifier",
    }
}

pub(super) fn definition_start(node: Node<'_>) -> usize {
    if node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "function_definition" | "function_signature")
            && parent.child_by_field_name("name") == Some(node)
    }) && let Some(prefix) = node.prev_sibling()
        && matches!(prefix.kind(), "get." | "set.")
    {
        return prefix.start_byte();
    }
    node.start_byte()
}

pub(super) fn header_end(node: Node<'_>) -> usize {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| {
            matches!(
                child.kind(),
                "\n" | ";"
                    | ","
                    | "block"
                    | "arguments_statement"
                    | "properties"
                    | "methods"
                    | "events"
                    | "enumeration"
                    | "end"
                    | "endfunction"
            )
        })
        .map_or(node.end_byte(), |child| child.start_byte())
}
