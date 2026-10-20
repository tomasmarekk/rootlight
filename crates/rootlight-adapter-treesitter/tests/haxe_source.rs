//! Written Haxe macro-method declarations and local Reads require native ownership.
//! Macro execution and emitted program content remain outside this contract.

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

const SOURCE: &str = include_str!("fixtures/haxe/MacroTools.hx");

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
    fs::write(directory.path().join("source.hx"), raw).unwrap();
    let repository = derive_repository(b"haxe-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.hx")).unwrap(),
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
            "haxe-source-contract",
            "1.0",
            content_hash(b"haxe-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("haxe").unwrap(),
        "haxe-static-contract",
        content_hash(b"haxe-contract-binary"),
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
        LanguageId::new("haxe").unwrap(),
        EncodingId::new(encoding).unwrap(),
        included_start
            .map(|start| {
                vec![rootlight_adapter_sdk::IncludedRange::new(
                    SourceSpan::new(snapshot.file(), start, u64::try_from(raw.len()).unwrap())
                        .unwrap(),
                    LanguageId::new("haxe").unwrap(),
                )]
            })
            .unwrap_or_default(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"haxe-contract-build-context")),
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
fn original_haxe_macro_method_and_local_read_keep_written_ownership() {
    let output = analyze(SOURCE);
    let document = output.document();
    let class = entity_at(&output, "MacroTools", EntityKind::Class, 6, 10);
    let method = entity_at(
        &output,
        "buildLatestReleaseNotes",
        EntityKind::Method,
        464,
        23,
    );
    let raw = entity_at(&output, "raw", EntityKind::Variable, 526, 3);
    contains(&output, class, method, 464, 487);
    contains(&output, method, raw, 526, 529);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(raw)
            && edge.object == RelationEndpoint::Entity(method)
    }));
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 609
                && site.source.span().end_byte() == 612
        })
        .collect();
    assert_eq!(reads.len(), 1, "{:#?}", document.occurrences);
    assert_eq!(reads[0].target, OccurrenceTarget::Resolved { symbol: raw });
    assert_eq!(reads[0].enclosing, Some(method));
    assert_eq!(
        reads[0].source.content_hash(),
        content_hash(SOURCE.as_bytes())
    );
    assert_eq!(&SOURCE.as_bytes()[609..612], b"raw");
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 609
            && site.source.span().end_byte() == 612
    }));
}

#[test]
fn haxe_formal_shadowing_forward_names_and_other_methods_stay_unresolved() {
    for source in [
        "class Written { function measure() { consume(value); var value = 1; } }",
        "class Written { function measure() { var value = 1; var value = 2; consume(value); } }",
        "class Written { function measure(value:Int) { var value = 1; consume(value); } }",
        "class Written { function measure() { var value = 1; { var value = 2; } consume(value); } }",
        "class Written { function first() { var value = 1; } function second() { consume(value); } }",
        "class Written { var value = 1; function measure() { consume(value); } }",
    ] {
        let output = analyze(source);
        let start =
            u64::try_from(source.rfind("consume(value)").unwrap() + "consume(".len()).unwrap();
        let reads: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read && site.source.span().start_byte() == start
            })
            .collect();
        assert_eq!(reads.len(), 1, "{source}");
        assert!(
            matches!(reads[0].target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "haxe-native-local-variable-binding-unavailable")
        );
    }
}

#[test]
fn haxe_quotes_splices_and_compiler_flags_do_not_publish_ordinary_host_definitions() {
    for body in [
        "return macro { var phantom = 0; consume(phantom); };",
        "return macro $e{phantom};",
        "\n#if enabled\nvar phantom = 0;\nconsume(phantom);\n#end\nreturn macro {};",
    ] {
        let source = format!("class Written {{ static macro function measure() {{ {body} }} }}");
        let output = analyze(&source);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "phantom")
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read)
        );
        assert!(output.document().skipped_regions.iter().any(|gap| matches!(
            gap.detail.as_str(),
            "haxe-quoted-macro-and-splice-declarations-unavailable"
                | "haxe-conditional-declarations-unavailable"
        )));
    }
}

