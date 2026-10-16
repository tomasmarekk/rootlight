//! Reads selected normalized facts directly from a sealed generation oracle.
//! Stable-ID pagination bypasses lexical projection and public query budgets;
//! this diagnostic does not index sources or prove whole-language completeness.

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
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationMetadata, GenerationReadLimit, GenerationReader,
    GenerationStats, OccurrenceReadRequest, ReadPage, ReadPageCompleteness, RelationReadDirection,
    RelationReadRequest,
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
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

struct Arguments {
    directory: PathBuf,
    repository: RepositoryId,
    generation: GenerationId,
    files: BTreeSet<FileId>,
    entities: BTreeSet<SymbolId>,
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
                _ => return Err("unknown selection flag".into()),
            }
        }
        if files.is_empty() {
            return Err("at least one native file id is required".into());
        }
        Ok(Self {
            directory,
            repository,
            generation,
            files,
            entities,
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
    for &file in &arguments.files {
        inspection.require_file(file)?;
        let receipt = inspection.occurrences(file)?;
        inspection.charge(&receipt)?;
        occurrence_reads.try_reserve(1)?;
        occurrence_reads.push(receipt);
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
        "relation_reads":relation_reads.len()
    });
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
        counts,
        diagnostic_limits: json!({
            "selected_files":MAX_SELECTED_FILES,"selected_entities":MAX_SELECTED_ENTITIES,
            "witness_files":MAX_WITNESS_FILES,"occurrences":MAX_OCCURRENCES,
            "unique_relations":MAX_RELATIONS,"provenance":MAX_PROVENANCE,
            "output_bytes":MAX_OUTPUT_BYTES,"deadline_seconds":null,
            "production_limits_changed":false
        }),
        qualification: json!({
            "selected_reads_complete":true,"metadata_unchanged":true,
            "stable_id_cursor_and_count_conservation_checked":true,
            "source_refs_checked_against_native_files":true,
            "source_bytes_read":false,"provenance_scope":"direct_selected_records_not_recursive_derivation_closure",
            "full_generation_identity_rederived":false,"database_bytes_pinned":false,
            "query_projection_executed":false,"sources_indexed":false,
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
    json!({
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

    fn fact(byte: u8) -> FactId {
        FactId::from_bytes([byte; 20])
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
}
