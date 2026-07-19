//! Transactional bounded sinks and synchronous adapter invocation contracts.
//!
//! Batch sequences are transport-only. Successful reports validate explicit
//! end markers before staged output is deterministically committed.

use std::{cmp::Ordering, convert::Infallible, fmt, mem};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ir::{
    AnalysisTier, CoverageRecord, CoverageStatus, DiagnosticRecord, DiagnosticSeverity,
    EntityRecord, ExtensionEnvelope, ExtensionSupport, FactEvidence, FileRecord, IrLimits,
    NormalizedIrDocument, OccurrenceRecord, ProvenanceRecord, RelationRecord, SkippedRegion,
    SourceMappingRecord, SourceRef, SourceSpan, canonicalize_ir_document,
};

use crate::{
    descriptor::{
        MemoryAdmissionPolicy, MemoryAdmissionStatus, MemoryEnforcement, ParseCapabilities,
        ProducerDescriptor, validated_label,
    },
    error::{
        AdapterError, LabelError, LabelField, ReportError, RequestError, ResourceKind, SinkError,
    },
    ir_accounting::{IrRawBudget, ir_batch_metrics},
    limits::{RemainingBudget, StreamLimits, StreamUsage},
    report::{AnalysisReport, ParseReport},
    request::{AnalysisRequest, ParseRequest},
};

const MAX_SYNTAX_KIND_BYTES: usize = 128;
const MAX_DIAGNOSTIC_CODE_BYTES: usize = 128;
const CANCELLATION_CHECK_INTERVAL: usize = 256;
// The logical weight is deliberately platform-independent; it bounds staged
// output without exposing allocator layout as part of the SDK contract.
const LOGICAL_RECORD_OVERHEAD: usize = 64;

/// A bounded parser-independent syntax-kind label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyntaxKindLabel(String);

impl SyntaxKindLabel {
    /// Creates a bounded source-free syntax-kind label.
    ///
    /// # Errors
    ///
    /// Returns [`LabelError`] for empty, oversized, or unsafe input.
    pub fn new(value: &str) -> Result<Self, LabelError> {
        validated_label(LabelField::SyntaxKind, value, MAX_SYNTAX_KIND_BYTES).map(Self)
    }

    /// Returns the validated syntax-kind label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SyntaxKindLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A bounded stable diagnostic code that never contains source text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    /// Creates a bounded source-free diagnostic code.
    ///
    /// # Errors
    ///
    /// Returns [`LabelError`] for empty, oversized, or unsafe input.
    pub fn new(value: &str) -> Result<Self, LabelError> {
        validated_label(LabelField::DiagnosticCode, value, MAX_DIAGNOSTIC_CODE_BYTES).map(Self)
    }

    /// Returns the validated diagnostic code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Closed parser-independent syntax fact classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SyntaxFactKind {
    /// Full-file syntax root.
    Root,
    /// Module, namespace, or equivalent container.
    Module,
    /// Named declaration.
    Declaration,
    /// Bounded declaration header or other parser-proven signature evidence.
    Signature,
    /// Import, include, or module dependency.
    Import,
    /// Lexical or semantic scope.
    Scope,
    /// Identifier or other source occurrence.
    Occurrence,
    /// Comment or documentation node.
    Comment,
    /// String-like literal.
    StringLiteral,
    /// Embedded-language region.
    EmbeddedRegion,
    /// Parser recovery node.
    ErrorRecovery,
}

/// One parser-native-type-free syntax fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxFact {
    local_id: u64,
    parent: Option<u64>,
    kind: SyntaxFactKind,
    span: SourceSpan,
    depth: usize,
    syntax_kind: SyntaxKindLabel,
}

impl SyntaxFact {
    /// Creates one syntax fact with a parser-local deterministic identity.
    #[must_use]
    pub const fn new(
        local_id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        span: SourceSpan,
        depth: usize,
        syntax_kind: SyntaxKindLabel,
    ) -> Self {
        Self {
            local_id,
            parent,
            kind,
            span,
            depth,
            syntax_kind,
        }
    }

    /// Returns the parser-local deterministic identity.
    #[must_use]
    pub const fn local_id(&self) -> u64 {
        self.local_id
    }

    /// Returns the optional parser-local parent identity.
    #[must_use]
    pub const fn parent(&self) -> Option<u64> {
        self.parent
    }

    /// Returns the normalized syntax fact class.
    #[must_use]
    pub const fn kind(&self) -> SyntaxFactKind {
        self.kind
    }

    /// Returns the authoritative byte span.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    /// Returns zero-based syntax nesting depth.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the producer-defined bounded syntax-kind label.
    #[must_use]
    pub const fn syntax_kind(&self) -> &SyntaxKindLabel {
        &self.syntax_kind
    }
}

/// One bounded source-free parser diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterDiagnostic {
    code: DiagnosticCode,
    severity: DiagnosticSeverity,
    source: Option<SourceRef>,
    coverage_effect: CoverageStatus,
}

impl AdapterDiagnostic {
    /// Creates a parser diagnostic without retaining an untrusted message.
    #[must_use]
    pub const fn new(
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
        source: Option<SourceRef>,
        coverage_effect: CoverageStatus,
    ) -> Self {
        Self {
            code,
            severity,
            source,
            coverage_effect,
        }
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &DiagnosticCode {
        &self.code
    }

    /// Returns diagnostic severity.
    #[must_use]
    pub const fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    /// Returns optional immutable source evidence.
    #[must_use]
    pub const fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    /// Returns the resulting coverage effect.
    #[must_use]
    pub const fn coverage_effect(&self) -> CoverageStatus {
        self.coverage_effect
    }
}

/// One contiguous transport batch of syntax facts and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxFactBatch {
    sequence: u64,
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
}

impl SyntaxFactBatch {
    /// Creates a syntax batch; the sink validates sequence and quotas.
    #[must_use]
    pub const fn new(
        sequence: u64,
        facts: Vec<SyntaxFact>,
        diagnostics: Vec<AdapterDiagnostic>,
    ) -> Self {
        Self {
            sequence,
            facts,
            diagnostics,
        }
    }

    /// Returns the transport sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns syntax facts in this transport batch.
    #[must_use]
    pub fn facts(&self) -> &[SyntaxFact] {
        &self.facts
    }

    /// Returns parser diagnostics in this transport batch.
    #[must_use]
    pub fn diagnostics(&self) -> &[AdapterDiagnostic] {
        &self.diagnostics
    }

