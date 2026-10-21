//! Written Scala formal references retain exact physical ownership.
//! Native proof, required retention and artifact reuse are checked independently.

use super::scala_native::SCALA;
use super::*;
use rootlight_ir::ContainerRef;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/scala_formal/endpoint.scala");
const SIMPLE: &str = "object Catalog { def owned(value: Int) = external(value) }\n";

fn run(raw: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(SCALA, raw);
    analyze(
        &analyzer(&native, SCALA),
        &request(&fixture.snapshot, &fixture.source, SCALA, &limits()),
        &ExtensionSupport::default(),
    )
}

fn binding(
    output: &AnalysisOutput,
    raw: &[u8],
    owner_name: &str,
    local_name: &str,
    declaration: (u64, u64),
    use_site: (u64, u64),
) {
    let doc = output.document();
    validate_ir_document(doc, limits().ir(), &ExtensionSupport::default()).unwrap();
    let owners: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Function && e.canonical_name == owner_name)
        .collect();
    assert_eq!(owners.len(), 1);
    let owner = owners[0];
    let locals: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| {
            e.kind == EntityKind::Parameter
                && e.canonical_name == local_name
                && e.container == Some(ContainerRef::Entity(owner.id))
        })
        .collect();
    assert_eq!(locals.len(), 1);
    let local = locals[0];
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|o| {
            o.role == OccurrenceRole::Definition
                && o.target == (OccurrenceTarget::Resolved { symbol: local.id })
                && (o.source.span().start_byte(), o.source.span().end_byte()) == declaration
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let uses: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|o| {
            (o.source.span().start_byte(), o.source.span().end_byte()) == use_site
                && o.role == OccurrenceRole::Reference
        })
        .collect();
    assert_eq!(uses.len(), 1);
    assert_eq!(
        uses[0].target,
        OccurrenceTarget::Resolved { symbol: local.id }
    );
    assert_eq!(uses[0].enclosing, Some(owner.id));
    for source in [definitions[0].source.clone(), uses[0].source.clone()] {
        assert_eq!(source.content_hash(), content_hash(raw));
        assert_eq!(source.repository(), doc.repository);
        assert_eq!(source.generation(), doc.generation);
        let start = usize::try_from(source.span().start_byte()).unwrap();
        let end = usize::try_from(source.span().end_byte()).unwrap();
        assert!(start < end && end <= raw.len());
    }
    assert!(
        doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(local.id)
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|s| s.span().start_byte() <= declaration.0
                        && s.span().end_byte() >= declaration.1))
    );
    assert!(
        !doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(local.id)
                && edge.object == RelationEndpoint::Entity(owner.id))
    );
    assert!(
        !doc.occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::Definition
                && (o.source.span().start_byte(), o.source.span().end_byte()) == use_site)
    );
}

#[test]
fn original_scala_formal_reference_keeps_exact_written_owner() {
    assert_eq!(ORIGINAL.len(), 3194);
    assert_eq!(&ORIGINAL[1386..1390], b"name");
    assert_eq!(&ORIGINAL[1425..1429], b"name");
    let output = run(ORIGINAL);
    binding(
        &output,
        ORIGINAL,
        "socket",
        "name",
        (1386, 1390),
        (1425, 1429),
    );
    let doc = output.document();
    let namespace: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Namespace && e.canonical_name == "Endpoint")
        .collect();
    assert_eq!(namespace.len(), 1);
    let function = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Function && e.canonical_name == "socket")
        .unwrap();
    assert_eq!(
        function.container,
        Some(ContainerRef::Entity(namespace[0].id))
    );
    for (entity, start, end) in [(namespace[0], 1184, 1192), (function, 1379, 1385)] {
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            (
                definitions[0].source.span().start_byte(),
                definitions[0].source.span().end_byte()
            ),
            (start, end)
        );
        assert_eq!(definitions[0].source.content_hash(), content_hash(ORIGINAL));
    }
    assert!(
        doc.relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::Contains
                && r.subject == RelationEndpoint::Entity(namespace[0].id)
                && r.object == RelationEndpoint::Entity(function.id))
    );
    assert!(doc.skipped_regions.iter().any(
        |gap| gap.detail == "scala-import-inheritance-implicit-dispatch-resolution-unavailable"
    ));
}

