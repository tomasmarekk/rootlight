//! Native host nodes establish written names; opaque code never donates bindings.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

fn contains_markup(raw: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    for chunk in raw.chunks(1024) {
        cancel.check()?;
        if chunk.contains(&b'<') {
            return Ok(true);
        }
    }
    Ok(false)
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
        (StructuralRole::Root, "template") => "vento.file",
        (StructuralRole::Signature, "template") if !valid => "vento.invalid_file",
        (StructuralRole::Module, "template") if valid => "vento.file",
        _ if !valid => return Ok(None),
        (StructuralRole::Scope | StructuralRole::Declaration, "function_block") => "vento.function",
        (StructuralRole::Declaration, "parameter") => "vento.parameter",
        (StructuralRole::Declaration, "set_tag" | "set_block") => "vento.variable",
        (StructuralRole::Definition, "identifier") => match node.parent().map(|n| n.kind()) {
            Some("function_tag_start" | "parameter" | "set_tag" | "set_tag_start") => "vento.name",
            _ => return Ok(None),
        },
        (StructuralRole::Reference, "code")
            if node.parent().is_some_and(|p| p.kind() == "filter") =>
        {
            let Some(raw) = source.get(span.clone()).filter(|raw| raw.len() <= 256) else {
                return Ok(None);
            };
            let leading = raw.iter().take_while(|b| b.is_ascii_whitespace()).count();
            let trailing = raw
                .iter()
                .rev()
                .take_while(|b| b.is_ascii_whitespace())
                .count();
            let Some(name) = raw.get(leading..raw.len().saturating_sub(trailing)) else {
                return Ok(None);
            };
            if !name
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
                || !name
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
            {
                return Ok(None);
            }
            span = span.start + leading..span.end - trailing;
            "vento.filter_name"
        }
        (StructuralRole::Signature, "code") => "vento.javascript",
        (StructuralRole::Signature, "filter") => "vento.filter",
        (StructuralRole::Signature, "content") => {
            let Some(raw) = source.get(span.clone()) else {
                return Ok(None);
            };
            if !contains_markup(raw, cancel)? {
                return Ok(None);
            }
            "vento.output_text"
        }
        (StructuralRole::Signature, "template") => return Ok(None),
        (StructuralRole::Signature, _) => "vento.unmodeled_host",
        (StructuralRole::Comment, "comment") => "vento.comment",
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
