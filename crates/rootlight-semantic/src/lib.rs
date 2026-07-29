//! Opt-in, model-agnostic semantic retrieval over caller-approved vectors.
//!
//! The crate never loads a model, reads a repository, or persists state. A caller
//! must explicitly build a generation-bound artifact from stable item identities,
//! content hashes, and vectors produced by its own local model runtime.
//!
//! # Examples
//!
//! ```
//! use rootlight_cancel::Cancellation;
//! use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
//! use rootlight_semantic::{
//!     SemanticContext, SemanticError, SemanticItem, SemanticLimits, SemanticQuery,
//!     build_artifact, query_artifact,
//! };
//!
//! # fn main() -> Result<(), SemanticError> {
//! let context = SemanticContext::new(
//!     RepositoryId::from_bytes([1; 16]),
//!     GenerationId::from_bytes([2; 20]),
//!     "local-model-v1".to_owned(),
//!     ContentHash::from_bytes([3; 32]),
//!     "chunk-policy-v1".to_owned(),
//! )?;
//! let item = SemanticItem::new(
//!     "item-a".to_owned(),
//!     ContentHash::from_bytes([4; 32]),
//!     vec![1.0, 0.0],
//! )?;
//! let artifact = build_artifact(
//!     context.clone(),
//!     vec![item],
//!     SemanticLimits::default(),
//!     &Cancellation::new(),
//! )?;
//! let query = SemanticQuery::new(context, vec![1.0, 0.0], 1)?;
//! let response = query_artifact(
//!     artifact.as_bytes(),
//!     &query,
//!     SemanticLimits::default(),
//!     &Cancellation::new(),
//! )?;
//! assert_eq!(response.matches()[0].item_id, "item-a");
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::BTreeSet;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId, content_hash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Version of the canonical semantic artifact envelope.
pub const SEMANTIC_ARTIFACT_SCHEMA: &str = "rootlight.semantic-artifact/1";

const HARD_MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
const HARD_MAX_DISK_BYTES: usize = 8 * 1024 * 1024;
const HARD_MAX_MEMORY_BYTES: usize = 32 * 1024 * 1024;
const HARD_MAX_ITEMS: usize = 10_000;
const HARD_MAX_DIMENSIONS: usize = 4_096;
const HARD_MAX_RESULTS: usize = 1_000;
const MAX_ITEM_ID_BYTES: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 128;
const MAX_CHUNK_POLICY_BYTES: usize = 128;
const ACCOUNTED_ITEM_OVERHEAD: usize = 96;
const CANCELLATION_INTERVAL: usize = 64;

/// Independent resource ceilings for semantic build, decode, and query work.
///
/// Input, encoded artifact, and retained-memory accounting are separate so a
/// compact artifact cannot justify an unexpectedly large working set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticLimits {
    max_input_bytes: usize,
    max_disk_bytes: usize,
    max_memory_bytes: usize,
    max_items: usize,
    max_dimensions: usize,
    max_results: usize,
}

impl SemanticLimits {
    /// Returns the configured caller-input byte ceiling.
    #[must_use]
    pub const fn max_input_bytes(self) -> usize {
        self.max_input_bytes
    }

    /// Returns the configured encoded-artifact byte ceiling.
    #[must_use]
    pub const fn max_disk_bytes(self) -> usize {
        self.max_disk_bytes
    }

    /// Returns the configured retained-memory accounting ceiling.
    #[must_use]
    pub const fn max_memory_bytes(self) -> usize {
        self.max_memory_bytes
    }

    /// Returns the configured item-count ceiling.
    #[must_use]
    pub const fn max_items(self) -> usize {
        self.max_items
    }

    /// Returns the configured vector-dimension ceiling.
    #[must_use]
    pub const fn max_dimensions(self) -> usize {
        self.max_dimensions
    }

    /// Returns the configured result-count ceiling.
    #[must_use]
    pub const fn max_results(self) -> usize {
        self.max_results
    }

    /// Replaces the caller-input byte ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidLimits`] when `value` is zero or exceeds
    /// the implementation hard ceiling.
    pub fn with_max_input_bytes(mut self, value: usize) -> Result<Self, SemanticError> {
        require_limit(value, HARD_MAX_INPUT_BYTES)?;
        self.max_input_bytes = value;
        Ok(self)
    }

    /// Replaces the encoded-artifact byte ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidLimits`] when `value` is zero or exceeds
    /// the implementation hard ceiling.
    pub fn with_max_disk_bytes(mut self, value: usize) -> Result<Self, SemanticError> {
        require_limit(value, HARD_MAX_DISK_BYTES)?;
        self.max_disk_bytes = value;
        Ok(self)
    }

    /// Replaces the retained-memory accounting ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidLimits`] when `value` is zero or exceeds
    /// the implementation hard ceiling.
    pub fn with_max_memory_bytes(mut self, value: usize) -> Result<Self, SemanticError> {
        require_limit(value, HARD_MAX_MEMORY_BYTES)?;
        self.max_memory_bytes = value;
        Ok(self)
    }

