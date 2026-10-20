//! Executable service regressions for genuine aggregate-only source recovery.
//! Immutable captured bytes must survive rejected structural output without
//! treating unknown analysis or incomplete text search as complete coverage.

use super::*;
use crate::aggregate_source_recovery::{AggregateCapacityFailure, PreparedSourceFrame};
use crate::office_fixture;
use rootlight_ir::{
    AggregateCapacityResource, AggregateCapacityStage, IrDocumentUsageError,
    IrDocumentValidationError, decode_aggregate_source_recovery_envelope,
};
use rootlight_source::{SourceEncoding, SourceReadOptions};

fn utf16(text: &str) -> Vec<u8> {
    std::iter::once(0xfeff)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn rust_source(prefix: &str, count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| format!("pub fn {prefix}_{index}() -> u8 {{ 1 }}\n"))
        .collect::<String>()
        .into_bytes()
}

fn individual(
    service: &FirstSliceService,
    path: &str,
    bytes: Vec<u8>,
    encoding: &str,
) -> NormalizedIrDocument {
    let repository = derive_repository(b"authored-aggregate-individual").id();
    let generation = GenerationId::from_bytes([73; 20]);
    let path = RelativePath::parse(Path::new(path)).unwrap();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let snapshot =
        SourceSnapshot::from_persisted(repository, path, file, content_hash(&bytes), bytes)
            .unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "rust",
        encoding,
        &service.analysis_limits,
    )
    .unwrap();
    let artifact = service.analyzers["rust"]
        .prepare_artifact(&request, &deadline())
        .unwrap();
    service.analyzers["rust"]
        .analyze_prepared_artifact(
            &request,
            &artifact,
            service.extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
        .document()
        .clone()
}

fn paths() -> (TempDir, RuntimePaths) {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    (storage, paths)
}

