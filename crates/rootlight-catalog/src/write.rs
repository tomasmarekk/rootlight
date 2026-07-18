//! Deterministic insertion for one validated normalized generation.
//!
//! The complete write is preflighted against hard operation budgets before the
//! single bounded transaction begins. Statements are fixed and parameterized.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use rootlight_ids::{ContentHash, FactId, FileId, RepositoryId, SymbolId};
use rootlight_ir::{
    ExtensionCriticality, FactEvidence, IrLimits, NormalizedIrDocument, OccurrenceTarget, SourceRef,
};
use rootlight_storage::{
    GenerationContext, GenerationResource, GenerationSnapshot, GenerationStats,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use crate::{CatalogError, CatalogErrorKind, codec, schema};

const JSON_CHECKPOINT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RegistryIdentity {
    Repository(RepositoryId),
    File(FileId),
    Entity(SymbolId),
    Fact(FactId),
}

impl RegistryIdentity {
    pub(crate) const fn kind(self) -> &'static str {
        match self {
            Self::Repository(_) => "repository",
            Self::File(_) => "file",
            Self::Entity(_) => "entity",
            Self::Fact(_) => "fact",
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Repository(id) => id.as_bytes(),
            Self::File(id) => id.as_bytes(),
            Self::Entity(id) => id.as_bytes(),
            Self::Fact(id) => id.as_bytes(),
        }
    }
}

pub(crate) struct WritePlan {
    pub(crate) identities: BTreeSet<RegistryIdentity>,
    pub(crate) source_ordinals: BTreeMap<SourceRef, i64>,
    pub(crate) stats: GenerationStats,
}