    /// Replaces the item-count ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidLimits`] when `value` is zero or exceeds
    /// the implementation hard ceiling.
    pub fn with_max_items(mut self, value: usize) -> Result<Self, SemanticError> {
        require_limit(value, HARD_MAX_ITEMS)?;
        self.max_items = value;
        Ok(self)
    }

    /// Replaces the vector-dimension ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidLimits`] when `value` is zero or exceeds
    /// the implementation hard ceiling.
    pub fn with_max_dimensions(mut self, value: usize) -> Result<Self, SemanticError> {
        require_limit(value, HARD_MAX_DIMENSIONS)?;
        self.max_dimensions = value;
        Ok(self)
    }

    /// Replaces the result-count ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidLimits`] when `value` is zero or exceeds
    /// the implementation hard ceiling.
    pub fn with_max_results(mut self, value: usize) -> Result<Self, SemanticError> {
        require_limit(value, HARD_MAX_RESULTS)?;
        self.max_results = value;
        Ok(self)
    }
}

impl Default for SemanticLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 4 * 1024 * 1024,
            max_disk_bytes: 4 * 1024 * 1024,
            max_memory_bytes: 16 * 1024 * 1024,
            max_items: 5_000,
            max_dimensions: 2_048,
            max_results: 100,
        }
    }
}

/// Immutable repository, generation, model, and chunk-policy binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticContext {
    repository: RepositoryId,
    generation: GenerationId,
    model_id: String,
    model_hash: ContentHash,
    chunk_policy_version: String,
}

impl SemanticContext {
    /// Creates a validated artifact context.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidIdentifier`] when the model identifier or
    /// chunk-policy version is empty, oversized, or not a stable ASCII token.
    pub fn new(
        repository: RepositoryId,
        generation: GenerationId,
        model_id: String,
        model_hash: ContentHash,
        chunk_policy_version: String,
    ) -> Result<Self, SemanticError> {
        validate_token(&model_id, MAX_MODEL_ID_BYTES)?;
        validate_token(&chunk_policy_version, MAX_CHUNK_POLICY_BYTES)?;
        Ok(Self {
            repository,
            generation,
            model_id,
            model_hash,
            chunk_policy_version,
        })
    }

    /// Returns the repository owning the artifact.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable structural generation bound to the artifact.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the caller-declared local model identifier.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Returns the hash of the exact local model build.
    #[must_use]
    pub const fn model_hash(&self) -> ContentHash {
        self.model_hash
    }

    /// Returns the caller-declared chunk-policy version.
    #[must_use]
    pub fn chunk_policy_version(&self) -> &str {
        &self.chunk_policy_version
    }
}

/// One caller-approved semantic item and its local vector.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticItem {
    item_id: String,
    content_hash: ContentHash,
    vector: Vec<f32>,
}

impl SemanticItem {
    /// Creates a validated semantic item.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidIdentifier`] for an unstable item ID and
    /// [`SemanticError::InvalidVector`] for an empty, non-finite, zero, or
    /// implementation-wide oversized vector.
    pub fn new(
        item_id: String,
        content_hash: ContentHash,
        vector: Vec<f32>,
    ) -> Result<Self, SemanticError> {
        Self::new_with_cancellation(
            item_id,
            content_hash,
            vector,
            HARD_MAX_DIMENSIONS,
            &Cancellation::new(),
        )
    }

    fn new_with_cancellation(
        item_id: String,
        content_hash: ContentHash,
        vector: Vec<f32>,
        max_dimensions: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, SemanticError> {
        validate_token(&item_id, MAX_ITEM_ID_BYTES)?;
        validate_vector(&vector, max_dimensions, cancellation)?;
        Ok(Self {
            item_id,
            content_hash,
            vector,
        })
    }

    /// Returns the caller-owned stable item identifier.
    #[must_use]
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    /// Returns the source-content hash approved by the caller.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns the local vector without exposing any source content.
    #[must_use]
    pub fn vector(&self) -> &[f32] {
        &self.vector
    }
}

/// A generation- and model-bound semantic query.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticQuery {
    context: SemanticContext,
    vector: Vec<f32>,
    max_results: usize,
}

impl SemanticQuery {
    /// Creates a validated query.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidVector`] for an invalid query vector and
    /// [`SemanticError::ResultLimitExceeded`] when the requested result count is
    /// zero or exceeds the implementation hard ceiling.
    pub fn new(
        context: SemanticContext,
        vector: Vec<f32>,
        max_results: usize,
    ) -> Result<Self, SemanticError> {
        if max_results == 0 || max_results > HARD_MAX_RESULTS {
            return Err(SemanticError::ResultLimitExceeded);
        }
        validate_vector(&vector, HARD_MAX_DIMENSIONS, &Cancellation::new())?;
        Ok(Self {
            context,
            vector,
            max_results,
        })
    }

