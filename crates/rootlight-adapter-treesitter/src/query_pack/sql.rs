//! Native SQL declaration ownership independent of query match ordering.
//! Extra comment nodes and optional clauses must not turn referenced objects or
//! authorization principals into definitions of the enclosing database object.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) fn procedure_header(procedure: Node<'_>) -> Option<Node<'_>> {
    if procedure.kind() != "tsql_procedure" || procedure.is_missing() || procedure.has_error() {
        return None;
    }
    let header = procedure.child_by_field_name("header")?;
    if header.has_error() || header.is_missing() {
        return None;
    }
    let as_keyword = procedure.child_by_field_name("as")?;
    if as_keyword.is_missing() || as_keyword.has_error() {
        return None;
    }
    let body = procedure.child_by_field_name("body")?;
    if body.kind() != "tsql_body" || body.is_missing() || body.has_error() {
        return None;
    }
    // Header tokens and body delimiters must be physical, not recovery insertions.
    if let Some(begin) = body.child_by_field_name("begin") {
        let end = body.child_by_field_name("end")?;
        if begin.is_missing() || end.is_missing() || begin.has_error() || end.has_error() {
            return None;
        }
    }
    Some(header)
}

pub(super) fn recovered_procedure_header<'tree>(
    procedure: Node<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancel.check()?;
    if procedure.kind() != "tsql_procedure" || procedure.is_missing() || !procedure.has_error() {
        return Ok(None);
    }
    let Some(header) = procedure.child_by_field_name("header") else {
        return Ok(None);
    };
    let Some(object) = header.child_by_field_name("object") else {
        return Ok(None);
    };
    let Some(as_keyword) = procedure.child_by_field_name("as") else {
        return Ok(None);
    };
    let Some(body) = procedure.child_by_field_name("body") else {
        return Ok(None);
    };
    if header.kind() != "tsql_procedure_header"
        || header.is_missing()
        || header.has_error()
        || object.is_missing()
        || object.has_error()
        || object.start_byte() >= object.end_byte()
        || as_keyword.kind() != "keyword_as"
        || as_keyword.is_missing()
        || as_keyword.has_error()
        || body.kind() != "tsql_body"
        || body.is_missing()
        || !body.has_error()
        || header.start_byte() != procedure.start_byte()
        || header.end_byte() > as_keyword.start_byte()
        || as_keyword.end_byte() > body.start_byte()
        || body.start_byte() >= body.end_byte()
    {
        return Ok(None);
    }
    let mut cursor = procedure.walk();
    for child in procedure.named_children(&mut cursor) {
        cancel.check()?;
        if child != header
            && child != as_keyword
            && child != body
            && !matches!(child.kind(), "comment" | "marginalia")
        {
            return Ok(None);
        }
    }
    // This proves the written declaration header only. The native body may end
    // at an inner END recovered as the procedure boundary, so it owns no locals.
    Ok(Some(header))
}

pub(super) fn declaration_header<'tree>(
    procedure: Node<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancel.check()?;
    if let Some(header) = procedure_header(procedure) {
        return Ok(Some(header));
    }
    recovered_procedure_header(procedure, cancel)
}

pub(super) fn recovered_body(body: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    let Some(procedure) = body.parent() else {
        return Ok(false);
    };
    Ok(procedure.child_by_field_name("body") == Some(body)
        && recovered_procedure_header(procedure, cancel)?.is_some())
}

pub(super) fn recovered_header_end(
    procedure: Node<'_>,
    cancel: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    if recovered_procedure_header(procedure, cancel)?.is_none() {
        return Ok(None);
    }
    Ok(procedure
        .child_by_field_name("as")
        .map(|keyword| keyword.end_byte()))
}

pub(super) fn declaration_name<'tree>(
    node: Node<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if node.kind() == "tsql_procedure" {
        return Ok(declaration_header(node, cancel)?
            .and_then(|header| header.child_by_field_name("object")));
    }
    cancel.check()?;
    Ok(definition_node(node))
}

