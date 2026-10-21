//! Genuine intrinsic query exhaustion must retain exact captured source owners.
//! These controls use the unchanged native hard ceiling, not a lowered caller
//! quota; they intentionally make no full-language or old-state repair claim.

use super::*;
use rootlight_runtime::RuntimePaths;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

fn many_scalars() -> Vec<u8> {
    // Native parent lookup scans preceding siblings. Bounded fanout exercises
    // the unchanged million-match ceiling without quadratic fixture-only work.
    let mut branch = vec![b'0'];
    for _ in 0..3 {
        let mut array = vec![b'['];
        for index in 0..32 {
            if index != 0 {
                array.push(b',');
            }
            array.extend_from_slice(&branch);
        }
        array.push(b']');
        branch = array;
    }
    let mut bytes = b"[\"ababa\"".to_vec();
    for _ in 0..32 {
        bytes.push(b',');
        bytes.extend_from_slice(&branch);
    }
    bytes.extend_from_slice(b"]\n");
    bytes
}

fn source_reference(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
) -> SourceRef {
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    let file = lease
        .generation()
        .document()
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    file.evidence.source.clone().unwrap()
}

fn assert_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
) {
    let cancellation = Cancellation::new();
    let reference = source_reference(service, receipt, path);
    assert_eq!(reference.content_hash(), content_hash(bytes));
    assert_eq!(reference.span().end_byte(), bytes.len() as u64);
    let mut actual = Vec::new();
    for start in (0..bytes.len()).step_by(32 * 1024) {
        let end = bytes.len().min(start + 32 * 1024);
        let selected = SourceRef::new(
            reference.repository(),
            reference.generation(),
            SourceSpan::new(reference.span().file(), start as u64, end as u64).unwrap(),
            reference.content_hash(),
            None,
        );
        let response = service
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
        assert_eq!(response.data.chunks[0].reference, selected);
        actual.extend_from_slice(&response.data.chunks[0].bytes);
    }
    assert_eq!(actual, bytes);
    let mut cursor = None;
    let mut next = 0;
    let mut spans = Vec::new();
    loop {
        let mut budget = SourceFindBudget::new();
        budget.matcher.max_scan_bytes = if next < 8 { 4 } else { bytes.len() };
        let page = service
            .find_literal(
                receipt.generation,
                reference.span().file(),
                "aba",
                cursor,
                budget,
                &cancellation,
            )
            .unwrap();
        assert_eq!(page.scanned.start, next);
        next = page.scanned.end;
        spans.extend(
            page.matches
                .iter()
                .map(|hit| (hit.span().start_byte(), hit.span().end_byte())),
        );
        cursor = page.continuation;
        if cursor.is_none() {
            assert_eq!(page.stop, SourceFindStop::EndOfSource);
            break;
        }
    }
    assert_eq!(next, bytes.len() as u64);
    assert_eq!(spans, [(2, 5), (4, 7)]);
    let absent = service
        .find_literal(
            receipt.generation,
            reference.span().file(),
            "source-marker-not-present",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert!(absent.matches.is_empty());
    assert_eq!(absent.stop, SourceFindStop::EndOfSource);
    assert!(absent.continuation.is_none());
}

fn assert_degraded(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt, path: &str) {
    let reference = source_reference(service, receipt, path);
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.id == reference.span().file())
        .unwrap();
    assert_eq!(file.language, "json");
    assert_eq!(file.encoding, "utf-8");
    assert!(!document.entities.iter().any(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file.id)
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
            .is_some_and(|source| source.span().file() == file.id)
    }));
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let gap = document
            .skipped_regions
            .iter()
            .find(|gap| gap.source == reference && gap.domain == domain)
            .unwrap();
        assert_eq!(gap.reason, SkippedRegionReason::ResourceLimit);
        assert_eq!(gap.detail, "native-identity-work-limit");
    }
    let diagnostic = document
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == "native-identity-work-limit"
                && diagnostic.source.as_ref() == Some(&reference)
        })
        .unwrap();
    assert!(
        diagnostic
            .message
            .ends_with("resource Records observed 1048577 above 1048576")
    );
    assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &Cancellation::new())
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::ResourceBounded)
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
}

