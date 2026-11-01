//! Real durable reads compare the explicit operation with ordinary source reads.
//! Failure controls preserve provisional callback semantics and independently
//! charged owners after resident source shedding, without changing public caches.

use super::*;
use rootlight_runtime::RuntimePaths;
use std::{fs, time::Instant};
use tempfile::TempDir;

struct Fixture {
    service: FirstSliceService,
    _storage: TempDir,
    root: TempDir,
    paths: RuntimePaths,
    receipt: FirstSliceIndexReceipt,
    references: Vec<SourceRef>,
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now() + Duration::from_secs(60))
}
fn limits() -> FirstSliceSourceReadOperationLimits {
    FirstSliceSourceReadOperationLimits::new(100, 4 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024)
        .unwrap()
}
fn options() -> SourceReadOptions {
    SourceReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0)
}

fn fixture() -> Fixture {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut large = Vec::from(b"pub fn alpha() {}\r\n// ".as_slice());
    large.extend(std::iter::repeat_n(b'a', 140 * 1024));
    large.extend_from_slice(b"\r\n");
    fs::write(root.path().join("alpha.rs"), large).unwrap();
    fs::write(root.path().join("beta.rs"), b"pub fn beta() {}\n").unwrap();
    fs::write(root.path().join("bom.rs"), b"\xef\xbb\xbfpub fn bom() {}\n").unwrap();
    fs::write(root.path().join("unicode.rs"), "//λ\npub fn unicode() {}\n").unwrap();
    fs::write(root.path().join("module.bas"), b"Attribute VB_Name = \"Authored\"\nOption Explicit\n#Const ENABLED = True\n#If ENABLED Then\nPublic Sub Run()\nEnd Sub\n#End If\n").unwrap();
    fs::write(root.path().join("empty.rs"), b"").unwrap();
    fs::write(
        root.path().join("wide.rs"),
        [
            0xff, 0xfe, b'/', 0, b'/', 0, b' ', 0, b'x', 0, b'\r', 0, b'\n', 0,
        ],
    )
    .unwrap();
    // Keep all seven original controls while crossing the real single-pack floor.
    for index in 0..9 {
        fs::write(
            root.path()
                .join(format!("packed-read-padding-{index:02}.rs")),
            b"// packed read fixture padding\n",
        )
        .unwrap();
    }
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert!(receipt.logical_snapshot.is_some());
    assert_eq!(receipt.indexed_files, 16);
    assert_packed_fixture(paths.state_dir(), &receipt);
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let references = lease
        .generation()
        .document()
        .files
        .iter()
        .map(|file| file.evidence.source.clone().unwrap())
        .collect();
    drop(lease);
    service
        .source_snapshots
        .shed_cached_sources(SourceSnapshotCacheEntry::Committed(receipt.generation))
        .unwrap();
    Fixture {
        _storage: storage,
        root,
        paths,
        service,
        receipt,
        references,
    }
}

fn assert_packed_fixture(state: &Path, receipt: &FirstSliceIndexReceipt) {
    let manifest = state
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string())
        .join("manifest.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(value["source_storage"]["version"], 2);
    assert_eq!(value["source_storage"]["files"], 16);
    assert_eq!(value["source_storage"]["packs"], 1);
}

fn part(reference: &SourceRef, start: u64, end: u64) -> SourceRef {
    SourceRef::new(
        reference.repository(),
        reference.generation(),
        SourceSpan::new(reference.span().file(), start, end).unwrap(),
        reference.content_hash(),
        None,
    )
}

fn alpha(fixture: &Fixture) -> SourceRef {
    let lease = fixture
        .service
        .generation_lease(fixture.receipt.generation, &deadline())
        .unwrap();
    lease
        .generation()
        .document()
        .files
        .iter()
        .find(|file| file.path == "alpha.rs")
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap()
}

fn generation_path(fixture: &Fixture) -> PathBuf {
    fixture
        .paths
        .state_dir()
        .join("first-slice/repositories")
        .join(fixture.receipt.repository.to_string())
        .join(fixture.receipt.generation.to_string())
}

fn reserved(service: &FirstSliceService) -> u64 {
    service
        .generation_cache
        .lock()
        .unwrap()
        .restored_reservation_bytes
}

fn locality_fixture(file_count: usize, medium_bytes: usize) -> Fixture {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let repository = derive_repository(b"packed-read-locality").id();
    service
        .restore_repository_registration(
            repository_path_hash(&fs::canonicalize(root.path()).unwrap()).unwrap(),
            repository,
        )
        .unwrap();
    let repository_root = RepositoryRoot::open(repository, root.path()).unwrap();
    let mut ordered: Vec<_> = (0..file_count)
        .map(|ordinal| {
            let path = format!("source-{ordinal:04}.rs");
            let relative = RelativePath::parse(Path::new(&path)).unwrap();
            (repository_root.file_id(&relative), path)
        })
        .collect();
    ordered.sort_unstable_by_key(|(file, _)| *file);
    for (ordinal, (_, path)) in ordered.iter().enumerate() {
        let mut bytes = format!("// packed member {ordinal}\n").into_bytes();
        if ordinal == file_count / 2 {
            bytes.extend_from_slice(b"// ");
            bytes.resize(medium_bytes, b'm');
        }
        fs::write(root.path().join(path), bytes).unwrap();
    }
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(receipt.repository, repository);
    assert_eq!(receipt.indexed_files, u64::try_from(file_count).unwrap());
    drop(service);
    let service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let mut files: Vec<_> = lease.generation().document().files.iter().collect();
    files.sort_unstable_by_key(|file| file.id);
    let references = files
        .iter()
        .zip(&ordered)
        .map(|(file, (expected, _))| {
            assert_eq!(file.id, *expected);
            file.evidence.source.clone().unwrap()
        })
        .collect();
    drop(lease);
    assert!(
        service
            .source_snapshots
            .snapshots(receipt.generation)
            .unwrap()
            .is_empty()
    );
    Fixture {
        _storage: storage,
        root,
        paths,
        service,
        receipt,
        references,
    }
}

