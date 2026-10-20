//! Actual native-source lifecycle controls for intrinsic projection admission.

use std::{cell::Cell, fs, sync::OnceLock};

use rootlight_cancel::CancellationReason;
use rootlight_runtime::RuntimePaths;
use rootlight_source::{SourceEncoding, SourceReadOptions};
use tempfile::TempDir;

use super::*;

thread_local! {
    static CEILING: Cell<Option<u64>> = const { Cell::new(None) };
    static CAPTURE: Cell<bool> = const { Cell::new(false) };
    static PROJECTION: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
}

pub(super) fn ceiling() -> Option<u64> {
    CEILING.with(Cell::get)
}

pub(super) fn observe_projection(admitted_before: u64, peak: u64) {
    if CAPTURE.with(Cell::get) {
        assert!(
            PROJECTION
                .with(|value| value.replace(Some((admitted_before, peak))))
                .is_none()
        );
        eprintln!(
            "{}",
            serde_json::json!({
                "schema": "rootlight.fresh-projection-test-observation/1",
                "event": "projection", "before_projection": admitted_before,
                "peak": peak, "ceiling": ceiling(),
            })
        );
    }
}

fn with_ceiling<T>(limit: u64, operation: impl FnOnce() -> T) -> T {
    struct Reset(Option<u64>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CEILING.with(|value| value.set(self.0));
        }
    }
    assert!(limit <= MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES);
    let _reset = Reset(CEILING.with(|value| value.replace(Some(limit))));
    operation()
}

fn capture<T>(operation: impl FnOnce() -> T) -> (T, (u64, u64)) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CAPTURE.with(|value| value.set(false));
        }
    }
    assert!(!CAPTURE.with(|value| value.replace(true)));
    PROJECTION.with(|value| value.set(None));
    let _reset = Reset;
    let result = operation();
    (
        result,
        PROJECTION
            .with(Cell::take)
            .expect("real native projection reached"),
    )
}

fn authored(file: usize) -> Vec<u8> {
    let mut source = String::from("// ababa\n");
    for ordinal in 0..256 {
        use std::fmt::Write as _;
        writeln!(
            source,
            "pub fn native_{file}_{ordinal}() -> u32 {{ {ordinal} }}"
        )
        .unwrap();
    }
    source.into_bytes()
}

fn fixture() -> (TempDir, TempDir, RuntimePaths) {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for file in 0..64 {
        fs::write(
            root.path().join(format!("member_{file:02}.rs")),
            authored(file),
        )
        .unwrap();
    }
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    (storage, root, paths)
}

fn observe_cache(phase: &str, service: &FirstSliceService, generation: GenerationId) {
    let cache = service.generation_cache.lock().unwrap();
    let resident = cache.resident_logical_charge_bytes().unwrap();
    let reserved = cache.reserved_memory_bytes().unwrap();
    let own_charge = cache
        .logical_charge_bytes_by_generation
        .get(&generation)
        .copied();
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "rootlight.fresh-projection-test-observation/2",
            "event": "cache", "phase": phase, "generation": generation.to_string(),
            "ceiling": ceiling(), "generation_loaded": cache.generations.is_loaded(generation),
            "resident_logical_bytes": resident, "reserved_bytes": reserved,
            "generation_logical_bytes": own_charge,
            "cold_reload_construction_bytes": own_charge.and_then(|bytes| bytes.checked_mul(3)),
            "restored_reservation_bytes": cache.restored_reservation_bytes,
            "prepared_artifact_reservation_bytes": cache.prepared_artifact_reservation_bytes,
            "generation_reservation_bytes": cache.reservations.get(&generation).copied(),
        })
    );
}

