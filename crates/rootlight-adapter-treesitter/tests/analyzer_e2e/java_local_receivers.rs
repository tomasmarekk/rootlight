//! Written Java local receivers retain exact References and source proof.
//! Native certificates, required retention and raw witness joins are independent.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/java_local_receiver/TestPipelineJava.java");
const SIMPLE: &str =
    "class Written { void owned() { Object receiver = external(); receiver.run(); } }\n";
const JAVA: LanguageCase = LanguageCase {
    name: "java",
    path: "Written.java",
    frontend: "tree-sitter-java-0.23.5",
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
    let formal = entity_at(doc, name, EntityKind::Variable, definition.0, definition.1);
    contains(doc, owners[0].id, formal, definition.0, definition.1);
    assert!(!doc.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(formal)
            && edge.object == RelationEndpoint::Entity(owners[0].id)
    }));
    resolved_at(doc, OccurrenceRole::Reference, usage.0, usage.1, formal);
    no_definition(doc, usage.0, usage.1);
    let site = reference(doc, usage);
    assert_eq!(site.syntax_kind, "java.local_receiver.reference");
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

fn clean(raw: &[u8]) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(raw, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "native prerequisite: {}",
        tree.root_node().to_sexp()
    );
}

fn simple_binding(result: &AnalysisOutput, raw: &[u8], text: &str) {
    let definition = u64::try_from(text.find("receiver =").unwrap()).unwrap();
    let usage = u64::try_from(text.rfind("receiver.").unwrap()).unwrap();
    bound(
        result,
        raw,
        "owned",
        "receiver",
        (definition, definition + 8),
        (usage, usage + 8),
    );
}

#[test]
fn original_java_local_receiver_keeps_exact_variable_method_ownership_and_reference() {
    clean(ORIGINAL);
    assert_eq!(ORIGINAL.len(), 1209);
    assert_eq!(&ORIGINAL[1020..1026], b"script");
    assert_eq!(&ORIGINAL[1082..1088], b"script");
    let result = output(JAVA, ORIGINAL, EncodingId::utf8());
    let class = entity_at(
        result.document(),
        "TestPipelineJava",
        EntityKind::Class,
        549,
        565,
    );
    let method = entity_at(
        result.document(),
        "should_return_clean_name",
        EntityKind::Method,
        975,
        999,
    );
    contains(result.document(), class, method, 975, 999);
    bound(
        &result,
        ORIGINAL,
        "should_return_clean_name",
        "script",
        (1020, 1026),
        (1082, 1088),
    );
    assert_eq!(
        ORIGINAL
            .windows(6)
            .filter(|bytes| *bytes == b"script")
            .count(),
        2
    );
    let doc = result.document();
    let receiver = reference(doc, (1082, 1088));
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| span(site) == (1082, 1088) && site.role == OccurrenceRole::CallSite)
    );
    assert!(
        !doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(receiver.id))
    );
}

#[test]
fn ordinary_prior_local_receivers_keep_exact_owners_without_initializer_or_type_inference() {
    for text in [
        SIMPLE,
        "class Written { Object receiver; void owned() { Object receiver = unknown(); receiver.run(); } }",
        "class Written { void sibling() { Object receiver = first(); receiver.run(); } void owned() { final Object receiver = other(); outer(receiver.run()).next(); } }",
        "class Written { void owned(Object input) { Object receiver = input; receiver.run(); } }",
    ] {
        clean(text.as_bytes());
        let result = output(JAVA, text.as_bytes(), EncodingId::utf8());
        let definition = u64::try_from(text.rfind("receiver =").unwrap()).unwrap();
        let usage = u64::try_from(text.rfind("receiver.").unwrap()).unwrap();
        bound(
            &result,
            text.as_bytes(),
            "owned",
            "receiver",
            (definition, definition + 8),
            (usage, usage + 8),
        );
    }
}

#[test]
fn competing_writes_scopes_unicode_escapes_and_recovery_withhold_local_receiver_binding() {
    for text in [
        "class Written { void owned() { Object receiver = first(); receiver = second(); receiver.run(); } }",
        "class Written { void owned() { int receiver = 1; receiver++; receiver.run(); } }",
        "class Written { void owned() { Object receiver = first(); Object receiver = second(); receiver.run(); } }",
        "class Written { void owned(Object receiver) { Object receiver = first(); receiver.run(); } }",
        "class Written { void owned() { Object receiver = first(); Runnable action = () -> receiver.run(); receiver.run(); } }",
        "class Written { void owned() { Object receiver = first(); class Nested { void run() {} } receiver.run(); } }",
        "class Written { void owned() { Object receiver = first(); { receiver.run(); } receiver.run(); } }",
        "class Written { void owned() { receiver.run(); Object receiver = first(); } }",
        "class Written { void sibling() { Object receiver = first(); } void owned() { receiver.run(); } }",
        "class Written { Object receiver; void owned() { receiver.run(); } }",
        "class Written { void owned(Object receiver) { receiver.run(); } }",
        "// \\u000a lexical translation\nclass Written { void owned() { Object receiver = first(); receiver.run(); } }",
    ] {
        clean(text.as_bytes());
        let result = output(JAVA, text.as_bytes(), EncodingId::utf8());
        let usage = u64::try_from(text.rfind("receiver.").unwrap()).unwrap();
        let site = reference(result.document(), (usage, usage + 8));
        assert_eq!(
            site.syntax_kind, "java.unproven_local_receiver.reference",
            "{text}"
        );
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{text}"
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)),
            "{text}"
        );
    }
    let text = b"class Written { void owned() { Object receiver = first(); receiver.run(); ";
    let result = output(JAVA, text, EncodingId::utf8());
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "java.local_receiver.reference"
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn ordinary_optional_names_survive_certification_and_declined_receiver_contexts() {
    for (text, proven) in [
        (
            "class Written { void owned() { Object receiver = first(); receiver.run(other); } }",
            true,
        ),
        (
            "class Written { void owned() { Object receiver = first(); if (flag) receiver.run(other); } }",
            false,
        ),
    ] {
        clean(text.as_bytes());
        let result = output(JAVA, text.as_bytes(), EncodingId::utf8());
        let other = u64::try_from(text.find("other").unwrap()).unwrap();
        let site = reference(result.document(), (other, other + 5));
        assert_eq!(site.syntax_kind, "java.identifier.reference");
        let receiver = u64::try_from(text.rfind("receiver.").unwrap()).unwrap();
        assert_eq!(
            matches!(
                reference(result.document(), (receiver, receiver + 8)).target,
                OccurrenceTarget::Resolved { .. }
            ),
            proven
        );
        assert_eq!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|site| span(site) == (receiver, receiver + 8)
                    && site.role == OccurrenceRole::Reference)
                .count(),
            1
        );
    }
}

