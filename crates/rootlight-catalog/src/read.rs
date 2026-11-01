//! Defensive reconstruction of owned generation data from a sealed oracle.
//!
//! Every query has fixed SQL and stable-ID ordering. Persisted scalars are
//! decoded as hostile input before normalized IR validation runs again.

use std::io::Write as _;

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageRecord, CoverageStatus, DiagnosticRecord,
    EntityFlag, EntityKind, EntityRecord, EntityVisibility, EvidenceKind, ExtensionEnvelope,
    ExtensionEnvelopeDecodeError, ExtensionSupport, FactDomain, FactEvidence, FactRef,
    FilePathLocator, FilePathLocatorEncoding, FileRecord, IrLimits, NormalizedIrDocument,
    NormalizedIrVersion, NormalizedRecordDecodeError, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord, RelationPredicate,
    RelationRecord, SkippedRegion, SourceMappingRecord, SourceRef,
    decode_diagnostic_record_with_checkpoint, decode_extension_envelope_with_checkpoint,
    decode_skipped_region_with_checkpoint, decode_source_mapping_record_with_checkpoint,
};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationContext, GenerationContractVersion, GenerationMetadata,
    GenerationResource, GenerationSnapshot, GenerationSnapshotError, GenerationStats,
};
use rusqlite::{Connection, OptionalExtension, Row, types::FromSql};

use crate::{
    CatalogError, CatalogErrorKind, OracleReadAdmissionError, OracleReadMemoryPlan, codec, write,
};

#[derive(Debug)]
struct EvidenceGroup {
    owner: FactRef,
    items: Vec<FactRef>,
}

#[derive(Debug)]
struct EvidenceGroups {
    groups: Vec<EvidenceGroup>,
    remaining: usize,
}

fn evidence_key(reference: &FactRef) -> (u8, &[u8]) {
    // SQLite's closed text tags sort entity, fact, file; enum ordering differs.
    match reference {
        FactRef::Entity(id) => (0, id.as_bytes()),
        FactRef::Fact(id) => (1, id.as_bytes()),
        FactRef::File(id) => (2, id.as_bytes()),
    }
}

impl EvidenceGroups {
    fn is_empty(&self) -> bool {
        self.remaining == 0
    }

    fn take(&mut self, owner: FactRef) -> Result<Vec<FactRef>, CatalogError> {
        let Ok(index) = self.groups.binary_search_by(|group| {
            evidence_key(&group.owner)
                .cmp(&evidence_key(&owner))
                .reverse()
        }) else {
            return Ok(Vec::new());
        };
        let group = &mut self.groups[index];
        if !group.items.is_empty() {
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or_else(memory_plan_overflow)?;
        }
        Ok(std::mem::take(&mut group.items))
    }
}

fn evidence_reference(kind: &str, id: &[u8]) -> Result<FactRef, CatalogError> {
    let id = id
        .try_into()
        .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
    match kind {
        "file" => Ok(FactRef::File(FileId::from_bytes(id))),
        "entity" => Ok(FactRef::Entity(SymbolId::from_bytes(id))),
        "fact" => Ok(FactRef::Fact(FactId::from_bytes(id))),
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

fn read_evidence_reference(row: &Row<'_>, kind: usize, id: usize) -> Result<FactRef, CatalogError> {
    let kind = row.get_ref(kind).map_err(CatalogError::sqlite)?;
    let id = row.get_ref(id).map_err(CatalogError::sqlite)?;
    evidence_reference(
        kind.as_str()
            .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?,
        id.as_blob()
            .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?,
    )
}

struct HeaderRow {
    contract_major: i64,
    contract_minor: i64,
    ir_major: i64,
    ir_minor: i64,
    repository: Vec<u8>,
    generation: Vec<u8>,
    parent: Option<Vec<u8>>,
    manifest_hash: Vec<u8>,
    configuration_hash: Vec<u8>,
    provider_set_hash: Vec<u8>,
    files: i64,
    entities: i64,
    occurrences: i64,
    relations: i64,
    provenance: i64,
    source_mappings: i64,
    coverage: i64,
    skipped_regions: i64,
    diagnostics: i64,
    extensions: i64,
    source_refs: i64,
    stored_rows: i64,
    text_bytes: i64,
    sealed: i64,
}

pub(crate) fn read_header(
    connection: &Connection,
    context: &GenerationContext<'_>,
) -> Result<(GenerationMetadata, GenerationStats, NormalizedIrVersion), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let raw = connection
        .query_row(
            "SELECT
                contract_major, contract_minor, ir_major, ir_minor,
                repository_id, generation_id, parent_generation_id,
                manifest_hash, configuration_hash, provider_set_hash,
                file_count, entity_count, occurrence_count, relation_count,
                provenance_count, source_mapping_count, coverage_count,
                skipped_region_count, diagnostic_count, extension_count,
                source_ref_count,
                stored_row_count, text_bytes, sealed
             FROM generation_meta
             WHERE singleton = 1",
            [],
            |row| {
                Ok(HeaderRow {
                    contract_major: row.get(0)?,
                    contract_minor: row.get(1)?,
                    ir_major: row.get(2)?,
                    ir_minor: row.get(3)?,
                    repository: row.get(4)?,
                    generation: row.get(5)?,
                    parent: row.get(6)?,
                    manifest_hash: row.get(7)?,
                    configuration_hash: row.get(8)?,
                    provider_set_hash: row.get(9)?,
                    files: row.get(10)?,
                    entities: row.get(11)?,
                    occurrences: row.get(12)?,
                    relations: row.get(13)?,
                    provenance: row.get(14)?,
                    source_mappings: row.get(15)?,
                    coverage: row.get(16)?,
                    skipped_regions: row.get(17)?,
                    diagnostics: row.get(18)?,
                    extensions: row.get(19)?,
                    source_refs: row.get(20)?,
                    stored_rows: row.get(21)?,
                    text_bytes: row.get(22)?,
                    sealed: row.get(23)?,
                })
            },
        )
        .map_err(CatalogError::sqlite)?;
    let contract_version = GenerationContractVersion::new(
        u16::try_from(raw.contract_major)
            .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?,
        u16::try_from(raw.contract_minor)
            .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?,
    );
    if raw.sealed != 1
        || (contract_version != GENERATION_CONTRACT_VERSION
            && contract_version != GenerationContractVersion::new(1, 1))
    {
        return Err(CatalogError::new(CatalogErrorKind::IncompatibleSchema));
    }
    let ir_version = match (raw.ir_major, raw.ir_minor) {
        (1, 1) => NormalizedIrVersion::V1_1,
        (1, 2) => NormalizedIrVersion::V1_2,
        (1, 3) => NormalizedIrVersion::V1_3,
        (1, 4) => NormalizedIrVersion::V1_4,
        (1, 5) => NormalizedIrVersion::V1_5,
        (1, 6) => NormalizedIrVersion::V1_6,
        (1, 7) => NormalizedIrVersion::V1_7,
        _ => return Err(CatalogError::new(CatalogErrorKind::IncompatibleSchema)),
    };
    let metadata = GenerationMetadata::new_for_contract(
        contract_version,
        codec::repository_id(raw.repository)?,
        codec::generation_id(raw.generation)?,
        codec::optional_generation_id(raw.parent)?,
        codec::content_hash(raw.manifest_hash)?,
        codec::content_hash(raw.configuration_hash)?,
        codec::content_hash(raw.provider_set_hash)?,
    )
    .map_err(CatalogError::corrupt_generation)?;
    let stats = GenerationStats::new(
        codec::nonnegative_u64(raw.files)?,
        codec::nonnegative_u64(raw.entities)?,
        codec::nonnegative_u64(raw.occurrences)?,
        codec::nonnegative_u64(raw.relations)?,
        codec::nonnegative_u64(raw.provenance)?,
        codec::nonnegative_u64(raw.source_mappings)?,
        codec::nonnegative_u64(raw.coverage)?,
        codec::nonnegative_u64(raw.skipped_regions)?,
        codec::nonnegative_u64(raw.diagnostics)?,
        codec::nonnegative_u64(raw.extensions)?,
        codec::nonnegative_u64(raw.source_refs)?,
        codec::nonnegative_u64(raw.stored_rows)?,
        codec::nonnegative_u64(raw.text_bytes)?,
    )
    .map_err(CatalogError::corrupt_generation)?;
    context
        .require(GenerationResource::Rows, stats.stored_rows())
        .map_err(CatalogError::control)?;
    context
        .require(GenerationResource::SourceReferences, stats.source_refs())
        .map_err(CatalogError::control)?;
    context
        .require(GenerationResource::EncodedTextBytes, stats.text_bytes())
        .map_err(CatalogError::control)?;
    validate_text_bytes(connection, stats, context)?;
    Ok((metadata, stats, ir_version))
}

fn validate_text_bytes(
    connection: &Connection,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let queries = [
        (
            "SELECT
                length(CAST(path AS BLOB))
              + coalesce(length(CAST(path_locator_encoding AS BLOB)), 0)
              + coalesce(length(CAST(path_locator_components AS BLOB)), 0)
              + length(CAST(language AS BLOB))
              + length(CAST(encoding AS BLOB))
             FROM files
             LIMIT ?1",
            stats.files(),
        ),
        (
            "SELECT
                length(CAST(language AS BLOB))
              + length(CAST(canonical_name AS BLOB))
              + length(CAST(display_name AS BLOB))
              + length(CAST(qualified_name AS BLOB))
             FROM entities
             LIMIT ?1",
            stats.entities(),
        ),
        (
            "SELECT length(CAST(syntax_kind AS BLOB))
             FROM occurrences
             LIMIT ?1",
            stats.occurrences(),
        ),
        (
            "SELECT
                length(CAST(producer_name AS BLOB))
              + length(CAST(producer_version AS BLOB))
              + coalesce(length(CAST(frontend_version AS BLOB)), 0)
              + length(CAST(language AS BLOB))
              + coalesce(length(CAST(rule AS BLOB)), 0)
             FROM provenance
             LIMIT ?1",
            stats.provenance(),
        ),
        (
            "SELECT length(CAST(payload AS BLOB))
             FROM source_mappings
             LIMIT ?1",
            stats.source_mappings(),
        ),
        (
            "SELECT length(CAST(payload AS BLOB))
             FROM skipped_regions
             LIMIT ?1",
            stats.skipped_regions(),
        ),
        (
            "SELECT length(CAST(payload AS BLOB))
             FROM diagnostics
             LIMIT ?1",
            stats.diagnostics(),
        ),
        (
            "SELECT length(CAST(payload AS BLOB))
             FROM extensions
             LIMIT ?1",
            stats.extensions(),
        ),
    ];
    let mut observed_bytes = 0_u64;
    let mut observed_rows = 0_u64;
    for (sql, expected_rows) in queries {
        accumulate_text_rows(
            connection,
            sql,
            expected_rows,
            &mut observed_rows,
            &mut observed_bytes,
            context,
        )?;
    }
    if observed_bytes != stats.text_bytes() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    context.check().map_err(CatalogError::control)
}

fn accumulate_text_rows(
    connection: &Connection,
    sql: &str,
    expected_rows: u64,
    observed_rows: &mut u64,
    observed_bytes: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let row_limit = expected_rows
        .checked_add(1)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let mut statement = connection.prepare(sql).map_err(CatalogError::sqlite)?;
    let mut rows = statement
        .query([codec::sqlite_i64(row_limit)?])
        .map_err(CatalogError::sqlite)?;
    let mut table_rows = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        table_rows = table_rows
            .checked_add(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        if table_rows > expected_rows {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        *observed_rows = observed_rows
            .checked_add(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::Rows, *observed_rows)
            .map_err(CatalogError::control)?;
        let row_bytes = codec::nonnegative_u64(get(row, 0)?)?;
        *observed_bytes = observed_bytes
            .checked_add(row_bytes)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::EncodedTextBytes, *observed_bytes)
            .map_err(CatalogError::control)?;
    }
    if table_rows != expected_rows {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

pub(crate) fn read_generation(
    connection: &Connection,
    expected_metadata: GenerationMetadata,
    expected_stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<GenerationSnapshot, CatalogError> {
    match read_generation_with_admission(
        connection,
        expected_metadata,
        expected_stats,
        context,
        |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(OracleReadAdmissionError::Catalog(error)) => Err(error),
        Err(OracleReadAdmissionError::Admission(never)) => match never {},
    }
}

pub(crate) fn read_generation_with_admission<E>(
    connection: &Connection,
    expected_metadata: GenerationMetadata,
    expected_stats: GenerationStats,
    context: &GenerationContext<'_>,
    admit: impl FnOnce(OracleReadMemoryPlan) -> Result<(), E>,
) -> Result<GenerationSnapshot, OracleReadAdmissionError<E>> {
    read_generation_with_policy(
        connection,
        expected_metadata,
        expected_stats,
        context,
        admit,
        false,
    )
}

pub(crate) fn has_rdf_partition_set(
    connection: &Connection,
    context: &GenerationContext<'_>,
) -> Result<bool, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let found: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM extensions WHERE json_extract(payload, '$.namespace') = ?1)",
        [rootlight_storage::RDF_PARTITION_SET_NAMESPACE],
        |row| row.get(0),
    ).map_err(CatalogError::sqlite)?;
    context.check().map_err(CatalogError::control)?;
    Ok(found)
}

pub(crate) fn read_rdf_base(
    connection: &Connection,
    metadata: GenerationMetadata,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<rootlight_storage::IdentityVerifiedRdfBase, CatalogError> {
    if !crate::schema::is_rdf_base(connection)? {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let snapshot = match read_generation_with_policy(
        connection,
        metadata,
        stats,
        context,
        |_| Ok::<(), std::convert::Infallible>(()),
        true,
    ) {
        Ok(snapshot) => snapshot,
        Err(OracleReadAdmissionError::Catalog(error)) => return Err(error),
        Err(OracleReadAdmissionError::Admission(never)) => match never {},
    };
    rootlight_storage::IdentityVerifiedRdfBase::verify_snapshot(snapshot, context)
        .map_err(crate::identity_catalog_error)
}

fn read_generation_with_policy<E>(
    connection: &Connection,
    expected_metadata: GenerationMetadata,
    expected_stats: GenerationStats,
    context: &GenerationContext<'_>,
    admit: impl FnOnce(OracleReadMemoryPlan) -> Result<(), E>,
    allow_rdf_base: bool,
) -> Result<GenerationSnapshot, OracleReadAdmissionError<E>> {
    let mut transaction = connection
        .unchecked_transaction()
        .map_err(CatalogError::sqlite)?;
    let (metadata, stats, ir_version) = read_header(&transaction, context)?;
    if metadata != expected_metadata || stats != expected_stats {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt).into());
    }
    if !allow_rdf_base
        && (crate::schema::is_rdf_base(&transaction)?
            || has_rdf_partition_set(&transaction, context)?)
    {
        return Err(CatalogError::new(CatalogErrorKind::PartitionedMaterializationRequired).into());
    }
    let cardinality =
        validate_payload_cardinality(&transaction, metadata.contract_version(), stats, context)?;
    let entity_flag_rows = cardinality.entity_flag_rows;
    let maximum_payload_bytes = maximum_payload_bytes(&transaction, stats, context)?;
    let symbol_envelopes =
        symbol_envelope_census(&transaction, metadata, stats.extensions(), context)?;
    let skipped_metadata_bytes = skipped_metadata_bytes(&transaction, stats, context)?;
    let memory_plan = oracle_read_memory_plan(
        metadata.contract_version(),
        stats,
        maximum_payload_bytes,
        cardinality,
        symbol_envelopes.rows,
    )?
    .with_nonretained_metadata_bytes(
        skipped_metadata_bytes
            .checked_add(symbol_envelopes.metadata_bytes)
            .ok_or_else(memory_plan_overflow)?,
    )?;
    validate_identity_registry_layout(&transaction, memory_plan.identity_registry_rows(), context)?;
    validate_source_reference_layout(&transaction, stats.source_refs(), context)?;
    admit(memory_plan).map_err(OracleReadAdmissionError::Admission)?;
    context.check().map_err(CatalogError::control)?;
    let sources = read_sources(&transaction, stats.source_refs(), context)?;
    let evidence_owners = evidence_owner_count(stats)?;
    let mut evidence = read_evidence(
        &transaction,
        cardinality.evidence_rows,
        evidence_owners,
        context,
    )?;
    let mut flags = read_flags(&transaction, entity_flag_rows, stats.entities(), context)?;
    let mut candidates = read_candidates(
        &transaction,
        cardinality.occurrence_candidate_rows,
        stats.occurrences(),
        context,
    )?;
    let mut provenance_sources = read_provenance_sources(
        &transaction,
        &sources,
        cardinality.provenance_source_rows,
        stats.provenance(),
        context,
    )?;
    let mut provenance_derivations = read_provenance_derivations(
        &transaction,
        cardinality.provenance_derivation_rows,
        stats.provenance(),
        context,
    )?;

    let provenance = read_provenance(
        &transaction,
        stats.provenance(),
        &mut provenance_sources,
        &mut provenance_derivations,
        context,
    )?;
    let files = read_files(
        &transaction,
        stats.files(),
        &sources,
        &mut evidence,
        context,
    )?;
    let entities = read_entities(
        &transaction,
        stats.entities(),
        &sources,
        &mut evidence,
        &mut flags,
        context,
    )?;
    let occurrences = read_occurrences(
        &transaction,
        stats.occurrences(),
        &sources,
        &mut evidence,
        &mut candidates,
        context,
    )?;
    let relations = read_relations(
        &transaction,
        stats.relations(),
        &sources,
        &mut evidence,
        context,
    )?;
    let coverage_records = read_coverage(
        &transaction,
        stats.coverage(),
        &sources,
        &mut evidence,
        context,
    )?;
    let document = read_complete_document(
        &transaction,
        metadata,
        ir_version,
        stats,
        files,
        entities,
        occurrences,
        relations,
        provenance,
        coverage_records,
        context,
    )?;

    if !evidence.is_empty()
        || !flags.is_empty()
        || !candidates.is_empty()
        || !provenance_sources.is_empty()
        || !provenance_derivations.is_empty()
    {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt).into());
    }
    for (observed, expected) in [
        (usize_to_u64(document.files.len())?, stats.files()),
        (usize_to_u64(document.entities.len())?, stats.entities()),
        (
            usize_to_u64(document.occurrences.len())?,
            stats.occurrences(),
        ),
        (usize_to_u64(document.relations.len())?, stats.relations()),
        (usize_to_u64(document.provenance.len())?, stats.provenance()),
        (
            usize_to_u64(document.source_mappings.len())?,
            stats.source_mappings(),
        ),
        (
            usize_to_u64(document.coverage_records.len())?,
            stats.coverage(),
        ),
        (
            usize_to_u64(document.skipped_regions.len())?,
            stats.skipped_regions(),
        ),
        (
            usize_to_u64(document.diagnostics.len())?,
            stats.diagnostics(),
        ),
        (usize_to_u64(document.extensions.len())?, stats.extensions()),
    ] {
        if observed != expected {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt).into());
        }
    }

    validate_document_hash(&transaction, &document, context)?;
    let support = if allow_rdf_base {
        rootlight_storage::rdf_partition_extension_support()
    } else {
        ExtensionSupport::default()
    };
    let snapshot = GenerationSnapshot::new_with_context(
        metadata,
        document,
        &IrLimits::default(),
        &support,
        context,
    )
    .map_err(|error| match error {
        GenerationSnapshotError::Control(error) => CatalogError::control(error),
        GenerationSnapshotError::Validation(error) => CatalogError::corrupt_generation(error),
    })?;
    context.check().map_err(CatalogError::control)?;
    let plan = if allow_rdf_base {
        write::measure_rdf_base(&snapshot, context)?
    } else {
        write::measure(&snapshot, context)?
    };
    let comparable_stats = GenerationStats::new(
        stats.files(),
        stats.entities(),
        stats.occurrences(),
        stats.relations(),
        stats.provenance(),
        stats.source_mappings(),
        stats.coverage(),
        stats.skipped_regions(),
        stats.diagnostics(),
        stats.extensions(),
        stats.source_refs(),
        plan.stats.stored_rows(),
        stats.text_bytes(),
    )
    .map_err(CatalogError::corrupt_generation)?;
    let stored_rows_match = if metadata.contract_version() == GenerationContractVersion::new(1, 1) {
        stats.stored_rows().checked_add(1) == Some(plan.stats.stored_rows())
    } else {
        stats.stored_rows() == plan.stats.stored_rows()
    };
    if comparable_stats != plan.stats || !stored_rows_match {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt).into());
    }
    validate_identity_registry(&transaction, &plan.identities, context)?;
    validate_source_registry(&sources, &plan.source_ordinals, context)?;
    if allow_rdf_base {
        // The logical reader retains this authenticated base snapshot alongside
        // its already pinned leaves; callers release it by closing the connection.
        transaction.set_drop_behavior(rusqlite::DropBehavior::Ignore);
    } else {
        transaction.commit().map_err(CatalogError::sqlite)?;
    }
    Ok(snapshot)
}

