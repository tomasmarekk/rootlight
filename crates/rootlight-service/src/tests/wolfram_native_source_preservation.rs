//! A genuine scanner depth refusal preserves source without admitting partial IR.
//! Caller quotas, Deep mode, cancellation, hard-memory policy and NUL stay strict.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

fn capacity_source() -> Vec<u8> {
    let mut bytes = "(*".repeat(1025).into_bytes();
    bytes.extend_from_slice("*)".repeat(1025).as_bytes());
    bytes.extend_from_slice(b"\n\"disabled\"\n");
    bytes
}

fn write_sources(root: &Path, bytes: &[u8]) {
    fs::write(root.join("written.wl"), bytes).unwrap();
    fs::write(root.join("lib.rs"), b"pub fn retained_sibling() {}\n").unwrap();
}

#[test]
fn wolfram_default_native_capacity_preserves_all_bytes_without_partial_facts() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = capacity_source();
    write_sources(root.path(), &bytes);
    let mut service = FirstSliceService::new(2).unwrap();
    let cancel = Cancellation::new();
    let receipt = service.index_repository(root.path(), &cancel).unwrap();
    let lease = service
        .generation_lease(receipt.generation, &cancel)
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "written.wl")
        .unwrap();
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(&bytes));
    assert_eq!(source.span().start_byte(), 0);
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
            .any(|site| site.source.span().file() == file.id)
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.evidence
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
                .any(|gap| gap.source == *source
                    && gap.domain == domain
                    && gap.reason == SkippedRegionReason::ResourceLimit
                    && gap.detail == "native-scanner-capacity")
        );
    }
    let diagnostic = document
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.source.as_ref() == Some(source)
                && diagnostic.code == "native-scanner-capacity"
        })
        .unwrap();
    assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
    assert!(diagnostic.message.contains("limit 1024; observed unknown"));
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
    assert_eq!(
        service
            .source_file_coverage_until(receipt.generation, file.id, &cancel)
            .unwrap()
            .status,
        CoverageStatus::Unknown
    );
    let mut restored = Vec::new();
    for (index, expected) in bytes.chunks(64 * 1024).enumerate() {
        let start = u64::try_from(index * 64 * 1024).unwrap();
        let selected = SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(
                file.id,
                start,
                start + u64::try_from(expected.len()).unwrap(),
            )
            .unwrap(),
            source.content_hash(),
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![selected.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancel,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, selected);
        assert_eq!(read.data.chunks[0].bytes, expected);
        restored.extend_from_slice(&read.data.chunks[0].bytes);
    }
    assert_eq!(restored, bytes);
    for (query, count) in [("disabled", 1), ("disabled__absent_source_control__", 0)] {
        let page = service
            .find_literal(
                receipt.generation,
                file.id,
                query,
                None,
                SourceFindBudget::new(),
                &cancel,
            )
            .unwrap();
        assert_eq!(page.stop, SourceFindStop::EndOfSource);
        assert!(page.continuation.is_none());
        assert_eq!(page.matches.len(), count);
    }
}

#[test]
fn wolfram_caller_quotas_and_deep_mode_cannot_use_intrinsic_source_fallback() {
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
        assert!(matches!(
            service.index_repository_with_mode(root.path(), mode, &Cancellation::new()),
            Err(FirstSliceError::Adapter)
        ));
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
fn wolfram_capacity_certificate_binds_native_request_and_cancellation_stays_strict() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = capacity_source();
    write_sources(root.path(), &bytes);
    let repository = derive_repository(b"wolfram-scanner-certificate").id();
    let generation = GenerationId::from_bytes([85; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("written.wl")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "wolfram",
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
    assert_eq!(
        proof.details(),
        rootlight_adapter_treesitter::NativeScannerRefusalDetails::WolframCommentDepth
    );
    assert!(proof.was_issued_by(&provider));
    assert!(proof.matches_request(&request));
    assert!(!proof.matches_source(proof.source(), "coffeescript", "utf-8"));
    let foreign = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    assert!(!proof.was_issued_by(&foreign));
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        provider.certify_native_scanner_refusal(
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
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
    // Discovery classifies NUL-bearing bytes before parsing. This explicit SDK
    // request tests the native refusal without claiming discovery admitted it.
    write_sources(root.path(), b"\0");
    let nul = repository_root
        .snapshot(&RelativePath::parse(Path::new("written.wl")).unwrap(), 1)
        .unwrap();
    let nul_request = structural_analysis_request(
        repository,
        generation,
        &nul,
        false,
        "wolfram",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(
        matches!(provider.certify_native_scanner_refusal(&nul_request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "wolfram-scanner-nul-unavailable")
    );
}