#[test]
fn haxe_writing_argument_slots_members_and_literal_decoys_do_not_become_local_reads() {
    for argument in [
        "value = 0",
        "value += 1",
        "value++",
        "holder.value",
        "value + 1",
    ] {
        let source = format!(
            "class Written {{ function measure() {{ var value = 1; consume({argument}); }} }}"
        );
        let output = analyze(&source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read),
            "{source}"
        );
    }
    let source = "class Written { function measure() { /* var phantom = 0; */ var text = \"var phantom = 0;\"; var value = 1; consume(value); var other = 1; other = 2; } }";
    let output = analyze(source);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "phantom")
    );
    let start = source.find("value").unwrap();
    let target = entity_at(&output, "value", EntityKind::Variable, start, 5);
    let reads: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Read)
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
}

#[test]
fn haxe_utf8_bom_and_utf16_keep_exact_physical_leaves_with_truthful_binding_limits() {
    let text = "\u{feff}// marker λ😀\r\nclass Written { static function measure() { var value = 1; consume(value); } }\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |part: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                part.as_bytes().to_vec()
            } else {
                part.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(text);
        let output = analyze_raw(&raw, encoding);
        for (name, kind) in [
            ("Written", EntityKind::Class),
            ("measure", EntityKind::Method),
            ("value", EntityKind::Variable),
        ] {
            let at = text.find(name).unwrap();
            entity_at(
                &output,
                name,
                kind,
                encode(&text[..at]).len(),
                encode(name).len(),
            );
        }
        let start = u64::try_from(encode(&text[..text.rfind("value").unwrap()]).len()).unwrap();
        let reads: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read && site.source.span().start_byte() == start
            })
            .collect();
        assert_eq!(reads.len(), 1, "{encoding}");
        assert_eq!(
            reads[0].source.span().end_byte(),
            start + u64::try_from(encode("value").len()).unwrap()
        );
        assert_eq!(reads[0].source.content_hash(), content_hash(&raw));
        assert_eq!(
            matches!(reads[0].target, OccurrenceTarget::Resolved { .. }),
            encoding == "utf-8"
        );
        if encoding != "utf-8" {
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "haxe-native-local-variable-binding-unavailable")
            );
        }
    }
}

#[test]
fn haxe_required_local_read_survives_exact_fact_credit_and_rejects_one_less() {
    let (original, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    let exact = execute(SOURCE.as_bytes(), "utf-8", required).unwrap().0;
    let select = |output: &rootlight_adapter_sdk::AnalysisOutput| {
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read && site.source.span().start_byte() == 609
            })
            .map(|site| (site.target.clone(), site.enclosing))
            .collect::<Vec<_>>()
    };
    assert_eq!(select(&exact), select(&original));
    assert_eq!(select(&exact).len(), 1);
    assert!(matches!(
        execute(SOURCE.as_bytes(), "utf-8", required - 1),
        Err(AdapterError::Sink(
            rootlight_adapter_sdk::SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
                ..
            }
        ))
    ));
}

#[test]
fn haxe_partial_ranges_and_cancelled_preflight_cannot_supply_a_complete_local_certificate() {
    let source = "// omitted\nclass Written { static function measure() { var value = 1; consume(value); } }";
    let start = u64::try_from(source.find("class").unwrap()).unwrap();
    let output = execute_control(
        source.as_bytes(),
        "utf-8",
        4096,
        &Cancellation::new(),
        Some(start),
    )
    .unwrap()
    .0;
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haxe-native-local-variable-binding-unavailable"),
        "partial facts: {:#?}; gaps: {:#?}",
        output.document().occurrences,
        output.document().skipped_regions
    );
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        execute_control(source.as_bytes(), "utf-8", 4096, &cancel, None),
        Err(AdapterError::Cancelled { .. })
    ));
    assert!(matches!(
        execute(source.as_bytes(), "bytes", 4096),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::UnsupportedEncoding
        ))
    ));
}

