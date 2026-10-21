//! Source-only reopen stays exact when a complete semantic restore cannot fit.
//! Private fixture storage and logical competing reservations exercise the real
//! durable boundary without allocating a large repository or relaxing limits.

use super::*;
use crate::FirstSliceService;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::OracleReadAdmissionError;
use rootlight_query::LocateMode;
use rootlight_runtime::RuntimePaths;
use std::fs;
use tempfile::TempDir;

fn durable_test_tempdir() -> TempDir {
    tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
}

#[test]
fn source_authority_manifest_versions_and_required_descriptor_are_closed() {
    let (_storage, _fixture, paths, receipt, _, _) = source_only_reopen_fixture();
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let encoded = fs::read(directory.join(MANIFEST_FILENAME)).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(original["version"], 5);
    for version in 1..=5 {
        for descriptor_present in [false, true] {
            let mut value = original.clone();
            value["version"] = version.into();
            if !descriptor_present {
                value.as_object_mut().unwrap().remove("source_authority");
            }
            let manifest: DurableGenerationManifest = serde_json::from_value(value).unwrap();
            // Older source-storage shapes have their own unchanged constraints;
            // descriptor presence must never make a legacy version acceptable.
            if version == 5 {
                assert_eq!(
                    source_authority::manifest_source_layout(&manifest).is_ok(),
                    descriptor_present
                );
            } else if descriptor_present {
                assert!(source_authority::manifest_source_layout(&manifest).is_err());
            }
        }
    }
    for version in [0, 6, u16::MAX] {
        let mut value = original.clone();
        value["version"] = version.into();
        let manifest: DurableGenerationManifest = serde_json::from_value(value).unwrap();
        assert!(source_authority::manifest_source_layout(&manifest).is_err());
    }
    for bytes in [0, MAX_SOURCE_FILE_CATALOG_BYTES + 1] {
        let mut value = original.clone();
        value["source_authority"]["bytes"] = bytes.into();
        let manifest: DurableGenerationManifest = serde_json::from_value(value).unwrap();
        assert!(source_authority::manifest_source_layout(&manifest).is_err());
    }
}

#[test]
fn source_authority_corruption_never_falls_back_to_unverified_source() {
    let (_storage, _fixture, paths, receipt, reference, _) = source_only_reopen_fixture();
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let path = directory.join("source-authority.json");
    let original = fs::read(&path).unwrap();
    let cancellation = Cancellation::new();
    let (service, _deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    fs::write(&path, corrupt).unwrap();
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            reference.span().file(),
            "aba",
            None,
            rootlight_source::SourceFindBudget::new(),
            &cancellation
        ),
        Err(crate::FirstSliceSourceFindError::Service(
            crate::FirstSliceError::CatalogCorrupt
        ))
    ));
    assert!(
        !service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .is_loaded(receipt.generation)
    );
    fs::write(&path, original).unwrap();
    assert_eq!(
        service
            .find_literal(
                receipt.generation,
                reference.span().file(),
                "aba",
                None,
                rootlight_source::SourceFindBudget::new(),
                &cancellation
            )
            .unwrap()
            .matches
            .len(),
        2
    );
}