#[test]
fn utf8_bom_crlf_and_both_utf16_orders_keep_physical_local_receiver_evidence() {
    let text = format!("// λ😀\r\n{SIMPLE}");
    let definition = text.find("receiver =").unwrap();
    let usage = text.rfind("receiver.").unwrap();
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
            })
        }
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let result = output(
            JAVA,
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        bound(
            &result,
            &raw,
            "owned",
            "receiver",
            (physical(definition), physical(definition + 8)),
            (physical(usage), physical(usage + 8)),
        );
    }
    let raw = format!("\u{feff}{text}").into_bytes();
    let result = output(JAVA, &raw, EncodingId::utf8());
    bound(
        &result,
        &raw,
        "owned",
        "receiver",
        (
            u64::try_from(definition + 3).unwrap(),
            u64::try_from(definition + 11).unwrap(),
        ),
        (
            u64::try_from(usage + 3).unwrap(),
            u64::try_from(usage + 11).unwrap(),
        ),
    );
}

#[test]
fn exact_required_provider_and_artifact_admission_keeps_original_local_receiver() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, JAVA);
    let fixture = Fixture::new(JAVA, ORIGINAL);
    let limits = limits();
    let input = request(&fixture.snapshot, &fixture.source, JAVA, &limits);
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
    let input = request(&fixture.snapshot, &fixture.source, JAVA, &exact);
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
        "should_return_clean_name",
        "script",
        (1020, 1026),
        (1082, 1088),
    );
    let next = fixture.next_generation();
    let current = request(&next.snapshot, &next.source, JAVA, &exact);
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
        "should_return_clean_name",
        "script",
        (1020, 1026),
        (1082, 1088),
    );
    let small = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, JAVA, &small),
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
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, JAVA, &exact),
        &retained, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn raw_witness_duplicates_missing_context_and_mismatches_withhold_local_receiver_binding() {
    let native = provider();
    let fixture = Fixture::new(JAVA, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, JAVA, &budget);
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
            language(JAVA),
            JAVA.frontend,
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
        "should_return_clean_name",
        "script",
        (1020, 1026),
        (1082, 1088),
    );
    let owner = baseline
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Method && entity.canonical_name == "should_return_clean_name"
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
                    && fact.syntax_kind().as_str() == "java.identifier.definition"
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
            let site = reference(changed.document(), (1082, 1088));
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
    let scopes: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "java.block.scope"
                && fact.span().start_byte() == 1002
                && fact.span().end_byte() == 1204
        })
        .collect();
    assert_eq!(scopes.len(), 1);
    let mut duplicate_scope = parsed.facts().to_vec();
    let scope = scopes[0];
    let next = duplicate_scope
        .iter()
        .map(SyntaxFact::local_id)
        .max()
        .unwrap()
        .checked_add(1)
        .unwrap();
    duplicate_scope.push(SyntaxFact::new(
        next,
        scope.parent(),
        scope.kind(),
        scope.span(),
        scope.depth(),
        scope.syntax_kind().clone(),
    ));
    let result = replay(duplicate_scope);
    let site = reference(result.document(), (1082, 1088));
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id))
    );
    let legacy: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.syntax_kind().as_str() == "java.setter_binding_context.signature"
                && fact.span().start_byte() == 959
                && fact.span().end_byte() == 1204
        })
        .collect();
    assert_eq!(legacy.len(), 1);
    let result = replay(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.local_id() != legacy[0].local_id())
            .cloned()
            .collect(),
    );
    assert!(matches!(
        reference(result.document(), (1082, 1088)).target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let contexts: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "java.local_receiver_context.signature")
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
    let site = reference(no_context.document(), (1082, 1088));
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(site.source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(site.source.repository(), no_context.document().repository);
    assert_eq!(site.source.generation(), no_context.document().generation);
    assert!(no_context.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.reason == SkippedRegionReason::UnsupportedConstruct
            && gap.source == site.source
            && gap.detail == "java-native-same-method-immutable-local-receiver-binding-unavailable"
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
                reference(changed.document(), (1082, 1088)).target,
                OccurrenceTarget::Unresolved { .. }
            ),
            "duplicate or foreign method metadata cannot certify"
        );
    }
}

#[test]
fn body_only_edits_keep_variable_identity_and_atomic_ir_admission() {
    let first = output(JAVA, SIMPLE.as_bytes(), EncodingId::utf8());
    let changed = SIMPLE.replace("receiver.run()", "receiver.next()");
    let second = output(JAVA, changed.as_bytes(), EncodingId::utf8());
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
    let frontend = analyzer(&native, JAVA);
    let fixture = Fixture::new(JAVA, SIMPLE.as_bytes());
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
                &request(&fixture.snapshot, &fixture.source, JAVA, &bounds),
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
