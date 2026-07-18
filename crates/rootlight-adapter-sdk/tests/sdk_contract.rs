//! Regression coverage for the bounded adapter SDK transaction boundary.
//!
//! Tests exercise public contracts through real immutable VFS snapshots and
//! the in-process mock adapters supplied by the SDK.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds,
    BoundedIrSink, BoundedSyntaxSink, CoverageReport, DiagnosticCode, DomainCoverage, EncodingId,
    GenerationBoundSnapshot, IncludedRange, IrBatch, IrBatchSink, IrRecord, LanguageId,
    MemoryAdmissionPolicy, MemoryAdmissionStatus, MemoryEnforcement, ParseCapabilities,
    ParseProvider, ParseReport, ParseRequest, ProducerDescriptor, ReportError, RequestError,
    ResourceKind, ResourceUsage, SinkError, StreamEnd, StreamLimits, SyntaxFact, SyntaxFactBatch,
    SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel, WorkReport, execute_analysis, execute_parse,
    testkit::{MockLanguageAnalyzer, MockParseProvider},
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, DiagnosticSeverity, ExtensionCriticality,
    ExtensionEnvelope, ExtensionSupport, FactDomain, FactEvidence, FileRecord, IrLimits,
    ProducerIdentity, ProvenanceRecord, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

#[test]
fn checked_snapshot_requires_exact_full_file_identity() {
    let (_temporary, snapshot, source) = source_fixture();
    let partial = SourceRef::new(
        source.repository(),
        source.generation(),
        SourceSpan::new(snapshot.file(), 0, 1).expect("test span is ordered"),
        snapshot.content_hash(),
        None,
    );
    let stale_hash = "b3_rc6zkrxh5srdoiia2cydtoqh5ug2jyctujxicstuvgf2yz377y5zl6hbcu"
        .parse()
        .expect("checked alternate content hash parses");
    let stale = SourceRef::new(
        source.repository(),
        source.generation(),
        source.span(),
        stale_hash,
        None,
    );

    assert!(GenerationBoundSnapshot::new(&snapshot, &source).is_ok());
    assert!(GenerationBoundSnapshot::new(&snapshot, &partial).is_err());
    assert!(GenerationBoundSnapshot::new(&snapshot, &stale).is_err());
}

#[test]
fn analysis_request_preserves_compatible_utf8_full_file_defaults() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let request = analysis_request(&snapshot, &source, &limits);

    assert_eq!(request.encoding().as_str(), "utf-8");
    assert!(request.included_ranges().is_empty());
    assert_eq!(request.generated_status(), None);
}

#[test]
fn parser_and_analyzer_share_included_range_validation() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let bound = GenerationBoundSnapshot::new(&snapshot, &source).expect("fixture snapshot binds");
    let language = LanguageId::new("rust").expect("test language is safe");
    let encoding = EncodingId::new("utf-8").expect("test encoding is safe");
    let first =
        SourceSpan::new(snapshot.file(), 0, 4).expect("first included range span is ordered");
    let second =
        SourceSpan::new(snapshot.file(), 8, 16).expect("second included range span is ordered");
    let ranges = vec![
        IncludedRange::new(first, language.clone()),
        IncludedRange::new(second, language.clone()),
    ];

    let analysis = AnalysisRequest::new_with_parse_context(
        bound.clone(),
        language.clone(),
        encoding.clone(),
        ranges.clone(),
        AnalysisTier::TierC,
        BuildContextIdentity::new(snapshot.content_hash()),
        &limits,
    )
    .expect("sorted disjoint analysis ranges are accepted");
    assert_eq!(analysis.encoding(), &encoding);
    assert_eq!(analysis.included_ranges(), ranges);
    assert_eq!(analysis.generated_status(), None);

    let classified = analysis.clone().with_generated_status(true);
    assert_eq!(classified.generated_status(), Some(true));

    let overlapping = vec![
        IncludedRange::new(first, language.clone()),
        IncludedRange::new(
            SourceSpan::new(snapshot.file(), 3, 10).expect("overlap fixture span is ordered"),
            language.clone(),
        ),
    ];
    let parse_error = ParseRequest::new(
        bound.clone(),
        language.clone(),
        encoding.clone(),
        overlapping.clone(),
        &limits,
    )
    .expect_err("parser request rejects overlapping ranges");
    let analysis_error = AnalysisRequest::new_with_parse_context(
        bound,
        language,
        encoding,
        overlapping,
        AnalysisTier::TierC,
        BuildContextIdentity::new(snapshot.content_hash()),
        &limits,
    )
    .expect_err("analysis request rejects overlapping ranges");
    assert_eq!(analysis_error, parse_error);
    assert_eq!(
        analysis_error,
        RequestError::IncludedRangeOrder { index: 1 }
    );
}

