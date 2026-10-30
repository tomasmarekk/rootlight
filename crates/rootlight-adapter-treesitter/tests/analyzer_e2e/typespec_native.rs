//! Native TypeSpec written owners retain exact definitions and conservative gaps.
//! Namespace intervals do not imply imported targets, decorator effects or type checking.

use super::*;

const TYPESPEC: LanguageCase = LanguageCase {
    name: "typespec",
    path: "src/written.tsp",
    frontend: "tree-sitter-typespec-0.0.1",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(TYPESPEC, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, TYPESPEC),
        &request(&fixture.snapshot, &fixture.source, TYPESPEC, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        let span = occurrence.source.span();
        let raw = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(occurrence.source.content_hash(), content_hash(source));
        assert_eq!(occurrence.syntactic_text_hash, content_hash(raw));
    }
    result
}

fn entity<'a>(
    result: &'a AnalysisOutput,
    kind: EntityKind,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let mut found = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == kind && entity.canonical_name == name);
    let entity = found
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"));
    assert!(found.next().is_none(), "ambiguous fixture: {kind:?} {name}");
    entity
}

fn owned(
    result: &AnalysisOutput,
    parent: &rootlight_ir::EntityRecord,
    child: &rootlight_ir::EntityRecord,
) {
    assert_eq!(
        child.container,
        Some(rootlight_ir::ContainerRef::Entity(parent.id))
    );
    assert!(
        result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id))
    );
}

#[test]
fn original_namespace_model_and_operation_have_native_written_owners() {
    let source = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../third_party/tree-sitter-typespec-0.0.1/tests/fixtures/namespace.tsp"
    ));
    let result = output(source);
    let doc = result.document();
    assert_eq!(doc.files[0].language, "typespec");
    assert!(
        doc.entities
            .iter()
            .all(|entity| entity.language == "typespec")
    );
    assert_eq!(doc.entities.len(), 6);
    let client = entity(&result, EntityKind::Namespace, "TspTest.NamespaceClient");
    let models = entity(&result, EntityKind::Namespace, "TspTest.NamespaceModel");
    let model = entity(&result, EntityKind::Struct, "Model");
    owned(
        &result,
        client,
        entity(&result, EntityKind::Function, "get"),
    );
    owned(&result, models, model);
    owned(
        &result,
        model,
        entity(&result, EntityKind::Property, "name"),
    );
    assert!(
        doc.relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_eq!(
        doc.occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
            .count(),
        5
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(doc.skipped_regions.iter().any(|gap| gap.domain == domain && gap.detail == "typespec-import-type-decorator-reference-resolution-and-other-declarations-unavailable"));
    }
    for occurrence in doc
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
    {
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            panic!("written definition has no symbol")
        };
        let target = doc
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            target.canonical_name.as_bytes()
        );
    }
}

#[test]
fn file_namespace_interval_owns_siblings_without_inventing_parameter_properties() {
    let source = "// 🌐 physical byte offsets\r\nnamespace Sample.Models;\r\nmodel First { value: string; }\r\nmodel Second { value?: string; }\r\ninterface Store { read(id: string): First; copy is Store.read; }\r\nop read(id: string): First;\r\nop copy is read;\r\n".as_bytes();
    let result = output(source);
    let doc = result.document();
    assert_eq!(doc.entities.len(), 11);
    let namespace = entity(&result, EntityKind::Namespace, "Sample.Models");
    for (kind, name) in [
        (EntityKind::Struct, "First"),
        (EntityKind::Struct, "Second"),
        (EntityKind::Interface, "Store"),
        (EntityKind::Function, "read"),
        (EntityKind::Function, "copy"),
    ] {
        owned(&result, namespace, entity(&result, kind, name));
    }
    let fields: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property && entity.canonical_name == "value")
        .collect();
    assert_eq!(fields.len(), 2);
    assert_ne!(fields[0].id, fields[1].id);
    for model_name in ["First", "Second"] {
        let model = entity(&result, EntityKind::Struct, model_name);
        let fields: Vec<_> = fields
            .iter()
            .filter(|field| field.container == Some(rootlight_ir::ContainerRef::Entity(model.id)))
            .collect();
        assert_eq!(fields.len(), 1);
        owned(&result, model, fields[0]);
    }
    let interface = entity(&result, EntityKind::Interface, "Store");
    owned(
        &result,
        interface,
        entity(&result, EntityKind::Method, "read"),
    );
    owned(
        &result,
        interface,
        entity(&result, EntityKind::Method, "copy"),
    );
    assert!(
        doc.entities
            .iter()
            .all(|entity| entity.canonical_name != "id")
    );
    assert!(
        doc.relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
}

#[test]
fn unsupported_owner_and_ambiguous_file_namespaces_cannot_donate_members() {
    let source = b"namespace `unsupported owner` { model Hidden { value: string; } }\nnamespace Good { model Kept { value: string; } }\n";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "Hidden")
    );
    let good = entity(&result, EntityKind::Namespace, "Good");
    let kept = entity(&result, EntityKind::Struct, "Kept");
    owned(&result, good, kept);
    for source in [
        b"namespace A; model One {} namespace B; model Two {}".as_slice(),
        b"namespace Outer { namespace Inner; model Hidden {} }".as_slice(),
    ] {
        let result = output(source);
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module)
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail
                    == "typespec-file-namespace-context-or-complete-native-proof-unavailable")
        );
    }
}

#[test]
fn malformed_native_owner_withholds_authored_declarations() {
    let result = output(b"namespace Broken { model Incomplete { value: ; } }\n");
    assert!(result.document().entities.iter().all(|entity| !matches!(
        entity.kind,
        EntityKind::Namespace | EntityKind::Struct | EntityKind::Property
    )));
    assert!(!result.document().skipped_regions.is_empty());
}

#[test]
fn ranges_cannot_hide_a_preceding_file_namespace() {
    let source = b"// prefix\nnamespace Hidden;\nnamespace Seen;\nmodel Value { field: string; }\n";
    let fixture = Fixture::new(TYPESPEC, source);
    let native = provider();
    let budget = limits();
    let start = source
        .windows(b"namespace Seen".len())
        .position(|window| window == b"namespace Seen")
        .unwrap();
    for spans in [
        vec![(start, source.len())],
        vec![(0, b"// prefix\n".len()), (start, source.len())],
    ] {
        let ranges = spans
            .into_iter()
            .map(|(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(
                        fixture.source.span().file(),
                        u64::try_from(start).unwrap(),
                        u64::try_from(end).unwrap(),
                    )
                    .unwrap(),
                    language(TYPESPEC),
                )
            })
            .collect();
        let ranged = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(TYPESPEC),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap();
        assert!(matches!(
            rootlight_adapter_sdk::execute_parse(
                &native,
                &ranged.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            ),
            Err(AdapterError::RejectedRequest(
                rootlight_adapter_sdk::RequestError::EmbeddedRangesUnsupported
            ))
        ));
    }
}