#[test]
fn cold_writer_packs_bound_unrelated_bytes_and_keep_one_slot_across_small_medium_small_reads() {
    for medium_bytes in [1024 * 1024, 1024 * 1024 + 1] {
        assert_cold_writer_pack_locality(257, medium_bytes);
    }
}

#[test]
fn small_generations_keep_packed_storage_above_the_write_target() {
    assert_cold_writer_pack_locality(16, 1024 * 1024 + 1);
}

fn assert_cold_writer_pack_locality(file_count: usize, medium_bytes: usize) {
    let fixture = locality_fixture(file_count, medium_bytes);
    let cancellation = deadline();
    let lease = fixture
        .service
        .generation_lease(fixture.receipt.generation, &cancellation)
        .unwrap();
    let mut files: Vec<_> = lease.generation().document().files.iter().collect();
    files.sort_unstable_by_key(|file| file.id);
    let midpoint = file_count / 2;
    let expected_pack_bytes = [
        files[..midpoint]
            .iter()
            .map(|file| file.byte_length)
            .sum::<u64>(),
        files[midpoint].byte_length,
        files[midpoint + 1..]
            .iter()
            .map(|file| file.byte_length)
            .sum::<u64>(),
    ];
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(generation_path(&fixture).join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["source_storage"]["version"], 2);
    assert_eq!(manifest["source_storage"]["packs"], 3);
    let before = reserved(&fixture.service);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    assert_eq!(
        operation.owner.as_ref().unwrap().reader.cached_pack_bytes(),
        None
    );
    for (read, (ordinal, pack)) in [(0, 0), (midpoint, 1), (0, 0), (file_count - 1, 2)]
        .into_iter()
        .enumerate()
    {
        let selected = part(&fixture.references[ordinal], 0, 8);
        let original = fs::read(fixture.root.path().join(&files[ordinal].path)).unwrap();
        operation
            .read(
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                |actual| {
                    let chunk = &actual.data.chunks[0];
                    assert_eq!(chunk.bytes, original[..8]);
                    assert_eq!(chunk.reference, selected);
                    assert_eq!(chunk.path, files[ordinal].path);
                    assert_eq!(chunk.content_hash, files[ordinal].content_hash);
                    Ok(())
                },
            )
            .unwrap();
        let reader = &operation.owner.as_ref().unwrap().reader;
        assert_eq!(reader.counters(), (1, u64::try_from(read + 1).unwrap(), 0));
        assert_eq!(
            reader.cached_pack_bytes(),
            Some(usize::try_from(expected_pack_bytes[pack]).unwrap())
        );
        if pack != 1 {
            assert!(expected_pack_bytes[pack] < files[midpoint].byte_length);
        }
    }
    drop(operation);
    assert_eq!(reserved(&fixture.service), before);
}

#[test]
fn another_member_corruption_in_a_new_writer_pack_fails_before_callback() {
    let fixture = locality_fixture(257, 1024 * 1024 + 1);
    let selected = part(&fixture.references[0], 0, 8);
    let pack = generation_path(&fixture).join("sources/pack-00000000.bin");
    let mut bytes = fs::read(&pack).unwrap();
    assert!(u64::try_from(bytes.len()).unwrap() > fixture.references[0].span().end_byte());
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(pack, bytes).unwrap();
    let cancellation = deadline();
    let before = reserved(&fixture.service);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    assert_eq!(
        operation
            .read(vec![selected], options(), FirstSliceBudget::new(), |_| {
                panic!("another member must be verified before the selected response")
            })
            .unwrap_err(),
        FirstSliceError::CatalogCorrupt
    );
    assert!(operation.owner.is_none());
    assert_eq!(reserved(&fixture.service), before);
}

