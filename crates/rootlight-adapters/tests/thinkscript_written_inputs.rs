//! Public provider/artifact controls for finite written ThinkScript inputs.
//! Full physical originals and valid raw replays precede malformed witness tests.

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseProvider, StreamLimits,
    execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::ThinkScriptParseProvider;
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::{fs, path::Path, sync::Arc};

const ORIGINAL: &[u8] = include_bytes!("fixtures/thinkscript/VIX_FGMR.txt");

fn with_request<R>(
    raw: &[u8],
    records: usize,
    action: impl FnOnce(&AnalysisRequest<'_>) -> R,
) -> R {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.txt"), raw).unwrap();
    let repository = derive_repository(b"thinkscript-written-input-witness-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.txt")).unwrap(),
            65_536,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([53; 20]),
        SourceSpan::new(snapshot.file(), 0, raw.len() as u64).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let syntax = StreamLimits::new(
        128,
        records,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(records.min(64), 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let ir = StreamLimits::new(
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
        65_536,
        16_384,
        128,
        1,
        16 * 1024 * 1024,
        syntax,
        ir,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("thinkscript").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"thinkscript-written-input-witness-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    action(&request)
}

fn analyzer(provider: Arc<dyn ParseProvider>) -> TreeSitterAnalyzer {
    TreeSitterAnalyzer::new(
        provider,
        ProducerIdentity::new(
            "thinkscript-source-contract",
            "1",
            content_hash(b"thinkscript-source-contract"),
        )
        .unwrap(),
        LanguageId::new("thinkscript").unwrap(),
        "thinkscript-source-1",
        ThinkScriptParseProvider::binary_digest(),
    )
    .unwrap()
}

fn analyze(raw: &[u8]) -> AnalysisOutput {
    with_request(raw, 8192, |request| {
        let output = execute_analysis(
            &analyzer(Arc::new(
                ThinkScriptParseProvider::new(65_536, 16_384).unwrap(),
            )),
            request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        validate_ir_document(
            output.document(),
            request.limits().ir(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        for site in &output.document().occurrences {
            assert_eq!(
                site.source.repository(),
                request.source().source_ref().repository()
            );
            assert_eq!(
                site.source.generation(),
                request.source().source_ref().generation()
            );
            assert_eq!(site.source.content_hash(), content_hash(raw));
        }
        output
    })
}

fn definition(
    doc: &NormalizedIrDocument,
    kind: EntityKind,
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact {name}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("resolved Definition")
    };
    let entity = doc
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.language, "thinkscript");
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.provenance, sites[0].provenance);
    symbol
}

fn read(doc: &NormalizedIrDocument, start: u64, end: u64, target: SymbolId, owner: SymbolId) {
    let sites: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
    assert_eq!(site.enclosing, Some(owner));
    let edges: Vec<_> = doc
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(target));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
    assert_eq!(edges[0].provenance, site.provenance);
}

#[test]
fn complete_original_and_bom_bind_written_script_inputs_without_calls_or_builtin_entities() {
    for (bytes, shift) in [
        (ORIGINAL.to_vec(), 0),
        ([b"\xef\xbb\xbf".as_slice(), ORIGINAL].concat(), 3),
    ] {
        let output = analyze(&bytes);
        let doc = output.document();
        let owner = definition(
            doc,
            EntityKind::Function,
            "GetBollingerBandPercent",
            316 + shift,
            339 + shift,
        );
        let upper = definition(
            doc,
            EntityKind::Parameter,
            "upper",
            380 + shift,
            385 + shift,
        );
        let lower = definition(
            doc,
            EntityKind::Parameter,
            "lower",
            402 + shift,
            407 + shift,
        );
        read(doc, 578 + shift, 583 + shift, upper, owner);
        for child in [upper, lower] {
            assert_eq!(
                doc.relations
                    .iter()
                    .filter(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner)
                        && edge.object == RelationEndpoint::Entity(child))
                    .count(),
                1
            );
        }
        assert!(
            !doc.relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(upper)
                    && edge.object == RelationEndpoint::Entity(owner))
        );
        assert!(
            !doc.relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert!(
            !doc.entities
                .iter()
                .any(|entity| entity.canonical_name == "BollingerBands")
        );
        assert!(
            doc.skipped_regions.iter().any(|gap| gap.detail
                == "thinkscript-evaluation-builtins-and-omitted-syntax-unqualified")
        );
    }
}

#[test]
fn renamed_scopes_refuse_duplicate_shadow_write_nested_and_literal_donated_bindings() {
    let good = b"# script Fake { input ghost=2; }\nscript Named { input value=2; input note=\"# literal\"; plot p=External(value).value; }";
    let output = analyze(good);
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            .count(),
        1
    );
    let duplicate = analyze(b"script Named { input value=2; input value=3; plot p=value; }");
    let duplicate_inputs: Vec<_> = duplicate
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
        .collect();
    assert_eq!(duplicate_inputs.len(), 2);
    assert_ne!(duplicate_inputs[0].id, duplicate_inputs[1].id);
    assert!(
        !duplicate
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    for raw in [
        b"script Named { input value=2; input VALUE=3; plot p=value; }".as_slice(),
        b"script Named { input value=2; def value=3; plot p=value; }",
        b"script Named { input value=2; value=3; plot p=value; }",
        b"script Named { input value=2; switch(value) { default: plot p=value; } }",
        b"script Named { input value=2; plot p=External(value=3); }",
        b"script Named { input value=2; plot p=value;",
        b"script Named { input value=2; plot p=\"unfinished; }",
    ] {
        assert!(
            !analyze(raw)
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
}

#[test]
fn exact_original_required_artifact_quota_retains_each_read_and_one_less_refuses() {
    let provider = Arc::new(ThinkScriptParseProvider::new(65_536, 16_384).unwrap());
    let count = with_request(ORIGINAL, 8192, |request| {
        provider
            .required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())
            .unwrap()
    });
    with_request(ORIGINAL, count, |request| {
        let driver = analyzer(provider.clone());
        let (output, artifact) = driver
            .analyze_and_capture(
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            artifact
                .required_syntax_fact_count(&Cancellation::new())
                .unwrap(),
            count
        );
        let owner = definition(
            output.document(),
            EntityKind::Function,
            "GetBollingerBandPercent",
            316,
            339,
        );
        let target = definition(output.document(), EntityKind::Parameter, "upper", 380, 385);
        read(output.document(), 578, 583, target, owner);
        let replay = driver
            .analyze_from_artifact(
                request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.document(), replay.document());
    });
    with_request(ORIGINAL, count - 1, |request| {
        assert!(
            analyzer(provider.clone())
                .prepare_artifact(request, &Cancellation::new())
                .is_err()
        );
    });
    with_request(ORIGINAL, 8192, |request| {
        for limited in [
            ThinkScriptParseProvider::new(64, 16_384).unwrap(),
            ThinkScriptParseProvider::new(65_536, 1).unwrap(),
        ] {
            assert!(
                execute_parse(
                    &limited,
                    &request.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                )
                .is_err()
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(
            execute_parse(
                provider.as_ref(),
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel
            )
            .is_err()
        );
    });
}

#[test]
fn baseline_positive_raw_replays_refuse_missing_duplicate_foreign_moved_and_reparented_context() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
    };
    let raw = b"script Named { input value=2; plot p=External(value).value; }";
    with_request(raw, 4096, |request| {
        let provider = ThinkScriptParseProvider::new(65_536, 16_384).unwrap();
        let parsed = execute_parse(
            &provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        let replay = |facts| {
            let mock = MockParseProvider::new(
                provider.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            execute_analysis(
                &analyzer(Arc::new(mock)),
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap()
        };
        let baseline = replay(parsed.facts().to_vec());
        assert_eq!(
            baseline
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
                .count(),
            1
        );
        let selector = raw.windows(5).rposition(|bytes| bytes == b"value").unwrap() as u64;
        for mode in 0..6 {
            let mut facts = parsed.facts().to_vec();
            let i = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == "thinkscript.input_value.signature")
                .unwrap();
            let marker = facts[i].clone();
            if mode == 0 {
                facts.remove(i);
            } else if mode == 1 {
                facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    marker.parent(),
                    marker.kind(),
                    marker.span(),
                    marker.depth(),
                    marker.syntax_kind().clone(),
                ));
            } else if mode == 2 {
                facts[i] = SyntaxFact::new(
                    marker.local_id(),
                    marker.parent(),
                    SyntaxFactKind::Occurrence,
                    marker.span(),
                    marker.depth(),
                    SyntaxKindLabel::new("thinkscript.foreign.reference").unwrap(),
                );
            } else {
                for fact in &mut facts {
                    if matches!(
                        fact.syntax_kind().as_str(),
                        "thinkscript.input_value.signature" | "thinkscript.input_value.read"
                    ) {
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            if mode == 3 { Some(1) } else { fact.parent() },
                            fact.kind(),
                            if mode >= 4 {
                                SourceSpan::new(fact.span().file(), selector, selector + 5).unwrap()
                            } else {
                                fact.span()
                            },
                            if mode == 3 { 1 } else { fact.depth() },
                            fact.syntax_kind().clone(),
                        );
                    }
                }
                if mode == 5 {
                    let i = facts
                        .iter()
                        .position(|fact| {
                            fact.syntax_kind().as_str() == "thinkscript.script.signature"
                        })
                        .unwrap();
                    let fact = facts[i].clone();
                    facts[i] = SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        fact.span(),
                        fact.depth(),
                        SyntaxKindLabel::new("thinkscript.foreign.signature").unwrap(),
                    );
                }
            }
            let output = replay(facts);
            assert!(
                output
                    .document()
                    .entities
                    .iter()
                    .all(|entity| entity.language != "thinkscript"),
                "an invalid full raw plan cannot donate declarations, mutant {mode}"
            );
            assert!(
                !output
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.role == OccurrenceRole::Read
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                "mutant {mode}"
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail
                        == "thinkscript-evaluation-builtins-and-omitted-syntax-unqualified")
            );
        }
    });
}
