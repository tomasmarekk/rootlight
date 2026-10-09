//! Native SQL declaration ownership independent of query match ordering.
//! Extra comment nodes and optional clauses must not turn referenced objects or
//! authorization principals into definitions of the enclosing database object.

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
    if body.is_missing() || body.has_error() {
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
