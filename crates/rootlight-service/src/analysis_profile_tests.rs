//! Behavioral checks for opt-in profiling without changing indexing decisions.
//! Real compressed chunks exercise Weak lifetimes; injected reservation loss
//! checks incomplete telemetry without relying on allocator exhaustion.

use super::*;
use std::{fs, panic::AssertUnwindSafe};
use tempfile::TempDir;

fn fixture(source: &str) -> TempDir {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("lib.rs"), source).unwrap();
    directory
}

struct FailingProfileParser {
    original: Arc<dyn StructuralPreflight>,
    prepare_error: AdapterError,
    identity_result: Result<usize, AdapterError>,
    cancel_during_identity: bool,
}

impl ParseProvider for FailingProfileParser {
    fn capabilities(&self) -> &rootlight_adapter_sdk::ParseCapabilities {
        self.original.capabilities()
    }

    fn parse(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _sink: &mut dyn rootlight_adapter_sdk::SyntaxFactSink,
        _cancellation: &Cancellation,
    ) -> Result<rootlight_adapter_sdk::ParseReport, AdapterError> {
        Err(self.prepare_error.clone())
    }
}

impl StructuralPreflight for FailingProfileParser {
    fn required_syntax_fact_count(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        if self.cancel_during_identity {
            cancellation.cancel(CancellationReason::ClientRequest);
        }
        self.identity_result.clone()
    }
}

fn failing_profile_service(
    prepare_error: AdapterError,
    identity_result: Result<usize, AdapterError>,
    cancel_during_identity: bool,
) -> FirstSliceService {
    let mut service = FirstSliceService::new(2).unwrap();
    let parser = Arc::new(FailingProfileParser {
        original: Arc::clone(&service.structural_parsers["rust"]),
        prepare_error,
        identity_result,
        cancel_during_identity,
    });
    let analyzer = TreeSitterAnalyzer::new_rust_structural(
        Arc::clone(&parser) as Arc<dyn ParseProvider>,
        ProducerIdentity::new("profile-failure-provider", "1", content_hash(b"profile")).unwrap(),
        LanguageId::new("rust").unwrap(),
        "profile-failure-1",
        content_hash(b"profile-failure-provider"),
    )
    .unwrap();
    service.analyzers.insert("rust".to_owned(), analyzer);
    service.structural_parsers.insert("rust".to_owned(), parser);
    service
}

fn profile_provider_error(code: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: rootlight_adapter_sdk::DiagnosticCode::new(code).unwrap(),
    }
}

#[test]
fn preflight_failure_profile_preserves_provider_code_and_physical_identity() {
    fn require_copy<T: Copy>() {}
    require_copy::<FirstSliceError>();
    let source = "// PrivateFailureSourceCanary\npub fn retained() {}\n";
    let directory = fixture(source);
    let mut service = failing_profile_service(
        profile_provider_error("known-provider-failure"),
        Ok(1),
        false,
    );
    let repository = RepositoryId::from_bytes([37; 16]);
    let canonical = canonical_repository_root(directory.path(), &Cancellation::new()).unwrap();
    service
        .restore_repository_registration(repository_path_hash(&canonical).unwrap(), repository)
        .unwrap();
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_eq!(result.unwrap_err(), FirstSliceError::Adapter);
    assert!(service.receipts.is_empty());
    let encoded = serde_json::to_value(&profile).unwrap();
    let failure = &encoded["terminal_file_failure"];
    assert_eq!(failure["failure"]["code"], "known-provider-failure");
    assert_eq!(failure["failure"]["kind"], "provider_failed");
    assert_eq!(failure["repository"], repository.to_string());
    let relative = RelativePath::parse(Path::new("lib.rs")).unwrap();
    let file = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
        repository,
        path_identity: relative.identity_bytes(),
    })
    .id();
    assert_eq!(failure["file"], file.to_string());
    assert_eq!(
        failure["content_hash"],
        content_hash(source.as_bytes()).to_string()
    );
    assert_eq!(failure["language"], "rust");
    assert_eq!(failure["phase"], "preflight");
    assert_eq!(failure["invocation"], "prepare_artifact");
    assert!(failure["preceding_prepare_failure"].is_null());
    let json = encoded.to_string();
    assert!(!json.contains("PrivateFailureSourceCanary"));
    assert!(!json.contains("lib.rs"));
    assert!(!json.contains(&directory.path().display().to_string()));
    assert!(profile.events_complete && profile.counts_complete);
}

#[test]
fn preflight_failure_profile_distinguishes_identity_failure_from_prepare_attempt() {
    let directory = fixture("pub fn ordinary() {}\n");
    let mut service = failing_profile_service(
        profile_provider_error("prepare-failed"),
        Err(profile_provider_error("identity-failed")),
        false,
    );
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_eq!(result.unwrap_err(), FirstSliceError::Adapter);
    assert!(service.receipts.is_empty());
    let encoded = serde_json::to_value(profile).unwrap();
    let failure = &encoded["terminal_file_failure"];
    assert_eq!(failure["invocation"], "identity_count_after_prepare_error");
    assert_eq!(failure["failure"]["code"], "identity-failed");
    assert_eq!(
        failure["preceding_prepare_failure"]["code"],
        "prepare-failed"
    );
    assert_eq!(failure["mapped_outcome"], "adapter");
    assert_eq!(failure["details_complete"], true);
}

#[test]
fn preflight_failure_profile_preserves_request_report_sink_and_resource_outcomes() {
    let directory = fixture("pub fn ordinary() {}\n");
    for (error, kind, code, expected) in [
        (
            AdapterError::RejectedRequest(RequestError::Snapshot(
                rootlight_adapter_sdk::SnapshotError::ContentHashMismatch,
            )),
            "request_rejected",
            "snapshot_content_hash_mismatch",
            FirstSliceError::Adapter,
        ),
        (
            AdapterError::RejectedRequest(RequestError::UnsupportedEncoding),
            "request_rejected",
            "unsupported_encoding",
            FirstSliceError::Adapter,
        ),
        (
            AdapterError::InvalidReport(ReportError::SourceLengthMismatch {
                expected: 1,
                observed: 2,
            }),
            "invalid_report",
            "source_length_mismatch",
            FirstSliceError::Adapter,
        ),
        (
            AdapterError::Sink(SinkError::SourceMismatch),
            "sink_rejected",
            "source_mismatch",
            FirstSliceError::Adapter,
        ),
        (
            AdapterError::RejectedRequest(RequestError::ProviderLimit {
                resource: ResourceKind::SyntaxNodes,
                observed: 19,
                limit: 10,
            }),
            "request_rejected",
            "provider_limit",
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::SyntaxNodes,
                observed: 19,
                limit: 10,
            },
        ),
    ] {
        let mut service = failing_profile_service(error.clone(), Ok(1), false);
        assert_eq!(
            service
                .index_rust_fixture(directory.path(), &Cancellation::new())
                .unwrap_err(),
            expected
        );
        let (result, profile) = capture_first_slice_analysis_profile(|| {
            service.index_rust_fixture(directory.path(), &Cancellation::new())
        });
        assert_eq!(result.unwrap_err(), expected);
        assert!(service.receipts.is_empty());
        let encoded = serde_json::to_value(profile).unwrap();
        let failure = &encoded["terminal_file_failure"];
        assert_eq!(failure["failure"]["kind"], kind);
        assert_eq!(failure["failure"]["code"], code);
        assert_eq!(
            failure["mapped_outcome"],
            if matches!(expected, FirstSliceError::ResourceLimit { .. }) {
                "resource_limit"
            } else {
                "adapter"
            }
        );
    }
}

#[test]
fn preflight_failure_profile_keeps_current_cancellation_authoritative() {
    let directory = fixture("pub fn ordinary() {}\n");
    let mut service =
        failing_profile_service(profile_provider_error("prepare-failed"), Ok(1), true);
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_eq!(
        result.unwrap_err(),
        FirstSliceError::Cancelled(CancellationReason::ClientRequest)
    );
    assert!(service.receipts.is_empty());
    let encoded = serde_json::to_value(profile).unwrap();
    let failure = &encoded["terminal_file_failure"];
    assert_eq!(failure["failure"]["code"], "prepare-failed");
    assert_eq!(failure["mapped_outcome"], "cancelled");
    assert_eq!(failure["cancellation_reason"], "client_request");
}

#[test]
fn preflight_failure_profile_does_not_promote_handled_encoding_or_identity_capacity() {
    let directory = fixture("pub fn retained() {}\n");
    fs::write(
        directory.path().join("invalid.rs"),
        b"pub fn unavailable() {}\xff",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    let receipt = result.unwrap();
    assert_eq!(receipt.indexed_files, 2);
    assert!(profile.terminal_file_failure.is_none());

    let directory = fixture("pub fn required() {}\n");
    let mut service =
        failing_profile_service(profile_provider_error("prepare-failed"), Ok(1), false);
    let required = service.analysis_limits.syntax_stream().max_records() + 1;
    let parser = Arc::new(FailingProfileParser {
        original: Arc::clone(&service.structural_parsers["rust"]),
        prepare_error: profile_provider_error("prepare-failed"),
        identity_result: Ok(required),
        cancel_during_identity: false,
    });
    service.structural_parsers.insert("rust".to_owned(), parser);
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert!(matches!(
        result,
        Err(FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::RequiredSyntaxFacts,
            ..
        })
    ));
    assert!(profile.terminal_file_failure.is_none());
    assert!(service.receipts.is_empty());
}

