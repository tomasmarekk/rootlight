//! Deterministic in-process parser and analyzer test doubles.
//!
//! The mocks use the same public sink contracts as production adapters, honor
//! remaining budgets, and check cooperative cancellation before every batch.

use rootlight_cancel::{Cancellation, CancellationReason};

use crate::{
    AdapterDiagnostic, AdapterError, AnalysisReport, AnalysisRequest, CoverageReport, IrBatch,
    IrBatchSink, IrRecord, LanguageAnalyzer, ParseCapabilities, ParseProvider, ParseReport,
    ParseRequest, ProducerDescriptor, ResourceUsage, SinkError, StreamEnd, StreamUsage, SyntaxFact,
    SyntaxFactBatch, SyntaxFactSink, WorkReport,
};

/// Deterministic parser double that greedily fills the sink's advertised budget.
#[derive(Debug, Clone)]
pub struct MockParseProvider {
    capabilities: ParseCapabilities,
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    coverage: CoverageReport,
    syntax_nodes: usize,
    reported_memory_bytes: Option<usize>,
    cancel_after_batches: Option<(usize, CancellationReason)>,
}

impl MockParseProvider {
    /// Creates an in-process parser double.
    #[must_use]
    pub fn new(
        capabilities: ParseCapabilities,
        facts: Vec<SyntaxFact>,
        diagnostics: Vec<AdapterDiagnostic>,
        coverage: CoverageReport,
    ) -> Self {
        let reported_memory_bytes = match capabilities.memory_enforcement() {
            crate::MemoryEnforcement::AccountedInProcess => Some(0),
            _ => None,
        };
        let syntax_nodes = facts.len();
        Self {
            capabilities,
            facts,
            diagnostics,
            coverage,
            syntax_nodes,
            reported_memory_bytes,
            cancel_after_batches: None,
        }
    }

    /// Configures concrete-syntax nodes independently of emitted facts.
    #[must_use]
    pub const fn with_syntax_nodes(mut self, nodes: usize) -> Self {
        self.syntax_nodes = nodes;
        self
    }

    /// Configures deterministic adapter-reported working memory.
    #[must_use]
    pub const fn with_reported_memory_bytes(mut self, bytes: usize) -> Self {
        self.reported_memory_bytes = Some(bytes);
        self
    }

    /// Requests cancellation immediately before the selected batch boundary.
    #[must_use]
    pub const fn with_cancellation_after_batches(
        mut self,
        batches: usize,
        reason: CancellationReason,
    ) -> Self {
        self.cancel_after_batches = Some((batches, reason));
        self
    }
}

impl ParseProvider for MockParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let mut fact_index = 0_usize;
        let mut diagnostic_index = 0_usize;
        let mut emitted_batches = 0_usize;
        while fact_index < self.facts.len() || diagnostic_index < self.diagnostics.len() {
            cancel_at_boundary(self.cancel_after_batches, emitted_batches, cancellation);
            cancellation.check()?;
            let budget = sink.remaining_budget();
            let mut facts = Vec::new();
            let mut diagnostics = Vec::new();
            let mut batch_usage = empty_batch_usage();
            while fact_index < self.facts.len() {
                let item_usage = SyntaxFactBatch::new(
                    sink.next_sequence(),
                    vec![self.facts[fact_index].clone()],
                    Vec::new(),
                )
                .usage()?;
                let candidate_usage = combine_batch_usage(batch_usage, item_usage)?;
                if !usage_fits(candidate_usage, budget) {
                    break;
                }
                batch_usage = candidate_usage;
                facts.push(self.facts[fact_index].clone());
                fact_index += 1;
            }
            while diagnostic_index < self.diagnostics.len() {
                let item_usage = SyntaxFactBatch::new(
                    sink.next_sequence(),
                    Vec::new(),
                    vec![self.diagnostics[diagnostic_index].clone()],
                )
                .usage()?;
                let candidate_usage = combine_batch_usage(batch_usage, item_usage)?;
                if !usage_fits(candidate_usage, budget) {
                    break;
                }
                batch_usage = candidate_usage;
                diagnostics.push(self.diagnostics[diagnostic_index].clone());
                diagnostic_index += 1;
            }
            if facts.is_empty() && diagnostics.is_empty() {
                if fact_index < self.facts.len() {
                    facts.push(self.facts[fact_index].clone());
                    fact_index += 1;
                } else {
                    diagnostics.push(self.diagnostics[diagnostic_index].clone());
                    diagnostic_index += 1;
                }
            }
            sink.push(SyntaxFactBatch::new(
                sink.next_sequence(),
                facts,
                diagnostics,
            ))?;
            emitted_batches += 1;
        }
        cancellation.check()?;
        let usage = sink.staged_usage();
        report(
            self.coverage.clone(),
            ResourceUsage::new(
                request.source().bytes().len(),
                self.facts.len(),
                self.syntax_nodes,
                self.facts.iter().map(SyntaxFact::depth).max().unwrap_or(0),
                self.reported_memory_bytes,
                usage,
            ),
            sink.next_sequence(),
        )
    }
}

