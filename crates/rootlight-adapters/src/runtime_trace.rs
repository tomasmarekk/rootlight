//! Bounded import of explicit local runtime observations.
//!
//! Runtime evidence is generation-bound and remains in a separate overlay so
//! observations cannot silently replace or strengthen canonical static facts.
//!
//! The strict JSON wire shape is:
//!
//! ```text
//! {
//!   "schema": "rootlight.runtime-trace/1",
//!   "repository": "<repo1 id>",
//!   "generation": "<gen1 id>",
//!   "producer": {
//!     "name": "<source-free label>",
//!     "version": "<source-free label>",
//!     "configuration_hash": "<b3 hash>",
//!     "binary_digest": "<b3 hash>"
//!   },
//!   "records": [{
//!     "kind": "calls",
//!     "subject": "<known sym1 id>",
//!     "object": "<known sym1 id>",
//!     "count": 1
//!   }]
//! }
//! ```
//!
//! Unknown fields and relation kinds are rejected. Supported kinds are
//! `calls`, `reads`, `writes`, `throws`, `handles_error`, `tests`,
//! `calls_route`, `publishes`, `consumes`, `reads_table`, `writes_table`, and
//! `calls_foreign`.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, GenerationId, RepositoryId, SymbolId, content_hash};
use rootlight_ir::{NormalizedIrDocument, ProducerIdentity, ProducerKind, RelationPredicate};
use serde::Deserialize;

use crate::ADAPTER_VERSION;

/// Exact wire schema accepted by [`import_runtime_trace`].
pub const RUNTIME_TRACE_SCHEMA_VERSION: &str = "rootlight.runtime-trace/1";

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CANONICAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 1_000_000;
const MAX_KNOWN_SYMBOLS: usize = 2_000_000;
const MAX_OBSERVATIONS: u64 = 1_000_000_000_000;
const CANCELLATION_CHECK_INTERVAL: usize = 128;
const CANONICAL_DOMAIN: &[u8] = b"rootlight/runtime-trace-overlay/v1";

/// Fixed resource classes enforced while importing runtime observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeTraceResource {
    /// Encoded caller-supplied JSON bytes.
    InputBytes,
    /// Canonical bytes hashed for semantic overlay identity.
    CanonicalBytes,
    /// Runtime relation records in the trace.
    Records,
    /// Static symbols admitted as valid relation endpoints.
    KnownSymbols,
    /// Sum of observation counts across unique relations.
    Observations,
}

/// Caller-selected limits constrained by process-wide hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeTraceLimits {
    max_input_bytes: usize,
    max_canonical_bytes: usize,
    max_records: usize,
    max_known_symbols: usize,
    max_observations: u64,
}

impl RuntimeTraceLimits {
    /// Creates an import policy no broader than the process hard ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeTraceImportError::InvalidLimit`] when a ceiling is
    /// zero or exceeds its corresponding hard maximum.
    pub fn new(
        max_input_bytes: usize,
        max_canonical_bytes: usize,
        max_records: usize,
        max_known_symbols: usize,
        max_observations: u64,
    ) -> Result<Self, RuntimeTraceImportError> {
        require_limit(
            RuntimeTraceResource::InputBytes,
            max_input_bytes,
            MAX_INPUT_BYTES,
        )?;
        require_limit(
            RuntimeTraceResource::CanonicalBytes,
            max_canonical_bytes,
            MAX_CANONICAL_BYTES,
        )?;
        require_limit(RuntimeTraceResource::Records, max_records, MAX_RECORDS)?;
        require_limit(
            RuntimeTraceResource::KnownSymbols,
            max_known_symbols,
            MAX_KNOWN_SYMBOLS,
        )?;
        require_u64_limit(
            RuntimeTraceResource::Observations,
            max_observations,
            MAX_OBSERVATIONS,
        )?;
        Ok(Self {
            max_input_bytes,
            max_canonical_bytes,
            max_records,
            max_known_symbols,
            max_observations,
        })
    }

    /// Returns the maximum encoded trace byte length.
    #[must_use]
    pub const fn max_input_bytes(self) -> usize {
        self.max_input_bytes
    }

