//! Default GraphQL routing must come from native operations in physical `.query` bytes.
//! Original fixtures are from Dgraph c587cba75f60445125459b46b4d81757a73384aa;
//! Apache-2.0 license is retained in native-grammars/dql/tests/fixtures/LICENSE.txt.

use super::*;
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationReadLimit, GenerationReader,
    OccurrenceReadRequest,
};

// graphql/testdata/custom_bench/profiling/benchmarks/0/queries/{00,01}.query.
const ORIGINALS: [(&str, &[u8]); 2] = [
    ("00.query", b"query {\n\tqueryCuisine {\n\t\tid\n\t\tname\n\t}\n}\n"),
    ("01.query", b"query {\n\tqueryCuisine {\n\t\tid\n\t\tname\n\t\trestaurants {\n\t\t\tname\n\t\t\trating\n\t\t}\n\t}\n}\n"),
];

#[test]
fn original_explicit_operations_admit_default_query_sources_with_written_references() {
    let fixture = durable_test_tempdir();
    for (path, bytes) in ORIGINALS {
        fs::write(fixture.path().join(path), bytes).unwrap();
    }
    assert_eq!(ORIGINALS[0].1.len(), 41);
    assert_eq!(ORIGINALS[1].1.len(), 79);
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    for (path, bytes) in ORIGINALS {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "graphql", "{path}");
        assert_eq!(file.content_hash, content_hash(bytes));
        let source = file.evidence.source.as_ref().unwrap();
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.span().start_byte(), 0);
        assert_eq!(
            source.span().end_byte(),
            u64::try_from(bytes.len()).unwrap()
        );
        let read = service
            .source_read(receipt.generation, vec![source.clone()], &deadline())
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, bytes);
        assert_eq!(read.data.chunks[0].language, "graphql");
        let written = document.occurrences.iter().any(|site| {
            let span = site.source.span();
            span.file() == file.id
                && usize::try_from(span.start_byte())
                    .ok()
                    .zip(usize::try_from(span.end_byte()).ok())
                    .and_then(|(start, end)| bytes.get(start..end))
                    == Some(b"queryCuisine".as_slice())
                && site.role == OccurrenceRole::Reference
        });
        assert!(written, "native written reference missing from {path}");
        assert!(!document.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "unsupported-language"
                && diagnostic
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        }));
    }
}

#[test]
fn named_operations_require_complete_native_documents_without_keyword_decoys() {
    for (text, accepted) in [
        ("query Named { field }\n", true),
        ("mutation Named { update }\n", true),
        ("subscription Named { changed }\n", true),
        ("{ field }\n", false),
        ("# query { decoy }\n{ field }\n", false),
        ("\"query { decoy }\"\n", false),
        ("query { field } trailing\n", false),
        ("query { field\n", false),
        ("{ q(func: uid(1)) { name } }\n", false),
        ("SELECT field FROM source;\n", false),
    ] {
        let fixture = durable_test_tempdir();
        fs::write(fixture.path().join("operation.query"), text).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let file = &snapshot.document().files[0];
        assert_eq!(file.language == "graphql", accepted, "{text}");
        let source = file.evidence.source.as_ref().unwrap();
        let read = service
            .source_read(receipt.generation, vec![source.clone()], &deadline())
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, text.as_bytes());
    }
}

