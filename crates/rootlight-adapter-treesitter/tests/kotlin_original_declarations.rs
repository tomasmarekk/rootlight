//! Written Kotlin declaration names must survive native capture and IR lowering.
//! The original fixture separates source-name evidence from calls and strings.

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseOutput, StreamLimits, SyntaxFact, SyntaxFactKind, execute_analysis,
    execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, EntityKind, ExtensionSupport, IrLimits,
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
    source_bytes: &[u8],
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
    assert_eq!(sites[0].source.content_hash(), content_hash(source_bytes));
    assert_eq!(sites[0].syntactic_text_hash, content_hash(name.as_bytes()));
    symbol
}

fn analyze(source_bytes: &[u8]) -> (ParseOutput, NormalizedIrDocument) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("Source.kt"), source_bytes).unwrap();
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
        SourceSpan::new(
            snapshot.file(),
            0,
            u64::try_from(source_bytes.len()).unwrap(),
        )
        .unwrap(),
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
    let (document, _, _) = output.into_parts();
    (parsed, document)
}

#[test]
fn original_kotlin_class_and_override_method_keep_names_and_containment() {
    assert_eq!(ORIGINAL.len(), 835);
    assert_eq!(&ORIGINAL[390..411], b"SlideInRightAnimation");
    assert_eq!(&ORIGINAL[580..588], b"animator");
    assert_eq!(&ORIGINAL[667..679], b"translationX");
    let (parsed, document) = analyze(ORIGINAL);
    let repository = derive_repository(b"kotlin-original-declaration-contract").id();
    native_definition(parsed.facts(), 390, 411, "kotlin.class.declaration");
    native_definition(parsed.facts(), 580, 588, "kotlin.function.declaration");
    native_definition(parsed.facts(), 589, 593, "kotlin.parameter.declaration");
    let document = &document;
    let class = definition(
        document,
        ORIGINAL,
        390,
        411,
        "SlideInRightAnimation",
        EntityKind::Class,
    );
    let method = definition(document, ORIGINAL, 580, 588, "animator", EntityKind::Method);
    let parameter = definition(document, ORIGINAL, 589, 593, "view", EntityKind::Parameter);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(method)
                    && edge.object == RelationEndpoint::Entity(parameter)
            })
            .count(),
        1
    );
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
    assert_eq!(witness.generation(), GenerationId::from_bytes([9; 20]));
    assert_eq!(witness.content_hash(), content_hash(ORIGINAL));
    assert!(witness.span().start_byte() <= 580 && witness.span().end_byte() >= 588);
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 667
            && site.source.span().end_byte() == 679
    }));
}

#[test]
fn ordinary_function_formals_keep_exact_names_and_function_owners() {
    let source = concat!(
        "internal fun zipFiles(destination: File, contents: List<File>) {\r\n",
        "  ZipOutputStream(BufferedOutputStream(FileOutputStream(destination.absolutePath))).use { out ->\r\n",
        "  }\r\n",
        "}\r\n",
        "fun neighboring(destination: Other, note: String = \"destination\", option: String = defaultOption(destination)) {\n",
        "    consume(destination, note, option)\n",
        "    val lambda = { destination: String -> destination }\n",
        "}\n",
        "class Holder(val constructorOnly: String)\n",
    ).as_bytes();
    let (parsed, document) = analyze(source);
    let start_of = |needle: &[u8]| {
        u64::try_from(
            source
                .windows(needle.len())
                .position(|window| window == needle)
                .unwrap(),
        )
        .unwrap()
    };
    let zip_start = start_of(b"zipFiles");
    let zip = definition(
        &document,
        source,
        zip_start,
        zip_start + 8,
        "zipFiles",
        EntityKind::Function,
    );
    let neighboring_start = start_of(b"neighboring");
    let neighboring = definition(
        &document,
        source,
        neighboring_start,
        neighboring_start + 11,
        "neighboring",
        EntityKind::Function,
    );
    let expected = [
        ("destination", b"destination: File".as_slice(), zip),
        ("contents", b"contents: List".as_slice(), zip),
        ("note", b"note: String".as_slice(), neighboring),
        ("destination", b"destination: Other".as_slice(), neighboring),
        ("option", b"option: String".as_slice(), neighboring),
    ];
    for (name, witness, owner) in expected {
        let start = start_of(witness);
        let end = start + u64::try_from(name.len()).unwrap();
        native_definition(parsed.facts(), start, end, "kotlin.parameter.declaration");
        let parameter = definition(&document, source, start, end, name, EntityKind::Parameter);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == parameter)
            .unwrap();
        assert_eq!(entity.container, Some(ContainerRef::Entity(owner)));
        let definitions: Vec<_> = document.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Definition
                && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == parameter)
        }).collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].enclosing, Some(owner));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner)
                        && edge.object == RelationEndpoint::Entity(parameter)
                })
                .count(),
            1
        );
        assert!(parsed.facts().iter().any(|fact| {
            fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "kotlin.parameters.signature"
                && fact.span().start_byte() <= start
                && fact.span().end_byte() >= end
        }));
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        expected.len()
    );
    assert!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .all(|entity| ![
                "File",
                "List",
                "String",
                "Other",
                "defaultOption",
                "constructorOnly"
            ]
            .contains(&entity.canonical_name.as_str()))
    );
}
