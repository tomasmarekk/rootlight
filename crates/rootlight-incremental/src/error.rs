//! Typed failures for incremental reconciliation and planning.
//!
//! Errors carry stable identifiers and counts only; repository paths and source
//! bodies never cross this crate boundary.

use rootlight_cancel::Cancelled;
use rootlight_ids::{FactId, FileId};

use crate::{ArtifactId, FactDomain, FactNode, InputKey, InputKind, LogicalDomain, PassId};

/// A bounded resource whose configured or observed size was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// Files in one metadata baseline or authoritative scan.
    Files,
    /// Typed generation input fingerprints.
    Inputs,
    /// Reusable generation artifacts.
    Artifacts,
    /// Declared analysis passes.
    Passes,
    /// Scoped fact nodes in the dependency graph.
    DependencyNodes,
    /// Typed edges in the dependency graph.
    DependencyEdges,
    /// Edge visits while computing a fixed-point closure.
    ClosureWork,
    /// Source-free invalidation trace entries.
    TraceEntries,
    /// Canonical bytes accepted for one logical equivalence component.
    LogicalBytes,
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Files => "files",
            Self::Inputs => "inputs",
            Self::Artifacts => "artifacts",
            Self::Passes => "passes",
            Self::DependencyNodes => "dependency nodes",
            Self::DependencyEdges => "dependency edges",
            Self::ClosureWork => "closure work",
            Self::TraceEntries => "trace entries",
            Self::LogicalBytes => "logical bytes",
        })
    }
}

