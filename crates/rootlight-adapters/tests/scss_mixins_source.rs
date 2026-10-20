//! SCSS controls exercise the existing source scanner and owned mixin proofs.
//! Binding is local to clean source-order scopes; unsupported compiler effects
//! and malformed/over-budget analysis cannot manufacture a resolved target.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::ScssParseProvider;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    RelationEndpoint, RelationPredicate, SourceRef, SourceSpan, validate_ir_document,
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
        &ScssParseProvider,
        &AnalysisRequest<'_>,
    ) -> Result<T, AdapterError>,
) -> Result<T, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("sample.scss"), source).unwrap();
    let repository = derive_repository(b"scss-native-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("sample.scss")).unwrap(),
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
    let provider = Arc::new(ScssParseProvider::new(262_144, nodes).unwrap());
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "scss-source-contract",
            "1.0",
            content_hash(b"scss-source-config"),
        )
        .unwrap(),
        LanguageId::new("scss").unwrap(),
        "existing-scss-source-contract",
        ScssParseProvider::binary_digest(),
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
        LanguageId::new("scss").unwrap(),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"scss-source-build")),
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

fn call_at<'a>(
    document: &'a NormalizedIrDocument,
    start: usize,
    name: &str,
) -> &'a OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == (start + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact include at {start}");
    sites[0]
}

#[test]
fn simple_mixin_and_function_formals_have_exact_owners_without_sigil_definitions() {
    let source = "@mixin shape($width, $enabled: false) { color: red; }\n@function scale($value: 2) { @return 2; }\n";
    let output = successful(source);
    let document = output.document();
    for (owner, name) in [("shape", "width"), ("shape", "enabled"), ("scale", "value")] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == name)
            .unwrap();
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == owner)
            .unwrap();
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
        let start = source.find(&format!("${name}")).unwrap() + 1;
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            (
                definitions[0].source.span().start_byte(),
                definitions[0].source.span().end_byte()
            ),
            (start as u64, (start + name.len()) as u64)
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(entity.id))
                .count(),
            1
        );
    }
}

#[test]
fn clean_earlier_file_mixins_resolve_empty_includes_and_identifier_equivalence() {
    for target in ["round_edges", "round-edges"] {
        let source = format!(
            "@mixin {target}() {{ color: red; }}\n@mixin caller($enabled: false) {{ @include round-edges(); }}\n"
        );
        let output = successful(&source);
        let document = output.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == target)
            .unwrap();
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "caller")
            .unwrap();
        let site = call_at(
            document,
            source.rfind("round-edges").unwrap(),
            "round-edges",
        );
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.enclosing, Some(owner.id));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Calls
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(target.id)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.source, Some(site.source.clone()));
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "scss-mixin-target-unavailable")
        );
    }
}

#[test]
fn module_redefinition_conditional_namespace_and_dynamic_boundaries_withhold_targets() {
    for source in [
        "@use 'theme'; @mixin target() {} @mixin caller() { @include target(); }",
        "@forward 'theme'; @mixin target() {} @mixin caller() { @include target(); }",
        "@import 'theme'; @mixin target() {} @mixin caller() { @include target(); }",
        "@mixin target() {} @mixin target() {} @mixin caller() { @include target(); }",
        "@mixin tar_get() {} @mixin tar-get() {} @mixin caller() { @include tar_get(); }",
        "@mixin caller() { @include target(); } @mixin target() {}",
        "@mixin target() {} @if true { @mixin other() {} } @mixin caller() { @include target(); }",
        "@mixin target() {} @mixin caller() { @if true { @include target(); } }",
        "@mixin target() {} @mixin caller() { .nested { @include target(); } }",
        "@mixin target() {} @mixin caller() { @include theme.target(); }",
        "@mixin target() {} @mixin caller() { @include target() { color: red; } }",
        "@mixin target($value) {} @mixin caller() { @include target(2); }",
        "@function target() { @return 1; } @mixin caller() { @include target(); }",
        "@mixin target() {} @mixin caller() { @include TARGET(); }",
        "@mixin target() {} @mixin caller() { @include target(); } .#{dynamic} { color: red; }",
        "@mixin target() {} @mixin caller() { @include target(); } /* unfinished",
        "@mixin target() {} @mixin caller() { @include target(); } .broken { content: 'unfinished; }",
        "@mixin target() {} @mixin caller() { @include target(); } .broken { value: ([)]; }",
    ] {
        let output = successful(source);
        let document = output.document();
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.syntax_kind == "scss.mixin.call"
            })
            .collect();
        assert!(
            !calls.is_empty(),
            "written call remains available: {source}"
        );
        for site in calls {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{source}"
            );
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "scss-mixin-target-unavailable")
            );
        }
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
}