#[test]
fn legacy_v4_keeps_full_restore_and_source_bytes_remain_hash_verified() {
    use crate::FirstSliceSourceFindError;
    use rootlight_source::{SourceError, SourceFindBudget, SourceFindError};
    let (_storage, _fixture, paths, receipt, reference, _) = source_only_reopen_fixture();
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let manifest_path = directory.join(MANIFEST_FILENAME);
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    value["version"] = RAW_CATALOG_GENERATION_MANIFEST_VERSION.into();
    value.as_object_mut().unwrap().remove("source_authority");
    fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
    let cancellation = Cancellation::new();
    let (mut service, deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    assert!(
        service
            .source_only_lease(receipt.generation, &cancellation)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        service.resolve_source_generation(
            receipt.repository,
            Some(receipt.generation),
            &cancellation
        ),
        Err(crate::FirstSliceError::GenerationNotFound)
    ));
    service
        .restore_deferred_for_queries(&deferred, &cancellation)
        .unwrap();
    assert_eq!(
        service
            .find_literal(
                receipt.generation,
                reference.span().file(),
                "aba",
                None,
                SourceFindBudget::new(),
                &cancellation
            )
            .unwrap()
            .matches
            .len(),
        2
    );
    drop(service);
    // Restore the actual v5 descriptor, then corrupt its real source payload.
    value["version"] = GENERATION_MANIFEST_VERSION.into();
    let authority_bytes = fs::read(directory.join("source-authority.json")).unwrap();
    value["source_authority"] = serde_json::to_value(DurableSidecarDescriptor {
        bytes: authority_bytes.len() as u64,
        digest: content_hash_bytes(&authority_bytes),
    })
    .unwrap();
    fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
    let manifest: DurableGenerationManifest = serde_json::from_value(value).unwrap();
    let payload = match source_authority::manifest_source_layout(&manifest).unwrap() {
        DurableSourceLayout::Inline => directory
            .join(SOURCES_DIRECTORY)
            .join(reference.span().file().to_string()),
        DurableSourceLayout::Packed(_) => {
            directory.join(SOURCES_DIRECTORY).join(source_pack_name(0))
        }
        DurableSourceLayout::Blobs => directory
            .parent()
            .unwrap()
            .join(SOURCE_BLOBS_DIRECTORY)
            .join(reference.content_hash().to_string())
            .join(SOURCE_BLOB_PAYLOAD_FILENAME),
    };
    let mut bytes = fs::read(&payload).unwrap();
    bytes[0] ^= 1;
    fs::write(&payload, bytes).unwrap();
    let (service, _deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            reference.span().file(),
            "aba",
            None,
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Source(
            SourceError::StaleSource
        )))
    ));
    assert!(
        service
            .loaded_generation_snapshot(receipt.generation)
            .is_err()
    );
}

#[test]
fn source_context_distinguishes_absent_repositories_from_missing_generations() {
    let (_storage, _fixture, paths, receipt, _, _) = source_only_reopen_fixture();
    let cancellation = Cancellation::new();
    let memory = FirstSliceService::new(2).unwrap();
    let (cold, _restore) = FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let absent = rootlight_ids::RepositoryId::from_bytes([238; 16]);
    assert_ne!(absent, receipt.repository);
    for service in [&memory, &cold] {
        for selected in [None, Some(receipt.generation)] {
            assert!(matches!(
                service.resolve_source_context(absent, selected, &cancellation),
                Err(crate::FirstSliceError::RepositoryNotFound)
            ));
        }
    }
    assert_eq!(
        cold.resolve_source_context(receipt.repository, None, &cancellation)
            .unwrap()
            .generation,
        receipt.generation
    );
    let missing = GenerationId::from_bytes([239; 20]);
    assert_ne!(missing, receipt.generation);
    assert!(matches!(
        cold.resolve_source_context(receipt.repository, Some(missing), &cancellation),
        Err(crate::FirstSliceError::GenerationNotFound)
    ));
}

