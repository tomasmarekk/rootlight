//! Certify direct Dgraph term roles over complete clean physical statements.
//! Native identifiers and default values never imply RDF IRIs or named graphs.

use super::{QueryCandidate, StructuralRole, native_depth};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn physical_line(node: Node<'_>, source: &[u8]) -> bool {
    let Some(before) = source.get(..node.start_byte()) else {
        return false;
    };
    let start = before
        .iter()
        .rposition(|byte| matches!(byte, b'\r' | b'\n'))
        .map_or(0, |index| index + 1);
    let mut prefix = &before[start..];
    if start == 0 {
        prefix = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix);
    }
    if !prefix.iter().all(|byte| matches!(byte, b' ' | b'\t')) {
        return false;
    }
    let Some(after) = source.get(node.end_byte()..) else {
        return false;
    };
    let end = after
        .iter()
        .position(|byte| matches!(byte, b'\r' | b'\n'))
        .unwrap_or(after.len());
    let suffix = after[..end]
        .iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\t'));
    matches!(suffix, None | Some(b'#'))
}

fn statement(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.kind() != "quad"
        || !clean(node, source)
        || !physical_line(node, source)
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
                "subject" | "predicate" => term.kind() == "identifier",
                "object" => matches!(term.kind(), "identifier" | "default_value"),
                _ => false,
            }
        {
            return Ok(false);
        }
        if field == "predicate" && source[term.byte_range()].contains(&b'@') {
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
        ("document", StructuralRole::Root | StructuralRole::Module) => "nquads-dgraph.file",
        ("quad", StructuralRole::Expression) if statement(node, source, cancellation)? => {
            "nquads-dgraph.statement"
        }
        ("identifier" | "default_value", StructuralRole::Expression) => {
            let Some(parent) = node.parent() else {
                return Ok(None);
            };
            if !statement(parent, source, cancellation)? {
                return Ok(None);
            }
            let mut selected = None;
            for (field, syntax) in [
                ("subject", "nquads-dgraph.subject"),
                ("predicate", "nquads-dgraph.predicate"),
                ("object", "nquads-dgraph.object"),
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
        ("comment", StructuralRole::Comment) => "nquads-dgraph.comment",
        ("string", StructuralRole::StringLiteral) => {
            let Some(value) = node
                .parent()
                .filter(|parent| parent.kind() == "default_value")
            else {
                return Ok(None);
            };
            let Some(parent) = value.parent() else {
                return Ok(None);
            };
            if !value
                .child_by_field_name("value")
                .is_some_and(|inner| inner.id() == node.id())
                || !parent
                    .child_by_field_name("object")
                    .is_some_and(|object| object.id() == value.id())
                || !statement(parent, source, cancellation)?
            {
                return Ok(None);
            }
            "nquads-dgraph.default-value"
        }
        _ => return Ok(None),
    };
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
            .set_language(&language_for(GrammarFamily::NQuadsDgraph))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn pack() -> QueryPack {
        QueryPack::compile(
            GrammarFamily::NQuadsDgraph,
            include_str!("../../queries/nquads-dgraph.scm"),
        )
        .unwrap()
    }

    #[test]
    fn native_roles_require_the_complete_physical_statement_and_direct_fields() {
        for source in [
            b"<alice><follows><bob>.\n".as_slice(),
            b"\xef\xbb\xbf \t<0x01><status>\"\\xFF\\v\". # native value\r\n",
            "<http://example.test/a><status>\"λ\0\".\r".as_bytes(),
        ] {
            let tree = parse(source);
            assert!(!tree.root_node().has_error());
            let facts = pack()
                .extract_identity(
                    GrammarFamily::NQuadsDgraph,
                    &tree,
                    source,
                    4096,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(facts.len(), 6);
            for syntax in [
                "nquads-dgraph.statement",
                "nquads-dgraph.subject",
                "nquads-dgraph.predicate",
                "nquads-dgraph.object",
            ] {
                let selected: Vec<_> = facts.iter().filter(|fact| fact.syntax == syntax).collect();
                assert_eq!(selected.len(), 1, "{syntax}");
                assert_eq!(selected[0].role, StructuralRole::Expression);
                assert!(selected[0].required);
            }
            assert!(facts.iter().all(|fact| !fact.syntax.ends_with(".graph")));
            assert!(facts.iter().all(|fact| !matches!(
                fact.role,
                StructuralRole::Declaration | StructuralRole::Definition
            )));
        }
        let source = b"<alice><status>\"value\".";
        let tree = parse(source);
        let value = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .child_by_field_name("object")
            .unwrap();
        let string = value.child_by_field_name("value").unwrap();
        assert!(
            candidate(
                string,
                StructuralRole::Expression,
                source,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
        let optional = candidate(
            string,
            StructuralRole::StringLiteral,
            source,
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(optional.syntax, "nquads-dgraph.default-value");
        assert!(!optional.required);
        assert_eq!(&source[optional.start..optional.end], b"\"value\"");
    }

    #[test]
    fn recovery_and_unsupported_native_constructs_cannot_certify_terms() {
        for source in [
            b"junk <alice><follows><bob>.".as_slice(),
            b"<alice><follows><bob>. junk",
            b"<alice><follows><bob>. <bob><follows><alice>.",
            b"<alice><follows><bob> <0x01>.",
            b"<alice><follows><bob> 0x01.",
            b"<alice><follows><bob> 1.",
            b"<alice><follows>uid(other).",
            b"<alice><follows>*.",
            b"<alice><follows>0x01.",
            b"_:alice <follows><bob>.",
            b"<alice><follows><bob> (since=2020).",
            b"<alice><status@en>\"value\".",
            b"<alice><status>\"value\"@en.",
            b"<alice><status>\"value\"^^<xs:string>.",
            b"<alice><status>\"\\q\".",
            b"<alice><status>\"unterminated",
        ] {
            let tree = parse(source);
            let facts = pack()
                .extract_identity(
                    GrammarFamily::NQuadsDgraph,
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
        let source = b"junk\n<alice><follows><bob>.\n";
        let tree = parse(source);
        assert!(tree.root_node().has_error());
        let facts = pack()
            .extract_identity(
                GrammarFamily::NQuadsDgraph,
                &tree,
                source,
                4096,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.syntax == "nquads-dgraph.statement")
                .count(),
            1
        );
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.role == StructuralRole::Expression)
                .count(),
            4
        );
    }

    #[test]
    fn native_role_certification_checks_cancellation_before_retaining_evidence() {
        let source = b"<alice><follows><bob>.";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            pack().extract_identity(
                GrammarFamily::NQuadsDgraph,
                &tree,
                source,
                4096,
                &cancellation
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
