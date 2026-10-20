//! Public Less controls use native Raffia syntax and physical normalized IR.
//! Negative cases preserve readable source while withholding unsupported binding.

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ResourceKind, SinkError,
    StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::LessParseProvider;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, FactDomain, IrDocumentUsage,
    IrLimits, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    RelationEndpoint, RelationPredicate, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::{fs, path::Path, sync::Arc};

fn analyze(
    raw: &[u8],
    encoding: &str,
    nodes: usize,
    depth: usize,
    ir: IrLimits,
    cancel: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("size.less"), raw).unwrap();
    let repository = derive_repository(b"less-native-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("size.less")).unwrap(),
            262_144,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([41; 20]),
        SourceSpan::new(snapshot.file(), 0, raw.len().try_into().unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(LessParseProvider::new(262_144, nodes).unwrap());
    let analyzer = TreeSitterAnalyzer::new(
        provider,
        ProducerIdentity::new(
            "less-source-contract",
            "1.0",
            content_hash(b"less-source-config"),
        )
        .unwrap(),
        LanguageId::new("less").unwrap(),
        "raffia-less-source-contract",
        LessParseProvider::binary_digest(),
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
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("less").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"less-source-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancel,
    )?;
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files.len(), 1);
    let file = &output.document().files[0];
    assert_eq!(file.content_hash, content_hash(raw));
    assert_eq!(file.byte_length, raw.len() as u64);
    assert_eq!(file.language, "less");
    Ok(output)
}
fn successful(source: &str) -> AnalysisOutput {
    analyze(
        source.as_bytes(),
        "utf-8",
        16_384,
        128,
        IrLimits::default(),
        &Cancellation::new(),
    )
    .unwrap()
}
fn original() -> &'static [u8] {
    include_bytes!("fixtures/less/size.less")
}
fn definition(
    document: &NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|e| e.language == "less" && e.kind == kind && e.canonical_name == name)
        .collect();
    assert_eq!(entities.len(), 1, "unique native {kind:?} {name}");
    let entity = entities[0];
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|s| {
            s.role == OccurrenceRole::Definition
                && s.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                && s.source.span().start_byte() == start
                && s.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact definition {name}");
    assert_eq!(
        sites[0].source.content_hash(),
        document.files[0].content_hash
    );
    entity.id
}
fn original_assertions(document: &NormalizedIrDocument, physical: impl Fn(u64) -> u64) {
    let size = definition(
        document,
        ".size",
        EntityKind::Function,
        physical(23),
        physical(28),
    );
    let square = definition(
        document,
        ".square",
        EntityKind::Function,
        physical(92),
        physical(99),
    );
    let width = definition(
        document,
        "width",
        EntityKind::Parameter,
        physical(30),
        physical(35),
    );
    let height = definition(
        document,
        "height",
        EntityKind::Parameter,
        physical(38),
        physical(44),
    );
    for target in [width, height] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(size)
                    && r.object == RelationEndpoint::Entity(target))
                .count(),
            1
        );
    }
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|r| {
            r.predicate == RelationPredicate::Calls
                && r.subject == RelationEndpoint::Entity(square)
                && r.object == RelationEndpoint::Entity(size)
        })
        .collect();
    assert_eq!(calls.len(), 1);
    let source = calls[0].evidence.source.as_ref().unwrap();
    assert_eq!(source.span().start_byte(), physical(112));
    assert_eq!(source.span().end_byte(), physical(117));
    assert_eq!(source.content_hash(), document.files[0].content_hash);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|s| {
            s.source == *source
                && s.role == OccurrenceRole::CallSite
                && s.enclosing == Some(square)
                && s.target == (OccurrenceTarget::Resolved { symbol: size })
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|g| g.domain == FactDomain::Relations
                && g.detail
                    == "less-import-dynamic-scope-expansion-and-runtime-bindings-unavailable")
    );
}

