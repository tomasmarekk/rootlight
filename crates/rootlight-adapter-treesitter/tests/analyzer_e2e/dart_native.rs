//! Dart declarations through the production analyzer and retained source evidence.
//! Native parse success is not a substitute for entity identity and owner assertions.

use super::*;

const DART: LanguageCase = LanguageCase {
    name: "dart",
    path: "src/catalog.dart",
    frontend: "tree-sitter-dart-0.2.0",
    source: "int count() => 1;",
    generated: false,
    body_before: "=> 1",
    body_after: "=> 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(DART, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, DART),
        &request(&fixture.snapshot, &fixture.source, DART, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn dart_declarations_operators_and_constructor_names_keep_exact_definitions() {
    let source = "typedef Callback = int Function(int value);\nmixin Counting { int count = 0; }\nclass Store<T> with Counting { final T value; Store(this.value); Store.named(this.value); int read(int input) => input; int get size => count; set size(int next) { count = next; } Store<T> operator /(Store<T> other) => this; int operator [](int index) => index; }\nenum Phase { open, closed }\nextension TextLength on String { int get doubled => length * 2; }\nextension type UserId(int raw) {}\nint run(int input) => input;";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind, count) in [
        ("Callback", EntityKind::TypeAlias, 1),
        ("Counting", EntityKind::Trait, 1),
        ("Store", EntityKind::Class, 1),
        ("Store", EntityKind::Constructor, 1),
        ("Store.named", EntityKind::Constructor, 1),
        ("read", EntityKind::Method, 1),
        ("size", EntityKind::Method, 2),
        ("/", EntityKind::Method, 1),
        ("[]", EntityKind::Method, 1),
        ("Phase", EntityKind::Enum, 1),
        ("open", EntityKind::Constant, 1),
        ("TextLength", EntityKind::Namespace, 1),
        ("UserId", EntityKind::Class, 1),
        ("raw", EntityKind::Field, 1),
        ("run", EntityKind::Function, 1),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.canonical_name == name && e.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            count,
            "{name}/{kind:?}: {:?}; gaps: {:?}",
            doc.entities,
            doc.skipped_regions
        );
        for entity in entities {
            let definitions: Vec<_> = doc
                .occurrences
                .iter()
                .filter(|o| {
                    o.role == OccurrenceRole::Definition
                        && o.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .collect();
            assert_eq!(definitions.len(), 1, "{name}");
            let span = definitions[0].source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(name)
            );
        }
    }
    assert!(
        doc.skipped_regions.iter().any(
            |g| g.detail == "dart-import-inheritance-extension-dispatch-resolution-unavailable"
        )
    );
}

#[test]
fn dart_disjoint_bindings_and_callable_owners_survive_body_edits() {
    let source = "class Store { int read(int input) { { final value = 1; } { final value = 2; } return input; } }\nvoid run() { final (left, right) = (1, 2); }";
    let initial = output(source);
    let changed = output(&source.replace("return input;", "return input + 1;"));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&initial), identities(&changed));
    let doc = initial.document();
    for (name, count) in [("value", 2), ("left", 1), ("right", 1)] {
        assert_eq!(
            doc.entities
                .iter()
                .filter(|e| e.canonical_name == name && e.kind == EntityKind::Variable)
                .count(),
            count,
            "{name}: {:?}; gaps: {:?}",
            doc.entities,
            doc.skipped_regions
        );
    }
    let read = doc
        .entities
        .iter()
        .find(|e| e.canonical_name == "read")
        .unwrap();
    let input = doc
        .entities
        .iter()
        .find(|e| e.canonical_name == "input")
        .unwrap();
    assert_eq!(
        input.container,
        Some(rootlight_ir::ContainerRef::Entity(read.id))
    );
}

