//! Public source availability is independent of parser snapshot memory.
//! Real private captures and durable blobs retain physical bytes after live
//! replacement; bounded reads and literal matches never imply deeper analysis.

use std::io::Write as _;

use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindCursor, SourceFindStop};

use super::*;

fn policy(maximum_snapshot_bytes: u64) -> FirstSliceStoragePolicy {
    let text = format!(
        "version = \"1.1\"\n[analysis]\nmax_source_file_bytes = {maximum_snapshot_bytes}\n"
    );
    let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: &text,
    }])
    .unwrap();
    FirstSliceStoragePolicy::from_config(config, 3)
}

fn encoded(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf-8" => text.as_bytes().to_vec(),
        "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
        "utf-16le" => std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect(),
        "utf-16be" => std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(u16::to_be_bytes)
            .collect(),
        _ => unreachable!("fixture encoding is closed"),
    }
}

fn check_sources(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    inputs: &[(String, Vec<u8>, &str)],
) {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    for (path, bytes, encoding) in inputs {
        let file = snapshot
            .document()
            .files
            .iter()
            .chain(
                snapshot
                    .source_files()
                    .entries()
                    .iter()
                    .map(|entry| entry.file()),
            )
            .find(|file| file.path == *path)
            .unwrap();
        assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(bytes));
        assert_eq!(file.encoding, *encoding);
        let reference = file.evidence.source.as_ref().unwrap().clone();
        let found = service
            .code_locate(
                receipt.generation,
                path.clone(),
                LocateMode::Exact,
                10,
                0,
                &cancellation,
            )
            .unwrap();
        assert!(
            found
                .data
                .hits
                .iter()
                .any(|hit| hit.path == *path && hit.symbol.is_none())
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, *bytes);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert!(
            service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::new()
                        .reduce_max_source_verification_bytes(file.byte_length - 1),
                    &cancellation
                )
                .is_err()
        );
        if *encoding == "bytes" {
            let path_result = service
                .code_locate_with_filters_and_budget(
                    receipt.generation,
                    path.clone(),
                    LocateMode::Prefix,
                    Vec::new(),
                    vec![path.clone()],
                    10,
                    0,
                    FirstSliceBudget::new(),
                    &cancellation,
                )
                .unwrap();
            assert!(!path_result.data.hits.is_empty());
            assert!(path_result.data.hits.iter().all(|hit| hit.path == *path));
            assert_eq!(
                service
                    .code_locate_with_filters_and_budget(
                        receipt.generation,
                        "marker".to_owned(),
                        LocateMode::Text,
                        Vec::new(),
                        vec![path.clone()],
                        10,
                        0,
                        FirstSliceBudget::new(),
                        &cancellation,
                    )
                    .unwrap_err(),
                FirstSliceError::SourceEncodingUnsupported
            );
            assert!(
                service
                    .source_read_lossless_text_with_options_and_budget(
                        receipt.generation,
                        vec![reference],
                        LosslessTextReadOptions::new()
                            .with_context_lines_before(0)
                            .with_context_lines_after(0),
                        FirstSliceBudget::new(),
                        &cancellation
                    )
                    .is_err()
            );
            assert!(
                service
                    .find_literal(
                        receipt.generation,
                        file.id,
                        "marker",
                        None,
                        SourceFindBudget::new(),
                        &cancellation
                    )
                    .is_err()
            );
            continue;
        }
        let text = service
            .source_read_lossless_text_with_options_and_budget(
                receipt.generation,
                vec![reference],
                LosslessTextReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert!(text.data.chunks[0].text().contains("λ😀marker"));
        let mut find_budget = SourceFindBudget::new();
        find_budget.matcher.max_scan_bytes = bytes.len();
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                "λ😀marker",
                None,
                find_budget,
                &cancellation,
            )
            .unwrap();
        assert_eq!(found.stop, SourceFindStop::EndOfSource);
        assert_eq!(found.matches.len(), 1);
        let expected = encoded("λ😀marker", encoding)
            .into_iter()
            .skip(if encoding.starts_with("utf-16") { 2 } else { 0 })
            .collect::<Vec<_>>();
        let span = found.matches[0].span();
        assert_eq!(
            &bytes[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            expected
        );
        assert_eq!(found.matches[0].content_hash(), file.content_hash);
    }
}

