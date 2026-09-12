//! Groovy declarations through the public analyzer and generation-bound source evidence.
//! Parsing alone cannot establish discoverable entities or exact definition ownership.

use super::*;

pub(super) const GROOVY: LanguageCase = LanguageCase {
    name: "groovy",
    path: "src/catalog.groovy",
    frontend: "dekobon-tree-sitter-groovy-0.2.0",
    source: "class Meter { private int value; String read() { 'x' } }\nString render() { 'y' }\n",
    generated: false,
    body_before: "'x'",
    body_after: "'updated'",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GROOVY, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, GROOVY),
        &request(&fixture.snapshot, &fixture.source, GROOVY, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn groovy_declarations_keep_exact_definition_sources() {
    let result = output(GROOVY.source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Meter", EntityKind::Class),
        ("value", EntityKind::Field),
        ("read", EntityKind::Method),
        ("render", EntityKind::Function),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(entities.len(), 1, "{name}/{kind:?}: {:?}", doc.entities);
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: entities[0].id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            GROOVY.source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
}

#[test]
fn groovy_multiple_bindings_and_parameters_have_distinct_owners() {
    let source = "trait Named { String label() { 'name' } }\nclass Meter { private int first, second = 2; Meter(int seed) { first = seed }\nint read(int delta) { int left = 1, right = 2; return left + right + delta } }\nenum Mode { OPEN, CLOSED }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Named", EntityKind::Trait),
        ("label", EntityKind::Method),
        ("Meter", EntityKind::Class),
        ("Meter", EntityKind::Constructor),
        ("first", EntityKind::Field),
        ("second", EntityKind::Field),
        ("seed", EntityKind::Parameter),
        ("read", EntityKind::Method),
        ("delta", EntityKind::Parameter),
        ("left", EntityKind::Variable),
        ("right", EntityKind::Variable),
        ("Mode", EntityKind::Enum),
        ("OPEN", EntityKind::Constant),
        ("CLOSED", EntityKind::Constant),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}/{kind:?}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        assert!(entities[0].container.is_some(), "{name}");
    }
}

#[test]
fn groovy_body_edits_preserve_declaration_identity() {
    let before = output(GROOVY.source);
    let after = output(&GROOVY.source.replace(GROOVY.body_before, GROOVY.body_after));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.canonical_name.clone(), entity.id))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn groovy_dynamic_receiver_is_not_resolved_by_matching_method_name() {
    let source = "class Meter { int read() { 1 } }\ndef run(target) { target.read() }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let calls: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(!matches!(
        calls[0].target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(
        !doc.relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(doc.skipped_regions.iter().any(|gap| {
        gap.detail == "groovy-import-inheritance-and-runtime-dispatch-resolution-unavailable"
    }));
}
