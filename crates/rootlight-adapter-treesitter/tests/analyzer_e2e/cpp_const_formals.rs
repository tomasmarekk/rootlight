//! Written C++ function formals retain exact receiver References and source proof.
//! Native certificates, required retention and raw witness joins are independent.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/cpp_waforth_formal/main.cpp");
const SIMPLE: &str = "void Owned(const Value &value) { External(value[0]); }\n";
const CPP: LanguageCase = LanguageCase {
    name: "cpp",
    path: "written.cpp",
    frontend: "tree-sitter-cpp-0.23.4",
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
    let fixture = Fixture::new(CPP, bytes);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, CPP, &budget);
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
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == owner_name)
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
    assert_eq!(site.syntax_kind, "cpp.const_formal_receiver.reference");
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
    let usage = u64::try_from(text.find("value[").unwrap()).unwrap();
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
fn original_cpp_const_reference_formal_preserves_exact_owner_reference_and_negatives() {
    assert_eq!(ORIGINAL.len(), 7028);
    assert_eq!(&ORIGINAL[814..821], b"emit_cb");
    assert_eq!(&ORIGINAL[866..872], b"params");
    assert_eq!(&ORIGINAL[935..941], b"params");
    let result = output(CPP, ORIGINAL, EncodingId::utf8());
    let doc = result.document();
    let owner = entity_at(doc, "emit_cb", EntityKind::Function, 814, 821);
    let formal = bound(
        &result,
        ORIGINAL,
        "emit_cb",
        "params",
        (866, 872),
        (935, 941),
    );
    assert_ne!(owner, formal);
    no_definition(doc, 935, 941);
    assert_eq!(doc.files[0].content_hash, content_hash(ORIGINAL));
    assert!(
        doc.coverage_records
            .iter()
            .any(|coverage| coverage.status != CoverageStatus::Complete)
    );
    assert!(
        !doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}

#[test]
fn ordinary_const_subscript_receivers_keep_same_function_targets_and_no_calls() {
    let text = "void Owned(const Value &value) { External(value[0]); } void Other(const Value &value) { External(value[1]); }\n";
    clean(text.as_bytes());
    let result = output(CPP, text.as_bytes(), EncodingId::utf8());
    simple_binding(&result, text.as_bytes(), text);
    let definition = u64::try_from(text.rfind("value)").unwrap()).unwrap();
    let usage = u64::try_from(text.rfind("value[").unwrap()).unwrap();
    let formal = bound(
        &result,
        text.as_bytes(),
        "Other",
        "value",
        (definition, definition + 5),
        (usage, usage + 5),
    );
    let own = u64::try_from(text.find("value[").unwrap()).unwrap();
    let own_site = reference(result.document(), (own, own + 5));
    assert_ne!(
        own_site.target,
        OccurrenceTarget::Resolved { symbol: formal }
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn shadow_scopes_macros_mutations_templates_and_qualified_uses_cannot_borrow_formals() {
    for text in [
        "void Owned(const Value &value) { Value value; External(value[0]); }",
        "void Owned(const Value &value) { { External(value[0]); } }",
        "void Owned(const Value &value) { auto f = [&] { return value[0]; }; External(value[0]); }",
        "template<class T> void Owned(const T &value) { External(value[0]); }",
        "void Owned(const Value &value, const Value &value) { External(value[0]); }",
        "void Owned(const Value &&value) { External(value[0]); }",
        "void Owned(Value &value) { External(value[0]); }",
        "void Owned(const Value &value) { value[0] = 1; External(value[0]); }",
        "void Owned(const Value &value) { External(space::value[0]); }",
        "#define value other\nvoid Owned(const Value &value) { External(value[0]); }",
        "#if ENABLED\nvoid Owned(const Value &value) { External(value[0]); }\n#endif",
        "void Owned(const Value &value) {\n#if ENABLED\n External(value[0]);\n#endif\n}",
    ] {
        clean(text.as_bytes());
        let result = output(CPP, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result.document().occurrences.iter().any(|site| matches!(
                site.syntax_kind.as_str(),
                "cpp.const_formal_receiver.reference" | "cpp.unproven_const_formal.reference"
            ) && matches!(
                site.target,
                OccurrenceTarget::Resolved { .. }
            )),
            "{text}"
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
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
            "void Owned(const Value &value) { External(value[0]); }",
            true,
        ),
        (
            "void Owned(const Value &value) { if (flag) { External(value[0]); } }",
            false,
        ),
    ] {
        clean(text.as_bytes());
        let result = output(CPP, text.as_bytes(), EncodingId::utf8());
        let external = u64::try_from(text.find("External").unwrap()).unwrap();
        assert!(matches!(
            reference(result.document(), (external, external + 8)).target,
            OccurrenceTarget::Unresolved { .. }
        ));
        let usage = u64::try_from(text.rfind("value[").unwrap()).unwrap();
        let site = reference(result.document(), (usage, usage + 5));
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            certified
        );
        if !certified {
            assert_eq!(site.syntax_kind, "cpp.unproven_const_formal.reference");
            assert!(
                !result
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(site.id))
            );
        }
    }
}

#[test]
fn utf8_bom_and_both_utf16_orders_preserve_physical_receiver_evidence() {
    let text = format!("// λ😀\r\n{SIMPLE}");
    let definition = text.find("value)").unwrap();
    let usage = text.rfind("value[").unwrap();
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
            CPP,
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
    let result = output(CPP, &raw, EncodingId::utf8());
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
    let frontend = analyzer(&native, CPP);
    let fixture = Fixture::new(CPP, ORIGINAL);
    let limits = limits();
    let input = request(&fixture.snapshot, &fixture.source, CPP, &limits);
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
    let input = request(&fixture.snapshot, &fixture.source, CPP, &exact);
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
        "emit_cb",
        "params",
        (866, 872),
        (935, 941),
    );
    let next = fixture.next_generation();
    let current = request(&next.snapshot, &next.source, CPP, &exact);
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
        "emit_cb",
        "params",
        (866, 872),
        (935, 941),
    );
    let small = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, CPP, &small),
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
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, CPP, &exact),
        &retained, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn native_raw_witness_duplicates_and_missing_context_cannot_donate_formal_resolution() {
    let native = provider();
    let fixture = Fixture::new(CPP, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
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
            language(CPP),
            CPP.frontend,
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
        "emit_cb",
        "params",
        (866, 872),
        (935, 941),
    );
    let owner = baseline
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "emit_cb")
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
                    && fact.syntax_kind().as_str() == "cpp.identifier.definition"
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
            let site = reference(changed.document(), (935, 941));
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
        .filter(|fact| fact.syntax_kind().as_str() == "cpp.const_formal_context.signature")
        .collect();
    assert!(!contexts.is_empty());
    let contexts: Vec<_> = contexts
        .into_iter()
        .filter(|fact| fact.span().start_byte() == 801 && fact.span().end_byte() == 995)
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
    let site = reference(no_context.document(), (935, 941));
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(site.source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(site.source.repository(), no_context.document().repository);
    assert_eq!(site.source.generation(), no_context.document().generation);
    assert!(no_context.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.reason == SkippedRegionReason::UnsupportedConstruct
            && gap.source == site.source
            && gap.detail == "cpp-native-const-reference-formal-receiver-binding-unavailable"
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
                reference(changed.document(), (935, 941)).target,
                OccurrenceTarget::Unresolved { .. }
            ),
            "duplicate or foreign function metadata cannot certify"
        );
    }
}

