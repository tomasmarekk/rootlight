//! Public contract tests for transactional whole-project adapter analysis.
//!
//! Fixtures bind multiple real VFS snapshots to one repository generation and
//! exercise admission, aggregate quotas, context identity, and atomic commit.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisUnitId, BatchThresholds, BuildTargetId, CoverageReport,
    GeneratedOriginMapping, GenerationBoundSnapshot, IrBatchSink, LanguageId,
    MemoryAdmissionPolicy, MemoryEnforcement, ProjectAnalysisLimits, ProjectAnalysisReport,
    ProjectAnalysisRequest, ProjectLanguageAnalyzer, ProjectSourceInput, RequestError,
    ResourceKind, ResourceUsage, StreamEnd, StreamLimits, TransformationId, WorkReport,
    execute_project_analysis, testkit::MockProjectLanguageAnalyzer,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationIdentity, content_hash, derive_generation};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, ExtensionSupport, FactEvidence, FileRecord,
    IrLimits, ProducerIdentity, ProducerKind, ProvenanceRecord, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

#[test]
fn project_analyzer_sees_all_inputs_and_commits_one_document() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let context = b"{\"target\":\"workspace\"}";
    let request = request(&limits, context, fixture.inputs_with_mapping());
    let (descriptor, records) = records(&fixture, request.build_context(), AnalysisTier::TierB);
    let analyzer = MockProjectLanguageAnalyzer::new(
        descriptor,
        records,
        complete_coverage(AnalysisTier::TierB, request.total_source_bytes()),
        1,
    )
    .with_syntax_nodes(4)
    .with_reported_memory_bytes(256);

    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &deadline(),
    )
    .expect("canonical project analysis commits");

    assert_eq!(output.document().files.len(), 2);
    assert_eq!(output.document().provenance.len(), 1);
    assert_eq!(output.report().analysis_unit(), request.analysis_unit());
    assert_eq!(output.report().build_target(), request.build_target());
    assert_eq!(output.report().build_context(), request.build_context());
    assert_eq!(output.report().requested_tier(), request.requested_tier());
}

#[test]
fn project_request_rejects_duplicate_and_noncanonical_inputs() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let context = b"{}";
    let mut reversed = fixture.inputs();
    reversed.reverse();
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        reversed,
        AnalysisTier::TierA,
        &limits,
    )
    .expect_err("noncanonical input order is rejected");
    assert_eq!(error, RequestError::ProjectPathOrder { index: 1 });

    let first = fixture.input(0, false, Vec::new());
    let duplicate = first.clone();
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        vec![first, duplicate],
        AnalysisTier::TierA,
        &limits,
    )
    .expect_err("duplicate file identity is rejected");
    assert_eq!(error, RequestError::DuplicateProjectFile { index: 1 });
}

#[test]
fn project_request_authenticates_context_and_generation() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let context = b"{\"cfg\":1}";
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(b"different"),
        context,
        fixture.inputs(),
        AnalysisTier::TierA,
        &limits,
    )
    .expect_err("stale context digest is rejected");
    assert_eq!(error, RequestError::ProjectConfigurationDigestMismatch);

    let original = &fixture.sources[1];
    let alternate_generation = derive_generation(GenerationIdentity {
        repository: original.repository(),
        parent: Some(original.generation()),
        manifest_hash: content_hash(b"alternate-manifest"),
        config_hash: content_hash(b"alternate-config"),
        provider_set_hash: content_hash(b"alternate-providers"),
        format_version: 1,
    })
    .id();
    let rebound = SourceRef::new(
        original.repository(),
        alternate_generation,
        original.span(),
        original.content_hash(),
        None,
    );
    let mismatched = ProjectSourceInput::new(
        GenerationBoundSnapshot::new(&fixture.snapshots[1], &rebound)
            .expect("snapshot can bind an explicit generation"),
        language(),
        encoding(),
        false,
        Vec::new(),
    );
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        vec![fixture.input(0, false, Vec::new()), mismatched],
        AnalysisTier::TierA,
        &limits,
    )
    .expect_err("mixed generations are rejected");
    assert_eq!(error, RequestError::ProjectGenerationMismatch { index: 1 });
}