fn assert_sources(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    unknown: bool,
    changed: bool,
    phase: &str,
) {
    let cancellation = Cancellation::new();
    observe_cache(phase, service, receipt.generation);
    let leased = service.generation_lease(receipt.generation, &cancellation);
    if let Err(error) = &leased {
        eprintln!(
            "{}",
            serde_json::json!({
                "schema": "rootlight.fresh-projection-test-observation/2",
                "event": "lease_error", "phase": phase,
                "generation": receipt.generation.to_string(), "error": format!("{error:?}"),
            })
        );
        observe_cache(phase, service, receipt.generation);
    }
    let lease = leased.unwrap();
    observe_cache(phase, service, receipt.generation);
    let snapshot = lease.generation();
    assert_eq!(snapshot.file_count(), 64);
    let mut count = 0;
    for file in snapshot.document().files.iter().chain(
        snapshot
            .source_files()
            .entries()
            .iter()
            .map(|entry| entry.file()),
    ) {
        let ordinal: usize = file
            .path
            .strip_prefix("member_")
            .unwrap()
            .strip_suffix(".rs")
            .unwrap()
            .parse()
            .unwrap();
        let mut expected = authored(ordinal);
        if changed && ordinal == 0 {
            expected.extend_from_slice(b"// changed source\n");
        }
        assert_eq!(file.language, "rust");
        assert_eq!(file.encoding, "utf-8");
        assert!(!file.generated);
        assert_eq!(file.content_hash, content_hash(&expected));
        assert_eq!(file.byte_length, expected.len() as u64);
        let full = file.evidence.source.clone().unwrap();
        assert_eq!(full.repository(), receipt.repository);
        assert_eq!(full.generation(), receipt.generation);
        assert_eq!(full.content_hash(), file.content_hash);
        assert_eq!(
            (full.span().start_byte(), full.span().end_byte()),
            (0, file.byte_length)
        );
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
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, full);
        assert_eq!(read.data.chunks[0].bytes, expected);
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &cancellation)
            .unwrap();
        if unknown {
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::ResourceBounded)
            );
        }
        count += 1;
    }
    assert_eq!(count, 64);
    if !unknown {
        let first = snapshot
            .document()
            .entities
            .iter()
            .find(|entity| entity.display_name == "native_0_0")
            .expect("native definition retained");
        assert_eq!(first.kind, EntityKind::Function);
        assert!(first.evidence.source.is_some());
    }
}

#[derive(Clone, Copy)]
struct Measured {
    before_projection: u64,
    source_only_peak: u64,
    source_only_resident: u64,
    source_only_cold_reload: u64,
    projection: u64,
    ceiling: u64,
}

fn measured() -> Measured {
    static MEASURED: OnceLock<Measured> = OnceLock::new();
    *MEASURED.get_or_init(|| {
        assert!(ceiling().is_none(), "measure the original default policy first");
        let (_storage, root, paths) = fixture();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let (receipt, (before_projection, projection)) = capture(|| {
            service.index_repository(root.path(), &cancellation).unwrap()
        });
        assert_sources(&service, &receipt, false, false, "calibration-native");
        drop(service);

        // This existing typed text boundary exercises a real conserved recovery
        // frame. Its additional aggregate marker makes the calibration stricter
        // than the proposed fresh projection's smaller file-only diagnostic.
        let (_raw_storage, raw_root, raw_paths) = fixture();
        let mut raw = FirstSliceService::new_durable(2, raw_paths.state_dir(), &cancellation).unwrap();
        let defaults = GenerationBudget::default();
        raw.generation_budget = GenerationBudget::new(
            defaults.limit(rootlight_storage::GenerationResource::Rows),
            defaults.limit(rootlight_storage::GenerationResource::SourceReferences),
            64 * 1024,
        ).unwrap();
        let prepared = raw.prepare_repository(raw_root.path(), &cancellation).unwrap();
        let FirstSliceIndexPreparation::Pending(pending) = &prepared else {
            panic!("fresh calibration must prepare");
        };
        assert!(pending.verified.document().entities.is_empty());
        assert!(pending.verified.document().diagnostics.iter().any(|diagnostic| {
            diagnostic.code == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_DIAGNOSTIC
        }));
        let source_only_peak = pending.reserved_memory_bytes;
        let source_only_resident = pending.memory_bytes;
        // reserve_reload admits three exact recorded resident charges before
        // decoding. Pending preparation alone does not include that envelope.
        let source_only_cold_reload = source_only_resident.checked_mul(3).unwrap();
        let lower = before_projection
            .max(source_only_peak)
            .max(source_only_cold_reload);
        assert!(lower < projection,
            "no real admission interval: before={before_projection}, source_only={source_only_peak}, resident={source_only_resident}, cold_reload={source_only_cold_reload}, projection={projection}");
        let ceiling = lower + (projection - lower) / 2;
        assert!(lower <= ceiling && ceiling < projection);
        assert!(projection < MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES);
        eprintln!("{}", serde_json::json!({
            "schema": "rootlight.fresh-projection-test-observation/1",
            "event": "calibration", "before_projection": before_projection,
            "source_only_peak": source_only_peak,
            "source_only_resident": source_only_resident,
            "source_only_cold_reload": source_only_cold_reload,
            "projection": projection,
            "ceiling": ceiling, "hard_ceiling": MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
        }));
        drop(prepared);
        Measured { before_projection, source_only_peak, source_only_resident, source_only_cold_reload, projection, ceiling }
    })
}

fn assert_no_staging_or_reservations(service: &FirstSliceService, paths: &RuntimePaths) {
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
    if repositories.exists() {
        for repository in fs::read_dir(repositories).unwrap() {
            for entry in fs::read_dir(repository.unwrap().path()).unwrap() {
                assert!(
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("stage-")
                );
            }
        }
    }
}

