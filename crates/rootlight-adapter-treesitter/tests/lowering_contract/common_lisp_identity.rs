//! Source-occurrence contracts use reviewed mock facts, not a reader or compiler.
//! Native context admission is tested independently before product integration.

use std::collections::BTreeSet;

use super::*;
use rootlight_ir::{EntityKind, EntityVisibility, NormalizedIrDocument, OccurrenceTarget};

#[test]
fn common_lisp_written_declarations_do_not_merge_binding_names() {
    let document = written_document(false, "item", "item");
    assert_eq!(document.entities.len(), 6);
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        4
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .count(),
        2
    );
    assert!(document.entities.iter().all(|entity| {
        entity.canonical_name == "written"
            && entity.display_name == "written"
            && entity.visibility == EntityVisibility::Unknown
    }));
    let definitions = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 6);
    for definition in definitions {
        assert!(
            matches!(definition.target, OccurrenceTarget::Resolved { symbol }
            if document.entities.iter().any(|entity| entity.id == symbol))
        );
    }
}

#[test]
fn common_lisp_written_ids_survive_body_header_and_unrelated_prefix_edits() {
    let original = written_document(false, "|a b|", "|a b|");
    let edited = written_document(true, "|ab|", "(list |ab|)");
    let ids = |document: &NormalizedIrDocument| {
        document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "written")
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&original).len(), 6);
    assert_eq!(ids(&original), ids(&edited));
    let original_sources = original
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .collect::<Vec<_>>();
    let edited_sources = edited
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "written")
        .filter_map(|entity| entity.evidence.source.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(original_sources.len(), 6);
    assert_eq!(edited_sources.len(), 6);
    assert!(original_sources.iter().all(|source| {
        edited_sources
            .iter()
            .all(|edited| source.content_hash() != edited.content_hash())
    }));
}

#[test]
fn common_lisp_written_names_keep_exact_source_bound_spelling() {
    let names = [
        "|Mixed Name|",
        "|MixedName|",
        "escaped\\ name",
        "pkg::written",
        "|Mixed Package|::|Mixed Name|",
        "/",
    ];
    let rows = names
        .iter()
        .map(|name| {
            (
                "defun",
                *name,
                "item",
                "item",
                "common_lisp.function.declaration",
            )
        })
        .collect::<Vec<_>>();
    let (document, text, source) = captured_document(&rows);
    assert_eq!(document.entities.len(), names.len());
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        names.len()
    );
    for name in names {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .expect("written symbol spelling is not lost or normalized");
        assert_eq!(entity.display_name, name);
        assert_eq!(entity.visibility, EntityVisibility::Unknown);
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                matches!(occurrence.target, OccurrenceTarget::Resolved { symbol }
                if symbol == entity.id)
            })
            .expect("physical definition retains its own entity target");
        let definition_source = &definition.source;
        let start = usize::try_from(definition_source.span().start_byte()).unwrap();
        let end = usize::try_from(definition_source.span().end_byte()).unwrap();
        assert_eq!(&text[start..end], name);
        assert_eq!(definition_source.repository(), source.repository());
        assert_eq!(definition_source.generation(), source.generation());
        assert_eq!(definition_source.content_hash(), source.content_hash());
        assert_eq!(definition.evidence.source, entity.evidence.source);
    }
}

fn written_document(include_other: bool, parameter: &str, body: &str) -> NormalizedIrDocument {
    let mut rows = Vec::new();
    if include_other {
        rows.push((
            "defun",
            "other",
            "value",
            "value",
            "common_lisp.function.declaration",
        ));
    }
    rows.extend([
        (
            "defun",
            "written",
            parameter,
            body,
            "common_lisp.function.declaration",
        ),
        (
            "defun",
            "written",
            parameter,
            body,
            "common_lisp.function.declaration",
        ),
        (
            "defmacro",
            "written",
            parameter,
            body,
            "common_lisp.macro.declaration",
        ),
        (
            "defgeneric",
            "written",
            parameter,
            body,
            "common_lisp.generic_function.declaration",
        ),
        (
            "defmethod",
            "written",
            parameter,
            body,
            "common_lisp.method.declaration",
        ),
        (
            "defmethod",
            "written",
            parameter,
            body,
            "common_lisp.method.declaration",
        ),
    ]);
    captured_document(&rows).0
}

fn captured_document(
    rows: &[(&str, &str, &str, &str, &str)],
) -> (NormalizedIrDocument, String, SourceRef) {
    let mut text = String::new();
    let mut ranges = Vec::new();
    for &(operator, name, parameter, body, kind) in rows {
        let start = text.len();
        text.push('(');
        text.push_str(operator);
        text.push(' ');
        let name_start = text.len();
        text.push_str(name);
        let name_end = text.len();
        text.push_str(" (");
        text.push_str(parameter);
        text.push(')');
        let header_end = text.len();
        if operator != "defgeneric" {
            text.push(' ');
            text.push_str(body);
        }
        text.push_str(")\n");
        ranges.push((
            start,
            text.len() - 1,
            name_start,
            name_end,
            header_end,
            kind,
        ));
    }
    let (_directory, snapshot, source) =
        source_fixture_for(&text, "src/written.lisp", b"common-lisp-source-occurrences");
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("common_lisp.file.root"),
    )];
    for (index, &(start, end, name_start, name_end, header_end, kind)) in ranges.iter().enumerate()
    {
        let id = u64::try_from(index).unwrap() * 3 + 2;
        let span = |start, end| {
            SourceSpan::new(
                source.span().file(),
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap()
        };
        facts.extend([
            SyntaxFact::new(
                id,
                Some(1),
                SyntaxFactKind::Declaration,
                span(start, end),
                1,
                label(kind),
            ),
            SyntaxFact::new(
                id + 1,
                Some(id),
                SyntaxFactKind::Occurrence,
                span(name_start, name_end),
                2,
                label("common_lisp.name.definition"),
            ),
            SyntaxFact::new(
                id + 2,
                Some(id),
                SyntaxFactKind::Signature,
                span(start, header_end),
                2,
                label("common_lisp.header.signature"),
            ),
        ]);
    }
    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("common-lisp").unwrap(),
        &limits(IrLimits::default()),
        facts,
    )
    .expect("reviewed written facts lower without inventing bindings");
    (output.document().clone(), text, source)
}
