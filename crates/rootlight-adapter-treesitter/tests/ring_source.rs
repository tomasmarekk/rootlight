//! Written Ring class methods and formal Reads require native ownership.
//! Dynamic keyword remapping, DSL and caller-scope evaluation remain unproved.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/ring/htmlfunctions.ring");

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    execute(raw, encoding, 4096).unwrap().0
}

fn execute(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    execute_control(raw, encoding, maximum, &Cancellation::new(), None)
}

fn execute_control(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    cancellation: &Cancellation,
    included_start: Option<u64>,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.ring"), raw).unwrap();
    let repository = derive_repository(b"ring-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.ring")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "ring-source-contract",
            "1.0",
            content_hash(b"ring-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("ring").unwrap(),
        "ring-static-contract",
        content_hash(b"ring-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        maximum,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream,
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::new(encoding).unwrap(),
        included_start
            .map(|start| {
                vec![rootlight_adapter_sdk::IncludedRange::new(
                    SourceSpan::new(snapshot.file(), start, u64::try_from(raw.len()).unwrap())
                        .unwrap(),
                    LanguageId::new("ring").unwrap(),
                )]
            })
            .unwrap_or_default(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required =
        provider.required_syntax_fact_count(&request.to_parse_request(), cancellation)?;
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancellation,
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    Ok((result, required))
}

fn entity_at(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: usize,
    length: usize,
) -> rootlight_ids::SymbolId {
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                && occurrence.source.span().end_byte() == u64::try_from(start + length).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        definitions.len(),
        1,
        "{name}: {:?}",
        output.document().occurrences
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must be resolved")
    };
    let entity = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.display_name, name);
    assert_eq!(
        definitions[0].syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    symbol
}

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    member: rootlight_ids::SymbolId,
    name_start: usize,
    name_end: usize,
) {
    let matches = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1);
    let relation = matches[0];
    let reference = relation.evidence.source.as_ref().unwrap();
    assert!(reference.span().start_byte() <= u64::try_from(name_start).unwrap());
    assert!(reference.span().end_byte() >= u64::try_from(name_end).unwrap());
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn original_ring_class_method_parameter_and_read_keep_exact_source_ownership() {
    assert_eq!(SOURCE.len(), 1778);
    let output = analyze(SOURCE);
    let class = entity_at(&output, "HTMLFunctions", EntityKind::Class, 209, 13);
    let method = entity_at(&output, "HTMLSpecialChars", EntityKind::Method, 383, 16);
    let parameter = entity_at(&output, "cStr", EntityKind::Parameter, 400, 4);
    contains(&output, class, method, 383, 399);
    contains(&output, method, parameter, 400, 404);
    assert!(!output.document().relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(parameter)
            && edge.object == RelationEndpoint::Entity(method)
    }));
    let reads: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 439
                && site.source.span().end_byte() == 443
        })
        .collect();
    assert_eq!(reads.len(), 1, "{:#?}", output.document().occurrences);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(reads[0].enclosing, Some(method));
    assert_eq!(
        reads[0].source.content_hash(),
        content_hash(SOURCE.as_bytes())
    );
    assert_eq!(&SOURCE.as_bytes()[439..443], b"cStr");
    assert!(!output.document().occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition && site.source.span() == reads[0].source.span()
    }));
}

#[test]
fn case_folded_formals_remain_owned_by_their_exact_method() {
    let source = "class Written\nfunc first value { consume(VALUE) }\nfunc second value { consume(value) }\n";
    let output = analyze(source);
    for (method_name, read_name) in [("first", "VALUE"), ("second", "value")] {
        let start = source.find(method_name).unwrap();
        let method = entity_at(
            &output,
            method_name,
            EntityKind::Method,
            start,
            method_name.len(),
        );
        let parameter_start = start + method_name.len() + 1;
        let parameter = entity_at(&output, "value", EntityKind::Parameter, parameter_start, 5);
        let read_start = source[start..]
            .find(&format!("consume({read_name})"))
            .unwrap()
            + start
            + 8;
        let read = output
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == u64::try_from(read_start).unwrap()
            })
            .unwrap();
        assert_eq!(read.enclosing, Some(method));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved { symbol: parameter }
        );
    }
}

