//! Deterministic, bounded semantic resolution over normalized Rootlight facts.
//!
//! The crate resolves language-neutral candidate sets without executing
//! repository content or hiding ambiguity behind exact relations.

#![forbid(unsafe_code)]

mod application;
mod engine;
mod foreign;
mod model;
mod quality;

pub use engine::ResolutionEngine;
pub use foreign::{
    AppliedForeignLinks, ForeignLinkBatch, ForeignLinkDecision, ForeignLinkEngine,
    ForeignLinkError, ForeignLinkInput, ForeignLinkInputError, ForeignLinkLimitError,
    ForeignLinkLimits, ForeignLinkNamespace, ForeignLinkOutcome, MAX_FOREIGN_LINK_INPUTS,
    MIN_EXACT_FOREIGN_LINK_CONFIDENCE,
};
pub use model::{
    AppliedResolution, CandidateExplanation, CompletenessAssumption, DEFAULT_CANDIDATE_LIMIT,
    DynamicCallCalibration, DynamicCallCalibrationError, MAX_CANDIDATE_LIMIT,
    MIN_DYNAMIC_CALL_PRECISION_BASIS_POINTS, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION,
    RejectedCandidate, RejectionReason, ResolutionBatch, ResolutionDecision, ResolutionError,
    ResolutionExplanation, ResolutionLimitError, ResolutionLimits, ResolutionOutcome,
    ResolutionPenalty, ResolutionPolicy, ResolutionRule, ResolutionSignal, ResolverFactContext,
    UnresolvedReason,
};
pub use quality::{
    CalibrationBin, CalibrationReport, ExpectedResolution, QualityError, QualityRatio,
    ResolutionExpectation, ResolutionQualityReport, evaluate_resolution_quality,
};
