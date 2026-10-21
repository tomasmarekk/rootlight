//! Opaque Unicode source bytes retain identity without text or native admission.

use super::*;
use rootlight_source::{SourceEncoding, SourceError, SourceFindBudget, SourceFindError};

fn opaque_source() -> Vec<u8> {
    let mut bytes = b"\xff\xfe\0\0".to_vec();
    bytes.extend(
        "marker = 'value'\n"
            .repeat(256)
            .encode_utf16()
            .flat_map(u16::to_le_bytes),
    );
    bytes
}

fn policy(snapshot_bytes: u64) -> FirstSliceStoragePolicy {
    let contents = format!("version='1.1'\n[analysis]\nmax_source_file_bytes={snapshot_bytes}\n");
    let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: &contents,
    }])
    .unwrap();
    FirstSliceStoragePolicy::from_config(config, 3)
}

fn assert_opaque(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
    language: &str,
) -> FileId {
    let cancel = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .chain(
            snapshot
                .source_files()
                .entries()
                .iter()
                .map(|entry| entry.file()),
        )
        .find(|file| file.path == path)
        .unwrap();
    let file_id = file.id;
    assert_eq!(file.language, language);
    assert_eq!(file.encoding, "bytes");
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let source = file.evidence.source.as_ref().unwrap().clone();
    assert_eq!(source.repository(), receipt.repository);
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(
        source.span(),
        SourceSpan::new(file.id, 0, u64::try_from(bytes.len()).unwrap()).unwrap()
    );
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &cancel)
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedEncoding)
    );
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let selected: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source == source && gap.domain == domain)
            .collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].reason, SkippedRegionReason::UnsupportedEncoding);
        assert_eq!(selected[0].detail, "unsupported-encoding");
    }
    assert!(!document.entities.iter().any(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file.id)
    }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id)
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file.id)
    }));
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "unsupported-encoding"
                && diagnostic.source.as_ref() == Some(&source)
                && diagnostic.coverage_effect == CoverageStatus::Unknown)
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::new(),
            &cancel,
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, source);
    assert_eq!(read.data.chunks[0].bytes, bytes);
    let located = service
        .code_locate(
            receipt.generation,
            path.to_owned(),
            LocateMode::Exact,
            10,
            0,
            &cancel,
        )
        .unwrap();
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.file == file.id && hit.symbol.is_none())
    );
    assert_eq!(
        service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                "marker".to_owned(),
                LocateMode::Text,
                Vec::new(),
                vec![path.to_owned()],
                10,
                0,
                FirstSliceBudget::new(),
                &cancel
            )
            .unwrap_err(),
        FirstSliceError::SourceEncodingUnsupported
    );
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            file.id,
            "marker",
            None,
            SourceFindBudget::new(),
            &cancel
        ),
        Err(crate::source_find::FirstSliceSourceFindError::Find(
            SourceFindError::Source(SourceError::EncodingUnsupported)
        ))
    ));
    assert!(
        service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source.clone()],
                SourceReadOptions::new().with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::new()
                    .reduce_max_source_verification_bytes(u64::try_from(bytes.len()).unwrap() - 1),
                &cancel
            )
            .is_err()
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(
        service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source],
                SourceReadOptions::new().with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::new(),
                &cancelled
            )
            .is_err()
    );
    file_id
}

#[test]
fn opaque_bom_sources_keep_raw_identity_and_explicit_encoding_gaps_in_both_storage_paths() {
    let cancel = Cancellation::new();
    let bytes = opaque_source();
    // Both policies admit the fixed parser chunk; the payload still crosses
    // the smaller snapshot threshold and fits wholly beneath the larger one.
    assert!(bytes.len() > 4096 && bytes.len() <= 16384);
    for snapshot_bytes in [4096, 16384] {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let source = durable_test_tempdir();
        for path in ["opaque.toml", "ambiguous.mo"] {
            fs::write(source.path().join(path), &bytes).unwrap();
        }
        fs::write(
            source.path().join("sibling.rs"),
            b"pub fn retained_sibling() {}\n",
        )
        .unwrap();
        let policy = policy(snapshot_bytes);
        let mut service =
            FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &cancel)
                .unwrap();
        let receipt = service.index_repository(source.path(), &cancel).unwrap();
        assert_eq!(receipt.discovered_inputs, 3);
        let first = assert_opaque(&service, &receipt, "opaque.toml", &bytes, "toml");
        assert_opaque(&service, &receipt, "ambiguous.mo", &bytes, "unknown");
        assert_eq!(
            service.index_repository(source.path(), &cancel).unwrap(),
            receipt
        );
        for path in ["opaque.toml", "ambiguous.mo"] {
            fs::write(source.path().join(path), b"live replacement").unwrap();
        }
        assert_eq!(
            assert_opaque(&service, &receipt, "opaque.toml", &bytes, "toml"),
            first
        );
        drop(service);
        let restored =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &cancel).unwrap();
        assert_eq!(
            assert_opaque(&restored, &receipt, "opaque.toml", &bytes, "toml"),
            first
        );
        assert_opaque(&restored, &receipt, "ambiguous.mo", &bytes, "unknown");
    }
}

#[test]
fn opaque_bom_updates_preserve_file_identity_and_keep_old_generation_bytes() {
    let source = durable_test_tempdir();
    let before = opaque_source();
    let mut after = before.clone();
    after.extend_from_slice(b"!\0");
    fs::write(source.path().join("opaque.toml"), &before).unwrap();
    fs::write(
        source.path().join("sibling.rs"),
        b"pub fn retained_sibling() {}\n",
    )
    .unwrap();
    let cancel = Cancellation::new();
    let mut service = FirstSliceService::new(3).unwrap();
    let first = service.index_repository(source.path(), &cancel).unwrap();
    let file = assert_opaque(&service, &first, "opaque.toml", &before, "toml");
    fs::write(source.path().join("opaque.toml"), &after).unwrap();
    let second = service.index_repository(source.path(), &cancel).unwrap();
    assert_ne!(first.generation, second.generation);
    assert_eq!(
        file,
        assert_opaque(&service, &second, "opaque.toml", &after, "toml")
    );
    assert_eq!(
        file,
        assert_opaque(&service, &first, "opaque.toml", &before, "toml")
    );
}

#[test]
fn binary_and_non_bom_unknown_dispositions_are_not_reinterpreted() {
    let source = durable_test_tempdir();
    for (path, bytes) in [
        ("nul.toml", b"key=1\0".as_slice()),
        ("pdf.toml", b"%PDF-1.7\n\0"),
        ("png.rs", b"\x89PNG\r\n\x1a\n\0"),
        ("zip.rs", b"PK\x03\x04\0"),
        ("unknown.data", b"\xff\xfe\0\xd8\0\0"),
    ] {
        fs::write(source.path().join(path), bytes).unwrap();
    }
    let raw = b"opaque\xff";
    fs::write(source.path().join("unknown.unrecognized"), raw).unwrap();
    fs::write(
        source.path().join("sibling.rs"),
        b"pub fn retained_sibling() {}\n",
    )
    .unwrap();
    let cancel = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(source.path(), &cancel).unwrap();
    let lease = service
        .generation_lease(receipt.generation, &cancel)
        .unwrap();
    let document = lease.generation().document();
    assert_eq!(document.files.len(), 2);
    let unknown = document
        .files
        .iter()
        .find(|file| file.path == "unknown.unrecognized")
        .unwrap();
    assert_eq!(unknown.encoding, "bytes");
    let coverage = service
        .source_file_coverage_until(receipt.generation, unknown.id, &cancel)
        .unwrap();
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "retained_sibling")
    );
}
