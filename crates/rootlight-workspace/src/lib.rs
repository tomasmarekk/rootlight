//! Immutable multi-repository snapshots and bounded cross-repository overlays.
//!
//! The crate owns no filesystem, network, storage, or process capability. Hosts
//! provide validated identities and exact generation availability explicitly.

#![forbid(unsafe_code)]

mod catalog;
mod identity;
mod snapshot;

pub use catalog::{
    CatalogError, CatalogLimits, CatalogRepository, PackageDescriptor, PackageId,
    RepositoryDescriptor, RepositoryState, RepositoryTopology, WorkspaceAlias, WorkspaceCatalog,
};
pub use identity::{
    CrossLinkVersion, RepositoryRootIdentity, SharedContentIdentity, WorkspaceId,
    WorkspaceSnapshotId,
};
pub use snapshot::{
    SnapshotBuildMode, SnapshotError, SnapshotFailure, SnapshotFailureKind, SnapshotLimits,
    SnapshotMember, WorkspaceSnapshot, WorkspaceSnapshotRequest,
};