#[test]
fn duplicate_formals_unknown_effects_object_context_and_other_method_names_do_not_bind() {
    for source in [
        "class Written\nfunc measure value,VALUE { consume(value) }\n",
        "class Written\nfunc measure value { eval(\"value=1\") consume(value) }\n",
        "class Written\nfunc measure value { prior() consume(value) }\n",
        "class Written\nfunc measure value { value=1 consume(value) }\n",
        "class Written\nfunc measure value { object { consume(value) } }\n",
        "class Written\nfunc measure value { text=\"#{prior()}\" consume(value) }\n",
        "class Written\nfunc measure value { text='#{prior()}' consume(value) }\n",
        "class Written\nfunc measure value { text=`#{prior()}` consume(value) }\n",
        "class Written\nfunc first value { return value }\nfunc second other { consume(value) }\n",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "ring-native-formal-binding-unavailable"
                    || gap.detail == "ring-source-construct-semantics-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn multiple_classes_remapping_packages_and_recovery_never_create_cst_owned_definitions() {
    for source in [
        "class First\nfunc measure value { consume(value) }\nclass Second\nfunc measure value { consume(value) }\n",
        "ChangeRingKeyword func task\nclass Written\ntask measure value { consume(value) }\n",
        "package Example\nclass Written\nfunc measure value { consume(value) }\n",
        "class Written\nfunc measure(value, { consume(value) }\n",
    ] {
        let output = analyze(source);
        assert!(
            !output.document().entities.iter().any(|entity| matches!(
                entity.kind,
                EntityKind::Class | EntityKind::Method | EntityKind::Parameter
            )),
            "{source}"
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition),
            "{source}"
        );
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain),
                "{source} {domain:?}"
            );
        }
    }
    let source = "class Written\n# class Ghost func phantom value\nfunc measure value { text=\"func phantom value\" consume(value) }\n";
    let output = analyze(source);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| ["Ghost", "phantom"].contains(&entity.canonical_name.as_str()))
    );
    assert!(
        output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "measure")
    );
}

#[test]
fn utf8_bom_and_utf16_both_orders_preserve_physical_formal_and_read_spans() {
    let written = "# 雪\r\nclass Written\r\nfunc measure value { consume(VALUE) }\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), written.as_bytes()].concat()
        } else {
            let mut bytes = if encoding == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in written.encode_utf16() {
                bytes.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        };
        let output = analyze_raw(&raw, encoding);
        let parameter = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter)
            .unwrap();
        let read = output
            .document()
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::Read)
            .unwrap();
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        assert_eq!(read.source.content_hash(), content_hash(&raw));
        let start = usize::try_from(read.source.span().start_byte()).unwrap();
        let end = usize::try_from(read.source.span().end_byte()).unwrap();
        let expected = if encoding == "utf-8" {
            b"VALUE".to_vec()
        } else {
            "VALUE"
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        };
        assert_eq!(&raw[start..end], expected);
        assert_eq!(read.syntactic_text_hash, content_hash(b"VALUE"));
    }
}

#[test]
fn required_closure_limits_cancellation_and_partial_source_cannot_publish_certificate() {
    use rootlight_cancel::CancellationReason;
    let source = b"class Written\nfunc measure value { consume(value) }\n";
    let (_, required) = execute(source, "utf-8", 4096).unwrap();
    assert!(required > 1);
    let accepted = execute(source, "utf-8", required);
    assert!(accepted.is_ok(), "{accepted:?}");
    assert!(execute(source, "utf-8", required - 1).is_err());
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        execute_control(source, "utf-8", 4096, &cancellation, None),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let output = execute_control(source, "utf-8", 4096, &Cancellation::new(), Some(14))
        .unwrap()
        .0;
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "ring.parameter_read.reference")
    );
}