#[test]
fn cold_reopen_reads_historical_multi_member_packs_above_the_new_write_target() {
    let Fixture {
        _storage,
        root,
        paths,
        service,
        receipt,
        references,
    } = locality_fixture(257, 1024 * 1024 + 1);
    drop(service);
    durable::read_operation::coalesce_legacy_source_pack_for_test(paths.state_dir(), &receipt);
    let service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let cancellation = deadline();
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let payload_bytes: u64 = lease
        .generation()
        .document()
        .files
        .iter()
        .map(|file| file.byte_length)
        .sum();
    let mut operation = service
        .source_read_operation(receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    for ordinal in [0, 128, 256] {
        let selected = part(&references[ordinal], 0, 8);
        let file = lease
            .generation()
            .find_file(selected.span().file())
            .unwrap();
        let original = fs::read(root.path().join(&file.path)).unwrap();
        operation
            .read(
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                |actual| {
                    let chunk = &actual.data.chunks[0];
                    assert_eq!(chunk.bytes, original[..8]);
                    assert_eq!(chunk.reference, selected);
                    assert_eq!(chunk.content_hash, file.content_hash);
                    Ok(())
                },
            )
            .unwrap();
        let reader = &operation.owner.as_ref().unwrap().reader;
        assert_eq!(reader.counters(), (1, 1, 0));
        assert_eq!(
            reader.cached_pack_bytes(),
            Some(usize::try_from(payload_bytes).unwrap())
        );
    }
}

#[test]
fn packed_read_phase_profile_preserves_cold_cached_failed_and_lossy_reads() {
    let fixture = fixture();
    let cancellation = deadline();
    let selected = part(&alpha(&fixture), 0, 8);
    let original = fs::read(fixture.root.path().join("alpha.rs")).unwrap();
    let mut names = vec![
        "source_read_packed_allocation",
        "source_read_packed_parent_validation",
        "source_read_packed_open_file",
    ];
    #[cfg(any(windows, target_os = "macos"))]
    names.push("source_read_packed_initial_access_policy");
    names.extend([
        "source_read_packed_initial_metadata",
        "source_read_packed_seek",
        "source_read_packed_payload_read",
        "source_read_packed_visitor",
    ]);
    #[cfg(any(windows, target_os = "macos"))]
    names.push("source_read_packed_final_access_policy");
    names.push("source_read_packed_final_metadata");
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    for cold in [true, false] {
        let (result, profile) = capture_first_slice_analysis_profile(|| {
            operation.read(
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                |actual| {
                    assert_eq!(actual.data.chunks[0].bytes, original[..8]);
                    assert_eq!(actual.data.chunks[0].reference, selected);
                    Ok(())
                },
            )
        });
        result.unwrap();
        assert!(profile.events_complete && profile.counts_complete);
        let encoded = serde_json::to_value(profile).unwrap();
        let events = encoded["phases"].as_array().unwrap();
        for name in &names {
            let matching: Vec<_> = events
                .iter()
                .filter(|event| event["phase"] == *name)
                .collect();
            assert_eq!(matching.len(), usize::from(cold), "{name}");
            for event in matching {
                assert_eq!(event["file"], selected.span().file().to_string());
                assert!(event["wall_micros"].is_number());
                assert_eq!(event["scope_exit"], "expression_returned");
            }
        }
        assert_eq!(
            operation.owner.as_ref().unwrap().reader.counters(),
            (1, 1, 0)
        );
    }
    drop(operation);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let (result, lossy) = capture_first_slice_analysis_profile(|| {
        with_analysis_profile(|profile| profile.reject_event_reservation = true);
        operation.read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |actual| {
                assert_eq!(actual.data.chunks[0].bytes, original[..8]);
                Ok(())
            },
        )
    });
    result.unwrap();
    assert!(!lossy.events_complete);
    assert!(lossy.dropped_events >= u64::try_from(names.len()).unwrap());
    assert!(lossy.phases.is_empty());
    drop(operation);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let pack = generation_path(&fixture).join("sources/pack-00000000.bin");
    let mut bytes = fs::read(&pack).unwrap();
    bytes.push(0);
    fs::write(pack, bytes).unwrap();
    let (result, failed) = capture_first_slice_analysis_profile(|| {
        operation.read(vec![selected], options(), FirstSliceBudget::new(), |_| {
            panic!("wrong-length packed bytes remain provisional")
        })
    });
    assert_eq!(result.unwrap_err(), FirstSliceError::CatalogCorrupt);
    assert!(failed.events_complete && failed.counts_complete);
    let encoded = serde_json::to_value(failed).unwrap();
    let events = encoded["phases"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["phase"] == "source_read_packed_initial_metadata")
            .count(),
        1
    );
    assert!(events.iter().all(|event| {
        event["phase"] != "source_read_packed_payload_read"
            && event["phase"] != "source_read_packed_verification"
    }));
    drop(operation);
}

