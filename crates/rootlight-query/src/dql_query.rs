//! Read source-owned native DQL queries from immutable file-extension pages.
//! Unrelated namespaces retain their cursor positions; missing indexed reads
//! never become global scans or invented empty complete results.

use std::{collections::BTreeSet, error::Error, time::Instant};

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId};
use rootlight_ir::{DQL_QUERY_NAMESPACE, DqlQuery, decode_dql_query_envelope};
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationControlError, GenerationReadLimit,
    GenerationReader, GenerationResource, ReadPageCompleteness,
};
use serde::Serialize;

use crate::model::{checked_add, checked_u128_to_u64, checked_usize_to_u64};
use crate::service::{QueryControl, serialized_size};
use crate::{QueryBudget, QueryError, QueryResource, RepositoryDataTrust};

/// A file selection pinned to one generation and exclusive extension cursor.
#[derive(Debug, Clone, Copy)]
pub struct DqlQueryReadRequest {
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    after: Option<FactId>,
    limit: GenerationReadLimit,
}

impl DqlQueryReadRequest {
    /// Selects an immutable file and bounded backend page size.
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

    /// Continues after an unfiltered extension cursor, including empty DQL pages.
    #[must_use]
    pub const fn with_after(mut self, after: FactId) -> Self {
        self.after = Some(after);
        self
    }
}

/// A canonical native query and its persisted fact and provenance identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DqlQueryRecord {
    id: FactId,
    provenance: FactId,
    query: DqlQuery,
}

impl DqlQueryRecord {
    /// Returns the stored canonical query fact identity.
    #[must_use]
    pub const fn id(&self) -> FactId {
        self.id
    }
    /// Returns the verified provenance record identity.
    #[must_use]
    pub const fn provenance(&self) -> FactId {
        self.provenance
    }
    /// Borrows decoded native roles, local bindings and physical source evidence.
    #[must_use]
    pub const fn query(&self) -> &DqlQuery {
        &self.query
    }
}

/// Cooperative read/decode accounting, independent of process RSS measurement.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DqlQueryReadUsage {
    /// File, extension and provenance rows inspected, including continuation probes.
    pub rows: u64,
    /// Native query records returned.
    pub results: u64,
    /// Authored source bytes retained within the returned typed payloads.
    pub source_bytes: u64,
    /// Conservative admitted backend, codec and response workspace.
    pub memory_upper_bytes: u64,
    /// Exact serialized page bytes, including these counters.
    pub json_bytes: u64,
    /// Conservative UTF-8-byte token bound for the complete page.
    pub estimated_tokens: u64,
    /// Elapsed work before final serialization accounting, rounded up to microseconds.
    pub elapsed_micros: u64,
}

/// One filtered DQL result preserving unfiltered file-extension paging semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DqlQueryPage {
    scope: &'static str,
    trust: RepositoryDataTrust,
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    requested_after: Option<FactId>,
    queries: Vec<DqlQueryRecord>,
    scanned_extensions: u64,
    total_extensions: u64,
    skipped_non_dql: u64,
    range_complete: bool,
    next_cursor: Option<FactId>,
    total_matching_queries: Option<u64>,
    usage: DqlQueryReadUsage,
}

impl DqlQueryPage {
    /// Borrows canonical query records in stable persisted fact-ID order.
    #[must_use]
    pub fn queries(&self) -> &[DqlQueryRecord] {
        &self.queries
    }
    /// Returns the exclusive unfiltered extension cursor.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<FactId> {
        self.next_cursor
    }
    /// Reports exhaustion of this extension range, not complete language support.
    #[must_use]
    pub const fn range_complete(&self) -> bool {
        self.range_complete
    }
    /// Returns the selected total only when the first page exhausted the range.
    #[must_use]
    pub const fn total_matching_queries(&self) -> Option<u64> {
        self.total_matching_queries
    }
    /// Returns the total unfiltered extension rows for the selected file.
    #[must_use]
    pub const fn total_extensions(&self) -> u64 {
        self.total_extensions
    }
    /// Returns inspected extension rows, excluding the continuation probe.
    #[must_use]
    pub const fn scanned_extensions(&self) -> u64 {
        self.scanned_extensions
    }
    /// Identifies the bounded indexed-file scope.
    #[must_use]
    pub const fn scope(&self) -> &'static str {
        self.scope
    }
    /// Borrows observed cooperative work and output counters.
    #[must_use]
    pub const fn usage(&self) -> &DqlQueryReadUsage {
        &self.usage
    }
}

