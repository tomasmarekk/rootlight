//! Written C# method formals retain exact receiver References and source proof.
//! Native certificates, required retention and raw witness joins are independent.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/csharp_formal_observation/Extensions.cs");
const SIMPLE: &str = "class Catalog { object Owned(object value) { return value.Member; } }\n";
const CS: LanguageCase = LanguageCase {
    name: "csharp",
    path: "written.cs",
    frontend: "tree-sitter-c-sharp-0.23.5",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(case: LanguageCase, bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
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
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(bytes));
        assert_eq!(occurrence.source.repository(), fixture.source.repository());
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
        assert!(occurrence.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
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
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        definitions.len(),
        1,
        "one exact {kind:?} definition: {name}@{start}..{end}"
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must resolve to its own materialized entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn contains(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    member: SymbolId,
    start: u64,
    end: u64,
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == member)
        .unwrap();
    assert_eq!(
        entity.container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
    );
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    let source = relations[0]
        .evidence
        .source
        .as_ref()
        .expect("native ownership has exact source evidence");
    assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
}

fn resolved_at(
    document: &rootlight_ir::NormalizedIrDocument,
    role: OccurrenceRole,
    start: u64,
    end: u64,
    target: SymbolId,
) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|s| {
            s.role == role
                && s.source.span().start_byte() == start
                && s.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
}
fn no_definition(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64) {
    assert!(
        !document
            .occurrences
            .iter()
            .any(|s| s.role == OccurrenceRole::Definition
                && s.source.span().start_byte() == start
                && s.source.span().end_byte() == end)
    );
}
fn clean(bytes: &[u8]) {
    let provider = provider();
    let fixture = Fixture::new(CS, bytes);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, CS, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider,
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_ne!(
        parsed.report().coverage().status(),
        rootlight_ir::CoverageStatus::Unknown
    );
    assert!(
        !parsed
            .facts()
            .iter()
            .any(|f| f.kind() == rootlight_adapter_sdk::SyntaxFactKind::ErrorRecovery),
        "{}",
        String::from_utf8_lossy(bytes)
    );
}

fn span(site: &rootlight_ir::OccurrenceRecord) -> (u64, u64) {
    (
        site.source.span().start_byte(),
        site.source.span().end_byte(),
    )
}

fn reference(
    doc: &rootlight_ir::NormalizedIrDocument,
    at: (u64, u64),
) -> &rootlight_ir::OccurrenceRecord {
    let sites: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && span(site) == at)
        .collect();
    assert_eq!(sites.len(), 1, "one exact ordinary Reference at {at:?}");
    sites[0]
}

fn bound(
    result: &AnalysisOutput,
    raw: &[u8],
    owner_name: &str,
    name: &str,
    definition: (u64, u64),
    usage: (u64, u64),
) -> SymbolId {
    let doc = result.document();
    validate_ir_document(doc, limits().ir(), &ExtensionSupport::default()).unwrap();
    let owners: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Method && entity.canonical_name == owner_name)
        .collect();
    assert_eq!(owners.len(), 1);
    let formal = entity_at(doc, name, EntityKind::Parameter, definition.0, definition.1);
    contains(doc, owners[0].id, formal, definition.0, definition.1);
    assert!(!doc.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(formal)
            && edge.object == RelationEndpoint::Entity(owners[0].id)
    }));
    resolved_at(doc, OccurrenceRole::Reference, usage.0, usage.1, formal);
    no_definition(doc, usage.0, usage.1);
    let site = reference(doc, usage);
    assert_eq!(site.syntax_kind, "csharp.formal_receiver.reference");
    assert_eq!(site.enclosing, Some(owners[0].id));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.source.repository(), doc.repository);
    assert_eq!(site.source.generation(), doc.generation);
    assert!(doc.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(site.id)
            && edge.object == RelationEndpoint::Entity(formal)
            && edge.evidence.source.as_ref() == Some(&site.source)
            && edge.provenance == site.provenance
    }));
    formal
}