pub(crate) fn write_generation(
    connection: &mut Connection,
    generation: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<GenerationStats, CatalogError> {
    let plan = measure(generation, context)?;
    context.check().map_err(CatalogError::control)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(CatalogError::sqlite)?;

    insert_header(&transaction, generation, plan.stats, context)?;
    insert_identities(&transaction, &plan.identities, context)?;
    insert_sources(&transaction, &plan.source_ordinals, context)?;
    insert_provenance(
        &transaction,
        generation.document(),
        &plan.source_ordinals,
        context,
    )?;
    insert_files(
        &transaction,
        generation.document(),
        &plan.source_ordinals,
        context,
    )?;
    insert_entities(
        &transaction,
        generation.document(),
        &plan.source_ordinals,
        context,
    )?;
    insert_occurrences(
        &transaction,
        generation.document(),
        &plan.source_ordinals,
        context,
    )?;
    insert_relations(
        &transaction,
        generation.document(),
        &plan.source_ordinals,
        context,
    )?;
    insert_opaque_facts(&transaction, generation.document(), context)?;
    insert_coverage(
        &transaction,
        generation.document(),
        &plan.source_ordinals,
        context,
    )?;
    context.check().map_err(CatalogError::control)?;
    transaction.commit().map_err(CatalogError::sqlite)?;
    schema::validate_oracle(connection, context)?;
    Ok(plan.stats)
}

pub(crate) fn measure(
    generation: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<WritePlan, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let document = generation.document();
    if document
        .extensions
        .iter()
        .any(|extension| extension.criticality == ExtensionCriticality::Critical)
    {
        return Err(CatalogError::new(
            CatalogErrorKind::UnsupportedCriticalExtensions,
        ));
    }
    let mut sources = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut child_rows = 0_u64;
    let mut text_bytes = 0_u64;

    insert_measured_identity(
        &mut identities,
        RegistryIdentity::Repository(document.repository),
        context,
    )?;
    for file in &document.files {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(&mut identities, RegistryIdentity::File(file.id), context)?;
        collect_evidence(&file.evidence, &mut sources, &mut child_rows, context)?;
        add_text(&mut text_bytes, &file.path, context)?;
        if let Some(locator) = &file.path_locator {
            add_text(&mut text_bytes, locator.encoding().as_str(), context)?;
            let components = serialize_json_text(locator.components(), context)?;
            add_text(&mut text_bytes, &components, context)?;
        }
        add_text(&mut text_bytes, &file.language, context)?;
        add_text(&mut text_bytes, &file.encoding, context)?;
    }
    for entity in &document.entities {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Entity(entity.id),
            context,
        )?;
        collect_evidence(&entity.evidence, &mut sources, &mut child_rows, context)?;
        add_rows(&mut child_rows, usize_to_u64(entity.flags.len())?, context)?;
        add_text(&mut text_bytes, &entity.language, context)?;
        add_text(&mut text_bytes, &entity.canonical_name, context)?;
        add_text(&mut text_bytes, &entity.display_name, context)?;
        add_text(&mut text_bytes, &entity.qualified_name, context)?;
    }
    for occurrence in &document.occurrences {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Fact(occurrence.id),
            context,
        )?;
        collect_source(&occurrence.source, &mut sources, context)?;
        collect_evidence(&occurrence.evidence, &mut sources, &mut child_rows, context)?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &occurrence.target {
            add_rows(&mut child_rows, usize_to_u64(symbols.len())?, context)?;
        }
        add_text(&mut text_bytes, &occurrence.syntax_kind, context)?;
    }
    for relation in &document.relations {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Fact(relation.id),
            context,
        )?;
        collect_evidence(&relation.evidence, &mut sources, &mut child_rows, context)?;
    }
    for provenance in &document.provenance {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Fact(provenance.id),
            context,
        )?;
        for source in provenance
            .input_sources
            .iter()
            .chain(&provenance.evidence_sources)
        {
            context.check().map_err(CatalogError::control)?;
            collect_source(source, &mut sources, context)?;
            add_rows(&mut child_rows, 1, context)?;
        }
        add_rows(
            &mut child_rows,
            usize_to_u64(provenance.derivation_parents.len())?,
            context,
        )?;
        add_text(&mut text_bytes, provenance.producer.name(), context)?;
        add_text(&mut text_bytes, provenance.producer.version(), context)?;
        add_optional_text(
            &mut text_bytes,
            provenance.frontend_version.as_deref(),
            context,
        )?;
        add_text(&mut text_bytes, &provenance.language, context)?;
        add_optional_text(&mut text_bytes, provenance.rule.as_deref(), context)?;
    }
    for mapping in &document.source_mappings {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(&mut identities, RegistryIdentity::Fact(mapping.id), context)?;
        collect_source(&mapping.from, &mut sources, context)?;
        collect_source(&mapping.to, &mut sources, context)?;
        collect_opaque_evidence_source(&mapping.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, mapping, context)?;
    }
    for coverage in &document.coverage_records {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Fact(coverage.id),
            context,
        )?;
        collect_evidence(&coverage.evidence, &mut sources, &mut child_rows, context)?;
    }
    for region in &document.skipped_regions {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(&mut identities, RegistryIdentity::Fact(region.id), context)?;
        collect_source(&region.source, &mut sources, context)?;
        collect_opaque_evidence_source(&region.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, region, context)?;
    }
    for diagnostic in &document.diagnostics {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Fact(diagnostic.id),
            context,
        )?;
        if let Some(source) = &diagnostic.source {
            collect_source(source, &mut sources, context)?;
        }
        collect_opaque_evidence_source(&diagnostic.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, diagnostic, context)?;
    }
    for extension in &document.extensions {
        context.check().map_err(CatalogError::control)?;
        insert_measured_identity(
            &mut identities,
            RegistryIdentity::Fact(extension.id),
            context,
        )?;
        collect_opaque_evidence_source(&extension.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, extension, context)?;
    }

    let source_count = usize_to_u64(sources.len())?;
    context
        .require(GenerationResource::SourceReferences, source_count)
        .map_err(CatalogError::control)?;
    let logical_records = [
        usize_to_u64(document.files.len())?,
        usize_to_u64(document.entities.len())?,
        usize_to_u64(document.occurrences.len())?,
        usize_to_u64(document.relations.len())?,
        usize_to_u64(document.provenance.len())?,
        usize_to_u64(document.source_mappings.len())?,
        usize_to_u64(document.coverage_records.len())?,
        usize_to_u64(document.skipped_regions.len())?,
        usize_to_u64(document.diagnostics.len())?,
        usize_to_u64(document.extensions.len())?,
    ];
    let logical_rows = logical_records
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
    let identity_rows = usize_to_u64(identities.len())?;
    if identity_rows
        != logical_rows
            .checked_add(1)
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?
    {
        return Err(CatalogError::new(CatalogErrorKind::InvalidGeneration));
    }
    let stored_rows = 2_u64
        .checked_add(identity_rows)
        .and_then(|value| value.checked_add(source_count))
        .and_then(|value| value.checked_add(logical_rows))
        .and_then(|value| value.checked_add(child_rows))
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
    context
        .require(GenerationResource::Rows, stored_rows)
        .map_err(CatalogError::control)?;
    context
        .require(GenerationResource::TextBytes, text_bytes)
        .map_err(CatalogError::control)?;

    let stats = GenerationStats::new(
        logical_records[0],
        logical_records[1],
        logical_records[2],
        logical_records[3],
        logical_records[4],
        logical_records[5],
        logical_records[6],
        logical_records[7],
        logical_records[8],
        logical_records[9],
        source_count,
        stored_rows,
        text_bytes,
    )
    .map_err(CatalogError::invalid_generation)?;
    let mut source_ordinals = BTreeMap::new();
    for (ordinal, source) in sources.into_iter().enumerate() {
        context.check().map_err(CatalogError::control)?;
        source_ordinals.insert(source, usize_to_i64(ordinal)?);
    }
    Ok(WritePlan {
        identities,
        source_ordinals,
        stats,
    })
}