fn assert_exact(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
) -> SourceRef {
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let generation = lease.generation();
    let file = generation
        .document()
        .files
        .iter()
        .chain(
            generation
                .source_files()
                .entries()
                .iter()
                .map(|row| row.file()),
        )
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.content_hash, content_hash(bytes));
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    let reference = file.evidence.source.clone().unwrap();
    let mut observed = Vec::new();
    for offset in (0..bytes.len()).step_by(32 * 1024) {
        let end = bytes.len().min(offset + 32 * 1024);
        let selected = SourceRef::new(
            reference.repository(),
            reference.generation(),
            SourceSpan::new(
                file.id,
                u64::try_from(offset).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            reference.content_hash(),
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![selected.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, selected);
        observed.extend_from_slice(&read.data.chunks[0].bytes);
    }
    assert_eq!(observed, bytes);
    reference
}

#[test]
fn each_input_fits_but_combined_entities_publish_one_complete_source_generation() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let first = rust_source("alpha", 64);
    let second = rust_source("beta", 64);
    fs::write(root.path().join("first.rs"), &first).unwrap();
    fs::write(root.path().join("second.rs"), &second).unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let documents = [
        individual(&service, "first.rs", first.clone(), "utf-8"),
        individual(&service, "second.rs", second.clone(), "utf-8"),
    ];
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = documents
        .iter()
        .map(|document| document.entities.len())
        .max()
        .unwrap();
    // Keep this universal-fallback case outside the new three-gap minimum.
    limits.max_skipped_regions = 2;
    assert!(limits.max_entities > 0);
    assert!(
        documents
            .iter()
            .map(|document| document.entities.len())
            .sum::<usize>()
            > limits.max_entities
    );
    for document in &documents {
        IrDocumentUsage::measure(document, &limits).unwrap();
    }
    replace_service_ir_limits(&mut service, limits.clone());
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert_eq!(snapshot.file_count(), 2);
    assert_eq!(snapshot.document().files.len(), 1);
    assert_eq!(snapshot.source_files().len(), 1);
    assert!(snapshot.document().entities.is_empty());
    assert!(snapshot.document().occurrences.is_empty());
    assert!(snapshot.document().relations.is_empty());
    let reference = assert_exact(&service, &receipt, "first.rs", &first);
    assert_exact(&service, &receipt, "second.rs", &second);
    let detail = service
        .source_file_recovery_until(receipt.generation, reference.span().file(), &deadline())
        .unwrap()
        .unwrap();
    assert_eq!(detail.cause.stage, AggregateCapacityStage::StructuralAppend);
    assert_eq!(detail.cause.resource, AggregateCapacityResource::Entities);
    assert_eq!(
        detail.cause.limit,
        u64::try_from(limits.max_entities).unwrap()
    );
    assert_eq!(
        detail.cause.observed,
        u64::try_from(
            documents
                .iter()
                .map(|document| document.entities.len())
                .sum::<usize>()
        )
        .unwrap()
    );
    assert!(detail.cause.observed > detail.cause.limit);
    assert_eq!(detail.cause.members, 2);
    assert_eq!(detail.cause.derived_members, 0);
    assert_eq!(
        detail.cause.source_inventory,
        snapshot.metadata().manifest_hash()
    );
    for canary in ["alpha_63", "beta_63"] {
        let result = service
            .code_locate(
                receipt.generation,
                canary.to_owned(),
                LocateMode::Text,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(
            result
                .data
                .hits
                .iter()
                .any(|hit| hit.symbol.is_none() && hit.kind == "file")
        );
    }
    assert_eq!(
        service.index_repository(root.path(), &deadline()).unwrap(),
        receipt
    );
    let changed = rust_source("gamma", 64);
    fs::write(root.path().join("second.rs"), &changed).unwrap();
    let updated = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(updated.parent, Some(receipt.generation));
    assert_ne!(updated.generation, receipt.generation);
    assert_exact(&service, &receipt, "second.rs", &second);
    assert_exact(&service, &updated, "second.rs", &changed);
    drop(service);
    fs::remove_file(root.path().join("first.rs")).unwrap();
    fs::remove_file(root.path().join("second.rs")).unwrap();
    let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    assert_exact(&restored, &receipt, "first.rs", &first);
    assert_exact(&restored, &updated, "second.rs", &changed);
    assert_eq!(
        restored
            .source_file_recovery_until(receipt.generation, reference.span().file(), &deadline())
            .unwrap(),
        Some(detail)
    );
}

#[test]
fn original_derived_utf16_and_captured_members_keep_exact_bytes_and_separate_text_gaps() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let first = rust_source("alpha", 64);
    let second = rust_source("beta", 64);
    let encoded = utf16("// λ😀 aggregateEncodedCanary\npub fn encoded() {}\n");
    let module = b"Attribute VB_Name = \"Module\"\r\nPublic Sub Recover()\r\nEnd Sub\r\n";
    let book = office_fixture::book(&[("Module", module)]);
    let mut captured = vec![b' '; 65_537];
    captured.extend_from_slice(b"aggregateCapturedTailCanary");
    for (name, bytes) in [
        ("first.rs", &first),
        ("second.rs", &second),
        ("encoded.rs", &encoded),
        ("book.xlsm", &book),
        ("captured.rs", &captured),
    ] {
        fs::write(root.path().join(name), bytes).unwrap();
    }
    let configuration = "version = \"1.1\"\n[analysis]\nmax_source_file_bytes = 65536\n";
    let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
        source: rootlight_config::ConfigSource::User,
        contents: configuration,
    }])
    .unwrap();
    let policy = FirstSliceStoragePolicy::from_config(config, 4);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let single = individual(&service, "first.rs", first.clone(), "utf-8");
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = single.entities.len();
    IrDocumentUsage::measure(&single, &limits).unwrap();
    replace_service_ir_limits(&mut service, limits);
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert_eq!(receipt.discovered_inputs, 5);
    assert_eq!(snapshot.file_count(), 6);
    assert_eq!(receipt.extracted_files, 1);
    for (name, bytes) in [
        ("first.rs", &first),
        ("second.rs", &second),
        ("encoded.rs", &encoded),
        ("book.xlsm", &book),
        ("captured.rs", &captured),
    ] {
        assert_exact(&service, &receipt, name, bytes);
    }
    let derived = snapshot
        .document()
        .files
        .iter()
        .chain(
            snapshot
                .source_files()
                .entries()
                .iter()
                .map(|row| row.file()),
        )
        .find(|file| file.path.contains("/@vba/") && file.encoding == "utf-8")
        .unwrap();
    let derived_path = derived.path.clone();
    let derived_reference = assert_exact(&service, &receipt, &derived_path, module);
    assert_eq!(
        service
            .source_file_recovery_until(
                receipt.generation,
                derived_reference.span().file(),
                &deadline()
            )
            .unwrap()
            .unwrap()
            .text_search,
        FirstSliceTextSearchCoverage::WholeSourceProjection
    );
    assert!(
        book.len() < 65_536,
        "authored container isolates the short binary classification"
    );
    let container_reference = assert_exact(&service, &receipt, "book.xlsm", &book);
    assert_eq!(
        snapshot
            .find_file(container_reference.span().file())
            .unwrap()
            .encoding,
        "bytes"
    );
    let container_recovery = service
        .source_file_recovery_until(
            receipt.generation,
            container_reference.span().file(),
            &deadline(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        container_recovery.text_search,
        FirstSliceTextSearchCoverage::UnsupportedEncoding
    );
    let encoded_reference = assert_exact(&service, &receipt, "encoded.rs", &encoded);
    assert_eq!(
        service
            .source_file_recovery_until(
                receipt.generation,
                encoded_reference.span().file(),
                &deadline()
            )
            .unwrap()
            .unwrap()
            .text_search,
        FirstSliceTextSearchCoverage::WholeSourceProjection
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![derived_reference],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    let parent = read.data.chunks[0].extracted_from.as_ref().unwrap();
    assert_eq!(parent.content_hash(), content_hash(&book));
    let captured_ref = assert_exact(&service, &receipt, "captured.rs", &captured);
    let detail = service
        .source_file_recovery_until(receipt.generation, captured_ref.span().file(), &deadline())
        .unwrap()
        .unwrap();
    assert_eq!(detail.cause.derived_members, 1);
    assert_eq!(
        detail.text_search,
        FirstSliceTextSearchCoverage::MetadataOnlyCaptured
    );
    let no_text = service
        .code_locate(
            receipt.generation,
            "aggregateCapturedTailCanary".to_owned(),
            LocateMode::Text,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(
        no_text.data.hits.is_empty(),
        "explicit metadata-only gap cannot be a full-text pass"
    );
    assert_eq!(
        service.index_repository(root.path(), &deadline()).unwrap(),
        receipt
    );
    let updated_module = b"Attribute VB_Name = \"Module\"\r\nPublic Sub Changed()\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("book.xlsm"),
        office_fixture::typed_book(&[("Module", updated_module, false)]),
    )
    .unwrap();
    fs::write(root.path().join("second.rs"), rust_source("changed", 64)).unwrap();
    let updated = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(updated.parent, Some(receipt.generation));
    assert_exact(&service, &receipt, &derived_path, module);
    assert_exact(&service, &updated, &derived_path, updated_module);
    drop(service);
    let restored =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_exact(&restored, &receipt, "encoded.rs", &encoded);
    assert_exact(&restored, &receipt, &derived_path, module);
    assert_exact(&restored, &receipt, "book.xlsm", &book);
    assert_eq!(
        restored
            .source_file_recovery_until(
                receipt.generation,
                container_reference.span().file(),
                &deadline()
            )
            .unwrap(),
        Some(container_recovery)
    );
    let restored_derived = assert_exact(&restored, &receipt, &derived_path, module);
    assert_eq!(
        restored
            .source_file_recovery_until(
                receipt.generation,
                restored_derived.span().file(),
                &deadline()
            )
            .unwrap()
            .unwrap()
            .text_search,
        FirstSliceTextSearchCoverage::WholeSourceProjection
    );
    assert_eq!(
        restored
            .source_file_recovery_until(receipt.generation, captured_ref.span().file(), &deadline())
            .unwrap(),
        Some(detail)
    );
}

#[test]
fn only_checked_aggregate_errors_construct_recovery_and_append_stays_atomic() {
    let mut limits = IrLimits::default();
    limits.max_total_string_bytes = 37;
    let repository = derive_repository(b"aggregate-private-witness").id();
    let generation = GenerationId::from_bytes([47; 20]);
    let document = |id| {
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.files.push(FileRecord {
            id: FileId::from_bytes([id; 20]),
            repository,
            generation,
            path: "src/one.rs".to_owned(),
            path_locator: None,
            content_hash: content_hash(b"source"),
            byte_length: 6,
            language: "rust".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([48; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        });
        document
    };
    let mut target = NormalizedIrDocument::empty(repository, generation);
    let mut state = DocumentAppendState::from_document(&target).unwrap();
    state.aggregate_recovery = true;
    IrDocumentUsage::measure(&document(1), &limits).unwrap();
    IrDocumentUsage::measure(&document(2), &limits).unwrap();
    append_normalized_document(&mut target, document(1), &limits, &mut state).unwrap();
    let before = target.clone();
    let before_state = state.clone();
    assert!(matches!(
        append_normalized_document(&mut target, document(2), &limits, &mut state),
        Err(FirstSliceError::AggregateCapacity(_))
    ));
    assert_eq!(target, before);
    assert_eq!(state, before_state);
    for error in [
        IrDocumentUsageError::AccountingOverflow,
        IrDocumentUsageError::Interrupted,
        IrDocumentUsageError::Validation(IrDocumentValidationError::StringLimit {
            field: "path",
            observed: 8,
            limit: 7,
        }),
    ] {
        assert!(AggregateCapacityFailure::from_combined_usage(&error).is_none());
    }
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        append_normalized_document_with_cancellation(
            &mut target,
            document(2),
            &limits,
            &mut state,
            &cancellation
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(target, before);
}

#[test]
fn identity_bound_policy_changes_precede_noop_and_fallback_observer_failure_preserves_active() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let source = rust_source("original", 64);
    fs::write(root.path().join("first.rs"), &source).unwrap();
    fs::write(root.path().join("second.rs"), rust_source("peer", 64)).unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let initial = service.index_repository(root.path(), &deadline()).unwrap();
    let old_provider = service
        .provider_set_hash(FirstSliceIndexMode::Structural)
        .unwrap();
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = individual(&service, "first.rs", source.clone(), "utf-8")
        .entities
        .len();
    // Preserve the universal-fallback observer's meaningful trigger.
    limits.max_skipped_regions = 2;
    replace_service_ir_limits(&mut service, limits);
    assert_ne!(
        old_provider,
        service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap()
    );
    let mut admitted = service.admit_repository(root.path(), &deadline()).unwrap();
    let result = service.prepare_repository_after_admission_with_options_and_observers(
        root.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        &mut admitted,
        &deadline(),
        |_| {},
        |planning| {
            if planning.estimated_analysis_units == 0 {
                Err("reject complete fallback admission")
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(
        result,
        Err(FirstSlicePreparationError::PlanningObserver(
            "reject complete fallback admission"
        ))
    ));
    assert_eq!(
        service.active_generation_for(initial.repository),
        Some(initial.generation)
    );
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reservations
            .is_empty()
    );
    assert_exact(&service, &initial, "first.rs", &source);
}

#[test]
fn generation_text_capacity_has_its_own_typed_pre_persistence_origin() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let mut bytes = String::new();
    for index in 0..600 {
        bytes.push_str(&format!(
            "pub fn named_{}_{}() {{}}\n",
            "x".repeat(160),
            index
        ));
    }
    fs::write(root.path().join("named.rs"), &bytes).unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let document = individual(&service, "named.rs", bytes.as_bytes().to_vec(), "utf-8");
    IrDocumentUsage::measure(&document, service.analysis_limits.ir()).unwrap();
    let defaults = GenerationBudget::default();
    service.generation_budget = GenerationBudget::new(
        defaults.limit(rootlight_storage::GenerationResource::Rows),
        defaults.limit(rootlight_storage::GenerationResource::SourceReferences),
        32 * 1024,
    )
    .unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let reference = assert_exact(&service, &receipt, "named.rs", bytes.as_bytes());
    let recovery = service
        .source_file_recovery_until(receipt.generation, reference.span().file(), &deadline())
        .unwrap()
        .unwrap();
    assert_eq!(recovery.cause.stage, AggregateCapacityStage::GenerationText);
    assert!(matches!(
        recovery.cause.resource,
        AggregateCapacityResource::GenerationTextBytes
            | AggregateCapacityResource::GenerationEncodedTextBytes
    ));
    assert!(recovery.cause.observed > recovery.cause.limit);
    let unrelated = IdentityVerificationError::Control(
        rootlight_storage::GenerationControlError::BudgetExceeded {
            resource: rootlight_storage::GenerationResource::Rows,
            observed: 2,
            limit: 1,
        },
    );
    assert!(AggregateCapacityFailure::from_generation_text(&unrelated).is_none());
    let overflow = IdentityVerificationError::Control(
        rootlight_storage::GenerationControlError::BudgetExceeded {
            resource: rootlight_storage::GenerationResource::TextBytes,
            observed: u64::MAX,
            limit: 1,
        },
    );
    assert!(AggregateCapacityFailure::from_generation_text(&overflow).is_none());
}

#[test]
fn source_frame_refuses_missing_foreign_or_changed_raw_backing_and_cancellation() {
    let repository = derive_repository(b"aggregate-frame-authentication").id();
    let path = RelativePath::parse(Path::new("authored.rs")).unwrap();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let bytes = b"pub fn retained() {}\n".to_vec();
    let snapshot =
        SourceSnapshot::from_persisted(repository, path, file, content_hash(&bytes), bytes.clone())
            .unwrap();
    let input = ManifestInput {
        file,
        path: "authored.rs".to_owned(),
        content_hash: content_hash(&bytes),
        bytes: u64::try_from(bytes.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: rootlight_discovery::InputClass::Source,
        language_signals: Vec::new(),
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    };
    let claims =
        source_file_identity_claims(std::slice::from_ref(&input), repository, &deadline()).unwrap();
    let derived = container_sources::PreparedSources::default();
    let retained = BTreeMap::from([(file, snapshot.clone())]);
    let frame = PreparedSourceFrame::seal(
        std::slice::from_ref(&input),
        &derived,
        claims.clone(),
        retained.clone(),
        None,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        frame.snapshots[&file].content().as_ptr(),
        snapshot.content().as_ptr(),
        "sealed frame shares immutable bytes"
    );
    let charge = frame.retained_memory_bytes().unwrap();
    assert!(charge >= u64::try_from(snapshot.retained_memory_bytes().unwrap()).unwrap());
    assert!(
        charge > u64::try_from(bytes.len()).unwrap(),
        "frame metadata and raw capacity remain charged"
    );
    let service = FirstSliceService::new(2).unwrap();
    let pressure = service
        .reserve_generation_memory(
            GenerationId::from_bytes([196; 20]),
            MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES - charge + 1,
            PendingGenerationMemory::Reserved,
        )
        .unwrap();
    assert!(
        service
            .reserve_generation_memory(
                GenerationId::from_bytes([197; 20]),
                charge,
                PendingGenerationMemory::Reserved
            )
            .is_err(),
        "retained frame cannot be admitted above remaining capacity"
    );
    drop(pressure);
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reservations
            .is_empty()
    );
    assert!(matches!(
        PreparedSourceFrame::seal(
            std::slice::from_ref(&input),
            &derived,
            claims.clone(),
            BTreeMap::new(),
            None,
            &deadline()
        ),
        Err(FirstSliceError::DiscoveryDrift)
    ));
    let mut changed = input.clone();
    changed.content_hash = content_hash(b"changed");
    assert!(matches!(
        PreparedSourceFrame::seal(
            &[changed],
            &derived,
            claims.clone(),
            retained.clone(),
            None,
            &deadline()
        ),
        Err(FirstSliceError::Identity)
    ));
    let mut foreign = claims.clone();
    foreign[0].file = FileId::from_bytes([222; 20]);
    assert!(matches!(
        PreparedSourceFrame::seal(
            std::slice::from_ref(&input),
            &derived,
            foreign,
            retained.clone(),
            None,
            &deadline()
        ),
        Err(FirstSliceError::Identity)
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        PreparedSourceFrame::seal(&[input], &derived, claims, retained, None, &cancelled),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
}

#[test]
fn aggregate_explanation_missing_members_or_forged_inventory_cannot_verify() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let source = rust_source("bound", 32);
    fs::write(root.path().join("first.rs"), &source).unwrap();
    fs::write(root.path().join("second.rs"), rust_source("peer", 32)).unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = individual(&service, "first.rs", source, "utf-8")
        .entities
        .len();
    // Keep this envelope-forgery control on the original universal path.
    limits.max_skipped_regions = 2;
    replace_service_ir_limits(&mut service, limits.clone());
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let position = snapshot
        .document()
        .extensions
        .iter()
        .position(|extension| {
            extension.namespace == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE
        })
        .unwrap();
    for variant in 0..6 {
        let mut document = snapshot.document().clone();
        if variant == 0 {
            document.extensions.remove(position);
        } else {
            let envelope = document.extensions[position].clone();
            let mut cause = decode_aggregate_source_recovery_envelope(&envelope).unwrap();
            match variant {
                1 => cause.members += 1,
                2 => cause.source_inventory = content_hash(b"foreign complete inventory"),
                3 => cause.derived_members = 1,
                _ => {}
            }
            let mut anchor = envelope.evidence.source.unwrap();
            if variant == 5 {
                anchor = SourceRef::new(
                    anchor.repository(),
                    anchor.generation(),
                    SourceSpan::new(anchor.span().file(), 0, anchor.span().end_byte() - 1).unwrap(),
                    anchor.content_hash(),
                    None,
                );
            }
            document.extensions[position] = rootlight_ir::new_aggregate_source_recovery_envelope(
                document.repository,
                document.generation,
                envelope.provenance,
                anchor,
                cause,
            )
            .unwrap();
            if variant == 4 {
                document.extensions[position].version = "unknown".to_owned();
            }
        }
        assert!(
            IdentityVerifiedGeneration::verify_with_source_files(
                snapshot.metadata(),
                document,
                snapshot.source_files().clone(),
                &limits,
                &service.extensions,
                &GenerationContext::new(&deadline(), GenerationBudget::default())
            )
            .is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn individual_capacity_and_cancellation_never_turn_into_complete_source_recovery() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let source = rust_source("individual", 64);
    fs::write(root.path().join("single.rs"), &source).unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let initial = service.index_repository(root.path(), &deadline()).unwrap();
    let mut limits = service.analysis_limits.ir().clone();
    let demand = individual(&service, "single.rs", source.clone(), "utf-8")
        .entities
        .len();
    assert!(demand > 1);
    limits.max_entities = demand - 1;
    replace_service_ir_limits(&mut service, limits);
    assert!(service.index_repository(root.path(), &deadline()).is_err());
    assert_eq!(
        service.active_generation_for(initial.repository),
        Some(initial.generation)
    );
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reservations
            .is_empty()
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        service.index_repository(root.path(), &cancelled),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(
        service.active_generation_for(initial.repository),
        Some(initial.generation)
    );
    assert_exact(&service, &initial, "single.rs", &source);
}

#[test]
fn ordinary_modes_avoid_frame_copies_and_admit_retained_snapshot_metadata_before_analysis() {
    use crate::aggregate_source_recovery::FRAME_SEALS;
    let root = durable_test_tempdir();
    let name = format!("metadata_{}.rs", "x".repeat(72));
    let source = b"pub fn ordinary() {}\n";
    fs::write(root.path().join(&name), source).unwrap();
    for mode in 0..3 {
        let (_storage, paths) = paths();
        let service = if mode == 0 {
            FirstSliceService::new(2).unwrap()
        } else {
            FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap()
        };
        let representation = if mode == 2 {
            DurableGenerationRepresentation::RecoverySnapshot
        } else {
            DurableGenerationRepresentation::Oracle
        };
        let mut admission = service.admit_repository(root.path(), &deadline()).unwrap();
        FRAME_SEALS.with(|count| count.set(0));
        let floor =
            ensure_generation_memory_preflight(u64::try_from(source.len()).unwrap()).unwrap();
        let cache = service.generation_cache.clone();
        let mut observations = 0;
        let prepared = service
            .prepare_repository_with_representation_and_observers(
                root.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
                representation,
                &mut admission,
                &deadline(),
                FirstSlicePreparationObservers {
                    progress: |_| {},
                    planning: |_: &FirstSliceIndexPlanning| -> Result<(), ()> {
                        observations += 1;
                        let reserved = cache
                            .lock()
                            .unwrap()
                            .reservations
                            .values()
                            .copied()
                            .sum::<u64>();
                        if mode == 1 {
                            assert!(reserved > floor + u64::try_from(name.len() * 2).unwrap());
                        } else {
                            assert_eq!(
                                reserved, floor,
                                "disabled recovery retains no extra snapshot owner"
                            );
                        }
                        Ok(())
                    },
                },
            )
            .unwrap();
        assert!(matches!(prepared, FirstSliceIndexPreparation::Pending(_)));
        assert_eq!(observations, 1);
        FRAME_SEALS.with(|count| {
            assert_eq!(
                count.get(),
                0,
                "ordinary preparation never constructs a recovery frame"
            )
        });
        drop(prepared);
        assert!(
            service
                .generation_cache
                .lock()
                .unwrap()
                .reservations
                .is_empty()
        );
    }
    // Large metadata with a tiny body must influence prospective admission,
    // without constructing that metadata copy merely to measure it.
    let repository = derive_repository(b"aggregate-prospective-metadata").id();
    let relative = RelativePath::parse(Path::new("tiny.rs")).unwrap();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: relative.identity_bytes(),
    })
    .id();
    let snapshot = SourceSnapshot::from_persisted(
        repository,
        relative,
        file,
        content_hash(source),
        source.to_vec(),
    )
    .unwrap();
    let mut path = String::with_capacity(128 * 1024);
    path.push_str("tiny.rs");
    let input = ManifestInput {
        file,
        path,
        content_hash: content_hash(source),
        bytes: u64::try_from(source.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: Vec::with_capacity(4096),
        project_language_witnesses: Vec::with_capacity(1024),
        decisive_rule: None,
    };
    let claims =
        source_file_identity_claims(std::slice::from_ref(&input), repository, &deadline()).unwrap();
    let snapshots = BTreeMap::from([(file, snapshot)]);
    let derived = container_sources::PreparedSources::default();
    FRAME_SEALS.with(|count| count.set(0));
    let planned = PreparedSourceFrame::planned_memory_bytes(
        std::slice::from_ref(&input),
        &derived,
        &claims,
        &snapshots,
        None,
    )
    .unwrap();
    assert!(
        planned > 128 * 1024,
        "prospective accounting observes metadata capacities, not only source lengths"
    );
    FRAME_SEALS.with(|count| assert_eq!(count.get(), 0));
}

#[test]
fn canonical_shortened_origin_anchor_is_rejected_while_whole_anchor_verifies() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let source = rust_source("anchor", 32);
    fs::write(root.path().join("first.rs"), &source).unwrap();
    fs::write(root.path().join("second.rs"), rust_source("peer", 32)).unwrap();
    let module = b"Attribute VB_Name = \"Module\"\r\nPublic Sub Recover()\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("book.xlsm"),
        office_fixture::book(&[("Module", module)]),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = individual(&service, "first.rs", source, "utf-8")
        .entities
        .len();
    replace_service_ir_limits(&mut service, limits.clone());
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let verify = |document| {
        IdentityVerifiedGeneration::verify_with_source_files(
            snapshot.metadata(),
            document,
            snapshot.source_files().clone(),
            &limits,
            &service.extensions,
            &GenerationContext::new(&deadline(), GenerationBudget::default()),
        )
    };
    assert!(verify(snapshot.document().clone()).is_ok());
    let mut document = snapshot.document().clone();
    let index = document
        .extensions
        .iter()
        .position(|envelope| envelope.namespace == rootlight_ir::AGGREGATE_SOURCE_ORIGIN_NAMESPACE)
        .unwrap();
    let original = document.extensions[index].clone();
    let (derived, container) =
        rootlight_ir::decode_aggregate_source_origin_envelope(&original).unwrap();
    let whole = original.evidence.source.unwrap();
    assert!(whole.span().end_byte() > 1);
    let shortened = SourceRef::new(
        whole.repository(),
        whole.generation(),
        SourceSpan::new(whole.span().file(), 0, whole.span().end_byte() - 1).unwrap(),
        whole.content_hash(),
        None,
    );
    let altered = rootlight_ir::new_aggregate_source_origin_envelope(
        document.repository,
        document.generation,
        original.provenance,
        shortened,
        derived.clone(),
        container.clone(),
    )
    .unwrap();
    assert_eq!(
        rootlight_ir::decode_aggregate_source_origin_envelope(&altered).unwrap(),
        (derived, container),
        "the altered envelope is canonical; exact whole-anchor authentication must reject it"
    );
    document.extensions[index] = altered;
    assert!(verify(document).is_err());
}

#[test]
fn projection_labels_require_a_known_lossless_encoding_before_size_classification() {
    use crate::aggregate_source_recovery::text_search_coverage;
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        assert_eq!(
            text_search_coverage(encoding, 0, 32),
            FirstSliceTextSearchCoverage::WholeSourceProjection
        );
        assert_eq!(
            text_search_coverage(encoding, 32, 32),
            FirstSliceTextSearchCoverage::WholeSourceProjection
        );
        assert_eq!(
            text_search_coverage(encoding, 33, 32),
            FirstSliceTextSearchCoverage::MetadataOnlyCaptured
        );
    }
    for encoding in ["binary", "bytes", "unknown", "UTF-8", ""] {
        assert_eq!(
            text_search_coverage(encoding, 8, 32),
            FirstSliceTextSearchCoverage::UnsupportedEncoding
        );
        assert_eq!(
            text_search_coverage(encoding, 33, 32),
            FirstSliceTextSearchCoverage::UnsupportedEncoding
        );
    }
}

fn preserve_ordinary_capacity_case(collection: &str) {
    preserve_physical_capacity_case(collection, false);
}

fn preserve_physical_capacity_case(collection: &str, generated_culprit: bool) {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let string_case = collection == "string_bytes";
    let earlier_prefix = if string_case {
        format!("earlier_{}", "a".repeat(256))
    } else {
        "earlier".to_owned()
    };
    let culprit_prefix = if string_case {
        format!("culprit_{}", "b".repeat(2048))
    } else {
        "culprit".to_owned()
    };
    let later_prefix = if string_case {
        format!("later_{}", "c".repeat(256))
    } else {
        "later".to_owned()
    };
    let earlier_name = format!("{earlier_prefix}_0");
    let later_name = format!("{later_prefix}_0");
    let mut inputs = [
        (
            "a_earlier.rs",
            rust_source(&earlier_prefix, if string_case { 1 } else { 4 }),
        ),
        (
            "m_culprit.rs",
            rust_source(&culprit_prefix, if string_case { 1 } else { 64 }),
        ),
        (
            "z_later.rs",
            rust_source(&later_prefix, if string_case { 1 } else { 4 }),
        ),
    ];
    if generated_culprit {
        let mut generated = b"// generated file; do not edit\n".to_vec();
        generated.extend_from_slice(&inputs[1].1);
        inputs[1].1 = generated;
    }
    for (path, bytes) in &inputs {
        fs::write(root.path().join(path), bytes).unwrap();
    }
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let documents: Vec<_> = inputs
        .iter()
        .map(|(path, bytes)| individual(&service, path, bytes.clone(), "utf-8"))
        .collect();
    let mut limits = service.analysis_limits.ir().clone();
    let (counts, ceiling, cause_name) = if collection == "entities" {
        limits.max_entities = documents[1].entities.len();
        (
            documents
                .iter()
                .map(|document| document.entities.len())
                .collect::<Vec<_>>(),
            limits.max_entities,
            "resource=Entities",
        )
    } else if collection == "occurrences" {
        limits.max_occurrences = documents[1].occurrences.len();
        (
            documents
                .iter()
                .map(|document| document.occurrences.len())
                .collect::<Vec<_>>(),
            limits.max_occurrences,
            "resource=Occurrences",
        )
    } else {
        let measured: Vec<_> = documents
            .iter()
            .map(|document| {
                IrDocumentUsage::measure(document, &limits)
                    .unwrap()
                    .total_string_bytes()
            })
            .collect();
        let room = measured[0].min(measured[2]) / 2;
        limits.max_total_string_bytes = measured[1].checked_add(room).unwrap();
        assert!(
            room > 0 && measured[1] < limits.max_total_string_bytes,
            "E={} C={} L={} room={room} ceiling={}",
            measured[0],
            measured[1],
            measured[2],
            limits.max_total_string_bytes
        );
        (
            measured,
            limits.max_total_string_bytes,
            "resource=StringBytes",
        )
    };
    assert!(counts[0] + counts[2] < ceiling);
    assert!(counts[0] + counts[1] > ceiling);
    for document in &documents {
        IrDocumentUsage::measure(document, &limits).unwrap();
    }
    if string_case || generated_culprit {
        // Individual adapter usage omits service-context strings. Prove the
        // exact culprit remains analyzed by the real service under this limit.
        let isolated_root = durable_test_tempdir();
        fs::write(isolated_root.path().join(inputs[1].0), &inputs[1].1).unwrap();
        let mut isolated_service = FirstSliceService::new(2).unwrap();
        replace_service_ir_limits(&mut isolated_service, limits.clone());
        let isolated_receipt = isolated_service
            .index_repository(isolated_root.path(), &deadline())
            .expect(
                "the exact single culprit must be service-admissible before the aggregate case",
            );
        let isolated_snapshot = isolated_service
            .loaded_generation_snapshot(isolated_receipt.generation)
            .unwrap();
        assert_eq!(isolated_snapshot.file_count(), 1);
        assert_eq!(
            isolated_snapshot.document().files[0].generated,
            generated_culprit,
            "the isolated real-service prerequisite must observe the exact classification"
        );
        assert!(
            isolated_snapshot
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == format!("{culprit_prefix}_0")),
            "the single-file prerequisite must retain its actual definition, not source-only fallback"
        );
        assert_exact(
            &isolated_service,
            &isolated_receipt,
            inputs[1].0,
            &inputs[1].1,
        );
    }
    replace_service_ir_limits(&mut service, limits);
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(snapshot.file_count(), 3);
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == earlier_name)
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == later_name)
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name.starts_with("culprit_"))
    );
    let culprit = document
        .files
        .iter()
        .find(|file| file.path == "m_culprit.rs")
        .unwrap();
    assert_eq!(culprit.generated, generated_culprit);
    let provenance = document
        .provenance
        .iter()
        .find(|record| record.id == culprit.provenance)
        .unwrap();
    assert_eq!(provenance.rule.as_deref(), Some("aggregate-file-capacity"));
    assert_eq!(
        provenance.input_sources,
        vec![culprit.evidence.source.clone().unwrap()]
    );
    assert_eq!(provenance.evidence_sources, provenance.input_sources);
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.source.span().file() == culprit.id
                && gap.detail.starts_with("aggregate-file-capacity: ")
        })
        .collect();
    assert_eq!(gaps.len(), 3);
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        assert!(gaps.iter().any(|gap| gap.domain == domain
            && gap.reason == SkippedRegionReason::ResourceLimit
            && gap.source.span().start_byte() == 0
            && gap.source.span().end_byte() == culprit.byte_length
            && gap.source.content_hash() == culprit.content_hash
            && gap.detail.contains(cause_name)));
        assert!(
            document
                .coverage_records
                .iter()
                .any(|coverage| coverage.scope == CoverageScope::File(culprit.id)
                    && coverage.domain == domain
                    && coverage.status == CoverageStatus::Unknown)
        );
    }
    assert!(
        document
            .coverage_records
            .iter()
            .any(|coverage| coverage.scope == CoverageScope::File(culprit.id)
                && coverage.domain == IrFactDomain::Diagnostics
                && coverage.status == CoverageStatus::Unknown)
    );
    assert!(
        !document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "aggregate-file-capacity")
    );
    assert!(
        !document
            .extensions
            .iter()
            .any(|extension| extension.namespace
                == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE)
    );
    for (path, bytes) in &inputs {
        assert_exact(&service, &receipt, path, bytes);
    }
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
    let generation = receipt.generation;
    let noop = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(noop.generation, generation);
    drop(service);
    let reopened = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    for (path, bytes) in &inputs {
        assert_exact(&reopened, &receipt, path, bytes);
    }
    let reopened_snapshot = reopened.loaded_generation_snapshot(generation).unwrap();
    assert!(
        reopened_snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == earlier_name)
    );
    assert!(
        reopened_snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == later_name)
    );
}

