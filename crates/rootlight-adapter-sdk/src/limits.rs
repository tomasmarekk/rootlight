//! Explicit stream and analysis resource limits.
//!
//! The SDK supplies no product-policy defaults: every request carries checked
//! limits selected by its caller, including fixed per-batch thresholds.

use rootlight_ir::IrLimits;

use crate::error::{LimitError, SinkError};

/// Fixed thresholds applied independently to every emitted batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchThresholds {
    max_records: usize,
    max_output_bytes: usize,
    max_diagnostics: usize,
    max_diagnostic_bytes: usize,
}

impl BatchThresholds {
    /// Creates nonzero per-batch thresholds.
    ///
    /// # Errors
    ///
    /// Returns [`LimitError::Zero`] when any threshold is zero.
    pub fn new(
        max_records: usize,
        max_output_bytes: usize,
        max_diagnostics: usize,
        max_diagnostic_bytes: usize,
    ) -> Result<Self, LimitError> {
        require_nonzero("batch.max_records", max_records)?;
        require_nonzero("batch.max_output_bytes", max_output_bytes)?;
        require_nonzero("batch.max_diagnostics", max_diagnostics)?;
        require_nonzero("batch.max_diagnostic_bytes", max_diagnostic_bytes)?;
        Ok(Self {
            max_records,
            max_output_bytes,
            max_diagnostics,
            max_diagnostic_bytes,
        })
    }

    /// Returns the maximum facts or IR records in one batch.
    #[must_use]
    pub const fn max_records(self) -> usize {
        self.max_records
    }

    /// Returns the maximum deterministically accounted bytes in one batch.
    #[must_use]
    pub const fn max_output_bytes(self) -> usize {
        self.max_output_bytes
    }

    /// Returns the maximum diagnostics in one batch.
    #[must_use]
    pub const fn max_diagnostics(self) -> usize {
        self.max_diagnostics
    }

    /// Returns the maximum diagnostic UTF-8 bytes in one batch.
    #[must_use]
    pub const fn max_diagnostic_bytes(self) -> usize {
        self.max_diagnostic_bytes
    }
}

/// Whole-stream quotas plus immutable per-batch thresholds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLimits {
    max_batches: usize,
    max_records: usize,
    max_output_bytes: usize,
    max_diagnostics: usize,
    max_diagnostic_bytes: usize,
    max_string_bytes: usize,
    batch: BatchThresholds,
}

impl StreamLimits {
    /// Creates checked cumulative stream limits.
    ///
    /// # Errors
    ///
    /// Returns [`LimitError`] for zero totals or a batch threshold larger than
    /// its corresponding cumulative limit.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_batches: usize,
        max_records: usize,
        max_output_bytes: usize,
        max_diagnostics: usize,
        max_diagnostic_bytes: usize,
        max_string_bytes: usize,
        batch: BatchThresholds,
    ) -> Result<Self, LimitError> {
        require_nonzero("stream.max_batches", max_batches)?;
        require_nonzero("stream.max_records", max_records)?;
        require_nonzero("stream.max_output_bytes", max_output_bytes)?;
        require_nonzero("stream.max_diagnostics", max_diagnostics)?;
        require_nonzero("stream.max_diagnostic_bytes", max_diagnostic_bytes)?;
        require_nonzero("stream.max_string_bytes", max_string_bytes)?;
        require_batch_within_stream("records", batch.max_records, max_records)?;
        require_batch_within_stream("output_bytes", batch.max_output_bytes, max_output_bytes)?;
        require_batch_within_stream("diagnostics", batch.max_diagnostics, max_diagnostics)?;
        require_batch_within_stream(
            "diagnostic_bytes",
            batch.max_diagnostic_bytes,
            max_diagnostic_bytes,
        )?;
        Ok(Self {
            max_batches,
            max_records,
            max_output_bytes,
            max_diagnostics,
            max_diagnostic_bytes,
            max_string_bytes,
            batch,
        })
    }

    /// Returns the maximum batch count.
    #[must_use]
    pub const fn max_batches(&self) -> usize {
        self.max_batches
    }

    /// Returns the maximum raw record count before deduplication.
    #[must_use]
    pub const fn max_records(&self) -> usize {
        self.max_records
    }

    /// Returns the maximum accounted logical output bytes.
    #[must_use]
    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Returns the maximum diagnostic count.
    #[must_use]
    pub const fn max_diagnostics(&self) -> usize {
        self.max_diagnostics
    }

    /// Returns the maximum diagnostic UTF-8 bytes.
    #[must_use]
    pub const fn max_diagnostic_bytes(&self) -> usize {
        self.max_diagnostic_bytes
    }

    /// Returns the maximum cumulative non-payload UTF-8 bytes.
    #[must_use]
    pub const fn max_string_bytes(&self) -> usize {
        self.max_string_bytes
    }

    /// Returns the fixed thresholds for every batch.
    #[must_use]
    pub const fn batch(&self) -> BatchThresholds {
        self.batch
    }
}