#[test]
fn preflight_failure_profile_reports_missing_parser_without_fabricating_provider_failure() {
    let directory = fixture("pub fn ordinary() {}\n");
    let mut service = FirstSliceService::new(2).unwrap();
    service.structural_parsers.remove("rust");
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_eq!(result.unwrap_err(), FirstSliceError::Adapter);
    let encoded = serde_json::to_value(profile).unwrap();
    let failure = &encoded["terminal_file_failure"];
    assert_eq!(failure["failure"]["kind"], "service_setup");
    assert_eq!(failure["failure"]["code"], "missing_structural_parser");
    assert_eq!(failure["invocation"], "preflight_setup");
}

#[test]
fn preflight_failure_profile_labels_are_bounded_without_clipping() {
    let directory = fixture("pub fn ordinary() {}\n");
    let code = "p".repeat(128);
    let mut service = failing_profile_service(profile_provider_error(&code), Ok(1), false);
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        service.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_eq!(result.unwrap_err(), FirstSliceError::Adapter);
    let failure = profile.terminal_file_failure.unwrap();
    assert_eq!(serde_json::to_value(failure.failure).unwrap()["code"], code);
    assert!(failure.details_complete);
    assert!(size_of::<AnalysisProfileFileFailure>() < 1024);
    let language = "l".repeat(64);
    assert_eq!(
        serde_json::to_value(AnalysisProfileLabel::<64>::new(&language)).unwrap(),
        language
    );
    assert!(AnalysisProfileLabel::<64>::new(&"l".repeat(65)).is_none());
    assert!(AnalysisProfileLabel::<128>::new(&"p".repeat(129)).is_none());
    assert!(AnalysisProfileLabel::<128>::new("source text\n").is_none());
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        record_preflight_failure(
            Some(AnalysisProfileFileContext {
                repository: RepositoryId::from_bytes([1; 16]),
                file: FileId::from_bytes([2; 20]),
                content_hash: content_hash(b"x"),
                language: None,
                language_complete: false,
            }),
            AnalysisProfileInvocation::PreflightSetup,
            AnalysisProfileAdapterFailure {
                kind: AnalysisProfileFailureKind::ServiceSetup,
                code: None,
            },
            None,
            FirstSliceError::Adapter,
        );
    });
    let failure = profile.terminal_file_failure.unwrap();
    assert!(!failure.details_complete);
    assert!(failure.failure.code.is_none() && failure.language.is_none());
}

#[test]
fn locate_profile_preserves_results_errors_and_nested_scopes() {
    let root = fixture("// PhysicalSourceWitness\npub fn carrier() {}\n");
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_rust_fixture(root.path(), &Cancellation::new())
        .unwrap();
    let query = || {
        service.code_locate_with_filters_and_budget(
            receipt.generation,
            "PhysicalSourceWitness".to_owned(),
            LocateMode::Text,
            Vec::new(),
            vec!["lib.rs".to_owned()],
            64,
            0,
            FirstSliceBudget::default(),
            &Cancellation::new(),
        )
    };
    let expected = query().unwrap();
    let (actual, profile) = with_first_slice_locate_profile(query);
    let actual = actual.unwrap();
    assert!(!expected.data.hits.is_empty());
    assert_eq!(actual.data, expected.data);
    assert!(profile.scope_completed && profile.telemetry_complete);
    assert_eq!(profile.lease.scopes, 1);
    assert_eq!(profile.immutable_source_read.scopes, 1);
    assert_eq!(profile.projection.scopes, 1);
    assert_eq!(profile.ephemeral_build.scopes, 1);
    assert!(profile.text_projection.is_some());
    let encoded = serde_json::to_string(&profile).unwrap();
    assert!(!encoded.contains("PhysicalSourceWitness") && !encoded.contains("lib.rs"));
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    let (failed, failed_profile) = with_first_slice_locate_profile(|| {
        service.code_locate(
            receipt.generation,
            "carrier".to_owned(),
            LocateMode::Exact,
            1,
            0,
            &cancellation,
        )
    });
    assert!(matches!(failed, Err(FirstSliceError::Cancelled(_))));
    assert!(failed_profile.scope_completed);
    assert_eq!(failed_profile.lease.scopes, 0);
    let bounded = || {
        service.code_locate_with_filters_and_budget(
            receipt.generation,
            "PhysicalSourceWitness".to_owned(),
            LocateMode::Text,
            Vec::new(),
            vec!["lib.rs".to_owned()],
            64,
            0,
            FirstSliceBudget::default().reduce_max_duration(Duration::from_nanos(1)),
            &Cancellation::new(),
        )
    };
    assert!(matches!(
        bounded(),
        Err(FirstSliceError::Cancelled(
            CancellationReason::DeadlineExceeded
        ))
    ));
    let (bounded_result, bounded_profile) = with_first_slice_locate_profile(bounded);
    assert!(matches!(
        bounded_result,
        Err(FirstSliceError::Cancelled(
            CancellationReason::DeadlineExceeded
        ))
    ));
    assert_eq!(bounded_profile.projection.scopes, 1);
    assert_eq!(bounded_profile.ephemeral_build.scopes, 0);
    assert_eq!(bounded_profile.execution.scopes, 0);
    assert!(!bounded_profile.text_projection.unwrap().complete);
    let (inner, outer) = with_first_slice_locate_profile(|| {
        let inner = with_first_slice_locate_profile(query).1;
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            with_first_slice_locate_profile(|| panic!("nested observation failure"));
        }));
        query().unwrap();
        inner
    });
    assert_eq!(inner.lease.scopes, 1);
    assert_eq!(outer.lease.scopes, 1);
    assert!(outer.telemetry_complete);
    assert!(!locate_profile::enabled());
}

fn entry(source: &str) -> StructuralArtifactEntry {
    let directory = fixture(source);
    let service = FirstSliceService::new(2).unwrap();
    let prepared = service
        .prepare_rust_fixture(directory.path(), &Cancellation::new())
        .unwrap();
    let FirstSliceIndexPreparation::Pending(mut pending) = prepared else {
        panic!("new repository needs a pending generation");
    };
    let (_, entry) = pending.structural_artifacts.by_file.pop_first().unwrap();
    assert!(entry.normalized.is_some());
    entry
}

fn chunk(source: &str) -> Arc<compressed_chunk::CompressedNormalizedFileChunk> {
    entry(source).normalized.unwrap()
}

fn assert_same_index_outcome(
    mut actual: FirstSliceIndexReceipt,
    expected: &FirstSliceIndexReceipt,
) {
    // Separate runs observe different wall times, and opt-in clocks add work.
    actual.elapsed_micros = expected.elapsed_micros;
    assert_eq!(actual, *expected);
}

