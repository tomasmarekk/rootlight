//! JSONC routes through real discovery, structural queries and durable source reads.
//! Generation-owned evidence remains exact after mutations and a changed live file;
//! both durable source layouts preserve definitions and Contains relations.

use super::*;

const BEFORE: &str = "// jsoncheadmarker λ😀\r\n{\r\n  \"items\":[{\"id\":3,},null,{\"id\":4,},],\r\n  \"nested\":{\"id\":5,}, /* jsonctailmarker */\r\n}\r\n";

fn assert_jsonc_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
) -> BTreeSet<SymbolId> {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "settings.JSONC")
        .unwrap();
    assert_eq!(file.language, "jsonc");
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    let options = SourceReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0);
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![whole.clone()],
            options,
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, whole);
    assert_eq!(read.data.chunks[0].bytes, source.as_bytes());
    for term in ["jsoncheadmarker", "jsonctailmarker"] {
        let found = service
            .code_locate(
                receipt.generation,
                term.to_owned(),
                LocateMode::Text,
                16,
                0,
                &cancellation,
            )
            .unwrap();
        assert!(!found.data.truncated);
        assert!(
            found
                .data
                .hits
                .iter()
                .any(|hit| hit.path == "settings.JSONC"
                    && hit.language == "jsonc"
                    && hit.symbol.is_none())
        );
    }
    let located = service
        .code_locate(
            receipt.generation,
            "items".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(!located.data.truncated);
    let item = located
        .data
        .hits
        .iter()
        .find(|hit| hit.symbol.is_some() && hit.path == "settings.JSONC")
        .unwrap();
    assert_eq!(item.language, "jsonc");
    let item_id = item.symbol.unwrap();
    let explained = service
        .symbol_explain(receipt.generation, item_id, &cancellation)
        .unwrap();
    assert!(!explained.data.truncated);
    assert_eq!(explained.data.entity.kind, EntityKind::Property);
    assert_eq!(explained.data.entity.language, "jsonc");
    let definition = explained
        .data
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: item_id }
        })
        .unwrap();
    let start = usize::try_from(definition.source.span().start_byte()).unwrap();
    let end = usize::try_from(definition.source.span().end_byte()).unwrap();
    assert_eq!(&source[start..end], "\"items\"");
    assert_eq!(definition.syntactic_text_hash, content_hash(b"\"items\""));
    let exact = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![definition.source.clone()],
            options,
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(exact.data.chunks[0].bytes, b"\"items\"");
    assert_eq!(exact.data.chunks[0].reference, definition.source);
    let contained: BTreeSet<_> = explained
        .data
        .relations
        .iter()
        .filter_map(|relation| {
            if relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(item_id)
            {
                match relation.object {
                    RelationEndpoint::Entity(symbol) => Some(symbol),
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.display_name == "id" && contained.contains(&entity.id))
            .count(),
        2
    );
    document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property && entity.language == "jsonc")
        .map(|entity| entity.id)
        .collect()
}

fn jsonc_lifecycle(extra_files: usize) {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::write(fixture.path().join("settings.JSONC"), BEFORE).unwrap();
    for ordinal in 0..extra_files {
        fs::write(fixture.path().join(format!("empty-{ordinal}.txt")), b"").unwrap();
    }
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable_with_policy(
        FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap(),
        paths.state_dir(),
        &cancellation,
    )
    .unwrap();
    let first = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(
        first.discovered_inputs,
        u64::try_from(extra_files + 1).unwrap()
    );
    let initial_ids = assert_jsonc_generation(&service, &first, BEFORE);
    assert_eq!(initial_ids.len(), 5);
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        first
    );
    let changed = BEFORE.replacen(":3", ":6", 1);
    fs::write(fixture.path().join("settings.JSONC"), &changed).unwrap();
    let second = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_ne!(second.generation, first.generation);
    assert_eq!(
        initial_ids,
        assert_jsonc_generation(&service, &second, &changed)
    );
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        second
    );
    drop(service);
    fs::write(
        fixture.path().join("settings.JSONC"),
        b"live file no longer matches\n",
    )
    .unwrap();
    let reopened = FirstSliceService::new_durable_with_policy(
        FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap(),
        paths.state_dir(),
        &cancellation,
    )
    .unwrap();
    assert_eq!(
        initial_ids,
        assert_jsonc_generation(&reopened, &first, BEFORE)
    );
    assert_eq!(
        initial_ids,
        assert_jsonc_generation(&reopened, &second, &changed)
    );
}

#[test]
fn jsonc_blob_generations_keep_exact_queries_after_update_and_reopen() {
    jsonc_lifecycle(0);
}

#[test]
fn jsonc_packed_generations_keep_exact_queries_after_update_and_reopen() {
    jsonc_lifecycle(256);
}
