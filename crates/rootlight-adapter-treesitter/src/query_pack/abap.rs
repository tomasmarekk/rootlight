//! Admit complete native owners and direct declarations without implementation inference.

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

fn declaration_owner(node: Node<'_>, source: &[u8]) -> bool {
    matches!(node.kind(), "interface_definition" | "class_definition") && named_owner(node, source)
}

fn declaration_kind(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    if node.kind() != "methods_declaration" {
        return None;
    }
    let owner = node
        .parent()
        .filter(|owner| declaration_owner(*owner, source))?;
    let kind = node.child_by_field_name("kind")?;
    let raw = source.get(kind.byte_range())?;
    if raw.eq_ignore_ascii_case(b"methods") {
        Some("abap.method")
    } else if owner.kind() == "class_definition" && raw.eq_ignore_ascii_case(b"class-methods") {
        Some("abap.static_method")
    } else {
        None
    }
}

fn method_kind(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    if node.kind() != "method_entry" || !named_owner(node, source) {
        return None;
    }
    declaration_kind(node.parent()?, source)
}

fn class_header(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<std::ops::Range<usize>>, AdapterError> {
    let mut start = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancel.check()?;
        if child.kind() == "definition" {
            start = Some(child.end_byte());
        } else if child.kind() == "." {
            let Some(start) = start else {
                return Ok(Some(node.byte_range()));
            };
            let span = start..child.start_byte();
            return Ok(source
                .get(span.clone())
                .filter(|raw| raw.iter().any(|b| !b.is_ascii_whitespace()))
                .map(|_| span));
        }
    }
    Ok(Some(node.byte_range()))
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
        (StructuralRole::Scope | StructuralRole::Declaration, "class_definition")
            if named_owner(node, source) =>
        {
            "abap.class"
        }
        (StructuralRole::Declaration, "method_entry") => {
            let Some(kind) = method_kind(node, source) else {
                return Ok(None);
            };
            kind
        }
        (StructuralRole::Definition, "identifier") => match node.parent() {
            Some(owner)
                if declaration_owner(owner, source) || method_kind(owner, source).is_some() =>
            {
                "abap.name"
            }
            _ => return Ok(None),
        },
        (StructuralRole::Signature, "method_entry") if method_kind(node, source).is_some() => {
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
        (StructuralRole::Signature, "methods_declaration")
            if declaration_kind(node, source) == Some("abap.static_method") =>
        {
            let Some(kind) = node.child_by_field_name("kind") else {
                return Ok(None);
            };
            span = kind.byte_range();
            "abap.static_dispatch"
        }
        (StructuralRole::Signature, "methods_declaration") => return Ok(None),
        (StructuralRole::Signature, "class_definition") if named_owner(node, source) => {
            let Some(header) = class_header(node, source, cancel)? else {
                return Ok(None);
            };
            span = header;
            "abap.class_header"
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
