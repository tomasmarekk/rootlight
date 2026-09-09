//! Objective-C written declarations through the public parser and IR boundary.
//! Selector parts keep source provenance; categories must not invent class entities.

use super::*;

pub(super) const OBJECTIVE_C: LanguageCase = LanguageCase {
    name: "objective-c",
    path: "src/example.m",
    frontend: "tree-sitter-objc-3.0.2",
    source: include_str!("../../../../tests/fixtures/objective-c/selectors.m"),
    generated: false,
    body_before: "return 1;",
    body_after: "return 2;",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(OBJECTIVE_C, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, OBJECTIVE_C),
        &request(&fixture.snapshot, &fixture.source, OBJECTIVE_C, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn objective_c_selector_parts_form_exact_names_without_parameter_text() {
    let source = include_str!("../../../../tests/fixtures/objective-c/selectors.m");
    let result = output(source);
    let methods: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Method)
        .collect();
    let names: Vec<_> = methods
        .iter()
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(
        methods.len(),
        4,
        "{names:?}; gaps: {:?}",
        result
            .document()
            .skipped_regions
            .iter()
            .map(|gap| &gap.detail)
            .collect::<Vec<_>>()
    );
    assert!(names.contains(&"::"));
    assert!(names.contains(&"perform::"));
    assert_eq!(names.iter().filter(|name| **name == "value").count(), 2);
    let values: Vec<_> = methods
        .iter()
        .filter(|entity| entity.canonical_name == "value")
        .collect();
    assert_ne!(values[0].id, values[1].id);
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.syntax_kind.ends_with("definition_part"))
    );
    for (name, written) in [
        ("::", ":(int)first :"),
        ("perform::", "perform:(int)first :"),
    ] {
        let entity = methods
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let definition = result.document().occurrences.iter().find(|occurrence|
            occurrence.role == OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)).unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            written
        );
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
}

#[test]
fn objective_c_property_declarators_keep_written_pointer_names() {
    let source = "@interface Sample\n@property int count;\n@property Sample *next;\n@end\n";
    let result = output(source);
    let mut names: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["count", "next"]);
}

#[test]
fn objective_c_partial_structure_does_not_claim_complete_language_analysis() {
    let result = output(include_str!(
        "../../../../tests/fixtures/objective-c/declarations.m"
    ));
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    for detail in [
        "objective-c-ivar-parameter-forward-and-preprocessed-declarations-unavailable",
        "objective-c-inheritance-message-dispatch-and-cross-file-binding-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.evidence.source.is_some())
        );
    }
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 4);
    assert!(
        calls
            .iter()
            .all(|call| matches!(call.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn objective_c_body_edits_preserve_every_authored_entity_identity() {
    let source = include_str!("../../../../tests/fixtures/objective-c/declarations.m");
    let fixture = Fixture::new(OBJECTIVE_C, source.as_bytes());
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, OBJECTIVE_C);
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, OBJECTIVE_C, &budget),
        &ExtensionSupport::default(),
    );
    let edited = source
        .replace("return left + right;", "return left + right + 1;")
        .replace("return [self add:1 to:1];", "return [self add:2 to:2];");
    assert_ne!(edited, source);
    let variant = fixture.rewrite(edited.as_bytes());
    let second = analyze(
        &analyzer,
        &request(&variant.snapshot, &variant.source, OBJECTIVE_C, &budget),
        &ExtensionSupport::default(),
    );
    let ids = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&first), ids(&second));
    assert_eq!(ids(&first).len(), first.document().entities.len());
    let implemented: Vec<_> = second
        .document()
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Method && entity.qualified_name == "Counter::add:to:"
        })
        .collect();
    assert_eq!(
        implemented.len(),
        1,
        "implementation uses its authored owner prefix"
    );
}

#[test]
fn objective_c_selector_identity_closure_is_atomic_at_the_fact_budget() {
    let source = include_str!("../../../../tests/fixtures/objective-c/selectors.m");
    let fixture = Fixture::new(OBJECTIVE_C, source.as_bytes());
    let provider = provider();
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, OBJECTIVE_C, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let exact = limits_with_syntax_records(required);
    let request = request(&fixture.snapshot, &fixture.source, OBJECTIVE_C, &exact);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "objective_c.selector.definition_part")
            .count(),
        7
    );
    let too_small = limits_with_syntax_records(required - 1);
    let rejected = rootlight_adapter_sdk::execute_parse(
        &provider,
        &super::request(&fixture.snapshot, &fixture.source, OBJECTIVE_C, &too_small)
            .to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    );
    assert!(matches!(
        rejected,
        Err(AdapterError::Sink(
            rootlight_adapter_sdk::SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
                ..
            }
        ))
    ));
}

#[test]
fn objective_c_parser_keeps_every_selector_component() {
    let source = include_str!("../../../../tests/fixtures/objective-c/selectors.m");
    let fixture = Fixture::new(OBJECTIVE_C, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, OBJECTIVE_C, &budget);
    let result = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let selectors: Vec<_> = result
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "objective_c.selector.definition_part")
        .collect();
    assert_eq!(selectors.len(), 7, "{selectors:?}");
    let signatures: Vec<_> = result
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "objective_c.method.signature")
        .map(|fact| {
            let span = fact.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            (text, fact.parent())
        })
        .collect();
    assert_eq!(
        signatures.iter().map(|(text, _)| *text).collect::<Vec<_>>(),
        [
            "- (int):(int)first :(int)second",
            "- (int)perform:(int)first :(int)second",
            "- (int)value",
            "+ (int)value"
        ],
        "{signatures:?}; {:?}",
        result
            .facts()
            .iter()
            .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature)
            .collect::<Vec<_>>()
    );
    for part in selectors {
        let mut owner = part.parent();
        while let Some(id) = owner {
            let fact = result
                .facts()
                .iter()
                .find(|fact| fact.local_id() == id)
                .unwrap();
            if rootlight_adapter_sdk::structural_entity_kind(fact).is_some() {
                assert_eq!(
                    fact.syntax_kind().as_str(),
                    "objective_c.method.declaration",
                    "{part:?}"
                );
                break;
            }
            owner = fact.parent();
        }
    }
}

#[test]
fn objective_c_categories_and_implementations_do_not_invent_classes() {
    let source = include_str!("../../../../tests/fixtures/objective-c/declarations.m");
    let result = output(source);
    let entities = &result.document().entities;
    let count = |kind| entities.iter().filter(|entity| entity.kind == kind).count();
    assert_eq!(count(EntityKind::Class), 2);
    assert_eq!(count(EntityKind::Protocol), 1);
    assert_eq!(count(EntityKind::Property), 1);
    assert_eq!(count(EntityKind::Method), 9);
    assert_eq!(
        entities
            .iter()
            .filter(|entity| entity.canonical_name == "add:to:")
            .count(),
        2
    );
}
