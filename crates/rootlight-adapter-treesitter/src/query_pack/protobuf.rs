//! Protobuf captures prove complete physical schema owners and direct names.
//! The upstream parser's broader recovery and omitted oneof terminators cannot
//! establish declarations, while all original bytes remain available to readers.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind && selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(selected)
}

fn owner_kind(kind: &str) -> bool {
    matches!(
        kind,
        "message"
            | "enum"
            | "enum_field"
            | "field"
            | "map_field"
            | "oneof"
            | "oneof_field"
            | "service"
            | "rpc"
            | "package"
    )
}

fn owner_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let wrapper = match node.kind() {
        "message" => "message_name",
        "enum" => "enum_name",
        "service" => "service_name",
        "rpc" => "rpc_name",
        "package" => "full_ident",
        "field" | "map_field" | "oneof_field" | "enum_field" | "oneof" => "identifier",
        _ => return Ok(None),
    };
    let Some(name) = direct(node, wrapper, cancellation)? else {
        return Ok(None);
    };
    if matches!(wrapper, "identifier" | "full_ident") {
        Ok(Some(name))
    } else {
        direct(name, "identifier", cancellation)
    }
}

fn physical_terminator<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let mut next = node.next_named_sibling();
    while let Some(comment) = next.filter(|candidate| candidate.kind() == "comment") {
        next = comment.next_named_sibling();
    }
    next.filter(|candidate| {
        candidate.kind() == "empty_statement" && text(*candidate, source) == Some(";")
    })
}

fn owner_is_complete(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !owner_kind(node.kind())
        || node.is_missing()
        || node.has_error()
        || text(node, source).is_none()
    {
        return Ok(false);
    }
    let parent = node.parent().map(|parent| parent.kind());
    let admitted_parent = match node.kind() {
        "message" | "enum" => matches!(parent, Some("source_file" | "message_body")),
        "field" | "map_field" | "oneof" => parent == Some("message_body"),
        "enum_field" => parent == Some("enum_body"),
        "oneof_field" => parent == Some("oneof") && physical_terminator(node, source).is_some(),
        "rpc" => parent == Some("service"),
        "service" | "package" => parent == Some("source_file"),
        _ => false,
    };
    if !admitted_parent {
        return Ok(false);
    }
    let Some(name) = owner_name(node, cancellation)? else {
        return Ok(false);
    };
    Ok(!name.is_missing()
        && !name.has_error()
        && text(name, source).is_some_and(|name| {
            structural_captured_name_for_language("protobuf", name, name.len()).is_some()
        }))
}

fn clean_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(parent) = current {
        cancellation.check()?;
        if parent.is_missing()
            || matches!(parent.kind(), "ERROR" | "extend")
            || (owner_kind(parent.kind()) && !owner_is_complete(parent, source, cancellation)?)
        {
            return Ok(false);
        }
        current = parent.parent();
    }
    Ok(true)
}

/// Admits only source-owned identity captures; cancellation propagates unchanged.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "source_file" {
        return Ok(matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Scope
                | StructuralRole::Expression
        ));
    }
    if role == StructuralRole::Expression {
        return Ok(text(node, source).is_some() && !node.byte_range().is_empty());
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) {
        return Ok(!node.is_missing() && !node.has_error() && text(node, source).is_some());
    }
    if !clean_chain(node, source, cancellation)? {
        return Ok(false);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        return owner_is_complete(node, source, cancellation);
    }
    if role == StructuralRole::Import {
        return Ok(node
            .parent()
            .is_some_and(|parent| parent.kind() == "import" && !parent.has_error())
            && text(node, source)
                .is_some_and(|path| path.starts_with('"') && path.ends_with('"')));
    }
    if role == StructuralRole::Reference
        && node.kind() == "message_or_enum_type"
        && !node.has_error()
    {
        let context = node.parent();
        let owned = context.is_some_and(|parent| parent.kind() == "rpc")
            || context
                .filter(|parent| parent.kind() == "type")
                .and_then(|parent| parent.parent())
                .is_some_and(|owner| matches!(owner.kind(), "field" | "map_field" | "oneof_field"));
        return Ok(owned
            && text(node, source).is_some_and(|path| {
                structural_captured_name_for_language("protobuf", path, path.len()).is_some()
            }));
    }
    Ok(false)
}