#[test]
fn original_less_four_definitions_three_relations_and_exact_native_witnesses() {
    assert_eq!(original().len(), 139);
    let output = analyze(
        original(),
        "utf-8",
        16_384,
        128,
        IrLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    original_assertions(output.document(), |n| n);
}

#[test]
fn guarded_overloaded_imported_nested_and_dynamic_mixins_do_not_leak_resolved_calls() {
    for source in [
        ".size(@a; @b) when (@a > 0) {width:@a;} .square(@x){.size(@x;@x);}",
        ".size(@a; @b){width:@a;} .size(@a; @b){height:@b;} .square(@x){.size(@x;@x);}",
        "@import 'other.less'; .size(@a; @b){width:@a;} .square(@x){.size(@x;@x);}",
        ".size(@a; @b){width:@a;} .square(@x){.size(@a;@b){height:@a;} .size(@x;@x);}",
        ".size(@a; @b){width:@a;} .square(@x){@rules:{.size(@a;@b){width:0;}}; @rules(); .size(@x;@x);}",
        ".size(@a; @b:1px){width:@a;} .square(@x){.size(@x;@x);}",
        ".size(@args...){width:1px;} .square(@x){.size(@x;@x);}",
        ".size(@a; @b){width:@a;} .square(@x){.size(@x);}",
        ".size(@a; @b){width:@a;} .square(@x){.other(@x;@x);}",
    ] {
        let output = successful(source);
        let document = output.document();
        assert!(
            !document
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls),
            "{source}"
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|s| s.role == OccurrenceRole::CallSite
                    && matches!(s.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|g| g.domain == FactDomain::Relations
                    && g.source.content_hash() == content_hash(source.as_bytes())),
            "{source}"
        );
    }
}

#[test]
fn case_sensitive_names_comments_and_strings_do_not_become_mixin_declarations() {
    let source = "// .fake(@a) {}\n@literal: '.other(@x) {}';\n.size(@a;@b){width:@a;} .square(@x){.Size(@x;@x);}";
    let output = successful(source);
    let document = output.document();
    assert!(
        !document
            .entities
            .iter()
            .any(|e| matches!(e.canonical_name.as_str(), ".fake" | ".other" | ".Size"))
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::Calls)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|g| g.domain == FactDomain::Relations)
    );
}

