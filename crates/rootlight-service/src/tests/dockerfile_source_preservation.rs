//! Genuine scanner refusal preserves exact source without publishing native facts.
//! These controls use the unchanged scanner format and default service policy.

use super::*;
use rootlight_runtime::RuntimePaths;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

fn capacity_source() -> Vec<u8> {
    let marker = "x".repeat(1014);
    format!("FROM scratch AS written\n# ababa\nRUN <<{marker}\nbody\n{marker}\n").into_bytes()
}

fn assert_preserved(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    bytes: &[u8],
    diagnostic_code: &str,
    reason: SkippedRegionReason,
) {
    let cancellation = Cancellation::new();
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Dockerfile")
        .unwrap();
    assert_eq!(file.language, "dockerfile");
    assert_eq!(file.encoding, "utf-8");
    let source = file.evidence.source.clone().unwrap();
    assert_eq!(source.content_hash(), content_hash(bytes));
    assert_eq!(
        source.span().end_byte(),
        u64::try_from(bytes.len()).unwrap()
    );
    assert!(!document.entities.iter().any(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file.id)
    }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.source.span().file() == file.id)
    );
    assert!(!document.relations.iter().any(|relation| {
        relation
            .evidence
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file.id)
    }));
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == source
                    && gap.domain == domain
                    && gap.reason == reason
                    && gap.detail == diagnostic_code)
        );
    }
    let diagnostic = document
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.source.as_ref() == Some(&source) && diagnostic.code == diagnostic_code
        })
        .unwrap();
    assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
    assert!(diagnostic.message.contains("observed unknown"));
    assert!(!diagnostic.message.contains("Records"));
    assert!(!diagnostic.message.contains("1025"));
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &cancellation)
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    let response = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(response.data.chunks.len(), 1);
    assert_eq!(response.data.chunks[0].reference, source);
    assert_eq!(response.data.chunks[0].bytes, bytes);
    let page = service
        .find_literal(
            receipt.generation,
            file.id,
            "ababa",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    let expected = bytes
        .windows(5)
        .position(|window| window == b"ababa")
        .unwrap();
    assert_eq!(page.stop, SourceFindStop::EndOfSource);
    assert!(page.continuation.is_none());
    assert_eq!(page.matches.len(), 1);
    assert_eq!(
        page.matches[0].span().start_byte(),
        u64::try_from(expected).unwrap()
    );
    assert_eq!(
        page.matches[0].span().end_byte(),
        u64::try_from(expected + 5).unwrap()
    );
}

fn write_sources(root: &Path, bytes: &[u8]) {
    fs::write(root.join("Dockerfile"), bytes).unwrap();
    fs::write(root.join("lib.rs"), b"pub fn retained_sibling() {}\n").unwrap();
}

#[test]
fn dockerfile_scanner_capacity_preserves_source_sibling_reopen_and_repair() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let storage = durable_test_tempdir();
    let bytes = capacity_source();
    write_sources(root.path(), &bytes);
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    let first = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_preserved(
        &service,
        &first,
        &bytes,
        "native-scanner-capacity",
        SkippedRegionReason::ResourceLimit,
    );
    assert_eq!(
        service
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        first
    );
    let mut changed = bytes.clone();
    changed.extend_from_slice(b"# changed\n");
    fs::write(root.path().join("Dockerfile"), &changed).unwrap();
    let second = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(second.parent, Some(first.generation));
    assert_preserved(
        &service,
        &second,
        &changed,
        "native-scanner-capacity",
        SkippedRegionReason::ResourceLimit,
    );
    drop(service);
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    assert_preserved(
        &service,
        &first,
        &bytes,
        "native-scanner-capacity",
        SkippedRegionReason::ResourceLimit,
    );
    assert_preserved(
        &service,
        &second,
        &changed,
        "native-scanner-capacity",
        SkippedRegionReason::ResourceLimit,
    );
    let repaired = b"FROM scratch AS restored\n";
    fs::write(root.path().join("Dockerfile"), repaired).unwrap();
    let third = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(third.parent, Some(second.generation));
    let lease = service
        .generation_lease(third.generation, &cancellation)
        .unwrap();
    let document = lease.generation().document();
    assert!(document.entities.iter().any(
        |entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "restored"
    ));
    assert!(
        !document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.starts_with("native-scanner-"))
    );
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn dockerfile_unsupported_marker_is_a_construct_gap_not_a_fabricated_capacity() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = b"FROM scratch AS written\n# ababa\nRUN <<\"a\\b\"\nbody\na\\b\n";
    write_sources(root.path(), bytes);
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(root.path(), &Cancellation::new())
        .unwrap();
    assert_preserved(
        &service,
        &receipt,
        bytes,
        "native-scanner-unsupported-marker",
        SkippedRegionReason::UnsupportedConstruct,
    );
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    let diagnostic = lease
        .generation()
        .document()
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "native-scanner-unsupported-marker")
        .unwrap();
    assert!(!diagnostic.message.contains("limit 1024"));
}