#[test]
fn intrinsic_native_work_limit_preserves_exact_source_across_publication_and_reopen() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let storage = durable_test_tempdir();
    let bytes = many_scalars();
    fs::write(root.path().join("values.json"), &bytes).unwrap();
    fs::write(
        root.path().join("lib.rs"),
        b"pub fn retained_sibling() {}\n",
    )
    .unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    let first = service
        .index_repository(root.path(), &cancellation)
        .expect("intrinsic work limit preserves captured source");
    assert_degraded(&service, &first, "values.json");
    assert_source(&service, &first, "values.json", &bytes);
    assert_eq!(
        service
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        first
    );
    let mut changed = bytes.clone();
    changed.extend_from_slice(b" ");
    fs::write(root.path().join("values.json"), &changed).unwrap();
    let second = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(second.parent, Some(first.generation));
    assert_source(&service, &second, "values.json", &changed);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
    drop(service);
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(second.generation)
    );
    assert_degraded(&service, &first, "values.json");
    assert_source(&service, &first, "values.json", &bytes);
    assert_source(&service, &second, "values.json", &changed);
    let restored = b"{\"restored_analysis\":1}\n";
    fs::write(root.path().join("values.json"), restored).unwrap();
    let third = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(third.parent, Some(second.generation));
    let lease = service
        .generation_lease(third.generation, &cancellation)
        .unwrap();
    let property = lease
        .generation()
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Property
                && entity.language == "json"
                && entity.canonical_name == r#""restored_analysis""#
        })
        .expect("the smaller source resumes native property analysis");
    assert_eq!(property.display_name, "restored_analysis");
    let evidence = property.evidence.source.as_ref().unwrap();
    assert_eq!(evidence.generation(), third.generation);
    assert_eq!(evidence.content_hash(), content_hash(restored));
    assert!(
        !lease
            .generation()
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "native-identity-work-limit")
    );
    let repository = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(first.repository.to_string());
    assert!(fs::read_dir(repository).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")
    }));
}

