//! Small real lifecycle checks for identical seal/reopen capacity admission.
//! A thread-local test ceiling models the unchanged production hard ceiling;
//! production builds never consult this state or relax their normal guards.

use std::{cell::Cell, fs};

use rootlight_runtime::RuntimePaths;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};
use tempfile::TempDir;

use super::*;
use crate::{
    FirstSliceBudget, FirstSliceService, GENERATION_MEMORY_FIXED_OVERHEAD_BYTES,
    PendingGenerationMemory,
};
use rootlight_adapter_sdk::AnalysisLimits;

thread_local! {
    pub(super) static RESTORE_CEILING: Cell<Option<u64>> = const { Cell::new(None) };
}

pub(super) fn admit_restore(bytes: u64) -> Result<(), FirstSliceError> {
    if let Some(limit) = RESTORE_CEILING.with(Cell::get)
        && bytes > limit
    {
        let mut error = crate::generation_memory_limit(0, bytes, PendingGenerationMemory::Reserved);
        if let FirstSliceError::GenerationMemoryLimit { limit: actual, .. } = &mut error {
            *actual = limit;
        }
        return Err(error);
    }
    Ok(())
}

fn with_ceiling<T>(limit: u64, operation: impl FnOnce() -> T) -> T {
    struct Reset(Option<u64>);
    impl Drop for Reset {
        fn drop(&mut self) {
            RESTORE_CEILING.with(|slot| slot.set(self.0));
        }
    }
    let _reset = Reset(RESTORE_CEILING.with(|slot| slot.replace(Some(limit))));
    operation()
}

fn source() -> Vec<u8> {
    let mut text = String::from("// ababa\n");
    for ordinal in 0..1024 {
        use std::fmt::Write as _;
        writeln!(text, "pub fn capacity_{ordinal}() -> u32 {{ {ordinal} }}").unwrap();
    }
    text.into_bytes()
}

fn fixture() -> (TempDir, TempDir, RuntimePaths) {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(root.path().join("lib.rs"), source()).unwrap();
    fs::write(root.path().join("notes.txt"), b"written source note\n").unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    (storage, root, paths)
}

fn manifest_path(paths: &RuntimePaths, receipt: &crate::FirstSliceIndexReceipt) -> PathBuf {
    paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string())
}

fn peak(paths: &RuntimePaths, receipt: &crate::FirstSliceIndexReceipt) -> u64 {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let directory = manifest_path(paths, receipt);
    let manifest: DurableGenerationManifest =
        serde_json::from_slice(&fs::read(directory.join(MANIFEST_FILENAME)).unwrap()).unwrap();
    let mut plan = None;
    let refusal = OracleReader::open_in(&directory, &context)
        .unwrap()
        .read_with_admission(&context, |measured| {
            plan = Some(measured);
            Err::<(), ()>(())
        });
    assert!(matches!(
        refusal,
        Err(OracleReadAdmissionError::Admission(()))
    ));
    oracle_restore_memory_bytes(
        plan.unwrap(),
        receipt.indexed_files > 1 && manifest.source_file_catalog.is_some(),
        recovery_sidecar_memory_bytes(&manifest).unwrap(),
    )
    .unwrap()
}

fn force_file_catalog(service: &mut FirstSliceService) {
    let current = &service.analysis_limits;
    let mut ir = current.ir().clone();
    ir.max_files = 1;
    let mut limited = AnalysisLimits::new(
        current.max_source_bytes(),
        current.max_syntax_nodes(),
        current.max_syntax_depth(),
        current.max_embedded_ranges(),
        current.max_reported_memory_bytes(),
        current.syntax_stream().clone(),
        current.ir_stream().clone(),
        ir,
    )
    .unwrap();
    if let Some(project) = current.project() {
        limited = limited.with_project_limits(project);
    }
    service.analysis_limits = limited;
}