#[test]
fn project_request_enforces_file_total_context_and_mapping_quotas() {
    let fixture = ProjectFixture::new();
    let context = b"manifest";
    let file_limits = limits(ProjectAnalysisLimits::new(1, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        fixture.inputs(),
        AnalysisTier::TierA,
        &file_limits,
    )
    .expect_err("project file quota is enforced");
    assert_eq!(
        error,
        RequestError::ProjectLimit {
            resource: ResourceKind::ProjectFiles,
            observed: 2,
            limit: 1,
        }
    );

    let total_limits = limits(ProjectAnalysisLimits::new(4, 1, 1024, 8, 4096, 128, 128).unwrap());
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        fixture.inputs(),
        AnalysisTier::TierA,
        &total_limits,
    )
    .expect_err("aggregate source quota is enforced");
    assert!(matches!(
        error,
        RequestError::ProjectLimit {
            resource: ResourceKind::ProjectSourceBytes,
            limit: 1,
            ..
        }
    ));

    let context_limits = limits(ProjectAnalysisLimits::new(4, 4096, 2, 8, 4096, 128, 128).unwrap());
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        fixture.inputs(),
        AnalysisTier::TierA,
        &context_limits,
    )
    .expect_err("context quota is enforced");
    assert_eq!(
        error,
        RequestError::ProjectLimit {
            resource: ResourceKind::ProjectContextBytes,
            observed: context.len(),
            limit: 2,
        }
    );

    let target_limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 2).unwrap());
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        fixture.inputs(),
        AnalysisTier::TierA,
        &target_limits,
    )
    .expect_err("build-target identity quota is enforced");
    assert!(matches!(
        error,
        RequestError::ProjectLimit {
            resource: ResourceKind::BuildTargetBytes,
            limit: 2,
            ..
        }
    ));

    let mapping_limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 1, 1, 128, 128).unwrap());
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        fixture.inputs_with_mapping(),
        AnalysisTier::TierA,
        &mapping_limits,
    )
    .expect_err("mapping byte quota is enforced");
    assert!(matches!(
        error,
        RequestError::ProjectLimit {
            resource: ResourceKind::GeneratedMappingBytes,
            ..
        }
    ));
}

#[test]
fn project_request_rejects_invalid_generated_origins() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let mapping = fixture.mapping();
    let context = b"{}";
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        vec![
            fixture.input(0, false, Vec::new()),
            fixture.input(1, false, vec![mapping]),
        ],
        AnalysisTier::TierA,
        &limits,
    )
    .expect_err("origins require an explicit generated classification");
    assert_eq!(
        error,
        RequestError::OriginsRequireGeneratedSource { index: 1 }
    );

    let invalid = GeneratedOriginMapping::new(
        SourceSpan::new(
            fixture.sources[1].span().file(),
            0,
            fixture.sources[1].span().end_byte() + 1,
        )
        .expect("ordered out-of-bounds span constructs"),
        fixture.snapshots[0].path().clone(),
        fixture.sources[0].span(),
        transformation(),
        None,
    );
    let error = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        vec![
            fixture.input(0, false, Vec::new()),
            fixture.input(1, true, vec![invalid]),
        ],
        AnalysisTier::TierA,
        &limits,
    )
    .expect_err("generated ranges must remain inside their source");
    assert_eq!(
        error,
        RequestError::GeneratedOriginOutsideSource {
            input: 1,
            mapping: 0,
        }
    );
}

