//! Reviewed mock captures prove written IR identity, not assembled bindings.
//! Native owner admission and installed MCP are separate verification boundaries.

use std::collections::BTreeSet;

use super::*;
use rootlight_ir::{EntityKind, EntityVisibility, NormalizedIrDocument, OccurrenceTarget};

#[test]
fn assembly_file_module_retains_path_and_explicit_binding_gap() {
    const TEXT: &str = "written PROC\nret\nwritten ENDP\n";
    const PATH: &str = "src/owners.asm";
    let (_directory, snapshot, source) = source_fixture_for(TEXT, PATH, b"assembly-file-module");
    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("assembly").unwrap(),
        &limits(IrLimits::default()),
        vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("assembly.file.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Module,
                source.span(),
                1,
                label("assembly.file.module"),
            ),
        ],
    )
    .expect("file identity does not require an assembled namespace");
    let document = output.document();
    assert_eq!(document.entities.len(), 1);
    let module = &document.entities[0];
    assert_eq!(module.kind, EntityKind::Module);
    assert_eq!(module.canonical_name, PATH);
    assert_eq!(module.display_name, PATH);
    assert_eq!(module.visibility, EntityVisibility::Unknown);
    assert_eq!(module.flags, [EntityFlag::Synthetic]);
    assert!(document.occurrences.is_empty());
    assert_eq!(document.relations.len(), 1);
    let relation = &document.relations[0];
    assert_eq!(relation.predicate, RelationPredicate::Contains);
    assert_eq!(
        relation.subject,
        rootlight_ir::RelationEndpoint::File(source.span().file())
    );
    assert_eq!(
        relation.object,
        rootlight_ir::RelationEndpoint::Entity(module.id)
    );
    assert_eq!(relation.evidence.source.as_ref(), Some(&source));
    let gap = document
        .skipped_regions
        .iter()
        .find(|region| region.detail == "assembly-expansion-and-binding-unavailable")
        .expect("unimplemented expansion and binding are visible");
    assert_eq!(gap.domain, rootlight_ir::FactDomain::Relations);
    assert_eq!(gap.source, source);
}

#[test]
fn assembly_repeated_written_owners_keep_distinct_source_targets() {
    let (document, text, source) = written_document(false, false);
    assert_eq!(document.entities.len(), 14);
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        14
    );
    assert_eq!(document.occurrences.len(), 14);
    for entity in &document.entities {
        assert_eq!(entity.visibility, EntityVisibility::Unknown);
        assert!(!entity.flags.contains(&EntityFlag::Synthetic));
        let occurrence = document.occurrences.iter().find(|occurrence| {
            matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)
        }).expect("each physical definition targets its own written entity");
        assert_eq!(occurrence.role, OccurrenceRole::Definition);
        let span = occurrence.source.span();
        let range =
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap();
        assert_eq!(&text[range], entity.canonical_name);
        assert_eq!(entity.canonical_name, entity.display_name);
        assert_eq!(occurrence.source.repository(), source.repository());
        assert_eq!(occurrence.source.generation(), source.generation());
        assert_eq!(occurrence.source.content_hash(), source.content_hash());
        assert_eq!(occurrence.evidence.source, entity.evidence.source);
    }
    for (kind, count) in [
        (EntityKind::Function, 4),
        (EntityKind::Module, 2),
        (EntityKind::Constant, 4),
        (EntityKind::Variable, 2),
        (EntityKind::StatementLabel, 2),
    ] {
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == kind)
                .count(),
            count
        );
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
}

#[test]
fn assembly_written_ids_survive_header_body_and_unrelated_prefix_edits() {
    let (original, _, source) = written_document(false, false);
    let (edited, _, edited_source) = written_document(true, true);
    let ids = |document: &NormalizedIrDocument| {
        document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name != "other")
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&original).len(), 14);
    assert_eq!(ids(&original), ids(&edited));
    assert_ne!(source.content_hash(), edited_source.content_hash());
    assert!(edited.occurrences.iter().all(|occurrence| {
        occurrence.source.content_hash() == edited_source.content_hash()
            && occurrence.source.generation() == edited_source.generation()
    }));
}

fn written_document(
    include_other: bool,
    edited: bool,
) -> (NormalizedIrDocument, String, SourceRef) {
    // All owners are deliberately siblings. Repeated source occurrences must not
    // merge merely because their names, headers or entity kinds agree.
    let owners = [
        ("procedure", "written", "masm_name", "PROC", "ENDP"),
        ("macro", "written", "masm_name", "MACRO", "ENDM"),
        ("segment", "written", "masm_name", "SEGMENT", "ENDS"),
        ("constant", "written", "masm_name", "EQU", ""),
        ("data", "written", "masm_name", "DB", ""),
        ("source_label", ".Lwritten", "source_label", ":", ""),
        ("source_constant", "written", "source_constant", "=", ""),
    ];
    let mut rows = Vec::new();
    if include_other {
        rows.push(("procedure", "other", "masm_name", "PROC", "ENDP"));
    }
    for row in owners {
        rows.extend([row, row]);
    }
    let mut text = String::new();
    let mut ranges = Vec::new();
    for (owner, name, capture, directive, closer) in rows {
        let start = text.len();
        text.push_str(name);
        let name_end = text.len();
        text.push(' ');
        text.push_str(directive);
        if edited {
            text.push_str(" 1");
        }
        let header_end = text.len();
        text.push('\n');
        if !closer.is_empty() {
            text.push_str(if edited { "nop\n" } else { "ret\n" });
            text.push_str(name);
            text.push(' ');
            text.push_str(closer);
            text.push('\n');
        }
        ranges.push((owner, capture, start, name_end, header_end, text.len()));
    }
    let (_directory, snapshot, source) =
        source_fixture_for(&text, "src/written.asm", b"assembly-written-identities");
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("assembly.file.root"),
    )];
    for (index, (owner, capture, start, name_end, header_end, end)) in
        ranges.into_iter().enumerate()
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
                label(&format!("assembly.{owner}.declaration")),
            ),
            SyntaxFact::new(
                id + 1,
                Some(id),
                SyntaxFactKind::Occurrence,
                span(start, name_end),
                2,
                label(&format!("assembly.{capture}.definition")),
            ),
            SyntaxFact::new(
                id + 2,
                Some(id),
                SyntaxFactKind::Signature,
                span(start, header_end),
                2,
                label("assembly.header.signature"),
            ),
        ]);
    }
    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("assembly").unwrap(),
        &limits(IrLimits::default()),
        facts,
    )
    .expect("reviewed complete source captures lower without assembled identity");
    (output.document().clone(), text, source)
}
