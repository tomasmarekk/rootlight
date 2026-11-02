//! Native Dgraph terms must survive discovery, durable indexing and cold typed reads.
//! Physical source identity remains independent of identifiers and default-value bytes.

use super::*;
use rootlight_ir::{RDF_STATEMENT_NAMESPACE, RdfStatement, RdfStatementFormat};
use rootlight_query::{QueryBudget, RdfStatementReadRequest, read_rdf_statement_page};
use rootlight_storage::{GenerationBudget, GenerationContext, GenerationReadLimit};

// Apache-2.0 fixture from dgraph-io/dgraph at the pinned source revision.
// https://github.com/dgraph-io/dgraph/blob/c587cba75f60445125459b46b4d81757a73384aa/dgraph/cmd/alpha/testdata.nq
// The complete notice is retained in native-grammars/nquads-dgraph/LICENSE-DGRAPH.
const ORIGINAL: &[u8] = b"<alice> <follows> <bob> .\n<bob> <follows> <fred> .\n<bob> <status> \"I'm your Uncle Bob!\" .\n<charlie> <follows> <bob> .\n<charlie> <follows> <dani> .\n<dani> <follows> <bob> .\n<dani> <follows> <greg> .\n<dani> <status> \"Smart Dani\" .\n<emily> <follows> <fred> .\n<fred> <follows> <greg> .\n<greg> <status> \"Mr. Cool greg\" .\n";
const NATIVE: &[u8] = b"\xef\xbb\xbf<0x01><status>\"native\\xFF\\v\0\".\r\n# comment\0\r\n";
const CONFIG: &str = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'original.data'='nquads-dgraph'\n'native.data'='nquads-dgraph'\n";

fn statements_for_file(document: &NormalizedIrDocument, file: FileId) -> Vec<RdfStatement> {
    let mut statements: Vec<_> = document
        .extensions
        .iter()
        .filter(|record| {
            record.namespace == RDF_STATEMENT_NAMESPACE
                && record.evidence.source.as_ref().unwrap().span().file() == file
        })
        .map(|record| {
            assert_eq!(record.version, "3");
            rootlight_ir::decode_rdf_statement_envelope(record).unwrap()
        })
        .collect();
    statements.sort_by_key(|statement| statement.source().span().start_byte());
    statements
}

fn assert_native(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    native: &[u8],
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    for (path, bytes, count) in [("original.data", ORIGINAL, 11), ("native.data", native, 1)] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "nquads-dgraph");
        assert_eq!(file.content_hash, content_hash(bytes));
        let read = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, bytes);
        assert_eq!(read.data.chunks[0].language, "nquads-dgraph");
        let statements = statements_for_file(document, file.id);
        assert_eq!(statements.len(), count);
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == file.id)
        );
        for statement in &statements {
            assert_eq!(statement.format(), RdfStatementFormat::NQuadsDgraph);
            assert_eq!(statement.source().generation(), receipt.generation);
            assert_eq!(statement.source().content_hash(), content_hash(bytes));
            assert!(statement.graph().is_none());
            let span = statement.source().span();
            let text = std::str::from_utf8(
                &bytes[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
            )
            .unwrap();
            assert_eq!(statement.source_text(), text);
            statement.verify_source_text(text).unwrap();
            for term in [
                statement.subject(),
                statement.predicate(),
                statement.object(),
            ] {
                assert_eq!(term.source().generation(), receipt.generation);
                assert_eq!(term.source().span().file(), file.id);
                assert!(term.iri().is_none());
                assert!(term.datatype().is_none());
                assert!(term.blank_node_label().is_none());
            }
        }
        if path == "original.data" {
            let expected = [
                ["alice", "follows", "bob"],
                ["bob", "follows", "fred"],
                ["bob", "status", "I'm your Uncle Bob!"],
                ["charlie", "follows", "bob"],
                ["charlie", "follows", "dani"],
                ["dani", "follows", "bob"],
                ["dani", "follows", "greg"],
                ["dani", "status", "Smart Dani"],
                ["emily", "follows", "fred"],
                ["fred", "follows", "greg"],
                ["greg", "status", "Mr. Cool greg"],
            ];
            for (statement, terms) in statements.iter().zip(expected) {
                assert_eq!(statement.subject().dgraph_identifier(), Some(terms[0]));
                assert_eq!(statement.predicate().dgraph_identifier(), Some(terms[1]));
                if terms[1] == "status" {
                    assert_eq!(
                        statement.object().dgraph_default_value(),
                        Some(terms[2].as_bytes())
                    );
                    assert!(statement.object().literal_value().is_none());
                } else {
                    assert_eq!(statement.object().dgraph_identifier(), Some(terms[2]));
                }
                for (term, value) in [
                    statement.subject(),
                    statement.predicate(),
                    statement.object(),
                ]
                .into_iter()
                .zip(terms)
                {
                    let span = term.source().span();
                    let token = &bytes[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()];
                    let expected_token = if term.dgraph_identifier().is_some() {
                        format!("<{value}>")
                    } else {
                        format!("\"{value}\"")
                    };
                    assert_eq!(token, expected_token.as_bytes());
                }
            }
        } else {
            assert_eq!(statements[0].subject().dgraph_identifier(), Some("0x01"));
            let value = statements[0].object();
            let expected: &[u8] = if native == NATIVE {
                b"native\xff\x0b\0"
            } else {
                b"changed\xff\x0b\0"
            };
            assert_eq!(value.dgraph_default_value(), Some(expected));
            assert!(value.dgraph_default_text().is_none());
        }
        let cancellation = deadline();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
        let page = read_rdf_statement_page(
            &reader,
            RdfStatementReadRequest::new(
                receipt.repository,
                receipt.generation,
                file.id,
                GenerationReadLimit::default(),
            ),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert!(page.range_complete());
        let mut cold: Vec<_> = page
            .statements()
            .iter()
            .map(|record| record.statement().clone())
            .collect();
        cold.sort_by_key(|statement| statement.source().span().start_byte());
        assert_eq!(cold, statements);
    }
}

