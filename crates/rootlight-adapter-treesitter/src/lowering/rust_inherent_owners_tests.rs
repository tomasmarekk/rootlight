//! Native owner staging controls exercise actual identity materialization.
//! Source proof qualification remains covered by the shared and analyzer suites.

use super::*;
use rootlight_adapter_sdk::{RustInherentOwnerPlan, RustInherentOwnerProof};
use rootlight_ids::{FileId, GenerationId, RepositoryId};
use rootlight_ir::BuildContextIdentity;

fn draft(
    local: u64,
    kind: EntityKind,
    name: &str,
    span: SourceSpan,
    definition: u64,
) -> EntityDraft {
    EntityDraft {
        local_id: local,
        parent_entity: None,
        lexical_module: None,
        scope_identity: (kind == EntityKind::Method).then_some([9; 32]),
        data_identity: None,
        data_root_entity: None,
        data_qualified_name: None,
        scope_collision_guard: None,
        qualified_prefix: None,
        synthetic: false,
        is_test: false,
        definition_local_id: Some(definition),
        span,
        depth: 1,
        kind,
        name: name.to_owned(),
        authored_display_span: None,
        signature: String::new(),
        signature_evidence: None,
        signature_span: None,
        language: "rust".to_owned(),
        qualified_length: name.len(),
    }
}

fn fixture() -> (
    SourceRef,
    Vec<EntityDraft>,
    HashMap<u64, MaterializedEntity>,
    RustInherentOwnerPlan,
) {
    let text = "struct Owner {} impl Owner { fn run() {} }";
    let file = FileId::from_bytes([3; 20]);
    let source = SourceRef::new(
        RepositoryId::from_bytes([1; 16]),
        GenerationId::from_bytes([2; 20]),
        SourceSpan::new(file, 0, text.len() as u64).unwrap(),
        content_hash(text.as_bytes()),
        None,
    );
    let span = |start: usize, end: usize| SourceSpan::new(file, start as u64, end as u64).unwrap();
    let method_start = text.find("fn run").unwrap();
    let drafts = vec![
        draft(
            1,
            EntityKind::Struct,
            "Owner",
            span(0, "struct Owner {}".len()),
            101,
        ),
        draft(
            2,
            EntityKind::Method,
            "run",
            span(method_start, method_start + "fn run() {}".len()),
            102,
        ),
    ];
    let cancellation = Cancellation::new();
    let view = NativeSourceView::new(text.as_bytes(), "utf-8", 1024, 1024, &cancellation).unwrap();
    let coordinates = SourceCoordinates::new(&view, &[], &[], 1024, &cancellation).unwrap();
    let mut entities = HashMap::new();
    for draft in &drafts {
        let entity = materialize_entity(
            draft,
            &source,
            &coordinates,
            None,
            BuildContextIdentity::new(content_hash(b"context")),
            FactId::from_bytes([4; 20]),
            AnalysisTier::TierB,
            1024,
            &entities,
        )
        .unwrap();
        entities.insert(draft.local_id, entity);
    }
    let plan = RustInherentOwnerPlan {
        source: source.clone(),
        proofs: vec![RustInherentOwnerProof {
            method: 2,
            definition: 102,
            owner: 1,
            owner_definition: 101,
            lexical_module: None,
        }],
        fields: Vec::new(),
        unproven_methods: Vec::new(),
    };
    (source, drafts, entities, plan)
}

fn unchanged(before: &HashMap<u64, MaterializedEntity>, after: &HashMap<u64, MaterializedEntity>) {
    assert_eq!(before.len(), after.len());
    for (local, old) in before {
        let current = &after[local];
        assert_eq!(old.record, current.record);
        assert_eq!(old.identity_claim, current.identity_claim);
        assert_eq!(old.direct_parent, current.direct_parent);
    }
}

#[test]
fn combined_resident_gap_plan_and_optional_bounds_are_exact_and_one_less() {
    for admitted in [false, true] {
        let (source, drafts, mut entities, plan) = fixture();
        let old = entities.clone();
        let cancellation = Cancellation::new();
        let resident = 4096;
        let (gaps, base) = required_gaps(
            &drafts,
            &entities,
            resident,
            usize::MAX,
            &Allocation::default(),
            &cancellation,
        )
        .unwrap();
        let held = slots(plan.proofs.capacity(), size_of::<RustInherentOwnerProof>()).unwrap();
        let required = add(
            add(base, held).unwrap(),
            optional_bytes(drafts.len(), entities.len(), plan.proofs.len()).unwrap(),
        )
        .unwrap();
        let maximum = required - usize::from(!admitted);
        let result = consume_plan(
            &mut entities,
            &drafts,
            &source,
            plan,
            gaps,
            resident,
            base + held,
            maximum,
            &Allocation::default(),
            &cancellation,
        )
        .unwrap();
        if admitted {
            assert!(result.gaps.is_empty());
            let owner = entities[&1].record.id;
            assert_eq!(
                entities[&2].record.container,
                Some(ContainerRef::Entity(owner))
            );
            assert_eq!(
                entities[&2].identity_claim.container,
                Some(ContainerRef::Entity(owner))
            );
            assert_eq!(entities[&2].direct_parent, ContainerRef::Entity(owner));
            assert_eq!(entities[&2].record.id, old[&2].record.id);
            assert!(result.modeled_peak <= maximum);
        } else {
            unchanged(&old, &entities);
            assert_eq!(result.gaps, vec![drafts[1].span]);
            assert_eq!(result.optional_index_entries, 0);
        }
    }
}

