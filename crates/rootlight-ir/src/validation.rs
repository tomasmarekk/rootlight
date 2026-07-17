//! Validation and deterministic canonicalization for normalized IR 1.1.
//!
//! Limits are checked before deduplication. Canonicalization then normalizes all
//! collection order before referential and source-integrity checks run.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId};

use crate::{
    ContainerRef, CoverageScope, ExtensionCriticality, FactEvidence, FactRef,
    NORMALIZED_IR_VERSION, NormalizedIrDocument, OccurrenceTarget, RelationEndpoint, SourceRef,
};

/// Resource limits applied to one decoded normalized IR document.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IrLimits {
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

impl Default for IrLimits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_entities: 1_000_000,
            max_occurrences: 5_000_000,
            max_relations: 5_000_000,
            max_provenance_records: 100_000,
            max_source_mappings: 1_000_000,
            max_coverage_records: 500_000,
            max_skipped_regions: 100_000,
            max_diagnostics: 10_000,
            max_extensions: 10_000,
            max_total_records: 10_000_000,
            max_nested_items_per_record: 4_096,
            max_total_nested_items: 10_000_000,
            max_string_bytes: 32_768,
            max_total_string_bytes: 256 * 1024 * 1024,
            max_extension_payload_bytes: 1024 * 1024,
            max_total_extension_bytes: 16 * 1024 * 1024,
            max_diagnostic_message_bytes: 4_096,
            max_total_diagnostic_bytes: 4 * 1024 * 1024,
        }
    }
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
    validate_version(document)?;
    validate_limits(document, limits)?;
    prepare_bounded_document(document.clone(), extensions).map(|_| ())
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
    validate_version(&document)?;
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

fn validate_version(document: &NormalizedIrDocument) -> Result<(), IrDocumentValidationError> {
    let version = document.version.value();
    if version == NORMALIZED_IR_VERSION {
        Ok(())
    } else {
        Err(IrDocumentValidationError::UnsupportedVersion {
            major: version.major(),
            minor: version.minor(),
        })
    }
}