fn insert_measured_identity(
    identities: &mut BTreeSet<RegistryIdentity>,
    identity: RegistryIdentity,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    if !identities.insert(identity) {
        return Err(CatalogError::new(CatalogErrorKind::InvalidGeneration));
    }
    context
        .require(GenerationResource::Rows, usize_to_u64(identities.len())?)
        .map_err(CatalogError::control)
}

fn collect_evidence(
    evidence: &FactEvidence,
    sources: &mut BTreeSet<SourceRef>,
    child_rows: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    if let Some(source) = &evidence.source {
        collect_source(source, sources, context)?;
    }
    add_rows(
        child_rows,
        usize_to_u64(evidence.derivation.len())?,
        context,
    )
}

fn collect_opaque_evidence_source(
    evidence: &FactEvidence,
    sources: &mut BTreeSet<SourceRef>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    if let Some(source) = &evidence.source {
        collect_source(source, sources, context)?;
    }
    Ok(())
}

fn collect_source(
    source: &SourceRef,
    sources: &mut BTreeSet<SourceRef>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    sources.insert(source.clone());
    context
        .require(
            GenerationResource::SourceReferences,
            usize_to_u64(sources.len())?,
        )
        .map_err(CatalogError::control)
}

fn add_rows(
    rows: &mut u64,
    increment: u64,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    *rows = rows
        .checked_add(increment)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
    context
        .require(GenerationResource::Rows, *rows)
        .map_err(CatalogError::control)
}

fn add_text(
    bytes: &mut u64,
    value: &str,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    *bytes = bytes
        .checked_add(usize_to_u64(value.len())?)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
    context
        .require(GenerationResource::TextBytes, *bytes)
        .map_err(CatalogError::control)
}

fn add_optional_text(
    bytes: &mut u64,
    value: Option<&str>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    if let Some(value) = value {
        add_text(bytes, value, context)?;
    }
    Ok(())
}

fn add_serialized_text(
    bytes: &mut u64,
    value: &impl serde::Serialize,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut writer = CheckedJsonWriter::for_text(io::sink(), *bytes, context)?;
    serialize_json(&mut writer, value)?;
    *bytes = writer.total_bytes();
    Ok(())
}

