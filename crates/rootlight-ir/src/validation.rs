//! Validation and deterministic canonicalization for supported normalized IR versions.
//!
//! Limits are checked before deduplication. Canonicalization then normalizes all
//! collection order before referential and source-integrity checks run.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId};

use crate::{
    ContainerRef, CoverageScope, EntityKind, ExtensionCriticality, ExtensionEnvelope, FactEvidence,
    FactRef, NormalizedIrDocument, OccurrenceTarget, RelationEndpoint, SourceRef,
};

const DEFAULT_MAX_NESTED_ITEMS_PER_RECORD: usize = 4_096;
const DEFAULT_MAX_STRING_BYTES: usize = 32_768;
const DEFAULT_MAX_TOTAL_STRING_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_EXTENSION_PAYLOAD_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_TOTAL_EXTENSION_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_EXTENSION_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_RECORDS: usize = 10_000_000;
const JSON_ESCAPE_EXPANSION: usize = 6;
const MAX_ENCODED_FACT_REF_BYTES: usize = 320;
const FIXED_ENVELOPE_WIRE_BYTES: usize = 4 * 1024;
const MIN_DEFAULT_EXTENSION_ENVELOPE_BYTES: usize = checked_default_envelope_bytes();

const _: () = assert!(DEFAULT_MAX_EXTENSION_ENVELOPE_BYTES >= MIN_DEFAULT_EXTENSION_ENVELOPE_BYTES);

const fn checked_default_envelope_bytes() -> usize {
    let Some(metadata_bytes) = DEFAULT_MAX_STRING_BYTES.checked_mul(2) else {
        panic!("default extension metadata byte calculation overflowed");
    };
    let Some(decoded_text_bytes) = DEFAULT_MAX_EXTENSION_PAYLOAD_BYTES.checked_add(metadata_bytes)
    else {
        panic!("default extension text byte calculation overflowed");
    };
    let Some(escaped_text_bytes) = decoded_text_bytes.checked_mul(JSON_ESCAPE_EXPANSION) else {
        panic!("default escaped extension text calculation overflowed");
    };
    let Some(nested_bytes) =
        DEFAULT_MAX_NESTED_ITEMS_PER_RECORD.checked_mul(MAX_ENCODED_FACT_REF_BYTES)
    else {
        panic!("default nested extension evidence calculation overflowed");
    };
    let Some(variable_bytes) = escaped_text_bytes.checked_add(nested_bytes) else {
        panic!("default variable extension byte calculation overflowed");
    };
    let Some(total_bytes) = variable_bytes.checked_add(FIXED_ENVELOPE_WIRE_BYTES) else {
        panic!("default extension envelope byte calculation overflowed");
    };
    total_bytes
}

/// Resource limits applied to decoded IR documents and extension envelopes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IrLimits {
    /// Maximum bytes in the encoded JSON document before decoding.
    ///
    /// This encoded-size limit is independent of decoded string and payload
    /// quotas; JSON structure and escaping also consume it.
    pub max_document_bytes: usize,
    /// Maximum bytes in one encoded standalone extension envelope.
    ///
    /// The 8 MiB default covers six-byte worst-case JSON escaping for the
    /// one-MiB payload and two maximum metadata strings, 320 encoded bytes for
    /// each maximum nested fact reference, and fixed envelope metadata.
    pub max_extension_envelope_bytes: usize,
    /// Maximum file records before deduplication.
    pub max_files: usize,
    /// Maximum entity records before deduplication.
    pub max_entities: usize,
    /// Maximum occurrence records before deduplication.
    pub max_occurrences: usize,
    /// Maximum relation records before deduplication.
    pub max_relations: usize,
    /// Maximum provenance records before deduplication.
    pub max_provenance_records: usize,
    /// Maximum source-mapping records before deduplication.
    pub max_source_mappings: usize,
    /// Maximum coverage records before deduplication.
    pub max_coverage_records: usize,
    /// Maximum skipped regions before deduplication.
    pub max_skipped_regions: usize,
    /// Maximum diagnostic records before deduplication.
    pub max_diagnostics: usize,
    /// Maximum extension envelopes before deduplication.
    pub max_extensions: usize,
    /// Maximum total top-level records before deduplication.
    pub max_total_records: usize,
    /// Maximum items in any nested collection before deduplication.
    pub max_nested_items_per_record: usize,
    /// Maximum total items across all nested collections before deduplication.
    pub max_total_nested_items: usize,
    /// Maximum UTF-8 bytes in one non-payload string.
    pub max_string_bytes: usize,
    /// Maximum UTF-8 bytes across all non-payload strings.
    pub max_total_string_bytes: usize,
    /// Maximum UTF-8 bytes in one extension payload.
    pub max_extension_payload_bytes: usize,
    /// Maximum UTF-8 bytes across all extension payloads.
    pub max_total_extension_bytes: usize,
    /// Maximum UTF-8 bytes in one diagnostic message.
    pub max_diagnostic_message_bytes: usize,
    /// Maximum UTF-8 bytes across diagnostic codes and messages.
    pub max_total_diagnostic_bytes: usize,
}

impl IrLimits {
    /// Combined default UTF-8 capacity for non-payload strings and extension payloads.
    ///
    /// Diagnostic strings and extension metadata already belong to the
    /// non-payload quota. Encoded document size is limited independently.
    pub const DEFAULT_MAX_TOTAL_TEXT_BYTES: usize =
        match DEFAULT_MAX_TOTAL_STRING_BYTES.checked_add(DEFAULT_MAX_TOTAL_EXTENSION_BYTES) {
            Some(bytes) => bytes,
            None => panic!("default combined IR text capacity overflowed"),
        };
}

impl Default for IrLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: 512 * 1024 * 1024,
            max_extension_envelope_bytes: DEFAULT_MAX_EXTENSION_ENVELOPE_BYTES,
            max_files: 100_000,
            max_entities: 1_000_000,
            max_occurrences: 5_000_000,
            max_relations: 5_000_000,
            max_provenance_records: 2_000_000,
            max_source_mappings: 1_000_000,
            max_coverage_records: 500_000,
            // Concrete gap evidence competes for the aggregate record and byte
            // capacities instead of losing later files to a separate count cap.
            max_skipped_regions: DEFAULT_MAX_TOTAL_RECORDS,
            max_diagnostics: 10_000,
            max_extensions: 1_100_000,
            max_total_records: DEFAULT_MAX_TOTAL_RECORDS,
            max_nested_items_per_record: DEFAULT_MAX_NESTED_ITEMS_PER_RECORD,
            max_total_nested_items: 20_000_000,
            max_string_bytes: DEFAULT_MAX_STRING_BYTES,
            max_total_string_bytes: DEFAULT_MAX_TOTAL_STRING_BYTES,
            max_extension_payload_bytes: DEFAULT_MAX_EXTENSION_PAYLOAD_BYTES,
            max_total_extension_bytes: DEFAULT_MAX_TOTAL_EXTENSION_BYTES,
            max_diagnostic_message_bytes: 4_096,
            max_total_diagnostic_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Raw document quota usage measured before canonicalization or deduplication.
