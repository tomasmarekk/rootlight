//! Elixir declarations require complete native owners and direct written names.
//! Quoted forms and dynamic declaration heads remain explicit analysis gaps.

use std::ops::Range;

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn text<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn target<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    let target = node.child_by_field_name("target")?;
    (target.kind() == "identifier")
        .then(|| text(target, source))
        .flatten()
}

fn owner<'tree>(node: Node<'tree>, source: &[u8]) -> Option<(&'static str, Node<'tree>)> {
    if node.kind() != "call" || node.has_error() || node.is_missing() {
        return None;
    }
    let keyword = target(node, source)?;
    let arguments = node
        .named_child(1)
        .filter(|child| child.kind() == "arguments")?;
    let head = arguments.named_child(0)?;
    let block = node
        .named_child(2)
        .is_some_and(|child| child.kind() == "do_block");
    let short_body = arguments.named_child_count() == 2
        && arguments
            .named_child(1)
            .filter(|child| child.kind() == "keywords")
            .filter(|child| child.named_child_count() == 1)
            .and_then(|child| child.named_child(0))
            .and_then(|pair| pair.child_by_field_name("key"))
            .and_then(|key| text(key, source))
            // The upstream keyword token includes its mandatory trailing whitespace.
            .map(str::trim_end)
            == Some("do:");
    if !(block && arguments.named_child_count() == 1 || !block && short_body) {
        return None;
    }
    let (syntax, name) = match keyword {
        "defmodule" | "defprotocol" if head.kind() == "alias" => (
            if keyword == "defmodule" {
                "elixir.module"
            } else {
                "elixir.protocol"
            },
            head,
        ),
        "def" | "defp" => {
            let head = if head.kind() == "binary_operator"
                && head
                    .child_by_field_name("operator")
                    .and_then(|operator| text(operator, source))
                    == Some("when")
            {
                head.child_by_field_name("left")?
            } else {
                head
            };
            let name = if head.kind() == "call" {
                let name = head
                    .child_by_field_name("target")
                    .filter(|name| name.kind() == "identifier")?;
                // A spliced head does not supply a written function name.
                // Unquotes inside arguments leave a literal outer target intact.
                if matches!(text(name, source), Some("unquote" | "unquote_splicing")) {
                    return None;
                }
                name
            } else if head.kind() == "identifier" {
                head
            } else {
                return None;
            };
            ("elixir.function", name)
        }
        _ => return None,
    };
    let spelling = text(name, source)?;
    structural_captured_name_for_language("elixir", spelling, spelling.len())?;
    Some((syntax, name))
}

fn visible(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        cancellation.check()?;
        // Only direct written module/protocol chains establish ownership.
        // Other call bodies may be quoted or generated even with qualified targets.
        match ancestor.kind() {
            "source" | "do_block" => {}
            "call"
                if matches!(target(ancestor, source), Some("defmodule" | "defprotocol"))
                    && owner(ancestor, source).is_some() => {}
            _ => return Ok(false),
        }
        parent = ancestor.parent();
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
    if node.kind() == "source" || role == StructuralRole::Signature {
        return Ok(true);
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) {
        return Ok(!node.has_error() && !node.is_missing());
    }
    Ok(owner(node, source).is_some() && visible(node, source, cancellation)?)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    let mut range: Range<usize> = node.byte_range();
    let syntax = if node.kind() == "source" {
        range = 0..source.len();
        "elixir.file"
    } else if role == StructuralRole::Signature {
        let owner = if visible(node, source, cancellation)? {
            owner(node, source)
        } else {
            None
        };
        if owner.is_some() {
            // Clause patterns and guards are part of written signature identity.
            // Stopping at the name conflates distinct same-name clauses.
            range.end = node
                .named_child(2)
                .filter(|child| child.kind() == "do_block")
                .or_else(|| node.named_child(1)?.named_child(1))
                .ok_or_else(|| query_failure("query-elixir-header"))?
                .start_byte();
            "elixir.written_header"
        } else {
            "elixir.construct_analysis_unavailable"
        }
    } else if matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) {
        if role == StructuralRole::Comment {
            "elixir.comment"
        } else {
            "elixir.string"
        }
    } else {
        let (syntax, name) =
            owner(node, source).ok_or_else(|| query_failure("query-elixir-owner"))?;
        if role == StructuralRole::Definition {
            range = name.byte_range();
            "elixir.name"
        } else {
            syntax
        }
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