    /// Returns the maximum canonical identity payload byte length.
    #[must_use]
    pub const fn max_canonical_bytes(self) -> usize {
        self.max_canonical_bytes
    }

    /// Returns the maximum runtime relation record count.
    #[must_use]
    pub const fn max_records(self) -> usize {
        self.max_records
    }

    /// Returns the maximum static symbol count accepted for endpoint lookup.
    #[must_use]
    pub const fn max_known_symbols(self) -> usize {
        self.max_known_symbols
    }

    /// Returns the maximum sum of unique relation observation counts.
    #[must_use]
    pub const fn max_observations(self) -> u64 {
        self.max_observations
    }
}

impl Default for RuntimeTraceLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: MAX_INPUT_BYTES,
            max_canonical_bytes: MAX_CANONICAL_BYTES,
            max_records: MAX_RECORDS,
            max_known_symbols: MAX_KNOWN_SYMBOLS,
            max_observations: MAX_OBSERVATIONS,
        }
    }
}

/// Generation binding and policy for one runtime trace import.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeTraceImportRequest<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    normalized_generation: &'a NormalizedIrDocument,
    limits: RuntimeTraceLimits,
    cancellation: &'a Cancellation,
}

impl<'a> RuntimeTraceImportRequest<'a> {
    /// Creates a request bound to one already-normalized static generation.
    ///
    /// The importer reads only `normalized_generation`; it never reads source
    /// paths, executes repository code, or performs network access.
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        generation: GenerationId,
        normalized_generation: &'a NormalizedIrDocument,
        cancellation: &'a Cancellation,
    ) -> Self {
        Self {
            repository,
            generation,
            normalized_generation,
            limits: RuntimeTraceLimits {
                max_input_bytes: MAX_INPUT_BYTES,
                max_canonical_bytes: MAX_CANONICAL_BYTES,
                max_records: MAX_RECORDS,
                max_known_symbols: MAX_KNOWN_SYMBOLS,
                max_observations: MAX_OBSERVATIONS,
            },
            cancellation,
        }
    }

    /// Applies a narrower caller-selected resource policy.
    #[must_use]
    pub const fn with_limits(mut self, limits: RuntimeTraceLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Runtime relation kinds admitted by the version 1 trace subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RuntimeTraceRelationKind {
    /// One callable invoked another callable.
    Calls,
    /// One symbol read another symbol.
    Reads,
    /// One symbol wrote another symbol.
    Writes,
    /// One callable threw an error symbol.
    Throws,
    /// One callable handled an error symbol.
    HandlesError,
    /// One test exercised another symbol.
    Tests,
    /// One client symbol called a route symbol.
    CallsRoute,
    /// One symbol published to a message-topic symbol.
    Publishes,
    /// One symbol consumed from a message-topic symbol.
    Consumes,
    /// One symbol read a database-object symbol.
    ReadsTable,
    /// One symbol wrote a database-object symbol.
    WritesTable,
    /// One symbol invoked a foreign symbol.
    CallsForeign,
}

impl RuntimeTraceRelationKind {
    /// Returns the corresponding common relation predicate.
    #[must_use]
    pub const fn predicate(self) -> RelationPredicate {
        match self {
            Self::Calls => RelationPredicate::Calls,
            Self::Reads => RelationPredicate::Reads,
            Self::Writes => RelationPredicate::Writes,
            Self::Throws => RelationPredicate::Throws,
            Self::HandlesError => RelationPredicate::HandlesError,
            Self::Tests => RelationPredicate::Tests,
            Self::CallsRoute => RelationPredicate::CallsRoute,
            Self::Publishes => RelationPredicate::Publishes,
            Self::Consumes => RelationPredicate::Consumes,
            Self::ReadsTable => RelationPredicate::ReadsTable,
            Self::WritesTable => RelationPredicate::WritesTable,
            Self::CallsForeign => RelationPredicate::CallsForeign,
        }
    }

    const fn canonical_tag(self) -> u8 {
        match self {
            Self::Calls => 0,
            Self::Reads => 1,
            Self::Writes => 2,
            Self::Throws => 3,
            Self::HandlesError => 4,
            Self::Tests => 5,
            Self::CallsRoute => 6,
            Self::Publishes => 7,
            Self::Consumes => 8,
            Self::ReadsTable => 9,
            Self::WritesTable => 10,
            Self::CallsForeign => 11,
        }
    }
}

/// One exact observed relation retained outside canonical static semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeTraceRelation {
    kind: RuntimeTraceRelationKind,
    subject: SymbolId,
    object: SymbolId,
    count: u64,
}

