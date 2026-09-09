//! Astro host discovery through durable indexing and public query operations.
//! Embedded symbols retain host bytes and generation identity across cache reuse.

use super::*;

#[test]
fn astro_sources_survive_incremental_publication_and_restart() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let source = concat!(
        "---\r\nfunction greet(name: string) { return name; }\r\n---\r\n",
        "<main title='é'>{greet('first')}",
        "<script>function greet(name: string) { return name + '!'; }</script>",
        "<style>.card { color: red; }</style></main>\r\n",
    );
    fs::write(fixture.path().join("view.ASTRO"), source).unwrap();
    fs::write(
        fixture.path().join("companion.rs"),
        "pub fn companion() {}\n",
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let initial = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(initial.indexed_files, 2);
    let status = service.repository_status(initial.repository, None).unwrap();
    let coverage = status
        .coverage
        .iter()
        .find(|row| row.language == "astro")
        .unwrap();
    assert_eq!((coverage.discovered_files, coverage.indexed_files), (1, 1));
    assert_eq!(coverage.tier, "tier_d");
    assert_ne!(coverage.status, "complete");
    let capabilities = service.support_inventory_snapshot().unwrap();
    let capability = capabilities
        .languages
        .iter()
        .find(|row| row.language == "astro")
        .unwrap();
    assert_eq!(capability.analyzers, ["treesitter"]);
    assert_eq!(capability.maximum_tier, "tier_d");
    let no_op = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(no_op.generation, initial.generation);
    let changed = source.replace("'first'", "'second'");
    fs::write(fixture.path().join("view.ASTRO"), &changed).unwrap();
    let updated = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, initial.generation);
    let evidence = service.incremental_evidence(updated.generation).unwrap();
    assert_eq!(evidence.parsed_files(), 1);
    assert_eq!(evidence.reused_parser_artifacts(), 1);
    assert_fresh_equivalent(
        &service,
        fixture.path(),
        initial.generation,
        &updated,
        &deadline(),
    );
    drop(service);
    let mut restored = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let mut previous_symbols = None;
    for (receipt, expected) in [(&initial, source), (&updated, changed.as_str())] {
        let snapshot = restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "view.ASTRO")
            .unwrap();
        assert_eq!(file.language, "astro");
        let reference = file.evidence.source.clone().unwrap();
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(expected.as_bytes()));
        let read = restored
            .source_read(receipt.generation, vec![reference], &deadline())
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, expected.as_bytes());
        assert_eq!(read.data.chunks[0].language, "astro");
        let symbols: BTreeSet<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "greet"
            })
            .map(|entity| entity.id)
            .collect();
        assert_eq!(symbols.len(), 2);
        if let Some(previous) = previous_symbols {
            assert_eq!(symbols, previous);
        }
        previous_symbols = Some(symbols.clone());
        let located = restored
            .code_locate(
                receipt.generation,
                "greet".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &deadline(),
            )
            .unwrap();
        let located_symbols: BTreeSet<_> = located
            .data
            .hits
            .iter()
            .filter_map(|hit| hit.symbol)
            .collect();
        assert_eq!(located_symbols, symbols);
        assert!(!located.data.truncated);
        for symbol in symbols {
            let explained = restored
                .symbol_explain(receipt.generation, symbol, &deadline())
                .unwrap();
            assert_eq!(explained.data.entity.language, "typescript");
            let reference = explained.data.entity.evidence.source.unwrap();
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), content_hash(expected.as_bytes()));
            let start = usize::try_from(reference.span().start_byte()).unwrap();
            let end = usize::try_from(reference.span().end_byte()).unwrap();
            let read = restored
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(
                read.data.chunks[0].bytes,
                expected.as_bytes().get(start..end).unwrap()
            );
        }
        for (query, mode) in [
            ("view.ASTRO", LocateMode::Text),
            ("color: red", LocateMode::Text),
        ] {
            let located = restored
                .code_locate_with_languages_and_budget(
                    receipt.generation,
                    query.to_owned(),
                    mode,
                    vec!["astro".to_owned()],
                    8,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert!(!located.data.hits.is_empty(), "{query}: {located:?}");
            assert!(located.data.hits.iter().all(|hit| {
                hit.source
                    .as_ref()
                    .is_some_and(|source| source.generation() == receipt.generation)
            }));
        }
    }
    let retained = restored
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(retained.generation, updated.generation);
}