fn measured_ceiling() -> u64 {
    let (_storage, root, paths) = fixture();
    let cancellation = Cancellation::new();
    let mut full = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let receipt = full.index_repository(root.path(), &cancellation).unwrap();
    assert!(
        !full
            .loaded_generation_snapshot(receipt.generation)
            .unwrap()
            .document()
            .entities
            .is_empty()
    );
    let full_peak = peak(&paths, &receipt);
    let (_raw_storage, raw_root, raw_paths) = fixture();
    let mut raw = FirstSliceService::new_durable(2, raw_paths.state_dir(), &cancellation).unwrap();
    force_file_catalog(&mut raw);
    let raw_receipt = raw
        .index_repository(raw_root.path(), &cancellation)
        .unwrap();
    let raw_peak = peak(&raw_paths, &raw_receipt);
    assert!(
        full_peak > raw_peak + 1024,
        "full={full_peak}, raw={raw_peak}"
    );
    raw_peak + (full_peak - raw_peak) / 2
}

fn assert_sources(
    service: &FirstSliceService,
    receipt: &crate::FirstSliceIndexReceipt,
    expected: &[u8],
) {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
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
        .find(|file| file.path == "lib.rs")
        .unwrap();
    assert_eq!(file.language, "rust");
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.byte_length, expected.len() as u64);
    assert_eq!(file.content_hash, rootlight_ids::content_hash(expected));
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &cancellation)
        .unwrap();
    assert_eq!(coverage.status, rootlight_ir::CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(crate::FirstSliceSourceCoverageReason::ResourceBounded)
    );
    let full = file.evidence.source.clone().unwrap();
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![full.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].reference, full);
    assert_eq!(read.data.chunks[0].bytes, expected);
    let mut cursor = None;
    let mut next = 0;
    let mut matches = Vec::new();
    loop {
        let mut budget = SourceFindBudget::new();
        budget.matcher.max_scan_bytes = if next < 8 { 4 } else { expected.len() };
        let page = service
            .find_literal(
                receipt.generation,
                file.id,
                "aba",
                cursor,
                budget,
                &cancellation,
            )
            .unwrap();
        assert_eq!(page.scanned.start, next);
        next = page.scanned.end;
        matches.extend(page.matches);
        cursor = page.continuation;
        if cursor.is_none() {
            assert_eq!(page.stop, SourceFindStop::EndOfSource);
            break;
        }
    }
    assert_eq!(next, expected.len() as u64);
    assert_eq!(
        matches
            .iter()
            .map(|source| (source.span().start_byte(), source.span().end_byte()))
            .collect::<Vec<_>>(),
        [(3, 6), (5, 8)]
    );
    let absent = service
        .find_literal(
            receipt.generation,
            file.id,
            "missing-source-marker",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert!(absent.matches.is_empty());
    assert_eq!(absent.stop, SourceFindStop::EndOfSource);
    assert!(absent.continuation.is_none());
}

