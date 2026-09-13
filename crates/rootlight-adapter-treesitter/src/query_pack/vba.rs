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
    // Keep application evidence even when its named procedure can be bound:
    // argument compatibility and dispatch through returned values are separate.
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
        "preprocessor_if" | "type_preprocessor_if" => "vba.preprocessor",
        "identifier" | "bang_identifier" => "vba.identifier",
        "qualified_member_expression" | "implicit_member_expression" => "vba.member",
        "call_expression" | "call_statement" if role == StructuralRole::Reference => {
            "vba.application"
        }
        "comment" => "vba.comment",
        "string_literal" => "vba.string",
        _ => return None,
    })
}

pub(super) fn reference_syntax(
    mut child: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let invoked = child.parent().is_some_and(|parent| {
        (parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(child))
            || (parent.kind() == "call_statement"
                && parent.child_by_field_name("callee") == Some(child))
    });
    while let Some(parent) = child.parent() {
        cancellation.check()?;
        let kind = parent.kind();
        if matches!(
            kind,
            "type_expression"
                | "dotted_type_expression"
                | "implements_statement"
                | "new_expression"
                | "addressof_expression"
                | "preprocessor_const"
                | "goto_statement"
                | "gosub_statement"
                | "on_error_statement"
                | "resume_statement"
        ) || (kind == "named_argument" && parent.child_by_field_name("name") == Some(child))
            || (kind == "on_goto_statement"
                && parent.child_by_field_name("selector") != Some(child))
            || (matches!(
                kind,
                "preprocessor_if"
                    | "preprocessor_elseif"
                    | "type_preprocessor_if"
                    | "type_preprocessor_elseif"
            ) && parent.child_by_field_name("condition") == Some(child))
            || (kind == "raise_event_statement"
                && parent.child_by_field_name("event") == Some(child))
        {
            return Ok("vba.nonvalue_identifier");
        }
        if kind == "parameter" {
            return Ok("vba.default_constant_identifier");
        }
        if kind.starts_with("conditional_")
            && declaration_syntax(kind).is_some()
            && !matches!(child.kind(), "block" | "conditional_branch_body")
        {
            return Ok("vba.nonvalue_identifier");
        }
        if declaration_syntax(kind).is_some() || kind == "source_file" {
            break;
        }
        child = parent;
    }
    Ok(if invoked {
        "vba.invoked_identifier"
    } else {
        "vba.identifier"
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
