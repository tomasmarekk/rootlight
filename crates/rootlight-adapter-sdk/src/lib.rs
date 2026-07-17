//! Bounded synchronous contracts for Rootlight parser and language adapters.
//!
//! Adapters receive one immutable generation-bound source file and can publish
//! only through transactional sinks with explicit cumulative budgets.

#![forbid(unsafe_code)]

mod descriptor;
mod error;
mod ir_accounting;
mod limits;
mod report;
mod request;
mod sink;
pub mod testkit;

pub use descriptor::{
    EncodingId, LanguageId, MemoryAdmissionPolicy, MemoryAdmissionStatus, MemoryEnforcement,
    ParseCapabilities, ProducerDescriptor,
};
pub use error::{
    AdapterError, DescriptorError, LabelError, LabelField, LabelViolation, LimitError, ReportError,
    RequestError, ResourceKind, SinkError, SnapshotError,
};
pub use limits::{AnalysisLimits, BatchThresholds, RemainingBudget, StreamLimits, StreamUsage};
pub use report::{
    AnalysisReport, CoverageReport, DomainCoverage, ParseReport, ResourceUsage, StreamEnd,
    WorkReport,
};
pub use request::{AnalysisRequest, GenerationBoundSnapshot, IncludedRange, ParseRequest};
pub use sink::{
    AdapterDiagnostic, AnalysisOutput, BoundedIrSink, BoundedSyntaxSink, DiagnosticCode, IrBatch,
    IrBatchSink, IrRecord, IrRemainingBudget, LanguageAnalyzer, ParseOutput, ParseProvider,
    SyntaxFact, SyntaxFactBatch, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel, execute_analysis,
    execute_parse,
};