#[test]
fn empty_sdk_plan_keeps_gaps_without_optional_indices_or_repeated_owner_scans() {
    let (source, drafts, mut entities, mut plan) = fixture();
    let old = entities.clone();
    plan.proofs = Vec::new();
    let cancellation = Cancellation::new();
    let resident = 4096;
    let (gaps, base) = required_gaps(
        &drafts,
        &entities,
        resident,
        usize::MAX,
        &Allocation::default(),
        &cancellation,
    )
    .unwrap();
    let allocation = Allocation {
        fail: Some(Stage::OptionalIndex),
    };
    let result = consume_plan(
        &mut entities,
        &drafts,
        &source,
        plan,
        gaps,
        resident,
        base,
        base,
        &allocation,
        &cancellation,
    )
    .unwrap();
    unchanged(&old, &entities);
    assert_eq!(result.gaps, vec![drafts[1].span]);
    assert_eq!(result.optional_index_entries, 0);
    assert_eq!(result.modeled_peak, base);
}

#[test]
fn optional_index_and_update_allocation_failures_preserve_records_claims_parents_and_gaps() {
    for stage in [Stage::OptionalIndex, Stage::OptionalUpdates] {
        let (source, drafts, mut entities, plan) = fixture();
        let old = entities.clone();
        let cancellation = Cancellation::new();
        let resident = 4096;
        let (gaps, base) = required_gaps(
            &drafts,
            &entities,
            resident,
            usize::MAX,
            &Allocation::default(),
            &cancellation,
        )
        .unwrap();
        let result = consume_plan(
            &mut entities,
            &drafts,
            &source,
            plan,
            gaps,
            resident,
            base,
            usize::MAX,
            &Allocation { fail: Some(stage) },
            &cancellation,
        )
        .unwrap();
        unchanged(&old, &entities);
        assert_eq!(result.gaps, vec![drafts[1].span]);
        assert_eq!(result.optional_index_entries, 0);
    }
}

#[test]
fn required_gap_allocation_or_combined_resident_refusal_is_an_honest_error() {
    let (_, drafts, entities, _) = fixture();
    let cancellation = Cancellation::new();
    assert!(matches!(
        required_gaps(
            &drafts,
            &entities,
            4096,
            usize::MAX,
            &Allocation {
                fail: Some(Stage::RequiredGaps)
            },
            &cancellation
        ),
        Err(AdapterError::Sink(SinkError::AllocationFailed))
    ));
    let required = 4096 + size_of::<SourceSpan>();
    assert!(
        required_gaps(
            &drafts,
            &entities,
            4096,
            required,
            &Allocation::default(),
            &cancellation
        )
        .is_ok()
    );
    assert!(matches!(
        required_gaps(
            &drafts,
            &entities,
            4096,
            required - 1,
            &Allocation::default(),
            &cancellation
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::ReportedMemoryBytes,
            ..
        }))
    ));
}

#[test]
fn in_place_native_index_sort_orders_duplicates_and_propagates_cancellation() {
    for mut values in [vec![], vec![1], vec![3, 1, 3, 2], (0..200).rev().collect()] {
        let mut expected = values.clone();
        expected.sort_unstable();
        sort(&mut values, &Cancellation::new(), Ord::cmp).unwrap();
        assert_eq!(values, expected);
    }
    let cancellation = Cancellation::new();
    let comparisons = std::cell::Cell::new(0usize);
    let compare = |left: &usize, right: &usize| {
        comparisons.set(comparisons.get() + 1);
        if comparisons.get() == 2 {
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        }
        left.cmp(right)
    };
    assert!(matches!(
        sort(&mut [4, 3, 2, 1], &cancellation, compare),
        Err(AdapterError::Cancelled {
            reason: rootlight_cancel::CancellationReason::ClientRequest
        })
    ));
}

fn configured_work_test_limits(source_bytes: usize, syntax_nodes: usize) -> AnalysisLimits {
    let stream = rootlight_adapter_sdk::StreamLimits::new(
        8,
        128,
        32 * 1024,
        16,
        8 * 1024,
        32 * 1024,
        rootlight_adapter_sdk::BatchThresholds::new(32, 4096, 4, 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        source_bytes,
        syntax_nodes,
        64,
        4,
        1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
}

#[test]
fn finite_native_work_credit_keeps_exact_subtraction_and_one_less_decline() {
    let bytes = b"struct Owner {} impl Owner { fn run() {} }";
    let limits = configured_work_test_limits(64, 16);
    let work = 64 * 16;
    for one_less in [false, true] {
        let (source, drafts, mut entities, _) = fixture();
        let before = entities.clone();
        let resident =
            work - native_work(drafts.len(), entities.len(), 0).unwrap() + usize::from(one_less);
        let result = reconcile(
            &mut entities,
            &drafts,
            &[],
            &source,
            bytes,
            true,
            resident,
            &limits,
            &Cancellation::new(),
        )
        .unwrap();
        unchanged(&before, &entities);
        assert_eq!(result.gaps, vec![drafts[1].span]);
        let base = resident + size_of::<SourceSpan>();
        if one_less {
            assert_eq!(
                native_work(drafts.len(), entities.len(), resident).unwrap(),
                work + 1
            );
            assert_eq!(result.modeled_peak, base);
        } else {
            assert_eq!(
                native_work(drafts.len(), entities.len(), resident).unwrap(),
                work
            );
            // Exact subtraction leaves zero SDK work; its actual-source quote
            // is reached, then refuses proof without renewing that credit.
            assert_eq!(result.modeled_peak, base + bytes.len() * 128);
        }
    }
}

#[test]
fn actual_native_cost_overflow_remains_a_checked_error() {
    for result in [slots(usize::MAX, 2), native_work(usize::MAX, 1, 0)] {
        assert!(matches!(
            result,
            Err(AdapterError::Sink(SinkError::AccountingOverflow))
        ));
    }
}