#[test]
fn project_execution_enforces_deadline_cancellation_and_output_quota() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let context = b"{}";
    let project_request = request(&limits, context, fixture.inputs());
    let (descriptor, output_records) = records(
        &fixture,
        project_request.build_context(),
        AnalysisTier::TierB,
    );
    let analyzer = MockProjectLanguageAnalyzer::new(
        descriptor,
        output_records,
        complete_coverage(AnalysisTier::TierB, project_request.total_source_bytes()),
        1,
    );

    let error = execute_project_analysis(
        &analyzer,
        &project_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &Cancellation::new(),
    )
    .expect_err("deadline is mandatory");
    assert_eq!(
        error,
        AdapterError::RejectedRequest(RequestError::DeadlineRequired)
    );

    let cancelled = MockProjectLanguageAnalyzer::new(
        analyzer.descriptor().clone(),
        Vec::new(),
        complete_coverage(AnalysisTier::TierB, project_request.total_source_bytes()),
        0,
    )
    .with_cancellation_after_batches(0, CancellationReason::ClientRequest);
    let cancellation = deadline();
    cancellation.cancel(CancellationReason::ClientRequest);
    let error = execute_project_analysis(
        &cancelled,
        &project_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &cancellation,
    )
    .expect_err("pre-commit cancellation discards the transaction");
    assert_eq!(
        error,
        AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest,
        }
    );

    let batch = BatchThresholds::new(1, 32 * 1024, 16, 4096).unwrap();
    let stream = StreamLimits::new(64, 128, 1024 * 1024, 64, 64 * 1024, 64 * 1024, batch).unwrap();
    let cancellable_limits = AnalysisLimits::new(
        2048,
        4096,
        64,
        8,
        1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
    .with_project_limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let cancellable_request = request(&cancellable_limits, context, fixture.inputs());
    let (descriptor, output_records) = records(
        &fixture,
        cancellable_request.build_context(),
        AnalysisTier::TierB,
    );
    let analyzer = MockProjectLanguageAnalyzer::new(
        descriptor,
        output_records,
        complete_coverage(
            AnalysisTier::TierB,
            cancellable_request.total_source_bytes(),
        ),
        1,
    )
    .with_cancellation_after_batches(1, CancellationReason::ResourceLimit);
    let error = execute_project_analysis(
        &analyzer,
        &cancellable_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &deadline(),
    )
    .expect_err("cancellation after staged output rolls back the transaction");
    assert_eq!(
        error,
        AdapterError::Cancelled {
            reason: CancellationReason::ResourceLimit,
        }
    );

    let batch = BatchThresholds::new(8, 1, 16, 4096).unwrap();
    let stream = StreamLimits::new(64, 128, 1, 64, 64 * 1024, 64 * 1024, batch).unwrap();
    let tight_limits = AnalysisLimits::new(
        2048,
        4096,
        64,
        8,
        1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
    .with_project_limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let tight_request = request(&tight_limits, context, fixture.inputs());
    let (descriptor, records) =
        records(&fixture, tight_request.build_context(), AnalysisTier::TierB);
    let analyzer = MockProjectLanguageAnalyzer::new(
        descriptor,
        records,
        complete_coverage(AnalysisTier::TierB, tight_request.total_source_bytes()),
        1,
    );
    let error = execute_project_analysis(
        &analyzer,
        &tight_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &deadline(),
    )
    .expect_err("project output bytes remain bounded");
    assert!(matches!(
        error,
        AdapterError::Sink(rootlight_adapter_sdk::SinkError::BatchLimit {
            resource: ResourceKind::OutputBytes,
            ..
        })
    ));
}

#[test]
fn project_report_identity_mismatch_never_returns_partial_output() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let context = b"{}";
    let request = request(&limits, context, fixture.inputs());
    let descriptor = descriptor(&fixture, request.build_context(), AnalysisTier::TierB);
    let analyzer = WrongContextAnalyzer { descriptor };

    let error = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &deadline(),
    )
    .expect_err("a mismatched report cannot commit");
    assert_eq!(
        error,
        AdapterError::InvalidReport(
            rootlight_adapter_sdk::ReportError::ProjectBuildContextMismatch
        )
    );
}

#[test]
fn requested_tier_is_the_highest_permitted_project_tier() {
    let fixture = ProjectFixture::new();
    let limits = limits(ProjectAnalysisLimits::new(4, 4096, 1024, 8, 4096, 128, 128).unwrap());
    let context = b"{}";
    let request = ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build")),
        content_hash(context),
        context,
        fixture.inputs(),
        AnalysisTier::TierB,
        &limits,
    )
    .expect("project request is valid");
    let analyzer = MockProjectLanguageAnalyzer::new(
        descriptor(&fixture, request.build_context(), AnalysisTier::TierA),
        Vec::new(),
        complete_coverage(AnalysisTier::TierA, request.total_source_bytes()),
        0,
    );

    let error = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &deadline(),
    )
    .expect_err("a tier above the requested ceiling is rejected");
    assert_eq!(
        error,
        AdapterError::RejectedRequest(RequestError::UnsupportedTier)
    );
}