///
/// This fixed-size summary borrows records while measuring and owns no source
/// text. It does not establish identity validity, encoded size, or heap usage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IrDocumentUsage {
    files: usize,
    entities: usize,
    occurrences: usize,
    relations: usize,
    provenance_records: usize,
    source_mappings: usize,
    coverage_records: usize,
    skipped_regions: usize,
    diagnostics: usize,
    extensions: usize,
    total_records: usize,
    total_nested_items: usize,
    total_string_bytes: usize,
    total_extension_bytes: usize,
    total_diagnostic_bytes: usize,
    maximum_nested: (&'static str, usize),
    maximum_string: (&'static str, usize),
    maximum_payload: Option<(FactId, usize)>,
    maximum_diagnostic_message: Option<(FactId, usize)>,
}

impl IrDocumentUsage {
    /// Measures all raw quota dimensions using the document limit traversal.
    ///
    /// # Errors
    /// Returns a resource or entity-kind version failure, or accounting overflow.
    pub fn measure(
        document: &NormalizedIrDocument,
        limits: &IrLimits,
    ) -> Result<Self, IrDocumentUsageError> {
        Self::measure_with_checkpoint(document, limits, || true)
    }

    /// Measures usage with checkpoints before, during, and after traversal.
    ///
    /// Returning `false` stops at the next record or component boundary. No
    /// partial summary is returned, and the borrowed document is never changed.
    ///
    /// # Errors
    /// Returns [`IrDocumentUsageError::Interrupted`] when a checkpoint refuses
    /// work, or a validation or checked accounting failure.
    pub fn measure_with_checkpoint(
        document: &NormalizedIrDocument,
        limits: &IrLimits,
        mut checkpoint: impl FnMut() -> bool,
    ) -> Result<Self, IrDocumentUsageError> {
        collect_document_usage(
            document,
            limits,
            &mut || {
                if checkpoint() {
                    Ok(())
                } else {
                    Err(IrDocumentUsageError::Interrupted)
                }
            },
            checked_usage_add,
        )
    }

    /// Measures one borrowed extension, including its metadata and evidence.
    ///
    /// # Errors
    /// Returns a quota failure or accounting overflow, without identity checks.
    pub fn measure_extension(
        extension: &ExtensionEnvelope,
        limits: &IrLimits,
    ) -> Result<Self, IrDocumentUsageError> {
        let usage = Self {
            extensions: 1,
            total_records: 1,
            ..Self::default()
        };
        usage.validate_record_totals(limits)?;
        let mut checkpoint = || Ok(());
        let mut budget = NestedBudget::new(usage, &mut checkpoint, checked_usage_add);
        budget.extension(extension, limits)?;
        budget.finish(limits)
    }

    /// Measures one borrowed diagnostic, including strings and evidence.
    ///
    /// # Errors
    /// Returns a quota failure or accounting overflow, without identity checks.
    pub fn measure_diagnostic(
        diagnostic: &crate::DiagnosticRecord,
        limits: &IrLimits,
    ) -> Result<Self, IrDocumentUsageError> {
        let usage = Self {
            diagnostics: 1,
            total_records: 1,
            ..Self::default()
        };
        usage.validate_record_totals(limits)?;
        let mut checkpoint = || Ok(());
        let mut budget = NestedBudget::new(usage, &mut checkpoint, checked_usage_add);
        budget.diagnostic(diagnostic, limits)?;
        budget.finish(limits)
    }

    /// Combines raw summaries and checks every aggregate and per-record quota.
    ///
    /// Retained maximum witnesses allow a stricter `limits` value without
    /// revisiting records. Counts remain pre-deduplication, including duplicates.
    ///
    /// # Errors
    /// Returns accounting overflow or a quota failure. The caller commits the
    /// returned summary only after this operation succeeds.
    pub fn checked_add(self, other: Self, limits: &IrLimits) -> Result<Self, IrDocumentUsageError> {
        let combined = Self {
            files: checked_usage_add(self.files, other.files)?,
            entities: checked_usage_add(self.entities, other.entities)?,
            occurrences: checked_usage_add(self.occurrences, other.occurrences)?,
            relations: checked_usage_add(self.relations, other.relations)?,
            provenance_records: checked_usage_add(
                self.provenance_records,
                other.provenance_records,
            )?,
            source_mappings: checked_usage_add(self.source_mappings, other.source_mappings)?,
            coverage_records: checked_usage_add(self.coverage_records, other.coverage_records)?,
            skipped_regions: checked_usage_add(self.skipped_regions, other.skipped_regions)?,
            diagnostics: checked_usage_add(self.diagnostics, other.diagnostics)?,
            extensions: checked_usage_add(self.extensions, other.extensions)?,
            total_records: checked_usage_add(self.total_records, other.total_records)?,
            total_nested_items: checked_usage_add(
                self.total_nested_items,
                other.total_nested_items,
            )?,
            total_string_bytes: checked_usage_add(
                self.total_string_bytes,
                other.total_string_bytes,
            )?,
            total_extension_bytes: checked_usage_add(
                self.total_extension_bytes,
                other.total_extension_bytes,
            )?,
            total_diagnostic_bytes: checked_usage_add(
                self.total_diagnostic_bytes,
                other.total_diagnostic_bytes,
            )?,
            maximum_nested: larger_named_count(self.maximum_nested, other.maximum_nested),
            maximum_string: larger_named_count(self.maximum_string, other.maximum_string),
            maximum_payload: larger_fact_count(self.maximum_payload, other.maximum_payload),
            maximum_diagnostic_message: larger_fact_count(
                self.maximum_diagnostic_message,
                other.maximum_diagnostic_message,
            ),
        };
        combined.validate_record_totals(limits)?;
        combined.validate_maxima(limits)?;
        combined.validate_nested_totals(limits)?;
        Ok(combined)
    }

    /// Subtracts removed raw quota counts without rescanning retained records.
    ///
    /// The caller establishes that `other` describes records actually removed.
    /// Per-record maxima remain conservative witnesses from `self`, even when
    /// the record providing a maximum was removed.
    ///
    /// # Errors
    /// Returns [`IrDocumentUsageError::AccountingOverflow`] for any underflow.
    pub fn checked_sub(self, other: Self) -> Result<Self, IrDocumentUsageError> {
        Ok(Self {
            files: checked_usage_sub(self.files, other.files)?,
            entities: checked_usage_sub(self.entities, other.entities)?,
            occurrences: checked_usage_sub(self.occurrences, other.occurrences)?,
            relations: checked_usage_sub(self.relations, other.relations)?,
            provenance_records: checked_usage_sub(
                self.provenance_records,
                other.provenance_records,
            )?,
            source_mappings: checked_usage_sub(self.source_mappings, other.source_mappings)?,
            coverage_records: checked_usage_sub(self.coverage_records, other.coverage_records)?,
            skipped_regions: checked_usage_sub(self.skipped_regions, other.skipped_regions)?,
            diagnostics: checked_usage_sub(self.diagnostics, other.diagnostics)?,
            extensions: checked_usage_sub(self.extensions, other.extensions)?,
            total_records: checked_usage_sub(self.total_records, other.total_records)?,
            total_nested_items: checked_usage_sub(
                self.total_nested_items,
                other.total_nested_items,
            )?,
            total_string_bytes: checked_usage_sub(
                self.total_string_bytes,
                other.total_string_bytes,
            )?,
            total_extension_bytes: checked_usage_sub(
                self.total_extension_bytes,
                other.total_extension_bytes,
            )?,
            total_diagnostic_bytes: checked_usage_sub(
                self.total_diagnostic_bytes,
                other.total_diagnostic_bytes,
            )?,
            maximum_nested: self.maximum_nested,
            maximum_string: self.maximum_string,
            maximum_payload: self.maximum_payload,
            maximum_diagnostic_message: self.maximum_diagnostic_message,
        })
    }

    /// Returns raw file records before deduplication.
    #[must_use]
    pub fn files(&self) -> usize {
        self.files
    }
    /// Returns raw entity records before deduplication.
    #[must_use]
    pub fn entities(&self) -> usize {
        self.entities
    }
    /// Returns raw occurrence records before deduplication.
    #[must_use]
    pub fn occurrences(&self) -> usize {
        self.occurrences
    }
    /// Returns raw relation records before deduplication.
    #[must_use]
    pub fn relations(&self) -> usize {
        self.relations
    }
    /// Returns raw provenance records before deduplication.
    #[must_use]
    pub fn provenance_records(&self) -> usize {
        self.provenance_records
    }
    /// Returns raw source-mapping records before deduplication.
    #[must_use]
    pub fn source_mappings(&self) -> usize {
        self.source_mappings
    }
    /// Returns raw coverage records before deduplication.
    #[must_use]
    pub fn coverage_records(&self) -> usize {
        self.coverage_records
    }
    /// Returns raw skipped regions before deduplication.
    #[must_use]
    pub fn skipped_regions(&self) -> usize {
        self.skipped_regions
    }
    /// Returns raw diagnostic records before deduplication.
    #[must_use]
    pub fn diagnostics(&self) -> usize {
        self.diagnostics
    }
    /// Returns raw extension envelopes before deduplication.
    #[must_use]
    pub fn extensions(&self) -> usize {
        self.extensions
    }
    /// Returns the sum of all ten raw record-domain counts.
    #[must_use]
    pub fn total_records(&self) -> usize {
        self.total_records
    }
    /// Returns raw nested collection items, including locator components.
    #[must_use]
    pub fn total_nested_items(&self) -> usize {
        self.total_nested_items
    }
    /// Returns non-payload UTF-8 string bytes, including diagnostic strings.
    #[must_use]
    pub fn total_string_bytes(&self) -> usize {
        self.total_string_bytes
    }
    /// Returns raw extension payload UTF-8 bytes, excluding envelope metadata.
    #[must_use]
    pub fn total_extension_bytes(&self) -> usize {
        self.total_extension_bytes
    }
    /// Returns diagnostic code and message UTF-8 bytes.
    #[must_use]
    pub fn total_diagnostic_bytes(&self) -> usize {
        self.total_diagnostic_bytes
    }

    fn collections(&self, limits: &IrLimits) -> [(&'static str, usize, usize); 10] {
        [
            ("files", self.files, limits.max_files),
            ("entities", self.entities, limits.max_entities),
            ("occurrences", self.occurrences, limits.max_occurrences),
            ("relations", self.relations, limits.max_relations),
            (
                "provenance",
                self.provenance_records,
                limits.max_provenance_records,
            ),
            (
                "source_mappings",
                self.source_mappings,
                limits.max_source_mappings,
            ),
            (
                "coverage_records",
                self.coverage_records,
                limits.max_coverage_records,
            ),
            (
                "skipped_regions",
                self.skipped_regions,
                limits.max_skipped_regions,
            ),
            ("diagnostics", self.diagnostics, limits.max_diagnostics),
            ("extensions", self.extensions, limits.max_extensions),
        ]
    }

    fn validate_record_totals(&self, limits: &IrLimits) -> Result<(), IrDocumentValidationError> {
        for (collection, observed, limit) in self.collections(limits) {
            check_collection_limit(collection, observed, limit)?;
        }
        if self.total_records > limits.max_total_records {
            return Err(IrDocumentValidationError::TotalRecordLimit {
                observed: self.total_records,
                limit: limits.max_total_records,
            });
        }
        Ok(())
    }

    fn validate_maxima(&self, limits: &IrLimits) -> Result<(), IrDocumentValidationError> {
        check_collection_limit(
            self.maximum_nested.0,
            self.maximum_nested.1,
            limits.max_nested_items_per_record,
        )?;
        if self.maximum_string.1 > limits.max_string_bytes {
            return Err(IrDocumentValidationError::StringLimit {
                field: self.maximum_string.0,
                observed: self.maximum_string.1,
                limit: limits.max_string_bytes,
            });
        }
        if let Some((id, observed)) = self.maximum_payload
            && observed > limits.max_extension_payload_bytes
        {
            return Err(IrDocumentValidationError::ExtensionPayloadLimit {
                id,
                observed,
                limit: limits.max_extension_payload_bytes,
            });
        }
        if let Some((id, observed)) = self.maximum_diagnostic_message
            && observed > limits.max_diagnostic_message_bytes
        {
            return Err(IrDocumentValidationError::DiagnosticMessageLimit {
                id,
                observed,
                limit: limits.max_diagnostic_message_bytes,
            });
        }
        Ok(())
    }

    fn validate_nested_totals(&self, limits: &IrLimits) -> Result<(), IrDocumentValidationError> {
        if self.total_nested_items > limits.max_total_nested_items {
            return Err(IrDocumentValidationError::TotalNestedItemLimit {
                observed: self.total_nested_items,
                limit: limits.max_total_nested_items,
            });
        }
        if self.total_string_bytes > limits.max_total_string_bytes {
            return Err(IrDocumentValidationError::TotalStringLimit {
                observed: self.total_string_bytes,
                limit: limits.max_total_string_bytes,
            });
        }
        if self.total_extension_bytes > limits.max_total_extension_bytes {
            return Err(IrDocumentValidationError::TotalExtensionBytesLimit {
                observed: self.total_extension_bytes,
                limit: limits.max_total_extension_bytes,
            });
        }
        if self.total_diagnostic_bytes > limits.max_total_diagnostic_bytes {
            return Err(IrDocumentValidationError::TotalDiagnosticBytesLimit {
                observed: self.total_diagnostic_bytes,
                limit: limits.max_total_diagnostic_bytes,
            });
        }
        Ok(())
    }
}

/// Failures specific to borrowed raw document quota accounting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IrDocumentUsageError {
    /// The existing resource or entity-kind version contract was rejected.
    #[error(transparent)]
    Validation(#[from] IrDocumentValidationError),
    /// Addition overflowed or subtraction underflowed the accounting integer.
    #[error("normalized IR usage accounting is out of range")]
    AccountingOverflow,
    /// A caller checkpoint stopped measurement before a complete summary.
    #[error("normalized IR usage measurement was interrupted")]
    Interrupted,
}

fn checked_usage_add(left: usize, right: usize) -> Result<usize, IrDocumentUsageError> {
    left.checked_add(right)
        .ok_or(IrDocumentUsageError::AccountingOverflow)
}

fn checked_usage_sub(left: usize, right: usize) -> Result<usize, IrDocumentUsageError> {
    left.checked_sub(right)
        .ok_or(IrDocumentUsageError::AccountingOverflow)
}

fn larger_named_count(
    left: (&'static str, usize),
    right: (&'static str, usize),
) -> (&'static str, usize) {
    if right.1 > left.1 { right } else { left }
}

fn larger_fact_count(
    left: Option<(FactId, usize)>,
    right: Option<(FactId, usize)>,
) -> Option<(FactId, usize)> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if right.1 > left.1 { right } else { left }),
        (left, right) => left.or(right),
    }
}

pub(crate) enum StandaloneExtensionValidationError {
    Limit,
    Identity,
}

pub(crate) fn validate_standalone_extension_envelope(
    envelope: &ExtensionEnvelope,
    limits: &IrLimits,
) -> Result<(), StandaloneExtensionValidationError> {
    let nested_items = envelope.evidence.derivation.len();
    if nested_items > limits.max_nested_items_per_record
        || nested_items > limits.max_total_nested_items
    {
        return Err(StandaloneExtensionValidationError::Limit);
    }

    let namespace_bytes = envelope.namespace.len();
    let version_bytes = envelope.version.len();
    if namespace_bytes > limits.max_string_bytes || version_bytes > limits.max_string_bytes {
        return Err(StandaloneExtensionValidationError::Limit);
    }
    let Some(total_string_bytes) = namespace_bytes.checked_add(version_bytes) else {
        return Err(StandaloneExtensionValidationError::Limit);
    };
    if total_string_bytes > limits.max_total_string_bytes {
        return Err(StandaloneExtensionValidationError::Limit);
    }

    let payload_bytes = envelope.payload.len();
    if payload_bytes > limits.max_extension_payload_bytes
        || payload_bytes > limits.max_total_extension_bytes
    {
        return Err(StandaloneExtensionValidationError::Limit);
    }

    if !valid_extension_namespace(&envelope.namespace)
        || !valid_extension_version(&envelope.version)
        || (envelope.namespace == crate::VBA_COMPILATION_NAMESPACE
            && crate::decode_vba_compilation_envelope(envelope).is_err())
    {
        return Err(StandaloneExtensionValidationError::Identity);
    }
    Ok(())
}

/// Identity of an extension namespace version understood by the core.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtensionIdentifier {
    /// Extension namespace.
    pub namespace: String,
    /// Namespace-specific version.
    pub version: String,
}

impl ExtensionIdentifier {
    /// Creates an extension identity used by validation policy.
    #[must_use]
    pub fn new(namespace: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            version: version.into(),
        }
    }
}

/// Policy for unknown noncritical extension envelopes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnknownNoncriticalExtensionPolicy {
    /// Preserve the opaque envelope in canonical output.
    #[default]
    Preserve,
    /// Skip the opaque envelope while preserving all common facts.
    Skip,
}

/// Declared extension support used by validation and canonicalization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExtensionSupport {
    /// Critical namespace versions understood by the caller.
    pub supported_critical: BTreeSet<ExtensionIdentifier>,
    /// Handling for unknown noncritical envelopes.
    pub unknown_noncritical: UnknownNoncriticalExtensionPolicy,
}

