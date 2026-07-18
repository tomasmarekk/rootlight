//! Defensive reconstruction of owned generation data from a sealed oracle.
//!
//! Every query has fixed SQL and stable-ID ordering. Persisted scalars are
//! decoded as hostile input before normalized IR validation runs again.

use std::collections::BTreeMap;

use rootlight_ids::{FactId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageRecord, CoverageStatus, EntityFlag, EntityKind,
    EntityRecord, EntityVisibility, EvidenceKind, ExtensionEnvelope, ExtensionEnvelopeDecodeError,
    ExtensionSupport, FactDomain, FactEvidence, FactRef, FileRecord, IrLimits,
    NORMALIZED_IR_VERSION, NormalizedIrDocument, NormalizedRecordDecodeError, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationPredicate, RelationRecord, SkippedRegion, SourceMappingRecord, SourceRef,
    decode_diagnostic_record_with_checkpoint, decode_extension_envelope_with_checkpoint,
    decode_skipped_region_with_checkpoint, decode_source_mapping_record_with_checkpoint,
};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationContext, GenerationContractVersion, GenerationMetadata,
    GenerationResource, GenerationSnapshot, GenerationSnapshotError, GenerationStats,
};
use rusqlite::{Connection, OptionalExtension, Row, types::FromSql};

use crate::{CatalogError, CatalogErrorKind, codec, write};

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
) -> Result<(GenerationMetadata, GenerationStats), CatalogError> {
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
        || raw.ir_major != i64::from(NORMALIZED_IR_VERSION.major())
        || raw.ir_minor != i64::from(NORMALIZED_IR_VERSION.minor())
    {
        return Err(CatalogError::new(CatalogErrorKind::IncompatibleSchema));
    }
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
        .require(GenerationResource::TextBytes, stats.text_bytes())
        .map_err(CatalogError::control)?;
    validate_text_bytes(connection, stats.text_bytes(), context)?;
    Ok((metadata, stats))
}

