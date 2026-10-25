//! Named Rust field captures use unique source names and the nearest struct.
//! Mock-provider cases preserve strict source binding without claiming native parsing.

use super::*;
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceTarget, RelationEndpoint, validate_ir_document,
};

const TEXT: &str = "struct Owner { value: u64 }\n";

fn facts(source: &SourceRef) -> Vec<SyntaxFact> {
    vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("rust.file.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(TEXT, source, TEXT.trim_end(), 0),
            1,
            label("rust.struct.declaration"),
        ),
        SyntaxFact::new(
            3,
            Some(2),
            SyntaxFactKind::Occurrence,
            span_in(TEXT, source, "Owner", 0),
            2,
            label("rust.type_identifier.definition"),
        ),
        SyntaxFact::new(
            4,
            Some(2),
            SyntaxFactKind::Declaration,
            span_in(TEXT, source, "value: u64", 0),
            2,
            label("rust.field.declaration"),
        ),
        SyntaxFact::new(
            5,
            Some(4),
            SyntaxFactKind::Occurrence,
            span_in(TEXT, source, "value", 0),
            3,
            label("rust.field_identifier.definition"),
        ),
    ]
}

#[test]
fn unique_named_field_capture_has_one_exact_definition_and_source_backed_struct_edge() {
    let (_directory, snapshot, source) =
        source_fixture_for(TEXT, "src/lib.rs", b"rust-named-field-owner");
    let budget = limits(IrLimits::default());
    let result = analyze_custom(&snapshot, &source, language(), &budget, facts(&source)).unwrap();
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    let document = result.document();
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Owner")
        .unwrap();
    let field = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "value")
        .unwrap();
    assert_eq!(owner.kind, EntityKind::Struct);
    assert_eq!(field.kind, EntityKind::Field);
    assert_eq!(field.language, "rust");
    assert_eq!(field.container, Some(ContainerRef::Entity(owner.id)));
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: field.id }
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].source,
        source_for_span(&source, span_in(TEXT, &source, "value", 0))
    );
    assert_eq!(sites[0].syntactic_text_hash, content_hash(b"value"));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(field.id)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].evidence.source.as_ref(),
        Some(&source_for_span(
            &source,
            span_in(TEXT, &source, "value: u64", 0)
        ))
    );
}

#[test]
fn missing_ambiguous_or_invalid_field_names_never_fall_back_to_the_type_or_declaration_text() {
    let (_directory, snapshot, source) =
        source_fixture_for(TEXT, "src/lib.rs", b"rust-named-field-refusals");
    let mut missing = facts(&source);
    missing.pop();
    let mut ambiguous = facts(&source);
    ambiguous.push(SyntaxFact::new(
        6,
        Some(4),
        SyntaxFactKind::Occurrence,
        span_in(TEXT, &source, "u64", 0),
        3,
        label("rust.type_identifier.definition"),
    ));
    let mut invalid = facts(&source);
    invalid[4] = SyntaxFact::new(
        5,
        Some(4),
        SyntaxFactKind::Occurrence,
        span_in(TEXT, &source, "value: u64", 0),
        3,
        label("rust.field_identifier.definition"),
    );
    for (description, captures) in [
        ("missing name", missing),
        ("ambiguous type/name", ambiguous),
        ("invalid written name", invalid),
    ] {
        let budget = limits(IrLimits::default());
        let result = analyze_custom(&snapshot, &source, language(), &budget, captures).unwrap();
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Struct)
                .count(),
            1,
            "{description}"
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind != EntityKind::Field),
            "{description}"
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::Definition
                    || site.source.span() != span_in(TEXT, &source, "u64", 0)),
            "{description}"
        );
    }
}

#[test]
fn named_field_analysis_rejects_stale_content_hash_and_foreign_file_captures() {
    let (_directory, snapshot, source) =
        source_fixture_for(TEXT, "src/lib.rs", b"rust-named-field-source");
    let stale = SourceRef::new(
        source.repository(),
        source.generation(),
        source.span(),
        content_hash(b"stale field source"),
        None,
    );
    assert!(GenerationBoundSnapshot::new(&snapshot, &stale).is_err());
    let mut foreign = facts(&source);
    foreign[4] = SyntaxFact::new(
        5,
        Some(4),
        SyntaxFactKind::Occurrence,
        SourceSpan::new(rootlight_ids::FileId::from_bytes([99; 20]), 15, 20).unwrap(),
        3,
        label("rust.field_identifier.definition"),
    );
    let budget = limits(IrLimits::default());
    assert!(analyze_custom(&snapshot, &source, language(), &budget, foreign).is_err());
}
