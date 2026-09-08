//! Bounded synchronous contracts for Rootlight parser and language adapters.
//!
//! Adapters receive either one immutable generation-bound source or one
//! canonical project input set and can publish only through transactional
//! sinks with explicit cumulative budgets.

#![forbid(unsafe_code)]

mod dart_names;
mod descriptor;
mod error;
mod ir_accounting;
mod json_names;
mod limits;
mod lua_names;
mod r_names;
mod report;
mod request;
mod sink;
mod sql_names;
mod structural;
pub mod testkit;
mod toml_names;
mod yaml_names;

pub use descriptor::{
    EncodingId, LanguageId, MemoryAdmissionPolicy, MemoryAdmissionStatus, MemoryEnforcement,
    ParseCapabilities, ProducerDescriptor,
};
pub use error::{
    AdapterError, DescriptorError, LabelError, LabelField, LabelViolation, LimitError, ReportError,
    RequestError, ResourceKind, SinkError, SnapshotError,
};
pub use limits::{
    AnalysisLimits, BatchThresholds, ProjectAnalysisLimits, RemainingBudget, StreamLimits,
    StreamUsage,
};
pub use report::{
    AnalysisReport, CoverageReport, DomainCoverage, ParseReport, ProjectAnalysisReport,
    ResourceUsage, StreamEnd, WorkReport,
};
pub use request::{
    AnalysisRequest, AnalysisUnitId, BuildTargetId, GeneratedOriginMapping,
    GenerationBoundSnapshot, IncludedRange, ParseRequest, ProjectAnalysisRequest,
    ProjectSourceInput, TransformationId,
};
pub use sink::{
    AdapterDiagnostic, AnalysisOutput, BoundedIrSink, BoundedSyntaxSink, DiagnosticCode, IrBatch,
    IrBatchSink, IrRecord, IrRemainingBudget, LanguageAnalyzer, ParseOutput, ParseProvider,
    ProjectAnalysisOutput, ProjectLanguageAnalyzer, SyntaxFact, SyntaxFactBatch, SyntaxFactKind,
    SyntaxFactSink, SyntaxKindLabel, execute_analysis, execute_parse, execute_parse_transaction,
    execute_project_analysis,
};
pub use structural::{
    structural_captured_name, structural_captured_name_for_fact,
    structural_captured_name_for_language, structural_display_name_for_language,
    structural_entity_kind, structural_entity_kind_from_source, structural_syntax_fact_order,
};
pub use yaml_names::{
    YamlBlockScalar, YamlCollectionKind, YamlCollectionTag, YamlDocumentContext, YamlScalarIdentity,
};
