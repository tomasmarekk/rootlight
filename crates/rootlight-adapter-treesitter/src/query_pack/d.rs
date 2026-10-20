//! D declaration names come only from admitted native headers.
//! Conditional, aggregate, template-generated and nested scopes remain explicit gaps.
use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

fn direct<'t>(
    node: Node<'t>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'t>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}
fn function_visible(node: Node<'_>) -> bool {
    node.kind() == "function_declaration"
        && !node.has_error()
        && !node.is_missing()
        && node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "source_file" | "module_def"))
}
fn key<'t>(node: Node<'t>, cancellation: &Cancellation) -> Result<Option<Node<'t>>, AdapterError> {
    cancellation.check()?;
    if node.is_missing() || node.has_error() {
        return Ok(None);
    }
    match node.kind() {
        "module_def" => {
            let Some(declaration) = direct(node, "module_declaration", cancellation)? else {
                return Ok(None);
            };
            direct(declaration, "module_fqn", cancellation)
        }
        "function_declaration" if function_visible(node) => {
            direct(node, "identifier", cancellation)
        }
        "parameter" => {
            let Some(parameters) = node.parent().filter(|p| p.kind() == "parameters") else {
                return Ok(None);
            };
            if !parameters.parent().is_some_and(function_visible) {
                return Ok(None);
            }
            let Some(ty) = direct(node, "type", cancellation)? else {
                return Ok(None);
            };
            // The next physical syntax child must be the binder: an unnamed default's RHS is not a name.
            Ok(ty.next_sibling().filter(|n| n.kind() == "identifier"))
        }
        _ => Ok(None),
    }
}
fn admitted(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(name) = key(node, cancellation)? else {
        return Ok(false);
    };
    let Some(text) = source
        .get(name.byte_range())
        .and_then(|b| std::str::from_utf8(b).ok())
    else {
        return Ok(false);
    };
    if node.kind() == "module_def" {
        Ok(text
            .split('.')
            .all(|part| structural_captured_name_for_language("d", part, part.len()).is_some()))
    } else {
        Ok(structural_captured_name_for_language("d", text, text.len()).is_some())
    }
}
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    Ok(match role {
        StructuralRole::Root | StructuralRole::Module => node.kind() == "source_file",
        StructuralRole::Scope => {
            node.kind() == "source_file" || admitted(node, source, cancellation)?
        }
        StructuralRole::Declaration | StructuralRole::Definition => {
            admitted(node, source, cancellation)?
        }
        StructuralRole::Signature => {
            function_visible(node) && admitted(node, source, cancellation)?
        }
        StructuralRole::Expression => {
            !matches!(
                node.kind(),
                "module_def" | "function_declaration" | "parameter"
            ) || !admitted(node, source, cancellation)?
        }
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
        StructuralRole::Root | StructuralRole::Module => {
            range = 0..source.len();
            "d.file"
        }
        StructuralRole::Scope => "d.scope",
        StructuralRole::Declaration | StructuralRole::Definition => {
            let name = key(node, cancellation)?
                .ok_or_else(|| query_failure("query-d-header-name-proof"))?;
            range.start = name.start_byte();
            if role == StructuralRole::Definition {
                range.end = name.end_byte();
                if node.kind() == "module_def" {
                    "d.module.name"
                } else {
                    "d.name"
                }
            } else {
                match node.kind() {
                    "module_def" => "d.module",
                    "function_declaration" => "d.function",
                    "parameter" => "d.parameter",
                    _ => return Err(query_failure("query-d-declaration")),
                }
            }
        }
        StructuralRole::Signature => {
            let parameters = direct(node, "parameters", cancellation)?
                .ok_or_else(|| query_failure("query-d-parameters"))?;
            range.end = parameters.end_byte();
            "d.header"
        }
        StructuralRole::Expression => "d.unsupported",
        StructuralRole::Comment => "d.comment",
        StructuralRole::StringLiteral => "d.string",
        _ => return Err(query_failure("query-d-role")),
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
    fn native_header_selection_does_not_promote_unnamed_default_values() {
        let source = b"module sample; void measure(int = fallback, int named = value) {}";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_d::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.walk();
        let mut names = Vec::new();
        loop {
            let node = cursor.node();
            if node.kind() == "parameter"
                && let Some(n) = key(node, &Cancellation::new()).unwrap()
            {
                names.push(&source[n.byte_range()]);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    assert_eq!(names, vec![b"named".as_slice()]);
                    return;
                }
            }
        }
    }
    #[test]
    fn native_header_walk_keeps_cancellation() {
        let source = b"module sample; void measure(int value) {}";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_d::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancellation = Cancellation::new();
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
