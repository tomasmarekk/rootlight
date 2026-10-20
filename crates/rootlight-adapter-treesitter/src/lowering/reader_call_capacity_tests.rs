//! Reader language calls reserve all three source derivations before lowering.
//! Real parser facts exercise record and derivation quotas independently of the
//! final sink, which would otherwise detect under-accounting only after lowering.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{BatchThresholds, GenerationBoundSnapshot, LanguageId, StreamLimits};
use rootlight_ids::{GenerationId, derive_repository};
use rootlight_ir::BuildContextIdentity;
use rootlight_vfs::{RelativePath, RepositoryRoot};

use super::*;

const CASES: [(&str, &str, &str); 2] = [
    (
        "clojure",
        "written.clj",
        "(ns Written) (defn target [x] x) (defn caller [x] [(target x) (target x) (target x)])",
    ),
    (
        "racket",
        "written.rkt",
        "#lang racket/base\n(define (target x) x) (define (caller x) (begin (target x) (target x) (target x)))",
    ),
];

#[derive(Clone, Copy)]
enum Boundary {
    Unbounded,
    TotalDerivations,
    PerRecordDerivations,
}

fn limits(ir: IrLimits) -> AnalysisLimits {
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap()
}

fn preflight(
    language: &str,
    path: &str,
    source: &str,
    boundary: Boundary,
) -> (Result<LoweringBudget, AdapterError>, usize) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(path), source).unwrap();
    let repository = derive_repository(b"reader-call-capacity").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 1024 * 1024)
        .unwrap();
    let source_ref = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = crate::TreeSitterProvider::new(
        crate::RuntimeConfig::new(
            1024 * 1024,
            16_384,
            128,
            32,
            64,
            1,
            16 * 1024 * 1024,
            crate::ParserSettings::new(4096).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let producer =
        ProducerIdentity::new("reader-call-capacity", "1.0", content_hash(b"test")).unwrap();
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        producer.clone(),
        LanguageId::new(language).unwrap(),
        "native-reader-call-capacity",
        content_hash(b"reader-call-capacity-binary"),
    )
    .unwrap();
    let parse_limits = limits(IrLimits::default());
    let parse_request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
        LanguageId::new(language).unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"reader-call-capacity-build")),
        &parse_limits,
    )
    .unwrap()
    .with_generated_status(false);
    let cancellation = Cancellation::new();
    let parsed = analyzer
        .parse_request(&parse_request, &cancellation)
        .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(
                |fact| occurrence_role(fact) == Some(OccurrenceRole::CallSite)
                    && matches!(
                        fact.syntax_kind().as_str(),
                        "clojure.call.reference" | "racket.call.reference"
                    )
            )
            .count(),
        3,
        "{language} fixture has exactly three admitted direct call heads"
    );
    let mut ir = IrLimits::default();
    let expected = match boundary {
        Boundary::Unbounded | Boundary::TotalDerivations => 11,
        Boundary::PerRecordDerivations => 3,
    };
    match boundary {
        Boundary::Unbounded => {}
        Boundary::TotalDerivations => ir.max_total_nested_items = expected - 1,
        Boundary::PerRecordDerivations => ir.max_nested_items_per_record = expected - 1,
    }
    let lowering_limits = limits(ir);
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
        LanguageId::new(language).unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"reader-call-capacity-build")),
        &lowering_limits,
    )
    .unwrap()
    .with_generated_status(false);
    (
        preflight_lowering_limits(
            &request,
            &parsed,
            &producer,
            "native-reader-call-capacity",
            &[],
            &cancellation,
        ),
        expected,
    )
}

#[test]
fn reader_calls_admit_the_exact_complete_derivation_upper_bound() {
    for (language, path, source) in CASES {
        let (admitted, expected) = preflight(language, path, source, Boundary::Unbounded);
        assert_eq!(admitted.unwrap().nested_items, expected);
    }
}

#[test]
fn reader_calls_reject_total_and_per_record_derivation_overflow_before_lowering() {
    for (language, path, source) in CASES {
        for boundary in [Boundary::TotalDerivations, Boundary::PerRecordDerivations] {
            let (rejected, expected) = preflight(language, path, source, boundary);
            assert!(matches!(
                rejected,
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::NestedItems,
                    observed,
                    limit,
                })) if observed == expected && limit == expected - 1
            ));
        }
    }
}

