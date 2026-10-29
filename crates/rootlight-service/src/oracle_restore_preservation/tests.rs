//! Real ordinary-source lifecycle and strict ownership controls for admission.
//! Lowered test restore ceilings expose the conservative sidecar reservation;
//! exact source retention is asserted separately from retained structural facts.

use std::{
    cell::{Cell, RefCell},
    fs,
};

use rootlight_ir::{OccurrenceTarget, derive_occurrence_record_id};
use rootlight_runtime::RuntimePaths;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceReadOptions};

use super::*;

thread_local! {
    static OBSERVED: RefCell<Vec<(FileId, u64)>> = const { RefCell::new(Vec::new()) };
    static UNDO_CAPTURED: Cell<u64> = const { Cell::new(0) };
    static UNDO_ADDITIONAL: Cell<u64> = const { Cell::new(0) };
    static UNDO_ADDITIONAL_LIMIT: Cell<Option<u64>> = const { Cell::new(None) };
}

pub(super) fn observe_captured_undo(bytes: u64) {
    UNDO_CAPTURED.with(|captured| captured.set(bytes));
}

pub(super) fn admit_undo_additional(bytes: u64) -> Result<(), FirstSliceError> {
    let observed = UNDO_ADDITIONAL
        .with(Cell::get)
        .checked_add(bytes)
        .ok_or(FirstSliceError::Limits)?;
    UNDO_ADDITIONAL.with(|additional| additional.set(observed));
    if let Some(limit) = UNDO_ADDITIONAL_LIMIT.with(Cell::get)
        && observed > limit
    {
        let mut error = generation_memory_limit(0, observed, PendingGenerationMemory::Staged);
        if let FirstSliceError::GenerationMemoryLimit { limit: actual, .. } = &mut error {
            *actual = limit;
        }
        return Err(error);
    }
    Ok(())
}

fn with_undo_additional_limit<T>(limit: u64, operation: impl FnOnce() -> T) -> T {
    struct Reset {
        limit: Option<u64>,
        captured: u64,
        additional: u64,
    }
    impl Drop for Reset {
        fn drop(&mut self) {
            UNDO_ADDITIONAL_LIMIT.with(|value| value.set(self.limit));
            UNDO_CAPTURED.with(|value| value.set(self.captured));
            UNDO_ADDITIONAL.with(|value| value.set(self.additional));
        }
    }
    let _reset = Reset {
        limit: UNDO_ADDITIONAL_LIMIT.with(|value| value.replace(Some(limit))),
        captured: UNDO_CAPTURED.with(|value| value.replace(0)),
        additional: UNDO_ADDITIONAL.with(|value| value.replace(0)),
    };
    operation()
}

pub(super) fn observe(file: FileId, bytes: u64) {
    OBSERVED.with(|values| values.borrow_mut().push((file, bytes)));
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, RuntimePaths) {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(
        root.path().join("a.cs"),
        "// written-lifecycle\nclass First { public int Keep() { return 1; } }\n",
    )
    .unwrap();
    let mut large = String::from("// written-lifecycle\nclass Large {\n");
    for ordinal in 0..512 {
        use std::fmt::Write as _;
        writeln!(
            large,
            "public int Member{ordinal}(int input) {{ return input + {ordinal}; }}"
        )
        .unwrap();
    }
    large.push_str("}\n");
    fs::write(root.path().join("b.cs"), large).unwrap();
    fs::write(
        root.path().join("z.cs"),
        "// written-lifecycle\nclass Last { public int Keep() { return 2; } }\n",
    )
    .unwrap();
    fs::write(
        root.path().join("notes.txt"),
        "written-lifecycle exact unsupported physical metadata\n",
    )
    .unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    (storage, root, paths)
}