#[test]
fn native_fresh_projection_preserves_exact_sources_under_the_same_ceiling() {
    let measured = measured();
    assert!(measured.before_projection <= measured.ceiling);
    assert!(measured.source_only_peak <= measured.ceiling);
    assert!(measured.source_only_resident <= measured.ceiling);
    assert!(measured.source_only_cold_reload <= measured.ceiling);
    assert!(measured.projection > measured.ceiling);
    with_ceiling(measured.ceiling, || {
        let (_storage, root, paths) = fixture();
        let cancellation = Cancellation::new();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let (first, (_, actual_peak)) = capture(|| {
            service.index_repository(root.path(), &cancellation).expect(
                "intrinsic fresh projection refusal must preserve exact source availability",
            )
        });
        assert!(actual_peak > measured.ceiling);
        assert_sources(&service, &first, true, false, "fresh-first");
        let lease = service
            .generation_lease(first.generation, &cancellation)
            .unwrap();
        let snapshot = lease.generation();
        assert!(snapshot.document().entities.is_empty());
        assert!(snapshot.document().occurrences.is_empty());
        assert!(snapshot.document().relations.is_empty());
        assert!(snapshot.document().diagnostics.iter().any(|diagnostic| {
            diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE
                && diagnostic.message.contains("fresh projection capacity")
                && diagnostic.message.contains("prevented graph verification")
                && diagnostic.message.contains(&actual_peak.to_string())
                && diagnostic.message.contains(&measured.ceiling.to_string())
        }));
        drop(lease);
        assert_eq!(
            service
                .index_repository(root.path(), &cancellation)
                .unwrap(),
            first
        );
        let mut changed = authored(0);
        changed.extend_from_slice(b"// changed source\n");
        fs::write(root.path().join("member_00.rs"), changed).unwrap();
        let second = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        assert_eq!(second.parent, Some(first.generation));
        assert_sources(&service, &second, true, true, "incremental-second");
        assert_no_staging_or_reservations(&service, &paths);
        drop(service);
        let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        assert_eq!(
            reopened.active_generation_for(first.repository),
            Some(second.generation)
        );
        assert_sources(&reopened, &first, true, false, "reopened-first");
        assert_sources(&reopened, &second, true, true, "reopened-second");
        assert_no_staging_or_reservations(&reopened, &paths);
    });
}

fn tighten_ir_without_rejecting_fixture(service: &mut FirstSliceService) {
    let old = &service.analysis_limits;
    let mut ir = old.ir().clone();
    ir.max_entities -= 1;
    let mut limits = rootlight_adapter_sdk::AnalysisLimits::new(
        old.max_source_bytes(),
        old.max_syntax_nodes(),
        old.max_syntax_depth(),
        old.max_embedded_ranges(),
        old.max_reported_memory_bytes(),
        old.syntax_stream().clone(),
        old.ir_stream().clone(),
        ir,
    )
    .unwrap();
    if let Some(project) = old.project() {
        limits = limits.with_project_limits(project);
    }
    service.analysis_limits = limits;
}

#[test]
fn explicit_deep_and_custom_limits_keep_their_real_refusal() {
    let measured = measured();
    for control in 0..3 {
        with_ceiling(measured.ceiling, || {
            let (_storage, root, paths) = fixture();
            let cancellation = Cancellation::new();
            let mut service =
                FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
            let mode = if control == 0 {
                FirstSliceIndexMode::Deep
            } else {
                FirstSliceIndexMode::Structural
            };
            if control == 1 {
                tighten_ir_without_rejecting_fixture(&mut service);
            }
            if control == 2 {
                let defaults = GenerationBudget::default();
                service.generation_budget = GenerationBudget::new(
                    defaults.limit(rootlight_storage::GenerationResource::Rows) - 1,
                    defaults.limit(rootlight_storage::GenerationResource::SourceReferences),
                    defaults.limit(rootlight_storage::GenerationResource::TextBytes),
                )
                .unwrap();
            }
            aggregate_source_recovery::FRAME_SEALS.with(|value| value.set(0));
            let result = service.index_repository_with_mode(root.path(), mode, &cancellation);
            assert!(
                matches!(result, Err(FirstSliceError::GenerationMemoryLimit { observed, limit, .. })
                if observed > measured.ceiling && limit == measured.ceiling),
                "{result:?}"
            );
            assert_eq!(aggregate_source_recovery::FRAME_SEALS.with(Cell::get), 0);
            assert_no_staging_or_reservations(&service, &paths);
        });
    }
}