const SCHEME_CAPACITY_SOURCE: &str =
    "(define (caller value) (later value)) (define (later arg) arg)";

fn with_scheme_capacity_source(
    text: &str,
    records: usize,
    check: impl FnOnce(&crate::TreeSitterProvider, &TreeSitterAnalyzer, &AnalysisRequest<'_>),
) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.scm"), text).unwrap();
    let repository = derive_repository(b"scheme-artifact-capacity").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.scm")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, text.len() as u64).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                32,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new("scheme-artifact-capacity", "1.0", content_hash(b"test")).unwrap(),
        LanguageId::new("scheme").unwrap(),
        "native-scheme-artifact-capacity",
        content_hash(b"scheme-artifact-capacity-binary"),
    )
    .unwrap();
    let defaults = limits(IrLimits::default());
    let stream = StreamLimits::new(
        128,
        records,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(records.min(128), 1024 * 1024, 32, 128 * 1024).unwrap(),
    )
    .unwrap();
    let selected = AnalysisLimits::new(
        defaults.max_source_bytes(),
        defaults.max_syntax_nodes(),
        defaults.max_syntax_depth(),
        defaults.max_embedded_ranges(),
        defaults.max_reported_memory_bytes(),
        stream,
        defaults.ir_stream().clone(),
        defaults.ir().clone(),
    )
    .unwrap();
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("scheme").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"scheme-artifact-capacity-build")),
        &selected,
    )
    .unwrap()
    .with_generated_status(false);
    check(&provider, &analyzer, &request);
}

fn assert_scheme_required_source_facts(output: &ParseOutput) {
    use SyntaxFactKind::{Declaration, Module, Occurrence, Root, Scope, Signature};
    // The complete source-authored census is three file records, two written
    // procedure headers/scopes/names, two formal names, and the one call head.
    let expected = [
        (Root, "scheme.file.root", 0, 62),
        (Module, "scheme.file.module", 0, 62),
        (Scope, "scheme.file.scope", 0, 62),
        (Declaration, "scheme.procedure.declaration", 0, 37),
        (Scope, "scheme.procedure.scope", 0, 37),
        (Occurrence, "scheme.name.definition", 9, 15),
        (Signature, "scheme.header.signature", 0, 22),
        (Declaration, "scheme.parameter.declaration", 16, 21),
        (Occurrence, "scheme.name.definition", 16, 21),
        (Declaration, "scheme.procedure.declaration", 38, 62),
        (Scope, "scheme.procedure.scope", 38, 62),
        (Occurrence, "scheme.name.definition", 47, 52),
        (Signature, "scheme.header.signature", 38, 57),
        (Declaration, "scheme.parameter.declaration", 53, 56),
        (Occurrence, "scheme.name.definition", 53, 56),
        (Occurrence, "scheme.direct_file_call.reference", 24, 29),
    ];
    assert_eq!(expected.len(), 16);
    for (kind, label, start, end) in expected {
        assert_eq!(
            output
                .facts()
                .iter()
                .filter(|fact| fact.kind() == kind
                    && fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end)
                .count(),
            1,
            "exact required {kind:?} {label} {start}..{end}; facts={:?}",
            output.facts()
        );
    }
}

#[test]
fn scheme_artifact_required_count_includes_all_sixteen_source_facts() {
    with_scheme_capacity_source(
        SCHEME_CAPACITY_SOURCE,
        16_384,
        |provider, analyzer, request| {
            let cancel = Cancellation::new();
            let demanded = provider
                .required_syntax_fact_count(&request.to_parse_request(), &cancel)
                .unwrap();
            assert_eq!(
                demanded, 16,
                "fifteen owner facts plus the source-certified call"
            );
            let artifact = analyzer.prepare_artifact(request, &cancel).unwrap();
            assert_scheme_required_source_facts(&artifact.parse_output);
            assert_eq!(
                artifact.required_syntax_fact_count(&cancel).unwrap(),
                demanded
            );
        },
    );
    with_scheme_capacity_source(SCHEME_CAPACITY_SOURCE, 16, |provider, analyzer, request| {
        let cancel = Cancellation::new();
        assert_eq!(
            provider
                .required_syntax_fact_count(&request.to_parse_request(), &cancel)
                .unwrap(),
            16
        );
        let artifact = analyzer.prepare_artifact(request, &cancel).unwrap();
        assert_scheme_required_source_facts(&artifact.parse_output);
        assert_eq!(
            artifact.syntax_fact_count(),
            16,
            "zero optional record allowance"
        );
        assert_eq!(artifact.required_syntax_fact_count(&cancel).unwrap(), 16);
        assert_eq!(
            artifact.parse_output.report().coverage().status(),
            CoverageStatus::Bounded
        );
        assert!(
            artifact
                .parse_output
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code().as_str() == "syntax-extraction-limit")
        );
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            artifact.required_syntax_fact_count(&cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    });
    with_scheme_capacity_source(SCHEME_CAPACITY_SOURCE, 15, |_, analyzer, request| {
        let error = analyzer
            .prepare_artifact(request, &Cancellation::new())
            .unwrap_err();
        assert_eq!(
            error,
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: 16,
                limit: 15,
            })
        );
    });
}