#[test]
fn parser_context_shares_checked_included_range_ownership() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let bound = GenerationBoundSnapshot::new(&snapshot, &source).expect("fixture snapshot binds");
    let language = LanguageId::new("rust").expect("test language is safe");
    let encoding = EncodingId::new("utf-8").expect("test encoding is safe");
    let ranges = vec![
        IncludedRange::new(
            SourceSpan::new(snapshot.file(), 0, 4).expect("test span is ordered"),
            language.clone(),
        ),
        IncludedRange::new(
            SourceSpan::new(snapshot.file(), 8, 16).expect("test span is ordered"),
            language.clone(),
        ),
    ];
    let analysis = AnalysisRequest::new_with_parse_context(
        bound,
        language,
        encoding,
        ranges.clone(),
        AnalysisTier::TierC,
        BuildContextIdentity::new(snapshot.content_hash()),
        &limits,
    )
    .expect("sorted disjoint analysis ranges are accepted");

    let first_parse = analysis.to_parse_request();
    let second_parse = analysis.clone().to_parse_request();
    let first_ranges = first_parse.shared_included_ranges();
    let second_ranges = second_parse.shared_included_ranges();
    let cloned_parse_ranges = first_parse.clone().shared_included_ranges();

    assert_eq!(first_parse.included_ranges(), ranges);
    assert_eq!(first_parse.language(), analysis.language());
    assert_eq!(first_parse.encoding(), analysis.encoding());
    assert_eq!(
        first_parse.source().source_ref(),
        analysis.source().source_ref()
    );
    assert!(std::ptr::eq(first_parse.limits(), analysis.limits()));
    assert!(Arc::ptr_eq(&first_ranges, &second_ranges));
    assert!(Arc::ptr_eq(&first_ranges, &cloned_parse_ranges));
}

#[test]
fn mock_parser_honors_batch_backpressure_and_canonicalizes_order() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(1, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let facts = vec![
        syntax_fact(&source, 3, 2),
        syntax_fact(&source, 1, 0),
        syntax_fact(&source, 2, 1),
    ];
    let provider = MockParseProvider::new(
        parse_capabilities(),
        facts,
        Vec::new(),
        complete_coverage(source_len(&source)),
    );

    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::default(),
        &deadline(),
    )
    .expect("bounded parser transaction commits");

    assert_eq!(
        output
            .facts()
            .iter()
            .map(SyntaxFact::local_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(output.report().resources().stream().batches(), 3);
    assert_eq!(output.report().resources().stream().records(), 3);
    assert_eq!(
        output.memory_admission(),
        MemoryAdmissionStatus::AccountedInProcess
    );
}

#[test]
fn syntax_node_limits_are_independent_of_emitted_facts() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(1, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let provider = MockParseProvider::new(
        parse_capabilities(),
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
        complete_coverage(source_len(&source)),
    )
    .with_syntax_nodes(1_000_000);

    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::InvalidReport(ReportError::ResourceLimit {
            resource: ResourceKind::SyntaxNodes,
            observed: 1_000_000,
            limit: 1024,
        }))
    );

    let oversized_limits = limits_with_syntax_nodes(1, IrLimits::default(), 4097);
    let oversized_request = parse_request(&snapshot, &source, &oversized_limits);
    assert_eq!(
        execute_parse(
            &provider,
            &oversized_request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
            resource: ResourceKind::SyntaxNodes,
            observed: 4097,
            limit: 4096,
        }))
    );
}