fn insert_header(
    transaction: &Transaction<'_>,
    generation: &GenerationSnapshot,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let metadata = generation.metadata();
    let contract = metadata.contract_version();
    let ir = metadata.ir_version();
    transaction
        .execute(
            "INSERT INTO generation_meta (
                singleton, contract_major, contract_minor, ir_major, ir_minor,
                repository_id, generation_id, parent_generation_id,
                manifest_hash, configuration_hash, provider_set_hash,
                file_count, entity_count, occurrence_count, relation_count,
                provenance_count, source_mapping_count, coverage_count,
                skipped_region_count, diagnostic_count, extension_count, source_ref_count,
                stored_row_count, text_bytes, sealed
             ) VALUES (
                1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                ?21, ?22, ?23, 1
             )",
            params![
                i64::from(contract.major()),
                i64::from(contract.minor()),
                i64::from(ir.major()),
                i64::from(ir.minor()),
                metadata.repository().as_bytes().as_slice(),
                metadata.generation().as_bytes().as_slice(),
                metadata
                    .parent()
                    .map(|value| value.as_bytes().as_slice().to_vec()),
                metadata.manifest_hash().as_bytes().as_slice(),
                metadata.configuration_hash().as_bytes().as_slice(),
                metadata.provider_set_hash().as_bytes().as_slice(),
                codec::sqlite_i64(stats.files())?,
                codec::sqlite_i64(stats.entities())?,
                codec::sqlite_i64(stats.occurrences())?,
                codec::sqlite_i64(stats.relations())?,
                codec::sqlite_i64(stats.provenance())?,
                codec::sqlite_i64(stats.source_mappings())?,
                codec::sqlite_i64(stats.coverage())?,
                codec::sqlite_i64(stats.skipped_regions())?,
                codec::sqlite_i64(stats.diagnostics())?,
                codec::sqlite_i64(stats.extensions())?,
                codec::sqlite_i64(stats.source_refs())?,
                codec::sqlite_i64(stats.stored_rows())?,
                codec::sqlite_i64(stats.text_bytes())?,
            ],
        )
        .map_err(CatalogError::sqlite)?;
    let document_hash = canonical_document_hash(
        generation.document(),
        context,
        CatalogErrorKind::InvalidGeneration,
    )?;
    transaction
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('document_hash', ?1)",
            [document_hash.as_bytes().as_slice()],
        )
        .map_err(CatalogError::sqlite)?;
    Ok(())
}

fn insert_identities(
    transaction: &Transaction<'_>,
    identities: &BTreeSet<RegistryIdentity>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare("INSERT INTO identity_registry(kind, identity) VALUES (?1, ?2)")
        .map_err(CatalogError::sqlite)?;
    for identity in identities {
        context.check().map_err(CatalogError::control)?;
        statement
            .execute(params![identity.kind(), identity.bytes()])
            .map_err(CatalogError::sqlite)?;
    }
    Ok(())
}

fn insert_sources(
    transaction: &Transaction<'_>,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO source_refs (
                ordinal, repository_id, generation_id, file_id, start_byte,
                end_byte, content_hash, line_start, line_end
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(CatalogError::sqlite)?;
    for (source, ordinal) in sources {
        context.check().map_err(CatalogError::control)?;
        let span = source.span();
        let (line_start, line_end) = source
            .line_hint()
            .map_or((0, 0), |line| (line.start_line(), line.end_line()));
        statement
            .execute(params![
                ordinal,
                source.repository().as_bytes().as_slice(),
                source.generation().as_bytes().as_slice(),
                span.file().as_bytes().as_slice(),
                codec::sqlite_i64(span.start_byte())?,
                codec::sqlite_i64(span.end_byte())?,
                source.content_hash().as_bytes().as_slice(),
                codec::sqlite_i64(line_start)?,
                codec::sqlite_i64(line_end)?,
            ])
            .map_err(CatalogError::sqlite)?;
    }
    Ok(())
}

fn insert_provenance(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut record_statement = transaction
        .prepare(
            "INSERT INTO provenance (
                provenance_id, repository_id, generation_id, producer_kind,
                producer_name, producer_version, producer_configuration_hash,
                binary_digest, frontend_version, language, tier,
                build_context_digest, rule
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut source_statement = transaction
        .prepare(
            "INSERT INTO provenance_sources (
                provenance_id, source_kind, position, source_ordinal
             ) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut derivation_statement = transaction
        .prepare(
            "INSERT INTO provenance_derivations (
                provenance_id, position, reference_kind, reference_id
             ) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(CatalogError::sqlite)?;
    for record in &document.provenance {
        context.check().map_err(CatalogError::control)?;
        record_statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                codec::encode_enum(&record.producer_kind)?,
                record.producer.name(),
                record.producer.version(),
                record.producer.configuration_hash().as_bytes().as_slice(),
                record.binary_digest.as_bytes().as_slice(),
                record.frontend_version.as_deref(),
                record.language,
                codec::encode_enum(&record.tier)?,
                record.build_context.digest().as_bytes().as_slice(),
                record.rule.as_deref(),
            ])
            .map_err(CatalogError::sqlite)?;
        for (source_kind, values) in [
            ("input", record.input_sources.as_slice()),
            ("evidence", record.evidence_sources.as_slice()),
        ] {
            for (position, source) in values.iter().enumerate() {
                context.check().map_err(CatalogError::control)?;
                source_statement
                    .execute(params![
                        record.id.as_bytes().as_slice(),
                        source_kind,
                        usize_to_i64(position)?,
                        source_ordinal(sources, source)?,
                    ])
                    .map_err(CatalogError::sqlite)?;
            }
        }
        for (position, reference) in record.derivation_parents.iter().enumerate() {
            context.check().map_err(CatalogError::control)?;
            let (kind, id) = codec::encode_fact_ref(reference);
            derivation_statement
                .execute(params![
                    record.id.as_bytes().as_slice(),
                    usize_to_i64(position)?,
                    kind,
                    id,
                ])
                .map_err(CatalogError::sqlite)?;
        }
    }
    Ok(())
}

