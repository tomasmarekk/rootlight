//! Native key positions distinguish literal object keys from value expressions.
//! Generated, computed and quoted containers cannot donate static ownership.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn key<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if node.has_error() || node.is_missing() {
        return Ok(None);
    }
    let name = match node.kind() {
        "attribute" => node
            .named_child(0)
            .filter(|name| name.kind() == "identifier"),
        "object_elem" => {
            let Some(expression) = node.child_by_field_name("key") else {
                return Ok(None);
            };
            let Some(variable) = expression.named_child(0).filter(|child| {
                expression.kind() == "expression"
                    && expression.named_child_count() == 1
                    && child.kind() == "variable_expr"
                    && child.named_child_count() == 1
            }) else {
                return Ok(None);
            };
            variable
                .named_child(0)
                .filter(|name| name.kind() == "identifier")
        }
        _ => None,
    };
    Ok(name.filter(|name| {
        source
            .get(name.byte_range())
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .and_then(|text| structural_captured_name_for_language("hcl", text, text.len()))
            .is_some()
    }))
}

fn visible(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.is_missing()
            || matches!(
                owner.kind(),
                "ERROR" | "for_expr" | "template_expr" | "function_call"
            )
        {
            return Ok(false);
        }
        if matches!(owner.kind(), "attribute" | "object_elem")
            && key(owner, source, cancellation)?.is_none()
        {
            return Ok(false);
        }
        if owner.kind() == "block"
            && owner
                .named_child(0)
                .is_some_and(|name| source.get(name.byte_range()) == Some(b"dynamic"))
        {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    Ok(match role {
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => {
            node.kind() == "config_file"
        }
        StructuralRole::Declaration | StructuralRole::Definition => {
            matches!(node.kind(), "attribute" | "object_elem")
                && visible(node, source, cancellation)?
        }
        StructuralRole::Expression => match node.kind() {
            "attribute" | "object_elem" => !visible(node, source, cancellation)?,
            "for_expr" | "template_expr" | "function_call" | "ERROR" => true,
            _ => false,
        },
        StructuralRole::Comment | StructuralRole::StringLiteral => !node.is_missing(),
        _ => false,
    })
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => {
            range = 0..source.len();
            "hcl.file"
        }
        StructuralRole::Declaration | StructuralRole::Definition => {
            let name = key(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-hcl-key-proof"))?;
            range.start = name.start_byte();
            if role == StructuralRole::Definition {
                range.end = name.end_byte();
                "hcl.name"
            } else {
                "hcl.property"
            }
        }
        StructuralRole::Expression => "hcl.unsupported",
        StructuralRole::Comment => "hcl.comment",
        StructuralRole::StringLiteral => "hcl.string",
        _ => return Err(query_failure("query-hcl-role")),
    };
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        syntax,
        role,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn native_key_context_walk_preserves_cancellation() {
        let source = b"stable = { child = 1 }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hcl::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let cancellation = Cancellation::new();
        assert!(
            retain_capture(
                tree.root_node(),
                StructuralRole::Module,
                source,
                &cancellation
            )
            .unwrap()
        );
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            retain_capture(
                tree.root_node(),
                StructuralRole::Module,
                source,
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
