//! Contract tests for exact cross-repository links and bounded workflows.
//!
//! All relations are host-supplied declarations pinned to immutable snapshot
//! members, so the tests also exercise the capability-free trust boundary.

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId};
use rootlight_ir::Confidence;
use rootlight_workspace::{
    CatalogLimits, CrossLinkVersion, LinkCaveat, LinkDeclaration, LinkDirection, LinkError,
    LinkKind, LinkLimits, RepositoryDescriptor, RepositoryRootIdentity, RepositoryState,
    ServiceKey, SharedContentIdentity, SnapshotBuildMode, SnapshotLimits, WorkflowBudget,
    WorkflowError, WorkflowKind, WorkflowRequest, WorkspaceCatalog, WorkspaceFactRef, WorkspaceId,
    WorkspaceSnapshot, WorkspaceSnapshotRequest, build_link_overlay, execute_workflow,
};

fn hash(seed: u8) -> ContentHash {
    ContentHash::from_bytes([seed; 32])
}

fn repository(seed: u8) -> RepositoryId {
    RepositoryId::from_bytes([seed; 16])
}

fn generation(seed: u8) -> GenerationId {
    GenerationId::from_bytes([seed; 20])
}

fn endpoint(repository_seed: u8, generation_seed: u8, fact_seed: u8) -> WorkspaceFactRef {
    WorkspaceFactRef::new(
        repository(repository_seed),
        generation(generation_seed),
        FactId::from_bytes([fact_seed; 20]),
    )
}

fn confidence(value: u16) -> Confidence {
    Confidence::new(value).expect("fixture confidence should be valid")
}

fn catalog(entries: &[(u8, u8)]) -> WorkspaceCatalog {
    let cancellation = Cancellation::new();
    let mut catalog =
        WorkspaceCatalog::new(WorkspaceId::from_hash(hash(199)), CatalogLimits::default());
    for (repository_seed, generation_seed) in entries {
        catalog
            .register(
                RepositoryDescriptor::new(
                    repository(*repository_seed),
                    RepositoryRootIdentity::from_hash(hash(*repository_seed)),
                    SharedContentIdentity::from_hash(hash(repository_seed.saturating_add(64))),
                ),
                &cancellation,
            )
            .expect("fixture repository registration should succeed");
        catalog
            .publish_generation(
                repository(*repository_seed),
                generation(*generation_seed),
                &cancellation,
            )
            .expect("fixture generation publication should succeed");
    }
    catalog
}

fn snapshot(
    catalog: &WorkspaceCatalog,
    entries: &[(u8, u8)],
    mode: SnapshotBuildMode,
) -> WorkspaceSnapshot {
    let request = entries.iter().fold(
        WorkspaceSnapshotRequest::new(hash(200), CrossLinkVersion::from_hash(hash(201)), 10, 20),
        |request, (repository_seed, generation_seed)| {
            request.with_member(repository(*repository_seed), generation(*generation_seed))
        },
    );
    WorkspaceSnapshot::build(
        catalog,
        request,
        mode,
        SnapshotLimits::default(),
        &Cancellation::new(),
    )
    .expect("fixture snapshot should build")
}

fn declaration(
    endpoint: WorkspaceFactRef,
    kind: LinkKind,
    direction: LinkDirection,
    key: ServiceKey,
    confidence: u16,
) -> LinkDeclaration {
    LinkDeclaration::new(endpoint, kind, direction, key, self::confidence(confidence))
}

fn named_key(kind: LinkKind, value: &str) -> ServiceKey {
    ServiceKey::named(kind, value).expect("fixture key should be valid")
}