#[test]
fn successive_source_accounting_sized_reads_reuse_one_index_and_pack_after_shedding() {
    let fixture = fixture();
    let reference = alpha(&fixture);
    let cancellation = deadline();
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let original = fs::read(fixture.root.path().join("alpha.rs")).unwrap();
    let mut output = Vec::new();
    for start in (0..original.len()).step_by(64 * 1024) {
        let end = (start + 64 * 1024).min(original.len());
        let selected = part(
            &reference,
            start.try_into().unwrap(),
            end.try_into().unwrap(),
        );
        let expected = fixture
            .service
            .source_read_with_options_and_budget(
                fixture.receipt.generation,
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap();
        operation
            .read(
                vec![selected],
                options(),
                FirstSliceBudget::new(),
                |actual| {
                    assert_eq!(actual.data, expected.data);
                    assert_eq!(actual.plan, expected.plan);
                    output.extend_from_slice(&actual.data.chunks[0].bytes);
                    assert!(
                        fixture.service.generation_cache.try_lock().is_ok(),
                        "no service lock may cross a callback"
                    );
                    Ok(())
                },
            )
            .unwrap();
    }
    assert_eq!(output, original);
    assert_eq!(
        operation.owner.as_ref().unwrap().reader.counters(),
        (1, 1, 0)
    );
    assert!(
        fixture
            .service
            .source_snapshots
            .snapshots(fixture.receipt.generation)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn streamed_utf8_chunks_reuse_one_immutable_body_with_ordinary_line_metadata() {
    let Fixture {
        _storage,
        root,
        paths,
        service,
        receipt,
        references,
    } = fixture();
    drop(service);
    let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: "version = \"1.1\"\n[analysis]\nmax_source_file_bytes = 4096\n",
    }])
    .unwrap();
    let cancellation = deadline();
    let service = FirstSliceService::new_durable_with_policy(
        FirstSliceStoragePolicy::from_config(config, 4),
        paths.state_dir(),
        &cancellation,
    )
    .unwrap();
    let fixture = Fixture {
        _storage,
        root,
        paths,
        service,
        receipt,
        references,
    };
    let reference = alpha(&fixture);
    let original = fs::read(fixture.root.path().join("alpha.rs")).unwrap();
    assert!(
        u64::try_from(original.len()).unwrap()
            > fixture.service.config.analysis().max_source_file_bytes
    );
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let mut output = Vec::new();
    for start in (0..original.len()).step_by(64 * 1024) {
        let selected = part(
            &reference,
            start.try_into().unwrap(),
            (start + 64 * 1024).min(original.len()).try_into().unwrap(),
        );
        let expected = fixture
            .service
            .source_read_with_options_and_budget(
                fixture.receipt.generation,
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap();
        operation
            .read(
                vec![selected],
                options(),
                FirstSliceBudget::new(),
                |actual| {
                    assert_eq!(actual.data, expected.data);
                    assert_eq!(actual.plan, expected.plan);
                    output.extend_from_slice(&actual.data.chunks[0].bytes);
                    Ok(())
                },
            )
            .unwrap();
    }
    assert_eq!(output, original);
    assert_eq!(operation.test_io_counts(), (1, 1, 0));
    let owner = operation.owner.as_ref().unwrap();
    assert_eq!(
        owner.prepared.as_ref().unwrap().view.snapshot().file(),
        reference.span().file()
    );
}

#[test]
fn prepared_view_has_one_charged_slot_and_inadmissible_views_keep_ordinary_results() {
    let fixture = fixture();
    let cancellation = deadline();
    let baseline = reserved(&fixture.service);
    let file_bytes = alpha(&fixture).span().end_byte();
    let alpha = part(&alpha(&fixture), 0, 8);
    let beta = fixture
        .references
        .iter()
        .find(|reference| {
            let lease = fixture
                .service
                .generation_lease(fixture.receipt.generation, &cancellation)
                .unwrap();
            lease
                .generation()
                .find_file(reference.span().file())
                .unwrap()
                .path
                == "beta.rs"
        })
        .unwrap();
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let opened = reserved(&fixture.service);
    operation
        .read(
            vec![alpha.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    let owner = operation.owner.as_ref().unwrap();
    let first = Arc::downgrade(owner.prepared.as_ref().unwrap().view.snapshot());
    let alpha_bytes = owner.prepared.as_ref().unwrap().bytes;
    assert_eq!(reserved(&fixture.service), opened + alpha_bytes);
    operation
        .read(
            vec![beta.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    assert!(
        first.upgrade().is_none(),
        "replacing the slot releases the old body"
    );
    let owner = operation.owner.as_ref().unwrap();
    assert_eq!(
        owner.prepared.as_ref().unwrap().view.snapshot().file(),
        beta.span().file()
    );
    assert_eq!(
        reserved(&fixture.service),
        opened + owner.prepared.as_ref().unwrap().bytes
    );
    operation
        .read(
            vec![alpha.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(reserved(&fixture.service), opened + alpha_bytes);
    let held = operation.owner.as_ref().unwrap().held_bytes;
    drop(operation);
    assert_eq!(reserved(&fixture.service), baseline);

    let maximum = held + file_bytes + 2 * FirstSliceBudget::new().query().max_memory_bytes();
    let small =
        FirstSliceSourceReadOperationLimits::new(10, limits().maximum_work, maximum).unwrap();
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, small, &cancellation)
        .unwrap()
        .unwrap();
    let expected = fixture
        .service
        .source_read_with_options_and_budget(
            fixture.receipt.generation,
            vec![alpha.clone()],
            options(),
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    operation
        .read(vec![alpha], options(), FirstSliceBudget::new(), |actual| {
            assert_eq!(actual.data, expected.data);
            Ok(())
        })
        .unwrap();
    assert!(operation.owner.as_ref().unwrap().prepared.is_none());
    drop(operation);
    assert_eq!(reserved(&fixture.service), baseline);
}

#[test]
fn duplicate_order_context_empty_and_reduced_byte_errors_match_ordinary_reads() {
    let fixture = fixture();
    let cancellation = deadline();
    let reference = alpha(&fixture);
    let selected = part(&reference, 0, 5);
    let repeated = vec![selected.clone(), selected.clone()];
    let context = options().with_context_lines_before(1);
    let expected = fixture
        .service
        .source_read_with_options_and_budget(
            fixture.receipt.generation,
            repeated.clone(),
            context,
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    operation
        .read(repeated, context, FirstSliceBudget::new(), |actual| {
            assert_eq!(actual.data, expected.data);
            Ok(())
        })
        .unwrap();
    let empty = fixture
        .references
        .iter()
        .find(|reference| reference.span().end_byte() == 0)
        .unwrap()
        .clone();
    let expected = fixture
        .service
        .source_read_with_options_and_budget(
            fixture.receipt.generation,
            vec![empty.clone()],
            options(),
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    operation
        .read(vec![empty], options(), FirstSliceBudget::new(), |actual| {
            assert_eq!(actual.data, expected.data);
            Ok(())
        })
        .unwrap();
    let budget = FirstSliceBudget::new().reduce_max_source_bytes(4);
    let expected = fixture
        .service
        .source_read_with_options_and_budget(
            fixture.receipt.generation,
            vec![selected.clone()],
            options(),
            budget,
            &cancellation,
        )
        .unwrap_err();
    assert_eq!(
        operation
            .read(vec![selected], options(), budget, |_| panic!(
                "failed request cannot call visitor"
            ))
            .unwrap_err(),
        expected
    );
}

#[test]
fn callback_charge_is_live_until_return_and_error_releases_every_operation_owner() {
    let fixture = fixture();
    let cancellation = deadline();
    let before = reserved(&fixture.service);
    let selected = part(&alpha(&fixture), 0, 8);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let held = reserved(&fixture.service);
    assert!(held > before);
    let error = operation
        .read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| {
                assert!(
                    reserved(&fixture.service) > held,
                    "staged response/snapshot remains charged through callback"
                );
                Err(FirstSliceError::Source)
            },
        )
        .unwrap_err();
    assert_eq!(error, FirstSliceError::Source);
    assert_eq!(reserved(&fixture.service), before);
    assert_eq!(
        operation
            .read(
                vec![selected],
                options(),
                FirstSliceBudget::new(),
                |_| panic!("closed operation")
            )
            .unwrap_err(),
        error
    );
}

#[test]
fn exact_memory_boundary_and_one_less_are_admitted_before_index_allocation() {
    let fixture = fixture();
    let cancellation = deadline();
    let before = reserved(&fixture.service);
    let operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let required = reserved(&fixture.service) - before;
    drop(operation);
    assert_eq!(reserved(&fixture.service), before);
    for (maximum, succeeds) in [(required - 1, false), (required, true)] {
        let bounds =
            FirstSliceSourceReadOperationLimits::new(10, 1024 * 1024 * 1024, maximum).unwrap();
        let opened = fixture.service.source_read_operation(
            fixture.receipt.generation,
            bounds,
            &cancellation,
        );
        assert_eq!(opened.is_ok(), succeeds);
        if let Err(error) = opened {
            assert!(matches!(
                error,
                FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::MemoryBytes,
                    ..
                }
            ));
        }
    }
    assert_eq!(reserved(&fixture.service), before);
}

#[test]
fn cache_hit_cancellation_and_lifetime_work_exhaustion_are_terminal() {
    let fixture = fixture();
    let cancellation = deadline();
    let selected = part(&alpha(&fixture), 0, 8);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    operation
        .read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert_eq!(
        operation
            .read(
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                |_| panic!("cancelled cache hit")
            )
            .unwrap_err(),
        FirstSliceError::Cancelled(CancellationReason::ClientRequest)
    );
    let cancellation = deadline();
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    operation
        .read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    operation.work.remaining.set(0);
    assert_eq!(
        operation
            .read(
                vec![selected],
                options(),
                FirstSliceBudget::new(),
                |_| panic!("exhausted work")
            )
            .unwrap_err(),
        FirstSliceError::BudgetExceeded
    );
    assert!(operation.owner.is_none());
}

#[test]
fn loaded_arc_does_not_authorize_a_logically_removed_generation() {
    let mut fixture = fixture();
    fs::write(
        fixture.root.path().join("beta.rs"),
        b"pub fn successor() {}\n",
    )
    .unwrap();
    fixture
        .service
        .index_repository(fixture.root.path(), &deadline())
        .unwrap();
    let cancellation = deadline();
    let selected = part(&alpha(&fixture), 0, 8);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    fixture
        .service
        .generation_cache
        .lock()
        .unwrap()
        .generations
        .remove(fixture.receipt.generation)
        .unwrap();
    assert_eq!(
        operation
            .read(
                vec![selected],
                options(),
                FirstSliceBudget::new(),
                |_| panic!("retired generation")
            )
            .unwrap_err(),
        FirstSliceError::Query
    );
    assert!(operation.owner.is_none());
}

#[test]
fn changed_manifest_or_removed_activation_cannot_authorize_a_cached_response() {
    for remove_marker in [false, true] {
        let fixture = fixture();
        let cancellation = deadline();
        let selected = part(&alpha(&fixture), 0, 8);
        let mut operation = fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .unwrap();
        operation
            .read(
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                |_| Ok(()),
            )
            .unwrap();
        let directory = generation_path(&fixture);
        if remove_marker {
            let repository = directory.parent().unwrap();
            for entry in fs::read_dir(repository).unwrap() {
                let entry = entry.unwrap();
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("activation-")
                {
                    fs::remove_dir_all(entry.path()).unwrap();
                }
            }
        } else {
            let manifest = directory.join("manifest.json");
            let mut bytes = fs::read(&manifest).unwrap();
            bytes.push(b' ');
            fs::write(&manifest, bytes).unwrap();
        }
        assert!(
            operation
                .read(
                    vec![selected],
                    options(),
                    FirstSliceBudget::new(),
                    |_| panic!("publication changed")
                )
                .is_err()
        );
        assert!(operation.owner.is_none());
    }
}

#[test]
fn malformed_index_and_changed_pack_never_produce_partial_success() {
    for index in [false, true] {
        let fixture = fixture();
        let directory = generation_path(&fixture).join("sources");
        let path = directory.join(if index {
            "index.msgpack"
        } else {
            "pack-00000000.bin"
        });
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        fs::write(path, bytes).unwrap();
        let cancellation = deadline();
        let before = reserved(&fixture.service);
        let opened = fixture.service.source_read_operation(
            fixture.receipt.generation,
            limits(),
            &cancellation,
        );
        if index {
            assert!(opened.is_err());
        } else {
            let mut operation = opened.unwrap().unwrap();
            assert!(
                operation
                    .read(
                        vec![part(&alpha(&fixture), 0, 8)],
                        options(),
                        FirstSliceBudget::new(),
                        |_| panic!("tampered pack")
                    )
                    .is_err()
            );
            assert!(operation.owner.is_none());
        }
        assert_eq!(reserved(&fixture.service), before);
    }
}

#[test]
fn unsupported_unloaded_and_memory_backings_return_explicit_none() {
    let fixture = fixture();
    let cancellation = deadline();
    let memory = FirstSliceService::new(2).unwrap();
    assert!(
        memory
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .unload(fixture.receipt.generation)
            .unwrap()
    );
    assert!(
        fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture
            .service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .is_loaded(fixture.receipt.generation)
    );
}

#[test]
fn foreign_and_stale_references_keep_the_ordinary_typed_refusal() {
    let fixture = fixture();
    let cancellation = deadline();
    let original = part(&alpha(&fixture), 0, 8);
    let other = fixture
        .references
        .iter()
        .find(|reference| reference.span().file() != original.span().file())
        .unwrap();
    let stale = SourceRef::new(
        original.repository(),
        original.generation(),
        original.span(),
        other.content_hash(),
        None,
    );
    let foreign_fixture = self::fixture();
    let foreign = SourceRef::new(
        foreign_fixture.receipt.repository,
        original.generation(),
        original.span(),
        original.content_hash(),
        None,
    );
    for selected in [stale, foreign] {
        let expected = fixture
            .service
            .source_read_with_options_and_budget(
                fixture.receipt.generation,
                vec![selected.clone()],
                options(),
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap_err();
        let mut operation = fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(
            operation
                .read(
                    vec![selected],
                    options(),
                    FirstSliceBudget::new(),
                    |_| panic!("invalid source identity")
                )
                .unwrap_err(),
            expected
        );
    }
}

#[test]
fn new_operation_refuses_linked_pack_and_cancelled_open_does_not_reserve() {
    let fixture = fixture();
    let cancellation = deadline();
    cancellation.cancel(CancellationReason::ClientRequest);
    let before = reserved(&fixture.service);
    assert!(matches!(
        fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(reserved(&fixture.service), before);
    let directory = generation_path(&fixture).join("sources");
    fs::hard_link(
        directory.join("pack-00000000.bin"),
        directory.join("foreign-link"),
    )
    .unwrap();
    let cancellation = deadline();
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    assert!(
        operation
            .read(
                vec![part(&alpha(&fixture), 0, 8)],
                options(),
                FirstSliceBudget::new(),
                |_| panic!("linked payload")
            )
            .is_err()
    );
    assert_eq!(reserved(&fixture.service), before);
}

#[test]
fn pack_a_b_a_reloads_the_single_slot_without_a_large_fixture() {
    let fixture = fixture();
    let lease = fixture
        .service
        .generation_lease(fixture.receipt.generation, &deadline())
        .unwrap();
    let files: Vec<_> = lease
        .generation()
        .document()
        .files
        .iter()
        .filter(|file| matches!(file.path.as_str(), "alpha.rs" | "beta.rs"))
        .cloned()
        .collect();
    let content: Vec<_> = files
        .iter()
        .map(|file| fs::read(fixture.root.path().join(&file.path)).unwrap())
        .collect();
    crate::durable::read_operation::assert_one_slot_for_records(&files, &content);
}

#[test]
fn opaque_utf16_bytes_use_the_same_streamed_range_proof_without_pack_hydration() {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let original = [
        0xff, 0xfe, b'/', 0, b'/', 0, b' ', 0, b'x', 0, b'\r', 0, b'\n', 0,
    ];
    // Keep the exact short prefix, then write a full UTF16 source above 4096 bytes.
    let original_prefix = original;
    let mut original = original.to_vec();
    original.extend(std::iter::repeat_n([b' ', 0], 2042).flatten());
    assert_eq!(&original[..original_prefix.len()], &original_prefix);
    assert_eq!(original.len(), 4098);
    fs::write(root.path().join("wide.rs"), &original).unwrap();
    for index in 0..15 {
        fs::write(
            root.path()
                .join(format!("packed-read-padding-{index:02}.rs")),
            b"// packed read fixture padding\n",
        )
        .unwrap();
    }
    let configuration = "version = \"1.1\"\n[analysis]\nmax_source_file_bytes = 4096\n";
    let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: configuration,
    }])
    .unwrap();
    let policy = FirstSliceStoragePolicy::from_config(config, 4);
    let cancellation = deadline();
    // Publish a packed snapshot first; the reopened policy selects range reads.
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert!(receipt.logical_snapshot.is_some());
    assert_eq!(receipt.indexed_files, 16);
    assert_packed_fixture(paths.state_dir(), &receipt);
    drop(service);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &cancellation)
            .unwrap();
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let wide = lease
        .generation()
        .document()
        .files
        .iter()
        .chain(
            lease
                .generation()
                .source_files()
                .entries()
                .iter()
                .map(|entry| entry.file()),
        )
        .find(|file| file.path == "wide.rs")
        .unwrap();
    assert_eq!(wide.encoding, "utf-16le");
    assert_eq!(wide.byte_length, u64::try_from(original.len()).unwrap());
    assert!(wide.path_locator.is_some());
    assert!(wide.byte_length > service.config.analysis().max_source_file_bytes);
    assert!(
        !service
            .streamed_sources
            .get(&receipt.generation)
            .is_some_and(|sources| sources.contains(wide.id))
    );
    let reference = wide.evidence.source.clone().unwrap();
    service
        .source_snapshots
        .shed_cached_sources(SourceSnapshotCacheEntry::Committed(receipt.generation))
        .unwrap();
    let options = options().with_encoding(SourceEncoding::Bytes);
    let expected = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            options,
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(expected.data.chunks[0].bytes, original);
    let mut operation = service
        .source_read_operation(receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        operation
            .read(
                vec![reference.clone()],
                options,
                FirstSliceBudget::new(),
                |actual| {
                    assert_eq!(actual.data, expected.data);
                    Ok(())
                },
            )
            .unwrap();
    }
    assert_eq!(operation.test_io_counts(), (1, 0, 2));
}

#[test]
fn conditional_vba_and_utf8_bom_responses_preserve_the_existing_query_projection() {
    let fixture = fixture();
    let cancellation = deadline();
    let lease = fixture
        .service
        .generation_lease(fixture.receipt.generation, &cancellation)
        .unwrap();
    for path in ["module.bas", "bom.rs"] {
        let file = lease
            .generation()
            .document()
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        let reference = file.evidence.source.clone().unwrap();
        let expected = fixture
            .service
            .source_read_with_options_and_budget(
                fixture.receipt.generation,
                vec![reference.clone()],
                options(),
                FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap();
        if path == "module.bas" {
            assert!(matches!(
                file.language.as_str(),
                "vba" | "visual-basic-classic"
            ));
            assert!(!expected.data.conditional_activity.is_empty());
        }
        let mut operation = fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .unwrap();
        operation
            .read(
                vec![reference],
                options(),
                FirstSliceBudget::new(),
                |actual| {
                    assert_eq!(actual.data, expected.data);
                    assert_eq!(actual.plan, expected.plan);
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn final_callback_retirement_and_read_count_bound_never_return_success() {
    let mut fixture = fixture();
    fs::write(
        fixture.root.path().join("beta.rs"),
        b"pub fn successor() {}\n",
    )
    .unwrap();
    fixture
        .service
        .index_repository(fixture.root.path(), &deadline())
        .unwrap();
    let cancellation = deadline();
    let selected = part(&alpha(&fixture), 0, 8);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let error = operation
        .read(vec![selected], options(), FirstSliceBudget::new(), |_| {
            fixture
                .service
                .generation_cache
                .try_lock()
                .unwrap()
                .generations
                .remove(fixture.receipt.generation)
                .unwrap();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error, FirstSliceError::Query);
    assert!(operation.owner.is_none());
    let fixture = self::fixture();
    let bounds =
        FirstSliceSourceReadOperationLimits::new(1, 4 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024)
            .unwrap();
    let selected = part(&alpha(&fixture), 0, 8);
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, bounds, &cancellation)
        .unwrap()
        .unwrap();
    operation
        .read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        operation
            .read(
                vec![selected],
                options(),
                FirstSliceBudget::new(),
                |_| panic!("second read exceeds lifetime cap")
            )
            .unwrap_err(),
        FirstSliceError::BudgetExceeded
    );
}

#[test]
fn cumulative_work_exact_and_one_less_include_the_final_authorization() {
    let fixture = fixture();
    let cancellation = deadline();
    let selected = part(&alpha(&fixture), 0, 8);
    let mut measured = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    measured
        .read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    let required = limits().maximum_work - measured.work.remaining.get();
    drop(measured);
    let baseline = reserved(&fixture.service);
    for (maximum, succeeds) in [(required - 1, false), (required, true)] {
        let bounds =
            FirstSliceSourceReadOperationLimits::new(1, maximum, limits().maximum_memory_bytes)
                .unwrap();
        let mut operation = fixture
            .service
            .source_read_operation(fixture.receipt.generation, bounds, &cancellation)
            .unwrap()
            .unwrap();
        let result = operation.read(
            vec![selected.clone()],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        );
        assert_eq!(result.is_ok(), succeeds);
        if !succeeds {
            assert_eq!(result.unwrap_err(), FirstSliceError::BudgetExceeded);
        }
        drop(operation);
        assert_eq!(reserved(&fixture.service), baseline);
    }
}

#[test]
fn scalar_boundary_refusal_and_byte_presentation_match_ordinary_reads() {
    let fixture = fixture();
    let cancellation = deadline();
    let lease = fixture
        .service
        .generation_lease(fixture.receipt.generation, &cancellation)
        .unwrap();
    let file = lease
        .generation()
        .document()
        .files
        .iter()
        .find(|file| file.path == "unicode.rs")
        .unwrap();
    let reference = part(file.evidence.source.as_ref().unwrap(), 3, 4);
    for encoding in [SourceEncoding::Utf8, SourceEncoding::Bytes] {
        let options = options().with_encoding(encoding);
        let expected = fixture.service.source_read_with_options_and_budget(
            fixture.receipt.generation,
            vec![reference.clone()],
            options,
            FirstSliceBudget::new(),
            &cancellation,
        );
        assert_eq!(expected.is_ok(), encoding == SourceEncoding::Bytes);
        let mut operation = fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .unwrap();
        match expected {
            Ok(expected) => operation
                .read(
                    vec![reference.clone()],
                    options,
                    FirstSliceBudget::new(),
                    |actual| {
                        assert_eq!(actual.data, expected.data);
                        Ok(())
                    },
                )
                .unwrap(),
            Err(expected) => assert_eq!(
                operation
                    .read(
                        vec![reference.clone()],
                        options,
                        FirstSliceBudget::new(),
                        |_| panic!("invalid text boundary")
                    )
                    .unwrap_err(),
                expected
            ),
        }
    }
}

#[test]
fn independent_operations_and_request_reductions_never_share_or_reset_a_reservation() {
    let fixture = fixture();
    let cancellation = deadline();
    let baseline = reserved(&fixture.service);
    let first = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    let one = reserved(&fixture.service) - baseline;
    let second = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    assert_eq!(reserved(&fixture.service), baseline + 2 * one);
    drop(first);
    assert_eq!(reserved(&fixture.service), baseline + one);
    drop(second);
    assert_eq!(reserved(&fixture.service), baseline);
    let selected = part(&alpha(&fixture), 0, 8);
    let full_bytes = alpha(&fixture).span().end_byte();
    let reductions = [
        FirstSliceBudget::new().reduce_source_max_selectors(0),
        FirstSliceBudget::new().reduce_source_max_snapshot_bytes(full_bytes - 1),
        FirstSliceBudget::new().reduce_max_source_verification_bytes(full_bytes - 1),
        FirstSliceBudget::new().reduce_max_memory_bytes(1),
    ];
    for budget in reductions {
        let expected = fixture
            .service
            .source_read_with_options_and_budget(
                fixture.receipt.generation,
                vec![selected.clone()],
                options(),
                budget,
                &cancellation,
            )
            .unwrap_err();
        let mut operation = fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(
            operation
                .read(vec![selected.clone()], options(), budget, |_| panic!(
                    "reduced request refused"
                ))
                .unwrap_err(),
            expected
        );
        assert_eq!(reserved(&fixture.service), baseline);
    }
}

#[test]
fn optional_deadline_keeps_finite_operation_limits_and_honors_expired_callers() {
    let fixture = fixture();
    let cancellation = Cancellation::new();
    assert!(!cancellation.has_deadline());
    let mut operation = fixture
        .service
        .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
        .unwrap()
        .unwrap();
    operation
        .read(
            vec![part(&alpha(&fixture), 0, 8)],
            options(),
            FirstSliceBudget::new(),
            |_| Ok(()),
        )
        .unwrap();
    drop(operation);
    let expired = Cancellation::with_deadline(Instant::now() - Duration::from_secs(1));
    let baseline = reserved(&fixture.service);
    assert!(matches!(
        fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &expired),
        Err(FirstSliceError::Cancelled(
            CancellationReason::DeadlineExceeded
        ))
    ));
    assert_eq!(reserved(&fixture.service), baseline);
}

#[test]
fn authenticated_malformed_index_arrays_refuse_inside_the_decoder_and_release_the_charge() {
    let malformed: &[&[u8]] = &[
        &[0x94],
        &[0x94, 0x01, 0x00, 0xdd, 0x00, 0x10, 0x00, 0x00],
        &[0x94, 0x01, 0x00, 0x91, 0x00, 0xdd, 0x00, 0x10, 0x00, 0x00],
        &[0x94, 0x01, 0x00, 0x91, 0x00, 0x91, 0x95],
        &[0x94, 0x01, 0x00, 0x92, 0x00],
    ];
    for encoded in malformed {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        let original = fixture
            .service
            .source_read_operation(fixture.receipt.generation, limits(), &cancellation)
            .unwrap()
            .unwrap();
        drop(original);
        crate::durable::read_operation::replace_index_with_matching_descriptor_for_test(
            fixture.paths.state_dir(),
            &fixture.receipt,
            encoded,
        );
        let baseline = reserved(&fixture.service);
        assert!(matches!(
            fixture.service.source_read_operation(
                fixture.receipt.generation,
                limits(),
                &cancellation
            ),
            Err(FirstSliceError::CatalogCorrupt)
        ));
        assert_eq!(reserved(&fixture.service), baseline);
    }
}
