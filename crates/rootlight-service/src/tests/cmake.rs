//! CMake discovery and public queries retain written-source evidence durably.
//! Reopening generations must preserve exact old bytes and physical containment
//! even when a live file changes and command spelling matches a declaration.

use super::*;

const BEFORE: &str = "# cmakeheadmarker λ😀\r\nfunction(Written first)\r\n set(local \"${first}\")\r\nendfunction()\r\nmacro(Forward arg)\r\n Written(\"${arg}\")\r\nendmacro()\r\nset(normal first_value)\r\nadd_library(written_target INTERFACE)\r\nWritten(hello)\r\n# cmaketailmarker\r\n";

fn assert_generation(
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
        .find(|file| file.path == "CMakeLists.txt")
        .unwrap();
    assert_eq!(file.language, "cmake");
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
    for term in ["cmakeheadmarker", "cmaketailmarker"] {
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
                .any(|hit| hit.path == "CMakeLists.txt"
                    && hit.language == "cmake"
                    && hit.symbol.is_none())
        );
    }
    let located = service
        .code_locate(
            receipt.generation,
            "Written".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(!located.data.truncated);
    let hits = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol.is_some() && hit.path == "CMakeLists.txt")
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1);
    let symbol = hits[0].symbol.unwrap();
    assert_eq!(hits[0].language, "cmake");
    let explained = service
        .symbol_explain(receipt.generation, symbol, &cancellation)
        .unwrap();
    assert!(!explained.data.truncated);
    assert_eq!(explained.data.entity.kind, EntityKind::Function);
    let definition = explained
        .data
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol }
        })
        .unwrap();
    assert_eq!(definition.syntactic_text_hash, content_hash(b"Written"));
    let exact = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![definition.source.clone()],
            options,
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(exact.data.chunks[0].bytes, b"Written");
    assert_eq!(exact.data.chunks[0].reference, definition.source);
    let contained = explained
        .data
        .relations
        .iter()
        .filter_map(|relation| {
            if relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(symbol)
                && let RelationEndpoint::Entity(child) = relation.object
            {
                return Some(child);
            }
            None
        })
        .collect::<BTreeSet<_>>();
    for (name, kind) in [
        ("first", EntityKind::Parameter),
        ("local", EntityKind::Variable),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name
                    && entity.kind == kind
                    && contained.contains(&entity.id))
        );
    }
    for occurrence in document.occurrences.iter().filter(|occurrence| {
        occurrence.syntax_kind.starts_with("cmake.")
            && matches!(
                occurrence.role,
                OccurrenceRole::CallSite | OccurrenceRole::Reference | OccurrenceRole::ImportUse
            )
    }) {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite
                && occurrence.syntactic_text_hash == content_hash(b"Written"))
            .count(),
        2
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cmake-command-dispatch-target-unavailable")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cmake-macro-expansion-and-placeholder-binding-unavailable")
    );
    document
        .entities
        .iter()
        .filter(|entity| entity.language == "cmake")
        .map(|entity| entity.id)
        .collect()
}

fn lifecycle(extra_files: usize) {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::write(fixture.path().join("CMakeLists.txt"), BEFORE).unwrap();
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
    let ids = assert_generation(&service, &first, BEFORE);
    assert_eq!(ids.len(), 8);
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        first
    );
    let changed = BEFORE.replace("first_value", "changed_value");
    fs::write(fixture.path().join("CMakeLists.txt"), &changed).unwrap();
    let second = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_ne!(second.generation, first.generation);
    assert_eq!(ids, assert_generation(&service, &second, &changed));
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        second
    );
    drop(service);
    fs::write(
        fixture.path().join("CMakeLists.txt"),
        b"unrelated live bytes\n",
    )
    .unwrap();
    let reopened = FirstSliceService::new_durable_with_policy(
        FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap(),
        paths.state_dir(),
        &cancellation,
    )
    .unwrap();
    assert_eq!(ids, assert_generation(&reopened, &first, BEFORE));
    assert_eq!(ids, assert_generation(&reopened, &second, &changed));
}

#[test]
fn cmake_blob_generations_keep_exact_queries_after_update_and_reopen() {
    lifecycle(0);
}

#[test]
fn cmake_packed_generations_keep_exact_queries_after_update_and_reopen() {
    lifecycle(256);
}

#[test]
fn cmake_option_template_queries_keep_written_definition_and_cache_state_gap() {
    let fixture = durable_test_tempdir();
    let source = "option(LIBUV_BUILD_SHARED \"Build shared lib\" ON)\r\nmessage(\"$CACHE{LIBUV_BUILD_SHARED}\")\r\n";
    fs::write(fixture.path().join("CMakeLists.txt.in"), source).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(receipt.discovered_inputs, 1);
    let located = service
        .code_locate(
            receipt.generation,
            "LIBUV_BUILD_SHARED".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(!located.data.truncated);
    let hits = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol.is_some())
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1);
    let hit = hits[0];
    assert_eq!(hit.path, "CMakeLists.txt.in");
    assert_eq!(hit.language, "cmake");
    let symbol = hit.symbol.unwrap();
    let header = hit.signature.as_ref().unwrap();
    assert_eq!(header.subject(), rootlight_ir::FactRef::Entity(symbol));
    assert_eq!(header.kind(), rootlight_ir::LexicalEvidenceKind::Signature);
    assert!(!header.is_truncated());
    assert_eq!(
        header.text(),
        "option(LIBUV_BUILD_SHARED \"Build shared lib\" ON)"
    );
    header
        .verify_source_bytes(header.text().as_bytes())
        .unwrap();
    let explained = service
        .symbol_explain(receipt.generation, symbol, &cancellation)
        .unwrap();
    assert!(!explained.data.truncated);
    assert_eq!(explained.data.entity.kind, EntityKind::Variable);
    assert_eq!(explained.data.signature.as_ref(), Some(header));
    let definitions = explained
        .data
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol }
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![definitions[0].source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, definitions[0].source);
    assert_eq!(read.data.chunks[0].bytes, b"LIBUV_BUILD_SHARED");
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let module = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Module && entity.canonical_name == "CMakeLists.txt.in"
        })
        .unwrap();
    assert!(explained.data.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Contains
            && relation.subject == RelationEndpoint::Entity(module.id)
            && relation.object == RelationEndpoint::Entity(symbol)
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.domain == rootlight_ir::FactDomain::Relations
            && gap.detail == "cmake-option-initialization-and-effective-cache-state-unavailable"
    }));
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.syntax_kind == "cmake.cache_variable.reference"
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
}