fn insert_files(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO files (
                file_id, repository_id, generation_id, path, path_locator_encoding,
                path_locator_components, content_hash, byte_length, language, encoding,
                generated, provenance_id, evidence_source_ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut evidence_statement = evidence_statement(transaction)?;
    for record in &document.files {
        context.check().map_err(CatalogError::control)?;
        let locator_encoding = record
            .path_locator
            .as_ref()
            .map(|locator| locator.encoding().as_str());
        let locator_components = record
            .path_locator
            .as_ref()
            .map(|locator| serialize_json_text(locator.components(), context))
            .transpose()?;
        statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                record.path,
                locator_encoding,
                locator_components,
                record.content_hash.as_bytes().as_slice(),
                codec::sqlite_i64(record.byte_length)?,
                record.language,
                record.encoding,
                i64::from(record.generated),
                record.provenance.as_bytes().as_slice(),
                optional_source_ordinal(sources, record.evidence.source.as_ref())?,
            ])
            .map_err(CatalogError::sqlite)?;
        insert_evidence(
            &mut evidence_statement,
            "file",
            record.id.as_bytes(),
            &record.evidence,
            context,
        )?;
    }
    Ok(())
}

fn insert_entities(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO entities (
                entity_id, repository_id, generation_id, kind, language, tier,
                canonical_name, display_name, qualified_name, container_kind,
                container_id, visibility, provenance_id, evidence_source_ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut flag_statement = transaction
        .prepare("INSERT INTO entity_flags(entity_id, flag) VALUES (?1, ?2)")
        .map_err(CatalogError::sqlite)?;
    let mut evidence_statement = evidence_statement(transaction)?;
    for record in &document.entities {
        context.check().map_err(CatalogError::control)?;
        let (container_kind, container_id) = codec::encode_container(&record.container);
        statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                codec::encode_enum(&record.kind)?,
                record.language,
                codec::encode_enum(&record.tier)?,
                record.canonical_name,
                record.display_name,
                record.qualified_name,
                container_kind,
                container_id,
                codec::encode_enum(&record.visibility)?,
                record.provenance.as_bytes().as_slice(),
                optional_source_ordinal(sources, record.evidence.source.as_ref())?,
            ])
            .map_err(CatalogError::sqlite)?;
        for flag in &record.flags {
            context.check().map_err(CatalogError::control)?;
            flag_statement
                .execute(params![
                    record.id.as_bytes().as_slice(),
                    codec::encode_enum(flag)?
                ])
                .map_err(CatalogError::sqlite)?;
        }
        insert_evidence(
            &mut evidence_statement,
            "entity",
            record.id.as_bytes(),
            &record.evidence,
            context,
        )?;
    }
    Ok(())
}

fn insert_occurrences(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO occurrences (
                occurrence_id, repository_id, generation_id, file_id,
                source_ordinal, role, enclosing_entity_id, target_kind,
                target_symbol_id, target_text_hash, target_total_count,
                target_completeness, syntactic_text_hash, syntax_kind,
                provenance_id, confidence, evidence_source_ordinal
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17
             )",
        )
        .map_err(CatalogError::sqlite)?;
    let mut candidate_statement = transaction
        .prepare(
            "INSERT INTO occurrence_candidates (
                occurrence_id, position, entity_id
             ) VALUES (?1, ?2, ?3)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut evidence_statement = evidence_statement(transaction)?;
    for record in &document.occurrences {
        context.check().map_err(CatalogError::control)?;
        let (target_kind, target_symbol, target_hash, total_count, completeness) =
            encode_target(&record.target)?;
        statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                record.file.as_bytes().as_slice(),
                source_ordinal(sources, &record.source)?,
                codec::encode_enum(&record.role)?,
                record.enclosing.map(|id| id.as_bytes().as_slice().to_vec()),
                target_kind,
                target_symbol,
                target_hash,
                total_count,
                completeness,
                record.syntactic_text_hash.as_bytes().as_slice(),
                record.syntax_kind,
                record.provenance.as_bytes().as_slice(),
                i64::from(record.confidence.get()),
                optional_source_ordinal(sources, record.evidence.source.as_ref())?,
            ])
            .map_err(CatalogError::sqlite)?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &record.target {
            for (position, symbol) in symbols.iter().enumerate() {
                context.check().map_err(CatalogError::control)?;
                candidate_statement
                    .execute(params![
                        record.id.as_bytes().as_slice(),
                        usize_to_i64(position)?,
                        symbol.as_bytes().as_slice(),
                    ])
                    .map_err(CatalogError::sqlite)?;
            }
        }
        insert_evidence(
            &mut evidence_statement,
            "fact",
            record.id.as_bytes(),
            &record.evidence,
            context,
        )?;
    }
    Ok(())
}