#[test]
fn unrelated_memory_and_cancellation_never_mint_projection_recovery() {
    let measured = measured();
    let ceiling = measured.projection.checked_mul(2).unwrap();
    assert!(ceiling <= MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES);
    let pressure_bytes = ceiling
        - measured.before_projection
        - (measured.projection - measured.before_projection) / 2;
    assert!(measured.before_projection + pressure_bytes < ceiling);
    assert!(measured.projection + pressure_bytes > ceiling);
    with_ceiling(ceiling, || {
        let (_storage, root, paths) = fixture();
        let cancellation = Cancellation::new();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let pressure = RestoredMemoryReservation::new(
            Arc::clone(&service.generation_cache),
            pressure_bytes,
            true,
        )
        .unwrap();
        aggregate_source_recovery::FRAME_SEALS.with(|value| value.set(0));
        let (result, (_, own_peak)) =
            capture(|| service.index_repository(root.path(), &cancellation));
        assert!(
            own_peak <= ceiling,
            "the intrinsic candidate fits independently"
        );
        assert!(
            matches!(result,
            Err(FirstSliceError::GenerationMemoryLimit { observed, limit, .. })
                if observed > ceiling && limit == ceiling),
            "{result:?}"
        );
        assert_eq!(aggregate_source_recovery::FRAME_SEALS.with(Cell::get), 0);
        drop(pressure);
        assert_no_staging_or_reservations(&service, &paths);
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            service.index_repository(root.path(), &cancelled),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        assert_eq!(aggregate_source_recovery::FRAME_SEALS.with(Cell::get), 0);
        assert_no_staging_or_reservations(&service, &paths);
    });
}

#[test]
fn unfit_source_only_admission_is_still_a_real_typed_failure() {
    with_ceiling(GENERATION_MEMORY_FIXED_OVERHEAD_BYTES - 1, || {
        let (_storage, root, paths) = fixture();
        let cancellation = Cancellation::new();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let result = service.index_repository(root.path(), &cancellation);
        assert!(
            matches!(result, Err(FirstSliceError::GenerationMemoryLimit { observed, limit, .. })
            if observed > limit && limit == GENERATION_MEMORY_FIXED_OVERHEAD_BYTES - 1),
            "{result:?}"
        );
        assert_no_staging_or_reservations(&service, &paths);
        assert!(
            service
                .generation_cache
                .lock()
                .unwrap()
                .logical_charge_bytes_by_generation
                .is_empty()
        );
    });
}

// Additional controls use ordinary native source, without changing the cap in flight.
thread_local! {
    static DOCUMENT_FOOTPRINT: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
}

pub(super) fn observe_document_footprint(before: u64, bytes: u64) {
    if CAPTURE.with(Cell::get) {
        DOCUMENT_FOOTPRINT.with(|value| value.set(Some((before, bytes))));
        eprintln!(
            "{}",
            serde_json::json!({
                "schema":"rootlight.fresh-document-footprint-test/1",
                "before":before,"bytes":bytes,"ceiling":ceiling(),
            })
        );
    }
}

type CapturedFootprints<T> = (T, Option<(u64, u64)>, Option<(u64, u64)>);

fn capture_details<T>(operation: impl FnOnce() -> T) -> CapturedFootprints<T> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CAPTURE.with(|value| value.set(false));
        }
    }
    assert!(!CAPTURE.with(|value| value.replace(true)));
    DOCUMENT_FOOTPRINT.with(|value| value.set(None));
    PROJECTION.with(|value| value.set(None));
    let _reset = Reset;
    let result = operation();
    (
        result,
        DOCUMENT_FOOTPRINT.with(Cell::take),
        PROJECTION.with(Cell::take),
    )
}

#[derive(Clone)]
struct SourceMember {
    path: String,
    bytes: Vec<u8>,
    language: &'static str,
    generated: bool,
}

fn write_members(members: &[SourceMember]) -> (TempDir, TempDir, RuntimePaths) {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for member in members {
        fs::write(root.path().join(&member.path), &member.bytes).unwrap();
    }
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    (storage, root, paths)
}

fn mixed_members() -> Vec<SourceMember> {
    // Bounded sibling fanout avoids the known unrelated wide-array parent cost.
    let mut layer = vec!["0".to_owned(); 4096];
    while layer.len() > 1 {
        layer = layer
            .chunks(32)
            .map(|values| format!("[{}]", values.join(",")))
            .collect();
    }
    let json = format!("{{\"cells\":{}}}\n", layer[0]).into_bytes();
    let mut members: Vec<_> = (0..8)
        .map(|file| SourceMember {
            path: format!("values_{file:02}.json"),
            bytes: json.clone(),
            language: "json",
            generated: false,
        })
        .collect();
    members.extend([
        SourceMember {
            path: "healthy.rs".to_owned(),
            bytes: b"pub fn healthy() -> u32 { 1 }\n".to_vec(),
            language: "rust",
            generated: false,
        },
        SourceMember {
            path: "generated.rs".to_owned(),
            bytes: b"// Code generated by fixture. DO NOT EDIT.\npub fn produced() -> u32 { 2 }\n"
                .to_vec(),
            language: "rust",
            generated: true,
        },
        SourceMember {
            path: "Makefile".to_owned(),
            bytes: b"all:\n\t@echo ok\n".to_vec(),
            language: "make",
            generated: false,
        },
        SourceMember {
            path: "measure.erl".to_owned(),
            bytes: b"-module(measure).\n-export([run/1]).\nrun(Value) -> Value.\n".to_vec(),
            language: "erlang",
            generated: false,
        },
    ]);
    // Reuse the real native density already calibrated by fixture()/measured().
    members.extend((0..64).map(|file| SourceMember {
        path: format!("member_{file:02}.rs"),
        bytes: authored(file),
        language: "rust",
        generated: false,
    }));
    members
}

