//! Native N-Quads retains typed graph terms through durable product transitions.
//! Physical source reads and cold indexed pages authenticate the same statements.

use super::*;
use rootlight_ir::{RDF_STATEMENT_NAMESPACE, RdfStatement, RdfStatementFormat};
use rootlight_query::{QueryBudget, RdfStatementReadRequest, read_rdf_statement_page};
use rootlight_storage::{GenerationBudget, GenerationContext, GenerationReadLimit};

const SOURCE: &[u8] = b"\xef\xbb\xbf_:shared <urn:p> \"first\0value\"@en <urn:g> .\r\n_:shared <urn:p> <urn:o> _:graph .\r\n_:shared <urn:p> \"typed\"^^<urn:type> .\r\n_:graph <urn:p> _:shared _:graph .\r\n# comment\0\r\n";
const ORIGINAL_NUL_BOUNDARY: &[u8] =
    b"<http://a.example/s> <http://a.example/p> \"\0\t\x0b\x0c\x0e&([]\x7f\" .\n";
const PEER: &[u8] = b"_:shared <urn:p> <urn:o> .\n";

fn statements_for_file(document: &NormalizedIrDocument, file: FileId) -> Vec<RdfStatement> {
    let mut statements: Vec<_> = document
        .extensions
        .iter()
        .filter(|record| {
            record.namespace == RDF_STATEMENT_NAMESPACE
                && record.evidence.source.as_ref().unwrap().span().file() == file
        })
        .map(|record| rootlight_ir::decode_rdf_statement_envelope(record).unwrap())
        .collect();
    statements.sort_by_key(|statement| statement.source().span().start_byte());
    statements
}

fn assert_native(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    source: &[u8],
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    let file = document
        .files
        .iter()
        .find(|file| file.path == "data.nq")
        .unwrap();
    let peer = document
        .files
        .iter()
        .find(|file| file.path == "peer.nq")
        .unwrap();
    assert_eq!(file.language, "nquads");
    assert_eq!(file.content_hash, content_hash(source));
    let source_ref = file.evidence.source.as_ref().unwrap();
    let read = service
        .source_read(receipt.generation, vec![source_ref.clone()], &deadline())
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "nquads");
    let statements = statements_for_file(document, file.id);
    assert_eq!(statements.len(), 4);
    for statement in &statements {
        assert_eq!(statement.format(), RdfStatementFormat::NQuads);
        assert_eq!(statement.source().generation(), receipt.generation);
        assert_eq!(statement.source().content_hash(), content_hash(source));
        assert_eq!(statement.predicate().iri(), Some("urn:p"));
        let span = statement.source().span();
        let text = std::str::from_utf8(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
        )
        .unwrap();
        assert_eq!(statement.source_text(), text);
        statement.verify_source_text(text).unwrap();
        for term in [
            Some(statement.subject()),
            Some(statement.predicate()),
            Some(statement.object()),
            statement.graph(),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(term.source().generation(), receipt.generation);
            assert_eq!(term.source().span().file(), file.id);
        }
    }
    assert_eq!(statements[0].object().language(), Some("en"));
    assert_eq!(statements[0].graph().unwrap().iri(), Some("urn:g"));
    assert_eq!(
        statements[1].graph().unwrap().blank_node_label(),
        Some("graph")
    );
    assert_eq!(statements[2].object().datatype(), Some("urn:type"));
    assert!(statements[2].graph().is_none());
    assert!(
        statements[0]
            .subject()
            .same_blank_node(statements[1].subject())
    );
    assert!(
        statements[1]
            .graph()
            .unwrap()
            .same_blank_node(statements[3].subject())
    );
    assert!(
        statements[3]
            .object()
            .same_blank_node(statements[0].subject())
    );
    let peer_statements = statements_for_file(document, peer.id);
    assert_eq!(peer_statements.len(), 1);
    assert!(
        !statements[0]
            .subject()
            .same_blank_node(peer_statements[0].subject())
    );
    assert!(!document.entities.iter().any(|entity| {
        entity.kind != EntityKind::Module
            && entity
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

#[test]
fn nquads_graphs_survive_fresh_noop_incremental_restored_and_cold_query() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let input = repository.path().join("data.nq");
    fs::write(&input, SOURCE).unwrap();
    fs::write(repository.path().join("peer.nq"), PEER).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_native(&service, &initial, &paths, SOURCE);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(SOURCE)
        .unwrap()
        .replace("first", "changed")
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
    fs::write(&input, SOURCE).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_native(&reopened, &restored, &paths, SOURCE);
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_native(&final_open, &restored, &paths, SOURCE);
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn nquads_native_routes_preserve_original_controls_and_reject_relative_ids() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let config = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'configured.data'='nquads'\n";
    fs::write(
        repository.path().join("configured.data"),
        b"<urn:s><urn:p>\"value\0tail\"<urn:g>.\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("relative.nq"),
        b"<relative><urn:p>\"value\"<urn:g>.\n",
    )
    .unwrap();
    assert_eq!(ORIGINAL_NUL_BOUNDARY.len(), 57);
    fs::write(
        repository.path().join("literal_ascii_boundaries.nq"),
        ORIGINAL_NUL_BOUNDARY,
    )
    .unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(config), 2).unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let configured = document
        .files
        .iter()
        .find(|file| file.path == "configured.data")
        .unwrap();
    assert_eq!(configured.language, "nquads");
    let statements = statements_for_file(document, configured.id);
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].graph().unwrap().iri(), Some("urn:g"));
    let boundary = document
        .files
        .iter()
        .find(|file| file.path == "literal_ascii_boundaries.nq")
        .unwrap();
    assert_eq!(boundary.language, "nquads");
    let original_statements = statements_for_file(document, boundary.id);
    assert_eq!(original_statements.len(), 1);
    assert_eq!(original_statements[0].format(), RdfStatementFormat::NQuads);
    assert!(original_statements[0].graph().is_none());
    assert_eq!(
        original_statements[0].object().literal_value(),
        Some("\0\t\u{000b}\u{000c}\u{000e}&([]\u{007f}")
    );
    let exact = service
        .source_read(
            receipt.generation,
            vec![boundary.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(exact.data.chunks[0].bytes, ORIGINAL_NUL_BOUNDARY);
    let relative = document
        .files
        .iter()
        .find(|file| file.path == "relative.nq")
        .unwrap();
    assert_eq!(relative.language, "nquads");
    assert!(statements_for_file(document, relative.id).is_empty());
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().file() == relative.id)
    );
}