fn assert_note_metadata(service: &FirstSliceService, generation: GenerationId) -> DiagnosticRecord {
    let snapshot = service.loaded_generation_snapshot(generation).unwrap();
    let document = snapshot.document();
    let note = document
        .files
        .iter()
        .find(|file| file.path == "notes.txt")
        .unwrap();
    let expected = unsupported_source_document(
        document.repository,
        generation,
        &UnsupportedSourceInput {
            claim: FileIdentityClaim {
                file: note.id,
                repository: document.repository,
                path: note.path.clone(),
                path_identity: RelativePath::parse(Path::new(&note.path))
                    .unwrap()
                    .identity_bytes()
                    .to_vec(),
                content_hash: note.content_hash,
                byte_length: note.byte_length,
            },
            language: note.language.clone(),
            encoding: note.encoding.clone(),
            generated: note.generated,
            disposition: UnsupportedSourceDisposition::Language,
        },
    )
    .unwrap();
    assert_eq!(note, &expected.files[0]);
    let diagnostic = document
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == "unsupported-language"
                && diagnostic.source.as_ref() == note.evidence.source.as_ref()
        })
        .unwrap();
    assert_eq!(diagnostic, &expected.diagnostics[0]);
    assert_eq!(
        document
            .provenance
            .iter()
            .find(|record| record.id == diagnostic.provenance),
        expected.provenance.first()
    );
    diagnostic.clone()
}

fn assert_sources(
    service: &FirstSliceService,
    generation: GenerationId,
    root: &Path,
) -> DiagnosticRecord {
    let cancellation = Cancellation::new();
    let snapshot = service.loaded_generation_snapshot(generation).unwrap();
    assert!(snapshot.source_files().is_empty());
    assert_eq!(snapshot.document().files.len(), 4);
    let preserved_note = assert_note_metadata(service, generation);
    for file in &snapshot.document().files {
        let expected = fs::read(root.join(&file.path)).unwrap();
        assert_eq!(file.content_hash, content_hash(&expected));
        assert_eq!(file.byte_length, u64::try_from(expected.len()).unwrap());
        let whole = file.evidence.source.clone().unwrap();
        let read = service
            .source_read_with_options_and_budget(
                generation,
                vec![whole.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].reference, whole);
        assert_eq!(read.data.chunks[0].bytes, expected);
        let found = service
            .find_literal(
                generation,
                file.id,
                "written-lifecycle",
                None,
                SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(found.matches.len(), 1);
    }
    preserved_note
}

fn assert_selective_facts(
    service: &FirstSliceService,
    generation: GenerationId,
    stage: &str,
    calibration_observed: u64,
    ceiling: u64,
) {
    let cancellation = Cancellation::new();
    let snapshot = service.loaded_generation_snapshot(generation).unwrap();
    let document = snapshot.document();
    let omitted = document
        .files
        .iter()
        .find(|file| file.path == "b.cs")
        .unwrap();
    let failure_context = || {
        let files: Vec<_> = document
            .files
            .iter()
            .map(|file| {
                let entities = document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                    })
                    .count();
                let occurrences = document
                    .occurrences
                    .iter()
                    .filter(|site| site.source.span().file() == file.id)
                    .count();
                let first_gap = document
                    .skipped_regions
                    .iter()
                    .find(|gap| gap.source.span().file() == file.id)
                    .map(|gap| gap.detail.as_str());
                let latest_preview = OBSERVED.with(|values| {
                    values
                        .borrow()
                        .iter()
                        .rev()
                        .find(|(id, _)| *id == file.id)
                        .map(|(_, bytes)| *bytes)
                });
                (
                    file.path.as_str(),
                    file.id,
                    entities,
                    occurrences,
                    first_gap,
                    latest_preview,
                )
            })
            .collect();
        format!(
            "stage={stage} generation={generation} calibration_observed={calibration_observed} ceiling={ceiling} files=(path,id,entities,occurrences,first_gap,latest_recorded_file_preview){files:?}"
        )
    };
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "First"),
        "{}",
        failure_context()
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Last"),
        "{}",
        failure_context()
    );
    assert!(!document.entities.iter().any(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == omitted.id)
    }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.source.span().file() == omitted.id)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().file() == omitted.id
                && gap.detail.starts_with("durable-restore-capacity: "))
    );
    assert!(
        !document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
    );
    let coverage = service
        .source_file_coverage_until(generation, omitted.id, &cancellation)
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::ResourceBounded)
    );
    if let Some(cache) = service.structural_artifacts.generation(generation) {
        assert!(cache.get(omitted.id).is_none());
    }
}