/// Deterministic analyzer double that produces canonicalizable IR batches.
#[derive(Debug, Clone)]
pub struct MockLanguageAnalyzer {
    descriptor: ProducerDescriptor,
    records: Vec<IrRecord>,
    coverage: CoverageReport,
    syntax_nodes: usize,
    max_syntax_depth: usize,
    reported_memory_bytes: Option<usize>,
    cancel_after_batches: Option<(usize, CancellationReason)>,
}

impl MockLanguageAnalyzer {
    /// Creates an in-process normalized-IR analyzer double.
    #[must_use]
    pub fn new(
        descriptor: ProducerDescriptor,
        records: Vec<IrRecord>,
        coverage: CoverageReport,
        max_syntax_depth: usize,
    ) -> Self {
        let reported_memory_bytes = match descriptor.memory_enforcement() {
            crate::MemoryEnforcement::AccountedInProcess => Some(0),
            _ => None,
        };
        Self {
            descriptor,
            records,
            coverage,
            syntax_nodes: 0,
            max_syntax_depth,
            reported_memory_bytes,
            cancel_after_batches: None,
        }
    }

    /// Configures concrete-syntax nodes observed by the analyzer.
    #[must_use]
    pub const fn with_syntax_nodes(mut self, nodes: usize) -> Self {
        self.syntax_nodes = nodes;
        self
    }

    /// Configures deterministic adapter-reported working memory.
    #[must_use]
    pub const fn with_reported_memory_bytes(mut self, bytes: usize) -> Self {
        self.reported_memory_bytes = Some(bytes);
        self
    }

    /// Requests cancellation immediately before the selected batch boundary.
    #[must_use]
    pub const fn with_cancellation_after_batches(
        mut self,
        batches: usize,
        reason: CancellationReason,
    ) -> Self {
        self.cancel_after_batches = Some((batches, reason));
        self
    }
}

impl LanguageAnalyzer for MockLanguageAnalyzer {
    fn descriptor(&self) -> &ProducerDescriptor {
        &self.descriptor
    }

    fn analyze(
        &self,
        request: &AnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<AnalysisReport, AdapterError> {
        let mut record_index = 0_usize;
        let mut emitted_batches = 0_usize;
        while record_index < self.records.len() {
            cancel_at_boundary(self.cancel_after_batches, emitted_batches, cancellation);
            cancellation.check()?;
            let budget = sink.remaining_budget();
            let mut records = Vec::new();
            let mut batch_usage = empty_batch_usage();
            while record_index < self.records.len() {
                let item_usage = IrBatch::new(
                    sink.next_sequence(),
                    vec![self.records[record_index].clone()],
                )
                .usage(request.limits().ir())?;
                let candidate_usage = combine_batch_usage(batch_usage, item_usage)?;
                if !usage_fits(candidate_usage, budget) {
                    break;
                }
                batch_usage = candidate_usage;
                records.push(self.records[record_index].clone());
                record_index += 1;
            }
            if records.is_empty() {
                records.push(self.records[record_index].clone());
                record_index += 1;
            }
            sink.push(IrBatch::new(sink.next_sequence(), records))?;
            emitted_batches += 1;
        }
        cancellation.check()?;
        let usage = sink.staged_usage();
        report(
            self.coverage.clone(),
            ResourceUsage::new(
                request.source().bytes().len(),
                self.records.len(),
                self.syntax_nodes,
                self.max_syntax_depth,
                self.reported_memory_bytes,
                usage,
            ),
            sink.next_sequence(),
        )
    }
}

fn cancel_at_boundary(
    configured: Option<(usize, CancellationReason)>,
    emitted_batches: usize,
    cancellation: &Cancellation,
) {
    if let Some((batch, reason)) = configured
        && batch == emitted_batches
    {
        cancellation.cancel(reason);
    }
}

const fn empty_batch_usage() -> StreamUsage {
    StreamUsage::new(1, 0, 0, 0, 0, 0)
}

fn combine_batch_usage(current: StreamUsage, item: StreamUsage) -> Result<StreamUsage, SinkError> {
    Ok(StreamUsage::new(
        1,
        current
            .records()
            .checked_add(item.records())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .output_bytes()
            .checked_add(item.output_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .diagnostics()
            .checked_add(item.diagnostics())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .diagnostic_bytes()
            .checked_add(item.diagnostic_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .string_bytes()
            .checked_add(item.string_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
    ))
}

fn usage_fits(usage: StreamUsage, budget: crate::RemainingBudget) -> bool {
    let batch = budget.batch();
    let remaining = budget.remaining();
    usage.batches() <= remaining.batches()
        && usage.records() <= batch.max_records().min(remaining.records())
        && usage.output_bytes() <= batch.max_output_bytes().min(remaining.output_bytes())
        && usage.diagnostics() <= batch.max_diagnostics().min(remaining.diagnostics())
        && usage.diagnostic_bytes()
            <= batch
                .max_diagnostic_bytes()
                .min(remaining.diagnostic_bytes())
        && usage.string_bytes() <= remaining.string_bytes()
}

fn report(
    coverage: CoverageReport,
    resources: ResourceUsage,
    next_sequence: u64,
) -> Result<WorkReport, AdapterError> {
    let usage = resources.stream();
    WorkReport::new(coverage, resources, StreamEnd::new(next_sequence, usage))
        .map_err(AdapterError::from)
}