fn assert_members(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    members: &[SourceMember],
    unknown: bool,
) {
    let cancellation = Cancellation::new();
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    let snapshot = lease.generation();
    assert_eq!(snapshot.file_count(), members.len());
    let mut count = 0;
    for file in snapshot.document().files.iter().chain(
        snapshot
            .source_files()
            .entries()
            .iter()
            .map(|entry| entry.file()),
    ) {
        let member = members
            .iter()
            .find(|member| member.path == file.path)
            .expect("no invented or omitted source member");
        assert_eq!(file.language, member.language);
        assert_eq!(
            file.generated, member.generated,
            "generated classification is conserved"
        );
        assert_eq!(file.encoding, "utf-8");
        assert_eq!(file.content_hash, content_hash(&member.bytes));
        assert_eq!(file.byte_length, member.bytes.len() as u64);
        let source = file.evidence.source.clone().unwrap();
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.content_hash(), content_hash(&member.bytes));
        assert_eq!(
            (source.span().start_byte(), source.span().end_byte()),
            (0, member.bytes.len() as u64)
        );
        // Source reads have their own caller budget, independent of generation
        // admission. Verify every physical byte through bounded requests.
        let mut offset = 0;
        loop {
            let end = (offset + 8 * 1024).min(member.bytes.len());
            let page = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(file.id, offset as u64, end as u64).unwrap(),
                source.content_hash(),
                None,
            );
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![page.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &cancellation,
                )
                .unwrap();
            assert_eq!(read.data.chunks.len(), 1);
            assert_eq!(read.data.chunks[0].reference, page);
            assert_eq!(read.data.chunks[0].bytes, member.bytes[offset..end]);
            if end == member.bytes.len() {
                break;
            }
            offset = end;
        }
        if unknown {
            let coverage = service
                .source_file_coverage_until(receipt.generation, file.id, &cancellation)
                .unwrap();
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::ResourceBounded)
            );
        }
        count += 1;
    }
    assert_eq!(count, members.len());
    if unknown {
        assert!(snapshot.document().entities.is_empty());
        assert!(snapshot.document().occurrences.is_empty());
        assert!(snapshot.document().relations.is_empty());
        assert!(
            snapshot
                .document()
                .diagnostics
                .iter()
                .any(
                    |diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE
                        && diagnostic.message.contains("prevented graph verification")
                )
        );
    }
}

fn source_only_bounds(members: &[SourceMember]) -> (u64, u64) {
    let (_storage, root, paths) = write_members(members);
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let defaults = GenerationBudget::default();
    service.generation_budget = GenerationBudget::new(
        defaults.limit(rootlight_storage::GenerationResource::Rows),
        defaults.limit(rootlight_storage::GenerationResource::SourceReferences),
        64 * 1024,
    )
    .unwrap();
    let prepared = service
        .prepare_repository(root.path(), &cancellation)
        .unwrap();
    let FirstSliceIndexPreparation::Pending(pending) = &prepared else {
        panic!("fresh source calibration must prepare");
    };
    assert!(pending.verified.document().entities.is_empty());
    assert!(pending.verified.document().diagnostics.iter().any(|diagnostic|diagnostic.code==rootlight_ir::AGGREGATE_SOURCE_RECOVERY_DIAGNOSTIC));
    (
        pending.reserved_memory_bytes,
        pending.memory_bytes.checked_mul(3).unwrap(),
    )
}