#[test]
fn dockerfile_caller_policy_and_deep_mode_keep_scanner_refusal_strict() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write_sources(root.path(), &capacity_source());
    for mode in [FirstSliceIndexMode::Structural, FirstSliceIndexMode::Deep] {
        let mut service = FirstSliceService::new(2).unwrap();
        if mode == FirstSliceIndexMode::Structural {
            let current = &service.analysis_limits;
            let mut ir = current.ir().clone();
            ir.max_entities = 1;
            service.analysis_limits = AnalysisLimits::new(
                current.max_source_bytes(),
                current.max_syntax_nodes(),
                current.max_syntax_depth(),
                current.max_embedded_ranges(),
                current.max_reported_memory_bytes(),
                current.syntax_stream().clone(),
                current.ir_stream().clone(),
                ir,
            )
            .unwrap();
        }
        let error = service
            .index_repository_with_mode(root.path(), mode, &Cancellation::new())
            .unwrap_err();
        assert!(matches!(error, FirstSliceError::Adapter), "{error:?}");
        assert!(service.active_generation().is_none());
        assert_eq!(
            service
                .generation_cache
                .lock()
                .unwrap()
                .reserved_memory_bytes()
                .unwrap(),
            0
        );
    }
}

#[test]
fn dockerfile_native_certificate_binds_source_context_and_keeps_strict_failures() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = capacity_source();
    write_sources(root.path(), &bytes);
    let repository = derive_repository(b"dockerfile-scanner-certificate").id();
    let generation = GenerationId::from_bytes([81; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("Dockerfile")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "dockerfile",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let provider = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    let proof = provider
        .certify_native_scanner_refusal(
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
    assert!(proof.was_issued_by(&provider));
    assert!(proof.matches_request(&request));
    assert!(proof.matches_error(&proof.adapter_error()));
    assert_eq!(
        proof.details(),
        rootlight_adapter_treesitter::NativeScannerRefusalDetails::DockerfileStateCapacity
    );
    assert_eq!(provider.stats().pooled_parsers, 0);
    assert_eq!(provider.stats().checked_out_parsers, 0);
    assert_eq!(provider.stats().cache.entries, 0);
    let other = structural_analysis_request(
        repository,
        GenerationId::from_bytes([82; 20]),
        &snapshot,
        false,
        "dockerfile",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(!proof.matches_request(&other));
    assert!(!proof.matches_source(proof.source(), "bash", "utf-8"));
    assert!(!proof.matches_source(proof.source(), "dockerfile", "utf-16le"));
    let foreign = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    assert!(!proof.was_issued_by(&foreign));
    assert!(matches!(
        provider.certify_native_scanner_refusal(
            &request,
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &Cancellation::new()
        ),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::MemoryEnforcementUnavailable
        ))
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        provider.certify_native_scanner_refusal(
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let unsupported = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "dockerfile",
        "bytes",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(matches!(
        provider.certify_native_scanner_refusal(
            &unsupported,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new()
        ),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::UnsupportedEncoding
        ))
    ));
    fs::write(root.path().join("Dockerfile"), b"FROM scratch AS healthy\n").unwrap();
    let healthy = repository_root
        .snapshot(&RelativePath::parse(Path::new("Dockerfile")).unwrap(), 4096)
        .unwrap();
    let healthy = structural_analysis_request(
        repository,
        generation,
        &healthy,
        false,
        "dockerfile",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(
        provider
            .certify_native_scanner_refusal(
                &healthy,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(provider.stats().pooled_parsers, 1);
    assert_eq!(provider.stats().available_parsers, 1);
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn dockerfile_scanner_certificate_preserves_original_utf16_source_identity() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let written = String::from_utf8(capacity_source()).unwrap();
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(written.encode_utf16().flat_map(u16::to_le_bytes));
    fs::write(root.path().join("Dockerfile"), &bytes).unwrap();
    let repository = derive_repository(b"dockerfile-encoded-scanner-certificate").id();
    let generation = GenerationId::from_bytes([83; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("Dockerfile")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "dockerfile",
        "utf-16le",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let proof = service.structural_parsers["dockerfile"]
        .certify_native_scanner_refusal(&request, &Cancellation::new())
        .unwrap()
        .unwrap();
    assert!(service.structural_parsers["dockerfile"].owns_native_scanner_refusal(&proof));
    assert!(proof.matches_request(&request));
    assert_eq!(proof.source().content_hash(), content_hash(&bytes));
    assert_eq!(
        proof.source().span().end_byte(),
        u64::try_from(bytes.len()).unwrap()
    );
    assert!(!proof.matches_source(proof.source(), "dockerfile", "utf-8"));
}

struct UncertifiedScannerParser {
    capabilities: rootlight_adapter_sdk::ParseCapabilities,
    foreign: Option<rootlight_adapter_treesitter::NativeScannerRefusal>,
}

impl ParseProvider for UncertifiedScannerParser {
    fn capabilities(&self) -> &rootlight_adapter_sdk::ParseCapabilities {
        &self.capabilities
    }
    fn parse(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _sink: &mut dyn rootlight_adapter_sdk::SyntaxFactSink,
        _cancellation: &Cancellation,
    ) -> Result<rootlight_adapter_sdk::ParseReport, AdapterError> {
        Err(AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("dockerfile-scanner-capacity")
                .unwrap(),
        })
    }
}

impl StructuralPreflight for UncertifiedScannerParser {
    fn required_syntax_fact_count(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Err(AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("dockerfile-scanner-capacity")
                .unwrap(),
        })
    }
    fn certify_native_scanner_refusal(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _cancellation: &Cancellation,
    ) -> Result<Option<rootlight_adapter_treesitter::NativeScannerRefusal>, AdapterError> {
        Ok(self.foreign.clone())
    }
}

#[test]
fn dockerfile_copied_scanner_code_and_foreign_proof_do_not_grant_fallback() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = capacity_source();
    write_sources(root.path(), &bytes);
    let repository = derive_repository(b"uncertified-scanner").id();
    let generation = GenerationId::from_bytes([84; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("Dockerfile")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "dockerfile",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let proof = service.structural_parsers["dockerfile"]
        .certify_native_scanner_refusal(&request, &Cancellation::new())
        .unwrap()
        .unwrap();
    for foreign in [None, Some(proof)] {
        let parser = Arc::new(UncertifiedScannerParser {
            capabilities: service.structural_parsers["dockerfile"]
                .capabilities()
                .clone(),
            foreign,
        });
        let analyzer = TreeSitterAnalyzer::new(
            Arc::clone(&parser) as Arc<dyn ParseProvider>,
            ProducerIdentity::new("uncertified-scanner", "1.0", first_slice_build_context())
                .unwrap(),
            LanguageId::new("dockerfile").unwrap(),
            "fixture-native",
            content_hash(b"uncertified-scanner"),
        )
        .unwrap();
        let original = AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("dockerfile-scanner-capacity")
                .unwrap(),
        };
        let result = crate::native_source_admission::certify(
            &service.analysis_limits,
            FirstSliceIndexMode::Structural,
            &analyzer,
            parser.as_ref(),
            &request,
            &original,
            &Cancellation::new(),
        );
        if parser.foreign.is_none() {
            assert!(result.unwrap().is_none());
        } else {
            assert!(
                matches!(result, Err(AdapterError::ProviderFailed { code }) if code.as_str() == "native-scanner-certificate-mismatch")
            );
        }
        assert!(
            crate::native_source_admission::certify(
                &service.analysis_limits,
                FirstSliceIndexMode::Structural,
                &analyzer,
                parser.as_ref(),
                &request,
                &AdapterError::Sink(SinkError::AllocationFailed),
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
        assert!(
            crate::native_source_admission::certify(
                &service.analysis_limits,
                FirstSliceIndexMode::Structural,
                &service.analyzers["dockerfile"],
                parser.as_ref(),
                &request,
                &original,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
    }
}