fn assert_current_restore_quotes(
    service: &FirstSliceService,
    generation: GenerationId,
    stage: &str,
    ceiling: u64,
) {
    let snapshot = service.loaded_generation_snapshot(generation).unwrap();
    for (path, admitted) in [("a.cs", true), ("b.cs", false), ("z.cs", true)] {
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
            .id;
        OBSERVED.with(|values| {
            let values = values.borrow();
            let mut quotes = values.iter().filter(|(id, _)| *id == file);
            let observed = quotes
                .next()
                .unwrap_or_else(|| panic!("stage={stage} path={path} has no current restore quote"))
                .1;
            assert!(
                quotes.next().is_none(),
                "stage={stage} path={path} has duplicate current restore quotes"
            );
            assert_eq!(
                observed <= ceiling,
                admitted,
                "stage={stage} path={path} observed={observed} ceiling={ceiling}"
            );
        });
    }
}

#[test]
fn ordinary_restore_limit_preserves_prefix_later_files_metadata_and_exact_source_through_lifecycle()
{
    let (_calibration_storage, calibration_root, calibration_paths) = fixture();
    let cancellation = Cancellation::new();
    let mut calibration =
        FirstSliceService::new_durable(4, calibration_paths.state_dir(), &cancellation).unwrap();
    OBSERVED.with(|values| values.borrow_mut().clear());
    let full = calibration
        .index_repository(calibration_root.path(), &cancellation)
        .unwrap();
    let snapshot = calibration
        .loaded_generation_snapshot(full.generation)
        .unwrap();
    let large = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "b.cs")
        .unwrap()
        .id;
    let observed = OBSERVED.with(|values| {
        values
            .borrow()
            .iter()
            .find(|(file, _)| *file == large)
            .unwrap()
            .1
    });
    let prefix = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "a.cs")
        .unwrap()
        .id;
    let prefix_observed = OBSERVED.with(|values| {
        values
            .borrow()
            .iter()
            .find(|(file, _)| *file == prefix)
            .unwrap()
            .1
    });
    let separation = observed.checked_sub(prefix_observed).unwrap();
    let ceiling = prefix_observed.checked_add(separation / 2).unwrap();
    // Fresh repositories can encode raw identity discriminators differently.
    // An interior ceiling avoids depending on exact cross-repository quote equality.
    assert!(prefix_observed < ceiling && ceiling < observed);
    assert!(ceiling > 256 * 1024 * 1024 * 8);
    assert!(
        snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Large")
    );
    drop((snapshot, calibration));
    let (_storage, root, paths) = fixture();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &cancellation).unwrap();
    OBSERVED.with(|values| values.borrow_mut().clear());
    let ((first, refused, refusal_count), profile) = capture_first_slice_analysis_profile(|| {
        let first = durable::with_restore_test_ceiling(ceiling, || {
            service.index_repository(root.path(), &cancellation)
        })
        .unwrap();
        let refused = OBSERVED.with(|values| {
            values
                .borrow()
                .iter()
                .copied()
                .filter(|(_, bytes)| *bytes > ceiling)
                .collect::<Vec<_>>()
        });
        let first_refusal_count = refused.len();
        assert_current_restore_quotes(&service, first.generation, "first-index", ceiling);
        assert_selective_facts(&service, first.generation, "first-index", observed, ceiling);
        let original_note = assert_sources(&service, first.generation, root.path());
        let noop = durable::with_restore_test_ceiling(ceiling, || {
            service.index_repository(root.path(), &cancellation)
        })
        .unwrap();
        assert_eq!(noop, first);
        assert_eq!(
            assert_note_metadata(&service, noop.generation),
            original_note
        );
        fs::write(
            root.path().join("a.cs"),
            "// written-lifecycle\nclass First { public int Keep() { return 3; } }\n",
        )
        .unwrap();
        OBSERVED.with(|values| values.borrow_mut().clear());
        let changed = durable::with_restore_test_ceiling(ceiling, || {
            service.index_repository(root.path(), &cancellation)
        })
        .unwrap();
        let refusal_count = first_refusal_count
            + OBSERVED.with(|values| {
                values
                    .borrow()
                    .iter()
                    .filter(|(_, bytes)| *bytes > ceiling)
                    .count()
            });
        assert_ne!(changed.generation, first.generation);
        assert_eq!(changed.parent, Some(first.generation));
        assert_current_restore_quotes(&service, changed.generation, "changed-index", ceiling);
        assert_selective_facts(
            &service,
            changed.generation,
            "changed-index",
            observed,
            ceiling,
        );
        let changed_note = assert_sources(&service, changed.generation, root.path());
        drop(service);
        let reopened = durable::with_restore_test_ceiling(ceiling, || {
            FirstSliceService::new_durable(4, paths.state_dir(), &cancellation)
        })
        .unwrap();
        assert_eq!(
            reopened.active_generation_for(first.repository),
            Some(changed.generation)
        );
        assert_selective_facts(
            &reopened,
            first.generation,
            "reopened-first",
            observed,
            ceiling,
        );
        assert_selective_facts(
            &reopened,
            changed.generation,
            "reopened-changed",
            observed,
            ceiling,
        );
        assert_eq!(
            assert_note_metadata(&reopened, first.generation),
            original_note
        );
        assert_eq!(
            assert_sources(&reopened, changed.generation, root.path()),
            changed_note
        );
        (first, refused, refusal_count)
    });
    let observation = profile.first_oracle_restore_refusal.as_ref().unwrap();
    assert_eq!(observation.repository, first.repository);
    assert_eq!(observation.generation, first.generation);
    assert_eq!(
        (observation.file, observation.memory.total_bytes),
        refused[0]
    );
    assert_eq!(observation.limit, ceiling);
    assert_eq!(
        observation.scope,
        "prospective_cumulative_restore_model_not_heap_or_rss"
    );
    assert_eq!(
        observation.sidecar_input_scope,
        "reserved_envelope_not_actual_written_descriptors"
    );
    assert_eq!(
        observation.ir_text_scope,
        "prospective_ir_utf8_content_excludes_sources_catalog_sidecars_allocation_capacity_and_rss"
    );
    assert!(observation.prospective_ir_string_content_bytes > 0);
    assert!(observation.memory.total_bytes > ceiling);
    assert!(profile.counts_complete);
    assert!(
        refusal_count > 1,
        "both real indexing operations refuse a candidate"
    );
    assert_eq!(
        profile.later_oracle_restore_refusals,
        u64::try_from(refusal_count - 1).unwrap()
    );
    let memory = &observation.memory;
    assert_eq!(
        memory.materialization_bytes,
        memory.record_workspace_bytes + memory.transient_decode_bytes
    );
    assert_eq!(
        memory.index_build_bytes,
        memory.physical_rows * memory.index_build_bytes_per_row
    );
    assert_eq!(
        memory.text_memory_bytes,
        memory.retained_text_bytes * memory.text_copies
    );
    assert!(memory.retained_text_bytes <= memory.text_bytes);
    assert_eq!(memory.text_copies, 6);
    assert_eq!(
        memory.total_bytes,
        memory.materialization_bytes
            + memory.index_build_bytes
            + memory.text_memory_bytes
            + memory.sidecar_staging_bytes
    );
    let sidecars = serde_json::to_value(&observation.reserved_sidecars).unwrap();
    assert_eq!(sidecars["incremental_encoded_bytes"], 256_u64 * 1024 * 1024);
    assert_eq!(
        memory.sidecar_staging_bytes,
        (sidecars["incremental_encoded_bytes"].as_u64().unwrap()
            + sidecars["authority_encoded_bytes"].as_u64().unwrap()
            + sidecars["packed_index_encoded_bytes"].as_u64().unwrap())
            * sidecars["memory_multiplier"].as_u64().unwrap()
            + sidecars["fixed_overhead_bytes"].as_u64().unwrap()
    );
    let encoded = serde_json::to_string(&profile).unwrap();
    assert!(!encoded.contains("written-lifecycle"));
    assert!(!encoded.contains("b.cs"));
    let encoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        encoded["first_oracle_restore_refusal"]["prospective_ir_string_content_bytes"],
        observation.prospective_ir_string_content_bytes
    );
    assert_eq!(
        encoded["first_oracle_restore_refusal"]["prospective_ir_extension_payload_bytes"],
        observation.prospective_ir_extension_payload_bytes
    );
}

