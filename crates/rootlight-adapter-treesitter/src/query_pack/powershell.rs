//! PowerShell source declarations and body-independent callable headers.
//! Dynamic commands and member assignments must not masquerade as new bindings.

use tree_sitter::Node;

use super::StructuralRole;

fn assignment_variable(mut node: Node<'_>) -> Option<Node<'_>> {
    while node.kind() != "variable" {
        if node.named_child_count() != 1 {
            return None;
        }
        node = node.named_child(0)?;
    }
    Some(node)
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if node.kind() == "left_assignment_expression" {
        return assignment_variable(node)
            .is_some_and(|variable| variable.byte_range() == node.byte_range());
    }
    if matches!(role, StructuralRole::CallName | StructuralRole::Reference)
        && node.kind() == "command_name"
    {
        return node.named_child_count() == 0;
    }
    true
}

pub(super) fn declaration_syntax(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
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
    let mut end = brace.map_or(node.end_byte(), |brace| brace.start_byte());
    if node.kind() == "function_statement" {
        let mut cursor = node.walk();
        if let Some(block) = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "script_block")
        {
            let mut cursor = block.walk();
            if let Some(parameters) = block
                .named_children(&mut cursor)
                .find(|child| child.kind() == "param_block")
            {
                // PowerShell's param block lives inside braces but belongs to the signature.
                end = parameters.end_byte();
            }
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
        "left_assignment_expression" => "powershell.variable",
        "simple_name" | "function_name" | "variable" => "powershell.identifier",
        "type_name" => "powershell.type_name",
        "command_name" => "powershell.command_name",
        "script_block_expression" => "powershell.script_block",
        "hash_literal_expression" => "powershell.hashtable",
        "data_statement" => "powershell.data",
        "comment" => "powershell.comment",
        "string_literal" => "powershell.string",
        _ => return None,
    })
}