fn run_preservation_lifecycle(members: &[SourceMember], early: bool) {
    let cancellation = Cancellation::new();
    let (_baseline_storage, baseline_root, baseline_paths) = write_members(members);
    let mut baseline =
        FirstSliceService::new_durable(2, baseline_paths.state_dir(), &cancellation).unwrap();
    let (receipt, document, peak) = capture_details(|| {
        baseline
            .index_repository(baseline_root.path(), &cancellation)
            .unwrap()
    });
    assert_members(&baseline, &receipt, members, false);
    {
        let lease = baseline
            .generation_lease(receipt.generation, &cancellation)
            .unwrap();
        let graph = lease.generation().document();
        if early {
            let calls: Vec<_> = graph
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == rootlight_ir::OccurrenceRole::CallSite
                        && site.syntactic_text_hash == content_hash(b"selectTarget")
                })
                .collect();
            assert_eq!(calls.len(), 1024, "real cross-file expansion prerequisite");
            assert!(calls.iter().all(|site|matches!(&site.target,
                rootlight_ir::OccurrenceTarget::Candidates{symbols,total_count:128,completeness:CoverageStatus::Complete} if symbols.len()==128)),
                "the final-document fixture must actually expand128 native candidates");
        } else {
            for (language, kind, name) in [
                ("rust", EntityKind::Function, "healthy"),
                ("make", EntityKind::BuildTarget, "all"),
                ("erlang", EntityKind::Function, "run/1"),
            ] {
                assert!(
                    graph
                        .entities
                        .iter()
                        .any(|entity| entity.language == language
                            && entity.kind == kind
                            && entity.canonical_name == name
                            && (language != "erlang" || entity.qualified_name == "measure::run/1")),
                    "healthy native {language} {name}; actual first8={:?}",
                    graph
                        .entities
                        .iter()
                        .filter(|entity| entity.language == language)
                        .take(8)
                        .map(|entity| (
                            entity.kind,
                            entity.canonical_name.as_str(),
                            entity.qualified_name.as_str()
                        ))
                        .collect::<Vec<_>>()
                );
            }
            let member = members
                .iter()
                .find(|member| member.path == "measure.erl")
                .unwrap();
            let file = graph
                .files
                .iter()
                .find(|file| file.path == member.path)
                .unwrap();
            assert_eq!(file.language, "erlang");
            assert_eq!(file.content_hash, content_hash(&member.bytes));
            assert_eq!(file.byte_length, u64::try_from(member.bytes.len()).unwrap());
            let functions: Vec<_> = graph
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == "erlang"
                        && entity.kind == EntityKind::Function
                        && entity.canonical_name == "run/1"
                        && entity.qualified_name == "measure::run/1"
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
                .collect();
            assert_eq!(functions.len(), 1, "one native measure.erl function");
            let function = functions[0];
            let module = graph
                .entities
                .iter()
                .find(|entity| {
                    entity.language == "erlang"
                        && entity.kind == EntityKind::Module
                        && entity.canonical_name == "measure"
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
                .expect("native module owns the written function");
            assert_eq!(
                function.container,
                Some(rootlight_ir::ContainerRef::Entity(module.id))
            );
            let definitions: Vec<_> = graph
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == rootlight_ir::OccurrenceRole::Definition
                        && site.target
                            == rootlight_ir::OccurrenceTarget::Resolved {
                                symbol: function.id,
                            }
                        && site.file == file.id
                })
                .collect();
            assert_eq!(
                definitions.len(),
                1,
                "one resolved native function Definition"
            );
            let definition = definitions[0];
            assert_eq!(definition.syntax_kind, "erlang.function.definition");
            assert_eq!(definition.source.repository(), receipt.repository);
            assert_eq!(definition.source.generation(), receipt.generation);
            assert_eq!(definition.source.content_hash(), file.content_hash);
            assert_eq!(definition.source.span().file(), file.id);
            assert_eq!(definition.source.span().start_byte(), 36);
            assert_eq!(definition.source.span().end_byte(), 46);
            assert_eq!(member.bytes.get(36..46), Some(b"run(Value)".as_slice()));
            assert_eq!(definition.syntactic_text_hash, content_hash(b"run(Value)"));
        }
    }
    let (before_document, document_bytes) =
        document.expect("actual document-footprint stage reached");
    let (before_peak, peak_bytes) = peak.expect("actual full projection stage reached");
    drop(baseline);
    let (source_peak, cold_reload) = source_only_bounds(members);
    let (before, own) = if early {
        (before_document, document_bytes)
    } else {
        (before_peak, peak_bytes)
    };
    let lower = before.max(source_peak).max(cold_reload);
    assert!(
        lower < own,
        "no actual preservation interval: early={early} before_document={before_document} document={document_bytes} before_peak={before_peak} peak={peak_bytes} source_peak={source_peak} cold_reload={cold_reload}"
    );
    let limit = lower + (own - lower) / 2;
    assert!(lower <= limit && limit < own && own < MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES);
    eprintln!(
        "{}",
        serde_json::json!({"schema":"rootlight.fresh-source-route-calibration/1","early":early,"before_document":before_document,"document":document_bytes,"before_peak":before_peak,"peak":peak_bytes,"source_peak":source_peak,"cold_reload":cold_reload,"ceiling":limit})
    );
    with_ceiling(limit, || {
        let (_storage, root, paths) = write_members(members);
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let (first, actual_document, actual_peak) =
            capture_details(|| service.index_repository(root.path(), &cancellation));
        if let Err(error) = &first {
            let observed = if early {
                actual_document.expect("actual early boundary reached").1
            } else {
                actual_peak.expect("actual final peak reached").1
            };
            assert!(observed > limit);
            assert!(
                matches!(error,FirstSliceError::GenerationMemoryLimit{observed:actual,limit:ceiling,..}
                if *actual==observed && *ceiling==limit),
                "unrelated failure is not a capacity RED: {error:?}"
            );
        }
        let first =
            first.expect("actual own projection capacity must preserve every authenticated source");
        let actual_document = actual_document.expect("refusal reached the exact document boundary");
        if early {
            assert!(actual_document.1 > limit);
            assert!(actual_peak.is_none());
        } else {
            assert!(actual_document.1 <= limit);
            assert!(actual_peak.unwrap().1 > limit);
        }
        assert_members(&service, &first, members, true);
        {
            let lease = service
                .generation_lease(first.generation, &cancellation)
                .unwrap();
            let expected = if early {
                "document and lexical coverage"
            } else {
                "full projection peak"
            };
            assert!(
                lease
                    .generation()
                    .document()
                    .diagnostics
                    .iter()
                    .any(
                        |diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE
                            && diagnostic.message.contains(expected)
                    )
            );
        }
        assert_eq!(
            service
                .index_repository(root.path(), &cancellation)
                .unwrap(),
            first
        );
        let mut changed_members = members.to_vec();
        let changed = changed_members
            .iter_mut()
            .find(|member| {
                if early {
                    member.path == "caller.ts"
                } else {
                    member.language == "rust" && !member.generated
                }
            })
            .unwrap();
        changed.bytes.extend_from_slice(b"// changed source\n");
        fs::write(root.path().join(&changed.path), &changed.bytes).unwrap();
        let second = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        assert_eq!(second.parent, Some(first.generation));
        assert_members(&service, &second, &changed_members, true);
        assert_no_staging_or_reservations(&service, &paths);
        drop(service);
        let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        assert_eq!(
            reopened.active_generation_for(first.repository),
            Some(second.generation)
        );
        assert_members(&reopened, &first, members, true);
        assert_members(&reopened, &second, &changed_members, true);
        assert_no_staging_or_reservations(&reopened, &paths);
        drop(reopened);
        // The same own refusal in process-local mode cannot acquire a durable source frame.
        let (_plain_storage, plain_root, _plain_paths) = write_members(members);
        let mut plain = FirstSliceService::new(2).unwrap();
        aggregate_source_recovery::FRAME_SEALS.with(|value| value.set(0));
        let (result, _, plain_peak) =
            capture_details(|| plain.index_repository(plain_root.path(), &cancellation));
        assert!(
            matches!(result,Err(FirstSliceError::GenerationMemoryLimit{observed,limit:actual,..})if observed>limit && actual==limit),
            "process-local route must keep actual refusal: {result:?}"
        );
        if !early {
            assert!(plain_peak.unwrap().1 > limit);
        }
        assert_eq!(aggregate_source_recovery::FRAME_SEALS.with(Cell::get), 0);
    });
}