#[test]
fn first_refusal_text_metrics_match_committed_candidate_after_optional_eviction() {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/compatibility/ir/1.1/document.json"
    ));
    let rootlight_ir::IrDocument::NormalizedV1_1(mut source) =
        rootlight_ir::decode_ir_document(bytes, &IrLimits::default(), &ExtensionSupport::default())
            .unwrap()
    else {
        panic!("normalized fixture required");
    };
    let manifest_hash = content_hash(b"text-observation-manifest");
    let configuration_hash = content_hash(b"text-observation-config");
    let provider_set_hash = content_hash(b"text-observation-provider");
    let generation = derive_generation(GenerationIdentity {
        repository: source.repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
    })
    .id();
    let rebound = String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace(&source.generation.to_string(), &generation.to_string());
    let rootlight_ir::IrDocument::NormalizedV1_1(rebound) = rootlight_ir::decode_ir_document(
        rebound.as_bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap() else {
        panic!("generation-rebound normalized fixture required");
    };
    source = rebound;
    source.source_mappings.clear();
    assert_eq!(source.files.len(), 1);
    assert_eq!(source.extensions.len(), 1);
    let file = source.files[0].clone();
    let reference = source.occurrences[0].source.clone();
    let lexical = rootlight_ir::LexicalEvidenceV1::from_complete_text(
        rootlight_ir::LexicalEvidenceKind::CommentSummary,
        FactRef::File(file.id),
        rootlight_ir::LexicalEvidenceFormat::PlainText,
        "optional prefix text with a UTF-8 symbol: λ",
    )
    .unwrap();
    let optional = rootlight_ir::new_lexical_evidence_envelope(
        source.repository,
        source.generation,
        file.provenance,
        reference,
        &lexical,
    )
    .unwrap();
    let optional_id = optional.id;
    let mut target = NormalizedIrDocument::empty(source.repository, source.generation);
    let mut coverage = CoverageRecord {
        id: FactId::from_bytes([0; 20]),
        repository: source.repository,
        generation: source.generation,
        scope: CoverageScope::File(file.id),
        domain: IrFactDomain::Extensions,
        tier: AnalysisTier::TierC,
        status: CoverageStatus::Complete,
        discovered: 1,
        indexed: 1,
        skipped: 0,
        provenance: file.provenance,
        evidence: optional.evidence.clone(),
    };
    coverage.id = derive_coverage_record_id(&coverage).unwrap();
    target.coverage_records.push(coverage);
    target.extensions.push(optional);
    let before = IrDocumentUsage::measure(&target, &IrLimits::default()).unwrap();
    assert!(before.total_string_bytes() > 0);
    assert!(before.total_extension_bytes() > 0);
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let mut floor_document = NormalizedIrDocument::empty(source.repository, source.generation);
    floor_document.files.push(file.clone());
    let floor = OracleLayoutFragment::measure(&floor_document, 0, &context, &mut |_| {
        Ok::<_, FirstSliceError>(())
    })
    .unwrap();
    let floor_bytes = floor.retained_bytes().unwrap();
    let mut initial = target.clone();
    initial.files.push(file.clone());
    let initial =
        OracleLayoutFragment::measure(&initial, 0, &context, &mut |_| Ok::<_, FirstSliceError>(()))
            .unwrap();
    let mut census = OracleLayoutCensus::new(source.repository, source.generation);
    census
        .preview(
            OracleLayoutFragment::empty(source.repository, source.generation),
            initial,
            0,
            &context,
            &mut |_| Ok::<_, FirstSliceError>(()),
        )
        .unwrap()
        .commit();
    let metadata = GenerationMetadata::new(
        source.repository,
        source.generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .unwrap();
    let claim = FileIdentityClaim {
        file: file.id,
        repository: file.repository,
        path: file.path.clone(),
        path_identity: RelativePath::parse(Path::new(&file.path))
            .unwrap()
            .identity_bytes()
            .to_vec(),
        content_hash: file.content_hash,
        byte_length: file.byte_length,
    };
    let envelope = durable::restore_envelope::Envelope::prepare(
        metadata,
        &[durable::restore_envelope::Source {
            claim: &claim,
            language: &file.language,
            encoding: &file.encoding,
            generated: file.generated,
        }],
        &cancellation,
        &mut |_| Ok(()),
    )
    .unwrap();
    let mut reservation = Reservation {
        census,
        floors: vec![Slot {
            file: file.id,
            floor: Some(floor),
        }],
        floor_bytes,
        table_bytes: u64::try_from(size_of::<Slot>()).unwrap(),
        next: 0,
        workspace: Workspace::default(),
        host_floor: OracleLayoutFragment::empty(source.repository, source.generation),
        envelope,
    };
    let mut state = DocumentAppendState::from_document(&target).unwrap();
    let mut limits = IrLimits::default();
    limits.max_extensions = 1;
    let prepared = prepare_normalized_document_append(
        &target,
        source,
        &limits,
        &state,
        &cancellation,
        None,
        None,
    )
    .unwrap();
    assert_eq!(prepared.extension_plan.dropped_target_indices, [0]);
    let (refusal, profile) = capture_first_slice_analysis_profile(|| {
        durable::with_restore_test_ceiling(1, || {
            reservation.refusal(
                &target,
                &prepared,
                file.id,
                &context,
                &cancellation,
                &mut |_| Ok(()),
            )
        })
    });
    assert!(refusal.unwrap().is_some());
    let observation = profile.first_oracle_restore_refusal.as_ref().unwrap();
    assert_eq!(observation.file, file.id);
    assert_eq!(observation.repository, target.repository);
    assert_eq!(observation.generation, target.generation);
    assert_eq!(observation.limit, 1);
    assert!(observation.memory.total_bytes > observation.limit);
    assert!(profile.counts_complete);
    assert_eq!(
        target.extensions[0].id, optional_id,
        "refusal leaves the prefix untouched"
    );
    commit_prepared_document_append(&mut target, prepared, &limits, &mut state, &cancellation)
        .unwrap();
    assert_eq!(target.extensions.len(), 1);
    assert_ne!(target.extensions[0].id, optional_id);
    let coverage = target
        .coverage_records
        .iter()
        .find(|record| {
            record.scope == CoverageScope::File(file.id)
                && record.domain == IrFactDomain::Extensions
        })
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Bounded);
    assert_eq!(
        (coverage.discovered, coverage.indexed, coverage.skipped),
        (1, 0, 1)
    );
    let actual = IrDocumentUsage::measure(&target, &IrLimits::default()).unwrap();
    assert_eq!(
        observation.prospective_ir_string_content_bytes,
        u64::try_from(actual.total_string_bytes()).unwrap()
    );
    assert_eq!(
        observation.prospective_ir_extension_payload_bytes,
        u64::try_from(actual.total_extension_bytes()).unwrap()
    );
    assert!(actual.total_string_bytes() > 0);
    assert!(actual.total_extension_bytes() > 0);
}