#[test]
fn cancellation_wins_before_and_between_batches() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(1, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let coverage = complete_coverage(source_len(&source));
    let provider = MockParseProvider::new(
        parse_capabilities(),
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
        coverage.clone(),
    );
    let before = deadline();
    before.cancel(CancellationReason::ClientRequest);

    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::default(),
            &before,
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest,
        })
    );

    let between = MockParseProvider::new(
        parse_capabilities(),
        vec![syntax_fact(&source, 1, 0), syntax_fact(&source, 2, 1)],
        Vec::new(),
        coverage,
    )
    .with_cancellation_after_batches(1, CancellationReason::ResourceLimit);
    assert_eq!(
        execute_parse(
            &between,
            &request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ResourceLimit,
        })
    );

    let deadline = Cancellation::with_deadline(Instant::now());
    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::default(),
            &deadline,
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::DeadlineExceeded,
        })
    );
}

#[test]
fn invocation_admission_requires_deadline_and_parser_checkpoints() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(1, IrLimits::default());
    let parse_request = parse_request(&snapshot, &source, &limits);
    let provider = MockParseProvider::new(
        parse_capabilities(),
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
        complete_coverage(source_len(&source)),
    );

    assert_eq!(
        execute_parse(
            &provider,
            &parse_request,
            MemoryAdmissionPolicy::default(),
            &Cancellation::new(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::DeadlineRequired
        ))
    );

    let missing_checkpoints = MockParseProvider::new(
        parse_capabilities_with(MemoryEnforcement::AccountedInProcess, false),
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
        complete_coverage(source_len(&source)),
    );
    assert_eq!(
        execute_parse(
            &missing_checkpoints,
            &parse_request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::CancellationCheckpointsRequired
        ))
    );

    let analysis_request = analysis_request(&snapshot, &source, &limits);
    let (file, provenance, descriptor) = minimal_ir_records(&source);
    let analyzer = MockLanguageAnalyzer::new(
        descriptor,
        vec![IrRecord::File(file), IrRecord::Provenance(provenance)],
        analysis_coverage(AnalysisTier::TierC, source_len(&source)),
        0,
    );
    assert_eq!(
        execute_analysis(
            &analyzer,
            &analysis_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::default(),
            &Cancellation::new(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::DeadlineRequired
        ))
    );
}

#[test]
fn monotonic_deadline_expires_between_parser_batches() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(1, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let known_deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("test deadline derives");
    let provider = DeadlineBetweenBatchesProvider {
        capabilities: parse_capabilities(),
        fact: syntax_fact(&source, 1, 0),
        coverage: complete_coverage(source_len(&source)),
        known_deadline,
    };
    let cancellation = Cancellation::with_deadline(known_deadline);

    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::default(),
            &cancellation,
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::DeadlineExceeded,
        })
    );
}

#[test]
fn sink_rejects_duplicate_out_of_order_and_oversized_batches_atomically() {
    let (_temporary, _snapshot, source) = source_fixture();
    let limits = limits(1, IrLimits::default());
    let mut sink = BoundedSyntaxSink::new(
        source.clone(),
        limits.syntax_stream().clone(),
        limits.max_syntax_depth(),
    );
    sink.push(SyntaxFactBatch::new(
        0,
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
    ))
    .expect("first batch is accepted");
    assert_eq!(
        sink.push(SyntaxFactBatch::new(
            0,
            vec![syntax_fact(&source, 2, 0)],
            Vec::new(),
        )),
        Err(SinkError::DuplicateSequence { sequence: 0 })
    );
    assert_eq!(
        sink.push(SyntaxFactBatch::new(
            2,
            vec![syntax_fact(&source, 2, 0)],
            Vec::new(),
        )),
        Err(SinkError::OutOfOrder {
            expected: 1,
            observed: 2,
        })
    );
    assert_eq!(sink.staged_usage().records(), 1);

    let mut fresh = BoundedSyntaxSink::new(
        source.clone(),
        limits.syntax_stream().clone(),
        limits.max_syntax_depth(),
    );
    assert!(matches!(
        fresh.push(SyntaxFactBatch::new(
            0,
            vec![syntax_fact(&source, 1, 0), syntax_fact(&source, 2, 0),],
            Vec::new(),
        )),
        Err(SinkError::BatchLimit {
            resource: ResourceKind::Records,
            observed: 2,
            limit: 1,
        })
    ));
    assert_eq!(fresh.staged_usage().records(), 0);
    assert_eq!(fresh.next_sequence(), 0);
}