fn validate_text_bytes(
    connection: &Connection,
    expected: u64,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let observed: i64 = connection
        .query_row(
            "SELECT
                coalesce((SELECT sum(
                    length(CAST(path AS BLOB))
                  + length(CAST(language AS BLOB))
                  + length(CAST(encoding AS BLOB))
                ) FROM files), 0)
              + coalesce((SELECT sum(
                    length(CAST(language AS BLOB))
                  + length(CAST(canonical_name AS BLOB))
                  + length(CAST(display_name AS BLOB))
                  + length(CAST(qualified_name AS BLOB))
                ) FROM entities), 0)
              + coalesce((SELECT sum(length(CAST(syntax_kind AS BLOB)))
                          FROM occurrences), 0)
              + coalesce((SELECT sum(
                    length(CAST(producer_name AS BLOB))
                  + length(CAST(producer_version AS BLOB))
                  + coalesce(length(CAST(frontend_version AS BLOB)), 0)
                  + length(CAST(language AS BLOB))
                  + coalesce(length(CAST(rule AS BLOB)), 0)
                ) FROM provenance), 0)
              + coalesce((SELECT sum(length(CAST(payload AS BLOB)))
                          FROM source_mappings), 0)
              + coalesce((SELECT sum(length(CAST(payload AS BLOB)))
                          FROM skipped_regions), 0)
              + coalesce((SELECT sum(length(CAST(payload AS BLOB)))
                          FROM diagnostics), 0)
              + coalesce((SELECT sum(length(CAST(payload AS BLOB)))
                          FROM extensions), 0)",
            [],
            |row| row.get(0),
        )
        .map_err(CatalogError::sqlite)?;
    let observed = codec::nonnegative_u64(observed)?;
    context
        .require(GenerationResource::TextBytes, observed)
        .map_err(CatalogError::control)?;
    if observed != expected {
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
    let transaction = connection
        .unchecked_transaction()
        .map_err(CatalogError::sqlite)?;
    let (metadata, stats) = read_header(&transaction, context)?;
    if metadata != expected_metadata || stats != expected_stats {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    validate_payload_cardinality(&transaction, metadata.contract_version(), stats, context)?;
    let sources = read_sources(&transaction, stats.source_refs(), context)?;
    let mut evidence = read_evidence(&transaction, context)?;
    let mut flags = read_flags(&transaction, context)?;
    let mut candidates = read_candidates(&transaction, context)?;
    let mut provenance_sources = read_provenance_sources(&transaction, &sources, context)?;
    let mut provenance_derivations = read_provenance_derivations(&transaction, context)?;

    let provenance = read_provenance(
        &transaction,
        &mut provenance_sources,
        &mut provenance_derivations,
        context,
    )?;
    let files = read_files(&transaction, &sources, &mut evidence, context)?;
    let entities = read_entities(&transaction, &sources, &mut evidence, &mut flags, context)?;
    let occurrences = read_occurrences(
        &transaction,
        &sources,
        &mut evidence,
        &mut candidates,
        context,
    )?;
    let relations = read_relations(&transaction, &sources, &mut evidence, context)?;
    let coverage_records = read_coverage(&transaction, &sources, &mut evidence, context)?;
    let document = read_complete_document(
        &transaction,
        metadata,
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
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
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
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
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
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    validate_identity_registry(&transaction, &plan.identities, context)?;
    validate_source_registry(&sources, &plan.source_ordinals, context)?;
    transaction.commit().map_err(CatalogError::sqlite)?;
    Ok(snapshot)
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
    let expected = [
        Some(1),
        Some(identity_rows),
        Some(stats.source_refs()),
        Some(stats.provenance()),
        Some(stats.files()),
        Some(stats.entities()),
        None,
        Some(stats.occurrences()),
        None,
        Some(stats.relations()),
        Some(stats.source_mappings()),
        Some(stats.coverage()),
        Some(stats.skipped_regions()),
        Some(stats.diagnostics()),
        Some(stats.extensions()),
        None,
        None,
        None,
        Some(1),
    ];
    let mut statement = connection
        .prepare(
            "SELECT 0 AS ordinal, count(*) AS row_count FROM generation_meta
             UNION ALL SELECT 1, count(*) FROM identity_registry
             UNION ALL SELECT 2, count(*) FROM source_refs
             UNION ALL SELECT 3, count(*) FROM provenance
             UNION ALL SELECT 4, count(*) FROM files
             UNION ALL SELECT 5, count(*) FROM entities
             UNION ALL SELECT 6, count(*) FROM entity_flags
             UNION ALL SELECT 7, count(*) FROM occurrences
             UNION ALL SELECT 8, count(*) FROM occurrence_candidates
             UNION ALL SELECT 9, count(*) FROM relations
             UNION ALL SELECT 10, count(*) FROM source_mappings
             UNION ALL SELECT 11, count(*) FROM coverage_records
             UNION ALL SELECT 12, count(*) FROM skipped_regions
             UNION ALL SELECT 13, count(*) FROM diagnostics
             UNION ALL SELECT 14, count(*) FROM extensions
             UNION ALL SELECT 15, count(*) FROM evidence_derivations
             UNION ALL SELECT 16, count(*) FROM provenance_sources
             UNION ALL SELECT 17, count(*) FROM provenance_derivations
             UNION ALL SELECT 18, count(*) FROM application_meta
                 WHERE key = 'document_hash'
             ORDER BY ordinal",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut observed_total = 0_u64;
    let mut observed_tables = 0_usize;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let ordinal: i64 = get(row, 0)?;
        if ordinal != usize_to_i64(observed_tables)? || observed_tables >= expected.len() {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        let count = codec::nonnegative_u64(get(row, 1)?)?;
        if expected[observed_tables].is_some_and(|expected| expected != count) {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        observed_total = observed_total
            .checked_add(count)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::Rows, observed_total)
            .map_err(CatalogError::control)?;
        observed_tables += 1;
    }
    let generation_owned_rows = if contract_version == GenerationContractVersion::new(1, 1) {
        observed_total.checked_sub(1)
    } else {
        Some(observed_total)
    }
    .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
    if observed_tables != expected.len() || generation_owned_rows != stats.stored_rows() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
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
    observed: &BTreeMap<i64, SourceRef>,
    expected: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    if observed.len() != expected.len() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    for (source, ordinal) in expected {
        context.check().map_err(CatalogError::control)?;
        if observed.get(ordinal) != Some(source) {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
    }
    Ok(())
}

fn read_sources(
    connection: &Connection,
    expected_count: u64,
    context: &GenerationContext<'_>,
) -> Result<BTreeMap<i64, SourceRef>, CatalogError> {
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
    let mut sources = BTreeMap::new();
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
        if sources.insert(ordinal, source).is_some() {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
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
    sources: &BTreeMap<i64, SourceRef>,
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
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
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
    sources: &BTreeMap<i64, SourceRef>,
    evidence: &mut EvidenceMap,
    context: &GenerationContext<'_>,
) -> Result<Vec<FileRecord>, CatalogError> {
    let mut statement = connection
        .prepare(
            "SELECT
                file_id, repository_id, generation_id, path, content_hash,
                byte_length, language, encoding, generated, provenance_id,
                evidence_source_ordinal
             FROM files
             ORDER BY file_id",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let raw_id: Vec<u8> = get(row, 0)?;
        records.push(FileRecord {
            id: codec::file_id(raw_id.clone())?,
            repository: codec::repository_id(get(row, 1)?)?,
            generation: codec::generation_id(get(row, 2)?)?,
            path: get(row, 3)?,
            content_hash: codec::content_hash(get(row, 4)?)?,
            byte_length: codec::nonnegative_u64(get(row, 5)?)?,
            language: get(row, 6)?,
            encoding: get(row, 7)?,
            generated: codec::bool_value(get(row, 8)?)?,
            provenance: codec::fact_id(get(row, 9)?)?,
            evidence: take_evidence("file", raw_id, get(row, 10)?, sources, evidence)?,
        });
    }
    Ok(records)
}

fn read_entities(
    connection: &Connection,
    sources: &BTreeMap<i64, SourceRef>,
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
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
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
    sources: &BTreeMap<i64, SourceRef>,
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
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
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
    sources: &BTreeMap<i64, SourceRef>,
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
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
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
    sources: &BTreeMap<i64, SourceRef>,
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
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
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
    context: &GenerationContext<'_>,
    decode: impl Fn(&[u8], &GenerationContext<'_>) -> Result<T, CatalogError>,
) -> Result<Vec<T>, CatalogError> {
    let mut statement = connection.prepare(sql).map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    let mut records = Vec::new();
    let mut payload_bytes = 0_u64;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        context.check().map_err(CatalogError::control)?;
        let id = codec::fact_id(get(row, 0)?)?;
        let repository = codec::repository_id(get(row, 1)?)?;
        let generation = codec::generation_id(get(row, 2)?)?;
        let provenance = codec::fact_id(get(row, 3)?)?;
        let payload: String = get(row, 4)?;
        payload_bytes = payload_bytes
            .checked_add(usize_to_u64(payload.len())?)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::TextBytes, payload_bytes)
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
    sources: &BTreeMap<i64, SourceRef>,
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