fn signature_batch() -> ExtensionEnvelope {
    let repository = RepositoryId::from_bytes([71; 16]);
    let generation = GenerationId::from_bytes([72; 20]);
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(FileId::from_bytes([73; 20]), 0, 1).unwrap(),
        content_hash(b"x"),
        None,
    );
    let children = [(74, "fn first()"), (75, "fn second()")]
        .into_iter()
        .map(|(id, text)| {
            let lexical = rootlight_ir::LexicalEvidenceV1::from_complete_text(
                rootlight_ir::LexicalEvidenceKind::Signature,
                FactRef::Entity(SymbolId::from_bytes([id; 20])),
                rootlight_ir::LexicalEvidenceFormat::SourceText,
                text,
            )
            .unwrap();
            rootlight_ir::new_lexical_evidence_envelope(
                repository,
                generation,
                FactId::from_bytes([76; 20]),
                source.clone(),
                &lexical,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    rootlight_ir::new_lexical_signature_batch_envelope(&children).unwrap()
}

fn extension_document(extensions: Vec<ExtensionEnvelope>) -> NormalizedIrDocument {
    let first = extensions.first().unwrap();
    let mut document = NormalizedIrDocument::empty(first.repository, first.generation);
    let logical_count = extensions
        .iter()
        .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
        .map(|extension| optional_extension_logical_count(extension).unwrap())
        .sum::<u64>();
    if logical_count > 0 {
        let mut coverage = CoverageRecord {
            id: FactId::from_bytes([0; 20]),
            repository: first.repository,
            generation: first.generation,
            scope: CoverageScope::File(first.evidence.source.as_ref().unwrap().span().file()),
            domain: IrFactDomain::Extensions,
            tier: AnalysisTier::TierB,
            status: CoverageStatus::Complete,
            discovered: logical_count,
            indexed: logical_count,
            skipped: 0,
            provenance: first.provenance,
            evidence: first.evidence.clone(),
        };
        coverage.id = derive_coverage_record_id(&coverage).unwrap();
        document.coverage_records.push(coverage);
    }
    document.extensions = extensions;
    document
}

fn summary_pressure_fixture() -> (
    NormalizedIrDocument,
    NormalizedIrDocument,
    DocumentAppendState,
    IrLimits,
) {
    let batch = signature_batch();
    let mut required = batch.clone();
    required.namespace = "test.required".to_owned();
    required.payload = "{}".to_owned();
    let mut extensions = vec![batch.clone()];
    extensions.extend(std::iter::repeat_n(required, 32));
    let target = extension_document(extensions);
    let incoming = extension_document(vec![batch]);
    let state = DocumentAppendState::from_document(&target).unwrap();
    let mut limits = IrLimits::default();
    limits.max_extensions = target.extensions.len();
    (target, incoming, state, limits)
}

pub(super) fn metadata_pressure_batch(seed: u8, signature: bool) -> ExtensionEnvelope {
    let template = signature_batch();
    let source = SourceRef::new(
        template.repository,
        template.generation,
        SourceSpan::new(FileId::from_bytes([seed; 20]), 0, 1).unwrap(),
        content_hash(b"x"),
        None,
    );
    let children = (0..2)
        .map(|child| {
            let lexical = rootlight_ir::LexicalEvidenceV1::from_complete_text(
                if signature {
                    rootlight_ir::LexicalEvidenceKind::Signature
                } else {
                    rootlight_ir::LexicalEvidenceKind::CommentSummary
                },
                if signature {
                    FactRef::Entity(SymbolId::from_bytes([seed + child; 20]))
                } else {
                    FactRef::File(source.span().file())
                },
                if signature {
                    rootlight_ir::LexicalEvidenceFormat::SourceText
                } else {
                    rootlight_ir::LexicalEvidenceFormat::PlainText
                },
                &format!("batch {seed} child {child}"),
            )
            .unwrap();
            rootlight_ir::new_lexical_evidence_envelope(
                template.repository,
                template.generation,
                template.provenance,
                source.clone(),
                &lexical,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    if signature {
        rootlight_ir::new_lexical_signature_batch_envelope(&children).unwrap()
    } else {
        rootlight_ir::new_lexical_summary_batch_envelope(&children).unwrap()
    }
}

fn metadata_pressure_document(extensions: Vec<ExtensionEnvelope>) -> NormalizedIrDocument {
    let coverage = extensions
        .iter()
        .flat_map(|extension| extension_document(vec![extension.clone()]).coverage_records)
        .collect();
    let mut document = extension_document(extensions);
    document.coverage_records = coverage;
    document
}

fn metadata_pressure_fixture() -> (NormalizedIrDocument, DocumentAppendState, IrLimits) {
    let extensions = (10..30)
        .map(|seed| metadata_pressure_batch(seed, false))
        .chain((40..50).map(|seed| metadata_pressure_batch(seed, true)))
        .collect();
    let target = metadata_pressure_document(extensions);
    let mut state = DocumentAppendState::from_document(&target).unwrap();
    state.optional_metadata = Some(optional_extension_metadata::Cache::default());
    let mut limits = IrLimits::default();
    limits.max_extensions = target.extensions.len();
    (target, state, limits)
}

#[test]
fn append_metadata_reuses_batch_proofs_while_summaries_remain() {
    let (mut target, mut state, limits) = metadata_pressure_fixture();
    for round in 0..3 {
        let incoming = metadata_pressure_document(vec![metadata_pressure_batch(100 + round, true)]);
        let mut expected_target = target.clone();
        let mut expected_state = state.clone();
        expected_state.optional_metadata = None;
        let (expected_result, expected_profile) = capture_first_slice_analysis_profile(|| {
            append_normalized_document(
                &mut expected_target,
                incoming.clone(),
                &limits,
                &mut expected_state,
            )
        });
        expected_result
            .unwrap_or_else(|error| panic!("uncached append: {error:?} {expected_profile:?}"));
        let (result, profile) = capture_first_slice_analysis_profile(|| {
            append_normalized_document_with_metadata_admission(
                &mut target,
                incoming,
                &limits,
                &mut state,
                &Cancellation::new(),
                Some(&mut |_| Ok(())),
            )
        });
        result.unwrap();
        assert_eq!(target, expected_target);
        assert_eq!(
            state.truncated_extensions,
            expected_state.truncated_extensions
        );
        assert_eq!(
            state.extension_payload_bytes,
            expected_state.extension_payload_bytes
        );
        assert_eq!(
            state.required_extension_usage,
            expected_state.required_extension_usage
        );
        assert_eq!(state.summary_free_prefix, 0);
        assert!(
            target.extensions.iter().any(|extension| {
                extension.version == rootlight_ir::LEXICAL_SUMMARY_BATCH_VERSION
            })
        );
        assert!(profile.append.summary_search_entries_visited >= 11);
        println!(
            "metadata-pressure round={round} signature_decodes={} verified_children={} visits={} omissions={}",
            profile.append.signature_batch_classifications,
            profile.append.signature_batch_verified_children,
            profile.append.summary_search_entries_visited,
            state.truncated_extensions,
        );
        if round > 0 {
            assert!(
                profile.append.signature_batch_classifications <= 3,
                "retained batch proofs must avoid repeating canonical decodes: {:?}",
                profile.append,
            );
        }
    }
}

#[test]
fn append_metadata_admission_failure_preserves_uncached_ir_and_per_file_gaps() {
    let (mut target, mut state, limits) = metadata_pressure_fixture();
    let incoming = metadata_pressure_document(vec![metadata_pressure_batch(100, true)]);
    let mut expected = target.clone();
    let mut expected_state = state.clone();
    expected_state.optional_metadata = None;
    append_normalized_document(
        &mut expected,
        incoming.clone(),
        &limits,
        &mut expected_state,
    )
    .unwrap();
    let mut attempted = 0;
    append_normalized_document_with_metadata_admission(
        &mut target,
        incoming,
        &limits,
        &mut state,
        &Cancellation::new(),
        Some(&mut |_| {
            attempted += 1;
            Err(generation_memory_limit(
                0,
                u64::MAX,
                PendingGenerationMemory::Staged,
            ))
        }),
    )
    .unwrap();
    assert_eq!(attempted, 1);
    assert_eq!(target, expected);
    assert_eq!(
        state.truncated_extensions,
        expected_state.truncated_extensions
    );
    assert_eq!(
        state.extension_payload_bytes,
        expected_state.extension_payload_bytes
    );
    assert_eq!(
        state.required_extension_usage,
        expected_state.required_extension_usage
    );
    assert_eq!(state.usage, expected_state.usage);
    assert_eq!(
        state.optional_metadata,
        Some(optional_extension_metadata::Cache::default())
    );
}

#[test]
fn append_metadata_staging_is_not_committed_after_cancellation_or_later_quota_failure() {
    for cancelled in [false, true] {
        let (mut target, mut state, mut limits) = metadata_pressure_fixture();
        limits.max_coverage_records = target.coverage_records.len();
        let before = target.clone();
        let before_state = state.clone();
        let cancellation = Cancellation::new();
        let mut charged = 0;
        let incoming = metadata_pressure_document(vec![metadata_pressure_batch(100, true)]);
        let (result, profile) = capture_first_slice_analysis_profile(|| {
            append_normalized_document_with_metadata_admission(
                &mut target,
                incoming,
                &limits,
                &mut state,
                &cancellation,
                Some(&mut |bytes| {
                    charged += bytes;
                    if cancelled {
                        cancellation.cancel(CancellationReason::ClientRequest);
                    }
                    Ok(())
                }),
            )
        });
        assert!(
            charged > 0,
            "metadata staging must precede the injected failure"
        );
        if cancelled {
            assert!(matches!(
                result,
                Err(FirstSliceError::Cancelled(
                    CancellationReason::ClientRequest
                ))
            ));
        } else {
            assert!(matches!(
                result,
                Err(FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::Coverage,
                    ..
                })
            ));
            assert!(profile.phases.iter().any(|event| {
                matches!(event.phase, AnalysisProfilePhase::AppendSourceUsageAndQuota)
                    && matches!(
                        event.scope_exit,
                        AnalysisProfileScopeExit::EarlyReturnOrUnwind
                    )
            }));
        }
        assert_eq!(target, before);
        assert_eq!(state, before_state);
    }
}

#[test]
fn append_metadata_rejects_corrupted_retained_payload_without_invalidation() {
    let (mut target, mut state, limits) = metadata_pressure_fixture();
    append_normalized_document_with_metadata_admission(
        &mut target,
        metadata_pressure_document(vec![metadata_pressure_batch(100, true)]),
        &limits,
        &mut state,
        &Cancellation::new(),
        Some(&mut |_| Ok(())),
    )
    .unwrap();
    let retained = target.extensions.last_mut().unwrap();
    let original_id = retained.id;
    retained.payload = retained.payload.replace("batch 100", "batch 101");
    assert_eq!(retained.id, original_id);
    let before = target.clone();
    let before_state = state.clone();
    assert!(matches!(
        append_normalized_document_with_metadata_admission(
            &mut target,
            metadata_pressure_document(vec![metadata_pressure_batch(102, true)]),
            &limits,
            &mut state,
            &Cancellation::new(),
            Some(&mut |_| Ok(())),
        ),
        Err(FirstSliceError::Identity)
    ));
    assert_eq!(target, before);
    assert_eq!(state, before_state);
}

#[test]
fn append_summary_search_reuses_validated_prefix_under_repeated_pressure() {
    let (mut target, incoming, mut state, limits) = summary_pressure_fixture();

    for round in 0..3 {
        let mut expected_target = target.clone();
        let mut expected_incoming = incoming.clone();
        let mut invalidated = state.clone();
        invalidated.invalidate_usage();
        let expected = truncate_optional_extensions(
            &mut expected_target,
            &mut expected_incoming,
            &invalidated,
            0,
            0,
            &limits,
        )
        .unwrap();
        let mut observed_incoming = incoming.clone();
        let (result, profile) = capture_first_slice_analysis_profile(|| {
            truncate_optional_extensions(&mut target, &mut observed_incoming, &state, 0, 0, &limits)
        });
        state = result.unwrap();
        assert_eq!(target, expected_target);
        assert_eq!(observed_incoming, expected_incoming);
        assert_eq!(state.truncated_extensions, expected.truncated_extensions);
        assert_eq!(
            state.extension_payload_bytes,
            expected.extension_payload_bytes
        );
        assert_eq!(
            state.required_extension_usage,
            expected.required_extension_usage
        );
        assert_eq!(
            profile.append.summary_search_entries_visited,
            if round == 0 { 33 } else { 0 },
            "an unchanged validated prefix must not be scanned again",
        );
        assert_eq!(
            profile.append.signature_batch_classifications,
            if round == 0 { 3 } else { 2 },
            "each incoming batch still receives both canonical classifications",
        );
        assert!(profile.events_complete && profile.counts_complete);
    }
}

#[test]
fn append_summary_prefix_tracks_new_summaries_and_earlier_evictions() {
    let (mut target, incoming, state, mut limits) = summary_pressure_fixture();
    let mut state =
        truncate_optional_extensions(&mut target, &mut incoming.clone(), &state, 0, 0, &limits)
            .unwrap();
    let original_prefix = target.extensions.len();
    assert_eq!(state.summary_free_prefix, original_prefix);
    let batch = signature_batch();
    let source = SourceRef::new(
        batch.repository,
        batch.generation,
        SourceSpan::new(FileId::from_bytes([80; 20]), 0, 1).unwrap(),
        content_hash(b"x"),
        None,
    );
    let summary = rootlight_ir::new_lexical_evidence_envelope(
        batch.repository,
        batch.generation,
        batch.provenance,
        source.clone(),
        &rootlight_ir::LexicalEvidenceV1::from_complete_text(
            rootlight_ir::LexicalEvidenceKind::CommentSummary,
            FactRef::File(source.span().file()),
            rootlight_ir::LexicalEvidenceFormat::PlainText,
            "new summary",
        )
        .unwrap(),
    )
    .unwrap();
    limits.max_extensions += 1;
    append_normalized_document(
        &mut target,
        extension_document(vec![summary]),
        &limits,
        &mut state,
    )
    .unwrap();
    assert_eq!(state.summary_free_prefix, original_prefix);
    assert_eq!(target.extensions.len(), original_prefix + 1);

    limits.max_extensions = original_prefix - 1;
    let mut expected_target = target.clone();
    let mut expected_incoming = incoming.clone();
    let mut invalidated = state.clone();
    invalidated.invalidate_usage();
    let expected_state = truncate_optional_extensions(
        &mut expected_target,
        &mut expected_incoming,
        &invalidated,
        0,
        0,
        &limits,
    )
    .unwrap();
    let mut observed_incoming = incoming;
    let (observed_state, profile) = capture_first_slice_analysis_profile(|| {
        truncate_optional_extensions(&mut target, &mut observed_incoming, &state, 0, 0, &limits)
    });
    let observed_state = observed_state.unwrap();
    assert_eq!(target, expected_target);
    assert_eq!(observed_incoming, expected_incoming);
    assert_eq!(observed_state, expected_state);
    assert_eq!(observed_state.summary_free_prefix, target.extensions.len());
    assert_eq!(profile.append.summary_search_entries_visited, 1);
    assert_eq!(profile.append.planned_target_extension_drops, 2);
    assert_eq!(target.coverage_records[0].indexed, 0);
    assert_eq!(target.coverage_records[1].indexed, 0);
}

#[test]
fn append_summary_prefix_preserves_invalidations_errors_and_cancellation() {
    let (mut target, incoming, state, limits) = summary_pressure_fixture();
    let mut state =
        truncate_optional_extensions(&mut target, &mut incoming.clone(), &state, 0, 0, &limits)
            .unwrap();
    assert!(state.summary_free_prefix > 0);
    let original = target.clone();
    let original_state = state.clone();
    let mut malformed = incoming.clone();
    malformed.extensions[0].payload.push(' ');
    assert!(matches!(
        append_normalized_document(&mut target, malformed, &limits, &mut state),
        Err(FirstSliceError::Identity)
    ));
    assert_eq!(target, original);
    assert_eq!(state, original_state);

    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        append_normalized_document_with_cancellation(
            &mut target,
            incoming.clone(),
            &limits,
            &mut state,
            &cancelled,
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(target, original);
    assert_eq!(state, original_state);

    target.extensions[0].payload.push(' ');
    state.invalidate_usage();
    assert_eq!(state.summary_free_prefix, 0);
    let invalid_target = target.clone();
    let invalidated_state = state.clone();
    assert!(matches!(
        append_normalized_document(&mut target, incoming, &limits, &mut state),
        Err(FirstSliceError::Identity)
    ));
    assert_eq!(target, invalid_target);
    assert_eq!(state, invalidated_state);
}

#[test]
fn append_summary_prefix_is_not_committed_after_later_quota_failure() {
    let (mut target, incoming, mut state, mut limits) = summary_pressure_fixture();
    limits.max_coverage_records = 1;
    let before = target.clone();
    let before_state = state.clone();
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        append_normalized_document(&mut target, incoming, &limits, &mut state)
    });
    assert!(result.is_err());
    assert_eq!(target, before);
    assert_eq!(state, before_state);
    assert_eq!(profile.append.summary_search_entries_visited, 33);
    assert!(profile.phases.iter().any(|event| {
        matches!(event.phase, AnalysisProfilePhase::AppendSourceUsageAndQuota)
            && matches!(
                event.scope_exit,
                AnalysisProfileScopeExit::EarlyReturnOrUnwind
            )
    }));
}

#[test]
fn append_search_counts_actual_visits_and_verified_batch_children() {
    let batch = signature_batch();
    let payload_bytes = batch.payload.len();
    let mut required = batch.clone();
    required.namespace = "test.required".to_owned();
    let extensions = vec![batch, required];
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        let mut summary_cursor = extensions.len();
        assert_eq!(
            previous_optional_extension(
                &extensions,
                &mut summary_cursor,
                0,
                LexicalRetentionClass::Summary,
                &Cancellation::new(),
            )
            .unwrap(),
            None,
        );
        assert_eq!(summary_cursor, 0);
        let mut signature_cursor = extensions.len();
        assert_eq!(
            previous_optional_extension(
                &extensions,
                &mut signature_cursor,
                0,
                LexicalRetentionClass::Signature,
                &Cancellation::new(),
            )
            .unwrap(),
            Some(0),
        );
    });
    assert_eq!(profile.append.summary_search_calls, 1);
    assert_eq!(profile.append.signature_search_calls, 1);
    assert_eq!(profile.append.summary_search_entries_visited, 2);
    assert_eq!(profile.append.signature_search_entries_visited, 2);
    assert_eq!(profile.append.signature_batch_classifications, 2);
    assert_eq!(
        profile.append.signature_batch_payload_bytes,
        (payload_bytes * 2) as u128
    );
    assert_eq!(profile.append.signature_batch_verified_children, 4);
    assert_eq!(profile.append.signature_batch_decode_abandoned_scopes, 0);
    assert_eq!(profile.append.atomic_lexical_classifications, 0);
    assert_eq!(
        profile.phases.len(),
        2,
        "decoder observations are aggregates"
    );
    assert!(profile.events_complete && profile.counts_complete);
}

#[test]
fn append_decoder_errors_and_cancellation_preserve_cursor_behavior() {
    let mut batch = signature_batch();
    batch.payload = "{}".to_owned();
    let extensions = vec![batch];
    let mut disabled_cursor = 1;
    assert!(matches!(
        previous_optional_extension(
            &extensions,
            &mut disabled_cursor,
            0,
            LexicalRetentionClass::Summary,
            &Cancellation::new()
        ),
        Err(FirstSliceError::Identity)
    ));
    let mut observed_cursor = 1;
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        previous_optional_extension(
            &extensions,
            &mut observed_cursor,
            0,
            LexicalRetentionClass::Summary,
            &Cancellation::new(),
        )
    });
    assert!(matches!(result, Err(FirstSliceError::Identity)));
    assert_eq!(observed_cursor, disabled_cursor);
    assert_eq!(profile.append.signature_batch_classifications, 1);
    assert_eq!(profile.append.signature_batch_verified_children, 0);
    assert_eq!(profile.append.signature_batch_decode_abandoned_scopes, 1);
    assert!(matches!(
        profile.phases[0].scope_exit,
        AnalysisProfileScopeExit::EarlyReturnOrUnwind
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    let mut cursor = 1;
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        previous_optional_extension(
            &extensions,
            &mut cursor,
            0,
            LexicalRetentionClass::Signature,
            &cancelled,
        )
    });
    assert!(matches!(
        result,
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(cursor, 1);
    assert_eq!(profile.append.signature_search_entries_visited, 0);
    assert_eq!(profile.append.signature_batch_classifications, 0);
    assert!(profile.events_complete && profile.counts_complete);
}

#[test]
fn append_decoder_disabled_unwind_and_counter_loss_remain_explicit() {
    let disabled = SignatureBatchProfileGuard::begin(usize::MAX);
    assert!(disabled.started.is_none());
    disabled.finish(usize::MAX);
    assert!(!analysis_profile_enabled());
    let batch = signature_batch();
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        let unwind = std::panic::catch_unwind(|| {
            let _guard = SignatureBatchProfileGuard::begin(batch.payload.len());
            panic!("controlled decoder observation unwind");
        });
        assert!(unwind.is_err());
        assert!(is_lexical_signature(&batch).unwrap());
    });
    assert_eq!(profile.append.signature_batch_classifications, 2);
    assert_eq!(profile.append.signature_batch_decode_abandoned_scopes, 1);
    assert_eq!(profile.append.signature_batch_verified_children, 2);
    assert!(profile.phases.is_empty());
    assert!(!analysis_profile_enabled());
    let (result, lost) = capture_first_slice_analysis_profile(|| {
        with_analysis_profile(|profile| {
            profile.append.signature_batch_classifications = u64::MAX;
            profile.append.signature_batch_payload_bytes = u128::MAX;
            profile.append.signature_batch_verified_children = u128::MAX;
        });
        is_lexical_signature(&batch)
    });
    assert!(result.unwrap());
    assert!(!lost.counts_complete);
    assert_eq!(lost.append.signature_batch_classifications, u64::MAX);
    assert_eq!(lost.append.signature_batch_payload_bytes, u128::MAX);
    assert_eq!(lost.append.signature_batch_verified_children, u128::MAX);
}

