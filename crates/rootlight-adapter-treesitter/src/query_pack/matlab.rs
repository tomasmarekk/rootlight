//! Native MATLAB binding positions and body-independent declaration headers.
//! Applications retain uncertainty because MATLAB shares call and indexing syntax.

use tree_sitter::Node;

use super::StructuralRole;

fn binding(node: Node<'_>) -> Option<&'static str> {
    let parent = node.parent()?;
    Some(match parent.kind() {
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
        "assignment" if parent.child_by_field_name("left") == Some(node) => "matlab.variable",
        "iterator" if parent.named_child(0) == Some(node) => "matlab.variable",
        "global_operator" | "persistent_operator" => "matlab.variable",
        _ => return None,
    })
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if node.kind() != "identifier" {
        return true;
    }
    match role {
        StructuralRole::Declaration => binding(node).is_some(),
        StructuralRole::Definition => {
            binding(node).is_some()
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
    }
}

pub(super) fn syntax(node: Node<'_>, role: StructuralRole, source: &[u8]) -> Option<&'static str> {
    if role == StructuralRole::Declaration && node.kind() == "identifier" {
        return binding(node);
    }
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