/// Failures returned by bounded incremental contracts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IncrementalError {
    /// Cooperative cancellation or a monotonic deadline stopped the operation.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// A configured limit was zero or exceeded its hard ceiling.
    #[error("invalid {resource} limit {value}; hard maximum is {hard_maximum}")]
    InvalidLimit {
        /// Rejected resource class.
        resource: ResourceKind,
        /// Rejected configured value.
        value: usize,
        /// Compile-time hard safety ceiling.
        hard_maximum: usize,
    },
    /// An observed bounded collection exceeded its configured ceiling.
    #[error("{resource} count {observed} exceeds limit {limit}")]
    ResourceLimit {
        /// Exhausted resource class.
        resource: ResourceKind,
        /// Observed count or byte length.
        observed: usize,
        /// Configured ceiling.
        limit: usize,
    },
    /// A pass identifier was empty, too long, or used an unsafe character.
    #[error("invalid incremental pass identifier")]
    InvalidPassId,
    /// One generation input key appeared more than once.
    #[error("duplicate generation input key {key:?}")]
    DuplicateInput {
        /// Duplicate typed key.
        key: InputKey,
    },
    /// One artifact identity appeared more than once.
    #[error("duplicate artifact identity {artifact:?}")]
    DuplicateArtifact {
        /// Duplicate artifact.
        artifact: ArtifactId,
    },
    /// One artifact repeated an output node.
    #[error("artifact {artifact:?} repeats output node {node:?}")]
    DuplicateArtifactOutput {
        /// Artifact carrying the duplicate.
        artifact: ArtifactId,
        /// Duplicate scoped fact node.
        node: FactNode,
    },
    /// One artifact repeated an input dependency.
    #[error("artifact {artifact:?} repeats dependency {key:?}")]
    DuplicateArtifactDependency {
        /// Artifact carrying the duplicate.
        artifact: ArtifactId,
        /// Duplicate typed input.
        key: InputKey,
    },
    /// An artifact omitted all outputs or all dependencies.
    #[error("artifact {artifact:?} has an empty {part}")]
    EmptyArtifactPart {
        /// Invalid artifact.
        artifact: ArtifactId,
        /// Static name of the missing part.
        part: &'static str,
    },
    /// An artifact dependency did not match the parent generation inputs.
    #[error("artifact {artifact:?} dependency {key:?} is absent or mismatched")]
    ArtifactDependencyMismatch {
        /// Invalid artifact.
        artifact: ArtifactId,
        /// Mismatched dependency.
        key: InputKey,
    },
    /// An artifact referenced a fact node absent from the dependency graph.
    #[error("artifact {artifact:?} references unknown output node {node:?}")]
    ArtifactUnknownOutput {
        /// Invalid artifact.
        artifact: ArtifactId,
        /// Unknown output node.
        node: FactNode,
    },
    /// One pass identity appeared more than once.
    #[error("duplicate incremental pass declaration {pass}")]
    DuplicatePass {
        /// Duplicate pass identity.
        pass: PassId,
    },
    /// A pass declaration omitted all output domains.
    #[error("incremental pass {pass} declares no output domains")]
    EmptyPassOutputs {
        /// Invalid pass identity.
        pass: PassId,
    },
    /// A pass used an input kind absent from its declaration.
    #[error("incremental pass {pass} did not declare input kind {kind:?}")]
    UndeclaredInputKind {
        /// Pass whose observation violated its declaration.
        pass: PassId,
        /// Missing input kind.
        kind: InputKind,
    },
    /// A pass used an input fact domain absent from its declaration.
    #[error("incremental pass {pass} did not declare input domain {domain:?}")]
    UndeclaredInputDomain {
        /// Pass whose observation violated its declaration.
        pass: PassId,
        /// Missing input domain.
        domain: FactDomain,
    },
    /// A pass produced a fact domain absent from its declaration.
    #[error("incremental pass {pass} did not declare output domain {domain:?}")]
    UndeclaredOutputDomain {
        /// Pass whose observation violated its declaration.
        pass: PassId,
        /// Missing output domain.
        domain: FactDomain,
    },
    /// A dependency edge named an unknown pass.
    #[error("dependency edge names unknown pass {pass}")]
    UnknownPass {
        /// Unknown pass identity.
        pass: PassId,
    },
    /// A dependency edge referenced a fact node absent from the graph.
    #[error("dependency edge references unknown fact node {node:?}")]
    UnknownFactNode {
        /// Unknown node.
        node: FactNode,
    },
    /// One file identity appeared more than once in a baseline or scan.
    #[error("duplicate file identity {file}")]
    DuplicateFile {
        /// Duplicate file identity.
        file: FileId,
    },
    /// Two files used the same canonical path identity hash.
    #[error("canonical path identity hash collision between {first} and {second}")]
    PathIdentityCollision {
        /// First file owning the digest.
        first: FileId,
        /// Second file owning the digest.
        second: FileId,
    },
    /// A reconcile completion omitted a requested content hash.
    #[error("missing requested content hash for {file}")]
    MissingHash {
        /// File that still requires hashing.
        file: FileId,
    },
    /// A reconcile completion supplied a hash that was not requested.
    #[error("unexpected content hash for {file}")]
    UnexpectedHash {
        /// File that should not have been hashed for this plan.
        file: FileId,
    },
    /// One logical equivalence component appeared more than once.
    #[error("duplicate logical equivalence component {domain:?}")]
    DuplicateLogicalDomain {
        /// Duplicate component domain.
        domain: LogicalDomain,
    },
    /// A required logical equivalence component was absent.
    #[error("missing logical equivalence component {domain:?}")]
    MissingLogicalDomain {
        /// Missing component domain.
        domain: LogicalDomain,
    },
    /// Incremental and clean logical components were not equal.
    #[error("incremental and clean logical snapshots differ")]
    LogicalInequality,
    /// Canonical trace serialization failed unexpectedly.
    #[error("failed to serialize incremental trace")]
    SerializeTrace(#[source] serde_json::Error),
    /// A generic stable fact identity was duplicated where uniqueness is required.
    #[error("duplicate incremental identity {id}")]
    DuplicateIdentity {
        /// Duplicate stable identity.
        id: FactId,
    },
}
