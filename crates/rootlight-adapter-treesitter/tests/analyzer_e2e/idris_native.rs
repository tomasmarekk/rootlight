//! Native Idris equations and source uses keep physical provenance.
//! These checks leave elaboration, execution and application Calls unqualified.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/idris/stlc_small.idr");
const IDRIS: LanguageCase = LanguageCase {
    name: "idris",
    path: "written.idr",
    frontend: "idris-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(IDRIS, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(IDRIS),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, IDRIS),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for site in &result.document().occurrences {
        assert_eq!(site.repository, fixture.source.repository());
        assert_eq!(site.generation, fixture.source.generation());
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
        assert_eq!(site.source.content_hash(), content_hash(bytes));
        assert!(site.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    result
}

fn site_at(
    document: &rootlight_ir::NormalizedIrDocument,
    role: OccurrenceRole,
    start: u64,
    end: u64,
) -> &rootlight_ir::OccurrenceRecord {
    let matches: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == role
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(matches.len(), 1, "Idris {role:?}@{start}..{end}");
    matches[0]
}

fn definition_at(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let site = site_at(document, OccurrenceRole::Definition, start, end);
    let OccurrenceTarget::Resolved { symbol } = site.target else {
        panic!("the written equation must define its entity");
    };
    let entity = document
        .entities
        .iter()
        .find(|item| item.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Function);
    assert_eq!(entity.language, "idris");
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn original_contract(document: &rootlight_ir::NormalizedIrDocument, at: impl Fn(u64) -> u64) {
    let global = definition_at(document, "var", at(1215), at(1218));
    let caller = definition_at(document, "v0", at(1587), at(1589));
    assert_ne!(global, caller);
    let site = site_at(document, OccurrenceRole::Reference, at(1592), at(1595));
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: global });
    assert_eq!(site.enclosing, Some(caller));
    assert_eq!(site.syntactic_text_hash, content_hash(b"var"));
    assert!(document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(site.id)
            && edge.object == RelationEndpoint::Entity(global)
            && edge.evidence.source.as_ref() == Some(&site.source)
    }));
    let local = site_at(document, OccurrenceRole::Reference, at(1247), at(1250));
    assert_ne!(local.target, OccurrenceTarget::Resolved { symbol: global });
    match local.target {
        OccurrenceTarget::Resolved { symbol } => {
            let target = document
                .entities
                .iter()
                .find(|item| item.id == symbol)
                .unwrap();
            assert_eq!(target.language, "idris");
            assert_eq!(target.kind, EntityKind::Parameter);
            assert_eq!(target.canonical_name, "var");
        }
        OccurrenceTarget::Unresolved { .. } => {}
        _ => panic!("the lambda use must remain local or explicitly unresolved"),
    }
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && ((site.source.span().start_byte() == at(5)
                && site.source.span().end_byte() == at(9))
                || (site.source.span().start_byte() == at(1168)
                    && site.source.span().end_byte() == at(1171)))
    }));
    assert!(!document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == at(1230)
            && site.source.span().end_byte() == at(1233)
            && site.target == OccurrenceTarget::Resolved { symbol: global }
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && edge.subject == RelationEndpoint::Entity(caller)
    }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "idris-application-call-normalization-unqualified" })
    );
}

#[test]
fn complete_original_equations_and_body_reference_preserve_lambda_shadowing() {
    assert_eq!(ORIGINAL.len(), 2167);
    let result = output(ORIGINAL, EncodingId::new("utf-8").unwrap());
    original_contract(result.document(), |offset| offset);
}

#[test]
fn encoded_original_equations_and_references_keep_physical_source() {
    let text = std::str::from_utf8(ORIGINAL).unwrap();
    for (encoding, bytes) in [
        ("utf-8", [b"\xef\xbb\xbf".as_slice(), ORIGINAL].concat()),
        ("utf-16le", {
            let mut bytes = vec![0xff, 0xfe];
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            bytes
        }),
        ("utf-16be", {
            let mut bytes = vec![0xfe, 0xff];
            bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
            bytes
        }),
    ] {
        let result = output(&bytes, EncodingId::new(encoding).unwrap());
        original_contract(result.document(), |offset| {
            if encoding == "utf-8" {
                offset + 3
            } else {
                offset * 2 + 2
            }
        });
    }
}

#[test]
fn full_original_required_count_matches_artifact_and_refuses_one_less() {
    let provider = Arc::new(provider());
    let frontend = analyzer(&provider, IDRIS);
    let fixture = Fixture::new(IDRIS, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, IDRIS, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 0);
    let exact_budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, IDRIS, &exact_budget);
    let (output, artifact) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    original_contract(output.document(), |at| at);
    let replay = frontend
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), output.document());
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, IDRIS, &exact_budget);
    let rebound = frontend
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    original_contract(rebound.document(), |at| at);
    assert_eq!(
        rebound.document(),
        analyze(&frontend, &next_request, &ExtensionSupport::default()).document()
    );
    let small_budget = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, IDRIS, &small_budget);
    assert_eq!(
        provider
            .required_syntax_fact_count(&small.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    for error in [
        frontend.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &small.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
        }) if observed == required && limit == required - 1),
            "{error:?}"
        );
    }
}
