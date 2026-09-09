//! Parameter binding classification over native ECMAScript pattern edges.
//! Pattern captures also occur in assignments; only ancestry through binding
//! fields into a formal parameter may introduce a parameter declaration.

use super::{AdapterError, Cancellation, StructuralRole};
use tree_sitter::Node;

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
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
        )
    }) {
        return Ok(true);
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "required_parameter" | "optional_parameter" => {
                return Ok(parent.child_by_field_name("pattern") == Some(current)
                    || parent.child_by_field_name("name") == Some(current));
            }
            "arrow_function" => return Ok(parent.child_by_field_name("parameter") == Some(current)),
            "pair_pattern" if parent.child_by_field_name("value") == Some(current) => {}
            "assignment_pattern" | "object_assignment_pattern"
                if parent.child_by_field_name("left") == Some(current) => {}
            "array_pattern" | "object_pattern" | "rest_pattern" => {}
            _ => return Ok(false),
        }
        current = parent;
    }
    Ok(false)
}