#[test]
fn ordinary_entity_capacity_preserves_earlier_and_later_file_facts() {
    preserve_ordinary_capacity_case("entities");
}

#[test]
fn ordinary_occurrence_capacity_preserves_earlier_and_later_file_facts() {
    preserve_ordinary_capacity_case("occurrences");
}

#[test]
fn ordinary_string_capacity_preserves_earlier_and_later_file_facts() {
    preserve_ordinary_capacity_case("string_bytes");
}

#[test]
fn unavailable_metadata_reservation_preserves_strict_original_pipeline() {
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let first = rust_source("first", 64);
    let second = rust_source("second", 64);
    fs::write(root.path().join("first.rs"), &first).unwrap();
    fs::write(root.path().join("second.rs"), &second).unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = individual(&service, "first.rs", first.clone(), "utf-8")
        .entities
        .len();
    // A per-file disposition requires three exact gap records. The unchanged
    // universal fallback remains necessary when this mandatory floor cannot fit.
    limits.max_skipped_regions = 2;
    replace_service_ir_limits(&mut service, limits);
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(snapshot.document().entities.is_empty());
    assert!(
        snapshot
            .document()
            .extensions
            .iter()
            .any(|extension| extension.namespace
                == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE)
    );
    assert_exact(&service, &receipt, "first.rs", &first);
    assert_exact(&service, &receipt, "second.rs", &second);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn aggregate_omission_withholds_a_generic_competing_declaration() {
    fn measured_typescript(
        service: &FirstSliceService,
        path: &str,
        bytes: Vec<u8>,
    ) -> NormalizedIrDocument {
        let repository = derive_repository(b"aggregate-typescript-measurement").id();
        let generation = GenerationId::from_bytes([97; 20]);
        let path = RelativePath::parse(Path::new(path)).unwrap();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        let snapshot =
            SourceSnapshot::from_persisted(repository, path, file, content_hash(&bytes), bytes)
                .unwrap();
        let request = structural_analysis_request(
            repository,
            generation,
            &snapshot,
            false,
            "typescript",
            "utf-8",
            &service.analysis_limits,
        )
        .unwrap();
        let artifact = service.analyzers["typescript"]
            .prepare_artifact(&request, &deadline())
            .unwrap();
        service.analyzers["typescript"]
            .analyze_prepared_artifact(
                &request,
                &artifact,
                service.extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap()
            .document()
            .clone()
    }
    let (_storage, paths) = paths();
    let root = durable_test_tempdir();
    let target = b"function selectTarget() {}\n";
    let caller = b"function invoke() { selectTarget(); }\n";
    fs::write(root.path().join("a_target.ts"), target).unwrap();
    fs::write(root.path().join("b_caller.ts"), caller).unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let first = service.index_repository(root.path(), &deadline()).unwrap();
    let caller_file = assert_exact(&service, &first, "b_caller.ts", caller)
        .span()
        .file();
    let snapshot = service
        .loaded_generation_snapshot(first.generation)
        .unwrap();
    let first_call = snapshot
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.file == caller_file
                && site.role == OccurrenceRole::CallSite
                && site.syntactic_text_hash == content_hash(b"selectTarget")
        })
        .unwrap();
    assert!(
        matches!(&first_call.target, OccurrenceTarget::Candidates {
        symbols, total_count: 1, completeness: CoverageStatus::Complete,
    } if symbols.len() == 1),
        "baseline must exercise actual generic selection"
    );
    let mut missing = b"function selectTarget() {}\n".to_vec();
    for index in 0..64 {
        missing.extend_from_slice(format!("function padding{index}() {{}}\n").as_bytes());
    }
    let fragment = measured_typescript(&service, "m_competing.ts", missing.clone());
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_entities = fragment.entities.len();
    assert!(snapshot.document().entities.len() < limits.max_entities);
    assert!(snapshot.document().entities.len() + fragment.entities.len() > limits.max_entities);
    IrDocumentUsage::measure(&fragment, &limits).unwrap();
    drop(snapshot);
    replace_service_ir_limits(&mut service, limits);
    fs::write(root.path().join("m_competing.ts"), &missing).unwrap();
    let second = service.index_repository(root.path(), &deadline()).unwrap();
    let missing_source = assert_exact(&service, &second, "m_competing.ts", &missing);
    assert_exact(&service, &second, "a_target.ts", target);
    assert_exact(&service, &second, "b_caller.ts", caller);
    let snapshot = service
        .loaded_generation_snapshot(second.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source == missing_source
                && gap.detail.starts_with("aggregate-file-capacity: "))
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "selectTarget")
            .count(),
        1
    );
    let call = document
        .occurrences
        .iter()
        .find(|site| {
            site.file == caller_file
                && site.role == OccurrenceRole::CallSite
                && site.syntactic_text_hash == content_hash(b"selectTarget")
        })
        .unwrap();
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(call.source.content_hash(), content_hash(caller));
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