#[test]
fn body_edits_keep_written_owner_ids_and_refresh_exact_source_hashes() {
    let original = "class Written\nfunc measure value { consume(value) return 1 }\n";
    let changed = original.replace("return 1", "return 123456");
    let first = analyze(original);
    let second = analyze(&changed);
    for name in ["Written", "measure", "value"] {
        let before = first
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let after = second
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(before.id, after.id, "{name}");
        assert_eq!(
            before.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(original.as_bytes())
        );
        assert_eq!(
            after.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(changed.as_bytes())
        );
    }
    let read = second
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Read)
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Resolved { .. }));
    assert_eq!(read.source.content_hash(), content_hash(changed.as_bytes()));
}

#[test]
fn ring_native_artifact_reuse_reconstructs_required_read_demand_and_current_generation() {
    let raw = SOURCE.as_bytes();
    let encoding = "utf-8";
    let maximum = 4096_usize;
    let included_start: Option<u64> = None;
    let cancellation = Cancellation::new();
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.ring"), raw).unwrap();
    let repository = derive_repository(b"ring-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.ring")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "ring-source-contract",
            "1.0",
            content_hash(b"ring-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("ring").unwrap(),
        "ring-static-contract",
        content_hash(b"ring-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        maximum,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream,
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::new(encoding).unwrap(),
        included_start
            .map(|start| {
                vec![rootlight_adapter_sdk::IncludedRange::new(
                    SourceSpan::new(snapshot.file(), start, u64::try_from(raw.len()).unwrap())
                        .unwrap(),
                    LanguageId::new("ring").unwrap(),
                )]
            })
            .unwrap_or_default(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);

    let (first, artifact) = analyzer
        .analyze_and_capture(
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&cancellation).unwrap(),
        provider
            .required_syntax_fact_count(&request.to_parse_request(), &cancellation)
            .unwrap()
    );
    let successor = SourceRef::new(
        source.repository(),
        GenerationId::from_bytes([10; 20]),
        source.span(),
        source.content_hash(),
        None,
    );
    let next = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &successor).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let reused = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    let fresh = execute_analysis(
        &analyzer,
        &next,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    assert_eq!(reused.document(), fresh.document());
    assert_eq!(reused.report(), fresh.report());
    let select = |output: &rootlight_adapter_sdk::AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(select(&first), select(&reused));
    assert!(
        reused
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == successor.generation())
    );
    let reads: Vec<_> = reused
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == 439)
        .collect();
    assert_eq!(reads.len(), 1);
    assert!(matches!(reads[0].target, OccurrenceTarget::Resolved { .. }));
    let prepared = analyzer.prepare_artifact(&request, &cancellation).unwrap();
    let classified = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(true);
    let classified_reused = analyzer
        .analyze_prepared_artifact(
            &classified,
            &prepared,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    let classified_fresh = execute_analysis(
        &analyzer,
        &classified,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    assert_eq!(classified_reused.document(), classified_fresh.document());
    assert!(classified_reused.document().files[0].generated);
    let changed_build = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"different-ring-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let changed_reused = analyzer
        .analyze_prepared_artifact(
            &changed_build,
            &prepared,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    let changed_fresh = execute_analysis(
        &analyzer,
        &changed_build,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    assert_eq!(changed_reused.document(), changed_fresh.document());
    assert!(
        changed_reused
            .document()
            .provenance
            .iter()
            .all(|record| record.build_context == changed_build.build_context())
    );
    let partitioned = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::utf8(),
        vec![rootlight_adapter_sdk::IncludedRange::new(
            source.span(),
            LanguageId::new("ring").unwrap(),
        )],
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    assert!(
        matches!(analyzer.analyze_prepared_artifact(&partitioned, &prepared, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancellation), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_prepared_artifact(
            &request,
            &prepared,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn ring_prepared_artifact_rejects_changed_body_and_fresh_analysis_keeps_ids_and_exact_read() {
    let raw = SOURCE.as_bytes();
    let encoding = "utf-8";
    let maximum = 4096_usize;
    let included_start: Option<u64> = None;
    let cancellation = Cancellation::new();
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.ring"), raw).unwrap();
    let repository = derive_repository(b"ring-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.ring")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "ring-source-contract",
            "1.0",
            content_hash(b"ring-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("ring").unwrap(),
        "ring-static-contract",
        content_hash(b"ring-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        maximum,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream,
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::new(encoding).unwrap(),
        included_start
            .map(|start| {
                vec![rootlight_adapter_sdk::IncludedRange::new(
                    SourceSpan::new(snapshot.file(), start, u64::try_from(raw.len()).unwrap())
                        .unwrap(),
                    LanguageId::new("ring").unwrap(),
                )]
            })
            .unwrap_or_default(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);

    let before = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    let prepared = analyzer.prepare_artifact(&request, &cancellation).unwrap();
    let changed = SOURCE.replacen("cResult = \"\"", "cResult = 10", 1);
    assert_ne!(changed, SOURCE);
    assert_eq!(changed.len(), SOURCE.len());
    let next_directory = tempdir().unwrap();
    fs::write(
        next_directory.path().join("source.ring"),
        changed.as_bytes(),
    )
    .unwrap();
    let next_root = RepositoryRoot::open(repository, next_directory.path()).unwrap();
    let next_snapshot = next_root
        .snapshot(
            &RelativePath::parse(Path::new("source.ring")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    assert_eq!(next_snapshot.file(), snapshot.file());
    assert_ne!(next_snapshot.content_hash(), snapshot.content_hash());
    let next_source = SourceRef::new(
        repository,
        GenerationId::from_bytes([11; 20]),
        SourceSpan::new(
            next_snapshot.file(),
            0,
            u64::try_from(changed.len()).unwrap(),
        )
        .unwrap(),
        next_snapshot.content_hash(),
        None,
    );
    let next = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&next_snapshot, &next_source).unwrap(),
        LanguageId::new("ring").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        request.build_context(),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    assert!(
        matches!(analyzer.analyze_prepared_artifact(&next,&prepared,ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,&cancellation),
        Err(AdapterError::ProviderFailed { code }) if code.as_str()=="treesitter-structural-artifact-mismatch")
    );
    let after = execute_analysis(
        &analyzer,
        &next,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    for (name, kind, start, length) in [
        ("HTMLFunctions", EntityKind::Class, 209, 13),
        ("HTMLSpecialChars", EntityKind::Method, 383, 16),
        ("cStr", EntityKind::Parameter, 400, 4),
    ] {
        assert_eq!(
            entity_at(&before, name, kind, start, length),
            entity_at(&after, name, kind, start, length)
        );
    }
    let parameter = entity_at(&after, "cStr", EntityKind::Parameter, 400, 4);
    let read = after
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == 439)
        .unwrap();
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(read.source.span().end_byte(), 443);
    assert_eq!(read.source.generation(), next_source.generation());
    assert_eq!(read.source.content_hash(), content_hash(changed.as_bytes()));
    assert_eq!(&changed.as_bytes()[439..443], b"cStr");
    assert!(
        after
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == next_source.generation()
                && site.source.content_hash() == next_source.content_hash())
    );
}

// Exercise actual Ring IR admission without replacing native proof with a count.
// This append-only test fragment uses the existing original public fixture helpers.

fn checked_ring_quota_output(output: &rootlight_adapter_sdk::AnalysisOutput) {
    let class = entity_at(output, "HTMLFunctions", EntityKind::Class, 209, 13);
    let method = entity_at(output, "HTMLSpecialChars", EntityKind::Method, 383, 16);
    let parameter = entity_at(output, "cStr", EntityKind::Parameter, 400, 4);
    contains(output, class, method, 383, 399);
    contains(output, method, parameter, 400, 404);
    let reads: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 439
                && site.source.span().end_byte() == 443
        })
        .collect();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(read.enclosing, Some(method));
    assert_eq!(read.source.content_hash(), content_hash(SOURCE.as_bytes()));
    assert_eq!(read.syntactic_text_hash, content_hash(b"cStr"));
    assert!(!output.document().occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition && site.source.span() == read.source.span()
    }));
    for (owner, member) in [(class, method), (method, parameter)] {
        let edges: Vec<_> = output
            .document()
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(member)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        let edge = edges[0];
        let reference = edge.evidence.source.as_ref().unwrap();
        assert_eq!(reference.repository(), read.source.repository());
        assert_eq!(reference.generation(), read.source.generation());
        assert_eq!(reference.span().file(), read.source.span().file());
        assert_eq!(reference.content_hash(), read.source.content_hash());
        assert_eq!(edge.provenance, read.provenance);
        assert_eq!(edge.id, derive_relation_record_id(edge).unwrap());
    }
    let provenance = output
        .document()
        .provenance
        .iter()
        .find(|item| item.id == read.provenance)
        .unwrap();
    assert_eq!(provenance.language, "ring");
    assert_eq!(provenance.repository, read.source.repository());
    assert_eq!(provenance.generation, read.source.generation());
    assert!(provenance.input_sources.iter().any(|source| {
        source.span().file() == read.source.span().file()
            && source.content_hash() == read.source.content_hash()
            && source.repository() == read.source.repository()
            && source.generation() == read.source.generation()
    }));
    assert!(output.document().skipped_regions.iter().any(|gap| {
        gap.domain == rootlight_ir::FactDomain::Relations
            && gap.detail
                == "ring-general-binding-dsl-keyword-remapping-inheritance-and-runtime-unavailable"
            && gap.source.span().file() == read.source.span().file()
            && gap.source.content_hash() == read.source.content_hash()
    }));
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
}

fn ring_ir_capacity_boundary(total_records: bool) {
    use rootlight_adapter_sdk::{ResourceKind, SinkError};

    assert_eq!(SOURCE.len(), 1778);
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.ring"), SOURCE.as_bytes()).unwrap();
    let repository = derive_repository(b"ring-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.ring")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(SOURCE.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "ring-source-contract",
            "1.0",
            content_hash(b"ring-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("ring").unwrap(),
        "ring-static-contract",
        content_hash(b"ring-contract-binary"),
    )
    .unwrap();
    let execute = |ir: IrLimits| {
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
            1024 * 1024,
            16_384,
            128,
            16,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("ring").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"ring-contract-build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )?;
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        Ok::<_, AdapterError>(output)
    };
    let baseline = execute(IrLimits::default()).unwrap();
    checked_ring_quota_output(&baseline);
    let census =
        rootlight_ir::IrDocumentUsage::measure(baseline.document(), &IrLimits::default()).unwrap();
    let emitted = if total_records {
        census.total_records()
    } else {
        baseline.document().relations.len()
    };
    assert!(emitted > 1);
    let select = |maximum| {
        let mut ir = IrLimits::default();
        if total_records {
            ir.max_total_records = maximum;
        } else {
            ir.max_relations = maximum;
        }
        ir
    };
    // Calibrate the real native-lowering admission boundary through the public
    // transaction, independently of the actual emitted-document census above.
    let refusal = execute(select(0)).unwrap_err();
    let AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::Records,
        observed,
        limit: 0,
    }) = refusal
    else {
        panic!("expected selected IR record refusal, got {refusal:?}")
    };
    assert!(
        observed >= emitted,
        "reserved={observed}, emitted={emitted}"
    );
    let exact = execute(select(observed)).unwrap();
    checked_ring_quota_output(&exact);
    assert_eq!(exact.document(), baseline.document());
    let one_less = execute(select(observed - 1));
    assert_eq!(
        one_less.unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed,
            limit: observed - 1,
        })
    );
    // The failed transaction exposes no AnalysisOutput; retry the same provider
    // and immutable source to catch leaked state or damaged native proof reuse.
    let retry = execute(select(observed)).unwrap();
    checked_ring_quota_output(&retry);
    assert_eq!(retry.document(), baseline.document());
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn ring_relation_limit_exact_and_one_less_keep_original_native_read_proof() {
    ring_ir_capacity_boundary(false);
}

#[test]
fn ring_total_record_limit_exact_and_one_less_keep_original_native_read_proof() {
    ring_ir_capacity_boundary(true);
}

#[test]
fn original_ring_top_level_functions_keep_exact_unbraced_and_braced_definitions() {
    for (raw, expected) in [
        (
            include_bytes!("fixtures/ring/top-level/test16.ring").as_slice(),
            vec![("hello", 5, 10)],
        ),
        (
            include_bytes!("fixtures/ring/top-level/test4.ring").as_slice(),
            vec![("main", 5, 9), ("one", 64, 67)],
        ),
    ] {
        let source = std::str::from_utf8(raw).unwrap();
        assert_eq!(raw.len(), if expected.len() == 1 { 53 } else { 83 });
        let output = analyze(source);
        let module = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module)
            .unwrap();
        assert_eq!(
            output
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Function)
                .count(),
            expected.len()
        );
        for (name, start, end) in expected {
            assert_eq!(&raw[start..end], name.as_bytes());
            let function = entity_at(&output, name, EntityKind::Function, start, end - start);
            contains(&output, module.id, function, start, end);
        }
        assert!(!output.document().entities.iter().any(|entity| matches!(
            entity.kind,
            EntityKind::Class | EntityKind::Method | EntityKind::Parameter
        )));
        assert!(output.document().skipped_regions.iter().any(|gap| gap.detail == "ring-general-binding-dsl-keyword-remapping-inheritance-and-runtime-unavailable"));
    }
}

#[test]
fn ordinary_function_formals_keep_native_owners_beside_unrelated_loads_and_statements() {
    let source = "load \"unproved.ring\"\n? \"init\"\nfunc first(value, other) { return value }\nfunc second value { load \"body.ring\" return value }\n";
    let output = analyze(source);
    for (name, parameters) in [("first", vec!["value", "other"]), ("second", vec!["value"])] {
        let start = source.find(name).unwrap();
        let function = entity_at(&output, name, EntityKind::Function, start, name.len());
        let body = source[start..].find('{').unwrap() + start;
        for parameter_name in parameters {
            let parameter_start = source[start..body].find(parameter_name).unwrap() + start;
            let parameter = entity_at(
                &output,
                parameter_name,
                EntityKind::Parameter,
                parameter_start,
                parameter_name.len(),
            );
            contains(
                &output,
                function,
                parameter,
                parameter_start,
                parameter_start + parameter_name.len(),
            );
            let entity = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.id == parameter)
                .unwrap();
            assert_eq!(
                entity.container,
                Some(rootlight_ir::ContainerRef::Entity(function))
            );
        }
    }
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "ring-source-construct-semantics-unavailable")
    );
}

