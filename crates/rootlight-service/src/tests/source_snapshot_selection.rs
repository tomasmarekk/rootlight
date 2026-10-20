//! Selected source owners preserve exact request and lifecycle semantics.
//! The real service and strict source constructor must agree on the selected
//! owners while preserving global admission and immutable generation metadata.

use super::*;
use crate::office_fixture;
use rootlight_source::{SourceEncoding, SourceError, SourceService};

fn indexed_sources() -> (TempDir, FirstSliceService, FirstSliceIndexReceipt) {
    let root = tempfile::tempdir().unwrap();
    for (path, text) in [
        ("alpha.rs", "pub fn alpha() {}\n"),
        ("beta.rs", "pub fn beta() {}\n"),
        ("gamma.rs", "pub fn gamma() {}\n"),
    ] {
        fs::write(root.path().join(path), text).unwrap();
    }
    let mut service = FirstSliceService::new(4).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    (root, service, receipt)
}

fn references(service: &FirstSliceService, generation: GenerationId) -> Vec<SourceRef> {
    let lease = service.generation_lease(generation, &deadline()).unwrap();
    service
        .source_snapshots
        .snapshots(generation)
        .unwrap()
        .iter()
        .map(|source| {
            lease
                .generation()
                .find_file(source.file())
                .unwrap()
                .evidence
                .source
                .clone()
                .unwrap()
        })
        .collect()
}

fn admission(
    generation: GenerationId,
    retained: Vec<Arc<SourceSnapshot>>,
) -> SourceSnapshotAdmission {
    let mut additional_bytes = source_generation_reference_bytes(retained.len()).unwrap();
    for snapshot in &retained {
        additional_bytes = additional_bytes
            .checked_add(unique_source_snapshot_bytes(snapshot).unwrap())
            .unwrap();
    }
    SourceSnapshotAdmission {
        generation,
        retained,
        additional_bytes,
    }
}

#[test]
fn narrow_read_retains_only_requested_source_owner() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    assert!(refs.len() >= 3);
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let selected = service
        .source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            &refs[1..2],
            &deadline(),
        )
        .unwrap();
    assert_eq!(selected.len(), 1);
    let original = service
        .source_snapshots
        .snapshots(receipt.generation)
        .unwrap()
        .iter()
        .find(|snapshot| snapshot.file() == refs[1].span().file())
        .unwrap();
    assert!(Arc::ptr_eq(&selected[0], original));
    let source = SourceService::from_snapshots(&selected, lease.generation()).unwrap();
    let read = source
        .read(
            &refs[1..2],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.chunks[0].bytes, original.content());
}

#[test]
fn duplicate_selectors_keep_output_order_but_share_one_selected_owner() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    let requested = vec![refs[1].clone(), refs[0].clone(), refs[1].clone()];
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let selected = service
        .source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            &requested,
            &deadline(),
        )
        .unwrap();
    assert_eq!(selected.len(), 2);
    assert!(selected[0].file() < selected[1].file());
    let response = service
        .source_read(receipt.generation, requested.clone(), &deadline())
        .unwrap();
    assert_eq!(
        response
            .data
            .chunks
            .iter()
            .map(|chunk| chunk.reference.clone())
            .collect::<Vec<_>>(),
        requested
    );
}

#[test]
fn selected_owner_survives_service_release_with_its_generation_lease() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let selected = service
        .source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            &refs[..1],
            &deadline(),
        )
        .unwrap()
        .into_owned();
    assert_eq!(selected.len(), 1);
    drop(service);
    let source = SourceService::from_snapshots(&selected, lease.generation()).unwrap();
    assert_eq!(
        source
            .read(
                &refs[..1],
                SourceReadOptions::new(),
                SourceBudget::new(),
                &deadline()
            )
            .unwrap()
            .chunks
            .len(),
        1
    );
}