impl RuntimeTraceRelation {
    /// Returns the observed relation kind.
    #[must_use]
    pub const fn kind(self) -> RuntimeTraceRelationKind {
        self.kind
    }

    /// Returns the known static subject symbol.
    #[must_use]
    pub const fn subject(self) -> SymbolId {
        self.subject
    }

    /// Returns the known static object symbol.
    #[must_use]
    pub const fn object(self) -> SymbolId {
        self.object
    }

    /// Returns the positive aggregate observation count.
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }
}

/// Source-free provenance retained for one imported runtime overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTraceProvenance {
    producer: ProducerIdentity,
    binary_digest: ContentHash,
    trace_hash: ContentHash,
}

impl RuntimeTraceProvenance {
    /// Returns the immutable runtime-trace producer class.
    #[must_use]
    pub const fn producer_kind(&self) -> ProducerKind {
        ProducerKind::RuntimeTrace
    }

    /// Returns the caller-declared trace producer identity.
    #[must_use]
    pub const fn producer(&self) -> &ProducerIdentity {
        &self.producer
    }

    /// Returns the caller-declared producing binary digest.
    #[must_use]
    pub const fn binary_digest(&self) -> ContentHash {
        self.binary_digest
    }

    /// Returns the hash of the canonical, deduplicated trace semantics.
    #[must_use]
    pub const fn trace_hash(&self) -> ContentHash {
        self.trace_hash
    }

    /// Returns the Rootlight importer version that interpreted the trace.
    #[must_use]
    pub const fn importer_version(&self) -> &'static str {
        ADAPTER_VERSION
    }
}

/// Deterministic generation-bound runtime relation overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTraceOverlay {
    repository: RepositoryId,
    generation: GenerationId,
    provenance: RuntimeTraceProvenance,
    relations: Vec<RuntimeTraceRelation>,
    total_observations: u64,
}

impl RuntimeTraceOverlay {
    /// Returns the repository owning the static generation.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation observed by the trace.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the runtime producer and canonical trace identity.
    #[must_use]
    pub const fn provenance(&self) -> &RuntimeTraceProvenance {
        &self.provenance
    }

    /// Returns canonically ordered, deduplicated observed relations.
    #[must_use]
    pub fn relations(&self) -> &[RuntimeTraceRelation] {
        &self.relations
    }

    /// Returns the sum of unique relation observation counts.
    #[must_use]
    pub const fn total_observations(&self) -> u64 {
        self.total_observations
    }

    /// Consumes the overlay and returns its canonical runtime relations.
    #[must_use]
    pub fn into_relations(self) -> Vec<RuntimeTraceRelation> {
        self.relations
    }
}

