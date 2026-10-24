//! Behavioral controls for owned resolver rollback and strict failure classes.
//! Small normalized compatibility input keeps mutation ownership visible;
//! lifecycle restore-ceiling tests exercise this checkpoint through the service.

use std::cell::Cell;

use rootlight_ir::derive_occurrence_record_id;

use super::*;

thread_local! {
    pub(super) static FAIL_NEXT_ALLOCATION: Cell<bool> = const { Cell::new(false) };
    static COST: Cell<(u64, u64, usize)> = const { Cell::new((0, 0, 0)) };
}

pub(super) fn observe_cost(predicted: u64, actual: u64, fingerprints: usize) {
    COST.with(|value| value.set((predicted, actual, fingerprints)));
}

fn document() -> NormalizedIrDocument {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/compatibility/ir/1.1/document.json"
    ));
    let rootlight_ir::IrDocument::NormalizedV1_1(document) =
        rootlight_ir::decode_ir_document(bytes, &IrLimits::default(), &ExtensionSupport::default())
            .unwrap()
    else {
        panic!("normalized fixture required")
    };
    rootlight_ir::canonicalize_ir_document(
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap()
}

#[test]
fn owned_mutable_collections_and_append_only_provenance_restore_exact_input() {
    let original = document();
    let cancellation = Cancellation::new();
    let mut charged = 0;
    let undo = Undo::capture(&original, &cancellation, &mut |bytes| {
        charged += bytes;
        Ok(())
    })
    .unwrap();
    assert!(charged >= undo.retained_bytes().unwrap());
    let (predicted, actual, fingerprints) = COST.with(Cell::get);
    assert!(charged >= predicted && charged >= actual && fingerprints > 0);
    eprintln!(
        "resolver undo cost: predicted_bytes={predicted} actual_bytes={actual} fingerprints={fingerprints} admitted_bytes={charged}"
    );
    let mut candidate = original.clone();
    let mut added = candidate.provenance[0].clone();
    added.id = FactId::from_bytes([0x39; 20]);
    added.rule = Some("resolver-undo-control".into());
    candidate.occurrences[0].role = OccurrenceRole::Reference;
    candidate.occurrences[0].target = OccurrenceTarget::Unresolved {
        text_hash: content_hash(b"unresolved"),
    };
    candidate.occurrences[0].provenance = added.id;
    candidate.occurrences[0].id = derive_occurrence_record_id(&candidate.occurrences[0]).unwrap();
    candidate.relations[0].confidence = rootlight_ir::Confidence::new(500).unwrap();
    candidate.relations[0].id = derive_relation_record_id(&candidate.relations[0]).unwrap();
    candidate.coverage_records[0].status = CoverageStatus::Bounded;
    candidate.coverage_records[0].id =
        derive_coverage_record_id(&candidate.coverage_records[0]).unwrap();
    candidate.skipped_regions[0].detail = "resolver-adjusted-gap".into();
    candidate.skipped_regions[0].id =
        derive_skipped_region_id(&candidate.skipped_regions[0]).unwrap();
    candidate.diagnostics[0].message = "resolver-adjusted-diagnostic".into();
    candidate.diagnostics[0].id = derive_diagnostic_record_id(&candidate.diagnostics[0]).unwrap();
    candidate.provenance.push(added);
    let restored = undo.restore(candidate, &cancellation).unwrap();
    assert_eq!(restored, original);
}

#[test]
fn changed_or_missing_unowned_records_never_authorize_capacity_rollback() {
    let original = document();
    let cancellation = Cancellation::new();
    for mode in 0..5 {
        let undo = Undo::capture(&original, &cancellation, &mut |_| Ok(())).unwrap();
        let mut candidate = original.clone();
        match mode {
            0 => candidate.files[0].language = "changed".into(),
            1 => candidate.entities[0].display_name = "changed".into(),
            2 => candidate.provenance[0].rule = Some("changed".into()),
            3 => candidate.source_mappings[0]
                .evidence
                .derivation
                .push(FactRef::File(candidate.files[0].id)),
            4 => candidate.extensions[0].payload.push(' '),
            _ => unreachable!(),
        }
        assert!(
            matches!(
                undo.restore(candidate, &cancellation),
                Err(FirstSliceError::Identity)
            ),
            "mode={mode}"
        );
    }
}

