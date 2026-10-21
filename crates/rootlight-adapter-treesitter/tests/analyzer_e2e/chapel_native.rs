//! Exact Chapel originals and finite structural-provider qualification controls.
use super::*;
const ORIGINAL: &[u8] = include_bytes!("../fixtures/chapel/genhashkey32.chpl");
const CHAPEL: LanguageCase = LanguageCase {
    name: "chapel",
    path: "written.chpl",
    frontend: "chapel-native-source",
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
fn exact_call(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    target: SymbolId,
    start: u64,
    end: u64,
) {
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|r| {
            r.predicate == RelationPredicate::Calls
                && r.subject == RelationEndpoint::Entity(owner)
                && r.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    let source = edges[0].evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
}
#[test]
fn original_chapel_written_headers_ownership_and_negative_obligations_remain_exact() {
    let result = output(CHAPEL, ORIGINAL, EncodingId::utf8());
    original_structure(result.document());
    assert_eq!(ORIGINAL.len(), 761);
    assert_eq!(
        ORIGINAL
            .windows(b"GenHashKey32".len())
            .filter(|window| *window == b"GenHashKey32")
            .count(),
        1
    );
    assert!(!ORIGINAL.windows(b"__rootlight_absent_5e93c0b4409cd0928cd47a876ba82b94d653b490f6a3dd2f36e9be832317dc47__".len()).any(|window| window == b"__rootlight_absent_5e93c0b4409cd0928cd47a876ba82b94d653b490f6a3dd2f36e9be832317dc47__"));
}

fn original_structure(
    document: &rootlight_ir::NormalizedIrDocument,
) -> (SymbolId, SymbolId, SymbolId) {
    let module = entity_at(document, "GenHashKey32", EntityKind::Module, 75, 87);
    let first = entity_at(document, "genHashKey32", EntityKind::Function, 165, 177);
    let formal = entity_at(document, "h", EntityKind::Parameter, 178, 179);
    let other = entity_at(document, "genHashKey32", EntityKind::Function, 461, 473);
    contains(document, module, first, 165, 177);
    contains(document, first, formal, 178, 179);
    contains(document, module, other, 461, 473);
    no_definition(document, 637, 649);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(formal)
                && edge.object == RelationEndpoint::Entity(first))
    );
    (first, formal, other)
}

#[test]
fn original_chapel_formal_read_remains_a_mandatory_unclosed_requirement() {
    let result = output(CHAPEL, ORIGINAL, EncodingId::utf8());
    let (_, formal, _) = original_structure(result.document());
    resolved_at(result.document(), OccurrenceRole::Read, 320, 321, formal);
}

#[test]
fn original_chapel_typed_overload_call_remains_a_mandatory_unclosed_requirement() {
    let result = output(CHAPEL, ORIGINAL, EncodingId::utf8());
    let (target, _, caller) = original_structure(result.document());
    exact_call(result.document(), caller, target, 637, 649);
}

