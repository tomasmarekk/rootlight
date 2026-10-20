//! Prove CUE field names from native labels and written lexical containers.
//! Chained labels acquire nested source ranges; computed contexts are withheld.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone, Copy)]
struct Name {
    start: usize,
    end: usize,
    quoted: bool,
}

fn label_name(
    label: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Name>, AdapterError> {
    cancellation.check()?;
    if label.kind() != "label"
        || label.has_error()
        || label.is_missing()
        || label.child_by_field_name("alias").is_some()
        || label.named_child_count() != 1
    {
        return Ok(None);
    }
    let Some(mut name) = label.named_child(0) else {
        return Ok(None);
    };
    if matches!(name.kind(), "optional" | "required") {
        if name.named_child_count() != 1 {
            return Ok(None);
        }
        let Some(inner) = name.named_child(0) else {
            return Ok(None);
        };
        name = inner;
    }
    if name.has_error() || name.is_missing() {
        return Ok(None);
    }
    if name.kind() == "string" {
        let Some(raw) = source.get(name.byte_range()) else {
            return Ok(None);
        };
        if raw.len() > 2
            && raw.first() == Some(&b'"')
            && raw.last() == Some(&b'"')
            && !raw[1..raw.len() - 1].contains(&b'\\')
            && std::str::from_utf8(&raw[1..raw.len() - 1]).is_ok_and(|text| {
                text.chars()
                    .all(|character| !character.is_control() && character != '"')
            })
        {
            return Ok(Some(Name {
                start: name.start_byte() + 1,
                end: name.end_byte() - 1,
                quoted: true,
            }));
        }
        return Ok(None);
    }
    if !matches!(name.kind(), "identifier" | "keyword_identifier") {
        return Ok(None);
    }
    Ok(source
        .get(name.byte_range())
        .and_then(|raw| std::str::from_utf8(raw).ok())
        .and_then(|text| structural_captured_name_for_language("cue", text, text.len()))
        .map(|_| Name {
            start: name.start_byte(),
            end: name.end_byte(),
            quoted: false,
        }))
}

fn static_field(
    field: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if field.kind() != "field" || field.has_error() || field.is_missing() {
        return Ok(false);
    }
    let mut labels = 0usize;
    let mut cursor = field.walk();
    for child in field.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == "label" {
            labels += 1;
            if label_name(child, source, cancellation)?.is_none() {
                return Ok(false);
            }
        }
    }
    Ok(labels != 0)
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
            || matches!(owner.kind(), "ERROR" | "comprehension" | "function")
            || (owner.kind() == "field" && !static_field(owner, source, cancellation)?)
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
            node.kind() == "source_file"
        }
        StructuralRole::Declaration | StructuralRole::Definition => {
            node.parent().is_some_and(|parent| parent.kind() == "field")
                && visible(node, source, cancellation)?
                && label_name(node, source, cancellation)?.is_some()
        }
        StructuralRole::Expression => match node.kind() {
            "field" => !visible(node, source, cancellation)?,
            "ERROR" | "comprehension" | "function" => true,
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
    let depth = native_depth(node, cancellation)?;
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => {
            range = 0..source.len();
            "cue.file"
        }
        StructuralRole::Declaration | StructuralRole::Definition => {
            let name = label_name(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-cue-static-label"))?;
            range = name.start..name.end;
            if role == StructuralRole::Declaration {
                let field = node
                    .parent()
                    .filter(|parent| parent.kind() == "field")
                    .ok_or_else(|| query_failure("query-cue-field-owner"))?;
                range.end = field.end_byte();
                // One native field may spell a: b: c: value. The suffix ranges
                // prove that a owns b and b owns c without inventing value nodes.
                "cue.field"
            } else if name.quoted {
                "cue.string_name"
            } else {
                "cue.name"
            }
        }
        StructuralRole::Expression => "cue.unsupported",
        StructuralRole::Comment => "cue.comment",
        StructuralRole::StringLiteral => "cue.string",
        _ => return Err(query_failure("query-cue-role")),
    };
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
    fn native_static_label_walk_observes_cancellation() {
        let source = b"package sample\na: b: {c: string}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cue::LANGUAGE.into())
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