pub(crate) fn oracle_read_memory_plan(
    contract_version: GenerationContractVersion,
    stats: GenerationStats,
    maximum_payload_bytes: u64,
    cardinality: PayloadCardinality,
    symbol_envelopes: u64,
) -> Result<OracleReadMemoryPlan, CatalogError> {
    let PayloadCardinality {
        entity_flag_rows,
        evidence_rows,
        occurrence_candidate_rows,
        provenance_derivation_rows,
        provenance_source_rows,
    } = cardinality;
    if symbol_envelopes > stats.extensions() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let logical_rows = [
        stats.files(),
        stats.entities(),
        stats.occurrences(),
        stats.relations(),
        stats.provenance(),
        stats.source_mappings(),
        stats.coverage(),
        stats.skipped_regions(),
        stats.diagnostics(),
        stats.extensions(),
    ]
    .into_iter()
    .try_fold(0_u64, u64::checked_add)
    .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let identity_registry_rows = logical_rows
        .checked_add(1)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let physical_rows = if contract_version == GenerationContractVersion::new(1, 1) {
        stats.stored_rows().checked_add(1)
    } else {
        Some(stats.stored_rows())
    }
    .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let other_rows = physical_rows
        .checked_sub(identity_registry_rows)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;

    let closed_rows = stats
        .files()
        .checked_add(stats.entities())
        .and_then(|rows| rows.checked_add(stats.occurrences()))
        .and_then(|rows| rows.checked_add(stats.relations()))
        .and_then(|rows| rows.checked_add(stats.provenance()))
        .and_then(|rows| rows.checked_add(stats.coverage()))
        .and_then(|rows| rows.checked_add(stats.skipped_regions()))
        .and_then(|rows| rows.checked_add(stats.diagnostics()))
        .and_then(|rows| rows.checked_add(stats.source_refs()))
        .ok_or_else(memory_plan_overflow)?;
    let opaque_and_auxiliary_rows = other_rows
        .checked_sub(closed_rows)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let opaque_and_other_auxiliary_rows = opaque_and_auxiliary_rows
        .checked_sub(entity_flag_rows)
        .and_then(|rows| rows.checked_sub(symbol_envelopes))
        .and_then(|rows| rows.checked_sub(evidence_rows))
        .and_then(|rows| rows.checked_sub(occurrence_candidate_rows))
        .and_then(|rows| rows.checked_sub(provenance_derivation_rows))
        .and_then(|rows| rows.checked_sub(provenance_source_rows))
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let evidence_workspace = evidence_workspace_bytes(evidence_rows, evidence_owner_count(stats)?)?;
    let flag_workspace = flag_workspace_bytes(entity_flag_rows, stats.entities())?;
    let candidate_workspace =
        fact_group_workspace_bytes::<SymbolId>(occurrence_candidate_rows, stats.occurrences())?;
    let derivation_workspace =
        fact_group_workspace_bytes::<FactRef>(provenance_derivation_rows, stats.provenance())?;
    let provenance_source_workspace =
        provenance_source_workspace_bytes(provenance_source_rows, stats.provenance())?;

    // Exact buffers are allocated only after the same-transaction census and
    // admission. Canonicalization sorts one collection at a time; its validation
    // trees are dropped before snapshot indexes and writer registry measurement
    // begin. These temporary stages therefore share their largest workspace.
    // Gap and diagnostic JSON decodes to closed record shapes too. Their text
    // and evidence vectors remain charged through encoded text, while the
    // largest single-payload decoder has its own transient allowance. Other
    // opaque payloads and other child-row maps retain the four-KiB envelope.
    // Flag groups reserve six closed enum slots per owner. Their headers remain
    // resident while each owned vector moves into its final EntityRecord.
    // Evidence groups retain fixed typed headers and exact reference vectors;
    // each vector moves into its record while the empty group header remains.
    // Fixed-ID groups retain their reserved headers while exact item buffers move
    // into occurrences and provenance. Source groups use closed role keys and
    // retain both owned input and evidence vectors. Borrowed decoding and
    // in-place reversal need one item slot; the spare typed header covers the current owner and
    // the in-place header reversal after a primary-key reverse scan.
    // Symbol claims have closed outer records and sequential bounded decoders;
    // their opaque text retains the unchanged text and largest-payload charges.
    let id_set = tree_node_bytes::<FactId, ()>()?;
    let file_validation = tree_node_bytes::<FileId, &FileRecord>()?
        .checked_add(tree_node_bytes::<FileId, FileId>()?)
        .and_then(|bytes| bytes.checked_add(tree_node_bytes::<FileId, u8>().ok()?))
        .ok_or_else(memory_plan_overflow)?;
    let entity_validation = tree_node_bytes::<SymbolId, &EntityRecord>()?
        .checked_add(tree_node_bytes::<SymbolId, Option<SymbolId>>()?)
        .and_then(|bytes| bytes.checked_add(tree_node_bytes::<SymbolId, u8>().ok()?))
        .and_then(|bytes| {
            bytes.checked_add(u64::try_from(2 * std::mem::size_of::<SymbolId>()).ok()?)
        })
        .ok_or_else(memory_plan_overflow)?;
    let provenance_validation = id_set
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(tree_node_bytes::<FactId, &ProvenanceRecord>().ok()?))
        .ok_or_else(memory_plan_overflow)?;
    let record_buffers = [
        typed_record_buffer_bytes::<FileRecord>(stats.files())?,
        typed_record_buffer_bytes::<EntityRecord>(stats.entities())?,
        typed_record_buffer_bytes::<OccurrenceRecord>(stats.occurrences())?,
        typed_record_buffer_bytes::<RelationRecord>(stats.relations())?,
        typed_record_buffer_bytes::<ProvenanceRecord>(stats.provenance())?,
        typed_record_buffer_bytes::<CoverageRecord>(stats.coverage())?,
        typed_record_buffer_bytes::<SkippedRegion>(stats.skipped_regions())?,
        typed_record_buffer_bytes::<DiagnosticRecord>(stats.diagnostics())?,
        typed_record_buffer_bytes::<ExtensionEnvelope>(symbol_envelopes)?,
    ];
    let retained_record_bytes = record_buffers
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .ok_or_else(memory_plan_overflow)?;
    let sorting_workspace = record_buffers.into_iter().max().unwrap_or(0);
    let validation_workspace = [
        (stats.files(), file_validation),
        (stats.entities(), entity_validation),
        (
            stats.occurrences(),
            id_set.checked_mul(2).ok_or_else(memory_plan_overflow)?,
        ),
        (stats.relations(), id_set),
        (stats.provenance(), provenance_validation),
        (stats.coverage(), id_set),
        (stats.skipped_regions(), id_set),
        (stats.diagnostics(), id_set),
        (symbol_envelopes, id_set),
    ]
    .into_iter()
    .try_fold(0_u64, |total, (rows, bytes)| {
        rows.checked_add(1)
            .and_then(|rows| rows.checked_mul(bytes))
            .and_then(|bytes| total.checked_add(bytes))
    })
    .ok_or_else(memory_plan_overflow)?;
    let registry_workspace = identity_registry_rows
        .checked_add(1)
        .and_then(|rows| rows.checked_mul(tree_node_bytes::<write::RegistryIdentity, ()>().ok()?))
        .ok_or_else(memory_plan_overflow)?;
    let temporary_workspace = sorting_workspace
        .max(validation_workspace)
        .max(registry_workspace);

    // The dense read lookup owns SourceRef values. Writer measurement borrows
    // every input reference, including duplicates, and sorts/deduplicates those
    // pointer slots; it never builds a second owned source map. Physical child
    // rows bound provenance lists, while direct typed source fields add at most
    // the listed extra references beyond one evidence source per logical row.
    let source_input_slots = logical_rows
        .checked_add(stats.occurrences())
        .and_then(|rows| rows.checked_add(stats.source_mappings().checked_mul(2)?))
        .and_then(|rows| rows.checked_add(stats.skipped_regions()))
        .and_then(|rows| rows.checked_add(stats.diagnostics()))
        .and_then(|rows| rows.checked_add(opaque_and_auxiliary_rows))
        .ok_or_else(memory_plan_overflow)?;
    let source_buffers = stats
        .source_refs()
        .checked_mul(
            u64::try_from(std::mem::size_of::<SourceRef>()).map_err(|_| memory_plan_overflow())?,
        )
        .and_then(|bytes| {
            bytes.checked_add(
                source_input_slots
                    .checked_mul(u64::try_from(std::mem::size_of::<&SourceRef>()).ok()?)?,
            )
        })
        .ok_or_else(memory_plan_overflow)?;
    let record_workspace_bytes = opaque_and_other_auxiliary_rows
        .checked_mul(4 * 1024)
        .and_then(|bytes| bytes.checked_add(flag_workspace))
        .and_then(|bytes| bytes.checked_add(evidence_workspace))
        .and_then(|bytes| bytes.checked_add(candidate_workspace))
        .and_then(|bytes| bytes.checked_add(derivation_workspace))
        .and_then(|bytes| bytes.checked_add(provenance_source_workspace))
        .and_then(|bytes| bytes.checked_add(retained_record_bytes))
        .and_then(|bytes| bytes.checked_add(temporary_workspace))
        .and_then(|bytes| bytes.checked_add(source_buffers))
        .ok_or_else(memory_plan_overflow)?;

    // Malformed adjacent-tag JSON can buffer owned content before typed decode
    // rejects it. Four-slot vector growth and owned value/tag storage are charged
    // for the largest single payload, including locator String slots. Payloads
    // are decoded sequentially; retained valid data needs a separate text charge.
    let transient_factor = std::mem::size_of::<String>()
        .checked_add(std::mem::size_of::<usize>())
        .and_then(|bytes| bytes.checked_mul(4))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(memory_plan_overflow)?;
    let transient_decode_bytes = maximum_payload_bytes
        .checked_mul(transient_factor)
        .ok_or_else(memory_plan_overflow)?;
    let materialization_bytes = record_workspace_bytes
        .checked_add(transient_decode_bytes)
        .ok_or_else(memory_plan_overflow)?;
    Ok(OracleReadMemoryPlan {
        stats,
        retained_text_bytes: stats.text_bytes(),
        physical_rows,
        identity_registry_rows,
        record_workspace_bytes,
        maximum_payload_bytes,
        transient_decode_bytes,
        materialization_bytes,
    })
}

pub(crate) fn tree_node_bytes<K, V>() -> Result<u64, CatalogError> {
    const SLOTS: usize = 11;
    std::mem::size_of::<K>()
        .checked_add(std::mem::size_of::<V>())
        .and_then(|bytes| bytes.checked_mul(SLOTS))
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<usize>().checked_mul(16)?))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(memory_plan_overflow)
}

fn typed_record_buffer_bytes<R>(rows: u64) -> Result<u64, CatalogError> {
    let record_bytes =
        u64::try_from(std::mem::size_of::<R>()).map_err(|_| memory_plan_overflow())?;
    rows.checked_add(1)
        .and_then(|rows| rows.checked_mul(record_bytes))
        .ok_or_else(memory_plan_overflow)
}

fn evidence_owner_count(stats: GenerationStats) -> Result<u64, CatalogError> {
    stats
        .files()
        .checked_add(stats.entities())
        .and_then(|rows| rows.checked_add(stats.occurrences()))
        .and_then(|rows| rows.checked_add(stats.relations()))
        .and_then(|rows| rows.checked_add(stats.coverage()))
        .ok_or_else(memory_plan_overflow)
}