#[test]
fn cancellation_workspace_refusal_and_allocation_failure_keep_their_actual_error_class() {
    let original = document();
    let cancellation = Cancellation::new();
    let mut required = 0_u64;
    let measured = Undo::capture(&original, &cancellation, &mut |bytes| {
        required = required.checked_add(bytes).ok_or(FirstSliceError::Limits)?;
        Ok(())
    })
    .unwrap();
    let actual_owned = measured.retained_bytes().unwrap();
    assert!(actual_owned > 0 && required >= actual_owned);
    drop(measured);
    let capture_bounded = |limit: u64| {
        let mut charged = 0_u64;
        let result = Undo::capture(&original, &cancellation, &mut |bytes| {
            let next = charged.checked_add(bytes).ok_or(FirstSliceError::Limits)?;
            if next > limit {
                let mut error = generation_memory_limit(0, next, PendingGenerationMemory::Staged);
                if let FirstSliceError::GenerationMemoryLimit { limit: actual, .. } = &mut error {
                    *actual = limit;
                }
                return Err(error);
            }
            charged = next;
            Ok(())
        });
        (result, charged)
    };
    let (exact, charged) = capture_bounded(required);
    assert_eq!(charged, required);
    assert_eq!(
        exact
            .unwrap()
            .restore(original.clone(), &cancellation)
            .unwrap(),
        original
    );
    let (below, _) = capture_bounded(required - 1);
    assert!(
        matches!(below, Err(FirstSliceError::GenerationMemoryLimit { observed, limit, .. })
        if observed > limit && limit == required - 1)
    );
    assert_eq!(document(), original);
    eprintln!(
        "resolver undo admission boundary: required_bytes={required} actual_owned_bytes={actual_owned} below_limit_bytes={}",
        required - 1
    );

    let refusal = generation_memory_limit(0, 100, PendingGenerationMemory::Staged);
    assert_eq!(
        Undo::capture(&original, &cancellation, &mut |_| Err(refusal)).err(),
        Some(refusal)
    );
    FAIL_NEXT_ALLOCATION.with(|slot| slot.set(true));
    assert_eq!(
        Undo::capture(&original, &cancellation, &mut |_| Ok(())).err(),
        Some(unavailable())
    );
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest));
    let called = Cell::new(false);
    assert!(matches!(
        Undo::capture(&original, &cancelled, &mut |_| {
            called.set(true);
            Ok(())
        }),
        Err(FirstSliceError::Cancelled(_))
    ));
    assert!(!called.get());
    assert_eq!(document(), original);
}

#[test]
fn a_real_bounded_resolver_candidate_can_be_discarded_with_original_facts_retained() {
    let mut original = document();
    let cancellation = Cancellation::new();
    let mut site = original.occurrences[0].clone();
    site.role = OccurrenceRole::CallSite;
    site.target = OccurrenceTarget::Unresolved {
        text_hash: content_hash(original.entities[0].canonical_name.as_bytes()),
    };
    site.syntactic_text_hash = content_hash(original.entities[0].canonical_name.as_bytes());
    site.id = derive_occurrence_record_id(&site).unwrap();
    original.occurrences.push(site);
    original = rootlight_ir::canonicalize_ir_document(
        original,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let undo = Undo::capture(&original, &cancellation, &mut |_| Ok(())).unwrap();
    let (candidate, _) = ResolutionEngine::new(
        resolution_limits_for_occurrences(original.occurrences.len()).unwrap(),
    )
    .apply_document_bounded(
        original.clone(),
        ResolverFactContext::new(content_hash(RESOLVER_BINARY_SEED)),
        &cancellation,
    )
    .unwrap();
    assert_ne!(
        candidate.occurrences, original.occurrences,
        "fixture must exercise real application growth"
    );
    let restored = undo.restore(candidate, &cancellation).unwrap();
    assert_eq!(restored, original);
}
