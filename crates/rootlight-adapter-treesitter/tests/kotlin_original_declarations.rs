//! Written Kotlin declaration names must survive native capture and IR lowering.
//! The original fixture separates source-name evidence from calls and strings.

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, StreamLimits, SyntaxFact, SyntaxFactKind, execute_analysis,
    execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

const ORIGINAL: &[u8] =
    include_bytes!("fixtures/kotlin_original_declarations/SlideInRightAnimation.kt");

fn native_definition(facts: &[SyntaxFact], start: u64, end: u64, declaration: &str) {
    let names: Vec<_> = facts
        .iter()
        .filter(|fact| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "kotlin.identifier.definition"
                && fact.span().start_byte() == start
                && fact.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        names.len(),
        1,
        "exact native declaration name {start}..{end}"
    );
    let by_id: HashMap<_, _> = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
    let mut next = names[0].parent();
    let mut owner = None;
    for _ in 0..facts.len() {
        let Some(parent) = next.and_then(|parent| by_id.get(&parent).copied()) else {
            break;
        };
        if parent.kind() == SyntaxFactKind::Declaration {
            owner = Some(parent);
            break;
        }
        next = parent.parent();
    }
    let owner = owner.expect("a named native leaf needs its written declaration owner");
    assert_eq!(owner.syntax_kind().as_str(), declaration);
    assert!(owner.span().start_byte() <= start && owner.span().end_byte() >= end);
}

fn definition(
    document: &NormalizedIrDocument,
    start: u64,
    end: u64,
    name: &str,
    kind: EntityKind,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact lowered definition {name}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition must resolve to its own source entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(sites[0].source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(sites[0].syntactic_text_hash, content_hash(name.as_bytes()));
    symbol
}

#[test]
fn original_kotlin_class_and_override_method_keep_names_and_containment() {
    assert_eq!(ORIGINAL.len(), 835);
    assert_eq!(&ORIGINAL[390..411], b"SlideInRightAnimation");
    assert_eq!(&ORIGINAL[580..588], b"animator");
    assert_eq!(&ORIGINAL[667..679], b"translationX");
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("Source.kt"), ORIGINAL).unwrap();
    let repository = derive_repository(b"kotlin-original-declaration-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("Source.kt")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(ORIGINAL.len()).unwrap()).unwrap(),
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
            "kotlin-original-contract",
            "1.0",
            content_hash(b"configuration"),
        )
        .unwrap(),
        LanguageId::new("kotlin").unwrap(),
        "kotlin-original-source-contract",
        content_hash(b"kotlin-original-contract-binary"),
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
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("kotlin").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"kotlin-original-contract-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let cancellation = Cancellation::new();
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    // Only bounded fact identities and coordinates are printed, not source bodies.
    // This distinguishes a missing native name from failed lowerer association.
    for fact in parsed.facts() {
        eprintln!(
            "native fact local={} parent={:?} depth={} kind={:?} span={:?} syntax={}",
            fact.local_id(),
            fact.parent(),
            fact.depth(),
            fact.kind(),
            fact.span(),
            fact.syntax_kind().as_str()
        );
    }
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    eprintln!(
        "lowered entity count={} definition count={} gaps={:?}",
        output.document().entities.len(),
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .count(),
        output.document().skipped_regions
    );
    native_definition(parsed.facts(), 390, 411, "kotlin.class.declaration");
    native_definition(parsed.facts(), 580, 588, "kotlin.function.declaration");
    let document = output.document();
    let class = definition(
        document,
        390,
        411,
        "SlideInRightAnimation",
        EntityKind::Class,
    );
    let method = definition(document, 580, 588, "animator", EntityKind::Method);
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(class)
                && edge.object == RelationEndpoint::Entity(method)
        })
        .collect();
    assert_eq!(edges.len(), 1, "written class contains its override method");
    assert_eq!(edges[0].id, derive_relation_record_id(edges[0]).unwrap());
    let witness = edges[0].evidence.source.as_ref().unwrap();
    assert_eq!(witness.repository(), repository);
    assert_eq!(witness.generation(), source.generation());
    assert_eq!(witness.content_hash(), content_hash(ORIGINAL));
    assert!(witness.span().start_byte() <= 580 && witness.span().end_byte() >= 588);
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 667
            && site.source.span().end_byte() == 679
    }));
}
