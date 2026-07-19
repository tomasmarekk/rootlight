//! Bounded indexed reconstruction from a sealed SQLite oracle.
//!
//! Every collection query uses parameterized `LIMIT + 1`, stable fact-ID
//! ordering, cooperative cancellation, and the caller's generation budget.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{FactId, FileId, SymbolId};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageRecord, CoverageScope, CoverageStatus, EntityFlag,
    EntityKind, EntityRecord, EntityVisibility, EvidenceKind, FactDomain, FactEvidence, FactRef,
    FilePathLocator, FilePathLocatorEncoding, FileRecord, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord, RelationEndpoint,
    RelationPredicate, RelationRecord, SourceRef, derive_coverage_record_id_with_checkpoint,
    derive_occurrence_record_id_with_checkpoint, derive_provenance_record_id_with_checkpoint,
    derive_relation_record_id_with_checkpoint,
};
use rootlight_storage::{
    CoverageReadRequest, GenerationContext, GenerationMetadata, GenerationReadLimit,
    GenerationResource, OccurrenceReadRequest, ReadPage, RelationReadDirection,
    RelationReadRequest,
};
use rusqlite::{
    Connection, Params, Row, params, params_from_iter,
    types::{FromSql, Value},
};

use crate::{CatalogError, CatalogErrorKind, codec};

struct IndexedReadState {
    rows: u64,
    text_bytes: u64,
    source_ordinals: BTreeSet<i64>,
    sources: BTreeMap<i64, SourceRef>,
}

impl IndexedReadState {
    fn new(context: &GenerationContext<'_>) -> Result<Self, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        Ok(Self {
            rows: 0,
            text_bytes: 0,
            source_ordinals: BTreeSet::new(),
            sources: BTreeMap::new(),
        })
    }

    fn observe_row(&mut self, context: &GenerationContext<'_>) -> Result<(), CatalogError> {
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::Rows, self.rows)
            .map_err(CatalogError::control)
    }

    fn observe_text(
        &mut self,
        value: &str,
        context: &GenerationContext<'_>,
    ) -> Result<(), CatalogError> {
        let length =
            u64::try_from(value.len()).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
        self.text_bytes = self
            .text_bytes
            .checked_add(length)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        context
            .require(GenerationResource::TextBytes, self.text_bytes)
            .map_err(CatalogError::control)
    }

    fn observe_optional_text(
        &mut self,
        value: Option<&str>,
        context: &GenerationContext<'_>,
    ) -> Result<(), CatalogError> {
        if let Some(value) = value {
            self.observe_text(value, context)?;
        }
        Ok(())
    }

    fn observe_source(
        &mut self,
        ordinal: i64,
        context: &GenerationContext<'_>,
    ) -> Result<(), CatalogError> {
        if self.source_ordinals.insert(ordinal) {
            let observed = u64::try_from(self.source_ordinals.len())
                .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
            context
                .require(GenerationResource::SourceReferences, observed)
                .map_err(CatalogError::control)?;
        }
        Ok(())
    }

    fn child_probe_limit(&self, context: &GenerationContext<'_>) -> Result<i64, CatalogError> {
        let remaining = context
            .budget()
            .limit(GenerationResource::Rows)
            .saturating_sub(self.rows);
        let probe = remaining
            .checked_add(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        codec::sqlite_i64(probe)
    }

    fn source(
        &mut self,
        connection: &Connection,
        ordinal: i64,
        metadata: GenerationMetadata,
        context: &GenerationContext<'_>,
    ) -> Result<SourceRef, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        if let Some(source) = self.sources.get(&ordinal) {
            return Ok(source.clone());
        }
        let source = query_optional(
            connection,
            "SELECT
                repository_id, generation_id, file_id, start_byte, end_byte,
                content_hash, line_start, line_end
             FROM source_refs
             WHERE ordinal = ?1
             LIMIT ?2",
            params![ordinal, 2_i64],
            self,
            context,
            |row| {
                codec::source_ref(
                    get(row, 0)?,
                    get(row, 1)?,
                    get(row, 2)?,
                    get(row, 3)?,
                    get(row, 4)?,
                    get(row, 5)?,
                    get(row, 6)?,
                    get(row, 7)?,
                )
            },
        )?
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?;
        if source.repository() != metadata.repository()
            || source.generation() != metadata.generation()
        {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        self.observe_source(ordinal, context)?;
        self.sources.insert(ordinal, source.clone());
        Ok(source)
    }
}