#[test]
fn body_only_function_edits_preserve_identity_with_current_source_and_atomic_ir_admission() {
    let first = output(CPP, SIMPLE.as_bytes(), EncodingId::utf8());
    let changed = SIMPLE.replace("value[0]", "value[1]");
    let second = output(CPP, changed.as_bytes(), EncodingId::utf8());
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
    let frontend = analyzer(&native, CPP);
    let fixture = Fixture::new(CPP, SIMPLE.as_bytes());
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
                &request(&fixture.snapshot, &fixture.source, CPP, &bounds),
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

#[test]
fn ordinary_named_formals_preserve_written_owners_without_receiver_binding() {
    for text in [
        "void Owned(Value input, int count) { External(input[0]); }\n",
        "#if ENABLED\nvoid Owned(Value input, int count) { External(input[0]); }\n#endif\n",
    ] {
        let result = output(CPP, text.as_bytes(), EncodingId::new("utf-8").unwrap());
        let document = result.document();
        let at = u64::try_from(text.find("Owned").unwrap()).unwrap();
        let owner = entity_at(document, "Owned", EntityKind::Function, at, at + 5);
        for (name, size) in [("input", 5), ("count", 5)] {
            let at = u64::try_from(text.find(name).unwrap()).unwrap();
            let formal = entity_at(document, name, EntityKind::Parameter, at, at + size);
            contains(document, owner, formal, at, at + size);
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(formal)
                        && edge.object == RelationEndpoint::Entity(owner))
            );
        }
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "cpp.const_formal_receiver.reference")
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations)
        );
    }
}

#[test]
fn prototype_function_pointer_lambda_duplicate_and_local_names_are_not_ordinary_formals() {
    for (text, name) in [
        ("void Owned(Value input);", "input"),
        ("auto lambda = [](Value input) { return input; };", "input"),
        ("void Owned(Value input, Value input) {}", "input"),
        ("void Owned() { Value local; }", "local"),
        ("void Owned(void (*callback)(int nested)) {}", "nested"),
    ] {
        let result = output(CPP, text.as_bytes(), EncodingId::new("utf-8").unwrap());
        assert!(!result.document().entities.iter().any(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == name), "{text}");
    }
}

#[test]
fn ordinary_written_formals_keep_physical_utf8_bom_and_utf16_sources() {
    let text = "void Owned(Value input) {}\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let (raw, prefix, unit) = if encoding == "utf-8" {
            ([b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(), 3, 1)
        } else {
            let raw = std::iter::once(0xfeff_u16)
                .chain(text.encode_utf16())
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            (raw, 2, 2)
        };
        let result = output(CPP, &raw, EncodingId::new(encoding).unwrap());
        let owner = entity_at(
            result.document(),
            "Owned",
            EntityKind::Function,
            prefix + 5 * unit,
            prefix + 10 * unit,
        );
        let formal = entity_at(
            result.document(),
            "input",
            EntityKind::Parameter,
            prefix + 17 * unit,
            prefix + 22 * unit,
        );
        contains(
            result.document(),
            owner,
            formal,
            prefix + 17 * unit,
            prefix + 22 * unit,
        );
    }
}