#[test]
fn ordinary_nested_modules_and_distinct_procedure_headers_keep_direct_formals() {
    let text = "module Outer { module Inner { proc measure(value: int) { return value; } } proc measure(value: string) {} }";
    let result = output(CHAPEL, text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let at = |name: &str| u64::try_from(text.find(name).unwrap()).unwrap();
    let outer = entity_at(
        document,
        "Outer",
        EntityKind::Module,
        at("Outer"),
        at("Outer") + 5,
    );
    let inner = entity_at(
        document,
        "Inner",
        EntityKind::Module,
        at("Inner"),
        at("Inner") + 5,
    );
    let first = entity_at(
        document,
        "measure",
        EntityKind::Function,
        at("measure"),
        at("measure") + 7,
    );
    let formal = entity_at(
        document,
        "value",
        EntityKind::Parameter,
        at("value"),
        at("value") + 5,
    );
    contains(document, outer, inner, at("Inner"), at("Inner") + 5);
    contains(document, inner, first, at("measure"), at("measure") + 7);
    contains(document, first, formal, at("value"), at("value") + 5);
    let other = u64::try_from(text.rfind("measure").unwrap()).unwrap();
    let second = entity_at(document, "measure", EntityKind::Function, other, other + 7);
    assert_ne!(first, second);
    contains(document, outer, second, other, other + 7);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn unsupported_keyword_recovery_strings_and_comments_do_not_supply_header_proof() {
    let text = b"/* module False { /* proc hidden(value) {} */ } */ module Written { proc measure(value: int) { const text = \"module Fake {}\"; return value; } } begin;";
    let result = output(CHAPEL, text, EncodingId::utf8());
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Written" && entity.kind == EntityKind::Module)
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(result.document().skipped_regions.iter().any(|gap| gap.domain == domain && gap.detail == "chapel-native-unsupported-keyword-control-or-mutation-has-no-binding-certificate"));
        let coverage: Vec<_> = result
            .document()
            .coverage_records
            .iter()
            .filter(|record| {
                record.domain == domain
                    && record.scope == CoverageScope::File(result.document().files[0].id)
            })
            .collect();
        assert_eq!(coverage.len(), 1);
        assert_ne!(coverage[0].status, CoverageStatus::Complete);
        assert!(coverage[0].skipped > 0);
    }
    for name in ["False", "hidden", "Fake"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    for text in [
        "module { proc measure(value: int) {} }",
        "module Written { proc measure(ref value: int) {} }",
        "module Written { proc measure(value: int = 1) {} }",
    ] {
        let result = output(CHAPEL, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result.document().entities.iter().any(
                |entity| entity.canonical_name == "measure" || entity.canonical_name == "value"
            )
        );
    }
}

#[test]
fn bom_crlf_and_both_utf16_orders_preserve_raw_source_and_physical_formals() {
    let text = "// λ😀\r\nmodule Written { proc measure(value: int) { return value; } }\r\n";
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
        let result = output(
            CHAPEL,
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let function = text.find("measure").unwrap();
        let parameter = text.find("value").unwrap();
        let owner = entity_at(
            result.document(),
            "measure",
            EntityKind::Function,
            physical(function),
            physical(function + 7),
        );
        let formal = entity_at(
            result.document(),
            "value",
            EntityKind::Parameter,
            physical(parameter),
            physical(parameter + 5),
        );
        contains(
            result.document(),
            owner,
            formal,
            physical(parameter),
            physical(parameter + 5),
        );
    }
    let raw = format!("\u{feff}{text}").into_bytes();
    let at = u64::try_from(std::str::from_utf8(&raw).unwrap().find("value").unwrap()).unwrap();
    let result = output(CHAPEL, &raw, EncodingId::utf8());
    entity_at(
        result.document(),
        "value",
        EntityKind::Parameter,
        at,
        at + 5,
    );
}

#[test]
fn required_exact_fresh_artifact_one_less_and_cancellation_keep_structural_closure() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CHAPEL);
    let fixture = Fixture::new(CHAPEL, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, CHAPEL, &bounds);
    let full = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let required = full.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap()
    );
    assert!(required > 1);
    let exact = limits_with_syntax_records(required);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, CHAPEL, &exact),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    original_structure(result.document());
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let small = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, CHAPEL, &small),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::RequiredSyntaxFacts, observed, limit}) if observed == required && limit == required - 1),
        "{error:?}"
    );
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        frontend.prepare_artifact(&input, &cancelled),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn body_only_edits_keep_header_identity_and_never_select_an_overload_by_spelling() {
    let before = b"module Written { proc measure(value: int) { return 1; } proc measure(value: string) { return 2; } }";
    let after = b"module Written { proc measure(value: int) { return 3; } proc measure(value: string) { return 4; } }";
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CHAPEL);
    let fixture = Fixture::new(CHAPEL, before);
    let next = fixture.rewrite(after);
    let bounds = limits();
    let first = analyze(
        &frontend,
        &request(&fixture.snapshot, &fixture.source, CHAPEL, &bounds),
        &ExtensionSupport::default(),
    );
    let second = analyze(
        &frontend,
        &request(&next.snapshot, &next.source, CHAPEL, &bounds),
        &ExtensionSupport::default(),
    );
    let ids = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| (entity.kind, entity.canonical_name.clone(), entity.id))
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(first.document()), ids(second.document()));
    assert_ne!(fixture.source.content_hash(), next.source.content_hash());
    assert_ne!(fixture.source.generation(), next.source.generation());
    for text in [
        "module Written { proc target(value: int) {} proc target(value: string) {} proc caller() { target(1); } }",
        "module Written { proc target(value: int) {} proc caller() { target(1); } }",
    ] {
        let result = output(CHAPEL, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
}

fn initializer_read(
    output: &AnalysisOutput,
    owner_name: &str,
    formal_name: &str,
    definition: (u64, u64),
    usage: (u64, u64),
) {
    let document = output.document();
    let formal = entity_at(
        document,
        formal_name,
        EntityKind::Parameter,
        definition.0,
        definition.1,
    );
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.id == formal)
        .unwrap();
    let Some(rootlight_ir::ContainerRef::Entity(owner)) = parameter.container else {
        panic!("formal owner")
    };
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap();
    assert_eq!(owner.kind, EntityKind::Function);
    assert_eq!(owner.canonical_name, owner_name);
    contains(document, owner.id, formal, definition.0, definition.1);
    resolved_at(document, OccurrenceRole::Read, usage.0, usage.1, formal);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == usage.0 && site.source.span().end_byte() == usage.1
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "the certified Read owns this exact leaf without an unresolved duplicate"
    );
    assert_eq!(sites[0].syntax_kind, "chapel.formal_initializer.reference");
    assert_eq!(sites[0].enclosing, Some(owner.id));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                && edge.object == RelationEndpoint::Entity(formal)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&sites[0].source));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(document.skipped_regions.iter().any(|gap| gap.domain == domain
            && gap.detail == "chapel-written-headers-and-certified-formal-initializers-only-overload-and-runtime-semantics-unavailable"));
    }
}