#[test]
fn directly_written_formals_keep_siblings_imports_and_expression_roles_separate() {
    for text in [
        SIMPLE.to_owned(),
        format!("import other._\n{SIMPLE}"),
        "object Catalog { def sibling(value: Int) = value; def owned(value: Int) = value + 1 }\n"
            .to_owned(),
    ] {
        let declaration = text.rfind("value: Int").unwrap();
        let usage = text.rfind("value").unwrap();
        binding(
            &run(text.as_bytes()),
            text.as_bytes(),
            "owned",
            "value",
            (
                u64::try_from(declaration).unwrap(),
                u64::try_from(declaration + 5).unwrap(),
            ),
            (
                u64::try_from(usage).unwrap(),
                u64::try_from(usage + 5).unwrap(),
            ),
        );
    }
    let text = "object Catalog { def owned(value: Int) = receiver.value(value) }\n";
    let output = run(text.as_bytes());
    let selected = text.rfind("value").unwrap();
    binding(
        &output,
        text.as_bytes(),
        "owned",
        "value",
        (
            text.find("value: Int").unwrap() as u64,
            (text.find("value: Int").unwrap() + 5) as u64,
        ),
        (
            u64::try_from(selected).unwrap(),
            u64::try_from(selected + 5).unwrap(),
        ),
    );
    let selector = text.find(".value").unwrap() + 1;
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(
                |o| o.source.span().start_byte() == u64::try_from(selector).unwrap()
                    && o.syntax_kind == "scala.ordinary_formal.reference"
            )
    );
}

#[test]
fn shadowing_mutation_nested_scope_defaults_and_recovery_keep_binding_unproved() {
    for text in [
        "object Catalog { def owned(value: Int) = { val value = 1; value } }",
        "object Catalog { def owned(value: Int) = { value = 1; value } }",
        "object Catalog { def owned(value: Int) = { value += 1; value } }",
        "object Catalog { def owned(value: Int) = { import other._; value } }",
        "object Catalog { def owned(value: Int) = { def inner(value: Int) = value; value } }",
        "object Catalog { def owned(value: Int) = ((value: Int) => value) }",
        "object Catalog { def owned(value: Int) = values.map(value => value) }",
        "object Catalog { def owned(value: Int) = value match { case value => value } }",
        "object Catalog { def owned(value: Int) = for (value <- values) yield value }",
        "object Catalog { def owned(value: Int = external) = value }",
        "object Catalog { def owned(value: Int, value: Int) = value }",
        "object Catalog { def owned(implicit value: Int) = value }",
        "object Catalog { def owned(value: => Int) = value }",
        "object Catalog { def owned(value: Int*) = value }",
        "object Catalog { def owned[T](value: T) = value }",
        "object Catalog { def owned(value: Int) = s\"$value\" }",
        "object Catalog { def owned(value: Int) = external(value) ",
    ] {
        let output = run(text.as_bytes());
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|o| o.syntax_kind == "scala.ordinary_formal.reference"
                    && matches!(o.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
        assert_eq!(
            output.document().files[0].content_hash,
            content_hash(text.as_bytes())
        );
    }
    let text = "object Catalog { def donor(value: Int) = value; def owned() = value }";
    let output = run(text.as_bytes());
    let declaration = text.find("value: Int").unwrap();
    let donor_use = text.find("= value;").unwrap() + 2;
    binding(
        &output,
        text.as_bytes(),
        "donor",
        "value",
        (
            u64::try_from(declaration).unwrap(),
            u64::try_from(declaration + 5).unwrap(),
        ),
        (
            u64::try_from(donor_use).unwrap(),
            u64::try_from(donor_use + 5).unwrap(),
        ),
    );
    let owned: Vec<_> = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "owned")
        .collect();
    assert_eq!(owned.len(), 1);
    let unbound = u64::try_from(text.rfind("value").unwrap()).unwrap();
    let uses: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == unbound
                && site.source.span().end_byte() == unbound + 5
        })
        .collect();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].enclosing, Some(owned[0].id));
    assert_eq!(uses[0].source.content_hash(), content_hash(text.as_bytes()));
    assert_eq!(uses[0].source.repository(), output.document().repository);
    assert_eq!(uses[0].source.generation(), output.document().generation);
    assert_eq!(uses[0].syntax_kind, "scala.unproven_formal.reference");
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::RefersTo
            && relation.subject == RelationEndpoint::Occurrence(uses[0].id)
    }));
}

#[test]
fn utf16_both_orders_and_utf8_bom_keep_physical_formal_reference() {
    let text = format!("// λ😀\r\n{SIMPLE}");
    let declaration = text.find("value: Int").unwrap();
    let usage = text.rfind("value").unwrap();
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let native = Arc::new(provider());
        let fixture = Fixture::new(SCALA, &raw);
        let bounds = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("scala").unwrap(),
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &bounds,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(
            &analyzer(&native, SCALA),
            &input,
            &ExtensionSupport::default(),
        );
        let physical = |byte| u64::try_from(2 + 2 * text[..byte].encode_utf16().count()).unwrap();
        binding(
            &output,
            &raw,
            "owned",
            "value",
            (physical(declaration), physical(declaration + 5)),
            (physical(usage), physical(usage + 5)),
        );
        let expected: Vec<_> = "value"
            .encode_utf16()
            .flat_map(|u| {
                if little {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                }
            })
            .collect();
        assert_eq!(
            &raw[usize::try_from(physical(usage)).unwrap()
                ..usize::try_from(physical(usage + 5)).unwrap()],
            expected
        );
    }
    let raw = format!("\u{feff}{SIMPLE}").into_bytes();
    let text = std::str::from_utf8(&raw).unwrap();
    let declaration = text.find("value: Int").unwrap();
    let usage = text.rfind("value").unwrap();
    binding(
        &run(&raw),
        &raw,
        "owned",
        "value",
        (
            u64::try_from(declaration).unwrap(),
            u64::try_from(declaration + 5).unwrap(),
        ),
        (
            u64::try_from(usage).unwrap(),
            u64::try_from(usage + 5).unwrap(),
        ),
    );
}