#[test]
fn dart_callable_headers_retain_exact_source_without_bodies_or_initializers() {
    for (name, header, declaration) in [
        (
            "read",
            "int read(\nint input\n)",
            "int read(\nint input\n) => input + 1;",
        ),
        (
            "Store.named",
            "Store.named(int input)",
            "Store.named(int input) : value = input + 1 {}",
        ),
        (
            "/",
            "int operator /(int divisor)",
            "int operator /(int divisor) => value;",
        ),
    ] {
        let source = format!("class Store {{ final int value; {declaration} }}");
        let result = output(&source);
        assert!(result.document().diagnostics.is_empty());
        let entity = result
            .document()
            .entities
            .iter()
            .find(|e| e.canonical_name == name)
            .unwrap();
        let signatures = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|envelope| {
                let evidence = rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap();
                (evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && evidence.subject() == rootlight_ir::FactRef::Entity(entity.id))
                .then_some((envelope, evidence))
            })
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1, "{name}");
        let (envelope, evidence) = &signatures[0];
        assert_eq!(evidence.text(), header);
        assert!(!evidence.is_truncated());
        let span = envelope.evidence.source.as_ref().unwrap().span();
        let start = usize::try_from(span.start_byte()).unwrap();
        let end = usize::try_from(span.end_byte()).unwrap();
        assert_eq!(source.get(start..end), Some(header));
    }
}

#[test]
fn dart_artifact_replay_preserves_identity_and_rebinds_source_generation() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, DART);
    let fixture = Fixture::new(
        DART,
        b"class Store { Store.named(); int read(int input) => input; }",
    );
    let budget = limits();
    let initial_request = request(&fixture.snapshot, &fixture.source, DART, &budget);
    let (initial, artifact) = analyzer
        .analyze_and_capture(
            &initial_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let successor = fixture.next_generation();
    let reduced_limits =
        limits_with_syntax_records(artifact.syntax_fact_count().checked_add(1).unwrap());
    let successor_request = request(
        &successor.snapshot,
        &successor.source,
        DART,
        &reduced_limits,
    );
    let reused = analyzer
        .analyze_from_artifact(
            &successor_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &successor_request, &ExtensionSupport::default());
    assert_eq!(reused.document(), fresh.document());
    assert_eq!(reused.report(), fresh.report());
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|e| (e.id, e.container))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&initial), identities(&reused));
    assert!(reused.document().entities.iter().all(|e| {
        e.evidence
            .source
            .as_ref()
            .is_some_and(|r| r.generation() == successor.source.generation())
    }));
}

#[test]
fn dart_constructor_trivia_preserves_canonical_identity_and_written_definition() {
    let compact = output("class Store { Store.named(); }");
    let constructor = compact
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Constructor && entity.canonical_name == "Store.named"
        })
        .unwrap();
    for written in [
        "Store . named",
        "Store /* outer /* nested */ comment */ . named",
        "Store // qualifier\r\n . named",
    ] {
        let source = format!("class Store {{ {written}(); }}");
        let result = output(&source);
        assert!(result.document().diagnostics.is_empty());
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Constructor && entity.canonical_name == "Store.named"
            })
            .expect("formatted constructor remains indexed");
        assert_eq!(entity.id, constructor.id);
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        let headers = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .map(|envelope| rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap())
            .filter(|evidence| {
                evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && evidence.subject() == rootlight_ir::FactRef::Entity(entity.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].text(), format!("{written}()"));
    }
    let changed = output("class Store { Store.named(int value); }");
    let overloaded = changed
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Constructor && entity.canonical_name == "Store.named"
        })
        .unwrap();
    assert_ne!(overloaded.id, constructor.id);
}

#[test]
fn dart_literal_text_does_not_fabricate_entities_or_hide_parse_gaps() {
    let source =
        "/* class Ghost {} */\nclass Real { String text = r'class Phantom {}'; int read() => 1; }";
    let result = output(source);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|e| matches!(e.canonical_name.as_str(), "Ghost" | "Phantom"))
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|e| e.canonical_name == "Real")
    );
    let malformed = output("class Healthy {}\nclass Broken {");
    assert!(
        malformed
            .document()
            .entities
            .iter()
            .any(|e| e.canonical_name == "Healthy")
    );
    assert!(
        !malformed.document().diagnostics.is_empty()
            || malformed
                .document()
                .skipped_regions
                .iter()
                .any(|g| g.reason == rootlight_ir::SkippedRegionReason::ParseError)
    );
}
