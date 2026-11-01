//! Reads selected normalized facts directly from a sealed generation oracle.
//! Stable-ID graph reads and bounded typed RDF file reads preserve source evidence.
//! `--rdf-page-size N [--rdf-after FACT_ID]` selects one RDF page without graph reads.
//! This diagnostic bypasses lexical projection; it does not index sources or
//! prove whole-language completeness.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
};

use rootlight_cancel::Cancellation;
use rootlight_catalog::OracleReader;
use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{
    EntityRecord, FactEvidence, FileRecord, OccurrenceRecord, ProvenanceRecord, RelationEndpoint,
    RelationRecord, SourceRef,
};
use rootlight_query::{
    QueryBudget, RdfStatementPage, RdfStatementReadError, RdfStatementReadRequest,
    read_rdf_statement_page,
};
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationMetadata, GenerationReadLimit, GenerationReader,
    GenerationStats, GenerationStatsLayout, OccurrenceReadRequest, ReadPage, ReadPageCompleteness,
    RelationReadDirection, RelationReadRequest,
};
use serde::Serialize;
use serde_json::{Value, json};

type DiagnosticResult<T> = Result<T, Box<dyn Error>>;

const MAX_SELECTED_FILES: usize = 16;
const MAX_SELECTED_ENTITIES: usize = 128;
const MAX_WITNESS_FILES: usize = 512;
const MAX_OCCURRENCES: usize = 16_384;
const MAX_RELATIONS: usize = 32_768;
const MAX_PROVENANCE: usize = 32_768;
const MAX_RDF_STATEMENTS: usize = 16_384;
const MAX_RDF_EXTENSIONS: usize = 32_768;
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RDF_PAGE_SIZE: u32 = 256;

#[derive(Clone, Copy)]
struct RdfPageSelection {
    limit: GenerationReadLimit,
    after: Option<FactId>,
}

struct Arguments {
    directory: PathBuf,
    repository: RepositoryId,
    generation: GenerationId,
    files: BTreeSet<FileId>,
    entities: BTreeSet<SymbolId>,
    rdf_page: Option<RdfPageSelection>,
}

impl Arguments {
    fn parse(mut args: impl Iterator<Item = OsString>) -> DiagnosticResult<Self> {
        let directory = PathBuf::from(args.next().ok_or("missing generation directory")?);
        let repository = args
            .next()
            .ok_or("missing repository id")?
            .to_str()
            .ok_or("repository id is not utf-8")?
            .parse()?;
        let generation = args
            .next()
            .ok_or("missing generation id")?
            .to_str()
            .ok_or("generation id is not utf-8")?
            .parse()?;
        let mut files = BTreeSet::new();
        let mut entities = BTreeSet::new();
        let mut rdf_page_limit = None;
        let mut rdf_after = None;
        while let Some(flag) = args.next() {
            let id = args.next().ok_or("selection flag has no id")?;
            let id = id.to_str().ok_or("selection id is not utf-8")?;
            match flag.to_str() {
                Some("--file") => {
                    if files.len() == MAX_SELECTED_FILES || !files.insert(id.parse()?) {
                        return Err("duplicate file id or selected file bound exceeded".into());
                    }
                }
                Some("--entity") => {
                    if entities.len() == MAX_SELECTED_ENTITIES || !entities.insert(id.parse()?) {
                        return Err("duplicate entity id or selected entity bound exceeded".into());
                    }
                }
                Some("--rdf-page-size") => {
                    let size: u32 = id.parse()?;
                    if rdf_page_limit.is_some()
                        || !id.bytes().all(|byte| byte.is_ascii_digit())
                        || size == 0
                        || size > MAX_RDF_PAGE_SIZE
                    {
                        return Err("duplicate or invalid RDF page size".into());
                    }
                    rdf_page_limit = Some(GenerationReadLimit::new(size)?);
                }
                Some("--rdf-after") => {
                    if rdf_after.is_some() {
                        return Err("duplicate RDF continuation cursor".into());
                    }
                    rdf_after = Some(id.parse()?);
                }
                _ => return Err("unknown selection flag".into()),
            }
        }
        if files.is_empty() {
            return Err("at least one native file id is required".into());
        }
        if rdf_page_limit.is_some() && (files.len() != 1 || !entities.is_empty()) {
            return Err("RDF page mode requires exactly one file and no entities".into());
        }
        if rdf_after.is_some() && rdf_page_limit.is_none() {
            return Err("RDF continuation requires page mode".into());
        }
        Ok(Self {
            directory,
            repository,
            generation,
            files,
            entities,
            rdf_page: rdf_page_limit.map(|limit| RdfPageSelection {
                limit,
                after: rdf_after,
            }),
        })
    }
}

#[derive(Default, Serialize)]
struct Pagination {
    total_available: Option<u64>,
    returned: u64,
    pages: u64,
    continuation_cursors: Vec<FactId>,
    complete: bool,
    #[serde(skip)]
    last_id: Option<FactId>,
}