#[test]
fn signature_facts_consume_the_same_bounded_sink_budget_as_other_facts() {
    let (_temporary, _snapshot, source) = source_fixture();
    let batch = BatchThresholds::new(1, 1024, 1, 128).expect("signature batch limits are valid");
    let stream = StreamLimits::new(2, 1, 2048, 2, 256, 128, batch)
        .expect("signature stream limits are valid");
    let mut sink = BoundedSyntaxSink::new(source.clone(), stream, 8);
    let signature = SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Signature,
        SourceSpan::new(source.span().file(), 0, source.span().end_byte().min(1))
            .expect("signature span is valid"),
        0,
        SyntaxKindLabel::new("rust.function.signature").expect("signature syntax label is valid"),
    );

    sink.push(SyntaxFactBatch::new(0, vec![signature.clone()], Vec::new()))
        .expect("signature fact is accepted");
    assert_eq!(sink.staged_usage().records(), 1);
    assert_eq!(sink.remaining_budget().remaining().records(), 0);
    assert_eq!(
        sink.push(SyntaxFactBatch::new(1, vec![signature], Vec::new())),
        Err(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed: 2,
            limit: 1,
        })
    );
    assert_eq!(sink.staged_usage().records(), 1);
    assert_eq!(sink.next_sequence(), 1);
}

#[test]
fn discard_rolls_back_staged_state_and_closes_the_transaction() {
    let (_temporary, _snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let mut syntax = BoundedSyntaxSink::new(
        source.clone(),
        limits.syntax_stream().clone(),
        limits.max_syntax_depth(),
    );
    syntax
        .push(SyntaxFactBatch::new(
            0,
            vec![syntax_fact(&source, 1, 0)],
            Vec::new(),
        ))
        .expect("syntax batch is staged");
    syntax.discard();

    assert_eq!(
        syntax.staged_usage(),
        rootlight_adapter_sdk::StreamUsage::default()
    );
    assert_eq!(syntax.next_sequence(), 0);
    assert_eq!(
        syntax.push(SyntaxFactBatch::new(
            0,
            vec![syntax_fact(&source, 2, 0)],
            Vec::new(),
        )),
        Err(SinkError::Closed)
    );

    let mut ir = BoundedIrSink::new(
        source.clone(),
        limits.ir_stream().clone(),
        limits.ir().clone(),
        ExtensionSupport::default(),
    );
    let (file, _provenance, _descriptor) = minimal_ir_records(&source);
    ir.push(IrBatch::new(0, vec![IrRecord::File(file)]))
        .expect("IR batch is staged");
    ir.discard();

    assert_eq!(
        ir.staged_usage(),
        rootlight_adapter_sdk::StreamUsage::default()
    );
    assert_eq!(ir.next_sequence(), 0);
    assert_eq!(ir.remaining_ir_budget().files(), limits.ir().max_files);
}

#[test]
fn diagnostic_batch_limit_is_bounded_and_atomic() {
    let (_temporary, _snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let mut sink = BoundedSyntaxSink::new(
        source,
        limits.syntax_stream().clone(),
        limits.max_syntax_depth(),
    );
    let code = DiagnosticCode::new("parse.recovery").expect("test code is safe");
    let diagnostics = (0..17)
        .map(|_| {
            AdapterDiagnostic::new(
                code.clone(),
                DiagnosticSeverity::Warning,
                None,
                CoverageStatus::Bounded,
            )
        })
        .collect();

    assert!(matches!(
        sink.push(SyntaxFactBatch::new(0, Vec::new(), diagnostics)),
        Err(SinkError::BatchLimit {
            resource: ResourceKind::Diagnostics,
            observed: 17,
            limit: 16,
        })
    ));
    assert_eq!(sink.staged_usage().diagnostics(), 0);
    assert_eq!(sink.next_sequence(), 0);
}

#[test]
fn explicit_end_of_stream_must_match_staged_batches() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let provider = WrongEndProvider {
        capabilities: parse_capabilities(),
        fact: syntax_fact(&source, 1, 0),
        coverage: complete_coverage(source_len(&source)),
        reported_end_sequence: 2,
    };

    assert!(matches!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::InvalidReport(
            ReportError::EndSequenceMismatch {
                expected: 1,
                observed: 2,
            }
        ))
    ));
}