    /// Returns deterministic raw usage for this batch.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError::AccountingOverflow`] if counters are not
    /// representable.
    pub fn usage(&self) -> Result<StreamUsage, SinkError> {
        syntax_batch_usage(self)
    }
}

/// One normalized IR record accepted by [`IrBatchSink`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IrRecord {
    /// Immutable file record.
    File(FileRecord),
    /// Semantic entity record.
    Entity(EntityRecord),
    /// Source occurrence record.
    Occurrence(OccurrenceRecord),
    /// Typed relation record.
    Relation(RelationRecord),
    /// Producer provenance record.
    Provenance(ProvenanceRecord),
    /// Generated-source mapping.
    SourceMapping(SourceMappingRecord),
    /// Fact-domain coverage record.
    Coverage(CoverageRecord),
    /// Explicit skipped source region.
    SkippedRegion(SkippedRegion),
    /// Bounded normalized diagnostic.
    Diagnostic(DiagnosticRecord),
    /// Namespaced extension envelope.
    Extension(ExtensionEnvelope),
}

/// One contiguous transport batch of normalized IR records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrBatch {
    sequence: u64,
    records: Vec<IrRecord>,
}

impl IrBatch {
    /// Creates an IR batch; the sink validates sequence, ownership, and quotas.
    #[must_use]
    pub const fn new(sequence: u64, records: Vec<IrRecord>) -> Self {
        Self { sequence, records }
    }

    /// Returns the transport sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns normalized records in this transport batch.
    #[must_use]
    pub fn records(&self) -> &[IrRecord] {
        &self.records
    }

    /// Returns deterministic raw usage after detailed IR limit checks.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError`] for invalid per-record bounds or unrepresentable
    /// counters.
    pub fn usage(&self, limits: &IrLimits) -> Result<StreamUsage, SinkError> {
        ir_batch_metrics(self, limits).map(|metrics| metrics.usage)
    }
}

/// Remaining detailed raw normalized-IR quotas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrRemainingBudget {
    pub(crate) files: usize,
    pub(crate) entities: usize,
    pub(crate) occurrences: usize,
    pub(crate) relations: usize,
    pub(crate) provenance: usize,
    pub(crate) source_mappings: usize,
    pub(crate) coverage: usize,
    pub(crate) skipped_regions: usize,
    pub(crate) diagnostics: usize,
    pub(crate) extensions: usize,
    pub(crate) total_records: usize,
    pub(crate) nested_items: usize,
    pub(crate) string_bytes: usize,
    pub(crate) extension_bytes: usize,
    pub(crate) diagnostic_bytes: usize,
}

impl IrRemainingBudget {
    /// Returns remaining raw file records.
    #[must_use]
    pub const fn files(self) -> usize {
        self.files
    }

    /// Returns remaining raw entity records.
    #[must_use]
    pub const fn entities(self) -> usize {
        self.entities
    }

    /// Returns remaining raw occurrence records.
    #[must_use]
    pub const fn occurrences(self) -> usize {
        self.occurrences
    }

    /// Returns remaining raw relation records.
    #[must_use]
    pub const fn relations(self) -> usize {
        self.relations
    }

    /// Returns remaining raw provenance records.
    #[must_use]
    pub const fn provenance(self) -> usize {
        self.provenance
    }

    /// Returns remaining raw source mappings.
    #[must_use]
    pub const fn source_mappings(self) -> usize {
        self.source_mappings
    }

    /// Returns remaining raw coverage records.
    #[must_use]
    pub const fn coverage(self) -> usize {
        self.coverage
    }

    /// Returns remaining raw skipped regions.
    #[must_use]
    pub const fn skipped_regions(self) -> usize {
        self.skipped_regions
    }

    /// Returns remaining raw normalized diagnostics.
    #[must_use]
    pub const fn diagnostics(self) -> usize {
        self.diagnostics
    }

    /// Returns remaining raw extension envelopes.
    #[must_use]
    pub const fn extensions(self) -> usize {
        self.extensions
    }

    /// Returns remaining raw top-level records across all collections.
    #[must_use]
    pub const fn total_records(self) -> usize {
        self.total_records
    }

    /// Returns remaining raw nested collection items.
    #[must_use]
    pub const fn nested_items(self) -> usize {
        self.nested_items
    }

    /// Returns remaining raw non-payload string bytes.
    #[must_use]
    pub const fn string_bytes(self) -> usize {
        self.string_bytes
    }

    /// Returns remaining raw extension payload bytes.
    #[must_use]
    pub const fn extension_bytes(self) -> usize {
        self.extension_bytes
    }

    /// Returns remaining raw diagnostic code and message bytes.
    #[must_use]
    pub const fn diagnostic_bytes(self) -> usize {
        self.diagnostic_bytes
    }
}

/// Backpressured syntax-fact sink implemented by the SDK executor.
pub trait SyntaxFactSink {
    /// Returns remaining cumulative and fixed next-batch budgets.
    fn remaining_budget(&self) -> RemainingBudget;

    /// Returns exact raw usage already staged.
    fn staged_usage(&self) -> StreamUsage;

    /// Returns the only sequence accepted for the next batch.
    fn next_sequence(&self) -> u64;

    /// Stages one all-or-nothing batch.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError`] for sequence, source, or resource violations.
    fn push(&mut self, batch: SyntaxFactBatch) -> Result<(), SinkError>;

    /// Stages one batch with cooperative checkpoints around sink-owned work.
    ///
    /// The default preserves compatibility for existing sinks by checking
    /// cancellation at the call boundaries. Sinks that perform input-sized
    /// validation, accounting, or copying should override this method with
    /// checkpoints inside those bounded work units.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for cancellation or the same sink violations
    /// as [`SyntaxFactSink::push`].
    fn push_cancellable(
        &mut self,
        batch: SyntaxFactBatch,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        self.push(batch)?;
        cancellation.check()?;
        Ok(())
    }
}

/// Backpressured normalized-IR sink implemented by the SDK executor.
pub trait IrBatchSink {
    /// Returns remaining cumulative and fixed next-batch budgets.
    fn remaining_budget(&self) -> RemainingBudget;

    /// Returns exact raw usage already staged.
    fn staged_usage(&self) -> StreamUsage;

    /// Returns the only sequence accepted for the next batch.
    fn next_sequence(&self) -> u64;

    /// Stages one all-or-nothing batch.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError`] for sequence, source, or resource violations.
    fn push(&mut self, batch: IrBatch) -> Result<(), SinkError>;
}