fn clean_initializer_fixture(source: &[u8]) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_chapel::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "native prerequisite: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn direct_initializer_reads_keep_exact_own_formal_and_ordinary_optional_references() {
    let result = output(CHAPEL, ORIGINAL, EncodingId::utf8());
    initializer_read(&result, "genHashKey32", "h", (178, 179), (320, 321));
    let text = "module Written { proc measure(value: int) { var saved: int = value; saved += 1; return saved; } proc unrelated() { begin; } }";
    clean_initializer_fixture(text.as_bytes());
    let result = output(CHAPEL, text.as_bytes(), EncodingId::utf8());
    let formal = u64::try_from(text.find("value").unwrap()).unwrap();
    let usage = u64::try_from(text.find("= value").unwrap() + 2).unwrap();
    initializer_read(
        &result,
        "measure",
        "value",
        (formal, formal + 5),
        (usage, usage + 5),
    );
    let saved = u64::try_from(text.rfind("saved;").unwrap()).unwrap();
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == saved && site.source.span().end_byte() == saved + 5
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].role, OccurrenceRole::Reference);
    assert_eq!(sites[0].syntax_kind, "chapel.unresolved_name.reference");
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn whole_owner_competitors_writes_nested_scopes_and_calls_withhold_initializer_binding() {
    for text in [
        "module Written { proc measure(value: int, value: int) { var saved = value; } }",
        "module Written { proc measure(value: int) { var value = 1; var saved = value; } }",
        "module Written { proc measure(value: int) { var saved = value; value = 2; } }",
        "module Written { proc measure(value: int) { var saved = value; external(); } }",
        "module Written { proc measure(value: int) { var saved = value; { var other = 1; } } }",
        "module Written { proc measure(value: int) { var saved = value; proc nested() {} } }",
        "module Written { proc measure(value: int) { ref saved = value; } }",
        "module Written { proc measure(value: int) { var saved = value; begin; } }",
        "module Written { proc donor(value: int) {} proc caller() { var saved = value; } }",
        "module Written { proc measure(value: int) { var saved = value + 1; } }",
    ] {
        clean_initializer_fixture(text.as_bytes());
        let result = output(CHAPEL, text.as_bytes(), EncodingId::utf8());
        let usage = u64::try_from(text.rfind("= value").unwrap() + 2).unwrap();
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == usage
                    && site.source.span().end_byte() == usage + 5
            })
            .collect();
        assert_eq!(sites.len(), 1, "{text}");
        assert_eq!(sites[0].role, OccurrenceRole::Reference, "{text}");
        assert!(
            matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
            "{text}"
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(sites[0].id)),
            "{text}"
        );
    }
    let text = b"module Written { proc measure(value: int) { var saved = value; ";
    let result = output(CHAPEL, text, EncodingId::utf8());
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn initializer_read_bom_crlf_and_utf16_evidence_use_physical_source_spans() {
    let text = "// λ😀\r\nmodule Written { proc measure(value: int) { var saved = value; } }\r\n";
    clean_initializer_fixture(text.as_bytes());
    let definition = text.find("value").unwrap();
    let usage = text.rfind("value").unwrap();
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
            CHAPEL,
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        initializer_read(
            &result,
            "measure",
            "value",
            (physical(definition), physical(definition + 5)),
            (physical(usage), physical(usage + 5)),
        );
    }
    let bytes = format!("\u{feff}{text}").into_bytes();
    let result = output(CHAPEL, &bytes, EncodingId::utf8());
    initializer_read(
        &result,
        "measure",
        "value",
        (
            u64::try_from(definition + 3).unwrap(),
            u64::try_from(definition + 8).unwrap(),
        ),
        (
            u64::try_from(usage + 3).unwrap(),
            u64::try_from(usage + 8).unwrap(),
        ),
    );
}

