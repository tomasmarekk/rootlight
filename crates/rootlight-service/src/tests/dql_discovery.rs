//! Native query roles must survive configured discovery and durable generations.
//! Cold reads use the indexed extension API independently of service graph residency.

use super::*;
use rootlight_ir::{
    DQL_QUERY_NAMESPACE, DqlCallKind, DqlNodeKind, DqlQuery, decode_dql_query_envelope,
};
use rootlight_query::{DqlQueryReadRequest, QueryBudget, read_dql_query_page};
use rootlight_storage::{GenerationBudget, GenerationContext, GenerationReadLimit};

const ORIGINALS: [&[u8]; 3] = [
    include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite01.dql"),
    include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite02.dql"),
    include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite03.dql"),
];
const PATHS: [&str; 3] = [
    "dgraph/cmd/bulk/systest/suite01/query.json",
    "dgraph/cmd/bulk/systest/suite02/query.json",
    "dgraph/cmd/bulk/systest/suite03/query.json",
];
const CONFIG: &str = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'dgraph/cmd/bulk/systest/suite01/query.json'='dql'\n'dgraph/cmd/bulk/systest/suite02/query.json'='dql'\n'dgraph/cmd/bulk/systest/suite03/query.json'='dql'\n'standalone.query'='graphql'\n";

fn queries(document: &NormalizedIrDocument, file: FileId) -> Vec<DqlQuery> {
    document
        .extensions
        .iter()
        .filter(|extension| {
            extension.namespace == DQL_QUERY_NAMESPACE
                && extension.evidence.source.as_ref().unwrap().span().file() == file
        })
        .map(|extension| decode_dql_query_envelope(extension).unwrap())
        .collect()
}

fn assert_queries(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    first: &[u8],
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 5);
    for (index, path) in PATHS.into_iter().enumerate() {
        let bytes = if index == 0 { first } else { ORIGINALS[index] };
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "dql");
        assert_eq!(file.content_hash, content_hash(bytes));
        let source = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(source.data.chunks[0].bytes, bytes);
        assert_eq!(source.data.chunks[0].language, "dql");
        let warm = queries(document, file.id);
        assert_eq!(warm.len(), 1);
        let query = &warm[0];
        assert_eq!(
            query.source().span(),
            file.evidence.source.as_ref().unwrap().span()
        );
        assert_eq!(query.source().generation(), receipt.generation);
        query
            .verify_source_text(std::str::from_utf8(bytes).unwrap())
            .unwrap();
        let roots = [2, 18, 4][index];
        let filters = [0, 12, 0][index];
        let count = |role| {
            query
                .nodes()
                .iter()
                .filter(|node| node.role == role)
                .count()
        };
        assert_eq!(count(DqlNodeKind::Root { aggregation: false }), roots);
        assert_eq!(
            count(DqlNodeKind::Call {
                class: DqlCallKind::Generator
            }),
            roots
        );
        assert_eq!(count(DqlNodeKind::Filter), filters);
        assert_eq!(
            count(DqlNodeKind::Call {
                class: DqlCallKind::Nested
            }),
            filters
        );
        assert_eq!(
            count(DqlNodeKind::Call {
                class: DqlCallKind::Count
            }),
            [0, 6, 1][index]
        );
        assert_eq!(count(DqlNodeKind::Predicate { reverse: true }), filters / 2);
        assert_eq!(count(DqlNodeKind::Language), usize::from(index == 2));
        assert!(
            query.nodes().iter().enumerate().all(|(index, _)| query
                .node_source(index)
                .unwrap()
                .generation()
                == receipt.generation)
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == file.id),
            "{:?}",
            document.skipped_regions
        );
        assert!(!document.entities.iter().any(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id)
        }));
        let cancellation = deadline();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
        let mut after = None;
        let mut cold = Vec::new();
        let mut scanned = 0;
        let mut empty = 0;
        loop {
            let request = DqlQueryReadRequest::new(
                receipt.repository,
                receipt.generation,
                file.id,
                GenerationReadLimit::new(1).unwrap(),
            );
            let request = after.map_or(request, |after| request.with_after(after));
            let page =
                read_dql_query_page(&reader, request, &context, QueryBudget::default()).unwrap();
            assert_eq!(page.scope(), "file_extensions");
            assert_eq!(page.total_extensions(), 2);
            assert_eq!(page.total_matching_queries(), None);
            scanned += page.scanned_extensions();
            empty += usize::from(page.queries().is_empty());
            cold.extend(page.queries().iter().map(|record| record.query().clone()));
            if page.range_complete() {
                assert_eq!(page.next_cursor(), None);
                break;
            }
            let next = page.next_cursor().unwrap();
            assert!(after.is_none_or(|old| next > old));
            after = Some(next);
        }
        assert_eq!(scanned, 2);
        assert_eq!(empty, 1);
        assert_eq!(cold, warm);
    }
    for (path, language) in [("ordinary.json", "json"), ("standalone.query", "graphql")] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language);
        assert!(queries(document, file.id).is_empty());
    }
}

#[test]
fn original_dql_queries_survive_fresh_noop_incremental_restored_and_cold_reads() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    for (path, bytes) in PATHS.into_iter().zip(ORIGINALS) {
        let path = repository.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fs::write(
        repository.path().join("ordinary.json"),
        b"{\"name\":\"ordinary\"}",
    )
    .unwrap();
    fs::write(
        repository.path().join("standalone.query"),
        b"query Read { viewer { id } }",
    )
    .unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(CONFIG), 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_queries(&service, &initial, &paths, ORIGINALS[0]);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINALS[0])
        .unwrap()
        .replace("Peter", "Alice")
        .into_bytes();
    fs::write(repository.path().join(PATHS[0]), &changed).unwrap();
    let updated = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, initial.generation);
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_queries(&service, &updated, &paths, &changed);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_queries(&reopened, &updated, &paths, &changed);
    fs::write(repository.path().join(PATHS[0]), ORIGINALS[0]).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_queries(&reopened, &restored, &paths, ORIGINALS[0]);
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_queries(&final_open, &restored, &paths, ORIGINALS[0]);
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}