#[test]
fn scheme_required_call_survives_only_optional_partition_loss_with_full_derivation_admission() {
    with_scheme_capacity_source(SCHEME_CAPACITY_SOURCE, 16, |_, analyzer, request| {
        let cancel = Cancellation::new();
        let artifact = analyzer.prepare_artifact(request, &cancel).unwrap();
        assert_scheme_required_source_facts(&artifact.parse_output);
        let source = request.source().source_ref();
        assert_eq!(
            artifact.parse_output.report().coverage().status(),
            CoverageStatus::Bounded
        );
        assert!(
            scheme_calls::admits_certified_partition(&artifact.parse_output, source, &cancel)
                .unwrap()
        );
        let budget = preflight_lowering_limits(
            request,
            &artifact.parse_output,
            analyzer.descriptor.identity(),
            &analyzer.frontend_version,
            &[],
            &cancel,
        )
        .unwrap();
        assert_eq!(
            budget.nested_items, 5,
            "two ordinary source entries plus all three call proof references"
        );
        let result = analyzer
            .analyze_from_artifact(
                request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
        let calls: Vec<_> = result
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].evidence.derivation.len(), 3);
        assert_eq!(
            calls[0].evidence.source.as_ref().unwrap().span(),
            SourceSpan::new(source.span().file(), 24, 29).unwrap()
        );
        assert_eq!(
            calls[0].evidence.source.as_ref().unwrap().content_hash(),
            source.content_hash()
        );
        assert_eq!(calls[0].repository, source.repository());
        assert_eq!(calls[0].generation, source.generation());
        let changed_hash = SourceRef::new(
            source.repository(),
            source.generation(),
            source.span(),
            content_hash(b"stale"),
            None,
        );
        let foreign = SourceRef::new(
            derive_repository(b"foreign-scheme").id(),
            source.generation(),
            source.span(),
            source.content_hash(),
            None,
        );
        for wrong in [changed_hash, foreign] {
            assert!(
                !scheme_calls::admits_certified_partition(&artifact.parse_output, &wrong, &cancel)
                    .unwrap()
            );
        }
        let next_source = SourceRef::new(
            source.repository(),
            GenerationId::from_bytes([10; 20]),
            source.span(),
            source.content_hash(),
            None,
        );
        let next_request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(request.source().snapshot(), &next_source).unwrap(),
            LanguageId::new("scheme").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"scheme-artifact-capacity-build")),
            request.limits(),
        )
        .unwrap()
        .with_generated_status(false);
        let replay = analyzer
            .analyze_from_artifact(
                &next_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
        let replay_calls: Vec<_> = replay
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .collect();
        assert_eq!(replay_calls.len(), 1);
        assert_eq!(replay_calls[0].generation, next_source.generation());
        assert!(
            replay
                .document()
                .occurrences
                .iter()
                .all(|site| site.source.generation() == next_source.generation())
        );
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            scheme_calls::admits_certified_partition(&artifact.parse_output, source, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    });
    for text in [
        "(define (caller value) (missing value)) (define (later arg) arg)",
        "(define (caller value) (later value)) (define (later arg) arg",
        "(define (caller value) (later value)) (define (later arg) arg) (import unknown)",
    ] {
        with_scheme_capacity_source(text, 16_384, |_, analyzer, request| {
            let artifact = analyzer
                .prepare_artifact(request, &Cancellation::new())
                .unwrap();
            assert!(
                !scheme_calls::admits_certified_partition(
                    &artifact.parse_output,
                    request.source().source_ref(),
                    &Cancellation::new(),
                )
                .unwrap(),
                "declined native census or recovery cannot become proof: {text}"
            );
        });
    }
}