#[test]
fn body_edits_preserve_authored_headers_parameters_containers_and_symbol_ids() {
    let before = std::str::from_utf8(original()).unwrap();
    let after = before.replace("width: @width;", "width: 20px;");
    let a = successful(before);
    let b = successful(&after);
    for (name, kind) in [
        (".size", EntityKind::Function),
        (".square", EntityKind::Function),
        ("width", EntityKind::Parameter),
        ("height", EntityKind::Parameter),
    ] {
        let entity = |document: &NormalizedIrDocument| {
            document
                .entities
                .iter()
                .find(|e| e.kind == kind && e.canonical_name == name)
                .map(|e| (e.id, e.container))
        };
        assert_eq!(entity(a.document()), entity(b.document()), "stable {name}");
        assert!(entity(a.document()).is_some());
    }
    assert_eq!(
        a.document()
            .relations
            .iter()
            .filter(|r| r.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    assert_eq!(
        b.document()
            .relations
            .iter()
            .filter(|r| r.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
}

#[test]
fn utf8_bom_and_utf16_keep_exact_original_physical_definition_and_call_ranges() {
    let text = std::str::from_utf8(original()).unwrap();
    let utf8 = [b"\xef\xbb\xbf".as_slice(), original()].concat();
    let output = analyze(
        &utf8,
        "utf-8",
        16_384,
        128,
        IrLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    original_assertions(output.document(), |n| n + 3);
    for (encoding, bom, little) in [
        ("utf-16le", [0xff, 0xfe], true),
        ("utf-16be", [0xfe, 0xff], false),
    ] {
        let mut raw = bom.to_vec();
        for unit in text.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze(
            &raw,
            encoding,
            16_384,
            128,
            IrLimits::default(),
            &Cancellation::new(),
        )
        .unwrap();
        original_assertions(output.document(), |n| 2 + n * 2);
    }
}

fn quota_boundary(which: u8) {
    let baseline = analyze(
        original(),
        "utf-8",
        16_384,
        128,
        IrLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    let usage = IrDocumentUsage::measure(baseline.document(), &IrLimits::default()).unwrap();
    let mut without = baseline.document().clone();
    without
        .relations
        .retain(|r| r.predicate != RelationPredicate::Calls);
    let prior = IrDocumentUsage::measure(&without, &IrLimits::default()).unwrap();
    assert_eq!(usage.total_records(), prior.total_records() + 1);
    assert_eq!(usage.total_nested_items(), prior.total_nested_items() + 3);
    let resource = if which == 2 {
        ResourceKind::NestedItems
    } else {
        ResourceKind::Records
    };
    let execute = |maximum| {
        let mut limits = IrLimits::default();
        match which {
            0 => limits.max_relations = maximum,
            1 => limits.max_total_records = maximum,
            _ => limits.max_total_nested_items = maximum,
        }
        analyze(
            original(),
            "utf-8",
            16_384,
            128,
            limits,
            &Cancellation::new(),
        )
    };
    let mut maximum = 0;
    let mut admitted = None;
    for _ in 0..32 {
        match execute(maximum) {
            Ok(output) => {
                admitted = Some(output);
                break;
            }
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: actual,
                observed,
                limit,
            })) => {
                assert_eq!(actual, resource);
                assert_eq!(limit, maximum);
                assert!(observed > maximum);
                maximum = observed;
            }
            other => panic!("unexpected public admission {maximum}: {other:?}"),
        }
    }
    assert_eq!(
        admitted.expect("finite typed admission").document(),
        baseline.document()
    );
    assert!(maximum > 0);
    assert_eq!(
        execute(maximum - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource,
            observed: maximum,
            limit: maximum - 1
        })
    );
    assert_eq!(execute(maximum).unwrap().document(), baseline.document());
}
#[test]
fn less_relation_exact_and_one_less_preserve_native_call_proof() {
    quota_boundary(0);
}
#[test]
fn less_total_record_exact_and_one_less_preserve_native_call_proof() {
    quota_boundary(1);
}
#[test]
fn less_nested_item_exact_and_one_less_preserve_native_call_proof() {
    quota_boundary(2);
}

#[test]
fn syntax_depth_node_and_cancel_refusals_cannot_publish_partial_success() {
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze(
            original(),
            "utf-8",
            16_384,
            128,
            IrLimits::default(),
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    for (nodes, depth) in [(1, 128), (16_384, 1)] {
        assert!(
            analyze(
                original(),
                "utf-8",
                nodes,
                depth,
                IrLimits::default(),
                &Cancellation::new()
            )
            .is_err()
        );
    }
    original_assertions(
        successful(std::str::from_utf8(original()).unwrap()).document(),
        |n| n,
    );
}

#[test]
fn malformed_native_less_keeps_explicit_unknown_domains_without_fake_bindings() {
    let source = ".size(@a; @b) { width: @a;\n.square(@x) { .size(@x;@x);";
    let output = successful(source);
    let document = output.document();
    assert!(document.entities.is_empty());
    assert!(
        !document
            .occurrences
            .iter()
            .any(|s| matches!(s.target, OccurrenceTarget::Resolved { .. }))
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|g| g.domain == domain && g.detail.starts_with("less-native-parse-recovery-"))
        );
    }
}

#[test]
fn required_less_context_survives_exact_admission_and_artifacts_rebind_real_sources() {
    use rootlight_vfs::SourceSnapshot;

    struct Fixture {
        _directory: tempfile::TempDir,
        snapshot: SourceSnapshot,
        source: SourceRef,
    }
    impl Fixture {
        fn new(raw: &[u8], generation: u8) -> Self {
            let directory = tempfile::tempdir().unwrap();
            fs::write(directory.path().join("size.less"), raw).unwrap();
            let repository = derive_repository(b"less-artifact-source").id();
            let root = RepositoryRoot::open(repository, directory.path()).unwrap();
            let snapshot = root
                .snapshot(
                    &RelativePath::parse(Path::new("size.less")).unwrap(),
                    262_144,
                )
                .unwrap();
            let source = SourceRef::new(
                repository,
                GenerationId::from_bytes([generation; 20]),
                SourceSpan::new(snapshot.file(), 0, raw.len() as u64).unwrap(),
                snapshot.content_hash(),
                None,
            );
            Self {
                _directory: directory,
                snapshot,
                source,
            }
        }
        fn request<'a>(&'a self, limits: &'a AnalysisLimits) -> AnalysisRequest<'a> {
            AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
                LanguageId::new("less").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"less-artifact-build")),
                limits,
            )
            .unwrap()
            .with_generated_status(false)
        }
    }
    let limits = |syntax: usize| {
        let stream = |maximum| {
            StreamLimits::new(
                128,
                maximum,
                16 * 1024 * 1024,
                128,
                128 * 1024,
                1024 * 1024,
                BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap(),
            )
            .unwrap()
        };
        AnalysisLimits::new(
            262_144,
            16_384,
            128,
            1,
            16 * 1024 * 1024,
            stream(syntax),
            stream(4096),
            IrLimits::default(),
        )
        .unwrap()
    };
    let provider = Arc::new(LessParseProvider::new(262_144, 16_384).unwrap());
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "less-artifact-source",
            "1.0",
            content_hash(b"less-artifact-config"),
        )
        .unwrap(),
        LanguageId::new("less").unwrap(),
        "raffia-less-source-contract",
        LessParseProvider::binary_digest(),
    )
    .unwrap();
    let first = Fixture::new(original(), 1);
    let next = Fixture::new(original(), 2);
    let changed = Fixture::new(
        std::str::from_utf8(original())
            .unwrap()
            .replace("width: @width;", "width: 20px;")
            .as_bytes(),
        2,
    );
    let defaults = limits(4096);
    let request = first.request(&defaults);
    let (baseline, artifact) = analyzer
        .analyze_and_capture(
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    original_assertions(baseline.document(), |n| n);
    let required = provider
        .required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())
        .unwrap();
    assert_eq!(
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap(),
        required
    );
    assert!(required > 0);
    let exact = limits(required);
    let bounded = analyzer
        .prepare_artifact(&first.request(&exact), &Cancellation::new())
        .unwrap();
    assert_eq!(
        bounded
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap(),
        required
    );
    let short = limits(required - 1);
    let refusal = analyzer
        .prepare_artifact(&first.request(&short), &Cancellation::new())
        .expect_err("required Less context cannot be silently dropped");
    assert_eq!(
        refusal,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed: required,
            limit: required - 1,
        })
    );
    let rebound = next.request(&defaults);
    let reused = analyzer
        .analyze_from_artifact(
            &rebound,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    let fresh = execute_analysis(
        &analyzer,
        &rebound,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    assert_eq!(reused.document(), fresh.document());
    original_assertions(reused.document(), |n| n);
    assert!(
        reused
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == next.source.generation()
                && site.source.content_hash() == next.source.content_hash())
    );
    let stale = analyzer.analyze_from_artifact(
        &changed.request(&defaults),
        &artifact,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    );
    assert!(
        matches!(stale,Err(AdapterError::ProviderFailed{code}) if code.as_str()=="treesitter-structural-artifact-mismatch")
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_from_artifact(
            &rebound,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let retry = analyzer
        .analyze_from_artifact(
            &rebound,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(retry.document(), fresh.document());
}
