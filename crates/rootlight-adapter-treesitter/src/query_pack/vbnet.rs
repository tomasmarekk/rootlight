//! Admit complete native VB.NET owners and their direct method/formal names.
//! Recovery, complex names and unmodeled body semantics retain explicit gaps.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

pub(super) fn preprocessing_context(
    root: Node<'_>,
    maximum_nodes: usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = root.walk();
    let mut remaining = maximum_nodes;
    loop {
        cancel.check()?;
        if remaining == 0 || cursor.node().kind() == "preprocessor_directive" {
            return Ok(false);
        }
        remaining -= 1;
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

fn ordinary_name(node: Node<'_>, source: &[u8]) -> bool {
    let Some(raw) = source.get(node.byte_range()).filter(|raw| raw.len() <= 256) else {
        return false;
    };
    matches!(node.kind(), "identifier" | "namespace_name")
        && raw
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && raw.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

fn named(node: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let mut names = node.children_by_field_name("name", &mut cursor);
    names.next().is_some_and(|name| ordinary_name(name, source)) && names.next().is_none()
}

fn owner_kind(node: Node<'_>) -> Option<&'static str> {
    Some(match node.kind() {
        "namespace_block" => "vbnet.namespace",
        "class_block" => "vbnet.class",
        "module_block" => "vbnet.named_module",
        "structure_block" => "vbnet.struct",
        "interface_block" => "vbnet.interface",
        "enum_block" => "vbnet.enum",
        _ => return None,
    })
}

fn owner_chain(node: Node<'_>, source: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    let mut next = Some(node);
    while let Some(owner) = next {
        cancel.check()?;
        match owner.kind() {
            "source_file" => return Ok(owner.parent().is_none()),
            "type_declaration" => {}
            _ if owner_kind(owner).is_some() && named(owner, source) => {}
            _ => return Ok(false),
        }
        next = owner.parent();
    }
    Ok(false)
}

fn method(node: Node<'_>, source: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    if node.kind() != "method_declaration" || !named(node, source) {
        return Ok(false);
    }
    let Some(owner) = node.parent() else {
        return Ok(false);
    };
    Ok(matches!(
        owner.kind(),
        "class_block" | "module_block" | "structure_block" | "interface_block"
    ) && owner_chain(owner, source, cancel)?)
}

fn parameter(node: Node<'_>, source: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    if node.kind() != "parameter" || !named(node, source) {
        return Ok(false);
    }
    let Some(list) = node.parent().filter(|p| p.kind() == "parameter_list") else {
        return Ok(false);
    };
    let Some(owner) = list.parent() else {
        return Ok(false);
    };
    method(owner, source, cancel)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    root: Node<'_>,
    complete: bool,
    preprocessing_context: bool,
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    let complete = complete && !root.has_error() && !node.is_missing();
    let valid = complete && preprocessing_context;
    let syntax = match (role, node.kind()) {
        (StructuralRole::Root, "source_file") => "vbnet.file",
        (StructuralRole::Signature, "source_file") if !complete => "vbnet.invalid_file",
        (StructuralRole::Signature, "source_file") if !preprocessing_context => {
            "vbnet.preprocessing_context"
        }
        (StructuralRole::Module, "source_file") if valid => "vbnet.file",
        _ if !valid => return Ok(None),
        (StructuralRole::Scope | StructuralRole::Declaration, _)
            if owner_kind(node).is_some() && owner_chain(node, source, cancel)? =>
        {
            owner_kind(node).unwrap_or("vbnet.unmodeled_structure")
        }
        (StructuralRole::Scope | StructuralRole::Declaration, "method_declaration")
            if method(node, source, cancel)? =>
        {
            "vbnet.method"
        }
        (StructuralRole::Declaration, "parameter") if parameter(node, source, cancel)? => {
            "vbnet.parameter"
        }
        (StructuralRole::Definition, "identifier" | "namespace_name") => {
            let Some(owner) = node.parent() else {
                return Ok(None);
            };
            if (owner_kind(owner).is_some() && owner_chain(owner, source, cancel)?)
                || method(owner, source, cancel)?
                || parameter(owner, source, cancel)?
            {
                "vbnet.name"
            } else {
                return Ok(None);
            }
        }
        (StructuralRole::Signature, "source_file") => return Ok(None),
        (StructuralRole::Signature, "method_declaration") if method(node, source, cancel)? => {
            "vbnet.method_contract"
        }
        (StructuralRole::Signature, "parameter") if parameter(node, source, cancel)? => {
            "vbnet.parameter_contract"
        }
        (StructuralRole::Signature, _)
            if owner_kind(node).is_some() && owner_chain(node, source, cancel)? =>
        {
            "vbnet.type_contract"
        }
        (StructuralRole::Signature, "statement") => "vbnet.statement",
        (StructuralRole::Signature, "imports_statement") => "vbnet.import",
        (StructuralRole::Signature, "preprocessor_directive") => "vbnet.preprocessor",
        (StructuralRole::Signature, _) => "vbnet.unmodeled_structure",
        (StructuralRole::Comment, "comment") => "vbnet.comment",
        _ => return Ok(None),
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role != StructuralRole::Comment,
        native_depth: native_depth(node, cancel)?,
    }))
}