#[test]
fn append_substages_preserve_real_documents_and_cached_usage() {
    let directory = fixture("pub fn first() -> u8 { 1 }\n");
    fs::write(
        directory.path().join("second.rs"),
        "pub fn second() -> u8 { 2 }\n",
    )
    .unwrap();
    let mut reference = FirstSliceService::new(2).unwrap();
    let expected = reference
        .index_rust_fixture(directory.path(), &Cancellation::new())
        .unwrap();
    let root = *reference.repositories.keys().next().unwrap();
    let mut observed = FirstSliceService::new(2).unwrap();
    observed
        .restore_repository_registration(root, expected.repository)
        .unwrap();
    let (actual, profile) = capture_first_slice_analysis_profile(|| {
        observed.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    let actual = actual.unwrap();
    assert_same_index_outcome(actual.clone(), &expected);
    assert_eq!(
        observed
            .loaded_generation_snapshot(actual.generation)
            .unwrap()
            .document(),
        reference
            .loaded_generation_snapshot(expected.generation)
            .unwrap()
            .document(),
    );
    for phase in [
        AnalysisProfilePhase::AppendLexicalCompaction,
        AnalysisProfilePhase::AppendTargetUsage,
        AnalysisProfilePhase::AppendGapAndDiagnosticPlanning,
        AnalysisProfilePhase::AppendOptionalExtensionPlanning,
        AnalysisProfilePhase::AppendSourceUsageAndQuota,
        AnalysisProfilePhase::AppendReservations,
        AnalysisProfilePhase::AppendCommit,
    ] {
        assert!(
            profile
                .phases
                .iter()
                .filter(
                    |event| std::mem::discriminant(&event.phase) == std::mem::discriminant(&phase)
                )
                .count()
                >= 2
        );
    }
    assert!(profile.append.target_usage_cache_misses > 0);
    assert!(profile.append.target_usage_cache_hits > 0);
    assert!(profile.events_complete && profile.counts_complete);
    assert!(profile.native_parse_call_count.is_none());
}

#[test]
fn append_planned_drop_counts_preserve_eviction_and_atomic_cancellation() {
    let batch = signature_batch();
    let source = batch.evidence.source.clone().unwrap();
    let mut target = NormalizedIrDocument::empty(batch.repository, batch.generation);
    let mut coverage = CoverageRecord {
        id: FactId::from_bytes([0; 20]),
        repository: batch.repository,
        generation: batch.generation,
        scope: CoverageScope::File(source.span().file()),
        domain: IrFactDomain::Extensions,
        tier: AnalysisTier::TierB,
        status: CoverageStatus::Complete,
        discovered: 2,
        indexed: 2,
        skipped: 0,
        provenance: batch.provenance,
        evidence: batch.evidence.clone(),
    };
    coverage.id = derive_coverage_record_id(&coverage).unwrap();
    target.coverage_records.push(coverage);
    target.extensions.push(batch.clone());
    let mut incoming = NormalizedIrDocument::empty(batch.repository, batch.generation);
    let mut required = batch;
    required.namespace = "test.required".to_owned();
    required.version = "1".to_owned();
    required.payload = "{}".to_owned();
    incoming.extensions.push(required);
    let state = DocumentAppendState::from_document(&target).unwrap();
    let mut limits = IrLimits::default();
    limits.max_extensions = 1;
    let mut expected_target = target.clone();
    let mut expected_incoming = incoming.clone();
    let expected_state = truncate_optional_extensions(
        &mut expected_target,
        &mut expected_incoming,
        &state,
        0,
        0,
        &limits,
    )
    .unwrap();
    let mut observed_target = target.clone();
    let mut observed_incoming = incoming.clone();
    let (observed_state, profile) = capture_first_slice_analysis_profile(|| {
        truncate_optional_extensions(
            &mut observed_target,
            &mut observed_incoming,
            &state,
            0,
            0,
            &limits,
        )
    });
    assert_eq!(observed_state.unwrap(), expected_state);
    assert_eq!(observed_target, expected_target);
    assert_eq!(observed_incoming, expected_incoming);
    assert_eq!(profile.append.planned_target_extension_drops, 1);
    assert_eq!(profile.append.planned_source_extension_drops, 0);
    assert_eq!(
        expected_state.truncated_extensions, 2,
        "envelope counts differ from logical children"
    );
    let mut cancelled_target = target.clone();
    let mut cancelled_state = state.clone();
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        append_normalized_document_with_cancellation(
            &mut cancelled_target,
            incoming,
            &limits,
            &mut cancelled_state,
            &cancelled,
        )
    });
    assert!(matches!(
        result,
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(cancelled_target, target);
    assert_eq!(cancelled_state, state);
    assert!(profile.phases.is_empty());
    assert_eq!(profile.append.planned_target_extension_drops, 0);

    let mut empty = NormalizedIrDocument::empty(target.repository, target.generation);
    let empty_state = DocumentAppendState::default();
    let mut source_only = target;
    let mut limits = IrLimits::default();
    limits.max_total_extension_bytes = 0;
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        truncate_optional_extensions(&mut empty, &mut source_only, &empty_state, 0, 0, &limits)
    });
    assert_eq!(result.unwrap().truncated_extensions, 2);
    assert!(source_only.extensions.is_empty());
    assert_eq!(profile.append.planned_target_extension_drops, 0);
    assert_eq!(profile.append.planned_source_extension_drops, 1);
}