#[test]
fn explicit_caller_policy_and_deep_mode_do_not_acquire_native_fallback() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(root.path().join("values.json"), many_scalars()).unwrap();
    let cancellation = Cancellation::new();
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
        let failure = service
            .index_repository_with_mode(root.path(), mode, &cancellation)
            .unwrap_err();
        assert!(
            matches!(
                failure,
                FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::Records,
                    observed: 1_048_577,
                    limit: 1_048_576,
                }
            ),
            "original typed failure must survive: {failure:?}"
        );
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
fn native_certificate_is_source_bound_and_cancelled_probe_stays_failed() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = many_scalars();
    fs::write(root.path().join("values.json"), &bytes).unwrap();
    let repository = derive_repository(b"native-work-certificate-fixture").id();
    let generation = GenerationId::from_bytes([79; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("values.json")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "json",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let proof = service.structural_parsers["json"]
        .certify_intrinsic_identity_limit(&request, &Cancellation::new())
        .unwrap()
        .unwrap();
    assert!(proof.matches_request(&request));
    assert_eq!((proof.observed(), proof.limit()), (1_048_577, 1_048_576));
    assert!(proof.matches_error(&proof.adapter_error()));
    let other = structural_analysis_request(
        repository,
        GenerationId::from_bytes([80; 20]),
        &snapshot,
        false,
        "json",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(!proof.matches_request(&other));
    assert!(!proof.matches_source(proof.source(), "jsonc", "utf-8"));
    assert!(!proof.matches_source(proof.source(), "json", "utf-16le"));
    assert!(
        service.analyzers["json"].uses_parse_provider(service.structural_parsers["json"].as_ref())
    );
    let foreign = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        foreign.certify_intrinsic_identity_limit(
            &request,
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &Cancellation::new(),
        ),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::MemoryEnforcementUnavailable
        ))
    ));
    assert!(!service.analyzers["json"].uses_parse_provider(&foreign));
    assert!(!proof.was_issued_by(&foreign));
    let original = &service.analysis_limits;
    let constrained = AnalysisLimits::new(
        original.max_source_bytes(),
        1,
        original.max_syntax_depth(),
        original.max_embedded_ranges(),
        original.max_reported_memory_bytes(),
        original.syntax_stream().clone(),
        original.ir_stream().clone(),
        original.ir().clone(),
    )
    .unwrap();
    let constrained_request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "json",
        "utf-8",
        &constrained,
    )
    .unwrap()
    .to_parse_request();
    assert!(!proof.matches_request(&constrained_request));
    assert!(matches!(
        service.structural_parsers["json"]
            .certify_intrinsic_identity_limit(&constrained_request, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "syntax-node-parse-work-limit"
    ));
    let unsupported_encoding = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "json",
        "bytes",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(matches!(
        service.structural_parsers["json"]
            .certify_intrinsic_identity_limit(&unsupported_encoding, &Cancellation::new()),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::UnsupportedEncoding
        ))
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        service.structural_parsers["json"].certify_intrinsic_identity_limit(&request, &cancelled),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn source_only_generated_file_retains_exact_origin_mapping() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::create_dir(root.path().join("generated")).unwrap();
    let mut generated =
        b"// Code generated by fixture-gen. DO NOT EDIT.\n// source: origin.jsonc\n".to_vec();
    generated.extend(many_scalars());
    let origin = b"{\"origin_property\":1}\n";
    fs::write(root.path().join("generated/values.jsonc"), &generated).unwrap();
    fs::write(root.path().join("origin.jsonc"), origin).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let generated_reference = source_reference(&service, &receipt, "generated/values.jsonc");
    let origin_reference = source_reference(&service, &receipt, "origin.jsonc");
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.id == generated_reference.span().file())
        .unwrap();
    assert_eq!(
        (
            file.language.as_str(),
            file.encoding.as_str(),
            file.generated
        ),
        ("jsonc", "utf-8", true)
    );
    assert_eq!(generated_reference.content_hash(), content_hash(&generated));
    assert_eq!(origin_reference.content_hash(), content_hash(origin));
    let mappings = document
        .source_mappings
        .iter()
        .filter(|mapping| mapping.from.span().file() == file.id)
        .collect::<Vec<_>>();
    assert_eq!(mappings.len(), 1);
    let mapping = mappings[0];
    assert_eq!(mapping.kind, SourceMappingKind::GeneratedToOrigin);
    assert_eq!(mapping.from, generated_reference);
    assert_eq!(mapping.to, origin_reference);
    assert_eq!(
        mapping.evidence.derivation,
        vec![FactRef::File(origin_reference.span().file())]
    );
    let provenance = document
        .provenance
        .iter()
        .find(|row| row.id == mapping.provenance)
        .unwrap();
    assert_eq!(
        provenance.rule.as_deref(),
        Some("rootlight.generated-origin.v1:fixture-gen:none")
    );
    let mut expected_sources = vec![generated_reference.clone(), origin_reference];
    expected_sources.sort_unstable();
    assert_eq!(expected_sources.len(), 2);
    assert_eq!(provenance.input_sources, expected_sources);
    assert_eq!(provenance.evidence_sources, expected_sources);
    let mut read_back = Vec::new();
    for start in (0..generated.len()).step_by(32 * 1024) {
        let selected = SourceRef::new(
            generated_reference.repository(),
            generated_reference.generation(),
            SourceSpan::new(
                generated_reference.span().file(),
                start as u64,
                generated.len().min(start + 32 * 1024) as u64,
            )
            .unwrap(),
            generated_reference.content_hash(),
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
        assert_eq!(read.data.chunks[0].reference, selected);
        read_back.extend_from_slice(&read.data.chunks[0].bytes);
    }
    assert_eq!(read_back, generated);
}

