//! PowerShell source declarations and body-independent callable headers.
//! Dynamic commands and member assignments must not masquerade as new bindings.
//! Literal keys retain written spelling, not an evaluated runtime comparison key.

use tree_sitter::Node;

use super::StructuralRole;

pub(super) fn is_literal_key(mut node: Node<'_>) -> bool {
    loop {
        match node.kind() {
            "simple_name" | "integer_literal" | "real_literal" => return true,
            "key_expression" | "unary_expression" | "string_literal"
                if node.named_child_count() == 1 =>
            {
                let Some(child) = node.named_child(0) else {
                    return false;
                };
                node = child;
            }
            "verbatim_string_characters" | "verbatim_here_string_characters" => return true,
            "expandable_string_literal" | "expandable_here_string_literal" => {
                return node.named_child_count() == 0;
            }
            _ => return false,
        }
    }
}

fn is_assignment_target(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "left_assignment_expression" => return true,
            "array_literal_expression" => {}
            "cast_expression" if parent.named_child(1) == Some(node) => {}
            "logical_expression"
            | "bitwise_expression"
            | "comparison_expression"
            | "additive_expression"
            | "multiplicative_expression"
            | "format_expression"
            | "range_expression"
            | "unary_expression"
            | "expression_with_unary_operator"
                if parent.byte_range() == node.byte_range() => {}
            // A variable inside a receiver, index or evaluated expression is a
            // read, not a written binding. Only casts and comma targets may widen.
            _ => return false,
        }
        node = parent;
    }
    false
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if node.kind() == "key_expression" && role == StructuralRole::Definition {
        return is_literal_key(node);
    }
    if node.kind() == "variable"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "unary_expression")
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        )
    {
        return is_assignment_target(node);
    }
    if matches!(role, StructuralRole::CallName | StructuralRole::Reference)
        && node.kind() == "command_name"
    {
        return node.named_child_count() == 0;
    }
    true
}

pub(super) fn declaration_syntax(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    if node.kind() == "script_block_expression" {
        return Some("powershell.anonymous_function");
    }
    if node.kind() == "hash_entry" {
        return Some(if node.named_child(0).is_some_and(is_literal_key) {
            "powershell.property"
        } else {
            "powershell.dynamic_property"
        });
    }
    if node.kind() == "class_method_definition" {
        let mut cursor = node.walk();
        let name = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "simple_name")?;
        let owner = node.parent()?.named_child(0)?;
        let constructor = name
            .utf8_text(source)
            .ok()?
            .eq_ignore_ascii_case(owner.utf8_text(source).ok()?);
        return Some(if constructor {
            "powershell.constructor"
        } else {
            "powershell.method"
        });
    }
    if node.kind() == "variable" {
        return Some("powershell.variable");
    }
    canonical_syntax(node.kind())
}

pub(super) fn header_end(node: Node<'_>) -> usize {
    let mut cursor = node.walk();
    let brace = node.children(&mut cursor).find(|child| child.kind() == "{");
    let mut end = brace.map_or(node.end_byte(), |brace| {
        if node.kind() == "script_block_expression" {
            brace.end_byte()
        } else {
            brace.start_byte()
        }
    });
    if matches!(
        node.kind(),
        "function_statement" | "script_block_expression"
    ) {
        let mut cursor = node.walk();
        let parameters = node
            .named_children(&mut cursor)
            .find_map(|child| match child.kind() {
                "param_block" => Some(child),
                "script_block" => {
                    let mut cursor = child.walk();
                    child
                        .named_children(&mut cursor)
                        .find(|child| child.kind() == "param_block")
                }
                _ => None,
            });
        if let Some(parameters) = parameters {
            // A param block belongs to the signature even though it is inside braces.
            end = parameters.end_byte();
        }
    }
    end
}

pub(super) fn canonical_syntax(native: &str) -> Option<&'static str> {
    Some(match native {
        "program" => "powershell.file",
        "function_statement" => "powershell.function",
        "class_statement" => "powershell.class",
        "class_method_definition" => "powershell.method",
        "class_property_definition" => "powershell.field",
        "enum_statement" => "powershell.enum",
        "enum_member" => "powershell.constant",
        "script_parameter" | "class_method_parameter" => "powershell.parameter",
        "simple_name" | "function_name" | "variable" => "powershell.identifier",
        "type_name" => "powershell.type_name",
        "command_name" => "powershell.command_name",
        "script_block_expression" => "powershell.script_block",
        "hash_literal_expression" => "powershell.hashtable",
        "hash_entry" => "powershell.property",
        "key_expression" => "powershell.key",
        "data_statement" => "powershell.data",
        "comment" => "powershell.comment",
        "string_literal" => "powershell.string",
        _ => return None,
    })
}
