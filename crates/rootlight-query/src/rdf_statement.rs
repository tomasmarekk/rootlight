//! Bounded typed RDF reads over immutable, file-indexed extension pages.
//! Global-window diagnostics remain explicit; unsupported file indexing never
//! falls back to a generation scan or an invented empty complete result.

use std::{collections::BTreeMap, error::Error, time::Instant};

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId};
use rootlight_ir::{
    FileRecord, RDF_STATEMENT_NAMESPACE, RdfStatement, decode_rdf_statement_envelope,
};
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationControlError, GenerationMetadata,
    GenerationReadLimit, GenerationReader, GenerationResource, ReadPageCompleteness,
};
use serde::Serialize;

use crate::model::{checked_add, checked_u128_to_u64, checked_usize_to_u64};
use crate::service::{QueryControl, serialized_size};
use crate::{QueryBudget, QueryError, QueryResource, RepositoryDataTrust};

/// One file selection within a bounded immutable extension-ID range.
#[derive(Debug, Clone, Copy)]
pub struct RdfStatementReadRequest {
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    after: Option<FactId>,
    limit: GenerationReadLimit,
}

impl RdfStatementReadRequest {
    /// Pins a repository, generation, selected file and backend page limit.
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        generation: GenerationId,
        file: FileId,
        limit: GenerationReadLimit,
    ) -> Self {
        Self {
            repository,
            generation,
            file,
            after: None,
            limit,
        }
    }
    /// Continues strictly after the prior unfiltered extension cursor.
    #[must_use]
    pub const fn with_after(mut self, after: FactId) -> Self {
        self.after = Some(after);
        self
    }
}

/// One canonically decoded RDF statement with its persisted fact/provenance identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RdfStatementRecord {
    id: FactId,
    provenance: FactId,
    statement: RdfStatement,
}

impl RdfStatementRecord {
    /// Returns the canonical persisted statement fact ID.
    #[must_use]
    pub const fn id(&self) -> FactId {
        self.id
    }
    /// Returns the checked retained provenance identity.
    #[must_use]
    pub const fn provenance(&self) -> FactId {
        self.provenance
    }
    /// Borrows decoded RDF roles, values and exact source evidence.
    #[must_use]
    pub const fn statement(&self) -> &RdfStatement {
        &self.statement
    }
}

/// Measured row/output work and conservative decode-memory admission for one page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RdfStatementReadUsage {
    /// Extension, file and provenance records inspected, including the backend probe.
    pub rows: u64,
    /// Selected typed RDF statements returned.
    pub results: u64,
    /// Raw statement spelling bytes returned; no source-file IO is implied.
    pub source_bytes: u64,
    /// Conservative admitted page/decode workspace, rather than an RSS measurement.
    pub memory_upper_bytes: u64,
    /// Exact serialized bytes of this page including its counters.
    pub json_bytes: u64,
    /// Conservative UTF-8-byte token upper bound.
    pub estimated_tokens: u64,
    /// Elapsed work before final output accounting, rounded up to microseconds.
    pub elapsed_micros: u64,
}

/// A filtered typed result preserving its unfiltered cursor and completeness scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RdfStatementPage {
    scope: &'static str,
    trust: RepositoryDataTrust,
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    requested_after: Option<FactId>,
    statements: Vec<RdfStatementRecord>,
    scanned_extensions: u64,
    total_extensions: u64,
    skipped_non_rdf: u64,
    skipped_other_files: u64,
    range_complete: bool,
    next_cursor: Option<FactId>,
    total_matching_statements: Option<u64>,
    usage: RdfStatementReadUsage,
}

impl RdfStatementPage {
    /// Borrows the matching typed statements in stable fact-ID order.
    #[must_use]
    pub fn statements(&self) -> &[RdfStatementRecord] {
        &self.statements
    }
    /// Returns the exclusive unfiltered extension cursor, including empty filtered pages.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<FactId> {
        self.next_cursor
    }
    /// Reports exhaustion of the requested extension range, not whole-file semantics.
    #[must_use]
    pub const fn range_complete(&self) -> bool {
        self.range_complete
    }
    /// Returns a selected total only when this first page covered the entire range.
    #[must_use]
    pub const fn total_matching_statements(&self) -> Option<u64> {
        self.total_matching_statements
    }
    /// Returns the unfiltered extension count within the page's declared scope.
    #[must_use]
    pub const fn total_extensions(&self) -> u64 {
        self.total_extensions
    }
    /// Returns inspected extension rows, excluding the backend continuation probe.
    #[must_use]
    pub const fn scanned_extensions(&self) -> u64 {
        self.scanned_extensions
    }
    /// Returns whether the page used a file index or an explicit global diagnostic.
    #[must_use]
    pub const fn scope(&self) -> &'static str {
        self.scope
    }
    /// Borrows observed work/output accounting.
    #[must_use]
    pub const fn usage(&self) -> &RdfStatementReadUsage {
        &self.usage
    }
}

