//! Quoted Julia names keep exact identities through public and durable queries.
//! Empty/control canonical names use nonempty authored spelling or file lookup;
//! lexical query admission remains separate from exact source and symbol access.

use super::*;
use rootlight_ir::RelationEndpoint;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const SOURCE: &str =
    "# λ😀\nvar\"\" = 1\nvar\"\" = 2\nvar\"line\nname\" = 3\nvar\"tab\tname\" = 4\ntail = 5\n";

fn exact_read(service: &FirstSliceService, reference: &SourceRef, bytes: &[u8]) {
    let response = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(response.data.generation, reference.generation());
    assert_eq!(response.data.chunks.len(), 1);
    assert_eq!(response.data.chunks[0].reference, *reference);
    assert_eq!(response.data.chunks[0].bytes, bytes);
    assert_eq!(
        response.data.chunks[0].content_hash,
        reference.content_hash()
    );
}

fn assert_quoted_names(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
) -> BTreeSet<SymbolId> {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.path, "quoted.jl");
    assert_eq!(file.language, "julia");
    assert_eq!(file.content_hash, content_hash(SOURCE.as_bytes()));
    exact_read(
        service,
        file.evidence.source.as_ref().unwrap(),
        SOURCE.as_bytes(),
    );
    let variables: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.language == "julia" && entity.kind == EntityKind::Variable)
        .collect();
    assert_eq!(variables.len(), 5);
    let symbols: BTreeSet<_> = variables.iter().map(|entity| entity.id).collect();
    assert_eq!(symbols.len(), 5);
    let located = service
        .code_locate(
            receipt.generation,
            "quoted.jl".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(!located.data.truncated);
    assert_eq!(
        located
            .data
            .hits
            .iter()
            .filter(|hit| hit.kind == "variable")
            .count(),
        5
    );
    assert_eq!(
        located
            .data
            .hits
            .iter()
            .filter(|hit| hit.kind == "variable")
            .map(|hit| hit.symbol.unwrap())
            .collect::<BTreeSet<_>>(),
        symbols
    );
    let empty = service
        .code_locate(
            receipt.generation,
            "var\"\"".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(!empty.data.truncated);
    assert_eq!(
        empty
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .count(),
        2
    );
    assert_eq!(
        empty
            .data
            .hits
            .iter()
            .filter_map(|hit| hit.symbol)
            .collect::<BTreeSet<_>>(),
        variables
            .iter()
            .filter(|entity| entity.canonical_name.is_empty())
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    assert!(
        empty
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .all(|hit| {
                hit.identifier == "var\"\"" && hit.language == "julia" && hit.path == "quoted.jl"
            })
    );
    for (written, canonical, count) in [
        ("var\"\"", "", 2),
        ("var\"line\nname\"", "line\nname", 1),
        ("var\"tab\tname\"", "tab\tname", 1),
        ("tail", "tail", 1),
    ] {
        let entities: Vec<_> = variables
            .iter()
            .filter(|entity| entity.canonical_name == canonical)
            .collect();
        assert_eq!(entities.len(), count, "{canonical:?}");
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                written,
                None,
                SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(found.stop, SourceFindStop::EndOfSource);
        assert!(found.continuation.is_none());
        assert_eq!(found.matches.len(), count);
        let mut definition_starts = BTreeSet::new();
        for entity in entities {
            assert_eq!(entity.display_name, written);
            let explained = service
                .symbol_explain(receipt.generation, entity.id, &cancellation)
                .unwrap();
            assert!(!explained.data.truncated);
            assert_eq!(explained.data.entity, **entity);
            let definitions: Vec<_> = explained
                .data
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            let definition = definitions[0];
            assert_eq!(definition.source.repository(), receipt.repository);
            assert_eq!(definition.source.generation(), receipt.generation);
            assert_eq!(definition.source.span().file(), file.id);
            assert_eq!(definition.source.content_hash(), file.content_hash);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(written.as_bytes())
            );
            assert!(found.matches.contains(&definition.source));
            assert!(definition_starts.insert(definition.source.span().start_byte()));
            exact_read(service, &definition.source, written.as_bytes());
            let owner = match entity.container.unwrap() {
                ContainerRef::Repository(_) => {
                    panic!("written Julia variable lacks a source owner")
                }
                ContainerRef::File(file) => RelationEndpoint::File(file),
                ContainerRef::Entity(symbol) => RelationEndpoint::Entity(symbol),
            };
            assert!(explained.data.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == owner
                    && edge.object == RelationEndpoint::Entity(entity.id)
                    && edge.evidence.source == entity.evidence.source
            }));
        }
        assert_eq!(definition_starts.len(), count);
    }
    for query in ["", " ", "line\nname", "tab\tname", "\0"] {
        assert_eq!(
            service.code_locate(
                receipt.generation,
                query.to_owned(),
                LocateMode::Exact,
                16,
                0,
                &cancellation,
            ),
            Err(FirstSliceError::Search),
            "{query:?} must remain an explicit invalid lexical query"
        );
    }
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.detail == "declaration-name-unavailable" && gap.source.span().file() == file.id
    }));
    symbols
}

