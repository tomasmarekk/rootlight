//! Perl declaration positions, sigil-preserving names and callable headers.
//! Binding candidates follow native declaration fields, not arbitrary descendants.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn variable_kind(node: Node<'_>) -> bool {
    matches!(node.kind(), "scalar" | "array" | "hash")
}

fn binding(
    mut node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        match parent.kind() {
            "mandatory_parameter"
            | "optional_parameter"
            | "named_parameter"
            | "slurpy_parameter" => {
                return Ok((parent.named_child(0) == Some(node)).then_some("perl.parameter"));
            }
            "variable_declaration" => {
                return Ok(Some(
                    if parent.child(0).is_some_and(|child| child.kind() == "field") {
                        "perl.field"
                    } else {
                        "perl.variable"
                    },
                ));
            }
            "variable_group" | "refalias_variable" => node = parent,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !variable_kind(node) {
        return Ok(true);
    }
    let is_binding = binding(node, cancellation)?.is_some();
    Ok(match role {
        StructuralRole::Declaration | StructuralRole::Definition => is_binding,
        StructuralRole::Reference => !is_binding,
        _ => true,
    })
}

pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if role == StructuralRole::Declaration && variable_kind(node) {
        return binding(node, cancellation);
    }
    Ok(Some(match node.kind() {
        "source_file" => "perl.file",
        "package"
            if role == StructuralRole::Reference
                && node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "package_statement" | "class_statement" | "role_statement"
                    )
                }) =>
        {
            "perl.package_context"
        }
        "package_statement" => "perl.package",
        "class_statement" => "perl.class",
        "role_statement" => "perl.trait",
        "subroutine_declaration_statement" => "perl.function",
        "method_declaration_statement" => "perl.method",
        "mandatory_parameter" | "optional_parameter" | "named_parameter" | "slurpy_parameter" => {
            "perl.parameter"
        }
        "bareword" | "package" => "perl.identifier",
        "scalar" | "array" | "hash" => "perl.variable_name",
        "function" => "perl.function_name",
        "method_call_expression" => "perl.method_application",
        "block" => "perl.block",
        "comment" => "perl.comment",
        "pod" => "perl.documentation",
        "string_literal" | "interpolated_string_literal" | "command_string" => "perl.string",
        _ => return Ok(None),
    }))
}

pub(super) fn header_end(node: Node<'_>) -> usize {
    node.child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte())
}
