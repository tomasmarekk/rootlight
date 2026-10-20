//! Defensive reconstruction of owned generation data from a sealed oracle.
//!
//! Every query has fixed SQL and stable-ID ordering. Persisted scalars are
//! decoded as hostile input before normalized IR validation runs again.

use std::collections::BTreeMap;

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId};
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

type EvidenceMap = BTreeMap<(String, Vec<u8>), Vec<FactRef>>;

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
    let transaction = connection
        .unchecked_transaction()
        .map_err(CatalogError::sqlite)?;
    let (metadata, stats, ir_version) = read_header(&transaction, context)?;
    if metadata != expected_metadata || stats != expected_stats {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt).into());
    }
    validate_payload_cardinality(&transaction, metadata.contract_version(), stats, context)?;
    let maximum_payload_bytes = maximum_payload_bytes(&transaction, stats, context)?;
    let memory_plan =
        oracle_read_memory_plan(metadata.contract_version(), stats, maximum_payload_bytes)?;
    validate_identity_registry_layout(&transaction, memory_plan.identity_registry_rows(), context)?;
    validate_source_reference_layout(&transaction, stats.source_refs(), context)?;
    admit(memory_plan).map_err(OracleReadAdmissionError::Admission)?;
    context.check().map_err(CatalogError::control)?;
    let sources = read_sources(&transaction, stats.source_refs(), context)?;
    let mut evidence = read_evidence(&transaction, context)?;
    let mut flags = read_flags(&transaction, context)?;
    let mut candidates = read_candidates(&transaction, context)?;
    let mut provenance_sources = read_provenance_sources(&transaction, &sources, context)?;
    let mut provenance_derivations = read_provenance_derivations(&transaction, context)?;

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
    let snapshot = GenerationSnapshot::new_with_context(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        context,
    )
    .map_err(|error| match error {
        GenerationSnapshotError::Control(error) => CatalogError::control(error),
        GenerationSnapshotError::Validation(error) => CatalogError::corrupt_generation(error),
    })?;
    context.check().map_err(CatalogError::control)?;
    let plan = write::measure(&snapshot, context)?;
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
    transaction.commit().map_err(CatalogError::sqlite)?;
    Ok(snapshot)
}

pub(crate) fn oracle_read_memory_plan(
    contract_version: GenerationContractVersion,
    stats: GenerationStats,
    maximum_payload_bytes: u64,
) -> Result<OracleReadMemoryPlan, CatalogError> {
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

    // Exact buffers are allocated only after the same-transaction census and
    // admission. Canonicalization sorts one collection at a time; its validation
    // trees are dropped before snapshot indexes and writer registry measurement
    // begin. These temporary stages therefore share their largest workspace.
    // Gap and diagnostic JSON decodes to closed record shapes too. Their text
    // and evidence vectors remain charged through encoded text, while the
    // largest single-payload decoder has its own transient allowance. Other
    // opaque payloads and child-row maps retain the four-KiB envelope.
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
    let record_workspace_bytes = opaque_and_auxiliary_rows
        .checked_mul(4 * 1024)
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
        physical_rows,
        identity_registry_rows,
        record_workspace_bytes,
        maximum_payload_bytes,
        transient_decode_bytes,
        materialization_bytes,
    })
}

fn tree_node_bytes<K, V>() -> Result<u64, CatalogError> {
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

fn validate_payload_cardinality(
    connection: &Connection,
    contract_version: GenerationContractVersion,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
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
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM entity_flags LIMIT ?1
             )",
            None,
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM occurrences LIMIT ?1
             )",
            Some(stats.occurrences()),
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM occurrence_candidates LIMIT ?1
             )",
            None,
        ),
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
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM evidence_derivations LIMIT ?1
             )",
            None,
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM provenance_sources LIMIT ?1
             )",
            None,
        ),
        (
            "SELECT count(*) FROM (
                SELECT 1 FROM provenance_derivations LIMIT ?1
             )",
            None,
        ),
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
    Ok(())
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
    context: &GenerationContext<'_>,
) -> Result<EvidenceMap, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT owner_kind, owner_id, position, reference_kind, reference_id
             FROM evidence_derivations
             ORDER BY owner_kind, owner_id, position",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut values = BTreeMap::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let owner_kind: String = get(row, 0)?;
        let owner_id: Vec<u8> = get(row, 1)?;
        let position: i64 = get(row, 2)?;
        let reference_kind: String = get(row, 3)?;
        let reference_id: Vec<u8> = get(row, 4)?;
        push_positioned(
            &mut values,
            (owner_kind, owner_id),
            position,
            codec::decode_fact_ref(&reference_kind, reference_id)?,
        )?;
    }
    Ok(values)
}

fn read_flags(
    connection: &Connection,
    context: &GenerationContext<'_>,
) -> Result<BTreeMap<SymbolId, Vec<EntityFlag>>, CatalogError> {
    let mut statement = connection
        .prepare("SELECT entity_id, flag FROM entity_flags ORDER BY entity_id, flag")
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut values: BTreeMap<_, Vec<_>> = BTreeMap::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        values
            .entry(codec::symbol_id(get(row, 0)?)?)
            .or_default()
            .push(codec::decode_enum(get(row, 1)?)?);
    }
    for flags in values.values_mut() {
        flags.sort_unstable();
    }
    Ok(values)
}

