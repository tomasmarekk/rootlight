//! Deterministic normalized-IR usage accounting.
//!
//! Raw collection, string, nested-item, diagnostic, and extension quotas are
//! checked before canonical deduplication can remove repeated records.

use rootlight_ir::{FactEvidence, IrLimits, OccurrenceTarget};

use crate::{
    error::{ResourceKind, SinkError},
    limits::StreamUsage,
    sink::{IrBatch, IrRecord, IrRemainingBudget},
};

// Fixed logical weights keep backpressure identical across pointer widths and
// avoid treating architecture-dependent `size_of` values as a wire contract.
const LOGICAL_RECORD_OVERHEAD: usize = 64;
const LOGICAL_NESTED_ITEM_BYTES: usize = 24;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IrRawBudget {
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
    nested_items: usize,
    pub(crate) string_bytes: usize,
    extension_bytes: usize,
    pub(crate) diagnostic_bytes: usize,
}

impl IrRawBudget {
    pub(crate) fn total_records(self) -> usize {
        [
            self.files,
            self.entities,
            self.occurrences,
            self.relations,
            self.provenance,
            self.source_mappings,
            self.coverage,
            self.skipped_regions,
            self.diagnostics,
            self.extensions,
        ]
        .into_iter()
        .fold(0_usize, usize::saturating_add)
    }

    pub(crate) fn checked_add(self, other: Self) -> Result<Self, SinkError> {
        macro_rules! add {
            ($field:ident) => {
                self.$field
                    .checked_add(other.$field)
                    .ok_or(SinkError::AccountingOverflow)?
            };
        }
        Ok(Self {
            files: add!(files),
            entities: add!(entities),
            occurrences: add!(occurrences),
            relations: add!(relations),
            provenance: add!(provenance),
            source_mappings: add!(source_mappings),
            coverage: add!(coverage),
            skipped_regions: add!(skipped_regions),
            diagnostics: add!(diagnostics),
            extensions: add!(extensions),
            nested_items: add!(nested_items),
            string_bytes: add!(string_bytes),
            extension_bytes: add!(extension_bytes),
            diagnostic_bytes: add!(diagnostic_bytes),
        })
    }

    pub(crate) fn validate(self, limits: &IrLimits) -> Result<(), SinkError> {
        require_limit(ResourceKind::Records, self.files, limits.max_files)?;
        require_limit(ResourceKind::Records, self.entities, limits.max_entities)?;
        require_limit(
            ResourceKind::Records,
            self.occurrences,
            limits.max_occurrences,
        )?;
        require_limit(ResourceKind::Records, self.relations, limits.max_relations)?;
        require_limit(
            ResourceKind::Records,
            self.provenance,
            limits.max_provenance_records,
        )?;
        require_limit(
            ResourceKind::Records,
            self.source_mappings,
            limits.max_source_mappings,
        )?;
        require_limit(
            ResourceKind::Records,
            self.coverage,
            limits.max_coverage_records,
        )?;
        require_limit(
            ResourceKind::Records,
            self.skipped_regions,
            limits.max_skipped_regions,
        )?;
        require_limit(
            ResourceKind::Diagnostics,
            self.diagnostics,
            limits.max_diagnostics,
        )?;
        require_limit(
            ResourceKind::Records,
            self.extensions,
            limits.max_extensions,
        )?;
        require_limit(
            ResourceKind::Records,
            self.total_records(),
            limits.max_total_records,
        )?;
        require_limit(
            ResourceKind::NestedItems,
            self.nested_items,
            limits.max_total_nested_items,
        )?;
        require_limit(
            ResourceKind::StringBytes,
            self.string_bytes,
            limits.max_total_string_bytes,
        )?;
        require_limit(
            ResourceKind::ExtensionBytes,
            self.extension_bytes,
            limits.max_total_extension_bytes,
        )?;
        require_limit(
            ResourceKind::DiagnosticBytes,
            self.diagnostic_bytes,
            limits.max_total_diagnostic_bytes,
        )
    }