pub(super) fn header_signature_range(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<std::ops::Range<usize>>, AdapterError> {
    if node.kind() == "tsql_procedure" {
        let Some(header) = declaration_header(node, cancel)? else {
            return Ok(None);
        };
        return Ok(node
            .child_by_field_name("as")
            .map(|keyword| header.start_byte()..keyword.end_byte()));
    }
    cancel.check()?;
    Ok(signature_range(node, source))
}

pub(super) fn signature_range(node: Node<'_>, source: &[u8]) -> Option<std::ops::Range<usize>> {
    if node.kind() == "tsql_procedure" {
        let header = procedure_header(node)?;
        let as_keyword = node.child_by_field_name("as")?;
        return Some(header.start_byte()..as_keyword.end_byte());
    }
    let body = first_child(node, "function_body")?;
    let start = node.start_byte();
    let prefix = source.get(start..body.start_byte())?;
    // AS/RETURN/BEGIN inside names, comments or defaults are not body boundaries.
    // A contiguous header retains RETURNS and pre-body modifiers; modifiers
    // written after the body cannot be represented by this source span.
    let trailing = prefix
        .iter()
        .rev()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    let end = body.start_byte().checked_sub(trailing)?;
    (start < end).then_some(start..end)
}

pub(super) fn is_declared_name(node: Node<'_>) -> bool {
    node.parent().and_then(definition_node) == Some(node)
}

pub(super) fn definition_node(declaration: Node<'_>) -> Option<Node<'_>> {
    match declaration.kind() {
        "create_index" => declaration.child_by_field_name("column"),
        "column_definition" => declaration.child_by_field_name("name"),
        "function_argument" => first_child(declaration, "identifier"),
        "tsql_procedure" => procedure_header(declaration)?.child_by_field_name("object"),
        "tsql_procedure_header" => declaration.child_by_field_name("object"),
        "tsql_parameter" | "tsql_variable" | "tsql_table_variable" => {
            declaration.child_by_field_name("name")
        }
        "create_table"
        | "create_view"
        | "create_materialized_view"
        | "create_function"
        | "create_sequence"
        | "create_trigger"
        | "create_type" => first_child(declaration, "object_reference"),
        "create_database" | "create_role" | "create_extension" => {
            first_child(declaration, "identifier")
        }
        "create_schema" => {
            let mut cursor = declaration.walk();
            for child in declaration.named_children(&mut cursor) {
                match child.kind() {
                    "keyword_authorization" => return None,
                    "identifier" => return Some(child),
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

fn first_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

#[cfg(test)]
mod full_original_decline_observation {
    use super::*;

    #[test]
    fn command_execute_native_owner_decline_is_observed_without_source_text() {
        const ORIGINAL: &[u8] =
            include_bytes!("../../tests/fixtures/tsql_full_original/CommandExecute.sql");
        assert_eq!(ORIGINAL.len(), 12_267);
        assert_eq!(&ORIGINAL[295..317], b"[dbo].[CommandExecute]");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Sql))
            .unwrap();
        let tree = parser.parse(ORIGINAL, None).unwrap();
        let root = tree.root_node();
        assert_eq!(root.end_byte(), ORIGINAL.len());
        let mut stack = vec![root];
        let mut visited = 0;
        let mut reported = 0;
        while let Some(node) = stack.pop() {
            visited += 1;
            assert!(visited <= 65_536, "bounded native observation traversal");
            if reported < 8
                && node.start_byte() <= 295
                && node.end_byte() >= 317
                && matches!(
                    node.kind(),
                    "tsql_procedure" | "tsql_procedure_header" | "ERROR"
                )
            {
                reported += 1;
                let header = node.child_by_field_name("header");
                let body = node.child_by_field_name("body");
                let as_keyword = node.child_by_field_name("as");
                eprintln!(
                    "tsql-original-native kind={} range={}..{} missing={} error={} admitted={} header={:?} as={:?} body={:?}",
                    node.kind(),
                    node.start_byte(),
                    node.end_byte(),
                    node.is_missing(),
                    node.has_error(),
                    procedure_header(node).is_some(),
                    header.map(|n| (
                        n.kind(),
                        n.start_byte(),
                        n.end_byte(),
                        n.has_error(),
                        n.is_missing()
                    )),
                    as_keyword.map(|n| (
                        n.start_byte(),
                        n.end_byte(),
                        n.has_error(),
                        n.is_missing()
                    )),
                    body.map(|n| (
                        n.kind(),
                        n.start_byte(),
                        n.end_byte(),
                        n.has_error(),
                        n.is_missing()
                    ))
                );
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        eprintln!(
            "tsql-original-native root={} range={}..{} error={} visited={} relevant-reports={}",
            root.kind(),
            root.start_byte(),
            root.end_byte(),
            root.has_error(),
            visited,
            reported
        );
        // Recovery can erase the owner entirely; this is an observation test.
        // Only the separate public test can qualify missing-definition RED.
    }
}

#[cfg(test)]
mod recovered_header_controls {
    use super::procedure_header;
    use crate::query_pack::{QueryPack, StructuralRole};
    use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};
    use rootlight_cancel::{Cancellation, CancellationReason};

    #[test]
    fn original_recovered_header_is_independent_of_full_body_proof_and_optional_capture_room() {
        const ORIGINAL: &[u8] =
            include_bytes!("../../tests/fixtures/tsql_full_original/CommandExecute.sql");
        let family = crate::GrammarFamily::Sql;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        let tree = parser.parse(ORIGINAL, None).unwrap();
        let mut owner = tree
            .root_node()
            .descendant_for_byte_range(295, 317)
            .unwrap();
        while owner.kind() != "tsql_procedure" {
            owner = owner.parent().unwrap();
        }
        assert!(owner.has_error());
        assert!(
            procedure_header(owner).is_none(),
            "full-body admission remains strict"
        );
        let header = owner.child_by_field_name("header").unwrap();
        assert!(!header.has_error() && !header.is_missing());
        assert_eq!(header.byte_range(), 279..955);
        let pack = QueryPack::compile(family, include_str!("../../queries/sql.scm")).unwrap();
        let extracted = pack
            .extract(family, &tree, ORIGINAL, 16_384, 0, &Cancellation::new())
            .unwrap();
        let bodies: Vec<_> = extracted
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.role == StructuralRole::Signature
                    && candidate.syntax == "sql.recovered_body"
            })
            .collect();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].start..bodies[0].end, 965..10_780);
        assert!(bodies[0].required);
        assert!(extracted.candidates.iter().any(|candidate| candidate.role
            == StructuralRole::Declaration
            && candidate.syntax == "sql.function"
            && candidate.start == 279
            && candidate.end == 961));
        assert!(!extracted.candidates.iter().any(|candidate| candidate.role
            == StructuralRole::Declaration
            && matches!(candidate.syntax, "sql.variable" | "sql.column")
            && candidate.start >= 965
            && candidate.end <= 10_780));
        assert!(matches!(
            pack.extract_identity(family, &tree, ORIGINAL, 1, &Cancellation::new()),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::Records,
                ..
            }))
        ));
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            pack.extract_identity(family, &tree, ORIGINAL, 16_384, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
