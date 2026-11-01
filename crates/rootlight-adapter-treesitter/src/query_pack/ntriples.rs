//! Certify complete native N-Triples statements and their direct term fields.
//! These required expression facts transport source evidence without declarations.

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
    if node.kind() != "triple"
        || !clean(node, source)
        || !node
            .parent()
            .is_some_and(|parent| parent.kind() == "document")
    {
        return Ok(false);
    }
    for field in ["subject", "predicate", "object"] {
        cancellation.check()?;
        let mut cursor = node.walk();
        let mut children = node.children_by_field_name(field, &mut cursor);
        let Some(term) = children.next() else {
            return Ok(false);
        };
        if children.next().is_some()
            || !clean(term, source)
            || !term.parent().is_some_and(|parent| parent.id() == node.id())
            || !match field {
                "subject" => matches!(term.kind(), "iri_reference" | "blank_node_label"),
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
        ("document", StructuralRole::Root | StructuralRole::Module) => "ntriples.file",
        ("triple", StructuralRole::Expression) if statement(node, source, cancellation)? => {
            "ntriples.statement"
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
                ("subject", "ntriples.subject"),
                ("predicate", "ntriples.predicate"),
                ("object", "ntriples.object"),
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
        ("comment", StructuralRole::Comment) => "ntriples.comment",
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
                || !statement(parent, source, cancellation)?
            {
                return Ok(None);
            }
            "ntriples.literal"
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

    #[test]
    fn native_statement_and_three_direct_terms_are_required_identity_facts() {
        let source = b"_:s <https://example.test/p> \"v\"@en .\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::NTriples))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let pack = QueryPack::compile(
            GrammarFamily::NTriples,
            include_str!("../../queries/ntriples.scm"),
        )
        .unwrap();
        let facts = pack
            .extract_identity(
                GrammarFamily::NTriples,
                &tree,
                source,
                4096,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(facts.len(), 6);
        for syntax in [
            "ntriples.statement",
            "ntriples.subject",
            "ntriples.predicate",
            "ntriples.object",
        ] {
            let selected: Vec<_> = facts.iter().filter(|fact| fact.syntax == syntax).collect();
            assert_eq!(selected.len(), 1, "{syntax}");
            assert_eq!(selected[0].role, StructuralRole::Expression);
            assert!(selected[0].required);
        }
        assert!(facts.iter().all(|fact| !matches!(
            fact.role,
            StructuralRole::Declaration | StructuralRole::Definition
        )));
    }
}