#[test]
fn producer_and_consumer_share_actual_sidecar_and_catalog_charges() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    for mode in 0..3 {
        let (_storage, root, paths) = fixture();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        if mode == 2 {
            force_file_catalog(&mut service);
        }
        let receipt = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let verified =
            IdentityVerifiedGeneration::verify_snapshot(snapshot.clone(), &context).unwrap();
        let prepared = service
            .durable
            .as_ref()
            .unwrap()
            .begin_generation(receipt.repository, receipt.generation)
            .unwrap();
        if mode != 0 {
            prepared
                .write_source_file_catalog(snapshot.source_files(), &cancellation)
                .unwrap();
        }
        prepared
            .write_source_authority(&verified, &cancellation)
            .unwrap();
        let incremental = PreparedIncrementalState {
            baseline: service.incremental_baselines[&receipt.generation].clone(),
            inputs: service.incremental_inputs[&receipt.generation].clone(),
            evidence: service.incremental_evidence[&receipt.generation].clone(),
        };
        prepared
            .write_incremental_state(&incremental, &cancellation)
            .unwrap();
        let (reader, _, written) = rootlight_catalog::OracleWriter::create_in(prepared.path())
            .unwrap()
            .seal_preserving_verified_with_read_plan(verified, &context)
            .unwrap();
        prepared
            .account_external_staging_bytes(reader.allocated_bytes(&context).unwrap())
            .unwrap();
        let mut read_plan = None;
        let stopped = reader.read_with_admission(&context, |plan| {
            read_plan = Some(plan);
            Err::<(), ()>(())
        });
        assert!(matches!(
            stopped,
            Err(OracleReadAdmissionError::Admission(()))
        ));
        assert_eq!(read_plan, Some(written));
        let mut manifest: DurableGenerationManifest = serde_json::from_slice(
            &fs::read(manifest_path(&paths, &receipt).join(MANIFEST_FILENAME)).unwrap(),
        )
        .unwrap();
        manifest.incremental_state = *prepared.incremental_state.lock().unwrap();
        manifest.source_file_catalog = *prepared.source_file_catalog.lock().unwrap();
        manifest.source_authority = *prepared.source_authority.lock().unwrap();
        manifest.source_storage = *prepared.source_storage.lock().unwrap();
        let actual_nonempty = !snapshot.source_files().is_empty();
        assert_eq!(actual_nonempty, mode == 2);
        assert_eq!(manifest.source_file_catalog.is_some(), mode != 0);
        let consumer = oracle_restore_memory_bytes(
            read_plan.unwrap(),
            actual_nonempty,
            recovery_sidecar_memory_bytes(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            prepared
                .oracle_restore_memory_bytes(written, receipt.indexed_files, actual_nonempty)
                .unwrap(),
            consumer
        );
    }
}

#[test]
fn producer_preserves_sources_when_its_full_restore_cannot_fit() {
    let ceiling = measured_ceiling();
    with_ceiling(ceiling, || {
        let (_storage, root, paths) = fixture();
        let cancellation = Cancellation::new();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let first = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(first.generation)
            .unwrap();
        assert!(
            snapshot.document().entities.is_empty(),
            "newly published full graph cannot satisfy its own reopen ceiling"
        );
        assert!(snapshot.document().occurrences.is_empty());
        assert!(snapshot.document().relations.is_empty());
        assert_eq!(snapshot.file_count(), 2);
        assert!(
            snapshot
                .document()
                .diagnostics
                .iter()
                .any(
                    |diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE
                        && diagnostic.message.contains("modeled bytes above limit")
                )
        );
        assert_sources(&service, &first, &source());
        assert_eq!(
            service
                .index_repository(root.path(), &cancellation)
                .unwrap(),
            first
        );
        let mut changed = source();
        changed.extend_from_slice(b"// source changed\n");
        fs::write(root.path().join("lib.rs"), &changed).unwrap();
        let second = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        assert_ne!(second.generation, first.generation);
        assert_eq!(second.parent, Some(first.generation));
        assert_sources(&service, &second, &changed);
        assert_eq!(
            service
                .generation_cache
                .lock()
                .unwrap()
                .reserved_memory_bytes()
                .unwrap(),
            0
        );
        drop(service);
        let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        assert_eq!(
            reopened.active_generation_for(first.repository),
            Some(second.generation)
        );
        assert_sources(&reopened, &first, &source());
        assert_sources(&reopened, &second, &changed);
        let repository = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(first.repository.to_string());
        assert!(fs::read_dir(repository).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        }));
        assert_eq!(
            reopened
                .generation_cache
                .lock()
                .unwrap()
                .reserved_memory_bytes()
                .unwrap(),
            0
        );
    });
}

