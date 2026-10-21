//! Exact written Pug names must survive the public analyzer boundary.
//! Native ownership is required; rendered templates and embedded binding remain unknown.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/pug/hash.pug");
const PUG: LanguageCase = LanguageCase {
    name: "pug",
    path: "written.pug",
    frontend: "pug-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output() -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(PUG, ORIGINAL);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, PUG),
        &request(&fixture.snapshot, &fixture.source, PUG, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(
        result.document().files[0].content_hash,
        content_hash(ORIGINAL)
    );
    assert_eq!(result.document().files[0].byte_length, 718);
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.repository(), fixture.source.repository());
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
        assert_eq!(occurrence.source.content_hash(), content_hash(ORIGINAL));
        assert!(occurrence.source.span().end_byte() <= 718);
    }
    result
}

fn entity_at(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact {kind:?} {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("a written definition must resolve to its own entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    symbol
}

#[test]
fn pug_original_written_tag_and_attributes_have_exact_ownership() {
    assert_eq!(ORIGINAL.len(), 718);
    assert_eq!(&ORIGINAL[410..415], b"input");
    assert_eq!(&ORIGINAL[434..438], b"type");
    assert_eq!(&ORIGINAL[447..455], b"disabled");
    assert_eq!(&ORIGINAL[440..444], b"text");
    let result = output();
    let document = result.document();
    let input = entity_at(document, "input", EntityKind::MarkupElement, 410, 415);
    let attribute_type = entity_at(document, "type", EntityKind::MarkupAttribute, 434, 438);
    let disabled = entity_at(document, "disabled", EntityKind::MarkupAttribute, 447, 455);
    for (attribute, start, end) in [(attribute_type, 434, 438), (disabled, 447, 455)] {
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(input)
                    && edge.object == RelationEndpoint::Entity(attribute)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        let witness = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(witness.content_hash(), content_hash(ORIGINAL));
        assert!(witness.span().start_byte() <= start && witness.span().end_byte() >= end);
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == attribute)
                .unwrap()
                .container,
            Some(rootlight_ir::ContainerRef::Entity(input))
        );
    }
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 440
            && site.source.span().end_byte() == 444
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(attribute_type)
            && edge.object == RelationEndpoint::Entity(input)
    }));
}