    pub(crate) fn remaining(self, limits: &IrLimits) -> IrRemainingBudget {
        IrRemainingBudget {
            files: limits.max_files.saturating_sub(self.files),
            entities: limits.max_entities.saturating_sub(self.entities),
            occurrences: limits.max_occurrences.saturating_sub(self.occurrences),
            relations: limits.max_relations.saturating_sub(self.relations),
            provenance: limits
                .max_provenance_records
                .saturating_sub(self.provenance),
            source_mappings: limits
                .max_source_mappings
                .saturating_sub(self.source_mappings),
            coverage: limits.max_coverage_records.saturating_sub(self.coverage),
            skipped_regions: limits
                .max_skipped_regions
                .saturating_sub(self.skipped_regions),
            diagnostics: limits.max_diagnostics.saturating_sub(self.diagnostics),
            extensions: limits.max_extensions.saturating_sub(self.extensions),
            total_records: limits
                .max_total_records
                .saturating_sub(self.total_records()),
            nested_items: limits
                .max_total_nested_items
                .saturating_sub(self.nested_items),
            string_bytes: limits
                .max_total_string_bytes
                .saturating_sub(self.string_bytes),
            extension_bytes: limits
                .max_total_extension_bytes
                .saturating_sub(self.extension_bytes),
            diagnostic_bytes: limits
                .max_total_diagnostic_bytes
                .saturating_sub(self.diagnostic_bytes),
        }
    }
}

pub(crate) struct IrBatchMetrics {
    pub(crate) usage: StreamUsage,
    pub(crate) raw: IrRawBudget,
}

pub(crate) fn ir_batch_metrics(
    batch: &IrBatch,
    limits: &IrLimits,
) -> Result<IrBatchMetrics, SinkError> {
    let mut raw = IrRawBudget::default();
    let mut output_bytes = 0_usize;
    for record in batch.records() {
        let metrics = ir_record_metrics(record, limits)?;
        raw = raw.checked_add(metrics.raw)?;
        output_bytes = output_bytes
            .checked_add(metrics.usage.output_bytes())
            .ok_or(SinkError::AccountingOverflow)?;
    }
    let usage = StreamUsage::new(
        1,
        batch.records().len(),
        output_bytes,
        raw.diagnostics,
        raw.diagnostic_bytes,
        raw.string_bytes,
    );
    raw.validate(limits)?;
    Ok(IrBatchMetrics { usage, raw })
}

fn ir_record_metrics(record: &IrRecord, limits: &IrLimits) -> Result<IrBatchMetrics, SinkError> {
    let mut meter = RecordMeter::new(limits);
    match record {
        IrRecord::File(record) => {
            meter.raw.files = 1;
            meter.evidence(&record.evidence)?;
            meter.string(&record.path)?;
            meter.string(&record.language)?;
            meter.string(&record.encoding)?;
        }
        IrRecord::Entity(record) => {
            meter.raw.entities = 1;
            meter.nested(record.flags.len())?;
            meter.evidence(&record.evidence)?;
            meter.string(&record.language)?;
            meter.string(&record.canonical_name)?;
            meter.string(&record.display_name)?;
            meter.string(&record.qualified_name)?;
        }
        IrRecord::Occurrence(record) => {
            meter.raw.occurrences = 1;
            if let OccurrenceTarget::Candidates { symbols, .. } = &record.target {
                meter.nested(symbols.len())?;
            }
            meter.evidence(&record.evidence)?;
            meter.string(&record.syntax_kind)?;
        }
        IrRecord::Relation(record) => {
            meter.raw.relations = 1;
            meter.evidence(&record.evidence)?;
        }
        IrRecord::Provenance(record) => {
            meter.raw.provenance = 1;
            meter.nested(record.input_sources.len())?;
            meter.nested(record.evidence_sources.len())?;
            meter.nested(record.derivation_parents.len())?;
            meter.string(record.producer.name())?;
            meter.string(record.producer.version())?;
            meter.string(&record.language)?;
            if let Some(version) = &record.frontend_version {
                meter.string(version)?;
            }
            if let Some(rule) = &record.rule {
                meter.string(rule)?;
            }
        }
        IrRecord::SourceMapping(record) => {
            meter.raw.source_mappings = 1;
            meter.evidence(&record.evidence)?;
        }
        IrRecord::Coverage(record) => {
            meter.raw.coverage = 1;
            meter.evidence(&record.evidence)?;
        }
        IrRecord::SkippedRegion(record) => {
            meter.raw.skipped_regions = 1;
            meter.evidence(&record.evidence)?;
            meter.string(&record.detail)?;
        }
        IrRecord::Diagnostic(record) => {
            meter.raw.diagnostics = 1;
            meter.evidence(&record.evidence)?;
            meter.string(&record.code)?;
            meter.string(&record.message)?;
            if record.message.len() > limits.max_diagnostic_message_bytes {
                return Err(SinkError::StreamLimit {
                    resource: ResourceKind::DiagnosticBytes,
                    observed: record.message.len(),
                    limit: limits.max_diagnostic_message_bytes,
                });
            }
            meter.raw.diagnostic_bytes = record
                .code
                .len()
                .checked_add(record.message.len())
                .ok_or(SinkError::AccountingOverflow)?;
        }
        IrRecord::Extension(record) => {
            meter.raw.extensions = 1;
            meter.evidence(&record.evidence)?;
            meter.string(&record.namespace)?;
            meter.string(&record.version)?;
            if record.payload.len() > limits.max_extension_payload_bytes {
                return Err(SinkError::StreamLimit {
                    resource: ResourceKind::ExtensionBytes,
                    observed: record.payload.len(),
                    limit: limits.max_extension_payload_bytes,
                });
            }
            meter.raw.extension_bytes = record.payload.len();
            meter.output_bytes = meter
                .output_bytes
                .checked_add(record.payload.len())
                .ok_or(SinkError::AccountingOverflow)?;
        }
    }
    Ok(IrBatchMetrics {
        usage: StreamUsage::new(
            0,
            1,
            meter.output_bytes,
            meter.raw.diagnostics,
            meter.raw.diagnostic_bytes,
            meter.raw.string_bytes,
        ),
        raw: meter.raw,
    })
}