#[test]
fn generated_physical_make_and_erlang_sources_survive_fresh_capacity_lifecycle() {
    run_preservation_lifecycle(&mixed_members(), false);
}

#[test]
fn earlier_document_footprint_refusal_preserves_actual_expanded_native_sources() {
    let mut members: Vec<_> = (0..128)
        .map(|target| SourceMember {
            path: format!("target_{target:03}.ts"),
            bytes: b"function selectTarget() {}\n".to_vec(),
            language: "typescript",
            generated: false,
        })
        .collect();
    let caller = format!(
        "function invoke() {{ /*{}*/ {} }}\n",
        "physical source trivia ".repeat(1024),
        "selectTarget();".repeat(1024)
    );
    members.push(SourceMember {
        path: "caller.ts".to_owned(),
        bytes: caller.into_bytes(),
        language: "typescript",
        generated: false,
    });
    run_preservation_lifecycle(&members, true);
}

struct SourcePolicyFixture {
    _root: TempDir,
    inputs: Vec<ManifestInput>,
    claims: Vec<FileIdentityClaim>,
    snapshots: BTreeMap<FileId, SourceSnapshot>,
    sources: Vec<RustSourceInput>,
    languages: BTreeMap<FileId, String>,
}

fn source_policy_fixture() -> SourcePolicyFixture {
    let root = tempfile::tempdir().unwrap();
    let repository = rootlight_ids::derive_repository(b"fresh-native-source-policy").id();
    let mut inputs = Vec::new();
    let mut snapshots = BTreeMap::new();
    let mut sources = Vec::new();
    let mut languages = BTreeMap::new();
    for member in mixed_members()
        .into_iter()
        .filter(|member| member.generated || matches!(member.language, "make" | "erlang"))
    {
        fs::write(root.path().join(&member.path), &member.bytes).unwrap();
        let owner = RepositoryRoot::open(repository, root.path()).unwrap();
        let path = RelativePath::parse(Path::new(&member.path)).unwrap();
        let snapshot = owner.snapshot(&path, 1024 * 1024).unwrap();
        inputs.push(ManifestInput {
            file: snapshot.file(),
            path: member.path,
            content_hash: snapshot.content_hash(),
            bytes: member.bytes.len() as u64,
            encoding: "utf-8".to_owned(),
            class: if member.generated {
                InputClass::Generated
            } else {
                InputClass::Source
            },
            language_signals: Vec::new(),
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        });
        languages.insert(snapshot.file(), member.language.to_owned());
        snapshots.insert(snapshot.file(), snapshot.clone());
        sources.push(RustSourceInput {
            snapshot,
            encoding: "utf-8".to_owned(),
            generated: member.generated,
            origins: Vec::new(),
        });
    }
    let claims = source_file_identity_claims(&inputs, repository, &Cancellation::new()).unwrap();
    SourcePolicyFixture {
        _root: root,
        inputs,
        claims,
        snapshots,
        sources,
        languages,
    }
}