/// Redacted failures from typed RDF reads and their existing backend/query controls.
#[derive(Debug, thiserror::Error)]
pub enum RdfStatementReadError<E: Error + Send + Sync + 'static> {
    /// A backend refused the bounded read.
    #[error("rdf generation read failed")]
    Backend(#[source] E),
    /// Existing query controls refused work or output.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Existing generation controls refused work.
    #[error(transparent)]
    Control(#[from] GenerationControlError),
    /// The requested or source-witness file is absent or inconsistent.
    #[error("rdf source file identity is inconsistent")]
    InvalidFile,
    /// An RDF envelope, provenance or term source is malformed or foreign.
    #[error("rdf statement identity or evidence is inconsistent")]
    InvalidRecord,
    /// Stable IDs, cardinalities or continuation semantics drifted.
    #[error("rdf extension page is inconsistent")]
    InvalidPage,
    /// The backend cannot provide indexed extension pages for this file.
    #[error("indexed rdf file reads are unsupported by this generation reader")]
    IndexedFileReadUnsupported,
}

/// Reads exactly one indexed file-extension page and returns validated RDF roles.
///
/// Other namespaces are counted, not modified. A continuation can accompany zero
/// RDF matches. The file-extension total is not a selected RDF total. Backends
/// without this index fail explicitly; no global scan fallback is attempted.
///
/// # Errors
/// Rejects stale selectors, malformed/spoofed RDF evidence, missing provenance,
/// invalid paging, resource refusal and cancellation. Source bytes are not read;
/// immutable source-text verification is a separate capability.
pub fn read_rdf_statement_page<R: GenerationReader>(
    reader: &R,
    request: RdfStatementReadRequest,
    context: &GenerationContext<'_>,
    budget: QueryBudget,
) -> Result<RdfStatementPage, RdfStatementReadError<R::Error>> {
    read_page(reader, request, context, budget, true)
}

/// Reads one global extension window for explicit bounded diagnostics only.
///
/// Empty filtered pages preserve their global continuation. This operation cannot
/// establish whole-file completeness unless its first page covers all extensions.
///
/// # Errors
/// Returns the same ownership, codec, page, control and budget failures as
/// [`read_rdf_statement_page`]. It does not require the indexed-file capability.
pub fn read_rdf_statement_global_page<R: GenerationReader>(
    reader: &R,
    request: RdfStatementReadRequest,
    context: &GenerationContext<'_>,
    budget: QueryBudget,
) -> Result<RdfStatementPage, RdfStatementReadError<R::Error>> {
    read_page(reader, request, context, budget, false)
}

fn read_page<R: GenerationReader>(
    reader: &R,
    request: RdfStatementReadRequest,
    context: &GenerationContext<'_>,
    budget: QueryBudget,
    file_index: bool,
) -> Result<RdfStatementPage, RdfStatementReadError<R::Error>> {
    budget.validate()?;
    let started = Instant::now();
    let control = QueryControl::new(context.cancellation(), budget.max_duration());
    control.check()?;
    context.check()?;
    let metadata = reader.metadata();
    let stats = reader.stats();
    if metadata.repository() != request.repository || metadata.generation() != request.generation {
        return Err(QueryError::GenerationMismatch.into());
    }
    // The codec reconstructs bounded text and canonical identities. Restrict the
    // backend's decoded text/row workspace before it materializes the page.
    let text_limit = (budget.max_memory_bytes() / 16).max(1);
    let backend_budget = GenerationBudget::new(
        context
            .budget()
            .limit(GenerationResource::Rows)
            .min(budget.max_rows())
            .min((text_limit / 64).max(1)),
        context
            .budget()
            .limit(GenerationResource::SourceReferences)
            .min((text_limit / 128).max(1)),
        context
            .budget()
            .limit(GenerationResource::TextBytes)
            .min(text_limit),
    )
    .map_err(|_| QueryError::PlanRejected {
        resource: QueryResource::MemoryBytes,
    })?;
    let backend_context = GenerationContext::new(context.cancellation(), backend_budget);
    let mut usage = RdfStatementReadUsage::default();
    let mut files = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    checked_file(
        reader,
        request.file,
        metadata,
        &backend_context,
        &control,
        budget,
        &mut usage,
        &mut files,
    )?;
    let raw = if file_index {
        reader.extensions_for_file(request.file, request.after, request.limit, &backend_context)
    } else {
        reader
            .extensions(request.after, request.limit, &backend_context)
            .map(Some)
    };
    control.check()?;
    context.check()?;
    let raw = raw
        .map_err(RdfStatementReadError::Backend)?
        .ok_or(RdfStatementReadError::IndexedFileReadUnsupported)?;
    if (file_index && raw.total_available() > stats.extensions())
        || (!file_index && raw.total_available() != stats.extensions())
        || raw.items().len() > usize::from(request.limit.get())
        || raw.items().windows(2).any(|p| p[0].id >= p[1].id)
        || request
            .after
            .is_some_and(|after| raw.items().iter().any(|e| e.id <= after))
        || (raw.completeness() == ReadPageCompleteness::Complete
            && (raw.next_cursor().is_some()
                || (request.after.is_none()
                    && u64::try_from(raw.items().len()).ok() != Some(raw.total_available()))))
        || (raw.completeness() == ReadPageCompleteness::Truncated
            && (raw.items().is_empty() || raw.next_cursor() != raw.items().last().map(|e| e.id)))
    {
        return Err(RdfStatementReadError::InvalidPage);
    }
    let encoded = serialized_size(&raw.items(), budget.max_memory_bytes() / 16, &control)?;
    usage.memory_upper_bytes = checked_add(
        usage.memory_upper_bytes,
        encoded
            .checked_mul(16)
            .ok_or(QueryError::MemoryUnavailable)?,
        QueryResource::MemoryBytes,
        budget.max_memory_bytes(),
    )?;
    let scanned_extensions = checked_usize_to_u64(raw.items().len())?;
    usage.rows = checked_add(
        usage.rows,
        scanned_extensions + u64::from(raw.next_cursor().is_some()),
        QueryResource::Rows,
        budget.max_rows(),
    )?;
    let mut statements = Vec::new();
    let mut skipped_non_rdf = 0;
    let mut skipped_other_files = 0;
    for envelope in raw.items() {
        control.check()?;
        context.check()?;
        if envelope.repository != metadata.repository()
            || envelope.generation != metadata.generation()
        {
            return Err(RdfStatementReadError::InvalidRecord);
        }
        if file_index {
            let source = envelope
                .evidence
                .source
                .as_ref()
                .ok_or(RdfStatementReadError::InvalidRecord)?;
            let file = files
                .get(&request.file)
                .ok_or(RdfStatementReadError::InvalidFile)?;
            if source.repository() != metadata.repository()
                || source.generation() != metadata.generation()
                || source.span().file() != request.file
                || source.content_hash() != file.content_hash
                || source.span().end_byte() > file.byte_length
            {
                return Err(RdfStatementReadError::InvalidRecord);
            }
        }
        if envelope.namespace != RDF_STATEMENT_NAMESPACE {
            skipped_non_rdf += 1;
            continue;
        }
        let statement = decode_rdf_statement_envelope(envelope)
            .map_err(|_| RdfStatementReadError::InvalidRecord)?;
        control.check()?;
        let source = statement.source();
        checked_file(
            reader,
            source.span().file(),
            metadata,
            &backend_context,
            &control,
            budget,
            &mut usage,
            &mut files,
        )?;
        let file = files
            .get(&source.span().file())
            .ok_or(RdfStatementReadError::InvalidFile)?;
        if source.content_hash() != file.content_hash || source.span().end_byte() > file.byte_length
        {
            return Err(RdfStatementReadError::InvalidRecord);
        }
        if let std::collections::btree_map::Entry::Vacant(entry) =
            provenance.entry(envelope.provenance)
        {
            usage.rows = checked_add(usage.rows, 1, QueryResource::Rows, budget.max_rows())?;
            let record = reader.provenance(envelope.provenance, &backend_context);
            control.check()?;
            let record = record
                .map_err(RdfStatementReadError::Backend)?
                .ok_or(RdfStatementReadError::InvalidRecord)?;
            if record.id != envelope.provenance
                || record.repository != metadata.repository()
                || record.generation != metadata.generation()
            {
                return Err(RdfStatementReadError::InvalidRecord);
            }
            entry.insert(());
        }
        if source.span().file() != request.file {
            skipped_other_files += 1;
            continue;
        }
        usage.results = checked_add(
            usage.results,
            1,
            QueryResource::Results,
            budget.max_results(),
        )?;
        usage.source_bytes = checked_add(
            usage.source_bytes,
            checked_usize_to_u64(statement.source_text().len())?,
            QueryResource::SourceBytes,
            budget.max_source_bytes(),
        )?;
        statements
            .try_reserve(1)
            .map_err(|_| QueryError::MemoryUnavailable)?;
        statements.push(RdfStatementRecord {
            id: envelope.id,
            provenance: envelope.provenance,
            statement,
        });
    }
    if reader.metadata() != metadata || reader.stats() != stats {
        return Err(RdfStatementReadError::InvalidPage);
    }
    let range_complete = raw.completeness() == ReadPageCompleteness::Complete;
    let mut result = RdfStatementPage {
        scope: if file_index {
            "file_extensions"
        } else {
            "global_extensions"
        },
        trust: RepositoryDataTrust::UntrustedRepositoryData,
        repository: request.repository,
        generation: request.generation,
        file: request.file,
        requested_after: request.after,
        statements,
        scanned_extensions,
        total_extensions: raw.total_available(),
        skipped_non_rdf,
        skipped_other_files,
        range_complete,
        next_cursor: raw.next_cursor(),
        total_matching_statements: if request.after.is_none() && range_complete {
            Some(usage.results)
        } else {
            None
        },
        usage,
    };
    result.usage.elapsed_micros =
        checked_u128_to_u64(started.elapsed().as_nanos().saturating_add(999) / 1000);
    for _ in 0..8 {
        let bytes =
            serialized_size(&result, budget.max_json_bytes(), &control).map_err(|e| match e {
                QueryError::BudgetExceeded {
                    resource: QueryResource::MemoryBytes,
                    ..
                } => QueryError::BudgetExceeded {
                    resource: QueryResource::JsonBytes,
                    limit: budget.max_json_bytes(),
                },
                e => e,
            })?;
        if bytes > budget.max_tokens() {
            return Err(QueryError::BudgetExceeded {
                resource: QueryResource::Tokens,
                limit: budget.max_tokens(),
            }
            .into());
        }
        if result.usage.json_bytes == bytes && result.usage.estimated_tokens == bytes {
            context.check()?;
            return Ok(result);
        }
        result.usage.json_bytes = bytes;
        result.usage.estimated_tokens = bytes;
    }
    Err(QueryError::ResultEncoding.into())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one bounded page shares its controls and checked file cache"
)]
fn checked_file<R: GenerationReader>(
    reader: &R,
    id: FileId,
    metadata: GenerationMetadata,
    context: &GenerationContext<'_>,
    control: &QueryControl<'_>,
    budget: QueryBudget,
    usage: &mut RdfStatementReadUsage,
    files: &mut BTreeMap<FileId, FileRecord>,
) -> Result<(), RdfStatementReadError<R::Error>> {
    if files.contains_key(&id) {
        return Ok(());
    }
    usage.rows = checked_add(usage.rows, 1, QueryResource::Rows, budget.max_rows())?;
    let file = reader.file(id, context);
    control.check()?;
    let file = file
        .map_err(RdfStatementReadError::Backend)?
        .ok_or(RdfStatementReadError::InvalidFile)?;
    let source = file
        .evidence
        .source
        .as_ref()
        .ok_or(RdfStatementReadError::InvalidFile)?;
    if file.id != id
        || file.repository != metadata.repository()
        || file.generation != metadata.generation()
        || source.repository() != file.repository
        || source.generation() != file.generation
        || source.span().file() != id
        || source.span().start_byte() != 0
        || source.span().end_byte() != file.byte_length
        || source.content_hash() != file.content_hash
    {
        return Err(RdfStatementReadError::InvalidFile);
    }
    let bytes = serialized_size(&file, budget.max_memory_bytes(), control)?;
    usage.memory_upper_bytes = checked_add(
        usage.memory_upper_bytes,
        bytes.checked_mul(2).ok_or(QueryError::MemoryUnavailable)?,
        QueryResource::MemoryBytes,
        budget.max_memory_bytes(),
    )?;
    files.insert(id, file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DqlQueryReadError, DqlQueryReadRequest, read_dql_query_page};
    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::{GenerationIdentity, content_hash, derive_generation};
    use rootlight_ir::{
        CoverageRecord, EntityRecord, ExtensionEnvelope, ExtensionSupport, IrDocument, IrLimits,
        OccurrenceRecord, ProvenanceRecord, RelationRecord, SourceRef, SourceSpan,
        decode_ir_document, new_rdf_statement_envelope,
    };
    use rootlight_storage::{
        CoverageReadRequest, GenerationStats, IdentityVerifiedGeneration, OccurrenceReadRequest,
        ReadPage, RelationReadRequest,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct Reader {
        metadata: GenerationMetadata,
        file: FileRecord,
        provenance: ProvenanceRecord,
        extensions: Vec<ExtensionEnvelope>,
        pages: AtomicUsize,
        file_pages: AtomicUsize,
        supports_file_index: bool,
        wrong_file_page: bool,
        other_files: Vec<FileRecord>,
        ignore_cursor: bool,
        reported_total: Option<u64>,
        cancel_on_page: Option<Cancellation>,
        fail_page: bool,
        page_budget: Mutex<Option<GenerationBudget>>,
    }

    impl GenerationReader for Reader {
        type Error = std::io::Error;
        fn metadata(&self) -> GenerationMetadata {
            self.metadata
        }
        fn stats(&self) -> GenerationStats {
            GenerationStats::new(
                1 + u64::try_from(self.other_files.len()).unwrap(),
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                0,
                u64::try_from(self.extensions.len()).unwrap(),
                1,
                20 + u64::try_from(self.extensions.len()).unwrap(),
                0,
            )
            .unwrap()
        }
        fn file(
            &self,
            id: FileId,
            context: &GenerationContext<'_>,
        ) -> Result<Option<FileRecord>, Self::Error> {
            context.check().map_err(std::io::Error::other)?;
            Ok(std::iter::once(&self.file)
                .chain(&self.other_files)
                .find(|file| file.id == id)
                .cloned())
        }
        fn entity(
            &self,
            _: rootlight_ids::SymbolId,
            _: &GenerationContext<'_>,
        ) -> Result<Option<EntityRecord>, Self::Error> {
            panic!("RDF pages must not read entities")
        }
        fn relations(
            &self,
            _: &RelationReadRequest,
            _: &GenerationContext<'_>,
        ) -> Result<ReadPage<RelationRecord>, Self::Error> {
            panic!("RDF pages must not read relations")
        }
        fn occurrences(
            &self,
            _: &OccurrenceReadRequest,
            _: &GenerationContext<'_>,
        ) -> Result<ReadPage<OccurrenceRecord>, Self::Error> {
            panic!("RDF pages must not read occurrences")
        }
        fn provenance(
            &self,
            id: FactId,
            context: &GenerationContext<'_>,
        ) -> Result<Option<ProvenanceRecord>, Self::Error> {
            context.check().map_err(std::io::Error::other)?;
            Ok((id == self.provenance.id).then(|| self.provenance.clone()))
        }
        fn coverage(
            &self,
            _: &CoverageReadRequest,
            _: &GenerationContext<'_>,
        ) -> Result<ReadPage<CoverageRecord>, Self::Error> {
            panic!("RDF pages must not read coverage")
        }
        fn extensions(
            &self,
            after: Option<FactId>,
            limit: GenerationReadLimit,
            context: &GenerationContext<'_>,
        ) -> Result<ReadPage<ExtensionEnvelope>, Self::Error> {
            context.check().map_err(std::io::Error::other)?;
            self.pages.fetch_add(1, Ordering::Relaxed);
            *self.page_budget.lock().unwrap() = Some(context.budget());
            if let Some(cancellation) = &self.cancel_on_page {
                cancellation.cancel(CancellationReason::ClientRequest);
            }
            if self.fail_page {
                return Err(std::io::Error::other("private backend detail"));
            }
            let items = self
                .extensions
                .iter()
                .filter(|e| self.ignore_cursor || after.is_none_or(|after| e.id > after))
                .take(usize::from(limit.get()) + 1)
                .cloned()
                .collect();
            ReadPage::from_limit_plus_one(
                items,
                self.reported_total
                    .unwrap_or_else(|| u64::try_from(self.extensions.len()).unwrap()),
                limit,
                |e| e.id,
            )
            .map_err(std::io::Error::other)
        }
        fn read_generation(
            &self,
            _: &GenerationContext<'_>,
        ) -> Result<IdentityVerifiedGeneration, Self::Error> {
            panic!("RDF pages must not materialize the generation")
        }
        fn extensions_for_file(
            &self,
            file: FileId,
            after: Option<FactId>,
            limit: GenerationReadLimit,
            context: &GenerationContext<'_>,
        ) -> Result<Option<ReadPage<ExtensionEnvelope>>, Self::Error> {
            context.check().map_err(std::io::Error::other)?;
            self.file_pages.fetch_add(1, Ordering::Relaxed);
            if !self.supports_file_index {
                return Ok(None);
            }
            *self.page_budget.lock().unwrap() = Some(context.budget());
            if let Some(cancellation) = &self.cancel_on_page {
                cancellation.cancel(CancellationReason::ClientRequest);
            }
            if self.fail_page {
                return Err(std::io::Error::other("private backend detail"));
            }
            let selected: Vec<_> = self
                .extensions
                .iter()
                .filter(|e| {
                    self.wrong_file_page
                        || e.evidence
                            .source
                            .as_ref()
                            .is_some_and(|s| s.span().file() == file)
                })
                .collect();
            let items = selected
                .iter()
                .filter(|e| self.ignore_cursor || after.is_none_or(|after| e.id > after))
                .take(usize::from(limit.get()) + 1)
                .map(|e| (*e).clone())
                .collect();
            ReadPage::from_limit_plus_one(
                items,
                self.reported_total
                    .unwrap_or_else(|| u64::try_from(selected.len()).unwrap()),
                limit,
                |e| e.id,
            )
            .map(Some)
            .map_err(std::io::Error::other)
        }
    }

    fn fixture() -> Reader {
        let encoded = include_str!("../../../tests/fixtures/compatibility/ir/1.1/document.json");
        let IrDocument::NormalizedV1_1(template) = decode_ir_document(
            encoded.as_bytes(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap() else {
            panic!("normalized fixture")
        };
        let hash = content_hash(b"rdf-paged-reader-fixture");
        let generation = derive_generation(GenerationIdentity {
            repository: template.repository,
            parent: None,
            manifest_hash: hash,
            config_hash: hash,
            provider_set_hash: hash,
            format_version: (u32::from(rootlight_storage::GENERATION_CONTRACT_VERSION.major())
                << 16)
                | u32::from(rootlight_storage::GENERATION_CONTRACT_VERSION.minor()),
        })
        .id();
        let encoded = encoded.replace(&template.generation.to_string(), &generation.to_string());
        let IrDocument::NormalizedV1_1(document) = decode_ir_document(
            encoded.as_bytes(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap() else {
            panic!("normalized fixture")
        };
        let file = document.files[0].clone();
        let provenance = document
            .provenance
            .iter()
            .find(|p| p.id == file.provenance)
            .unwrap()
            .clone();
        let source = SourceRef::new(
            document.repository,
            generation,
            SourceSpan::new(file.id, 0, 12).unwrap(),
            file.content_hash,
            None,
        );
        let spans = [(0, 4), (4, 8), (8, 11)].map(|(a, b)| SourceSpan::new(file.id, a, b).unwrap());
        let statement = RdfStatement::from_ntriples(source, "<s:><p:>_:a.", spans).unwrap();
        let extension = new_rdf_statement_envelope(provenance.id, &statement).unwrap();
        Reader {
            metadata: GenerationMetadata::new(
                document.repository,
                generation,
                None,
                hash,
                hash,
                hash,
            )
            .unwrap(),
            file,
            provenance,
            extensions: vec![extension],
            pages: AtomicUsize::new(0),
            file_pages: AtomicUsize::new(0),
            supports_file_index: true,
            wrong_file_page: false,
            other_files: Vec::new(),
            ignore_cursor: false,
            reported_total: None,
            cancel_on_page: None,
            fail_page: false,
            page_budget: Mutex::new(None),
        }
    }

    #[test]
    fn typed_rdf_page_returns_roles_and_exact_file_completion_without_materializing() {
        let reader = fixture();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let request = RdfStatementReadRequest::new(
            reader.metadata.repository(),
            reader.metadata.generation(),
            reader.file.id,
            GenerationReadLimit::default(),
        );
        let page =
            read_rdf_statement_page(&reader, request, &context, QueryBudget::default()).unwrap();
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
        assert_eq!(reader.file_pages.load(Ordering::Relaxed), 1);
        assert_eq!(page.scope(), "file_extensions");
        assert_eq!(page.statements().len(), 1);
        let returned = &page.statements()[0];
        assert_eq!(returned.id(), reader.extensions[0].id);
        assert_eq!(returned.provenance(), reader.provenance.id);
        assert_eq!(returned.statement().predicate().iri(), Some("p:"));
        assert_eq!(returned.statement().object().blank_node_label(), Some("a"));
        assert!(page.range_complete());
        assert_eq!(page.total_matching_statements(), Some(1));
        assert_eq!(page.next_cursor(), None);
        assert_eq!(
            u64::try_from(serde_json::to_vec(&page).unwrap().len()).unwrap(),
            page.usage().json_bytes
        );
    }

    fn request(reader: &Reader, limit: u32) -> RdfStatementReadRequest {
        RdfStatementReadRequest::new(
            reader.metadata.repository(),
            reader.metadata.generation(),
            reader.file.id,
            GenerationReadLimit::new(limit).unwrap(),
        )
    }

    #[test]
    fn filtered_empty_page_keeps_file_cursor_and_never_invents_selected_total() {
        let mut reader = fixture();
        let mut unrelated = reader.extensions[0].clone();
        unrelated.id = FactId::from_bytes([0; 20]);
        unrelated.namespace = "rootlight.unrelated".into();
        unrelated.payload = "opaque unrelated payload".into();
        reader.extensions.push(unrelated.clone());
        reader.extensions.sort_by_key(|e| e.id);
        let original = reader.extensions.clone();
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        let first = read_rdf_statement_page(
            &reader,
            request(&reader, 1),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert!(first.statements().is_empty());
        assert!(!first.range_complete());
        assert_eq!(first.total_matching_statements(), None);
        assert_eq!(first.next_cursor(), Some(unrelated.id));
        assert_eq!(first.total_extensions(), 2);
        assert_eq!(first.skipped_non_rdf, 1);
        let second = read_rdf_statement_page(
            &reader,
            request(&reader, 1).with_after(first.next_cursor().unwrap()),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert_eq!(second.statements().len(), 1);
        assert!(second.range_complete());
        assert_eq!(second.total_matching_statements(), None);
        assert_eq!(second.next_cursor(), None);
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
        assert_eq!(reader.file_pages.load(Ordering::Relaxed), 2);
        assert_eq!(reader.extensions, original);
        let end = read_rdf_statement_page(
            &reader,
            request(&reader, 1).with_after(second.statements()[0].id()),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert!(end.statements().is_empty());
        assert!(end.range_complete());
        assert_eq!(end.total_matching_statements(), None);
    }

    #[test]
    fn selected_file_filter_validates_other_rdf_sources_before_omission() {
        let mut reader = fixture();
        let mut other = reader.file.clone();
        other.id = FileId::from_bytes([8; 20]);
        other.path = "other.nt".into();
        other.evidence.source = Some(SourceRef::new(
            other.repository,
            other.generation,
            SourceSpan::new(other.id, 0, other.byte_length).unwrap(),
            other.content_hash,
            None,
        ));
        let source = SourceRef::new(
            other.repository,
            other.generation,
            SourceSpan::new(other.id, 0, 12).unwrap(),
            other.content_hash,
            None,
        );
        let spans =
            [(0, 4), (4, 8), (8, 11)].map(|(a, b)| SourceSpan::new(other.id, a, b).unwrap());
        let statement = RdfStatement::from_ntriples(source, "<s:><p:>_:b.", spans).unwrap();
        let envelope = new_rdf_statement_envelope(reader.provenance.id, &statement).unwrap();
        reader.extensions = vec![envelope];
        reader.other_files.push(other);
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        let page = read_rdf_statement_global_page(
            &reader,
            request(&reader, 10),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert!(page.statements().is_empty());
        assert_eq!(page.skipped_other_files, 1);
        assert_eq!(page.total_matching_statements(), Some(0));
        reader.other_files[0].content_hash = content_hash(b"spoofed source");
        assert!(matches!(
            read_rdf_statement_global_page(
                &reader,
                request(&reader, 10),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::InvalidFile)
        ));
    }

    #[test]
    fn query_refuses_stale_missing_and_foreign_file_metadata_before_returning_data() {
        let mut reader = fixture();
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        let wrong = RdfStatementReadRequest::new(
            RepositoryId::from_bytes([9; 16]),
            reader.metadata.generation(),
            reader.file.id,
            GenerationReadLimit::default(),
        );
        assert!(matches!(
            read_rdf_statement_page(&reader, wrong, &context, QueryBudget::default()),
            Err(RdfStatementReadError::Query(QueryError::GenerationMismatch))
        ));
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
        let wrong = RdfStatementReadRequest::new(
            reader.metadata.repository(),
            reader.metadata.generation(),
            FileId::from_bytes([9; 20]),
            GenerationReadLimit::default(),
        );
        assert!(matches!(
            read_rdf_statement_page(&reader, wrong, &context, QueryBudget::default()),
            Err(RdfStatementReadError::InvalidFile)
        ));
        reader
            .file
            .evidence
            .source
            .as_mut()
            .unwrap()
            .clone_from(&SourceRef::new(
                reader.file.repository,
                reader.file.generation,
                SourceSpan::new(reader.file.id, 0, reader.file.byte_length).unwrap(),
                content_hash(b"wrong"),
                None,
            ));
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 10),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::InvalidFile)
        ));
    }

    #[test]
    fn malformed_unknown_version_and_foreign_rdf_envelopes_are_not_silently_skipped() {
        for mutate in [
            |e: &mut ExtensionEnvelope| e.version = "99".into(),
            |e: &mut ExtensionEnvelope| e.payload = "{}".into(),
            |e: &mut ExtensionEnvelope| e.payload.push(' '),
            |e: &mut ExtensionEnvelope| e.repository = RepositoryId::from_bytes([9; 16]),
            |e: &mut ExtensionEnvelope| e.generation = GenerationId::from_bytes([9; 20]),
            |e: &mut ExtensionEnvelope| e.id = FactId::from_bytes([9; 20]),
        ] {
            let mut reader = fixture();
            mutate(&mut reader.extensions[0]);
            let token = Cancellation::new();
            let context = GenerationContext::new(&token, GenerationBudget::default());
            assert!(matches!(
                read_rdf_statement_page(
                    &reader,
                    request(&reader, 10),
                    &context,
                    QueryBudget::default()
                ),
                Err(RdfStatementReadError::InvalidRecord)
            ));
        }
    }

    #[test]
    fn canonically_encoded_rdf_still_requires_matching_file_hash_bounds_and_provenance() {
        for fault in ["hash", "bounds", "provenance"] {
            let mut reader = fixture();
            let offset = if fault == "bounds" { 17 } else { 0 };
            let source = SourceRef::new(
                reader.metadata.repository(),
                reader.metadata.generation(),
                SourceSpan::new(reader.file.id, offset, offset + 12).unwrap(),
                if fault == "hash" {
                    content_hash(b"wrong")
                } else {
                    reader.file.content_hash
                },
                None,
            );
            let spans = [(0, 4), (4, 8), (8, 11)]
                .map(|(a, b)| SourceSpan::new(reader.file.id, offset + a, offset + b).unwrap());
            let statement = RdfStatement::from_ntriples(source, "<s:><p:>_:a.", spans).unwrap();
            reader.extensions[0] = new_rdf_statement_envelope(
                if fault == "provenance" {
                    FactId::from_bytes([9; 20])
                } else {
                    reader.provenance.id
                },
                &statement,
            )
            .unwrap();
            let token = Cancellation::new();
            let context = GenerationContext::new(&token, GenerationBudget::default());
            assert!(
                matches!(
                    read_rdf_statement_page(
                        &reader,
                        request(&reader, 10),
                        &context,
                        QueryBudget::default()
                    ),
                    Err(RdfStatementReadError::InvalidRecord)
                ),
                "{fault}"
            );
        }
    }

    #[test]
    fn page_boundaries_and_scoped_total_must_match_the_pinned_reader() {
        let mut reader = fixture();
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        reader.reported_total = Some(2);
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 1),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::InvalidPage)
        ));
        reader.reported_total = None;
        reader.ignore_cursor = true;
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 1).with_after(reader.extensions[0].id),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::InvalidPage)
        ));
    }

    #[test]
    fn existing_query_budgets_refuse_work_and_output_without_partial_success() {
        for (budget, resource) in [
            (QueryBudget::default().with_max_rows(1), QueryResource::Rows),
            (
                QueryBudget::default().with_max_source_bytes(1),
                QueryResource::SourceBytes,
            ),
            (
                QueryBudget::default().with_max_json_bytes(1),
                QueryResource::JsonBytes,
            ),
            (
                QueryBudget::default().with_max_tokens(1),
                QueryResource::Tokens,
            ),
            (
                QueryBudget::default().with_max_memory_bytes(1),
                QueryResource::MemoryBytes,
            ),
        ] {
            let reader = fixture();
            let token = Cancellation::new();
            let context = GenerationContext::new(&token, GenerationBudget::default());
            match read_rdf_statement_page(&reader, request(&reader, 10), &context, budget) {
                Err(RdfStatementReadError::Query(QueryError::BudgetExceeded {
                    resource: actual,
                    ..
                })) => assert_eq!(actual, resource),
                result => panic!("unexpected resource result for {resource:?}: {result:?}"),
            }
        }
        let reader = fixture();
        let token = Cancellation::new();
        let original = GenerationBudget::new(100, 10, 1000).unwrap();
        let context = GenerationContext::new(&token, original);
        read_rdf_statement_page(
            &reader,
            request(&reader, 10),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        let observed = reader.page_budget.lock().unwrap().unwrap();
        for resource in [
            GenerationResource::Rows,
            GenerationResource::TextBytes,
            GenerationResource::SourceReferences,
        ] {
            assert!(observed.limit(resource) <= original.limit(resource));
        }
    }

    #[test]
    fn cancellation_before_and_during_backend_read_preserves_terminal_reason() {
        let mut reader = fixture();
        let token = Cancellation::new();
        token.cancel(CancellationReason::ClientRequest);
        let context = GenerationContext::new(&token, GenerationBudget::default());
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 10),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::Query(QueryError::Cancelled(
                CancellationReason::ClientRequest
            )))
        ));
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
        let token = Cancellation::new();
        reader.cancel_on_page = Some(token.clone());
        reader.fail_page = true;
        let context = GenerationContext::new(&token, GenerationBudget::default());
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 10),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::Query(QueryError::Cancelled(
                CancellationReason::ClientRequest
            )))
        ));
        reader.cancel_on_page = None;
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        let error = read_rdf_statement_page(
            &reader,
            request(&reader, 10),
            &context,
            QueryBudget::default(),
        )
        .unwrap_err();
        assert!(matches!(error, RdfStatementReadError::Backend(_)));
        assert_eq!(error.to_string(), "rdf generation read failed");
    }

    #[test]
    fn unsupported_file_index_is_explicit_and_never_attempts_global_fallback() {
        let mut reader = fixture();
        reader.supports_file_index = false;
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 10),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::IndexedFileReadUnsupported)
        ));
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
        assert_eq!(reader.file_pages.load(Ordering::Relaxed), 1);
        let diagnostic = read_rdf_statement_global_page(
            &reader,
            request(&reader, 10),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert_eq!(diagnostic.scope(), "global_extensions");
        assert_eq!(diagnostic.statements().len(), 1);
    }

    #[test]
    fn file_index_must_not_return_a_different_primary_source_file() {
        let mut reader = fixture();
        reader.wrong_file_page = true;
        let file = FileId::from_bytes([8; 20]);
        let source = SourceRef::new(
            reader.metadata.repository(),
            reader.metadata.generation(),
            SourceSpan::new(file, 0, 12).unwrap(),
            reader.file.content_hash,
            None,
        );
        let spans = [(0, 4), (4, 8), (8, 11)].map(|(a, b)| SourceSpan::new(file, a, b).unwrap());
        let statement = RdfStatement::from_ntriples(source, "<s:><p:>_:a.", spans).unwrap();
        reader.extensions =
            vec![new_rdf_statement_envelope(reader.provenance.id, &statement).unwrap()];
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        assert!(matches!(
            read_rdf_statement_page(
                &reader,
                request(&reader, 10),
                &context,
                QueryBudget::default()
            ),
            Err(RdfStatementReadError::InvalidRecord)
        ));
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
    }

    fn dql_fixture() -> Reader {
        use rootlight_ir::{DqlCallKind, DqlLiteralKind, DqlNode, DqlNodeKind, DqlQuery};
        let mut reader = fixture();
        let text = "{q(func:uid(1)){name}}";
        reader.file.language = "dql".to_owned();
        reader.file.content_hash = content_hash(text.as_bytes());
        reader.file.byte_length = u64::try_from(text.len()).unwrap();
        let span = |start, end| SourceSpan::new(reader.file.id, start, end).unwrap();
        let source = SourceRef::new(
            reader.metadata.repository(),
            reader.metadata.generation(),
            span(0, reader.file.byte_length),
            reader.file.content_hash,
            None,
        );
        reader.file.evidence.source = Some(source.clone());
        let nodes = [
            (0, 22, None, DqlNodeKind::Operation, None),
            (
                1,
                21,
                Some(0),
                DqlNodeKind::Root { aggregation: false },
                Some(span(1, 2)),
            ),
            (
                8,
                14,
                Some(1),
                DqlNodeKind::Call {
                    class: DqlCallKind::Generator,
                },
                Some(span(8, 11)),
            ),
            (
                12,
                13,
                Some(2),
                DqlNodeKind::Literal {
                    class: DqlLiteralKind::Number,
                },
                None,
            ),
            (16, 20, Some(1), DqlNodeKind::Selection, None),
            (
                16,
                20,
                Some(4),
                DqlNodeKind::Predicate { reverse: false },
                Some(span(16, 20)),
            ),
        ]
        .into_iter()
        .map(|(start, end, parent, role, name)| DqlNode {
            span: span(start, end),
            parent,
            role,
            name,
        })
        .collect();
        let query = DqlQuery::from_evidence(source, text, nodes).unwrap();
        reader.extensions =
            vec![rootlight_ir::new_dql_query_envelope(reader.provenance.id, &query).unwrap()];
        reader
    }

    fn dql_request(reader: &Reader) -> DqlQueryReadRequest {
        DqlQueryReadRequest::new(
            reader.metadata.repository(),
            reader.metadata.generation(),
            reader.file.id,
            GenerationReadLimit::default(),
        )
    }

    #[test]
    fn dql_wrong_language_and_unsupported_index_never_fall_back_to_global_reads() {
        let mut reader = dql_fixture();
        reader.file.language = "json".to_owned();
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        assert!(matches!(
            read_dql_query_page(
                &reader,
                dql_request(&reader),
                &context,
                QueryBudget::default()
            ),
            Err(DqlQueryReadError::InvalidFile)
        ));
        assert_eq!(reader.file_pages.load(Ordering::Relaxed), 0);
        reader.file.language = "dql".to_owned();
        reader.supports_file_index = false;
        assert!(matches!(
            read_dql_query_page(
                &reader,
                dql_request(&reader),
                &context,
                QueryBudget::default()
            ),
            Err(DqlQueryReadError::IndexedFileReadUnsupported)
        ));
        assert_eq!(reader.file_pages.load(Ordering::Relaxed), 1);
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dql_canonical_envelopes_still_require_retained_provenance_identity() {
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        let reader = dql_fixture();
        let page = read_dql_query_page(
            &reader,
            dql_request(&reader),
            &context,
            QueryBudget::default(),
        )
        .unwrap();
        assert_eq!(page.queries().len(), 1);
        assert_eq!(page.queries()[0].query().text(), "{q(func:uid(1)){name}}");
        for fault in [
            "payload",
            "version",
            "id",
            "missing_provenance",
            "foreign_provenance",
        ] {
            let mut reader = dql_fixture();
            match fault {
                "payload" => reader.extensions[0].payload.push(' '),
                "version" => reader.extensions[0].version = "2".to_owned(),
                "id" => reader.extensions[0].id = FactId::from_bytes([99; 20]),
                "missing_provenance" => reader.provenance.id = FactId::from_bytes([98; 20]),
                _ => reader.provenance.generation = GenerationId::from_bytes([97; 20]),
            }
            assert!(
                matches!(
                    read_dql_query_page(
                        &reader,
                        dql_request(&reader),
                        &context,
                        QueryBudget::default()
                    ),
                    Err(DqlQueryReadError::InvalidRecord)
                ),
                "{fault}"
            );
            assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn dql_query_budgets_refuse_actual_work_and_output_without_partial_success() {
        let reader = dql_fixture();
        let token = Cancellation::new();
        let context = GenerationContext::new(&token, GenerationBudget::default());
        for (budget, resource) in [
            (QueryBudget::default().with_max_rows(1), QueryResource::Rows),
            (
                QueryBudget::default().with_max_source_bytes(1),
                QueryResource::SourceBytes,
            ),
            (
                QueryBudget::default().with_max_json_bytes(1),
                QueryResource::JsonBytes,
            ),
        ] {
            match read_dql_query_page(&reader, dql_request(&reader), &context, budget) {
                Err(DqlQueryReadError::Query(QueryError::BudgetExceeded {
                    resource: actual,
                    ..
                })) => assert_eq!(actual, resource),
                result => panic!("unexpected DQL resource result for {resource:?}: {result:?}"),
            }
        }
        assert_eq!(reader.pages.load(Ordering::Relaxed), 0);
    }
}
