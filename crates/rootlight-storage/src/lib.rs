//! Backend-neutral contracts for immutable Rootlight generations.
//!
//! Keeping these contracts outside `rootlight-ir` leaves normalized-model
//! ownership there while SQLite and future backends share one bounded boundary.

#![forbid(unsafe_code)]

mod generation;
mod lifecycle;
mod publication;
mod reader;

pub use generation::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationBudgetError, GenerationContext,
    GenerationContractVersion, GenerationControlError, GenerationManifestRecipe,
    GenerationMetadata, GenerationResource, GenerationSnapshot, GenerationSnapshotError,
    GenerationStats, GenerationValidationError, GenerationWriter, HARD_MAX_GENERATION_ROWS,
    HARD_MAX_GENERATION_SOURCE_REFS, HARD_MAX_GENERATION_TEXT_BYTES, IdentityVerificationError,
    IdentityVerifiedGeneration, PROPOSED_IDENTITY_CLAIM_VERSION,
};
pub use lifecycle::{
    DiskReservation, GenerationFootprint, GenerationLease, GenerationLifecycle, GenerationSequence,
    HARD_MAX_GENERATION_LEASES, HARD_MAX_LIFECYCLE_BYTES, HARD_MAX_LIFECYCLE_GENERATIONS,
    HARD_MAX_RECLAIM_GENERATIONS, LeaseTick, LifecycleError, LifecycleLimits,
    PublicationSpaceEstimate, ReclaimPlan, ReclaimReceipt, ReclaimTransaction, RetentionPolicy,
};
pub use publication::{
    GenerationArtifact, GenerationArtifactKind, HARD_MAX_PUBLICATION_ARTIFACT_BYTES,
    HARD_MAX_PUBLICATION_ARTIFACTS, HARD_MAX_PUBLICATION_MANIFEST_BYTES,
    HARD_MAX_PUBLICATION_MARKER_BYTES, PUBLICATION_MANIFEST_SCHEMA, PUBLICATION_MARKER_SCHEMA,
    PublicationCorruption, PublicationError, PublicationExpectation, PublicationLimits,
    PublicationManifest, PublicationMarker, PublicationStage, RecoveryState, VerifiedPublication,
    classify_recovery, verify_publication,
};
pub use reader::{
    CoverageReadRequest, GenerationReadLimit, GenerationReadLimitError, GenerationReader,
    HARD_MAX_GENERATION_READ_ITEMS, OccurrenceReadRequest, ReadPage, ReadPageCompleteness,
    ReadPageError, RelationReadDirection, RelationReadRequest, RelationReadRequestError,
};