struct RawFile {
    id: FileId,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    path: String,
    locator_encoding: Option<String>,
    locator_components: Option<String>,
    content_hash: rootlight_ids::ContentHash,
    byte_length: u64,
    language: String,
    encoding: String,
    generated: bool,
    provenance: FactId,
    evidence_source: Option<i64>,
}

struct RawEntity {
    id: SymbolId,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    kind: EntityKind,
    language: String,
    tier: AnalysisTier,
    canonical_name: String,
    display_name: String,
    qualified_name: String,
    container_kind: Option<String>,
    container_id: Option<Vec<u8>>,
    visibility: EntityVisibility,
    provenance: FactId,
    evidence_source: Option<i64>,
}

struct RawOccurrence {
    id: FactId,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    file: FileId,
    source_ordinal: i64,
    role: OccurrenceRole,
    enclosing: Option<SymbolId>,
    target_kind: String,
    target_symbol: Option<Vec<u8>>,
    target_hash: Option<Vec<u8>>,
    target_total: Option<i64>,
    target_completeness: Option<String>,
    syntactic_text_hash: rootlight_ids::ContentHash,
    syntax_kind: String,
    provenance: FactId,
    confidence: rootlight_ir::Confidence,
    evidence_source: Option<i64>,
}

struct RawRelation {
    id: FactId,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    subject: RelationEndpoint,
    predicate: RelationPredicate,
    object: RelationEndpoint,
    confidence: rootlight_ir::Confidence,
    evidence_kind: EvidenceKind,
    provenance: FactId,
    evidence_source: Option<i64>,
}

struct RawProvenance {
    id: FactId,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    producer_kind: ProducerKind,
    producer_name: String,
    producer_version: String,
    producer_configuration_hash: rootlight_ids::ContentHash,
    binary_digest: rootlight_ids::ContentHash,
    frontend_version: Option<String>,
    language: String,
    tier: AnalysisTier,
    build_context_digest: rootlight_ids::ContentHash,
    rule: Option<String>,
}

struct RawCoverage {
    id: FactId,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    scope: CoverageScope,
    domain: FactDomain,
    tier: AnalysisTier,
    status: CoverageStatus,
    discovered: u64,
    indexed: u64,
    skipped: u64,
    provenance: FactId,
    evidence_source: Option<i64>,
}

pub(crate) fn file(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FileId,
    context: &GenerationContext<'_>,
) -> Result<Option<FileRecord>, CatalogError> {
    let mut state = IndexedReadState::new(context)?;
    read_file(connection, metadata, id, context, &mut state)
}

pub(crate) fn entity(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: SymbolId,
    context: &GenerationContext<'_>,
) -> Result<Option<EntityRecord>, CatalogError> {
    let mut state = IndexedReadState::new(context)?;
    read_entity(connection, metadata, id, context, &mut state)
}