struct UncertifiedRecordsParser {
    capabilities: rootlight_adapter_sdk::ParseCapabilities,
    foreign: Option<rootlight_adapter_treesitter::NativeIdentityWorkLimit>,
    probes: std::sync::atomic::AtomicUsize,
}

fn generic_records_error() -> AdapterError {
    AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
        resource: rootlight_adapter_sdk::ResourceKind::Records,
        observed: 1_048_577,
        limit: 1_048_576,
    })
}

impl ParseProvider for UncertifiedRecordsParser {
    fn capabilities(&self) -> &rootlight_adapter_sdk::ParseCapabilities {
        &self.capabilities
    }
    fn parse(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _sink: &mut dyn rootlight_adapter_sdk::SyntaxFactSink,
        _cancellation: &Cancellation,
    ) -> Result<rootlight_adapter_sdk::ParseReport, AdapterError> {
        Err(generic_records_error())
    }
}

impl StructuralPreflight for UncertifiedRecordsParser {
    fn required_syntax_fact_count(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Err(generic_records_error())
    }
    fn certify_intrinsic_identity_limit(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _cancellation: &Cancellation,
    ) -> Result<Option<rootlight_adapter_treesitter::NativeIdentityWorkLimit>, AdapterError> {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.foreign.clone())
    }
}