#[test]
fn haxe_native_recovery_and_unsupported_reification_keep_sources_and_no_repaired_read() {
    for source in [
        "class Written { function measure() { var value = ; consume(value); } }",
        "class Written { static macro function measure() { return macro { $b{value}; }; } }",
        "class Written { static macro function measure() { return macro : Value; } }",
    ] {
        let output = analyze(source);
        assert_eq!(
            output.document().files[0].byte_length,
            u64::try_from(source.len()).unwrap()
        );
        assert_eq!(
            output.document().files[0].content_hash,
            content_hash(source.as_bytes())
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
        assert!(!output.document().skipped_regions.is_empty());
    }
}

#[test]
fn haxe_native_artifact_reuse_reconstructs_required_read_demand_and_current_generation() {
    let raw = SOURCE.as_bytes();
    let encoding = "utf-8";
    let maximum = 4096_usize;
    let included_start: Option<u64> = None;
    let cancellation = Cancellation::new();
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.hx"), raw).unwrap();
    let repository = derive_repository(b"haxe-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.hx")).unwrap(),
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
            "haxe-source-contract",
            "1.0",
            content_hash(b"haxe-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("haxe").unwrap(),
        "haxe-static-contract",
        content_hash(b"haxe-contract-binary"),
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
        LanguageId::new("haxe").unwrap(),
        EncodingId::new(encoding).unwrap(),
        included_start
            .map(|start| {
                vec![rootlight_adapter_sdk::IncludedRange::new(
                    SourceSpan::new(snapshot.file(), start, u64::try_from(raw.len()).unwrap())
                        .unwrap(),
                    LanguageId::new("haxe").unwrap(),
                )]
            })
            .unwrap_or_default(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"haxe-contract-build-context")),
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
        LanguageId::new("haxe").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"haxe-contract-build-context")),
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
        .filter(|site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == 609)
        .collect();
    assert_eq!(reads.len(), 1);
    assert!(matches!(reads[0].target, OccurrenceTarget::Resolved { .. }));
    let prepared = analyzer.prepare_artifact(&request, &cancellation).unwrap();
    let classified = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("haxe").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"haxe-contract-build-context")),
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
        LanguageId::new("haxe").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"different-haxe-build-context")),
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
        LanguageId::new("haxe").unwrap(),
        EncodingId::utf8(),
        vec![rootlight_adapter_sdk::IncludedRange::new(
            source.span(),
            LanguageId::new("haxe").unwrap(),
        )],
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"haxe-contract-build-context")),
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

// Qualification uses the same public analyzer boundary with independently varied quotas.
fn qualification_limits(
    source_bytes: usize,
    nodes: usize,
    depth: usize,
    syntax_records: usize,
    ir: IrLimits,
) -> AnalysisLimits {
    let stream = |records: usize| {
        StreamLimits::new(
            1024,
            records,
            64 * 1024 * 1024,
            1024,
            1024 * 1024,
            8 * 1024 * 1024,
            BatchThresholds::new(records.min(64), 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap()
    };
    AnalysisLimits::new(
        source_bytes,
        nodes,
        depth,
        16,
        64 * 1024 * 1024,
        stream(syntax_records),
        stream(65_536),
        ir,
    )
    .unwrap()
}

fn with_haxe_qualification<T>(
    raw: &[u8],
    encoding: &str,
    limits: &AnalysisLimits,
    operation: impl FnOnce(&TreeSitterAnalyzer, &TreeSitterProvider, &AnalysisRequest<'_>) -> T,
) -> T {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.hx"), raw).unwrap();
    let repository = derive_repository(b"haxe-qualification-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.hx")).unwrap(),
            8 * 1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([12; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                8 * 1024 * 1024,
                1_048_576,
                256,
                16,
                128,
                1,
                64 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "haxe-qualification-contract",
            "1.0",
            content_hash(b"haxe-qualification-configuration"),
        )
        .unwrap(),
        LanguageId::new("haxe").unwrap(),
        "haxe-static-contract",
        content_hash(b"haxe-qualification-binary"),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("haxe").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"haxe-qualification-build-context")),
        limits,
    )
    .unwrap()
    .with_generated_status(false);
    operation(&analyzer, &provider, &request)
}

