//! Public-contract tests for bounded reconcile, invalidation, and equivalence.
//!
//! Fixtures use only stable IDs and source-free hashes so ordering, cancellation,
//! and conservative fallback remain observable without storage or M12 publication.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use proptest::prelude::*;
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId};
use rootlight_incremental::{
    AnalysisUnitId, ArtifactDecisionKind, ArtifactId, ArtifactSummary, AuthoritativeScan,
    BaselineFile, DependencyEdge, DependencyGraph, DependencyRegistry, DependencySource,
    EquivalenceSnapshot, FactDomain, FactDomainSet, FactNode, FallbackReason, FileChangeKind,
    FileDescriptor, FileMetadata, GenerationSummary, GraphLimits, HashDecisionReason,
    IncrementalError, InputFingerprint, InputKey, InputKind, InputSnapshot, LogicalComponent,
    LogicalDomain, MetadataBaseline, PassDeclaration, PassId, PassObservation, PlanningLimits,
    PlatformFileIdentity, ReconcileLimits, ReconcileMode, ScannedFile, TraceAction,
    plan_invalidation, plan_reconcile,
};

fn cancellation() -> Cancellation {
    Cancellation::new()
}

fn fact(seed: u8) -> FactId {
    FactId::from_bytes([seed; 20])
}

fn file(seed: u8) -> FileId {
    FileId::from_bytes([seed; 20])
}

fn hash(seed: u8) -> ContentHash {
    ContentHash::from_bytes([seed; 32])
}

fn unit(seed: u8) -> AnalysisUnitId {
    AnalysisUnitId::new(fact(seed))
}

fn planning_limits() -> PlanningLimits {
    PlanningLimits::new(64, 32, 256, 256).expect("fixture limits are valid")
}

fn graph_limits(nodes: usize, edges: usize) -> GraphLimits {
    GraphLimits::new(16, nodes.max(1), edges.max(1)).expect("fixture limits are valid")
}

fn body_pass() -> PassDeclaration {
    PassDeclaration::new(
        PassId::parse("body.resolve").expect("fixture pass ID is valid"),
        [InputKind::BodySummary],
        FactDomainSet::new([FactDomain::Body]),
        FactDomainSet::new([FactDomain::Body]),
    )
    .expect("fixture declaration has an output")
}

fn body_registry() -> DependencyRegistry {
    DependencyRegistry::new([body_pass()], graph_limits(16, 64), &cancellation())
        .expect("fixture registry is valid")
}

fn input_snapshot(entries: impl IntoIterator<Item = InputFingerprint>) -> InputSnapshot {
    InputSnapshot::new(entries, planning_limits(), &cancellation())
        .expect("fixture input snapshot is valid")
}

fn summary(inputs: InputSnapshot, artifacts: Vec<ArtifactSummary>) -> GenerationSummary {
    GenerationSummary::new(inputs, artifacts, planning_limits(), &cancellation())
        .expect("fixture generation summary is valid")
}

#[test]
fn pass_ids_are_bounded_and_canonical() {
    assert!(PassId::parse("resolver.calls_v1").is_ok());
    for invalid in ["", "Upper", "contains space", "path/shaped"] {
        assert!(matches!(
            PassId::parse(invalid),
            Err(IncrementalError::InvalidPassId)
        ));
    }
}

#[test]
fn hidden_pass_dependency_fails_validation() {
    let registry = body_registry();
    let observation = PassObservation::new(
        [InputKind::BodySummary, InputKind::PublicSurface],
        FactDomainSet::new([FactDomain::Body]),
        FactDomainSet::new([FactDomain::Body]),
    );

    assert!(matches!(
        registry.verify_observation(
            &PassId::parse("body.resolve").expect("fixture pass ID is valid"),
            &observation
        ),
        Err(IncrementalError::UndeclaredInputKind {
            kind: InputKind::PublicSurface,
            ..
        })
    ));
}