type EncodedTarget = (
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<i64>,
    Option<String>,
);

fn encode_target(target: &OccurrenceTarget) -> Result<EncodedTarget, CatalogError> {
    match target {
        OccurrenceTarget::Resolved { symbol } => Ok((
            "resolved",
            Some(symbol.as_bytes().to_vec()),
            None,
            None,
            None,
        )),
        OccurrenceTarget::Candidates {
            total_count,
            completeness,
            ..
        } => Ok((
            "candidates",
            None,
            None,
            Some(codec::sqlite_i64(*total_count)?),
            Some(codec::encode_enum(completeness)?),
        )),
        OccurrenceTarget::Unresolved { text_hash } => Ok((
            "unresolved",
            None,
            Some(text_hash.as_bytes().to_vec()),
            None,
            None,
        )),
    }
}

fn insert_relations(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO relations (
                relation_id, repository_id, generation_id, subject_kind,
                subject_id, predicate, object_kind, object_id, confidence,
                evidence_kind, provenance_id, evidence_source_ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut evidence_statement = evidence_statement(transaction)?;
    for record in &document.relations {
        context.check().map_err(CatalogError::control)?;
        let (subject_kind, subject_id) = codec::encode_endpoint(&record.subject);
        let (object_kind, object_id) = codec::encode_endpoint(&record.object);
        statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                subject_kind,
                subject_id,
                codec::encode_enum(&record.predicate)?,
                object_kind,
                object_id,
                i64::from(record.confidence.get()),
                codec::encode_enum(&record.evidence_kind)?,
                record.provenance.as_bytes().as_slice(),
                optional_source_ordinal(sources, record.evidence.source.as_ref())?,
            ])
            .map_err(CatalogError::sqlite)?;
        insert_evidence(
            &mut evidence_statement,
            "fact",
            record.id.as_bytes(),
            &record.evidence,
            context,
        )?;
    }
    Ok(())
}

fn insert_coverage(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    sources: &BTreeMap<SourceRef, i64>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO coverage_records (
                coverage_id, repository_id, generation_id, scope_kind, scope_id,
                domain, tier, status, discovered, indexed, skipped,
                provenance_id, evidence_source_ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut evidence_statement = evidence_statement(transaction)?;
    for record in &document.coverage_records {
        context.check().map_err(CatalogError::control)?;
        let (scope_kind, scope_id) = codec::encode_scope(&record.scope);
        statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                scope_kind,
                scope_id,
                codec::encode_enum(&record.domain)?,
                codec::encode_enum(&record.tier)?,
                codec::encode_enum(&record.status)?,
                codec::sqlite_i64(record.discovered)?,
                codec::sqlite_i64(record.indexed)?,
                codec::sqlite_i64(record.skipped)?,
                record.provenance.as_bytes().as_slice(),
                optional_source_ordinal(sources, record.evidence.source.as_ref())?,
            ])
            .map_err(CatalogError::sqlite)?;
        insert_evidence(
            &mut evidence_statement,
            "fact",
            record.id.as_bytes(),
            &record.evidence,
            context,
        )?;
    }
    Ok(())
}