#[test]
fn haxe_body_only_edits_preserve_written_signature_and_reject_stale_body_artifact() {
    let original = "class Written { static function measure() { var value = 1; consume(value); } }";
    let edited = original.replacen("= 1", "= 2", 1);
    assert_eq!(original.len(), edited.len());
    let limits = qualification_limits(8 * 1024 * 1024, 1_048_576, 256, 16_384, IrLimits::default());
    with_haxe_qualification(
        original.as_bytes(),
        "utf-8",
        &limits,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let before = execute_analysis(
                analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            let parsed_before = rootlight_adapter_sdk::execute_parse(
                provider,
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            let artifact = analyzer.prepare_artifact(request, &cancel).unwrap();
            let directory = tempdir().unwrap();
            fs::write(directory.path().join("source.hx"), edited.as_bytes()).unwrap();
            let repository = request.source().source_ref().repository();
            let root = RepositoryRoot::open(repository, directory.path()).unwrap();
            let snapshot = root
                .snapshot(
                    &RelativePath::parse(Path::new("source.hx")).unwrap(),
                    8 * 1024 * 1024,
                )
                .unwrap();
            assert_eq!(snapshot.file(), request.source().source_ref().span().file());
            assert_ne!(
                snapshot.content_hash(),
                request.source().source_ref().content_hash()
            );
            let source = SourceRef::new(
                repository,
                GenerationId::from_bytes([13; 20]),
                SourceSpan::new(snapshot.file(), 0, u64::try_from(edited.len()).unwrap()).unwrap(),
                snapshot.content_hash(),
                None,
            );
            let next = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
                LanguageId::new("haxe").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &limits,
            )
            .unwrap()
            .with_generated_status(false);
            assert!(
                matches!(analyzer.analyze_prepared_artifact(&next, &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
            );
            let after = execute_analysis(
                analyzer,
                &next,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            let parsed_after = rootlight_adapter_sdk::execute_parse(
                provider,
                &next.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            let headers = |output: &rootlight_adapter_sdk::ParseOutput, text: &str| {
                output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                            && fact.syntax_kind().as_str() == "haxe.header.signature"
                    })
                    .map(|fact| {
                        (
                            fact.span(),
                            text[usize::try_from(fact.span().start_byte()).unwrap()
                                ..usize::try_from(fact.span().end_byte()).unwrap()]
                                .to_owned(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                headers(&parsed_before, original),
                headers(&parsed_after, &edited)
            );
            let method_start = original.find("static function").unwrap();
            let body_start = original[method_start..].find('{').unwrap() + method_start;
            assert!(
                headers(&parsed_before, original)
                    .iter()
                    .any(
                        |(span, text)| span.start_byte() == u64::try_from(method_start).unwrap()
                            && span.end_byte() == u64::try_from(body_start).unwrap()
                            && text == "static function measure() "
                    ),
                "headers: {:?}",
                headers(&parsed_before, original)
            );
            for (name, kind) in [
                ("Written", EntityKind::Class),
                ("measure", EntityKind::Method),
                ("value", EntityKind::Variable),
            ] {
                let select = |output: &rootlight_adapter_sdk::AnalysisOutput| {
                    output
                        .document()
                        .entities
                        .iter()
                        .find(|entity| entity.kind == kind && entity.canonical_name == name)
                        .unwrap()
                        .id
                };
                assert_eq!(
                    select(&before),
                    select(&after),
                    "body-only edit changed {kind:?}:{name}"
                );
            }
            assert!(
                after
                    .document()
                    .occurrences
                    .iter()
                    .all(|site| site.source.generation() == source.generation()
                        && site.source.content_hash() == source.content_hash())
            );
            assert!(after.document().entities.iter().all(|entity| {
                entity.evidence.source.as_ref().is_some_and(|reference| {
                    reference.generation() == source.generation()
                        && reference.content_hash() == source.content_hash()
                })
            }));
            let read = after
                .document()
                .occurrences
                .iter()
                .find(|site| site.role == OccurrenceRole::Read)
                .unwrap();
            assert!(matches!(read.target, OccurrenceTarget::Resolved { .. }));
        },
    );
}

#[test]
fn haxe_original_required_preflight_keeps_both_contains_edges_and_exact_record_boundaries() {
    let full = qualification_limits(8 * 1024 * 1024, 1_048_576, 256, 16_384, IrLimits::default());
    with_haxe_qualification(
        SOURCE.as_bytes(),
        "utf-8",
        &full,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let baseline = execute_analysis(
                analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            let required = provider
                .required_syntax_fact_count(&request.to_parse_request(), &cancel)
                .unwrap();
            assert!(required > 1);
            let exact_limits = qualification_limits(
                8 * 1024 * 1024,
                1_048_576,
                256,
                required,
                IrLimits::default(),
            );
            let exact_request = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(
                    request.source().snapshot(),
                    request.source().source_ref(),
                )
                .unwrap(),
                LanguageId::new("haxe").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &exact_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let exact = execute_analysis(
                analyzer,
                &exact_request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            let class = entity_at(&exact, "MacroTools", EntityKind::Class, 6, 10);
            let method = entity_at(
                &exact,
                "buildLatestReleaseNotes",
                EntityKind::Method,
                464,
                23,
            );
            let variable = entity_at(&exact, "raw", EntityKind::Variable, 526, 3);
            contains(&exact, class, method, 464, 487);
            contains(&exact, method, variable, 526, 529);
            let read = exact
                .document()
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Read
                        && site.source.span().start_byte() == 609
                        && site.source.span().end_byte() == 612
                })
                .unwrap();
            assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: variable });
            assert_eq!(read.enclosing, Some(method));
            assert_eq!(
                baseline.document().files[0].content_hash,
                exact.document().files[0].content_hash
            );
        },
    );
    // This separate fixture has no optional comments, strings or unresolved references.
    let mandatory =
        b"class Written { static function measure() { var value = 1; consume(value); } }";
    with_haxe_qualification(mandatory, "utf-8", &full, |analyzer, _, request| {
        let cancel = Cancellation::new();
        let baseline = execute_analysis(
            analyzer,
            request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        let emitted_relations = baseline.document().relations.len();
        let emitted_records = baseline.report().resources().stream().records();
        assert!(emitted_relations >= 2 && emitted_records > emitted_relations);
        let analyze_limited = |ir: IrLimits| {
            let limits = qualification_limits(8 * 1024 * 1024, 1_048_576, 256, 16_384, ir);
            with_haxe_qualification(mandatory, "utf-8", &limits, |analyzer, _, request| {
                execute_analysis(
                    analyzer,
                    request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
            })
        };
        // Preflight reserves candidates before optional output is selected. Measure
        // its actual typed admission boundary rather than equating it to emitted rows.
        let measure_capacity = |mut capacity: usize, relations: Option<usize>| loop {
            let mut ir = IrLimits::default();
            let ceiling = if let Some(relations) = relations {
                ir.max_relations = relations;
                ir.max_total_records = capacity;
                IrLimits::default().max_total_records
            } else {
                ir.max_relations = capacity;
                IrLimits::default().max_relations
            };
            match analyze_limited(ir) {
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
                    assert!(observed > capacity && observed <= ceiling);
                    capacity = observed;
                }
                result => panic!("unexpected capacity result: {result:?}"),
            }
        };
        let relations = measure_capacity(emitted_relations, None);
        let total = measure_capacity(emitted_records, Some(relations));
        let mut ir = IrLimits::default();
        ir.max_relations = relations;
        ir.max_total_records = total;
        let exact = analyze_limited(ir.clone()).unwrap();
        assert_eq!(exact.document(), baseline.document());
        for (relation_limit, total_limit, expected) in [
            (relations - 1, total, relations - 1),
            (relations, total - 1, total - 1),
        ] {
            let mut bounded = ir.clone();
            bounded.max_relations = relation_limit;
            bounded.max_total_records = total_limit;
            let result = analyze_limited(bounded);
            assert!(
                matches!(result, Err(AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit { resource: rootlight_adapter_sdk::ResourceKind::Records, observed, limit })) if limit == expected && observed > limit),
                "relations={relation_limit}, total={total_limit}: {result:?}"
            );
        }
    });
}

#[test]
fn haxe_source_node_depth_and_native_work_limits_keep_typed_refusal_and_no_certificates() {
    let text = "class Written { static function measure() { var value = 1; consume(value); } }";
    let small_source =
        qualification_limits(text.len() - 1, 1_048_576, 256, 16_384, IrLimits::default());
    let admitted = qualification_limits(text.len(), 1_048_576, 256, 16_384, IrLimits::default());
    with_haxe_qualification(
        text.as_bytes(),
        "utf-8",
        &admitted,
        |analyzer, _, request| {
            assert!(
                execute_analysis(
                    analyzer,
                    request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                )
                .is_ok()
            );
            let result = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(
                    request.source().snapshot(),
                    request.source().source_ref(),
                )
                .unwrap(),
                LanguageId::new("haxe").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &small_source,
            );
            assert!(
                matches!(result, Err(rootlight_adapter_sdk::RequestError::SourceTooLarge { observed, limit }) if observed == text.len() && limit == text.len()-1)
            );
        },
    );
    for (nodes, depth, expected) in [
        (2, 256, "syntax-node-limit"),
        (1_048_576, 2, "syntax-depth-limit"),
    ] {
        let limits =
            qualification_limits(8 * 1024 * 1024, nodes, depth, 16_384, IrLimits::default());
        with_haxe_qualification(text.as_bytes(), "utf-8", &limits, |_, provider, request| {
            let output = rootlight_adapter_sdk::execute_parse(
                provider,
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
            assert!(output.facts().is_empty());
            assert!(
                output
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.code().as_str() == expected)
            );
            assert_eq!(provider.stats().cache.entries, 0);
        });
    }
    let wide = format!(
        "class Written {{ static function measure() {{ {} }} }}",
        (0..4096)
            .map(|index| format!("var v{index} = {index};"))
            .collect::<String>()
    );
    let limits = qualification_limits(8 * 1024 * 1024, 1, 256, 16_384, IrLimits::default());
    with_haxe_qualification(wide.as_bytes(), "utf-8", &limits, |_, provider, request| {
        let result = rootlight_adapter_sdk::execute_parse(
            provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        );
        assert!(
            matches!(result, Err(AdapterError::ProviderFailed { code }) if code.as_str() == "syntax-node-parse-work-limit")
        );
        assert_eq!(provider.stats().cache.entries, 0);
    });
}

#[test]
fn haxe_active_native_progress_budget_stops_and_parser_can_be_reset_without_a_tree() {
    use std::{cell::Cell, ops::ControlFlow};
    let source = format!(
        "class Written {{ static function measure() {{ {} }} }}",
        (0..4096)
            .map(|index| format!("var v{index} = {index};"))
            .collect::<String>()
    );
    let reads = Cell::new(0_usize);
    let checks = Cell::new(0_usize);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_haxe::LANGUAGE.into())
        .unwrap();
    let mut input = |offset: usize, _: tree_sitter::Point| {
        let end = offset.saturating_add(256).min(source.len());
        reads.set(reads.get().max(end));
        source.as_bytes().get(offset..end).unwrap_or_default()
    };
    let mut progress = |_: &tree_sitter::ParseState| {
        checks.set(checks.get() + 1);
        if checks.get() == 4 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    assert!(tree.is_none());
    assert_eq!(checks.get(), 4);
    assert!(
        reads.get() < source.len(),
        "finite work stop must precede reading every source byte"
    );
    parser.reset();
    assert!(
        !parser
            .parse("class Clean {}", None)
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn haxe_invalid_native_declaration_operators_cannot_create_written_variable_or_read_proof() {
    for operator in ["+", "-", "==", "+=", "&&"] {
        let text = format!(
            "class Written {{ static function measure() {{ var value {operator} 1; consume(value); }} }}"
        );
        let output = analyze(&text);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable
                    && entity.canonical_name == "value"),
            "invalid operator {operator}"
        );
        let start = u64::try_from(text.find("value").unwrap()).unwrap();
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start),
            "invalid operator {operator}"
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "invalid operator {operator}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "haxe-written-declaration-identity-unavailable"),
            "invalid operator {operator}"
        );
    }
}