#[test]
fn source_publication_context_retains_superseded_parent_and_exact_language() {
    let (_storage, fixture, paths, first, reference, _) = source_only_reopen_fixture();
    let cancellation = Cancellation::new();
    let mut writer = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    fs::write(fixture.path().join("lib.rs"), "pub fn replacement() {}\n").unwrap();
    let second = writer
        .index_rust_fixture(fixture.path(), &cancellation)
        .unwrap();
    drop(writer);
    let (service, _deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let context = service
        .resolve_source_context(first.repository, Some(first.generation), &cancellation)
        .unwrap();
    assert!(!context.active);
    assert_eq!(context.active_generation, second.generation);
    assert_eq!(context.active_parent, second.parent);
    assert_eq!(context.parent, first.parent);
    assert_eq!(
        service
            .source_generation_freshness(first.repository, first.generation, &cancellation)
            .unwrap()
            .structural,
        crate::FirstSliceObservedFreshness::Superseded
    );
    assert_eq!(
        service
            .source_file_language_until(first.generation, reference.span().file(), &cancellation)
            .unwrap(),
        "rust"
    );
    assert_eq!(
        service
            .resolve_source_generation(first.repository, None, &cancellation)
            .unwrap(),
        second.generation
    );
    assert!(
        !service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .is_loaded(first.generation)
    );
    assert!(
        !service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .is_loaded(second.generation)
    );
}

#[test]
fn degraded_source_authority_retains_aggregate_cause_origins_and_vba_fallback() {
    use crate::{AnalysisLimits, FirstSliceBudget};
    use rootlight_source::{SourceEncoding, SourceFindBudget, SourceReadOptions};
    let storage = durable_test_tempdir();
    let fixture = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let first = (0..64)
        .map(|index| format!("pub fn alpha_{index}() {{}}\n"))
        .collect::<String>();
    let second = (0..64)
        .map(|index| format!("pub fn beta_{index}() {{}}\n"))
        .collect::<String>();
    let module = b"Attribute VB_Name = \"Module\"\r\nPublic Sub Recover()\r\nEnd Sub\r\n";
    let book = crate::office_fixture::book(&[("Module", module)]);
    fs::write(fixture.path().join("first.rs"), first.as_bytes()).unwrap();
    fs::write(fixture.path().join("second.rs"), second).unwrap();
    fs::write(fixture.path().join("book.xlsm"), &book).unwrap();
    let cancellation = Cancellation::new();
    let mut writer = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let current = &writer.analysis_limits;
    let mut limits = current.ir().clone();
    limits.max_entities = 70;
    writer.analysis_limits = AnalysisLimits::new(
        current.max_source_bytes(),
        current.max_syntax_nodes(),
        current.max_syntax_depth(),
        current.max_embedded_ranges(),
        current.max_reported_memory_bytes(),
        current.syntax_stream().clone(),
        current.ir_stream().clone(),
        limits,
    )
    .unwrap();
    let receipt = writer
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(receipt.extracted_files, 1);
    let snapshot = writer
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let files = snapshot.document().files.iter().chain(
        snapshot
            .source_files()
            .entries()
            .iter()
            .map(|entry| entry.file()),
    );
    let references = files
        .map(|file| (file.path.clone(), file.evidence.source.clone().unwrap()))
        .collect::<Vec<_>>();
    let ordinary = references
        .iter()
        .find(|(path, _)| path == "first.rs")
        .unwrap()
        .1
        .clone();
    let derived = references
        .iter()
        .find(|(path, _)| path.contains("/@vba/"))
        .unwrap()
        .1
        .clone();
    let container = references
        .iter()
        .find(|(path, _)| path == "book.xlsm")
        .unwrap()
        .1
        .clone();
    let cause = writer
        .source_file_recovery_until(receipt.generation, ordinary.span().file(), &cancellation)
        .unwrap()
        .unwrap();
    drop(writer);
    let (mut service, deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let found = service
        .find_literal(
            receipt.generation,
            ordinary.span().file(),
            "alpha_63",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(
        service
            .source_file_recovery_until(receipt.generation, ordinary.span().file(), &cancellation)
            .unwrap(),
        Some(cause)
    );
    let found = service
        .find_literal(
            receipt.generation,
            derived.span().file(),
            "Recover",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.file.extracted_from.as_ref(), Some(&container));
    assert!(
        !service
            .source_only_read_available(
                receipt.repository,
                receipt.generation,
                std::slice::from_ref(&derived),
                &cancellation
            )
            .unwrap()
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![ordinary.clone(), container.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, first.as_bytes());
    assert_eq!(read.data.chunks[1].bytes, book);
    assert!(
        service
            .loaded_generation_snapshot(receipt.generation)
            .is_err()
    );
    service
        .restore_deferred_for_queries(&deferred, &cancellation)
        .unwrap();
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![derived],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, module);
    assert_eq!(
        read.data.chunks[0].extracted_from.as_ref(),
        Some(&container)
    );
}

pub(super) fn source_only_reopen_fixture() -> (
    TempDir,
    TempDir,
    RuntimePaths,
    super::super::FirstSliceIndexReceipt,
    SourceRef,
    Vec<u8>,
) {
    let storage = durable_test_tempdir();
    let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
        .expect("private runtime paths are valid");
    paths.prepare_owner().expect("private paths prepare");
    let fixture = durable_test_tempdir();
    let mut original = String::from("// ababa\n");
    for ordinal in 0..1024 {
        use std::fmt::Write as _;
        writeln!(original, "pub fn entry_{ordinal}() -> u32 {{ {ordinal} }}")
            .expect("fixture string writes");
    }
    fs::write(fixture.path().join("lib.rs"), &original).expect("fixture source writes");
    let (receipt, reference) = {
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("genuine semantic generation publishes");
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .expect("published generation is resident");
        let reference = snapshot.document().files[0]
            .evidence
            .source
            .clone()
            .expect("file carries its full immutable source reference");
        (receipt, reference)
    };
    (
        storage,
        fixture,
        paths,
        receipt,
        reference,
        original.into_bytes(),
    )
}

#[test]
fn deferred_literal_and_exact_reads_fit_without_semantic_graph_materialization() {
    use crate::{FirstSliceBudget, FirstSliceSourceFindError};
    use rootlight_source::{
        LiteralSearchError, SourceEncoding, SourceError, SourceFindBudget, SourceFindCursor,
        SourceFindError, SourceFindStop, SourceReadOptions,
    };

    let (_storage, fixture, paths, receipt, reference, original) = source_only_reopen_fixture();
    let cancellation = Cancellation::new();
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let manifest: DurableGenerationManifest = serde_json::from_slice(
        &fs::read(directory.join(MANIFEST_FILENAME)).expect("private manifest reads"),
    )
    .expect("private manifest decodes");
    assert!(manifest.receipt.oracle_allocated_bytes > 0);
    assert!(!directory.join(RECOVERY_MANIFEST_FILENAME).exists());
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let oracle = OracleReader::open_in(&directory, &context).expect("sealed oracle opens");
    let mut full_peak = None;
    let refusal = oracle.read_with_admission(&context, |plan| {
        let posting_bytes = u64::try_from(
            4 * std::mem::size_of::<(rootlight_ids::SymbolId, usize)>()
                + 2 * std::mem::size_of::<usize>(),
        )
        .unwrap();
        let text_copies = if manifest.source_file_catalog.is_some() {
            14
        } else {
            6
        };
        full_peak = Some(
            plan.materialization_bytes()
                .checked_add(plan.physical_rows().checked_mul(posting_bytes).unwrap())
                .and_then(|bytes| {
                    bytes.checked_add(plan.stats().text_bytes().checked_mul(text_copies)?)
                })
                .and_then(|bytes| bytes.checked_add(recovery_sidecar_memory_bytes(&manifest).ok()?))
                .expect("genuine full-restore plan is representable"),
        );
        Err::<(), ()>(())
    });
    assert!(matches!(
        refusal,
        Err(OracleReadAdmissionError::Admission(()))
    ));
    let available = full_peak.unwrap().checked_sub(1).unwrap();
    assert!(available < MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES);
    let (mut reopened, deferred) = FirstSliceService::open_durable_deferred(2, paths.state_dir())
        .expect("metadata-only durable open succeeds");
    let competing = super::super::RestoredMemoryReservation::new(
        Arc::clone(&reopened.generation_cache),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES - available,
        true,
    )
    .expect("competing logical work is admitted without allocating its bytes");
    assert!(matches!(
        deferred.restore_active_repository(receipt.repository, &cancellation),
        Err(FirstSliceError::GenerationMemoryLimit { .. })
    ));

    // Current source fails at this call because generation_lease requires the
    // semantic GenerationSet. The intended repair must authenticate source
    // authority independently, while the genuine full-restore refusal remains.
    let mut budget = SourceFindBudget::new();
    budget.matcher.max_scan_bytes = 1;
    budget.matcher.max_matches = 1;
    let mut cursor = None;
    let mut next_byte = 0;
    let mut matches = Vec::new();
    loop {
        let page = reopened
            .find_literal(
                receipt.generation,
                reference.span().file(),
                "aba",
                cursor,
                budget,
                &cancellation,
            )
            .expect("source-only work fits without materializing the semantic graph");
        assert_eq!(page.generation, receipt.generation);
        assert_eq!(page.file.id, reference.span().file());
        assert_eq!(page.file.content_hash, reference.content_hash());
        assert_eq!(page.scanned.start, next_byte);
        assert!(page.scanned.end > next_byte);
        next_byte = page.scanned.end;
        matches.extend(page.matches);
        cursor = page.continuation;
        if cursor.is_none() {
            assert_eq!(page.stop, SourceFindStop::EndOfSource);
            break;
        }
        if next_byte >= 8 {
            budget.matcher.max_scan_bytes = original.len();
        }
    }
    assert_eq!(next_byte, original.len() as u64);
    assert_eq!(
        matches
            .iter()
            .map(|r| (r.span().start_byte(), r.span().end_byte()))
            .collect::<Vec<_>>(),
        [(3, 6), (5, 8)]
    );
    for selected in &matches {
        let read = reopened
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![selected.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .expect("the same source lease supports ordinary exact SourceRead accounting");
        assert_eq!(read.data.generation, receipt.generation);
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, *selected);
        assert_eq!(read.data.chunks[0].bytes, b"aba");
    }
    let absent = reopened
        .find_literal(
            receipt.generation,
            reference.span().file(),
            "absent",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(absent.stop, SourceFindStop::EndOfSource);
    assert!(absent.matches.is_empty());
    assert!(absent.continuation.is_none());
    assert!(matches!(
        reopened.find_literal(
            receipt.generation,
            reference.span().file(),
            "aba",
            Some(SourceFindCursor {
                next_byte: original.len() + 1,
                matched_prefix: 0
            }),
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Literal(
            LiteralSearchError::InvalidCursor
        )))
    ));
    assert!(matches!(
        reopened.find_literal(
            receipt.generation,
            rootlight_ids::FileId::from_bytes([231; 20]),
            "aba",
            None,
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Source(
            SourceError::FileNotFound
        )))
    ));
    let stale = SourceRef::new(
        reference.repository(),
        reference.generation(),
        reference.span(),
        rootlight_ids::ContentHash::from_bytes([227; 32]),
        None,
    );
    assert!(matches!(
        reopened.source_read_with_options_and_budget(
            receipt.generation,
            vec![stale],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        ),
        Err(FirstSliceError::Source)
    ));
    assert!(
        !reopened
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .is_loaded(receipt.generation)
    );

    // Changing the live repository cannot change the published source view.
    fs::write(fixture.path().join("lib.rs"), b"different live bytes").unwrap();
    let whole = reopened
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(whole.data.chunks[0].bytes, original);
    assert_eq!(whole.data.chunks[0].reference, reference);
    drop(competing);
    reopened
        .install_complete_deferred_restore_progressively(&deferred, &cancellation)
        .expect("an explicit semantic request still performs full strict restoration");
    let located = reopened
        .code_locate(
            receipt.generation,
            "entry_0".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &cancellation,
        )
        .unwrap();
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.identifier == "entry_0")
    );
}