#[test]
fn julia_empty_and_control_names_survive_public_lookup_and_durable_reopen() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("quoted.jl"), SOURCE).unwrap();
    let mut service =
        FirstSliceService::new_durable(3, paths.state_dir(), &Cancellation::new()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &Cancellation::new())
        .unwrap();
    assert_eq!(receipt.discovered_inputs, 1);
    assert!(receipt.discovery_complete);
    assert_eq!(receipt.indexed_files, 1);
    let symbols = assert_quoted_names(&service, &receipt);
    let noop = service
        .index_repository(repository.path(), &Cancellation::new())
        .unwrap();
    assert_eq!(noop, receipt);
    drop(service);
    fs::write(
        repository.path().join("quoted.jl"),
        "unrelated_live_replacement = 0\n",
    )
    .unwrap();
    let mut reopened =
        FirstSliceService::new_durable(3, paths.state_dir(), &Cancellation::new()).unwrap();
    assert_eq!(assert_quoted_names(&reopened, &receipt), symbols);
    fs::write(repository.path().join("quoted.jl"), SOURCE).unwrap();
    let noop = reopened
        .index_repository(repository.path(), &Cancellation::new())
        .unwrap();
    assert_eq!(noop, receipt);
    assert_eq!(assert_quoted_names(&reopened, &receipt), symbols);
}

#[test]
fn julia_nul_source_is_an_explicit_binary_exclusion_and_keeps_ordinary_neighbors() {
    let repository = durable_test_tempdir();
    fs::write(
        repository.path().join("invalid.jl"),
        b"var\"bad\0name\" = 1\n",
    )
    .unwrap();
    fs::write(repository.path().join("ordinary.jl"), "ordinary = 2\n").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &Cancellation::new())
        .unwrap();
    assert!(receipt.discovery_complete);
    assert_eq!(receipt.binary_inputs, 1);
    assert_eq!(receipt.discovered_inputs, 1);
    assert_eq!(receipt.indexed_files, 1);
    let located = service
        .code_locate(
            receipt.generation,
            "ordinary".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!located.data.truncated);
    let hits: Vec<_> = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol.is_some())
        .collect();
    assert_eq!(hits.len(), 1);
    let explained = service
        .symbol_explain(
            receipt.generation,
            hits[0].symbol.unwrap(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(explained.data.entity.canonical_name, "ordinary");
    assert_eq!(explained.data.entity.display_name, "ordinary");
    let definitions: Vec<_> = explained
        .data
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == OccurrenceTarget::Resolved {
                        symbol: hits[0].symbol.unwrap(),
                    }
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    exact_read(&service, &definitions[0].source, b"ordinary");
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(snapshot.document().entities.iter().all(|entity| {
        !entity.canonical_name.contains('\0') && !entity.display_name.contains('\0')
    }));
}