#[test]
fn haxe_ordinary_equal_typed_and_uninitialized_written_declarations_keep_identity() {
    for declaration in [
        "var value = 1;",
        "var value:Int = 1;",
        "var value:Int; value = 1;",
    ] {
        let text = format!(
            "class Written {{ static function measure() {{ {declaration} consume(value); }} }}"
        );
        let output = analyze(&text);
        let variable = entity_at(
            &output,
            "value",
            EntityKind::Variable,
            text.find("value").unwrap(),
            5,
        );
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read)
            .collect();
        assert_eq!(sites.len(), 1, "{declaration}");
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved { symbol: variable },
            "{declaration}"
        );
    }
}

#[test]
fn haxe_local_field_headers_cannot_publish_variables_definitions_or_resolved_reads() {
    for declaration in [
        "var value(get,set):Int;",
        "public var value = 1;",
        "private var value = 1;",
        "override var value = 1;",
    ] {
        let source =
            format!("class Written {{ function measure() {{ {declaration} consume(value); }} }}");
        let output = analyze(&source);
        let start = u64::try_from(source.find("value").unwrap()).unwrap();
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable
                    && entity.canonical_name == "value"),
            "{source}"
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start),
            "{source}"
        );
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
                .any(|gap| gap.detail == "haxe-written-declaration-identity-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn haxe_static_and_metadata_locals_keep_identity_without_static_initializer_capture() {
    for declaration in [
        "@:keep var value:Int = 1 + 2;",
        "static var value:Int = 1;",
        "static final value = 1;",
    ] {
        let source =
            format!("class Written {{ function measure() {{ {declaration} consume(value); }} }}");
        let output = analyze(&source);
        let symbol = entity_at(
            &output,
            "value",
            EntityKind::Variable,
            source.find("value").unwrap(),
            5,
        );
        let start = u64::try_from(source.rfind("value").unwrap()).unwrap();
        let read = output
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Read && site.source.span().start_byte() == start
            })
            .unwrap();
        assert_eq!(read.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
    }
    let source = "class Written { function measure() { var value = 1; static var cached = consume(value); consume(cached); } }";
    let output = analyze(source);
    let cached = entity_at(
        &output,
        "cached",
        EntityKind::Variable,
        source.find("cached").unwrap(),
        6,
    );
    let forbidden = u64::try_from(source.rfind("value").unwrap()).unwrap();
    let read = output
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Read && site.source.span().start_byte() == forbidden
        })
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
    let later = u64::try_from(source.rfind("cached").unwrap()).unwrap();
    let read = output
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == later)
        .unwrap();
    assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: cached });
}