#[test]
fn exact_required_provider_and_artifact_admission_keeps_original_reference() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, SCALA);
    let fixture = Fixture::new(SCALA, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, SCALA, &bounds);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert!(required > 1);
    assert_eq!(
        required,
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap()
    );
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, SCALA, &exact);
    // Replay cannot discard the larger artifact's already retained optional facts.
    let mismatch = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(matches!(mismatch, AdapterError::ProviderFailed { code }
        if code.as_str() == "treesitter-structural-artifact-mismatch"));
    let (output, retained) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        retained.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert_eq!(
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    binding(
        &output,
        ORIGINAL,
        "socket",
        "name",
        (1386, 1390),
        (1425, 1429),
    );
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, SCALA, &exact);
    let replayed = frontend
        .analyze_from_artifact(
            &next_request,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&frontend, &next_request, &ExtensionSupport::default());
    assert_eq!(replayed.document(), fresh.document());
    binding(
        &replayed,
        ORIGINAL,
        "socket",
        "name",
        (1386, 1390),
        (1425, 1429),
    );
    let below = limits_with_syntax_records(required - 1);
    let input = request(&fixture.snapshot, &fixture.source, SCALA, &below);
    let error = frontend.prepare_artifact(&input, &deadline()).unwrap_err();
    assert!(
        matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::RequiredSyntaxFacts,observed,limit})if observed==required&&limit==required-1),
        "{error:?}"
    );
}

#[test]
fn exact_native_protocol_retains_formal_and_context_without_signature_identity_changes() {
    let native = Arc::new(provider());
    let fixture = Fixture::new(SCALA, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, SCALA, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        native.as_ref(),
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let facts = parsed.facts();
    let roots: Vec<_> = facts
        .iter()
        .filter(|f| f.kind() == rootlight_adapter_sdk::SyntaxFactKind::Root)
        .collect();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].syntax_kind().as_str(), "scala.file.root");
    for (label, start, end) in [
        ("scala.function.declaration", 1375, 1440),
        ("scala.parameter.declaration", 1386, 1390),
        ("scala.identifier.definition", 1379, 1385),
        ("scala.identifier.definition", 1386, 1390),
    ] {
        assert_eq!(
            facts
                .iter()
                .filter(|f| f.syntax_kind().as_str() == label
                    && f.span().start_byte() == start
                    && f.span().end_byte() == end)
                .count(),
            1,
            "{label}:{start}..{end}"
        );
    }

    let uses: Vec<_> = facts
        .iter()
        .filter(|f| {
            f.syntax_kind().as_str() == "scala.ordinary_formal.reference"
                && f.span().start_byte() == 1425
                && f.span().end_byte() == 1429
        })
        .collect();
    assert_eq!(uses.len(), 1);
    assert!(facts.iter().any(
        |f| f.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
            && f.syntax_kind().as_str() == "scala.formal_context.signature"
            && f.span().start_byte() <= 1386
            && f.span().end_byte() > 1429
    ));
    let frontend = analyzer(&native, SCALA);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let successor = fixture.next_generation();
    let current = request(&successor.snapshot, &successor.source, SCALA, &bounds);
    let rebound = frontend
        .analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    binding(
        &rebound,
        ORIGINAL,
        "socket",
        "name",
        (1386, 1390),
        (1425, 1429),
    );
    let changed = fixture.rewrite(
        &ORIGINAL
            .iter()
            .copied()
            .chain(b" ".iter().copied())
            .collect::<Vec<_>>(),
    );
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, SCALA, &bounds),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn body_only_edits_keep_written_symbol_identity_and_current_hash() {
    let first = run(SIMPLE.as_bytes());
    let changed = SIMPLE.replace("external(value)", "other(value + 1)");
    let second = run(changed.as_bytes());
    let identities = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&first), identities(&second));
    let declaration = changed.find("value: Int").unwrap();
    let usage = changed.rfind("value").unwrap();
    binding(
        &second,
        changed.as_bytes(),
        "owned",
        "value",
        (
            u64::try_from(declaration).unwrap(),
            u64::try_from(declaration + 5).unwrap(),
        ),
        (
            u64::try_from(usage).unwrap(),
            u64::try_from(usage + 5).unwrap(),
        ),
    );
    assert_ne!(
        first.document().files[0].content_hash,
        second.document().files[0].content_hash
    );
}