/// Explicit checked limits carried by every parse or analysis request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisLimits {
    max_source_bytes: usize,
    max_syntax_nodes: usize,
    max_syntax_depth: usize,
    max_embedded_ranges: usize,
    max_reported_memory_bytes: usize,
    syntax_stream: StreamLimits,
    ir_stream: StreamLimits,
    ir: IrLimits,
}

impl AnalysisLimits {
    /// Creates an explicit limit set without applying product-policy defaults.
    ///
    /// `max_embedded_ranges` may be zero to disable embedded parsing.
    ///
    /// # Errors
    ///
    /// Returns [`LimitError::Zero`] when a universally required bound is zero.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_source_bytes: usize,
        max_syntax_nodes: usize,
        max_syntax_depth: usize,
        max_embedded_ranges: usize,
        max_reported_memory_bytes: usize,
        syntax_stream: StreamLimits,
        ir_stream: StreamLimits,
        ir: IrLimits,
    ) -> Result<Self, LimitError> {
        require_nonzero("analysis.max_source_bytes", max_source_bytes)?;
        require_nonzero("analysis.max_syntax_nodes", max_syntax_nodes)?;
        require_nonzero("analysis.max_syntax_depth", max_syntax_depth)?;
        require_nonzero(
            "analysis.max_reported_memory_bytes",
            max_reported_memory_bytes,
        )?;
        Ok(Self {
            max_source_bytes,
            max_syntax_nodes,
            max_syntax_depth,
            max_embedded_ranges,
            max_reported_memory_bytes,
            syntax_stream,
            ir_stream,
            ir,
        })
    }

    /// Returns the admitted source-file byte ceiling.
    #[must_use]
    pub const fn max_source_bytes(&self) -> usize {
        self.max_source_bytes
    }

    /// Returns the admitted concrete-syntax node count.
    #[must_use]
    pub const fn max_syntax_nodes(&self) -> usize {
        self.max_syntax_nodes
    }

    /// Returns the admitted syntax nesting depth.
    #[must_use]
    pub const fn max_syntax_depth(&self) -> usize {
        self.max_syntax_depth
    }

    /// Returns the admitted embedded-range count.
    #[must_use]
    pub const fn max_embedded_ranges(&self) -> usize {
        self.max_embedded_ranges
    }

    /// Returns the ceiling for adapter-reported in-process memory.
    ///
    /// This bounds cooperative provider accounting and is not a hostile-process
    /// memory guarantee.
    #[must_use]
    pub const fn max_reported_memory_bytes(&self) -> usize {
        self.max_reported_memory_bytes
    }

    /// Returns syntax-fact stream limits.
    #[must_use]
    pub const fn syntax_stream(&self) -> &StreamLimits {
        &self.syntax_stream
    }

    /// Returns normalized-IR stream limits.
    #[must_use]
    pub const fn ir_stream(&self) -> &StreamLimits {
        &self.ir_stream
    }

    /// Returns detailed normalized-IR raw quotas.
    #[must_use]
    pub const fn ir(&self) -> &IrLimits {
        &self.ir
    }
}