#[test]
fn stage_rejects_unrequested_global_disorder_before_any_publication() {
    let (_root, service, receipt) = indexed_sources();
    let mut snapshots = service
        .source_snapshots
        .snapshots(receipt.generation)
        .unwrap()
        .to_vec();
    snapshots.reverse();
    let mut retention = SourceSnapshotRetention::new(4, usize::MAX).unwrap();
    assert!(matches!(
        retention.stage(admission(receipt.generation, snapshots)),
        Err(FirstSliceError::Retention)
    ));
    assert!(retention.shared.is_empty());
    assert!(retention.staged.is_empty());
    assert_eq!(retention.retained_bytes, 0);
}

#[test]
fn stage_rejects_duplicate_identity_before_any_shared_reference_update() {
    let (_root, service, receipt) = indexed_sources();
    let source = service
        .source_snapshots
        .snapshots(receipt.generation)
        .unwrap()[0]
        .clone();
    let mut retention = SourceSnapshotRetention::new(4, usize::MAX).unwrap();
    assert!(matches!(
        retention.stage(admission(receipt.generation, vec![source.clone(), source])),
        Err(FirstSliceError::Retention)
    ));
    assert!(retention.shared.is_empty());
    assert!(retention.staged.is_empty());
}

#[test]
fn legitimate_unsorted_analysis_inputs_are_canonicalized_before_stage() {
    let (_root, service, receipt) = indexed_sources();
    let mut sources: Vec<_> = service
        .source_snapshots
        .snapshots(receipt.generation)
        .unwrap()
        .iter()
        .map(|snapshot| RustSourceInput {
            snapshot: snapshot.as_ref().clone(),
            encoding: "utf-8".to_owned(),
            generated: false,
            origins: Vec::new(),
        })
        .collect();
    sources.reverse();
    let mut retention = SourceSnapshotRetention::new(4, usize::MAX).unwrap();
    let admitted = retention
        .admit(receipt.generation, sources, &deadline())
        .unwrap();
    retention.stage(admitted).unwrap();
    retention.commit_staged(receipt.generation).unwrap();
    assert!(
        retention
            .snapshots(receipt.generation)
            .unwrap()
            .windows(2)
            .all(|pair| pair[0].file() < pair[1].file())
    );
}

#[test]
fn strict_source_constructor_still_rejects_unordered_external_owners() {
    let (_root, service, receipt) = indexed_sources();
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let mut snapshots = service
        .source_snapshots
        .snapshots(receipt.generation)
        .unwrap()
        .to_vec();
    snapshots.reverse();
    assert!(matches!(
        SourceService::from_snapshots(&snapshots, lease.generation()),
        Err(SourceError::InvalidSnapshotSet)
    ));
}

#[test]
fn duplicate_hard_cap_does_not_introduce_a_new_selector_limit() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    let requested = vec![refs[0].clone(); rootlight_source::HARD_MAX_SOURCE_SELECTORS + 1];
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let selected = service
        .source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            &requested,
            &deadline(),
        )
        .unwrap();
    assert!(matches!(selected, Cow::Borrowed(_)));
    assert!(matches!(
        service.source_read(receipt.generation, requested, &deadline()),
        Err(FirstSliceError::BudgetExceeded)
    ));
    let admitted = vec![refs[0].clone(); rootlight_source::HARD_MAX_SOURCE_SELECTORS];
    assert_eq!(
        service
            .source_read(receipt.generation, admitted, &deadline())
            .unwrap()
            .data
            .chunks
            .len(),
        rootlight_source::HARD_MAX_SOURCE_SELECTORS
    );
}