#[test]
fn accounted_in_process_provider_must_report_memory_usage() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let provider = WrongEndProvider {
        capabilities: parse_capabilities(),
        fact: syntax_fact(&source, 1, 0),
        coverage: complete_coverage(source_len(&source)),
        reported_end_sequence: 1,
    };

    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::InvalidReport(
            ReportError::MissingMemoryAccounting
        ))
    );
}

#[test]
fn unavailable_memory_requires_explicit_visible_fallback() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let parse_request = parse_request(&snapshot, &source, &limits);
    let provider = MockParseProvider::new(
        parse_capabilities_with(MemoryEnforcement::Unavailable, true),
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
        complete_coverage(source_len(&source)),
    );

    assert_eq!(
        execute_parse(
            &provider,
            &parse_request,
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::MemoryEnforcementUnavailable
        ))
    );
    let parse_output = execute_parse(
        &provider,
        &parse_request,
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("explicit parser fallback commits");
    assert_eq!(
        parse_output.memory_admission(),
        MemoryAdmissionStatus::UnavailableM05Fallback
    );

    let analysis_request = analysis_request(&snapshot, &source, &limits);
    let (file, provenance, descriptor) = minimal_ir_records(&source);
    let fallback_descriptor = ProducerDescriptor::new(
        descriptor.identity().clone(),
        descriptor.kind(),
        descriptor.language().clone(),
        descriptor.tier(),
        MemoryEnforcement::Unavailable,
        descriptor.supports_noncritical_extensions(),
    );
    let analyzer = MockLanguageAnalyzer::new(
        fallback_descriptor,
        vec![IrRecord::File(file), IrRecord::Provenance(provenance)],
        analysis_coverage(AnalysisTier::TierC, source_len(&source)),
        0,
    );
    assert_eq!(
        execute_analysis(
            &analyzer,
            &analysis_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::MemoryEnforcementUnavailable
        ))
    );
    let analysis_output = execute_analysis(
        &analyzer,
        &analysis_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("explicit analyzer fallback commits");
    assert_eq!(
        analysis_output.memory_admission(),
        MemoryAdmissionStatus::UnavailableM05Fallback
    );
}

#[test]
fn raw_ir_quotas_cannot_be_bypassed_with_duplicate_batches() {
    let (_temporary, _snapshot, source) = source_fixture();
    let (file, _provenance, _descriptor) = minimal_ir_records(&source);
    let mut ir_limits = IrLimits::default();
    ir_limits.max_files = 2;
    ir_limits.max_total_records = 10;
    let analysis_limits = limits(2, ir_limits.clone());
    let mut sink = BoundedIrSink::new(
        source,
        analysis_limits.ir_stream().clone(),
        ir_limits,
        ExtensionSupport::default(),
    );
    sink.push(IrBatch::new(0, vec![IrRecord::File(file.clone())]))
        .expect("first raw file is accepted");
    sink.push(IrBatch::new(1, vec![IrRecord::File(file.clone())]))
        .expect("second raw duplicate is accepted before canonicalization");

    assert!(matches!(
        sink.push(IrBatch::new(2, vec![IrRecord::File(file)])),
        Err(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed: 3,
            limit: 2,
        })
    ));
    assert_eq!(sink.staged_usage().records(), 2);
    assert_eq!(sink.remaining_ir_budget().files(), 0);
}

#[test]
fn canonical_ir_is_independent_of_allowed_batch_order() {
    let (_temporary, snapshot, source) = source_fixture();
    let ir_limits = IrLimits::default();
    let limits = limits(1, ir_limits);
    let request = analysis_request(&snapshot, &source, &limits);
    let (file, provenance, descriptor) = minimal_ir_records(&source);
    let coverage = analysis_coverage(AnalysisTier::TierC, source_len(&source));
    let first = MockLanguageAnalyzer::new(
        descriptor.clone(),
        vec![
            IrRecord::File(file.clone()),
            IrRecord::Provenance(provenance.clone()),
        ],
        coverage.clone(),
        0,
    );
    let second = MockLanguageAnalyzer::new(
        descriptor,
        vec![IrRecord::Provenance(provenance), IrRecord::File(file)],
        coverage,
        0,
    );

    let first_output = execute_analysis(
        &first,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::default(),
        &deadline(),
    )
    .expect("first batch order canonicalizes");
    let second_output = execute_analysis(
        &second,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::default(),
        &deadline(),
    )
    .expect("second batch order canonicalizes");

    assert_eq!(first_output.document(), second_output.document());
    assert_eq!(first_output.document().files.len(), 1);
    assert_eq!(first_output.document().provenance.len(), 1);
}

