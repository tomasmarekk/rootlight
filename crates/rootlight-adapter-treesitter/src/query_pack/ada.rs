//! Native Ada declaration owners exclude terminators, subtype uses and aspects.
//! Only direct bare procedure calls become lexical binding candidates.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn bare(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "identifier"
        && !node.is_missing()
        && !node.has_error()
        && source
            .get(node.byte_range())
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .is_some_and(|name| {
                structural_captured_name_for_language("ada", name, name.len()).is_some()
            })
}

fn direct_child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn supported_ancestors(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut current = node.parent();
    while let Some(parent) = current {
        cancellation.check()?;
        if parent.is_missing()
            || matches!(
                parent.kind(),
                "ERROR"
                    | "generic_package_declaration"
                    | "generic_subprogram_declaration"
                    | "task_body"
                    | "protected_body"
                    | "task_definition"
                    | "protected_definition"
            )
        {
            return Ok(false);
        }
        current = parent.parent();
    }
    Ok(true)
}

fn parameter(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !bare(node, source) {
        return Ok(false);
    }
    let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "parameter_specification" && !parent.has_error())
    else {
        return Ok(false);
    };
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == ":" {
            return Ok(false);
        }
        if child == node {
            return Ok(true);
        }
    }
    Ok(false)
}

fn declaration<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<(&'static str, Node<'tree>, Node<'tree>)>, AdapterError> {
    cancellation.check()?;
    if node.is_missing() || node.has_error() || !supported_ancestors(node, cancellation)? {
        return Ok(None);
    }
    if parameter(node, source, cancellation)? {
        return Ok(Some(("ada.parameter", node, node)));
    }
    if matches!(node.kind(), "package_declaration" | "package_body") {
        return Ok(node
            .child_by_field_name("name")
            .filter(|name| bare(*name, source))
            .map(|name| ("ada.package", name, name)));
    }
    if !matches!(
        node.kind(),
        "subprogram_body" | "subprogram_declaration" | "expression_function_declaration"
    ) {
        return Ok(None);
    }
    for (kind, syntax) in [
        ("procedure_specification", "ada.procedure"),
        ("function_specification", "ada.function"),
    ] {
        if let Some(specification) = direct_child(node, kind, cancellation)? {
            return Ok(specification
                .child_by_field_name("name")
                .filter(|name| bare(*name, source))
                .map(|name| (syntax, name, specification)));
        }
    }
    Ok(None)
}

fn direct_call(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !bare(node, source) || !supported_ancestors(node, cancellation)? {
        return Ok(false);
    }
    let Some(call) = node.parent().filter(|parent| {
        parent.kind() == "procedure_call_statement"
            && !parent.has_error()
            && parent.child_by_field_name("name") == Some(node)
    }) else {
        return Ok(false);
    };
    let mut owner = call.parent();
    while let Some(parent) = owner {
        cancellation.check()?;
        if parent.kind() == "subprogram_body" {
            let Some(part) = direct_child(parent, "non_empty_declarative_part", cancellation)?
            else {
                return Ok(false);
            };
            let mut cursor = part.walk();
            for item in part.named_children(&mut cursor) {
                cancellation.check()?;
                if !matches!(
                    item.kind(),
                    "subprogram_declaration" | "subprogram_body" | "comment"
                ) || item.kind() != "comment"
                    && declaration(item, source, cancellation)?.is_none()
                {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if !matches!(
            parent.kind(),
            "sequence_of_statements" | "handled_sequence_of_statements"
        ) {
            return Ok(false);
        }
        owner = parent.parent();
    }
    Ok(false)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    Ok(match role {
        StructuralRole::Root | StructuralRole::Module => node.kind() == "compilation",
        StructuralRole::Scope => {
            node.kind() != "identifier" && declaration(node, source, cancellation)?.is_some()
        }
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature => {
            declaration(node, source, cancellation)?.is_some()
        }
        StructuralRole::Reference => direct_call(node, source, cancellation)?,
        StructuralRole::Expression | StructuralRole::Comment | StructuralRole::StringLiteral => {
            !node.is_missing()
        }
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
            "ada.file"
        }
        StructuralRole::Declaration | StructuralRole::Scope => {
            declaration(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-ada-owner"))?
                .0
        }
        StructuralRole::Definition => {
            range = declaration(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-ada-definition"))?
                .1
                .byte_range();
            "ada.name"
        }
        StructuralRole::Signature => {
            range = declaration(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-ada-signature"))?
                .2
                .byte_range();
            "ada.header"
        }
        StructuralRole::Reference => "ada.direct_call",
        StructuralRole::Expression => "ada.unsupported",
        StructuralRole::Comment => "ada.comment",
        StructuralRole::StringLiteral => "ada.string",
        _ => return Err(query_failure("query-ada-role")),
    };
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn original_nested_procedure_and_parameter_binders_have_exact_native_proof() {
        let source = include_bytes!("../../tests/fixtures/ada/raylib.adb");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ada::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let cancellation = Cancellation::new();
        for (start, end, name) in [
            (13, 19, "Raylib"),
            (38, 57, "Draw_Triangle_Strip"),
            (113, 134, "Draw_Triangle_Strip_C"),
            (58, 64, "Points"),
        ] {
            let leaf = tree
                .root_node()
                .named_descendant_for_byte_range(start, end)
                .unwrap();
            let owner = if start == 58 {
                leaf
            } else if start == 13 {
                leaf.parent().unwrap()
            } else {
                leaf.parent().unwrap().parent().unwrap()
            };
            let proved = declaration(owner, source, &cancellation).unwrap().unwrap();
            assert_eq!(&source[proved.1.byte_range()], name.as_bytes());
            assert_eq!(proved.1.byte_range(), start..end);
        }
        let use_site = tree
            .root_node()
            .named_descendant_for_byte_range(332, 353)
            .unwrap();
        assert!(direct_call(use_site, source, &cancellation).unwrap());
        assert!(
            declaration(use_site, source, &cancellation)
                .unwrap()
                .is_none()
        );
        let type_site = tree
            .root_node()
            .named_descendant_for_byte_range(66, 79)
            .unwrap();
        assert!(!parameter(type_site, source, &cancellation).unwrap());
        let end_name = tree
            .root_node()
            .named_descendant_for_byte_range(398, 404)
            .unwrap();
        assert!(!direct_call(end_name, source, &cancellation).unwrap());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(declaration(use_site, source, &cancellation).is_err());
    }
}