#[test]
fn link_overlay_preserves_all_supported_declarative_families() {
    let catalog = catalog(&[(1, 11), (2, 21)]);
    let snapshot = snapshot(&catalog, &[(1, 11), (2, 21)], SnapshotBuildMode::Strict);
    let families = [
        (
            LinkKind::Package,
            ServiceKey::immutable(LinkKind::Package, hash(50))
                .expect("package key should be valid"),
        ),
        (
            LinkKind::Http,
            ServiceKey::http("get", "/v1/users/{user_id}").expect("HTTP key should be valid"),
        ),
        (LinkKind::Rpc, named_key(LinkKind::Rpc, "User.Get")),
        (
            LinkKind::Messaging,
            named_key(LinkKind::Messaging, "Orders.Created"),
        ),
        (
            LinkKind::Database,
            named_key(LinkKind::Database, "Public.Users"),
        ),
    ];
    let mut declarations = Vec::new();
    for (index, (kind, key)) in families.into_iter().enumerate() {
        let fact_seed = u8::try_from(index).expect("fixture index should fit");
        declarations.push(
            declaration(
                endpoint(1, 11, fact_seed.saturating_add(1)),
                kind,
                LinkDirection::Provider,
                key.clone(),
                950,
            )
            .with_caveat(LinkCaveat::GeneratedConfiguration),
        );
        declarations.push(
            declaration(
                endpoint(2, 21, fact_seed.saturating_add(20)),
                kind,
                LinkDirection::Consumer,
                key,
                800,
            )
            .with_caveat(LinkCaveat::MissingSchema),
        );
    }
    declarations.push(declaration(
        endpoint(2, 21, 40),
        LinkKind::Rpc,
        LinkDirection::Consumer,
        named_key(LinkKind::Rpc, "unresolved.call"),
        700,
    ));

    let overlay = build_link_overlay(
        &snapshot,
        declarations,
        LinkLimits::default(),
        &Cancellation::new(),
    )
    .expect("supported declarations should link");
    assert_eq!(overlay.links().len(), 5);
    assert_eq!(overlay.unresolved_consumers(), 1);
    assert!(
        overlay
            .links()
            .iter()
            .all(|link| link.candidates()[0].confidence().get() == 800)
    );
    assert!(overlay.links().iter().all(|link| {
        link.candidates()[0]
            .caveats()
            .contains(&LinkCaveat::MissingSchema)
            && link.candidates()[0]
                .caveats()
                .contains(&LinkCaveat::GeneratedConfiguration)
    }));
}

#[test]
fn normalized_http_routes_keep_ambiguity_and_candidate_limits_explicit() {
    let catalog = catalog(&[(1, 11), (2, 21), (3, 31), (4, 41)]);
    let snapshot = snapshot(
        &catalog,
        &[(1, 11), (2, 21), (3, 31), (4, 41)],
        SnapshotBuildMode::Strict,
    );
    let provider_key = ServiceKey::http("GET", "/users/:id").expect("provider key should be valid");
    let consumer_key =
        ServiceKey::http("get", "/users/{user}").expect("consumer key should be valid");
    assert_eq!(provider_key, consumer_key);
    let mut declarations = vec![
        declaration(
            endpoint(4, 41, 4),
            LinkKind::Http,
            LinkDirection::Consumer,
            consumer_key,
            850,
        )
        .with_caveat(LinkCaveat::EnvironmentDependentBase),
    ];
    for repository_seed in 1..=3 {
        declarations.push(declaration(
            endpoint(repository_seed, repository_seed * 10 + 1, repository_seed),
            LinkKind::Http,
            LinkDirection::Provider,
            provider_key.clone(),
            900,
        ));
    }
    let limits = LinkLimits::new(10, 10, 1, 512, 16).expect("fixture limits should be valid");

    let overlay = build_link_overlay(&snapshot, declarations, limits, &Cancellation::new())
        .expect("ambiguous declarations should produce a bounded candidate set");
    let link = &overlay.links()[0];
    assert_eq!(link.candidate_count(), 3);
    assert_eq!(link.candidates().len(), 1);
    assert!(link.truncated());
    assert!(
        link.candidates()[0]
            .caveats()
            .contains(&LinkCaveat::MultipleCandidates)
    );
    assert!(
        link.candidates()[0]
            .caveats()
            .contains(&LinkCaveat::CandidateLimit)
    );
}