/// Validates a normalized document without changing the caller's value.
///
/// Equal duplicate records are valid because canonicalization removes them.
/// Unequal records sharing an ID are rejected as collisions.
///
/// # Errors
///
/// Returns [`IrDocumentValidationError`] when any version, quota, ownership,
/// reference, source, evidence, cycle, coverage, or extension invariant fails.
pub fn validate_ir_document(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<(), IrDocumentValidationError> {
    canonicalize_ir_document_ref(document, limits, extensions).map(|_| ())
}

/// Checks document resource limits without cloning or canonicalizing facts.
///
/// This preflight does not validate identities, references, or evidence. It is
/// intended for atomic admission of optional fact groups before full validation.
///
/// # Errors
///
/// Returns [`IrDocumentValidationError`] when a resource limit is exceeded or
/// an entity kind is incompatible with the document version.
pub fn preflight_ir_document_limits(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), IrDocumentValidationError> {
    validate_limits(document, limits)
}

pub(crate) fn canonicalize_ir_document_ref(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<NormalizedIrDocument, IrDocumentValidationError> {
    validate_limits(document, limits)?;
    prepare_bounded_document(document.clone(), extensions)
}

pub(crate) fn validate_canonical_ir_document(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<(), IrDocumentValidationError> {
    validate_limits(document, limits)?;
    validate_canonical_collections(document, extensions)?;
    validate_extension_envelopes(document, extensions)?;
    validate_invariants(document, extensions)
}

/// Validates and deterministically canonicalizes a normalized document.
///
/// The returned document is independent of producer order, preserves equal
/// records once, and applies the declared unknown-noncritical extension policy.
///
/// # Errors
///
/// Returns [`IrDocumentValidationError`] under the same conditions as
/// [`validate_ir_document`].
pub fn canonicalize_ir_document(
    document: NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<NormalizedIrDocument, IrDocumentValidationError> {
    validate_limits(&document, limits)?;
    prepare_bounded_document(document, extensions)
}

fn prepare_bounded_document(
    mut document: NormalizedIrDocument,
    extensions: &ExtensionSupport,
) -> Result<NormalizedIrDocument, IrDocumentValidationError> {
    canonicalize_nested_collections(&mut document);
    canonicalize_top_level_collections(&mut document)?;
    validate_extension_envelopes(&document, extensions)?;
    validate_invariants(&document, extensions)?;
    let extension_count = document.extensions.len();
    apply_noncritical_extension_policy(&mut document, extensions);
    if document.extensions.len() != extension_count {
        validate_invariants(&document, extensions)?;
    }
    Ok(document)
}

fn validate_limits(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), IrDocumentValidationError> {
    // Existing validators retain their saturating quota/error contract; usage
    // admission uses checked arithmetic through the same borrowed traversal.
    collect_document_usage(document, limits, &mut || Ok(()), |left, right| {
        Ok(left.saturating_add(right))
    })
    .map(|_| ())
}

fn collect_document_usage<E: From<IrDocumentValidationError>>(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    add: fn(usize, usize) -> Result<usize, E>,
) -> Result<IrDocumentUsage, E> {
    checkpoint()?;
    let mut usage = IrDocumentUsage {
        files: document.files.len(),
        entities: document.entities.len(),
        occurrences: document.occurrences.len(),
        relations: document.relations.len(),
        provenance_records: document.provenance.len(),
        source_mappings: document.source_mappings.len(),
        coverage_records: document.coverage_records.len(),
        skipped_regions: document.skipped_regions.len(),
        diagnostics: document.diagnostics.len(),
        extensions: document.extensions.len(),
        ..IrDocumentUsage::default()
    };
    let mut total_records = 0_usize;
    for (collection, observed, limit) in usage.collections(limits) {
        checkpoint()?;
        check_collection_limit(collection, observed, limit).map_err(E::from)?;
        total_records = add(total_records, observed)?;
    }
    if total_records > limits.max_total_records {
        return Err(IrDocumentValidationError::TotalRecordLimit {
            observed: total_records,
            limit: limits.max_total_records,
        }
        .into());
    }
    usage.total_records = total_records;
    let mut budget = NestedBudget::new(usage, checkpoint, add);
    for file in &document.files {
        budget.checkpoint()?;
        budget.evidence("file.evidence.derivation", &file.evidence, limits)?;
        budget.string("file.path", &file.path, limits)?;
        if let Some(locator) = &file.path_locator {
            budget.nested(
                "file.path_locator.components",
                locator.components().len(),
                limits,
            )?;
            for component in locator.components() {
                budget.string("file.path_locator.component", component, limits)?;
            }
        }
        budget.string("file.language", &file.language, limits)?;
        budget.string("file.encoding", &file.encoding, limits)?;
    }
    for entity in &document.entities {
        budget.checkpoint()?;
        if document.version < entity.kind.minimum_ir_version() {
            return Err(IrDocumentValidationError::EntityKindVersion { kind: entity.kind }.into());
        }
        budget.nested("entity.flags", entity.flags.len(), limits)?;
        budget.evidence("entity.evidence.derivation", &entity.evidence, limits)?;
        budget.string("entity.language", &entity.language, limits)?;
        budget.string("entity.canonical_name", &entity.canonical_name, limits)?;
        budget.string("entity.display_name", &entity.display_name, limits)?;
        budget.string("entity.qualified_name", &entity.qualified_name, limits)?;
    }
    for occurrence in &document.occurrences {
        budget.checkpoint()?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &occurrence.target {
            budget.nested("occurrence.target.symbols", symbols.len(), limits)?;
        }
        budget.evidence(
            "occurrence.evidence.derivation",
            &occurrence.evidence,
            limits,
        )?;
        budget.string("occurrence.syntax_kind", &occurrence.syntax_kind, limits)?;
    }
    for relation in &document.relations {
        budget.checkpoint()?;
        budget.evidence("relation.evidence.derivation", &relation.evidence, limits)?;
    }
    for provenance in &document.provenance {
        budget.checkpoint()?;
        budget.nested(
            "provenance.input_sources",
            provenance.input_sources.len(),
            limits,
        )?;
        budget.nested(
            "provenance.evidence_sources",
            provenance.evidence_sources.len(),
            limits,
        )?;
        budget.nested(
            "provenance.derivation_parents",
            provenance.derivation_parents.len(),
            limits,
        )?;
        budget.string(
            "provenance.producer.name",
            provenance.producer.name(),
            limits,
        )?;
        budget.string(
            "provenance.producer.version",
            provenance.producer.version(),
            limits,
        )?;
        budget.string("provenance.language", &provenance.language, limits)?;
        if let Some(version) = &provenance.frontend_version {
            budget.string("provenance.frontend_version", version, limits)?;
        }
        if let Some(rule) = &provenance.rule {
            budget.string("provenance.rule", rule, limits)?;
        }
    }
    for mapping in &document.source_mappings {
        budget.checkpoint()?;
        budget.evidence(
            "source_mapping.evidence.derivation",
            &mapping.evidence,
            limits,
        )?;
    }
    for coverage in &document.coverage_records {
        budget.checkpoint()?;
        budget.evidence("coverage.evidence.derivation", &coverage.evidence, limits)?;
    }
    for skipped in &document.skipped_regions {
        budget.checkpoint()?;
        budget.evidence(
            "skipped_region.evidence.derivation",
            &skipped.evidence,
            limits,
        )?;
        budget.string("skipped_region.detail", &skipped.detail, limits)?;
    }
    for diagnostic in &document.diagnostics {
        budget.diagnostic(diagnostic, limits)?;
    }
    for extension in &document.extensions {
        budget.extension(extension, limits)?;
    }
    budget.finish(limits)
}

fn check_collection_limit(
    collection: &'static str,
    observed: usize,
    limit: usize,
) -> Result<(), IrDocumentValidationError> {
    if observed > limit {
        Err(IrDocumentValidationError::CollectionLimit {
            collection,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

struct NestedBudget<'a, E, C> {
    usage: IrDocumentUsage,
    checkpoint: &'a mut C,
    add: fn(usize, usize) -> Result<usize, E>,
}

impl<'a, E: From<IrDocumentValidationError>, C: FnMut() -> Result<(), E>> NestedBudget<'a, E, C> {
    fn new(
        usage: IrDocumentUsage,
        checkpoint: &'a mut C,
        add: fn(usize, usize) -> Result<usize, E>,
    ) -> Self {
        Self {
            usage,
            checkpoint,
            add,
        }
    }

    fn checkpoint(&mut self) -> Result<(), E> {
        (self.checkpoint)()
    }

    fn finish(mut self, limits: &IrLimits) -> Result<IrDocumentUsage, E> {
        self.usage.validate_nested_totals(limits).map_err(E::from)?;
        self.checkpoint()?;
        Ok(self.usage)
    }

    fn nested(
        &mut self,
        collection: &'static str,
        observed: usize,
        limits: &IrLimits,
    ) -> Result<(), E> {
        self.checkpoint()?;
        check_collection_limit(collection, observed, limits.max_nested_items_per_record)
            .map_err(E::from)?;
        self.usage.total_nested_items = (self.add)(self.usage.total_nested_items, observed)?;
        self.usage.maximum_nested =
            larger_named_count(self.usage.maximum_nested, (collection, observed));
        Ok(())
    }

    fn evidence(
        &mut self,
        collection: &'static str,
        evidence: &FactEvidence,
        limits: &IrLimits,
    ) -> Result<(), E> {
        self.nested(collection, evidence.derivation.len(), limits)
    }

    fn string(&mut self, field: &'static str, value: &str, limits: &IrLimits) -> Result<(), E> {
        self.checkpoint()?;
        let observed = value.len();
        if observed > limits.max_string_bytes {
            return Err(IrDocumentValidationError::StringLimit {
                field,
                observed,
                limit: limits.max_string_bytes,
            }
            .into());
        }
        self.usage.total_string_bytes = (self.add)(self.usage.total_string_bytes, observed)?;
        self.usage.maximum_string =
            larger_named_count(self.usage.maximum_string, (field, observed));
        Ok(())
    }

    fn diagnostic(
        &mut self,
        diagnostic: &crate::DiagnosticRecord,
        limits: &IrLimits,
    ) -> Result<(), E> {
        self.checkpoint()?;
        self.evidence(
            "diagnostic.evidence.derivation",
            &diagnostic.evidence,
            limits,
        )?;
        self.string("diagnostic.code", &diagnostic.code, limits)?;
        self.string("diagnostic.message", &diagnostic.message, limits)?;
        let message_bytes = diagnostic.message.len();
        if message_bytes > limits.max_diagnostic_message_bytes {
            return Err(IrDocumentValidationError::DiagnosticMessageLimit {
                id: diagnostic.id,
                observed: message_bytes,
                limit: limits.max_diagnostic_message_bytes,
            }
            .into());
        }
        self.usage.total_diagnostic_bytes =
            (self.add)(self.usage.total_diagnostic_bytes, diagnostic.code.len())?;
        self.usage.total_diagnostic_bytes =
            (self.add)(self.usage.total_diagnostic_bytes, message_bytes)?;
        self.usage.maximum_diagnostic_message = larger_fact_count(
            self.usage.maximum_diagnostic_message,
            Some((diagnostic.id, message_bytes)),
        );
        Ok(())
    }

    fn extension(&mut self, extension: &ExtensionEnvelope, limits: &IrLimits) -> Result<(), E> {
        self.checkpoint()?;
        self.evidence("extension.evidence.derivation", &extension.evidence, limits)?;
        self.string("extension.namespace", &extension.namespace, limits)?;
        self.string("extension.version", &extension.version, limits)?;
        let payload_bytes = extension.payload.len();
        if payload_bytes > limits.max_extension_payload_bytes {
            return Err(IrDocumentValidationError::ExtensionPayloadLimit {
                id: extension.id,
                observed: payload_bytes,
                limit: limits.max_extension_payload_bytes,
            }
            .into());
        }
        self.usage.total_extension_bytes =
            (self.add)(self.usage.total_extension_bytes, payload_bytes)?;
        self.usage.maximum_payload = larger_fact_count(
            self.usage.maximum_payload,
            Some((extension.id, payload_bytes)),
        );
        Ok(())
    }
}

fn canonicalize_nested_collections(document: &mut NormalizedIrDocument) {
    for file in &mut document.files {
        canonicalize_evidence(&mut file.evidence);
    }
    for entity in &mut document.entities {
        entity.flags.sort_unstable();
        entity.flags.dedup();
        canonicalize_evidence(&mut entity.evidence);
    }
    for occurrence in &mut document.occurrences {
        if let OccurrenceTarget::Candidates { symbols, .. } = &mut occurrence.target {
            symbols.sort_unstable();
            symbols.dedup();
        }
        canonicalize_evidence(&mut occurrence.evidence);
    }
    for relation in &mut document.relations {
        canonicalize_evidence(&mut relation.evidence);
    }
    for provenance in &mut document.provenance {
        provenance.input_sources.sort_unstable();
        provenance.input_sources.dedup();
        provenance.evidence_sources.sort_unstable();
        provenance.evidence_sources.dedup();
        provenance.derivation_parents.sort_unstable();
        provenance.derivation_parents.dedup();
    }
    for mapping in &mut document.source_mappings {
        canonicalize_evidence(&mut mapping.evidence);
    }
    for coverage in &mut document.coverage_records {
        canonicalize_evidence(&mut coverage.evidence);
    }
    for skipped in &mut document.skipped_regions {
        canonicalize_evidence(&mut skipped.evidence);
    }
    for diagnostic in &mut document.diagnostics {
        canonicalize_evidence(&mut diagnostic.evidence);
    }
    for extension in &mut document.extensions {
        canonicalize_evidence(&mut extension.evidence);
    }
}

fn canonicalize_evidence(evidence: &mut FactEvidence) {
    evidence.derivation.sort_unstable();
    evidence.derivation.dedup();
}

fn validate_canonical_collections(
    document: &NormalizedIrDocument,
    extensions: &ExtensionSupport,
) -> Result<(), IrDocumentValidationError> {
    let top_level_is_canonical = strictly_increasing_by(&document.files, |record| record.id)
        && strictly_increasing_by(&document.entities, |record| record.id)
        && strictly_increasing_by(&document.occurrences, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.relations, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.provenance, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.source_mappings, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.coverage_records, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.skipped_regions, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.diagnostics, IdentifiedFact::fact_id)
        && strictly_increasing_by(&document.extensions, IdentifiedFact::fact_id);
    let nested_is_canonical = document
        .files
        .iter()
        .all(|record| strictly_increasing(&record.evidence.derivation))
        && document.entities.iter().all(|record| {
            strictly_increasing(&record.flags) && strictly_increasing(&record.evidence.derivation)
        })
        && document.occurrences.iter().all(|record| {
            let candidates_are_canonical = match &record.target {
                OccurrenceTarget::Candidates { symbols, .. } => strictly_increasing(symbols),
                _ => true,
            };
            candidates_are_canonical && strictly_increasing(&record.evidence.derivation)
        })
        && document
            .relations
            .iter()
            .all(|record| strictly_increasing(&record.evidence.derivation))
        && document.provenance.iter().all(|record| {
            strictly_increasing(&record.input_sources)
                && strictly_increasing(&record.evidence_sources)
                && strictly_increasing(&record.derivation_parents)
        })
        && document
            .source_mappings
            .iter()
            .all(|record| strictly_increasing(&record.evidence.derivation))
        && document
            .coverage_records
            .iter()
            .all(|record| strictly_increasing(&record.evidence.derivation))
        && document
            .skipped_regions
            .iter()
            .all(|record| strictly_increasing(&record.evidence.derivation))
        && document
            .diagnostics
            .iter()
            .all(|record| strictly_increasing(&record.evidence.derivation))
        && document
            .extensions
            .iter()
            .all(|record| strictly_increasing(&record.evidence.derivation));
    let extension_policy_is_applied = extensions.unknown_noncritical
        != UnknownNoncriticalExtensionPolicy::Skip
        || document.extensions.iter().all(|extension| {
            extension.criticality == ExtensionCriticality::Critical
                || extension_is_supported(extension, extensions)
        });
    if top_level_is_canonical && nested_is_canonical && extension_policy_is_applied {
        Ok(())
    } else {
        Err(IrDocumentValidationError::NonCanonicalDocument)
    }
}

fn strictly_increasing<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn strictly_increasing_by<T, K: Ord + Copy>(values: &[T], key: fn(&T) -> K) -> bool {
    values.windows(2).all(|pair| key(&pair[0]) < key(&pair[1]))
}

fn apply_noncritical_extension_policy(
    document: &mut NormalizedIrDocument,
    support: &ExtensionSupport,
) {
    if support.unknown_noncritical == UnknownNoncriticalExtensionPolicy::Skip {
        document.extensions.retain(|extension| {
            extension.criticality == ExtensionCriticality::Critical
                || extension_is_supported(extension, support)
        });
    }
}

fn extension_is_supported(
    extension: &crate::ExtensionEnvelope,
    support: &ExtensionSupport,
) -> bool {
    support.supported_critical.contains(&ExtensionIdentifier {
        namespace: extension.namespace.clone(),
        version: extension.version.clone(),
    })
}

fn validate_extension_envelopes(
    document: &NormalizedIrDocument,
    support: &ExtensionSupport,
) -> Result<(), IrDocumentValidationError> {
    for extension in &document.extensions {
        if !valid_extension_namespace(&extension.namespace)
            || !valid_extension_version(&extension.version)
        {
            return Err(IrDocumentValidationError::InvalidExtensionIdentity {
                namespace: extension.namespace.clone(),
                version: extension.version.clone(),
            });
        }
        if extension.criticality == ExtensionCriticality::Critical
            && !extension_is_supported(extension, support)
        {
            return Err(IrDocumentValidationError::UnsupportedCriticalExtension {
                namespace: extension.namespace.clone(),
                version: extension.version.clone(),
            });
        }
    }
    Ok(())
}

fn canonicalize_top_level_collections(
    document: &mut NormalizedIrDocument,
) -> Result<(), IrDocumentValidationError> {
    sort_dedup_records(
        &mut document.files,
        |record| record.id,
        IrDocumentValidationError::DuplicateUnequalFileId,
    )?;
    sort_dedup_records(
        &mut document.entities,
        |record| record.id,
        IrDocumentValidationError::DuplicateUnequalSymbolId,
    )?;
    sort_dedup_fact_records(&mut document.occurrences)?;
    sort_dedup_fact_records(&mut document.relations)?;
    sort_dedup_fact_records(&mut document.provenance)?;
    sort_dedup_fact_records(&mut document.source_mappings)?;
    sort_dedup_fact_records(&mut document.coverage_records)?;
    sort_dedup_fact_records(&mut document.skipped_regions)?;
    sort_dedup_fact_records(&mut document.diagnostics)?;
    sort_dedup_fact_records(&mut document.extensions)?;

    let mut fact_ids = BTreeSet::new();
    for id in all_fact_ids(document) {
        if !fact_ids.insert(id) {
            return Err(IrDocumentValidationError::DuplicateUnequalFactId(id));
        }
    }
    Ok(())
}

fn sort_dedup_fact_records<T>(records: &mut Vec<T>) -> Result<(), IrDocumentValidationError>
where
    T: IdentifiedFact + PartialEq,
{
    sort_dedup_records(
        records,
        IdentifiedFact::fact_id,
        IrDocumentValidationError::DuplicateUnequalFactId,
    )
}

fn sort_dedup_records<T, I>(
    records: &mut Vec<T>,
    id: impl Fn(&T) -> I + Copy,
    collision: impl Fn(I) -> IrDocumentValidationError,
) -> Result<(), IrDocumentValidationError>
where
    T: PartialEq,
    I: Copy + Ord,
{
    records.sort_by_key(id);
    for pair in records.windows(2) {
        let Some([left, right]) = pair.get(..2) else {
            continue;
        };
        if id(left) == id(right) && left != right {
            return Err(collision(id(left)));
        }
    }
    records.dedup_by_key(|record| id(record));
    Ok(())
}

trait IdentifiedFact {
    fn fact_id(&self) -> FactId;
}

macro_rules! identified_fact {
    ($($type:ty),+ $(,)?) => {
        $(
            impl IdentifiedFact for $type {
                fn fact_id(&self) -> FactId {
                    self.id
                }
            }
        )+
    };
}

identified_fact!(
    crate::OccurrenceRecord,
    crate::RelationRecord,
    crate::ProvenanceRecord,
    crate::SourceMappingRecord,
    crate::CoverageRecord,
    crate::SkippedRegion,
    crate::DiagnosticRecord,
    crate::ExtensionEnvelope,
);

fn all_fact_ids(document: &NormalizedIrDocument) -> impl Iterator<Item = FactId> + '_ {
    document
        .occurrences
        .iter()
        .map(|record| record.id)
        .chain(document.relations.iter().map(|record| record.id))
        .chain(document.provenance.iter().map(|record| record.id))
        .chain(document.source_mappings.iter().map(|record| record.id))
        .chain(document.coverage_records.iter().map(|record| record.id))
        .chain(document.skipped_regions.iter().map(|record| record.id))
        .chain(document.diagnostics.iter().map(|record| record.id))
        .chain(document.extensions.iter().map(|record| record.id))
}

fn validate_invariants(
    document: &NormalizedIrDocument,
    extensions: &ExtensionSupport,
) -> Result<(), IrDocumentValidationError> {
    let files: BTreeMap<_, _> = document.files.iter().map(|file| (file.id, file)).collect();
    let entities: BTreeMap<_, _> = document
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect();
    let occurrences: BTreeSet<_> = document
        .occurrences
        .iter()
        .map(|occurrence| occurrence.id)
        .collect();
    let provenance: BTreeSet<_> = document.provenance.iter().map(|record| record.id).collect();
    let facts: BTreeSet<_> = all_fact_ids(document).collect();
    let mut compilation_scopes = BTreeSet::new();
    let compilation_parsers: BTreeMap<_, _> = if document
        .extensions
        .iter()
        .any(|extension| extension.namespace == crate::VBA_COMPILATION_NAMESPACE)
    {
        document
            .provenance
            .iter()
            .map(|record| (record.id, record))
            .collect()
    } else {
        BTreeMap::new()
    };

    for file in &document.files {
        validate_owner(
            "file",
            file.repository,
            file.generation,
            document.repository,
            document.generation,
        )?;
        validate_provenance_ref(file.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::File(file.id),
            &file.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }
    for entity in &document.entities {
        validate_owner(
            "entity",
            entity.repository,
            entity.generation,
            document.repository,
            document.generation,
        )?;
        if let Some(container) = entity.container {
            validate_container(container, document, &files, &entities)?;
        }
        validate_provenance_ref(entity.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Entity(entity.id),
            &entity.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }
    validate_container_cycles(document)?;

    for occurrence in &document.occurrences {
        validate_owner(
            "occurrence",
            occurrence.repository,
            occurrence.generation,
            document.repository,
            document.generation,
        )?;
        if !files.contains_key(&occurrence.file) {
            return Err(IrDocumentValidationError::MissingFile(occurrence.file));
        }
        validate_source(&occurrence.source, document, &files)?;
        if occurrence.source.span().file() != occurrence.file {
            return Err(IrDocumentValidationError::OccurrenceSourceFileMismatch {
                occurrence: occurrence.id,
                declared: occurrence.file,
                source_file: occurrence.source.span().file(),
            });
        }
        if let Some(enclosing) = occurrence.enclosing
            && !entities.contains_key(&enclosing)
        {
            return Err(IrDocumentValidationError::MissingEntity(enclosing));
        }
        validate_occurrence_target(occurrence, &entities)?;
        validate_provenance_ref(occurrence.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(occurrence.id),
            &occurrence.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }

    for relation in &document.relations {
        validate_owner(
            "relation",
            relation.repository,
            relation.generation,
            document.repository,
            document.generation,
        )?;
        validate_endpoint(relation.subject, document, &files, &entities, &occurrences)?;
        validate_endpoint(relation.object, document, &files, &entities, &occurrences)?;
        validate_provenance_ref(relation.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(relation.id),
            &relation.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }

    for record in &document.provenance {
        validate_owner(
            "provenance",
            record.repository,
            record.generation,
            document.repository,
            document.generation,
        )?;
        for source in record.input_sources.iter().chain(&record.evidence_sources) {
            validate_source(source, document, &files)?;
        }
        for parent in &record.derivation_parents {
            if *parent == FactRef::Fact(record.id) {
                return Err(IrDocumentValidationError::SelfDerivation(*parent));
            }
            validate_fact_ref(*parent, document, &files, &entities, &facts)?;
        }
        if record.input_sources.is_empty()
            && record.evidence_sources.is_empty()
            && record.derivation_parents.is_empty()
        {
            return Err(IrDocumentValidationError::ProvenanceMissingEvidence(
                record.id,
            ));
        }
    }

    for mapping in &document.source_mappings {
        validate_owner(
            "source_mapping",
            mapping.repository,
            mapping.generation,
            document.repository,
            document.generation,
        )?;
        validate_source(&mapping.from, document, &files)?;
        validate_source(&mapping.to, document, &files)?;
        validate_provenance_ref(mapping.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(mapping.id),
            &mapping.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }
    validate_extraction_dependencies(document, &files)?;

    for coverage in &document.coverage_records {
        validate_owner(
            "coverage",
            coverage.repository,
            coverage.generation,
            document.repository,
            document.generation,
        )?;
        validate_coverage_scope(coverage.scope, document, &files, &entities)?;
        let accounted = coverage.indexed.checked_add(coverage.skipped);
        if accounted.is_none_or(|accounted| accounted > coverage.discovered) {
            return Err(IrDocumentValidationError::InvalidCoverageCounts(
                coverage.id,
            ));
        }
        validate_provenance_ref(coverage.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(coverage.id),
            &coverage.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }

    for skipped in &document.skipped_regions {
        validate_owner(
            "skipped_region",
            skipped.repository,
            skipped.generation,
            document.repository,
            document.generation,
        )?;
        validate_source(&skipped.source, document, &files)?;
        validate_provenance_ref(skipped.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(skipped.id),
            &skipped.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }

    for diagnostic in &document.diagnostics {
        validate_owner(
            "diagnostic",
            diagnostic.repository,
            diagnostic.generation,
            document.repository,
            document.generation,
        )?;
        if let Some(source) = &diagnostic.source {
            validate_source(source, document, &files)?;
        }
        validate_provenance_ref(diagnostic.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(diagnostic.id),
            &diagnostic.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
    }

    for extension in &document.extensions {
        validate_owner(
            "extension",
            extension.repository,
            extension.generation,
            document.repository,
            document.generation,
        )?;
        if !valid_extension_namespace(&extension.namespace)
            || !valid_extension_version(&extension.version)
        {
            return Err(IrDocumentValidationError::InvalidExtensionIdentity {
                namespace: extension.namespace.clone(),
                version: extension.version.clone(),
            });
        }
        if extension.criticality == ExtensionCriticality::Critical
            && !extension_is_supported(extension, extensions)
        {
            return Err(IrDocumentValidationError::UnsupportedCriticalExtension {
                namespace: extension.namespace.clone(),
                version: extension.version.clone(),
            });
        }
        validate_provenance_ref(extension.provenance, &provenance)?;
        validate_fact_evidence(
            FactRef::Fact(extension.id),
            &extension.evidence,
            document,
            &files,
            &entities,
            &facts,
        )?;
        if extension.namespace == crate::VBA_COMPILATION_NAMESPACE {
            let compilation = crate::decode_vba_compilation_envelope(extension)
                .map_err(|_| IrDocumentValidationError::InvalidVbaCompilation(extension.id))?;
            let source = extension.evidence.source.as_ref().ok_or(
                IrDocumentValidationError::InvalidVbaCompilation(extension.id),
            )?;
            let parser = compilation_parsers.get(&extension.provenance).ok_or(
                IrDocumentValidationError::InvalidVbaCompilation(extension.id),
            )?;
            if parser.producer_kind != crate::ProducerKind::Parser
                || !parser.input_sources.iter().any(|input| {
                    input.repository() == source.repository()
                        && input.generation() == source.generation()
                        && input.content_hash() == source.content_hash()
                        && input.span().file() == source.span().file()
                        && input.span().start_byte() <= compilation.scope.start_byte()
                        && compilation.scope.end_byte() <= input.span().end_byte()
                })
                || !compilation_scopes.insert(compilation.scope)
            {
                return Err(IrDocumentValidationError::InvalidVbaCompilation(
                    extension.id,
                ));
            }
        }
    }

    let mut previous_scope = None;
    for scope in compilation_scopes {
        if previous_scope.is_some_and(|previous: crate::SourceSpan| {
            previous.file() == scope.file() && previous.end_byte() > scope.start_byte()
        }) {
            return Err(IrDocumentValidationError::OverlappingVbaCompilationScopes);
        }
        previous_scope = Some(scope);
    }

    Ok(())
}

fn validate_owner(
    record: &'static str,
    actual_repository: RepositoryId,
    actual_generation: GenerationId,
    expected_repository: RepositoryId,
    expected_generation: GenerationId,
) -> Result<(), IrDocumentValidationError> {
    if actual_repository != expected_repository {
        return Err(IrDocumentValidationError::RepositoryMismatch {
            record,
            expected: expected_repository,
            actual: actual_repository,
        });
    }
    if actual_generation != expected_generation {
        return Err(IrDocumentValidationError::GenerationMismatch {
            record,
            expected: expected_generation,
            actual: actual_generation,
        });
    }
    Ok(())
}

fn validate_source(
    source: &SourceRef,
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
) -> Result<(), IrDocumentValidationError> {
    validate_owner(
        "source_ref",
        source.repository(),
        source.generation(),
        document.repository,
        document.generation,
    )?;
    let file_id = source.span().file();
    let file = files
        .get(&file_id)
        .ok_or(IrDocumentValidationError::MissingFile(file_id))?;
    if source.content_hash() != file.content_hash {
        return Err(IrDocumentValidationError::SourceHashMismatch(file_id));
    }
    if source.span().end_byte() > file.byte_length {
        return Err(IrDocumentValidationError::SourceSpanOutOfBounds {
            file: file_id,
            end_byte: source.span().end_byte(),
            byte_length: file.byte_length,
        });
    }
    Ok(())
}

fn validate_container(
    container: ContainerRef,
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
    entities: &BTreeMap<SymbolId, &crate::EntityRecord>,
) -> Result<(), IrDocumentValidationError> {
    match container {
        ContainerRef::Repository(repository) if repository == document.repository => Ok(()),
        ContainerRef::Repository(repository) => {
            Err(IrDocumentValidationError::RepositoryMismatch {
                record: "entity.container",
                expected: document.repository,
                actual: repository,
            })
        }
        ContainerRef::File(file) if files.contains_key(&file) => Ok(()),
        ContainerRef::File(file) => Err(IrDocumentValidationError::MissingFile(file)),
        ContainerRef::Entity(entity) if entities.contains_key(&entity) => Ok(()),
        ContainerRef::Entity(entity) => Err(IrDocumentValidationError::MissingEntity(entity)),
    }
}

fn validate_endpoint(
    endpoint: RelationEndpoint,
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
    entities: &BTreeMap<SymbolId, &crate::EntityRecord>,
    occurrences: &BTreeSet<FactId>,
) -> Result<(), IrDocumentValidationError> {
    match endpoint {
        RelationEndpoint::Repository(repository) if repository == document.repository => Ok(()),
        RelationEndpoint::Repository(repository) => {
            Err(IrDocumentValidationError::RepositoryMismatch {
                record: "relation.endpoint",
                expected: document.repository,
                actual: repository,
            })
        }
        RelationEndpoint::File(file) if files.contains_key(&file) => Ok(()),
        RelationEndpoint::File(file) => Err(IrDocumentValidationError::MissingFile(file)),
        RelationEndpoint::Entity(entity) if entities.contains_key(&entity) => Ok(()),
        RelationEndpoint::Entity(entity) => Err(IrDocumentValidationError::MissingEntity(entity)),
        RelationEndpoint::Occurrence(occurrence) if occurrences.contains(&occurrence) => Ok(()),
        RelationEndpoint::Occurrence(occurrence) => {
            Err(IrDocumentValidationError::MissingOccurrence(occurrence))
        }
    }
}

fn validate_coverage_scope(
    scope: CoverageScope,
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
    entities: &BTreeMap<SymbolId, &crate::EntityRecord>,
) -> Result<(), IrDocumentValidationError> {
    match scope {
        CoverageScope::Repository(repository) if repository == document.repository => Ok(()),
        CoverageScope::Repository(repository) => {
            Err(IrDocumentValidationError::RepositoryMismatch {
                record: "coverage.scope",
                expected: document.repository,
                actual: repository,
            })
        }
        CoverageScope::File(file) if files.contains_key(&file) => Ok(()),
        CoverageScope::File(file) => Err(IrDocumentValidationError::MissingFile(file)),
        CoverageScope::Entity(entity) if entities.contains_key(&entity) => Ok(()),
        CoverageScope::Entity(entity) => Err(IrDocumentValidationError::MissingEntity(entity)),
    }
}

fn validate_occurrence_target(
    occurrence: &crate::OccurrenceRecord,
    entities: &BTreeMap<SymbolId, &crate::EntityRecord>,
) -> Result<(), IrDocumentValidationError> {
    match &occurrence.target {
        OccurrenceTarget::Resolved { symbol } => {
            if entities.contains_key(symbol) {
                Ok(())
            } else {
                Err(IrDocumentValidationError::MissingEntity(*symbol))
            }
        }
        OccurrenceTarget::Candidates {
            symbols,
            total_count,
            ..
        } => {
            let materialized = u64::try_from(symbols.len()).map_err(|_| {
                IrDocumentValidationError::InvalidCandidateCount {
                    occurrence: occurrence.id,
                    materialized: u64::MAX,
                    total: *total_count,
                }
            })?;
            if materialized > *total_count {
                return Err(IrDocumentValidationError::InvalidCandidateCount {
                    occurrence: occurrence.id,
                    materialized,
                    total: *total_count,
                });
            }
            for symbol in symbols {
                if !entities.contains_key(symbol) {
                    return Err(IrDocumentValidationError::MissingEntity(*symbol));
                }
            }
            Ok(())
        }
        OccurrenceTarget::Unresolved { .. } => Ok(()),
    }
}

fn validate_provenance_ref(
    reference: FactId,
    provenance: &BTreeSet<FactId>,
) -> Result<(), IrDocumentValidationError> {
    if provenance.contains(&reference) {
        Ok(())
    } else {
        Err(IrDocumentValidationError::MissingProvenance(reference))
    }
}

fn validate_fact_evidence(
    record: FactRef,
    evidence: &FactEvidence,
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
    entities: &BTreeMap<SymbolId, &crate::EntityRecord>,
    facts: &BTreeSet<FactId>,
) -> Result<(), IrDocumentValidationError> {
    if evidence.source.is_none() && evidence.derivation.is_empty() {
        return Err(IrDocumentValidationError::MissingFactEvidence(record));
    }
    if let Some(source) = &evidence.source {
        validate_source(source, document, files)?;
    }
    for reference in &evidence.derivation {
        if *reference == record {
            return Err(IrDocumentValidationError::SelfDerivation(record));
        }
        validate_fact_ref(*reference, document, files, entities, facts)?;
    }
    Ok(())
}

fn validate_fact_ref(
    reference: FactRef,
    _document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
    entities: &BTreeMap<SymbolId, &crate::EntityRecord>,
    facts: &BTreeSet<FactId>,
) -> Result<(), IrDocumentValidationError> {
    let valid = match reference {
        FactRef::File(file) => files.contains_key(&file),
        FactRef::Entity(entity) => entities.contains_key(&entity),
        FactRef::Fact(fact) => facts.contains(&fact),
    };
    if valid {
        Ok(())
    } else {
        Err(IrDocumentValidationError::MissingDerivation(reference))
    }
}

fn validate_extraction_dependencies(
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, &crate::FileRecord>,
) -> Result<(), IrDocumentValidationError> {
    let mut parents = BTreeMap::new();
    for mapping in &document.source_mappings {
        if mapping.kind != crate::SourceMappingKind::ExtractedFromContainer {
            continue;
        }
        if document.version < crate::NormalizedIrVersion::V1_6 {
            return Err(IrDocumentValidationError::ExtractionVersion(mapping.id));
        }
        for source in [&mapping.from, &mapping.to] {
            let file = files
                .get(&source.span().file())
                .ok_or(IrDocumentValidationError::MissingFile(source.span().file()))?;
            if source.span().start_byte() != 0 || source.span().end_byte() != file.byte_length {
                return Err(IrDocumentValidationError::PartialExtractionDependency(
                    mapping.id,
                ));
            }
        }
        let file = mapping.from.span().file();
        if parents.insert(file, mapping.to.span().file()).is_some() {
            return Err(IrDocumentValidationError::DuplicateExtractionOrigin(file));
        }
    }
    // Iterative coloring visits each retained dependency once, including shared
    // nested containers. A source-controlled nesting depth never uses the stack.
    let mut states = BTreeMap::<FileId, u8>::new();
    for start in parents.keys().copied() {
        let mut current = start;
        while let Some(parent) = parents.get(&current) {
            match states.get(&current) {
                Some(1) => return Err(IrDocumentValidationError::ExtractionCycle(current)),
                Some(2) => break,
                Some(_) | None => {
                    states.insert(current, 1);
                    current = *parent;
                }
            }
        }
        current = start;
        while states.get(&current) == Some(&1) {
            states.insert(current, 2);
            let Some(parent) = parents.get(&current) else {
                break;
            };
            current = *parent;
        }
    }
    Ok(())
}

fn validate_container_cycles(
    document: &NormalizedIrDocument,
) -> Result<(), IrDocumentValidationError> {
    let parents: BTreeMap<_, _> = document
        .entities
        .iter()
        .map(|entity| {
            let parent = match entity.container {
                Some(ContainerRef::Entity(parent)) => Some(parent),
                Some(ContainerRef::Repository(_) | ContainerRef::File(_)) | None => None,
            };
            (entity.id, parent)
        })
        .collect();
    let mut states = BTreeMap::<SymbolId, u8>::new();
    for start in parents.keys().copied() {
        if states.get(&start) == Some(&2) {
            continue;
        }
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(entity) = current {
            match states.get(&entity) {
                Some(1) => return Err(IrDocumentValidationError::ContainerCycle(entity)),
                Some(2) => break,
                Some(_) | None => {
                    states.insert(entity, 1);
                    path.push(entity);
                    current = parents.get(&entity).copied().flatten();
                }
            }
        }
        for entity in path {
            states.insert(entity, 2);
        }
    }
    Ok(())
}

fn valid_extension_namespace(value: &str) -> bool {
    let mut segments = value.split('.');
    let Some(first) = segments.next() else {
        return false;
    };
    let Some(second) = segments.next() else {
        return false;
    };
    valid_namespace_segment(first)
        && valid_namespace_segment(second)
        && segments.all(valid_namespace_segment)
}

fn valid_namespace_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_extension_version(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

/// Validation failures for supported normalized IR documents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IrDocumentValidationError {
    /// The document uses an unsupported normalized version.
    #[error("unsupported normalized IR version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major component.
        major: u16,
        /// Unsupported minor component.
        minor: u16,
    },
    /// An entity kind was supplied under an older document contract.
    #[error("entity kind {kind:?} requires a newer normalized IR version")]
    EntityKindVersion {
        /// Kind requiring the newer document version.
        kind: EntityKind,
    },
    /// A caller requiring canonical input observed unsorted or duplicate records.
    #[error("normalized IR document is not canonical")]
    NonCanonicalDocument,
    /// One top-level or nested collection exceeds its limit.
    #[error("{collection} contains {observed} items, limit is {limit}")]
    CollectionLimit {
        /// Bounded collection name.
        collection: &'static str,
        /// Observed item count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// All top-level records together exceed their limit.
    #[error("document contains {observed} records, total limit is {limit}")]
    TotalRecordLimit {
        /// Observed record count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// All nested items together exceed their limit.
    #[error("document contains {observed} nested items, total limit is {limit}")]
    TotalNestedItemLimit {
        /// Observed nested-item count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// One string exceeds its byte limit.
    #[error("{field} contains {observed} UTF-8 bytes, limit is {limit}")]
    StringLimit {
        /// Bounded string field.
        field: &'static str,
        /// Observed UTF-8 byte count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// All non-payload strings together exceed their byte limit.
    #[error("document strings contain {observed} UTF-8 bytes, total limit is {limit}")]
    TotalStringLimit {
        /// Observed UTF-8 byte count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// One extension payload exceeds its byte limit.
    #[error("extension {id} payload contains {observed} UTF-8 bytes, limit is {limit}")]
    ExtensionPayloadLimit {
        /// Extension fact identity.
        id: FactId,
        /// Observed UTF-8 byte count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// All extension payloads together exceed their byte limit.
    #[error("extension payloads contain {observed} UTF-8 bytes, total limit is {limit}")]
    TotalExtensionBytesLimit {
        /// Observed UTF-8 byte count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// One diagnostic message exceeds its byte limit.
    #[error("diagnostic {id} message contains {observed} UTF-8 bytes, limit is {limit}")]
    DiagnosticMessageLimit {
        /// Diagnostic fact identity.
        id: FactId,
        /// Observed UTF-8 byte count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// All diagnostic codes and messages together exceed their byte limit.
    #[error("diagnostics contain {observed} UTF-8 bytes, total limit is {limit}")]
    TotalDiagnosticBytesLimit {
        /// Observed UTF-8 byte count.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Unequal file records share one compact ID.
    #[error("unequal file records share ID {0}")]
    DuplicateUnequalFileId(FileId),
    /// Unequal entity records share one compact ID.
    #[error("unequal entity records share ID {0}")]
    DuplicateUnequalSymbolId(SymbolId),
    /// Unequal fact records or fact domains share one compact ID.
    #[error("unequal fact records share ID {0}")]
    DuplicateUnequalFactId(FactId),
    /// A record belongs to another repository.
    #[error("{record} repository mismatch: expected {expected}, got {actual}")]
    RepositoryMismatch {
        /// Record or reference class.
        record: &'static str,
        /// Document repository.
        expected: RepositoryId,
        /// Record repository.
        actual: RepositoryId,
    },
    /// A record belongs to another generation.
    #[error("{record} generation mismatch: expected {expected}, got {actual}")]
    GenerationMismatch {
        /// Record or reference class.
        record: &'static str,
        /// Document generation.
        expected: GenerationId,
        /// Record generation.
        actual: GenerationId,
    },
    /// A file reference has no owned file record.
    #[error("missing referenced file {0}")]
    MissingFile(FileId),
    /// An entity reference has no owned entity record.
    #[error("missing referenced entity {0}")]
    MissingEntity(SymbolId),
    /// An occurrence endpoint has no owned occurrence record.
    #[error("missing referenced occurrence {0}")]
    MissingOccurrence(FactId),
    /// A fact references a missing provenance record.
    #[error("missing referenced provenance {0}")]
    MissingProvenance(FactId),
    /// A derivation references a missing base fact.
    #[error("missing derivation reference {0:?}")]
    MissingDerivation(FactRef),
    /// A fact incorrectly names itself as derivation evidence.
    #[error("fact {0:?} cannot derive from itself")]
    SelfDerivation(FactRef),
    /// A source reference hash differs from the owned file.
    #[error("source hash does not match owned file {0}")]
    SourceHashMismatch(FileId),
    /// A source reference extends beyond the owned file.
    #[error("source span for {file} ends at {end_byte}, file length is {byte_length}")]
    SourceSpanOutOfBounds {
        /// Referenced file.
        file: FileId,
        /// Exclusive source end byte.
        end_byte: u64,
        /// Authoritative file length.
        byte_length: u64,
    },
    /// An occurrence's declared file differs from its source span file.
    #[error("occurrence {occurrence} declares file {declared}, but its source uses {source_file}")]
    OccurrenceSourceFileMismatch {
        /// Occurrence fact identity.
        occurrence: FactId,
        /// Declared occurrence file.
        declared: FileId,
        /// Source-span file.
        source_file: FileId,
    },
    /// Semantic entity containers form a cycle.
    #[error("entity container cycle includes {0}")]
    ContainerCycle(SymbolId),
    /// Container extraction is not admitted by the document's selected version.
    #[error("extraction mapping {0} requires normalized IR version 1.6")]
    ExtractionVersion(FactId),
    /// Extraction dependencies must bind complete source and container snapshots.
    #[error("extraction mapping {0} does not bind two complete files")]
    PartialExtractionDependency(FactId),
    /// Distinct extraction records claim ownership of the same derived file.
    #[error("extracted file {0} has multiple container origins")]
    DuplicateExtractionOrigin(FileId),
    /// Container extraction dependencies form a cycle, including self-extraction.
    #[error("container extraction cycle includes {0}")]
    ExtractionCycle(FileId),
    /// A fact has neither direct source nor derivation evidence.
    #[error("fact {0:?} has neither source nor derivation evidence")]
    MissingFactEvidence(FactRef),
    /// A provenance record has neither source nor derivation evidence.
    #[error("provenance {0} has neither source nor derivation evidence")]
    ProvenanceMissingEvidence(FactId),
    /// A candidate set materializes more targets than its declared total.
    #[error(
        "occurrence {occurrence} materializes {materialized} candidates, declared total is {total}"
    )]
    InvalidCandidateCount {
        /// Occurrence fact identity.
        occurrence: FactId,
        /// Materialized unique candidate count.
        materialized: u64,
        /// Declared total candidate count.
        total: u64,
    },
    /// Indexed and skipped coverage counts exceed discovered units.
    #[error("coverage record {0} has inconsistent counts")]
    InvalidCoverageCounts(FactId),
    /// An extension namespace or version is not a safe bounded identity.
    #[error("invalid extension identity {namespace}@{version}")]
    InvalidExtensionIdentity {
        /// Invalid namespace.
        namespace: String,
        /// Invalid namespace-specific version.
        version: String,
    },
    /// A known VBA compilation envelope is malformed or lacks its parser input.
    #[error("invalid vba compilation evidence {0}")]
    InvalidVbaCompilation(FactId),
    /// Two source-written VBA compilation scopes overlap in one source file.
    #[error("vba compilation scopes overlap")]
    OverlappingVbaCompilationScopes,
    /// A critical extension is not declared supported.
    #[error("unsupported critical extension {namespace}@{version}")]
    UnsupportedCriticalExtension {
        /// Unsupported namespace.
        namespace: String,
        /// Unsupported namespace-specific version.
        version: String,
    },
}

#[cfg(test)]
mod tests {
    mod extraction;

    use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};

    use super::*;
    use crate::{
        ContainerRef, CoverageStatus, ExtensionIdentifier, IrDocument, IrLimits,
        UnknownNoncriticalExtensionPolicy, decode_ir_document,
    };

    fn fixture() -> NormalizedIrDocument {
        let decoded = decode_ir_document(
            include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json"),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("frozen normalized IR fixture decodes");
        let IrDocument::NormalizedV1_1(document) = decoded else {
            panic!("normalized fixture must dispatch to version 1.1");
        };
        document
    }

    fn canonical(document: NormalizedIrDocument) -> NormalizedIrDocument {
        canonicalize_ir_document(document, &IrLimits::default(), &ExtensionSupport::default())
            .expect("fixture canonicalizes")
    }

    fn source_for(
        document: &NormalizedIrDocument,
        file: FileId,
        hash: ContentHash,
        end_byte: u64,
    ) -> SourceRef {
        SourceRef::new(
            document.repository,
            document.generation,
            crate::SourceSpan::new(file, 0, end_byte).expect("test span is ordered"),
            hash,
            None,
        )
    }

    #[test]
    fn default_gap_credit_shares_total_records_and_preserves_explicit_limits() {
        let mut document = fixture();
        document
            .skipped_regions
            .resize(100_001, document.skipped_regions[0].clone());
        let limits = IrLimits::default();
        let usage = IrDocumentUsage::measure(&document, &limits).unwrap();
        assert_eq!(usage.skipped_regions(), 100_001);
        assert!(usage.total_records() < limits.max_total_records);
        let mut limited = limits.clone();
        limited.max_skipped_regions = 100_000;
        assert!(matches!(
            IrDocumentUsage::measure(&document, &limited),
            Err(IrDocumentUsageError::Validation(
                IrDocumentValidationError::CollectionLimit {
                    collection: "skipped_regions",
                    observed: 100_001,
                    limit: 100_000,
                }
            ))
        ));
        limited.max_skipped_regions = limits.max_skipped_regions;
        limited.max_total_string_bytes = usage.total_string_bytes() - 1;
        assert!(IrDocumentUsage::measure(&document, &limited).is_err());
    }

    #[test]
    fn usage_measures_all_raw_domains_strings_and_locator_components() {
        let mut document = fixture();
        let limits = IrLimits::default();
        let usage = IrDocumentUsage::measure(&document, &limits).expect("fixture usage is bounded");
        assert_eq!(
            [
                usage.files(),
                usage.entities(),
                usage.occurrences(),
                usage.relations(),
                usage.provenance_records(),
                usage.source_mappings(),
                usage.coverage_records(),
                usage.skipped_regions(),
                usage.diagnostics(),
                usage.extensions()
            ],
            [1; 10]
        );
        assert_eq!(usage.total_records(), 10);
        assert_eq!(usage.total_nested_items(), 3);
        assert_eq!(usage.total_string_bytes(), 187);
        assert_eq!(usage.total_extension_bytes(), 2);
        assert_eq!(usage.total_diagnostic_bytes(), 45);

        document.files[0].path_locator = Some(
            crate::FilePathLocator::new(
                crate::FilePathLocatorEncoding::UnixBytesV1,
                vec!["737263".to_owned(), "6c69622e7273".to_owned()],
            )
            .expect("canonical locator components"),
        );
        let locator_usage =
            IrDocumentUsage::measure(&document, &limits).expect("locator is bounded");
        assert_eq!(locator_usage.total_nested_items(), 5);
        assert_eq!(locator_usage.total_string_bytes(), 205);

        let reference = FactRef::File(document.files[0].id);
        document.files[0].evidence.derivation.push(reference);
        document.entities[0].evidence.derivation.push(reference);
        document.occurrences[0].evidence.derivation.push(reference);
        document.relations[0].evidence.derivation.push(reference);
        document.source_mappings[0]
            .evidence
            .derivation
            .push(reference);
        document.coverage_records[0]
            .evidence
            .derivation
            .push(reference);
        document.skipped_regions[0]
            .evidence
            .derivation
            .push(reference);
        document.diagnostics[0].evidence.derivation.push(reference);
        document.extensions[0].evidence.derivation.push(reference);
        document.occurrences[0].target = OccurrenceTarget::Candidates {
            symbols: vec![document.entities[0].id; 2],
            total_count: 2,
            completeness: CoverageStatus::Complete,
        };
        document.entities[0].flags.push(crate::EntityFlag::Exported);
        let input = document.provenance[0].input_sources[0].clone();
        let evidence = document.provenance[0].evidence_sources[0].clone();
        document.provenance[0].input_sources.push(input);
        document.provenance[0].evidence_sources.push(evidence);
        document.provenance[0]
            .derivation_parents
            .extend([reference; 2]);
        document.provenance[0].rule = Some("rule".to_owned());
        document.entities[0].display_name = "\u{03b4}".to_owned();
        document.diagnostics[0].message = "\u{03b4}".to_owned();
        let expanded =
            IrDocumentUsage::measure(&document, &limits).expect("raw nested usage is bounded");
        assert_eq!(expanded.total_nested_items(), 21);
        assert_eq!(expanded.total_string_bytes(), 176);
        assert_eq!(expanded.total_diagnostic_bytes(), 17);
        assert_eq!(expanded.total_extension_bytes(), 2);
        assert!(validate_ir_document(&document, &limits, &ExtensionSupport::default()).is_err());
    }

    #[test]
    fn usage_counts_equal_duplicates_before_canonicalization() {
        let document = fixture();
        let limits = IrLimits::default();
        let usage = IrDocumentUsage::measure(&document, &limits).expect("fixture usage is bounded");
        let mut duplicate = document.clone();
        duplicate.files.extend(document.files.clone());
        duplicate.entities.extend(document.entities.clone());
        duplicate.occurrences.extend(document.occurrences.clone());
        duplicate.relations.extend(document.relations.clone());
        duplicate.provenance.extend(document.provenance.clone());
        duplicate
            .source_mappings
            .extend(document.source_mappings.clone());
        duplicate
            .coverage_records
            .extend(document.coverage_records.clone());
        duplicate
            .skipped_regions
            .extend(document.skipped_regions.clone());
        duplicate.diagnostics.extend(document.diagnostics.clone());
        duplicate.extensions.extend(document.extensions.clone());
        let expected = usage
            .clone()
            .checked_add(usage, &limits)
            .expect("doubled raw usage is bounded");
        assert_eq!(IrDocumentUsage::measure(&duplicate, &limits), Ok(expected));
        assert_eq!(canonical(duplicate.clone()), canonical(document));
        let tight = IrLimits {
            max_files: 1,
            ..limits
        };
        let expected_error = IrDocumentValidationError::CollectionLimit {
            collection: "files",
            observed: 2,
            limit: 1,
        };
        assert_eq!(
            preflight_ir_document_limits(&duplicate, &tight),
            Err(expected_error.clone())
        );
        assert_eq!(
            IrDocumentUsage::measure(&duplicate, &tight),
            Err(IrDocumentUsageError::Validation(expected_error))
        );
    }

    fn exact_pair_limits(usage: &IrDocumentUsage) -> IrLimits {
        IrLimits {
            max_files: usage.files * 2,
            max_entities: usage.entities * 2,
            max_occurrences: usage.occurrences * 2,
            max_relations: usage.relations * 2,
            max_provenance_records: usage.provenance_records * 2,
            max_source_mappings: usage.source_mappings * 2,
            max_coverage_records: usage.coverage_records * 2,
            max_skipped_regions: usage.skipped_regions * 2,
            max_diagnostics: usage.diagnostics * 2,
            max_extensions: usage.extensions * 2,
            max_total_records: usage.total_records * 2,
            max_total_nested_items: usage.total_nested_items * 2,
            max_total_string_bytes: usage.total_string_bytes * 2,
            max_total_extension_bytes: usage.total_extension_bytes * 2,
            max_total_diagnostic_bytes: usage.total_diagnostic_bytes * 2,
            ..IrLimits::default()
        }
    }

    #[test]
    fn usage_merges_accept_exact_and_reject_one_above_each_aggregate_quota() {
        let usage =
            IrDocumentUsage::measure(&fixture(), &IrLimits::default()).expect("fixture is bounded");
        let exact = exact_pair_limits(&usage);
        assert!(usage.clone().checked_add(usage.clone(), &exact).is_ok());
        let tighten: [fn(&mut IrLimits); 15] = [
            |limits| limits.max_files -= 1,
            |limits| limits.max_entities -= 1,
            |limits| limits.max_occurrences -= 1,
            |limits| limits.max_relations -= 1,
            |limits| limits.max_provenance_records -= 1,
            |limits| limits.max_source_mappings -= 1,
            |limits| limits.max_coverage_records -= 1,
            |limits| limits.max_skipped_regions -= 1,
            |limits| limits.max_diagnostics -= 1,
            |limits| limits.max_extensions -= 1,
            |limits| limits.max_total_records -= 1,
            |limits| limits.max_total_nested_items -= 1,
            |limits| limits.max_total_string_bytes -= 1,
            |limits| limits.max_total_extension_bytes -= 1,
            |limits| limits.max_total_diagnostic_bytes -= 1,
        ];
        for (dimension, tighten) in tighten.into_iter().enumerate() {
            let mut limits = exact.clone();
            tighten(&mut limits);
            assert!(
                matches!(
                    usage.clone().checked_add(usage.clone(), &limits),
                    Err(IrDocumentUsageError::Validation(_))
                ),
                "dimension {dimension} must reject its exact +1 demand"
            );
        }
    }

    #[test]
    fn usage_merges_recheck_per_record_maxima_under_stricter_limits() {
        let document = fixture();
        let usage =
            IrDocumentUsage::measure(&document, &IrLimits::default()).expect("fixture is bounded");
        let cases = [
            (
                IrLimits {
                    max_nested_items_per_record: 0,
                    ..IrLimits::default()
                },
                IrDocumentValidationError::CollectionLimit {
                    collection: "entity.flags",
                    observed: 1,
                    limit: 0,
                },
            ),
            (
                IrLimits {
                    max_string_bytes: 29,
                    ..IrLimits::default()
                },
                IrDocumentValidationError::StringLimit {
                    field: "diagnostic.message",
                    observed: 30,
                    limit: 29,
                },
            ),
            (
                IrLimits {
                    max_extension_payload_bytes: 1,
                    ..IrLimits::default()
                },
                IrDocumentValidationError::ExtensionPayloadLimit {
                    id: document.extensions[0].id,
                    observed: 2,
                    limit: 1,
                },
            ),
            (
                IrLimits {
                    max_diagnostic_message_bytes: 29,
                    ..IrLimits::default()
                },
                IrDocumentValidationError::DiagnosticMessageLimit {
                    id: document.diagnostics[0].id,
                    observed: 30,
                    limit: 29,
                },
            ),
        ];
        for (limits, expected) in cases {
            assert_eq!(
                usage
                    .clone()
                    .checked_add(IrDocumentUsage::default(), &limits),
                Err(IrDocumentUsageError::Validation(expected))
            );
        }
    }

    #[test]
    fn usage_record_measurement_and_subtraction_share_document_accounting() {
        let document = fixture();
        let limits = IrLimits::default();
        let whole = IrDocumentUsage::measure(&document, &limits).expect("fixture is bounded");
        let extension = IrDocumentUsage::measure_extension(&document.extensions[0], &limits)
            .expect("extension is bounded");
        let diagnostic = IrDocumentUsage::measure_diagnostic(&document.diagnostics[0], &limits)
            .expect("diagnostic is bounded");
        assert_eq!(extension.extensions(), 1);
        assert_eq!(extension.total_records(), 1);
        assert_eq!(extension.total_string_bytes(), 24);
        assert_eq!(extension.total_extension_bytes(), 2);
        assert_eq!(diagnostic.diagnostics(), 1);
        assert_eq!(diagnostic.total_records(), 1);
        assert_eq!(diagnostic.total_string_bytes(), 45);
        assert_eq!(diagnostic.total_diagnostic_bytes(), 45);
        let removed = extension
            .checked_add(diagnostic, &limits)
            .expect("removed records combine");
        let remainder = whole
            .clone()
            .checked_sub(removed.clone())
            .expect("owned records subtract");
        assert_eq!(remainder.total_records(), 8);
        assert_eq!(remainder.total_string_bytes(), 118);
        assert_eq!(remainder.total_extension_bytes(), 0);
        assert_eq!(remainder.total_diagnostic_bytes(), 0);
        assert_eq!(remainder.clone().checked_add(removed, &limits), Ok(whole));
        assert!(matches!(
            remainder.checked_add(
                IrDocumentUsage::default(),
                &IrLimits {
                    max_diagnostic_message_bytes: 29,
                    ..limits
                }
            ),
            Err(IrDocumentUsageError::Validation(
                IrDocumentValidationError::DiagnosticMessageLimit { observed: 30, .. }
            ))
        ));
    }

    #[test]
    fn usage_checked_arithmetic_rejects_every_counter_overflow_and_underflow() {
        let fields: [fn(&mut IrDocumentUsage, usize); 15] = [
            |usage, value| usage.files = value,
            |usage, value| usage.entities = value,
            |usage, value| usage.occurrences = value,
            |usage, value| usage.relations = value,
            |usage, value| usage.provenance_records = value,
            |usage, value| usage.source_mappings = value,
            |usage, value| usage.coverage_records = value,
            |usage, value| usage.skipped_regions = value,
            |usage, value| usage.diagnostics = value,
            |usage, value| usage.extensions = value,
            |usage, value| usage.total_records = value,
            |usage, value| usage.total_nested_items = value,
            |usage, value| usage.total_string_bytes = value,
            |usage, value| usage.total_extension_bytes = value,
            |usage, value| usage.total_diagnostic_bytes = value,
        ];
        for field in fields {
            let mut maximum = IrDocumentUsage::default();
            let mut one = IrDocumentUsage::default();
            field(&mut maximum, usize::MAX);
            field(&mut one, 1);
            let preserved = maximum.clone();
            assert_eq!(
                maximum
                    .clone()
                    .checked_add(one.clone(), &IrLimits::default()),
                Err(IrDocumentUsageError::AccountingOverflow)
            );
            assert_eq!(maximum, preserved);
            assert_eq!(
                IrDocumentUsage::default().checked_sub(one),
                Err(IrDocumentUsageError::AccountingOverflow)
            );
        }
    }

    #[test]
    fn usage_checkpoints_interrupt_at_every_boundary_without_partial_commit() {
        let mut document = fixture();
        document.files[0].path_locator = Some(
            crate::FilePathLocator::new(
                crate::FilePathLocatorEncoding::UnixBytesV1,
                vec!["737263".to_owned(), "6c69622e7273".to_owned()],
            )
            .expect("canonical locator components"),
        );
        let original = document.clone();
        let limits = IrLimits::default();
        let mut count = 0;
        let complete = IrDocumentUsage::measure_with_checkpoint(&document, &limits, || {
            count += 1;
            true
        })
        .expect("complete traversal succeeds");
        assert!(count > complete.total_records());
        for allowed in 0..count {
            let mut observed = 0;
            let mut committed = IrDocumentUsage::default();
            let result = IrDocumentUsage::measure_with_checkpoint(&document, &limits, || {
                observed += 1;
                observed <= allowed
            });
            if let Ok(summary) = &result {
                committed = summary.clone();
            }
            assert_eq!(result, Err(IrDocumentUsageError::Interrupted));
            assert_eq!(observed, allowed + 1);
            assert_eq!(committed, IrDocumentUsage::default());
            assert_eq!(document, original);
        }
        assert_eq!(IrDocumentUsage::measure(&document, &limits), Ok(complete));
    }

    #[test]
    fn valid_fixture_canonicalizes_and_is_idempotent() {
        let first = canonical(fixture());
        let second = canonical(first.clone());

        assert_eq!(first, second);
        assert_eq!(first.extensions.len(), 1);
    }

    #[test]
    fn canonicalization_is_worker_order_independent_for_shuffled_records() {
        let expected = canonical(fixture());
        for seed in 0..32 {
            let mut shuffled = fixture();
            duplicate_and_shuffle(&mut shuffled.files, seed);
            duplicate_and_shuffle(&mut shuffled.entities, seed + 1);
            duplicate_and_shuffle(&mut shuffled.occurrences, seed + 2);
            duplicate_and_shuffle(&mut shuffled.relations, seed + 3);
            duplicate_and_shuffle(&mut shuffled.provenance, seed + 4);
            duplicate_and_shuffle(&mut shuffled.source_mappings, seed + 5);
            duplicate_and_shuffle(&mut shuffled.coverage_records, seed + 6);
            duplicate_and_shuffle(&mut shuffled.skipped_regions, seed + 7);
            duplicate_and_shuffle(&mut shuffled.diagnostics, seed + 8);
            duplicate_and_shuffle(&mut shuffled.extensions, seed + 9);
            shuffled.entities[0].flags.extend([
                crate::EntityFlag::Exported,
                crate::EntityFlag::Generated,
                crate::EntityFlag::Generated,
            ]);
            shuffled.entities[1].flags = shuffled.entities[0].flags.clone();

            let mut expected_with_flag = expected.clone();
            expected_with_flag.entities[0]
                .flags
                .push(crate::EntityFlag::Generated);
            expected_with_flag.entities[0].flags.sort_unstable();
            assert_eq!(canonical(shuffled), expected_with_flag);
        }
    }

    fn duplicate_and_shuffle<T: Clone>(records: &mut Vec<T>, seed: usize) {
        records.extend(records.clone());
        let length = records.len();
        if length > 0 {
            records.rotate_left(seed % length);
        }
        if seed % 2 == 1 {
            records.reverse();
        }
    }

    #[test]
    fn rejects_owner_endpoint_container_target_and_derivation_mismatches() {
        let mut repository_mismatch = fixture();
        repository_mismatch.files[0].repository = RepositoryId::from_bytes([31; 16]);
        assert!(matches!(
            canonicalize_ir_document(
                repository_mismatch,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::RepositoryMismatch { record: "file", .. })
        ));

        let mut generation_mismatch = fixture();
        generation_mismatch.files[0].generation = GenerationId::from_bytes([32; 20]);
        assert!(matches!(
            canonicalize_ir_document(
                generation_mismatch,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::GenerationMismatch { record: "file", .. })
        ));

        let missing_symbol = SymbolId::from_bytes([33; 20]);
        let mut endpoint = fixture();
        endpoint.relations[0].object = RelationEndpoint::Entity(missing_symbol);
        assert_eq!(
            canonicalize_ir_document(endpoint, &IrLimits::default(), &ExtensionSupport::default()),
            Err(IrDocumentValidationError::MissingEntity(missing_symbol))
        );

        let missing_occurrence = FactId::from_bytes([39; 20]);
        let mut occurrence_endpoint = fixture();
        occurrence_endpoint.relations[0].subject = RelationEndpoint::Occurrence(missing_occurrence);
        assert_eq!(
            canonicalize_ir_document(
                occurrence_endpoint,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::MissingOccurrence(
                missing_occurrence
            ))
        );

        let mut container = fixture();
        container.entities[0].container = Some(ContainerRef::Entity(missing_symbol));
        assert_eq!(
            canonicalize_ir_document(
                container,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::MissingEntity(missing_symbol))
        );

        let mut target = fixture();
        target.occurrences[0].target = OccurrenceTarget::Resolved {
            symbol: missing_symbol,
        };
        assert_eq!(
            canonicalize_ir_document(target, &IrLimits::default(), &ExtensionSupport::default()),
            Err(IrDocumentValidationError::MissingEntity(missing_symbol))
        );

        let missing_fact = FactId::from_bytes([34; 20]);
        let mut derivation = fixture();
        derivation.files[0]
            .evidence
            .derivation
            .push(FactRef::Fact(missing_fact));
        assert_eq!(
            canonicalize_ir_document(
                derivation,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::MissingDerivation(FactRef::Fact(
                missing_fact
            )))
        );

        let mut self_derivation = fixture();
        let file = self_derivation.files[0].id;
        self_derivation.files[0].evidence.source = None;
        self_derivation.files[0]
            .evidence
            .derivation
            .push(FactRef::File(file));
        assert!(matches!(
            canonicalize_ir_document(
                self_derivation,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::SelfDerivation(FactRef::File(_)))
        ));
    }

    #[test]
    fn rejects_provenance_and_source_evidence_failures() {
        let missing_provenance = FactId::from_bytes([35; 20]);
        let mut provenance_ref = fixture();
        provenance_ref.files[0].provenance = missing_provenance;
        assert_eq!(
            canonicalize_ir_document(
                provenance_ref,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::MissingProvenance(
                missing_provenance
            ))
        );

        let mut no_fact_evidence = fixture();
        no_fact_evidence.files[0].evidence.source = None;
        no_fact_evidence.files[0].evidence.derivation.clear();
        assert!(matches!(
            canonicalize_ir_document(
                no_fact_evidence,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::MissingFactEvidence(
                FactRef::File(_)
            ))
        ));

        let mut no_provenance_evidence = fixture();
        no_provenance_evidence.provenance[0].input_sources.clear();
        no_provenance_evidence.provenance[0]
            .evidence_sources
            .clear();
        no_provenance_evidence.provenance[0]
            .derivation_parents
            .clear();
        assert!(matches!(
            canonicalize_ir_document(
                no_provenance_evidence,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::ProvenanceMissingEvidence(_))
        ));
    }

    #[test]
    fn rejects_source_file_hash_and_span_mismatches() {
        let mut missing_file = fixture();
        let absent = FileId::from_bytes([36; 20]);
        let hash = missing_file.files[0].content_hash;
        missing_file.entities[0].evidence.source = Some(source_for(&missing_file, absent, hash, 1));
        assert_eq!(
            canonicalize_ir_document(
                missing_file,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::MissingFile(absent))
        );

        let mut wrong_hash = fixture();
        let file = wrong_hash.files[0].id;
        wrong_hash.entities[0].evidence.source = Some(source_for(
            &wrong_hash,
            file,
            ContentHash::from_bytes([37; 32]),
            1,
        ));
        assert_eq!(
            canonicalize_ir_document(
                wrong_hash,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::SourceHashMismatch(file))
        );

        let mut out_of_bounds = fixture();
        let file = out_of_bounds.files[0].id;
        let hash = out_of_bounds.files[0].content_hash;
        out_of_bounds.entities[0].evidence.source =
            Some(source_for(&out_of_bounds, file, hash, 27));
        assert!(matches!(
            canonicalize_ir_document(
                out_of_bounds,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::SourceSpanOutOfBounds {
                file: observed,
                ..
            }) if observed == file
        ));

        let mut owner_mismatch = fixture();
        let file = owner_mismatch.files[0].id;
        let hash = owner_mismatch.files[0].content_hash;
        owner_mismatch.entities[0].evidence.source = Some(SourceRef::new(
            RepositoryId::from_bytes([40; 16]),
            owner_mismatch.generation,
            crate::SourceSpan::new(file, 0, 1).expect("test span is ordered"),
            hash,
            None,
        ));
        assert!(matches!(
            canonicalize_ir_document(
                owner_mismatch,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::RepositoryMismatch {
                record: "source_ref",
                ..
            })
        ));

        let mut occurrence_file = fixture();
        let second_file = FileId::from_bytes([41; 20]);
        let mut owned_second = occurrence_file.files[0].clone();
        owned_second.id = second_file;
        owned_second.evidence.source = Some(source_for(
            &occurrence_file,
            second_file,
            owned_second.content_hash,
            owned_second.byte_length,
        ));
        occurrence_file.files.push(owned_second);
        occurrence_file.occurrences[0].file = second_file;
        assert!(matches!(
            canonicalize_ir_document(
                occurrence_file,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::OccurrenceSourceFileMismatch {
                declared,
                ..
            }) if declared == second_file
        ));
    }

    #[test]
    fn rejects_unequal_id_collisions_and_deduplicates_equal_records() {
        let mut unequal_file = fixture();
        let mut collision = unequal_file.files[0].clone();
        collision.path = "other.rs".to_owned();
        unequal_file.files.push(collision);
        assert!(matches!(
            canonicalize_ir_document(
                unequal_file,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::DuplicateUnequalFileId(_))
        ));

        let mut unequal_symbol = fixture();
        let mut collision = unequal_symbol.entities[0].clone();
        collision.display_name = "other".to_owned();
        unequal_symbol.entities.push(collision);
        assert!(matches!(
            canonicalize_ir_document(
                unequal_symbol,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::DuplicateUnequalSymbolId(_))
        ));

        let mut cross_domain = fixture();
        cross_domain.diagnostics[0].id = cross_domain.relations[0].id;
        assert!(matches!(
            canonicalize_ir_document(
                cross_domain,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::DuplicateUnequalFactId(_))
        ));
    }

    #[test]
    fn rejects_entity_container_cycles() {
        let mut document = fixture();
        let first = document.entities[0].id;
        let second = SymbolId::from_bytes([38; 20]);
        document.entities[0].container = Some(ContainerRef::Entity(second));
        let mut second_entity = document.entities[0].clone();
        second_entity.id = second;
        second_entity.container = Some(ContainerRef::Entity(first));
        document.entities.push(second_entity);

        assert!(matches!(
            canonicalize_ir_document(document, &IrLimits::default(), &ExtensionSupport::default()),
            Err(IrDocumentValidationError::ContainerCycle(_))
        ));
    }

    #[test]
    fn rejects_candidate_and_coverage_count_inconsistency() {
        let mut candidates = fixture();
        let symbol = candidates.entities[0].id;
        candidates.occurrences[0].target = OccurrenceTarget::Candidates {
            symbols: vec![symbol],
            total_count: 0,
            completeness: CoverageStatus::Bounded,
        };
        assert!(matches!(
            canonicalize_ir_document(
                candidates,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::InvalidCandidateCount { .. })
        ));

        let mut coverage = fixture();
        coverage.coverage_records[0].indexed = 2;
        coverage.coverage_records[0].skipped = 1;
        coverage.coverage_records[0].discovered = 2;
        assert!(matches!(
            canonicalize_ir_document(coverage, &IrLimits::default(), &ExtensionSupport::default()),
            Err(IrDocumentValidationError::InvalidCoverageCounts(_))
        ));
    }

    #[test]
    fn default_provenance_capacity_matches_large_entity_corpora() {
        let limits = IrLimits::default();

        assert_eq!(limits.max_provenance_records, 2_000_000);
        assert!(limits.max_provenance_records >= limits.max_entities);
        assert!(limits.max_provenance_records < limits.max_total_records);
        assert_eq!(limits.max_total_nested_items, 20_000_000);
    }

    #[test]
    fn enforces_every_top_level_collection_limit() {
        type TightenLimit = fn(&mut IrLimits);
        let cases: [(&str, TightenLimit); 10] = [
            ("files", |limits| limits.max_files = 0),
            ("entities", |limits| limits.max_entities = 0),
            ("occurrences", |limits| limits.max_occurrences = 0),
            ("relations", |limits| limits.max_relations = 0),
            ("provenance", |limits| limits.max_provenance_records = 0),
            ("source_mappings", |limits| limits.max_source_mappings = 0),
            ("coverage_records", |limits| limits.max_coverage_records = 0),
            ("skipped_regions", |limits| limits.max_skipped_regions = 0),
            ("diagnostics", |limits| limits.max_diagnostics = 0),
            ("extensions", |limits| limits.max_extensions = 0),
        ];
        for (expected, tighten) in cases {
            let mut limits = IrLimits::default();
            tighten(&mut limits);
            assert!(matches!(
                canonicalize_ir_document(fixture(), &limits, &ExtensionSupport::default()),
                Err(IrDocumentValidationError::CollectionLimit {
                    collection,
                    observed: 1,
                    limit: 0
                }) if collection == expected
            ));
        }
    }

    #[test]
    fn borrowed_validation_checks_raw_limits_before_semantic_work() {
        let mut document = fixture();
        document.extensions[0].namespace = "not_namespaced".to_owned();
        let limits = IrLimits {
            max_extensions: 0,
            ..IrLimits::default()
        };

        assert!(matches!(
            validate_ir_document(&document, &limits, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::CollectionLimit {
                collection: "extensions",
                observed: 1,
                limit: 0
            })
        ));
    }

    #[test]
    fn enforces_total_nested_string_extension_and_diagnostic_limits() {
        let total_records = IrLimits {
            max_total_records: 9,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &total_records, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::TotalRecordLimit {
                observed: 10,
                limit: 9
            })
        ));

        let nested = IrLimits {
            max_nested_items_per_record: 0,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &nested, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::CollectionLimit { collection, .. })
                if collection == "entity.flags"
        ));

        let total_nested = IrLimits {
            max_total_nested_items: 0,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &total_nested, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::TotalNestedItemLimit { .. })
        ));

        let string = IrLimits {
            max_string_bytes: 1,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &string, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::StringLimit { .. })
        ));

        let total_string = IrLimits {
            max_total_string_bytes: 1,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &total_string, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::TotalStringLimit { .. })
        ));

        let extension = IrLimits {
            max_extension_payload_bytes: 1,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &extension, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::ExtensionPayloadLimit { .. })
        ));

        let total_extension = IrLimits {
            max_total_extension_bytes: 1,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &total_extension, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::TotalExtensionBytesLimit { .. })
        ));

        let diagnostic = IrLimits {
            max_diagnostic_message_bytes: 1,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &diagnostic, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::DiagnosticMessageLimit { .. })
        ));

        let total_diagnostic = IrLimits {
            max_total_diagnostic_bytes: 1,
            ..IrLimits::default()
        };
        assert!(matches!(
            canonicalize_ir_document(fixture(), &total_diagnostic, &ExtensionSupport::default()),
            Err(IrDocumentValidationError::TotalDiagnosticBytesLimit { .. })
        ));
    }

    #[test]
    fn critical_extensions_require_support_and_unknown_noncritical_policy_is_explicit() {
        let mut critical = fixture();
        critical.extensions[0].criticality = ExtensionCriticality::Critical;
        assert!(matches!(
            canonicalize_ir_document(
                critical.clone(),
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(IrDocumentValidationError::UnsupportedCriticalExtension { .. })
        ));

        let identifier = ExtensionIdentifier::new(
            critical.extensions[0].namespace.clone(),
            critical.extensions[0].version.clone(),
        );
        let supported = ExtensionSupport {
            supported_critical: BTreeSet::from([identifier]),
            unknown_noncritical: UnknownNoncriticalExtensionPolicy::Preserve,
        };
        let supported_output = canonicalize_ir_document(critical, &IrLimits::default(), &supported)
            .expect("declared critical extension canonicalizes");
        assert_eq!(supported_output.extensions.len(), 1);

        let skipped = ExtensionSupport {
            supported_critical: BTreeSet::new(),
            unknown_noncritical: UnknownNoncriticalExtensionPolicy::Skip,
        };
        let output = canonicalize_ir_document(fixture(), &IrLimits::default(), &skipped)
            .expect("unknown noncritical extension skips");
        assert!(output.extensions.is_empty());

        let mut invalid_reference = fixture();
        invalid_reference.extensions[0].provenance = FactId::from_bytes([42; 20]);
        assert!(matches!(
            canonicalize_ir_document(invalid_reference, &IrLimits::default(), &skipped),
            Err(IrDocumentValidationError::MissingProvenance(_))
        ));

        let mut common_dependency = fixture();
        let extension = common_dependency.extensions[0].id;
        common_dependency.files[0]
            .evidence
            .derivation
            .push(FactRef::Fact(extension));
        assert_eq!(
            canonicalize_ir_document(common_dependency, &IrLimits::default(), &skipped),
            Err(IrDocumentValidationError::MissingDerivation(FactRef::Fact(
                extension
            )))
        );

        let mut invalid = fixture();
        invalid.extensions[0].namespace = "not_namespaced".to_owned();
        assert!(matches!(
            canonicalize_ir_document(invalid, &IrLimits::default(), &skipped),
            Err(IrDocumentValidationError::InvalidExtensionIdentity { .. })
        ));
    }
}
