//! Stable, source-free failures exposed by the adapter boundary.
//!
//! Errors retain typed limit and sequence context without retaining repository
//! paths, source text, parser diagnostics, or adapter-owned payloads.

use crate::sink::DiagnosticCode;
use rootlight_cancel::{CancellationReason, Cancelled};
use rootlight_ir::AnalysisTier;

/// A bounded label field understood by the SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LabelField {
    /// Normalized language identity.
    Language,
    /// Declared source encoding.
    Encoding,
    /// Parser-independent syntax kind.
    SyntaxKind,
    /// Stable diagnostic code.
    DiagnosticCode,
}

/// The invariant violated by a bounded label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LabelViolation {
    /// The label was empty.
    Empty,
    /// The label exceeded its fixed byte ceiling.
    TooLong,
    /// The label contained a byte outside its source-free grammar.
    InvalidByte,
}

/// Failure to construct a bounded source-free label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid {field:?} label: {violation:?}")]
pub struct LabelError {
    /// Label category that failed validation.
    pub field: LabelField,
    /// Stable reason the label was rejected.
    pub violation: LabelViolation,
}

/// Invalid provider capability or producer metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DescriptorError {
    /// A descriptor collection was empty.
    #[error("{collection} capability collection must not be empty")]
    EmptyCollection {
        /// Stable collection name.
        collection: &'static str,
    },
    /// A descriptor collection exceeded its fixed item ceiling.
    #[error("{collection} contains {observed} items, limit is {limit}")]
    TooManyItems {
        /// Stable collection name.
        collection: &'static str,
        /// Observed item count.
        observed: usize,
        /// Fixed SDK ceiling.
        limit: usize,
    },
    /// A required numeric capability was zero.
    #[error("{field} capability must be nonzero")]
    ZeroMaximum {
        /// Stable capability field.
        field: &'static str,
    },
}

/// Invalid resource-limit configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LimitError {
    /// A required limit was zero.
    #[error("{field} limit must be nonzero")]
    Zero {
        /// Stable limit field.
        field: &'static str,
    },
    /// A per-batch limit exceeded the corresponding stream limit.
    #[error("{field} batch limit {batch} exceeds stream limit {stream}")]
    BatchExceedsStream {
        /// Stable resource field.
        field: &'static str,
        /// Per-batch maximum.
        batch: usize,
        /// Whole-stream maximum.
        stream: usize,
    },
}

/// Failure to bind VFS bytes to one full-file generation reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// The file identities differed.
    #[error("snapshot file does not match source reference")]
    FileMismatch,
    /// The immutable content hashes differed.
    #[error("snapshot content hash does not match source reference")]
    ContentHashMismatch,
    /// The source reference did not cover exactly the full file.
    #[error("source reference must cover the full snapshot")]
    NotFullFile,
    /// The captured byte length could not be represented by the IR span.
    #[error("snapshot byte length is not representable")]
    LengthOverflow,
}

/// Invalid per-file parse or analysis request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RequestError {
    /// Snapshot and generation reference failed to bind.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// The source file exceeded the requested analysis bound.
    #[error("source contains {observed} bytes, limit is {limit}")]
    SourceTooLarge {
        /// Captured source bytes.
        observed: usize,
        /// Requested source maximum.
        limit: usize,
    },
    /// Too many embedded source ranges were requested.
    #[error("request contains {observed} included ranges, limit is {limit}")]
    TooManyIncludedRanges {
        /// Included range count.
        observed: usize,
        /// Requested maximum.
        limit: usize,
    },
    /// An included range named another file or exceeded the source span.
    #[error("included range {index} is outside the bound source")]
    IncludedRangeOutsideSource {
        /// Zero-based range index.
        index: usize,
    },
    /// Included ranges were not strictly ordered and disjoint.
    #[error("included range {index} overlaps or precedes its predecessor")]
    IncludedRangeOrder {
        /// Zero-based range index.
        index: usize,
    },
    /// The selected provider does not advertise the requested language.
    #[error("provider does not support the requested language")]
    UnsupportedLanguage,
    /// The selected parser does not advertise the requested encoding.
    #[error("provider does not support the requested encoding")]
    UnsupportedEncoding,
    /// Embedded ranges were supplied to a provider without that capability.
    #[error("provider does not support embedded included ranges")]
    EmbeddedRangesUnsupported,
    /// The analyzer cannot satisfy the requested analysis tier.
    #[error("provider does not support the requested analysis tier")]
    UnsupportedTier,
    /// An analyzer requiring source classification received no generated status.
    #[error("analysis request requires an explicit generated-source classification")]
    GeneratedStatusRequired,
    /// The invocation omitted the required process-local monotonic deadline.
    #[error("adapter invocation requires a monotonic deadline")]
    DeadlineRequired,
    /// The parser cannot cooperatively observe cancellation while parsing.
    #[error("parser provider does not advertise cancellation checkpoints")]
    CancellationCheckpointsRequired,
    /// The selected memory policy rejected unavailable enforcement.
    #[error("provider memory enforcement is unavailable under the selected admission policy")]
    MemoryEnforcementUnavailable,
    /// The request exceeded a provider's advertised capability.
    #[error("{resource:?} request bound {observed} exceeds provider maximum {limit}")]
    ProviderLimit {
        /// Advertised resource.
        resource: ResourceKind,
        /// Requested or observed value.
        observed: usize,
        /// Provider maximum.
        limit: usize,
    },
}

