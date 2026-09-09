//! Binding classification over native ECMAScript pattern edges.
//! Identical patterns occur in declarations and assignments; only ancestry
//! through the declared binding fields may introduce a source definition.

use super::{AdapterError, Cancellation, StructuralRole};
use tree_sitter::Node;

#[derive(Clone, Copy)]
pub(super) enum BindingKind {
    Parameter,
    Variable,
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if role == StructuralRole::Declaration && node.kind() == "variable_declarator" {
        // Destructuring has one declaration per binding, not one unnamed
        // declaration for the entire pattern. Simple declarators keep their span.
        return Ok(node
            .child_by_field_name("name")
            .is_some_and(|name| name.kind() == "identifier"));
    }
    if !matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) || !matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) || !node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "required_parameter"
                | "optional_parameter"
                | "arrow_function"
                | "array_pattern"
                | "object_pattern"
                | "pair_pattern"
                | "assignment_pattern"
                | "object_assignment_pattern"
                | "rest_pattern"
                | "variable_declarator"
                | "for_in_statement"
                | "catch_clause"
        )
    }) {
        return Ok(true);
    }
    Ok(binding_kind(node, cancellation)?.is_some())
}

pub(super) fn binding_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<BindingKind>, AdapterError> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "required_parameter" | "optional_parameter" => {
                return Ok((parent.child_by_field_name("pattern") == Some(current)
                    || parent.child_by_field_name("name") == Some(current))
                .then_some(BindingKind::Parameter));
            }
            "arrow_function" => {
                return Ok((parent.child_by_field_name("parameter") == Some(current))
                    .then_some(BindingKind::Parameter));
            }
            "variable_declarator" => {
                return Ok((parent.child_by_field_name("name") == Some(current))
                    .then_some(BindingKind::Variable));
            }
            "catch_clause" => {
                return Ok((parent.child_by_field_name("parameter") == Some(current))
                    .then_some(BindingKind::Variable));
            }
            "for_in_statement" => {
                return Ok((parent.child_by_field_name("left") == Some(current)
                    && parent.child_by_field_name("kind").is_some())
                .then_some(BindingKind::Variable));
            }
            "pair_pattern" if parent.child_by_field_name("value") == Some(current) => {}
            "assignment_pattern" | "object_assignment_pattern"
                if parent.child_by_field_name("left") == Some(current) => {}
            "array_pattern" | "object_pattern" | "rest_pattern" => {}
            _ => return Ok(None),
        }
        current = parent;
    }
    Ok(None)
}