    /// Returns the exact query context expected from the artifact.
    #[must_use]
    pub const fn context(&self) -> &SemanticContext {
        &self.context
    }

    /// Returns the caller-supplied local query vector.
    #[must_use]
    pub fn vector(&self) -> &[f32] {
        &self.vector
    }

    /// Returns the requested maximum number of matches.
    #[must_use]
    pub const fn max_results(&self) -> usize {
        self.max_results
    }
}

/// Resource accounting recorded for a built or decoded artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticAccounting {
    input_bytes: usize,
    memory_bytes: usize,
    disk_bytes: usize,
    items: usize,
    dimensions: usize,
}

impl SemanticAccounting {
    /// Returns caller payload bytes accounted during the operation.
    #[must_use]
    pub const fn input_bytes(self) -> usize {
        self.input_bytes
    }

    /// Returns deterministic retained semantic payload accounting.
    #[must_use]
    pub const fn memory_bytes(self) -> usize {
        self.memory_bytes
    }

    /// Returns canonical encoded artifact bytes.
    #[must_use]
    pub const fn disk_bytes(self) -> usize {
        self.disk_bytes
    }

    /// Returns the number of indexed items.
    #[must_use]
    pub const fn items(self) -> usize {
        self.items
    }

    /// Returns the common vector dimension.
    #[must_use]
    pub const fn dimensions(self) -> usize {
        self.dimensions
    }
}

/// A canonical, integrity-bound semantic artifact produced by an explicit build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSemanticArtifact {
    repository: RepositoryId,
    generation: GenerationId,
    encoded: Vec<u8>,
    accounting: SemanticAccounting,
}

impl BuiltSemanticArtifact {
    /// Returns the repository bound to the artifact.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable structural generation bound to the artifact.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the canonical source-free artifact bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.encoded
    }

    /// Consumes the wrapper and returns the canonical artifact bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.encoded
    }

    /// Returns build-time resource accounting.
    #[must_use]
    pub const fn accounting(&self) -> SemanticAccounting {
        self.accounting
    }
}

/// One model-local cosine similarity result.
///
/// `score` is comparable only with results from the same exact `model_id` and
/// `model_hash`. It is retrieval similarity, never structural relation
/// confidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SemanticMatch {
    /// Stable caller-owned item identifier.
    pub item_id: String,
    /// Cosine similarity for this query and model.
    pub score: f64,
    /// Caller-declared local model identifier.
    pub model_id: String,
    /// Hash of the exact local model build.
    pub model_hash: ContentHash,
    /// Caller-declared chunk-policy version.
    pub chunk_policy_version: String,
}

/// Deterministically ordered semantic retrieval results.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticQueryResponse {
    repository: RepositoryId,
    generation: GenerationId,
    matches: Vec<SemanticMatch>,
}

impl SemanticQueryResponse {
    /// Returns the repository pinned by this result.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable structural generation pinned by this result.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns matches ordered by descending cosine similarity and then item ID.
    #[must_use]
    pub fn matches(&self) -> &[SemanticMatch] {
        &self.matches
    }

    /// Consumes the response and returns its matches.
    #[must_use]
    pub fn into_matches(self) -> Vec<SemanticMatch> {
        self.matches
    }
}

