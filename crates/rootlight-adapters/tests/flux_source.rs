//! Flux adapter controls exercise real native ASTs and normalized source proofs.
//! Binding is local to clean source-order scopes; unsupported compiler effects
//! and malformed/over-budget analysis cannot manufacture a resolved target.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::FluxParseProvider;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, FactDomain,
    IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    ProducerIdentity, SourceRef, SourceSpan, decode_symbol_identity_claim_envelope,
    validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

fn with_case<T>(
    source: &[u8],
    encoding: EncodingId,
    ir: IrLimits,
    nodes: usize,
    depth: usize,
    operation: impl FnOnce(
        &TreeSitterAnalyzer,
        &FluxParseProvider,
        &AnalysisRequest<'_>,
    ) -> Result<T, AdapterError>,
) -> Result<T, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("sample.flux"), source).unwrap();
    let repository = derive_repository(b"flux-native-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("sample.flux")).unwrap(),
            262_144,
        )
        .unwrap();
    let reference = SourceRef::new(
        repository,
        GenerationId::from_bytes([31; 20]),
        SourceSpan::new(
            snapshot.file(),
            0,
            snapshot.content().len().try_into().unwrap(),
        )
        .unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(FluxParseProvider::new(262_144, nodes).unwrap());
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "flux-source-contract",
            "1.0",
            content_hash(b"flux-source-config"),
        )
        .unwrap(),
        LanguageId::new("flux").unwrap(),
        "official-flux-source-contract",
        FluxParseProvider::binary_digest(),
    )
    .unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        262_144,
        nodes,
        depth,
        1,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &reference).unwrap(),
        LanguageId::new("flux").unwrap(),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"flux-source-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    operation(&analyzer, provider.as_ref(), &request)
}

fn analyze_raw(
    source: &[u8],
    encoding: EncodingId,
    ir: IrLimits,
    nodes: usize,
    depth: usize,
    cancellation: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    with_case(
        source,
        encoding,
        ir,
        nodes,
        depth,
        |analyzer, _, request| {
            let output = execute_analysis(
                analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                cancellation,
            )?;
            validate_ir_document(
                output.document(),
                request.limits().ir(),
                &ExtensionSupport::default(),
            )
            .unwrap();
            Ok(output)
        },
    )
}

fn analyze(
    source: &str,
    nodes: usize,
    depth: usize,
    cancel: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    analyze_raw(
        source.as_bytes(),
        EncodingId::utf8(),
        IrLimits::default(),
        nodes,
        depth,
        cancel,
    )
}

fn successful(source: &str) -> AnalysisOutput {
    analyze(source, 16_384, 128, &Cancellation::new()).unwrap()
}

fn read<'a>(document: &'a NormalizedIrDocument, start: usize, name: &str) -> &'a OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == (start + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "native Read {start}:{name}");
    sites[0]
}

fn symbol(
    document: &NormalizedIrDocument,
    source: &str,
    marker: &str,
    name: &str,
) -> rootlight_ids::SymbolId {
    let start = source.find(marker).unwrap() + marker.find(name).unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == (start + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "definition {marker:?}:{name:?}: {sites:#?}");
    let OccurrenceTarget::Resolved { symbol } = &sites[0].target else {
        panic!("definition target");
    };
    *symbol
}

#[test]
fn direct_native_parameter_values_are_reads_and_argument_keys_are_not() {
    let source = "scale = (d, n) => date.scale(d: d, n: n)\n";
    let output = successful(source);
    let document = output.document();
    let function = symbol(document, source, "scale =", "scale");
    for name in ["d", "n"] {
        let target = symbol(document, source, "(d, n)", name);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Parameter);
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(function))
        );
        let value = source.rfind(name).unwrap();
        let site = read(document, value, name);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert_eq!(site.enclosing, Some(function));
        let key = source.find(&format!("{name}:")).unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == key as u64)
        );
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .count(),
        3
    );
}

#[test]
fn sibling_functions_with_equal_formals_keep_distinct_targets() {
    let source = "one = (value) => consume(v: value)\ntwo = (value) => consume(v: value)\n";
    let output = successful(source);
    let document = output.document();
    let targets: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect();
    assert_eq!(targets.len(), 2);
    assert_ne!(targets[0].id, targets[1].id);
    for target in targets {
        let owner = target.container.unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.target == (OccurrenceTarget::Resolved { symbol: target.id })
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            owner,
            rootlight_ir::ContainerRef::Entity(sites[0].enclosing.unwrap())
        );
    }
}