#[test]
fn expired_identity_cannot_charge_a_later_discard() {
    let first = chunk("pub fn first() -> u8 { 1 }\n");
    let larger_source = (0..32)
        .map(|number| format!("pub fn later_{number}() -> u32 {{ {number} }}\n"))
        .collect::<String>();
    let second = chunk(&larger_source);
    let first_bytes = first.accounted_bytes();
    let second_bytes = second.accounted_bytes();
    assert_ne!(first_bytes, second_bytes);
    assert_eq!(Arc::strong_count(&first), 1);
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        profile_built_chunk(&first);
        assert_eq!(Arc::strong_count(&first), 1);
        assert_eq!(Arc::weak_count(&first), 1);
        drop(first);
        with_analysis_profile(|profile| {
            assert_eq!(profile.pending_built_chunks.len(), 1);
            assert_eq!(profile.pending_built_chunks[0].0.strong_count(), 0);
        });
        profile_built_chunk(&second);
        profile_discarded_chunk(&second);
    });
    assert_eq!(profile.admitted_new_chunks, 2);
    assert_eq!(profile.expired_tracked_new_chunks, 1);
    assert_eq!(profile.expired_tracked_new_chunk_bytes, first_bytes as u128);
    assert_eq!(profile.admitted_new_chunks_discarded, 1);
    assert_eq!(
        profile.admitted_new_chunk_discarded_bytes,
        second_bytes as u128
    );
    assert!(profile.pending_built_chunks.is_empty());
    assert_eq!(Arc::weak_count(&second), 0);
    assert!(profile.counts_complete);
}