struct WrongContextAnalyzer {
    descriptor: rootlight_adapter_sdk::ProducerDescriptor,
}

impl ProjectLanguageAnalyzer for WrongContextAnalyzer {
    fn descriptor(&self) -> &rootlight_adapter_sdk::ProducerDescriptor {
        &self.descriptor
    }

    fn analyze_project(
        &self,
        request: &ProjectAnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        _cancellation: &Cancellation,
    ) -> Result<ProjectAnalysisReport, AdapterError> {
        let usage = sink.staged_usage();
        let work = WorkReport::new(
            complete_coverage(self.descriptor.tier(), request.total_source_bytes()),
            ResourceUsage::new(request.total_source_bytes(), 0, 0, 0, Some(0), usage),
            StreamEnd::new(sink.next_sequence(), usage),
        )?;
        Ok(ProjectAnalysisReport::new(
            work,
            request.analysis_unit().clone(),
            request.build_target().clone(),
            BuildContextIdentity::new(content_hash(b"wrong")),
            request.requested_tier(),
        ))
    }
}

struct ProjectFixture {
    _temporary: TempDir,
    snapshots: [SourceSnapshot; 2],
    sources: [SourceRef; 2],
}

impl ProjectFixture {
    fn new() -> Self {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("local temporary directory is available");
        fs::create_dir(temporary.path().join("src")).expect("fixture source directory is created");
        fs::write(temporary.path().join("src/a.rs"), b"pub fn a() {}\n")
            .expect("first fixture source is written");
        fs::write(temporary.path().join("src/b.rs"), b"pub fn b() { a(); }\n")
            .expect("second fixture source is written");
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
            .parse()
            .expect("checked repository identity parses");
        let generation = "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .expect("checked generation identity parses");
        let repository =
            RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
        let paths = ["src/a.rs", "src/b.rs"].map(|path| {
            RelativePath::parse(Path::new(path)).expect("fixture relative path is valid")
        });
        let snapshots = paths.map(|path| {
            repository
                .snapshot(&path, 1024)
                .expect("fixture snapshot is stable")
        });
        let sources = snapshots.each_ref().map(|snapshot| {
            let end = u64::try_from(snapshot.content().len()).expect("small fixture length fits");
            SourceRef::new(
                repository_id,
                generation,
                SourceSpan::new(snapshot.file(), 0, end).expect("full-file span is valid"),
                snapshot.content_hash(),
                None,
            )
        });
        Self {
            _temporary: temporary,
            snapshots,
            sources,
        }
    }

    fn input(
        &self,
        index: usize,
        generated: bool,
        origins: Vec<GeneratedOriginMapping>,
    ) -> ProjectSourceInput<'_> {
        ProjectSourceInput::new(
            GenerationBoundSnapshot::new(&self.snapshots[index], &self.sources[index])
                .expect("fixture snapshot binds"),
            language(),
            encoding(),
            generated,
            origins,
        )
    }

    fn inputs(&self) -> Vec<ProjectSourceInput<'_>> {
        vec![
            self.input(0, false, Vec::new()),
            self.input(1, false, Vec::new()),
        ]
    }

    fn inputs_with_mapping(&self) -> Vec<ProjectSourceInput<'_>> {
        vec![
            self.input(0, false, Vec::new()),
            self.input(1, true, vec![self.mapping()]),
        ]
    }

    fn mapping(&self) -> GeneratedOriginMapping {
        GeneratedOriginMapping::new(
            SourceSpan::new(self.sources[1].span().file(), 0, 3)
                .expect("generated fixture span is valid"),
            self.snapshots[0].path().clone(),
            SourceSpan::new(self.sources[0].span().file(), 0, 3)
                .expect("origin fixture span is valid"),
            transformation(),
            Some(content_hash(b"generator")),
        )
    }
}