#[test]
fn file_capacity_witness_is_finite_pinned_and_distinct_from_structural_append() {
    let file = FileId::from_bytes([0x61; 20]);
    let limit = durable::oracle_restore_memory_limit();
    assert!(FileRestoreCapacity::from_observed(file, limit).is_none());
    assert!(FileRestoreCapacity::from_observed(file, limit - 1).is_none());
    let failure = FileRestoreCapacity::from_observed(file, limit + 1).unwrap();
    assert!(failure.detail(file).unwrap().len() <= EXPLANATION_BYTES);
    assert!(matches!(
        failure.detail(FileId::from_bytes([0x62; 20])),
        Err(FirstSliceError::Identity)
    ));
}

#[test]
fn workspace_reuses_high_water_but_host_refusal_never_advances_its_owner() {
    let mut workspace = Workspace::default();
    let mut charged = 0;
    workspace
        .admit(256, &mut |bytes| {
            charged += bytes;
            Ok(())
        })
        .unwrap();
    workspace
        .admit(128, &mut |_| panic!("no new simultaneous workspace"))
        .unwrap();
    let failure = generation_memory_limit(0, 257, PendingGenerationMemory::Staged);
    assert_eq!(workspace.admit(257, &mut |_| Err(failure)), Err(failure));
    assert_eq!(workspace.peak, 256);
    workspace
        .admit(300, &mut |bytes| {
            charged += bytes;
            Ok(())
        })
        .unwrap();
    assert_eq!(charged, 300);
}