fn simple_binding(result: &AnalysisOutput, raw: &[u8], text: &str) {
    let declaration = u64::try_from(text.find("value)").unwrap()).unwrap();
    let usage = u64::try_from(text.rfind("value.").unwrap()).unwrap();
    bound(
        result,
        raw,
        "Owned",
        "value",
        (declaration, declaration + 5),
        (usage, usage + 5),
    );
}

#[test]
fn original_csharp_receiver_formal_preserves_all_exact_definitions_ownership_and_negatives() {
    assert_eq!(ORIGINAL.len(), 919);
    assert_eq!(&ORIGINAL[458..468], b"Extensions");
    assert_eq!(&ORIGINAL[521..538], b"EnumerateOverAxis");
    assert_eq!(&ORIGINAL[561..566], b"array");
    assert_eq!(&ORIGINAL[712..717], b"array");
    clean(ORIGINAL);
    let result = output(CS, ORIGINAL, EncodingId::utf8());
    let doc = result.document();
    let class = entity_at(doc, "Extensions", EntityKind::Class, 458, 468);
    let method = entity_at(doc, "EnumerateOverAxis", EntityKind::Method, 521, 538);
    contains(doc, class, method, 521, 538);
    bound(
        &result,
        ORIGINAL,
        "EnumerateOverAxis",
        "array",
        (561, 566),
        (712, 717),
    );
    no_definition(doc, 712, 717);
    assert_eq!(doc.files[0].content_hash, content_hash(ORIGINAL));
    assert!(
        !doc.diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error)
    );
}

#[test]
fn ordinary_receivers_are_same_method_formals_without_member_or_call_target_inference() {
    let text = "class Catalog { object Owned(object value) { return value.Member; } object Other(object value) { return value[0]; } }\n";
    clean(text.as_bytes());
    let result = output(CS, text.as_bytes(), EncodingId::utf8());
    simple_binding(
        &result,
        text.as_bytes(),
        &text[..text.find(" object Other").unwrap()],
    );
    let declaration = text.rfind("value)").unwrap() as u64;
    let usage = text.rfind("value[").unwrap() as u64;
    let other = bound(
        &result,
        text.as_bytes(),
        "Other",
        "value",
        (declaration, declaration + 5),
        (usage, usage + 5),
    );
    let owned = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "Owned")
        .unwrap();
    assert_ne!(
        result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == other)
            .unwrap()
            .container,
        Some(rootlight_ir::ContainerRef::Entity(owned.id))
    );
    let member = text.find("Member").unwrap() as u64;
    let site = reference(result.document(), (member, member + 6));
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn unsupported_binders_mutation_recovery_and_escaped_names_keep_formal_binding_unproved() {
    for text in [
        "class Catalog { object Owned(object value) { object value = other; return value.Member; } }",
        "class Catalog { object Owned(object value) { value = other; return value.Member; } }",
        "class Catalog { object Owned(object value) { value += other; return value.Member; } }",
        "class Catalog { object Owned(object value) { object Inner(object value) { return value.Member; } return value.Member; } }",
        "class Catalog { object Owned(object value) { var action = (object value) => value.Member; return value.Member; } }",
        "class Catalog { object Owned(object value) { foreach (var value in items) { External(value.Member); } return value.Member; } }",
        "class Catalog { object Owned(object value) { return value is object other ? value.Member : other; } }",
        "class Catalog { object Owned(object value) { var query = from value in items select value.Member; return value.Member; } }",
        "class Catalog { object Owned<T>(object value) { return value.Member; } }",
        "class Catalog { object Owned(object value, object value) { return value.Member; } }",
        "class Catalog { object Owned(object @value) { return value.Member; } }",
        "class Catalog { object Owned(object value) { return @value.Member; } }",
        "class Catalog { object Owned(object value) { return value.Member; ",
    ] {
        let result = output(CS, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result.document().occurrences.iter().any(|site| {
                matches!(
                    site.syntax_kind.as_str(),
                    "csharp.formal_receiver.reference" | "csharp.unproven_formal.reference"
                ) && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{text}"
        );
        assert_eq!(
            result.document().files[0].content_hash,
            content_hash(text.as_bytes())
        );
    }
}

#[test]
fn ordinary_optional_names_survive_certification_and_closed_body_decline() {
    for (text, certified) in [
        (
            "class Catalog { object Owned(object value) { External(value.Member); return value.Member; } }",
            true,
        ),
        (
            "class Catalog { object Owned(object value) { if (flag) { External(value.Member); } return value.Member; } }",
            false,
        ),
    ] {
        clean(text.as_bytes());
        let result = output(CS, text.as_bytes(), EncodingId::utf8());
        let external = text.find("External").unwrap() as u64;
        let site = reference(result.document(), (external, external + 8));
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        let usage = text.rfind("value.").unwrap() as u64;
        let site = reference(result.document(), (usage, usage + 5));
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            certified
        );
        if !certified {
            assert_eq!(site.syntax_kind, "csharp.unproven_formal.reference");
            assert!(!result.document().relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            }));
        }
    }
}