#[test]
fn all_analysis_tiers_share_the_checked_ir_transaction_contract() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());

    for tier in [
        AnalysisTier::TierA,
        AnalysisTier::TierB,
        AnalysisTier::TierC,
        AnalysisTier::TierD,
    ] {
        let request = analysis_request_for_tier(&snapshot, &source, &limits, tier);
        let (file, provenance, descriptor) = minimal_ir_records_for_tier(&source, tier);
        let analyzer = MockLanguageAnalyzer::new(
            descriptor,
            vec![IrRecord::File(file), IrRecord::Provenance(provenance)],
            analysis_coverage(tier, source_len(&source)),
            0,
        );
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::default(),
            &deadline(),
        )
        .expect("each declared tier commits through the same normalized IR boundary");

        assert_eq!(output.report().coverage().tier(), tier);
        assert_eq!(output.document().provenance.len(), 1);
        assert_eq!(output.document().provenance[0].tier, tier);
        assert_eq!(output.document().files.len(), 1);
    }
}

#[test]
fn analyzer_report_cannot_claim_a_different_tier() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let request = analysis_request_for_tier(&snapshot, &source, &limits, AnalysisTier::TierC);
    let (file, provenance, descriptor) = minimal_ir_records_for_tier(&source, AnalysisTier::TierC);
    let analyzer = MockLanguageAnalyzer::new(
        descriptor,
        vec![IrRecord::File(file), IrRecord::Provenance(provenance)],
        analysis_coverage(AnalysisTier::TierD, source_len(&source)),
        0,
    );

    assert_eq!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::default(),
            &deadline(),
        ),
        Err(AdapterError::InvalidReport(
            ReportError::AnalysisTierMismatch {
                expected: AnalysisTier::TierC,
                observed: AnalysisTier::TierD,
            }
        ))
    );
}

#[test]
fn partial_coverage_is_preserved_without_claiming_completeness() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let request = parse_request(&snapshot, &source, &limits);
    let source_bytes = source_len(&source);
    let coverage = CoverageReport::new(
        AnalysisTier::TierD,
        CoverageStatus::Bounded,
        source_bytes,
        source_bytes - 1,
        1,
        vec![
            DomainCoverage::new(FactDomain::Entities, CoverageStatus::Bounded, 2, 1, 1)
                .expect("partial domain coverage is consistent"),
        ],
    )
    .expect("partial coverage is internally consistent");
    let provider = MockParseProvider::new(
        parse_capabilities(),
        vec![syntax_fact(&source, 1, 0)],
        Vec::new(),
        coverage,
    );

    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::default(),
        &deadline(),
    )
    .expect("partial result still commits");

    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(output.report().coverage().skipped_regions(), 1);
    assert_eq!(output.report().coverage().domains()[0].skipped(), 1);
}

#[test]
fn public_errors_do_not_echo_rejected_source_shaped_input() {
    let secret = "../../private/token\nsource text";
    let error = DiagnosticCode::new(secret).expect_err("unsafe code is rejected");
    let rendered = format!("{error:?} {error}");

    assert!(!rendered.contains("private"));
    assert!(!rendered.contains("token"));
    assert!(!rendered.contains("source text"));
}

#[test]
fn invalid_ir_errors_do_not_retain_extension_payloads() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(2, IrLimits::default());
    let request = analysis_request(&snapshot, &source, &limits);
    let (file, provenance, descriptor) = minimal_ir_records(&source);
    let secret = "../../private/token-source";
    let extension = ExtensionEnvelope {
        id: "fact1_baeaqcaibaeaqcaibaeaqcaibaeaqcai4pro3da"
            .parse()
            .expect("checked extension identity parses"),
        repository: source.repository(),
        generation: source.generation(),
        namespace: secret.to_owned(),
        version: "1.0".to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload: "{}".to_owned(),
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        },
    };
    let analyzer = MockLanguageAnalyzer::new(
        descriptor,
        vec![
            IrRecord::File(file),
            IrRecord::Provenance(provenance),
            IrRecord::Extension(extension),
        ],
        analysis_coverage(AnalysisTier::TierC, source_len(&source)),
        0,
    );

    let error = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::default(),
        &deadline(),
    )
    .expect_err("invalid extension identity is rejected");
    let rendered = format!("{error:?} {error}");

    assert!(matches!(error, AdapterError::Sink(SinkError::InvalidIr)));
    assert!(!rendered.contains("private"));
    assert!(!rendered.contains("token-source"));
}