#[test]
fn workflow_edge_budget_bounds_overlay_expansion() {
    let catalog = catalog(&[(1, 11), (2, 21), (3, 31), (4, 41)]);
    let snapshot = snapshot(
        &catalog,
        &[(1, 11), (2, 21), (3, 31), (4, 41)],
        SnapshotBuildMode::Strict,
    );
    let key = named_key(LinkKind::Rpc, "inventory.reserve");
    let consumer = endpoint(4, 41, 4);
    let mut declarations = vec![declaration(
        consumer,
        LinkKind::Rpc,
        LinkDirection::Consumer,
        key.clone(),
        850,
    )];
    for repository_seed in 1..=3 {
        declarations.push(declaration(
            endpoint(repository_seed, repository_seed * 10 + 1, repository_seed),
            LinkKind::Rpc,
            LinkDirection::Provider,
            key.clone(),
            900,
        ));
    }
    let overlay = build_link_overlay(
        &snapshot,
        declarations,
        LinkLimits::new(10, 10, 3, 512, 16).expect("fixture limits should be valid"),
        &Cancellation::new(),
    )
    .expect("wide overlay should build");
    let budget = WorkflowBudget::new(4, 10, 1, 4, 4).expect("tiny edge budget should be valid");

    let result = execute_workflow(
        &snapshot,
        &overlay,
        WorkflowRequest::new(WorkflowKind::Flow, budget).with_seed(consumer),
        &Cancellation::new(),
    )
    .expect("bounded workflow should return its admitted prefix");

    assert_eq!(result.edges_scanned(), 1);
    assert_eq!(result.rows().len(), 1);
    assert!(result.truncated());
    assert!(result.continuation().is_none());
}

#[test]
fn overlay_identity_and_encoding_are_independent_of_declaration_order() {
    let catalog = catalog(&[(1, 11), (2, 21), (3, 31)]);
    let snapshot = snapshot(
        &catalog,
        &[(1, 11), (2, 21), (3, 31)],
        SnapshotBuildMode::Strict,
    );
    let key = named_key(LinkKind::Rpc, "inventory.reserve");
    let declarations = vec![
        declaration(
            endpoint(1, 11, 1),
            LinkKind::Rpc,
            LinkDirection::Provider,
            key.clone(),
            900,
        ),
        declaration(
            endpoint(2, 21, 2),
            LinkKind::Rpc,
            LinkDirection::Provider,
            key.clone(),
            800,
        ),
        declaration(
            endpoint(3, 31, 3),
            LinkKind::Rpc,
            LinkDirection::Consumer,
            key,
            850,
        ),
    ];
    let mut reversed = declarations.clone();
    reversed.reverse();

    let first = build_link_overlay(
        &snapshot,
        declarations,
        LinkLimits::default(),
        &Cancellation::new(),
    )
    .expect("canonical overlay should build");
    let second = build_link_overlay(
        &snapshot,
        reversed,
        LinkLimits::default(),
        &Cancellation::new(),
    )
    .expect("reversed overlay should build");
    assert_eq!(first.id(), second.id());
    assert_eq!(
        serde_json::to_vec(&first).expect("overlay serialization should succeed"),
        serde_json::to_vec(&second).expect("overlay serialization should succeed")
    );
}

#[test]
fn linker_rejects_unpinned_duplicate_invalid_and_cancelled_inputs() {
    assert!(matches!(
        ServiceKey::http("GET", "https://example.test/users"),
        Err(LinkError::InvalidKey)
    ));
    assert!(matches!(
        ServiceKey::http("GET", "/users?admin=true"),
        Err(LinkError::InvalidKey)
    ));
    let catalog = catalog(&[(1, 11), (2, 21)]);
    let snapshot = snapshot(&catalog, &[(1, 11), (2, 21)], SnapshotBuildMode::Strict);
    let key = named_key(LinkKind::Rpc, "user.get");
    let outside = declaration(
        endpoint(1, 12, 1),
        LinkKind::Rpc,
        LinkDirection::Provider,
        key.clone(),
        900,
    );
    assert!(matches!(
        build_link_overlay(
            &snapshot,
            vec![outside],
            LinkLimits::default(),
            &Cancellation::new()
        ),
        Err(LinkError::EndpointOutsideSnapshot)
    ));
    let exact = declaration(
        endpoint(1, 11, 1),
        LinkKind::Rpc,
        LinkDirection::Provider,
        key,
        900,
    );
    assert!(matches!(
        build_link_overlay(
            &snapshot,
            vec![exact.clone(), exact],
            LinkLimits::default(),
            &Cancellation::new()
        ),
        Err(LinkError::DuplicateDeclaration)
    ));
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        build_link_overlay(&snapshot, Vec::new(), LinkLimits::default(), &cancellation),
        Err(LinkError::Cancelled(_))
    ));
}