/// Deterministic raw usage accumulated for one stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamUsage {
    batches: usize,
    records: usize,
    output_bytes: usize,
    diagnostics: usize,
    diagnostic_bytes: usize,
    string_bytes: usize,
}

impl StreamUsage {
    /// Creates an exact usage report.
    #[must_use]
    pub const fn new(
        batches: usize,
        records: usize,
        output_bytes: usize,
        diagnostics: usize,
        diagnostic_bytes: usize,
        string_bytes: usize,
    ) -> Self {
        Self {
            batches,
            records,
            output_bytes,
            diagnostics,
            diagnostic_bytes,
            string_bytes,
        }
    }

    /// Returns accepted batch count.
    #[must_use]
    pub const fn batches(self) -> usize {
        self.batches
    }

    /// Returns raw record count before deduplication.
    #[must_use]
    pub const fn records(self) -> usize {
        self.records
    }

    /// Returns deterministically accounted logical bytes.
    #[must_use]
    pub const fn output_bytes(self) -> usize {
        self.output_bytes
    }

    /// Returns diagnostic count.
    #[must_use]
    pub const fn diagnostics(self) -> usize {
        self.diagnostics
    }

    /// Returns diagnostic code and message bytes.
    #[must_use]
    pub const fn diagnostic_bytes(self) -> usize {
        self.diagnostic_bytes
    }

    /// Returns non-payload string bytes.
    #[must_use]
    pub const fn string_bytes(self) -> usize {
        self.string_bytes
    }

    pub(crate) fn checked_add(self, other: Self) -> Result<Self, SinkError> {
        Ok(Self {
            batches: self
                .batches
                .checked_add(other.batches)
                .ok_or(SinkError::AccountingOverflow)?,
            records: self
                .records
                .checked_add(other.records)
                .ok_or(SinkError::AccountingOverflow)?,
            output_bytes: self
                .output_bytes
                .checked_add(other.output_bytes)
                .ok_or(SinkError::AccountingOverflow)?,
            diagnostics: self
                .diagnostics
                .checked_add(other.diagnostics)
                .ok_or(SinkError::AccountingOverflow)?,
            diagnostic_bytes: self
                .diagnostic_bytes
                .checked_add(other.diagnostic_bytes)
                .ok_or(SinkError::AccountingOverflow)?,
            string_bytes: self
                .string_bytes
                .checked_add(other.string_bytes)
                .ok_or(SinkError::AccountingOverflow)?,
        })
    }
}

/// Remaining cumulative budget and the immutable next-batch thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemainingBudget {
    remaining: StreamUsage,
    batch: BatchThresholds,
}

impl RemainingBudget {
    pub(crate) fn new(limits: &StreamLimits, used: StreamUsage) -> Self {
        Self {
            remaining: StreamUsage::new(
                limits.max_batches.saturating_sub(used.batches),
                limits.max_records.saturating_sub(used.records),
                limits.max_output_bytes.saturating_sub(used.output_bytes),
                limits.max_diagnostics.saturating_sub(used.diagnostics),
                limits
                    .max_diagnostic_bytes
                    .saturating_sub(used.diagnostic_bytes),
                limits.max_string_bytes.saturating_sub(used.string_bytes),
            ),
            batch: limits.batch,
        }
    }

    pub(crate) const fn from_parts(remaining: StreamUsage, batch: BatchThresholds) -> Self {
        Self { remaining, batch }
    }

    /// Returns remaining whole-stream usage.
    #[must_use]
    pub const fn remaining(self) -> StreamUsage {
        self.remaining
    }

    /// Returns the fixed threshold for the next batch.
    #[must_use]
    pub const fn batch(self) -> BatchThresholds {
        self.batch
    }
}

fn require_nonzero(field: &'static str, value: usize) -> Result<(), LimitError> {
    if value == 0 {
        Err(LimitError::Zero { field })
    } else {
        Ok(())
    }
}

fn require_batch_within_stream(
    field: &'static str,
    batch: usize,
    stream: usize,
) -> Result<(), LimitError> {
    if batch > stream {
        Err(LimitError::BatchExceedsStream {
            field,
            batch,
            stream,
        })
    } else {
        Ok(())
    }
}
