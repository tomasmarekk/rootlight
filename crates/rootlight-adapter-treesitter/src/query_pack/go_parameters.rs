//! Keep written Go formals under their native named function or method owner.
//! Each grouped name gets its own entity; receiver, result, function-type and
//! closure parameter lists cannot borrow that named callable's body.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

pub(super) fn is_parameter_capture(node: Node<'_>, role: StructuralRole) -> bool {
    matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) && node.kind() == "identifier"
        && node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "parameter_declaration" | "variadic_parameter_declaration"
            )
        })
}

/// Keep only named input-formal fields of a clean named callable with a body.
/// The query captures repeated `name` fields separately; Go types use aliased
/// `type_identifier` nodes, so an unnamed type cannot become a formal name.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    if !is_parameter_capture(node, role)
        || node.is_missing()
        || node.has_error()
        || source
            .get(node.byte_range())
            .is_none_or(|name| name == b"_")
    {
        return Ok(None);
    }
    let Some(parameter) = node
        .parent()
        .filter(|node| !node.has_error() && !node.is_missing())
    else {
        return Ok(None);
    };
    let Some(parameters) = parameter
        .parent()
        .filter(|node| node.kind() == "parameter_list")
    else {
        return Ok(None);
    };
    let Some(_) = parameters.parent().filter(|node| {
        matches!(node.kind(), "function_declaration" | "method_declaration")
            && node.child_by_field_name("parameters") == Some(parameters)
            && node
                .child_by_field_name("name")
                .is_some_and(|name| !name.is_missing())
            && node
                .child_by_field_name("body")
                .is_some_and(|body| body.kind() == "block" && !body.is_missing())
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "source_file")
            && !node.has_error()
            && !node.is_missing()
    }) else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax: if role == StructuralRole::Declaration {
            "go.parameter"
        } else {
            "go.identifier"
        },
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn at<'tree>(tree: &'tree tree_sitter::Tree, source: &[u8], name: &[u8]) -> Node<'tree> {
        let start = source
            .windows(name.len())
            .position(|part| part == name)
            .unwrap();
        tree.root_node()
            .descendant_for_byte_range(start, start + name.len())
            .unwrap()
    }

    #[test]
    fn grouped_written_names_have_separate_exact_native_capture_spans() {
        let source = b"package sample\nfunc owned(first, second int, rest ...string) {}\n";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        for name in [b"first".as_slice(), b"second", b"rest"] {
            let node = at(&tree, source, name);
            for role in [StructuralRole::Declaration, StructuralRole::Definition] {
                let capture = candidate(node, role, source, &Cancellation::new())
                    .unwrap()
                    .unwrap();
                assert_eq!(&source[capture.start..capture.end], name);
                assert!(capture.required);
                assert_eq!(
                    capture.syntax,
                    if role == StructuralRole::Declaration {
                        "go.parameter"
                    } else {
                        "go.identifier"
                    }
                );
            }
        }
    }

    #[test]
    fn result_receiver_function_type_prototype_and_closure_names_cannot_borrow_an_owner() {
        for (source, name) in [
            (
                "package sample\ntype Receiver struct{}\nfunc (receiver Receiver) owned() {}\n",
                "receiver",
            ),
            (
                "package sample\nfunc owned() (result int) { return }\n",
                "result",
            ),
            (
                "package sample\nfunc owned(callback func(inner int)) {}\n",
                "inner",
            ),
            (
                "package sample\nfunc external(prototype int)\n",
                "prototype",
            ),
            (
                "package sample\nfunc owned(actual int) { _ = func(closure int) {} }\n",
                "closure",
            ),
            ("package sample\nfunc owned(_ int) {}\n", "_"),
            (
                "package sample\nfunc broken(recovered int,, other int) {}\n",
                "recovered",
            ),
        ] {
            let tree = parse(source.as_bytes());
            let node = at(&tree, source.as_bytes(), name.as_bytes());
            assert!(
                candidate(
                    node,
                    StructuralRole::Declaration,
                    source.as_bytes(),
                    &Cancellation::new()
                )
                .unwrap()
                .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn formal_capture_propagates_typed_cancellation() {
        let source = b"package sample\nfunc owned(value int) {}\n";
        let tree = parse(source);
        let node = at(&tree, source, b"value");
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            candidate(node, StructuralRole::Declaration, source, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