#[test]
fn caught_error_and_retry_keep_distinct_identity_charges() {
    let first = chunk("pub fn failed() -> u8 { 1 }\n");
    let second = chunk("pub fn retried() -> u64 { 18446744073709551615 }\n");
    let first_bytes = first.accounted_bytes();
    let second_bytes = second.accounted_bytes();
    let (result, profile) = capture_first_slice_analysis_profile(|| {
        fn failed_operation(
            chunk: Arc<compressed_chunk::CompressedNormalizedFileChunk>,
        ) -> Result<(), FirstSliceError> {
            profile_built_chunk(&chunk);
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest,
            ))
        }
        let failed = failed_operation(first);
        assert!(matches!(
            failed,
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        profile_built_chunk(&second);
        profile_discarded_chunk(&second);
        Ok::<_, FirstSliceError>(())
    });
    assert!(result.is_ok());
    assert_eq!(profile.admitted_new_chunks, 2);
    assert_eq!(profile.expired_tracked_new_chunks, 1);
    assert_eq!(profile.expired_tracked_new_chunk_bytes, first_bytes as u128);
    assert_eq!(profile.admitted_new_chunks_discarded, 1);
    assert_eq!(
        profile.admitted_new_chunk_discarded_bytes,
        second_bytes as u128
    );
}

#[test]
fn finish_releases_live_and_expired_weak_identities() {
    let live = chunk("pub fn live() {}\n");
    let expired = chunk("pub fn expired() {}\n");
    let expired_bytes = expired.accounted_bytes();
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        profile_built_chunk(&live);
        profile_built_chunk(&expired);
        drop(expired);
    });
    assert_eq!(profile.tracked_new_chunks_with_strong_owners_at_finish, 1);
    assert_eq!(profile.expired_tracked_new_chunks, 1);
    assert_eq!(
        profile.expired_tracked_new_chunk_bytes,
        expired_bytes as u128
    );
    assert!(profile.pending_built_chunks.is_empty());
    assert_eq!(Arc::strong_count(&live), 1);
    assert_eq!(Arc::weak_count(&live), 0);
    assert!(!analysis_profile_enabled());
}

#[test]
fn nested_unwind_restores_outer_collector_and_releases_inner_identities() {
    let outer = chunk("pub fn outer() {}\n");
    let inner = chunk("pub fn inner() {}\n");
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        profile_built_chunk(&outer);
        let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
            capture_first_slice_analysis_profile(|| {
                profile_built_chunk(&inner);
                let _guard = AnalysisProfileGuard::begin(AnalysisProfilePhase::Preflight, None);
                panic!("controlled work panic");
            });
        }));
        assert!(panic.is_err());
        assert!(analysis_profile_enabled());
        assert_eq!(Arc::weak_count(&inner), 0);
        assert_eq!(Arc::weak_count(&outer), 1);
        profile_discarded_chunk(&outer);
    });
    assert_eq!(profile.admitted_new_chunks, 1);
    assert_eq!(profile.admitted_new_chunks_discarded, 1);
    assert!(profile.phases.is_empty());
    assert!(!analysis_profile_enabled());
    let ((), later) = capture_first_slice_analysis_profile(|| {});
    assert_eq!(later.admitted_new_chunks, 0);
    assert!(later.phases.is_empty());
}

#[test]
fn captures_are_independent_on_nested_return_and_other_threads() {
    let ((), outer) = capture_first_slice_analysis_profile(|| {
        let ((), inner) = capture_first_slice_analysis_profile(|| {
            profile_analysis_phase!(Preflight, None, ());
        });
        assert_eq!(inner.phases.len(), 1);
        let child = std::thread::spawn(|| {
            assert!(!analysis_profile_enabled());
            let ((), profile) = capture_first_slice_analysis_profile(|| {
                profile_analysis_phase!(FinalVerification, None, ());
            });
            assert_eq!(profile.phases.len(), 1);
        });
        child.join().unwrap();
        profile_analysis_phase!(FinalProjectionAdmission, None, ());
    });
    assert_eq!(outer.phases.len(), 1);
    assert!(matches!(
        outer.phases[0].phase,
        AnalysisProfilePhase::FinalProjectionAdmission
    ));
}

#[test]
fn disabled_hooks_read_no_clock_and_add_no_weak_owners() {
    let chunk = chunk("pub fn unprofiled() {}\n");
    assert!(!analysis_profile_enabled());
    let guard = AnalysisProfileGuard::begin(AnalysisProfilePhase::Preflight, None);
    assert!(guard.started.is_none());
    profile_built_chunk(&chunk);
    profile_discarded_chunk(&chunk);
    guard.finish();
    assert_eq!(Arc::strong_count(&chunk), 1);
    assert_eq!(Arc::weak_count(&chunk), 0);
    assert!(!analysis_profile_enabled());
}

#[test]
fn returned_error_and_propagated_error_have_distinct_scope_exits() {
    let (result, returned) = capture_first_slice_analysis_profile(|| {
        profile_analysis_phase!(Preflight, None, Err::<(), _>(FirstSliceError::Identity))
    });
    assert!(matches!(result, Err(FirstSliceError::Identity)));
    assert!(matches!(
        returned.phases[0].scope_exit,
        AnalysisProfileScopeExit::ExpressionReturned
    ));
    let (result, propagated) =
        capture_first_slice_analysis_profile(|| -> Result<(), FirstSliceError> {
            let guard = AnalysisProfileGuard::begin(AnalysisProfilePhase::Preflight, None);
            Err(FirstSliceError::Identity)?;
            guard.finish();
            Ok(())
        });
    assert!(matches!(result, Err(FirstSliceError::Identity)));
    assert!(matches!(
        propagated.phases[0].scope_exit,
        AnalysisProfileScopeExit::EarlyReturnOrUnwind
    ));
}

#[test]
fn checked_counter_loss_remains_explicit() {
    let counted = chunk("pub fn counted() {}\n");
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        with_analysis_profile(|profile| {
            profile.admitted_new_chunks = u64::MAX;
            profile.admitted_new_chunks_discarded = u64::MAX;
            profile.admitted_new_chunk_discarded_bytes = u128::MAX;
            profile.dropped_events = u64::MAX;
            profile.reject_event_reservation = true;
        });
        profile_built_chunk(&counted);
        profile_discarded_chunk(&counted);
        profile_analysis_phase!(Preflight, None, ());
    });
    assert!(!profile.counts_complete);
    assert!(!profile.events_complete);
    assert_eq!(profile.admitted_new_chunks, u64::MAX);
    assert_eq!(profile.admitted_new_chunks_discarded, u64::MAX);
    assert_eq!(profile.admitted_new_chunk_discarded_bytes, u128::MAX);
    assert_eq!(profile.dropped_events, u64::MAX);
    let expired = chunk("pub fn expired_overflow() {}\n");
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        with_analysis_profile(|profile| {
            profile.expired_tracked_new_chunks = u64::MAX;
            profile.expired_tracked_new_chunk_bytes = u128::MAX;
        });
        profile_built_chunk(&expired);
        drop(expired);
    });
    assert!(!profile.counts_complete);
    assert_eq!(profile.expired_tracked_new_chunks, u64::MAX);
    assert_eq!(profile.expired_tracked_new_chunk_bytes, u128::MAX);
}