#[test]
fn body_change_invalidates_only_declared_dependent_closure() {
    let changed = unit(1);
    let dependent = unit(2);
    let unrelated = unit(3);
    let changed_node = FactNode::new(changed, FactDomain::Body);
    let dependent_node = FactNode::new(dependent, FactDomain::Body);
    let unrelated_node = FactNode::new(unrelated, FactDomain::Body);
    let key = InputKey::BodySummary(changed);
    let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
    let graph = DependencyGraph::new(
        [changed_node, dependent_node, unrelated_node],
        [
            DependencyEdge::new(DependencySource::Input(key), changed_node, pass.clone()),
            DependencyEdge::new(DependencySource::Fact(changed_node), dependent_node, pass),
        ],
        &body_registry(),
        graph_limits(3, 2),
        &cancellation(),
    )
    .expect("fixture graph is valid");
    let parent = summary(
        input_snapshot([InputFingerprint::new(key, hash(1))]),
        Vec::new(),
    );
    let current = input_snapshot([InputFingerprint::new(key, hash(2))]);

    let plan = plan_invalidation(
        &parent,
        &current,
        &graph,
        planning_limits(),
        &cancellation(),
    )
    .expect("bounded closure succeeds");
    let invalidated: BTreeSet<_> = plan.invalidated_nodes().collect();

    assert_eq!(invalidated, BTreeSet::from([changed_node, dependent_node]));
    assert!(!invalidated.contains(&unrelated_node));
    assert_eq!(
        plan.reanalyze().collect::<Vec<_>>(),
        vec![changed, dependent]
    );
    assert!(plan.fallback().is_none());
    assert!(
        plan.trace()
            .entries()
            .iter()
            .all(|entry| entry.action() != TraceAction::ConservativeFallback)
    );
}