#[test]
fn generic_records_and_foreign_native_certificate_cannot_authorize_source_only() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = many_scalars();
    fs::write(root.path().join("values.json"), &bytes).unwrap();
    let repository = derive_repository(b"uncertified-records-fixture").id();
    let generation = GenerationId::from_bytes([81; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("values.json")).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "json",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let native = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    let proof = native
        .certify_intrinsic_identity_limit(
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
    for foreign in [None, Some(proof)] {
        let parser = Arc::new(UncertifiedRecordsParser {
            capabilities: native.capabilities().clone(),
            foreign,
            probes: std::sync::atomic::AtomicUsize::new(0),
        });
        let analyzer = TreeSitterAnalyzer::new(
            Arc::clone(&parser) as Arc<dyn ParseProvider>,
            ProducerIdentity::new(
                "uncertified-records-fixture",
                "1.0",
                first_slice_build_context(),
            )
            .unwrap(),
            LanguageId::new("json").unwrap(),
            "fixture-native",
            content_hash(b"uncertified-records-fixture"),
        )
        .unwrap();
        let result = crate::native_source_admission::certify(
            &service.analysis_limits,
            FirstSliceIndexMode::Structural,
            &analyzer,
            parser.as_ref(),
            &request,
            &generic_records_error(),
            &Cancellation::new(),
        );
        if parser.foreign.is_none() {
            assert!(result.unwrap().is_none());
        } else {
            assert!(matches!(result, Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "native-limit-certificate-mismatch"));
        }
        assert_eq!(parser.probes.load(std::sync::atomic::Ordering::Relaxed), 1);
        parser.probes.store(0, std::sync::atomic::Ordering::Relaxed);
        let mismatched = crate::native_source_admission::certify(
            &service.analysis_limits,
            FirstSliceIndexMode::Structural,
            &service.analyzers["json"],
            parser.as_ref(),
            &request,
            &generic_records_error(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(mismatched.is_none());
        assert_eq!(
            parser.probes.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a different registered parser must never run a certification probe"
        );
    }
}

#[test]
fn vbp_presence_stays_strict_across_snapshot_and_streamed_source_lanes() {
    // This is a parser-snapshot ceiling only; native Match/capture/IR ceilings
    // remain unchanged. The JSON remains below it, while the large VBP streams.
    const SNAPSHOT_BYTES: usize = 3 * 1024 * 1024;
    let configuration =
        format!("version = \"1.1\"\n[analysis]\nmax_source_file_bytes = {SNAPSHOT_BYTES}\n");
    let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: &configuration,
    }])
    .unwrap();
    let policy = FirstSliceStoragePolicy::from_config(config, 3);
    let json = many_scalars();
    assert!(json.len() < SNAPSHOT_BYTES);
    for streamed in [false, true] {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let path = if streamed {
            "project.VbP"
        } else {
            "project.vbp"
        };
        let mut project = b"Type=Exe\nName=\"fixture\"\n".to_vec();
        if streamed {
            project.resize(SNAPSHOT_BYTES + 1, b' ');
        }
        fs::write(root.path().join(path), &project).unwrap();
        fs::write(
            root.path().join("lib.rs"),
            b"pub fn retained_sibling() {}\n",
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &cancellation,
        )
        .unwrap();
        let first = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        let reference = source_reference(&service, &first, path);
        assert_eq!(reference.content_hash(), content_hash(&project));
        assert_eq!(reference.span().end_byte(), project.len() as u64);
        let lease = service
            .generation_lease(first.generation, &cancellation)
            .unwrap();
        let document = lease.generation().document();
        let parser_size = document.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "source-parser-snapshot-size-limit"
                && diagnostic.source.as_ref() == Some(&reference)
        });
        assert_eq!(
            parser_size, streamed,
            "the large VBP must really use ParserSizeLimit"
        );
        let retained = service
            .source_snapshots
            .snapshots(first.generation)
            .unwrap()
            .iter()
            .any(|snapshot| snapshot.file() == reference.span().file());
        assert_eq!(
            retained, !streamed,
            "the streamed VBP is absent from retained snapshots"
        );
        assert!(
            document
                .files
                .iter()
                .any(|file| file.id == reference.span().file() && file.path == path)
        );
        drop(lease);
        fs::write(root.path().join("values.json"), &json).unwrap();
        let failure = service
            .index_repository(root.path(), &cancellation)
            .unwrap_err();
        assert!(
            matches!(
                failure,
                FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::Records,
                    observed: 1_048_577,
                    limit: 1_048_576,
                }
            ),
            "all authenticated VBP paths must preserve the original typed refusal: {failure:?}"
        );
        assert_eq!(
            service.active_generation_for(first.repository),
            Some(first.generation)
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
        let still_retained = source_reference(&service, &first, path);
        assert_eq!(still_retained, reference);
    }
}