#[test]
fn duplicate_default_block_nested_and_shorthand_contexts_withhold_read_certificates() {
    for source in [
        "one = (value, value) => consume(v: value)\n",
        "one = (value=1) => consume(v: value)\n",
        "one = (value) => { value = 2 return value }\n",
        "one = (value) => ((other) => consume(v: value))\n",
        "one = (value) => ({value with other: value})\n",
        "one = (value) => if true then value else value\n",
    ] {
        let output = successful(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(!output.document().skipped_regions.is_empty(), "{source}");
    }
}

#[test]
fn native_shorthand_values_preserve_exact_formal_read_ownership() {
    for source in [
        "one = (value) => ({value})\n",
        "one = (value) => consume(value)\n",
        "one = (变量) => ({变量})\n",
    ] {
        let output = successful(source);
        let name = if source.contains("变量") {
            "变量"
        } else {
            "value"
        };
        let target = symbol(output.document(), source, &format!("({name}) =>"), name);
        let site = read(output.document(), source.rfind(name).unwrap(), name);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert_eq!(
            &source.as_bytes()[usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap()],
            name.as_bytes()
        );
    }
}

#[test]
fn member_property_labels_and_outer_names_do_not_acquire_parameter_targets() {
    let source = "outside = 1\none = (value) => consume(value: other.value)\n";
    let output = successful(source);
    let document = output.document();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == source.rfind("value").unwrap() as u64)
    );
}

#[test]
fn body_changes_preserve_declared_identity_and_signature_but_refresh_source_hashes() {
    let first = successful("measure = (value) => consume(v: value)\n");
    let second = successful("measure = (value) => consume(v: value + 1)\n");
    let claims = |output: &AnalysisOutput| {
        let mut claims: Vec<_> = output
            .document()
            .extensions
            .iter()
            .filter_map(|extension| decode_symbol_identity_claim_envelope(extension).ok())
            .map(|claim| {
                (
                    claim.declared_identity,
                    claim.symbol,
                    claim.signature_discriminator,
                )
            })
            .collect();
        claims.sort_by(|a, b| a.0.cmp(&b.0));
        claims
    };
    let a = claims(&first);
    let b = claims(&second);
    assert_eq!(a.len(), 2);
    assert_eq!(a, b);
    assert_ne!(
        first.document().files[0].content_hash,
        second.document().files[0].content_hash
    );
    for output in [&first, &second] {
        for site in &output.document().occurrences {
            assert_eq!(
                site.source.content_hash(),
                output.document().files[0].content_hash
            );
        }
    }
}

#[test]
fn bom_unicode_and_crlf_use_physical_byte_coordinates() {
    for source in [
        "\u{feff}measure = (变量) => consume(变量)\r\n",
        "// é\r\nmeasure = (é) => consume(é)\r\n",
    ] {
        let output = successful(source);
        let name = if source.contains("变量") {
            "变量"
        } else {
            "é"
        };
        let target = symbol(output.document(), source, &format!("({name}) =>"), name);
        let site = read(output.document(), source.rfind(name).unwrap(), name);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert_eq!(
            &source.as_bytes()[usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap()],
            name.as_bytes()
        );
    }
}

#[test]
fn utf16_physical_formals_and_reads_keep_the_original_hash_and_exact_spans() {
    let source = "// é\r\nmeasure = (value) => consume(v: value)\r\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let encoding = EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap();
        let output = analyze_raw(
            &raw,
            encoding,
            IrLimits::default(),
            16_384,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        let document = output.document();
        assert_eq!(document.files[0].content_hash, content_hash(&raw));
        let value_byte = source.rfind("value").unwrap();
        let start = 2 + source[..value_byte].encode_utf16().count() * 2;
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == start as u64
                    && site.source.span().end_byte() == (start + 10) as u64
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(matches!(sites[0].target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(sites[0].source.content_hash(), content_hash(&raw));
    }
}

#[test]
fn native_syntax_and_unsupported_validation_are_unknown_without_partial_certificates() {
    for (source, reason) in [
        ("one = (value) => consume(\n", "flux-native-parse-recovery"),
        (
            "one = (value) => /a+/\n",
            "flux-regex-validation-unavailable",
        ),
        (
            "one = (value) => 2020-01-01T00:00:00.12345678901234567890Z\n",
            "flux-time-precision-unavailable",
        ),
    ] {
        let output = successful(source);
        assert!(output.document().entities.is_empty());
        assert!(output.document().occurrences.is_empty());
        assert!(output.document().relations.is_empty());
        assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain && gap.detail == reason)
            );
        }
    }
}