fn insert_opaque_facts(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO source_mappings (
                source_mapping_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(CatalogError::sqlite)?;
    for record in &document.source_mappings {
        insert_opaque_record(
            &mut statement,
            record.id.as_bytes(),
            record.repository.as_bytes(),
            record.generation.as_bytes(),
            record.provenance.as_bytes(),
            record,
            context,
        )?;
    }

    let mut statement = transaction
        .prepare(
            "INSERT INTO skipped_regions (
                skipped_region_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(CatalogError::sqlite)?;
    for record in &document.skipped_regions {
        insert_opaque_record(
            &mut statement,
            record.id.as_bytes(),
            record.repository.as_bytes(),
            record.generation.as_bytes(),
            record.provenance.as_bytes(),
            record,
            context,
        )?;
    }

    let mut statement = transaction
        .prepare(
            "INSERT INTO diagnostics (
                diagnostic_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(CatalogError::sqlite)?;
    for record in &document.diagnostics {
        insert_opaque_record(
            &mut statement,
            record.id.as_bytes(),
            record.repository.as_bytes(),
            record.generation.as_bytes(),
            record.provenance.as_bytes(),
            record,
            context,
        )?;
    }

    let mut statement = transaction
        .prepare(
            "INSERT INTO extensions (
                extension_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(CatalogError::sqlite)?;
    for record in &document.extensions {
        insert_opaque_record(
            &mut statement,
            record.id.as_bytes(),
            record.repository.as_bytes(),
            record.generation.as_bytes(),
            record.provenance.as_bytes(),
            record,
            context,
        )?;
    }
    Ok(())
}

fn insert_opaque_record(
    statement: &mut rusqlite::Statement<'_>,
    id: &[u8],
    repository: &[u8],
    generation: &[u8],
    provenance: &[u8],
    record: &impl serde::Serialize,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let payload = serialize_json_text(record, context)?;
    statement
        .execute(params![id, repository, generation, provenance, payload])
        .map_err(CatalogError::sqlite)?;
    context.check().map_err(CatalogError::control)
}

fn evidence_statement<'transaction>(
    transaction: &'transaction Transaction<'_>,
) -> Result<rusqlite::Statement<'transaction>, CatalogError> {
    transaction
        .prepare(
            "INSERT INTO evidence_derivations (
                owner_kind, owner_id, position, reference_kind, reference_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(CatalogError::sqlite)
}

fn insert_evidence(
    statement: &mut rusqlite::Statement<'_>,
    owner_kind: &str,
    owner_id: &[u8],
    evidence: &FactEvidence,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    for (position, reference) in evidence.derivation.iter().enumerate() {
        context.check().map_err(CatalogError::control)?;
        let (reference_kind, reference_id) = codec::encode_fact_ref(reference);
        statement
            .execute(params![
                owner_kind,
                owner_id,
                usize_to_i64(position)?,
                reference_kind,
                reference_id,
            ])
            .map_err(CatalogError::sqlite)?;
    }
    Ok(())
}

fn source_ordinal(
    sources: &BTreeMap<SourceRef, i64>,
    source: &SourceRef,
) -> Result<i64, CatalogError> {
    sources
        .get(source)
        .copied()
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}

fn optional_source_ordinal(
    sources: &BTreeMap<SourceRef, i64>,
    source: Option<&SourceRef>,
) -> Result<Option<i64>, CatalogError> {
    source
        .map(|source| source_ordinal(sources, source))
        .transpose()
}

pub(crate) fn canonical_document_hash(
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
    overflow_kind: CatalogErrorKind,
) -> Result<ContentHash, CatalogError> {
    let maximum = usize_to_u64(IrLimits::default().max_document_bytes)?;
    let mut writer =
        CheckedJsonWriter::new(Blake3Writer::default(), 0, maximum, overflow_kind, context)?;
    serialize_json(&mut writer, document)?;
    context.check().map_err(CatalogError::control)?;
    let digest = writer.into_inner().0.finalize();
    Ok(ContentHash::from_bytes(*digest.as_bytes()))
}

fn serialize_json_text<T>(
    value: &T,
    context: &GenerationContext<'_>,
) -> Result<String, CatalogError>
where
    T: serde::Serialize + ?Sized,
{
    let mut writer = CheckedJsonWriter::for_text(Vec::new(), 0, context)?;
    serialize_json(&mut writer, value)?;
    context.check().map_err(CatalogError::control)?;
    String::from_utf8(writer.into_inner())
        .map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}

fn serialize_json<T>(
    writer: &mut CheckedJsonWriter<'_, impl Write>,
    value: &T,
) -> Result<(), CatalogError>
where
    T: serde::Serialize + ?Sized,
{
    match serde_json::to_writer(&mut *writer, value) {
        Ok(()) => Ok(()),
        Err(error) => writer.failure().map_or_else(
            || Err(CatalogError::json(error)),
            |kind| Err(CatalogError::new(kind)),
        ),
    }
}

struct CheckedJsonWriter<'a, W> {
    inner: W,
    context: GenerationContext<'a>,
    total_bytes: u64,
    maximum_bytes: u64,
    overflow_kind: CatalogErrorKind,
    failure: Option<CatalogErrorKind>,
}

impl<'a, W: Write> CheckedJsonWriter<'a, W> {
    fn new(
        inner: W,
        initial_bytes: u64,
        maximum_bytes: u64,
        overflow_kind: CatalogErrorKind,
        context: &GenerationContext<'a>,
    ) -> Result<Self, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        if initial_bytes > maximum_bytes {
            return Err(CatalogError::new(overflow_kind));
        }
        Ok(Self {
            inner,
            context: *context,
            total_bytes: initial_bytes,
            maximum_bytes,
            overflow_kind,
            failure: None,
        })
    }

    fn for_text(
        inner: W,
        initial_bytes: u64,
        context: &GenerationContext<'a>,
    ) -> Result<Self, CatalogError> {
        let maximum = context.budget().limit(GenerationResource::TextBytes);
        Self::new(
            inner,
            initial_bytes,
            maximum,
            CatalogErrorKind::BudgetExceeded {
                resource: GenerationResource::TextBytes,
                limit: maximum,
            },
            context,
        )
    }

    const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    const fn failure(&self) -> Option<CatalogErrorKind> {
        self.failure
    }

    fn into_inner(self) -> W {
        self.inner
    }

    fn stop(&mut self, kind: CatalogErrorKind) -> io::Error {
        self.failure = Some(kind);
        io::Error::other("controlled JSON write stopped")
    }
}

impl<W: Write> Write for CheckedJsonWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        for chunk in buffer.chunks(JSON_CHECKPOINT_BYTES) {
            if let Err(error) = self.context.check() {
                return Err(self.stop(CatalogError::control(error).kind()));
            }
            let increment =
                u64::try_from(chunk.len()).map_err(|_| self.stop(self.overflow_kind))?;
            let next = self
                .total_bytes
                .checked_add(increment)
                .ok_or_else(|| self.stop(self.overflow_kind))?;
            if next > self.maximum_bytes {
                return Err(self.stop(self.overflow_kind));
            }
            self.inner.write_all(chunk)?;
            self.total_bytes = next;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Default)]