fn probe_fixture(bytes: &[u8]) -> (RepositoryId, GenerationId, ManifestInput, SourceSnapshot) {
    let repository = RepositoryId::from_bytes([9; 16]);
    let generation = GenerationId::from_bytes([8; 20]);
    let path = RelativePath::parse(Path::new("source.query")).unwrap();
    let file = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let hash = content_hash(bytes);
    let snapshot =
        SourceSnapshot::from_persisted(repository, path, file, hash, bytes.to_vec()).unwrap();
    let input = ManifestInput {
        file,
        path: "source.query".to_owned(),
        content_hash: hash,
        bytes: u64::try_from(bytes.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: Vec::new(),
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    };
    (repository, generation, input, snapshot)
}

#[test]
fn probe_preserves_existing_owners_and_rejects_foreign_sources_providers_and_size() {
    use crate::graphql_query_admission::{Admission, probe};
    let mut service = FirstSliceService::new(2).unwrap();
    let (repository, generation, input, snapshot) = probe_fixture(ORIGINALS[0].1);
    let cancellation = deadline();
    assert!(matches!(
        probe(
            &service,
            repository,
            generation,
            &input,
            &snapshot,
            &cancellation
        )
        .unwrap(),
        Admission::Accepted {
            memory_admission:
                rootlight_adapter_sdk::MemoryAdmissionStatus::UnavailableEnforcementFallback,
            ..
        }
    ));
    for evidence in [
        LanguageEvidence::Configuration,
        LanguageEvidence::Content,
        LanguageEvidence::Project,
    ] {
        let mut owned = input.clone();
        owned
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "sql".to_owned(),
                evidence,
            });
        assert!(matches!(
            probe(
                &service,
                repository,
                generation,
                &owned,
                &snapshot,
                &cancellation
            )
            .unwrap(),
            Admission::NotAttempted
        ));
    }
    let mut owned = input.clone();
    owned
        .project_language_witnesses
        .push(rootlight_discovery::ProjectLanguageWitness {
            language: "fsharp".to_owned(),
            project_file: FileId::from_bytes([7; 20]),
            project_path: "owner.fsproj".to_owned(),
            project_content_hash: content_hash(b"project-owner"),
        });
    assert!(matches!(
        probe(
            &service,
            repository,
            generation,
            &owned,
            &snapshot,
            &cancellation
        )
        .unwrap(),
        Admission::NotAttempted
    ));
    for fault in ["file", "path", "hash", "bytes"] {
        let mut foreign = input.clone();
        match fault {
            "file" => foreign.file = FileId::from_bytes([6; 20]),
            "path" => foreign.path = "other.query".to_owned(),
            "hash" => foreign.content_hash = content_hash(b"other-source"),
            _ => foreign.bytes += 1,
        }
        assert!(
            matches!(
                probe(
                    &service,
                    repository,
                    generation,
                    &foreign,
                    &snapshot,
                    &cancellation
                ),
                Err(FirstSliceError::Identity)
            ),
            "{fault}"
        );
    }
    let large = [vec![b' '; 64 * 1024], ORIGINALS[0].1.to_vec()].concat();
    let (repository, generation, large_input, large_snapshot) = probe_fixture(&large);
    assert!(matches!(
        probe(
            &service,
            repository,
            generation,
            &large_input,
            &large_snapshot,
            &cancellation
        )
        .unwrap(),
        Admission::ResourceBounded
    ));
    let foreign_service = FirstSliceService::new(2).unwrap();
    let wrong = Arc::clone(foreign_service.structural_parsers.get("graphql").unwrap());
    service
        .structural_parsers
        .insert("graphql".to_owned(), wrong);
    assert!(matches!(
        probe(
            &service,
            repository,
            generation,
            &input,
            &snapshot,
            &cancellation
        ),
        Err(FirstSliceError::Identity)
    ));
}

#[test]
fn native_probe_never_admits_source_node_depth_or_fact_truncation() {
    use crate::graphql_query_admission::{Admission, probe};
    use rootlight_adapter_sdk::{AnalysisLimits, BatchThresholds, StreamLimits};
    let (repository, generation, input, snapshot) = probe_fixture(ORIGINALS[0].1);
    for bound in ["source", "nodes", "depth", "facts"] {
        let mut service = FirstSliceService::new(2).unwrap();
        let limits = &service.analysis_limits;
        let syntax = limits.syntax_stream();
        let batch = syntax.batch();
        let stream = if bound == "facts" {
            StreamLimits::new(
                syntax.max_batches(),
                3,
                syntax.max_output_bytes(),
                syntax.max_diagnostics(),
                syntax.max_diagnostic_bytes(),
                syntax.max_string_bytes(),
                BatchThresholds::new(
                    3,
                    batch.max_output_bytes(),
                    batch.max_diagnostics(),
                    batch.max_diagnostic_bytes(),
                )
                .unwrap(),
            )
            .unwrap()
        } else {
            syntax.clone()
        };
        service.analysis_limits = AnalysisLimits::new(
            if bound == "source" {
                40
            } else {
                limits.max_source_bytes()
            },
            if bound == "nodes" {
                1
            } else {
                limits.max_syntax_nodes()
            },
            if bound == "depth" {
                1
            } else {
                limits.max_syntax_depth()
            },
            limits.max_embedded_ranges(),
            limits.max_reported_memory_bytes(),
            stream,
            limits.ir_stream().clone(),
            limits.ir().clone(),
        )
        .unwrap();
        let result = probe(
            &service,
            repository,
            generation,
            &input,
            &snapshot,
            &deadline(),
        );
        assert!(
            matches!(
                result,
                Ok(Admission::ResourceBounded | Admission::NoEvidence)
            ),
            "{bound}: {result:?}"
        );
    }
}

