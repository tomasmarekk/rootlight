//! MATLAB source identities through the production parser and normalized IR.
//! Written applications remain distinct from proven runtime function calls.

use super::*;

pub(super) const MATLAB: LanguageCase = LanguageCase {
    name: "matlab",
    path: "src/measure.m",
    frontend: "tree-sitter-matlab-1.3.0",
    source: "function result = measure(value)\nresult = value + 1;\nend\n",
    generated: false,
    body_before: "value + 1",
    body_after: "value + 2",
};

const FUNCTIONS: &str =
    include_str!("../../../../tests/native-grammars/fixtures/matlab/functions.m");
const CLASS: &str = include_str!("../../../../tests/native-grammars/fixtures/matlab/Meter.m");

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MATLAB, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, MATLAB),
        &request(&fixture.snapshot, &fixture.source, MATLAB, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn matlab_functions_and_parameters_have_real_definition_sources() {
    let result = output(MATLAB.source);
    let entities = &result.document().entities;
    assert!(entities.iter().any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "measure"));
    assert!(
        entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
    );
    for name in ["measure", "value"] {
        let entity = entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let definitions: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &MATLAB.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
}

#[test]
fn matlab_class_members_and_accessors_have_exact_written_definitions() {
    let result = output(CLASS);
    for (name, kind) in [
        ("Meter", EntityKind::Class),
        ("Meter", EntityKind::Constructor),
        ("Value", EntityKind::Property),
        ("read", EntityKind::Method),
        ("get.Value", EntityKind::Method),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing {kind:?} {name}: {:?}", result.document().entities));
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &CLASS[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
}

#[test]
fn matlab_nested_functions_and_parameters_exclude_lexical_decoys() {
    for source in [FUNCTIONS.to_owned(), FUNCTIONS.replace('\n', "\r\n")] {
        let result = output(&source);
        for (name, kind) in [
            ("summarize", EntityKind::Function),
            ("adjust", EntityKind::Function),
            ("identity", EntityKind::Function),
            ("values", EntityKind::Parameter),
            ("scale", EntityKind::Parameter),
            ("item", EntityKind::Parameter),
            ("count", EntityKind::Variable),
        ] {
            assert!(
                result
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name && entity.kind == kind),
                "missing {kind:?} {name}: {:?}",
                result
                    .document()
                    .entities
                    .iter()
                    .map(|entity| (&entity.canonical_name, entity.kind))
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "fake" | "hidden"))
        );
        for occurrence in result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
        {
            let span = occurrence.source.span();
            let written = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(written.as_bytes())
            );
        }
    }
}

#[test]
fn matlab_signatures_exclude_bodies_and_preserve_accessor_prefixes() {
    for (source, expected) in [
        (MATLAB.source, vec!["function result = measure(value)"]),
        (
            CLASS,
            vec![
                "classdef Meter < handle",
                "function obj = Meter(value)",
                "function result = read(obj)",
                "function result = get.Value(obj)",
            ],
        ),
    ] {
        let fixture = Fixture::new(MATLAB, source.as_bytes());
        let budget = limits();
        let request = request(&fixture.snapshot, &fixture.source, MATLAB, &budget);
        let result = rootlight_adapter_sdk::execute_parse(
            &provider(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let signatures: Vec<_> = result
            .facts()
            .iter()
            .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature)
            .map(|fact| {
                let span = fact.span();
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            })
            .collect();
        assert_eq!(signatures, expected);
    }
}

#[test]
fn matlab_multiple_outputs_keep_the_function_name_capture() {
    let source =
        "function [first, second] = partition(values)\nfirst = values;\nsecond = values;\nend\n";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function
                && entity.canonical_name == "partition"),
        "{:?}",
        result
            .document()
            .entities
            .iter()
            .map(|entity| (&entity.canonical_name, entity.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn matlab_body_edits_preserve_written_symbol_identity() {
    let original = output(MATLAB.source);
    let updated = output(&MATLAB.source.replace(MATLAB.body_before, MATLAB.body_after));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| {
                (
                    entity.id,
                    (entity.kind, entity.canonical_name.clone(), entity.container),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&original), identities(&updated));
}

#[test]
fn matlab_applications_do_not_claim_calls_without_binding_evidence() {
    let source = "function result = select(values)\nresult = values(1);\ndisp result\nend\n";
    let result = output(source);
    let applications: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "matlab.application.reference")
        .collect();
    assert_eq!(applications.len(), 1);
    assert!(matches!(
        applications[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    );
    for detail in [
        "matlab-call-or-index-target-unavailable",
        "matlab-command-target-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "{detail}"
        );
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn matlab_artifact_replay_matches_fresh_ir_in_the_new_generation() {
    for source in [FUNCTIONS, CLASS] {
        let provider = Arc::new(provider());
        let budget = limits();
        let fixture = Fixture::new(MATLAB, source.as_bytes());
        let analyzer = analyzer(&provider, MATLAB);
        let (_, artifact) = analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, MATLAB, &budget),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let successor = fixture.next_generation();
        let request = request(&successor.snapshot, &successor.source, MATLAB, &budget);
        let reused = analyzer
            .analyze_from_artifact(
                &request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let fresh = analyze(&analyzer, &request, &ExtensionSupport::default());
        assert_eq!(reused.document(), fresh.document());
        assert_eq!(reused.report(), fresh.report());
        assert!(
            reused
                .document()
                .occurrences
                .iter()
                .all(|occurrence| occurrence.source.generation() == successor.source.generation())
        );
    }
}