#[test]
fn source_only_reopen_preserves_cancellation_and_aggregate_memory_refusal() {
    use crate::FirstSliceSourceFindError;
    use rootlight_source::{LiteralSearchError, SourceFindBudget, SourceFindError};

    let (_storage, _fixture, paths, receipt, reference, _original) = source_only_reopen_fixture();
    let (service, _deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let cancellation = Cancellation::new();
    let pressure = super::super::RestoredMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
        true,
    )
    .expect("existing aggregate memory ceiling is reserved without materialization");
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            reference.span().file(),
            "aba",
            None,
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Service(
            FirstSliceError::GenerationMemoryLimit { .. }
        ))
    ));
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            reference.span().file(),
            "",
            None,
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Literal(
            LiteralSearchError::EmptyQuery
        )))
    ));
    let cancelled = Cancellation::with_deadline(std::time::Instant::now());
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            reference.span().file(),
            "aba",
            None,
            SourceFindBudget::new(),
            &cancelled
        ),
        Err(FirstSliceSourceFindError::Service(
            FirstSliceError::Cancelled(CancellationReason::DeadlineExceeded)
        ))
    ));
    drop(pressure);
    let retry = service
        .find_literal(
            receipt.generation,
            reference.span().file(),
            "aba",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .expect("a resource refusal neither quarantines nor invalidates source authority");
    assert_eq!(retry.matches.len(), 2);
}

