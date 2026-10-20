//! Admit native bare function and const/var binders with physical source owners.
//! Branch-local written variables remain visible without selecting a runtime branch.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn bare(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "identifier"
        && !node.is_missing()
        && !node.has_error()
        && text(node, source).is_some_and(|raw| {
            structural_captured_name_for_language("zig", raw, raw.len()).is_some()
        })
}

fn source_owner(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut current = node.parent();
    while let Some(parent) = current {
        cancellation.check()?;
        if parent.is_missing()
            || matches!(
                parent.kind(),
                "ERROR"
                    | "struct_declaration"
                    | "enum_declaration"
                    | "union_declaration"
                    | "opaque_declaration"
                    | "comptime_declaration"
                    | "comptime_expression"
                    | "comptime_statement"
                    | "test_declaration"
            )
        {
            return Ok(false);
        }
        if parent.kind() == "function_declaration" {
            return Ok(!parent.has_error()
                && parent
                    .child_by_field_name("name")
                    .is_some_and(|name| bare(name, source))
                && parent
                    .parent()
                    .is_some_and(|owner| owner.kind() == "source_file"));
        }
        if parent.kind() == "source_file" {
            return Ok(true);
        }
        current = parent.parent();
    }
    Ok(false)
}

fn declaration<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<(&'static str, Node<'tree>)>, AdapterError> {
    cancellation.check()?;
    if node.is_missing() || node.has_error() || !source_owner(node, source, cancellation)? {
        return Ok(None);
    }
    if node.kind() == "function_declaration" {
        if !node
            .parent()
            .is_some_and(|parent| parent.kind() == "source_file")
        {
            return Ok(None);
        }
        return Ok(node
            .child_by_field_name("name")
            .filter(|name| bare(*name, source))
            .map(|name| ("zig.function", name)));
    }
    if node.kind() != "variable_declaration" {
        return Ok(None);
    }
    let mut binder = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if let Some(kind) = binder {
            // The grammar aliases assignment statements to this node kind too.
            // A direct const/var token followed by its direct name is mandatory.
            return Ok(bare(child, source).then_some((kind, child)));
        }
        binder = match child.kind() {
            "const" => Some("zig.constant"),
            "var" => Some("zig.variable"),
            _ => None,
        };
    }
    Ok(None)
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
            node.kind() == "source_file"
                || node.kind() == "function_declaration"
                    && declaration(node, source, cancellation)?.is_some()
        }
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature => {
            declaration(node, source, cancellation)?.is_some()
        }
        StructuralRole::Expression => declaration(node, source, cancellation)?.is_none(),
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
    let syntax = if node.kind() == "source_file" {
        range = 0..source.len();
        "zig.file"
    } else {
        match role {
            StructuralRole::Definition => {
                range = declaration(node, source, cancellation)?
                    .ok_or_else(|| query_failure("query-zig-binder"))?
                    .1
                    .byte_range();
                "zig.name"
            }
            StructuralRole::Declaration | StructuralRole::Scope => {
                declaration(node, source, cancellation)?
                    .ok_or_else(|| query_failure("query-zig-owner"))?
                    .0
            }
            StructuralRole::Signature => {
                if let Some(body) = node.child_by_field_name("body") {
                    range.end = body.start_byte();
                }
                "zig.header"
            }
            StructuralRole::Expression => "zig.unsupported",
            StructuralRole::Comment => "zig.comment",
            StructuralRole::StringLiteral => "zig.string",
            _ => return Err(query_failure("query-zig-role")),
        }
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() == "function_declaration" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-zig-depth"))?;
    }
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        syntax,
        role,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn native_binder_proofs_distinguish_address_uses_and_observe_cancellation() {
        let source = include_bytes!("../../tests/fixtures/zig/pipe.zig");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_zig::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let cancellation = Cancellation::new();
        let name = tree
            .root_node()
            .named_descendant_for_byte_range(452, 456)
            .unwrap();
        let binder = name.parent().unwrap();
        let (kind, proved) = declaration(binder, source, &cancellation).unwrap().unwrap();
        assert_eq!(kind, "zig.variable");
        assert_eq!(proved.byte_range(), 452..456);
        let address = tree
            .root_node()
            .named_descendant_for_byte_range(586, 590)
            .unwrap();
        assert!(
            declaration(address.parent().unwrap(), source, &cancellation)
                .unwrap()
                .is_none()
        );
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            retain_capture(
                tree.root_node(),
                StructuralRole::Root,
                source,
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            candidate(binder, StructuralRole::Definition, source, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