#[test]
fn healthy_graph_reuses_admitted_undo_workspace_without_additional_host_request() {
    let cancellation = Cancellation::new();
    let (_calibration_storage, calibration_root, calibration_paths) = fixture();
    let mut calibration =
        FirstSliceService::new_durable(4, calibration_paths.state_dir(), &cancellation).unwrap();
    UNDO_CAPTURED.with(|value| value.set(0));
    UNDO_ADDITIONAL.with(|value| value.set(0));
    let complete = calibration
        .index_repository(calibration_root.path(), &cancellation)
        .unwrap();
    let captured = UNDO_CAPTURED.with(Cell::get);
    let additional = UNDO_ADDITIONAL.with(Cell::get);
    assert!(captured > 0);
    assert_eq!(additional, 0);
    assert!(
        calibration
            .loaded_generation_snapshot(complete.generation)
            .unwrap()
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Large")
    );
    assert_sources(&calibration, complete.generation, calibration_root.path());
    eprintln!(
        "complete graph undo reuse: captured_owned_bytes={captured} additional_host_request_bytes={additional}"
    );

    let (_storage, root, paths) = fixture();
    let mut admitted = FirstSliceService::new_durable(4, paths.state_dir(), &cancellation).unwrap();
    let accepted = with_undo_additional_limit(0, || {
        let accepted = admitted
            .index_repository(root.path(), &cancellation)
            .unwrap();
        assert!(UNDO_CAPTURED.with(Cell::get) > 0);
        assert_eq!(UNDO_ADDITIONAL.with(Cell::get), 0);
        accepted
    });
    let snapshot = admitted
        .loaded_generation_snapshot(accepted.generation)
        .unwrap();
    assert!(
        snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Large")
    );
    assert!(
        !snapshot
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
    );
    assert_sources(&admitted, accepted.generation, root.path());
    assert!(admitted.structural_artifacts.staged.is_empty());
}