/// Synchronous cooperative parser contract without parser framework types.
///
/// Providers must reach cancellation checkpoints during parsing. This
/// in-process trait cannot terminate a noncooperative call; hard process-tree
/// ownership and termination belong to the isolated adapter host boundary.
pub trait ParseProvider: Send + Sync {
    /// Returns immutable admission-control capabilities.
    fn capabilities(&self) -> &ParseCapabilities;

    /// Parses one immutable file and stages bounded syntax batches.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for cancellation, backpressure, or provider
    /// failure. Implementations must check `cancellation` between bounded work
    /// units and before every emitted batch.
    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError>;
}

/// Synchronous cooperative language analyzer contract producing normalized IR.
///
/// The SDK checks admission and transaction boundaries, but cannot terminate a
/// noncooperative in-process call. Hard process-tree termination belongs to the isolated adapter supervisor.
pub trait LanguageAnalyzer: Send + Sync {
    /// Returns immutable producer identity and capabilities.
    fn descriptor(&self) -> &ProducerDescriptor;

    /// Analyzes one immutable file and stages bounded normalized IR batches.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for cancellation, backpressure, or provider
    /// failure. Implementations must check `cancellation` between bounded work
    /// units and before every emitted batch.
    fn analyze(
        &self,
        request: &AnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<AnalysisReport, AdapterError>;
}

/// Committed deterministic parser output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    report: ParseReport,
    memory_admission: MemoryAdmissionStatus,
}

impl ParseOutput {
    /// Returns syntax facts sorted by parser-local identity.
    #[must_use]
    pub fn facts(&self) -> &[SyntaxFact] {
        &self.facts
    }

    /// Returns deterministically sorted parser diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[AdapterDiagnostic] {
        &self.diagnostics
    }

    /// Returns the report that committed the staged stream.
    #[must_use]
    pub const fn report(&self) -> &ParseReport {
        &self.report
    }

    /// Returns the caller-selected memory enforcement outcome.
    #[must_use]
    pub const fn memory_admission(&self) -> MemoryAdmissionStatus {
        self.memory_admission
    }
}

/// Committed canonical normalized-IR output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisOutput {
    document: NormalizedIrDocument,
    report: AnalysisReport,
    memory_admission: MemoryAdmissionStatus,
}

