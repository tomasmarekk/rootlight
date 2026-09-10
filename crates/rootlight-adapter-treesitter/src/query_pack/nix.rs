//! Native Nix capture selectors keep written bindings separate from evaluated names.
//! Function bodies own parameters; attribute selection is not a lexical identifier read.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn function_value(node: Node<'_>) -> Option<Node<'_>> {
    let mut value = node.child_by_field_name("expression")?;
    while value.kind() == "parenthesized_expression" {
        value = value.child_by_field_name("expression")?;
    }
    (value.kind() == "function_expression").then_some(value)
}

fn is_bound_function(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    while parent.is_some_and(|parent| parent.kind() == "parenthesized_expression") {
        parent = parent.and_then(|parent| parent.parent());
    }
    parent
        .filter(|parent| parent.kind() == "binding")
        .and_then(function_value)
        == Some(node)
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if node.kind() == "function_expression" && is_bound_function(node) {
        return !matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Signature
        );
    }
    if role == StructuralRole::Signature && node.kind() == "binding" {
        return function_value(node).is_some();
    }
    true
}

fn binding_name(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "binding" => node.child_by_field_name("attrpath"),
        "formal" => node.child_by_field_name("name"),
        "identifier" | "string_expression" | "interpolation" => Some(node),
        _ => None,
    }
}

fn static_name(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    loop {
        cancellation.check()?;
        if cursor.node().kind() == "interpolation" {
            return Ok(false);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(true);
            }
        }
    }
}

pub(super) fn definition_node<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(name) = binding_name(node) else {
        return Ok(None);
    };
    Ok(static_name(name, cancellation)?.then_some(name))
}

pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if role == StructuralRole::DefinitionPart {
        return Ok(Some(if static_name(node, cancellation)? {
            "nix.path_segment"
        } else {
            "nix.dynamic_segment"
        }));
    }
    if role == StructuralRole::Definition {
        return Ok(Some("nix.binding_name"));
    }
    if role == StructuralRole::Declaration {
        return Ok(Some(match node.kind() {
            "function_expression" => "nix.anonymous_function",
            "formal" | "identifier"
                if node.kind() == "formal"
                    || node
                        .parent()
                        .is_some_and(|parent| parent.kind() == "function_expression") =>
            {
                "nix.parameter"
            }
            _ if definition_node(node, cancellation)?.is_none() => {
                let static_root = node
                    .child_by_field_name("attrpath")
                    .and_then(|path| path.child_by_field_name("attr"))
                    .map(|root| static_name(root, cancellation))
                    .transpose()?
                    .unwrap_or(false);
                match (
                    static_root,
                    node.kind() == "binding" && function_value(node).is_some(),
                ) {
                    (true, true) => "nix.dynamic_path_function",
                    (true, false) => "nix.dynamic_path_variable",
                    (false, true) => "nix.dynamic_function",
                    (false, false) => "nix.dynamic_variable",
                }
            }
            "binding" if function_value(node).is_some() => "nix.function",
            _ => "nix.variable",
        }));
    }
    Ok(Some(match (role, node.kind()) {
        (StructuralRole::Reference, "identifier" | "string_expression" | "interpolation") => {
            "nix.inherited_name"
        }
        (StructuralRole::Signature, _) => "nix.function_header",
        (StructuralRole::Call, _) => "nix.call",
        (StructuralRole::CallName, _) => "nix.call_name",
        (_, "source_code") => "nix.file",
        (_, "function_expression") => "nix.function",
        (_, "let_expression") => "nix.let",
        (_, "attrset_expression") => "nix.attrset",
        (_, "rec_attrset_expression" | "let_attrset_expression") => "nix.rec_attrset",
        (_, "with_expression") => "nix.with",
        (_, "variable_expression") => "nix.identifier",
        (_, "select_expression") => "nix.member_name",
        (_, "comment") => "nix.comment",
        (_, "string_expression" | "indented_string_expression") => "nix.string",
        (_, "path_expression" | "hpath_expression" | "spath_expression" | "uri_expression") => {
            "nix.path"
        }
        _ => return Ok(None),
    }))
}

pub(super) fn signature_range(node: Node<'_>) -> Option<std::ops::Range<usize>> {
    let function = if node.kind() == "binding" {
        function_value(node)?
    } else {
        node
    };
    Some(function.start_byte()..function.child_by_field_name("body")?.start_byte())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn attribute_name_scan_observes_cancellation_before_visiting_nodes() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_nix::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse("{ name = 1; }", None).unwrap();
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            static_name(tree.root_node(), &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
