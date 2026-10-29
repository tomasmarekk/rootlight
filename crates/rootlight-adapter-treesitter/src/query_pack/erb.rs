//! Preserve native ERB host extents without treating literal text as Ruby.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancel: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    cancel.check()?;
    let syntax = match (role, node.kind()) {
        (StructuralRole::Root, "template") => "erb.template",
        (StructuralRole::Module, "template") => "erb.file",
        (StructuralRole::Expression, "directive") => "erb.statement_unavailable",
        (StructuralRole::Expression, "output_directive") => "erb.output_unavailable",
        (StructuralRole::Comment, "comment_directive") => "erb.comment",
        (StructuralRole::StringLiteral, "content") => "erb.literal_content",
        _ => return Err(query_failure("query-erb-host-role")),
    };
    Ok(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancel)?,
    })
}