#[test]
fn reduced_selector_cap_and_cancel_keep_existing_typed_errors() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    assert!(matches!(
        service.source_read_with_budget(
            receipt.generation,
            refs[..2].to_vec(),
            FirstSliceBudget::default().reduce_source_max_selectors(1),
            &deadline()
        ),
        Err(FirstSliceError::BudgetExceeded)
    ));
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    assert!(matches!(
        service.source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            &refs[..1],
            &cancellation
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
}

#[test]
fn shed_durable_miss_hydrates_only_selected_file_without_installing_a_cache() {
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    for path in ["alpha.rs", "beta.rs", "gamma.rs"] {
        fs::write(root.path().join(path), "pub fn item() {}\n").unwrap();
    }
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let reference = lease
        .generation()
        .document()
        .files
        .iter()
        .find(|file| file.path == "beta.rs")
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    service
        .source_snapshots
        .shed_cached_sources(SourceSnapshotCacheEntry::Committed(receipt.generation))
        .unwrap();
    let selected = service
        .source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            &[reference],
            &deadline(),
        )
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert!(
        service
            .source_snapshots
            .snapshots(receipt.generation)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn derived_and_direct_container_reads_keep_exact_origin_metadata() {
    let root = tempfile::tempdir().unwrap();
    let body = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nEnd Sub\r\n";
    let bytes = office_fixture::book(&[("Client", body)]);
    fs::write(root.path().join("book.xlsm"), &bytes).unwrap();
    let mut service = FirstSliceService::new(4).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let mapping = lease
        .generation()
        .document()
        .source_mappings
        .iter()
        .find(|mapping| mapping.kind == rootlight_ir::SourceMappingKind::ExtractedFromContainer)
        .unwrap();
    let derived = mapping.from.clone();
    let container = mapping.to.clone();
    let selected = service
        .source_snapshots_for_read(
            receipt.generation,
            lease.generation(),
            std::slice::from_ref(&derived),
            &deadline(),
        )
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].file(), derived.span().file());
    let result = service
        .source_read(receipt.generation, vec![derived], &deadline())
        .unwrap();
    assert_eq!(
        result.data.chunks[0].extracted_from.as_ref(),
        Some(&container)
    );
    let result = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![container.clone()],
            SourceReadOptions::new()
                .with_encoding(SourceEncoding::Bytes)
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.data.chunks[0].reference, container);
    assert_eq!(
        content_hash(&result.data.chunks[0].bytes),
        content_hash(&bytes)
    );
}

#[test]
fn missing_direct_selector_does_not_hide_behind_other_selected_owners() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    let missing = SourceRef::new(
        refs[0].repository(),
        refs[0].generation(),
        SourceSpan::new(FileId::from_bytes([255; 20]), 0, 1).unwrap(),
        refs[0].content_hash(),
        None,
    );
    assert!(
        service
            .source_read(
                receipt.generation,
                vec![refs[0].clone(), missing],
                &deadline()
            )
            .is_err()
    );
    let stale = SourceRef::new(
        refs[0].repository(),
        GenerationId::from_bytes([254; 20]),
        refs[0].span(),
        refs[0].content_hash(),
        None,
    );
    let error = service
        .source_read(receipt.generation, vec![stale], &deadline())
        .expect_err("a stale generation must not return source bytes");
    // Source-read planning exposes foreign identities through the service's
    // existing Query category, including its generation mismatch error.
    assert!(matches!(error, FirstSliceError::Query), "{error:?}");
}

#[test]
fn foreign_repository_and_stale_hash_still_fail_closed_after_selection() {
    let (_root, service, receipt) = indexed_sources();
    let refs = references(&service, receipt.generation);
    let foreign = SourceRef::new(
        derive_repository(b"selected-source-foreign-repository").id(),
        refs[0].generation(),
        refs[0].span(),
        refs[0].content_hash(),
        None,
    );
    let error = service
        .source_read(receipt.generation, vec![foreign], &deadline())
        .expect_err("a foreign repository must not return source bytes");
    assert!(matches!(error, FirstSliceError::Query), "{error:?}");
    let stale = SourceRef::new(
        refs[0].repository(),
        refs[0].generation(),
        refs[0].span(),
        content_hash(b"different bytes"),
        None,
    );
    assert!(
        service
            .source_read(receipt.generation, vec![stale], &deadline())
            .is_err()
    );
}