fn read_candidates(
    connection: &Connection,
    context: &GenerationContext<'_>,
) -> Result<BTreeMap<FactId, Vec<SymbolId>>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT occurrence_id, position, entity_id
             FROM occurrence_candidates
             ORDER BY occurrence_id, position",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut values = BTreeMap::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        push_positioned(
            &mut values,
            codec::fact_id(get(row, 0)?)?,
            get(row, 1)?,
            codec::symbol_id(get(row, 2)?)?,
        )?;
    }
    Ok(values)
}

fn read_provenance_sources(
    connection: &Connection,
    sources: &[SourceRef],
    context: &GenerationContext<'_>,
) -> Result<BTreeMap<(FactId, String), Vec<SourceRef>>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT provenance_id, source_kind, position, source_ordinal
             FROM provenance_sources
             ORDER BY provenance_id, source_kind, position",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut values = BTreeMap::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        push_positioned(
            &mut values,
            (codec::fact_id(get(row, 0)?)?, get(row, 1)?),
            get(row, 2)?,
            codec::source_by_ordinal(sources, get(row, 3)?)?,
        )?;
    }
    Ok(values)
}

fn read_provenance_derivations(
    connection: &Connection,
    context: &GenerationContext<'_>,
) -> Result<BTreeMap<FactId, Vec<FactRef>>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT provenance_id, position, reference_kind, reference_id
             FROM provenance_derivations
             ORDER BY provenance_id, position",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut values = BTreeMap::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let kind: String = get(row, 2)?;
        push_positioned(
            &mut values,
            codec::fact_id(get(row, 0)?)?,
            get(row, 1)?,
            codec::decode_fact_ref(&kind, get(row, 3)?)?,
        )?;
    }
    Ok(values)
}

fn read_provenance(
    connection: &Connection,
    expected_count: u64,
    sources: &mut BTreeMap<(FactId, String), Vec<SourceRef>>,
    derivations: &mut BTreeMap<FactId, Vec<FactRef>>,
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
            input_sources: sources
                .remove(&(id, "input".to_owned()))
                .unwrap_or_default(),
            evidence_sources: sources
                .remove(&(id, "evidence".to_owned()))
                .unwrap_or_default(),
            derivation_parents: derivations.remove(&id).unwrap_or_default(),
            rule: get(row, 12)?,
        });
    }
    Ok(records)
}

fn read_files(
    connection: &Connection,
    expected_count: u64,
    sources: &[SourceRef],
    evidence: &mut EvidenceMap,
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
    evidence: &mut EvidenceMap,
    flags: &mut BTreeMap<SymbolId, Vec<EntityFlag>>,
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
            flags: flags.remove(&id).unwrap_or_default(),
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
    evidence: &mut EvidenceMap,
    candidates: &mut BTreeMap<FactId, Vec<SymbolId>>,
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
            candidates.remove(&id).unwrap_or_default(),
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
    evidence: &mut EvidenceMap,
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
    evidence: &mut EvidenceMap,
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
        let payload: String = get(row, 4)?;
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
    evidence: &mut EvidenceMap,
) -> Result<FactEvidence, CatalogError> {
    Ok(FactEvidence {
        source: codec::optional_source_by_ordinal(sources, source_ordinal)?,
        derivation: evidence
            .remove(&(owner_kind.to_owned(), owner_id))
            .unwrap_or_default(),
    })
}

fn push_positioned<K: Ord, V>(
    values: &mut BTreeMap<K, Vec<V>>,
    key: K,
    position: i64,
    value: V,
) -> Result<(), CatalogError> {
    let group = values.entry(key).or_default();
    if position != usize_to_i64(group.len())? {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    group.push(value);
    Ok(())
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
        let measured = oracle_read_memory_plan(
            metadata.contract_version(),
            written.stats,
            written.maximum_payload_bytes,
        )
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

    #[test]
    fn memory_plan_separates_closed_records_and_retains_opaque_envelopes() {
        let stats = stats(17, 1024);
        let plan = oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 97).unwrap();
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
        )
        .unwrap();
        assert_eq!(legacy.physical_rows(), plan.physical_rows());
        assert_eq!(legacy.materialization_bytes(), plan.materialization_bytes());
        for (rows, payload) in [(u64::MAX, 0), (17, u64::MAX)] {
            let error = oracle_read_memory_plan(
                GENERATION_CONTRACT_VERSION,
                self::stats(rows, 1024),
                payload,
            )
            .unwrap_err();
            assert_eq!(error.kind(), CatalogErrorKind::Storage);
        }
        let error = oracle_read_memory_plan(
            GenerationContractVersion::new(1, 1),
            self::stats(u64::MAX, 1024),
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
        let plan = oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 100_636).unwrap();
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
        let oversized =
            oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 512 * 1024 * 1024).unwrap();
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
        let plan = oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 100_636).unwrap();
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
        let oversized =
            oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, 512 * 1024 * 1024).unwrap();
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
        let plan = oracle_read_memory_plan(GENERATION_CONTRACT_VERSION, stats, maximum).unwrap();
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