/// A resource category used by stable quota errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceKind {
    /// Stream batches.
    Batches,
    /// Top-level facts or records.
    Records,
    /// Deterministically accounted logical output bytes.
    OutputBytes,
    /// Diagnostic records.
    Diagnostics,
    /// UTF-8 bytes in diagnostic codes and messages.
    DiagnosticBytes,
    /// UTF-8 bytes in non-payload strings.
    StringBytes,
    /// Nested IR collection items.
    NestedItems,
    /// UTF-8 extension payload bytes.
    ExtensionBytes,
    /// Source bytes.
    SourceBytes,
    /// Embedded included source ranges.
    IncludedRanges,
    /// Concrete-syntax nodes processed by a parser or analyzer.
    SyntaxNodes,
    /// Syntax nesting depth.
    SyntaxDepth,
    /// Adapter-reported in-process memory bytes.
    ReportedMemoryBytes,
}

/// Rejection from a bounded transactional stream sink.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SinkError {
    /// The sequence was already accepted.
    #[error("batch sequence {sequence} was already accepted")]
    DuplicateSequence {
        /// Repeated sequence number.
        sequence: u64,
    },
    /// The sequence skipped the next required transport position.
    #[error("batch sequence {observed} is out of order; expected {expected}")]
    OutOfOrder {
        /// Required next sequence number.
        expected: u64,
        /// Observed sequence number.
        observed: u64,
    },
    /// The sink was already committed or discarded.
    #[error("stream sink is closed")]
    Closed,
    /// A batch contained no facts and no diagnostics.
    #[error("empty stream batches are not accepted")]
    EmptyBatch,
    /// A fact or diagnostic referenced source outside the bound file.
    #[error("stream record source does not match the bound file")]
    SourceMismatch,
    /// Two unequal syntax facts reused one parser-local identity.
    #[error("unequal syntax facts reuse local identity {local_id}")]
    DuplicateSyntaxFact {
        /// Reused parser-local identity.
        local_id: u64,
    },
    /// One batch exceeded a fixed threshold.
    #[error("{resource:?} batch usage {observed} exceeds limit {limit}")]
    BatchLimit {
        /// Limited resource.
        resource: ResourceKind,
        /// Observed batch usage.
        observed: usize,
        /// Fixed batch threshold.
        limit: usize,
    },
    /// Cumulative raw usage exceeded a stream or IR quota.
    #[error("{resource:?} cumulative usage {observed} exceeds limit {limit}")]
    StreamLimit {
        /// Limited resource.
        resource: ResourceKind,
        /// Observed raw usage before deduplication.
        observed: usize,
        /// Whole-stream maximum.
        limit: usize,
    },
    /// A normalized IR record failed detailed contract validation.
    #[error("staged normalized IR is invalid")]
    InvalidIr,
    /// Usage accounting could not be represented.
    #[error("stream usage accounting overflowed")]
    AccountingOverflow,
}

/// Invalid coverage, resource, or explicit end-of-stream report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReportError {
    /// Covered bytes exceeded the declared source extent.
    #[error("coverage reports {covered} of {total} source bytes")]
    CoverageOutOfBounds {
        /// Covered source bytes.
        covered: usize,
        /// Total source bytes.
        total: usize,
    },
    /// Complete coverage contradicted skipped or uncovered work.
    #[error("complete coverage contains uncovered or skipped work")]
    InvalidCompleteCoverage,
    /// A domain count contradicted discovered work.
    #[error("domain coverage counts are inconsistent")]
    InvalidDomainCoverage,
    /// A report described a different source byte length.
    #[error("report source byte length {observed} differs from expected {expected}")]
    SourceLengthMismatch {
        /// Expected bound-source bytes.
        expected: usize,
        /// Reported source bytes.
        observed: usize,
    },
    /// A reported resource exceeded its request limit.
    #[error("{resource:?} report usage {observed} exceeds limit {limit}")]
    ResourceLimit {
        /// Limited resource.
        resource: ResourceKind,
        /// Reported usage.
        observed: usize,
        /// Requested maximum.
        limit: usize,
    },
    /// An accountable in-process adapter omitted its reported memory counter.
    #[error("accounted in-process adapter omitted its reported memory counter")]
    MissingMemoryAccounting,
    /// The analyzer's coverage report claimed a different tier from its descriptor.
    #[error("analysis report tier {observed:?} differs from provider tier {expected:?}")]
    AnalysisTierMismatch {
        /// Tier declared by the immutable provider descriptor.
        expected: AnalysisTier,
        /// Tier claimed by the completed coverage report.
        observed: AnalysisTier,
    },
    /// The report usage did not match the staged stream.
    #[error("reported stream usage does not match staged usage")]
    StreamUsageMismatch,
    /// The explicit end marker did not match the next required sequence.
    #[error("end-of-stream sequence {observed} differs from expected {expected}")]
    EndSequenceMismatch {
        /// Required next sequence.
        expected: u64,
        /// Reported next sequence.
        observed: u64,
    },
}

/// Failure returned by an adapter invocation or its transactional boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AdapterError {
    /// The request contradicted provider capabilities.
    #[error(transparent)]
    RejectedRequest(#[from] RequestError),
    /// Cooperative cancellation won before commit.
    #[error("adapter invocation was cancelled: {reason:?}")]
    Cancelled {
        /// Stable cancellation reason.
        reason: CancellationReason,
    },
    /// A sink rejected staged output.
    #[error(transparent)]
    Sink(#[from] SinkError),
    /// A successful adapter call returned an inconsistent report.
    #[error(transparent)]
    InvalidReport(#[from] ReportError),
    /// The provider failed with a bounded source-free code.
    #[error("adapter provider failed with code {code}")]
    ProviderFailed {
        /// Stable provider-defined failure code.
        code: DiagnosticCode,
    },
}

impl From<Cancelled> for AdapterError {
    fn from(cancelled: Cancelled) -> Self {
        Self::Cancelled {
            reason: cancelled.reason(),
        }
    }
}