struct DeadlineBetweenBatchesProvider {
    capabilities: ParseCapabilities,
    fact: SyntaxFact,
    coverage: CoverageReport,
    known_deadline: Instant,
}

impl ParseProvider for DeadlineBetweenBatchesProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        _request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        sink.push(SyntaxFactBatch::new(0, vec![self.fact.clone()], Vec::new()))?;

        // Advancing the deterministic checkpoint to the known deadline proves
        // the token's monotonic deadline path, without wall-clock sleeps.
        cancellation.check_at(self.known_deadline)?;

        let usage = sink.staged_usage();
        WorkReport::new(
            self.coverage.clone(),
            ResourceUsage::new(
                self.coverage.total_source_bytes(),
                usage.records(),
                1,
                self.fact.depth(),
                Some(0),
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}

struct WrongEndProvider {
    capabilities: ParseCapabilities,
    fact: SyntaxFact,
    coverage: CoverageReport,
    reported_end_sequence: u64,
}

impl ParseProvider for WrongEndProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        _request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        _cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        sink.push(SyntaxFactBatch::new(0, vec![self.fact.clone()], Vec::new()))?;
        let usage = sink.staged_usage();
        WorkReport::new(
            self.coverage.clone(),
            ResourceUsage::new(
                self.coverage.total_source_bytes(),
                usage.records(),
                1,
                self.fact.depth(),
                None,
                usage,
            ),
            StreamEnd::new(self.reported_end_sequence, usage),
        )
        .map_err(AdapterError::from)
    }
}

fn source_fixture() -> (TempDir, SourceSnapshot, SourceRef) {
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    fs::create_dir(temporary.path().join("src")).expect("fixture source directory is created");
    fs::write(
        temporary.path().join("src").join("lib.rs"),
        b"pub fn fixture() { let value = 1; }\n",
    )
    .expect("fixture source is written");
    let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        .parse()
        .expect("checked repository identity parses");
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let path =
        RelativePath::parse(Path::new("src/lib.rs")).expect("fixture relative path is valid");
    let snapshot = repository
        .snapshot(&path, 1024)
        .expect("fixture snapshot is stable");
    let end = u64::try_from(snapshot.content().len()).expect("small fixture length fits");
    let span = SourceSpan::new(snapshot.file(), 0, end).expect("full-file span is valid");
    let generation = "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
        .parse()
        .expect("checked generation identity parses");
    let source = SourceRef::new(
        repository_id,
        generation,
        span,
        snapshot.content_hash(),
        None,
    );
    (temporary, snapshot, source)
}

fn minimal_ir_records(
    source: &SourceRef,
) -> (
    rootlight_ir::FileRecord,
    rootlight_ir::ProvenanceRecord,
    ProducerDescriptor,
) {
    minimal_ir_records_for_tier(source, AnalysisTier::TierC)
}

fn minimal_ir_records_for_tier(
    source: &SourceRef,
    tier: AnalysisTier,
) -> (
    rootlight_ir::FileRecord,
    rootlight_ir::ProvenanceRecord,
    ProducerDescriptor,
) {
    let provenance_id = "fact1_aeaqcaibaeaqcaibaeaqcaibaeaqcaibwbicmga"
        .parse()
        .expect("checked provenance identity parses");
    let producer = ProducerIdentity::new("rootlight-sdk-test", "1.0", source.content_hash())
        .expect("test producer identity is valid");
    let provenance = ProvenanceRecord {
        id: provenance_id,
        repository: source.repository(),
        generation: source.generation(),
        producer_kind: rootlight_ir::ProducerKind::Parser,
        producer,
        binary_digest: source.content_hash(),
        frontend_version: Some("sdk-test-grammar-1".to_owned()),
        language: "rust".to_owned(),
        tier,
        build_context: BuildContextIdentity::new(source.content_hash()),
        input_sources: vec![source.clone()],
        evidence_sources: vec![source.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    };
    let file = FileRecord {
        id: source.span().file(),
        repository: source.repository(),
        generation: source.generation(),
        path: "src/lib.rs".to_owned(),
        path_locator: Some(
            RelativePath::parse(Path::new("src/lib.rs"))
                .expect("fixture file path is valid")
                .to_locator(),
        ),
        content_hash: source.content_hash(),
        byte_length: source.span().end_byte(),
        language: "rust".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: provenance_id,
        evidence: FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        },
    };
    let descriptor = ProducerDescriptor::new(
        provenance.producer.clone(),
        provenance.producer_kind,
        LanguageId::new(&provenance.language).expect("fixture language is safe"),
        provenance.tier,
        MemoryEnforcement::AccountedInProcess,
        false,
    );
    (file, provenance, descriptor)
}