#[test]
fn complex_or_duplicate_formal_lists_remain_explicit_gaps_without_partial_parameters() {
    for source in [
        "@mixin caller($first, $rest...) { color: red; }",
        "@mixin caller($a_b, $a-b) { color: red; }",
        "@mixin caller($value: variable(2)) { color: red; }",
        "@mixin caller($value: 1.2.3) { color: red; }",
        "@mixin caller($value: $external) { color: red; }",
    ] {
        let output = successful(source);
        let document = output.document();
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter),
            "{source}"
        );
        for reason in [
            "scss-complex-formal-declaration-unavailable",
            "scss-complex-formal-definition-unavailable",
            "scss-complex-formal-owner-unavailable",
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == reason),
                "{source}: {reason}"
            );
        }
    }
}

#[test]
fn quoted_or_commented_directives_do_not_poison_a_clean_file_environment() {
    let source = "/* @use 'ghost'; @mixin target() {} */\n@mixin target() { content: '#{literal} @import ghost'; }\n@mixin caller() { @include target(); }\n";
    let output = successful(source);
    assert!(matches!(
        call_at(
            output.document(),
            source.rfind("target()").unwrap(),
            "target"
        )
        .target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        2
    );
}

#[test]
fn utf16_both_orders_bind_physical_formals_calls_and_original_hash() {
    let source = "// λ\r\n@mixin target() { color: red; }\r\n@mixin caller($flag: false) { @include target(); }\r\n";
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
        let output = analyze_raw(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            IrLimits::default(),
            16_384,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        let document = output.document();
        assert_eq!(document.files[0].content_hash, content_hash(&raw));
        let start = 2 + source[..source.rfind("target()").unwrap()]
            .encode_utf16()
            .count()
            * 2;
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite
                    && site.source.span().start_byte() == start as u64
                    && site.source.span().end_byte() == (start + 12) as u64
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(matches!(sites[0].target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(sites[0].source.content_hash(), content_hash(&raw));
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "flag")
            .unwrap();
        let start = 2 + source[..source.find("flag").unwrap()]
            .encode_utf16()
            .count()
            * 2;
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            (
                definitions[0].source.span().start_byte(),
                definitions[0].source.span().end_byte()
            ),
            (start as u64, (start + 8) as u64)
        );
    }
}

#[test]
fn node_depth_and_cancellation_fail_without_partial_certificates() {
    let source = "@mixin target() {} @mixin caller($value: false) { @include target(); }";
    assert!(
        matches!(analyze(source, 3, 128, &Cancellation::new()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "scss-syntax-node-limit")
    );
    assert!(
        matches!(analyze(source, 16_384, 1, &Cancellation::new()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "scss-syntax-depth-limit")
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze(source, 16_384, 128, &cancel),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn utf8_bom_crlf_and_body_edits_preserve_callable_identity_without_marker_signatures() {
    let mut previous = None;
    for body in ["color: red;", "border-width: 2px;"] {
        let source = format!(
            "\u{feff}@mixin target() {{ {body} }}\r\n@mixin caller($flag: false) {{ @include target(); }}\r\n"
        );
        let output = successful(&source);
        let document = output.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "target")
            .unwrap();
        let caller = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "caller")
            .unwrap();
        let site = call_at(document, source.rfind("target()").unwrap(), "target");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        let identity = (target.id, caller.id);
        let hash = document.files[0].content_hash;
        if let Some((old_identity, old_hash)) = previous {
            assert_eq!(identity, old_identity);
            assert_ne!(hash, old_hash);
        }
        previous = Some((identity, hash));
    }
}

#[test]
fn exact_preflight_and_prepared_artifact_reuse_bind_the_actual_current_source() {
    let source = b"@mixin target() {} @mixin caller($value: false) { @include target(); }\n";
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

        let edited = b"@mixin target() { color: red; } @mixin caller($value: false) { @include target(); }\n";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("sample.scss"), edited).unwrap();
        let repository = request.source().source_ref().repository();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root.snapshot(&RelativePath::parse(Path::new("sample.scss")).unwrap(), 262_144).unwrap();
        let reference = SourceRef::new(repository, GenerationId::from_bytes([32;20]),
            SourceSpan::new(snapshot.file(), 0, edited.len() as u64).unwrap(), snapshot.content_hash(), None);
        let next = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &reference).unwrap(), LanguageId::new("scss").unwrap(),
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
    let source = b"@mixin target() {} @mixin caller($value: false) { @include target(); }\n";
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

fn call_proof_nested_boundary(per_record: bool) {
    let source = b"@mixin target() {} @mixin caller($value: false) { @include target(); }";
    // Four mandatory identity envelopes cover the file and three declarations.
    // Excluding optional lexical envelopes isolates the required call proof.
    let run = |mut ir: IrLimits| {
        ir.max_extensions = 4;
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
    assert_eq!(baseline.document().entities.len(), 3);
    assert_eq!(baseline.document().extensions.len(), 4);
    assert_eq!(
        baseline
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    let usage =
        rootlight_ir::IrDocumentUsage::measure(baseline.document(), &IrLimits::default()).unwrap();
    let mut without_calls = baseline.document().clone();
    without_calls
        .relations
        .retain(|edge| edge.predicate != RelationPredicate::Calls);
    let prior =
        rootlight_ir::IrDocumentUsage::measure(&without_calls, &IrLimits::default()).unwrap();
    assert_eq!(usage.total_nested_items(), prior.total_nested_items() + 3);
    let exact = if per_record {
        3
    } else {
        usage.total_nested_items()
    };
    for (limit, admitted) in [(exact, true), (exact - 1, false)] {
        let mut ir = IrLimits::default();
        if per_record {
            ir.max_nested_items_per_record = limit;
        } else {
            ir.max_total_nested_items = limit;
        }
        match (admitted, run(ir)) {
            (true, Ok(output)) => assert_eq!(output.document(), baseline.document()),
            (
                false,
                Err(AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::NestedItems,
                    observed,
                    limit: actual,
                })),
            ) => {
                assert_eq!(actual, limit);
                assert_eq!(observed, exact);
            }
            result => panic!("unexpected nested proof admission: {result:?}"),
        }
    }
}

#[test]
fn complete_public_call_proof_survives_exact_total_nested_items_and_rejects_one_less() {
    call_proof_nested_boundary(false);
}

#[test]
fn complete_public_call_proof_survives_exact_per_record_nested_items_and_rejects_one_less() {
    call_proof_nested_boundary(true);
}

#[test]
fn actual_producer_graph_contains_each_fact_and_resolves_a_cross_declaration_witness() {
    let source = b"@mixin target() {} @mixin caller($value: false) { @include target(); }\n";
    with_case(
        source,
        EncodingId::utf8(),
        IrLimits::default(),
        16_384,
        128,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let parsed = rootlight_adapter_sdk::execute_parse(
                provider,
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let facts: std::collections::BTreeMap<_, _> = parsed
                .facts()
                .iter()
                .map(|fact| (fact.local_id(), fact))
                .collect();
            assert_eq!(facts.len(), parsed.facts().len());
            for fact in parsed.facts() {
                assert_eq!(
                    fact.span().file(),
                    request.source().source_ref().span().file()
                );
                assert!(fact.span().end_byte() <= source.len() as u64);
                if let Some(parent_id) = fact.parent() {
                    let parent = facts.get(&parent_id).expect("every producer parent exists");
                    assert!(
                        parent.depth() < fact.depth(),
                        "{}",
                        fact.syntax_kind().as_str()
                    );
                    assert!(
                        parent.span().start_byte() <= fact.span().start_byte(),
                        "{}",
                        fact.syntax_kind().as_str()
                    );
                    assert!(
                        parent.span().end_byte() >= fact.span().end_byte(),
                        "{}",
                        fact.syntax_kind().as_str()
                    );
                }
            }
            let text = std::str::from_utf8(source).unwrap();
            let caller_start = text.find("@mixin caller").unwrap();
            let call_start = text.rfind("target").unwrap();
            let markers: Vec<_> = parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "scss.mixin_target.signature")
                .collect();
            assert_eq!(markers.len(), 1);
            assert_eq!(markers[0].span().start_byte(), 0);
            assert_eq!(markers[0].span().end_byte(), (call_start + 6) as u64);
            assert!(markers[0].span().start_byte() < caller_start as u64);
            let output = execute_analysis(
                analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            validate_ir_document(
                output.document(),
                request.limits().ir(),
                &ExtensionSupport::default(),
            )
            .unwrap();
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "target")
                .unwrap();
            let caller = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "caller")
                .unwrap();
            let call = call_at(output.document(), call_start, "target");
            assert_eq!(call.enclosing, Some(caller.id));
            assert_eq!(
                call.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert_eq!(call.source.content_hash(), content_hash(source));
            assert_eq!(
                output
                    .document()
                    .relations
                    .iter()
                    .filter(|edge| edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Entity(caller.id)
                        && edge.object == RelationEndpoint::Entity(target.id))
                    .count(),
                1
            );
            Ok(())
        },
    )
    .unwrap();
}