#[test]
fn utf8_bom_and_both_utf16_orders_preserve_physical_receiver_evidence() {
    let text = format!("// λ😀\r\n{SIMPLE}");
    let definition = text.find("value)").unwrap();
    let usage = text.rfind("value.").unwrap();
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
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let result = output(
            CS,
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        bound(
            &result,
            &bytes,
            "Owned",
            "value",
            (physical(definition), physical(definition + 5)),
            (physical(usage), physical(usage + 5)),
        );
    }
    let raw = format!("\u{feff}{text}").into_bytes();
    let result = output(CS, &raw, EncodingId::utf8());
    bound(
        &result,
        &raw,
        "Owned",
        "value",
        ((definition + 3) as u64, (definition + 8) as u64),
        ((usage + 3) as u64, (usage + 8) as u64),
    );
}

#[test]
fn provider_and_artifact_required_counts_keep_original_reference_at_exact_admission() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CS);
    let fixture = Fixture::new(CS, ORIGINAL);
    let limits = limits();
    let input = request(&fixture.snapshot, &fixture.source, CS, &limits);
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
    let input = request(&fixture.snapshot, &fixture.source, CS, &exact);
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
    let (bounded, retained) = frontend
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
    bound(
        &bounded,
        ORIGINAL,
        "EnumerateOverAxis",
        "array",
        (561, 566),
        (712, 717),
    );
    let next = fixture.next_generation();
    let current = request(&next.snapshot, &next.source, CS, &exact);
    let replayed = frontend
        .analyze_from_artifact(
            &current,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&frontend, &current, &ExtensionSupport::default());
    assert_eq!(replayed.document(), fresh.document());
    bound(
        &replayed,
        ORIGINAL,
        "EnumerateOverAxis",
        "array",
        (561, 566),
        (712, 717),
    );
    let small = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, CS, &small),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
    }) if observed == required && limit == required - 1),
        "{error:?}"
    );
    let cancel = Cancellation::new();
    assert!(cancel.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        frontend.analyze_from_artifact(
            &current,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let changed = fixture.rewrite(
        &ORIGINAL
            .iter()
            .copied()
            .chain(b"\n".iter().copied())
            .collect::<Vec<_>>(),
    );
    assert!(
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, CS, &exact),
        &retained, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn native_raw_witness_duplicates_and_missing_context_cannot_donate_formal_resolution() {
    let native = provider();
    let fixture = Fixture::new(CS, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, CS, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(CS),
            CS.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    let formal = bound(
        &baseline,
        ORIGINAL,
        "EnumerateOverAxis",
        "array",
        (561, 566),
        (712, 717),
    );
    let owner = baseline
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Method && entity.canonical_name == "EnumerateOverAxis"
        })
        .unwrap()
        .id;
    let identities = |doc: &rootlight_ir::NormalizedIrDocument| {
        doc.entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    for endpoint in [owner, formal] {
        let entity = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == endpoint)
            .unwrap();
        let own_span = entity.evidence.source.as_ref().unwrap().span();
        let declaration: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Declaration && fact.span() == own_span)
            .collect();
        assert_eq!(declaration.len(), 1);
        let declaration = declaration[0];
        let definitions: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "csharp.identifier.definition"
                    && fact.parent() == Some(declaration.local_id())
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        for witness in [declaration, definitions[0]] {
            let mut facts = parsed.facts().to_vec();
            let next = facts
                .iter()
                .map(SyntaxFact::local_id)
                .max()
                .unwrap()
                .checked_add(1)
                .unwrap();
            facts.push(SyntaxFact::new(
                next,
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.depth(),
                witness.syntax_kind().clone(),
            ));
            let changed = replay(facts);
            assert_eq!(
                identities(changed.document()),
                identities(baseline.document())
            );
            let site = reference(changed.document(), (712, 717));
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "duplicate raw {:?} for {}",
                witness.kind(),
                entity.canonical_name
            );
            assert!(
                !changed
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(site.id))
            );
        }
    }
    let contexts: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "csharp.formal_context.signature")
        .collect();
    assert_eq!(contexts.len(), 1);
    let no_context = replay(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.local_id() != contexts[0].local_id())
            .cloned()
            .collect(),
    );
    let site = reference(no_context.document(), (712, 717));
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(site.source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(site.source.repository(), no_context.document().repository);
    assert_eq!(site.source.generation(), no_context.document().generation);
    assert!(no_context.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.reason == SkippedRegionReason::UnsupportedConstruct
            && gap.source == site.source
            && gap.detail == "csharp-native-method-formal-receiver-binding-unavailable"
    }));
    assert!(!no_context.document().relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(site.id)
    }));
    let context = contexts[0];
    for duplicate in [false, true] {
        let mut facts = parsed.facts().to_vec();
        if duplicate {
            let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                next,
                context.parent(),
                context.kind(),
                context.span(),
                context.depth(),
                context.syntax_kind().clone(),
            ));
        } else {
            let wrong = SourceSpan::new(
                context.span().file(),
                context.span().start_byte() + 1,
                context.span().end_byte(),
            )
            .unwrap();
            let slot = facts
                .iter_mut()
                .find(|fact| fact.local_id() == context.local_id())
                .unwrap();
            *slot = SyntaxFact::new(
                context.local_id(),
                context.parent(),
                context.kind(),
                wrong,
                context.depth(),
                context.syntax_kind().clone(),
            );
        }
        let changed = replay(facts);
        assert_eq!(
            identities(changed.document()),
            identities(baseline.document())
        );
        assert!(
            matches!(
                reference(changed.document(), (712, 717)).target,
                OccurrenceTarget::Unresolved { .. }
            ),
            "duplicate or foreign method metadata cannot certify"
        );
    }
}