fn syntax_fact(source: &SourceRef, local_id: u64, depth: usize) -> SyntaxFact {
    let end = source.span().end_byte().min(1);
    SyntaxFact::new(
        local_id,
        None,
        SyntaxFactKind::Declaration,
        SourceSpan::new(source.span().file(), 0, end).expect("test span is valid"),
        depth,
        SyntaxKindLabel::new("function_item").expect("test syntax label is safe"),
    )
}

fn parse_capabilities() -> ParseCapabilities {
    parse_capabilities_with(MemoryEnforcement::AccountedInProcess, true)
}

fn parse_capabilities_with(
    memory_enforcement: MemoryEnforcement,
    cancellation_checkpoints: bool,
) -> ParseCapabilities {
    ParseCapabilities::new(
        vec![LanguageId::new("rust").expect("test language is safe")],
        vec![EncodingId::new("utf-8").expect("test encoding is safe")],
        4096,
        4096,
        32,
        8,
        true,
        true,
        cancellation_checkpoints,
        4,
        memory_enforcement,
    )
    .expect("test parser capabilities are valid")
}

fn limits(batch_records: usize, ir: IrLimits) -> AnalysisLimits {
    limits_with_syntax_nodes(batch_records, ir, 1024)
}

fn limits_with_syntax_nodes(
    batch_records: usize,
    ir: IrLimits,
    max_syntax_nodes: usize,
) -> AnalysisLimits {
    let batch =
        BatchThresholds::new(batch_records, 32 * 1024, 16, 4096).expect("batch limits are valid");
    let stream = StreamLimits::new(64, 128, 1024 * 1024, 64, 64 * 1024, 64 * 1024, batch)
        .expect("stream limits are valid");
    AnalysisLimits::new(
        4096,
        max_syntax_nodes,
        32,
        8,
        1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .expect("analysis limits are valid")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline derives"),
    )
}

fn parse_request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
) -> ParseRequest<'a> {
    ParseRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("fixture snapshot binds"),
        LanguageId::new("rust").expect("test language is safe"),
        EncodingId::new("utf-8").expect("test encoding is safe"),
        Vec::new(),
        limits,
    )
    .expect("parse request is valid")
}

fn analysis_request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
) -> AnalysisRequest<'a> {
    analysis_request_for_tier(snapshot, source, limits, AnalysisTier::TierC)
}

fn analysis_request_for_tier<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
    tier: AnalysisTier,
) -> AnalysisRequest<'a> {
    AnalysisRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("fixture snapshot binds"),
        LanguageId::new("rust").expect("test language is safe"),
        tier,
        BuildContextIdentity::new(snapshot.content_hash()),
        limits,
    )
    .expect("analysis request is valid")
}

fn complete_coverage(source_bytes: usize) -> CoverageReport {
    CoverageReport::new(
        AnalysisTier::TierD,
        CoverageStatus::Complete,
        source_bytes,
        source_bytes,
        0,
        Vec::new(),
    )
    .expect("complete test coverage is valid")
}

fn analysis_coverage(tier: AnalysisTier, source_bytes: usize) -> CoverageReport {
    CoverageReport::new(
        tier,
        CoverageStatus::Complete,
        source_bytes,
        source_bytes,
        0,
        Vec::new(),
    )
    .expect("complete analysis coverage is valid")
}

fn source_len(source: &SourceRef) -> usize {
    usize::try_from(source.span().end_byte()).expect("small fixture length fits")
}
