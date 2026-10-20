//! Exact TypeScript interface fields in native files and Astro frontmatter.
//!
//! These checks establish source-written structure and ownership, without template binding claims.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/astro_source_fields/Grid.astro");

fn typescript_case(path: &'static str) -> LanguageCase {
    let mut case = *CASES.iter().find(|case| case.name == "typescript").unwrap();
    case.path = path;
    case.generated = false;
    case
}

fn analyze_bytes(case: LanguageCase, bytes: &[u8]) -> AnalysisOutput {
    analyze_encoded_bytes(case, bytes, EncodingId::utf8())
}

fn analyze_encoded_bytes(case: LanguageCase, bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(case.generated);
    let result = analyze(
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn assert_owned_field(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    owner_name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        definitions.len(),
        1,
        "one exact field-name definition: {name}"
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("field-name definition is unresolved")
    };
    let field = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(field.kind, EntityKind::Field);
    assert_eq!(field.language, "typescript");
    assert_eq!(field.canonical_name, name);
    let owners: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Interface && entity.canonical_name == owner_name
        })
        .collect();
    assert_eq!(owners.len(), 1);
    assert_eq!(
        field.container,
        Some(rootlight_ir::ContainerRef::Entity(owners[0].id))
    );
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owners[0].id)
                && relation.object == RelationEndpoint::Entity(field.id)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    let relation_source = relations[0]
        .evidence
        .source
        .as_ref()
        .expect("direct containment retains exact source evidence");
    assert!(relation_source.span().start_byte() <= start);
    assert!(relation_source.span().end_byte() >= end);
    assert_eq!(
        relation_source.content_hash(),
        definitions[0].source.content_hash()
    );
    symbol
}

#[test]
fn original_astro_interface_field_and_contains_use_exact_original_bytes() {
    assert_eq!(ORIGINAL.len(), 319);
    assert_eq!(&ORIGINAL[127..132], b"posts");
    let result = analyze_bytes(astro_native::ASTRO, ORIGINAL);
    assert_owned_field(result.document(), "posts", "Props", 127, 132);
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(ORIGINAL));
    }
}

#[test]
fn ordinary_readonly_optional_and_callable_typed_interface_fields_keep_native_owners() {
    let text = "export interface Outer<T> { readonly value: T; optional?: Array<T>; callback: (input: T) => T; nested: { child: string }; }\ninterface Other { separate: number; }\n";
    for path in ["src/interfaces.ts", "src/interfaces.tsx"] {
        let result = analyze_bytes(typescript_case(path), text.as_bytes());
        for (name, owner) in [
            ("value", "Outer"),
            ("optional", "Outer"),
            ("callback", "Outer"),
            ("nested", "Outer"),
            ("separate", "Other"),
        ] {
            let start = text.find(name).unwrap() as u64;
            assert_owned_field(
                result.document(),
                name,
                owner,
                start,
                start + name.len() as u64,
            );
        }
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter
                    && entity.canonical_name == "input")
        );
    }
}

#[test]
fn methods_computed_keys_and_object_values_are_not_ordinary_interface_fields() {
    let text = "declare const dynamic: unique symbol; interface Outer { written: string; run(): void; [dynamic]: string; } interface Other { separate: number; } const object = { written: 1, dynamic: 2 };\n";
    let result = analyze_bytes(typescript_case("src/interfaces.ts"), text.as_bytes());
    let document = result.document();
    for (name, owner) in [("written", "Outer"), ("separate", "Other")] {
        let start = text.find(&format!("{name}: ")).unwrap() as u64;
        assert_owned_field(document, name, owner, start, start + name.len() as u64);
    }
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "run")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Field
                && matches!(entity.canonical_name.as_str(), "run" | "dynamic"))
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "written")
            .count(),
        1
    );
}

#[test]
fn malformed_interface_owner_cannot_certify_fields_from_recovery() {
    let text = "interface Broken { missing: ; }\ninterface Healthy { safe: number; }\n";
    let result = analyze_bytes(typescript_case("src/interfaces.ts"), text.as_bytes());
    let document = result.document();
    let start = text.find("safe").unwrap() as u64;
    assert_owned_field(document, "safe", "Healthy", start, start + 4);
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "missing")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}

#[test]
fn encoded_astro_field_definitions_keep_physical_spans_and_current_source_hashes() {
    let text = "---\n// \u{394}\u{1f642}\ninterface View { value?: string; }\n---\n<main />\n";
    let logical = text.find("value").unwrap();
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let encoding = EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap();
        let result = analyze_encoded_bytes(astro_native::ASTRO, &bytes, encoding);
        let start = 2 + text[..logical].encode_utf16().count() * 2;
        let symbol = assert_owned_field(
            result.document(),
            "value",
            "View",
            start as u64,
            (start + 10) as u64,
        );
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol }
            })
            .unwrap();
        assert_eq!(definition.source.content_hash(), content_hash(&bytes));
        assert_eq!(definition.syntactic_text_hash, content_hash(b"value"));
        let written: Vec<_> = "value"
            .encode_utf16()
            .flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        assert_eq!(&bytes[start..start + 10], written);
    }
}

#[test]
fn interface_fields_replay_with_required_syntax_quota_and_cooperative_cancellation() {
    let case = astro_native::ASTRO;
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let exact = limits_with_syntax_records(required);
    let initial = request(&fixture.snapshot, &fixture.source, case, &exact);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        required,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    assert_owned_field(fresh.document(), "posts", "Props", 127, 132);
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, case, &exact);
    let replay = analyzer
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_owned_field(replay.document(), "posts", "Props", 127, 132);
    assert_eq!(
        fresh
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        replay
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.source.generation() == next.source.generation())
    );
    let small = limits_with_syntax_records(required - 1);
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, case, &small),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
        "{error:?}"
    );
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(&initial, &cancelled),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}