impl Pagination {
    fn observe<T>(
        &mut self,
        page: &ReadPage<T>,
        maximum: usize,
        stable_id: impl Fn(&T) -> FactId,
    ) -> DiagnosticResult<()> {
        if self.complete
            || page.total_available() > u64::try_from(maximum)?
            || self
                .total_available
                .is_some_and(|total| total != page.total_available())
        {
            return Err("page total changed or diagnostic record bound exceeded".into());
        }
        self.total_available = Some(page.total_available());
        for item in page.items() {
            let id = stable_id(item);
            if self.last_id.is_some_and(|previous| previous >= id) {
                return Err("stable-id pagination did not advance".into());
            }
            self.last_id = Some(id);
        }
        self.returned = self
            .returned
            .checked_add(u64::try_from(page.items().len())?)
            .ok_or("page record count overflow")?;
        self.pages = self.pages.checked_add(1).ok_or("page count overflow")?;
        if self.returned > page.total_available() {
            return Err("materialized records exceed the page total".into());
        }
        match page.completeness() {
            ReadPageCompleteness::Complete => {
                if page.next_cursor().is_some() || self.returned != page.total_available() {
                    return Err("complete page did not conserve its record total".into());
                }
                self.complete = true;
            }
            ReadPageCompleteness::Truncated => {
                let cursor = page.next_cursor().ok_or("truncated page has no cursor")?;
                if page.items().is_empty()
                    || Some(cursor) != self.last_id
                    || self.returned >= page.total_available()
                {
                    return Err("truncated page has inconsistent continuation".into());
                }
                self.continuation_cursors.try_reserve(1)?;
                self.continuation_cursors.push(cursor);
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct OccurrenceRead {
    file: FileId,
    page_size: u16,
    pagination: Pagination,
}

#[derive(Serialize)]
struct RelationRead {
    anchor: RelationEndpoint,
    direction: &'static str,
    page_size: u16,
    pagination: Pagination,
}

#[derive(Serialize)]
struct RdfStatementRead {
    file: FileId,
    page_size: u16,
    total_file_extensions: u64,
    scanned_file_extensions: u64,
    returned_statements: u64,
    pages: Vec<RdfStatementPage>,
    complete: bool,
}

#[derive(Serialize)]
struct RdfStatementOmission {
    file: FileId,
    reason: &'static str,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    metadata: Value,
    generation_stats: Value,
    selected_file_ids: BTreeSet<FileId>,
    selected_entity_ids: BTreeSet<SymbolId>,
    files: Vec<FileRecord>,
    entities: Vec<EntityRecord>,
    missing_entity_ids: Vec<SymbolId>,
    occurrences: Vec<OccurrenceRecord>,
    relations: Vec<RelationRecord>,
    provenance: Vec<ProvenanceRecord>,
    occurrence_reads: Vec<OccurrenceRead>,
    relation_reads: Vec<RelationRead>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rdf_statement_reads: Vec<RdfStatementRead>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rdf_statement_omissions: Vec<RdfStatementOmission>,
    counts: Value,
    diagnostic_limits: Value,
    qualification: Value,
}

struct Inspection<'reader, 'cancel> {
    reader: &'reader OracleReader,
    context: GenerationContext<'cancel>,
    metadata: GenerationMetadata,
    files: BTreeMap<FileId, FileRecord>,
    occurrences: Vec<OccurrenceRecord>,
    relations: BTreeMap<FactId, RelationRecord>,
    provenance_ids: BTreeSet<FactId>,
    encoded: CountingWriter,
}

impl Inspection<'_, '_> {
    fn owner(&self, repository: RepositoryId, generation: GenerationId) -> DiagnosticResult<()> {
        self.context.check()?;
        if repository != self.metadata.repository() || generation != self.metadata.generation() {
            return Err("selected record belongs to another repository or generation".into());
        }
        Ok(())
    }

    fn charge(&mut self, value: &impl Serialize) -> DiagnosticResult<()> {
        self.context.check()?;
        serde_json::to_writer(&mut self.encoded, value)?;
        Ok(())
    }

    fn require_file(&mut self, id: FileId) -> DiagnosticResult<()> {
        if self.files.contains_key(&id) {
            return Ok(());
        }
        if self.files.len() == MAX_WITNESS_FILES {
            return Err("source witness file bound exceeded".into());
        }
        let file = self
            .reader
            .file(id, &self.context)?
            .ok_or("selected or source witness file is not retained")?;
        self.owner(file.repository, file.generation)?;
        let source = file.evidence.source.as_ref().ok_or("file has no source")?;
        if file.id != id
            || source.repository() != file.repository
            || source.generation() != file.generation
            || source.span().file() != id
            || source.span().start_byte() != 0
            || source.span().end_byte() != file.byte_length
            || source.content_hash() != file.content_hash
        {
            return Err("retained file identity or full source span is inconsistent".into());
        }
        self.charge(&file)?;
        self.provenance_ids.insert(file.provenance);
        self.files.insert(id, file);
        Ok(())
    }

    fn source(&mut self, source: &SourceRef) -> DiagnosticResult<()> {
        self.owner(source.repository(), source.generation())?;
        self.require_file(source.span().file())?;
        let file = self
            .files
            .get(&source.span().file())
            .ok_or("source witness file was not retained")?;
        if source.content_hash() != file.content_hash
            || source.span().start_byte() > source.span().end_byte()
            || source.span().end_byte() > file.byte_length
        {
            return Err("source witness hash or byte bounds differ from its file".into());
        }
        Ok(())
    }

    fn evidence(&mut self, evidence: &FactEvidence) -> DiagnosticResult<()> {
        if let Some(source) = &evidence.source {
            self.source(source)?;
        }
        Ok(())
    }

    fn occurrences(&mut self, file: FileId) -> DiagnosticResult<OccurrenceRead> {
        let limit = GenerationReadLimit::default();
        let mut request = OccurrenceReadRequest::new(file, limit);
        let mut pagination = Pagination::default();
        loop {
            self.context.check()?;
            let page = self.reader.occurrences(&request, &self.context)?;
            pagination.observe(&page, MAX_OCCURRENCES, |record| record.id)?;
            for record in page.items() {
                self.owner(record.repository, record.generation)?;
                if record.file != file || record.source.span().file() != file {
                    return Err("occurrence is outside its selected file".into());
                }
                self.source(&record.source)?;
                self.evidence(&record.evidence)?;
            }
            let cursor = page.next_cursor();
            for record in page.into_items() {
                if self.occurrences.len() == MAX_OCCURRENCES {
                    return Err("selected occurrence bound exceeded".into());
                }
                self.charge(&record)?;
                self.provenance_ids.insert(record.provenance);
                self.occurrences.try_reserve(1)?;
                self.occurrences.push(record);
            }
            let Some(cursor) = cursor else { break };
            request = request.with_after(cursor);
        }
        Ok(OccurrenceRead {
            file,
            page_size: limit.get(),
            pagination,
        })
    }

    fn relations(
        &mut self,
        anchor: RelationEndpoint,
        direction: RelationReadDirection,
    ) -> DiagnosticResult<RelationRead> {
        let limit = GenerationReadLimit::default();
        let mut request = RelationReadRequest::new(anchor, direction, limit);
        let mut pagination = Pagination::default();
        loop {
            self.context.check()?;
            let page = self.reader.relations(&request, &self.context)?;
            pagination.observe(&page, MAX_RELATIONS, |record| record.id)?;
            let cursor = page.next_cursor();
            for record in page.into_items() {
                self.owner(record.repository, record.generation)?;
                let endpoint = match direction {
                    RelationReadDirection::Outgoing => record.subject,
                    RelationReadDirection::Incoming => record.object,
                };
                if endpoint != anchor {
                    return Err("relation read returned another endpoint".into());
                }
                if let Some(existing) = self.relations.get(&record.id) {
                    if existing != &record {
                        return Err("same relation id returned conflicting records".into());
                    }
                    continue;
                }
                if self.relations.len() == MAX_RELATIONS {
                    return Err("selected relation bound exceeded".into());
                }
                self.evidence(&record.evidence)?;
                self.charge(&record)?;
                self.provenance_ids.insert(record.provenance);
                self.relations.insert(record.id, record);
            }
            let Some(cursor) = cursor else { break };
            request = request.with_after(cursor);
        }
        Ok(RelationRead {
            anchor,
            direction: match direction {
                RelationReadDirection::Outgoing => "outgoing",
                RelationReadDirection::Incoming => "incoming",
            },
            page_size: limit.get(),
            pagination,
        })
    }

    fn provenance(&mut self) -> DiagnosticResult<Vec<ProvenanceRecord>> {
        let mut records = BTreeMap::new();
        while let Some(id) = self.provenance_ids.pop_first() {
            self.context.check()?;
            if records.contains_key(&id) {
                continue;
            }
            if records.len() == MAX_PROVENANCE {
                return Err("selected provenance bound exceeded".into());
            }
            let record = self
                .reader
                .provenance(id, &self.context)?
                .ok_or("selected fact provenance is not retained")?;
            self.owner(record.repository, record.generation)?;
            if record.id != id {
                return Err("provenance read returned another id".into());
            }
            for source in record.input_sources.iter().chain(&record.evidence_sources) {
                self.source(source)?;
            }
            self.charge(&record)?;
            records.insert(id, record);
        }
        map_values(records)
    }

    fn rdf_statements(
        &mut self,
        file: FileId,
        selection: Option<RdfPageSelection>,
    ) -> DiagnosticResult<Option<RdfStatementRead>> {
        let limit = selection.map_or_else(GenerationReadLimit::default, |page| page.limit);
        let mut request = RdfStatementReadRequest::new(
            self.metadata.repository(),
            self.metadata.generation(),
            file,
            limit,
        );
        let initial_after = selection.and_then(|page| page.after);
        if let Some(after) = initial_after {
            request = request.with_after(after);
        }
        let mut total = None;
        let mut scanned = 0u64;
        let mut returned = 0u64;
        let mut after = initial_after;
        let mut pages = Vec::new();
        loop {
            self.context.check()?;
            let page = match read_rdf_statement_page(
                self.reader,
                request,
                &self.context,
                QueryBudget::default(),
            ) {
                Ok(page) => page,
                Err(RdfStatementReadError::IndexedFileReadUnsupported) if pages.is_empty() => {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            if page.scope() != "file_extensions"
                || page.total_extensions() > u64::try_from(MAX_RDF_EXTENSIONS)?
                || total.is_some_and(|old| old != page.total_extensions())
            {
                return Err("RDF indexed page total or scope is inconsistent".into());
            }
            total = Some(page.total_extensions());
            scanned = scanned
                .checked_add(page.scanned_extensions())
                .ok_or("RDF scanned count overflow")?;
            returned = returned
                .checked_add(u64::try_from(page.statements().len())?)
                .ok_or("RDF statement count overflow")?;
            if returned > u64::try_from(MAX_RDF_STATEMENTS)? || scanned > page.total_extensions() {
                return Err("RDF diagnostic bound or page total exceeded".into());
            }
            for record in page.statements() {
                let statement = record.statement();
                self.source(statement.source())?;
                for term in [
                    statement.subject(),
                    statement.predicate(),
                    statement.object(),
                ] {
                    self.source(term.source())?;
                }
                self.provenance_ids.insert(record.provenance());
            }
            let complete = page.range_complete();
            let cursor = page.next_cursor();
            if complete {
                if cursor.is_some()
                    || (initial_after.is_none() && scanned != page.total_extensions())
                {
                    return Err("RDF complete pages did not conserve the file total".into());
                }
            } else if page.scanned_extensions() == 0
                || scanned >= page.total_extensions()
                || cursor.is_none()
                || after.is_some_and(|previous| cursor.is_some_and(|cursor| cursor <= previous))
            {
                return Err("RDF indexed continuation did not advance".into());
            }
            self.charge(&page)?;
            pages.try_reserve(1)?;
            pages.push(page);
            if complete || selection.is_some() {
                return Ok(Some(RdfStatementRead {
                    file,
                    page_size: limit.get(),
                    total_file_extensions: total.ok_or("RDF total missing")?,
                    scanned_file_extensions: scanned,
                    returned_statements: returned,
                    pages,
                    // A terminal suffix exhausts its range without covering the file prefix.
                    complete: complete && initial_after.is_none(),
                }));
            }
            after = cursor;
            request = request.with_after(cursor.ok_or("RDF cursor missing")?);
        }
    }
}

fn inspect(
    reader: &OracleReader,
    arguments: Arguments,
    context: GenerationContext<'_>,
) -> DiagnosticResult<Report> {
    let metadata = reader.metadata();
    let stats = reader.stats();
    if metadata.repository() != arguments.repository
        || metadata.generation() != arguments.generation
    {
        return Err("oracle metadata does not match the requested identity".into());
    }
    let mut inspection = Inspection {
        reader,
        context,
        metadata,
        files: BTreeMap::new(),
        occurrences: Vec::new(),
        relations: BTreeMap::new(),
        provenance_ids: BTreeSet::new(),
        encoded: CountingWriter::new(MAX_OUTPUT_BYTES),
    };
    let mut occurrence_reads = Vec::new();
    let mut rdf_statement_reads = Vec::new();
    let mut rdf_statement_omissions = Vec::new();
    for &file in &arguments.files {
        inspection.require_file(file)?;
        if arguments.rdf_page.is_some()
            && !inspection
                .files
                .get(&file)
                .is_some_and(|file| file.language == "ntriples")
        {
            return Err("RDF page mode requires a native ntriples file".into());
        }
        if arguments.rdf_page.is_none() {
            let receipt = inspection.occurrences(file)?;
            inspection.charge(&receipt)?;
            occurrence_reads.try_reserve(1)?;
            occurrence_reads.push(receipt);
        }
        if inspection
            .files
            .get(&file)
            .is_some_and(|file| file.language == "ntriples")
        {
            if let Some(read) = inspection.rdf_statements(file, arguments.rdf_page)? {
                rdf_statement_reads.try_reserve(1)?;
                rdf_statement_reads.push(read);
            } else {
                rdf_statement_omissions.push(RdfStatementOmission {
                    file,
                    reason: "indexed_file_extensions_unsupported",
                });
            }
        }
    }
    let mut entities = Vec::new();
    let mut missing_entity_ids = Vec::new();
    for &id in &arguments.entities {
        context.check()?;
        if let Some(entity) = reader.entity(id, &context)? {
            inspection.owner(entity.repository, entity.generation)?;
            if entity.id != id {
                return Err("entity read returned another id".into());
            }
            inspection.evidence(&entity.evidence)?;
            inspection.charge(&entity)?;
            inspection.provenance_ids.insert(entity.provenance);
            entities.try_reserve(1)?;
            entities.push(entity);
        } else {
            missing_entity_ids.try_reserve(1)?;
            missing_entity_ids.push(id);
        }
    }
    let mut anchors = Vec::new();
    anchors.try_reserve_exact(
        arguments
            .entities
            .len()
            .checked_add(inspection.occurrences.len())
            .ok_or("anchor count overflow")?,
    )?;
    anchors.extend(
        arguments
            .entities
            .iter()
            .copied()
            .map(RelationEndpoint::Entity),
    );
    anchors.extend(
        inspection
            .occurrences
            .iter()
            .map(|record| RelationEndpoint::Occurrence(record.id)),
    );
    let mut relation_reads = Vec::new();
    for anchor in anchors {
        for direction in [
            RelationReadDirection::Outgoing,
            RelationReadDirection::Incoming,
        ] {
            let receipt = inspection.relations(anchor, direction)?;
            if matches!(anchor, RelationEndpoint::Entity(id) if missing_entity_ids.contains(&id))
                && receipt.pagination.total_available != Some(0)
            {
                return Err("relations refer to a missing selected entity".into());
            }
            inspection.charge(&receipt)?;
            relation_reads.try_reserve(1)?;
            relation_reads.push(receipt);
        }
    }
    let provenance = inspection.provenance()?;
    // Reopening verifies the same sealed header after all indexed reads. Full
    // generation recipe rederivation and database byte pins belong to the caller.
    let after = OracleReader::open_in(
        &arguments.directory,
        &GenerationContext::new(context.cancellation(), GenerationBudget::default()),
    )?;
    if after.metadata() != metadata || after.stats() != stats {
        return Err("oracle metadata or cardinalities changed during inspection".into());
    }
    context.check()?;
    let counts = json!({
        "requested_files":arguments.files.len(),
        "source_witness_files":inspection.files.len(),
        "requested_entities":arguments.entities.len(),
        "returned_entities":entities.len(),
        "missing_entities":missing_entity_ids.len(),
        "occurrences":inspection.occurrences.len(),
        "unique_relations":inspection.relations.len(),
        "direct_provenance_records":provenance.len(),
        "occurrence_reads":occurrence_reads.len(),
        "relation_reads":relation_reads.len(),
        "rdf_statements":rdf_statement_reads.iter().map(|read|read.returned_statements).sum::<u64>(),
        "rdf_statement_reads":rdf_statement_reads.len(),
        "rdf_statement_omissions":rdf_statement_omissions.len()
    });
    let rdf_complete =
        rdf_statement_omissions.is_empty() && rdf_statement_reads.iter().all(|read| read.complete);
    let rdf_executed = !rdf_statement_reads.is_empty();
    Ok(Report {
        schema: "rootlight-snapshot-facts-diagnostic/1",
        metadata: metadata_json(metadata),
        generation_stats: stats_json(stats),
        selected_file_ids: arguments.files,
        selected_entity_ids: arguments.entities,
        files: map_values(inspection.files)?,
        entities,
        missing_entity_ids,
        occurrences: inspection.occurrences,
        relations: map_values(inspection.relations)?,
        provenance,
        occurrence_reads,
        relation_reads,
        rdf_statement_reads,
        rdf_statement_omissions,
        counts,
        diagnostic_limits: json!({
            "selected_files":MAX_SELECTED_FILES,"selected_entities":MAX_SELECTED_ENTITIES,
            "witness_files":MAX_WITNESS_FILES,"occurrences":MAX_OCCURRENCES,
            "unique_relations":MAX_RELATIONS,"provenance":MAX_PROVENANCE,
            "rdf_statements":MAX_RDF_STATEMENTS,"rdf_file_extensions":MAX_RDF_EXTENSIONS,
            "output_bytes":MAX_OUTPUT_BYTES,"deadline_seconds":null,
            "production_limits_changed":false
        }),
        qualification: json!({
            "selected_reads_complete":rdf_complete,"metadata_unchanged":true,
            "selected_read_scope":if arguments.rdf_page.is_some() { "single_rdf_file_extension_page" } else { "selected_files_and_entities" },
            "graph_reads_executed":arguments.rdf_page.is_none(),
            "stable_id_cursor_and_count_conservation_checked":true,
            "source_refs_checked_against_native_files":true,
            "source_bytes_read":false,"provenance_scope":"direct_selected_records_not_recursive_derivation_closure",
            "full_generation_identity_rederived":false,"database_bytes_pinned":false,
            "query_projection_executed":false,"sources_indexed":false,
            "typed_rdf_query_executed":rdf_executed,"rdf_statement_reads_complete":rdf_complete,
            "rdf_statement_scope":"selected_native_ntriples_files_only",
            "source_code_executed":false,"public_queries_qualified":false,
            "installed_mcp_qualified":false,"full_semantic_completeness_proven":false
        }),
    })
}

fn metadata_json(metadata: GenerationMetadata) -> Value {
    json!({
        "repository":metadata.repository(),"generation":metadata.generation(),"parent":metadata.parent(),
        "contract_version":metadata.contract_version(),"ir_version":metadata.ir_version(),
        "manifest_hash":metadata.manifest_hash(),"configuration_hash":metadata.configuration_hash(),
        "provider_set_hash":metadata.provider_set_hash()
    })
}

fn stats_json(stats: GenerationStats) -> Value {
    let layout = match stats.layout() {
        GenerationStatsLayout::CanonicalOracle => "canonical",
        GenerationStatsLayout::CanonicalOracleWithRdfV1 => "canonical_with_rdf_v1",
    };
    json!({
        "layout":layout,
        "files":stats.files(),"entities":stats.entities(),"occurrences":stats.occurrences(),
        "relations":stats.relations(),"provenance":stats.provenance(),"source_mappings":stats.source_mappings(),
        "coverage":stats.coverage(),"skipped_regions":stats.skipped_regions(),"diagnostics":stats.diagnostics(),
        "extensions":stats.extensions(),"source_refs":stats.source_refs(),"stored_rows":stats.stored_rows(),
        "text_bytes":stats.text_bytes()
    })
}

fn map_values<K, V>(map: BTreeMap<K, V>) -> DiagnosticResult<Vec<V>> {
    let mut values = Vec::new();
    values.try_reserve_exact(map.len())?;
    values.extend(map.into_values());
    Ok(values)
}

struct CountingWriter {
    bytes: usize,
    maximum: usize,
}

impl CountingWriter {
    const fn new(maximum: usize) -> Self {
        Self { bytes: 0, maximum }
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("diagnostic byte count overflow"))?;
        if bytes > self.maximum {
            return Err(io::Error::other("diagnostic output bound exceeded"));
        }
        self.bytes = bytes;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct OutputWriter {
    bytes: Vec<u8>,
    counter: CountingWriter,
}

impl Write for OutputWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.counter.write_all(buffer)?;
        self.bytes
            .try_reserve(buffer.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn main() -> DiagnosticResult<()> {
    let mut arguments = Arguments::parse(std::env::args_os().skip(1))?;
    arguments.directory = std::fs::canonicalize(&arguments.directory)?;
    let cancellation = Cancellation::new();
    let open_context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let reader = OracleReader::open_in(&arguments.directory, &open_context)?;
    let read_budget = GenerationBudget::new(262_144, 65_536, u64::try_from(MAX_OUTPUT_BYTES)?)?;
    let report = inspect(
        &reader,
        arguments,
        GenerationContext::new(&cancellation, read_budget),
    )?;
    // Nothing reaches stdout until the complete report fits the diagnostic bound.
    let mut output = OutputWriter {
        bytes: Vec::new(),
        counter: CountingWriter::new(MAX_OUTPUT_BYTES),
    };
    serde_json::to_writer(&mut output, &report)?;
    output.write_all(b"\n")?;
    cancellation.check()?;
    io::stdout().lock().write_all(&output.bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_catalog::OracleWriter;
    use rootlight_service::FirstSliceService;
    use rootlight_storage::IdentityVerifiedGeneration;

    fn fact(byte: u8) -> FactId {
        FactId::from_bytes([byte; 20])
    }

    #[test]
    fn generation_stats_report_layout_without_changing_numeric_fields() {
        let canonical = GenerationStats::new(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 100, 101).unwrap();
        let mut expected = json!({
            "layout":"canonical",
            "files":1,"entities":2,"occurrences":3,"relations":4,"provenance":5,
            "source_mappings":6,"coverage":7,"skipped_regions":8,"diagnostics":9,
            "extensions":10,"source_refs":11,"stored_rows":100,"text_bytes":101
        });
        assert_eq!(stats_json(canonical), expected);

        let packed = canonical.checked_add_rdf_v1(3, 2, 20, 30, 10).unwrap();
        expected["layout"] = json!("canonical_with_rdf_v1");
        expected["extensions"] = json!(13);
        expected["source_refs"] = json!(13);
        expected["stored_rows"] = json!(104);
        expected["text_bytes"] = json!(161);
        assert_eq!(stats_json(packed), expected);

        let same_counters =
            GenerationStats::new(1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 13, 104, 161).unwrap();
        expected["layout"] = json!("canonical");
        assert_eq!(stats_json(same_counters), expected);
    }

    fn page(ids: &[u8], total: u64) -> ReadPage<FactId> {
        ReadPage::from_limit_plus_one(
            ids.iter().copied().map(fact).collect(),
            total,
            GenerationReadLimit::new(1).unwrap(),
            |id| *id,
        )
        .unwrap()
    }

    #[test]
    fn stable_pages_conserve_totals_and_complete_empty_reads() {
        let mut progress = Pagination::default();
        progress.observe(&page(&[1, 2], 2), 2, |id| *id).unwrap();
        assert!(!progress.complete);
        assert_eq!(progress.continuation_cursors, vec![fact(1)]);
        progress.observe(&page(&[2], 2), 2, |id| *id).unwrap();
        assert!(progress.complete);
        assert_eq!(progress.returned, 2);
        assert_eq!(progress.pages, 2);
        let mut empty = Pagination::default();
        empty.observe(&page(&[], 0), 2, |id| *id).unwrap();
        assert!(empty.complete);
        assert_eq!(empty.returned, 0);
    }

    #[test]
    fn pagination_rejects_changed_totals_repeated_ids_and_early_completion() {
        let mut changed = Pagination::default();
        changed.observe(&page(&[1, 2], 2), 3, |id| *id).unwrap();
        assert!(changed.observe(&page(&[2], 3), 3, |id| *id).is_err());
        let mut repeated = Pagination::default();
        repeated.observe(&page(&[1, 2], 2), 2, |id| *id).unwrap();
        assert!(repeated.observe(&page(&[1], 2), 2, |id| *id).is_err());
        let mut incomplete = Pagination::default();
        assert!(incomplete.observe(&page(&[1], 2), 2, |id| *id).is_err());
    }

    #[test]
    fn diagnostic_bound_rejects_page_before_qualification() {
        let mut progress = Pagination::default();
        assert!(progress.observe(&page(&[1, 2], 2), 1, |id| *id).is_err());
        assert!(!progress.complete);
    }

    #[test]
    fn encoded_output_is_bounded_without_partial_external_output() {
        let mut output = OutputWriter {
            bytes: Vec::new(),
            counter: CountingWriter::new(3),
        };
        output.write_all(b"abc").unwrap();
        assert!(output.write_all(b"d").is_err());
        assert_eq!(output.bytes, b"abc");
        let mut counter = CountingWriter::new(3);
        assert!(serde_json::to_writer(&mut counter, &"abc").is_err());
    }

    #[test]
    fn arguments_require_native_selection_and_reject_duplicate_or_unknown_flags() {
        let prefix = || {
            vec![
                OsString::from("generation"),
                RepositoryId::from_bytes([1; 16]).to_string().into(),
                GenerationId::from_bytes([2; 20]).to_string().into(),
            ]
        };
        assert!(Arguments::parse(prefix().into_iter()).is_err());
        let file = FileId::from_bytes([3; 20]).to_string();
        let mut valid = prefix();
        valid.extend([OsString::from("--file"), file.clone().into()]);
        assert!(Arguments::parse(valid.clone().into_iter()).is_ok());
        valid.extend([OsString::from("--file"), file.into()]);
        assert!(Arguments::parse(valid.into_iter()).is_err());
        let mut unknown = prefix();
        unknown.extend([OsString::from("--path"), OsString::from("source.fs")]);
        assert!(Arguments::parse(unknown.into_iter()).is_err());
    }

    fn page_arguments(flags: &[&str]) -> DiagnosticResult<Arguments> {
        let mut args = vec![
            OsString::from("generation"),
            RepositoryId::from_bytes([1; 16]).to_string().into(),
            GenerationId::from_bytes([2; 20]).to_string().into(),
            OsString::from("--file"),
            FileId::from_bytes([3; 20]).to_string().into(),
        ];
        args.extend(flags.iter().map(OsString::from));
        Arguments::parse(args.into_iter())
    }

    #[test]
    fn rdf_page_arguments_accept_first_and_resumed_bounded_reads() {
        for size in ["1", "256"] {
            let first = page_arguments(&["--rdf-page-size", size]).unwrap();
            assert_eq!(first.rdf_page.unwrap().limit.get().to_string(), size);
            assert!(first.rdf_page.unwrap().after.is_none());
        }
        let cursor = fact(9).to_string();
        let resumed = page_arguments(&["--rdf-after", &cursor, "--rdf-page-size", "2"]).unwrap();
        assert_eq!(resumed.rdf_page.unwrap().after, Some(fact(9)));
    }

    #[test]
    fn rdf_page_arguments_reject_invalid_limits_cursors_and_selections() {
        for size in ["0", "257", "4096", "-1", "+1", "1.5", "", "4294967296"] {
            assert!(page_arguments(&["--rdf-page-size", size]).is_err());
        }
        let cursor = fact(9).to_string();
        let second_file = FileId::from_bytes([4; 20]).to_string();
        let entity = SymbolId::from_bytes([5; 20]).to_string();
        for flags in [
            vec!["--rdf-page-size"],
            vec!["--rdf-after", &cursor],
            vec!["--rdf-page-size", "1", "--rdf-after"],
            vec!["--rdf-page-size", "1", "--rdf-after", "malformed"],
            vec!["--rdf-page-size", "1", "--rdf-page-size", "2"],
            vec![
                "--rdf-page-size",
                "1",
                "--rdf-after",
                &cursor,
                "--rdf-after",
                &cursor,
            ],
            vec!["--rdf-page-size", "1", "--file", &second_file],
            vec!["--rdf-page-size", "1", "--entity", &entity],
        ] {
            assert!(page_arguments(&flags).is_err(), "accepted {flags:?}");
        }
    }

    fn sealed_rdf_fixture() -> (tempfile::TempDir, PathBuf, OracleReader, FileId) {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let source_root = root.join("source");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::write(
            source_root.join("statements.nt"),
            "<s:one> <p:value> _:blank .\n<s:two> <p:value> \"literal\"@en .\n_:blank <p:value> <o:three> .\n",
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new(1).unwrap();
        let receipt = service
            .index_repository(&source_root, &cancellation)
            .unwrap();
        let snapshot = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                Clone::clone,
            )
            .unwrap();
        let file = snapshot.document().files[0].id;
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let verified = IdentityVerifiedGeneration::verify_snapshot(snapshot, &context).unwrap();
        let oracle = root.join("oracle");
        std::fs::create_dir(&oracle).unwrap();
        let reader = OracleWriter::create_in(&oracle)
            .unwrap()
            .seal(verified, &context)
            .unwrap();
        (directory, oracle, reader, file)
    }

    fn inspect_rdf_fixture(
        reader: &OracleReader,
        directory: &std::path::Path,
        file: FileId,
        rdf_page: Option<RdfPageSelection>,
    ) -> Report {
        let cancellation = Cancellation::new();
        inspect(
            reader,
            Arguments {
                directory: directory.to_owned(),
                repository: reader.metadata().repository(),
                generation: reader.metadata().generation(),
                files: BTreeSet::from([file]),
                entities: BTreeSet::new(),
                rdf_page,
            },
            GenerationContext::new(&cancellation, GenerationBudget::default()),
        )
        .unwrap()
    }

    #[test]
    fn rdf_pages_conserve_sealed_statements_and_keep_suffix_scope_incomplete() {
        let (_directory, oracle, reader, file) = sealed_rdf_fixture();
        let bulk = inspect_rdf_fixture(&reader, &oracle, file, None);
        let bulk_read = &bulk.rdf_statement_reads[0];
        assert!(bulk_read.complete);
        assert_eq!(bulk.occurrence_reads.len(), 1);
        assert_eq!(bulk.qualification["graph_reads_executed"], true);
        let expected: Vec<_> = bulk_read
            .pages
            .iter()
            .flat_map(|page| page.statements().iter().cloned())
            .collect();
        assert_eq!(expected.len(), 3);
        let mut actual = Vec::new();
        let mut after = None;
        let mut scanned = 0;
        let mut empty_continuations = 0;
        loop {
            let report = inspect_rdf_fixture(
                &reader,
                &oracle,
                file,
                Some(RdfPageSelection {
                    limit: GenerationReadLimit::new(1).unwrap(),
                    after,
                }),
            );
            let read = &report.rdf_statement_reads[0];
            assert!(report.occurrence_reads.is_empty());
            assert!(report.relation_reads.is_empty());
            assert!(report.occurrences.is_empty());
            assert!(report.relations.is_empty());
            assert_eq!(report.qualification["graph_reads_executed"], false);
            assert_eq!(read.pages.len(), 1);
            let page = &read.pages[0];
            assert_eq!(
                serde_json::to_value(page).unwrap()["requested_after"],
                json!(after)
            );
            assert_eq!(page.total_extensions(), bulk_read.total_file_extensions);
            assert_eq!(page.total_matching_statements(), None);
            assert!(!read.complete);
            assert_eq!(report.qualification["rdf_statement_reads_complete"], false);
            assert_eq!(report.qualification["selected_reads_complete"], false);
            scanned += page.scanned_extensions();
            actual.extend_from_slice(page.statements());
            if page.range_complete() {
                assert!(after.is_some());
                assert!(page.next_cursor().is_none());
                assert!(read.scanned_file_extensions < read.total_file_extensions);
                break;
            }
            let cursor = page.next_cursor().unwrap();
            assert!(after.is_none_or(|previous| previous < cursor));
            if page.statements().is_empty() {
                empty_continuations += 1;
            }
            after = Some(cursor);
        }
        assert!(empty_continuations > 0);
        assert_eq!(scanned, bulk_read.total_file_extensions);
        assert_eq!(actual, expected);
        assert_eq!(bulk.qualification["selected_reads_complete"], true);
    }

    #[test]
    fn first_rdf_page_covering_entire_file_is_complete() {
        let (_directory, oracle, reader, file) = sealed_rdf_fixture();
        let report = inspect_rdf_fixture(
            &reader,
            &oracle,
            file,
            Some(RdfPageSelection {
                limit: GenerationReadLimit::new(MAX_RDF_PAGE_SIZE).unwrap(),
                after: None,
            }),
        );
        let read = &report.rdf_statement_reads[0];
        assert!(read.complete);
        assert_eq!(read.pages.len(), 1);
        assert!(read.pages[0].range_complete());
        assert_eq!(read.pages[0].total_matching_statements(), Some(3));
        assert_eq!(read.scanned_file_extensions, read.total_file_extensions);
        assert_eq!(report.qualification["rdf_statement_reads_complete"], true);
        assert_eq!(report.qualification["selected_reads_complete"], true);
    }
}