struct ChainFixture {
    snapshot: WorkspaceSnapshot,
    overlay: rootlight_workspace::LinkOverlay,
    provider: WorkspaceFactRef,
    middle: WorkspaceFactRef,
    consumer: WorkspaceFactRef,
}

fn chain_fixture(partial: bool) -> ChainFixture {
    let mut catalog = catalog(&[(1, 11), (2, 21), (3, 31), (4, 41)]);
    if partial {
        catalog
            .set_state(repository(4), RepositoryState::Unavailable)
            .expect("fixture state transition should succeed");
    }
    let snapshot = snapshot(
        &catalog,
        &[(1, 11), (2, 21), (3, 31), (4, 41)],
        if partial {
            SnapshotBuildMode::AllowPartial
        } else {
            SnapshotBuildMode::Strict
        },
    );
    let provider = endpoint(1, 11, 1);
    let middle = endpoint(2, 21, 2);
    let consumer = endpoint(3, 31, 3);
    let first = named_key(LinkKind::Rpc, "catalog.lookup");
    let second = named_key(LinkKind::Rpc, "orders.reserve");
    let declarations = vec![
        declaration(
            provider,
            LinkKind::Rpc,
            LinkDirection::Provider,
            first.clone(),
            950,
        ),
        declaration(middle, LinkKind::Rpc, LinkDirection::Consumer, first, 900),
        declaration(
            middle,
            LinkKind::Rpc,
            LinkDirection::Provider,
            second.clone(),
            900,
        ),
        declaration(
            consumer,
            LinkKind::Rpc,
            LinkDirection::Consumer,
            second,
            850,
        ),
    ];
    let overlay = build_link_overlay(
        &snapshot,
        declarations,
        LinkLimits::default(),
        &Cancellation::new(),
    )
    .expect("chain overlay should build");
    ChainFixture {
        snapshot,
        overlay,
        provider,
        middle,
        consumer,
    }
}

#[test]
fn flow_and_impact_traverse_exact_generations_in_opposite_directions() {
    let fixture = chain_fixture(false);
    let budget = WorkflowBudget::new(4, 10, 10, 4, 4).expect("budget should be valid");
    let flow = execute_workflow(
        &fixture.snapshot,
        &fixture.overlay,
        WorkflowRequest::new(WorkflowKind::Flow, budget).with_seed(fixture.consumer),
        &Cancellation::new(),
    )
    .expect("flow traversal should succeed");
    assert_eq!(flow.rows().len(), 2);
    assert_eq!(flow.rows()[0].from(), fixture.consumer);
    assert_eq!(flow.rows()[0].to(), fixture.middle);
    assert_eq!(flow.rows()[1].from(), fixture.middle);
    assert_eq!(flow.rows()[1].to(), fixture.provider);
    assert!(!flow.truncated());

    let impact = execute_workflow(
        &fixture.snapshot,
        &fixture.overlay,
        WorkflowRequest::new(WorkflowKind::Impact, budget).with_seed(fixture.provider),
        &Cancellation::new(),
    )
    .expect("impact traversal should succeed");
    assert_eq!(impact.rows().len(), 2);
    assert_eq!(impact.rows()[0].from(), fixture.provider);
    assert_eq!(impact.rows()[1].to(), fixture.consumer);
    assert!(
        impact
            .repository_usage()
            .iter()
            .all(|usage| usage.rows() > 0)
    );
}

