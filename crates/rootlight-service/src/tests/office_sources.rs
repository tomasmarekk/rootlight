//! Opaque Office inputs must survive the complete durable source-read path.
//! Retention is file-only evidence and never proves extracted module coverage.

use super::*;

fn assert_container(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
    format: &str,
) -> SourceRef {
    let located = service
        .code_locate(
            receipt.generation,
            path.to_owned(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    let hit = located
        .data
        .hits
        .iter()
        .find(|hit| hit.symbol.is_none() && hit.path == path)
        .unwrap();
    let reference = hit.source.clone().unwrap();
    assert_eq!(reference.content_hash(), content_hash(bytes));
    let coverage = service
        .source_file_coverage_until(receipt.generation, hit.file, &deadline())
        .unwrap();
    assert_eq!(coverage.language, format);
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedEncoding)
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, bytes);
    assert_eq!(read.data.chunks[0].reference, reference);
    assert!(
        service
            .source_read(receipt.generation, vec![reference.clone()], &deadline())
            .is_err()
    );
    reference
}

#[test]
fn office_sources_remain_file_only_across_edit_rebuild_and_restart() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let first = b"PK\x03\x04\0initial opaque package";
    let second = b"PK\x03\x04\0updated opaque package";
    let compound = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\0opaque compound";
    fs::write(fixture.path().join("book.xlsm"), first).unwrap();
    fs::write(fixture.path().join("legacy.doc"), compound).unwrap();
    fs::write(fixture.path().join("peer.rs"), "pub fn peer() {}\n").unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let initial = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(initial.discovered_inputs, 3);
    assert_eq!(
        initial.indexed_files, 3,
        "receipt counts retained file facts, not structural coverage"
    );
    let initial_ref = assert_container(&service, &initial, "book.xlsm", first, "office-open-xml");
    assert_container(
        &service,
        &initial,
        "legacy.doc",
        compound,
        "compound-document",
    );
    assert_eq!(
        service
            .index_repository(fixture.path(), &deadline())
            .unwrap(),
        initial
    );
    fs::write(fixture.path().join("book.xlsm"), second).unwrap();
    let updated = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(initial.generation, updated.generation);
    let updated_ref = assert_container(&service, &updated, "book.xlsm", second, "office-open-xml");
    assert_eq!(initial_ref.span().file(), updated_ref.span().file());
    assert_ne!(initial_ref.content_hash(), updated_ref.content_hash());
    assert!(
        service
            .source_read_with_options_and_budget(
                updated.generation,
                vec![initial_ref],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(rootlight_source::SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline()
            )
            .is_err()
    );
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        0
    );
    let prepared = service
        .prepare_repository_with_options(
            fixture.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &deadline(),
        )
        .unwrap();
    let clean = service.publish_prepared(prepared, &deadline()).unwrap();
    assert!(updated.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, updated.logical_snapshot);
    drop(service);
    fs::remove_file(fixture.path().join("book.xlsm")).unwrap();
    fs::remove_file(fixture.path().join("legacy.doc")).unwrap();
    let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    for (receipt, bytes) in [
        (&initial, first.as_slice()),
        (&updated, second.as_slice()),
        (&clean, second.as_slice()),
    ] {
        assert_container(&restored, receipt, "book.xlsm", bytes, "office-open-xml");
        assert_container(
            &restored,
            receipt,
            "legacy.doc",
            compound,
            "compound-document",
        );
        let status = restored
            .repository_status(receipt.repository, Some(receipt.generation))
            .unwrap();
        assert_eq!(
            status
                .coverage
                .iter()
                .map(|row| row.discovered_files)
                .sum::<u64>(),
            3
        );
        assert_eq!(
            status
                .coverage
                .iter()
                .map(|row| row.indexed_files)
                .sum::<u64>(),
            1
        );
    }
}