/// Typed failures without exposing repository payloads in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum DqlQueryReadError<E: Error + Send + Sync + 'static> {
    /// The immutable backend refused a read.
    #[error("DQL generation read failed")]
    Backend(#[source] E),
    /// Existing query controls refused work or output.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Existing generation controls refused work.
    #[error(transparent)]
    Control(#[from] GenerationControlError),
    /// The selected file is absent, foreign, non-DQL or source-inconsistent.
    #[error("DQL source file identity is inconsistent")]
    InvalidFile,
    /// A query, envelope or retained provenance record is inconsistent.
    #[error("DQL query identity or evidence is inconsistent")]
    InvalidRecord,
    /// Stable identities, totals or continuation semantics drifted.
    #[error("DQL extension page is inconsistent")]
    InvalidPage,
    /// The backend cannot supply indexed extensions for this file.
    #[error("indexed DQL file reads are unsupported by this generation reader")]
    IndexedFileReadUnsupported,
}

/// Reads one indexed file-extension page and canonically decodes native DQL.
///
/// The selected file must have language `dql`. Other namespaces are counted;
/// their rows can advance a cursor without yielding a query. Source text is
/// authenticated against the stored file hash, without filesystem access.
///
/// # Errors
/// Rejects unsupported file indexing, stale selectors, foreign or malformed
/// records, invalid paging, cancellation and existing resource-limit refusals.
pub fn read_dql_query_page<R: GenerationReader>(
    reader: &R,
    request: DqlQueryReadRequest,
    context: &GenerationContext<'_>,
    budget: QueryBudget,
) -> Result<DqlQueryPage, DqlQueryReadError<R::Error>> {
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
    // Native decoding owns a bounded AST and canonical identity temporaries.
    // Constrain backend materialization before those additional copies exist.
    let text_limit = (budget.max_memory_bytes() / 64).max(1);
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
    let backend = GenerationContext::new(context.cancellation(), backend_budget);
    let mut usage = DqlQueryReadUsage::default();
    usage.rows = checked_add(usage.rows, 1, QueryResource::Rows, budget.max_rows())?;
    let file = reader
        .file(request.file, &backend)
        .map_err(DqlQueryReadError::Backend)?
        .ok_or(DqlQueryReadError::InvalidFile)?;
    control.check()?;
    let file_source = file
        .evidence
        .source
        .as_ref()
        .ok_or(DqlQueryReadError::InvalidFile)?;
    if file.id != request.file
        || file.language != "dql"
        || file.repository != request.repository
        || file.generation != request.generation
        || file_source.repository() != request.repository
        || file_source.generation() != request.generation
        || file_source.span().file() != request.file
        || file_source.span().start_byte() != 0
        || file_source.span().end_byte() != file.byte_length
        || file_source.content_hash() != file.content_hash
    {
        return Err(DqlQueryReadError::InvalidFile);
    }
    let file_bytes = serialized_size(&file, text_limit, &control)?;
    usage.memory_upper_bytes = checked_add(
        usage.memory_upper_bytes,
        file_bytes
            .checked_mul(2)
            .ok_or(QueryError::MemoryUnavailable)?,
        QueryResource::MemoryBytes,
        budget.max_memory_bytes(),
    )?;
    let raw = reader.extensions_for_file(request.file, request.after, request.limit, &backend);
    control.check()?;
    context.check()?;
    let raw = raw
        .map_err(DqlQueryReadError::Backend)?
        .ok_or(DqlQueryReadError::IndexedFileReadUnsupported)?;
    if raw.total_available() > stats.extensions()
        || raw.items().len() > usize::from(request.limit.get())
        || raw.items().windows(2).any(|pair| pair[0].id >= pair[1].id)
        || request
            .after
            .is_some_and(|after| raw.items().iter().any(|record| record.id <= after))
        || (raw.completeness() == ReadPageCompleteness::Complete
            && (raw.next_cursor().is_some()
                || (request.after.is_none()
                    && u64::try_from(raw.items().len()).ok() != Some(raw.total_available()))))
        || (raw.completeness() == ReadPageCompleteness::Truncated
            && (raw.items().is_empty()
                || raw.next_cursor() != raw.items().last().map(|record| record.id)))
    {
        return Err(DqlQueryReadError::InvalidPage);
    }
    let encoded = serialized_size(&raw.items(), text_limit, &control)?;
    usage.memory_upper_bytes = checked_add(
        usage.memory_upper_bytes,
        encoded
            .checked_mul(64)
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
    let mut provenance = BTreeSet::new();
    let mut queries = Vec::new();
    let mut skipped_non_dql = 0;
    for envelope in raw.items() {
        control.check()?;
        context.check()?;
        let source = envelope
            .evidence
            .source
            .as_ref()
            .ok_or(DqlQueryReadError::InvalidRecord)?;
        if envelope.repository != request.repository
            || envelope.generation != request.generation
            || source.repository() != request.repository
            || source.generation() != request.generation
            || source.span().file() != request.file
            || source.content_hash() != file.content_hash
            || source.span().end_byte() > file.byte_length
        {
            return Err(DqlQueryReadError::InvalidRecord);
        }
        if envelope.namespace != DQL_QUERY_NAMESPACE {
            skipped_non_dql += 1;
            continue;
        }
        let query =
            decode_dql_query_envelope(envelope).map_err(|_| DqlQueryReadError::InvalidRecord)?;
        control.check()?;
        if query.source().span() != file_source.span()
            || query.source().content_hash() != file.content_hash
        {
            return Err(DqlQueryReadError::InvalidRecord);
        }
        if provenance.insert(envelope.provenance) {
            usage.rows = checked_add(usage.rows, 1, QueryResource::Rows, budget.max_rows())?;
            let record = reader
                .provenance(envelope.provenance, &backend)
                .map_err(DqlQueryReadError::Backend)?
                .ok_or(DqlQueryReadError::InvalidRecord)?;
            control.check()?;
            if record.id != envelope.provenance
                || record.repository != request.repository
                || record.generation != request.generation
            {
                return Err(DqlQueryReadError::InvalidRecord);
            }
        }
        usage.results = checked_add(
            usage.results,
            1,
            QueryResource::Results,
            budget.max_results(),
        )?;
        usage.source_bytes = checked_add(
            usage.source_bytes,
            checked_usize_to_u64(query.text().len())?,
            QueryResource::SourceBytes,
            budget.max_source_bytes(),
        )?;
        queries
            .try_reserve(1)
            .map_err(|_| QueryError::MemoryUnavailable)?;
        queries.push(DqlQueryRecord {
            id: envelope.id,
            provenance: envelope.provenance,
            query,
        });
    }
    if reader.metadata() != metadata || reader.stats() != stats {
        return Err(DqlQueryReadError::InvalidPage);
    }
    let range_complete = raw.completeness() == ReadPageCompleteness::Complete;
    usage.elapsed_micros =
        checked_u128_to_u64(started.elapsed().as_nanos().saturating_add(999) / 1000);
    let mut result = DqlQueryPage {
        scope: "file_extensions",
        trust: RepositoryDataTrust::UntrustedRepositoryData,
        repository: request.repository,
        generation: request.generation,
        file: request.file,
        requested_after: request.after,
        queries,
        scanned_extensions,
        total_extensions: raw.total_available(),
        skipped_non_dql,
        range_complete,
        next_cursor: raw.next_cursor(),
        total_matching_queries: if request.after.is_none() && range_complete {
            Some(usage.results)
        } else {
            None
        },
        usage,
    };
    for _ in 0..8 {
        let bytes = serialized_size(&result, budget.max_json_bytes(), &control).map_err(
            |error| match error {
                QueryError::BudgetExceeded {
                    resource: QueryResource::MemoryBytes,
                    ..
                } => QueryError::BudgetExceeded {
                    resource: QueryResource::JsonBytes,
                    limit: budget.max_json_bytes(),
                },
                error => error,
            },
        )?;
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
