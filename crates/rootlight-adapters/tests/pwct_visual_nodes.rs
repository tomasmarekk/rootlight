//! Exercise the native PWCT producer and normalized visual ownership together.
//! Written IDs survive caption, code, row-order and visual-parent edits; code
//! fragments and organizational captions do not become executable declarations.

use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::PwctParseProvider;
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, ExtensionSupport,
    IrLimits, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

fn analyze(raw: &[u8]) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.pwct"), raw).unwrap();
    let repository = derive_repository(b"pwct-visual-node-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.pwct")).unwrap(),
            2 * 1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([61; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let stream = StreamLimits::new(
        512,
        16_384,
        32 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        2 * 1024 * 1024,
        131_072,
        64,
        1,
        32 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("pwct").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"pwct-visual-node-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(PwctParseProvider::new(2 * 1024 * 1024, 131_072).unwrap()),
        ProducerIdentity::new(
            "pwct-visual-node-contract",
            "1",
            PwctParseProvider::binary_digest(),
        )
        .unwrap(),
        LanguageId::new("pwct").unwrap(),
        "pwct-visual-source-1",
        PwctParseProvider::binary_digest(),
    )
    .unwrap();
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    Ok(output)
}

fn row(id: u64, parent: u64, caption: &str, code: &str) -> String {
    format!(
        r#"[{id},{parent},[["name","{caption}"],["active",1],["code","{code}"],["interactionid",1],["visible",1],["stepnumber",1],["steptype",2]]]"#
    )
}

fn document(rows: &[String]) -> String {
    format!(
        "cVisualSourceFileVersion='1.0.0'\ncVisualLanguageName='RingPWCT'\ncNaturalLanguageName='english'\nnStepsID=100\nnInteractionsID=2\naStepsTable=[{}]\naInteractionsTable=[[1,1,'print','01/01/2023','12:00:00','']]\n",
        rows.join(",\n")
    )
}

fn properties(output: &AnalysisOutput, raw: &[u8], count: usize) -> BTreeMap<u64, SymbolId> {
    let doc = output.document();
    assert_eq!(doc.files.len(), 1);
    assert_eq!(doc.entities.len(), count);
    assert_eq!(doc.occurrences.len(), count);
    let mut symbols = BTreeMap::new();
    for entity in &doc.entities {
        assert_eq!(entity.kind, EntityKind::Property);
        assert_eq!(entity.language, "pwct");
        let id: u64 = entity.canonical_name.parse().unwrap();
        assert!(symbols.insert(id, entity.id).is_none());
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| site.target == OccurrenceTarget::Resolved { symbol: entity.id })
            .collect();
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        assert_eq!(definition.role, OccurrenceRole::Definition);
        assert_eq!(definition.source.content_hash(), content_hash(raw));
        assert_eq!(definition.source.span().file(), doc.files[0].id);
        assert_eq!(definition.source.repository(), doc.files[0].repository);
        assert_eq!(definition.source.generation(), doc.files[0].generation);
        let start = usize::try_from(definition.source.span().start_byte()).unwrap();
        let end = usize::try_from(definition.source.span().end_byte()).unwrap();
        assert_eq!(
            std::str::from_utf8(&raw[start..end])
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            id
        );
    }
    assert_eq!(symbols.len(), count);
    assert!(
        doc.relations
            .iter()
            .all(|edge| edge.predicate == RelationPredicate::Contains)
    );
    assert_eq!(doc.relations.len(), count);
    symbols
}

fn owner(output: &AnalysisOutput, child: SymbolId, parent: Option<SymbolId>) {
    let doc = output.document();
    let subject = parent.map_or(
        RelationEndpoint::File(doc.files[0].id),
        RelationEndpoint::Entity,
    );
    let container = parent.map_or(ContainerRef::File(doc.files[0].id), ContainerRef::Entity);
    let entity = doc
        .entities
        .iter()
        .find(|entity| entity.id == child)
        .unwrap();
    assert_eq!(entity.container, Some(container));
    assert_eq!(
        doc.relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == subject
                    && edge.object == RelationEndpoint::Entity(child)
            })
            .count(),
        1
    );
}

#[test]
fn visual_parent_ids_own_sibling_rows_in_topological_order() {
    let raw = document(&[
        row(7, 1, "Functions", "func phantom"),
        row(1, 0, "Classes", "class Ghost"),
        row(3, 7, "Print", "see 1"),
    ]);
    let output = analyze(raw.as_bytes()).unwrap();
    let ids = properties(&output, raw.as_bytes(), 3);
    owner(&output, ids[&1], None);
    owner(&output, ids[&7], Some(ids[&1]));
    owner(&output, ids[&3], Some(ids[&7]));
}

#[test]
fn written_ids_survive_caption_code_order_and_parent_changes() {
    let first = document(&[row(1, 0, "Start", ""), row(7, 1, "First", "see 1")]);
    let changed = document(&[row(7, 0, "Changed", "see 99"), row(1, 7, "Moved", "")]);
    let before = analyze(first.as_bytes()).unwrap();
    let after = analyze(changed.as_bytes()).unwrap();
    let old_ids = properties(&before, first.as_bytes(), 2);
    let new_ids = properties(&after, changed.as_bytes(), 2);
    assert_eq!(old_ids, new_ids);
    owner(&after, new_ids[&7], None);
    owner(&after, new_ids[&1], Some(new_ids[&7]));
}

#[test]
fn duplicate_or_cyclic_visual_graphs_do_not_publish_properties() {
    for rows in [
        vec![row(1, 0, "First", ""), row(1, 0, "Duplicate", "")],
        vec![row(1, 7, "First", ""), row(7, 1, "Cycle", "")],
        vec![row(1, 9, "Missing", "")],
    ] {
        assert!(analyze(document(&rows).as_bytes()).is_err());
    }
}

#[test]
#[ignore = "requires the independently pinned original PWCT2 checkout"]
fn original_visual_ids_and_owners_reach_normalized_ir() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_PWCT_REPOSITORY").expect("set ROOTLIGHT_PWCT_REPOSITORY"),
    );
    for (path, count) in [
        ("applications/FromPWCTDocs/program9.pwct", 2),
        ("applications/FromPWCTDocs/program3.pwct", 7),
        ("applications/FromPWCTDocs/program2.pwct", 6),
        ("applications/FromPWCTVideos/lesson4.pwct", 11),
        ("applications/FromPWCTVideos/lesson32_part2.pwct", 3),
        ("applications/Cards/cards.pwct", 452),
        ("src/tools/vsfgenerator/test/test.pwct", 21),
    ] {
        let raw = fs::read(root.join(path)).unwrap();
        let output = analyze(&raw).unwrap();
        let ids = properties(&output, &raw, count);
        if path.ends_with("program9.pwct") {
            let mut spans = output
                .document()
                .occurrences
                .iter()
                .map(|site| {
                    (
                        site.source.span().start_byte(),
                        site.source.span().end_byte(),
                    )
                })
                .collect::<Vec<_>>();
            spans.sort_unstable();
            assert_eq!(spans, [(225, 226), (523, 524)]);
            owner(&output, ids[&1], None);
            owner(&output, ids[&2], Some(ids[&1]));
        }
        println!(
            "{path}: {count} native visual Properties/Definitions/Contains; decoded Ring semantics remain unqualified"
        );
    }
}