struct RecordMeter<'a> {
    limits: &'a IrLimits,
    raw: IrRawBudget,
    output_bytes: usize,
}

impl<'a> RecordMeter<'a> {
    const fn new(limits: &'a IrLimits) -> Self {
        Self {
            limits,
            raw: IrRawBudget {
                files: 0,
                entities: 0,
                occurrences: 0,
                relations: 0,
                provenance: 0,
                source_mappings: 0,
                coverage: 0,
                skipped_regions: 0,
                diagnostics: 0,
                extensions: 0,
                nested_items: 0,
                string_bytes: 0,
                extension_bytes: 0,
                diagnostic_bytes: 0,
            },
            output_bytes: LOGICAL_RECORD_OVERHEAD,
        }
    }

    fn evidence(&mut self, evidence: &FactEvidence) -> Result<(), SinkError> {
        self.nested(evidence.derivation.len())
    }

    fn nested(&mut self, observed: usize) -> Result<(), SinkError> {
        if observed > self.limits.max_nested_items_per_record {
            return Err(SinkError::StreamLimit {
                resource: ResourceKind::NestedItems,
                observed,
                limit: self.limits.max_nested_items_per_record,
            });
        }
        self.raw.nested_items = self
            .raw
            .nested_items
            .checked_add(observed)
            .ok_or(SinkError::AccountingOverflow)?;
        self.output_bytes = self
            .output_bytes
            .checked_add(
                observed
                    .checked_mul(LOGICAL_NESTED_ITEM_BYTES)
                    .ok_or(SinkError::AccountingOverflow)?,
            )
            .ok_or(SinkError::AccountingOverflow)?;
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), SinkError> {
        if value.len() > self.limits.max_string_bytes {
            return Err(SinkError::StreamLimit {
                resource: ResourceKind::StringBytes,
                observed: value.len(),
                limit: self.limits.max_string_bytes,
            });
        }
        self.raw.string_bytes = self
            .raw
            .string_bytes
            .checked_add(value.len())
            .ok_or(SinkError::AccountingOverflow)?;
        self.output_bytes = self
            .output_bytes
            .checked_add(value.len())
            .ok_or(SinkError::AccountingOverflow)?;
        Ok(())
    }
}

fn require_limit(resource: ResourceKind, observed: usize, limit: usize) -> Result<(), SinkError> {
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