impl AnalysisOutput {
    /// Returns canonical IR independent of accepted batch ordering.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        &self.document
    }

    /// Returns the report that committed the staged stream.
    #[must_use]
    pub const fn report(&self) -> &AnalysisReport {
        &self.report
    }

    /// Returns the caller-selected memory enforcement outcome.
    #[must_use]
    pub const fn memory_admission(&self) -> MemoryAdmissionStatus {
        self.memory_admission
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SinkState {
    Open,
    Closed,
}

#[derive(Debug)]
enum CheckpointedSinkError<E> {
    Sink(SinkError),
    Interrupted(E),
}

impl<E> From<SinkError> for CheckpointedSinkError<E> {
    fn from(error: SinkError) -> Self {
        Self::Sink(error)
    }
}

/// Transactional in-memory syntax sink with fixed and cumulative quotas.
#[derive(Debug)]
pub struct BoundedSyntaxSink {
    source: SourceRef,
    limits: StreamLimits,
    max_syntax_depth: usize,
    state: SinkState,
    next_sequence: u64,
    usage: StreamUsage,
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
}

impl BoundedSyntaxSink {
    /// Creates an open per-file syntax sink.
    #[must_use]
    pub fn new(source: SourceRef, limits: StreamLimits, max_syntax_depth: usize) -> Self {
        Self {
            source,
            limits,
            max_syntax_depth,
            state: SinkState::Open,
            next_sequence: 0,
            usage: StreamUsage::default(),
            facts: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Discards all staged output and permanently closes the sink.
    pub fn discard(&mut self) {
        self.facts.clear();
        self.diagnostics.clear();
        self.usage = StreamUsage::default();
        self.next_sequence = 0;
        self.state = SinkState::Closed;
    }

    fn commit(
        &mut self,
        cancellation: &Cancellation,
    ) -> Result<(Vec<SyntaxFact>, Vec<AdapterDiagnostic>), AdapterError> {
        let result = self.commit_checked(|| cancellation.check());
        match result {
            Ok(output) => Ok(output),
            Err(error) => {
                self.discard();
                Err(map_checkpointed_error(error))
            }
        }
    }

    fn commit_checked<E>(
        &mut self,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(Vec<SyntaxFact>, Vec<AdapterDiagnostic>), CheckpointedSinkError<E>> {
        run_checkpoint(&mut checkpoint)?;
        self.ensure_open()?;
        sort_cancellable_by(&mut self.facts, &mut checkpoint, |left, right| {
            left.local_id.cmp(&right.local_id)
        })?;
        let facts = deduplicate_syntax_facts(mem::take(&mut self.facts), &mut checkpoint)?;
        sort_cancellable_by(&mut self.diagnostics, &mut checkpoint, compare_diagnostics)?;
        let diagnostics = deduplicate_sorted(mem::take(&mut self.diagnostics), &mut checkpoint)?;
        run_checkpoint(&mut checkpoint)?;
        self.state = SinkState::Closed;
        Ok((facts, diagnostics))
    }

    fn push_checked<E>(
        &mut self,
        batch: SyntaxFactBatch,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(), CheckpointedSinkError<E>> {
        run_checkpoint(&mut checkpoint)?;
        self.ensure_open()?;
        validate_sequence(self.next_sequence, batch.sequence)?;
        if batch.facts.is_empty() && batch.diagnostics.is_empty() {
            return Err(SinkError::EmptyBatch.into());
        }
        for (index, fact) in batch.facts.iter().enumerate() {
            if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                run_checkpoint(&mut checkpoint)?;
            }
            if !span_within_source(fact.span, &self.source) {
                return Err(SinkError::SourceMismatch.into());
            }
            if fact.depth > self.max_syntax_depth {
                return Err(SinkError::StreamLimit {
                    resource: ResourceKind::SyntaxDepth,
                    observed: fact.depth,
                    limit: self.max_syntax_depth,
                }
                .into());
            }
        }
        for (index, diagnostic) in batch.diagnostics.iter().enumerate() {
            if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                run_checkpoint(&mut checkpoint)?;
            }
            if diagnostic
                .source
                .as_ref()
                .is_some_and(|source| !source_within_source(source, &self.source))
            {
                return Err(SinkError::SourceMismatch.into());
            }
        }
        let batch_usage = syntax_batch_usage_checked(&batch, &mut checkpoint)?;
        validate_batch_usage(batch_usage, &self.limits)?;
        let next_usage = self.usage.checked_add(batch_usage)?;
        validate_stream_usage(next_usage, &self.limits)?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;

        run_checkpoint(&mut checkpoint)?;
        self.facts
            .try_reserve_exact(batch.facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        run_checkpoint(&mut checkpoint)?;
        self.diagnostics
            .try_reserve_exact(batch.diagnostics.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let original_fact_count = self.facts.len();
        let original_diagnostic_count = self.diagnostics.len();
        let append_result = append_syntax_batch(
            &mut self.facts,
            &mut self.diagnostics,
            batch,
            &mut checkpoint,
        );
        if let Err(error) = append_result {
            self.facts.truncate(original_fact_count);
            self.diagnostics.truncate(original_diagnostic_count);
            return Err(error);
        }
        self.usage = next_usage;
        self.next_sequence = next_sequence;
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), SinkError> {
        if self.state == SinkState::Open {
            Ok(())
        } else {
            Err(SinkError::Closed)
        }
    }
}

impl SyntaxFactSink for BoundedSyntaxSink {
    fn remaining_budget(&self) -> RemainingBudget {
        RemainingBudget::new(&self.limits, self.usage)
    }

    fn staged_usage(&self) -> StreamUsage {
        self.usage
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn push(&mut self, batch: SyntaxFactBatch) -> Result<(), SinkError> {
        match self.push_checked(batch, || Ok::<(), Infallible>(())) {
            Ok(()) => Ok(()),
            Err(CheckpointedSinkError::Sink(error)) => Err(error),
            Err(CheckpointedSinkError::Interrupted(never)) => match never {},
        }
    }

    fn push_cancellable(
        &mut self,
        batch: SyntaxFactBatch,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.push_checked(batch, || cancellation.check())
            .map_err(map_checkpointed_error)
    }
}

/// Transactional in-memory normalized-IR sink with raw pre-dedupe quotas.
#[derive(Debug)]
pub struct BoundedIrSink {
    source: SourceRef,
    stream_limits: StreamLimits,
    ir_limits: IrLimits,
    extensions: ExtensionSupport,
    state: SinkState,
    next_sequence: u64,
    usage: StreamUsage,
    raw: IrRawBudget,
    document: NormalizedIrDocument,
}

impl BoundedIrSink {
    /// Creates an open per-file normalized-IR sink.
    #[must_use]
    pub fn new(
        source: SourceRef,
        stream_limits: StreamLimits,
        ir_limits: IrLimits,
        extensions: ExtensionSupport,
    ) -> Self {
        let document = NormalizedIrDocument::empty(source.repository(), source.generation());
        Self {
            source,
            stream_limits,
            ir_limits,
            extensions,
            state: SinkState::Open,
            next_sequence: 0,
            usage: StreamUsage::default(),
            raw: IrRawBudget::default(),
            document,
        }
    }

    /// Returns remaining detailed raw IR quotas before deduplication.
    #[must_use]
    pub fn remaining_ir_budget(&self) -> IrRemainingBudget {
        self.raw.remaining(&self.ir_limits)
    }

    /// Discards all staged output and permanently closes the sink.
    pub fn discard(&mut self) {
        self.document =
            NormalizedIrDocument::empty(self.source.repository(), self.source.generation());
        self.usage = StreamUsage::default();
        self.raw = IrRawBudget::default();
        self.next_sequence = 0;
        self.state = SinkState::Closed;
    }

    fn commit(&mut self) -> Result<NormalizedIrDocument, SinkError> {
        self.ensure_open()?;
        let staged = mem::replace(
            &mut self.document,
            NormalizedIrDocument::empty(self.source.repository(), self.source.generation()),
        );
        self.state = SinkState::Closed;
        canonicalize_ir_document(staged, &self.ir_limits, &self.extensions)
            .map_err(|_| SinkError::InvalidIr)
    }

    fn ensure_open(&self) -> Result<(), SinkError> {
        if self.state == SinkState::Open {
            Ok(())
        } else {
            Err(SinkError::Closed)
        }
    }
}

impl IrBatchSink for BoundedIrSink {
    fn remaining_budget(&self) -> RemainingBudget {
        let remaining = RemainingBudget::new(&self.stream_limits, self.usage);
        let stream_remaining = remaining.remaining();
        let total_ir_remaining = self
            .ir_limits
            .max_total_records
            .saturating_sub(self.raw.total_records());
        let adjusted = StreamUsage::new(
            stream_remaining.batches(),
            stream_remaining.records().min(total_ir_remaining),
            stream_remaining.output_bytes(),
            stream_remaining.diagnostics().min(
                self.ir_limits
                    .max_diagnostics
                    .saturating_sub(self.raw.diagnostics),
            ),
            stream_remaining.diagnostic_bytes().min(
                self.ir_limits
                    .max_total_diagnostic_bytes
                    .saturating_sub(self.raw.diagnostic_bytes),
            ),
            stream_remaining.string_bytes().min(
                self.ir_limits
                    .max_total_string_bytes
                    .saturating_sub(self.raw.string_bytes),
            ),
        );
        RemainingBudget::from_parts(adjusted, remaining.batch())
    }

    fn staged_usage(&self) -> StreamUsage {
        self.usage
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn push(&mut self, batch: IrBatch) -> Result<(), SinkError> {
        self.ensure_open()?;
        validate_sequence(self.next_sequence, batch.sequence)?;
        if batch.records.is_empty() {
            return Err(SinkError::EmptyBatch);
        }
        for record in &batch.records {
            if !record_matches_bound_source(record, &self.source) {
                return Err(SinkError::SourceMismatch);
            }
        }
        let metrics = ir_batch_metrics(&batch, &self.ir_limits)?;
        validate_batch_usage(metrics.usage, &self.stream_limits)?;
        let next_usage = self.usage.checked_add(metrics.usage)?;
        validate_stream_usage(next_usage, &self.stream_limits)?;
        let next_raw = self.raw.checked_add(metrics.raw)?;
        next_raw.validate(&self.ir_limits)?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        for record in batch.records {
            append_ir_record(&mut self.document, record);
        }
        self.usage = next_usage;
        self.raw = next_raw;
        self.next_sequence = next_sequence;
        Ok(())
    }
}

/// Executes one parser transaction and commits only a valid successful report.
///
/// `cancellation` must carry a process-local monotonic deadline. The explicit
/// `memory_policy` either admits hard/reported enforcement or intentionally
/// selects the visible unavailable-enforcement fallback.
///
/// # Errors
///
/// Returns [`AdapterError`] for missing deadline or cancellation checkpoints,
/// request-capability mismatch, rejected memory admission, cancellation,
/// provider failure, sink rejection, or inconsistent reporting.
pub fn execute_parse<P: ParseProvider + ?Sized>(
    provider: &P,
    request: &ParseRequest<'_>,
    memory_policy: MemoryAdmissionPolicy,
    cancellation: &Cancellation,
) -> Result<ParseOutput, AdapterError> {
    execute_parse_transaction(
        provider.capabilities(),
        request,
        memory_policy,
        cancellation,
        |sink, cancellation| {
            provider
                .parse(request, sink, cancellation)
                .map(|report| (report, ()))
        },
    )
    .map(|(output, ())| output)
}

/// Executes a parser transaction that returns provider-owned continuation data.
///
/// This is the safe admission path for incremental parsers whose successful
/// operation returns metadata in addition to a [`ParseReport`]. The SDK applies
/// the same deadline, capability, memory-policy, bounded-sink, report, and
/// transactional commit checks as [`execute_parse`] before returning either
/// the committed [`ParseOutput`] or the provider-owned continuation.
///
/// # Errors
///
/// Returns [`AdapterError`] under the same conditions as [`execute_parse`].
/// Continuation data is dropped whenever admission, parsing, validation, or
/// commit fails.
pub fn execute_parse_transaction<T>(
    capabilities: &ParseCapabilities,
    request: &ParseRequest<'_>,
    memory_policy: MemoryAdmissionPolicy,
    cancellation: &Cancellation,
    operation: impl FnOnce(
        &mut dyn SyntaxFactSink,
        &Cancellation,
    ) -> Result<(ParseReport, T), AdapterError>,
) -> Result<(ParseOutput, T), AdapterError> {
    validate_deadline_admission(cancellation)?;
    validate_parse_capabilities(capabilities, request)?;
    let memory_admission = admit_memory(capabilities.memory_enforcement(), memory_policy)?;
    let mut sink = BoundedSyntaxSink::new(
        request.source().source_ref().clone(),
        request.limits().syntax_stream().clone(),
        request.limits().max_syntax_depth(),
    );
    if let Err(cancelled) = cancellation.check() {
        sink.discard();
        return Err(cancelled.into());
    }
    let (report, continuation) = match operation(&mut sink, cancellation) {
        Ok(result) => result,
        Err(error) => {
            sink.discard();
            return Err(error);
        }
    };
    if let Err(cancelled) = cancellation.check() {
        sink.discard();
        return Err(cancelled.into());
    }
    if let Err(error) = report.validate_commit(
        request.source().bytes().len(),
        request.limits(),
        sink.usage,
        sink.next_sequence,
    ) {
        sink.discard();
        return Err(error.into());
    }
    if let Err(error) = validate_memory_report(
        capabilities.memory_enforcement(),
        report.resources().reported_memory_bytes(),
    ) {
        sink.discard();
        return Err(error.into());
    }
    let (facts, diagnostics) = sink.commit(cancellation)?;
    if let Err(cancelled) = cancellation.check() {
        sink.discard();
        return Err(cancelled.into());
    }
    Ok((
        ParseOutput {
            facts,
            diagnostics,
            report,
            memory_admission,
        },
        continuation,
    ))
}

/// Executes one analyzer transaction and commits only canonical valid IR.
///
/// `cancellation` must carry a process-local monotonic deadline. The explicit
/// `memory_policy` either admits hard/reported enforcement or intentionally
/// selects the visible unavailable-enforcement fallback.
///
/// # Errors
///
/// Returns [`AdapterError`] for missing deadline, request-capability mismatch,
/// rejected memory admission, cancellation, provider failure, sink rejection,
/// invalid IR, or inconsistent reporting.
pub fn execute_analysis<A: LanguageAnalyzer + ?Sized>(
    analyzer: &A,
    request: &AnalysisRequest<'_>,
    extensions: ExtensionSupport,
    memory_policy: MemoryAdmissionPolicy,
    cancellation: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    validate_deadline_admission(cancellation)?;
    validate_analyzer_descriptor(analyzer.descriptor(), request)?;
    let memory_admission = admit_memory(analyzer.descriptor().memory_enforcement(), memory_policy)?;
    let mut sink = BoundedIrSink::new(
        request.source().source_ref().clone(),
        request.limits().ir_stream().clone(),
        request.limits().ir().clone(),
        extensions,
    );
    if let Err(cancelled) = cancellation.check() {
        sink.discard();
        return Err(cancelled.into());
    }
    let report = match analyzer.analyze(request, &mut sink, cancellation) {
        Ok(report) => report,
        Err(error) => {
            sink.discard();
            return Err(error);
        }
    };
    if let Err(cancelled) = cancellation.check() {
        sink.discard();
        return Err(cancelled.into());
    }
    if let Err(error) = report.validate_commit(
        request.source().bytes().len(),
        request.limits(),
        sink.usage,
        sink.next_sequence,
    ) {
        sink.discard();
        return Err(error.into());
    }
    if report.coverage().tier() != analyzer.descriptor().tier() {
        sink.discard();
        return Err(ReportError::AnalysisTierMismatch {
            expected: analyzer.descriptor().tier(),
            observed: report.coverage().tier(),
        }
        .into());
    }
    if let Err(error) = validate_memory_report(
        analyzer.descriptor().memory_enforcement(),
        report.resources().reported_memory_bytes(),
    ) {
        sink.discard();
        return Err(error.into());
    }
    let document = sink.commit()?;
    Ok(AnalysisOutput {
        document,
        report,
        memory_admission,
    })
}

fn validate_parse_capabilities(
    capabilities: &ParseCapabilities,
    request: &ParseRequest<'_>,
) -> Result<(), RequestError> {
    if !capabilities.has_cancellation_checkpoints() {
        return Err(RequestError::CancellationCheckpointsRequired);
    }
    if !capabilities.languages().contains(request.language()) {
        return Err(RequestError::UnsupportedLanguage);
    }
    if !capabilities.encodings().contains(request.encoding()) {
        return Err(RequestError::UnsupportedEncoding);
    }
    if !request.included_ranges().is_empty() && !capabilities.supports_embedded_ranges() {
        return Err(RequestError::EmbeddedRangesUnsupported);
    }
    require_provider_limit(
        ResourceKind::SourceBytes,
        request.source().bytes().len(),
        capabilities.max_source_bytes(),
    )?;
    require_provider_limit(
        ResourceKind::SyntaxNodes,
        request.limits().max_syntax_nodes(),
        capabilities.max_syntax_nodes(),
    )?;
    require_provider_limit(
        ResourceKind::SyntaxDepth,
        request.limits().max_syntax_depth(),
        capabilities.max_syntax_depth(),
    )?;
    require_provider_limit(
        ResourceKind::IncludedRanges,
        request.included_ranges().len(),
        capabilities.max_embedded_ranges(),
    )
}

fn validate_analyzer_descriptor(
    descriptor: &ProducerDescriptor,
    request: &AnalysisRequest<'_>,
) -> Result<(), RequestError> {
    if descriptor.language() != request.language() {
        return Err(RequestError::UnsupportedLanguage);
    }
    if tier_rank(descriptor.tier()) > tier_rank(request.tier()) {
        return Err(RequestError::UnsupportedTier);
    }
    Ok(())
}

fn require_provider_limit(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<(), RequestError> {
    if observed > limit {
        Err(RequestError::ProviderLimit {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

fn validate_memory_report(
    enforcement: MemoryEnforcement,
    reported_memory_bytes: Option<usize>,
) -> Result<(), ReportError> {
    if enforcement == MemoryEnforcement::AccountedInProcess && reported_memory_bytes.is_none() {
        Err(ReportError::MissingMemoryAccounting)
    } else {
        Ok(())
    }
}

fn validate_deadline_admission(cancellation: &Cancellation) -> Result<(), RequestError> {
    if cancellation.has_deadline() {
        Ok(())
    } else {
        Err(RequestError::DeadlineRequired)
    }
}

fn admit_memory(
    enforcement: MemoryEnforcement,
    policy: MemoryAdmissionPolicy,
) -> Result<MemoryAdmissionStatus, RequestError> {
    match (enforcement, policy) {
        (MemoryEnforcement::HardProcess, _) => Ok(MemoryAdmissionStatus::HardProcess),
        (MemoryEnforcement::AccountedInProcess, _) => Ok(MemoryAdmissionStatus::AccountedInProcess),
        (
            MemoryEnforcement::Unavailable,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        ) => Ok(MemoryAdmissionStatus::UnavailableEnforcementFallback),
        (MemoryEnforcement::Unavailable, MemoryAdmissionPolicy::RequireHardOrAccounted) => {
            Err(RequestError::MemoryEnforcementUnavailable)
        }
    }
}

const fn tier_rank(tier: AnalysisTier) -> usize {
    match tier {
        AnalysisTier::TierA => 0,
        AnalysisTier::TierB => 1,
        AnalysisTier::TierC => 2,
        AnalysisTier::TierD => 3,
        _ => usize::MAX,
    }
}

fn validate_sequence(expected: u64, observed: u64) -> Result<(), SinkError> {
    match observed.cmp(&expected) {
        Ordering::Less => Err(SinkError::DuplicateSequence { sequence: observed }),
        Ordering::Greater => Err(SinkError::OutOfOrder { expected, observed }),
        Ordering::Equal => Ok(()),
    }
}

fn syntax_batch_usage(batch: &SyntaxFactBatch) -> Result<StreamUsage, SinkError> {
    match syntax_batch_usage_checked(batch, &mut || Ok::<(), Infallible>(())) {
        Ok(usage) => Ok(usage),
        Err(CheckpointedSinkError::Sink(error)) => Err(error),
        Err(CheckpointedSinkError::Interrupted(never)) => match never {},
    }
}

fn syntax_batch_usage_checked<E>(
    batch: &SyntaxFactBatch,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<StreamUsage, CheckpointedSinkError<E>> {
    let mut fact_strings = 0_usize;
    for (index, fact) in batch.facts.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            run_checkpoint(checkpoint)?;
        }
        fact_strings = fact_strings
            .checked_add(fact.syntax_kind.0.len())
            .ok_or(SinkError::AccountingOverflow)?;
    }
    let mut diagnostic_bytes = 0_usize;
    for (index, diagnostic) in batch.diagnostics.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            run_checkpoint(checkpoint)?;
        }
        diagnostic_bytes = diagnostic_bytes
            .checked_add(diagnostic.code.0.len())
            .ok_or(SinkError::AccountingOverflow)?;
    }
    let string_bytes = fact_strings
        .checked_add(diagnostic_bytes)
        .ok_or(SinkError::AccountingOverflow)?;
    let item_count = batch
        .facts
        .len()
        .checked_add(batch.diagnostics.len())
        .ok_or(SinkError::AccountingOverflow)?;
    let structural_bytes = item_count
        .checked_mul(LOGICAL_RECORD_OVERHEAD)
        .ok_or(SinkError::AccountingOverflow)?;
    let output_bytes = structural_bytes
        .checked_add(string_bytes)
        .ok_or(SinkError::AccountingOverflow)?;
    Ok(StreamUsage::new(
        1,
        batch.facts.len(),
        output_bytes,
        batch.diagnostics.len(),
        diagnostic_bytes,
        string_bytes,
    ))
}

fn append_syntax_batch<E>(
    facts: &mut Vec<SyntaxFact>,
    diagnostics: &mut Vec<AdapterDiagnostic>,
    batch: SyntaxFactBatch,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), CheckpointedSinkError<E>> {
    for (index, fact) in batch.facts.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            run_checkpoint(checkpoint)?;
        }
        facts.push(fact);
    }
    for (index, diagnostic) in batch.diagnostics.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            run_checkpoint(checkpoint)?;
        }
        diagnostics.push(diagnostic);
    }
    run_checkpoint(checkpoint)
}

fn deduplicate_syntax_facts<E>(
    facts: Vec<SyntaxFact>,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<SyntaxFact>, CheckpointedSinkError<E>> {
    let mut deduplicated = Vec::<SyntaxFact>::new();
    run_checkpoint(checkpoint)?;
    deduplicated
        .try_reserve_exact(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for (index, fact) in facts.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            run_checkpoint(checkpoint)?;
        }
        if let Some(previous) = deduplicated.last()
            && previous.local_id == fact.local_id
        {
            if previous != &fact {
                return Err(SinkError::DuplicateSyntaxFact {
                    local_id: fact.local_id,
                }
                .into());
            }
            continue;
        }
        deduplicated.push(fact);
    }
    run_checkpoint(checkpoint)?;
    Ok(deduplicated)
}

fn deduplicate_sorted<T: PartialEq, E>(
    values: Vec<T>,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<T>, CheckpointedSinkError<E>> {
    let mut deduplicated = Vec::new();
    run_checkpoint(checkpoint)?;
    deduplicated
        .try_reserve_exact(values.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for (index, value) in values.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            run_checkpoint(checkpoint)?;
        }
        if deduplicated
            .last()
            .is_some_and(|previous| previous == &value)
        {
            continue;
        }
        deduplicated.push(value);
    }
    run_checkpoint(checkpoint)?;
    Ok(deduplicated)
}

fn sort_cancellable_by<T, E>(
    values: &mut [T],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    compare: impl Fn(&T, &T) -> Ordering + Copy,
) -> Result<(), CheckpointedSinkError<E>> {
    run_checkpoint(checkpoint)?;
    let mut work = 0_usize;
    for root in (0..values.len() / 2).rev() {
        sift_down(values, root, values.len(), checkpoint, compare, &mut work)?;
    }
    for end in (1..values.len()).rev() {
        checkpoint_sort_work(&mut work, checkpoint)?;
        values.swap(0, end);
        sift_down(values, 0, end, checkpoint, compare, &mut work)?;
    }
    run_checkpoint(checkpoint)
}

fn sift_down<T, E>(
    values: &mut [T],
    mut root: usize,
    end: usize,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    compare: impl Fn(&T, &T) -> Ordering + Copy,
    work: &mut usize,
) -> Result<(), CheckpointedSinkError<E>> {
    loop {
        let Some(left) = root.checked_mul(2).and_then(|index| index.checked_add(1)) else {
            return Ok(());
        };
        if left >= end {
            return Ok(());
        }
        let mut greatest = root;
        checkpoint_sort_work(work, checkpoint)?;
        if compare(&values[greatest], &values[left]) == Ordering::Less {
            greatest = left;
        }
        let right = left.saturating_add(1);
        if right < end {
            checkpoint_sort_work(work, checkpoint)?;
            if compare(&values[greatest], &values[right]) == Ordering::Less {
                greatest = right;
            }
        }
        if greatest == root {
            return Ok(());
        }
        values.swap(root, greatest);
        root = greatest;
    }
}

fn checkpoint_sort_work<E>(
    work: &mut usize,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), CheckpointedSinkError<E>> {
    if work.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
        run_checkpoint(checkpoint)?;
    }
    *work = work.saturating_add(1);
    Ok(())
}

fn run_checkpoint<E>(
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), CheckpointedSinkError<E>> {
    checkpoint().map_err(CheckpointedSinkError::Interrupted)
}

fn map_checkpointed_error(error: CheckpointedSinkError<Cancelled>) -> AdapterError {
    match error {
        CheckpointedSinkError::Sink(error) => error.into(),
        CheckpointedSinkError::Interrupted(cancelled) => cancelled.into(),
    }
}

fn validate_batch_usage(usage: StreamUsage, limits: &StreamLimits) -> Result<(), SinkError> {
    let batch = limits.batch();
    require_batch(ResourceKind::Records, usage.records(), batch.max_records())?;
    require_batch(
        ResourceKind::OutputBytes,
        usage.output_bytes(),
        batch.max_output_bytes(),
    )?;
    require_batch(
        ResourceKind::Diagnostics,
        usage.diagnostics(),
        batch.max_diagnostics(),
    )?;
    require_batch(
        ResourceKind::DiagnosticBytes,
        usage.diagnostic_bytes(),
        batch.max_diagnostic_bytes(),
    )
}

fn validate_stream_usage(usage: StreamUsage, limits: &StreamLimits) -> Result<(), SinkError> {
    require_stream(ResourceKind::Batches, usage.batches(), limits.max_batches())?;
    require_stream(ResourceKind::Records, usage.records(), limits.max_records())?;
    require_stream(
        ResourceKind::OutputBytes,
        usage.output_bytes(),
        limits.max_output_bytes(),
    )?;
    require_stream(
        ResourceKind::Diagnostics,
        usage.diagnostics(),
        limits.max_diagnostics(),
    )?;
    require_stream(
        ResourceKind::DiagnosticBytes,
        usage.diagnostic_bytes(),
        limits.max_diagnostic_bytes(),
    )?;
    require_stream(
        ResourceKind::StringBytes,
        usage.string_bytes(),
        limits.max_string_bytes(),
    )
}

fn require_batch(resource: ResourceKind, observed: usize, limit: usize) -> Result<(), SinkError> {
    if observed > limit {
        Err(SinkError::BatchLimit {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

fn require_stream(resource: ResourceKind, observed: usize, limit: usize) -> Result<(), SinkError> {
    if observed > limit {
        Err(SinkError::StreamLimit {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

fn span_within_source(span: SourceSpan, source: &SourceRef) -> bool {
    let full = source.span();
    span.file() == full.file()
        && span.start_byte() >= full.start_byte()
        && span.end_byte() <= full.end_byte()
}

fn source_within_source(candidate: &SourceRef, source: &SourceRef) -> bool {
    candidate.repository() == source.repository()
        && candidate.generation() == source.generation()
        && candidate.content_hash() == source.content_hash()
        && span_within_source(candidate.span(), source)
}

fn compare_diagnostics(left: &AdapterDiagnostic, right: &AdapterDiagnostic) -> Ordering {
    left.code
        .cmp(&right.code)
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| severity_rank(left.severity).cmp(&severity_rank(right.severity)))
        .then_with(|| {
            coverage_rank(left.coverage_effect).cmp(&coverage_rank(right.coverage_effect))
        })
}

const fn severity_rank(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Info => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Error => 2,
    }
}

const fn coverage_rank(status: CoverageStatus) -> u8 {
    match status {
        CoverageStatus::Complete => 0,
        CoverageStatus::Bounded => 1,
        CoverageStatus::Sampled => 2,
        CoverageStatus::Unknown => 3,
        _ => u8::MAX,
    }
}

fn append_ir_record(document: &mut NormalizedIrDocument, record: IrRecord) {
    match record {
        IrRecord::File(record) => document.files.push(record),
        IrRecord::Entity(record) => document.entities.push(record),
        IrRecord::Occurrence(record) => document.occurrences.push(record),
        IrRecord::Relation(record) => document.relations.push(record),
        IrRecord::Provenance(record) => document.provenance.push(record),
        IrRecord::SourceMapping(record) => document.source_mappings.push(record),
        IrRecord::Coverage(record) => document.coverage_records.push(record),
        IrRecord::SkippedRegion(record) => document.skipped_regions.push(record),
        IrRecord::Diagnostic(record) => document.diagnostics.push(record),
        IrRecord::Extension(record) => document.extensions.push(record),
    }
}

fn record_matches_bound_source(record: &IrRecord, source: &SourceRef) -> bool {
    let owner_matches = |repository, generation| {
        repository == source.repository() && generation == source.generation()
    };
    match record {
        IrRecord::File(record) => {
            owner_matches(record.repository, record.generation)
                && record.id == source.span().file()
                && record.content_hash == source.content_hash()
                && record.byte_length == source.span().end_byte()
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Entity(record) => {
            owner_matches(record.repository, record.generation)
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Occurrence(record) => {
            owner_matches(record.repository, record.generation)
                && record.file == source.span().file()
                && source_within_source(&record.source, source)
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Relation(record) => {
            owner_matches(record.repository, record.generation)
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Provenance(record) => {
            owner_matches(record.repository, record.generation)
                && record
                    .input_sources
                    .iter()
                    .all(|candidate| source_within_source(candidate, source))
                && record
                    .evidence_sources
                    .iter()
                    .all(|candidate| source_within_source(candidate, source))
        }
        IrRecord::SourceMapping(record) => {
            owner_matches(record.repository, record.generation)
                && source_within_source(&record.from, source)
                && source_within_source(&record.to, source)
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Coverage(record) => {
            owner_matches(record.repository, record.generation)
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::SkippedRegion(record) => {
            owner_matches(record.repository, record.generation)
                && source_within_source(&record.source, source)
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Diagnostic(record) => {
            owner_matches(record.repository, record.generation)
                && record
                    .source
                    .as_ref()
                    .is_none_or(|candidate| source_within_source(candidate, source))
                && evidence_matches(&record.evidence, source)
        }
        IrRecord::Extension(record) => {
            owner_matches(record.repository, record.generation)
                && evidence_matches(&record.evidence, source)
        }
    }
}

fn evidence_matches(evidence: &FactEvidence, source: &SourceRef) -> bool {
    evidence
        .source
        .as_ref()
        .is_none_or(|candidate| source_within_source(candidate, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn cancellable_push_prioritizes_existing_cancellation_over_sink_validation() {
        let mut sink = syntax_sink(1);
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));

        assert!(matches!(
            sink.push_cancellable(
                SyntaxFactBatch::new(0, Vec::new(), Vec::new()),
                &cancellation,
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(sink.next_sequence(), 0);
        assert_eq!(sink.staged_usage(), StreamUsage::default());
    }

    #[test]
    fn checkpointed_push_rolls_back_an_interrupted_partial_append() {
        let fact_count = CANCELLATION_CHECK_INTERVAL * 4;
        let mut sink = syntax_sink(fact_count);
        let batch = SyntaxFactBatch::new(
            0,
            (0..fact_count)
                .map(|index| syntax_fact(u64::try_from(index).expect("test index fits u64")))
                .collect(),
            Vec::new(),
        );
        let mut checkpoints = 0usize;

        let result = sink.push_checked(batch, || {
            checkpoints += 1;
            if checkpoints == 13 { Err(()) } else { Ok(()) }
        });

        assert!(matches!(
            result,
            Err(CheckpointedSinkError::Interrupted(()))
        ));
        assert_eq!(sink.next_sequence(), 0);
        assert_eq!(sink.staged_usage(), StreamUsage::default());
        assert!(sink.facts.is_empty());
        assert!(sink.diagnostics.is_empty());
    }

    #[test]
    fn checkpointed_commit_stops_inside_canonical_sort() {
        let fact_count = CANCELLATION_CHECK_INTERVAL * 4;
        let mut sink = syntax_sink(fact_count);
        let facts = (0..fact_count)
            .rev()
            .map(|index| syntax_fact(u64::try_from(index).expect("test index fits u64")))
            .collect();
        sink.push(SyntaxFactBatch::new(0, facts, Vec::new()))
            .expect("test facts fit the sink");
        let mut checkpoints = 0usize;

        let result = sink.commit_checked(|| {
            checkpoints += 1;
            if checkpoints == 3 { Err(()) } else { Ok(()) }
        });

        assert!(matches!(
            result,
            Err(CheckpointedSinkError::Interrupted(()))
        ));
        assert_eq!(checkpoints, 3);
        assert_eq!(sink.state, SinkState::Open);
    }

    #[test]
    fn checkpointed_sort_matches_canonical_order() {
        let mut values = (0..4097_u64)
            .map(|value| value.wrapping_mul(7919) % 4097)
            .collect::<Vec<_>>();
        let mut expected = values.clone();
        expected.sort_unstable();

        sort_cancellable_by(&mut values, &mut || Ok::<(), Infallible>(()), Ord::cmp)
            .expect("checkpointed sort succeeds");

        assert_eq!(values, expected);
    }

    fn syntax_sink(max_records: usize) -> BoundedSyntaxSink {
        let batch = crate::BatchThresholds::new(
            max_records,
            max_records.saturating_mul(LOGICAL_RECORD_OVERHEAD + MAX_SYNTAX_KIND_BYTES),
            1,
            MAX_DIAGNOSTIC_CODE_BYTES,
        )
        .expect("test batch limits are nonzero");
        let stream = StreamLimits::new(
            1,
            max_records,
            max_records.saturating_mul(LOGICAL_RECORD_OVERHEAD + MAX_SYNTAX_KIND_BYTES),
            1,
            MAX_DIAGNOSTIC_CODE_BYTES,
            max_records.saturating_mul(MAX_SYNTAX_KIND_BYTES),
            batch,
        )
        .expect("test stream limits are nonzero");
        BoundedSyntaxSink::new(source_ref(), stream, 8)
    }

    fn syntax_fact(local_id: u64) -> SyntaxFact {
        SyntaxFact::new(
            local_id,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(
                "file1_mj52q2t6rtn7fisze2ehwnyu4smi6oe4unpyfla"
                    .parse()
                    .expect("test file identity is valid"),
                0,
                1,
            )
            .expect("test span is ordered"),
            0,
            SyntaxKindLabel::new("function_item").expect("test syntax kind is valid"),
        )
    }

    fn source_ref() -> SourceRef {
        SourceRef::new(
            "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                .parse()
                .expect("test repository identity is valid"),
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .expect("test generation identity is valid"),
            SourceSpan::new(
                "file1_mj52q2t6rtn7fisze2ehwnyu4smi6oe4unpyfla"
                    .parse()
                    .expect("test file identity is valid"),
                0,
                1,
            )
            .expect("test span is ordered"),
            "b3_rc6zkrxh5srdoiia2cydtoqh5ug2jyctujxicstuvgf2yz377y5zl6hbcu"
                .parse()
                .expect("test content hash is valid"),
            None,
        )
    }
}