pub(crate) fn relations(
    connection: &Connection,
    metadata: GenerationMetadata,
    maximum_total: u64,
    request: &RelationReadRequest,
    context: &GenerationContext<'_>,
) -> Result<ReadPage<RelationRecord>, CatalogError> {
    let mut state = IndexedReadState::new(context)?;
    let (filter, mut parameters) = relation_filter(request)?;
    let total = query_count(
        connection,
        &format!("SELECT count(*) FROM relations WHERE {filter}"),
        &parameters,
        &mut state,
        context,
    )?;
    require_total(total, maximum_total)?;

    let mut sql = format!("SELECT relation_id FROM relations WHERE {filter}");
    if let Some(after) = request.after() {
        sql.push_str(" AND relation_id > ?");
        parameters.push(Value::Blob(after.as_bytes().to_vec()));
    }
    sql.push_str(" ORDER BY relation_id LIMIT ?");
    parameters.push(Value::Integer(limit_plus_one(request.limit())));
    let ids = query_fact_ids(connection, &sql, &parameters, &mut state, context)?;
    let mut records = Vec::new();
    records
        .try_reserve(ids.len())
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    for id in ids {
        context.check().map_err(CatalogError::control)?;
        records.push(
            read_relation(connection, metadata, id, context, &mut state)?
                .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?,
        );
    }
    ReadPage::from_limit_plus_one(records, total, request.limit(), |record| record.id)
        .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

pub(crate) fn occurrences(
    connection: &Connection,
    metadata: GenerationMetadata,
    maximum_total: u64,
    request: &OccurrenceReadRequest,
    context: &GenerationContext<'_>,
) -> Result<ReadPage<OccurrenceRecord>, CatalogError> {
    let mut state = IndexedReadState::new(context)?;
    let file = request.file().as_bytes().to_vec();
    let total = query_count(
        connection,
        "SELECT count(*) FROM occurrences WHERE file_id = ?",
        &[Value::Blob(file.clone())],
        &mut state,
        context,
    )?;
    require_total(total, maximum_total)?;

    let mut parameters = vec![Value::Blob(file)];
    let mut sql = "SELECT occurrence_id FROM occurrences WHERE file_id = ?".to_owned();
    if let Some(after) = request.after() {
        sql.push_str(" AND occurrence_id > ?");
        parameters.push(Value::Blob(after.as_bytes().to_vec()));
    }
    sql.push_str(" ORDER BY occurrence_id LIMIT ?");
    parameters.push(Value::Integer(limit_plus_one(request.limit())));
    let ids = query_fact_ids(connection, &sql, &parameters, &mut state, context)?;
    let mut records = Vec::new();
    records
        .try_reserve(ids.len())
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    for id in ids {
        context.check().map_err(CatalogError::control)?;
        records.push(
            read_occurrence(connection, metadata, id, context, &mut state)?
                .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?,
        );
    }
    ReadPage::from_limit_plus_one(records, total, request.limit(), |record| record.id)
        .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

pub(crate) fn provenance(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FactId,
    context: &GenerationContext<'_>,
) -> Result<Option<ProvenanceRecord>, CatalogError> {
    let mut state = IndexedReadState::new(context)?;
    read_provenance(connection, metadata, id, context, &mut state)
}

pub(crate) fn coverage(
    connection: &Connection,
    metadata: GenerationMetadata,
    maximum_total: u64,
    request: &CoverageReadRequest,
    context: &GenerationContext<'_>,
) -> Result<ReadPage<CoverageRecord>, CatalogError> {
    let mut state = IndexedReadState::new(context)?;
    let scope = request.scope();
    let (scope_kind, scope_id) = codec::encode_scope(&scope);
    let total = query_count(
        connection,
        "SELECT count(*) FROM coverage_records
         WHERE scope_kind = ? AND scope_id = ?",
        &[
            Value::Text(scope_kind.to_owned()),
            Value::Blob(scope_id.to_vec()),
        ],
        &mut state,
        context,
    )?;
    require_total(total, maximum_total)?;

    let mut parameters = vec![
        Value::Text(scope_kind.to_owned()),
        Value::Blob(scope_id.to_vec()),
    ];
    let mut sql = "SELECT coverage_id FROM coverage_records
                   WHERE scope_kind = ? AND scope_id = ?"
        .to_owned();
    if let Some(after) = request.after() {
        sql.push_str(" AND coverage_id > ?");
        parameters.push(Value::Blob(after.as_bytes().to_vec()));
    }
    sql.push_str(" ORDER BY coverage_id LIMIT ?");
    parameters.push(Value::Integer(limit_plus_one(request.limit())));
    let ids = query_fact_ids(connection, &sql, &parameters, &mut state, context)?;
    let mut records = Vec::new();
    records
        .try_reserve(ids.len())
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    for id in ids {
        context.check().map_err(CatalogError::control)?;
        records.push(
            read_coverage(connection, metadata, id, context, &mut state)?
                .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))?,
        );
    }
    ReadPage::from_limit_plus_one(records, total, request.limit(), |record| record.id)
        .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

fn read_file(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FileId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Option<FileRecord>, CatalogError> {
    let raw = query_optional(
        connection,
        "SELECT
            file_id, repository_id, generation_id, path, path_locator_encoding,
            path_locator_components, content_hash, byte_length, language, encoding,
            generated, provenance_id, evidence_source_ordinal
         FROM files
         WHERE file_id = ?1
         LIMIT ?2",
        params![id.as_bytes().as_slice(), 2_i64],
        state,
        context,
        |row| {
            Ok(RawFile {
                id: codec::file_id(get(row, 0)?)?,
                repository: codec::repository_id(get(row, 1)?)?,
                generation: codec::generation_id(get(row, 2)?)?,
                path: get(row, 3)?,
                locator_encoding: get(row, 4)?,
                locator_components: get(row, 5)?,
                content_hash: codec::content_hash(get(row, 6)?)?,
                byte_length: codec::nonnegative_u64(get(row, 7)?)?,
                language: get(row, 8)?,
                encoding: get(row, 9)?,
                generated: codec::bool_value(get(row, 10)?)?,
                provenance: codec::fact_id(get(row, 11)?)?,
                evidence_source: get(row, 12)?,
            })
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    require_owner(metadata, raw.repository, raw.generation)?;
    if raw.id != id {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    state.observe_text(&raw.path, context)?;
    state.observe_optional_text(raw.locator_encoding.as_deref(), context)?;
    state.observe_optional_text(raw.locator_components.as_deref(), context)?;
    state.observe_text(&raw.language, context)?;
    state.observe_text(&raw.encoding, context)?;
    let path_locator = decode_path_locator(raw.locator_encoding, raw.locator_components)?;
    let evidence = read_evidence(
        connection,
        "file",
        raw.id.as_bytes(),
        raw.evidence_source,
        metadata,
        context,
        state,
    )?;
    Ok(Some(FileRecord {
        id: raw.id,
        repository: raw.repository,
        generation: raw.generation,
        path: raw.path,
        path_locator,
        content_hash: raw.content_hash,
        byte_length: raw.byte_length,
        language: raw.language,
        encoding: raw.encoding,
        generated: raw.generated,
        provenance: raw.provenance,
        evidence,
    }))
}

fn read_entity(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: SymbolId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Option<EntityRecord>, CatalogError> {
    let raw = query_optional(
        connection,
        "SELECT
            entity_id, repository_id, generation_id, kind, language, tier,
            canonical_name, display_name, qualified_name, container_kind,
            container_id, visibility, provenance_id, evidence_source_ordinal
         FROM entities
         WHERE entity_id = ?1
         LIMIT ?2",
        params![id.as_bytes().as_slice(), 2_i64],
        state,
        context,
        |row| {
            Ok(RawEntity {
                id: codec::symbol_id(get(row, 0)?)?,
                repository: codec::repository_id(get(row, 1)?)?,
                generation: codec::generation_id(get(row, 2)?)?,
                kind: codec::decode_enum(get(row, 3)?)?,
                language: get(row, 4)?,
                tier: codec::decode_enum(get(row, 5)?)?,
                canonical_name: get(row, 6)?,
                display_name: get(row, 7)?,
                qualified_name: get(row, 8)?,
                container_kind: get(row, 9)?,
                container_id: get(row, 10)?,
                visibility: codec::decode_enum(get(row, 11)?)?,
                provenance: codec::fact_id(get(row, 12)?)?,
                evidence_source: get(row, 13)?,
            })
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    require_owner(metadata, raw.repository, raw.generation)?;
    if raw.id != id {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    state.observe_text(&raw.language, context)?;
    state.observe_text(&raw.canonical_name, context)?;
    state.observe_text(&raw.display_name, context)?;
    state.observe_text(&raw.qualified_name, context)?;
    let flags = read_entity_flags(connection, raw.id, context, state)?;
    let evidence = read_evidence(
        connection,
        "entity",
        raw.id.as_bytes(),
        raw.evidence_source,
        metadata,
        context,
        state,
    )?;
    Ok(Some(EntityRecord {
        id: raw.id,
        repository: raw.repository,
        generation: raw.generation,
        kind: raw.kind,
        language: raw.language,
        tier: raw.tier,
        canonical_name: raw.canonical_name,
        display_name: raw.display_name,
        qualified_name: raw.qualified_name,
        container: codec::decode_container(raw.container_kind, raw.container_id)?,
        visibility: raw.visibility,
        flags,
        provenance: raw.provenance,
        evidence,
    }))
}

fn read_occurrence(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FactId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Option<OccurrenceRecord>, CatalogError> {
    let raw = query_optional(
        connection,
        "SELECT
            occurrence_id, repository_id, generation_id, file_id,
            source_ordinal, role, enclosing_entity_id, target_kind,
            target_symbol_id, target_text_hash, target_total_count,
            target_completeness, syntactic_text_hash, syntax_kind,
            provenance_id, confidence, evidence_source_ordinal
         FROM occurrences
         WHERE occurrence_id = ?1
         LIMIT ?2",
        params![id.as_bytes().as_slice(), 2_i64],
        state,
        context,
        |row| {
            Ok(RawOccurrence {
                id: codec::fact_id(get(row, 0)?)?,
                repository: codec::repository_id(get(row, 1)?)?,
                generation: codec::generation_id(get(row, 2)?)?,
                file: codec::file_id(get(row, 3)?)?,
                source_ordinal: get(row, 4)?,
                role: codec::decode_enum(get(row, 5)?)?,
                enclosing: codec::optional_symbol_id(get(row, 6)?)?,
                target_kind: get(row, 7)?,
                target_symbol: get(row, 8)?,
                target_hash: get(row, 9)?,
                target_total: get(row, 10)?,
                target_completeness: get(row, 11)?,
                syntactic_text_hash: codec::content_hash(get(row, 12)?)?,
                syntax_kind: get(row, 13)?,
                provenance: codec::fact_id(get(row, 14)?)?,
                confidence: codec::confidence(get(row, 15)?)?,
                evidence_source: get(row, 16)?,
            })
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    require_owner(metadata, raw.repository, raw.generation)?;
    if raw.id != id {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    state.observe_text(&raw.syntax_kind, context)?;
    let source = state.source(connection, raw.source_ordinal, metadata, context)?;
    let candidates = read_occurrence_candidates(connection, raw.id, context, state)?;
    let target = decode_target(
        &raw.target_kind,
        raw.target_symbol,
        raw.target_hash,
        raw.target_total,
        raw.target_completeness,
        candidates,
    )?;
    let evidence = read_evidence(
        connection,
        "fact",
        raw.id.as_bytes(),
        raw.evidence_source,
        metadata,
        context,
        state,
    )?;
    let record = OccurrenceRecord {
        id: raw.id,
        repository: raw.repository,
        generation: raw.generation,
        file: raw.file,
        source,
        role: raw.role,
        enclosing: raw.enclosing,
        target,
        syntactic_text_hash: raw.syntactic_text_hash,
        syntax_kind: raw.syntax_kind,
        provenance: raw.provenance,
        confidence: raw.confidence,
        evidence,
    };
    require_fact_id(
        record.id,
        derive_occurrence_record_id_with_checkpoint(&record, || context.check().is_ok()),
        context,
    )?;
    Ok(Some(record))
}

fn read_relation(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FactId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Option<RelationRecord>, CatalogError> {
    let raw = query_optional(
        connection,
        "SELECT
            relation_id, repository_id, generation_id, subject_kind,
            subject_id, predicate, object_kind, object_id, confidence,
            evidence_kind, provenance_id, evidence_source_ordinal
         FROM relations
         WHERE relation_id = ?1
         LIMIT ?2",
        params![id.as_bytes().as_slice(), 2_i64],
        state,
        context,
        |row| {
            Ok(RawRelation {
                id: codec::fact_id(get(row, 0)?)?,
                repository: codec::repository_id(get(row, 1)?)?,
                generation: codec::generation_id(get(row, 2)?)?,
                subject: codec::decode_endpoint(&get::<String>(row, 3)?, get(row, 4)?)?,
                predicate: codec::decode_enum(get(row, 5)?)?,
                object: codec::decode_endpoint(&get::<String>(row, 6)?, get(row, 7)?)?,
                confidence: codec::confidence(get(row, 8)?)?,
                evidence_kind: codec::decode_enum(get(row, 9)?)?,
                provenance: codec::fact_id(get(row, 10)?)?,
                evidence_source: get(row, 11)?,
            })
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    require_owner(metadata, raw.repository, raw.generation)?;
    if raw.id != id {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let evidence = read_evidence(
        connection,
        "fact",
        raw.id.as_bytes(),
        raw.evidence_source,
        metadata,
        context,
        state,
    )?;
    let record = RelationRecord {
        id: raw.id,
        repository: raw.repository,
        generation: raw.generation,
        subject: raw.subject,
        predicate: raw.predicate,
        object: raw.object,
        confidence: raw.confidence,
        evidence_kind: raw.evidence_kind,
        provenance: raw.provenance,
        evidence,
    };
    require_fact_id(
        record.id,
        derive_relation_record_id_with_checkpoint(&record, || context.check().is_ok()),
        context,
    )?;
    Ok(Some(record))
}

fn read_provenance(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FactId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Option<ProvenanceRecord>, CatalogError> {
    let raw = query_optional(
        connection,
        "SELECT
            provenance_id, repository_id, generation_id, producer_kind,
            producer_name, producer_version, producer_configuration_hash,
            binary_digest, frontend_version, language, tier,
            build_context_digest, rule
         FROM provenance
         WHERE provenance_id = ?1
         LIMIT ?2",
        params![id.as_bytes().as_slice(), 2_i64],
        state,
        context,
        |row| {
            Ok(RawProvenance {
                id: codec::fact_id(get(row, 0)?)?,
                repository: codec::repository_id(get(row, 1)?)?,
                generation: codec::generation_id(get(row, 2)?)?,
                producer_kind: codec::decode_enum(get(row, 3)?)?,
                producer_name: get(row, 4)?,
                producer_version: get(row, 5)?,
                producer_configuration_hash: codec::content_hash(get(row, 6)?)?,
                binary_digest: codec::content_hash(get(row, 7)?)?,
                frontend_version: get(row, 8)?,
                language: get(row, 9)?,
                tier: codec::decode_enum(get(row, 10)?)?,
                build_context_digest: codec::content_hash(get(row, 11)?)?,
                rule: get(row, 12)?,
            })
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    require_owner(metadata, raw.repository, raw.generation)?;
    if raw.id != id {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    state.observe_text(&raw.producer_name, context)?;
    state.observe_text(&raw.producer_version, context)?;
    state.observe_optional_text(raw.frontend_version.as_deref(), context)?;
    state.observe_text(&raw.language, context)?;
    state.observe_optional_text(raw.rule.as_deref(), context)?;
    let producer = ProducerIdentity::new(
        &raw.producer_name,
        &raw.producer_version,
        raw.producer_configuration_hash,
    )
    .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
    let input_sources =
        read_provenance_sources(connection, raw.id, "input", metadata, context, state)?;
    let evidence_sources =
        read_provenance_sources(connection, raw.id, "evidence", metadata, context, state)?;
    let derivation_parents = read_provenance_derivations(connection, raw.id, context, state)?;
    let record = ProvenanceRecord {
        id: raw.id,
        repository: raw.repository,
        generation: raw.generation,
        producer_kind: raw.producer_kind,
        producer,
        binary_digest: raw.binary_digest,
        frontend_version: raw.frontend_version,
        language: raw.language,
        tier: raw.tier,
        build_context: BuildContextIdentity::new(raw.build_context_digest),
        input_sources,
        evidence_sources,
        derivation_parents,
        rule: raw.rule,
    };
    require_fact_id(
        record.id,
        derive_provenance_record_id_with_checkpoint(&record, || context.check().is_ok()),
        context,
    )?;
    Ok(Some(record))
}

fn read_coverage(
    connection: &Connection,
    metadata: GenerationMetadata,
    id: FactId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Option<CoverageRecord>, CatalogError> {
    let raw = query_optional(
        connection,
        "SELECT
            coverage_id, repository_id, generation_id, scope_kind, scope_id,
            domain, tier, status, discovered, indexed, skipped,
            provenance_id, evidence_source_ordinal
         FROM coverage_records
         WHERE coverage_id = ?1
         LIMIT ?2",
        params![id.as_bytes().as_slice(), 2_i64],
        state,
        context,
        |row| {
            Ok(RawCoverage {
                id: codec::fact_id(get(row, 0)?)?,
                repository: codec::repository_id(get(row, 1)?)?,
                generation: codec::generation_id(get(row, 2)?)?,
                scope: codec::decode_scope(&get::<String>(row, 3)?, get(row, 4)?)?,
                domain: codec::decode_enum(get(row, 5)?)?,
                tier: codec::decode_enum(get(row, 6)?)?,
                status: codec::decode_enum(get(row, 7)?)?,
                discovered: codec::nonnegative_u64(get(row, 8)?)?,
                indexed: codec::nonnegative_u64(get(row, 9)?)?,
                skipped: codec::nonnegative_u64(get(row, 10)?)?,
                provenance: codec::fact_id(get(row, 11)?)?,
                evidence_source: get(row, 12)?,
            })
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    require_owner(metadata, raw.repository, raw.generation)?;
    if raw.id != id
        || raw.indexed > raw.discovered
        || raw.skipped > raw.discovered.saturating_sub(raw.indexed)
    {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let evidence = read_evidence(
        connection,
        "fact",
        raw.id.as_bytes(),
        raw.evidence_source,
        metadata,
        context,
        state,
    )?;
    let record = CoverageRecord {
        id: raw.id,
        repository: raw.repository,
        generation: raw.generation,
        scope: raw.scope,
        domain: raw.domain,
        tier: raw.tier,
        status: raw.status,
        discovered: raw.discovered,
        indexed: raw.indexed,
        skipped: raw.skipped,
        provenance: raw.provenance,
        evidence,
    };
    require_fact_id(
        record.id,
        derive_coverage_record_id_with_checkpoint(&record, || context.check().is_ok()),
        context,
    )?;
    Ok(Some(record))
}

fn read_entity_flags(
    connection: &Connection,
    id: SymbolId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Vec<EntityFlag>, CatalogError> {
    let limit = state.child_probe_limit(context)?;
    let flags = query_list(
        connection,
        "SELECT flag
         FROM entity_flags
         WHERE entity_id = ?1
         ORDER BY flag
         LIMIT ?2",
        params![id.as_bytes().as_slice(), limit],
        state,
        context,
        |row| codec::decode_enum(get(row, 0)?),
    )?;
    if flags.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(flags)
}

fn read_occurrence_candidates(
    connection: &Connection,
    id: FactId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Vec<SymbolId>, CatalogError> {
    let limit = state.child_probe_limit(context)?;
    let positioned = query_list(
        connection,
        "SELECT position, entity_id
         FROM occurrence_candidates
         WHERE occurrence_id = ?1
         ORDER BY position
         LIMIT ?2",
        params![id.as_bytes().as_slice(), limit],
        state,
        context,
        |row| Ok((get::<i64>(row, 0)?, codec::symbol_id(get(row, 1)?)?)),
    )?;
    let values = positioned_values(positioned)?;
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(values)
}

fn read_evidence(
    connection: &Connection,
    owner_kind: &str,
    owner_id: &[u8],
    source_ordinal: Option<i64>,
    metadata: GenerationMetadata,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<FactEvidence, CatalogError> {
    let source = source_ordinal
        .map(|ordinal| state.source(connection, ordinal, metadata, context))
        .transpose()?;
    let limit = state.child_probe_limit(context)?;
    let positioned = query_list(
        connection,
        "SELECT position, reference_kind, reference_id
         FROM evidence_derivations
         WHERE owner_kind = ?1 AND owner_id = ?2
         ORDER BY position
         LIMIT ?3",
        params![owner_kind, owner_id, limit],
        state,
        context,
        |row| {
            Ok((
                get::<i64>(row, 0)?,
                codec::decode_fact_ref(&get::<String>(row, 1)?, get(row, 2)?)?,
            ))
        },
    )?;
    let derivation = positioned_values(positioned)?;
    if derivation.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(FactEvidence { source, derivation })
}

fn read_provenance_sources(
    connection: &Connection,
    id: FactId,
    source_kind: &str,
    metadata: GenerationMetadata,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Vec<SourceRef>, CatalogError> {
    let limit = state.child_probe_limit(context)?;
    let positioned = query_list(
        connection,
        "SELECT position, source_ordinal
         FROM provenance_sources
         WHERE provenance_id = ?1 AND source_kind = ?2
         ORDER BY position
         LIMIT ?3",
        params![id.as_bytes().as_slice(), source_kind, limit],
        state,
        context,
        |row| Ok((get::<i64>(row, 0)?, get::<i64>(row, 1)?)),
    )?;
    let ordinals = positioned_values(positioned)?;
    let mut sources = Vec::new();
    sources
        .try_reserve(ordinals.len())
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    for ordinal in ordinals {
        sources.push(state.source(connection, ordinal, metadata, context)?);
    }
    if sources.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(sources)
}

fn read_provenance_derivations(
    connection: &Connection,
    id: FactId,
    context: &GenerationContext<'_>,
    state: &mut IndexedReadState,
) -> Result<Vec<FactRef>, CatalogError> {
    let limit = state.child_probe_limit(context)?;
    let positioned = query_list(
        connection,
        "SELECT position, reference_kind, reference_id
         FROM provenance_derivations
         WHERE provenance_id = ?1
         ORDER BY position
         LIMIT ?2",
        params![id.as_bytes().as_slice(), limit],
        state,
        context,
        |row| {
            Ok((
                get::<i64>(row, 0)?,
                codec::decode_fact_ref(&get::<String>(row, 1)?, get(row, 2)?)?,
            ))
        },
    )?;
    let values = positioned_values(positioned)?;
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(values)
}

fn relation_filter(request: &RelationReadRequest) -> Result<(String, Vec<Value>), CatalogError> {
    let anchor = request.anchor();
    let (anchor_kind, anchor_id) = codec::encode_endpoint(&anchor);
    let anchor_column = match request.direction() {
        RelationReadDirection::Outgoing => "subject",
        RelationReadDirection::Incoming => "object",
    };
    let mut filter = format!("{anchor_column}_kind = ? AND {anchor_column}_id = ?");
    let mut parameters = vec![
        Value::Text(anchor_kind.to_owned()),
        Value::Blob(anchor_id.to_vec()),
    ];
    if !request.predicates().is_empty() {
        filter.push_str(" AND predicate IN (");
        for index in 0..request.predicates().len() {
            if index > 0 {
                filter.push(',');
            }
            filter.push('?');
        }
        filter.push(')');
        for predicate in request.predicates() {
            parameters.push(Value::Text(codec::encode_enum(predicate)?));
        }
    }
    Ok((filter, parameters))
}

fn query_count(
    connection: &Connection,
    sql: &str,
    parameters: &[Value],
    state: &mut IndexedReadState,
    context: &GenerationContext<'_>,
) -> Result<u64, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let count: i64 = connection
        .query_row(sql, params_from_iter(parameters.iter()), |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    state.observe_row(context)?;
    context.check().map_err(CatalogError::control)?;
    codec::nonnegative_u64(count)
}

fn query_fact_ids(
    connection: &Connection,
    sql: &str,
    parameters: &[Value],
    state: &mut IndexedReadState,
    context: &GenerationContext<'_>,
) -> Result<Vec<FactId>, CatalogError> {
    query_list(
        connection,
        sql,
        params_from_iter(parameters.iter()),
        state,
        context,
        |row| codec::fact_id(get(row, 0)?),
    )
}

fn query_optional<T>(
    connection: &Connection,
    sql: &str,
    parameters: impl Params,
    state: &mut IndexedReadState,
    context: &GenerationContext<'_>,
    decode: impl FnOnce(&Row<'_>) -> Result<T, CatalogError>,
) -> Result<Option<T>, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let mut statement = connection.prepare(sql).map_err(CatalogError::sqlite)?;
    let mut rows = statement.query(parameters).map_err(CatalogError::sqlite)?;
    let Some(row) = rows.next().map_err(CatalogError::sqlite)? else {
        context.check().map_err(CatalogError::control)?;
        return Ok(None);
    };
    state.observe_row(context)?;
    let value = decode(row)?;
    if rows.next().map_err(CatalogError::sqlite)?.is_some() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    context.check().map_err(CatalogError::control)?;
    Ok(Some(value))
}

fn query_list<T>(
    connection: &Connection,
    sql: &str,
    parameters: impl Params,
    state: &mut IndexedReadState,
    context: &GenerationContext<'_>,
    mut decode: impl FnMut(&Row<'_>) -> Result<T, CatalogError>,
) -> Result<Vec<T>, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let mut statement = connection.prepare(sql).map_err(CatalogError::sqlite)?;
    let mut rows = statement.query(parameters).map_err(CatalogError::sqlite)?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        state.observe_row(context)?;
        values
            .try_reserve(1)
            .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
        values.push(decode(row)?);
    }
    context.check().map_err(CatalogError::control)?;
    Ok(values)
}

fn positioned_values<T>(positioned: Vec<(i64, T)>) -> Result<Vec<T>, CatalogError> {
    let mut values = Vec::new();
    values
        .try_reserve(positioned.len())
        .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
    for (expected, (position, value)) in positioned.into_iter().enumerate() {
        let expected =
            i64::try_from(expected).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
        if position != expected {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        values.push(value);
    }
    Ok(values)
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
            let total_count = codec::nonnegative_u64(total_count)?;
            let materialized = u64::try_from(candidates.len())
                .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
            if materialized > total_count {
                return Err(CatalogError::new(CatalogErrorKind::Corrupt));
            }
            Ok(OccurrenceTarget::Candidates {
                symbols: candidates,
                total_count,
                completeness: codec::decode_enum(completeness)?,
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

fn require_owner(
    metadata: GenerationMetadata,
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
) -> Result<(), CatalogError> {
    if repository == metadata.repository() && generation == metadata.generation() {
        Ok(())
    } else {
        Err(CatalogError::new(CatalogErrorKind::Corrupt))
    }
}

fn require_fact_id<T>(
    observed: FactId,
    derived: Result<FactId, T>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let derived = derived.map_err(|_| {
        context.check().map_or_else(CatalogError::control, |()| {
            CatalogError::new(CatalogErrorKind::Corrupt)
        })
    })?;
    context.check().map_err(CatalogError::control)?;
    if observed == derived {
        Ok(())
    } else {
        Err(CatalogError::new(CatalogErrorKind::Corrupt))
    }
}

fn require_total(observed: u64, maximum: u64) -> Result<(), CatalogError> {
    if observed <= maximum {
        Ok(())
    } else {
        Err(CatalogError::new(CatalogErrorKind::Corrupt))
    }
}

fn limit_plus_one(limit: GenerationReadLimit) -> i64 {
    i64::from(limit.get()) + 1
}

fn get<T: FromSql>(row: &Row<'_>, index: usize) -> Result<T, CatalogError> {
    row.get(index).map_err(CatalogError::sqlite)
}
