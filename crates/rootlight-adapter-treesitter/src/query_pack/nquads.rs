//! Certify standard native N-Quads statements and direct physical term roles.
//! Default graphs have no graph fact; recovery and nested datatype IRIs grant no roles.

use super::{QueryCandidate, StructuralRole, native_depth};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn statement(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.kind() != "quad"
        || !clean(node, source)
        || !node
            .parent()
            .is_some_and(|parent| parent.kind() == "document")
    {
        return Ok(false);
    }
    for field in ["subject", "predicate", "object", "graph"] {
        cancellation.check()?;
        let mut cursor = node.walk();
        let mut children = node.children_by_field_name(field, &mut cursor);
        let Some(term) = children.next() else {
            if field == "graph" {
                continue;
            }
            return Ok(false);
        };
        if children.next().is_some()
            || !clean(term, source)
            || !term.parent().is_some_and(|parent| parent.id() == node.id())
            || !match field {
                "subject" | "graph" => matches!(term.kind(), "iri_reference" | "blank_node_label"),
                "predicate" => term.kind() == "iri_reference",
                "object" => matches!(
                    term.kind(),
                    "iri_reference" | "blank_node_label" | "literal"
                ),
                _ => false,
            }
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    if !clean(node, source) && !matches!(role, StructuralRole::Root | StructuralRole::Module) {
        return Ok(None);
    }
    let syntax = match (node.kind(), role) {
        ("document", StructuralRole::Root | StructuralRole::Module) => "nquads.file",
        ("quad", StructuralRole::Expression) if statement(node, source, cancellation)? => {
            "nquads.statement"
        }
        ("iri_reference" | "blank_node_label" | "literal", StructuralRole::Expression) => {
            let Some(parent) = node.parent() else {
                return Ok(None);
            };
            if !statement(parent, source, cancellation)? {
                return Ok(None);
            }
            let mut selected = None;
            for (field, syntax) in [
                ("subject", "nquads.subject"),
                ("predicate", "nquads.predicate"),
                ("object", "nquads.object"),
                ("graph", "nquads.graph"),
            ] {
                cancellation.check()?;
                if parent
                    .child_by_field_name(field)
                    .is_some_and(|term| term.id() == node.id())
                    && selected.replace(syntax).is_some()
                {
                    return Ok(None);
                }
            }
            let Some(syntax) = selected else {
                return Ok(None);
            };
            syntax
        }
        ("comment", StructuralRole::Comment) => "nquads.comment",
        ("string", StructuralRole::StringLiteral) => {
            let Some(literal) = node.parent().filter(|parent| parent.kind() == "literal") else {
                return Ok(None);
            };
            let Some(parent) = literal.parent() else {
                return Ok(None);
            };
            if !literal
                .child_by_field_name("value")
                .is_some_and(|value| value.id() == node.id())
                || !parent
                    .child_by_field_name("object")
                    .is_some_and(|object| object.id() == literal.id())
                || !statement(parent, source, cancellation)?
            {
                return Ok(None);
            }
            "nquads.literal"
        }
        _ => return Ok(None),
    };
    cancellation.check()?;
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure() || role == StructuralRole::Expression,
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrammarFamily, query_pack::QueryPack, registry::language_for};
    use rootlight_cancel::CancellationReason;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::NQuads))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn pack() -> QueryPack {
        QueryPack::compile(
            GrammarFamily::NQuads,
            include_str!("../../queries/nquads.scm"),
        )
        .unwrap()
    }

    #[test]
    fn native_quad_terms_are_required_and_default_graph_has_no_capture() {
        for (source, graph) in [
            ("<s:><p:>\"λ\0\"@EN.\n", None),
            (
                "<s:><p:>\"v\"^^<urn:type><urn:g#anchor>.",
                Some("<urn:g#anchor>"),
            ),
            ("_:a <p:> _:a _:a.\r\n", Some("_:a")),
        ] {
            let source = source.as_bytes();
            let tree = parse(source);
            assert!(!tree.root_node().has_error());
            let facts = pack()
                .extract_identity(
                    GrammarFamily::NQuads,
                    &tree,
                    source,
                    4096,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(facts.len(), if graph.is_some() { 7 } else { 6 });
            for syntax in [
                "nquads.statement",
                "nquads.subject",
                "nquads.predicate",
                "nquads.object",
            ] {
                let selected: Vec<_> = facts.iter().filter(|fact| fact.syntax == syntax).collect();
                assert_eq!(selected.len(), 1, "{syntax}");
                assert_eq!(selected[0].role, StructuralRole::Expression);
                assert!(selected[0].required);
            }
            let graphs: Vec<_> = facts
                .iter()
                .filter(|fact| fact.syntax == "nquads.graph")
                .collect();
            if let Some(graph) = graph {
                assert_eq!(graphs.len(), 1);
                assert_eq!(&source[graphs[0].start..graphs[0].end], graph.as_bytes());
                assert!(graphs[0].required);
                assert_eq!(graphs[0].role, StructuralRole::Expression);
            } else {
                assert!(graphs.is_empty());
            }
            assert!(facts.iter().all(|fact| !matches!(
                fact.role,
                StructuralRole::Declaration | StructuralRole::Definition
            )));
        }
    }

    #[test]
    fn native_quad_certification_refuses_recovery_and_nested_datatype_roles() {
        for source in [
            "<s:><p:><o:>\"g\".",
            "<s:><p:><o:><g:><extra:>.",
            "<s:><p:><o:><relative>.",
            "<s:><p:>\"\\q\"<g:>.",
            "<s:><p:>\"unterminated",
            "<s:><p:><o:>\n<g:>.",
            "<s:><p:><o:><urn:\0g>.",
            "<alice><follows><bob>.",
        ] {
            let source = source.as_bytes();
            let tree = parse(source);
            assert!(tree.root_node().has_error());
            let facts = pack()
                .extract_identity(
                    GrammarFamily::NQuads,
                    &tree,
                    source,
                    4096,
                    &Cancellation::new(),
                )
                .unwrap();
            assert!(
                facts
                    .iter()
                    .all(|fact| fact.role != StructuralRole::Expression),
                "{source:?}"
            );
        }
        let source = b"<s:><p:>\"v\"^^<urn:type><g:>.";
        let tree = parse(source);
        let mut cursor = tree.root_node().walk();
        let quad = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "quad")
            .unwrap();
        let datatype = quad
            .child_by_field_name("object")
            .unwrap()
            .child_by_field_name("datatype")
            .unwrap();
        assert!(
            candidate(
                datatype,
                StructuralRole::Expression,
                source,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn native_quad_certification_checks_cancellation_before_retaining_evidence() {
        let source = b"<s:><p:><o:><g:>.";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            pack().extract_identity(GrammarFamily::NQuads, &tree, source, 4096, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
