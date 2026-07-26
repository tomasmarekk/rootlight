//! Contract tests for independent repository lifecycles and immutable snapshots.
//!
//! Fixtures use opaque identities only; no test grants filesystem or discovery
//! capabilities to the workspace layer.

use proptest::prelude::*;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use rootlight_workspace::{
    CatalogError, CatalogLimits, CrossLinkVersion, PackageDescriptor, PackageId,
    RepositoryDescriptor, RepositoryRootIdentity, RepositoryState, RepositoryTopology,
    SharedContentIdentity, SnapshotBuildMode, SnapshotError, SnapshotFailureKind, SnapshotLimits,
    WorkspaceAlias, WorkspaceCatalog, WorkspaceId, WorkspaceSnapshot, WorkspaceSnapshotRequest,
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

fn descriptor(seed: u8) -> RepositoryDescriptor {
    RepositoryDescriptor::new(
        repository(seed),
        RepositoryRootIdentity::from_hash(hash(seed)),
        SharedContentIdentity::from_hash(hash(seed.saturating_add(64))),
    )
}

fn request(entries: &[(u8, u8)]) -> WorkspaceSnapshotRequest {
    entries.iter().fold(
        WorkspaceSnapshotRequest::new(hash(200), CrossLinkVersion::from_hash(hash(201)), 10, 20),
        |request, (repository, generation)| {
            request.with_member(self::repository(*repository), self::generation(*generation))
        },
    )
}

fn populated_catalog(entries: &[(u8, u8)]) -> WorkspaceCatalog {
    let cancellation = Cancellation::new();
    let mut catalog =
        WorkspaceCatalog::new(WorkspaceId::from_hash(hash(199)), CatalogLimits::default());
    for (repository, generation) in entries {
        catalog
            .register(descriptor(*repository), &cancellation)
            .expect("fixture repository registration should succeed");
        catalog
            .publish_generation(
                self::repository(*repository),
                self::generation(*generation),
                &cancellation,
            )
            .expect("fixture generation publication should succeed");
    }
    catalog
}

#[test]
fn catalog_preserves_topology_aliases_packages_and_independent_state() {
    let cancellation = Cancellation::new();
    let mut catalog =
        WorkspaceCatalog::new(WorkspaceId::from_hash(hash(199)), CatalogLimits::default());
    let primary = descriptor(1)
        .with_topology(RepositoryTopology::Monorepo)
        .with_alias(WorkspaceAlias::new("primary").expect("fixture alias should be valid"))
        .with_package(PackageDescriptor::new(
            PackageId::from_hash(hash(40)),
            hash(41),
            hash(42),
        ));
    catalog
        .register(primary, &cancellation)
        .expect("primary registration should succeed");
    let worktree = RepositoryDescriptor::new(
        repository(2),
        RepositoryRootIdentity::from_hash(hash(2)),
        SharedContentIdentity::from_hash(hash(65)),
    )
    .with_topology(RepositoryTopology::Worktree(repository(1)))
    .with_alias(WorkspaceAlias::new("review").expect("fixture alias should be valid"));
    catalog
        .register(worktree, &cancellation)
        .expect("worktree registration should succeed");
    catalog
        .publish_generation(repository(1), generation(1), &cancellation)
        .expect("primary publication should succeed");
    catalog
        .publish_generation(repository(2), generation(2), &cancellation)
        .expect("worktree publication should succeed");

    let primary = catalog
        .repository(repository(1))
        .expect("primary repository should remain registered");
    assert_eq!(primary.topology(), RepositoryTopology::Monorepo);
    assert_eq!(primary.packages().len(), 1);
    assert_eq!(
        catalog.repository_for_alias(
            &WorkspaceAlias::new("review").expect("fixture alias should be valid")
        ),
        Some(repository(2))
    );

    catalog
        .set_state(repository(1), RepositoryState::Corrupt)
        .expect("availability transition should succeed");
    assert_eq!(
        catalog
            .repository(repository(2))
            .expect("worktree should remain registered")
            .state(),
        RepositoryState::Ready
    );
    assert!(
        catalog
            .repository(repository(2))
            .expect("worktree should remain registered")
            .retains(generation(2))
    );
}

#[test]
fn catalog_rejects_identity_collisions_and_inconsistent_worktrees_atomically() {
    let cancellation = Cancellation::new();
    let mut catalog =
        WorkspaceCatalog::new(WorkspaceId::from_hash(hash(199)), CatalogLimits::default());
    catalog
        .register(
            descriptor(1)
                .with_alias(WorkspaceAlias::new("one").expect("fixture alias should be valid")),
            &cancellation,
        )
        .expect("primary registration should succeed");
    let before = catalog.repositories().len();

    let duplicate_alias = descriptor(2)
        .with_alias(WorkspaceAlias::new("one").expect("fixture alias should be valid"));
    assert!(matches!(
        catalog.register(duplicate_alias, &cancellation),
        Err(CatalogError::DuplicateAlias)
    ));
    let inconsistent_worktree =
        descriptor(3).with_topology(RepositoryTopology::Worktree(repository(1)));
    assert!(matches!(
        catalog.register(inconsistent_worktree, &cancellation),
        Err(CatalogError::WorktreeContentMismatch)
    ));
    assert_eq!(catalog.repositories().len(), before);
    assert!(catalog.repository(repository(2)).is_none());
    assert!(catalog.repository(repository(3)).is_none());
}

#[test]
fn snapshots_pin_retained_generations_across_independent_advances() {
    let cancellation = Cancellation::new();
    let mut catalog = populated_catalog(&[(1, 11), (2, 21)]);
    let snapshot = WorkspaceSnapshot::build(
        &catalog,
        request(&[(2, 21), (1, 11)]),
        SnapshotBuildMode::Strict,
        SnapshotLimits::default(),
        &cancellation,
    )
    .expect("complete snapshot should build");
    assert!(snapshot.is_complete());
    assert_eq!(snapshot.members()[0].repository(), repository(1));

    catalog
        .publish_generation(repository(1), generation(12), &cancellation)
        .expect("independent advance should succeed");
    snapshot
        .validate(&catalog, 15, &cancellation)
        .expect("retained generation should keep the snapshot valid");
    assert_eq!(snapshot.generation_for(repository(1)), Some(generation(11)));

    catalog
        .reclaim_generation(repository(1), generation(11), &cancellation)
        .expect("noncurrent generation should be reclaimable");
    assert!(matches!(
        snapshot.validate(&catalog, 15, &cancellation),
        Err(SnapshotError::MemberInvalidated)
    ));
    assert!(
        catalog
            .repository(repository(2))
            .expect("unrelated repository should remain registered")
            .retains(generation(21))
    );
}

#[test]
fn partial_snapshots_expose_every_omission_without_false_completeness() {
    let cancellation = Cancellation::new();
    let mut catalog = populated_catalog(&[(1, 11), (2, 21)]);
    catalog
        .set_authorized(repository(2), false)
        .expect("fixture authorization change should succeed");

    assert!(matches!(
        WorkspaceSnapshot::build(
            &catalog,
            request(&[(1, 11), (2, 21), (3, 31)]),
            SnapshotBuildMode::Strict,
            SnapshotLimits::default(),
            &cancellation,
        ),
        Err(SnapshotError::StrictMemberFailure)
    ));
    let partial = WorkspaceSnapshot::build(
        &catalog,
        request(&[(1, 11), (2, 21), (3, 31)]),
        SnapshotBuildMode::AllowPartial,
        SnapshotLimits::default(),
        &cancellation,
    )
    .expect("partial snapshot should retain its available member");
    assert!(!partial.is_complete());
    assert_eq!(partial.requested_members(), 3);
    assert_eq!(partial.members().len(), 1);
    assert_eq!(
        partial
            .failures()
            .iter()
            .map(|failure| failure.kind())
            .collect::<Vec<_>>(),
        vec![
            SnapshotFailureKind::Unauthorized,
            SnapshotFailureKind::MissingRepository
        ]
    );
}

#[test]
fn reindexing_keeps_retained_generations_available_and_deletion_is_isolated() {
    let cancellation = Cancellation::new();
    let mut catalog = populated_catalog(&[(1, 11), (2, 21)]);
    catalog
        .set_state(repository(1), RepositoryState::Reindexing)
        .expect("reindexing transition should succeed");
    let reindexing = WorkspaceSnapshot::build(
        &catalog,
        request(&[(1, 11), (2, 21)]),
        SnapshotBuildMode::Strict,
        SnapshotLimits::default(),
        &cancellation,
    )
    .expect("retained reindexing generation should remain usable");
    assert!(reindexing.is_complete());

    catalog
        .set_state(repository(1), RepositoryState::Deleted)
        .expect("deletion should succeed");
    let partial = WorkspaceSnapshot::build(
        &catalog,
        request(&[(1, 11), (2, 21)]),
        SnapshotBuildMode::AllowPartial,
        SnapshotLimits::default(),
        &cancellation,
    )
    .expect("unrelated repository should still produce a partial snapshot");
    assert_eq!(partial.members()[0].repository(), repository(2));
    assert_eq!(partial.failures()[0].kind(), SnapshotFailureKind::Deleted);
}

#[test]
fn cancelled_catalog_and_snapshot_operations_fail_before_mutation() {
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    let mut catalog =
        WorkspaceCatalog::new(WorkspaceId::from_hash(hash(199)), CatalogLimits::default());
    assert!(matches!(
        catalog.register(descriptor(1), &cancellation),
        Err(CatalogError::Cancelled(_))
    ));
    assert_eq!(catalog.repositories().len(), 0);
    assert!(matches!(
        WorkspaceSnapshot::build(
            &catalog,
            request(&[(1, 11)]),
            SnapshotBuildMode::AllowPartial,
            SnapshotLimits::default(),
            &cancellation,
        ),
        Err(SnapshotError::Cancelled(_))
    ));
}

proptest! {
    #[test]
    fn snapshot_identity_and_encoding_ignore_request_order(
        priorities in prop::array::uniform3(any::<u8>()),
    ) {
        let cancellation = Cancellation::new();
        let catalog = populated_catalog(&[(1, 11), (2, 21), (3, 31)]);
        let mut entries = vec![(1, 11), (2, 21), (3, 31)];
        entries.sort_by_key(|(repository, _)| (priorities[usize::from(*repository) - 1], *repository));
        let shuffled = WorkspaceSnapshot::build(
            &catalog,
            request(&entries),
            SnapshotBuildMode::Strict,
            SnapshotLimits::default(),
            &cancellation,
        ).expect("shuffled request should build");
        let canonical = WorkspaceSnapshot::build(
            &catalog,
            request(&[(1, 11), (2, 21), (3, 31)]),
            SnapshotBuildMode::Strict,
            SnapshotLimits::default(),
            &cancellation,
        ).expect("canonical request should build");

        prop_assert_eq!(shuffled.id(), canonical.id());
        prop_assert_eq!(
            serde_json::to_vec(&shuffled).expect("snapshot serialization should succeed"),
            serde_json::to_vec(&canonical).expect("snapshot serialization should succeed"),
        );
    }
}
