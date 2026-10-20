//! Full original libuv source preserves the two authored same-file call witnesses.
//! Includes, preprocessing and external heap bindings remain unproved.

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
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, FactRef,
    IrLimits, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const ORIGINAL: &[u8] = include_bytes!("fixtures/c-original-timer/timer.c");

fn execute_language(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    language: &str,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.c"), raw).unwrap();
    let repository = derive_repository(b"c-original-timer-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.c")).unwrap(),
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
            "c-source-contract",
            "1.0",
            content_hash(b"c-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new(language).unwrap(),
        "c-static-contract",
        content_hash(b"c-contract-binary"),
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
        LanguageId::new(language).unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"c-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required =
        provider.required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())?;
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
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

fn assert_call(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    caller: rootlight_ids::SymbolId,
    target: rootlight_ids::SymbolId,
    start: usize,
    end: usize,
) {
    let document = output.document();
    let occurrences: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.target == OccurrenceTarget::Resolved { symbol: target }
        })
        .collect();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].syntax_kind, "c.direct_file_call.call");
    assert_eq!(occurrences[0].enclosing, Some(caller));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(caller)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1, "written same-file C call must be present");
    let edge = edges[0];
    assert_eq!(edge.id, derive_relation_record_id(edge).unwrap());
    let mut expected = vec![
        FactRef::Entity(caller),
        FactRef::Fact(occurrences[0].id),
        FactRef::Entity(target),
    ];
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(expected.len(), 3);
    assert_eq!(edge.evidence.derivation, expected);
    let source = edge.evidence.source.as_ref().unwrap();
    assert_eq!(source, &occurrences[0].source);
    assert!(source.span().start_byte() <= u64::try_from(start).unwrap());
    assert!(source.span().end_byte() >= u64::try_from(end).unwrap());
}

#[test]
fn original_libuv_timer_retains_both_source_authored_calls_and_exact_proof() {
    assert_eq!(ORIGINAL.len(), 5393);
    let (output, _) = execute_language(ORIGINAL, "utf-8", 4096, "c").unwrap();
    let document = output.document();
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    let mut names = Vec::new();
    for (name, start, end) in [
        ("timer_heap", 1284, 1294),
        ("uv_timer_start", 2239, 2253),
        ("uv_timer_stop", 3047, 3060),
        ("uv_timer_again", 3408, 3422),
    ] {
        assert_eq!(&ORIGINAL[start..end], name.as_bytes());
        names.push(entity_at(
            &output,
            name,
            EntityKind::Function,
            start,
            end - start,
        ));
    }
    for (caller, target, start, end) in [
        (names[3], names[1], 3561, 3575),
        (names[1], names[0], 2877, 2887),
    ] {
        let target_name = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap();
        assert_eq!(&ORIGINAL[start..end], target_name.canonical_name.as_bytes());
        assert_call(&output, caller, target, start, end);
    }
    for edge in document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
    {
        let source = edge.evidence.source.as_ref().unwrap();
        assert_eq!(source.content_hash(), content_hash(ORIGINAL));
        assert_eq!(source.repository(), document.files[0].repository);
        assert_eq!(source.generation(), document.files[0].generation);
    }
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && document.entities.iter().any(|entity| {
                entity.canonical_name == "heap_insert"
                    && edge.object == RelationEndpoint::Entity(entity.id)
            })
    }));
}