/// Typed, source-free semantic boundary failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SemanticError {
    /// A configured limit is zero or broader than the implementation hard cap.
    #[error("semantic limits are invalid")]
    InvalidLimits,
    /// A caller-controlled stable identifier is malformed.
    #[error("semantic identifier is invalid")]
    InvalidIdentifier,
    /// A vector is empty, non-finite, zero, or dimensionally invalid.
    #[error("semantic vector is invalid")]
    InvalidVector,
    /// Caller payload bytes exceed their independent ceiling.
    #[error("semantic input byte limit exceeded")]
    InputLimitExceeded,
    /// The number of items exceeds its independent ceiling.
    #[error("semantic item limit exceeded")]
    ItemLimitExceeded,
    /// Vector dimensions exceed the configured ceiling or disagree.
    #[error("semantic dimension mismatch")]
    DimensionMismatch,
    /// Requested result count exceeds its independent ceiling.
    #[error("semantic result limit exceeded")]
    ResultLimitExceeded,
    /// Retained semantic payload accounting exceeds its ceiling.
    #[error("semantic memory limit exceeded")]
    MemoryLimitExceeded,
    /// Canonical encoded artifact bytes exceed their ceiling.
    #[error("semantic artifact byte limit exceeded")]
    DiskLimitExceeded,
    /// Two caller-approved items have the same stable identifier.
    #[error("semantic item identifier is duplicated")]
    DuplicateItem,
    /// Artifact and query repositories differ.
    #[error("semantic repository binding mismatch")]
    RepositoryMismatch,
    /// Artifact and query structural generations differ.
    #[error("semantic generation binding mismatch")]
    GenerationMismatch,
    /// Artifact and query model identity or hash differ.
    #[error("semantic model binding mismatch")]
    ModelMismatch,
    /// Artifact and query chunk-policy versions differ.
    #[error("semantic chunk policy binding mismatch")]
    ChunkPolicyMismatch,
    /// Artifact bytes are not valid for the supported schema.
    #[error("semantic artifact is malformed")]
    MalformedArtifact,
    /// Artifact bytes use a valid but non-canonical representation.
    #[error("semantic artifact is not canonical")]
    NonCanonicalArtifact,
    /// Artifact payload integrity does not match its checksum.
    #[error("semantic artifact integrity check failed")]
    IntegrityMismatch,
    /// Work was cancelled before it could complete.
    #[error("semantic operation was cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactEnvelope {
    schema: String,
    payload: ArtifactPayload,
    checksum: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactPayload {
    repository: RepositoryId,
    generation: GenerationId,
    model_id: String,
    model_hash: ContentHash,
    chunk_policy_version: String,
    dimensions: usize,
    items: Vec<ArtifactItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactItem {
    item_id: String,
    content_hash: ContentHash,
    vector: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
struct SemanticIndex {
    context: SemanticContext,
    items: Vec<SemanticItem>,
    dimensions: usize,
    accounting: SemanticAccounting,
}

/// Builds a canonical semantic artifact from explicitly approved local vectors.
///
/// No model runtime, filesystem, network, or persistence capability is used.
///
/// # Errors
///
/// Returns a [`SemanticError`] when identifiers, vectors, dimensions, resource
/// accounting, serialization bounds, or cancellation violate the contract.
pub fn build_artifact(
    context: SemanticContext,
    mut items: Vec<SemanticItem>,
    limits: SemanticLimits,
    cancellation: &Cancellation,
) -> Result<BuiltSemanticArtifact, SemanticError> {
    cancellation.check().map_err(|_| SemanticError::Cancelled)?;
    if items.is_empty() || items.len() > limits.max_items {
        return Err(SemanticError::ItemLimitExceeded);
    }

    items.sort_by(|left, right| left.item_id.cmp(&right.item_id));
    let dimensions = items
        .first()
        .map(|item| item.vector.len())
        .ok_or(SemanticError::ItemLimitExceeded)?;
    if dimensions > limits.max_dimensions {
        return Err(SemanticError::DimensionMismatch);
    }

    let mut seen = BTreeSet::new();
    let mut input_bytes = accounted_context_input_bytes(&context)?;
    if input_bytes > limits.max_input_bytes {
        return Err(SemanticError::InputLimitExceeded);
    }
    for (index, item) in items.iter().enumerate() {
        if index % CANCELLATION_INTERVAL == 0 {
            cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        }
        if item.vector.len() != dimensions {
            return Err(SemanticError::DimensionMismatch);
        }
        validate_vector(&item.vector, limits.max_dimensions, cancellation)?;
        if !seen.insert(item.item_id.as_str()) {
            return Err(SemanticError::DuplicateItem);
        }
        input_bytes = input_bytes
            .checked_add(accounted_item_input_bytes(item)?)
            .ok_or(SemanticError::InputLimitExceeded)?;
        if input_bytes > limits.max_input_bytes {
            return Err(SemanticError::InputLimitExceeded);
        }
    }

    let memory_bytes = accounted_memory_bytes(input_bytes, items.len())?;
    if memory_bytes > limits.max_memory_bytes {
        return Err(SemanticError::MemoryLimitExceeded);
    }
    let repository = context.repository;
    let generation = context.generation;
    let payload = ArtifactPayload {
        repository: context.repository,
        generation: context.generation,
        model_id: context.model_id,
        model_hash: context.model_hash,
        chunk_policy_version: context.chunk_policy_version,
        dimensions,
        items: items
            .into_iter()
            .map(|item| ArtifactItem {
                item_id: item.item_id,
                content_hash: item.content_hash,
                vector: item.vector,
            })
            .collect(),
    };
    cancellation.check().map_err(|_| SemanticError::Cancelled)?;
    let payload_bytes =
        serde_json::to_vec(&payload).map_err(|_| SemanticError::MalformedArtifact)?;
    let envelope = ArtifactEnvelope {
        schema: SEMANTIC_ARTIFACT_SCHEMA.to_owned(),
        payload,
        checksum: content_hash(&payload_bytes),
    };
    let encoded = serde_json::to_vec(&envelope).map_err(|_| SemanticError::MalformedArtifact)?;
    if encoded.len() > limits.max_disk_bytes {
        return Err(SemanticError::DiskLimitExceeded);
    }
    let accounting = SemanticAccounting {
        input_bytes,
        memory_bytes,
        disk_bytes: encoded.len(),
        items: envelope.payload.items.len(),
        dimensions,
    };
    Ok(BuiltSemanticArtifact {
        repository,
        generation,
        encoded,
        accounting,
    })
}

/// Queries a canonical semantic artifact with an exact generation/model binding.
///
/// Scores are deterministic cosine similarities. They are meaningful only within
/// the exact model binding carried by each [`SemanticMatch`] and must not be used
/// as structural relation confidence.
///
/// # Errors
///
/// Returns a [`SemanticError`] for malformed or corrupt artifacts, context or
/// dimension mismatches, resource-limit violations, invalid vectors, or
/// cancellation.
pub fn query_artifact(
    encoded: &[u8],
    query: &SemanticQuery,
    limits: SemanticLimits,
    cancellation: &Cancellation,
) -> Result<SemanticQueryResponse, SemanticError> {
    let index = decode_artifact(encoded, limits, cancellation)?;
    validate_query_binding(&index, query, limits, cancellation)?;

    let query_norm = squared_norm(&query.vector, cancellation)?;
    let mut matches = Vec::with_capacity(query.max_results);
    for (index_position, item) in index.items.iter().enumerate() {
        if index_position % CANCELLATION_INTERVAL == 0 {
            cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        }
        let item_norm = squared_norm(&item.vector, cancellation)?;
        let dot = dot_product(&item.vector, &query.vector, cancellation)?;
        let denominator = (item_norm * query_norm).sqrt();
        let score = (dot / denominator).clamp(-1.0, 1.0);
        if !score.is_finite() {
            return Err(SemanticError::InvalidVector);
        }
        let candidate = SemanticMatch {
            item_id: item.item_id.clone(),
            score,
            model_id: index.context.model_id.clone(),
            model_hash: index.context.model_hash,
            chunk_policy_version: index.context.chunk_policy_version.clone(),
        };
        retain_best_match(&mut matches, candidate, query.max_results);
    }
    matches.sort_by(match_order);
    Ok(SemanticQueryResponse {
        repository: index.context.repository,
        generation: index.context.generation,
        matches,
    })
}

/// Verifies and accounts a canonical artifact without querying it.
///
/// This function has no side effects; callers remain responsible for the
/// lifecycle and removal of the returned byte container.
///
/// # Errors
///
/// Returns a [`SemanticError`] for malformed, non-canonical, corrupt, oversized,
/// or cancelled input.
pub fn verify_artifact(
    encoded: &[u8],
    limits: SemanticLimits,
    cancellation: &Cancellation,
) -> Result<SemanticAccounting, SemanticError> {
    decode_artifact(encoded, limits, cancellation).map(|index| index.accounting)
}

fn decode_artifact(
    encoded: &[u8],
    limits: SemanticLimits,
    cancellation: &Cancellation,
) -> Result<SemanticIndex, SemanticError> {
    cancellation.check().map_err(|_| SemanticError::Cancelled)?;
    if encoded.len() > limits.max_disk_bytes {
        return Err(SemanticError::DiskLimitExceeded);
    }
    if encoded.is_empty() {
        return Err(SemanticError::MalformedArtifact);
    }
    let envelope: ArtifactEnvelope =
        serde_json::from_slice(encoded).map_err(|_| SemanticError::MalformedArtifact)?;
    if envelope.schema != SEMANTIC_ARTIFACT_SCHEMA {
        return Err(SemanticError::MalformedArtifact);
    }
    let canonical = serde_json::to_vec(&envelope).map_err(|_| SemanticError::MalformedArtifact)?;
    if canonical != encoded {
        return Err(SemanticError::NonCanonicalArtifact);
    }
    let payload_bytes =
        serde_json::to_vec(&envelope.payload).map_err(|_| SemanticError::MalformedArtifact)?;
    if content_hash(&payload_bytes) != envelope.checksum {
        return Err(SemanticError::IntegrityMismatch);
    }

    let context = SemanticContext::new(
        envelope.payload.repository,
        envelope.payload.generation,
        envelope.payload.model_id,
        envelope.payload.model_hash,
        envelope.payload.chunk_policy_version,
    )?;
    if envelope.payload.items.is_empty() || envelope.payload.items.len() > limits.max_items {
        return Err(SemanticError::ItemLimitExceeded);
    }
    if envelope.payload.dimensions == 0 || envelope.payload.dimensions > limits.max_dimensions {
        return Err(SemanticError::DimensionMismatch);
    }

    let mut items = Vec::with_capacity(envelope.payload.items.len());
    let mut previous_item_id: Option<String> = None;
    let mut input_bytes = accounted_context_input_bytes(&context)?;
    if input_bytes > limits.max_input_bytes {
        return Err(SemanticError::InputLimitExceeded);
    }
    for (index, item) in envelope.payload.items.into_iter().enumerate() {
        if index % CANCELLATION_INTERVAL == 0 {
            cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        }
        let item = SemanticItem::new_with_cancellation(
            item.item_id,
            item.content_hash,
            item.vector,
            limits.max_dimensions,
            cancellation,
        )?;
        if previous_item_id
            .as_deref()
            .is_some_and(|previous| previous >= item.item_id.as_str())
        {
            return Err(SemanticError::NonCanonicalArtifact);
        }
        if item.vector.len() != envelope.payload.dimensions {
            return Err(SemanticError::DimensionMismatch);
        }
        input_bytes = input_bytes
            .checked_add(accounted_item_input_bytes(&item)?)
            .ok_or(SemanticError::InputLimitExceeded)?;
        if input_bytes > limits.max_input_bytes {
            return Err(SemanticError::InputLimitExceeded);
        }
        previous_item_id = Some(item.item_id.clone());
        items.push(item);
    }
    let memory_bytes = accounted_memory_bytes(input_bytes, items.len())?;
    if memory_bytes > limits.max_memory_bytes {
        return Err(SemanticError::MemoryLimitExceeded);
    }
    Ok(SemanticIndex {
        context,
        dimensions: envelope.payload.dimensions,
        accounting: SemanticAccounting {
            input_bytes,
            memory_bytes,
            disk_bytes: encoded.len(),
            items: items.len(),
            dimensions: envelope.payload.dimensions,
        },
        items,
    })
}

fn validate_query_binding(
    index: &SemanticIndex,
    query: &SemanticQuery,
    limits: SemanticLimits,
    cancellation: &Cancellation,
) -> Result<(), SemanticError> {
    if query.max_results > limits.max_results {
        return Err(SemanticError::ResultLimitExceeded);
    }
    let query_input_bytes = accounted_context_input_bytes(&query.context)?
        .checked_add(
            query
                .vector
                .len()
                .checked_mul(size_of::<f32>())
                .ok_or(SemanticError::InputLimitExceeded)?,
        )
        .ok_or(SemanticError::InputLimitExceeded)?;
    if query_input_bytes > limits.max_input_bytes {
        return Err(SemanticError::InputLimitExceeded);
    }
    validate_vector(&query.vector, limits.max_dimensions, cancellation)?;
    if query.vector.len() != index.dimensions {
        return Err(SemanticError::DimensionMismatch);
    }
    if query.context.repository != index.context.repository {
        return Err(SemanticError::RepositoryMismatch);
    }
    if query.context.generation != index.context.generation {
        return Err(SemanticError::GenerationMismatch);
    }
    if query.context.model_id != index.context.model_id
        || query.context.model_hash != index.context.model_hash
    {
        return Err(SemanticError::ModelMismatch);
    }
    if query.context.chunk_policy_version != index.context.chunk_policy_version {
        return Err(SemanticError::ChunkPolicyMismatch);
    }
    Ok(())
}

fn validate_vector(
    vector: &[f32],
    max_dimensions: usize,
    cancellation: &Cancellation,
) -> Result<(), SemanticError> {
    if vector.is_empty() || vector.len() > max_dimensions {
        return Err(SemanticError::InvalidVector);
    }
    let norm = squared_norm(vector, cancellation)?;
    if norm == 0.0 || !norm.is_finite() {
        return Err(SemanticError::InvalidVector);
    }
    Ok(())
}

fn squared_norm(vector: &[f32], cancellation: &Cancellation) -> Result<f64, SemanticError> {
    let mut norm = 0.0_f64;
    for (index, value) in vector.iter().copied().enumerate() {
        if index % CANCELLATION_INTERVAL == 0 {
            cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        }
        if !value.is_finite() {
            return Err(SemanticError::InvalidVector);
        }
        let value = f64::from(value);
        norm += value * value;
        if !norm.is_finite() {
            return Err(SemanticError::InvalidVector);
        }
    }
    Ok(norm)
}

fn dot_product(
    left: &[f32],
    right: &[f32],
    cancellation: &Cancellation,
) -> Result<f64, SemanticError> {
    if left.len() != right.len() {
        return Err(SemanticError::DimensionMismatch);
    }
    let mut dot = 0.0_f64;
    for (index, (left, right)) in left.iter().zip(right).enumerate() {
        if index % CANCELLATION_INTERVAL == 0 {
            cancellation.check().map_err(|_| SemanticError::Cancelled)?;
        }
        dot += f64::from(*left) * f64::from(*right);
        if !dot.is_finite() {
            return Err(SemanticError::InvalidVector);
        }
    }
    Ok(dot)
}

fn retain_best_match(
    matches: &mut Vec<SemanticMatch>,
    candidate: SemanticMatch,
    max_results: usize,
) {
    if matches.len() < max_results {
        matches.push(candidate);
        return;
    }
    let Some((worst_index, worst)) = matches
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| match_order(left, right))
    else {
        return;
    };
    if match_order(&candidate, worst) == Ordering::Less {
        matches[worst_index] = candidate;
    }
}

fn match_order(left: &SemanticMatch, right: &SemanticMatch) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.item_id.cmp(&right.item_id))
}