fn validate_limits(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), IrDocumentValidationError> {
    let collections = [
        ("files", document.files.len(), limits.max_files),
        ("entities", document.entities.len(), limits.max_entities),
        (
            "occurrences",
            document.occurrences.len(),
            limits.max_occurrences,
        ),
        ("relations", document.relations.len(), limits.max_relations),
        (
            "provenance",
            document.provenance.len(),
            limits.max_provenance_records,
        ),
        (
            "source_mappings",
            document.source_mappings.len(),
            limits.max_source_mappings,
        ),
        (
            "coverage_records",
            document.coverage_records.len(),
            limits.max_coverage_records,
        ),
        (
            "skipped_regions",
            document.skipped_regions.len(),
            limits.max_skipped_regions,
        ),
        (
            "diagnostics",
            document.diagnostics.len(),
            limits.max_diagnostics,
        ),
        (
            "extensions",
            document.extensions.len(),
            limits.max_extensions,
        ),
    ];
    let mut total_records = 0_usize;
    for (collection, observed, limit) in collections {
        check_collection_limit(collection, observed, limit)?;
        total_records = total_records.saturating_add(observed);
    }
    if total_records > limits.max_total_records {
        return Err(IrDocumentValidationError::TotalRecordLimit {
            observed: total_records,
            limit: limits.max_total_records,
        });
    }

    let mut budget = NestedBudget::default();
    for file in &document.files {
        budget.evidence("file.evidence.derivation", &file.evidence, limits)?;
        budget.string("file.path", &file.path, limits)?;
        budget.string("file.language", &file.language, limits)?;
        budget.string("file.encoding", &file.encoding, limits)?;
    }
    for entity in &document.entities {
        budget.nested("entity.flags", entity.flags.len(), limits)?;
        budget.evidence("entity.evidence.derivation", &entity.evidence, limits)?;
        budget.string("entity.language", &entity.language, limits)?;
        budget.string("entity.canonical_name", &entity.canonical_name, limits)?;
        budget.string("entity.display_name", &entity.display_name, limits)?;
        budget.string("entity.qualified_name", &entity.qualified_name, limits)?;
    }
    for occurrence in &document.occurrences {
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
        budget.evidence("relation.evidence.derivation", &relation.evidence, limits)?;
    }
    for provenance in &document.provenance {
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
        budget.evidence(
            "source_mapping.evidence.derivation",
            &mapping.evidence,
            limits,
        )?;
    }
    for coverage in &document.coverage_records {
        budget.evidence("coverage.evidence.derivation", &coverage.evidence, limits)?;
    }
    for skipped in &document.skipped_regions {
        budget.evidence(
            "skipped_region.evidence.derivation",
            &skipped.evidence,
            limits,
        )?;
        budget.string("skipped_region.detail", &skipped.detail, limits)?;
    }
    for diagnostic in &document.diagnostics {
        budget.evidence(
            "diagnostic.evidence.derivation",
            &diagnostic.evidence,
            limits,
        )?;
        budget.string("diagnostic.code", &diagnostic.code, limits)?;
        budget.string("diagnostic.message", &diagnostic.message, limits)?;
        let message_bytes = diagnostic.message.len();
        if message_bytes > limits.max_diagnostic_message_bytes {
            return Err(IrDocumentValidationError::DiagnosticMessageLimit {
                id: diagnostic.id,
                observed: message_bytes,
                limit: limits.max_diagnostic_message_bytes,
            });
        }
        budget.total_diagnostic_bytes = budget
            .total_diagnostic_bytes
            .saturating_add(diagnostic.code.len())
            .saturating_add(message_bytes);
    }
    for extension in &document.extensions {
        budget.evidence("extension.evidence.derivation", &extension.evidence, limits)?;
        budget.string("extension.namespace", &extension.namespace, limits)?;
        budget.string("extension.version", &extension.version, limits)?;
        let payload_bytes = extension.payload.len();
        if payload_bytes > limits.max_extension_payload_bytes {
            return Err(IrDocumentValidationError::ExtensionPayloadLimit {
                id: extension.id,
                observed: payload_bytes,
                limit: limits.max_extension_payload_bytes,
            });
        }
        budget.total_extension_bytes = budget.total_extension_bytes.saturating_add(payload_bytes);
    }

    if budget.total_nested_items > limits.max_total_nested_items {
        return Err(IrDocumentValidationError::TotalNestedItemLimit {
            observed: budget.total_nested_items,
            limit: limits.max_total_nested_items,
        });
    }
    if budget.total_string_bytes > limits.max_total_string_bytes {
        return Err(IrDocumentValidationError::TotalStringLimit {
            observed: budget.total_string_bytes,
            limit: limits.max_total_string_bytes,
        });
    }
    if budget.total_extension_bytes > limits.max_total_extension_bytes {
        return Err(IrDocumentValidationError::TotalExtensionBytesLimit {
            observed: budget.total_extension_bytes,
            limit: limits.max_total_extension_bytes,
        });
    }
    if budget.total_diagnostic_bytes > limits.max_total_diagnostic_bytes {
        return Err(IrDocumentValidationError::TotalDiagnosticBytesLimit {
            observed: budget.total_diagnostic_bytes,
            limit: limits.max_total_diagnostic_bytes,
        });
    }
    Ok(())
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

#[derive(Default)]
struct NestedBudget {
    total_nested_items: usize,
    total_string_bytes: usize,
    total_extension_bytes: usize,
    total_diagnostic_bytes: usize,
}

impl NestedBudget {
    fn nested(
        &mut self,
        collection: &'static str,
        observed: usize,
        limits: &IrLimits,
    ) -> Result<(), IrDocumentValidationError> {
        check_collection_limit(collection, observed, limits.max_nested_items_per_record)?;
        self.total_nested_items = self.total_nested_items.saturating_add(observed);
        Ok(())
    }

    fn evidence(
        &mut self,
        collection: &'static str,
        evidence: &FactEvidence,
        limits: &IrLimits,
    ) -> Result<(), IrDocumentValidationError> {
        self.nested(collection, evidence.derivation.len(), limits)
    }

    fn string(
        &mut self,
        field: &'static str,
        value: &str,
        limits: &IrLimits,
    ) -> Result<(), IrDocumentValidationError> {
        let observed = value.len();
        if observed > limits.max_string_bytes {
            return Err(IrDocumentValidationError::StringLimit {
                field,
                observed,
                limit: limits.max_string_bytes,
            });
        }
        self.total_string_bytes = self.total_string_bytes.saturating_add(observed);
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

/// Validation failures for normalized IR 1.1 documents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IrDocumentValidationError {
    /// The document uses a version other than exactly 1.1.
    #[error("unsupported normalized IR version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major component.
        major: u16,
        /// Unsupported minor component.
        minor: u16,
    },
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
    use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};

    use super::*;
    use crate::{
        ContainerRef, CoverageStatus, ExtensionIdentifier, IrLimits,
        UnknownNoncriticalExtensionPolicy,
    };

    fn fixture() -> NormalizedIrDocument {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/compatibility/ir/1.1/document.json"
        ))
        .expect("frozen normalized IR fixture decodes")
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
