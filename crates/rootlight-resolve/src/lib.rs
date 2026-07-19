//! Deterministic, bounded semantic resolution over normalized Rootlight facts.
//!
//! The crate resolves language-neutral candidate sets without executing
//! repository content or hiding ambiguity behind exact relations.

#![forbid(unsafe_code)]

mod engine;
mod model;

pub use engine::ResolutionEngine;
pub use model::{
    CandidateExplanation, CompletenessAssumption, DEFAULT_CANDIDATE_LIMIT, MAX_CANDIDATE_LIMIT,
    RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, RejectedCandidate, RejectionReason,
    ResolutionBatch, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionLimitError, ResolutionLimits, ResolutionOutcome, ResolutionPenalty, ResolutionRule,
    ResolutionSignal, UnresolvedReason,
};