fn policy_eligible(
    service: &FirstSliceService,
    fixture: &SourcePolicyFixture,
    cancel: &Cancellation,
) -> Result<bool, FirstSliceError> {
    fresh_projection_capacity::eligible(
        service,
        FirstSliceIndexMode::Structural,
        fresh_projection_capacity::Context {
            inputs: &fixture.inputs,
            claims: &fixture.claims,
            derived: &container_sources::PreparedSources::default(),
            languages: &fixture.languages,
            sources: &fixture.sources,
        },
        cancel,
    )
}

#[test]
fn generated_classification_and_each_registered_custom_frontend_have_source_admission() {
    let service = FirstSliceService::new(2).unwrap();
    for language in ["rust", "make", "erlang"] {
        let mut fixture = source_policy_fixture();
        fixture
            .inputs
            .retain(|input| fixture.languages[&input.file] == language);
        fixture
            .claims
            .retain(|claim| fixture.languages[&claim.file] == language);
        fixture
            .sources
            .retain(|source| fixture.languages[&source.snapshot.file()] == language);
        fixture
            .languages
            .retain(|_, value| value.as_str() == language);
        assert!(
            policy_eligible(&service, &fixture, &Cancellation::new()).unwrap(),
            "registered source frontend {language}"
        );
        if language != "rust" {
            assert!(
                !service.structural_parsers[language].supports_retained_identity_preflight(),
                "identity-preflight capability must remain distinct"
            );
        }
    }
}

#[test]
fn true_origins_foreign_parser_stale_source_and_cancellation_remain_strict() {
    let mut service = FirstSliceService::new(2).unwrap();
    let mut fixture = source_policy_fixture();
    let other = FirstSliceService::new(2).unwrap();
    let own = service
        .structural_parsers
        .insert(
            "rust".to_owned(),
            Arc::clone(&other.structural_parsers["rust"]),
        )
        .unwrap();
    assert!(!policy_eligible(&service, &fixture, &Cancellation::new()).unwrap());
    service.structural_parsers.insert("rust".to_owned(), own);
    assert!(policy_eligible(&service, &fixture, &Cancellation::new()).unwrap());
    let generated = fixture
        .sources
        .iter_mut()
        .find(|source| source.generated)
        .unwrap();
    let origin_path = RelativePath::parse(Path::new("origin.rs")).unwrap();
    let origin = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
        repository: fixture.claims[0].repository,
        path_identity: origin_path.identity_bytes(),
    })
    .id();
    generated.origins.push(GeneratedOriginMapping::new(
        SourceSpan::new(generated.snapshot.file(), 0, 1).unwrap(),
        origin_path,
        SourceSpan::new(origin, 0, 1).unwrap(),
        TransformationId::new("preserved-origin").unwrap(),
        None,
    ));
    assert!(!policy_eligible(&service, &fixture, &Cancellation::new()).unwrap());
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        policy_eligible(&service, &fixture, &cancel),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    for mutation in 0..2 {
        let mut claims = fixture.claims.clone();
        if mutation == 0 {
            claims[0].content_hash = content_hash(b"stale");
        } else {
            claims[0].byte_length += 1;
        }
        assert!(matches!(
            aggregate_source_recovery::PreparedSourceFrame::seal(
                &fixture.inputs,
                &container_sources::PreparedSources::default(),
                claims,
                fixture.snapshots.clone(),
                None,
                &Cancellation::new()
            ),
            Err(FirstSliceError::Identity)
        ));
    }
}