/// Failure importing an explicit local runtime trace.
///
/// Variants intentionally retain only source-free classes and bounded numeric
/// accounting; trace payloads, repository paths, and producer labels are never
/// embedded in an error or its source chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeTraceImportError {
    /// A configured ceiling was zero or exceeded its process hard maximum.
    #[error("runtime trace limit is invalid for {resource:?}")]
    InvalidLimit {
        /// Resource whose configured ceiling was invalid.
        resource: RuntimeTraceResource,
    },
    /// Input or derived accounting exceeded the active bounded policy.
    #[error("runtime trace exceeded {resource:?} limit: {observed} > {max}")]
    LimitExceeded {
        /// Resource whose active ceiling was exceeded.
        resource: RuntimeTraceResource,
        /// Observed bounded count.
        observed: u64,
        /// Active caller-selected ceiling.
        max: u64,
    },
    /// JSON syntax, shape, identity encoding, or enum values were invalid.
    #[error("runtime trace input is malformed")]
    MalformedTrace,
    /// The exact supported schema marker was absent.
    #[error("runtime trace schema is unsupported")]
    UnsupportedSchema,
    /// The trace or static generation named another repository.
    #[error("runtime trace repository binding does not match")]
    RepositoryMismatch,
    /// The trace or static document named another immutable generation.
    #[error("runtime trace generation binding is stale")]
    StaleGeneration,
    /// The supplied normalized generation contained inconsistent entity ownership.
    #[error("runtime trace static generation is invalid")]
    InvalidGeneration,
    /// Producer labels violated source-free normalized identity rules.
    #[error("runtime trace producer identity is invalid")]
    InvalidProducer,
    /// An observation count was zero or could not be aggregated safely.
    #[error("runtime trace observation count is invalid")]
    InvalidObservationCount,
    /// A runtime endpoint was absent from the supplied static generation.
    #[error("runtime trace relation references an unknown symbol")]
    UnknownSymbol,
    /// Equivalent endpoints and kind declared different aggregate counts.
    #[error("runtime trace contains conflicting duplicate relations")]
    ConflictingRecord,
    /// Cooperative cancellation interrupted bounded import work.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// Canonical semantic identity could not be represented within its bound.
    #[error("runtime trace canonical identity is invalid")]
    CanonicalIdentity,
}

/// Imports one versioned caller-supplied JSON trace as a separate overlay.
///
/// Exact duplicate records are collapsed. Records with identical endpoints and
/// kind but different counts are rejected instead of guessing aggregation
/// semantics. Every endpoint must be an entity in the supplied generation.
///
/// # Errors
///
/// Returns [`RuntimeTraceImportError`] for cancellation, malformed or
/// unsupported input, identity mismatch, unknown endpoints, conflicting
/// records, invalid counts, or resource-limit violations.
pub fn import_runtime_trace(
    trace: &[u8],
    request: RuntimeTraceImportRequest<'_>,
) -> Result<RuntimeTraceOverlay, RuntimeTraceImportError> {
    request.cancellation.check()?;
    check_usize_limit(
        RuntimeTraceResource::InputBytes,
        trace.len(),
        request.limits.max_input_bytes,
    )?;
    validate_generation_binding(&request)?;

    let wire: RuntimeTraceWire =
        serde_json::from_slice(trace).map_err(|_| RuntimeTraceImportError::MalformedTrace)?;
    request.cancellation.check()?;
    if wire.schema != RUNTIME_TRACE_SCHEMA_VERSION {
        return Err(RuntimeTraceImportError::UnsupportedSchema);
    }
    if wire.repository != request.repository {
        return Err(RuntimeTraceImportError::RepositoryMismatch);
    }
    if wire.generation != request.generation {
        return Err(RuntimeTraceImportError::StaleGeneration);
    }
    check_usize_limit(
        RuntimeTraceResource::Records,
        wire.records.len(),
        request.limits.max_records,
    )?;

    let producer = ProducerIdentity::new(
        &wire.producer.name,
        &wire.producer.version,
        wire.producer.configuration_hash,
    )
    .map_err(|_| RuntimeTraceImportError::InvalidProducer)?;
    let known_symbols = collect_known_symbols(&request)?;
    let (relations, total_observations) =
        canonicalize_records(wire.records, &known_symbols, &request)?;
    let trace_hash = canonical_trace_hash(
        request.repository,
        request.generation,
        &producer,
        wire.producer.binary_digest,
        &relations,
        request.limits.max_canonical_bytes,
    )?;
    request.cancellation.check()?;

    Ok(RuntimeTraceOverlay {
        repository: request.repository,
        generation: request.generation,
        provenance: RuntimeTraceProvenance {
            producer,
            binary_digest: wire.producer.binary_digest,
            trace_hash,
        },
        relations,
        total_observations,
    })
}

fn validate_generation_binding(
    request: &RuntimeTraceImportRequest<'_>,
) -> Result<(), RuntimeTraceImportError> {
    if request.normalized_generation.repository != request.repository {
        return Err(RuntimeTraceImportError::RepositoryMismatch);
    }
    if request.normalized_generation.generation != request.generation {
        return Err(RuntimeTraceImportError::StaleGeneration);
    }
    check_usize_limit(
        RuntimeTraceResource::KnownSymbols,
        request.normalized_generation.entities.len(),
        request.limits.max_known_symbols,
    )
}

