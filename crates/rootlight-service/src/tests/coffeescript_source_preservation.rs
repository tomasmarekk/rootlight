//! Default intrinsic scanner refusal retains source without publishing partial syntax.
//! Caller quotas and Deep mode remain strict; diagnostics retain native units.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

fn work_source() -> Vec<u8> {
    // An ordinary implicit-call argument drives the external identifier loop.
    // Its spelling is source data, not a guessed measurement of native work.
    let mut bytes = b"# ababa\nprint ".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 2_000_100));
    bytes.push(b'\n');
    bytes
}

fn write_sources(root: &Path, bytes: &[u8]) {
    fs::write(root.join("written.coffee"), bytes).unwrap();
    fs::write(root.join("lib.rs"), b"pub fn retained_sibling() {}\n").unwrap();
}

#[test]
fn coffeescript_full_original_and_both_literal_contracts_keep_exact_source() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/coffeescript/line.coffee"
    );
    assert_eq!(bytes.len(), 467);
    write_sources(root.path(), bytes);
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let file = lease
        .generation()
        .document()
        .files
        .iter()
        .find(|file| file.path == "written.coffee")
        .unwrap();
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(bytes));
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &cancellation)
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, *source);
    assert_eq!(read.data.chunks[0].bytes, bytes);
    for (query, count) in [
        ("_declareDefaults", 1),
        ("_declareDefaults__absent_source_control__", 0),
    ] {
        let page = service
            .find_literal(
                receipt.generation,
                file.id,
                query,
                None,
                SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(page.stop, SourceFindStop::EndOfSource);
        assert!(page.continuation.is_none());
        assert_eq!(page.matches.len(), count);
        if count == 1 {
            assert_eq!(page.matches[0].span().start_byte(), 89);
            assert_eq!(page.matches[0].span().end_byte(), 105);
            let selected = SourceRef::new(
                source.repository(),
                source.generation(),
                page.matches[0].span(),
                source.content_hash(),
                None,
            );
            let exact = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![selected.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &cancellation,
                )
                .unwrap();
            assert_eq!(exact.data.chunks[0].reference, selected);
            assert_eq!(exact.data.chunks[0].bytes, query.as_bytes());
        }
    }
}

#[test]
fn coffeescript_default_external_work_preserves_complete_source_and_sibling() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = work_source();
    write_sources(root.path(), &bytes);
    let mut service = FirstSliceService::new(2).unwrap();
    let cancellation = Cancellation::new();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "written.coffee")
        .unwrap();
    assert_eq!(file.language, "coffeescript");
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
                .any(|gap| gap.source == *source
                    && gap.domain == domain
                    && gap.reason == SkippedRegionReason::ResourceLimit
                    && gap.detail == "native-scanner-external-work-limit")
        );
    }
    let diagnostic = document
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.source.as_ref() == Some(source)
                && diagnostic.code == "native-scanner-external-work-limit"
        })
        .unwrap();
    assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
    assert!(
        diagnostic
            .message
            .contains("unit accepted-external-scanner-logical-steps")
    );
    assert!(diagnostic.message.contains("limit 2000000; consumed 2000000; demanded unknown; remaining-demand unknown; next charge declined"));
    assert!(!diagnostic.message.contains("2000001"));
    assert!(!diagnostic.message.contains("Records"));
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
    assert_eq!(
        service
            .source_file_coverage_until(receipt.generation, file.id, &cancellation)
            .unwrap()
            .status,
        CoverageStatus::Unknown
    );
    let mut restored = Vec::new();
    for (index, expected) in bytes.chunks(64 * 1024).enumerate() {
        let start = u64::try_from(index * 64 * 1024).unwrap();
        let end = start + u64::try_from(expected.len()).unwrap();
        let selected = SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(file.id, start, end).unwrap(),
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
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, selected);
        assert_eq!(read.data.chunks[0].bytes, expected);
        restored.extend_from_slice(&read.data.chunks[0].bytes);
    }
    assert_eq!(restored, bytes);
    let literal = service
        .find_literal(
            receipt.generation,
            file.id,
            "ababa",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(literal.stop, SourceFindStop::EndOfSource);
    assert!(literal.continuation.is_none());
    assert_eq!(literal.matches.len(), 1);
    assert_eq!(literal.matches[0].span().start_byte(), 2);
    assert_eq!(literal.matches[0].span().end_byte(), 7);
}

#[test]
fn coffeescript_caller_limits_and_deep_mode_do_not_accept_scanner_fallback() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write_sources(root.path(), &work_source());
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
fn coffeescript_work_certificate_binds_actual_context_and_cancellation_stays_strict() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = work_source();
    write_sources(root.path(), &bytes);
    let repository = derive_repository(b"coffeescript-scanner-certificate").id();
    let generation = GenerationId::from_bytes([84; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("written.coffee")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "coffeescript",
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
        rootlight_adapter_treesitter::NativeScannerRefusalDetails::CoffeescriptExternalWork {
            consumed: 2_000_000,
            limit: 2_000_000
        }
    );
    assert!(proof.was_issued_by(&provider));
    assert!(proof.matches_request(&request));
    assert!(!proof.matches_source(proof.source(), "dockerfile", "utf-8"));
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
}