#[test]
fn collector_overflow_keeps_compatible_files_for_partial_publication() {
    let mut first = entry("pub fn retained() -> u32 { 1 }\n");
    first.normalized = None;
    let first_file = first.artifact.file();
    let first_artifact = Arc::clone(&first.artifact);
    let budget = first.accounted_bytes().unwrap();
    let mut second = entry("pub fn uncached() -> u32 { 2 }\n");
    second.normalized = None;
    let second_file = second.artifact.file();
    let mut collector = StructuralArtifactCollector::new(budget);
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        collector.push(first, &Cancellation::new()).unwrap();
        collector.push(second, &Cancellation::new()).unwrap();
    });
    assert!(!collector.complete);
    assert_eq!(collector.entries.len(), 1);
    assert_eq!(collector.accounted_bytes, budget);
    assert!(Arc::ptr_eq(
        &collector.get(first_file).unwrap().artifact,
        &first_artifact,
    ));
    assert!(collector.get(second_file).is_none());
    assert_eq!(
        profile
            .first_irreversible_collector_overflow
            .unwrap()
            .parser_entries_cleared,
        0,
    );
    let artifacts = collector.finish(&Cancellation::new()).unwrap();
    assert!(!artifacts.retention_complete);
    let generation = GenerationId::from_bytes([79; 20]);
    let mut retention = StructuralArtifactRetention::new(2, budget).unwrap();
    assert!(
        retention
            .stage(generation, artifacts, &Cancellation::new())
            .unwrap()
    );
    retention.commit_staged(generation).unwrap();
    assert_eq!(retention.retained_bytes, budget);
    assert!(
        retention
            .generation(generation)
            .unwrap()
            .get(first_file)
            .is_some()
    );
    assert!(
        retention
            .generation(generation)
            .unwrap()
            .get(second_file)
            .is_none()
    );
}

#[test]
fn partial_parser_retention_matches_fresh_analysis_after_an_unrelated_edit() {
    let root = TempDir::new().unwrap();
    for (name, source) in [
        ("alpha.rs", "pub fn alpha() -> u32 { 1 }\n"),
        ("bravo.rs", "pub fn bravo() -> u32 { 2 }\n"),
        ("notes.unrecognized", "a\n"),
    ] {
        fs::write(root.path().join(name), source).unwrap();
    }
    let cancellation = Cancellation::new();
    let probe = FirstSliceService::new(2).unwrap();
    let FirstSliceIndexPreparation::Pending(prepared) = probe
        .prepare_rust_fixture(root.path(), &cancellation)
        .unwrap()
    else {
        panic!("new fixture requires preparation");
    };
    let budget = prepared
        .structural_artifacts
        .iter()
        .map(|(_, entry)| entry.artifact.accounted_bytes())
        .max()
        .unwrap();
    drop(prepared);
    drop(probe);
    let mut service = FirstSliceService::new(2).unwrap();
    service.structural_artifacts.maximum_bytes = budget;
    let first = service
        .index_rust_fixture(root.path(), &cancellation)
        .unwrap();
    let retained = service
        .structural_artifacts
        .generation(first.generation)
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert!(!retained.retention_complete);
    assert!(retained.accounted_bytes <= budget);
    fs::write(root.path().join("notes.unrecognized"), "b\n").unwrap();
    let changed = service
        .index_rust_fixture(root.path(), &cancellation)
        .unwrap();
    let evidence = service.incremental_evidence(changed.generation).unwrap();
    assert_eq!(evidence.reused_parser_artifacts(), 1);
    assert_eq!(evidence.parsed_files(), 1);
    assert!(service.structural_artifacts.retained_bytes <= budget);
    let document = service
        .inspect_generation(
            changed.repository,
            Some(changed.generation),
            &cancellation,
            |snapshot| snapshot.document().clone(),
        )
        .unwrap();
    // Compare the complete output against a clean rebuild under the same
    // catalog authority and retained lineage.
    let FirstSliceIndexPreparation::Pending(rebuilt) = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &cancellation,
        )
        .unwrap()
    else {
        panic!("clean rebuild requires fresh preparation");
    };
    assert_eq!(rebuilt.incremental.evidence.parsed_files(), 2);
    assert_eq!(rebuilt.incremental.evidence.reused_parser_artifacts(), 0);
    let expected = rebuilt.verified.document();
    assert_eq!(document.repository, expected.repository);
    let digest = |document: &NormalizedIrDocument| {
        rootlight_ir::canonical_generation_neutral_digests(
            document,
            service.analysis_limits.ir(),
            &service.extensions,
            || true,
        )
        .unwrap()
    };
    assert_eq!(digest(&document), digest(expected));
}

#[test]
fn rejected_cache_replacement_removes_only_its_previous_file() {
    let mut first = entry("pub fn replaced() -> u32 { 1 }\n");
    first.normalized = None;
    let file = first.artifact.file();
    let replacement = StructuralArtifactEntry {
        id: first.id,
        artifact: Arc::clone(&first.artifact),
        normalized: None,
    };
    let mut second = entry("pub fn unaffected() -> u32 { 2 }\n");
    second.normalized = None;
    let second_file = second.artifact.file();
    let second_bytes = second.accounted_bytes().unwrap();
    let budget = first.accounted_bytes().unwrap() + second_bytes;
    let mut collector = StructuralArtifactCollector::new(budget);
    collector.push(first, &Cancellation::new()).unwrap();
    collector.push(second, &Cancellation::new()).unwrap();
    collector.maximum_bytes -= 1;
    collector.push(replacement, &Cancellation::new()).unwrap();
    assert!(!collector.complete);
    assert!(collector.get(file).is_none());
    assert!(collector.get(second_file).is_some());
    assert_eq!(collector.accounted_bytes, second_bytes);
    assert_eq!(collector.by_file[&second_file], 0);
    assert_eq!(collector.entries.len(), 1);
    collector.finish(&Cancellation::new()).unwrap();
}

#[test]
fn collector_overflow_and_later_discards_preserve_first_observation() {
    let first = entry("pub fn first() {}\n");
    let later = entry("pub fn later() {}\n");
    let first_file = first.artifact.file();
    let parser_bytes = first.artifact.accounted_bytes();
    let first_chunk_bytes = first.normalized.as_ref().unwrap().accounted_bytes();
    let later_chunk_bytes = later.normalized.as_ref().unwrap().accounted_bytes();
    let mut collector = StructuralArtifactCollector::new(1);
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        profile_built_chunk(first.normalized.as_ref().unwrap());
        collector.push(first, &Cancellation::new()).unwrap();
        assert!(!collector.complete);
        profile_built_chunk(later.normalized.as_ref().unwrap());
        collector.push(later, &Cancellation::new()).unwrap();
    });
    let first = profile.first_irreversible_collector_overflow.unwrap();
    assert_eq!(first.file, first_file);
    assert_eq!(first.maximum_bytes, 1);
    assert_eq!(first.incoming_parser_bytes, parser_bytes);
    assert_eq!(first.admitted_bytes_after_optional_eviction, parser_bytes);
    assert_eq!(first.retained_bytes_before_clear, 0);
    assert_eq!(first.parser_entries_cleared, 0);
    assert_eq!(profile.admitted_new_chunks, 2);
    assert_eq!(profile.admitted_new_chunks_discarded, 2);
    assert_eq!(
        profile.admitted_new_chunk_discarded_bytes,
        (first_chunk_bytes + later_chunk_bytes) as u128
    );
    assert_eq!(profile.expired_tracked_new_chunks, 0);
}

#[test]
fn old_shared_chunk_is_not_counted_as_new_for_the_same_file() {
    let directory = fixture("pub fn shared() {}\n");
    let service = FirstSliceService::new(2).unwrap();
    let prepared = service
        .prepare_rust_fixture(directory.path(), &Cancellation::new())
        .unwrap();
    let FirstSliceIndexPreparation::Pending(pending) = prepared else {
        panic!("new source needs preparation");
    };
    let old = pending
        .structural_artifacts
        .iter()
        .next()
        .unwrap()
        .1
        .normalized
        .as_ref()
        .unwrap();
    let document = old
        .rebind(
            pending.receipt.generation,
            service.analysis_limits.ir(),
            &service.extensions,
            &Cancellation::new(),
        )
        .unwrap();
    let workspace = generation_neutral_workspace_bytes(&document, || true).unwrap();
    let fresh = Arc::new(
        compressed_chunk::CompressedNormalizedFileChunk::new(
            &document,
            service.analysis_limits.ir(),
            &service.extensions,
            workspace,
            MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES,
            &Cancellation::new(),
        )
        .unwrap(),
    );
    assert!(!Arc::ptr_eq(old, &fresh));
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        profile_built_chunk(&fresh);
        profile_discarded_chunk(old);
        profile_discarded_chunk(&fresh);
    });
    assert_eq!(profile.admitted_new_chunks, 1);
    assert_eq!(profile.admitted_new_chunks_discarded, 1);
    assert_eq!(
        profile.admitted_new_chunk_discarded_bytes,
        fresh.accounted_bytes() as u128
    );
}