fn collect_known_symbols(
    request: &RuntimeTraceImportRequest<'_>,
) -> Result<BTreeSet<SymbolId>, RuntimeTraceImportError> {
    let mut known = BTreeSet::new();
    for (index, entity) in request.normalized_generation.entities.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            request.cancellation.check()?;
        }
        if entity.repository != request.repository
            || entity.generation != request.generation
            || !known.insert(entity.id)
        {
            return Err(RuntimeTraceImportError::InvalidGeneration);
        }
    }
    Ok(known)
}

fn canonicalize_records(
    records: Vec<RuntimeTraceRecordWire>,
    known_symbols: &BTreeSet<SymbolId>,
    request: &RuntimeTraceImportRequest<'_>,
) -> Result<(Vec<RuntimeTraceRelation>, u64), RuntimeTraceImportError> {
    let mut unique = BTreeMap::new();
    let mut total = 0_u64;
    for (index, record) in records.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            request.cancellation.check()?;
        }
        if record.count == 0 {
            return Err(RuntimeTraceImportError::InvalidObservationCount);
        }
        if !known_symbols.contains(&record.subject) || !known_symbols.contains(&record.object) {
            return Err(RuntimeTraceImportError::UnknownSymbol);
        }
        let kind = RuntimeTraceRelationKind::from(record.kind);
        let key = (record.subject, kind, record.object);
        match unique.get(&key) {
            Some(existing) if *existing == record.count => continue,
            Some(_) => return Err(RuntimeTraceImportError::ConflictingRecord),
            None => {
                total = total
                    .checked_add(record.count)
                    .ok_or(RuntimeTraceImportError::InvalidObservationCount)?;
                check_u64_limit(
                    RuntimeTraceResource::Observations,
                    total,
                    request.limits.max_observations,
                )?;
                unique.insert(key, record.count);
            }
        }
    }

    Ok((
        unique
            .into_iter()
            .map(|((subject, kind, object), count)| RuntimeTraceRelation {
                kind,
                subject,
                object,
                count,
            })
            .collect(),
        total,
    ))
}

fn canonical_trace_hash(
    repository: RepositoryId,
    generation: GenerationId,
    producer: &ProducerIdentity,
    binary_digest: ContentHash,
    relations: &[RuntimeTraceRelation],
    max_canonical_bytes: usize,
) -> Result<ContentHash, RuntimeTraceImportError> {
    let relation_bytes = relations
        .len()
        .checked_mul(1 + 20 + 20 + 8)
        .ok_or(RuntimeTraceImportError::CanonicalIdentity)?;
    let canonical_bytes = CANONICAL_DOMAIN
        .len()
        .checked_add(repository.as_bytes().len())
        .and_then(|value| value.checked_add(generation.as_bytes().len()))
        .and_then(|value| value.checked_add(8 + producer.name().len()))
        .and_then(|value| value.checked_add(8 + producer.version().len()))
        .and_then(|value| value.checked_add(producer.configuration_hash().as_bytes().len()))
        .and_then(|value| value.checked_add(binary_digest.as_bytes().len()))
        .and_then(|value| value.checked_add(8 + ADAPTER_VERSION.len()))
        .and_then(|value| value.checked_add(relation_bytes))
        .ok_or(RuntimeTraceImportError::CanonicalIdentity)?;
    check_usize_limit(
        RuntimeTraceResource::CanonicalBytes,
        canonical_bytes,
        max_canonical_bytes,
    )?;

    let mut canonical = Vec::with_capacity(canonical_bytes);
    canonical.extend_from_slice(CANONICAL_DOMAIN);
    canonical.extend_from_slice(repository.as_bytes());
    canonical.extend_from_slice(generation.as_bytes());
    append_bounded_string(&mut canonical, producer.name())?;
    append_bounded_string(&mut canonical, producer.version())?;
    canonical.extend_from_slice(producer.configuration_hash().as_bytes());
    canonical.extend_from_slice(binary_digest.as_bytes());
    append_bounded_string(&mut canonical, ADAPTER_VERSION)?;
    for relation in relations {
        canonical.push(relation.kind.canonical_tag());
        canonical.extend_from_slice(relation.subject.as_bytes());
        canonical.extend_from_slice(relation.object.as_bytes());
        canonical.extend_from_slice(&relation.count.to_be_bytes());
    }
    if canonical.len() != canonical_bytes {
        return Err(RuntimeTraceImportError::CanonicalIdentity);
    }
    Ok(content_hash(&canonical))
}