#[test]
fn unavailable_competing_source_withholds_generic_singleton_resolution() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(
        root.path().join("target.ts"),
        b"function selectTarget() {}\n",
    )
    .unwrap();
    let caller = b"function invoke() { selectTarget(); }\n";
    fs::write(root.path().join("caller.ts"), caller).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(3).unwrap();
    let first = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let caller_file = source_reference(&service, &first, "caller.ts")
        .span()
        .file();
    let first_lease = service
        .generation_lease(first.generation, &cancellation)
        .unwrap();
    let first_call = first_lease
        .generation()
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.file == caller_file
                && occurrence.role == rootlight_ir::OccurrenceRole::CallSite
                && occurrence.syntactic_text_hash == content_hash(b"selectTarget")
        })
        .unwrap();
    assert!(
        matches!(&first_call.target, rootlight_ir::OccurrenceTarget::Candidates {
        symbols, total_count: 1, completeness: CoverageStatus::Complete,
    } if symbols.len() == 1),
        "the control must exercise actual generic singleton selection: {:?}",
        first_call.target
    );
    drop(first_lease);
    // Bounded sibling fanout avoids unrelated quadratic native parent lookup.
    // The written competing declaration is real but unavailable after query work
    // refusal; its source must prevent a supposedly complete singleton decision.
    let mut branch = b"{}".to_vec();
    for _ in 0..3 {
        let mut parent = vec![b'{'];
        for _ in 0..32 {
            parent.extend_from_slice(&branch);
        }
        parent.push(b'}');
        branch = parent;
    }
    let mut unavailable = b"function selectTarget() {}\n".to_vec();
    for _ in 0..32 {
        unavailable.extend_from_slice(&branch);
    }
    fs::write(root.path().join("competing.ts"), &unavailable).unwrap();
    let second = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(second.parent, Some(first.generation));
    let missing = source_reference(&service, &second, "competing.ts");
    assert_eq!(missing.content_hash(), content_hash(&unavailable));
    let lease = service
        .generation_lease(second.generation, &cancellation)
        .unwrap();
    let document = lease.generation().document();
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "native-identity-work-limit"
                && diagnostic.source.as_ref() == Some(&missing))
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "selectTarget")
            .count(),
        1
    );
    let call = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.file == caller_file
                && occurrence.role == rootlight_ir::OccurrenceRole::CallSite
                && occurrence.syntactic_text_hash == content_hash(b"selectTarget")
        })
        .unwrap();
    assert!(
        matches!(
            call.target,
            rootlight_ir::OccurrenceTarget::Unresolved { .. }
        ),
        "missing declarations must not yield a generic complete singleton: {:?}",
        call.target
    );
    let source = caller
        .windows(b"selectTarget".len())
        .position(|part| part == b"selectTarget")
        .unwrap();
    assert_eq!(
        (
            call.source.span().start_byte(),
            call.source.span().end_byte()
        ),
        (source as u64, (source + b"selectTarget".len()) as u64)
    );
    assert_eq!(call.source.content_hash(), content_hash(caller));
}
// Public source preservation for a genuine native census refusal.
// Native limits and native proof requirements stay unchanged throughout.

// Bounded fanout keeps native depth and sibling lookup small while the complete
// C formal census genuinely exceeds its unchanged two-million-step allowance.
fn many_native_census_nodes() -> Vec<u8> {
    let mut branch = vec![b'0'];
    for _ in 0..4 {
        let mut group = vec![b'{'];
        for index in 0..32 {
            if index != 0 {
                group.push(b',');
            }
            group.extend_from_slice(&branch);
        }
        group.push(b'}');
        branch = group;
    }
    let mut bytes =
        b"/*ababa*/\nvoid forward(int value) { sink(value); }\nstatic int values[] = ".to_vec();
    bytes.extend_from_slice(&branch);
    bytes.extend_from_slice(b";\n");
    bytes
}

fn assert_native_helper_gap(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt) {
    let reference = source_reference(service, receipt, "values.c");
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    let document = lease.generation().document();
    let file = reference.span().file();
    assert!(
        document
            .files
            .iter()
            .any(|record| record.id == file && record.language == "c")
    );
    assert!(!document.entities.iter().any(|record| {
        record
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file)
    }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|record| record.source.span().file() == file)
    );
    assert!(!document.relations.iter().any(|record| {
        record
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file)
    }));
    let diagnostic = document
        .diagnostics
        .iter()
        .find(|record| {
            record.code == "native-query-refusal" && record.source.as_ref() == Some(&reference)
        })
        .unwrap();
    assert!(
        diagnostic
            .message
            .contains("origin=query-identity-c-formal-reads-limit")
    );
    assert!(diagnostic.message.contains("unit=checked-native-work"));
    assert!(
        diagnostic
            .message
            .contains("observed=2097153 limit=2097152")
    );
    assert!(!diagnostic.message.contains("resource Records"));
    assert!(!diagnostic.message.contains("ababa"));
    assert!(!diagnostic.message.contains("forward"));
    assert!(!diagnostic.message.contains("values.c"));
    assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == reference
                    && gap.domain == domain
                    && gap.reason == SkippedRegionReason::ResourceLimit
                    && gap.detail == "native-query-refusal")
        );
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file, &Cancellation::new())
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::ResourceBounded)
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
}