struct OptionalAppendFixture {
    sources: Vec<RustSourceInput>,
    documents: Vec<NormalizedIrDocument>,
    limits: IrLimits,
}

fn optional_append_fixture(service: &FirstSliceService) -> OptionalAppendFixture {
    let mut sources = Vec::new();
    let mut documents = Vec::new();
    for index in 0..3 {
        let path = format!("part_{index}.rs");
        let bytes = rust_source(&format!("native_{index}"), 16);
        let mut document = individual(service, &path, bytes.clone(), "utf-8");
        let file = document.files[0].clone();
        let source = file.evidence.source.clone().unwrap();
        assert_eq!(source.content_hash(), content_hash(&bytes));
        for ordinal in 0..3 {
            let mut diagnostic = DiagnosticRecord {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                code: format!("authored-optional-warning-{ordinal}"),
                message: format!("authored bounded diagnostic {ordinal}"),
                severity: DiagnosticSeverity::Warning,
                source: Some(source.clone()),
                coverage_effect: CoverageStatus::Bounded,
                provenance: file.provenance,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            diagnostic.id = derive_diagnostic_record_id(&diagnostic).unwrap();
            document.diagnostics.push(diagnostic);
        }
        for ordinal in 0..4 {
            let mut gap = rootlight_ir::SkippedRegion {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                source: source.clone(),
                domain: IrFactDomain::Relations,
                reason: SkippedRegionReason::UnsupportedConstruct,
                detail: format!("authored optional relation detail {ordinal}"),
                provenance: file.provenance,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            gap.id = derive_skipped_region_id(&gap).unwrap();
            document.skipped_regions.push(gap);
        }
        for coverage in &mut document.coverage_records {
            if coverage.scope != CoverageScope::File(file.id)
                || coverage.provenance != file.provenance
            {
                continue;
            }
            if coverage.domain == IrFactDomain::Diagnostics {
                coverage.discovered += 3;
                coverage.indexed += 3;
            } else if coverage.domain == IrFactDomain::Relations {
                coverage.discovered += 4;
                coverage.skipped += 4;
                if coverage.status == CoverageStatus::Complete {
                    coverage.status = CoverageStatus::Bounded;
                }
            }
            coverage.id = derive_coverage_record_id(coverage).unwrap();
        }
        rootlight_ir::validate_ir_document(
            &document,
            service.analysis_limits.ir(),
            &service.extensions,
        )
        .unwrap();
        let path = RelativePath::parse(Path::new(&path)).unwrap();
        sources.push(RustSourceInput {
            snapshot: SourceSnapshot::from_persisted(
                document.repository,
                path,
                file.id,
                content_hash(&bytes),
                bytes,
            )
            .unwrap(),
            encoding: "utf-8".to_owned(),
            generated: false,
            origins: Vec::new(),
        });
        documents.push(document);
    }
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_diagnostics = 3;
    limits.max_skipped_regions = 9;
    let required: Vec<_> = documents
        .iter()
        .map(|document| required_extension_usage(&document.extensions, &deadline()).unwrap())
        .collect();
    let required_count = required.iter().map(|usage| usage.0).sum::<usize>();
    let required_bytes = required.iter().map(|usage| usage.1).sum::<usize>();
    let single_count = documents
        .iter()
        .map(|document| document.extensions.len())
        .max()
        .unwrap();
    let raw_count = documents
        .iter()
        .map(|document| document.extensions.len())
        .sum::<usize>();
    let raw_bytes: Vec<_> = documents
        .iter()
        .map(|document| {
            IrDocumentUsage::measure(document, service.analysis_limits.ir())
                .unwrap()
                .total_extension_bytes()
        })
        .collect();
    let single_bytes = *raw_bytes.iter().max().unwrap();
    let total_raw_bytes = raw_bytes.iter().sum::<usize>();
    // The real append packs lexical evidence before optional retention. Reserve
    // one genuine packed envelope, then force truncation beyond that boundary.
    let mut compacted = documents.clone();
    for document in &mut compacted {
        compact_lexical_summaries(document, service.analysis_limits.ir()).unwrap();
    }
    let compacted_count = compacted
        .iter()
        .map(|document| document.extensions.len())
        .sum::<usize>();
    let compacted_bytes = compacted
        .iter()
        .map(|document| {
            IrDocumentUsage::measure(document, service.analysis_limits.ir())
                .unwrap()
                .total_extension_bytes()
        })
        .sum::<usize>();
    let optional_room = compacted
        .iter()
        .flat_map(|document| &document.extensions)
        .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
        .map(|extension| extension.payload.len())
        .max()
        .expect("native lexical evidence");
    limits.max_extensions = single_count.max(required_count.checked_add(1).unwrap());
    limits.max_total_extension_bytes =
        single_bytes.max(required_bytes.checked_add(optional_room).unwrap());
    assert!(
        required_count < limits.max_extensions,
        "count R={required_count} S={single_count} T={raw_count} P={compacted_count} ceiling={}",
        limits.max_extensions
    );
    assert!(
        limits.max_extensions < raw_count && limits.max_extensions < compacted_count,
        "count R={required_count} S={single_count} T={raw_count} P={compacted_count} ceiling={}",
        limits.max_extensions
    );
    assert!(
        optional_room > 0 && required_bytes < limits.max_total_extension_bytes,
        "bytes R={required_bytes} S={single_bytes} T={total_raw_bytes} P={compacted_bytes} room={optional_room} ceiling={}",
        limits.max_total_extension_bytes
    );
    assert!(
        limits.max_total_extension_bytes < total_raw_bytes
            && limits.max_total_extension_bytes < compacted_bytes,
        "bytes R={required_bytes} S={single_bytes} T={total_raw_bytes} P={compacted_bytes} room={optional_room} ceiling={}",
        limits.max_total_extension_bytes
    );
    for document in &documents {
        IrDocumentUsage::measure(document, &limits).unwrap_or_else(|error| panic!(
            "single raw fragment must fit: count R={required_count} S={single_count} T={raw_count} P={compacted_count}; bytes R={required_bytes} S={single_bytes} T={total_raw_bytes} P={compacted_bytes}; {error:?}"
        ));
    }
    OptionalAppendFixture {
        sources,
        documents,
        limits,
    }
}

fn assert_optional_append_facts(
    fixture: &OptionalAppendFixture,
    document: &NormalizedIrDocument,
    state: &DocumentAppendState,
) {
    let expected_entities: BTreeSet<_> = fixture
        .documents
        .iter()
        .flat_map(|source| source.entities.iter().map(|entity| entity.id))
        .collect();
    let expected_occurrences: BTreeSet<_> = fixture
        .documents
        .iter()
        .flat_map(|source| source.occurrences.iter().map(|site| site.id))
        .collect();
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        expected_entities
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .map(|site| site.id)
            .collect::<BTreeSet<_>>(),
        expected_occurrences
    );
    assert!(state.truncated_diagnostics > 0);
    assert!(state.truncated_extensions > 0);
    assert!(state.truncated_skipped_regions > 0);
    assert!(document.diagnostics.len() <= fixture.limits.max_diagnostics);
    assert!(document.skipped_regions.len() <= fixture.limits.max_skipped_regions);
    assert!(document.extensions.len() <= fixture.limits.max_extensions);
    assert!(
        !document
            .extensions
            .iter()
            .any(|extension| extension.namespace
                == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE)
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("aggregate-file-capacity: "))
    );
    for input in &fixture.sources {
        let file = document
            .files
            .iter()
            .find(|file| file.id == input.snapshot.file())
            .unwrap();
        let source = file.evidence.source.as_ref().unwrap();
        assert_eq!(file.content_hash, content_hash(input.snapshot.content()));
        assert_eq!(
            file.byte_length,
            u64::try_from(input.snapshot.content().len()).unwrap()
        );
        assert_eq!(source.span().start_byte(), 0);
        assert_eq!(source.span().end_byte(), file.byte_length);
    }
    rootlight_ir::validate_ir_document(document, &fixture.limits, &ExtensionSupport::default())
        .unwrap();
}