fn assert_durable_graphql(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    first: &[u8],
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    let cancellation = deadline();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
    for (index, (path, original)) in ORIGINALS.into_iter().enumerate() {
        let bytes = if index == 0 { first } else { original };
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "graphql");
        assert_eq!(file.content_hash, content_hash(bytes));
        assert_eq!(reader.file(file.id, &context).unwrap().as_ref(), Some(file));
        let source = file.evidence.source.as_ref().unwrap();
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.span().start_byte(), 0);
        assert_eq!(
            source.span().end_byte(),
            u64::try_from(bytes.len()).unwrap()
        );
        let read = service
            .source_read(receipt.generation, vec![source.clone()], &deadline())
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, bytes);
        assert_eq!(read.data.chunks[0].language, "graphql");
        let mut expected = document
            .occurrences
            .iter()
            .filter(|site| site.source.span().file() == file.id)
            .cloned()
            .collect::<Vec<_>>();
        expected.sort_by_key(|site| site.id);
        let mut cold = Vec::new();
        let mut after = None;
        loop {
            let request = OccurrenceReadRequest::new(file.id, GenerationReadLimit::new(1).unwrap());
            let request = after.map_or(request, |cursor| request.with_after(cursor));
            let page = reader.occurrences(&request, &context).unwrap();
            cold.extend_from_slice(page.items());
            let Some(next) = page.next_cursor() else {
                break;
            };
            assert!(after.is_none_or(|old| next > old));
            after = Some(next);
        }
        assert_eq!(cold, expected);
        assert!(!cold.is_empty());
        for site in &cold {
            assert_eq!(site.source.repository(), receipt.repository);
            assert_eq!(site.source.generation(), receipt.generation);
            assert_eq!(site.source.content_hash(), content_hash(bytes));
            assert_eq!(site.source.span().file(), file.id);
            assert!(site.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
        }
        let name = if index == 0 && bytes != original {
            b"queryRestaurants".as_slice()
        } else {
            b"queryCuisine".as_slice()
        };
        assert!(cold.iter().any(|site| {
            let span = site.source.span();
            bytes.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            ) == Some(name)
                && site.role == OccurrenceRole::Reference
        }));
    }
}

#[test]
fn default_queries_survive_changed_body_restore_reopen_and_cold_pages() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    for (path, bytes) in ORIGINALS {
        fs::write(repository.path().join(path), bytes).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_durable_graphql(&service, &initial, &paths, ORIGINALS[0].1);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINALS[0].1)
        .unwrap()
        .replace("queryCuisine", "queryRestaurants")
        .into_bytes();
    fs::write(repository.path().join(ORIGINALS[0].0), &changed).unwrap();
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
    assert_durable_graphql(&service, &updated, &paths, &changed);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_durable_graphql(&reopened, &updated, &paths, &changed);
    fs::write(repository.path().join(ORIGINALS[0].0), ORIGINALS[0].1).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_durable_graphql(&reopened, &restored, &paths, ORIGINALS[0].1);
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_durable_graphql(&final_open, &restored, &paths, ORIGINALS[0].1);
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn preceding_unknown_cache_is_reparsed_with_current_native_language_policy() {
    struct PrecedingPolicy;
    impl Drop for PrecedingPolicy {
        fn drop(&mut self) {
            crate::graphql_query_admission::PREDECESSOR_POLICY.with(|flag| flag.set(false));
        }
    }
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    for (path, bytes) in ORIGINALS {
        fs::write(repository.path().join(path), bytes).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let (previous, previous_provider) = {
        crate::graphql_query_admission::PREDECESSOR_POLICY.with(|flag| flag.set(true));
        let _guard = PrecedingPolicy;
        let parser = first_slice_parser_provider_hash().unwrap();
        let expected = hash_static_components(&[
            PROVIDER_SET_SEED,
            parser.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            RESOLVER_BINARY_SEED,
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            b"rootlight.first-slice.language-disposition/15",
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_eq!(first_slice_provider_set_hash().unwrap(), expected);
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let previous = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(previous.generation)
            .unwrap();
        assert_eq!(snapshot.document().files.len(), 2);
        assert!(
            snapshot
                .document()
                .files
                .iter()
                .all(|file| file.language == "unknown")
        );
        assert!(snapshot.document().occurrences.is_empty());
        for file in &snapshot.document().files {
            let provenance = snapshot
                .document()
                .provenance
                .iter()
                .find(|p| p.id == file.provenance)
                .unwrap();
            assert_eq!(
                provenance.binary_digest,
                content_hash(b"rootlight.first-slice.language-disposition/15")
            );
        }
        (previous, expected)
    };
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous_provider);
    let mut current =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let requalified = current
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(requalified.generation, previous.generation);
    assert_eq!(
        current
            .incremental_evidence(requalified.generation)
            .unwrap()
            .parsed_files(),
        2
    );
    assert_durable_graphql(&current, &requalified, &paths, ORIGINALS[0].1);
    drop(current);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_durable_graphql(&reopened, &requalified, &paths, ORIGINALS[0].1);
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        requalified.generation
    );
}