#[test]
fn telemetry_reservation_loss_preserves_index_receipts() {
    let directory = fixture("pub fn profiled() -> u8 { 7 }\n");
    let mut reference = FirstSliceService::new(2).unwrap();
    let expected = reference
        .index_rust_fixture(directory.path(), &Cancellation::new())
        .unwrap();
    let root = *reference.repositories.keys().next().unwrap();
    let mut observed = FirstSliceService::new(2).unwrap();
    observed
        .restore_repository_registration(root, expected.repository)
        .unwrap();
    let (actual, profile) = capture_first_slice_analysis_profile(|| {
        with_analysis_profile(|profile| {
            profile.reject_event_reservation = true;
            profile.reject_identity_reservation = true;
        });
        observed.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_same_index_outcome(actual.unwrap(), &expected);
    assert!(!profile.events_complete);
    assert!(!profile.counts_complete);
    assert!(profile.dropped_events > 0);
    assert_eq!(profile.admitted_new_chunks, 1);
    assert_eq!(profile.admitted_new_chunks_discarded, 0);
    assert!(profile.first_irreversible_collector_overflow.is_none());
    let (no_op, no_op_profile) = capture_first_slice_analysis_profile(|| {
        observed.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    assert_same_index_outcome(no_op.unwrap(), &expected);
    assert!(no_op_profile.phases.is_empty());
    assert_eq!(no_op_profile.admitted_new_chunks, 0);
}

#[test]
fn closed_parser_collector_skips_optional_chunks_and_preserves_exact_facts() {
    let source = "pub fn retained() -> u8 { 7 }\n";
    let directory = fixture(source);
    let mut reference = FirstSliceService::new(2).unwrap();
    let expected = reference
        .index_rust_fixture(directory.path(), &Cancellation::new())
        .unwrap();
    let root = *reference.repositories.keys().next().unwrap();
    let mut bounded = FirstSliceService::new(2).unwrap();
    bounded
        .restore_repository_registration(root, expected.repository)
        .unwrap();
    bounded.structural_artifacts.maximum_bytes = 1;
    let (actual, profile) = capture_first_slice_analysis_profile(|| {
        bounded.index_rust_fixture(directory.path(), &Cancellation::new())
    });
    let actual = actual.unwrap();
    assert_same_index_outcome(actual.clone(), &expected);
    let expected_snapshot = reference
        .loaded_generation_snapshot(expected.generation)
        .unwrap();
    let actual_snapshot = bounded
        .loaded_generation_snapshot(actual.generation)
        .unwrap();
    assert_eq!(actual_snapshot.document(), expected_snapshot.document());
    bounded
        .inspect_generation(
            actual.repository,
            Some(actual.generation),
            &Cancellation::new(),
            |snapshot| {
                let record = snapshot
                    .document()
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "retained")
                    .unwrap();
                let source_ref = record.evidence.source.clone().unwrap();
                let result = bounded
                    .source_read_with_options_and_budget(
                        actual.generation,
                        vec![source_ref],
                        SourceReadOptions::new()
                            .with_context_lines_before(0)
                            .with_context_lines_after(0)
                            .with_encoding(SourceEncoding::Bytes),
                        FirstSliceBudget::default(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert_eq!(result.data.chunks.len(), 1);
                assert_eq!(
                    result.data.chunks[0].bytes,
                    b"pub fn retained() -> u8 { 7 }"
                );
            },
        )
        .unwrap();
    assert!(profile.first_irreversible_collector_overflow.is_some());
    assert!(profile.events_complete && profile.counts_complete);
    assert_eq!(profile.admitted_new_chunks, 0);
    assert_eq!(profile.admitted_new_chunks_discarded, 0);
    assert!(!profile.phases.iter().any(|phase| matches!(
        phase.phase,
        AnalysisProfilePhase::OptionalWorkspace
            | AnalysisProfilePhase::OptionalChunkConstructionAndAdmission
    )));
}

#[test]
fn new_chunk_budget_matches_replacement_and_incoming_eviction_priority() {
    let first = entry("pub fn first_cached() {}\n");
    let mut second = entry("pub fn second_cached() {}\n");
    second.normalized = None;
    let first_parser = Arc::clone(&first.artifact);
    let second_parser = Arc::clone(&second.artifact);
    let chunk_bytes = first.normalized.as_ref().unwrap().accounted_bytes();
    let budget = first.accounted_bytes().unwrap() + second.accounted_bytes().unwrap();
    let mut collector = StructuralArtifactCollector::new(budget);
    collector.push(first, &Cancellation::new()).unwrap();
    collector.push(second, &Cancellation::new()).unwrap();
    assert_eq!(collector.accounted_bytes, budget);
    assert_eq!(
        collector
            .new_normalized_chunk_budget(&first_parser)
            .unwrap(),
        Some(chunk_bytes),
    );
    assert_eq!(
        collector
            .new_normalized_chunk_budget(&second_parser)
            .unwrap(),
        None,
        "an incoming optional chunk cannot displace an older retained one",
    );
    let kept = collector.get(first_parser.file()).unwrap();
    assert!(kept.normalized.is_some());
    assert!(collector.complete);
    collector.maximum_bytes -= 1;
    assert_eq!(
        collector
            .new_normalized_chunk_budget(&first_parser)
            .unwrap(),
        Some(chunk_bytes - 1),
    );
}

#[test]
fn new_chunk_budget_rejects_closed_cursor_and_parser_overflow() {
    let mut retained = entry("pub fn parser_only() {}\n");
    retained.normalized = None;
    let artifact = Arc::clone(&retained.artifact);
    let parser_bytes = retained.accounted_bytes().unwrap();
    let mut collector = StructuralArtifactCollector::new(parser_bytes);
    collector.push(retained, &Cancellation::new()).unwrap();
    assert!(collector.complete);
    assert!(
        collector
            .new_normalized_chunk_budget(&artifact)
            .unwrap()
            .is_none()
    );
    collector.maximum_bytes = parser_bytes - 1;
    assert!(
        collector
            .new_normalized_chunk_budget(&artifact)
            .unwrap()
            .is_none()
    );
    collector.maximum_bytes = MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES;
    assert!(
        collector
            .new_normalized_chunk_budget(&artifact)
            .unwrap()
            .is_some()
    );
    collector.next_normalized = 1;
    assert!(
        collector
            .new_normalized_chunk_budget(&artifact)
            .unwrap()
            .is_none()
    );
    collector.next_normalized = 0;
    collector.complete = false;
    assert!(
        collector
            .new_normalized_chunk_budget(&artifact)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cancelled_staging_then_retry_preserves_error_generation_and_source() {
    let directory = fixture("pub fn retried() -> u8 { 9 }\n");
    let mut service = FirstSliceService::new(2).unwrap();
    let canonical = canonical_repository_root(directory.path(), &Cancellation::new()).unwrap();
    service
        .restore_repository_registration(
            repository_path_hash(&canonical).unwrap(),
            RepositoryId::from_bytes([37; 16]),
        )
        .unwrap();
    let (receipt, profile) = capture_first_slice_analysis_profile(|| {
        let prepared = service
            .prepare_rust_fixture(directory.path(), &Cancellation::new())
            .unwrap();
        let expected = prepared.receipt();
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            service.stage_prepared(prepared, &cancelled),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        assert!(service.receipts.is_empty());
        let actual = service
            .index_rust_fixture(directory.path(), &Cancellation::new())
            .unwrap();
        assert_same_index_outcome(actual.clone(), &expected);
        actual
    });
    assert_eq!(profile.admitted_new_chunks, 2);
    assert_eq!(profile.expired_tracked_new_chunks, 1);
    assert_eq!(profile.admitted_new_chunks_discarded, 0);
    assert_eq!(profile.tracked_new_chunks_with_strong_owners_at_finish, 1);
    service
        .inspect_generation(
            receipt.repository,
            Some(receipt.generation),
            &Cancellation::new(),
            |snapshot| {
                let source = snapshot
                    .document()
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "retried")
                    .unwrap()
                    .evidence
                    .source
                    .clone()
                    .unwrap();
                let result = service
                    .source_read_with_options_and_budget(
                        receipt.generation,
                        vec![source],
                        SourceReadOptions::new()
                            .with_context_lines_before(0)
                            .with_context_lines_after(0)
                            .with_encoding(SourceEncoding::Bytes),
                        FirstSliceBudget::default(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert_eq!(result.data.chunks.len(), 1);
                assert_eq!(result.data.chunks[0].bytes, b"pub fn retried() -> u8 { 9 }");
            },
        )
        .unwrap();
}

#[test]
fn profile_serialization_and_debug_exclude_source_and_private_identities() {
    let marker = "distinctive_private_path_and_source_marker";
    let chunk = chunk(&format!("pub fn {marker}() {{}}\n"));
    let ((), profile) = capture_first_slice_analysis_profile(|| {
        profile_built_chunk(&chunk);
        profile_analysis_phase!(Preflight, None, ());
    });
    let json = serde_json::to_string(&profile).unwrap();
    let debug = format!("{profile:?}");
    for value in [json, debug] {
        assert!(!value.contains(marker));
        assert!(!value.contains("pending_built_chunks"));
        assert!(!value.contains("Weak"));
    }
    assert!(profile.native_parse_call_count.is_none());
    assert!(profile.pending_built_chunks.is_empty());
}