#[test]
fn top_level_structural_headers_do_not_supply_method_read_certificates() {
    let source = "func measure value { consume(value) }\n";
    let output = analyze(source);
    let function = entity_at(&output, "measure", EntityKind::Function, 5, 7);
    let parameter = entity_at(&output, "value", EntityKind::Parameter, 13, 5);
    contains(&output, function, parameter, 13, 18);
    let reads: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Read)
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        &source.as_bytes()[usize::try_from(reads[0].source.span().start_byte()).unwrap()
            ..usize::try_from(reads[0].source.span().end_byte()).unwrap()],
        b"value"
    );
    assert!(!matches!(
        reads[0].target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "ring-native-formal-binding-unavailable")
    );
    let source = "func outside value { consume(value) }\nclass Written\nfunc inside value { consume(value) }\n";
    let output = analyze(source);
    entity_at(&output, "outside", EntityKind::Function, 5, 7);
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn ambiguous_typed_remapped_packaged_and_recovered_functions_keep_explicit_gaps() {
    for source in [
        "func measure value { return value }\nfunc MEASURE other { return other }\n",
        "func measure value,VALUE { return value }\n",
        "func measure(int value) { return value }\n",
        "ChangeRingKeyword func task\nfunc measure value { return value }\n",
        "package Written\nfunc measure value { return value }\n",
        "func measure(value, { consume(value) }\n",
    ] {
        if !source.contains("ChangeRingKeyword")
            && !source.contains("package")
            && !source.contains("value, {")
        {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_ring::LANGUAGE.into())
                .unwrap();
            let parsed = parser.parse(source.as_bytes(), None).unwrap();
            assert!(!parsed.root_node().has_error(), "{source}");
        }
        let output = analyze(source);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Parameter)),
            "{source}"
        );
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain),
                "{source} {domain:?}"
            );
        }
    }
    let source =
        "# func phantom value\nfunc measure value { text=\"func phantom shadow\" return value }\n";
    let output = analyze(source);
    entity_at(
        &output,
        "measure",
        EntityKind::Function,
        source.find("measure").unwrap(),
        7,
    );
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| ["phantom", "shadow"].contains(&entity.canonical_name.as_str()))
    );
}

