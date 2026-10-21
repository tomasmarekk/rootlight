//! Native module-call proofs must survive strict generation identity verification.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint, derive_relation_record_id};

#[test]
fn typescript_module_call_survives_merge_query_and_exact_source_read() {
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/typescript_calls/vue-util.ts"
    );
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("util.ts"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let function = |name: &str| {
        document
            .entities
            .iter()
            .find(|row| {
                row.language == "typescript"
                    && row.kind == EntityKind::Function
                    && row.canonical_name == name
            })
            .unwrap()
    };
    let owner = function("isPromise");
    let target = function("isDef");
    let occurrence = document
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::CallSite
                && row.source.span().start_byte() == 2018
                && row.enclosing == Some(owner.id)
                && row.target == (OccurrenceTarget::Resolved { symbol: target.id })
        })
        .expect("the exact written call remains resolved after identity-verified merge");
    let relation = document
        .relations
        .iter()
        .find(|row| {
            row.predicate == RelationPredicate::Calls
                && row.subject == RelationEndpoint::Entity(owner.id)
                && row.object == RelationEndpoint::Entity(target.id)
                && row.evidence.source.as_ref() == Some(&occurrence.source)
        })
        .unwrap();
    let mut parents = [
        FactRef::Entity(owner.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target.id),
    ];
    parents.sort_unstable();
    assert_eq!(relation.evidence.derivation, parents);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([owner.id]),
            vec![RelationFamily::Calls],
            Some(rootlight_query::RelationDirection::Outbound),
            0,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(relationships.data.groups.iter().any(|group| {
        group
            .items
            .iter()
            .any(|row| row.symbol == target.id && row.source_refs.contains(&occurrence.source))
    }));
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![occurrence.source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, b"isDef");
}

const WRITTEN_VALUE_ORIGINAL: &[u8] =
    include_bytes!("../../tests/fixtures/typescript_value_uses/annotation-tests.ts");

#[test]
fn typescript_complete_original_definitions_source_and_unknown_callable_targets_remain_exact() {
    assert_eq!(WRITTEN_VALUE_ORIGINAL.len(), 856);
    assert_eq!(
        WRITTEN_VALUE_ORIGINAL
            .windows(2)
            .filter(|bytes| *bytes == b"\r\n")
            .count(),
        31
    );
    assert_eq!(&WRITTEN_VALUE_ORIGINAL[160..177], b"validateTestSuite");
    assert_eq!(&WRITTEN_VALUE_ORIGINAL[490..495], b"suite");
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("annotation-tests.ts"),
        WRITTEN_VALUE_ORIGINAL,
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let definition = |name: &str, start: u64, end: u64| {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "written local Definition {name}");
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("written Definition resolves")
        };
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.id == symbol)
            .collect();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].language, "typescript");
        assert_eq!(entities[0].kind, EntityKind::Variable);
        assert_eq!(entities[0].canonical_name, name);
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        assert_eq!(
            sites[0].source.content_hash(),
            content_hash(WRITTEN_VALUE_ORIGINAL)
        );
        symbol
    };
    let validator = definition("validateTestSuite", 160, 177);
    let suite = definition("suite", 490, 495);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(validator)
                && edge.object == RelationEndpoint::Entity(suite))
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 589
                && site.source.span().end_byte() == 594)
    );
    for symbol in [validator, suite] {
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
    }
    let file = document
        .files
        .iter()
        .find(|file| file.language == "typescript")
        .unwrap();
    let full = rootlight_ir::SourceRef::new(
        receipt.repository,
        receipt.generation,
        rootlight_ir::SourceSpan::new(file.id, 0, 856).unwrap(),
        content_hash(WRITTEN_VALUE_ORIGINAL),
        None,
    );
    let full_read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![full],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(full_read.data.chunks.len(), 1);
    assert_eq!(full_read.data.chunks[0].bytes, WRITTEN_VALUE_ORIGINAL);
}

#[test]
fn typescript_complete_original_native_value_use_context_is_reported() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("annotation-tests.ts"),
        WRITTEN_VALUE_ORIGINAL,
    )
    .unwrap();
    let repository = derive_repository(b"typescript-value-context").id();
    let generation = GenerationId::from_bytes([67; 20]);
    let root = RepositoryRoot::open(repository, fixture.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("annotation-tests.ts")).unwrap(),
            65536,
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "typescript",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap();
    let parsed = rootlight_adapter_sdk::execute_parse(
        service.structural_parsers["typescript"].as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let mut selected = BTreeSet::new();
    for (start, end) in [(160, 177), (490, 495), (538, 555), (556, 561)] {
        let matches: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.span().start_byte() == start
                    && fact.span().end_byte() == end
                    && fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Occurrence
            })
            .collect();
        assert!(!matches.is_empty(), "native source witness {start}..{end}");
        for fact in matches {
            let mut current = Some(fact.local_id());
            for _ in 0..parsed.facts().len() {
                let Some(id) = current else { break };
                if !selected.insert(id) {
                    break;
                }
                current = parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.local_id() == id)
                    .unwrap()
                    .parent();
            }
        }
    }
    let rows: Vec<_> = parsed.facts().iter().filter(|fact| selected.contains(&fact.local_id())).map(|fact| serde_json::json!({"local_id":fact.local_id(),"parent":fact.parent(),"kind":format!("{:?}",fact.kind()),"syntax":fact.syntax_kind().as_str(),"depth":fact.depth(),"start":fact.span().start_byte(),"end":fact.span().end_byte(),"text":std::str::from_utf8(&WRITTEN_VALUE_ORIGINAL[usize::try_from(fact.span().start_byte()).unwrap()..usize::try_from(fact.span().end_byte()).unwrap()]).unwrap()})).collect();
    println!(
        "{}",
        serde_json::json!({"schema":"typescript-complete-original-native-value-context/1","bytes":856,"coverage":format!("{:?}",parsed.report().coverage().status()),"facts":rows})
    );
}
