//! Admit only native interface owners and their direct, unqualified method names.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

fn ordinary_name(node: Node<'_>, source: &[u8]) -> bool {
    let Some(raw) = source.get(node.byte_range()).filter(|raw| raw.len() <= 256) else {
        return false;
    };
    node.kind() == "identifier"
        && raw
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && raw.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

fn named_owner(node: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let mut names = node.children_by_field_name("name", &mut cursor);
    names.next().is_some_and(|name| ordinary_name(name, source)) && names.next().is_none()
}

fn direct_method(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "method_entry"
        && named_owner(node, source)
        && node.parent().is_some_and(|declaration| {
            declaration.kind() == "methods_declaration"
                && declaration.child_by_field_name("kind").is_some_and(|kind| {
                    source
                        .get(kind.byte_range())
                        .is_some_and(|raw| raw.eq_ignore_ascii_case(b"methods"))
                })
                && declaration.parent().is_some_and(|owner| {
                    owner.kind() == "interface_definition" && named_owner(owner, source)
                })
        })
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    root: Node<'_>,
    complete: bool,
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    let valid = complete && !root.has_error() && !node.is_missing();
    let mut span = node.byte_range();
    let syntax = match (role, node.kind()) {
        (StructuralRole::Root, "source_file") => "abap.file",
        (StructuralRole::Signature, "source_file") if !valid => "abap.invalid_file",
        (StructuralRole::Module, "source_file") if valid => "abap.file",
        _ if !valid => return Ok(None),
        (StructuralRole::Scope | StructuralRole::Declaration, "interface_definition")
            if named_owner(node, source) =>
        {
            "abap.interface"
        }
        (StructuralRole::Declaration, "method_entry") if direct_method(node, source) => {
            "abap.method"
        }
        (StructuralRole::Definition, "identifier") => match node.parent() {
            Some(owner)
                if (owner.kind() == "interface_definition" && named_owner(owner, source))
                    || direct_method(owner, source) =>
            {
                "abap.name"
            }
            _ => return Ok(None),
        },
        (StructuralRole::Signature, "method_entry") if direct_method(node, source) => {
            let Some(name) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            span.start = name.end_byte();
            if !source
                .get(span.clone())
                .is_some_and(|raw| raw.iter().any(|b| !b.is_ascii_whitespace()))
            {
                return Ok(None);
            }
            "abap.method_contract"
        }
        (StructuralRole::Signature, "source_file") => return Ok(None),
        (StructuralRole::Signature, "interface_definition") if named_owner(node, source) => {
            return Ok(None);
        }
        (StructuralRole::Signature, "statement") => "abap.unmodeled_statement",
        (StructuralRole::Signature, _) => "abap.unmodeled_structure",
        (StructuralRole::Comment, "line_comment" | "star_comment") => "abap.comment",
        _ => return Ok(None),
    };
    Ok(Some(QueryCandidate {
        start: span.start,
        end: span.end,
        role,
        syntax,
        required: role != StructuralRole::Comment,
        native_depth: native_depth(node, cancel)?,
    }))
}