#[test]
fn native_helper_refusal_preserves_source_across_reopen_and_repair() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let storage = durable_test_tempdir();
    let bytes = many_native_census_nodes();
    fs::write(root.path().join("values.c"), &bytes).unwrap();
    fs::write(
        root.path().join("lib.rs"),
        b"pub fn retained_sibling() {}\n",
    )
    .unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    let first = service
        .index_repository(root.path(), &cancellation)
        .expect("genuine native helper refusal preserves exact captured source");
    assert_native_helper_gap(&service, &first);
    assert_source(&service, &first, "values.c", &bytes);
    assert_eq!(
        service
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        first
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
    drop(service);
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    assert_native_helper_gap(&service, &first);
    assert_source(&service, &first, "values.c", &bytes);
    let restored = b"void restored_analysis(int value) { sink(value); }\n";
    fs::write(root.path().join("values.c"), restored).unwrap();
    let second = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(second.parent, Some(first.generation));
    let lease = service
        .generation_lease(second.generation, &cancellation)
        .unwrap();
    let entity = lease
        .generation()
        .document()
        .entities
        .iter()
        .find(|entity| entity.language == "c" && entity.canonical_name == "restored_analysis")
        .unwrap();
    let evidence = entity.evidence.source.as_ref().unwrap();
    assert_eq!(evidence.generation(), second.generation);
    assert_eq!(evidence.content_hash(), content_hash(restored));
    assert!(
        !lease
            .generation()
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "native-query-refusal")
    );
    let reference = source_reference(&service, &second, "values.c");
    let read = service
        .source_read_with_options_and_budget(
            second.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, reference);
    assert_eq!(read.data.chunks[0].bytes, restored);
    assert_source(&service, &first, "values.c", &bytes);
}

#[test]
fn helper_refusal_preserves_explicit_policy_and_deep_mode_failure() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(root.path().join("values.c"), many_native_census_nodes()).unwrap();
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
        assert!(
            service
                .index_repository_with_mode(root.path(), mode, &Cancellation::new())
                .is_err()
        );
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
// Genuine native issuance, source binding and admission must agree.
// Copied source-free error codes never authorize file-only publication.

#[test]
fn native_helper_certificate_conserves_source_provider_and_admission() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = many_native_census_nodes();
    fs::write(root.path().join("values.c"), &bytes).unwrap();
    let repository = derive_repository(b"native-helper-certificate-fixture").id();
    let generation = GenerationId::from_bytes([82; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("values.c")).unwrap(),
            bytes.len() as u64,
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "c",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let proof = service.structural_parsers["c"]
        .certify_intrinsic_identity_refusal(&request, &Cancellation::new())
        .unwrap()
        .unwrap();
    assert!(service.structural_parsers["c"].owns_native_identity_refusal(&proof));
    assert!(proof.matches_request(&request));
    assert!(proof.matches_error(&proof.adapter_error()));
    assert_eq!(
        proof.details().origin(),
        "query-identity-c-formal-reads-limit"
    );
    assert_eq!(
        (proof.details().observed(), proof.details().limit()),
        (Some(2_097_153), Some(2_097_152))
    );
    assert!(proof.details().progress().0.unwrap() < 100);
    assert!(proof.details().progress().1.unwrap() < 100);
    assert!(!proof.matches_error(&generic_records_error()));
    let other = structural_analysis_request(
        repository,
        GenerationId::from_bytes([83; 20]),
        &snapshot,
        false,
        "c",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(!proof.matches_request(&other));
    assert!(!proof.matches_source(proof.source(), "cpp", "utf-8"));
    assert!(!proof.matches_source(proof.source(), "c", "utf-16le"));
    let foreign = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    assert!(!proof.was_issued_by(&foreign));
    assert!(matches!(
        foreign.certify_intrinsic_identity_refusal(
            &request,
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &Cancellation::new()
        ),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::MemoryEnforcementUnavailable
        ))
    ));
    let current = &service.analysis_limits;
    let constrained = AnalysisLimits::new(
        current.max_source_bytes(),
        1,
        current.max_syntax_depth(),
        current.max_embedded_ranges(),
        current.max_reported_memory_bytes(),
        current.syntax_stream().clone(),
        current.ir_stream().clone(),
        current.ir().clone(),
    )
    .unwrap();
    let constrained_request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "c",
        "utf-8",
        &constrained,
    )
    .unwrap()
    .to_parse_request();
    assert!(!proof.matches_request(&constrained_request));
    assert!(
        service.structural_parsers["c"]
            .certify_intrinsic_identity_refusal(&constrained_request, &Cancellation::new())
            .is_err()
    );
    let encoded = structural_analysis_request(
        repository, generation, &snapshot, false, "c", "bytes", current,
    )
    .unwrap()
    .to_parse_request();
    assert!(matches!(
        service.structural_parsers["c"]
            .certify_intrinsic_identity_refusal(&encoded, &Cancellation::new()),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::UnsupportedEncoding
        ))
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        service.structural_parsers["c"].certify_intrinsic_identity_refusal(&request, &cancelled),
        Err(AdapterError::Cancelled { .. })
    ));
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