#[test]
fn top_level_function_and_formal_definitions_preserve_bom_crlf_and_utf16_physical_bytes() {
    let source = "# 雪\r\nfunc measure(value, other) { return value }\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), source.as_bytes()].concat()
        } else {
            let mut raw = if encoding == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in source.encode_utf16() {
                raw.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            raw
        };
        let output = analyze_raw(&raw, encoding);
        let mut owner = None;
        for (name, kind) in [
            ("measure", EntityKind::Function),
            ("value", EntityKind::Parameter),
            ("other", EntityKind::Parameter),
        ] {
            let start = source.find(name).unwrap();
            let (physical_start, physical_len) = if encoding == "utf-8" {
                (start + 3, name.len())
            } else {
                (
                    source[..start].encode_utf16().count() * 2 + 2,
                    name.encode_utf16().count() * 2,
                )
            };
            let id = entity_at(&output, name, kind, physical_start, physical_len);
            let definition = output
                .document()
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: id })
                })
                .unwrap();
            let expected = if encoding == "utf-8" {
                name.as_bytes().to_vec()
            } else {
                name.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            };
            assert_eq!(
                &raw[physical_start..physical_start + physical_len],
                expected
            );
            assert_eq!(definition.source.content_hash(), content_hash(&raw));
            if kind == EntityKind::Function {
                owner = Some(id);
            } else {
                contains(
                    &output,
                    owner.unwrap(),
                    id,
                    physical_start,
                    physical_start + physical_len,
                );
            }
        }
    }
}