#[test]
fn atomic_fsharp_and_classic_projects_leave_prospective_scope_unavailable() {
    let (_storage, root, paths) = fixture();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let mut claims = snapshot
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::FILE_IDENTITY_CLAIM_NAMESPACE)
        .map(|extension| rootlight_ir::decode_file_identity_claim_envelope(extension).unwrap())
        .collect::<Vec<_>>();
    assert!(scope_supported(&claims, &[]));
    claims[0].path = "Project.fsproj".to_owned();
    assert!(!scope_supported(&claims, &[]));
    claims[0].path = "Project.vbp".to_owned();
    assert!(!scope_supported(&claims, &[]));
}

#[test]
fn real_resolver_growth_above_restore_floor_keeps_exact_original_structural_facts() {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/compatibility/ir/1.1/document.json"
    ));
    let rootlight_ir::IrDocument::NormalizedV1_1(mut original) =
        rootlight_ir::decode_ir_document(bytes, &IrLimits::default(), &ExtensionSupport::default())
            .unwrap()
    else {
        panic!("normalized fixture required");
    };
    let manifest_hash = content_hash(b"restore-control-manifest");
    let configuration_hash = content_hash(b"restore-control-config");
    let provider_set_hash = content_hash(b"restore-control-provider");
    let generation = derive_generation(GenerationIdentity {
        repository: original.repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
    })
    .id();
    let rebound = String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace(&original.generation.to_string(), &generation.to_string());
    let rootlight_ir::IrDocument::NormalizedV1_1(rebound) = rootlight_ir::decode_ir_document(
        rebound.as_bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap() else {
        panic!("generation-rebound normalized fixture required");
    };
    original = rebound;
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let source = original.occurrences[0].source.clone();
    let file = original
        .files
        .iter()
        .find(|file| file.id == source.span().file())
        .unwrap();
    assert!(file.byte_length >= 8);
    for offset in 0..file.byte_length.min(32) {
        let mut site = original.occurrences[0].clone();
        site.source = SourceRef::new(
            original.repository,
            original.generation,
            SourceSpan::new(file.id, offset, offset + 1).unwrap(),
            file.content_hash,
            None,
        );
        site.role = OccurrenceRole::CallSite;
        site.target = OccurrenceTarget::Unresolved {
            text_hash: content_hash(original.entities[0].canonical_name.as_bytes()),
        };
        site.syntactic_text_hash = content_hash(original.entities[0].canonical_name.as_bytes());
        site.id = derive_occurrence_record_id(&site).unwrap();
        original.occurrences.push(site);
    }
    original = rootlight_ir::canonicalize_ir_document(
        original,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let metadata = GenerationMetadata::new(
        original.repository,
        original.generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .unwrap();
    let claims = original
        .files
        .iter()
        .map(|file| FileIdentityClaim {
            file: file.id,
            repository: file.repository,
            path: file.path.clone(),
            path_identity: RelativePath::parse(Path::new(&file.path))
                .unwrap()
                .identity_bytes()
                .to_vec(),
            content_hash: file.content_hash,
            byte_length: file.byte_length,
        })
        .collect::<Vec<_>>();
    let descriptions = claims
        .iter()
        .map(|claim| {
            let file = original
                .files
                .iter()
                .find(|file| file.id == claim.file)
                .unwrap();
            durable::restore_envelope::Source {
                claim,
                language: &file.language,
                encoding: &file.encoding,
                generated: file.generated,
            }
        })
        .collect::<Vec<_>>();
    let envelope = durable::restore_envelope::Envelope::prepare(
        metadata,
        &descriptions,
        &cancellation,
        &mut |_| Ok(()),
    )
    .unwrap();
    let floor = host_floor(
        original.repository,
        original.generation,
        original.files[0].id,
        0,
    )
    .unwrap();
    let mut modeled_original = original.clone();
    modeled_original.diagnostics.extend(floor.diagnostics);
    let before = OracleLayoutFragment::measure(&modeled_original, 0, &context, &mut |_| {
        Ok::<_, FirstSliceError>(())
    })
    .unwrap();
    let limit = envelope
        .observed(before.read_memory_plan(&context).unwrap())
        .unwrap();
    let limits = resolution_limits_for_occurrences(original.occurrences.len()).unwrap();
    let candidate = apply_bounded_resolution(
        original.clone(),
        limits,
        ResolverFactContext::new(content_hash(RESOLVER_BINARY_SEED)),
        &cancellation,
    )
    .unwrap();
    assert_ne!(candidate.occurrences, original.occurrences);
    let after = OracleLayoutFragment::measure(&candidate, 0, &context, &mut |_| {
        Ok::<_, FirstSliceError>(())
    })
    .unwrap();
    let observed = envelope
        .observed(after.read_memory_plan(&context).unwrap())
        .unwrap();
    assert!(
        observed > limit,
        "actual resolver growth must exceed its bounded warning floor: {observed} <= {limit}"
    );
    let mut admission = FinalAdmission {
        envelope,
        workspace: Workspace::default(),
        expected_final_plan: None,
    };
    let mut charged = 0;
    let restored = durable::with_restore_test_ceiling(limit, || {
        admission.resolve(
            original.clone(),
            limits,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
            &cancellation,
            &mut |bytes| {
                charged += bytes;
                Ok(())
            },
        )
    })
    .unwrap();
    assert!(charged > 0);
    assert_eq!(restored.files, original.files);
    assert_eq!(restored.entities, original.entities);
    assert_eq!(restored.occurrences, original.occurrences);
    assert_eq!(restored.relations, original.relations);
    assert_eq!(restored.provenance, original.provenance);
    assert_eq!(restored.source_mappings, original.source_mappings);
    assert_eq!(restored.extensions, original.extensions);
    assert!(
        restored
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "resolution-restore-capacity")
    );
    assert!(
        !restored
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
    );
    assert!(
        restored
            .coverage_records
            .iter()
            .filter(|coverage| matches!(
                coverage.domain,
                IrFactDomain::Occurrences | IrFactDomain::Relations
            ))
            .all(|coverage| matches!(
                coverage.status,
                CoverageStatus::Unknown | CoverageStatus::Bounded
            ))
    );
}