struct UncertifiedRefusalParser {
    capabilities: rootlight_adapter_sdk::ParseCapabilities,
    foreign: Option<rootlight_adapter_treesitter::NativeIdentityRefusal>,
}

impl ParseProvider for UncertifiedRefusalParser {
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
            code: rootlight_adapter_sdk::DiagnosticCode::new("query-identity-c-formal-reads-limit")
                .unwrap(),
        })
    }
}

impl StructuralPreflight for UncertifiedRefusalParser {
    fn required_syntax_fact_count(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Err(AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("query-identity-c-formal-reads-limit")
                .unwrap(),
        })
    }
    fn certify_intrinsic_identity_refusal(
        &self,
        _request: &rootlight_adapter_sdk::ParseRequest<'_>,
        _cancellation: &Cancellation,
    ) -> Result<Option<rootlight_adapter_treesitter::NativeIdentityRefusal>, AdapterError> {
        Ok(self.foreign.clone())
    }
}

#[test]
fn copied_refusal_code_and_foreign_proof_cannot_authorize_source_only() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = many_native_census_nodes();
    fs::write(root.path().join("values.c"), &bytes).unwrap();
    let repository = derive_repository(b"uncertified-helper-fixture").id();
    let generation = GenerationId::from_bytes([84; 20]);
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let snapshot = repository_root
        .snapshot(
            &RelativePath::parse(Path::new("values.c")).unwrap(),
            bytes.len() as u64,
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "c",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let proof = service.structural_parsers["c"]
        .certify_intrinsic_identity_refusal(&request, &Cancellation::new())
        .unwrap()
        .unwrap();
    for foreign in [None, Some(proof)] {
        let parser = Arc::new(UncertifiedRefusalParser {
            capabilities: service.structural_parsers["c"].capabilities().clone(),
            foreign,
        });
        let analyzer = TreeSitterAnalyzer::new(
            Arc::clone(&parser) as Arc<dyn ParseProvider>,
            ProducerIdentity::new(
                "uncertified-helper-fixture",
                "1.0",
                first_slice_build_context(),
            )
            .unwrap(),
            LanguageId::new("c").unwrap(),
            "fixture-native",
            content_hash(b"uncertified-helper-fixture"),
        )
        .unwrap();
        let original = AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("query-identity-c-formal-reads-limit")
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
            assert!(matches!(result, Err(AdapterError::ProviderFailed { code })
                if code.as_str() == "native-refusal-certificate-mismatch"));
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
    }
}