#[test]
fn active_source_parent_must_match_verified_v5_recipe_for_cold_and_cached_contexts() {
    let (_storage, fixture, paths, first, reference, original_bytes) = source_only_reopen_fixture();
    let cancellation = Cancellation::new();
    let mut writer = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    fs::write(fixture.path().join("lib.rs"), "pub fn replacement() {}\n").unwrap();
    let second = writer
        .index_rust_fixture(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(second.parent, Some(first.generation));
    drop(writer);
    let (cold, _cold_restore) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let (cached, _cached_restore) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    assert_eq!(
        cached
            .resolve_source_context(first.repository, Some(first.generation), &cancellation)
            .unwrap()
            .active_parent,
        second.parent
    );
    assert_eq!(
        cached
            .resolve_source_context(first.repository, Some(second.generation), &cancellation)
            .unwrap()
            .parent,
        second.parent
    );
    let manifest_path = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(first.repository.to_string())
        .join(second.generation.to_string())
        .join(MANIFEST_FILENAME);
    let original = fs::read(&manifest_path).unwrap();
    let mut altered: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let foreign_parent = GenerationId::from_bytes([201; 20]);
    assert_ne!(Some(foreign_parent), second.parent);
    altered["receipt"]["parent"] = serde_json::to_value(foreign_parent).unwrap();
    fs::write(&manifest_path, serde_json::to_vec(&altered).unwrap()).unwrap();
    assert!(matches!(
        cold.resolve_source_context(first.repository, Some(second.generation), &cancellation),
        Err(crate::FirstSliceError::CatalogCorrupt)
    ));
    let observations = [
        cold.resolve_source_context(first.repository, Some(first.generation), &cancellation),
        cached.resolve_source_context(first.repository, Some(first.generation), &cancellation),
    ];
    for (kind, observed) in ["cold", "cached"].into_iter().zip(observations) {
        assert!(
            matches!(observed, Err(crate::FirstSliceError::CatalogCorrupt)),
            "{kind} returned metadata without an authenticated active-parent recipe: {observed:?}"
        );
    }
    fs::write(&manifest_path, original).unwrap();
    for service in [&cold, &cached] {
        let context = service
            .resolve_source_context(first.repository, Some(first.generation), &cancellation)
            .unwrap();
        assert_eq!(context.active_parent, second.parent);
        assert_eq!(context.parent, first.parent);
        assert!(!context.active);
        assert_eq!(
            service
                .find_literal(
                    first.generation,
                    reference.span().file(),
                    "aba",
                    None,
                    rootlight_source::SourceFindBudget::new(),
                    &cancellation
                )
                .unwrap()
                .matches
                .len(),
            2
        );
        let lease = service
            .source_only_lease(first.generation, &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(lease.authority.metadata().parent(), first.parent);
        let read = service
            .source_read_with_options_and_budget(
                first.generation,
                vec![reference.clone()],
                rootlight_source::SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(rootlight_source::SourceEncoding::Bytes),
                crate::FirstSliceBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, original_bytes);
        assert!(
            !service
                .generation_cache
                .lock()
                .unwrap()
                .generations
                .is_loaded(first.generation)
        );
        assert!(
            !service
                .generation_cache
                .lock()
                .unwrap()
                .generations
                .is_loaded(second.generation)
        );
    }
    // Active-first daemon recovery may already have installed only B. Its
    // verified publication metadata must remain sufficient while A stays cold.
    let (mut recovering, deferred) =
        FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
    let restored = deferred
        .restore_active_repository(first.repository, &cancellation)
        .unwrap();
    recovering
        .install_progressive_deferred_restore(restored, &cancellation)
        .unwrap();
    assert!(
        recovering
            .loaded_generation_snapshot(second.generation)
            .is_ok()
    );
    assert!(
        recovering
            .loaded_generation_snapshot(first.generation)
            .is_err()
    );
    let context = recovering
        .resolve_source_context(first.repository, Some(first.generation), &cancellation)
        .unwrap();
    assert_eq!(context.active_parent, second.parent);
    assert!(
        recovering
            .loaded_generation_snapshot(first.generation)
            .is_err()
    );
}
