//! Deterministic insertion for one validated normalized generation.
//!
//! The complete write is preflighted against hard operation budgets before the
//! single bounded transaction begins. Statements are fixed and parameterized.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::content_hash;
use rootlight_ir::{FactEvidence, NormalizedIrDocument, OccurrenceTarget, SourceRef};
use rootlight_storage::{
    GenerationContext, GenerationResource, GenerationSnapshot, GenerationStats,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use crate::{CatalogError, CatalogErrorKind, codec, schema};

struct WritePlan {
    source_ordinals: BTreeMap<SourceRef, i64>,
    stats: GenerationStats,
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

    insert_header(&transaction, generation, plan.stats)?;
    insert_identities(&transaction, generation.document(), context)?;
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
    schema::validate_oracle(connection)?;
    Ok(plan.stats)
}

pub(crate) fn measure_stats(
    generation: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<GenerationStats, CatalogError> {
    measure(generation, context).map(|plan| plan.stats)
}

fn measure(
    generation: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<WritePlan, CatalogError> {
    context.check().map_err(CatalogError::control)?;
    let document = generation.document();
    let mut sources = BTreeSet::new();
    let mut child_rows = 0_u64;
    let mut text_bytes = 0_u64;

    for file in &document.files {
        collect_evidence(&file.evidence, &mut sources, &mut child_rows, context)?;
        add_text(&mut text_bytes, &file.path, context)?;
        add_text(&mut text_bytes, &file.language, context)?;
        add_text(&mut text_bytes, &file.encoding, context)?;
    }
    for entity in &document.entities {
        collect_evidence(&entity.evidence, &mut sources, &mut child_rows, context)?;
        add_rows(&mut child_rows, usize_to_u64(entity.flags.len())?, context)?;
        add_text(&mut text_bytes, &entity.language, context)?;
        add_text(&mut text_bytes, &entity.canonical_name, context)?;
        add_text(&mut text_bytes, &entity.display_name, context)?;
        add_text(&mut text_bytes, &entity.qualified_name, context)?;
    }
    for occurrence in &document.occurrences {
        collect_source(&occurrence.source, &mut sources, context)?;
        collect_evidence(&occurrence.evidence, &mut sources, &mut child_rows, context)?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &occurrence.target {
            add_rows(&mut child_rows, usize_to_u64(symbols.len())?, context)?;
        }
        add_text(&mut text_bytes, &occurrence.syntax_kind, context)?;
    }
    for relation in &document.relations {
        collect_evidence(&relation.evidence, &mut sources, &mut child_rows, context)?;
    }
    for provenance in &document.provenance {
        for source in provenance
            .input_sources
            .iter()
            .chain(&provenance.evidence_sources)
        {
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
        collect_source(&mapping.from, &mut sources, context)?;
        collect_source(&mapping.to, &mut sources, context)?;
        collect_opaque_evidence_source(&mapping.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, mapping, context)?;
    }
    for coverage in &document.coverage_records {
        collect_evidence(&coverage.evidence, &mut sources, &mut child_rows, context)?;
    }
    for region in &document.skipped_regions {
        collect_source(&region.source, &mut sources, context)?;
        collect_opaque_evidence_source(&region.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, region, context)?;
    }
    for diagnostic in &document.diagnostics {
        if let Some(source) = &diagnostic.source {
            collect_source(source, &mut sources, context)?;
        }
        collect_opaque_evidence_source(&diagnostic.evidence, &mut sources, context)?;
        add_serialized_text(&mut text_bytes, diagnostic, context)?;
    }
    for extension in &document.extensions {
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
    let identity_rows = logical_rows
        .checked_add(1)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
    let stored_rows = 1_u64
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
    let source_ordinals = sources
        .into_iter()
        .enumerate()
        .map(|(ordinal, source)| Ok((source, usize_to_i64(ordinal)?)))
        .collect::<Result<_, CatalogError>>()?;
    Ok(WritePlan {
        source_ordinals,
        stats,
    })
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
    let encoded = serde_json::to_vec(value).map_err(CatalogError::json)?;
    *bytes = bytes
        .checked_add(usize_to_u64(encoded.len())?)
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
    context
        .require(GenerationResource::TextBytes, *bytes)
        .map_err(CatalogError::control)
}

fn insert_header(
    transaction: &Transaction<'_>,
    generation: &GenerationSnapshot,
    stats: GenerationStats,
) -> Result<(), CatalogError> {
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
    let document = serde_json::to_vec(generation.document()).map_err(CatalogError::json)?;
    transaction
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('document_hash', ?1)",
            [content_hash(&document).as_bytes().as_slice()],
        )
        .map_err(CatalogError::sqlite)?;
    Ok(())
}

fn insert_identities(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut statement = transaction
        .prepare("INSERT INTO identity_registry(kind, identity) VALUES (?1, ?2)")
        .map_err(CatalogError::sqlite)?;
    statement
        .execute(params![
            "repository",
            document.repository.as_bytes().as_slice()
        ])
        .map_err(CatalogError::sqlite)?;
    for (kind, id) in document
        .files
        .iter()
        .map(|record| ("file", record.id.as_bytes().as_slice()))
        .chain(
            document
                .entities
                .iter()
                .map(|record| ("entity", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .occurrences
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .relations
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .provenance
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .coverage_records
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .source_mappings
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .skipped_regions
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .diagnostics
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
        .chain(
            document
                .extensions
                .iter()
                .map(|record| ("fact", record.id.as_bytes().as_slice())),
        )
    {
        context.check().map_err(CatalogError::control)?;
        statement
            .execute(params![kind, id])
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
                file_id, repository_id, generation_id, path, content_hash,
                byte_length, language, encoding, generated, provenance_id,
                evidence_source_ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .map_err(CatalogError::sqlite)?;
    let mut evidence_statement = evidence_statement(transaction)?;
    for record in &document.files {
        context.check().map_err(CatalogError::control)?;
        statement
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.repository.as_bytes().as_slice(),
                record.generation.as_bytes().as_slice(),
                record.path,
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
        )?;
    }
    Ok(())
}

fn insert_opaque_facts(
    transaction: &Transaction<'_>,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    for (sql, records) in [
        (
            "INSERT INTO source_mappings (
                source_mapping_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            document
                .source_mappings
                .iter()
                .map(|record| {
                    (
                        record.id.as_bytes().as_slice(),
                        record.repository.as_bytes().as_slice(),
                        record.generation.as_bytes().as_slice(),
                        record.provenance.as_bytes().as_slice(),
                        serde_json::to_string(record),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        (
            "INSERT INTO skipped_regions (
                skipped_region_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            document
                .skipped_regions
                .iter()
                .map(|record| {
                    (
                        record.id.as_bytes().as_slice(),
                        record.repository.as_bytes().as_slice(),
                        record.generation.as_bytes().as_slice(),
                        record.provenance.as_bytes().as_slice(),
                        serde_json::to_string(record),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        (
            "INSERT INTO diagnostics (
                diagnostic_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            document
                .diagnostics
                .iter()
                .map(|record| {
                    (
                        record.id.as_bytes().as_slice(),
                        record.repository.as_bytes().as_slice(),
                        record.generation.as_bytes().as_slice(),
                        record.provenance.as_bytes().as_slice(),
                        serde_json::to_string(record),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        (
            "INSERT INTO extensions (
                extension_id, repository_id, generation_id, provenance_id, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            document
                .extensions
                .iter()
                .map(|record| {
                    (
                        record.id.as_bytes().as_slice(),
                        record.repository.as_bytes().as_slice(),
                        record.generation.as_bytes().as_slice(),
                        record.provenance.as_bytes().as_slice(),
                        serde_json::to_string(record),
                    )
                })
                .collect::<Vec<_>>(),
        ),
    ] {
        let mut statement = transaction.prepare(sql).map_err(CatalogError::sqlite)?;
        for (id, repository, generation, provenance, payload) in records {
            context.check().map_err(CatalogError::control)?;
            statement
                .execute(params![
                    id,
                    repository,
                    generation,
                    provenance,
                    payload.map_err(CatalogError::json)?,
                ])
                .map_err(CatalogError::sqlite)?;
        }
    }
    Ok(())
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
) -> Result<(), CatalogError> {
    for (position, reference) in evidence.derivation.iter().enumerate() {
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

fn usize_to_u64(value: usize) -> Result<u64, CatalogError> {
    u64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}

fn usize_to_i64(value: usize) -> Result<i64, CatalogError> {
    i64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}