#[test]
fn streamed_sources_preserve_all_encodings_raw_only_and_durable_reopen() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let source = format!(
        "// {}\n// λ😀marker\nfn deliberately_unparsed() {{}}\n",
        "a".repeat(8192)
    );
    let mut inputs = Vec::new();
    for (index, encoding) in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"]
        .into_iter()
        .enumerate()
    {
        let path = format!("source{index}.rs");
        let bytes = encoded(&source, encoding);
        fs::write(fixture.path().join(&path), &bytes).unwrap();
        inputs.push((
            path,
            bytes,
            encoding.strip_suffix("-bom").unwrap_or(encoding),
        ));
    }
    let mut raw = vec![b'x'; 8192];
    raw.extend_from_slice(b"\xffmarker");
    fs::write(fixture.path().join("raw.rs"), &raw).unwrap();
    inputs.push(("raw.rs".to_owned(), raw, "bytes"));
    let raw_small = vec![0xff, b'r', b'a', b'w'];
    fs::write(fixture.path().join("raw-small.rs"), &raw_small).unwrap();
    inputs.push(("raw-small.rs".to_owned(), raw_small, "bytes"));
    fs::write(
        fixture.path().join("small.rs"),
        b"pub fn small_definition() {}\n",
    )
    .unwrap();
    let cancellation = Cancellation::new();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy(4096), paths.state_dir(), &cancellation)
            .unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(receipt.discovered_inputs, 7);
    assert_eq!(receipt.oversized_inputs, 0);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(
        snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "small_definition")
    );
    assert!(
        !snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "deliberately_unparsed")
    );
    assert!(
        snapshot
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "source-parser-snapshot-size-limit")
    );
    drop(snapshot);
    check_sources(&service, &receipt, &inputs);
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        receipt
    );
    for (path, _, _) in &inputs {
        fs::write(fixture.path().join(path), b"live replacement").unwrap()
    }
    check_sources(&service, &receipt, &inputs);
    drop(service);
    let mut restored =
        FirstSliceService::new_durable_with_policy(policy(4096), paths.state_dir(), &cancellation)
            .unwrap();
    check_sources(&restored, &receipt, &inputs);
    restored.delete_repository(receipt.repository).unwrap();
    assert!(
        restored
            .generation_lease(receipt.generation, &cancellation)
            .is_err()
    );
}

#[test]
fn streamed_sources_above_snapshot_cap_return_bounded_tail_after_live_replacement() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let mut file = fs::File::create(fixture.path().join("large.rs")).unwrap();
    let mut hasher = blake3::Hasher::new();
    let block = [b' '; 64 * 1024];
    for _ in 0..rootlight_vfs::MAX_SNAPSHOT_BYTES / u64::try_from(block.len()).unwrap() {
        file.write_all(&block).unwrap();
        hasher.update(&block);
    }
    let tail = b"// exacttailmarker\n";
    file.write_all(tail).unwrap();
    hasher.update(tail);
    drop(file);
    fs::write(
        fixture.path().join("large.rs-extra"),
        b"// sibling source\n",
    )
    .unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(receipt.discovered_inputs, 2);
    assert_eq!(receipt.oversized_inputs, 0);
    assert!(receipt.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "source-parser-snapshot-size-limit"
            && diagnostic.message == "streamed source remains available for exact reads and literal matching but parser memory limits prevent structural analysis and lexical vocabulary projection"
    }));
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "large.rs")
        .unwrap()
        .clone();
    assert_eq!(
        file.content_hash,
        ContentHash::from_bytes(*hasher.finalize().as_bytes())
    );
    assert_eq!(
        file.byte_length,
        rootlight_vfs::MAX_SNAPSHOT_BYTES + u64::try_from(tail.len()).unwrap()
    );
    assert!(snapshot.document().entities.is_empty());
    drop(snapshot);
    let located = service
        .code_locate_with_filters_and_budget(
            receipt.generation,
            "large.rs".to_owned(),
            LocateMode::Prefix,
            Vec::new(),
            vec!["large.rs".to_owned()],
            10,
            0,
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert!(!located.data.hits.is_empty());
    assert!(located.data.hits.iter().all(|hit| hit.path == "large.rs"));
    assert_eq!(
        service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                "exacttailmarker".to_owned(),
                LocateMode::Text,
                Vec::new(),
                vec!["large.rs".to_owned()],
                10,
                0,
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap_err(),
        FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::SourceBytes,
            observed: file.byte_length,
            limit: service.config.analysis().max_source_file_bytes,
        }
    );
    fs::write(fixture.path().join("large.rs"), b"different live bytes").unwrap();
    let start = rootlight_vfs::MAX_SNAPSHOT_BYTES;
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
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, tail);
    assert_eq!(read.data.chunks[0].reference, reference);
    let found = service
        .find_literal(
            receipt.generation,
            file.id,
            "exacttailmarker",
            Some(SourceFindCursor {
                next_byte: usize::try_from(start).unwrap(),
                matched_prefix: 0,
            }),
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.stop, SourceFindStop::EndOfSource);
    drop(service);
    let restored = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    let read = restored
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, tail);
}