fn assert_no_publication(service: &FirstSliceService, paths: &RuntimePaths) {
    assert!(service.repositories.is_empty());
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
    let repositories = paths.state_dir().join("first-slice/repositories");
    for repository in fs::read_dir(repositories).unwrap() {
        for generation in fs::read_dir(repository.unwrap().path()).unwrap() {
            let entry = generation.unwrap();
            assert!(
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STAGING_PREFIX)
            );
            assert!(!entry.path().join(MANIFEST_FILENAME).exists());
        }
    }
}

#[test]
fn private_capacity_cause_keeps_exact_limit_and_rejects_unrelated_failures() {
    use crate::aggregate_source_recovery::DurableRestoreCapacity;
    with_ceiling(1024, || {
        assert!(DurableRestoreCapacity::from_sealed_plan(1024).is_none());
        assert!(DurableRestoreCapacity::from_sealed_plan(u64::MAX).is_none());
        let cause = DurableRestoreCapacity::from_sealed_plan(1025).unwrap();
        let error = cause.error();
        assert!(matches!(
            error,
            FirstSliceError::GenerationMemoryLimit {
                observed: 1025,
                limit: 1024,
                ..
            }
        ));
        assert!(cause.matches(&error));
        assert!(!cause.matches(&crate::generation_memory_limit(
            0,
            1025,
            PendingGenerationMemory::Staged
        )));
        assert!(!cause.matches(&FirstSliceError::Cancelled(
            rootlight_cancel::CancellationReason::ClientRequest
        )));
        assert!(!cause.matches(&FirstSliceError::CatalogCorrupt));
    });
    assert_eq!(
        oracle_restore_memory_limit(),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
    );
}

#[test]
fn unfit_source_only_output_and_derived_sources_remain_typed_failures() {
    for derived in [false, true] {
        let (_storage, root, paths) = fixture();
        if derived {
            let module = b"Attribute VB_Name = \"Module\"\r\nPublic Sub Keep()\r\nEnd Sub\r\n";
            fs::write(
                root.path().join("book.xlsm"),
                crate::office_fixture::book(&[("Module", module)]),
            )
            .unwrap();
        }
        let cancellation = Cancellation::new();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        crate::aggregate_source_recovery::FRAME_SEALS.with(|count| count.set(0));
        let limit = GENERATION_MEMORY_FIXED_OVERHEAD_BYTES - 1;
        let error = with_ceiling(limit, || {
            service.index_repository(root.path(), &cancellation)
        })
        .unwrap_err();
        assert!(
            matches!(error, FirstSliceError::GenerationMemoryLimit { observed, limit: actual, .. } if observed > limit && actual == limit),
            "{error:?}"
        );
        assert_eq!(
            crate::aggregate_source_recovery::FRAME_SEALS.with(Cell::get),
            usize::from(!derived)
        );
        assert_no_publication(&service, &paths);
    }
}

#[test]
fn cancellation_and_competing_memory_never_turn_into_capacity_recovery() {
    let (_storage, root, paths) = fixture();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let pressure = crate::RestoredMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
        true,
    )
    .unwrap();
    crate::aggregate_source_recovery::FRAME_SEALS.with(|count| count.set(0));
    let error = service
        .index_repository(root.path(), &cancellation)
        .unwrap_err();
    assert!(matches!(
        error,
        FirstSliceError::GenerationMemoryLimit {
            limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            ..
        }
    ));
    assert_eq!(
        crate::aggregate_source_recovery::FRAME_SEALS.with(Cell::get),
        0
    );
    drop(pressure);
    assert_no_publication(&service, &paths);
    let cancelled = Cancellation::new();
    let result = service.prepare_repository_with_mode_and_progress(
        root.path(),
        crate::FirstSliceIndexMode::Structural,
        &cancelled,
        |progress| {
            if progress.stage == crate::FirstSliceIndexStage::Search {
                cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            }
        },
    );
    assert!(matches!(
        result,
        Err(FirstSliceError::Cancelled(
            rootlight_cancel::CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(
        crate::aggregate_source_recovery::FRAME_SEALS.with(Cell::get),
        0
    );
    assert_no_publication(&service, &paths);
}
