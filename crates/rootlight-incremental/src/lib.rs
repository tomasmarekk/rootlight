//! Dependency-directed invalidation and clean-equivalence primitives.
//!
//! The crate owns versioned incremental inputs, authoritative metadata
//! reconciliation, bounded closure planning, artifact reuse, and source-free traces.

#![forbid(unsafe_code)]

mod dependency;
mod equivalence;
mod error;
mod model;
mod plan;
mod reconcile;

pub use dependency::{
    DependencyEdge, DependencyGraph, DependencyRegistry, DependencySource, PassDeclaration,
    PassObservation,
};
pub use equivalence::{
    EquivalenceMismatch, EquivalenceReport, EquivalenceSnapshot, LogicalComponent, LogicalDomain,
};
pub use error::{IncrementalError, ResourceKind};
pub use model::{
    AnalysisUnitId, ArtifactId, ArtifactSummary, ChangeClass, ChangeSet, FactDomain, FactDomainSet,
    FactNode, GenerationSummary, GraphLimits, InputDelta, InputFingerprint, InputKey, InputKind,
    InputSnapshot, PassId, PlanningLimits,
};
pub use plan::{
    ArtifactDecision, ArtifactDecisionKind, ConservativeFallback, FallbackReason, InvalidationPlan,
    InvalidationTrace, TraceAction, TraceEntry, TraceReason, TraceTarget, plan_invalidation,
};
pub use reconcile::{
    AuthoritativeScan, BaselineFile, FileChange, FileChangeKind, FileDescriptor, FileMetadata,
    HashDecision, HashDecisionReason, MetadataBaseline, MetadataReliability, PlatformFileIdentity,
    ReconcileLimits, ReconcileMode, ReconcileOutcome, ReconcilePlan, ScannedFile, plan_reconcile,
};

/// Version of typed input keys, dependency declarations, and invalidation traces.
pub const INCREMENTAL_SCHEMA_VERSION: &str = "1.0";
