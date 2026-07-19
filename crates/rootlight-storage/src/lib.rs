//! Backend-neutral contracts for immutable Rootlight generations.
//!
//! Keeping these contracts outside `rootlight-ir` leaves normalized-model
//! ownership there while SQLite and future backends share one bounded boundary.

#![forbid(unsafe_code)]

mod generation;
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