struct Blake3Writer(blake3::Hasher);

impl Write for Blake3Writer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn usize_to_u64(value: usize) -> Result<u64, CatalogError> {
    u64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}

fn usize_to_i64(value: usize) -> Result<i64, CatalogError> {
    i64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_storage::GenerationBudget;
    use serde::ser::SerializeSeq;

    use super::*;

    struct StopDuringSerialization<'a> {
        cancellation: &'a Cancellation,
        reason: Option<CancellationReason>,
    }

    impl serde::Serialize for StopDuringSerialization<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&"x".repeat(2 * JSON_CHECKPOINT_BYTES))?;
            match self.reason {
                Some(reason) => {
                    self.cancellation.cancel(reason);
                }
                None => {
                    while self.cancellation.check().is_ok() {
                        std::thread::yield_now();
                    }
                }
            }
            sequence.serialize_element("unreachable after the next writer checkpoint")?;
            sequence.end()
        }
    }

    #[test]
    fn streaming_json_observes_mid_operation_cancellation_and_deadline() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let error = serialize_json_text(
            &StopDuringSerialization {
                cancellation: &cancellation,
                reason: Some(CancellationReason::ClientRequest),
            },
            &context,
        )
        .expect_err("the writer observes explicit cancellation between values");
        assert_eq!(error.kind(), CatalogErrorKind::Cancelled);

        let deadline = Cancellation::with_deadline(Instant::now() + Duration::from_millis(1));
        let context = GenerationContext::new(&deadline, GenerationBudget::default());
        let error = serialize_json_text(
            &StopDuringSerialization {
                cancellation: &deadline,
                reason: None,
            },
            &context,
        )
        .expect_err("the writer observes deadline expiry between values");
        assert_eq!(error.kind(), CatalogErrorKind::Cancelled);
        assert_eq!(
            deadline.reason(),
            Some(CancellationReason::DeadlineExceeded)
        );
    }
}
