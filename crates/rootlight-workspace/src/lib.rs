//! Immutable multi-repository snapshots and bounded cross-repository overlays.
//!
//! The crate owns no filesystem, network, storage, or process capability. Hosts
//! provide validated identities and exact generation availability explicitly.

#![forbid(unsafe_code)]

mod catalog;
mod identity;
mod link;
mod snapshot;
mod workflow;

pub use catalog::{
    CatalogError, CatalogLimits, CatalogRepository, PackageDescriptor, PackageId,
    RepositoryDescriptor, RepositoryState, RepositoryTopology, WorkspaceAlias, WorkspaceCatalog,
};
pub use identity::{
    CrossLinkVersion, RepositoryRootIdentity, SharedContentIdentity, WorkspaceId,
    WorkspaceSnapshotId,
};
pub use link::{
    CrossRepositoryCandidate, CrossRepositoryLink, LinkCaveat, LinkDeclaration, LinkDirection,
    LinkError, LinkKind, LinkLimits, LinkOverlay, LinkOverlayId, ServiceKey, WorkspaceFactRef,
    build_link_overlay,
};
pub use snapshot::{
    SnapshotBuildMode, SnapshotError, SnapshotFailure, SnapshotFailureKind, SnapshotLimits,
    SnapshotMember, WorkspaceSnapshot, WorkspaceSnapshotRequest,
};
pub use workflow::{
    RepositoryUsage, WorkflowBudget, WorkflowContinuation, WorkflowEdge, WorkflowError,
    WorkflowKind, WorkflowRequest, WorkflowResult, execute_workflow,
};