fn syntax(node: Node<'_>) -> &'static str {
    match node.kind() {
        "message" => "protobuf.message",
        "enum" => "protobuf.enum",
        "enum_field" => "protobuf.enum_value",
        "field" => "protobuf.field",
        "map_field" => "protobuf.map_field",
        "oneof" => "protobuf.oneof",
        "oneof_field" => "protobuf.oneof_field",
        "service" => "protobuf.service",
        "rpc" => "protobuf.rpc",
        "package" => "protobuf.package",
        _ => "protobuf.unsupported_owner",
    }
}

/// Projects whole owners and exact written names without replacing source bytes.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let label = if node.kind() == "source_file" {
        range = 0..source.len();
        if role == StructuralRole::Expression {
            "protobuf.source_semantics"
        } else {
            "protobuf.file"
        }
    } else if role == StructuralRole::Expression {
        match node.kind() {
            "ERROR" => "protobuf.unsupported_construct",
            "import"
                if node
                    .child_by_field_name("path")
                    .and_then(|path| text(path, source))
                    .is_some_and(|path| !path.starts_with('"')) =>
            {
                "protobuf.unsupported_construct"
            }
            "extend" | "extensions" => "protobuf.extension_semantics",
            "oneof_field" if physical_terminator(node, source).is_none() => {
                "protobuf.oneof_terminator"
            }
            "oneof_field" => "protobuf.field_semantics",
            _ => "protobuf.schema_semantics",
        }
    } else if role == StructuralRole::Definition {
        let name = owner_name(node, cancellation)?
            .ok_or_else(|| query_failure("query-protobuf-name-owner"))?;
        range = name.byte_range();
        if node.kind() == "package" {
            "protobuf.package_name"
        } else {
            "protobuf.name"
        }
    } else if role == StructuralRole::Signature {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            cancellation.check()?;
            if matches!(child.kind(), "message_body" | "enum_body" | "{") {
                range.end = child.start_byte();
                break;
            }
        }
        "protobuf.written_header"
    } else if role == StructuralRole::Declaration {
        if node.kind() == "oneof_field" {
            range.end = physical_terminator(node, source)
                .ok_or_else(|| query_failure("query-protobuf-oneof-terminator"))?
                .end_byte();
        }
        syntax(node)
    } else {
        match role {
            StructuralRole::Reference => "protobuf.type",
            StructuralRole::Import => "protobuf.written_import",
            StructuralRole::Comment => "protobuf.comment",
            StructuralRole::StringLiteral => "protobuf.string",
            _ => return Err(query_failure("query-protobuf-role")),
        }
    };
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax: label,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    fn captures(source: &str) -> Vec<QueryCandidate> {
        let mut parser = Parser::new();
        let language = tree_sitter_proto::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = Query::new(&language, include_str!("../../queries/protobuf.scm")).unwrap();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let cancellation = Cancellation::new();
        let mut result = Vec::new();
        while let Some(found) = matches.next() {
            for capture in found.captures {
                let role = StructuralRole::from_capture_name(
                    query.capture_names()[usize::try_from(capture.index).unwrap()],
                )
                .unwrap();
                if retain_capture(capture.node, role, source.as_bytes(), &cancellation).unwrap() {
                    result.push(
                        candidate(capture.node, role, source.as_bytes(), &cancellation).unwrap(),
                    );
                }
            }
        }
        result
    }

    #[test]
    fn written_schema_owners_names_and_type_paths_have_physical_spans() {
        let source = "syntax = \"proto3\";\r\npackage sample.api;\r\nimport public \"other.proto\";\r\nmessage Request { message Nested { string value = 1; } enum State { READY = 0; } Nested item = 1; map<string, .sample.api.Request.Nested> entries = 2; oneof choice { Nested selected = 3; string text = 4; } }\r\nservice Gateway { rpc Fetch(stream Request) returns (Request.Nested); }\r\n// message Phantom {}\r\n";
        let facts = captures(source);
        for name in [
            "sample.api",
            "Request",
            "Nested",
            "value",
            "State",
            "READY",
            "item",
            "entries",
            "choice",
            "selected",
            "text",
            "Gateway",
            "Fetch",
        ] {
            assert!(
                facts
                    .iter()
                    .any(|fact| fact.role == StructuralRole::Definition
                        && source.get(fact.start..fact.end) == Some(name)),
                "{name}"
            );
        }
        assert!(
            !facts
                .iter()
                .any(|fact| fact.role == StructuralRole::Definition
                    && source.get(fact.start..fact.end) == Some("Phantom"))
        );
        for written in [
            "Nested",
            ".sample.api.Request.Nested",
            "Request",
            "Request.Nested",
        ] {
            assert!(
                facts.iter().any(|fact| fact.syntax == "protobuf.type"
                    && fact.role == StructuralRole::Reference
                    && source.get(fact.start..fact.end) == Some(written)),
                "{written}"
            );
        }
        assert!(
            facts
                .iter()
                .any(|fact| fact.syntax == "protobuf.written_import"
                    && source.get(fact.start..fact.end) == Some("\"other.proto\""))
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.syntax == "protobuf.oneof_field"
                    && fact.role == StructuralRole::Declaration
                    && source.get(fact.start..fact.end) == Some("Nested selected = 3;"))
        );
    }

    #[test]
    fn unsupported_and_overpermissive_grammar_regions_remain_applicable_gaps() {
        for source in [
            "edition = \"2023\"; message Item {}",
            "syntax = \"proto2\"; message Item { optional group Legacy = 1 { optional string value = 2; } }",
            "import 'other.proto';",
            "message Item { oneof choice { string missing = 1 } }",
        ] {
            let facts = captures(source);
            assert!(
                facts
                    .iter()
                    .any(|fact| fact.role == StructuralRole::Expression
                        && matches!(
                            fact.syntax,
                            "protobuf.unsupported_construct" | "protobuf.oneof_terminator"
                        )),
                "{source}"
            );
            assert!(
                !facts
                    .iter()
                    .any(|fact| fact.role == StructuralRole::Definition
                        && source.get(fact.start..fact.end) == Some("missing"))
            );
        }
    }

    #[test]
    fn extension_body_does_not_invent_a_file_owned_field() {
        let source = "syntax = \"proto2\"; extend External { optional string injected = 1; }";
        let facts = captures(source);
        assert!(
            !facts
                .iter()
                .any(|fact| fact.role == StructuralRole::Definition)
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.syntax == "protobuf.extension_semantics")
        );
    }
}