fn accounted_item_input_bytes(item: &SemanticItem) -> Result<usize, SemanticError> {
    item.vector
        .len()
        .checked_mul(size_of::<f32>())
        .and_then(|vector_bytes| vector_bytes.checked_add(item.item_id.len()))
        .and_then(|bytes| bytes.checked_add(item.content_hash.as_bytes().len()))
        .ok_or(SemanticError::InputLimitExceeded)
}

fn accounted_context_input_bytes(context: &SemanticContext) -> Result<usize, SemanticError> {
    context
        .repository
        .as_bytes()
        .len()
        .checked_add(context.generation.as_bytes().len())
        .and_then(|bytes| bytes.checked_add(context.model_id.len()))
        .and_then(|bytes| bytes.checked_add(context.model_hash.as_bytes().len()))
        .and_then(|bytes| bytes.checked_add(context.chunk_policy_version.len()))
        .ok_or(SemanticError::InputLimitExceeded)
}

fn accounted_memory_bytes(input_bytes: usize, items: usize) -> Result<usize, SemanticError> {
    items
        .checked_mul(ACCOUNTED_ITEM_OVERHEAD)
        .and_then(|overhead| overhead.checked_add(input_bytes))
        .ok_or(SemanticError::MemoryLimitExceeded)
}

fn validate_token(value: &str, max_bytes: usize) -> Result<(), SemanticError> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(SemanticError::InvalidIdentifier);
    }
    Ok(())
}