#[test]
fn top_level_body_changes_preserve_header_identity_and_current_source_provenance() {
    let source = "func measure(value) { return 1 }\n";
    let changed = source.replace("return 1", "return 123456");
    let before = analyze(source);
    let after = analyze(&changed);
    for name in ["measure", "value"] {
        let first = before
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let second = after
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(first.id, second.id, "{name}");
        assert_eq!(
            first.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(
            second.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(changed.as_bytes())
        );
    }
}

#[test]
fn top_level_required_records_cancellation_and_partial_context_remain_authoritative() {
    let source = b"func measure(value, other) { consume(value) }\n";
    let (_, required) = execute(source, "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, counted) = execute(source, "utf-8", required).unwrap();
    assert_eq!(counted, required);
    let function = entity_at(&bounded, "measure", EntityKind::Function, 5, 7);
    for (name, start) in [("value", 13), ("other", 20)] {
        assert_eq!(&source[start..start + name.len()], name.as_bytes());
        let formal = entity_at(&bounded, name, EntityKind::Parameter, start, name.len());
        contains(&bounded, function, formal, start, start + name.len());
        let record = bounded
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == formal)
            .unwrap();
        assert_eq!(
            record.container,
            Some(rootlight_ir::ContainerRef::Entity(function))
        );
        assert_eq!(record.repository, bounded.document().repository);
        assert_eq!(record.generation, bounded.document().generation);
        let witness = record.evidence.source.as_ref().unwrap();
        assert_eq!(witness.span().file(), bounded.document().files[0].id);
        assert_eq!(witness.content_hash(), content_hash(source));
        assert_eq!(witness.repository(), record.repository);
        assert_eq!(witness.generation(), record.generation);
    }
    assert_eq!(
        execute(source, "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        execute_control(source, "utf-8", 4096, &cancel, None),
        Err(AdapterError::Cancelled {
            reason: rootlight_cancel::CancellationReason::ClientRequest
        })
    ));
    let output = execute_control(source, "utf-8", 4096, &Cancellation::new(), Some(5))
        .unwrap()
        .0;
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Parameter))
    );
}