#[test]
fn existing_optional_heavy_append_remains_healthy_without_reservation() {
    let service = FirstSliceService::new(2).unwrap();
    let fixture = optional_append_fixture(&service);
    let mut document = NormalizedIrDocument::empty(
        fixture.documents[0].repository,
        fixture.documents[0].generation,
    );
    let mut state = DocumentAppendState::from_document(&document).unwrap();
    state.aggregate_recovery = true;
    for source in &fixture.documents {
        append_normalized_document(&mut document, source.clone(), &fixture.limits, &mut state)
            .unwrap();
    }
    assert_optional_append_facts(&fixture, &document, &state);
}

#[test]
fn reserved_optional_heavy_append_preserves_the_healthy_native_baseline() {
    let service = FirstSliceService::new(2).unwrap();
    let fixture = optional_append_fixture(&service);
    let repository = fixture.documents[0].repository;
    let generation = fixture.documents[0].generation;
    let languages = fixture
        .sources
        .iter()
        .map(|input| (input.snapshot.file(), "rust".to_owned()))
        .collect();
    let mut floor = ensure_generation_memory_preflight(
        fixture
            .sources
            .iter()
            .map(|input| input.snapshot.metadata().length)
            .sum(),
    )
    .unwrap();
    let mut memory = service
        .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
        .unwrap();
    let mut reservation = crate::aggregate_file_preservation::Reservation::new(
        repository,
        generation,
        &fixture.sources,
        &languages,
        &BTreeMap::new(),
        &fixture.limits,
        &mut floor,
        &mut memory,
        false,
        &deadline(),
    )
    .unwrap()
    .unwrap();
    let mut document = NormalizedIrDocument::empty(repository, generation);
    let mut state = DocumentAppendState::from_document(&document).unwrap();
    state.aggregate_recovery = true;
    let mut contextual = false;
    for (input, source) in fixture.sources.iter().zip(&fixture.documents) {
        let (prepared, replaced) = reservation
            .choose(
                &document,
                &state,
                input,
                "rust",
                source.clone(),
                &fixture.limits,
                &deadline(),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert!(!replaced);
        contextual |= !prepared.context_free_source;
        // The same actual planned records must fit with every still-required
        // source disposition. No pre-compaction or approximate row count passes.
        prepared
            .target_usage
            .clone()
            .checked_add(prepared.source_usage.clone(), &fixture.limits)
            .unwrap()
            .checked_add(reservation.remaining().clone(), &fixture.limits)
            .unwrap();
        commit_prepared_document_append(
            &mut document,
            prepared,
            &fixture.limits,
            &mut state,
            &deadline(),
        )
        .unwrap();
    }
    reservation.finish().unwrap();
    assert_eq!(reservation.remaining().total_records(), 0);
    assert!(!reservation.unavailable());
    assert!(
        contextual,
        "target-dependent compaction cannot become a reusable raw chunk"
    );
    assert_optional_append_facts(&fixture, &document, &state);
    drop(reservation);
    drop(memory);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn rejected_owned_optional_plan_leaves_target_and_cached_state_unchanged() {
    let service = FirstSliceService::new(2).unwrap();
    let fixture = optional_append_fixture(&service);
    let mut document = NormalizedIrDocument::empty(
        fixture.documents[0].repository,
        fixture.documents[0].generation,
    );
    let mut state = DocumentAppendState::from_document(&document).unwrap();
    state.aggregate_recovery = true;
    append_normalized_document(
        &mut document,
        fixture.documents[0].clone(),
        &fixture.limits,
        &mut state,
    )
    .unwrap();
    let before = document.clone();
    let before_state = state.clone();
    let mut limits = fixture.limits.clone();
    limits.max_entities = fixture.documents[1].entities.len();
    let failure = prepare_normalized_document_append(
        &document,
        fixture.documents[1].clone(),
        &limits,
        &state,
        &deadline(),
        None,
        None,
    );
    assert!(
        matches!(failure, Err(FirstSliceError::AggregateCapacity(cause)) if cause.is_structural_append())
    );
    assert_eq!(document, before);
    assert_eq!(state, before_state);
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        prepare_normalized_document_append(
            &document,
            fixture.documents[1].clone(),
            &fixture.limits,
            &state,
            &cancelled,
            None,
            None
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(document, before);
    assert_eq!(state, before_state);
}

#[test]
fn compacted_reused_metadata_counts_only_original_ids() {
    let service = FirstSliceService::new(2).unwrap();
    let fixture = optional_append_fixture(&service);
    let generation = fixture.documents[0].generation;
    let floor = ensure_generation_memory_preflight(
        fixture
            .sources
            .iter()
            .map(|input| input.snapshot.metadata().length)
            .sum(),
    )
    .unwrap();
    let mut memory = service
        .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
        .unwrap();
    let mut admission = generation_admission::GenerationAdmission::new(floor).unwrap();
    let mut document = NormalizedIrDocument::empty(fixture.documents[0].repository, generation);
    let mut state = DocumentAppendState::from_document(&document).unwrap();
    state.aggregate_recovery = true;
    let mut raw_reuse = ReusedNormalizedRecords::default();
    let mut selected_reuse = ReusedNormalizedRecords::default();
    let mut changed_coverage = 0;
    let mut packed_extensions = 0;
    let mut retained_metadata = 0;
    for source in &fixture.documents {
        raw_reuse.insert_document(source).unwrap();
        let witness = crate::aggregate_file_preservation::RawMetadataWitness::capture(
            source,
            &deadline(),
            &mut |bytes| admission.admit_known_bytes(bytes, &mut memory, false, &deadline()),
        )
        .unwrap();
        let prepared = prepare_normalized_document_append(
            &document,
            source.clone(),
            &fixture.limits,
            &state,
            &deadline(),
            None,
            None,
        )
        .unwrap();
        selected_reuse
            .insert_document_with_metadata_origin(&prepared.source, Some(&witness))
            .unwrap();
        for coverage in &prepared.source.coverage_records {
            let original = witness.contains(coverage.id);
            assert_eq!(selected_reuse.contains_coverage(coverage), original);
            changed_coverage += usize::from(!original);
            retained_metadata += usize::from(original);
        }
        for extension in &prepared.source.extensions {
            let original = witness.contains(extension.id);
            assert_eq!(selected_reuse.contains_extension(extension), original);
            packed_extensions += usize::from(!original);
            retained_metadata += usize::from(original);
        }
        assert!(
            prepared
                .source
                .entities
                .iter()
                .all(|record| selected_reuse.contains_entity(record))
        );
        assert!(
            prepared
                .source
                .occurrences
                .iter()
                .all(|record| selected_reuse.contains_occurrence(record))
        );
        drop(witness);
        commit_prepared_document_append(
            &mut document,
            prepared,
            &fixture.limits,
            &mut state,
            &deadline(),
        )
        .unwrap();
    }
    assert!(
        changed_coverage > 0,
        "real optional truncation adjusts coverage IDs"
    );
    assert!(
        packed_extensions > 0,
        "real lexical packing produces new extension IDs"
    );
    assert!(retained_metadata > 0, "unchanged metadata remains reused");
    assert_eq!(
        selected_reuse.retained_count(&document).unwrap(),
        raw_reuse.retained_count(&document).unwrap()
    );
    assert_optional_append_facts(&fixture, &document, &state);

    let before = selected_reuse.retained_count(&document).unwrap();
    let rejected_source = individual(
        &service,
        "rejected_peer.rs",
        rust_source("rejected_peer", 1),
        "utf-8",
    );
    assert!(rejected_source.files.iter().all(|source| {
        !document
            .files
            .iter()
            .any(|retained| retained.id == source.id)
    }));
    assert!(!rejected_source.entities.is_empty());
    let rejected_witness = crate::aggregate_file_preservation::RawMetadataWitness::capture(
        &rejected_source,
        &deadline(),
        &mut |bytes| admission.admit_known_bytes(bytes, &mut memory, false, &deadline()),
    )
    .unwrap();
    // Default optional limits isolate the configured entity append ceiling from
    // the earlier fixture's intentional metadata pressure and duplicate IDs.
    let mut rejecting_limits = service.analysis_limits.ir().clone();
    rejecting_limits.max_entities = document.entities.len();
    IrDocumentUsage::measure(&rejected_source, &rejecting_limits).unwrap();
    assert!(
        document.entities.len() + rejected_source.entities.len() > rejecting_limits.max_entities
    );
    let failure = prepare_normalized_document_append(
        &document,
        rejected_source,
        &rejecting_limits,
        &state,
        &deadline(),
        None,
        None,
    )
    .err()
    .expect("the independent source must exceed only the combined entity ceiling");
    assert!(
        matches!(failure, FirstSliceError::AggregateCapacity(cause) if cause.is_structural_append()
        && cause.source_disposition_detail().unwrap().contains("resource=Entities")),
        "expected genuine entity append refusal, observed {failure:?}"
    );
    drop(rejected_witness);
    assert_eq!(selected_reuse.retained_count(&document).unwrap(), before);

    let mut denied_admissions = 0;
    assert!(matches!(
        crate::aggregate_file_preservation::RawMetadataWitness::capture(
            &fixture.documents[0],
            &deadline(),
            &mut |_| {
                denied_admissions += 1;
                Err(FirstSliceError::ResourceUnavailable {
                    resource: FirstSliceResource::MemoryBytes,
                })
            },
        ),
        Err(FirstSliceError::ResourceUnavailable {
            resource: FirstSliceResource::MemoryBytes
        })
    ));
    assert_eq!(denied_admissions, 1);
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        crate::aggregate_file_preservation::RawMetadataWitness::capture(
            &fixture.documents[0],
            &cancelled,
            &mut |_| {
                denied_admissions += 1;
                Ok(())
            },
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(denied_admissions, 1);
    assert_eq!(selected_reuse.retained_count(&document).unwrap(), before);
    assert!(admission.charged_bytes() > 0);
    // Staging allowances are intentionally monotonic, even after a witness drops.
    // The existing reservation guard releases them when this preparation ends.
    drop(memory);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn generated_physical_entity_capacity_preserves_ordinary_neighbors_and_exact_source() {
    preserve_physical_capacity_case("entities", true);
}

#[test]
fn generated_physical_occurrence_capacity_preserves_ordinary_neighbors_and_exact_source() {
    preserve_physical_capacity_case("occurrences", true);
}

#[test]
fn generated_physical_string_capacity_preserves_ordinary_neighbors_and_exact_source() {
    preserve_physical_capacity_case("string_bytes", true);
}