#[test]
fn body_only_method_edits_preserve_identity_with_current_source_and_atomic_ir_admission() {
    let first = output(CS, SIMPLE.as_bytes(), EncodingId::utf8());
    let changed = SIMPLE.replace("value.Member", "value.Other");
    let second = output(CS, changed.as_bytes(), EncodingId::utf8());
    let identities = |out: &AnalysisOutput| {
        out.document()
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
    assert_eq!(identities(&first), identities(&second));
    simple_binding(&second, changed.as_bytes(), &changed);
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CS);
    let fixture = Fixture::new(CS, SIMPLE.as_bytes());
    let base = limits();
    let run = |ir| {
        let bounds = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            ir,
        )
        .unwrap();
        frontend
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, CS, &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    simple_binding(&baseline, SIMPLE.as_bytes(), SIMPLE);
    for dimension in 0..4 {
        let default = IrLimits::default();
        let mut low = 0;
        let mut high = match dimension {
            0 => default.max_relations,
            1 => default.max_total_records,
            2 => default.max_total_nested_items,
            _ => default.max_nested_items_per_record,
        };
        let set = |limit| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_relations = limit,
                1 => ir.max_total_records = limit,
                2 => ir.max_total_nested_items = limit,
                _ => ir.max_nested_items_per_record = limit,
            };
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected resource prerequisite: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit }) if observed > limit && limit == low - 1 && resource == if dimension < 2 { ResourceKind::Records } else { ResourceKind::NestedItems }),
            "{error:?}"
        );
    }
}

const FIELD_ORIGINAL: &[u8] = include_bytes!("../fixtures/csharp_fields/Original.cs");