#[test]
fn cycles_reach_a_deterministic_fixed_point() {
    let first = FactNode::new(unit(1), FactDomain::Body);
    let second = FactNode::new(unit(2), FactDomain::Body);
    let key = InputKey::BodySummary(unit(1));
    let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
    let graph = DependencyGraph::new(
        [second, first],
        [
            DependencyEdge::new(DependencySource::Fact(second), first, pass.clone()),
            DependencyEdge::new(DependencySource::Input(key), first, pass.clone()),
            DependencyEdge::new(DependencySource::Fact(first), second, pass),
        ],
        &body_registry(),
        graph_limits(2, 3),
        &cancellation(),
    )
    .expect("cyclic fixture graph is valid");
    let parent = summary(
        input_snapshot([InputFingerprint::new(key, hash(1))]),
        Vec::new(),
    );
    let current = input_snapshot([InputFingerprint::new(key, hash(2))]);

    let first_plan = plan_invalidation(
        &parent,
        &current,
        &graph,
        planning_limits(),
        &cancellation(),
    )
    .expect("cyclic closure terminates");
    let second_plan = plan_invalidation(
        &parent,
        &current,
        &graph,
        planning_limits(),
        &cancellation(),
    )
    .expect("repeated cyclic closure terminates");

    assert_eq!(
        first_plan.invalidated_nodes().collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(
        first_plan
            .trace()
            .canonical_json()
            .expect("trace serializes"),
        second_plan
            .trace()
            .canonical_json()
            .expect("trace serializes")
    );
}

#[test]
fn missing_dependency_selects_repository_rebuild_and_disables_reuse() {
    let node = FactNode::new(unit(1), FactDomain::Body);
    let declared_key = InputKey::BodySummary(unit(1));
    let changed_key = InputKey::BodySummary(unit(2));
    let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
    let graph = DependencyGraph::new(
        [node],
        [DependencyEdge::new(
            DependencySource::Input(declared_key),
            node,
            pass,
        )],
        &body_registry(),
        graph_limits(1, 1),
        &cancellation(),
    )
    .expect("fixture graph is valid");
    let parent_inputs = input_snapshot([
        InputFingerprint::new(declared_key, hash(1)),
        InputFingerprint::new(changed_key, hash(2)),
    ]);
    let artifact = ArtifactSummary::new(
        ArtifactId::new(fact(9)),
        [node],
        [InputFingerprint::new(declared_key, hash(1))],
        planning_limits(),
        &cancellation(),
    )
    .expect("fixture artifact is valid");
    let parent = summary(parent_inputs, vec![artifact]);
    let current = input_snapshot([
        InputFingerprint::new(declared_key, hash(1)),
        InputFingerprint::new(changed_key, hash(3)),
    ]);

    let plan = plan_invalidation(
        &parent,
        &current,
        &graph,
        planning_limits(),
        &cancellation(),
    )
    .expect("missing edge chooses a safe fallback");

    assert!(plan.fallback().is_some());
    assert_eq!(plan.invalidated_nodes().collect::<Vec<_>>(), vec![node]);
    assert_eq!(
        plan.artifact_decisions()[0].kind(),
        ArtifactDecisionKind::Rebuild
    );
    assert!(
        plan.trace()
            .entries()
            .iter()
            .any(|entry| entry.action() == TraceAction::ConservativeFallback)
    );
}

#[test]
fn exhausted_closure_budget_selects_repository_rebuild() {
    let first = FactNode::new(unit(1), FactDomain::Body);
    let second = FactNode::new(unit(2), FactDomain::Body);
    let key = InputKey::BodySummary(unit(1));
    let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
    let graph = DependencyGraph::new(
        [first, second],
        [
            DependencyEdge::new(DependencySource::Input(key), first, pass.clone()),
            DependencyEdge::new(DependencySource::Fact(first), second, pass),
        ],
        &body_registry(),
        graph_limits(2, 2),
        &cancellation(),
    )
    .expect("fixture graph is valid");
    let parent = summary(
        input_snapshot([InputFingerprint::new(key, hash(1))]),
        Vec::new(),
    );
    let current = input_snapshot([InputFingerprint::new(key, hash(2))]);
    let tight_limits = PlanningLimits::new(64, 32, 1, 256).expect("fixture limits are valid");

    let plan = plan_invalidation(&parent, &current, &graph, tight_limits, &cancellation())
        .expect("closure exhaustion chooses a safe fallback");

    assert_eq!(
        plan.fallback().expect("fallback is explicit").reason(),
        FallbackReason::ClosureWorkExceeded
    );
    assert_eq!(
        plan.invalidated_nodes().collect::<Vec<_>>(),
        vec![first, second]
    );
}

#[test]
fn complete_artifact_fingerprint_is_required_for_reuse() {
    let node = FactNode::new(unit(1), FactDomain::Body);
    let key = InputKey::BodySummary(unit(1));
    let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
    let graph = DependencyGraph::new(
        [node],
        [DependencyEdge::new(
            DependencySource::Input(key),
            node,
            pass,
        )],
        &body_registry(),
        graph_limits(1, 1),
        &cancellation(),
    )
    .expect("fixture graph is valid");
    let artifact = ArtifactSummary::new(
        ArtifactId::new(fact(8)),
        [node],
        [InputFingerprint::new(key, hash(1))],
        planning_limits(),
        &cancellation(),
    )
    .expect("fixture artifact is valid");
    let parent = summary(
        input_snapshot([InputFingerprint::new(key, hash(1))]),
        vec![artifact],
    );

    let no_op = plan_invalidation(
        &parent,
        parent.inputs(),
        &graph,
        planning_limits(),
        &cancellation(),
    )
    .expect("no-op plan succeeds");
    assert!(no_op.changes().is_empty());
    assert_eq!(
        no_op.artifact_decisions()[0].kind(),
        ArtifactDecisionKind::Reuse
    );

    let changed = input_snapshot([InputFingerprint::new(key, hash(2))]);
    let update = plan_invalidation(
        &parent,
        &changed,
        &graph,
        planning_limits(),
        &cancellation(),
    )
    .expect("changed plan succeeds");
    assert_eq!(
        update.artifact_decisions()[0].kind(),
        ArtifactDecisionKind::Rebuild
    );
}

fn trusted_metadata(modified_ns: u128, identity: u64) -> FileMetadata {
    FileMetadata::trusted(5, modified_ns, PlatformFileIdentity::new(1, identity))
}

fn baseline_file(
    file_id: FileId,
    path_hash: ContentHash,
    metadata: FileMetadata,
    content_hash: ContentHash,
) -> BaselineFile {
    BaselineFile::new(
        FileDescriptor::new(file_id, path_hash, metadata),
        content_hash,
    )
}

fn scan_file(file_id: FileId, path_hash: ContentHash, metadata: FileMetadata) -> ScannedFile {
    ScannedFile::new(FileDescriptor::new(file_id, path_hash, metadata))
}

#[test]
fn trusted_no_op_reuses_hash_without_reading_content() {
    let limits = ReconcileLimits::new(10).expect("fixture limits are valid");
    let record = baseline_file(file(1), hash(1), trusted_metadata(10, 1), hash(10));
    let baseline =
        MetadataBaseline::new([record], limits, &cancellation()).expect("baseline is valid");
    let scan = AuthoritativeScan::new(
        [scan_file(file(1), hash(1), trusted_metadata(10, 1))],
        limits,
        &cancellation(),
    )
    .expect("scan is valid");

    let plan = plan_reconcile(
        &baseline,
        &scan,
        ReconcileMode::Normal,
        limits,
        &cancellation(),
    )
    .expect("no-op reconcile plans");

    assert!(plan.files_to_hash().next().is_none());
    assert_eq!(
        plan.decisions().next().expect("one decision").reason(),
        HashDecisionReason::TrustedMetadataUnchanged
    );
    let outcome = plan
        .finish(&BTreeMap::new(), limits, &cancellation())
        .expect("reused hash completes");
    assert_eq!(outcome.changes()[0].kind(), FileChangeKind::NoChange);
}

#[test]
fn metadata_changes_detect_same_size_rewrite_clock_skew_and_replacement() {
    let limits = ReconcileLimits::new(10).expect("fixture limits are valid");
    let baseline_record = baseline_file(file(1), hash(1), trusted_metadata(10, 1), hash(10));
    let baseline = MetadataBaseline::new([baseline_record], limits, &cancellation())
        .expect("baseline is valid");
    let cases = [
        trusted_metadata(11, 1),
        trusted_metadata(9, 1),
        trusted_metadata(10, 2),
    ];

    for metadata in cases {
        let scan = AuthoritativeScan::new(
            [scan_file(file(1), hash(1), metadata)],
            limits,
            &cancellation(),
        )
        .expect("scan is valid");
        let plan = plan_reconcile(
            &baseline,
            &scan,
            ReconcileMode::Normal,
            limits,
            &cancellation(),
        )
        .expect("changed metadata plans");
        assert_eq!(plan.files_to_hash().collect::<Vec<_>>(), vec![file(1)]);
        let outcome = plan
            .finish(
                &BTreeMap::from([(file(1), hash(11))]),
                limits,
                &cancellation(),
            )
            .expect("requested hash completes");
        assert_eq!(outcome.changes()[0].kind(), FileChangeKind::Modified);
    }
}

#[test]
fn unique_stable_identity_detects_move_without_rehashing() {
    let limits = ReconcileLimits::new(10).expect("fixture limits are valid");
    let metadata = trusted_metadata(10, 7);
    let baseline = MetadataBaseline::new(
        [baseline_file(file(1), hash(1), metadata, hash(10))],
        limits,
        &cancellation(),
    )
    .expect("baseline is valid");
    let scan = AuthoritativeScan::new(
        [scan_file(file(2), hash(2), metadata)],
        limits,
        &cancellation(),
    )
    .expect("scan is valid");

    let plan = plan_reconcile(
        &baseline,
        &scan,
        ReconcileMode::Normal,
        limits,
        &cancellation(),
    )
    .expect("move plans");
    assert!(plan.files_to_hash().next().is_none());
    let outcome = plan
        .finish(&BTreeMap::new(), limits, &cancellation())
        .expect("move completes");

    assert_eq!(outcome.changes()[0].kind(), FileChangeKind::Moved);
    assert_eq!(outcome.changes()[0].previous_file(), Some(file(1)));
    assert_eq!(outcome.changes()[0].current_file(), Some(file(2)));
}

#[test]
fn audit_and_untrusted_metadata_force_hashing() {
    let limits = ReconcileLimits::new(10).expect("fixture limits are valid");
    let trusted = trusted_metadata(10, 1);
    let baseline = MetadataBaseline::new(
        [baseline_file(file(1), hash(1), trusted, hash(10))],
        limits,
        &cancellation(),
    )
    .expect("baseline is valid");
    let audit_scan = AuthoritativeScan::new(
        [scan_file(file(1), hash(1), trusted)],
        limits,
        &cancellation(),
    )
    .expect("scan is valid");
    let audit = plan_reconcile(
        &baseline,
        &audit_scan,
        ReconcileMode::Audit,
        limits,
        &cancellation(),
    )
    .expect("audit plans");
    assert_eq!(audit.files_to_hash().collect::<Vec<_>>(), vec![file(1)]);

    let untrusted_scan = AuthoritativeScan::new(
        [scan_file(
            file(1),
            hash(1),
            FileMetadata::untrusted(5, Some(10), Some(PlatformFileIdentity::new(1, 1))),
        )],
        limits,
        &cancellation(),
    )
    .expect("scan is valid");
    let untrusted = plan_reconcile(
        &baseline,
        &untrusted_scan,
        ReconcileMode::Normal,
        limits,
        &cancellation(),
    )
    .expect("untrusted metadata plans");
    assert_eq!(
        untrusted.decisions().next().expect("one decision").reason(),
        HashDecisionReason::MetadataUntrusted
    );
}

#[test]
fn reconcile_completion_requires_exactly_requested_hashes() {
    let limits = ReconcileLimits::new(10).expect("fixture limits are valid");
    let baseline =
        MetadataBaseline::new([], limits, &cancellation()).expect("empty baseline is valid");
    let scan = AuthoritativeScan::new(
        [scan_file(file(1), hash(1), trusted_metadata(10, 1))],
        limits,
        &cancellation(),
    )
    .expect("scan is valid");
    let missing_plan = plan_reconcile(
        &baseline,
        &scan,
        ReconcileMode::Normal,
        limits,
        &cancellation(),
    )
    .expect("new file requires hashing");
    assert!(matches!(
        missing_plan.finish(&BTreeMap::new(), limits, &cancellation()),
        Err(IncrementalError::MissingHash { file: missing }) if missing == file(1)
    ));

    let unexpected_plan = plan_reconcile(
        &baseline,
        &scan,
        ReconcileMode::Normal,
        limits,
        &cancellation(),
    )
    .expect("new file requires hashing");
    assert!(matches!(
        unexpected_plan.finish(
            &BTreeMap::from([(file(1), hash(10)), (file(2), hash(20))]),
            limits,
            &cancellation(),
        ),
        Err(IncrementalError::UnexpectedHash { file: unexpected }) if unexpected == file(2)
    ));
}

fn all_logical_components(changed_domain: Option<LogicalDomain>) -> Vec<LogicalComponent> {
    [
        LogicalDomain::Discovery,
        LogicalDomain::NormalizedIr,
        LogicalDomain::LogicalStore,
        LogicalDomain::QueryOutputs,
        LogicalDomain::Coverage,
        LogicalDomain::Provenance,
        LogicalDomain::StableIds,
    ]
    .into_iter()
    .map(|domain| {
        let byte = if changed_domain == Some(domain) { 2 } else { 1 };
        LogicalComponent::from_canonical_bytes(domain, &[byte], 1, 1024, &cancellation())
            .expect("fixture component hashes")
    })
    .collect()
}

#[test]
fn clean_equivalence_is_exact_and_reports_domain_mismatch() {
    let incremental = EquivalenceSnapshot::new(all_logical_components(None), &cancellation())
        .expect("incremental snapshot is complete");
    let clean = EquivalenceSnapshot::new(all_logical_components(None), &cancellation())
        .expect("clean snapshot is complete");
    let equal = incremental
        .compare_clean(&clean, &cancellation())
        .expect("comparison completes");
    assert!(equal.is_equivalent());
    assert!(equal.require_equivalent().is_ok());

    let divergent = EquivalenceSnapshot::new(
        all_logical_components(Some(LogicalDomain::QueryOutputs)),
        &cancellation(),
    )
    .expect("divergent snapshot is complete");
    let report = incremental
        .compare_clean(&divergent, &cancellation())
        .expect("comparison completes");
    assert_eq!(report.mismatches().len(), 1);
    assert_eq!(report.mismatches()[0].domain(), LogicalDomain::QueryOutputs);
    assert!(matches!(
        report.require_equivalent(),
        Err(IncrementalError::LogicalInequality)
    ));
}

#[test]
fn expired_deadline_stops_each_long_running_entrypoint() {
    let expired = Cancellation::with_deadline(Instant::now());
    assert!(matches!(
        InputSnapshot::new(
            [InputFingerprint::new(InputKey::ResolverVersion, hash(1))],
            planning_limits(),
            &expired,
        ),
        Err(IncrementalError::Cancelled(_))
    ));
    assert!(matches!(
        LogicalComponent::from_canonical_bytes(
            LogicalDomain::Discovery,
            &[1, 2, 3],
            1,
            1024,
            &expired,
        ),
        Err(IncrementalError::Cancelled(_))
    ));
}

proptest! {
    #[test]
    fn dependency_closure_matches_reference_model(
        raw_edges in proptest::collection::vec((0_u8..6, 0_u8..6), 0..24)
    ) {
        let key = InputKey::BodySummary(unit(1));
        let nodes: Vec<_> = (0_u8..6)
            .map(|index| FactNode::new(unit(index + 1), FactDomain::Body))
            .collect();
        let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
        let mut edges = vec![DependencyEdge::new(
            DependencySource::Input(key),
            nodes[0],
            pass.clone(),
        )];
        edges.extend(raw_edges.iter().map(|(from, to)| {
            DependencyEdge::new(
                DependencySource::Fact(nodes[usize::from(*from)]),
                nodes[usize::from(*to)],
                pass.clone(),
            )
        }));
        let graph = DependencyGraph::new(
            nodes.iter().copied(),
            edges,
            &body_registry(),
            graph_limits(nodes.len(), raw_edges.len() + 1),
            &cancellation(),
        )
        .expect("bounded random graph is valid");
        let parent = summary(
            input_snapshot([InputFingerprint::new(key, hash(1))]),
            Vec::new(),
        );
        let current = input_snapshot([InputFingerprint::new(key, hash(2))]);
        let plan = plan_invalidation(
            &parent,
            &current,
            &graph,
            planning_limits(),
            &cancellation(),
        )
        .expect("bounded closure succeeds");

        let mut expected = BTreeSet::from([0_u8]);
        loop {
            let before = expected.len();
            for (from, to) in &raw_edges {
                if expected.contains(from) {
                    expected.insert(*to);
                }
            }
            if expected.len() == before {
                break;
            }
        }
        let expected_nodes: BTreeSet<_> = expected
            .into_iter()
            .map(|index| nodes[usize::from(index)])
            .collect();
        prop_assert_eq!(
            plan.invalidated_nodes().collect::<BTreeSet<_>>(),
            expected_nodes
        );
    }

    #[test]
    fn input_and_edge_order_do_not_change_the_plan(reverse in any::<bool>()) {
        let first_key = InputKey::BodySummary(unit(1));
        let second_key = InputKey::BodySummary(unit(2));
        let first = FactNode::new(unit(1), FactDomain::Body);
        let second = FactNode::new(unit(2), FactDomain::Body);
        let pass = PassId::parse("body.resolve").expect("fixture pass ID is valid");
        let mut inputs = vec![
            InputFingerprint::new(first_key, hash(1)),
            InputFingerprint::new(second_key, hash(3)),
        ];
        let mut edges = vec![
            DependencyEdge::new(DependencySource::Input(first_key), first, pass.clone()),
            DependencyEdge::new(DependencySource::Fact(first), second, pass.clone()),
            DependencyEdge::new(DependencySource::Input(second_key), second, pass),
        ];
        if reverse {
            inputs.reverse();
            edges.reverse();
        }
        let parent = summary(input_snapshot(inputs), Vec::new());
        let current = input_snapshot([
            InputFingerprint::new(second_key, hash(3)),
            InputFingerprint::new(first_key, hash(2)),
        ]);
        let graph = DependencyGraph::new(
            [second, first],
            edges,
            &body_registry(),
            graph_limits(2, 3),
            &cancellation(),
        )
        .expect("fixture graph is valid");
        let plan = plan_invalidation(
            &parent,
            &current,
            &graph,
            planning_limits(),
            &cancellation(),
        )
        .expect("plan succeeds");

        prop_assert_eq!(plan.invalidated_nodes().collect::<Vec<_>>(), vec![first, second]);
        prop_assert!(plan.fallback().is_none());
    }
}