#[test]
fn syntax_work_depth_record_and_cancellation_limits_reject_without_successful_partial_results() {
    assert!(
        analyze(
            "one = (value) => consume(v: value)\n",
            4,
            128,
            &Cancellation::new()
        )
        .is_err()
    );
    assert!(
        analyze(
            "one = (value) => consume(v: value)\n",
            16_384,
            3,
            &Cancellation::new()
        )
        .is_err()
    );
    let long = format!("one = (value) => \"{}\"\n", "a".repeat(100_000));
    let result = analyze(&long, 32, 128, &Cancellation::new());
    assert!(
        matches!(result, Err(AdapterError::ProviderFailed { code }) if code.as_str() == "flux-native-work-limit")
    );
    let mut ir = IrLimits::default();
    ir.max_total_records = 8;
    assert!(
        analyze_raw(
            b"one = (value) => consume(v: value)\n",
            EncodingId::utf8(),
            ir,
            16_384,
            128,
            &Cancellation::new()
        )
        .is_err()
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze("one = (value) => value\n", 16_384, 128, &cancel),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn exact_preflight_and_prepared_artifact_reuse_bind_the_actual_current_source() {
    let source = b"measure = (value) => consume(v: value)\n";
    with_case(source, EncodingId::utf8(), IrLimits::default(), 16_384, 128, |analyzer, provider, request| {
        let cancel = Cancellation::new();
        let required = provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
        let parsed = rootlight_adapter_sdk::execute_parse(provider, &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel)?;
        assert_eq!(parsed.facts().len(), required);
        assert!(required > 3);
        let artifact = analyzer.prepare_artifact(request, &cancel)?;
        let baseline = execute_analysis(analyzer, request, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel)?;
        let cached = analyzer.analyze_prepared_artifact(request, &artifact, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel)?;
        assert_eq!(baseline.document(), cached.document());

        let edited = b"measure = (value) => consume(v: value + 1)\n";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("sample.flux"), edited).unwrap();
        let repository = request.source().source_ref().repository();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root.snapshot(&RelativePath::parse(Path::new("sample.flux")).unwrap(), 262_144).unwrap();
        let reference = SourceRef::new(repository, GenerationId::from_bytes([32;20]),
            SourceSpan::new(snapshot.file(), 0, edited.len() as u64).unwrap(), snapshot.content_hash(), None);
        let next = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &reference).unwrap(), LanguageId::new("flux").unwrap(),
            EncodingId::utf8(), Vec::new(), AnalysisTier::TierD, request.build_context(), request.limits()).unwrap()
            .with_generated_status(false);
        assert!(matches!(analyzer.analyze_prepared_artifact(&next, &artifact, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel),
            Err(AdapterError::ProviderFailed {code}) if code.as_str() == "treesitter-structural-artifact-mismatch"));
        let current = execute_analysis(analyzer, &next, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel)?;
        assert_eq!(current.document().files[0].content_hash, content_hash(edited));
        for site in &current.document().occurrences {
            assert_eq!(site.source.generation(), reference.generation());
            assert_eq!(site.source.content_hash(), reference.content_hash());
        }
        Ok(())
    }).unwrap();
}

#[test]
fn actual_relation_and_total_record_admission_boundaries_preserve_all_positive_facts() {
    let source = b"measure = (value) => value\n";
    let run = |ir| {
        analyze_raw(
            source,
            EncodingId::utf8(),
            ir,
            16_384,
            128,
            &Cancellation::new(),
        )
    };
    let baseline = run(IrLimits::default()).unwrap();
    let emitted_relations = baseline.document().relations.len();
    let emitted_records = baseline.report().resources().stream().records();
    assert!(emitted_relations >= 1 && emitted_records > emitted_relations);
    let measure = |mut capacity: usize, relations: Option<usize>| loop {
        let mut ir = IrLimits::default();
        let hard = if let Some(relations) = relations {
            ir.max_relations = relations;
            ir.max_total_records = capacity;
            IrLimits::default().max_total_records
        } else {
            ir.max_relations = capacity;
            IrLimits::default().max_relations
        };
        match run(ir) {
            Ok(output) => {
                assert_eq!(output.document(), baseline.document());
                break capacity;
            }
            Err(AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::Records,
                observed,
                limit,
            })) => {
                assert_eq!(limit, capacity);
                assert!(observed > capacity && observed <= hard);
                capacity = observed;
            }
            result => panic!("unexpected boundary: {result:?}"),
        }
    };
    let relations = measure(emitted_relations, None);
    let total = measure(emitted_records, Some(relations));
    for (relations_limit, total_limit, expected) in [
        (relations, total, None),
        (relations - 1, total, Some(relations - 1)),
        (relations, total - 1, Some(total - 1)),
    ] {
        let mut ir = IrLimits::default();
        ir.max_relations = relations_limit;
        ir.max_total_records = total_limit;
        match (expected, run(ir)) {
            (None, Ok(output)) => assert_eq!(output.document(), baseline.document()),
            (
                Some(expected),
                Err(AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::Records,
                    observed,
                    limit,
                })),
            ) => {
                assert_eq!(limit, expected);
                assert!(observed > limit);
            }
            result => panic!("unexpected boundary: {result:?}"),
        }
    }
}

#[test]
fn original_native_ast_count_remains_distinct_from_compact_ir_projection() {
    let source =
        std::str::from_utf8(include_bytes!("fixtures/flux/original-scale_test.flux")).unwrap();
    let output = successful(source);
    assert_eq!(output.report().resources().syntax_nodes(), 143);
    for (name, kind, start, end) in [
        ("scaleTime", EntityKind::Function, 117, 126),
        ("d", EntityKind::Parameter, 130, 131),
        ("n", EntityKind::Parameter, 133, 134),
    ] {
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1);
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("original definition");
        };
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
    }
}

#[path = "flux_source/depth_decline.rs"]
mod depth_decline;

#[path = "flux_source/audit_depth_decline.rs"]
mod audit_depth_decline;