fn written_field(
    document: &rootlight_ir::NormalizedIrDocument,
    raw: &[u8],
    owner: SymbolId,
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let field = entity_at(document, name, EntityKind::Field, start, end);
    contains(document, owner, field, start, end);
    let definition = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .unwrap();
    let relation = document
        .relations
        .iter()
        .find(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(field)
        })
        .unwrap();
    let source = relation.evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(raw));
    assert_eq!(source.repository(), definition.source.repository());
    assert_eq!(source.generation(), definition.source.generation());
    assert_eq!(source.span().file(), definition.source.span().file());
    assert!(source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    field
}

#[test]
fn written_field_original_struct_and_field_keep_exact_definition_and_contains() {
    assert_eq!(FIELD_ORIGINAL.len(), 80);
    assert_eq!(&FIELD_ORIGINAL[44..45], b"S");
    assert_eq!(&FIELD_ORIGINAL[68..69], b"x");
    clean(FIELD_ORIGINAL);
    let result = output(CS, FIELD_ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let owner = entity_at(document, "S", EntityKind::Struct, 44, 45);
    written_field(document, FIELD_ORIGINAL, owner, "x", 68, 69);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Field)
            .count(),
        1
    );
}

#[test]
fn written_field_multi_declarators_keep_separate_names_and_one_type_owner() {
    let text = "class Catalog { public int left = 1, right, later = 3; }\n";
    let raw = text.as_bytes();
    clean(raw);
    let result = output(CS, raw, EncodingId::utf8());
    let document = result.document();
    let at = u64::try_from(text.find("Catalog").unwrap()).unwrap();
    let owner = entity_at(document, "Catalog", EntityKind::Class, at, at + 7);
    let mut ids = Vec::new();
    for name in ["left", "right", "later"] {
        let start = u64::try_from(text.find(name).unwrap()).unwrap();
        ids.push(written_field(
            document,
            raw,
            owner,
            name,
            start,
            start + u64::try_from(name.len()).unwrap(),
        ));
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Field)
            .count(),
        3
    );
}

#[test]
fn written_field_nested_owners_do_not_reclassify_properties_parameters_or_locals() {
    let text = "class Outer { int top; struct Inner { public int nested; } int Property { get; set; } void Run(int input) { int local = input; } }\n";
    let raw = text.as_bytes();
    clean(raw);
    let result = output(CS, raw, EncodingId::utf8());
    let document = result.document();
    let at = |name: &str| u64::try_from(text.find(name).unwrap()).unwrap();
    let outer = entity_at(
        document,
        "Outer",
        EntityKind::Class,
        at("Outer"),
        at("Outer") + 5,
    );
    let inner = entity_at(
        document,
        "Inner",
        EntityKind::Struct,
        at("Inner"),
        at("Inner") + 5,
    );
    contains(document, outer, inner, at("Inner"), at("Inner") + 5);
    written_field(document, raw, outer, "top", at("top"), at("top") + 3);
    let nested = written_field(
        document,
        raw,
        inner,
        "nested",
        at("nested"),
        at("nested") + 6,
    );
    entity_at(
        document,
        "Property",
        EntityKind::Property,
        at("Property"),
        at("Property") + 8,
    );
    entity_at(
        document,
        "input",
        EntityKind::Parameter,
        at("input"),
        at("input") + 5,
    );
    let mut names: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Field)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["nested", "top"]);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(outer)
            && edge.object == RelationEndpoint::Entity(nested)
    }));
}

#[test]
fn written_field_original_bom_and_both_utf16_orders_preserve_physical_coordinates() {
    let text = std::str::from_utf8(FIELD_ORIGINAL).unwrap();
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
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let result = output(
            CS,
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let owner = entity_at(
            result.document(),
            "S",
            EntityKind::Struct,
            physical(44),
            physical(45),
        );
        written_field(
            result.document(),
            &raw,
            owner,
            "x",
            physical(68),
            physical(69),
        );
    }
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(FIELD_ORIGINAL);
    let result = output(CS, &bom, EncodingId::utf8());
    let owner = entity_at(result.document(), "S", EntityKind::Struct, 47, 48);
    written_field(result.document(), &bom, owner, "x", 71, 72);
}