#[test]
fn durable_streamed_projection_preserves_the_actual_snapshot_boundary_in_both_layouts() {
    for file_only in [false, true] {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let fixture = durable_test_tempdir();
        let mut inputs = Vec::new();
        for (name, word, length) in [
            ("below.rs", "belowword", 4095),
            ("at.rs", "atword", 4096),
            ("above.rs", "aboveword", 4097),
        ] {
            let mut bytes = format!("// {word}\n").into_bytes();
            bytes.resize(length, b' ');
            fs::write(fixture.path().join(name), &bytes).unwrap();
            inputs.push((name, word, bytes));
        }
        let mut raw = vec![b'r'; 4096];
        raw.extend_from_slice(b"\xff");
        fs::write(fixture.path().join("unknown.rs"), raw).unwrap();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy(4096),
            paths.state_dir(),
            &cancellation,
        )
        .unwrap();
        if file_only {
            let mut limits = service.analysis_limits.ir().clone();
            limits.max_files = 1;
            replace_service_ir_limits(&mut service, limits);
        }
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        let assert_projection = |service: &FirstSliceService| {
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            assert_eq!(snapshot.source_files().entries().is_empty(), !file_only);
            for (name, word, bytes) in &inputs {
                let file = snapshot
                    .document()
                    .files
                    .iter()
                    .chain(
                        snapshot
                            .source_files()
                            .entries()
                            .iter()
                            .map(|entry| entry.file()),
                    )
                    .find(|file| file.path == *name)
                    .unwrap();
                let located = service
                    .code_locate(
                        receipt.generation,
                        (*word).to_owned(),
                        LocateMode::Text,
                        10,
                        0,
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(
                    located.data.hits.iter().any(|hit| hit.path == *name),
                    bytes.len() <= 4096
                );
                let read = service
                    .source_read_with_options_and_budget(
                        receipt.generation,
                        vec![file.evidence.source.clone().unwrap()],
                        SourceReadOptions::new()
                            .with_context_lines_before(0)
                            .with_context_lines_after(0)
                            .with_encoding(SourceEncoding::Bytes),
                        FirstSliceBudget::new(),
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(read.data.chunks[0].bytes, *bytes);
                let found = service
                    .find_literal(
                        receipt.generation,
                        file.id,
                        word,
                        None,
                        SourceFindBudget::new(),
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(found.matches.len(), 1);
                assert_eq!(found.stop, SourceFindStop::EndOfSource);
            }
            let raw = snapshot
                .document()
                .files
                .iter()
                .chain(
                    snapshot
                        .source_files()
                        .entries()
                        .iter()
                        .map(|entry| entry.file()),
                )
                .find(|file| file.path == "unknown.rs")
                .unwrap();
            assert_eq!(raw.encoding, "bytes");
        };
        assert_projection(&service);
        for (name, _, _) in &inputs {
            fs::write(fixture.path().join(name), b"changed live source").unwrap();
        }
        drop(service);
        // Reopen with the ordinary larger snapshot limit: the durable descriptor
        // must reproduce the original reduced projection rather than current policy.
        let restored = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
        assert_projection(&restored);
    }
}

#[test]
fn ephemeral_generation_leases_retain_captured_bytes_through_eviction_delete_and_drop() {
    for action in ["evict", "delete", "drop"] {
        let parent_root = durable_test_tempdir();
        let paths = RuntimePaths::new(
            parent_root.path().join("private"),
            parent_root.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let parent =
            cap_std::fs::Dir::open_ambient_dir(paths.state_dir(), cap_std::ambient_authority())
                .unwrap();
        let mut owner = FirstSliceEphemeralSources::create(
            &parent,
            std::ffi::OsStr::new("source-owner"),
            FirstSliceStreamedStorageLimits::new(8192, 65536, 8).unwrap(),
        )
        .unwrap();
        let fixture = durable_test_tempdir();
        let original = vec![b' '; 4097];
        fs::write(fixture.path().join("source.rs"), &original).unwrap();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new_with_storage_policy(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            None,
            policy(4096),
        )
        .unwrap();
        service.source_owner = Some(owner.clone());
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        let lease = service
            .generation_lease(receipt.generation, &cancellation)
            .unwrap();
        let weak_sources =
            Arc::downgrade(service.streamed_sources.get(&receipt.generation).unwrap());
        let file = lease.generation().document().files[0].clone();
        match action {
            "evict" => {
                for value in *b"12" {
                    let mut changed = original.clone();
                    changed[0] = value;
                    fs::write(fixture.path().join("source.rs"), changed).unwrap();
                    service
                        .index_repository(fixture.path(), &cancellation)
                        .unwrap();
                }
                assert!(!service.streamed_sources.contains_key(&receipt.generation));
            }
            "delete" => {
                service.delete_repository(receipt.repository).unwrap();
            }
            "drop" => {}
            _ => unreachable!("closed lifetime controls"),
        }
        drop(service);
        fs::write(fixture.path().join("source.rs"), b"changed live file").unwrap();
        assert_eq!(owner.usage().unwrap().reserved_payload_bytes, 4097);
        assert!(!owner.finish_if_idle().unwrap());
        let sources = weak_sources
            .upgrade()
            .expect("the outstanding query lease pins physical captures");
        let mut actual = Vec::new();
        sources
            .visit(file.id, 8192, &cancellation, &mut |_, bytes| {
                actual.extend_from_slice(bytes);
                Ok(())
            })
            .unwrap();
        assert_eq!(actual, original);
        assert_eq!(content_hash(&actual), file.content_hash);
        drop(sources);
        drop(lease);
        assert!(weak_sources.upgrade().is_none());
        assert_eq!(owner.usage().unwrap().reserved_payload_bytes, 0);
        assert!(owner.finish_if_idle().unwrap());
        assert!(!paths.state_dir().join("source-owner").exists());
    }
}
