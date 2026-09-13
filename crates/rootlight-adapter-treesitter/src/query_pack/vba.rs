//! VBA authored binding positions and body-independent declaration headers.
//! Dynamic members and call/index syntax remain source references, not proven edges.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn declaration_syntax(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "sub_declaration" => "vba.sub_function",
        "function_declaration" => "vba.function",
        "conditional_sub_declaration" => "vba.conditional_sub_function",
        "conditional_function_declaration" => "vba.conditional_function",
        "conditional_property_declaration" => "vba.conditional_property",
        "declare_sub_statement" => "vba.external_sub_function",
        "declare_function_statement" => "vba.external_function",
        "property_get_declaration" => "vba.get_property",
        "property_let_declaration" => "vba.let_property",
        "property_set_declaration" => "vba.set_property",
        "event_declaration" => "vba.event",
        "type_declaration" => "vba.struct",
        "enum_declaration" => "vba.enum",
        "type_member" => "vba.field",
        "enum_member" => "vba.enum_constant",
        "parameter" => "vba.parameter",
        "variable_declarator" => "vba.variable",
        "const_declarator" => "vba.constant",
        _ => return None,
    })
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if role != StructuralRole::Reference || !matches!(node.kind(), "identifier" | "bang_identifier")
    {
        return true;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
    if parent.kind() == "bang_identifier" {
        return false;
    }
    if declaration_syntax(parent.kind()).is_some()
        && parent.child_by_field_name("name") == Some(node)
    {
        return false;
    }
    !matches!(parent.kind(), "attribute_statement" | "label_statement")
        && !(matches!(
            parent.kind(),
            "qualified_member_expression" | "implicit_member_expression"
        ) && parent.child_by_field_name("member") == Some(node))
}

pub(super) fn syntax(node: Node<'_>, role: StructuralRole) -> Option<&'static str> {
    if let Some(syntax) = declaration_syntax(node.kind()) {
        return Some(syntax);
    }
    Some(match node.kind() {
        "source_file" => "vba.file",
        "identifier" | "bang_identifier" => "vba.identifier",
        "qualified_member_expression" | "implicit_member_expression" => "vba.member",
        "call_expression" if role == StructuralRole::Reference => "vba.application",
        "comment" => "vba.comment",
        "string_literal" => "vba.string",
        _ => return None,
    })
}

pub(super) fn header_end(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        // A named newline terminates the header; escaped physical newlines are
        // grammar extras and must not truncate a continued parameter list.
        if child.kind() == "newline" || node.child_by_field_name("body") == Some(child) {
            return Ok(child.start_byte());
        }
    }
    Ok(node.end_byte())
}
