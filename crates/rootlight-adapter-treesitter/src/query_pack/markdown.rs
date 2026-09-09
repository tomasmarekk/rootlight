//! Source section extents from written heading levels, not rendered fragments.
//! Native section wrappers do not always reparent Setext headings, so ownership
//! follows heading markers within each document, quote or list-item container.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) fn section_end(
    heading: Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let level = heading_level(heading)
        .ok_or_else(|| super::query_failure("query-markdown-heading-level"))?;
    let mut node = heading;
    loop {
        cancellation.check()?;
        while node.next_named_sibling().is_none() {
            cancellation.check()?;
            let Some(parent) = node.parent() else {
                return Ok(node.end_byte());
            };
            if parent.kind() != "section" {
                return Ok(parent.end_byte());
            }
            node = parent;
        }
        let Some(next) = node.next_named_sibling() else {
            return Err(super::query_failure("query-markdown-sibling"));
        };
        node = next;
        while node.kind() == "section" {
            cancellation.check()?;
            let Some(child) = node.named_child(0) else {
                break;
            };
            node = child;
        }
        if heading_level(node).is_some_and(|next_level| next_level <= level) {
            return Ok(node.start_byte());
        }
        // Skip paragraph and nested block contents. At most six heading levels
        // can scan across a following block; bodies are never rescanned per byte.
    }
}

fn heading_level(node: Node<'_>) -> Option<u8> {
    if !matches!(node.kind(), "atx_heading" | "setext_heading") {
        return None;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| match child.kind() {
            "atx_h1_marker" | "setext_h1_underline" => Some(1),
            "atx_h2_marker" | "setext_h2_underline" => Some(2),
            "atx_h3_marker" => Some(3),
            "atx_h4_marker" => Some(4),
            "atx_h5_marker" => Some(5),
            "atx_h6_marker" => Some(6),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn section_extent_observes_cancellation_before_scanning() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_md::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse("# Heading\n\nBody\n", None).unwrap();
        let heading = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .named_child(0)
            .unwrap();
        assert_eq!(heading.kind(), "atx_heading");
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            section_end(heading, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