fn records(
    fixture: &ProjectFixture,
    build_context: BuildContextIdentity,
    tier: AnalysisTier,
) -> (
    rootlight_adapter_sdk::ProducerDescriptor,
    Vec<rootlight_adapter_sdk::IrRecord>,
) {
    let provenance_id = "fact1_aeaqcaibaeaqcaibaeaqcaibaeaqcaibwbicmga"
        .parse()
        .expect("checked provenance identity parses");
    let descriptor = descriptor(fixture, build_context, tier);
    let provenance = ProvenanceRecord {
        id: provenance_id,
        repository: fixture.sources[0].repository(),
        generation: fixture.sources[0].generation(),
        producer_kind: descriptor.kind(),
        producer: descriptor.identity().clone(),
        binary_digest: content_hash(b"project-adapter"),
        frontend_version: Some("project-test-1".to_owned()),
        language: "rust".to_owned(),
        tier,
        build_context,
        input_sources: fixture.sources.to_vec(),
        evidence_sources: fixture.sources.to_vec(),
        derivation_parents: Vec::new(),
        rule: None,
    };
    let mut output = Vec::new();
    for (snapshot, source) in fixture.snapshots.iter().zip(&fixture.sources) {
        output.push(rootlight_adapter_sdk::IrRecord::File(FileRecord {
            id: source.span().file(),
            repository: source.repository(),
            generation: source.generation(),
            path: snapshot.path().as_str().to_owned(),
            path_locator: Some(snapshot.path().to_locator()),
            content_hash: source.content_hash(),
            byte_length: source.span().end_byte(),
            language: "rust".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: snapshot.path().as_str().ends_with("b.rs"),
            provenance: provenance_id,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        }));
    }
    output.push(rootlight_adapter_sdk::IrRecord::Provenance(provenance));
    (descriptor, output)
}

fn descriptor(
    fixture: &ProjectFixture,
    _build_context: BuildContextIdentity,
    tier: AnalysisTier,
) -> rootlight_adapter_sdk::ProducerDescriptor {
    rootlight_adapter_sdk::ProducerDescriptor::new(
        ProducerIdentity::new(
            "rootlight-project-sdk-test",
            "1.0",
            fixture.sources[0].content_hash(),
        )
        .expect("test producer identity is valid"),
        ProducerKind::Compiler,
        language(),
        tier,
        MemoryEnforcement::AccountedInProcess,
        false,
    )
}

fn request<'a>(
    limits: &'a AnalysisLimits,
    context: &'a [u8],
    inputs: Vec<ProjectSourceInput<'a>>,
) -> ProjectAnalysisRequest<'a> {
    ProjectAnalysisRequest::new(
        analysis_unit(),
        build_target(),
        BuildContextIdentity::new(content_hash(b"build-context")),
        content_hash(context),
        context,
        inputs,
        AnalysisTier::TierA,
        limits,
    )
    .expect("project request is valid")
}

fn limits(project: ProjectAnalysisLimits) -> AnalysisLimits {
    let batch = BatchThresholds::new(8, 32 * 1024, 16, 4096).unwrap();
    let stream = StreamLimits::new(64, 128, 1024 * 1024, 64, 64 * 1024, 64 * 1024, batch).unwrap();
    AnalysisLimits::new(
        2048,
        4096,
        64,
        8,
        1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
    .with_project_limits(project)
}

fn complete_coverage(tier: AnalysisTier, source_bytes: usize) -> CoverageReport {
    CoverageReport::new(
        tier,
        CoverageStatus::Complete,
        source_bytes,
        source_bytes,
        0,
        Vec::new(),
    )
    .unwrap()
}

fn analysis_unit() -> AnalysisUnitId {
    AnalysisUnitId::new("workspace.core").unwrap()
}

fn build_target() -> BuildTargetId {
    BuildTargetId::new("//workspace:core").unwrap()
}

fn transformation() -> TransformationId {
    TransformationId::new("codegen.v1").unwrap()
}

fn language() -> LanguageId {
    LanguageId::new("rust").unwrap()
}

fn encoding() -> rootlight_adapter_sdk::EncodingId {
    rootlight_adapter_sdk::EncodingId::utf8()
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline derives"),
    )
}
