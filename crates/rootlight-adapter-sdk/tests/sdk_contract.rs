//! Regression coverage for the bounded adapter SDK transaction boundary.
//!
//! Tests exercise public contracts through real immutable VFS snapshots and
//! the in-process mock adapters supplied by the SDK.

use std::{fs, path::Path, time::Instant};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds,
    BoundedIrSink, BoundedSyntaxSink, CoverageReport, DiagnosticCode, DomainCoverage, EncodingId,
    GenerationBoundSnapshot, IrBatch, IrBatchSink, IrRecord, LanguageId, MemoryEnforcement,
    ParseCapabilities, ParseProvider, ParseReport, ParseRequest, ProducerDescriptor, ReportError,
    ResourceKind, ResourceUsage, SinkError, StreamEnd, StreamLimits, SyntaxFact, SyntaxFactBatch,
    SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel, WorkReport, execute_analysis, execute_parse,
    testkit::{MockLanguageAnalyzer, MockParseProvider},
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, DiagnosticSeverity, ExtensionSupport,
    FactDomain, IrLimits, NormalizedIrDocument, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const IR_FIXTURE: &str = include_str!("../../../tests/fixtures/compatibility/ir/1.1/document.json");

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
    let stale = fixture_document().files[0]
        .evidence
        .source
        .clone()
        .expect("fixture file has direct source evidence");

    assert!(GenerationBoundSnapshot::new(&snapshot, &source).is_ok());
    assert!(GenerationBoundSnapshot::new(&snapshot, &partial).is_err());
    assert!(GenerationBoundSnapshot::new(&snapshot, &stale).is_err());
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

    let output = execute_parse(&provider, &request, &Cancellation::new())
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
    let before = Cancellation::new();
    before.cancel(CancellationReason::ClientRequest);

    assert_eq!(
        execute_parse(&provider, &request, &before),
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
        execute_parse(&between, &request, &Cancellation::new()),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ResourceLimit,
        })
    );

    let deadline = Cancellation::with_deadline(Instant::now());
    assert_eq!(
        execute_parse(&provider, &request, &deadline),
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
        execute_parse(&provider, &request, &Cancellation::new()),
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
        execute_parse(&provider, &request, &Cancellation::new()),
        Err(AdapterError::InvalidReport(
            ReportError::MissingMemoryAccounting
        ))
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
    let coverage = complete_coverage(source_len(&source));
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
        &Cancellation::new(),
    )
    .expect("first batch order canonicalizes");
    let second_output = execute_analysis(
        &second,
        &request,
        ExtensionSupport::default(),
        &Cancellation::new(),
    )
    .expect("second batch order canonicalizes");

    assert_eq!(first_output.document(), second_output.document());
    assert_eq!(first_output.document().files.len(), 1);
    assert_eq!(first_output.document().provenance.len(), 1);
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

    let output = execute_parse(&provider, &request, &Cancellation::new())
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
    let mut fixture = fixture_document();
    let mut extension = fixture.extensions.remove(0);
    extension.repository = source.repository();
    extension.generation = source.generation();
    extension.namespace = secret.to_owned();
    extension.provenance = provenance.id;
    extension.evidence.source = Some(source.clone());
    extension.evidence.derivation.clear();
    let analyzer = MockLanguageAnalyzer::new(
        descriptor,
        vec![
            IrRecord::File(file),
            IrRecord::Provenance(provenance),
            IrRecord::Extension(extension),
        ],
        complete_coverage(source_len(&source)),
        0,
    );

    let error = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        &Cancellation::new(),
    )
    .expect_err("invalid extension identity is rejected");
    let rendered = format!("{error:?} {error}");

    assert!(matches!(error, AdapterError::Sink(SinkError::InvalidIr)));
    assert!(!rendered.contains("private"));
    assert!(!rendered.contains("token-source"));
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
    let seed = fixture_document().files[0]
        .evidence
        .source
        .clone()
        .expect("fixture file has direct source evidence");
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    fs::create_dir(temporary.path().join("src")).expect("fixture source directory is created");
    fs::write(
        temporary.path().join("src").join("lib.rs"),
        b"pub fn fixture() { let value = 1; }\n",
    )
    .expect("fixture source is written");
    let repository =
        RepositoryRoot::open(seed.repository(), temporary.path()).expect("temporary root opens");
    let path =
        RelativePath::parse(Path::new("src/lib.rs")).expect("fixture relative path is valid");
    let snapshot = repository
        .snapshot(&path, 1024)
        .expect("fixture snapshot is stable");
    let end = u64::try_from(snapshot.content().len()).expect("small fixture length fits");
    let span = SourceSpan::new(snapshot.file(), 0, end).expect("full-file span is valid");
    let source = SourceRef::new(
        seed.repository(),
        seed.generation(),
        span,
        snapshot.content_hash(),
        None,
    );
    (temporary, snapshot, source)
}

fn fixture_document() -> NormalizedIrDocument {
    serde_json::from_str(IR_FIXTURE).expect("checked IR compatibility fixture decodes")
}

fn minimal_ir_records(
    source: &SourceRef,
) -> (
    rootlight_ir::FileRecord,
    rootlight_ir::ProvenanceRecord,
    ProducerDescriptor,
) {
    let mut document = fixture_document();
    let mut file = document.files.remove(0);
    let mut provenance = document.provenance.remove(0);
    file.id = source.span().file();
    file.repository = source.repository();
    file.generation = source.generation();
    file.content_hash = source.content_hash();
    file.byte_length = source.span().end_byte();
    file.evidence.source = Some(source.clone());
    file.evidence.derivation.clear();
    file.provenance = provenance.id;
    provenance.repository = source.repository();
    provenance.generation = source.generation();
    provenance.input_sources = vec![source.clone()];
    provenance.evidence_sources = vec![source.clone()];
    provenance.derivation_parents.clear();
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
    ParseCapabilities::new(
        vec![LanguageId::new("rust").expect("test language is safe")],
        vec![EncodingId::new("utf-8").expect("test encoding is safe")],
        4096,
        32,
        8,
        true,
        true,
        true,
        true,
        4,
        MemoryEnforcement::AccountedInProcess,
    )
    .expect("test parser capabilities are valid")
}

fn limits(batch_records: usize, ir: IrLimits) -> AnalysisLimits {
    let batch =
        BatchThresholds::new(batch_records, 32 * 1024, 16, 4096).expect("batch limits are valid");
    let stream = StreamLimits::new(64, 128, 1024 * 1024, 64, 64 * 1024, 64 * 1024, batch)
        .expect("stream limits are valid");
    AnalysisLimits::new(4096, 32, 8, 1024 * 1024, stream.clone(), stream, ir)
        .expect("analysis limits are valid")
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
    AnalysisRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("fixture snapshot binds"),
        LanguageId::new("rust").expect("test language is safe"),
        AnalysisTier::TierC,
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

fn source_len(source: &SourceRef) -> usize {
    usize::try_from(source.span().end_byte()).expect("small fixture length fits")
}
