//! Complete source remains readable after an intrinsic native scanner refusal.
//! The fallback cannot promote a partial tree or waive caller, mode, or NUL errors.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

fn capacity_source() -> Vec<u8> {
    // This actual native boundary exceeds the scanner's 16-bit indentation width.
    let mut bytes = b"div\n".to_vec();
    bytes.extend(std::iter::repeat_n(b' ', 65_536));
    bytes.extend_from_slice(b"input(type='text', disabled)\n");
    bytes
}

fn write_sources(root: &Path, bytes: &[u8]) {
    fs::write(root.join("written.pug"), bytes).unwrap();
    fs::write(root.join("lib.rs"), b"pub fn retained_sibling() {}\n").unwrap();
}

#[test]
fn pug_full_original_and_exhaustive_literal_search_keep_exact_source() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/pug/hash.pug");
    assert_eq!(bytes.len(), 718);
    write_sources(root.path(), bytes);
    let cancel = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &cancel).unwrap();
    let lease = service
        .generation_lease(receipt.generation, &cancel)
        .unwrap();
    let file = lease
        .generation()
        .document()
        .files
        .iter()
        .find(|file| file.path == "written.pug")
        .unwrap();
    assert_eq!(file.language, "pug");
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(bytes));
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &cancel)
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, *source);
    assert_eq!(read.data.chunks[0].bytes, bytes);
    let document = lease.generation().document();
    assert_eq!(source.repository(), receipt.repository);
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(source.span(), SourceSpan::new(file.id, 0, 718).unwrap());
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let gaps = document.skipped_regions.iter().filter(|gap|
            gap.domain == domain && gap.source == *source
                && gap.detail == "pug-explicit-static-tags-and-attributes-only-template-execution-embedded-binding-and-rendered-dom-unavailable")
            .collect::<Vec<_>>();
        assert_eq!(
            gaps.len(),
            1,
            "one whole-source semantic limitation in {domain:?}"
        );
        assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gaps[0].repository, receipt.repository);
        assert_eq!(gaps[0].generation, receipt.generation);
        assert_eq!(gaps[0].provenance, file.provenance);
        assert_eq!(gaps[0].evidence.source.as_ref(), Some(source));
        let coverage = document
            .coverage_records
            .iter()
            .filter(|coverage| {
                coverage.scope == CoverageScope::File(file.id)
                    && coverage.domain == domain
                    && coverage.provenance == file.provenance
            })
            .collect::<Vec<_>>();
        assert_eq!(
            coverage.len(),
            1,
            "one normalized file-domain record in {domain:?}"
        );
        let coverage = coverage[0];
        assert_eq!(coverage.status, CoverageStatus::Bounded);
        assert_eq!(coverage.tier, rootlight_ir::AnalysisTier::TierD);
        assert_eq!(coverage.repository, receipt.repository);
        assert_eq!(coverage.generation, receipt.generation);
        assert_eq!(coverage.evidence.source.as_ref(), Some(source));
        let skipped = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.source.span().file() == file.id
                    && gap.provenance == file.provenance
            })
            .count();
        assert_eq!(coverage.skipped, u64::try_from(skipped).unwrap());
        assert!(coverage.skipped > 0);
        assert!(coverage.indexed > 0);
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &cancel)
        .unwrap();
    assert_eq!(coverage.language, "pug");
    assert_eq!(coverage.tier, rootlight_ir::AnalysisTier::TierD);
    assert_eq!(coverage.status, CoverageStatus::Bounded);
    assert_eq!(coverage.reason, None);
    let definition = |name: &str, kind, start, end| {
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().file() == file.id
                    && site.role == rootlight_ir::OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "one exact written {name}");
        assert_eq!(sites[0].source.content_hash(), content_hash(bytes));
        assert_eq!(sites[0].provenance, file.provenance);
        let rootlight_ir::OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("a written definition must resolve to its declaration");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.provenance, file.provenance);
        symbol
    };
    let input = definition("input", EntityKind::MarkupElement, 410, 415);
    let attribute_type = definition("type", EntityKind::MarkupAttribute, 434, 438);
    let disabled = definition("disabled", EntityKind::MarkupAttribute, 447, 455);
    for attribute in [attribute_type, disabled] {
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == rootlight_ir::RelationPredicate::Contains
                    && edge.subject == rootlight_ir::RelationEndpoint::Entity(input)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(attribute)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].provenance, file.provenance);
        assert_eq!(
            edges[0].evidence.source.as_ref().unwrap().content_hash(),
            content_hash(bytes)
        );
        assert!(!document.relations.iter().any(|edge| edge.predicate
            == rootlight_ir::RelationPredicate::Contains
            && edge.subject == rootlight_ir::RelationEndpoint::Entity(attribute)
            && edge.object == rootlight_ir::RelationEndpoint::Entity(input)));
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id
                && site.source.span().start_byte() == 440
                && site.source.span().end_byte() == 444
                && site.role == rootlight_ir::OccurrenceRole::Definition)
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.evidence
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file.id)
            && matches!(
                edge.predicate,
                rootlight_ir::RelationPredicate::Calls
                    | rootlight_ir::RelationPredicate::DispatchCandidate
            )
    }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id
                && matches!(
                    site.role,
                    rootlight_ir::OccurrenceRole::CallSite
                        | rootlight_ir::OccurrenceRole::Read
                        | rootlight_ir::OccurrenceRole::Write
                ))
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
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
        if count == 1 {
            assert_eq!(page.matches[0].span().start_byte(), 447);
            assert_eq!(page.matches[0].span().end_byte(), 455);
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
                    &cancel,
                )
                .unwrap();
            assert_eq!(exact.data.chunks[0].reference, selected);
            assert_eq!(exact.data.chunks[0].bytes, b"disabled");
        }
    }
}

#[test]
fn pug_recovered_native_context_keeps_source_and_unknown_domains_without_names() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = b"input(type=\n";
    write_sources(root.path(), bytes);
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
        .find(|file| file.path == "written.pug")
        .unwrap();
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(bytes));
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &cancel)
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, bytes);
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
                .any(|gap| gap.domain == domain
                    && gap.source == *source
                    && gap.detail == "pug-complete-clean-native-source-context-unavailable")
        );
    }
    assert_eq!(
        service
            .source_file_coverage_until(receipt.generation, file.id, &cancel)
            .unwrap()
            .status,
        CoverageStatus::Unknown
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
}

#[test]
fn pug_default_native_capacity_preserves_all_bytes_without_partial_facts() {
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
        .find(|file| file.path == "written.pug")
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
    assert!(!diagnostic.message.contains("consumed 65536"));
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
fn pug_caller_quotas_and_deep_mode_cannot_use_intrinsic_source_fallback() {
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
fn pug_capacity_certificate_binds_native_request_and_cancellation_stays_strict() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = capacity_source();
    write_sources(root.path(), &bytes);
    let repository = derive_repository(b"pug-scanner-certificate").id();
    let generation = GenerationId::from_bytes([85; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("written.pug")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "pug",
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
        rootlight_adapter_treesitter::NativeScannerRefusalDetails::PugStateCapacity
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
        .snapshot(&RelativePath::parse(Path::new("written.pug")).unwrap(), 1)
        .unwrap();
    let nul_request = structural_analysis_request(
        repository,
        generation,
        &nul,
        false,
        "pug",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(
        matches!(provider.certify_native_scanner_refusal(&nul_request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "pug-scanner-nul-unavailable")
    );
}

#[test]
fn pug_structural_admission_cannot_reuse_the_previous_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/298",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}
