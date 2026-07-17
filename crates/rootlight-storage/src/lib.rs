//! Backend-neutral contracts for immutable Rootlight generations.
//!
//! This crate owns versioning, owned boundary data, and bounded cancellation
//! context without depending on SQLite, segment, query, or publication code.

#![forbid(unsafe_code)]

mod generation;

pub use generation::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationBudgetError, GenerationContext,
    GenerationContractVersion, GenerationControlError, GenerationMetadata, GenerationReader,
    GenerationResource, GenerationSection, GenerationSnapshot, GenerationStats,
    GenerationValidationError, GenerationWriter, HARD_MAX_GENERATION_ROWS,
    HARD_MAX_GENERATION_SOURCE_REFS, HARD_MAX_GENERATION_TEXT_BYTES,
};