fn append_bounded_string(output: &mut Vec<u8>, value: &str) -> Result<(), RuntimeTraceImportError> {
    let length =
        u64::try_from(value.len()).map_err(|_| RuntimeTraceImportError::CanonicalIdentity)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn require_limit(
    resource: RuntimeTraceResource,
    requested: usize,
    hard_max: usize,
) -> Result<(), RuntimeTraceImportError> {
    if requested == 0 || requested > hard_max {
        return Err(RuntimeTraceImportError::InvalidLimit { resource });
    }
    Ok(())
}

fn require_u64_limit(
    resource: RuntimeTraceResource,
    requested: u64,
    hard_max: u64,
) -> Result<(), RuntimeTraceImportError> {
    if requested == 0 || requested > hard_max {
        return Err(RuntimeTraceImportError::InvalidLimit { resource });
    }
    Ok(())
}

fn check_usize_limit(
    resource: RuntimeTraceResource,
    observed: usize,
    max: usize,
) -> Result<(), RuntimeTraceImportError> {
    if observed > max {
        return Err(RuntimeTraceImportError::LimitExceeded {
            resource,
            observed: u64::try_from(observed).unwrap_or(u64::MAX),
            max: u64::try_from(max).unwrap_or(u64::MAX),
        });
    }
    Ok(())
}

fn check_u64_limit(
    resource: RuntimeTraceResource,
    observed: u64,
    max: u64,
) -> Result<(), RuntimeTraceImportError> {
    if observed > max {
        return Err(RuntimeTraceImportError::LimitExceeded {
            resource,
            observed,
            max,
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTraceWire {
    schema: String,
    repository: RepositoryId,
    generation: GenerationId,
    producer: RuntimeTraceProducerWire,
    records: Vec<RuntimeTraceRecordWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTraceProducerWire {
    name: String,
    version: String,
    configuration_hash: ContentHash,
    binary_digest: ContentHash,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTraceRecordWire {
    kind: RuntimeTraceRelationKindWire,
    subject: SymbolId,
    object: SymbolId,
    count: u64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeTraceRelationKindWire {
    Calls,
    Reads,
    Writes,
    Throws,
    HandlesError,
    Tests,
    CallsRoute,
    Publishes,
    Consumes,
    ReadsTable,
    WritesTable,
    CallsForeign,
}

impl From<RuntimeTraceRelationKindWire> for RuntimeTraceRelationKind {
    fn from(value: RuntimeTraceRelationKindWire) -> Self {
        match value {
            RuntimeTraceRelationKindWire::Calls => Self::Calls,
            RuntimeTraceRelationKindWire::Reads => Self::Reads,
            RuntimeTraceRelationKindWire::Writes => Self::Writes,
            RuntimeTraceRelationKindWire::Throws => Self::Throws,
            RuntimeTraceRelationKindWire::HandlesError => Self::HandlesError,
            RuntimeTraceRelationKindWire::Tests => Self::Tests,
            RuntimeTraceRelationKindWire::CallsRoute => Self::CallsRoute,
            RuntimeTraceRelationKindWire::Publishes => Self::Publishes,
            RuntimeTraceRelationKindWire::Consumes => Self::Consumes,
            RuntimeTraceRelationKindWire::ReadsTable => Self::ReadsTable,
            RuntimeTraceRelationKindWire::WritesTable => Self::WritesTable,
            RuntimeTraceRelationKindWire::CallsForeign => Self::CallsForeign,
        }
    }
}