fn evidence_workspace_bytes(rows: u64, owners: u64) -> Result<u64, CatalogError> {
    let limit = usize_to_u64(IrLimits::default().max_nested_items_per_record)?;
    if rows > owners.checked_mul(limit).ok_or_else(memory_plan_overflow)? {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    typed_record_buffer_bytes::<EvidenceGroup>(rows.min(owners))?
        .checked_add(
            rows.checked_mul(
                u64::try_from(std::mem::size_of::<FactRef>())
                    .map_err(|_| memory_plan_overflow())?,
            )
            .ok_or_else(memory_plan_overflow)?,
        )
        .and_then(|bytes| {
            bytes.checked_add(u64::try_from(std::mem::size_of::<EvidenceGroups>()).ok()?)
        })
        .ok_or_else(memory_plan_overflow)
}

fn fact_group_workspace_bytes<T>(rows: u64, owners: u64) -> Result<u64, CatalogError> {
    positioned_group_workspace_bytes::<T, FactId>(rows, owners)
}

fn provenance_source_workspace_bytes(rows: u64, owners: u64) -> Result<u64, CatalogError> {
    positioned_group_workspace_bytes::<SourceRef, (FactId, ProvenanceSourceKind)>(
        rows,
        owners.checked_mul(2).ok_or_else(memory_plan_overflow)?,
    )
}

fn positioned_group_workspace_bytes<T, K>(rows: u64, owners: u64) -> Result<u64, CatalogError> {
    let limit = usize_to_u64(IrLimits::default().max_nested_items_per_record)?;
    if rows > owners.checked_mul(limit).ok_or_else(memory_plan_overflow)? {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    typed_record_buffer_bytes::<FactGroup<T, K>>(rows.min(owners))?
        .checked_add(
            rows.checked_mul(
                u64::try_from(std::mem::size_of::<T>()).map_err(|_| memory_plan_overflow())?,
            )
            .ok_or_else(memory_plan_overflow)?,
        )
        .and_then(|bytes| {
            bytes.checked_add(u64::try_from(std::mem::size_of::<FactGroups<T, K>>()).ok()?)
        })
        .and_then(|bytes| bytes.checked_add(u64::try_from(std::mem::size_of::<T>()).ok()?))
        .ok_or_else(memory_plan_overflow)
}

const ENTITY_FLAG_SLOTS: usize = 6;

fn flag_workspace_bytes(flag_rows: u64, entities: u64) -> Result<u64, CatalogError> {
    if flag_rows > entities.checked_mul(6).ok_or_else(memory_plan_overflow)? {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let groups = flag_rows.min(entities);
    typed_record_buffer_bytes::<FlagGroup>(groups)?
        .checked_add(
            groups
                .checked_mul(
                    u64::try_from(ENTITY_FLAG_SLOTS * std::mem::size_of::<EntityFlag>())
                        .map_err(|_| memory_plan_overflow())?,
                )
                .ok_or_else(memory_plan_overflow)?,
        )
        .ok_or_else(memory_plan_overflow)
}

fn exact_record_buffer<R>(rows: u64) -> Result<Vec<R>, CatalogError> {
    let mut records = Vec::new();
    records
        .try_reserve_exact(usize::try_from(rows).map_err(|_| memory_plan_overflow())?)
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    Ok(records)
}

fn check_record_push(records: usize, expected_rows: u64) -> Result<(), CatalogError> {
    if usize_to_u64(records)? >= expected_rows {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn validate_source_reference_layout(
    connection: &Connection,
    expected_rows: u64,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let (rows, invalid): (i64, i64) = connection
        .query_row(
            "SELECT count(*), coalesce(sum(CASE WHEN
             typeof(ordinal) != 'integer' OR ordinal < 0
             OR typeof(repository_id) != 'blob' OR length(repository_id) != 16
             OR typeof(generation_id) != 'blob' OR length(generation_id) != 20
             OR typeof(file_id) != 'blob' OR length(file_id) != 20
             OR typeof(content_hash) != 'blob' OR length(content_hash) != 32
             OR typeof(start_byte) != 'integer' OR start_byte < 0
             OR typeof(end_byte) != 'integer' OR end_byte < start_byte
             OR typeof(line_start) != 'integer' OR line_start < 0
             OR typeof(line_end) != 'integer' OR line_end < line_start
             THEN 1 ELSE 0 END), 0)
         FROM (SELECT * FROM source_refs LIMIT ?1)",
            [codec::sqlite_i64(
                expected_rows
                    .checked_add(1)
                    .ok_or_else(memory_plan_overflow)?,
            )?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(CatalogError::sqlite)?;
    context.check().map_err(CatalogError::control)?;
    if codec::nonnegative_u64(rows)? != expected_rows || invalid != 0 {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn memory_plan_overflow() -> CatalogError {
    CatalogError::new(CatalogErrorKind::Storage)
}

fn validate_identity_registry_layout(
    connection: &Connection,
    expected_rows: u64,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let limit = expected_rows
        .checked_add(1)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    // The discounted keys must be authenticated in this transaction before
    // late registry validation can materialize SQL strings or byte vectors.
    let (rows, invalid): (i64, i64) = connection
        .query_row(
            "SELECT count(*), coalesce(sum(CASE WHEN
                 typeof(kind) != 'text'
                 OR kind NOT IN ('repository', 'file', 'entity', 'fact')
                 OR typeof(identity) != 'blob'
                 OR length(identity) != CASE WHEN kind = 'repository' THEN 16 ELSE 20 END
                 THEN 1 ELSE 0 END), 0)
             FROM (SELECT kind, identity FROM identity_registry LIMIT ?1)",
            [codec::sqlite_i64(limit)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(CatalogError::sqlite)?;
    context.check().map_err(CatalogError::control)?;
    let rows = codec::nonnegative_u64(rows)?;
    context
        .require(GenerationResource::Rows, rows)
        .map_err(CatalogError::control)?;
    if rows != expected_rows || codec::nonnegative_u64(invalid)? != 0 {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn maximum_payload_bytes(
    connection: &Connection,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<u64, CatalogError> {
    let queries = [
        (
            "SELECT count(*), coalesce(max(length(CAST(path_locator_components AS BLOB))), 0)
             FROM (SELECT path_locator_components FROM files LIMIT ?1)",
            stats.files(),
        ),
        (
            "SELECT count(*), coalesce(max(length(CAST(payload AS BLOB))), 0)
             FROM (SELECT payload FROM source_mappings LIMIT ?1)",
            stats.source_mappings(),
        ),
        (
            "SELECT count(*), coalesce(max(length(CAST(payload AS BLOB))), 0)
             FROM (SELECT payload FROM skipped_regions LIMIT ?1)",
            stats.skipped_regions(),
        ),
        (
            "SELECT count(*), coalesce(max(length(CAST(payload AS BLOB))), 0)
             FROM (SELECT payload FROM diagnostics LIMIT ?1)",
            stats.diagnostics(),
        ),
        (
            "SELECT count(*), coalesce(max(length(CAST(payload AS BLOB))), 0)
             FROM (SELECT payload FROM extensions LIMIT ?1)",
            stats.extensions(),
        ),
    ];
    let mut maximum = 0_u64;
    let mut observed_rows = 0_u64;
    for (sql, expected_rows) in queries {
        context.check().map_err(CatalogError::control)?;
        let limit = expected_rows
            .checked_add(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        let (rows, bytes): (i64, i64) = connection
            .query_row(sql, [codec::sqlite_i64(limit)?], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(CatalogError::sqlite)?;
        context.check().map_err(CatalogError::control)?;
        let rows = codec::nonnegative_u64(rows)?;
        let bytes = codec::nonnegative_u64(bytes)?;
        if rows != expected_rows || bytes > stats.text_bytes() {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        observed_rows = observed_rows
            .checked_add(rows)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::Rows, observed_rows)
            .map_err(CatalogError::control)?;
        maximum = maximum.max(bytes);
        context
            .require(GenerationResource::EncodedTextBytes, maximum)
            .map_err(CatalogError::control)?;
    }
    Ok(maximum)
}

pub(crate) fn skipped_metadata_bytes(
    connection: &Connection,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<u64, CatalogError> {
    let mut statement = connection
        .prepare("SELECT payload FROM skipped_regions ORDER BY skipped_region_id LIMIT ?1")
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement
        .query([codec::sqlite_i64(
            stats
                .skipped_regions()
                .checked_add(1)
                .ok_or_else(memory_plan_overflow)?,
        )?])
        .map_err(CatalogError::sqlite)?;
    let mut observed = 0_u64;
    let mut metadata_bytes = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        observed = observed.checked_add(1).ok_or_else(memory_plan_overflow)?;
        if observed > stats.skipped_regions() {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        let raw = row.get_ref(0).map_err(CatalogError::sqlite)?;
        if let Ok(payload) = raw.as_str() {
            metadata_bytes = metadata_bytes
                .checked_add(canonical_skipped_metadata_bytes(payload, context)?)
                .ok_or_else(memory_plan_overflow)?;
        }
    }
    if observed != stats.skipped_regions() || metadata_bytes > stats.text_bytes() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    context.check().map_err(CatalogError::control)?;
    Ok(metadata_bytes)
}

fn canonical_skipped_metadata_bytes(
    payload: &str,
    context: &GenerationContext<'_>,
) -> Result<u64, CatalogError> {
    const DETAIL: &[u8] = b",\"detail\":\"";
    let bytes = payload.as_bytes();
    if !bytes.starts_with(b"{\"id\":\"") || !bytes.ends_with(b",\"derivation\":[]}}") {
        return Ok(0);
    }
    // This shape recognizer credits the top-level detail/empty-derivation envelope.
    // Strict decoding remains authoritative. Valid closed records
    // own detail alone when derivation is empty; strict decode still rejects
    // malformed rows before retention. Its unchanged largest-payload allowance
    // covers that one row. This scan borrows bytes and allocates no decoder state.
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut detail_start = None;
    let mut detail_bytes = None;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if index % write::JSON_CHECKPOINT_BYTES == 0 {
            context.check().map_err(CatalogError::control)?;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
                if let Some(start) = detail_start
                    && index >= start
                {
                    if !bytes[index + 1..].starts_with(b",\"provenance\":\"") {
                        return Ok(0);
                    }
                    detail_bytes = Some(index - start);
                    detail_start = None;
                }
            }
            continue;
        }
        if depth == 1 && bytes[index..].starts_with(DETAIL) {
            if detail_start.is_some() || detail_bytes.is_some() {
                return Ok(0);
            }
            detail_start = Some(index + DETAIL.len());
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth = depth.checked_add(1).ok_or_else(memory_plan_overflow)?;
                if depth > 128 {
                    return Ok(0);
                }
            }
            b'}' | b']' => {
                let Some(next) = depth.checked_sub(1) else {
                    return Ok(0);
                };
                depth = next;
                if depth == 0 && index + 1 != bytes.len() {
                    return Ok(0);
                }
            }
            _ => {}
        }
    }
    if quoted || depth != 0 || detail_start.is_some() {
        return Ok(0);
    }
    let Some(detail_bytes) = detail_bytes else {
        return Ok(0);
    };
    u64::try_from(bytes.len() - detail_bytes).map_err(|_| memory_plan_overflow())
}

fn take_json_literal(input: &mut &[u8], literal: &[u8]) -> Option<()> {
    *input = input.strip_prefix(literal)?;
    Some(())
}

fn take_json_id<I: std::str::FromStr>(input: &mut &[u8]) -> Option<I> {
    let quoted = input.strip_prefix(b"\"")?;
    let length = quoted.iter().take(64).position(|byte| *byte == b'"')?;
    let id = std::str::from_utf8(quoted.get(..length)?)
        .ok()?
        .parse()
        .ok()?;
    *input = quoted.get(length + 1..)?;
    Some(id)
}

fn take_json_unsigned(input: &mut &[u8]) -> Option<u64> {
    let length = input
        .iter()
        .take(21)
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let digits = input.get(..length)?;
    if length == 0 || length > 20 || (length > 1 && digits.first() == Some(&b'0')) {
        return None;
    }
    let value = std::str::from_utf8(digits).ok()?.parse().ok()?;
    *input = input.get(length..)?;
    Some(value)
}

fn take_canonical_json_string(
    input: &mut &[u8],
    mut checkpoint: impl FnMut() -> Result<(), CatalogError>,
) -> Result<Option<usize>, CatalogError> {
    let Some(quoted) = input.strip_prefix(b"\"") else {
        return Ok(None);
    };
    let mut index = 0;
    let mut next_checkpoint = 0;
    while let Some(byte) = quoted.get(index) {
        if index >= next_checkpoint {
            checkpoint()?;
            next_checkpoint = index.saturating_add(write::JSON_CHECKPOINT_BYTES);
        }
        match byte {
            b'"' => {
                *input = &quoted[index + 1..];
                return Ok(Some(index));
            }
            b'\\' => {
                index += 1;
                match quoted.get(index) {
                    Some(b'"' | b'\\' | b'b' | b'f' | b'n' | b'r' | b't') => {}
                    Some(b'u') => {
                        let Some(hex) = quoted.get(index + 1..index + 5) else {
                            return Ok(None);
                        };
                        // The writer uses Unicode escapes only for remaining controls.
                        if hex.get(..2) != Some(b"00")
                            || !hex
                                .iter()
                                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                        {
                            return Ok(None);
                        }
                        let Some(value) = std::str::from_utf8(hex)
                            .ok()
                            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                        else {
                            return Ok(None);
                        };
                        if value >= 32 || matches!(value, 8 | 9 | 10 | 12 | 13) {
                            return Ok(None);
                        }
                        index += 4;
                    }
                    _ => return Ok(None),
                }
            }
            0..=31 => return Ok(None),
            _ => {}
        }
        index += 1;
    }
    Ok(None)
}

// A one-item JSON derivation retains at most four vector slots after conversion.
// Keep this child reserve for consumers that charge retained encoded bytes once;
// single-row wire/domain overlap remains in the unchanged decoder allowance.
pub(crate) const SYMBOL_EVIDENCE_CHILD_BYTES: usize = 4 * std::mem::size_of::<FactRef>();

fn canonical_symbol_metadata_bytes(
    payload: &str,
    prefix_length: usize,
    metadata: GenerationMetadata,
    context: &GenerationContext<'_>,
) -> Result<u64, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let Some(mut input) = payload.as_bytes().get(prefix_length..) else {
        return Ok(0);
    };
    let version = rootlight_ir::SYMBOL_IDENTITY_CLAIM_VERSION;
    if take_json_literal(&mut input, b"\"version\":\"").is_none()
        || take_json_literal(&mut input, version.as_bytes()).is_none()
        || take_json_literal(
            &mut input,
            b"\",\"criticality\":\"noncritical\",\"payload\":",
        )
        .is_none()
    {
        return Ok(0);
    }
    let Some(retained_payload) = take_canonical_json_string(&mut input, || {
        context.check().map_err(CatalogError::control)
    })?
    else {
        return Ok(0);
    };
    // Only the three dynamic strings survive this closed outer transport shape.
    // Keep their escaped bytes under the unchanged text multiplier, and keep all
    // typed/evidence and single-payload decoder charges. Strict decoding and full
    // identity verification remain authoritative, including for the inner claim.
    let recognized = (|| {
        take_json_literal(&mut input, b",\"provenance\":")?;
        take_json_id::<FactId>(&mut input)?;
        take_json_literal(&mut input, b",\"evidence\":{\"source\":{\"repository\":")?;
        if take_json_id::<RepositoryId>(&mut input)? != metadata.repository() {
            return None;
        }
        take_json_literal(&mut input, b",\"generation\":")?;
        if take_json_id::<GenerationId>(&mut input)? != metadata.generation() {
            return None;
        }
        take_json_literal(&mut input, b",\"span\":{\"file\":")?;
        take_json_id::<FileId>(&mut input)?;
        take_json_literal(&mut input, b",\"start_byte\":")?;
        let start = take_json_unsigned(&mut input)?;
        take_json_literal(&mut input, b",\"end_byte\":")?;
        if take_json_unsigned(&mut input)? < start {
            return None;
        }
        take_json_literal(&mut input, b"},\"content_hash\":")?;
        take_json_id::<ContentHash>(&mut input)?;
        if input.starts_with(b",\"line_hint\":") {
            take_json_literal(&mut input, b",\"line_hint\":{\"start_line\":")?;
            let start = take_json_unsigned(&mut input)?;
            take_json_literal(&mut input, b",\"end_line\":")?;
            if start == 0 || take_json_unsigned(&mut input)? < start {
                return None;
            }
            take_json_literal(&mut input, b"}")?;
        }
        take_json_literal(
            &mut input,
            b"},\"derivation\":[{\"kind\":\"entity\",\"id\":",
        )?;
        take_json_id::<SymbolId>(&mut input)?;
        take_json_literal(&mut input, b"}]}}")?;
        input.is_empty().then_some(())
    })();
    if recognized.is_none() {
        return Ok(0);
    }
    let retained = retained_payload
        .checked_add(rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE.len())
        .and_then(|bytes| bytes.checked_add(version.len()))
        .and_then(|bytes| bytes.checked_add(SYMBOL_EVIDENCE_CHILD_BYTES))
        .ok_or_else(memory_plan_overflow)?;
    u64::try_from(payload.len().saturating_sub(retained)).map_err(|_| memory_plan_overflow())
}

#[derive(Debug, Default)]
pub(crate) struct SymbolEnvelopeCensus {
    pub(crate) rows: u64,
    pub(crate) metadata_bytes: u64,
}

pub(crate) fn symbol_envelope_census(
    connection: &Connection,
    metadata: GenerationMetadata,
    expected_rows: u64,
    context: &GenerationContext<'_>,
) -> Result<SymbolEnvelopeCensus, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let mut statement = connection
        .prepare("SELECT extension_id, payload FROM extensions ORDER BY extension_id LIMIT ?1")
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement
        .query([codec::sqlite_i64(
            expected_rows
                .checked_add(1)
                .ok_or_else(memory_plan_overflow)?,
        )?])
        .map_err(CatalogError::sqlite)?;
    let mut observed = 0_u64;
    let mut census = SymbolEnvelopeCensus::default();
    let mut prefix = [0_u8; 256];
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        if observed >= expected_rows {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        observed = observed.checked_add(1).ok_or_else(memory_plan_overflow)?;
        let raw_id = row.get_ref(0).map_err(CatalogError::sqlite)?;
        let Some(id) = raw_id
            .as_blob()
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .map(FactId::from_bytes)
        else {
            continue;
        };
        let raw_payload = row.get_ref(1).map_err(CatalogError::sqlite)?;
        let Ok(payload) = raw_payload.as_str() else {
            continue;
        };
        // Current writer serialization fixes these first four fields. Every ID
        // is canonical ASCII; a nonmatching representation keeps its coarse charge.
        let mut cursor = std::io::Cursor::new(prefix.as_mut_slice());
        write!(
            cursor,
            "{{\"id\":\"{id}\",\"repository\":\"{}\",\"generation\":\"{}\",\"namespace\":\"{}\",",
            metadata.repository(),
            metadata.generation(),
            rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE,
        )
        .map_err(|_| memory_plan_overflow())?;
        let length = usize::try_from(cursor.position()).map_err(|_| memory_plan_overflow())?;
        if payload.as_bytes().starts_with(&prefix[..length]) {
            census.rows = census
                .rows
                .checked_add(1)
                .ok_or_else(memory_plan_overflow)?;
            census.metadata_bytes = census
                .metadata_bytes
                .checked_add(canonical_symbol_metadata_bytes(
                    payload, length, metadata, context,
                )?)
                .ok_or_else(memory_plan_overflow)?;
        }
    }
    if observed != expected_rows {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    context.check().map_err(CatalogError::control)?;
    Ok(census)
}

fn validate_document_hash(
    connection: &Connection,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let expected: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value
             FROM application_meta
             WHERE key = 'document_hash' AND length(value) = 32",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(CatalogError::sqlite)?;
    let expected = expected
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))
        .and_then(codec::content_hash)?;
    let observed = write::canonical_document_hash(document, context, CatalogErrorKind::Corrupt)?;
    if observed != expected {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PayloadCardinality {
    pub(crate) occurrence_candidate_rows: u64,
    pub(crate) provenance_derivation_rows: u64,
    pub(crate) provenance_source_rows: u64,
    pub(crate) entity_flag_rows: u64,
    pub(crate) evidence_rows: u64,
}

pub(crate) fn validate_payload_cardinality(
    connection: &Connection,
    contract_version: GenerationContractVersion,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<PayloadCardinality, CatalogError> {
    const CANDIDATE_COUNT_SQL: &str = "SELECT count(*) FROM (
                SELECT 1 FROM occurrence_candidates LIMIT ?1
             )";
    const DERIVATION_COUNT_SQL: &str = "SELECT count(*) FROM (
                SELECT 1 FROM provenance_derivations LIMIT ?1
             )";
    const SOURCE_COUNT_SQL: &str = "SELECT count(*) FROM (
                SELECT 1 FROM provenance_sources LIMIT ?1
             )";
    const EVIDENCE_COUNT_SQL: &str = "SELECT count(*) FROM (
                SELECT 1 FROM evidence_derivations LIMIT ?1
             )";
    const FLAG_COUNT_SQL: &str = "SELECT count(*) FROM (
                SELECT 1 FROM entity_flags LIMIT ?1
             )";
    let identity_rows = [
        1,
        stats.files(),
        stats.entities(),
        stats.occurrences(),
        stats.relations(),
        stats.provenance(),
        stats.source_mappings(),
        stats.coverage(),
        stats.skipped_regions(),
        stats.diagnostics(),
        stats.extensions(),
    ]
    .into_iter()
    .try_fold(0_u64, u64::checked_add)
    .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let tables = [
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM generation_meta LIMIT ?1
             )",
            Some(1),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM identity_registry LIMIT ?1
             )",
            Some(identity_rows),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM source_refs LIMIT ?1
             )",
            Some(stats.source_refs()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM provenance LIMIT ?1
             )",
            Some(stats.provenance()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM files LIMIT ?1
             )",
            Some(stats.files()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM entities LIMIT ?1
             )",
            Some(stats.entities()),
        ),
        (FLAG_COUNT_SQL, None),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM occurrences LIMIT ?1
             )",
            Some(stats.occurrences()),
        ),
        (CANDIDATE_COUNT_SQL, None),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM relations LIMIT ?1
             )",
            Some(stats.relations()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM source_mappings LIMIT ?1
             )",
            Some(stats.source_mappings()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM coverage_records LIMIT ?1
             )",
            Some(stats.coverage()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM skipped_regions LIMIT ?1
             )",
            Some(stats.skipped_regions()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM diagnostics LIMIT ?1
             )",
            Some(stats.diagnostics()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM extensions LIMIT ?1
             )",
            Some(stats.extensions()),
        ),
        (EVIDENCE_COUNT_SQL, None),
        (SOURCE_COUNT_SQL, None),
        (DERIVATION_COUNT_SQL, None),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM application_meta
                WHERE key = 'document_hash'
                LIMIT ?1
             )",
            Some(1),
        ),
    ];
    let physical_row_limit = if contract_version == GenerationContractVersion::new(1, 1) {
        stats.stored_rows().checked_add(1)
    } else {
        Some(stats.stored_rows())
    }
    .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let mut observed_total = 0_u64;
    let mut entity_flag_rows = 0_u64;
    let mut evidence_rows = 0_u64;
    let mut occurrence_candidate_rows = 0_u64;
    let mut provenance_derivation_rows = 0_u64;
    let mut provenance_source_rows = 0_u64;
    for (sql, expected) in tables {
        let remaining = physical_row_limit
            .checked_sub(observed_total)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        let maximum = match expected {
            Some(expected) if expected > remaining => {
                return Err(CatalogError::new(CatalogErrorKind::Corrupt));
            }
            Some(expected) => expected,
            None => remaining,
        };
        let count = bounded_table_count(connection, sql, maximum, context)?;
        if sql == FLAG_COUNT_SQL {
            entity_flag_rows = count;
        }
        if sql == EVIDENCE_COUNT_SQL {
            evidence_rows = count;
        }
        if sql == CANDIDATE_COUNT_SQL {
            occurrence_candidate_rows = count;
        }
        if sql == DERIVATION_COUNT_SQL {
            provenance_derivation_rows = count;
        }
        if sql == SOURCE_COUNT_SQL {
            provenance_source_rows = count;
        }
        if expected.is_some_and(|expected| expected != count) {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        observed_total = observed_total
            .checked_add(count)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    }
    if observed_total != physical_row_limit {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    flag_workspace_bytes(entity_flag_rows, stats.entities())?;
    Ok(PayloadCardinality {
        occurrence_candidate_rows,
        provenance_derivation_rows,
        provenance_source_rows,
        entity_flag_rows,
        evidence_rows,
    })
}

fn bounded_table_count(
    connection: &Connection,
    sql: &str,
    maximum: u64,
    context: &GenerationContext<'_>,
) -> Result<u64, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let limit = maximum
        .checked_add(1)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let observed: i64 = connection
        .query_row(sql, [codec::sqlite_i64(limit)?], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    context.check().map_err(CatalogError::control)?;
    let observed = codec::nonnegative_u64(observed)?;
    if observed > maximum {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(observed)
}

fn validate_identity_registry(
    connection: &Connection,
    expected: &std::collections::BTreeSet<write::RegistryIdentity>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT kind, identity
             FROM identity_registry
             ORDER BY CASE kind
                 WHEN 'repository' THEN 0
                 WHEN 'file' THEN 1
                 WHEN 'entity' THEN 2
                 WHEN 'fact' THEN 3
                 ELSE 4
             END, identity",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut expected = expected.iter();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let Some(expected_identity) = expected.next() else {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        };
        let kind: String = get(row, 0)?;
        let identity: Vec<u8> = get(row, 1)?;
        if kind != expected_identity.kind() || identity != expected_identity.bytes() {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
    }
    if expected.next().is_some() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn validate_source_registry(
    observed: &[SourceRef],
    expected: &[&SourceRef],
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    if observed.len() != expected.len() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    for (observed, expected) in observed.iter().zip(expected) {
        context.check().map_err(CatalogError::control)?;
        if observed != *expected {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
    }
    Ok(())
}

fn read_sources(
    connection: &Connection,
    expected_count: u64,
    context: &GenerationContext<'_>,
) -> Result<Vec<SourceRef>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                ordinal, repository_id, generation_id, file_id, start_byte,
                end_byte, content_hash, line_start, line_end
             FROM source_refs
             ORDER BY ordinal",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    // Ordinals are authenticated as dense before indexing this exact buffer.
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(usize::try_from(expected_count).map_err(|_| memory_plan_overflow())?)
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let ordinal: i64 = get(row, 0)?;
        if ordinal != usize_to_i64(sources.len())? {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        let source = codec::source_ref(
            get(row, 1)?,
            get(row, 2)?,
            get(row, 3)?,
            get(row, 4)?,
            get(row, 5)?,
            get(row, 6)?,
            get(row, 7)?,
            get(row, 8)?,
        )?;
        if usize_to_u64(sources.len())? >= expected_count {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        sources.push(source);
    }
    if usize_to_u64(sources.len())? != expected_count {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(sources)
}

fn read_evidence(
    connection: &Connection,
    expected_rows: u64,
    expected_owners: u64,
    context: &GenerationContext<'_>,
) -> Result<EvidenceGroups, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let nested_limit = usize_to_u64(IrLimits::default().max_nested_items_per_record)?;
    if expected_rows
        > expected_owners
            .checked_mul(nested_limit)
            .ok_or_else(memory_plan_overflow)?
    {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let maximum_groups = expected_rows.min(expected_owners);
    let mut groups: Vec<EvidenceGroup> = exact_record_buffer(maximum_groups)?;
    let mut statement = connection
        .prepare(
            "SELECT owner_kind, owner_id, position, reference_kind, reference_id
             FROM evidence_derivations
             ORDER BY owner_kind DESC, owner_id DESC, position DESC",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut observed_rows = 0_u64;
    let mut group_remaining = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let remaining_rows = expected_rows
            .checked_sub(observed_rows)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        let owner = read_evidence_reference(row, 0, 1)?;
        let reference = read_evidence_reference(row, 3, 4)?;
        let position = row.get_ref(2).map_err(CatalogError::sqlite)?;
        let position = position
            .as_i64()
            .ok()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        match groups
            .last()
            .map(|group| evidence_key(&group.owner).cmp(&evidence_key(&owner)))
        {
            Some(std::cmp::Ordering::Less) => {
                return Err(CatalogError::new(CatalogErrorKind::Corrupt));
            }
            Some(std::cmp::Ordering::Equal) => {}
            Some(std::cmp::Ordering::Greater) | None => {
                // Finish the prior group before accepting a new claimed allocation.
                if group_remaining != 0 {
                    return Err(CatalogError::new(CatalogErrorKind::Corrupt));
                }
                let claimed = position.checked_add(1).ok_or_else(memory_plan_overflow)?;
                if claimed > nested_limit || claimed > remaining_rows {
                    return Err(CatalogError::new(CatalogErrorKind::Corrupt));
                }
                check_record_push(groups.len(), maximum_groups)?;
                groups.push(EvidenceGroup {
                    owner,
                    items: exact_record_buffer(claimed)?,
                });
                group_remaining = claimed;
            }
        }
        let expected_position = group_remaining
            .checked_sub(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        if position != expected_position {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        groups
            .last_mut()
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?
            .items
            .push(reference);
        group_remaining = expected_position;
        observed_rows = observed_rows
            .checked_add(1)
            .ok_or_else(memory_plan_overflow)?;
    }
    if observed_rows != expected_rows || group_remaining != 0 {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    for group in &mut groups {
        context.check().map_err(CatalogError::control)?;
        group.items.reverse();
    }
    context.check().map_err(CatalogError::control)?;
    let remaining = groups.len();
    Ok(EvidenceGroups { groups, remaining })
}

#[derive(Debug)]
struct FlagGroup {
    entity: SymbolId,
    flags: Vec<EntityFlag>,
}

#[derive(Debug)]
struct FlagGroups {
    groups: Vec<FlagGroup>,
    next: usize,
}

impl FlagGroups {
    fn is_empty(&self) -> bool {
        self.next == self.groups.len()
    }

    fn take(&mut self, entity: SymbolId) -> Result<Vec<EntityFlag>, CatalogError> {
        let Some(group) = self.groups.get_mut(self.next) else {
            return Ok(Vec::new());
        };
        match group.entity.cmp(&entity) {
            std::cmp::Ordering::Less => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
            std::cmp::Ordering::Greater => Ok(Vec::new()),
            std::cmp::Ordering::Equal => {
                self.next = self.next.checked_add(1).ok_or_else(memory_plan_overflow)?;
                Ok(std::mem::take(&mut group.flags))
            }
        }
    }
}

fn read_flags(
    connection: &Connection,
    expected_rows: u64,
    expected_entities: u64,
    context: &GenerationContext<'_>,
) -> Result<FlagGroups, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    flag_workspace_bytes(expected_rows, expected_entities)?;
    let maximum_groups = expected_rows.min(expected_entities);
    let mut values: Vec<FlagGroup> = exact_record_buffer(maximum_groups)?;
    let mut statement = connection
        .prepare("SELECT entity_id, flag FROM entity_flags ORDER BY entity_id, flag")
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut observed_rows = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        if observed_rows >= expected_rows {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        observed_rows = observed_rows
            .checked_add(1)
            .ok_or_else(memory_plan_overflow)?;
        let raw_id = row.get_ref(0).map_err(CatalogError::sqlite)?;
        let entity = SymbolId::from_bytes(
            raw_id
                .as_blob()
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?,
        );
        let raw_flag = row.get_ref(1).map_err(CatalogError::sqlite)?;
        let flag = match raw_flag.as_str() {
            Ok("generated") => EntityFlag::Generated,
            Ok("external") => EntityFlag::External,
            Ok("test") => EntityFlag::Test,
            Ok("exported") => EntityFlag::Exported,
            Ok("deprecated") => EntityFlag::Deprecated,
            Ok("synthetic") => EntityFlag::Synthetic,
            _ => return Err(CatalogError::new(CatalogErrorKind::Corrupt)),
        };
        match values.last().map(|group| group.entity.cmp(&entity)) {
            Some(std::cmp::Ordering::Greater) => {
                return Err(CatalogError::new(CatalogErrorKind::Corrupt));
            }
            Some(std::cmp::Ordering::Equal) => {}
            Some(std::cmp::Ordering::Less) | None => {
                check_record_push(values.len(), maximum_groups)?;
                values.push(FlagGroup {
                    entity,
                    flags: exact_record_buffer(
                        u64::try_from(ENTITY_FLAG_SLOTS).map_err(|_| memory_plan_overflow())?,
                    )?,
                });
            }
        }
        let group = values
            .last_mut()
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        if group.flags.len() >= ENTITY_FLAG_SLOTS || group.flags.contains(&flag) {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        group.flags.push(flag);
    }
    if observed_rows != expected_rows {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    for group in &mut values {
        context.check().map_err(CatalogError::control)?;
        group.flags.sort_unstable();
    }
    context.check().map_err(CatalogError::control)?;
    Ok(FlagGroups {
        groups: values,
        next: 0,
    })
}

const CANDIDATE_GROUPS_SQL: &str =
    "SELECT occurrence_id, position, entity_id FROM occurrence_candidates
    ORDER BY occurrence_id DESC, position DESC";
const PROVENANCE_GROUPS_SQL: &str = "SELECT provenance_id, position, reference_kind, reference_id
    FROM provenance_derivations ORDER BY provenance_id DESC, position DESC";

#[derive(Debug)]
struct FactGroup<T, K = FactId> {
    owner: K,
    items: Vec<T>,
}

#[derive(Debug)]
struct FactGroups<T, K = FactId> {
    groups: Vec<FactGroup<T, K>>,
    next: usize,
}

impl<T, K: Ord> FactGroups<T, K> {
    fn is_empty(&self) -> bool {
        self.next == self.groups.len()
    }

    fn take(&mut self, owner: K) -> Result<Vec<T>, CatalogError> {
        let Some(group) = self.groups.get_mut(self.next) else {
            return Ok(Vec::new());
        };
        match group.owner.cmp(&owner) {
            std::cmp::Ordering::Less => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
            std::cmp::Ordering::Greater => Ok(Vec::new()),
            std::cmp::Ordering::Equal => {
                self.next = self.next.checked_add(1).ok_or_else(memory_plan_overflow)?;
                Ok(std::mem::take(&mut group.items))
            }
        }
    }
}

fn borrowed_id(row: &Row<'_>, column: usize) -> Result<[u8; 20], CatalogError> {
    row.get_ref(column)
        .map_err(CatalogError::sqlite)?
        .as_blob()
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))
}

fn read_fact_groups<T>(
    connection: &Connection,
    sql: &str,
    expected_rows: u64,
    expected_owners: u64,
    context: &GenerationContext<'_>,
    read_item: impl Fn(&Row<'_>) -> Result<T, CatalogError>,
) -> Result<FactGroups<T>, CatalogError> {
    read_positioned_groups(
        connection,
        sql,
        expected_rows,
        expected_owners,
        context,
        |row| Ok((FactId::from_bytes(borrowed_id(row, 0)?), 1)),
        read_item,
    )
}

fn read_positioned_groups<T, K: Copy + Ord>(
    connection: &Connection,
    sql: &str,
    expected_rows: u64,
    expected_owners: u64,
    context: &GenerationContext<'_>,
    read_owner: impl Fn(&Row<'_>) -> Result<(K, usize), CatalogError>,
    read_item: impl Fn(&Row<'_>) -> Result<T, CatalogError>,
) -> Result<FactGroups<T, K>, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let nested_limit = usize_to_u64(IrLimits::default().max_nested_items_per_record)?;
    if expected_rows
        > expected_owners
            .checked_mul(nested_limit)
            .ok_or_else(memory_plan_overflow)?
    {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let maximum_groups = expected_rows.min(expected_owners);
    let mut groups: Vec<FactGroup<T, K>> = exact_record_buffer(maximum_groups)?;
    let mut statement = connection.prepare(sql).map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut observed_rows = 0_u64;
    let mut group_remaining = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let remaining_rows = expected_rows
            .checked_sub(observed_rows)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        let (owner, position_column) = read_owner(row)?;
        let item = read_item(row)?;
        let position = row.get_ref(position_column).map_err(CatalogError::sqlite)?;
        let position = position
            .as_i64()
            .ok()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        match groups.last().map(|group| group.owner.cmp(&owner)) {
            Some(std::cmp::Ordering::Less) => {
                return Err(CatalogError::new(CatalogErrorKind::Corrupt));
            }
            Some(std::cmp::Ordering::Equal) => {}
            Some(std::cmp::Ordering::Greater) | None => {
                if group_remaining != 0 {
                    return Err(CatalogError::new(CatalogErrorKind::Corrupt));
                }
                // Descending positions bound this group before its buffer is allocated.
                let claimed = position.checked_add(1).ok_or_else(memory_plan_overflow)?;
                if claimed > nested_limit || claimed > remaining_rows {
                    return Err(CatalogError::new(CatalogErrorKind::Corrupt));
                }
                check_record_push(groups.len(), maximum_groups)?;
                groups.push(FactGroup {
                    owner,
                    items: exact_record_buffer(claimed)?,
                });
                group_remaining = claimed;
            }
        }
        let expected_position = group_remaining
            .checked_sub(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        if position != expected_position {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        groups
            .last_mut()
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?
            .items
            .push(item);
        group_remaining = expected_position;
        observed_rows = observed_rows
            .checked_add(1)
            .ok_or_else(memory_plan_overflow)?;
    }
    if observed_rows != expected_rows || group_remaining != 0 {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    for group in &mut groups {
        context.check().map_err(CatalogError::control)?;
        group.items.reverse();
    }
    // The primary-key reverse scan avoids a SQLite sorter; consumers stay ascending.
    groups.reverse();
    context.check().map_err(CatalogError::control)?;
    Ok(FactGroups { groups, next: 0 })
}

fn read_candidates(
    connection: &Connection,
    expected_rows: u64,
    expected_owners: u64,
    context: &GenerationContext<'_>,
) -> Result<FactGroups<SymbolId>, CatalogError> {
    read_fact_groups(
        connection,
        CANDIDATE_GROUPS_SQL,
        expected_rows,
        expected_owners,
        context,
        |row| Ok(SymbolId::from_bytes(borrowed_id(row, 2)?)),
    )
}

// The enum order matches SQLite's text order for the two persisted source roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProvenanceSourceKind {
    Evidence,
    Input,
}

type ProvenanceSourceGroups = FactGroups<SourceRef, (FactId, ProvenanceSourceKind)>;

const PROVENANCE_SOURCES_SQL: &str =
    "SELECT provenance_id, source_kind, position, source_ordinal FROM provenance_sources
     ORDER BY provenance_id DESC, source_kind DESC, position DESC";

fn read_provenance_sources(
    connection: &Connection,
    sources: &[SourceRef],
    expected_rows: u64,
    expected_owners: u64,
    context: &GenerationContext<'_>,
) -> Result<ProvenanceSourceGroups, CatalogError> {
    read_positioned_groups(
        connection,
        PROVENANCE_SOURCES_SQL,
        expected_rows,
        expected_owners
            .checked_mul(2)
            .ok_or_else(memory_plan_overflow)?,
        context,
        |row| {
            let kind = match row.get_ref(1).map_err(CatalogError::sqlite)? {
                rusqlite::types::ValueRef::Text(b"evidence") => ProvenanceSourceKind::Evidence,
                rusqlite::types::ValueRef::Text(b"input") => ProvenanceSourceKind::Input,
                _ => return Err(CatalogError::new(CatalogErrorKind::Corrupt)),
            };
            Ok(((FactId::from_bytes(borrowed_id(row, 0)?), kind), 2))
        },
        |row| {
            let ordinal = row
                .get_ref(3)
                .map_err(CatalogError::sqlite)?
                .as_i64()
                .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
            codec::source_by_ordinal(sources, ordinal)
        },
    )
}

fn read_provenance_derivations(
    connection: &Connection,
    expected_rows: u64,
    expected_owners: u64,
    context: &GenerationContext<'_>,
) -> Result<FactGroups<FactRef>, CatalogError> {
    read_fact_groups(
        connection,
        PROVENANCE_GROUPS_SQL,
        expected_rows,
        expected_owners,
        context,
        |row| read_evidence_reference(row, 2, 3),
    )
}

fn read_provenance(
    connection: &Connection,
    expected_count: u64,
    sources: &mut ProvenanceSourceGroups,
    derivations: &mut FactGroups<FactRef>,
    context: &GenerationContext<'_>,
) -> Result<Vec<ProvenanceRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                provenance_id, repository_id, generation_id, producer_kind,
                producer_name, producer_version, producer_configuration_hash,
                binary_digest, frontend_version, language, tier,
                build_context_digest, rule
             FROM provenance
             ORDER BY provenance_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        check_record_push(records.len(), expected_count)?;
        let id = codec::fact_id(get(row, 0)?)?;
        let producer = ProducerIdentity::new(
            &get::<String>(row, 4)?,
            &get::<String>(row, 5)?,
            codec::content_hash(get(row, 6)?)?,
        )
        .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
        records.push(ProvenanceRecord {
            id,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            producer_kind: codec::decode_enum::<ProducerKind>(get(row, 3)?)?,
            producer,
            binary_digest: codec::content_hash(get(row, 7)?)?,
            frontend_version: get(row, 8)?,
            language: get(row, 9)?,
            tier: codec::decode_enum::<AnalysisTier>(get(row, 10)?)?,
            build_context: BuildContextIdentity::new(codec::content_hash(get(row, 11)?)?),
            evidence_sources: sources.take((id, ProvenanceSourceKind::Evidence))?,
            input_sources: sources.take((id, ProvenanceSourceKind::Input))?,
            derivation_parents: derivations.take(id)?,
            rule: get(row, 12)?,
        });
    }
    Ok(records)
}

fn read_files(
    connection: &Connection,
    expected_count: u64,
    sources: &[SourceRef],
    evidence: &mut EvidenceGroups,
    context: &GenerationContext<'_>,
) -> Result<Vec<FileRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                file_id, repository_id, generation_id, path, path_locator_encoding,
                path_locator_components, content_hash, byte_length, language, encoding,
                generated, provenance_id, evidence_source_ordinal
             FROM files
             ORDER BY file_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        check_record_push(records.len(), expected_count)?;
        let raw_id: Vec<u8> = get(row, 0)?;
        records.push(FileRecord {
            id: codec::file_id(raw_id.clone())?,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            path: get(row, 3)?,
            path_locator: decode_path_locator(get(row, 4)?, get(row, 5)?)?,
            content_hash: codec::content_hash(get(row, 6)?)?,
            byte_length: codec::nonnegative_u64(get(row, 7)?)?,
            language: get(row, 8)?,
            encoding: get(row, 9)?,
            generated: codec::bool_value(get(row, 10)?)?,
            provenance: codec::fact_id(get(row, 11)?)?,
            evidence: take_evidence("file", raw_id, get(row, 12)?, sources, evidence)?,
        });
    }
    Ok(records)
}

fn decode_path_locator(
    encoding: Option<String>,
    components: Option<String>,
) -> Result<Option<FilePathLocator>, CatalogError> {
    match (encoding, components) {
        (None, None) => Ok(None),
        (Some(encoding), Some(components)) => {
            let encoding = FilePathLocatorEncoding::parse(&encoding)
                .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
            let components = serde_json::from_str(&components).map_err(CatalogError::json)?;
            FilePathLocator::new(encoding, components)
                .map(Some)
                .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
        }
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

fn read_entities(
    connection: &Connection,
    expected_count: u64,
    sources: &[SourceRef],
    evidence: &mut EvidenceGroups,
    flags: &mut FlagGroups,
    context: &GenerationContext<'_>,
) -> Result<Vec<EntityRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                entity_id, repository_id, generation_id, kind, language, tier,
                canonical_name, display_name, qualified_name, container_kind,
                container_id, visibility, provenance_id, evidence_source_ordinal
             FROM entities
             ORDER BY entity_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        check_record_push(records.len(), expected_count)?;
        let raw_id: Vec<u8> = get(row, 0)?;
        let id = codec::symbol_id(raw_id.clone())?;
        records.push(EntityRecord {
            id,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            kind: codec::decode_enum::<EntityKind>(get(row, 3)?)?,
            language: get(row, 4)?,
            tier: codec::decode_enum::<AnalysisTier>(get(row, 5)?)?,
            canonical_name: get(row, 6)?,
            display_name: get(row, 7)?,
            qualified_name: get(row, 8)?,
            container: codec::decode_container(get(row, 9)?, get(row, 10)?)?,
            visibility: codec::decode_enum::<EntityVisibility>(get(row, 11)?)?,
            flags: flags.take(id)?,
            provenance: codec::fact_id(get(row, 12)?)?,
            evidence: take_evidence("entity", raw_id, get(row, 13)?, sources, evidence)?,
        });
    }
    Ok(records)
}

fn read_occurrences(
    connection: &Connection,
    expected_count: u64,
    sources: &[SourceRef],
    evidence: &mut EvidenceGroups,
    candidates: &mut FactGroups<SymbolId>,
    context: &GenerationContext<'_>,
) -> Result<Vec<OccurrenceRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                occurrence_id, repository_id, generation_id, file_id,
                source_ordinal, role, enclosing_entity_id, target_kind,
                target_symbol_id, target_text_hash, target_total_count,
                target_completeness, syntactic_text_hash, syntax_kind,
                provenance_id, confidence, evidence_source_ordinal
             FROM occurrences
             ORDER BY occurrence_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        check_record_push(records.len(), expected_count)?;
        let raw_id: Vec<u8> = get(row, 0)?;
        let id = codec::fact_id(raw_id.clone())?;
        let target_kind: String = get(row, 7)?;
        let target = decode_target(
            &target_kind,
            get(row, 8)?,
            get(row, 9)?,
            get(row, 10)?,
            get(row, 11)?,
            candidates.take(id)?,
        )?;
        records.push(OccurrenceRecord {
            id,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            file: codec::file_id(get(row, 3)?)?,
            source: codec::source_by_ordinal(sources, get(row, 4)?)?,
            role: codec::decode_enum::<OccurrenceRole>(get(row, 5)?)?,
            enclosing: codec::optional_symbol_id(get(row, 6)?)?,
            target,
            syntactic_text_hash: codec::content_hash(get(row, 12)?)?,
            syntax_kind: get(row, 13)?,
            provenance: codec::fact_id(get(row, 14)?)?,
            confidence: codec::confidence(get(row, 15)?)?,
            evidence: take_evidence("fact", raw_id, get(row, 16)?, sources, evidence)?,
        });
    }
    Ok(records)
}

fn decode_target(
    kind: &str,
    symbol: Option<Vec<u8>>,
    hash: Option<Vec<u8>>,
    total_count: Option<i64>,
    completeness: Option<String>,
    candidates: Vec<SymbolId>,
) -> Result<OccurrenceTarget, CatalogError> {
    match (kind, symbol, hash, total_count, completeness) {
        ("resolved", Some(symbol), None, None, None) if candidates.is_empty() => {
            Ok(OccurrenceTarget::Resolved {
                symbol: codec::symbol_id(symbol)?,
            })
        }
        ("candidates", None, None, Some(total_count), Some(completeness)) => {
            Ok(OccurrenceTarget::Candidates {
                symbols: candidates,
                total_count: codec::nonnegative_u64(total_count)?,
                completeness: codec::decode_enum::<CoverageStatus>(completeness)?,
            })
        }
        ("unresolved", None, Some(hash), None, None) if candidates.is_empty() => {
            Ok(OccurrenceTarget::Unresolved {
                text_hash: codec::content_hash(hash)?,
            })
        }
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

fn read_relations(
    connection: &Connection,
    expected_count: u64,
    sources: &[SourceRef],
    evidence: &mut EvidenceGroups,
    context: &GenerationContext<'_>,
) -> Result<Vec<RelationRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                relation_id, repository_id, generation_id, subject_kind,
                subject_id, predicate, object_kind, object_id, confidence,
                evidence_kind, provenance_id, evidence_source_ordinal
             FROM relations
             ORDER BY relation_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        check_record_push(records.len(), expected_count)?;
        let raw_id: Vec<u8> = get(row, 0)?;
        records.push(RelationRecord {
            id: codec::fact_id(raw_id.clone())?,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            subject: codec::decode_endpoint(&get::<String>(row, 3)?, get(row, 4)?)?,
            predicate: codec::decode_enum::<RelationPredicate>(get(row, 5)?)?,
            object: codec::decode_endpoint(&get::<String>(row, 6)?, get(row, 7)?)?,
            confidence: codec::confidence(get(row, 8)?)?,
            evidence_kind: codec::decode_enum::<EvidenceKind>(get(row, 9)?)?,
            provenance: codec::fact_id(get(row, 10)?)?,
            evidence: take_evidence("fact", raw_id, get(row, 11)?, sources, evidence)?,
        });
    }
    Ok(records)
}

fn read_coverage(
    connection: &Connection,
    expected_count: u64,
    sources: &[SourceRef],
    evidence: &mut EvidenceGroups,
    context: &GenerationContext<'_>,
) -> Result<Vec<CoverageRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                coverage_id, repository_id, generation_id, scope_kind, scope_id,
                domain, tier, status, discovered, indexed, skipped,
                provenance_id, evidence_source_ordinal
             FROM coverage_records
             ORDER BY coverage_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        check_record_push(records.len(), expected_count)?;
        let raw_id: Vec<u8> = get(row, 0)?;
        records.push(CoverageRecord {
            id: codec::fact_id(raw_id.clone())?,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            scope: codec::decode_scope(&get::<String>(row, 3)?, get(row, 4)?)?,
            domain: codec::decode_enum::<FactDomain>(get(row, 5)?)?,
            tier: codec::decode_enum::<AnalysisTier>(get(row, 6)?)?,
            status: codec::decode_enum::<CoverageStatus>(get(row, 7)?)?,
            discovered: codec::nonnegative_u64(get(row, 8)?)?,
            indexed: codec::nonnegative_u64(get(row, 9)?)?,
            skipped: codec::nonnegative_u64(get(row, 10)?)?,
            provenance: codec::fact_id(get(row, 11)?)?,
            evidence: take_evidence("fact", raw_id, get(row, 12)?, sources, evidence)?,
        });
    }
    Ok(records)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the complete normalized document is validated as one ownership graph"
)]
fn read_complete_document(
    connection: &Connection,
    metadata: GenerationMetadata,
    ir_version: NormalizedIrVersion,
    stats: GenerationStats,
    files: Vec<FileRecord>,
    entities: Vec<EntityRecord>,
    occurrences: Vec<OccurrenceRecord>,
    relations: Vec<RelationRecord>,
    provenance: Vec<ProvenanceRecord>,
    coverage_records: Vec<CoverageRecord>,
    context: &GenerationContext<'_>,
) -> Result<NormalizedIrDocument, CatalogError> {
    let source_mappings = read_opaque_records(
        connection,
        "SELECT
            source_mapping_id, repository_id, generation_id, provenance_id, payload
         FROM source_mappings
         ORDER BY source_mapping_id",
        stats.source_mappings(),
        context,
        |encoded, context| {
            decode_source_mapping_record_with_checkpoint(encoded, &IrLimits::default(), || {
                context.check().is_ok()
            })
            .map_err(|error| map_record_decode_error(error, context))
        },
    )?;
    let skipped_regions = read_opaque_records(
        connection,
        "SELECT
            skipped_region_id, repository_id, generation_id, provenance_id, payload
         FROM skipped_regions
         ORDER BY skipped_region_id",
        stats.skipped_regions(),
        context,
        |encoded, context| {
            decode_skipped_region_with_checkpoint(encoded, &IrLimits::default(), || {
                context.check().is_ok()
            })
            .map_err(|error| map_record_decode_error(error, context))
        },
    )?;
    let diagnostics = read_opaque_records(
        connection,
        "SELECT
            diagnostic_id, repository_id, generation_id, provenance_id, payload
         FROM diagnostics
         ORDER BY diagnostic_id",
        stats.diagnostics(),
        context,
        |encoded, context| {
            decode_diagnostic_record_with_checkpoint(encoded, &IrLimits::default(), || {
                context.check().is_ok()
            })
            .map_err(|error| map_record_decode_error(error, context))
        },
    )?;
    let extensions = read_opaque_records(
        connection,
        "SELECT
            extension_id, repository_id, generation_id, provenance_id, payload
         FROM extensions
         ORDER BY extension_id",
        stats.extensions(),
        context,
        |encoded, context| {
            decode_extension_envelope_with_checkpoint(encoded, &IrLimits::default(), || {
                context.check().is_ok()
            })
            .map_err(|error| map_extension_decode_error(error, context))
        },
    )?;
    context.check().map_err(CatalogError::control)?;
    let mut document = NormalizedIrDocument::empty(metadata.repository(), metadata.generation());
    document.version = ir_version;
    document.files = files;
    document.entities = entities;
    document.occurrences = occurrences;
    document.relations = relations;
    document.provenance = provenance;
    document.source_mappings = source_mappings;
    document.coverage_records = coverage_records;
    document.skipped_regions = skipped_regions;
    document.diagnostics = diagnostics;
    document.extensions = extensions;
    Ok(document)
}

trait OpaqueRecord {
    fn id(&self) -> FactId;
    fn repository(&self) -> RepositoryId;
    fn generation(&self) -> GenerationId;
    fn provenance(&self) -> FactId;
}

macro_rules! opaque_record {
    ($type:ty) => {
        impl OpaqueRecord for $type {
            fn id(&self) -> FactId {
                self.id
            }

            fn repository(&self) -> RepositoryId {
                self.repository
            }

            fn generation(&self) -> GenerationId {
                self.generation
            }

            fn provenance(&self) -> FactId {
                self.provenance
            }
        }
    };
}

opaque_record!(SourceMappingRecord);
opaque_record!(SkippedRegion);
opaque_record!(rootlight_ir::DiagnosticRecord);
opaque_record!(ExtensionEnvelope);

fn read_opaque_records<T: OpaqueRecord>(
    connection: &Connection,
    sql: &'static str,
    expected_count: u64,
    context: &GenerationContext<'_>,
    decode: impl Fn(&[u8], &GenerationContext<'_>) -> Result<T, CatalogError>,
) -> Result<Vec<T>, CatalogError> {
    let mut statement = connection.prepare(sql).map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = exact_record_buffer(expected_count)?;
    let mut payload_bytes = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        if usize_to_u64(records.len())? >= expected_count {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        let id = codec::fact_id(get(row, 0)?)?;
        let repository = codec::repository_id(get(row, 1)?)?;
        let generation = codec::generation_id(get(row, 2)?)?;
        let provenance = codec::fact_id(get(row, 3)?)?;
        let raw_payload = row.get_ref(4).map_err(CatalogError::sqlite)?;
        let payload = raw_payload
            .as_str()
            .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
        payload_bytes = payload_bytes
            .checked_add(usize_to_u64(payload.len())?)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::EncodedTextBytes, payload_bytes)
            .map_err(CatalogError::control)?;
        let record = decode(payload.as_bytes(), context)?;
        if record.id() != id
            || record.repository() != repository
            || record.generation() != generation
            || record.provenance() != provenance
        {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        records.push(record);
    }
    if usize_to_u64(records.len())? != expected_count {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(records)
}

fn map_record_decode_error(
    error: NormalizedRecordDecodeError,
    context: &GenerationContext<'_>,
) -> CatalogError {
    if error == NormalizedRecordDecodeError::Interrupted {
        return context.check().map_or_else(CatalogError::control, |()| {
            CatalogError::new(CatalogErrorKind::Corrupt)
        });
    }
    CatalogError::new(CatalogErrorKind::Corrupt)
}

fn map_extension_decode_error(
    error: ExtensionEnvelopeDecodeError,
    context: &GenerationContext<'_>,
) -> CatalogError {
    if error == ExtensionEnvelopeDecodeError::Interrupted {
        return context.check().map_or_else(CatalogError::control, |()| {
            CatalogError::new(CatalogErrorKind::Corrupt)
        });
    }
    CatalogError::new(CatalogErrorKind::Corrupt)
}

fn take_evidence(
    owner_kind: &str,
    owner_id: Vec<u8>,
    source_ordinal: Option<i64>,
    sources: &[SourceRef],
    evidence: &mut EvidenceGroups,
) -> Result<FactEvidence, CatalogError> {
    Ok(FactEvidence {
        source: codec::optional_source_by_ordinal(sources, source_ordinal)?,
        derivation: evidence.take(evidence_reference(owner_kind, &owner_id)?)?,
    })
}

fn get<T: FromSql>(row: &Row<'_>, index: usize) -> Result<T, CatalogError> {
    row.get(index).map_err(CatalogError::sqlite)
}

fn usize_to_u64(value: usize) -> Result<u64, CatalogError> {
    u64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

fn usize_to_i64(value: usize) -> Result<i64, CatalogError> {
    i64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_storage::{GenerationBudget, GenerationReader};

    use super::*;
    use crate::{ORACLE_FILENAME, OracleReader, OracleWriter};

    fn legacy_reader(context: &GenerationContext<'_>) -> (tempfile::TempDir, OracleReader) {
        #[cfg(target_os = "macos")]
        let directory = tempfile::tempdir_in("/private/tmp").unwrap();
        #[cfg(not(target_os = "macos"))]
        let directory = tempfile::tempdir().unwrap();
        drop(OracleWriter::create_in(directory.path()).unwrap());
        let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        connection
            .execute_batch(include_str!(
                "../../../tests/fixtures/compatibility/storage/1.1/sealed-oracle.sql"
            ))
            .unwrap();
        drop(connection);
        let reader = OracleReader::open_in(directory.path(), context).unwrap();
        (directory, reader)
    }

    fn stats(rows: u64, text: u64) -> GenerationStats {
        GenerationStats::new(2, 0, 0, 0, 0, 1, 0, 1, 1, 2, 0, rows, text).unwrap()
    }

    #[test]
    fn skipped_metadata_keeps_nested_spoofs_and_nonempty_derivation_coarse() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let mut region = document.skipped_regions[0].clone();
        region.evidence.derivation.clear();
        region.detail = "detail with \\\"quote\\\" and trailing \\".into();
        let encoded = serde_json::to_string(&region).unwrap();
        let expected = encoded.len() - (serde_json::to_vec(&region.detail).unwrap().len() - 2);
        assert_eq!(
            canonical_skipped_metadata_bytes(&encoded, &context).unwrap(),
            u64::try_from(expected).unwrap()
        );
        let reordered = serde_json::to_string(&serde_json::to_value(&region).unwrap()).unwrap();
        let spaced = encoded.replacen(",\"detail\":\"", ",\"detail\": \"", 1);
        for alternate in [&reordered, &spaced] {
            assert_eq!(
                decode_skipped_region_with_checkpoint(
                    alternate.as_bytes(),
                    &IrLimits::default(),
                    || true
                )
                .unwrap(),
                region
            );
            assert_eq!(
                canonical_skipped_metadata_bytes(alternate, &context).unwrap(),
                0
            );
        }
        region
            .evidence
            .derivation
            .push(FactRef::File(document.files[0].id));
        let derived = serde_json::to_string(&region).unwrap();
        let duplicate =
            encoded.replacen(",\"detail\":\"", ",\"detail\":\"spoof\",\"detail\":\"", 1);
        for rejected in [
            derived.as_str(),
            duplicate.as_str(),
            r#"{"id":"x","source":{"detail":"nested","provenance":"x"},"evidence":{"source":null,"derivation":[]}}"#,
            r#"{"id":"x","detail":"unterminated,"provenance":"x","evidence":{"source":null,"derivation":[]}}"#,
        ] {
            assert_eq!(
                canonical_skipped_metadata_bytes(rejected, &context).unwrap(),
                0
            );
        }
        assert!(
            decode_skipped_region_with_checkpoint(
                duplicate.as_bytes(),
                &IrLimits::default(),
                || true
            )
            .is_err()
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            canonical_skipped_metadata_bytes(&encoded, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
    }

    #[test]
    fn skipped_metadata_credit_never_removes_malformed_payload_transients() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let mut region = document.skipped_regions[0].clone();
        region.evidence.derivation.clear();
        let encoded = serde_json::to_string(&region).unwrap();
        let malformed = encoded.replacen(",\"detail\":", ",\"extra\":[],\"detail\":", 1);
        assert!(
            decode_skipped_region_with_checkpoint(
                malformed.as_bytes(),
                &IrLimits::default(),
                || true
            )
            .is_err()
        );
        let maximum = u64::try_from(malformed.len()).unwrap();
        let before = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats(17, maximum + 1024),
            maximum,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        let credit = canonical_skipped_metadata_bytes(&malformed, &context).unwrap();
        assert!(credit > 0);
        let after = before.with_nonretained_metadata_bytes(credit).unwrap();
        assert_eq!(
            after.maximum_payload_bytes(),
            before.maximum_payload_bytes()
        );
        assert_eq!(
            after.transient_decode_bytes(),
            before.transient_decode_bytes()
        );
        assert_eq!(
            after.record_workspace_bytes(),
            before.record_workspace_bytes()
        );
        assert_eq!(
            after.materialization_bytes(),
            before.materialization_bytes()
        );
        assert!(
            before
                .with_nonretained_metadata_bytes(before.stats().text_bytes() + 1)
                .is_err()
        );
    }

    #[test]
    fn sealed_write_read_plan_matches_independent_reader() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let encoded = include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json");
        let template = rootlight_ir::decode_normalized_ir_document_with_checkpoint(
            encoded,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .unwrap();
        let manifest = rootlight_ids::content_hash(b"manifest");
        let configuration = rootlight_ids::content_hash(b"configuration");
        let providers = rootlight_ids::content_hash(b"providers");
        let generation = rootlight_ids::derive_generation(rootlight_ids::GenerationIdentity {
            repository: template.repository,
            parent: None,
            manifest_hash: manifest,
            config_hash: configuration,
            provider_set_hash: providers,
            format_version: (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
                | u32::from(GENERATION_CONTRACT_VERSION.minor()),
        })
        .id();
        let metadata = GenerationMetadata::new(
            template.repository,
            generation,
            None,
            manifest,
            configuration,
            providers,
        )
        .unwrap();
        let rebound = std::str::from_utf8(encoded)
            .unwrap()
            .replace(&template.generation.to_string(), &generation.to_string());
        let mut document = rootlight_ir::decode_normalized_ir_document_with_checkpoint(
            rebound.as_bytes(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .unwrap();
        let relative =
            rootlight_vfs::RelativePath::parse(std::path::Path::new("src/\u{3bb}.txt")).unwrap();
        document.files[0].path = relative.as_str().to_owned();
        document.files[0].path_locator = Some(relative.to_locator());
        document.diagnostics[0].message = "encoded \u{3bb} and \u{1f600} payload".repeat(32);
        let snapshot = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        #[cfg(target_os = "macos")]
        let directory = tempfile::tempdir_in("/private/tmp").unwrap();
        #[cfg(not(target_os = "macos"))]
        let directory = tempfile::tempdir().unwrap();
        drop(OracleWriter::create_in(directory.path()).unwrap());
        let mut connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        crate::schema::install_generation_cancellation(&connection, &context).unwrap();
        let written =
            crate::write::write_generation_with_metadata(&mut connection, &snapshot, &context)
                .unwrap();
        let expected_maximum = [
            serde_json::to_vec(
                snapshot.document().files[0]
                    .path_locator
                    .as_ref()
                    .unwrap()
                    .components(),
            )
            .unwrap()
            .len(),
            serde_json::to_vec(&snapshot.document().diagnostics[0])
                .unwrap()
                .len(),
            snapshot
                .document()
                .extensions
                .iter()
                .map(|record| serde_json::to_vec(record).unwrap().len())
                .max()
                .unwrap_or(0),
            snapshot
                .document()
                .source_mappings
                .iter()
                .map(|record| serde_json::to_vec(record).unwrap().len())
                .max()
                .unwrap_or(0),
            snapshot
                .document()
                .skipped_regions
                .iter()
                .map(|record| serde_json::to_vec(record).unwrap().len())
                .max()
                .unwrap_or(0),
        ]
        .into_iter()
        .max()
        .unwrap();
        assert_eq!(
            written.maximum_payload_bytes,
            u64::try_from(expected_maximum).unwrap()
        );
        let skipped_metadata =
            snapshot
                .document()
                .skipped_regions
                .iter()
                .fold(0_u64, |total, region| {
                    if region.evidence.derivation.is_empty() {
                        total
                            + u64::try_from(
                                serde_json::to_vec(region).unwrap().len()
                                    - (serde_json::to_vec(&region.detail).unwrap().len() - 2),
                            )
                            .unwrap()
                    } else {
                        total
                    }
                });
        let measured = oracle_read_memory_plan(
            metadata.contract_version(),
            written.stats,
            written.maximum_payload_bytes,
            PayloadCardinality {
                entity_flag_rows: snapshot
                    .document()
                    .entities
                    .iter()
                    .map(|entity| u64::try_from(entity.flags.len()).unwrap())
                    .sum(),
                evidence_rows: u64::try_from(
                    connection
                        .query_row("SELECT count(*) FROM evidence_derivations", [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .unwrap(),
                )
                .unwrap(),
                occurrence_candidate_rows: snapshot
                    .document()
                    .occurrences
                    .iter()
                    .map(|record| match &record.target {
                        OccurrenceTarget::Candidates { symbols, .. } => {
                            u64::try_from(symbols.len()).unwrap()
                        }
                        _ => 0,
                    })
                    .sum(),
                provenance_source_rows: snapshot
                    .document()
                    .provenance
                    .iter()
                    .map(|record| {
                        u64::try_from(record.input_sources.len() + record.evidence_sources.len())
                            .unwrap()
                    })
                    .sum(),
                provenance_derivation_rows: snapshot
                    .document()
                    .provenance
                    .iter()
                    .map(|record| u64::try_from(record.derivation_parents.len()).unwrap())
                    .sum(),
            },
            u64::try_from(
                snapshot
                    .document()
                    .extensions
                    .iter()
                    .filter(|envelope| {
                        envelope.namespace == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE
                    })
                    .count(),
            )
            .unwrap(),
        )
        .unwrap()
        .with_nonretained_metadata_bytes(skipped_metadata)
        .unwrap();
        drop(connection);
        let reader = OracleReader::open_in(directory.path(), &context).unwrap();
        let mut observed = None;
        let stopped = reader.read_with_admission(&context, |plan| {
            observed = Some(plan);
            Err::<(), ()>(())
        });
        assert!(matches!(
            stopped,
            Err(OracleReadAdmissionError::Admission(()))
        ));
        assert_eq!(observed, Some(measured));
        assert_eq!(reader.read(&context).unwrap(), snapshot);
    }

    fn payload_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE files(path_locator_components TEXT);
                 CREATE TABLE source_mappings(payload TEXT);
                 CREATE TABLE skipped_regions(payload TEXT);
                 CREATE TABLE diagnostics(payload TEXT);
                 CREATE TABLE extensions(payload TEXT);
                 INSERT INTO files VALUES (NULL), ('[\"ff\"]');
                 INSERT INTO source_mappings VALUES ('mapping');
                 INSERT INTO skipped_regions VALUES ('region');
                 INSERT INTO diagnostics VALUES ('diagnostic');
                 INSERT INTO extensions VALUES ('short'), ('opaque');",
            )
            .unwrap();
        connection
    }

    #[test]
    fn admission_preserves_typed_rejection_and_transaction_retry() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (_directory, reader) = legacy_reader(&context);
        let calls = Cell::new(0);
        let error = reader
            .read_with_admission(&context, |plan| {
                calls.set(calls.get() + 1);
                assert_eq!(plan.stats(), reader.stats());
                assert_eq!(plan.physical_rows(), 3);
                assert_eq!(plan.identity_registry_rows(), 1);
                assert_eq!(plan.maximum_payload_bytes(), 0);
                assert_eq!(plan.transient_decode_bytes(), 0);
                assert_eq!(plan.materialization_bytes(), plan.record_workspace_bytes());
                Err("private-admission-detail")
            })
            .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert!(!format!("{error:?}").contains("private-admission-detail"));
        assert!(!error.to_string().contains("private-admission-detail"));
        assert!(matches!(
            error,
            OracleReadAdmissionError::Admission("private-admission-detail")
        ));
        let ordinary = reader.read(&context).unwrap();
        assert_eq!(
            reader
                .read_with_admission(&context, |_| Ok::<(), ()>(()))
                .unwrap(),
            ordinary
        );
    }

    #[test]
    fn admission_cancellation_is_checked_before_and_after_callback() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (_directory, reader) = legacy_reader(&context);
        let called = Cell::new(false);
        let error = reader
            .read_with_admission(&context, |_| {
                called.set(true);
                assert!(cancellation.cancel(CancellationReason::ClientRequest));
                Ok::<(), ()>(())
            })
            .unwrap_err();
        assert!(called.get());
        assert!(matches!(
            error,
            OracleReadAdmissionError::Catalog(ref error)
                if error.kind() == CatalogErrorKind::Cancelled
        ));
        called.set(false);
        let error = reader
            .read_with_admission(&context, |_| {
                called.set(true);
                Ok::<(), ()>(())
            })
            .unwrap_err();
        assert!(!called.get());
        assert!(matches!(
            error,
            OracleReadAdmissionError::Catalog(ref error)
                if error.kind() == CatalogErrorKind::Cancelled
        ));
    }

    #[test]
    fn forged_physical_counts_fail_before_admission() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (directory, reader) = legacy_reader(&context);
        drop(reader);
        let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        connection
            .execute("UPDATE generation_meta SET stored_row_count = 3", [])
            .unwrap();
        drop(connection);
        let reader = OracleReader::open_in(directory.path(), &context).unwrap();
        let called = Cell::new(false);
        let error = reader
            .read_with_admission(&context, |_| {
                called.set(true);
                Ok::<(), ()>(())
            })
            .unwrap_err();
        assert!(!called.get());
        assert!(matches!(
            error,
            OracleReadAdmissionError::Catalog(ref error)
                if error.kind() == CatalogErrorKind::Corrupt
        ));
    }

    #[test]
    fn unchecked_registry_shapes_fail_before_admission_and_allow_repair_retry() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (directory, reader) = legacy_reader(&context);
        let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        let (original_kind, original_id): (String, Vec<u8>) = connection
            .query_row("SELECT kind, identity FROM identity_registry", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        for (kind, identity) in [
            ("unknown", vec![0_u8; 16]),
            ("repository", vec![0_u8; 64]),
            ("fact", vec![0_u8; 16]),
        ] {
            connection
                .execute(
                    "UPDATE identity_registry SET kind = ?1, identity = ?2",
                    rusqlite::params![kind, identity],
                )
                .unwrap();
            let called = Cell::new(false);
            let direct = read_generation_with_admission(
                &connection,
                reader.metadata,
                reader.stats,
                &context,
                |_| {
                    called.set(true);
                    Ok::<(), ()>(())
                },
            )
            .unwrap_err();
            assert!(!called.get());
            assert!(matches!(
                direct,
                OracleReadAdmissionError::Catalog(ref error)
                    if error.kind() == CatalogErrorKind::Corrupt
            ));
            let public = reader
                .read_with_admission(&context, |_| {
                    called.set(true);
                    Ok::<(), ()>(())
                })
                .unwrap_err();
            assert!(!called.get());
            assert!(matches!(
                public,
                OracleReadAdmissionError::Catalog(ref error)
                    if error.kind() == CatalogErrorKind::Corrupt
            ));
            connection
                .execute(
                    "UPDATE identity_registry SET kind = ?1, identity = ?2",
                    rusqlite::params![original_kind, original_id],
                )
                .unwrap();
            assert!(
                reader
                    .read_with_admission(&context, |_| {
                        called.set(true);
                        Ok::<(), ()>(())
                    })
                    .is_ok()
            );
            assert!(called.get());
        }
    }

    fn fixed_child_tables() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE occurrence_candidates(occurrence_id, position, entity_id);
             CREATE TABLE provenance_derivations(provenance_id, position, reference_kind, reference_id);",
        ).unwrap();
        connection
    }

    fn insert_fixed_children(
        connection: &Connection,
        owner: FactId,
        position: i64,
        candidate: SymbolId,
        reference: FactRef,
    ) {
        connection
            .execute(
                "INSERT INTO occurrence_candidates VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    owner.as_bytes().as_slice(),
                    position,
                    candidate.as_bytes().as_slice()
                ],
            )
            .unwrap();
        let (kind, id) = codec::encode_fact_ref(&reference);
        connection
            .execute(
                "INSERT INTO provenance_derivations VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![owner.as_bytes().as_slice(), position, kind, id],
            )
            .unwrap();
    }

    fn fixed_children_errors(
        connection: &Connection,
        rows: u64,
        owners: u64,
        context: &GenerationContext<'_>,
    ) -> (Option<CatalogErrorKind>, Option<CatalogErrorKind>) {
        (
            read_candidates(connection, rows, owners, context)
                .err()
                .map(|error| error.kind()),
            read_provenance_derivations(connection, rows, owners, context)
                .err()
                .map(|error| error.kind()),
        )
    }

    #[test]
    fn bounded_fixed_children_preserve_positions_and_move_reserved_vectors() {
        let connection = fixed_child_tables();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        assert!(
            read_candidates(&connection, 0, 3, &context)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            read_provenance_derivations(&connection, 0, 3, &context)
                .unwrap()
                .groups
                .capacity(),
            0
        );
        let first = FactId::from_bytes([1; 20]);
        let last = FactId::from_bytes([3; 20]);
        let symbols = [7, 8, 9].map(|byte| SymbolId::from_bytes([byte; 20]));
        let references = [
            FactRef::File(FileId::from_bytes([7; 20])),
            FactRef::Entity(symbols[1]),
            FactRef::Fact(FactId::from_bytes([9; 20])),
        ];
        for (owner, position, index) in [(last, 0, 2), (first, 1, 1), (first, 0, 0)] {
            insert_fixed_children(
                &connection,
                owner,
                position,
                symbols[index],
                references[index],
            );
        }
        let mut candidates = read_candidates(&connection, 3, 3, &context).unwrap();
        let mut derivations = read_provenance_derivations(&connection, 3, 3, &context).unwrap();
        assert_eq!(candidates.groups.len(), 2);
        assert_eq!(derivations.groups.len(), 2);
        assert_eq!(
            candidates
                .take(FactId::from_bytes([4; 20]))
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            derivations
                .take(FactId::from_bytes([4; 20]))
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
        let candidate_pointer = candidates.groups[0].items.as_ptr();
        let reference_pointer = derivations.groups[0].items.as_ptr();
        let moved = candidates.take(first).unwrap();
        let moved_references = derivations.take(first).unwrap();
        assert_eq!(moved, symbols[..2]);
        assert_eq!(moved_references, references[..2]);
        assert_eq!(moved.as_ptr(), candidate_pointer);
        assert_eq!(moved_references.as_ptr(), reference_pointer);
        assert_eq!(candidates.groups[0].items.capacity(), 0);
        assert_eq!(derivations.groups[0].items.capacity(), 0);
        let absent = FactId::from_bytes([2; 20]);
        assert!(candidates.take(absent).unwrap().is_empty());
        assert!(derivations.take(absent).unwrap().is_empty());
        assert!(!candidates.is_empty());
        assert!(!derivations.is_empty());
        assert_eq!(candidates.take(last).unwrap(), symbols[2..]);
        assert_eq!(derivations.take(last).unwrap(), references[2..]);
        assert!(candidates.is_empty());
        assert!(derivations.is_empty());
    }

    #[test]
    fn bounded_fixed_children_accept_exact_nested_limit_without_growth() {
        let connection = fixed_child_tables();
        let owner = FactId::from_bytes([1; 20]);
        let maximum = IrLimits::default().max_nested_items_per_record;
        for position in (0..maximum).rev() {
            let mut id = [0; 20];
            id[..8].copy_from_slice(&u64::try_from(position).unwrap().to_be_bytes());
            insert_fixed_children(
                &connection,
                owner,
                i64::try_from(position).unwrap(),
                SymbolId::from_bytes(id),
                FactRef::Fact(FactId::from_bytes(id)),
            );
        }
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let mut candidates =
            read_candidates(&connection, u64::try_from(maximum).unwrap(), 1, &context).unwrap();
        let mut derivations =
            read_provenance_derivations(&connection, u64::try_from(maximum).unwrap(), 1, &context)
                .unwrap();
        assert_eq!(candidates.groups[0].items.capacity(), maximum);
        assert_eq!(derivations.groups[0].items.capacity(), maximum);
        let symbols = candidates.take(owner).unwrap();
        let references = derivations.take(owner).unwrap();
        assert_eq!(symbols.len(), maximum);
        assert_eq!(references.len(), maximum);
        for (position, (symbol, reference)) in symbols.iter().zip(&references).enumerate() {
            let mut id = [0; 20];
            id[..8].copy_from_slice(&u64::try_from(position).unwrap().to_be_bytes());
            assert_eq!(*symbol, SymbolId::from_bytes(id));
            assert_eq!(*reference, FactRef::Fact(FactId::from_bytes(id)));
        }
    }

    #[test]
    fn bounded_fixed_children_reject_hostile_cells_counts_gaps_and_cancellation() {
        let connection = fixed_child_tables();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let owner = FactId::from_bytes([1; 20]);
        let symbol = SymbolId::from_bytes([7; 20]);
        let reference = FactRef::Fact(FactId::from_bytes([7; 20]));
        let corrupt = (
            Some(CatalogErrorKind::Corrupt),
            Some(CatalogErrorKind::Corrupt),
        );
        insert_fixed_children(&connection, owner, 0, symbol, reference);
        for (rows, owners) in [(0, 1), (2, 1), (1, 0)] {
            assert_eq!(
                fixed_children_errors(&connection, rows, owners, &context),
                corrupt
            );
        }
        for (column, value) in [
            ("owner", "zeroblob(19)"),
            ("owner", "zeroblob(65536)"),
            ("owner", "'not a blob'"),
            ("position", "-1"),
            ("position", "'0'"),
            ("position", "0.5"),
            ("position", "NULL"),
            ("position", "9223372036854775807"),
            ("item", "zeroblob(19)"),
            ("item", "zeroblob(65536)"),
            ("item", "'not a blob'"),
        ] {
            connection
                .execute_batch(
                    "DELETE FROM occurrence_candidates; DELETE FROM provenance_derivations;",
                )
                .unwrap();
            insert_fixed_children(&connection, owner, 0, symbol, reference);
            let (candidate_column, derivation_column) = match column {
                "owner" => ("occurrence_id", "provenance_id"),
                "position" => ("position", "position"),
                _ => ("entity_id", "reference_id"),
            };
            connection.execute_batch(&format!("UPDATE occurrence_candidates SET {candidate_column} = {value}; UPDATE provenance_derivations SET {derivation_column} = {value};")).unwrap();
            assert_eq!(
                fixed_children_errors(&connection, 1, 1, &context),
                corrupt,
                "{column}={value}"
            );
        }
        for kind in [
            "'unknown'",
            "NULL",
            "zeroblob(65536)",
            "CAST(zeroblob(65536) AS TEXT)",
        ] {
            connection
                .execute_batch("DELETE FROM provenance_derivations;")
                .unwrap();
            connection
                .execute(
                    "INSERT INTO provenance_derivations VALUES (?1, 0, 'fact', ?2)",
                    rusqlite::params![owner.as_bytes().as_slice(), owner.as_bytes().as_slice()],
                )
                .unwrap();
            connection
                .execute_batch(&format!(
                    "UPDATE provenance_derivations SET reference_kind = {kind}"
                ))
                .unwrap();
            assert_eq!(
                read_provenance_derivations(&connection, 1, 1, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        let other = FactId::from_bytes([3; 20]);
        for (items, owners) in [
            (vec![(owner, 0), (owner, 0)], 1),
            (vec![(owner, 0), (owner, 2)], 1),
            (vec![(owner, 1), (other, 0)], 2),
            (vec![(owner, 0), (other, 0)], 1),
        ] {
            connection
                .execute_batch(
                    "DELETE FROM occurrence_candidates; DELETE FROM provenance_derivations;",
                )
                .unwrap();
            for (owner, position) in &items {
                insert_fixed_children(&connection, *owner, *position, symbol, reference);
            }
            assert_eq!(
                fixed_children_errors(
                    &connection,
                    u64::try_from(items.len()).unwrap(),
                    owners,
                    &context
                ),
                corrupt
            );
        }
        connection
            .execute_batch("DELETE FROM occurrence_candidates; DELETE FROM provenance_derivations;")
            .unwrap();
        for (owner, position) in [(owner, 0), (owner, 1), (other, 0)] {
            insert_fixed_children(&connection, owner, position, symbol, reference);
        }
        for ordering in [
            "occurrence_id ASC, position DESC",
            "occurrence_id DESC, position ASC",
        ] {
            assert_eq!(read_fact_groups(&connection,
                &format!("SELECT occurrence_id, position, entity_id FROM occurrence_candidates ORDER BY {ordering}"),
                3, 2, &context, |row| Ok(SymbolId::from_bytes(borrowed_id(row, 2)?)))
                .unwrap_err().kind(), CatalogErrorKind::Corrupt);
        }
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            fixed_children_errors(&connection, 2, 1, &context),
            (
                Some(CatalogErrorKind::Cancelled),
                Some(CatalogErrorKind::Cancelled)
            )
        );
        for candidates in [true, false] {
            let during = Cancellation::new();
            let progress = during.clone();
            connection
                .progress_handler(
                    1,
                    Some(move || {
                        progress.cancel(CancellationReason::ClientRequest);
                        false
                    }),
                )
                .unwrap();
            let context = GenerationContext::new(&during, GenerationBudget::default());
            let error = if candidates {
                read_candidates(&connection, 2, 1, &context)
                    .unwrap_err()
                    .kind()
            } else {
                read_provenance_derivations(&connection, 2, 1, &context)
                    .unwrap_err()
                    .kind()
            };
            assert_eq!(error, CatalogErrorKind::Cancelled);
        }
    }

    #[test]
    fn bounded_fixed_children_reject_group_above_nested_limit() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE occurrence_candidates(occurrence_id, position, entity_id);
             CREATE TABLE provenance_derivations(provenance_id, position, reference_kind, reference_id);",
        ).unwrap();
        let maximum = IrLimits::default().max_nested_items_per_record;
        for position in 0..=maximum {
            connection
                .execute(
                    "INSERT INTO occurrence_candidates VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        [1_u8; 20].as_slice(),
                        i64::try_from(position).unwrap(),
                        [7_u8; 20].as_slice()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO provenance_derivations VALUES (?1, ?2, 'fact', ?3)",
                    rusqlite::params![
                        [1_u8; 20].as_slice(),
                        i64::try_from(position).unwrap(),
                        [7_u8; 20].as_slice()
                    ],
                )
                .unwrap();
        }
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        assert_eq!(
            (
                read_candidates(
                    &connection,
                    u64::try_from(maximum + 1).unwrap(),
                    2,
                    &context
                )
                .err()
                .map(|error| error.kind()),
                read_provenance_derivations(
                    &connection,
                    u64::try_from(maximum + 1).unwrap(),
                    2,
                    &context
                )
                .err()
                .map(|error| error.kind()),
            ),
            (
                Some(CatalogErrorKind::Corrupt),
                Some(CatalogErrorKind::Corrupt)
            ),
            "both fixed-ID families must reject an oversized group before allocation",
        );
    }

    #[test]
    fn bounded_evidence_rejects_group_above_nested_limit() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE evidence_derivations(owner_kind, owner_id, position, reference_kind, reference_id)")
            .unwrap();
        let maximum = IrLimits::default().max_nested_items_per_record;
        for position in 0..=maximum {
            let mut reference = [0_u8; 20];
            reference[..8].copy_from_slice(&u64::try_from(position).unwrap().to_be_bytes());
            connection
                .execute(
                    "INSERT INTO evidence_derivations VALUES ('file', ?1, ?2, 'fact', ?3)",
                    rusqlite::params![
                        [1_u8; 20].as_slice(),
                        i64::try_from(position).unwrap(),
                        reference.as_slice(),
                    ],
                )
                .unwrap();
        }
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        match read_evidence(
            &connection,
            u64::try_from(maximum + 1).unwrap(),
            2,
            &context,
        ) {
            Err(error) => assert_eq!(error.kind(), CatalogErrorKind::Corrupt),
            Ok(_) => panic!("oversized evidence group must be refused before allocation"),
        }
    }

    fn evidence_table() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE evidence_derivations(owner_kind, owner_id, position, reference_kind, reference_id)").unwrap();
        connection
    }

    fn insert_derivation(
        connection: &Connection,
        owner: FactRef,
        position: i64,
        reference: FactRef,
    ) {
        let (kind, id) = codec::encode_fact_ref(&owner);
        let (reference_kind, reference_id) = codec::encode_fact_ref(&reference);
        connection
            .execute(
                "INSERT INTO evidence_derivations VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![kind, id, position, reference_kind, reference_id],
            )
            .unwrap();
    }

    #[test]
    fn bounded_evidence_preserves_positions_and_moves_vectors_in_mixed_owner_order() {
        let connection = evidence_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let empty = read_evidence(&connection, 0, 0, &context).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.groups.capacity(), 0);
        let file = FactRef::File(FileId::from_bytes([1; 20]));
        let entity = FactRef::Entity(SymbolId::from_bytes([1; 20]));
        let fact = FactRef::Fact(FactId::from_bytes([1; 20]));
        let earlier_fact = FactRef::Fact(FactId::from_bytes([3; 20]));
        let later_fact = FactRef::Fact(FactId::from_bytes([5; 20]));
        let references = [
            FactRef::File(FileId::from_bytes([9; 20])),
            FactRef::Entity(SymbolId::from_bytes([9; 20])),
            FactRef::Fact(FactId::from_bytes([9; 20])),
        ];
        for (owner, values) in [
            (fact, &references[1..]),
            (file, references.as_slice()),
            (entity, &references[2..]),
            (earlier_fact, &references[..1]),
            (later_fact, &references[2..]),
        ] {
            for (position, reference) in values.iter().enumerate().rev() {
                insert_derivation(
                    &connection,
                    owner,
                    i64::try_from(position).unwrap(),
                    *reference,
                );
            }
        }
        let mut evidence = read_evidence(&connection, 8, 8, &context).unwrap();
        assert_eq!(evidence.groups.capacity(), 8);
        assert_eq!(evidence.groups.len(), 5);
        assert!(
            evidence
                .groups
                .iter()
                .all(|group| group.items.len() == group.items.capacity())
        );
        let file_pointer = evidence
            .groups
            .iter()
            .find(|group| group.owner == file)
            .unwrap()
            .items
            .as_ptr();
        let taken = take_evidence("file", vec![1; 20], None, &[], &mut evidence).unwrap();
        assert_eq!(taken.derivation, references);
        assert_eq!(taken.derivation.as_ptr(), file_pointer);
        assert!(
            take_evidence("file", vec![1; 20], None, &[], &mut evidence)
                .unwrap()
                .derivation
                .is_empty()
        );
        assert!(!evidence.is_empty());
        assert_eq!(evidence.remaining, 4);
        assert!(
            evidence
                .take(FactRef::Fact(FactId::from_bytes([2; 20])))
                .unwrap()
                .is_empty()
        );
        for (owner, expected) in [
            (entity, &references[2..]),
            (later_fact, &references[2..]),
            (fact, &references[1..]),
            (earlier_fact, &references[..1]),
        ] {
            let pointer = evidence
                .groups
                .iter()
                .find(|group| group.owner == owner)
                .unwrap()
                .items
                .as_ptr();
            let moved = evidence.take(owner).unwrap();
            assert_eq!(moved, expected);
            assert_eq!(moved.as_ptr(), pointer);
        }
        assert!(evidence.is_empty());
        assert!(
            evidence
                .groups
                .iter()
                .all(|group| group.items.capacity() == 0)
        );
    }

    #[test]
    fn bounded_evidence_accepts_exact_nested_limit_without_vector_growth() {
        let connection = evidence_table();
        let owner = FactRef::File(FileId::from_bytes([1; 20]));
        let maximum = IrLimits::default().max_nested_items_per_record;
        for position in 0..maximum {
            let mut id = [0; 20];
            id[..8].copy_from_slice(&u64::try_from(position).unwrap().to_be_bytes());
            insert_derivation(
                &connection,
                owner,
                i64::try_from(position).unwrap(),
                FactRef::Fact(FactId::from_bytes(id)),
            );
        }
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let mut evidence =
            read_evidence(&connection, u64::try_from(maximum).unwrap(), 1, &context).unwrap();
        assert_eq!(evidence.groups[0].items.capacity(), maximum);
        let pointer = evidence.groups[0].items.as_ptr();
        let moved = evidence.take(owner).unwrap();
        assert_eq!(moved.len(), maximum);
        assert_eq!(moved.as_ptr(), pointer);
        assert!(evidence.is_empty());
    }

    #[test]
    fn bounded_evidence_rejects_hostile_cells_before_owned_scalar_decode() {
        let connection = evidence_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for (columns, values) in [
            (
                &[0, 3][..],
                &[
                    "'unknown'",
                    "'FILE'",
                    "7",
                    "NULL",
                    "zeroblob(1048576)",
                    "CAST(zeroblob(1048576) AS TEXT)",
                    "CAST(x'ff' AS TEXT)",
                ][..],
            ),
            (
                &[1, 4][..],
                &[
                    "zeroblob(19)",
                    "zeroblob(21)",
                    "zeroblob(1048576)",
                    "'wrong scalar type'",
                    "7",
                    "NULL",
                ][..],
            ),
            (
                &[2][..],
                &[
                    "-1",
                    "1",
                    "2.0",
                    "'0'",
                    "NULL",
                    "9223372036854775807",
                    "zeroblob(1048576)",
                ][..],
            ),
        ] {
            for column in columns {
                for value in values {
                    connection
                        .execute("DELETE FROM evidence_derivations", [])
                        .unwrap();
                    let mut cells = ["'file'", "zeroblob(20)", "0", "'fact'", "zeroblob(20)"];
                    cells[*column] = value;
                    connection
                        .execute_batch(&format!(
                            "INSERT INTO evidence_derivations VALUES ({})",
                            cells.join(",")
                        ))
                        .unwrap();
                    assert_eq!(
                        read_evidence(&connection, 1, 1, &context)
                            .unwrap_err()
                            .kind(),
                        CatalogErrorKind::Corrupt,
                        "column{column} {value}"
                    );
                }
            }
        }
    }

    #[test]
    fn bounded_evidence_rejects_counts_dense_position_gaps_orphans_and_cancellation() {
        let connection = evidence_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let owner = FactRef::File(FileId::from_bytes([1; 20]));
        let other = FactRef::File(FileId::from_bytes([3; 20]));
        let reference = FactRef::Fact(FactId::from_bytes([7; 20]));
        insert_derivation(&connection, owner, 0, reference);
        for (rows, owners) in [(0, 1), (2, 1), (1, 0)] {
            assert_eq!(
                read_evidence(&connection, rows, owners, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        let mut orphan = read_evidence(&connection, 1, 2, &context).unwrap();
        assert!(orphan.take(other).unwrap().is_empty());
        assert!(!orphan.is_empty());
        assert_eq!(
            take_evidence("unknown", vec![1; 20], None, &[], &mut orphan)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            take_evidence("file", vec![1; 19], None, &[], &mut orphan)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
        assert!(!orphan.is_empty());
        for (entries, owners) in [
            (vec![(owner, 0), (owner, 0)], 1),
            (vec![(owner, 0), (owner, 2)], 1),
            (vec![(owner, 0), (other, 0)], 1),
            (vec![(other, 2), (other, 1), (owner, 0)], 2),
        ] {
            connection
                .execute("DELETE FROM evidence_derivations", [])
                .unwrap();
            for (owner, position) in &entries {
                insert_derivation(&connection, *owner, *position, reference);
            }
            assert_eq!(
                read_evidence(
                    &connection,
                    u64::try_from(entries.len()).unwrap(),
                    owners,
                    &context
                )
                .unwrap_err()
                .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            read_evidence(&connection, 3, 2, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
        let during_query = Cancellation::new();
        let progress = during_query.clone();
        connection
            .progress_handler(
                1,
                Some(move || {
                    progress.cancel(CancellationReason::ClientRequest);
                    false
                }),
            )
            .unwrap();
        let context = GenerationContext::new(&during_query, GenerationBudget::default());
        assert_eq!(
            read_evidence(&connection, 3, 2, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
    }

    #[test]
    fn bounded_flags_preserve_order_and_move_reserved_vectors() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE entity_flags(entity_id, flag)")
            .unwrap();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let empty = read_flags(&connection, 0, 3, &context).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.groups.capacity(), 0);
        let first = SymbolId::from_bytes([1; 20]);
        let last = SymbolId::from_bytes([3; 20]);
        let all_flags = [
            EntityFlag::Generated,
            EntityFlag::External,
            EntityFlag::Test,
            EntityFlag::Exported,
            EntityFlag::Deprecated,
            EntityFlag::Synthetic,
        ];
        connection
            .execute(
                "INSERT INTO entity_flags VALUES (?1, 'synthetic')",
                [last.as_bytes().as_slice()],
            )
            .unwrap();
        for flag in all_flags.into_iter().rev() {
            connection
                .execute(
                    "INSERT INTO entity_flags VALUES (?1, ?2)",
                    rusqlite::params![
                        first.as_bytes().as_slice(),
                        codec::encode_enum(&flag).unwrap()
                    ],
                )
                .unwrap();
        }
        let mut groups = read_flags(&connection, 7, 3, &context).unwrap();
        assert_eq!(groups.groups.capacity(), 3);
        assert_eq!(groups.groups.len(), 2);
        assert!(
            groups
                .groups
                .iter()
                .all(|group| group.flags.capacity() == 6)
        );
        let first_buffer = groups.groups[0].flags.as_ptr();
        let last_buffer = groups.groups[1].flags.as_ptr();
        assert!(
            groups
                .take(SymbolId::from_bytes([0; 20]))
                .unwrap()
                .is_empty()
        );
        let flags = groups.take(first).unwrap();
        assert_eq!(flags, all_flags);
        assert_eq!(flags.as_ptr(), first_buffer);
        assert!(groups.groups[0].flags.is_empty());
        assert!(
            groups
                .take(SymbolId::from_bytes([2; 20]))
                .unwrap()
                .is_empty()
        );
        let flags = groups.take(last).unwrap();
        assert_eq!(flags, [EntityFlag::Synthetic]);
        assert_eq!(flags.as_ptr(), last_buffer);
        assert!(groups.is_empty());
    }

    #[test]
    fn bounded_flags_reject_hostile_cells_before_owned_scalar_decode() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE entity_flags(entity_id, flag)")
            .unwrap();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for (entity, flag) in [
            ("zeroblob(19)", "'test'"),
            ("zeroblob(21)", "'test'"),
            ("zeroblob(1048576)", "'test'"),
            ("'wrong scalar type'", "'test'"),
            ("NULL", "'test'"),
            ("zeroblob(20)", "'unknown'"),
            ("zeroblob(20)", "zeroblob(1048576)"),
            ("zeroblob(20)", "CAST(zeroblob(1048576) AS TEXT)"),
            ("zeroblob(20)", "CAST(x'ff' AS TEXT)"),
            ("zeroblob(20)", "NULL"),
        ] {
            connection.execute("DELETE FROM entity_flags", []).unwrap();
            connection
                .execute_batch(&format!(
                    "INSERT INTO entity_flags VALUES ({entity}, {flag})"
                ))
                .unwrap();
            assert_eq!(
                read_flags(&connection, 1, 1, &context).unwrap_err().kind(),
                CatalogErrorKind::Corrupt,
                "{entity}, {flag}",
            );
        }
    }

    #[test]
    fn bounded_flags_reject_duplicates_excess_owners_counts_and_orphans() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE entity_flags(entity_id, flag);
                 INSERT INTO entity_flags VALUES (zeroblob(20), 'test')",
            )
            .unwrap();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for expected in [0, 2] {
            assert_eq!(
                read_flags(&connection, expected, 1, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt,
            );
        }
        let mut orphan = read_flags(&connection, 1, 1, &context).unwrap();
        assert!(!orphan.is_empty());
        assert_eq!(
            orphan
                .take(SymbolId::from_bytes([1; 20]))
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt,
        );
        for extra in [
            "INSERT INTO entity_flags VALUES (zeroblob(20), 'test')",
            "INSERT INTO entity_flags VALUES (x'0101010101010101010101010101010101010101', 'test')",
        ] {
            connection.execute_batch(extra).unwrap();
            assert_eq!(
                read_flags(&connection, 2, 1, &context).unwrap_err().kind(),
                CatalogErrorKind::Corrupt,
            );
            connection.execute("DELETE FROM entity_flags", []).unwrap();
            connection
                .execute_batch("INSERT INTO entity_flags VALUES (zeroblob(20), 'test')")
                .unwrap();
        }
        connection
            .execute_batch(
                "INSERT INTO entity_flags VALUES
                 (zeroblob(20), 'generated'), (zeroblob(20), 'external'),
                 (zeroblob(20), 'exported'), (zeroblob(20), 'deprecated'),
                 (zeroblob(20), 'synthetic'), (zeroblob(20), 'test')",
            )
            .unwrap();
        for entities in [1, 2] {
            assert_eq!(
                read_flags(&connection, 7, entities, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt,
            );
        }
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            read_flags(&connection, 7, 2, &context).unwrap_err().kind(),
            CatalogErrorKind::Cancelled,
        );
        let during_query = Cancellation::new();
        let progress_cancellation = during_query.clone();
        connection
            .progress_handler(
                1,
                Some(move || {
                    progress_cancellation.cancel(CancellationReason::ClientRequest);
                    false
                }),
            )
            .unwrap();
        let context = GenerationContext::new(&during_query, GenerationBudget::default());
        assert_eq!(
            read_flags(&connection, 7, 2, &context).unwrap_err().kind(),
            CatalogErrorKind::Cancelled,
        );
    }

    #[test]
    fn fixed_child_queries_use_native_indexes_without_temporary_sort() {
        let connection = crate::schema::create_ephemeral_oracle().unwrap();
        let plans = [
            CANDIDATE_GROUPS_SQL,
            PROVENANCE_GROUPS_SQL,
            PROVENANCE_SOURCES_SQL,
        ]
        .map(|sql| {
            let mut statement = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        });
        assert!(
            plans
                .iter()
                .flatten()
                .all(|detail| !detail.contains("TEMP B-TREE")),
            "fixed-ID reads must not add an unmodeled SQLite sort: {plans:?}"
        );
    }

    fn provenance_source_table() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE provenance_sources(provenance_id, source_kind, position, source_ordinal);"
        ).unwrap();
        connection
    }

    fn insert_provenance_source(
        connection: &Connection,
        owner: FactId,
        kind: &str,
        position: i64,
        ordinal: i64,
    ) {
        connection
            .execute(
                "INSERT INTO provenance_sources VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![owner.as_bytes().as_slice(), kind, position, ordinal],
            )
            .unwrap();
    }

    #[test]
    fn provenance_sources_preserve_both_roles_with_exact_capacity_and_identity() {
        let connection = provenance_source_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let source = &document.provenance[0].input_sources[0];
        let maximum = IrLimits::default().max_nested_items_per_record;
        let sources: Vec<_> = (0..maximum)
            .map(|position| {
                SourceRef::new(
                    source.repository(),
                    source.generation(),
                    rootlight_ir::SourceSpan::new(
                        source.span().file(),
                        u64::try_from(position).unwrap(),
                        u64::try_from(position + 1).unwrap(),
                    )
                    .unwrap(),
                    source.content_hash(),
                    source.line_hint(),
                )
            })
            .collect();
        let owner = document.provenance[0].id;
        for position in 0..maximum {
            for kind in ["input", "evidence"] {
                insert_provenance_source(
                    &connection,
                    owner,
                    kind,
                    i64::try_from(position).unwrap(),
                    i64::try_from(position).unwrap(),
                );
            }
        }
        let mut groups = read_provenance_sources(
            &connection,
            &sources,
            u64::try_from(maximum * 2).unwrap(),
            1,
            &context,
        )
        .unwrap();
        assert_eq!(groups.groups.capacity(), 2);
        assert!(
            groups
                .groups
                .iter()
                .all(|group| group.items.capacity() == maximum)
        );
        let mut restored = document.provenance[0].clone();
        restored.evidence_sources = groups
            .take((owner, ProvenanceSourceKind::Evidence))
            .unwrap();
        restored.input_sources = groups.take((owner, ProvenanceSourceKind::Input)).unwrap();
        assert!(groups.is_empty());
        assert_eq!(restored.input_sources, sources);
        assert_eq!(restored.evidence_sources, sources);
        let mut expected = document.provenance[0].clone();
        expected.input_sources = sources.clone();
        expected.evidence_sources = sources;
        assert_eq!(
            rootlight_ir::derive_provenance_record_id(&restored).unwrap(),
            rootlight_ir::derive_provenance_record_id(&expected).unwrap()
        );
    }

    #[test]
    fn provenance_sources_bound_headers_across_many_owners() {
        let connection = provenance_source_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let sources: Vec<_> = document
            .files
            .iter()
            .map(|file| file.evidence.source.clone().unwrap())
            .collect();
        let owners: Vec<_> = (0_u64..512)
            .map(|index| {
                let mut bytes = [0; 20];
                bytes[..8].copy_from_slice(&index.to_be_bytes());
                FactId::from_bytes(bytes)
            })
            .collect();
        for owner in &owners {
            for kind in ["input", "evidence"] {
                for position in 0..2 {
                    insert_provenance_source(&connection, *owner, kind, position, position);
                }
            }
        }
        let mut groups =
            read_provenance_sources(&connection, &sources, 2048, 512, &context).unwrap();
        assert_eq!(groups.groups.len(), 1024);
        assert_eq!(groups.groups.capacity(), 1024);
        assert!(
            groups
                .groups
                .iter()
                .all(|group| group.items.capacity() == 2)
        );
        for owner in owners {
            for kind in [ProvenanceSourceKind::Evidence, ProvenanceSourceKind::Input] {
                assert_eq!(groups.take((owner, kind)).unwrap(), sources);
            }
        }
        assert!(groups.is_empty());
    }

    #[test]
    fn provenance_sources_keep_distinct_role_contents_and_missing_roles() {
        let connection = provenance_source_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let sources: Vec<_> = document
            .files
            .iter()
            .map(|file| file.evidence.source.clone().unwrap())
            .collect();
        assert!(sources.len() >= 2);
        let owners = [1, 2, 3, 4].map(|byte| FactId::from_bytes([byte; 20]));
        for (owner, kind, position, ordinal) in [
            (owners[0], "evidence", 0, 1),
            (owners[0], "input", 0, 0),
            (owners[0], "input", 1, 1),
            (owners[1], "input", 0, 1),
            (owners[2], "evidence", 0, 0),
        ] {
            insert_provenance_source(&connection, owner, kind, position, ordinal);
        }
        let mut groups = read_provenance_sources(&connection, &sources, 5, 4, &context).unwrap();
        for (owner, evidence, input) in [
            (owners[0], vec![sources[1].clone()], sources.clone()),
            (owners[1], vec![], vec![sources[1].clone()]),
            (owners[2], vec![sources[0].clone()], vec![]),
            (owners[3], vec![], vec![]),
        ] {
            assert_eq!(
                groups
                    .take((owner, ProvenanceSourceKind::Evidence))
                    .unwrap(),
                evidence
            );
            assert_eq!(
                groups.take((owner, ProvenanceSourceKind::Input)).unwrap(),
                input
            );
        }
        assert!(groups.is_empty());
    }

    #[test]
    fn provenance_source_admission_precedes_child_workspace_and_allows_retry() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let generation = crate::oracle_roundtrip_tests::snapshot(
            crate::oracle_roundtrip_tests::fixture_documents().0,
        );
        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let reader = OracleWriter::create_in(directory.path())
            .unwrap()
            .seal_unverified_for_test(generation.clone(), &context)
            .unwrap();
        let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        connection
            .execute_batch("UPDATE provenance_sources SET position = position + 1000000000")
            .unwrap();
        let mut called = false;
        let rejected = read_generation_with_admission(
            &connection,
            reader.metadata(),
            reader.stats(),
            &context,
            |plan| {
                called = true;
                assert!(plan.record_workspace_bytes() > 1);
                Err::<(), _>("budget")
            },
        );
        assert!(called);
        assert!(matches!(
            rejected,
            Err(OracleReadAdmissionError::Admission("budget"))
        ));
        assert!(
            matches!(read_generation_with_admission(&connection, reader.metadata(), reader.stats(), &context, |_| Ok::<(), ()>(())), Err(OracleReadAdmissionError::Catalog(error)) if error.kind() == CatalogErrorKind::Corrupt)
        );
        connection
            .execute_batch("UPDATE provenance_sources SET position = position - 1000000000")
            .unwrap();
        assert_eq!(
            read_generation_with_admission(
                &connection,
                reader.metadata(),
                reader.stats(),
                &context,
                |_| Ok::<(), ()>(())
            )
            .unwrap(),
            generation
        );
    }

    #[test]
    fn provenance_sources_reject_hostile_roles_ordinals_positions_and_orphans() {
        let connection = provenance_source_table();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let sources = &document.provenance[0].input_sources;
        let owner = document.provenance[0].id;
        for (column, value) in [
            ("source_kind", "'unknown'"),
            ("source_kind", "NULL"),
            ("source_kind", "zeroblob(65536)"),
            ("source_kind", "CAST(zeroblob(65536) AS TEXT)"),
            ("source_ordinal", "-1"),
            ("source_ordinal", "9223372036854775807"),
            ("source_ordinal", "'0'"),
            ("source_ordinal", "0.5"),
            ("source_ordinal", "NULL"),
            ("position", "-1"),
            ("position", "'0'"),
            ("position", "0.5"),
            ("position", "NULL"),
            ("position", "9223372036854775807"),
            ("position", "4096"),
            ("provenance_id", "zeroblob(19)"),
            ("provenance_id", "zeroblob(65536)"),
            ("provenance_id", "'owner'"),
        ] {
            connection
                .execute("DELETE FROM provenance_sources", [])
                .unwrap();
            insert_provenance_source(&connection, owner, "input", 0, 0);
            connection
                .execute_batch(&format!("UPDATE provenance_sources SET {column} = {value}"))
                .unwrap();
            assert_eq!(
                read_provenance_sources(&connection, sources, 1, 1, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt,
                "{column}={value}"
            );
        }
        for positions in [[0, 0], [0, 2]] {
            connection
                .execute("DELETE FROM provenance_sources", [])
                .unwrap();
            for position in positions {
                insert_provenance_source(&connection, owner, "input", position, 0);
            }
            assert_eq!(
                read_provenance_sources(&connection, sources, 2, 1, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        connection
            .execute("DELETE FROM provenance_sources", [])
            .unwrap();
        insert_provenance_source(&connection, owner, "input", 0, 0);
        for (rows, owners) in [(0, 1), (2, 1), (1, 0)] {
            assert_eq!(
                read_provenance_sources(&connection, sources, rows, owners, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        let mut orphan = read_provenance_sources(&connection, sources, 1, 1, &context).unwrap();
        assert!(!orphan.is_empty());
        assert_eq!(
            orphan
                .take((FactId::from_bytes([255; 20]), ProvenanceSourceKind::Input))
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            read_provenance_sources(&connection, sources, 1, 1, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
    }

    #[test]
    fn provenance_source_workspace_replaces_only_its_authenticated_envelope() {
        let stats =
            GenerationStats::new(1, 0, 0, 0, 1000, 0, 0, 0, 0, 0, 0, 1_000_000, 17).unwrap();
        let empty = PayloadCardinality::default();
        let baseline =
            oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 97, empty, 0).unwrap();
        let empty_bytes = provenance_source_workspace_bytes(0, stats.provenance()).unwrap();
        for rows in [1, 2, 1000, 400_000] {
            let plan = oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                stats,
                97,
                PayloadCardinality {
                    provenance_source_rows: rows,
                    ..empty
                },
                0,
            )
            .unwrap();
            let typed_bytes = (rows.min(stats.provenance() * 2) + 1)
                * u64::try_from(std::mem::size_of::<
                    FactGroup<SourceRef, (FactId, ProvenanceSourceKind)>,
                >())
                .unwrap()
                + rows * u64::try_from(std::mem::size_of::<SourceRef>()).unwrap()
                + u64::try_from(
                    std::mem::size_of::<ProvenanceSourceGroups>()
                        + std::mem::size_of::<SourceRef>(),
                )
                .unwrap();
            assert_eq!(
                provenance_source_workspace_bytes(rows, stats.provenance()).unwrap(),
                typed_bytes
            );
            assert_eq!(
                plan.record_workspace_bytes() + rows * 4096 - typed_bytes,
                baseline.record_workspace_bytes() - empty_bytes
            );
            assert_eq!(
                plan.materialization_bytes() + rows * 4096 - typed_bytes,
                baseline.materialization_bytes() - empty_bytes
            );
            assert_eq!(plan.stats(), baseline.stats());
            assert_eq!(plan.physical_rows(), baseline.physical_rows());
            assert_eq!(plan.retained_text_bytes(), baseline.retained_text_bytes());
            assert_eq!(
                plan.identity_registry_rows(),
                baseline.identity_registry_rows()
            );
            assert_eq!(
                plan.maximum_payload_bytes(),
                baseline.maximum_payload_bytes()
            );
            assert_eq!(
                plan.transient_decode_bytes(),
                baseline.transient_decode_bytes()
            );
        }
        for (rows, owners) in [(8193, 1), (1, 0)] {
            assert_eq!(
                provenance_source_workspace_bytes(rows, owners)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        assert!(provenance_source_workspace_bytes(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn fixed_child_workspace_replaces_only_authenticated_family_envelopes() {
        let stats = GenerationStats::new(1, 1, 2, 0, 2, 0, 0, 0, 0, 0, 0, 115, 17).unwrap();
        let empty = PayloadCardinality::default();
        let baseline =
            oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 97, empty, 0).unwrap();
        let empty_candidate =
            fact_group_workspace_bytes::<SymbolId>(0, stats.occurrences()).unwrap();
        let empty_derivation =
            fact_group_workspace_bytes::<FactRef>(0, stats.provenance()).unwrap();
        for (candidates, derivations) in [(1, 0), (0, 1), (2, 2), (20, 30)] {
            let cardinality = PayloadCardinality {
                occurrence_candidate_rows: candidates,
                provenance_derivation_rows: derivations,
                ..empty
            };
            let plan =
                oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 97, cardinality, 0)
                    .unwrap();
            let candidate_bytes = (candidates.min(stats.occurrences()) + 1)
                * u64::try_from(std::mem::size_of::<FactGroup<SymbolId>>()).unwrap()
                + candidates * u64::try_from(std::mem::size_of::<SymbolId>()).unwrap()
                + u64::try_from(
                    std::mem::size_of::<FactGroups<SymbolId>>() + std::mem::size_of::<SymbolId>(),
                )
                .unwrap();
            let derivation_bytes = (derivations.min(stats.provenance()) + 1)
                * u64::try_from(std::mem::size_of::<FactGroup<FactRef>>()).unwrap()
                + derivations * u64::try_from(std::mem::size_of::<FactRef>()).unwrap()
                + u64::try_from(
                    std::mem::size_of::<FactGroups<FactRef>>() + std::mem::size_of::<FactRef>(),
                )
                .unwrap();
            assert_eq!(
                fact_group_workspace_bytes::<SymbolId>(candidates, stats.occurrences()).unwrap(),
                candidate_bytes
            );
            assert_eq!(
                fact_group_workspace_bytes::<FactRef>(derivations, stats.provenance()).unwrap(),
                derivation_bytes
            );
            let original_charge = (candidates + derivations) * 4096;
            assert_eq!(
                plan.record_workspace_bytes() + original_charge
                    - candidate_bytes
                    - derivation_bytes,
                baseline.record_workspace_bytes() - empty_candidate - empty_derivation
            );
            assert_eq!(
                plan.materialization_bytes() + original_charge - candidate_bytes - derivation_bytes,
                baseline.materialization_bytes() - empty_candidate - empty_derivation
            );
            assert_eq!(plan.stats(), baseline.stats());
            assert_eq!(plan.physical_rows(), baseline.physical_rows());
            assert_eq!(plan.retained_text_bytes(), baseline.retained_text_bytes());
            assert_eq!(
                plan.identity_registry_rows(),
                baseline.identity_registry_rows()
            );
            assert_eq!(
                plan.maximum_payload_bytes(),
                baseline.maximum_payload_bytes()
            );
            assert_eq!(
                plan.transient_decode_bytes(),
                baseline.transient_decode_bytes()
            );
        }
        for cardinality in [
            PayloadCardinality {
                occurrence_candidate_rows: 103,
                ..empty
            },
            PayloadCardinality {
                provenance_derivation_rows: 103,
                ..empty
            },
            PayloadCardinality {
                occurrence_candidate_rows: 60,
                provenance_derivation_rows: 43,
                ..empty
            },
            PayloadCardinality {
                occurrence_candidate_rows: u64::MAX,
                ..empty
            },
        ] {
            assert_eq!(
                oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 0, cardinality, 0)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        for (rows, owners) in [(4097, 1), (1, 0)] {
            assert_eq!(
                fact_group_workspace_bytes::<SymbolId>(rows, owners)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
            assert_eq!(
                fact_group_workspace_bytes::<FactRef>(rows, owners)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        assert_eq!(
            fact_group_workspace_bytes::<SymbolId>(u64::MAX, u64::MAX)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Storage
        );
        assert_eq!(
            fact_group_workspace_bytes::<FactRef>(u64::MAX, u64::MAX)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Storage
        );
    }

    #[test]
    fn evidence_workspace_replaces_only_its_complete_typed_family_charge() {
        let stats = GenerationStats::new(1, 1, 1, 1, 0, 0, 1, 0, 0, 0, 0, 40, 17).unwrap();
        let owners = evidence_owner_count(stats).unwrap();
        assert_eq!(owners, 5);
        let baseline = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            97,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        let empty = evidence_workspace_bytes(0, owners).unwrap();
        for rows in [1, 5, 20] {
            let plan = oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                stats,
                97,
                PayloadCardinality {
                    entity_flag_rows: 0,
                    evidence_rows: rows,
                    ..PayloadCardinality::default()
                },
                0,
            )
            .unwrap();
            let complete = typed_record_buffer_bytes::<EvidenceGroup>(rows.min(owners)).unwrap()
                + rows * u64::try_from(std::mem::size_of::<FactRef>()).unwrap()
                + u64::try_from(std::mem::size_of::<EvidenceGroups>()).unwrap();
            assert_eq!(evidence_workspace_bytes(rows, owners).unwrap(), complete);
            assert_eq!(
                plan.record_workspace_bytes() + rows * 4096 - complete,
                baseline.record_workspace_bytes() - empty
            );
            assert_eq!(
                plan.materialization_bytes() + rows * 4096 - complete,
                baseline.materialization_bytes() - empty
            );
            assert_eq!(plan.stats(), baseline.stats());
            assert_eq!(plan.retained_text_bytes(), baseline.retained_text_bytes());
            assert_eq!(plan.physical_rows(), baseline.physical_rows());
            assert_eq!(
                plan.identity_registry_rows(),
                baseline.identity_registry_rows()
            );
            assert_eq!(
                plan.maximum_payload_bytes(),
                baseline.maximum_payload_bytes()
            );
            assert_eq!(
                plan.transient_decode_bytes(),
                baseline.transient_decode_bytes()
            );
        }
        assert_eq!(
            evidence_workspace_bytes(4097, 1).unwrap_err().kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            evidence_workspace_bytes(1, 0).unwrap_err().kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            evidence_workspace_bytes(u64::MAX, u64::MAX)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Storage
        );
        let only_one_auxiliary =
            GenerationStats::new(0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0).unwrap();
        assert_eq!(
            oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                only_one_auxiliary,
                0,
                PayloadCardinality {
                    entity_flag_rows: 1,
                    evidence_rows: 1,
                    ..PayloadCardinality::default()
                },
                0,
            )
            .unwrap_err()
            .kind(),
            CatalogErrorKind::Corrupt
        );
    }

    #[test]
    fn flag_workspace_changes_only_the_flag_envelope_and_complete_bounded_buffer() {
        let stats = GenerationStats::new(0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 30, 17).unwrap();
        let baseline = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            97,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        let empty_workspace = flag_workspace_bytes(0, 3).unwrap();
        assert_eq!(
            empty_workspace,
            typed_record_buffer_bytes::<FlagGroup>(0).unwrap()
        );
        for flag_rows in [1, 6, 18] {
            let groups = flag_rows.min(stats.entities());
            let expected_workspace = typed_record_buffer_bytes::<FlagGroup>(groups).unwrap()
                + groups * u64::try_from(6 * std::mem::size_of::<EntityFlag>()).unwrap();
            assert_eq!(
                flag_workspace_bytes(flag_rows, 3).unwrap(),
                expected_workspace
            );
            let plan = oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                stats,
                97,
                PayloadCardinality {
                    entity_flag_rows: flag_rows,
                    evidence_rows: 0,
                    ..PayloadCardinality::default()
                },
                0,
            )
            .unwrap();
            assert_eq!(
                plan.record_workspace_bytes() + flag_rows * 4096 - expected_workspace,
                baseline.record_workspace_bytes() - empty_workspace,
            );
            assert_eq!(
                plan.transient_decode_bytes(),
                baseline.transient_decode_bytes()
            );
            assert_eq!(
                plan.maximum_payload_bytes(),
                baseline.maximum_payload_bytes()
            );
            assert_eq!(plan.stats(), baseline.stats());
            assert_eq!(plan.physical_rows(), baseline.physical_rows());
            assert_eq!(
                plan.identity_registry_rows(),
                baseline.identity_registry_rows()
            );
        }
        assert_eq!(
            flag_workspace_bytes(19, 3).unwrap_err().kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            flag_workspace_bytes(u64::MAX, u64::MAX).unwrap_err().kind(),
            CatalogErrorKind::Storage
        );
        assert_eq!(
            exact_record_buffer::<FlagGroup>(u64::MAX)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Storage,
        );
        let no_auxiliary = GenerationStats::new(0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0).unwrap();
        assert_eq!(
            oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                no_auxiliary,
                0,
                PayloadCardinality {
                    entity_flag_rows: 1,
                    evidence_rows: 0,
                    ..PayloadCardinality::default()
                },
                0,
            )
            .unwrap_err()
            .kind(),
            CatalogErrorKind::Corrupt,
        );
    }

    #[test]
    fn symbol_metadata_preserves_escaped_text_and_refuses_unrecognized_outer_shapes() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let metadata = crate::oracle_roundtrip_tests::snapshot(document.clone()).metadata();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE extensions(extension_id, payload)")
            .unwrap();
        let mut envelope = document.extensions[0].clone();
        envelope.namespace = rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE.into();
        envelope.version = rootlight_ir::SYMBOL_IDENTITY_CLAIM_VERSION.into();
        envelope.evidence.derivation = vec![FactRef::Entity(document.entities[0].id)];
        for text in [
            "",
            "ordinary",
            "λ 雪 🦀",
            "\0\n\r\t\u{0008}\u{000c}\u{001b}",
            r#"quoted \"payload\",\"provenance\":\" and trailing \\"#,
        ] {
            envelope.payload = text.into();
            for lines in [None, Some(rootlight_ir::LineRange::new(1, 2).unwrap())] {
                let source = envelope.evidence.source.as_ref().unwrap();
                envelope.evidence.source = Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    source.span(),
                    source.content_hash(),
                    lines,
                ));
                let encoded = serde_json::to_string(&envelope).unwrap();
                let retained: usize = [&envelope.namespace, &envelope.version, &envelope.payload]
                    .into_iter()
                    .map(|value| serde_json::to_vec(value).unwrap().len() - 2)
                    .sum();
                connection.execute("DELETE FROM extensions", []).unwrap();
                connection
                    .execute(
                        "INSERT INTO extensions VALUES (?1,?2)",
                        rusqlite::params![envelope.id.as_bytes().as_slice(), encoded],
                    )
                    .unwrap();
                let observed = symbol_envelope_census(&connection, metadata, 1, &context).unwrap();
                assert_eq!(observed.rows, 1);
                assert_eq!(
                    observed.metadata_bytes,
                    u64::try_from(encoded.len() - retained - SYMBOL_EVIDENCE_CHILD_BYTES).unwrap()
                );
                let decoded = decode_extension_envelope_with_checkpoint(
                    encoded.as_bytes(),
                    &IrLimits::default(),
                    || true,
                )
                .unwrap();
                let child_bytes = decoded
                    .evidence
                    .derivation
                    .capacity()
                    .checked_mul(std::mem::size_of::<FactRef>())
                    .unwrap();
                assert!(child_bytes <= SYMBOL_EVIDENCE_CHILD_BYTES);
                assert!(
                    u64::try_from(retained + child_bytes).unwrap()
                        <= u64::try_from(encoded.len()).unwrap() - observed.metadata_bytes
                );
                assert_eq!(decoded, envelope);
            }
        }
        envelope.payload = "payload".into();
        let encoded = serde_json::to_string(&envelope).unwrap();
        let reordered = serde_json::to_string(&serde_json::to_value(&envelope).unwrap()).unwrap();
        for rejected in [
            reordered,
            format!("{encoded} "),
            encoded.replacen("\"payload\":", "\"extra\":[],\"payload\":", 1),
            encoded.replacen("\"payload\":", "\"payload\":\"duplicate\",\"payload\":", 1),
            encoded.replacen("\"payload\":\"payload\"", "\"payload\":null", 1),
            encoded.replacen("\"payload\":\"payload\"", r#""payload":"\uD800""#, 1),
            encoded.replacen("\"payload\":\"payload\"", r#""payload":"\q""#, 1),
            encoded.replacen("\"version\":\"1.3\"", "\"version\":\"future\"", 1),
            encoded.replacen("\"kind\":\"entity\"", "\"kind\":\"file\"", 1),
            encoded.replacen("\"start_byte\":1", "\"start_byte\":01", 1),
            encoded.replacen("\"line_hint\":{", "\"line_hint\":null,\"extra\":{", 1),
        ] {
            connection.execute("DELETE FROM extensions", []).unwrap();
            connection
                .execute(
                    "INSERT INTO extensions VALUES (?1,?2)",
                    rusqlite::params![envelope.id.as_bytes().as_slice(), rejected],
                )
                .unwrap();
            assert_eq!(
                symbol_envelope_census(&connection, metadata, 1, &context)
                    .unwrap()
                    .metadata_bytes,
                0,
                "{rejected}"
            );
        }
    }

    #[test]
    fn escaped_symbol_text_checks_cancellation_after_crossing_chunk_boundaries() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let encoded = format!("\"a{}\"", r"\u0000".repeat(4096));
        let mut input = encoded.as_bytes();
        let mut checks = 0;
        let error = take_canonical_json_string(&mut input, || {
            checks += 1;
            if checks == 2 {
                cancellation.cancel(CancellationReason::ClientRequest);
            }
            context.check().map_err(CatalogError::control)
        })
        .unwrap_err();
        assert_eq!(error.kind(), CatalogErrorKind::Cancelled);
        assert_eq!(checks, 2);
        assert_eq!(input, encoded.as_bytes());
    }

    #[test]
    fn symbol_envelope_classifier_keeps_noncanonical_and_spoofed_headers_coarse() {
        let (document, _) = crate::oracle_roundtrip_tests::fixture_documents();
        let metadata = crate::oracle_roundtrip_tests::snapshot(document.clone()).metadata();
        let mut envelope = document.extensions[0].clone();
        envelope.namespace = rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE.to_owned();
        let canonical = serde_json::to_string(&envelope).unwrap();
        let reordered = serde_json::to_string(&serde_json::to_value(&envelope).unwrap()).unwrap();
        let escaped = canonical.replacen(
            "dev.rootlight.identity.symbol",
            "dev.rootlight.identity.\\u0073ymbol",
            1,
        );
        for text in [&reordered, &escaped] {
            assert_eq!(
                decode_extension_envelope_with_checkpoint(
                    text.as_bytes(),
                    &IrLimits::default(),
                    || true
                )
                .unwrap(),
                envelope
            );
        }
        let mut other = envelope.clone();
        other.namespace.push_str(".other");
        other.payload = canonical.clone();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE extensions(extension_id, payload)")
            .unwrap();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        assert_eq!(
            symbol_envelope_census(&connection, metadata, 0, &context)
                .unwrap()
                .rows,
            0
        );
        for (id, text, expected) in [
            (envelope.id, canonical.clone(), 1),
            (envelope.id, reordered, 0),
            (envelope.id, escaped, 0),
            (envelope.id, serde_json::to_string(&other).unwrap(), 0),
            (FactId::from_bytes([9; 20]), canonical.clone(), 0),
            (
                envelope.id,
                canonical.replacen(
                    &metadata.repository().to_string(),
                    &RepositoryId::from_bytes([9; 16]).to_string(),
                    1,
                ),
                0,
            ),
            (
                envelope.id,
                canonical.replacen(
                    &metadata.generation().to_string(),
                    &GenerationId::from_bytes([9; 20]).to_string(),
                    1,
                ),
                0,
            ),
        ] {
            connection.execute("DELETE FROM extensions", []).unwrap();
            connection
                .execute(
                    "INSERT INTO extensions VALUES (?1,?2)",
                    rusqlite::params![id.as_bytes().as_slice(), text],
                )
                .unwrap();
            assert_eq!(
                symbol_envelope_census(&connection, metadata, 1, &context)
                    .unwrap()
                    .rows,
                expected
            );
        }
        for sql in [
            "UPDATE extensions SET extension_id=zeroblob(19)",
            "UPDATE extensions SET extension_id=1",
            "UPDATE extensions SET extension_id=zeroblob(20), payload=zeroblob(1024)",
        ] {
            connection.execute_batch(sql).unwrap();
            assert_eq!(
                symbol_envelope_census(&connection, metadata, 1, &context)
                    .unwrap()
                    .rows,
                0
            );
        }
        connection.execute("DELETE FROM extensions", []).unwrap();
        for index in 0_u8..6 {
            envelope.id = FactId::from_bytes([index; 20]);
            envelope.namespace = if index < 4 {
                rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE
            } else {
                "example.other"
            }
            .to_owned();
            connection
                .execute(
                    "INSERT INTO extensions VALUES (?1,?2)",
                    rusqlite::params![
                        envelope.id.as_bytes().as_slice(),
                        serde_json::to_string(&envelope).unwrap()
                    ],
                )
                .unwrap();
        }
        assert_eq!(
            symbol_envelope_census(&connection, metadata, 6, &context)
                .unwrap()
                .rows,
            4
        );
        for expected_rows in [5, 7] {
            assert_eq!(
                symbol_envelope_census(&connection, metadata, expected_rows, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
        }
        assert_eq!(
            symbol_envelope_census(&connection, metadata, u64::MAX, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Storage
        );
        let progress = cancellation.clone();
        connection
            .progress_handler(
                1,
                Some(move || {
                    progress.cancel(CancellationReason::ClientRequest);
                    false
                }),
            )
            .unwrap();
        assert_eq!(
            symbol_envelope_census(&connection, metadata, 6, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
        // A retry must also stop before SQLite work once the token is cancelled.
        assert_eq!(
            symbol_envelope_census(&connection, metadata, 6, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
    }

    #[test]
    fn symbol_envelope_discount_keeps_other_charges_and_adds_full_typed_workspaces() {
        let stats =
            GenerationStats::new(0, 0, 0, 0, 100_000, 0, 0, 0, 0, 32, 0, 200_067, 17).unwrap();
        let baseline = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            185,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        // This provenance-dense frame keeps validation larger than registry and
        // sorting, so every added symbol validation node must remain charged.
        let id_set = tree_node_bytes::<FactId, ()>().unwrap();
        let validation =
            100_001 * (2 * id_set + tree_node_bytes::<FactId, &ProvenanceRecord>().unwrap());
        assert!(validation > 100_034 * tree_node_bytes::<write::RegistryIdentity, ()>().unwrap());
        assert!(validation > typed_record_buffer_bytes::<ProvenanceRecord>(100_000).unwrap());
        assert!(validation > typed_record_buffer_bytes::<ExtensionEnvelope>(32).unwrap());
        for count in [1, 4, 32] {
            let plan = oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                stats,
                185,
                PayloadCardinality {
                    entity_flag_rows: 0,
                    evidence_rows: 0,
                    ..PayloadCardinality::default()
                },
                count,
            )
            .unwrap();
            assert_eq!(
                plan.record_workspace_bytes() + count * 4096,
                baseline.record_workspace_bytes()
                    + count
                        * (u64::try_from(std::mem::size_of::<ExtensionEnvelope>()).unwrap()
                            + id_set)
            );
            assert_eq!(plan.transient_decode_bytes(), 185 * 128);
            assert_eq!(plan.stats(), baseline.stats());
            assert_eq!(
                plan.identity_registry_rows(),
                baseline.identity_registry_rows()
            );
        }
        assert_eq!(
            oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                stats,
                185,
                PayloadCardinality {
                    entity_flag_rows: 0,
                    evidence_rows: 0,
                    ..PayloadCardinality::default()
                },
                33,
            )
            .unwrap_err()
            .kind(),
            CatalogErrorKind::Corrupt
        );
        let no_auxiliary = GenerationStats::new(1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 4, 0).unwrap();
        assert_eq!(
            oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                no_auxiliary,
                0,
                PayloadCardinality {
                    entity_flag_rows: 0,
                    evidence_rows: 0,
                    ..PayloadCardinality::default()
                },
                1,
            )
            .unwrap_err()
            .kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            typed_record_buffer_bytes::<ExtensionEnvelope>(u64::MAX)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Storage
        );
    }

    #[test]
    fn memory_plan_separates_closed_records_and_retains_opaque_envelopes() {
        let stats = stats(17, 1024);
        let plan = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            97,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        assert_eq!(plan.physical_rows(), 17);
        assert_eq!(plan.identity_registry_rows(), 8);
        assert!(plan.record_workspace_bytes() >= 5 * 4096);
        assert!(
            plan.record_workspace_bytes() >= typed_record_buffer_bytes::<FileRecord>(2).unwrap()
        );
        let factor =
            u64::try_from(4 * (std::mem::size_of::<String>() + std::mem::size_of::<usize>()))
                .unwrap();
        assert_eq!(plan.transient_decode_bytes(), 97 * factor);
        assert_eq!(
            plan.materialization_bytes(),
            plan.record_workspace_bytes() + plan.transient_decode_bytes()
        );
        let legacy = oracle_read_memory_plan(
            GenerationContractVersion::new(1, 1),
            self::stats(16, 1024),
            97,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        assert_eq!(legacy.physical_rows(), plan.physical_rows());
        assert_eq!(legacy.materialization_bytes(), plan.materialization_bytes());
        for (rows, payload) in [(u64::MAX, 0), (17, u64::MAX)] {
            let error = oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                self::stats(rows, 1024),
                payload,
                PayloadCardinality {
                    entity_flag_rows: 0,
                    evidence_rows: 0,
                    ..PayloadCardinality::default()
                },
                0,
            )
            .unwrap_err();
            assert_eq!(error.kind(), CatalogErrorKind::Storage);
        }
        let error = oracle_read_memory_plan(
            GenerationContractVersion::new(1, 1),
            self::stats(u64::MAX, 1024),
            0,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap_err();
        assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
    }

    #[test]
    fn large_closed_record_plan_fits_without_reducing_text_or_payload_allowances() {
        let stats = GenerationStats::new(
            1_698,
            562_293,
            1_757_807,
            562_311,
            1_716,
            0,
            13_584,
            100_000,
            36,
            564_256,
            2_320_166,
            9_452_809,
            769_035_567,
        )
        .unwrap();
        let plan = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            100_636,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        let index_per_row = u64::try_from(
            4 * std::mem::size_of::<(SymbolId, usize)>() + 2 * std::mem::size_of::<usize>(),
        )
        .unwrap();
        let admitted = plan
            .materialization_bytes()
            .checked_add(plan.physical_rows().checked_mul(index_per_row).unwrap())
            .unwrap()
            .checked_add(stats.text_bytes().checked_mul(6).unwrap())
            .unwrap()
            .checked_add(128 * 1024 * 1024)
            .unwrap();
        assert!(
            admitted.checked_add(3_867_000_000).unwrap() < 16 * 1024 * 1024 * 1024,
            "closed record and borrowed source workspaces coexist with the resident generation under the unchanged ceiling: {admitted}"
        );
        let oversized = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            512 * 1024 * 1024,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        assert!(oversized.materialization_bytes() > 16 * 1024 * 1024 * 1024);
        assert_eq!(plan.transient_decode_bytes(), 100_636 * 128);
    }

    #[test]
    fn gap_dense_read_plan_fits_with_unchanged_text_and_decode_allowances() {
        let stats = GenerationStats::new(
            6_385,
            160_874,
            1_523_231,
            160_874,
            6_385,
            0,
            51_080,
            1_207_400,
            357,
            172_054,
            1_623_951,
            8_234_921,
            1_442_569_165,
        )
        .unwrap();
        let plan = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            100_636,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        let index_per_row = u64::try_from(
            4 * std::mem::size_of::<(SymbolId, usize)>() + 2 * std::mem::size_of::<usize>(),
        )
        .unwrap();
        let admitted = plan.materialization_bytes()
            + plan.physical_rows() * index_per_row
            + stats.text_bytes() * 6
            + 192 * 1024 * 1024;
        assert!(
            admitted < 16 * 1024 * 1024 * 1024,
            "complete gap details must fit alongside unchanged text, postings and sidecar charges: {admitted}"
        );
        assert_eq!(plan.transient_decode_bytes(), 100_636 * 128);
        assert_eq!(plan.stats(), stats);
        let oversized = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            512 * 1024 * 1024,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        assert!(oversized.materialization_bytes() > 16 * 1024 * 1024 * 1024);
    }

    #[test]
    fn source_reference_layout_rejects_unbounded_scalars_before_owned_decode() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE source_refs(ordinal,repository_id,generation_id,file_id,start_byte,end_byte,content_hash,line_start,line_end);
             INSERT INTO source_refs VALUES (0,zeroblob(16),zeroblob(20),zeroblob(20),0,1,zeroblob(32),0,0);",
        ).unwrap();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        validate_source_reference_layout(&connection, 1, &context).unwrap();
        for (column, expression) in [
            ("repository_id", "zeroblob(1048576)"),
            ("generation_id", "'wrong scalar type'"),
            ("file_id", "zeroblob(19)"),
            ("content_hash", "zeroblob(33)"),
            ("ordinal", "-1"),
            ("start_byte", "-1"),
            ("end_byte", "-1"),
            ("line_end", "-1"),
        ] {
            connection
                .execute_batch("SAVEPOINT corrupt_source")
                .unwrap();
            connection
                .execute_batch(&format!("UPDATE source_refs SET {column}={expression}"))
                .unwrap();
            assert_eq!(
                validate_source_reference_layout(&connection, 1, &context)
                    .unwrap_err()
                    .kind(),
                CatalogErrorKind::Corrupt
            );
            connection
                .execute_batch("ROLLBACK TO corrupt_source; RELEASE corrupt_source")
                .unwrap();
        }
        assert_eq!(
            codec::source_by_ordinal(&[], -1).unwrap_err().kind(),
            CatalogErrorKind::Corrupt
        );
        assert_eq!(
            codec::source_by_ordinal(&[], 0).unwrap_err().kind(),
            CatalogErrorKind::Corrupt
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            validate_source_reference_layout(&connection, 1, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
    }

    #[test]
    fn payload_scan_bounds_each_family_and_measures_bytes_not_characters() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let connection = payload_connection();
        let stats = stats(17, 1024);
        for sql in [
            "UPDATE files SET path_locator_components = ?1 WHERE path_locator_components IS NOT NULL",
            "UPDATE source_mappings SET payload = ?1",
            "UPDATE skipped_regions SET payload = ?1",
            "UPDATE diagnostics SET payload = ?1",
            "UPDATE extensions SET payload = ?1",
        ] {
            let payload = "\u{03bb}".repeat(60);
            connection.execute(sql, [&payload]).unwrap();
            assert_eq!(
                maximum_payload_bytes(&connection, stats, &context).unwrap(),
                120
            );
        }
        connection
            .execute("INSERT INTO files VALUES ('extra')", [])
            .unwrap();
        assert_eq!(
            maximum_payload_bytes(&connection, stats, &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
    }

    #[test]
    fn malformed_nested_payloads_keep_transient_charge_and_decode_limits() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let connection = payload_connection();
        let envelope = ExtensionEnvelope {
            id: FactId::from_bytes([1; 20]),
            repository: RepositoryId::from_bytes([2; 16]),
            generation: GenerationId::from_bytes([3; 20]),
            namespace: "rootlight.test".to_owned(),
            version: "1".to_owned(),
            criticality: rootlight_ir::ExtensionCriticality::Noncritical,
            payload: "{}".to_owned(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        };
        let zeros = vec!["0"; 1024].join(",");
        let invalid_evidence = format!("\"derivation\":[{{\"id\":[{zeros}],\"kind\":\"file\"}}]");
        let encoded = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            decode_extension_envelope_with_checkpoint(
                encoded.as_bytes(),
                &IrLimits::default(),
                || context.check().is_ok()
            )
            .unwrap(),
            envelope
        );
        let malformed = encoded.replacen("\"derivation\":[]", &invalid_evidence, 1);
        connection
            .execute("UPDATE extensions SET payload = ?1", [&malformed])
            .unwrap();
        let mut expected_max = u64::try_from(malformed.len()).unwrap();
        let empty_components = format!("[{}]", vec!["\"\""; 2048].join(","));
        connection.execute(
            "UPDATE files SET path_locator_components = ?1 WHERE path_locator_components IS NOT NULL",
            [&empty_components],
        ).unwrap();
        expected_max = expected_max.max(u64::try_from(empty_components.len()).unwrap());
        let stats = stats(17, 32_768);
        let maximum = maximum_payload_bytes(&connection, stats, &context).unwrap();
        assert_eq!(maximum, expected_max);
        let plan = oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            maximum,
            PayloadCardinality {
                entity_flag_rows: 0,
                evidence_rows: 0,
                ..PayloadCardinality::default()
            },
            0,
        )
        .unwrap();
        let factor =
            u64::try_from(4 * (std::mem::size_of::<String>() + std::mem::size_of::<usize>()))
                .unwrap();
        assert_eq!(plan.transient_decode_bytes(), maximum * factor);
        assert_eq!(
            decode_extension_envelope_with_checkpoint(
                malformed.as_bytes(),
                &IrLimits::default(),
                || context.check().is_ok()
            )
            .unwrap_err(),
            rootlight_ir::ExtensionEnvelopeDecodeError::InvalidEnvelopeShape
        );
        assert_eq!(
            decode_path_locator(Some("unix_bytes_v1".to_owned()), Some(empty_components))
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
    }
}