#[test]
fn row_budget_returns_snapshot_bound_continuations_without_duplicate_rows() {
    let fixture = chain_fixture(false);
    let budget = WorkflowBudget::new(4, 1, 10, 4, 4).expect("budget should be valid");
    let first = execute_workflow(
        &fixture.snapshot,
        &fixture.overlay,
        WorkflowRequest::new(WorkflowKind::Flow, budget).with_seed(fixture.consumer),
        &Cancellation::new(),
    )
    .expect("first page should succeed");
    let continuation = first
        .continuation()
        .expect("row truncation should return a continuation");
    assert!(first.truncated());
    assert_eq!(first.rows().len(), 1);
    assert_eq!(continuation.offset(), 1);

    let second = execute_workflow(
        &fixture.snapshot,
        &fixture.overlay,
        WorkflowRequest::new(WorkflowKind::Flow, budget)
            .with_seed(fixture.consumer)
            .with_continuation(continuation),
        &Cancellation::new(),
    )
    .expect("second page should succeed");
    assert_eq!(second.rows().len(), 1);
    assert_ne!(first.rows()[0].id(), second.rows()[0].id());
    assert!(!second.truncated());
    assert!(second.continuation().is_none());
}

#[test]
fn hard_traversal_limits_and_partial_snapshots_are_explicit_without_continuations() {
    let fixture = chain_fixture(false);
    let per_repository = WorkflowBudget::new(4, 10, 10, 4, 4)
        .expect("budget should be valid")
        .with_response_limits(1, 0, 4 * 1024 * 1024, 1_000_000)
        .expect("response limits should be valid");
    let limited = execute_workflow(
        &fixture.snapshot,
        &fixture.overlay,
        WorkflowRequest::new(WorkflowKind::Flow, per_repository).with_seed(fixture.consumer),
        &Cancellation::new(),
    )
    .expect("bounded traversal should return its safe prefix");
    assert!(limited.truncated());
    assert_eq!(limited.rows().len(), 1);
    assert!(limited.continuation().is_none());

    let partial = chain_fixture(true);
    let result = execute_workflow(
        &partial.snapshot,
        &partial.overlay,
        WorkflowRequest::new(WorkflowKind::Flow, WorkflowBudget::default())
            .with_seed(partial.consumer),
        &Cancellation::new(),
    )
    .expect("available repositories should still produce a partial workflow");
    assert_eq!(result.rows().len(), 2);
    assert_eq!(result.repository_failures().len(), 1);
    assert!(result.truncated());
    assert!(result.continuation().is_none());
}

#[test]
fn workflows_fail_closed_for_unpinned_seeds_stale_continuations_and_cancellation() {
    let fixture = chain_fixture(false);
    let budget = WorkflowBudget::new(4, 1, 10, 4, 4).expect("budget should be valid");
    assert!(matches!(
        execute_workflow(
            &fixture.snapshot,
            &fixture.overlay,
            WorkflowRequest::new(WorkflowKind::Flow, budget).with_seed(endpoint(3, 30, 3)),
            &Cancellation::new()
        ),
        Err(WorkflowError::SeedOutsideSnapshot)
    ));
    let first = execute_workflow(
        &fixture.snapshot,
        &fixture.overlay,
        WorkflowRequest::new(WorkflowKind::Flow, budget).with_seed(fixture.consumer),
        &Cancellation::new(),
    )
    .expect("first page should succeed");
    let continuation = first
        .continuation()
        .expect("first page should provide a continuation");
    assert!(matches!(
        execute_workflow(
            &fixture.snapshot,
            &fixture.overlay,
            WorkflowRequest::new(WorkflowKind::Impact, budget)
                .with_seed(fixture.consumer)
                .with_continuation(continuation),
            &Cancellation::new()
        ),
        Err(WorkflowError::ContinuationMismatch)
    ));

    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        execute_workflow(
            &fixture.snapshot,
            &fixture.overlay,
            WorkflowRequest::new(WorkflowKind::Flow, budget).with_seed(fixture.consumer),
            &cancellation
        ),
        Err(WorkflowError::Cancelled(_))
    ));
}