#[test]
fn initializer_required_provider_artifact_and_atomic_ir_admission_keep_original_read() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CHAPEL);
    let fixture = Fixture::new(CHAPEL, ORIGINAL);
    let base = limits();
    let input = request(&fixture.snapshot, &fixture.source, CHAPEL, &base);
    let full = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let required = full.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap()
    );
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, CHAPEL, &exact);
    let (result, retained) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    initializer_read(&result, "genHashKey32", "h", (178, 179), (320, 321));
    assert_eq!(
        required,
        retained.required_syntax_fact_count(&deadline()).unwrap()
    );
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, CHAPEL, &exact);
    let reused = frontend
        .analyze_from_artifact(
            &next_input,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&frontend, &next_input, &ExtensionSupport::default());
    assert_eq!(reused.document(), fresh.document());
    initializer_read(&reused, "genHashKey32", "h", (178, 179), (320, 321));
    let smaller = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, CHAPEL, &smaller),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required-1),
        "{error:?}"
    );
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        frontend.analyze_from_artifact(
            &input,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let changed = fixture.rewrite(
        &ORIGINAL
            .iter()
            .copied()
            .chain(b"\n".iter().copied())
            .collect::<Vec<_>>(),
    );
    assert!(
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, CHAPEL, &exact), &retained, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
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
                &request(&fixture.snapshot, &fixture.source, CHAPEL, &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    initializer_read(&baseline, "genHashKey32", "h", (178, 179), (320, 321));
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
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("resource prerequisite: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit }) if observed > limit && limit == low-1 && resource == if dimension < 2 { ResourceKind::Records } else { ResourceKind::NestedItems }),
            "{error:?}"
        );
    }
}

#[test]
fn raw_definition_declaration_context_cardinality_and_current_source_are_required_for_read() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
    };
    let native = provider();
    let fixture = Fixture::new(CHAPEL, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, CHAPEL, &budget);
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
            language(CHAPEL),
            CHAPEL.frontend,
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
    initializer_read(&baseline, "genHashKey32", "h", (178, 179), (320, 321));
    let denied = |facts| {
        let result = replay(facts);
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == 320
                    && site.source.span().end_byte() == 321
            })
            .collect();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert_eq!(site.source.content_hash(), content_hash(ORIGINAL));
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id))
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.source == site.source
                    && gap.detail == "chapel-native-own-formal-initializer-binding-unavailable")
        );
    };
    let (_, formal, _) = original_structure(baseline.document());
    let parameter = baseline
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == formal)
        .unwrap();
    let Some(rootlight_ir::ContainerRef::Entity(owner_id)) = parameter.container else {
        panic!("owner")
    };
    for endpoint in [formal, owner_id] {
        let entity = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == endpoint)
            .unwrap();
        let own_span = entity.evidence.source.as_ref().unwrap().span();
        let declarations: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Declaration && fact.span() == own_span)
            .collect();
        assert_eq!(declarations.len(), 1);
        let declaration = declarations[0];
        let definitions: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "chapel.name.definition"
                    && fact.parent() == Some(declaration.local_id())
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        for witness in [declaration, definitions[0]] {
            for foreign in [false, true] {
                let mut facts = parsed.facts().to_vec();
                let next = facts
                    .iter()
                    .map(SyntaxFact::local_id)
                    .max()
                    .unwrap()
                    .checked_add(1)
                    .unwrap();
                let label = if foreign {
                    SyntaxKindLabel::new(if witness.kind() == SyntaxFactKind::Declaration {
                        "rust.function.declaration"
                    } else {
                        "rust.identifier.definition"
                    })
                    .unwrap()
                } else {
                    witness.syntax_kind().clone()
                };
                facts.push(SyntaxFact::new(
                    next,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    label,
                ));
                denied(facts);
            }
        }
        denied(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.local_id() != definitions[0].local_id())
                .cloned()
                .collect(),
        );
    }
    let contexts: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "chapel.formal_initializer_context.signature")
        .collect();
    assert_eq!(contexts.len(), 1);
    let context = contexts[0];
    denied(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.local_id() != context.local_id())
            .cloned()
            .collect(),
    );
    for foreign in [false, true] {
        let mut facts = parsed.facts().to_vec();
        let next = facts
            .iter()
            .map(SyntaxFact::local_id)
            .max()
            .unwrap()
            .checked_add(1)
            .unwrap();
        let label = if foreign {
            SyntaxKindLabel::new("rust.function.signature").unwrap()
        } else {
            context.syntax_kind().clone()
        };
        facts.push(SyntaxFact::new(
            next,
            context.parent(),
            context.kind(),
            context.span(),
            context.depth(),
            label,
        ));
        denied(facts);
    }
    let mut facts = parsed.facts().to_vec();
    let slot = facts
        .iter_mut()
        .find(|fact| fact.local_id() == context.local_id())
        .unwrap();
    *slot = SyntaxFact::new(
        context.local_id(),
        context.parent(),
        context.kind(),
        SourceSpan::new(
            context.span().file(),
            context.span().start_byte() + 1,
            context.span().end_byte(),
        )
        .unwrap(),
        context.depth(),
        context.syntax_kind().clone(),
    );
    denied(facts);
}