#[test]
fn dgraph_native_terms_survive_fresh_noop_incremental_restored_and_cold_query() {
    assert_eq!(ORIGINAL.len(), 316);
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("original.data"), ORIGINAL).unwrap();
    let input = repository.path().join("native.data");
    fs::write(&input, NATIVE).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(CONFIG), 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_native(&service, &initial, &paths, NATIVE);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(NATIVE)
        .unwrap()
        .replace("native", "changed")
        .into_bytes();
    fs::write(&input, &changed).unwrap();
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
    assert_native(&service, &updated, &paths, &changed);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        updated.generation
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_native(&reopened, &updated, &paths, &changed);
    fs::write(&input, NATIVE).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_native(&reopened, &restored, &paths, NATIVE);
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_native(&final_open, &restored, &paths, NATIVE);
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn dgraph_configuration_switch_reindexes_same_bytes_without_weakening_standard_nquads() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("original.data"), ORIGINAL).unwrap();
    let standard = CONFIG.replace("nquads-dgraph", "nquads");
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(&standard), 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let before = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(before.generation)
        .unwrap();
    let file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "original.data")
        .unwrap();
    assert_eq!(file.language, "nquads");
    assert!(statements_for_file(snapshot.document(), file.id).is_empty());
    assert!(
        snapshot
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().file() == file.id)
    );
    drop(service);
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(CONFIG), 2).unwrap();
    let mut native =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let after = native
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(before.generation, after.generation);
    let snapshot = native.loaded_generation_snapshot(after.generation).unwrap();
    let file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "original.data")
        .unwrap();
    assert_eq!(file.language, "nquads-dgraph");
    assert_eq!(file.content_hash, content_hash(ORIGINAL));
    assert_eq!(statements_for_file(snapshot.document(), file.id).len(), 11);
    assert_eq!(
        native
            .incremental_evidence(after.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    drop(native);
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(&standard), 2).unwrap();
    let mut standard_service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let reverted = standard_service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(reverted.generation, after.generation);
    let snapshot = standard_service
        .loaded_generation_snapshot(reverted.generation)
        .unwrap();
    let file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "original.data")
        .unwrap();
    assert_eq!(file.language, "nquads");
    assert_eq!(file.content_hash, content_hash(ORIGINAL));
    assert!(statements_for_file(snapshot.document(), file.id).is_empty());
}
