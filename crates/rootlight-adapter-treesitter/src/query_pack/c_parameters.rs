//! Preserve written scalar/pointer names in clean C callable headers.
//! Prototype names retain declaration ownership; only function-definition
//! headers can supply native parameter-read certificates.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

pub(super) fn is_parameter_capture(node: Node<'_>, role: StructuralRole) -> bool {
    (role == StructuralRole::Declaration && node.kind() == "parameter_declaration")
        || (role == StructuralRole::Definition
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "parameter_declaration"))
}

/// Share the same ordinary-definition binder boundary with native read proofs.
/// Unsupported declarators and cancellation retain their existing outcomes.
pub(super) fn written_name<'tree>(
    parameter: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    header_name(parameter, false, cancellation)
}

fn header_name<'tree>(
    parameter: Node<'tree>,
    prototype_declarations: bool,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if parameter.kind() != "parameter_declaration"
        || parameter.is_missing()
        || parameter.has_error()
    {
        return Ok(None);
    }
    let Some(parameters) = parameter
        .parent()
        .filter(|node| node.kind() == "parameter_list")
    else {
        return Ok(None);
    };
    let Some(callable) = parameters.parent().filter(|node| {
        node.kind() == "function_declarator"
            && node.child_by_field_name("parameters") == Some(parameters)
            && !node.has_error()
            && !node.is_missing()
    }) else {
        return Ok(None);
    };
    if callable
        .child_by_field_name("declarator")
        .is_none_or(|node| node.kind() != "identifier")
    {
        return Ok(None);
    }
    let mut header = callable;
    let function = loop {
        cancellation.check()?;
        let Some(parent) = header.parent() else {
            return Ok(None);
        };
        match parent.kind() {
            "pointer_declarator" if parent.child_by_field_name("declarator") == Some(header) => {
                header = parent;
            }
            "function_definition" if parent.child_by_field_name("declarator") == Some(header) => {
                break parent;
            }
            "declaration"
                if prototype_declarations
                    && parent.child_by_field_name("declarator") == Some(header) =>
            {
                let mut declarators = 0usize;
                let mut cursor = parent.walk();
                if cursor.goto_first_child() {
                    loop {
                        cancellation.check()?;
                        declarators += usize::from(cursor.field_name() == Some("declarator"));
                        if declarators > 1 {
                            return Ok(None);
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                if declarators != 1 {
                    return Ok(None);
                }
                break parent;
            }
            _ => return Ok(None),
        }
    };
    if function.is_missing() || function.has_error() {
        return Ok(None);
    }
    let Some(mut name) = parameter.child_by_field_name("declarator") else {
        return Ok(None);
    };
    loop {
        cancellation.check()?;
        if name.is_missing() || name.has_error() {
            return Ok(None);
        }
        match name.kind() {
            "identifier" => return Ok(Some(name)),
            "pointer_declarator" => {
                let Some(inner) = name.child_by_field_name("declarator") else {
                    return Ok(None);
                };
                name = inner;
            }
            _ => return Ok(None),
        }
    }
}

/// Return one existing-budget capture only for a proved ordinary formal header.
/// Cancellation propagates before native ancestry or declarator traversal.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let parameter = if role == StructuralRole::Declaration {
        Some(node)
    } else {
        node.parent()
    };
    let Some(parameter) = parameter else {
        return Ok(None);
    };
    let Some(name) = header_name(parameter, true, cancellation)? else {
        return Ok(None);
    };
    let selected = if role == StructuralRole::Definition {
        name
    } else {
        parameter
    };
    Ok(Some(QueryCandidate {
        start: selected.start_byte(),
        end: selected.end_byte(),
        role,
        syntax: if role == StructuralRole::Definition {
            "c.identifier"
        } else {
            "c.parameter"
        },
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(selected, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter_nodes(tree: &tree_sitter::Tree) -> Vec<Node<'_>> {
        let mut result = Vec::new();
        let mut cursor = tree.root_node().walk();
        loop {
            if cursor.node().kind() == "parameter_declaration" {
                result.push(cursor.node());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return result;
                }
            }
        }
    }

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::C))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        tree
    }

    #[test]
    fn ordinary_prototype_names_are_declarations_without_body_read_certificates() {
        for (source, expected) in [
            ("int written(int value);", "value"),
            ("const unsigned char **load_buffer(int count);", "count"),
            ("void written(const char *text);", "text"),
        ] {
            let tree = parse(source.as_bytes());
            let parameters = parameter_nodes(&tree);
            assert_eq!(parameters.len(), 1);
            let parameter = parameters[0];
            let name = header_name(parameter, true, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(&source[name.byte_range()], expected);
            assert!(
                written_name(parameter, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
            let declaration =
                candidate(parameter, StructuralRole::Declaration, &Cancellation::new())
                    .unwrap()
                    .unwrap();
            assert_eq!(declaration.syntax, "c.parameter");
            assert_eq!(
                (declaration.start, declaration.end),
                (parameter.start_byte(), parameter.end_byte())
            );
            assert!(declaration.required);
            let definition = candidate(
                parameter.child_by_field_name("declarator").unwrap(),
                StructuralRole::Definition,
                &Cancellation::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(definition.syntax, "c.identifier");
            assert_eq!(&source[definition.start..definition.end], expected);
            assert!(definition.required);
        }
        let tree = parse(b"void body(int value) { external(value); }");
        assert!(
            written_name(parameter_nodes(&tree)[0], &Cancellation::new())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn prototype_pointer_variables_nested_binders_and_competing_declarators_stay_closed() {
        for source in [
            "int (*slot)(int nested);",
            "int outer(int (*callback)(int nested));",
            "int first(int value), second(int other);",
            "typedef int callable(int hidden);",
        ] {
            let tree = parse(source.as_bytes());
            let parameters = parameter_nodes(&tree);
            assert!(!parameters.is_empty());
            for parameter in parameters {
                assert!(
                    header_name(parameter, true, &Cancellation::new())
                        .unwrap()
                        .is_none(),
                    "{source}"
                );
                assert!(
                    written_name(parameter, &Cancellation::new())
                        .unwrap()
                        .is_none(),
                    "{source}"
                );
            }
        }
        let tree = parse(b"int written(int value);");
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            header_name(parameter_nodes(&tree)[0], true, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
