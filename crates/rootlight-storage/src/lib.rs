//! Backend-neutral contracts for immutable Rootlight generations.
//!
//! Keeping these contracts outside `rootlight-ir` leaves normalized-model
//! ownership there while SQLite and future backends share one bounded boundary.

#![forbid(unsafe_code)]

mod generation;

pub use generation::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationBudgetError, GenerationContext,
    GenerationContractVersion, GenerationControlError, GenerationMetadata, GenerationReader,
    GenerationResource, GenerationSnapshot, GenerationStats, GenerationValidationError,
    GenerationWriter, HARD_MAX_GENERATION_ROWS, HARD_MAX_GENERATION_SOURCE_REFS,
    HARD_MAX_GENERATION_TEXT_BYTES,
};
