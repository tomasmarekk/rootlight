//! Large readable sources keep exact generation-owned bytes through structural
//! analysis and persistence. Exercise both durable layouts and the public read path.

use super::*;

fn large_source_bytes() -> Vec<u8> {
    let mut bytes = vec![b' '; 16 * 1024 * 1024 + 1];
    let head = b"// largeheadmarker\n";
    bytes[..head.len()].copy_from_slice(head);
    bytes.extend_from_slice(b"\n// largetailmarker\npub fn large_definition() {}\n");
    bytes
}

fn assert_large_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    bytes: &[u8],
) {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "large.rs")
        .unwrap();
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    assert_eq!(file.language, "rust");
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.display_name == "large_definition")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.display_name == "small_definition")
    );
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    for term in ["largeheadmarker", "largetailmarker"] {
        let located = service
            .code_locate(
                receipt.generation,
                term.to_owned(),
                LocateMode::Text,
                10,
                0,
                &cancellation,
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.path == "large.rs" && hit.symbol.is_none())
        );
        assert!(!located.data.truncated);
    }
    let options = SourceReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0);
    let tail_start = file.byte_length - 47;
    let tail = SourceRef::new(
        whole.repository(),
        whole.generation(),
        SourceSpan::new(file.id, tail_start, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![tail.clone()],
            options,
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        read.data.chunks[0].bytes,
        bytes[usize::try_from(tail_start).unwrap()..]
    );
    assert_eq!(read.data.chunks[0].reference, tail);
    let mut reduced = FirstSliceBudget::default();
    reduced.source = reduced.source.with_max_snapshot_bytes(16 * 1024 * 1024);
    assert!(
        service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![tail],
                options,
                reduced,
                &cancellation
            )
            .is_err()
    );
    assert!(
        service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![whole.clone()],
                options,
                FirstSliceBudget::default(),
                &cancellation
            )
            .is_err()
    );
}

fn assert_large_source_layout(extra_files: usize) {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let bytes = large_source_bytes();
    fs::write(fixture.path().join("large.rs"), &bytes).unwrap();
    fs::write(
        fixture.path().join("small.rs"),
        b"pub fn small_definition() {}\n",
    )
    .unwrap();
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
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(
        receipt.discovered_inputs,
        u64::try_from(extra_files + 2).unwrap()
    );
    assert_large_source(&service, &receipt, &bytes);
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        receipt
    );
    let updated = if extra_files == 0 {
        let mut changed = bytes.clone();
        changed[8 * 1024 * 1024] = b'\t';
        fs::write(fixture.path().join("large.rs"), &changed).unwrap();
        let update = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        assert_ne!(update.generation, receipt.generation);
        assert_large_source(&service, &update, &changed);
        assert_eq!(
            service
                .index_repository(fixture.path(), &cancellation)
                .unwrap(),
            update
        );
        Some((update, changed))
    } else {
        None
    };
    fs::write(
        fixture.path().join("large.rs"),
        b"live file changed after publication\n",
    )
    .unwrap();
    drop(service);
    let restored = FirstSliceService::new_durable_with_policy(
        FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap(),
        paths.state_dir(),
        &cancellation,
    )
    .unwrap();
    if let Some((update, changed)) = updated {
        assert_large_source(&restored, &update, &changed);
    }
    assert_large_source(&restored, &receipt, &bytes);
    let file = restored
        .loaded_generation_snapshot(receipt.generation)
        .unwrap()
        .document()
        .files
        .iter()
        .find(|file| file.path == "large.rs")
        .unwrap()
        .clone();
    let snapshot = restored
        .durable
        .as_ref()
        .unwrap()
        .read_source(receipt.repository, receipt.generation, &file, &cancellation)
        .unwrap();
    assert_eq!(snapshot.content(), bytes);
    let repository_directory = paths
        .state_dir()
        .join("first-slice")
        .join("repositories")
        .join(receipt.repository.to_string());
    let payload = if extra_files == 0 {
        repository_directory
            .join("source-blobs")
            .join(file.content_hash.to_string())
            .join("content")
    } else {
        fs::read_dir(
            repository_directory
                .join(receipt.generation.to_string())
                .join("sources"),
        )
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| fs::metadata(path).unwrap().len() == u64::try_from(bytes.len()).unwrap())
        .expect("the larger source owns one complete durable pack")
    };
    assert_eq!(fs::read(&payload).unwrap(), bytes);
    let mut corrupted = bytes;
    *corrupted.last_mut().unwrap() ^= 1;
    fs::write(payload, corrupted).unwrap();
    let durable = restored.durable.as_ref().unwrap();
    assert_eq!(
        durable.storage_inventory(),
        Err(FirstSliceError::CatalogCorrupt)
    );
    assert_eq!(
        durable.read_source(receipt.repository, receipt.generation, &file, &cancellation),
        Err(FirstSliceError::CatalogCorrupt)
    );
}

#[test]
fn large_sources_survive_blob_reopen_and_narrow_reads() {
    assert_large_source_layout(0);
}

#[test]
fn large_sources_survive_packed_reopen_and_narrow_reads() {
    assert_large_source_layout(256);
}

#[test]
fn wide_native_tree_preserves_tail_definition_and_source() {
    let fixture = durable_test_tempdir();
    let mut source = String::from("{ values = [ ");
    for _ in 0..1_048_704 {
        source.push_str("0 ");
    }
    source.push_str("] ; retained_tail = 1; }\n");
    fs::write(fixture.path().join("wide.nix"), source.as_bytes()).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "wide.nix")
        .unwrap();
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    let located = service
        .code_locate(
            receipt.generation,
            "retained_tail".to_owned(),
            LocateMode::Exact,
            10,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(!located.data.truncated);
    assert!(
        located.data.hits.iter().any(|hit| hit.symbol.is_some()),
        "a wide native tree must retain the real tail binding"
    );
    let start = u64::try_from(source.find("retained_tail").unwrap()).unwrap();
    let reference = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, start, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].reference, reference);
    assert_eq!(
        read.data.chunks[0].bytes,
        source.as_bytes()[usize::try_from(start).unwrap()..]
    );
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        receipt
    );
}