fn require_limit(value: usize, hard_max: usize) -> Result<(), SemanticError> {
    if value == 0 || value > hard_max {
        return Err(SemanticError::InvalidLimits);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::{ContentHash, GenerationId, RepositoryId, content_hash};

    use super::{
        ArtifactEnvelope, SemanticContext, SemanticError, SemanticItem, SemanticLimits,
        SemanticQuery, build_artifact, query_artifact, verify_artifact,
    };

    fn context() -> SemanticContext {
        SemanticContext::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            "local-model-v1".to_owned(),
            ContentHash::from_bytes([3; 32]),
            "chunk-policy-v1".to_owned(),
        )
        .expect("fixture context is valid")
    }

    fn item(id: &str, vector: &[f32]) -> SemanticItem {
        SemanticItem::new(
            id.to_owned(),
            ContentHash::from_bytes([id.as_bytes()[0]; 32]),
            vector.to_vec(),
        )
        .expect("fixture item is valid")
    }

    #[test]
    fn artifact_is_absent_until_explicit_build() {
        let approved = vec![item("item-a", &[1.0, 0.0])];
        assert_eq!(approved.len(), 1);

        let artifact = build_artifact(
            context(),
            approved,
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("explicit build succeeds");

        assert!(!artifact.as_bytes().is_empty());
    }

    #[test]
    fn cosine_ranking_and_ties_are_deterministic() {
        let artifact = build_artifact(
            context(),
            vec![
                item("item-c", &[0.0, 1.0]),
                item("item-b", &[1.0, 0.0]),
                item("item-a", &[1.0, 0.0]),
            ],
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("artifact builds");
        let query =
            SemanticQuery::new(context(), vec![1.0, 0.0], 2).expect("fixture query is valid");

        let first = query_artifact(
            artifact.as_bytes(),
            &query,
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("query succeeds");
        let second = query_artifact(
            artifact.as_bytes(),
            &query,
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("query is repeatable");

        assert_eq!(first, second);
        assert_eq!(
            first
                .matches()
                .iter()
                .map(|entry| entry.item_id.as_str())
                .collect::<Vec<_>>(),
            ["item-a", "item-b"]
        );
        assert_eq!(first.matches()[0].score, 1.0);
        assert_eq!(first.matches()[0].model_id, "local-model-v1");
        assert_eq!(
            first.matches()[0].model_hash,
            ContentHash::from_bytes([3; 32])
        );
        assert_eq!(first.matches()[0].chunk_policy_version, "chunk-policy-v1");
    }

    #[test]
    fn generation_and_model_mismatches_fail_closed() {
        let artifact = build_artifact(
            context(),
            vec![item("item-a", &[1.0, 0.0])],
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("artifact builds");
        let generation_context = SemanticContext::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([9; 20]),
            "local-model-v1".to_owned(),
            ContentHash::from_bytes([3; 32]),
            "chunk-policy-v1".to_owned(),
        )
        .expect("fixture context is valid");
        let generation_query =
            SemanticQuery::new(generation_context, vec![1.0, 0.0], 1).expect("query is valid");
        assert_eq!(
            query_artifact(
                artifact.as_bytes(),
                &generation_query,
                SemanticLimits::default(),
                &Cancellation::new(),
            ),
            Err(SemanticError::GenerationMismatch)
        );

        let model_context = SemanticContext::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            "local-model-v2".to_owned(),
            ContentHash::from_bytes([4; 32]),
            "chunk-policy-v1".to_owned(),
        )
        .expect("fixture context is valid");
        let model_query =
            SemanticQuery::new(model_context, vec![1.0, 0.0], 1).expect("query is valid");
        assert_eq!(
            query_artifact(
                artifact.as_bytes(),
                &model_query,
                SemanticLimits::default(),
                &Cancellation::new(),
            ),
            Err(SemanticError::ModelMismatch)
        );
    }

    #[test]
    fn corruption_is_detected_and_removal_does_not_touch_structural_identity() {
        let repository = context().repository();
        let generation = context().generation();
        let artifact = build_artifact(
            context(),
            vec![item("item-a", &[1.0, 0.0])],
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("artifact builds");
        let mut corrupt = artifact.into_bytes();
        let position = corrupt
            .windows(b"item-a".len())
            .position(|window| window == b"item-a")
            .expect("fixture artifact contains the item ID")
            + b"item-".len();
        corrupt[position] = b'b';

        assert_eq!(
            verify_artifact(&corrupt, SemanticLimits::default(), &Cancellation::new()),
            Err(SemanticError::IntegrityMismatch)
        );
        drop(corrupt);
        assert_eq!(repository, RepositoryId::from_bytes([1; 16]));
        assert_eq!(generation, GenerationId::from_bytes([2; 20]));
    }

    #[test]
    fn reordered_items_are_rejected_even_with_a_matching_checksum() {
        let artifact = build_artifact(
            context(),
            vec![item("item-a", &[1.0, 0.0]), item("item-b", &[0.0, 1.0])],
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("artifact builds");
        let mut envelope: ArtifactEnvelope =
            serde_json::from_slice(artifact.as_bytes()).expect("artifact decodes");
        envelope.payload.items.swap(0, 1);
        let payload = serde_json::to_vec(&envelope.payload).expect("payload encodes");
        envelope.checksum = content_hash(&payload);
        let reordered = serde_json::to_vec(&envelope).expect("envelope encodes");

        assert_eq!(
            verify_artifact(&reordered, SemanticLimits::default(), &Cancellation::new()),
            Err(SemanticError::NonCanonicalArtifact)
        );
    }

    #[test]
    fn resource_limits_are_independent() {
        let approved = vec![item("item-a", &[1.0, 0.0]), item("item-b", &[0.0, 1.0])];
        let item_limited = SemanticLimits::default()
            .with_max_items(1)
            .expect("limit is valid");
        assert_eq!(
            build_artifact(
                context(),
                approved.clone(),
                item_limited,
                &Cancellation::new()
            ),
            Err(SemanticError::ItemLimitExceeded)
        );
        let dimension_limited = SemanticLimits::default()
            .with_max_dimensions(1)
            .expect("limit is valid");
        assert_eq!(
            build_artifact(
                context(),
                approved.clone(),
                dimension_limited,
                &Cancellation::new()
            ),
            Err(SemanticError::DimensionMismatch)
        );
        assert_eq!(
            build_artifact(
                context(),
                vec![item("item-a", &[1.0, 0.0]), item("item-b", &[1.0])],
                SemanticLimits::default(),
                &Cancellation::new()
            ),
            Err(SemanticError::DimensionMismatch)
        );
        let input_limited = SemanticLimits::default()
            .with_max_input_bytes(1)
            .expect("limit is valid");
        assert_eq!(
            build_artifact(
                context(),
                approved.clone(),
                input_limited,
                &Cancellation::new()
            ),
            Err(SemanticError::InputLimitExceeded)
        );
        let memory_limited = SemanticLimits::default()
            .with_max_memory_bytes(1)
            .expect("limit is valid");
        assert_eq!(
            build_artifact(
                context(),
                approved.clone(),
                memory_limited,
                &Cancellation::new()
            ),
            Err(SemanticError::MemoryLimitExceeded)
        );
        let disk_limited = SemanticLimits::default()
            .with_max_disk_bytes(1)
            .expect("limit is valid");
        assert_eq!(
            build_artifact(
                context(),
                approved.clone(),
                disk_limited,
                &Cancellation::new()
            ),
            Err(SemanticError::DiskLimitExceeded)
        );

        let artifact = build_artifact(
            context(),
            approved,
            SemanticLimits::default(),
            &Cancellation::new(),
        )
        .expect("artifact builds");
        let query = SemanticQuery::new(context(), vec![1.0, 0.0], 2).expect("query is valid");
        let result_limited = SemanticLimits::default()
            .with_max_results(1)
            .expect("limit is valid");
        assert_eq!(
            query_artifact(
                artifact.as_bytes(),
                &query,
                result_limited,
                &Cancellation::new()
            ),
            Err(SemanticError::ResultLimitExceeded)
        );
    }

    #[test]
    fn cancellation_and_invalid_vectors_are_rejected() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            build_artifact(
                context(),
                vec![item("item-a", &[1.0, 0.0])],
                SemanticLimits::default(),
                &cancellation,
            ),
            Err(SemanticError::Cancelled)
        );

        for vector in [
            Vec::new(),
            vec![0.0, 0.0],
            vec![f32::NAN, 1.0],
            vec![f32::INFINITY, 1.0],
        ] {
            assert_eq!(
                SemanticItem::new(
                    "item-a".to_owned(),
                    ContentHash::from_bytes([1; 32]),
                    vector,
                ),
                Err(SemanticError::InvalidVector)
            );
        }
    }
}
