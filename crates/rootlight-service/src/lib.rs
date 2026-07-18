//! Transport-independent first-slice indexing and query use cases.
//!
//! This crate composes existing bounded domain contracts. It does not parse
//! CLI, IPC, or MCP requests and does not own durable generation publication.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseProvider, StreamLimits,
    execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
pub use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::{CatalogError, CatalogErrorKind, EphemeralOracleWriter};
use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};
use rootlight_discovery::{
    DiscoveryError, DiscoveryLimits, DiscoveryPolicy, InputClass, LanguageEvidence, ManifestInput,
    discover,
};
use rootlight_ids::{
    ContentHash, FileId, GenerationId, GenerationIdentity, RepositoryId, SymbolId, content_hash,
    derive_generation, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, FileIdentityClaim, IrLimits,
    NormalizedIrDocument, ProducerIdentity, SourceRef, SourceSpan,
};
pub use rootlight_query::{
    CodeLocateResult, LocateMode, QueryResponse, SourceReadQueryResult, SymbolExplainResult,
};
use rootlight_query::{GenerationSet, QueryBudget, QueryError, project_lexical_documents};
use rootlight_search::{BuildBudget, LexicalIndex, SearchBudget, SearchError};
use rootlight_source::{SourceBudget, SourceError, SourceReadOptions, SourceService};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationControlError,
    GenerationManifestRecipe, GenerationMetadata, IdentityVerificationError,
    IdentityVerifiedGeneration,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot, VfsError};
use serde::Serialize;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_RETAINED_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SYNTAX_NODES: usize = 16_384;
const MAX_SYNTAX_DEPTH: usize = 128;
const MAX_REPOSITORY_PATH_IDENTITY_BYTES: usize = 64 * 1024;
const MAX_RANDOM_ID_ATTEMPTS: usize = 8;
const PROVIDER_SET_SEED: &[u8] = b"rootlight.first-slice.providers/1";
const BUILD_CONTEXT_SEED: &[u8] = b"rootlight.first-slice.build-context/1";
const ANALYZER_BINARY_SEED: &[u8] = b"rootlight.first-slice.treesitter-rust/1";

/// Bounded receipt for one ephemeral first-slice generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FirstSliceIndexReceipt {
    /// Random local-UUID identity stable for aliases in this service process.
    ///
    /// The canonical-root digest is only an internal lookup key, not this
    /// public identity. The UUID is not durable across process restarts.
    pub repository: RepositoryId,
    /// Immutable generation published into this service instance.
    pub generation: GenerationId,
    /// Prior generation in the same repository lineage, when present.
    pub parent: Option<GenerationId>,
    /// Regular inputs admitted by deterministic discovery.
    pub discovered_inputs: u64,
    /// Files committed into normalized IR.
    pub indexed_files: u64,
    /// Semantic entities committed into normalized IR.
    pub entities: u64,
    /// Lexical documents committed into the generation-pinned reader.
    pub lexical_documents: u64,
    /// SQLite pages allocated by the normalized in-memory oracle.
    pub oracle_allocated_bytes: u64,
    /// End-to-end indexing time rounded up to microseconds.
    pub elapsed_micros: u64,
}

/// Checked repository and generation correlation for one first-slice query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceGenerationContext {
    /// Repository owning the immutable generation.
    pub repository: RepositoryId,
    /// Selected immutable generation.
    pub generation: GenerationId,
    /// Optional predecessor generation.
    pub parent: Option<GenerationId>,
    /// Whether this generation is currently active for its repository.
    pub active: bool,
    /// Publication receipt retained with the generation.
    pub receipt: FirstSliceIndexReceipt,
}

/// Two-phase index result awaiting an explicit publication decision.
///
/// The prepared variant remains inline because adding a `Box` here would
/// introduce an infallible allocation after the pipeline's fallible admission
/// checks.
#[allow(clippy::large_enum_variant)]
pub enum FirstSliceIndexPreparation {
    /// An identical retained generation only needs reactivation.
    Retained(FirstSliceIndexReceipt),
    /// Newly built immutable state that has not entered the queryable set.
    Pending(PreparedFirstSliceIndex),
}

impl FirstSliceIndexPreparation {
    /// Returns the receipt that publication would make active.
    #[must_use]
    pub const fn receipt(&self) -> FirstSliceIndexReceipt {
        match self {
            Self::Retained(receipt) => *receipt,
            Self::Pending(prepared) => prepared.receipt,
        }
    }
}

/// Fully verified first-slice state that is not yet queryable.
pub struct PreparedFirstSliceIndex {
    verified: IdentityVerifiedGeneration,
    search: LexicalIndex,
    sources: Vec<RustSourceInput>,
    receipt: FirstSliceIndexReceipt,
    root_identity: ContentHash,
    register_repository: bool,
}

/// Retention-admitted generation awaiting durable lifecycle success.
///
/// Newly built state is reserved inside the bounded generation set but remains
/// invisible to every query path until this token is committed.
pub struct FirstSliceStagedIndex {
    receipt: FirstSliceIndexReceipt,
    publication: FirstSlicePublication,
}

impl FirstSliceStagedIndex {
    /// Returns the still-hidden generation receipt.
    #[must_use]
    pub const fn receipt(&self) -> FirstSliceIndexReceipt {
        self.receipt
    }
}

enum FirstSlicePublication {
    Retained,
    Pending {
        root_identity: ContentHash,
        register_repository: bool,
    },
}

struct RustSourceInput {
    snapshot: SourceSnapshot,
    generated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SourceSnapshotIdentity {
    file: FileId,
    content_hash: ContentHash,
}

impl From<&SourceSnapshot> for SourceSnapshotIdentity {
    fn from(snapshot: &SourceSnapshot) -> Self {
        Self {
            file: snapshot.file(),
            content_hash: snapshot.content_hash(),
        }
    }
}

struct SharedSourceSnapshot {
    snapshot: Arc<SourceSnapshot>,
    generation_references: usize,
}

struct SourceSnapshotAdmission {
    generation: GenerationId,
    retained: Vec<Arc<SourceSnapshot>>,
    additional_bytes: usize,
}

struct SourceSnapshotRelease {
    generation: GenerationId,
    snapshots: Vec<Arc<SourceSnapshot>>,
    updates: Vec<SourceSnapshotReleaseUpdate>,
    retained_bytes_after: usize,
}

enum SourceSnapshotReleaseUpdate {
    Retain {
        identity: SourceSnapshotIdentity,
        snapshot: Arc<SourceSnapshot>,
        generation_references: usize,
    },
    Remove(SourceSnapshotIdentity),
}

/// Bounded process-local source fallback mirroring generation publication.
///
/// Source bodies remain outside the core index and are never persisted. The
/// byte ceiling and content-identity sharing keep this Gate-1 fallback bounded
/// until the later durable source-retention architecture is authorized.
struct SourceSnapshotRetention {
    maximum_generations: usize,
    maximum_bytes: usize,
    retained_bytes: usize,
    shared: BTreeMap<SourceSnapshotIdentity, SharedSourceSnapshot>,
    committed: BTreeMap<GenerationId, Vec<Arc<SourceSnapshot>>>,
    staged: BTreeMap<GenerationId, Vec<Arc<SourceSnapshot>>>,
}

impl SourceSnapshotRetention {
    fn new(maximum_generations: usize, maximum_bytes: usize) -> Result<Self, FirstSliceError> {
        if maximum_generations == 0 || maximum_bytes == 0 {
            return Err(FirstSliceError::Retention);
        }
        Ok(Self {
            maximum_generations,
            maximum_bytes,
            retained_bytes: 0,
            shared: BTreeMap::new(),
            committed: BTreeMap::new(),
            staged: BTreeMap::new(),
        })
    }

    fn admit(
        &self,
        generation: GenerationId,
        mut sources: Vec<RustSourceInput>,
        cancellation: &Cancellation,
    ) -> Result<SourceSnapshotAdmission, FirstSliceError> {
        check_cancellation(cancellation)?;
        if self.committed.contains_key(&generation) || self.staged.contains_key(&generation) {
            return Err(FirstSliceError::Retention);
        }
        let retained_generations = self
            .committed
            .len()
            .checked_add(self.staged.len())
            .ok_or(FirstSliceError::Retention)?;
        if retained_generations >= self.maximum_generations {
            return Err(FirstSliceError::Retention);
        }

        sources.sort_unstable_by_key(|source| SourceSnapshotIdentity::from(&source.snapshot));
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(sources.len())
            .map_err(|_| FirstSliceError::Retention)?;
        let mut previous_file = None;
        let mut additional_bytes = 0usize;
        for source in &sources {
            check_cancellation(cancellation)?;
            let identity = SourceSnapshotIdentity::from(&source.snapshot);
            if previous_file == Some(identity.file) {
                return Err(FirstSliceError::Retention);
            }
            previous_file = Some(identity.file);
            if !self.shared.contains_key(&identity) {
                additional_bytes = additional_bytes
                    .checked_add(source.snapshot.content().len())
                    .ok_or(FirstSliceError::Retention)?;
            }
        }
        let admitted_bytes = self
            .retained_bytes
            .checked_add(additional_bytes)
            .ok_or(FirstSliceError::Retention)?;
        if admitted_bytes > self.maximum_bytes {
            return Err(FirstSliceError::Retention);
        }
        check_cancellation(cancellation)?;

        for source in sources {
            check_cancellation(cancellation)?;
            let identity = SourceSnapshotIdentity::from(&source.snapshot);
            let snapshot = self
                .shared
                .get(&identity)
                .map(|shared| Arc::clone(&shared.snapshot))
                .unwrap_or_else(|| Arc::new(source.snapshot));
            retained.push(snapshot);
        }
        check_cancellation(cancellation)?;

        Ok(SourceSnapshotAdmission {
            generation,
            retained,
            additional_bytes,
        })
    }

    fn stage(&mut self, admission: SourceSnapshotAdmission) -> Result<(), FirstSliceError> {
        if self.committed.contains_key(&admission.generation)
            || self.staged.contains_key(&admission.generation)
        {
            return Err(FirstSliceError::Retention);
        }
        let retained_generations = self
            .committed
            .len()
            .checked_add(self.staged.len())
            .ok_or(FirstSliceError::Retention)?;
        if retained_generations >= self.maximum_generations {
            return Err(FirstSliceError::Retention);
        }
        let admitted_bytes = self
            .retained_bytes
            .checked_add(admission.additional_bytes)
            .ok_or(FirstSliceError::Retention)?;
        if admitted_bytes > self.maximum_bytes {
            return Err(FirstSliceError::Retention);
        }
        let mut reference_updates = Vec::new();
        reference_updates
            .try_reserve_exact(admission.retained.len())
            .map_err(|_| FirstSliceError::Retention)?;
        for snapshot in &admission.retained {
            let identity = SourceSnapshotIdentity::from(snapshot.as_ref());
            let generation_references = self.shared.get(&identity).map_or(Ok(1), |shared| {
                shared
                    .generation_references
                    .checked_add(1)
                    .ok_or(FirstSliceError::Retention)
            })?;
            reference_updates.push((identity, generation_references));
        }

        let SourceSnapshotAdmission {
            generation,
            retained,
            additional_bytes: _,
        } = admission;
        let Self {
            retained_bytes,
            shared,
            staged,
            ..
        } = self;
        let std::collections::btree_map::Entry::Vacant(staged_entry) = staged.entry(generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        for ((identity, generation_references), snapshot) in
            reference_updates.into_iter().zip(&retained)
        {
            match shared.entry(identity) {
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().generation_references = generation_references;
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(SharedSourceSnapshot {
                        snapshot: Arc::clone(snapshot),
                        generation_references,
                    });
                }
            }
        }
        staged_entry.insert(retained);
        *retained_bytes = admitted_bytes;
        Ok(())
    }

    fn commit_staged(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let Self {
            committed, staged, ..
        } = self;
        let std::collections::btree_map::Entry::Vacant(committed_entry) =
            committed.entry(generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        let snapshots = staged
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        committed_entry.insert(snapshots);
        Ok(())
    }

    fn rollback_commit(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let Self {
            committed, staged, ..
        } = self;
        let std::collections::btree_map::Entry::Vacant(staged_entry) = staged.entry(generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        let snapshots = committed
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        staged_entry.insert(snapshots);
        Ok(())
    }

    fn begin_discard(
        &mut self,
        generation: GenerationId,
    ) -> Result<SourceSnapshotRelease, FirstSliceError> {
        let snapshots = self
            .staged
            .get(&generation)
            .ok_or(FirstSliceError::Retention)?;
        let mut updates = Vec::new();
        updates
            .try_reserve_exact(snapshots.len())
            .map_err(|_| FirstSliceError::Retention)?;
        let mut released_bytes = 0usize;
        for snapshot in snapshots {
            let identity = SourceSnapshotIdentity::from(snapshot.as_ref());
            let shared = self
                .shared
                .get(&identity)
                .ok_or(FirstSliceError::Retention)?;
            if shared.generation_references == 0 {
                return Err(FirstSliceError::Retention);
            }
            if shared.generation_references == 1 {
                released_bytes = released_bytes
                    .checked_add(shared.snapshot.content().len())
                    .ok_or(FirstSliceError::Retention)?;
                updates.push(SourceSnapshotReleaseUpdate::Remove(identity));
            } else {
                let generation_references = shared
                    .generation_references
                    .checked_sub(1)
                    .ok_or(FirstSliceError::Retention)?;
                updates.push(SourceSnapshotReleaseUpdate::Retain {
                    identity,
                    snapshot: Arc::clone(&shared.snapshot),
                    generation_references,
                });
            }
        }
        let retained_bytes_after = self
            .retained_bytes
            .checked_sub(released_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let snapshots = self
            .staged
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        Ok(SourceSnapshotRelease {
            generation,
            snapshots,
            updates,
            retained_bytes_after,
        })
    }

    fn finish_discard(&mut self, release: SourceSnapshotRelease) {
        for update in release.updates {
            match update {
                SourceSnapshotReleaseUpdate::Retain {
                    identity,
                    snapshot,
                    generation_references,
                } => match self.shared.entry(identity) {
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        entry.get_mut().generation_references = generation_references;
                    }
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(SharedSourceSnapshot {
                            snapshot,
                            generation_references,
                        });
                    }
                },
                SourceSnapshotReleaseUpdate::Remove(identity) => {
                    self.shared.remove(&identity);
                }
            }
        }
        self.retained_bytes = release.retained_bytes_after;
    }

    fn rollback_discard(&mut self, release: SourceSnapshotRelease) -> Result<(), FirstSliceError> {
        let std::collections::btree_map::Entry::Vacant(staged_entry) =
            self.staged.entry(release.generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        staged_entry.insert(release.snapshots);
        Ok(())
    }

    fn snapshots(&self, generation: GenerationId) -> Option<&[Arc<SourceSnapshot>]> {
        self.committed.get(&generation).map(Vec::as_slice)
    }

    #[cfg(test)]
    const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[cfg(test)]
    fn staged_generations(&self) -> usize {
        self.staged.len()
    }
}

/// Transport-independent owner of bounded ephemeral fixture generations.
///
/// The service retains at most the caller-selected hard-bounded generation
/// count and 64 MiB of deduplicated source content bytes. SQLite, lexical, and
/// source state are process-local because ADR-026 has not authorized durable
/// private-file creation. Full crash recovery, leases, and filesystem
/// publication remain M12 work.
pub struct FirstSliceService {
    config: ConfigSnapshot,
    analysis_limits: AnalysisLimits,
    extensions: ExtensionSupport,
    analyzer: TreeSitterAnalyzer,
    // The canonical-root digest is only a process-local lookup key. The
    // nondurable fallback uses a random local UUID rather than path-derived
    // public identity; durable UUID persistence remains outside this service.
    repositories: BTreeMap<ContentHash, RepositoryId>,
    active_by_repository: BTreeMap<RepositoryId, GenerationId>,
    generations: GenerationSet<LexicalIndex>,
    source_snapshots: SourceSnapshotRetention,
    receipts: BTreeMap<GenerationId, FirstSliceIndexReceipt>,
}

impl FirstSliceService {
    /// Creates the bounded Rust first-slice service.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] when a required bounded parser, analyzer,
    /// configuration, generation-retention, or source-retention contract cannot
    /// initialize.
    pub fn new(maximum_generations: usize) -> Result<Self, FirstSliceError> {
        Self::new_with_source_limit(maximum_generations, MAX_RETAINED_SOURCE_BYTES)
    }

    fn new_with_source_limit(
        maximum_generations: usize,
        maximum_source_bytes: usize,
    ) -> Result<Self, FirstSliceError> {
        let config = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::Defaults,
            contents: "version = \"1.0\"",
        }])
        .map_err(|_| FirstSliceError::Configuration)?;
        let analysis_limits = analysis_limits()?;
        let parser = Arc::new(
            TreeSitterProvider::new(parser_config()?).map_err(|_| FirstSliceError::Adapter)?,
        );
        let parse_provider: Arc<dyn ParseProvider> = parser;
        let producer =
            ProducerIdentity::new("rootlight-first-slice-treesitter", "1.0", config.hash())
                .map_err(|_| FirstSliceError::Adapter)?;
        let language = LanguageId::new("rust").map_err(|_| FirstSliceError::Adapter)?;
        let analyzer = TreeSitterAnalyzer::new(
            parse_provider,
            producer,
            language,
            "tree-sitter-rust-0.24.2",
            content_hash(ANALYZER_BINARY_SEED),
        )
        .map_err(|_| FirstSliceError::Adapter)?;
        let generations =
            GenerationSet::new(maximum_generations).map_err(|_| FirstSliceError::Retention)?;
        let source_snapshots =
            SourceSnapshotRetention::new(maximum_generations, maximum_source_bytes)?;
        Ok(Self {
            config,
            analysis_limits,
            extensions: ExtensionSupport::default(),
            analyzer,
            repositories: BTreeMap::new(),
            active_by_repository: BTreeMap::new(),
            generations,
            source_snapshots,
            receipts: BTreeMap::new(),
        })
    }

    /// Discovers, parses, validates, round-trips, indexes, and publishes one
    /// Rust repository.
    ///
    /// Repeating an unchanged active fixture is idempotent. The caller must
    /// supply a monotonic deadline so every synchronous stage stays bounded.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an invalid fixture shape, missing
    /// deadline, cancellation, resource limit, identity drift, persistence,
    /// search, or retention failure.
    pub fn index_rust_fixture(
        &mut self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        let prepared = self.prepare_rust_fixture(path, cancellation)?;
        self.publish_prepared(prepared, cancellation)
    }

    /// Builds and verifies one fixture generation without making it queryable.
    ///
    /// This phase may perform all bounded discovery, parsing, normalization,
    /// oracle, and lexical work. Publication remains an explicit second step so
    /// the daemon can durably linearize lifecycle completion before activation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] under the same bounded validation,
    /// cancellation, identity, storage, and retention conditions as
    /// [`Self::index_rust_fixture`].
    pub fn prepare_rust_fixture(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        let started = Instant::now();
        require_deadline(cancellation)?;
        cancellation
            .check()
            .map_err(|cancelled| FirstSliceError::Cancelled(cancelled.reason()))?;
        let canonical = canonical_repository_root(path, cancellation)?;
        let root_identity = repository_path_hash(&canonical)?;
        let existing_repository = self.repositories.get(&root_identity).copied();
        let repository_result = match existing_repository {
            Some(repository) => repository,
            None => random_repository_id(&self.repositories)?,
        };
        check_cancellation(cancellation)?;
        let repository = repository_result;
        let root_result = RepositoryRoot::open(repository, &canonical);
        check_cancellation(cancellation)?;
        let root = root_result.map_err(|_| FirstSliceError::Repository)?;
        let policy =
            DiscoveryPolicy::build(Vec::new(), false).map_err(|_| FirstSliceError::Discovery)?;
        let manifest = discover(
            &root,
            &self.config,
            &policy,
            DiscoveryLimits::from_config(&self.config),
            cancellation,
        )
        .map_err(|error| map_discovery_error(error, cancellation))?;
        let rust_source_count = manifest
            .inputs
            .iter()
            .filter(|input| is_rust_source(input))
            .count();
        if rust_source_count == 0 {
            return Err(FirstSliceError::FixtureShape);
        }
        if rust_source_count > self.analysis_limits.ir().max_files {
            return Err(FirstSliceError::Limits);
        }
        let mut file_claims = Vec::new();
        file_claims
            .try_reserve_exact(rust_source_count)
            .map_err(|_| FirstSliceError::Limits)?;
        let mut rust_sources = Vec::new();
        rust_sources
            .try_reserve_exact(rust_source_count)
            .map_err(|_| FirstSliceError::Limits)?;
        let maximum_source_bytes = u64::try_from(self.analysis_limits.max_source_bytes())
            .map_err(|_| FirstSliceError::Limits)?;
        for input in manifest.inputs.iter().filter(|input| is_rust_source(input)) {
            check_cancellation(cancellation)?;
            let relative = RelativePath::parse(Path::new(&input.path))
                .map_err(|_| FirstSliceError::Repository)?;
            let snapshot = root
                .snapshot_with_cancellation(&relative, maximum_source_bytes, cancellation)
                .map_err(|error| map_vfs_error(error, cancellation))?;
            if snapshot.file() != input.file
                || snapshot.content_hash() != input.content_hash
                || u64::try_from(snapshot.content().len()).ok() != Some(input.bytes)
            {
                return Err(FirstSliceError::DiscoveryDrift);
            }
            file_claims.push(FileIdentityClaim {
                file: input.file,
                repository,
                path: fallible_copy_string(&input.path)?,
                path_identity: fallible_copy_bytes(relative.identity_bytes())?,
                content_hash: input.content_hash,
                byte_length: input.bytes,
            });
            rust_sources.push(RustSourceInput {
                snapshot,
                generated: matches!(input.class, InputClass::Generated),
            });
        }
        let manifest_hash =
            GenerationManifestRecipe::new(repository, self.config.hash(), file_claims)
                .map_err(|_| FirstSliceError::Identity)?
                .canonical_hash()
                .map_err(|_| FirstSliceError::Identity)?;
        let provider_set_hash = content_hash(PROVIDER_SET_SEED);
        let active = self.active_by_repository.get(&repository).copied();
        if let Some(active) = active
            && let Ok(snapshot) = self.generations.generation(active)
        {
            let metadata = snapshot.metadata();
            if metadata.repository() == repository
                && metadata.manifest_hash() == manifest_hash
                && metadata.configuration_hash() == self.config.hash()
                && metadata.provider_set_hash() == provider_set_hash
                && let Some(receipt) = self.receipts.get(&active).copied()
            {
                check_cancellation(cancellation)?;
                return Ok(FirstSliceIndexPreparation::Retained(receipt));
            }
        }
        let parent = active;
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent,
            manifest_hash,
            config_hash: self.config.hash(),
            provider_set_hash,
            format_version: generation_format_version(),
        })
        .id();
        if let Some(receipt) = self.receipts.get(&generation).copied() {
            check_cancellation(cancellation)?;
            return Ok(FirstSliceIndexPreparation::Retained(receipt));
        }
        let mut document = NormalizedIrDocument::empty(repository, generation);
        for input in &rust_sources {
            check_cancellation(cancellation)?;
            let snapshot = &input.snapshot;
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(snapshot.file(), 0, snapshot.metadata().length)
                    .map_err(|_| FirstSliceError::Identity)?,
                snapshot.content_hash(),
                None,
            );
            let request = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(snapshot, &source)
                    .map_err(|_| FirstSliceError::Adapter)?,
                LanguageId::new("rust").map_err(|_| FirstSliceError::Adapter)?,
                EncodingId::utf8(),
                Vec::new(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(BUILD_CONTEXT_SEED)),
                &self.analysis_limits,
            )
            .map_err(|_| FirstSliceError::Adapter)?
            .with_generated_status(input.generated);
            let output = execute_analysis(
                &self.analyzer,
                &request,
                self.extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
                cancellation,
            )
            .map_err(|error| map_adapter_error(error, cancellation))?;
            append_normalized_document(
                &mut document,
                output.document().clone(),
                self.analysis_limits.ir(),
            )?;
        }
        let metadata = GenerationMetadata::new(
            repository,
            generation,
            parent,
            manifest_hash,
            self.config.hash(),
            provider_set_hash,
        )
        .map_err(|_| FirstSliceError::Identity)?;
        let context = GenerationContext::new(cancellation, GenerationBudget::default());
        let verified = IdentityVerifiedGeneration::verify(
            metadata,
            document,
            self.analysis_limits.ir(),
            &self.extensions,
            &context,
        )
        .map_err(|error| map_identity_error(error, cancellation))?;
        let oracle = EphemeralOracleWriter::create()
            .map_err(|error| map_catalog_error(&error, cancellation))?
            .seal(verified, &context)
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        let oracle_allocated_bytes = oracle
            .allocated_bytes()
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        let persisted = oracle
            .read(&context)
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        let documents = project_lexical_documents(&persisted, BuildBudget::default(), cancellation)
            .map_err(|error| map_query_error(error, cancellation))?;
        let lexical_documents =
            u64::try_from(documents.len()).map_err(|_| FirstSliceError::Limits)?;
        let search = LexicalIndex::build_ephemeral(
            generation,
            documents,
            BuildBudget::default(),
            cancellation,
        )
        .map_err(|error| map_search_error(error, cancellation))?;
        let indexed_files =
            u64::try_from(persisted.document().files.len()).map_err(|_| FirstSliceError::Limits)?;
        let entities = u64::try_from(persisted.document().entities.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let verified = oracle
            .read_verified(&context)
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        let receipt = FirstSliceIndexReceipt {
            repository,
            generation,
            parent,
            discovered_inputs: manifest.coverage.included,
            indexed_files,
            entities,
            lexical_documents,
            oracle_allocated_bytes,
            elapsed_micros: elapsed_micros(started),
        };
        check_cancellation(cancellation)?;
        Ok(FirstSliceIndexPreparation::Pending(
            PreparedFirstSliceIndex {
                verified,
                search,
                sources: rust_sources,
                receipt,
                root_identity,
                register_repository: existing_repository.is_none(),
            },
        ))
    }

    /// Publishes or reactivates one prepared generation for standalone use.
    ///
    /// Its final token check is a process-local, nondurable cancellation
    /// linearization point. The daemon instead stages first, atomically records
    /// durable journal success, and then invokes [`Self::commit_staged`].
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Cancelled`] when cancellation was already
    /// established, or [`FirstSliceError::Retention`] when the bounded
    /// generation set cannot publish the prepared state.
    pub fn publish_prepared(
        &mut self,
        prepared: FirstSliceIndexPreparation,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        let staged = self.stage_prepared(prepared, cancellation)?;
        if let Err(error) = check_cancellation(cancellation) {
            self.discard_staged(staged)?;
            return Err(error);
        }
        self.commit_staged(staged)
    }

    /// Retention-admits prepared state without exposing it to queries.
    ///
    /// The daemon invokes this before its serialized durable publication
    /// completion. A cancellation that wins first can therefore discard the
    /// reservation without publishing partial state.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Cancelled`] when cancellation already won or
    /// [`FirstSliceError::Retention`] when bounded admission fails.
    pub fn stage_prepared(
        &mut self,
        prepared: FirstSliceIndexPreparation,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceStagedIndex, FirstSliceError> {
        check_cancellation(cancellation)?;
        match prepared {
            FirstSliceIndexPreparation::Retained(receipt) => Ok(FirstSliceStagedIndex {
                receipt,
                publication: FirstSlicePublication::Retained,
            }),
            FirstSliceIndexPreparation::Pending(prepared) => {
                let PreparedFirstSliceIndex {
                    verified,
                    search,
                    sources,
                    receipt,
                    root_identity,
                    register_repository,
                } = prepared;
                let source_admission =
                    self.source_snapshots
                        .admit(receipt.generation, sources, cancellation)?;
                self.generations
                    .stage(verified, search)
                    .map_err(|_| FirstSliceError::Retention)?;
                if let Err(error) = self.source_snapshots.stage(source_admission) {
                    self.generations
                        .discard_staged(receipt.generation)
                        .map_err(|_| FirstSliceError::Retention)?;
                    return Err(error);
                }
                Ok(FirstSliceStagedIndex {
                    receipt,
                    publication: FirstSlicePublication::Pending {
                        root_identity,
                        register_repository,
                    },
                })
            }
        }
    }

    /// Commits one correctly linearized staged generation.
    ///
    /// Daemon callers first durably terminalize the owning operation as
    /// succeeded. Standalone [`Self::publish_prepared`] callers instead use its
    /// final nondurable cancellation-token checkpoint. The staging token proves
    /// that capacity and generation/search correlation were already admitted.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Retention`] only when the staging token no
    /// longer matches this service instance.
    pub fn commit_staged(
        &mut self,
        staged: FirstSliceStagedIndex,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        let receipt = staged.receipt;
        match staged.publication {
            FirstSlicePublication::Retained => {
                self.generations
                    .activate(receipt.generation)
                    .map_err(|_| FirstSliceError::Retention)?;
            }
            FirstSlicePublication::Pending {
                root_identity,
                register_repository,
            } => {
                self.source_snapshots
                    .commit_staged(receipt.generation)
                    .map_err(|_| FirstSliceError::Retention)?;
                if self
                    .generations
                    .commit_staged(receipt.generation, true)
                    .is_err()
                {
                    self.source_snapshots
                        .rollback_commit(receipt.generation)
                        .map_err(|_| FirstSliceError::Retention)?;
                    return Err(FirstSliceError::Retention);
                }
                self.receipts.insert(receipt.generation, receipt);
                if register_repository {
                    self.repositories.insert(root_identity, receipt.repository);
                }
            }
        }
        self.active_by_repository
            .insert(receipt.repository, receipt.generation);
        Ok(receipt)
    }

    /// Releases one pre-terminal staging reservation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Retention`] when a newly built reservation
    /// was already consumed or does not belong to this service.
    pub fn discard_staged(&mut self, staged: FirstSliceStagedIndex) -> Result<(), FirstSliceError> {
        if matches!(staged.publication, FirstSlicePublication::Pending { .. }) {
            let source_release = self
                .source_snapshots
                .begin_discard(staged.receipt.generation)?;
            if self
                .generations
                .discard_staged(staged.receipt.generation)
                .is_err()
            {
                self.source_snapshots
                    .rollback_discard(source_release)
                    .map_err(|_| FirstSliceError::Retention)?;
                return Err(FirstSliceError::Retention);
            }
            self.source_snapshots.finish_discard(source_release);
        }
        Ok(())
    }

    /// Returns the most recently activated generation across all repositories.
    ///
    /// Callers that already know a repository should use
    /// [`Self::active_generation_for`] to avoid cross-repository ambiguity.
    #[must_use]
    pub const fn active_generation(&self) -> Option<GenerationId> {
        self.generations.active_generation()
    }

    /// Returns the active immutable generation for one repository.
    #[must_use]
    pub fn active_generation_for(&self, repository: RepositoryId) -> Option<GenerationId> {
        self.active_by_repository.get(&repository).copied()
    }

    /// Resolves and verifies one repository-owned immutable generation.
    ///
    /// Passing `None` selects the repository's active generation. Explicit
    /// generations remain queryable while retained, including superseded ones.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::RepositoryNotFound`] when the repository is
    /// unknown, [`FirstSliceError::GenerationNotFound`] when the generation is
    /// not retained, or [`FirstSliceError::GenerationMismatch`] when it belongs
    /// to another repository.
    pub fn resolve_generation(
        &self,
        repository: RepositoryId,
        generation: Option<GenerationId>,
    ) -> Result<FirstSliceGenerationContext, FirstSliceError> {
        let active = self
            .active_by_repository
            .get(&repository)
            .copied()
            .ok_or(FirstSliceError::RepositoryNotFound)?;
        let generation = generation.unwrap_or(active);
        let receipt = self
            .receipts
            .get(&generation)
            .copied()
            .ok_or(FirstSliceError::GenerationNotFound)?;
        if receipt.repository != repository {
            return Err(FirstSliceError::GenerationMismatch);
        }
        Ok(FirstSliceGenerationContext {
            repository,
            generation,
            parent: receipt.parent,
            active: generation == active,
            receipt,
        })
    }

    /// Executes a generation-pinned bounded `code.locate` query.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    pub fn code_locate(
        &self,
        generation: GenerationId,
        query: String,
        mode: LocateMode,
        maximum_results: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeLocateResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_code_locate(
                query,
                mode,
                maximum_results,
                SearchBudget::default(),
                QueryBudget::new(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_code_locate(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `symbol.explain` query.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, symbol, invalid
    /// plan, or bounded execution failure.
    pub fn symbol_explain(
        &self,
        generation: GenerationId,
        symbol: SymbolId,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SymbolExplainResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_symbol_explain(symbol, QueryBudget::new())
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_symbol_explain(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `source.read` query.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, stale source,
    /// invalid plan, or bounded execution failure.
    pub fn source_read(
        &self,
        generation: GenerationId,
        references: Vec<SourceRef>,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SourceReadQueryResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let snapshot = self
            .generations
            .generation(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let source_snapshots = self
            .source_snapshots
            .snapshots(generation)
            .ok_or(FirstSliceError::Query)?;
        let source = SourceService::from_snapshots(source_snapshots, snapshot)
            .map_err(|error| map_source_error(error, cancellation))?;
        let plan = service
            .plan_source_read(
                references,
                SourceReadOptions::new(),
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_source_read(&plan, &source, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }
}

impl std::fmt::Debug for FirstSliceService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FirstSliceService")
            .field("active_generation", &self.generations.active_generation())
            .field("retained_generations", &self.receipts.len())
            .finish()
    }
}

/// Stable source-redacted first-slice service failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FirstSliceError {
    /// Effective configuration could not initialize.
    #[error("first-slice configuration is invalid")]
    Configuration,
    /// The operating system could not create a local repository UUID.
    #[error("first-slice repository identity is unavailable")]
    RandomUnavailable,
    /// The caller omitted the required monotonic deadline.
    #[error("first-slice indexing requires a monotonic deadline")]
    DeadlineRequired,
    /// Cooperative cancellation or deadline stopped the operation.
    #[error("first-slice operation was cancelled: {0:?}")]
    Cancelled(CancellationReason),
    /// The repository capability could not be established safely.
    #[error("first-slice repository is unavailable")]
    Repository,
    /// The bounded fixture contains an unsupported number or kind of inputs.
    #[error("first-slice fixture shape is unsupported")]
    FixtureShape,
    /// Deterministic discovery failed.
    #[error("first-slice discovery failed")]
    Discovery,
    /// Source changed between discovery and capability snapshot.
    #[error("first-slice discovery snapshot changed")]
    DiscoveryDrift,
    /// Parser or normalized adapter output failed.
    #[error("first-slice analysis failed")]
    Adapter,
    /// Stable identity verification failed.
    #[error("first-slice identity verification failed")]
    Identity,
    /// Normalized SQLite persistence or verification failed.
    #[error("first-slice oracle failed")]
    Catalog,
    /// Lexical projection, construction, or validation failed.
    #[error("first-slice search failed")]
    Search,
    /// A bounded source read failed.
    #[error("first-slice source read failed")]
    Source,
    /// A query plan or execution failed.
    #[error("first-slice query failed")]
    Query,
    /// The process-local repository registration is unavailable.
    #[error("first-slice repository was not found")]
    RepositoryNotFound,
    /// The immutable generation is not retained by this daemon process.
    #[error("first-slice generation was not found")]
    GenerationNotFound,
    /// The immutable generation belongs to another repository.
    #[error("first-slice generation does not belong to the repository")]
    GenerationMismatch,
    /// Generation or process-local source retention cannot admit more state.
    #[error("first-slice retention is exhausted")]
    Retention,
    /// A configured integer or duration is not representable.
    #[error("first-slice limits are invalid")]
    Limits,
}

fn is_rust_source(input: &ManifestInput) -> bool {
    input
        .language_signals
        .iter()
        .any(|signal| signal.language == "rust" && signal.evidence == LanguageEvidence::Extension)
}

fn append_normalized_document(
    target: &mut NormalizedIrDocument,
    source: NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    if source.version != target.version
        || source.repository != target.repository
        || source.generation != target.generation
    {
        return Err(FirstSliceError::Identity);
    }
    let target_total = normalized_record_count(target)?;
    let source_total = normalized_record_count(&source)?;
    checked_combined_length(target_total, source_total, limits.max_total_records)?;

    reserve_records(&mut target.files, source.files.len(), limits.max_files)?;
    reserve_records(
        &mut target.entities,
        source.entities.len(),
        limits.max_entities,
    )?;
    reserve_records(
        &mut target.occurrences,
        source.occurrences.len(),
        limits.max_occurrences,
    )?;
    reserve_records(
        &mut target.relations,
        source.relations.len(),
        limits.max_relations,
    )?;
    reserve_records(
        &mut target.provenance,
        source.provenance.len(),
        limits.max_provenance_records,
    )?;
    reserve_records(
        &mut target.source_mappings,
        source.source_mappings.len(),
        limits.max_source_mappings,
    )?;
    reserve_records(
        &mut target.coverage_records,
        source.coverage_records.len(),
        limits.max_coverage_records,
    )?;
    reserve_records(
        &mut target.skipped_regions,
        source.skipped_regions.len(),
        limits.max_skipped_regions,
    )?;
    reserve_records(
        &mut target.diagnostics,
        source.diagnostics.len(),
        limits.max_diagnostics,
    )?;
    reserve_records(
        &mut target.extensions,
        source.extensions.len(),
        limits.max_extensions,
    )?;

    let NormalizedIrDocument {
        mut files,
        mut entities,
        mut occurrences,
        mut relations,
        mut provenance,
        mut source_mappings,
        mut coverage_records,
        mut skipped_regions,
        mut diagnostics,
        mut extensions,
        ..
    } = source;
    target.files.append(&mut files);
    target.entities.append(&mut entities);
    target.occurrences.append(&mut occurrences);
    target.relations.append(&mut relations);
    target.provenance.append(&mut provenance);
    target.source_mappings.append(&mut source_mappings);
    target.coverage_records.append(&mut coverage_records);
    target.skipped_regions.append(&mut skipped_regions);
    target.diagnostics.append(&mut diagnostics);
    target.extensions.append(&mut extensions);
    Ok(())
}

fn normalized_record_count(document: &NormalizedIrDocument) -> Result<usize, FirstSliceError> {
    [
        document.files.len(),
        document.entities.len(),
        document.occurrences.len(),
        document.relations.len(),
        document.provenance.len(),
        document.source_mappings.len(),
        document.coverage_records.len(),
        document.skipped_regions.len(),
        document.diagnostics.len(),
        document.extensions.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, length| {
        total.checked_add(length).ok_or(FirstSliceError::Limits)
    })
}

fn reserve_records<T>(
    target: &mut Vec<T>,
    additional: usize,
    maximum: usize,
) -> Result<(), FirstSliceError> {
    checked_combined_length(target.len(), additional, maximum)?;
    target
        .try_reserve(additional)
        .map_err(|_| FirstSliceError::Limits)
}

fn checked_combined_length(
    current: usize,
    additional: usize,
    maximum: usize,
) -> Result<usize, FirstSliceError> {
    let combined = current
        .checked_add(additional)
        .ok_or(FirstSliceError::Limits)?;
    if combined > maximum {
        return Err(FirstSliceError::Limits);
    }
    Ok(combined)
}

fn require_deadline(cancellation: &Cancellation) -> Result<(), FirstSliceError> {
    if cancellation.has_deadline() {
        Ok(())
    } else {
        Err(FirstSliceError::DeadlineRequired)
    }
}

fn check_cancellation(cancellation: &Cancellation) -> Result<(), FirstSliceError> {
    cancellation
        .check()
        .map_err(|cancelled| FirstSliceError::Cancelled(cancelled.reason()))
}

fn current_cancellation(cancellation: &Cancellation) -> Option<FirstSliceError> {
    cancellation
        .check()
        .err()
        .map(|cancelled| FirstSliceError::Cancelled(cancelled.reason()))
}

fn map_discovery_error(error: DiscoveryError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        DiscoveryError::Cancelled(cancelled) => FirstSliceError::Cancelled(cancelled.reason()),
        DiscoveryError::Vfs(VfsError::Cancelled(reason)) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::Discovery,
    }
}

fn map_vfs_error(error: VfsError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        VfsError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::Repository,
    }
}

fn map_adapter_error(error: AdapterError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        AdapterError::Cancelled { reason } => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::Adapter,
    }
}

fn map_identity_error(
    error: IdentityVerificationError,
    cancellation: &Cancellation,
) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        IdentityVerificationError::Control(GenerationControlError::Cancelled { reason }) => {
            FirstSliceError::Cancelled(reason)
        }
        _ => FirstSliceError::Identity,
    }
}

fn map_catalog_error(error: &CatalogError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    if error.kind() == CatalogErrorKind::Cancelled {
        FirstSliceError::Cancelled(
            cancellation
                .reason()
                .unwrap_or(CancellationReason::ParentCancelled),
        )
    } else {
        FirstSliceError::Catalog
    }
}

fn map_search_error(error: SearchError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        SearchError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::Search,
    }
}

fn map_source_error(error: SourceError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        SourceError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::Source,
    }
}

fn map_query_error(error: QueryError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        QueryError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::Query,
    }
}

fn analysis_limits() -> Result<AnalysisLimits, FirstSliceError> {
    let batch = BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024)
        .map_err(|_| FirstSliceError::Limits)?;
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .map_err(|_| FirstSliceError::Limits)?;
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .map_err(|_| FirstSliceError::Limits)
}

fn parser_config() -> Result<RuntimeConfig, FirstSliceError> {
    let settings = ParserSettings::new(4096).map_err(|_| FirstSliceError::Limits)?;
    RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        64,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .map_err(|_| FirstSliceError::Limits)
}

fn fallible_copy_bytes(value: &[u8]) -> Result<Vec<u8>, FirstSliceError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| FirstSliceError::Limits)?;
    copy.extend_from_slice(value);
    Ok(copy)
}

fn fallible_copy_string(value: &str) -> Result<String, FirstSliceError> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| FirstSliceError::Limits)?;
    copy.push_str(value);
    Ok(copy)
}

fn canonical_repository_root(
    path: &Path,
    cancellation: &Cancellation,
) -> Result<PathBuf, FirstSliceError> {
    validate_repository_path_length(path)?;
    check_cancellation(cancellation)?;
    let absolute = std::path::absolute(path).map_err(|_| FirstSliceError::Repository)?;
    validate_repository_path_length(&absolute)?;
    check_cancellation(cancellation)?;
    let canonical_result = std::fs::canonicalize(absolute);
    check_cancellation(cancellation)?;
    let canonical = canonical_result.map_err(|_| FirstSliceError::Repository)?;
    validate_repository_path_length(&canonical)?;
    Ok(canonical)
}

fn random_repository_id(
    repositories: &BTreeMap<ContentHash, RepositoryId>,
) -> Result<RepositoryId, FirstSliceError> {
    for _ in 0..MAX_RANDOM_ID_ATTEMPTS {
        let mut local_uuid = [0_u8; 16];
        getrandom::fill(&mut local_uuid).map_err(|_| FirstSliceError::RandomUnavailable)?;
        local_uuid[6] = (local_uuid[6] & 0x0f) | 0x40;
        local_uuid[8] = (local_uuid[8] & 0x3f) | 0x80;
        let candidate = derive_repository(&local_uuid).id();
        if !repositories
            .values()
            .any(|repository| *repository == candidate)
        {
            return Ok(candidate);
        }
    }
    Err(FirstSliceError::RandomUnavailable)
}

fn validate_repository_path_length(path: &Path) -> Result<(), FirstSliceError> {
    if repository_path_identity_bytes(path)? > MAX_REPOSITORY_PATH_IDENTITY_BYTES {
        return Err(FirstSliceError::Repository);
    }
    Ok(())
}

fn repository_path_identity_bytes(path: &Path) -> Result<usize, FirstSliceError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        Ok(path.as_os_str().as_bytes().len())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        path.as_os_str()
            .encode_wide()
            .count()
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or(FirstSliceError::Repository)
    }
}

fn repository_path_hash(path: &Path) -> Result<ContentHash, FirstSliceError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let bytes = path.as_os_str().as_bytes();
        validate_repository_path_length(path)?;
        Ok(content_hash(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let byte_length = repository_path_identity_bytes(path)?;
        validate_repository_path_length(path)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_length)
            .map_err(|_| FirstSliceError::Limits)?;
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        Ok(content_hash(&bytes))
    }
}

fn generation_format_version() -> u32 {
    (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
        | u32::from(GENERATION_CONTRACT_VERSION.minor())
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };

    use rootlight_ids::GenerationId;
    use rootlight_ir::{CoverageScope, CoverageStatus};
    use tempfile::TempDir;

    use super::*;

    const GATE_FIXTURE_ROOT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gate-1/first-slice/v1"
    );
    const GATE_V2_PATCH: &str =
        include_str!("../../../tests/fixtures/gate-1/first-slice/v1-to-v2.patch");
    const IGNORED_SENTINEL: &str = "ROOTLIGHT_IGNORED_SENTINEL";

    #[test]
    fn malformed_file_retains_unknown_coverage_and_recovery_diagnostic() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    42\n}\n",
        )
        .expect("valid source writes");
        fs::write(
            fixture.path().join("src/malformed.rs"),
            "pub fn broken( {\n",
        )
        .expect("malformed source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("malformed syntax preserves a generation");
        assert_eq!(receipt.indexed_files, 2);
        let snapshot = service
            .generations
            .generation(receipt.generation)
            .expect("published generation remains retained");
        let document = snapshot.document();
        let malformed = document
            .files
            .iter()
            .find(|file| file.path == "src/malformed.rs")
            .expect("malformed file remains represented")
            .id;

        assert!(document.coverage_records.iter().any(|coverage| {
            coverage.scope == CoverageScope::File(malformed)
                && coverage.status == CoverageStatus::Unknown
        }));
        assert!(document.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "syntax-error-recovery"
                && diagnostic.coverage_effect == CoverageStatus::Unknown
                && diagnostic
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == malformed)
        }));
    }

    #[test]
    fn aggregate_length_checks_bounds_and_overflow() {
        assert_eq!(
            checked_combined_length(2, 2, 3),
            Err(FirstSliceError::Limits)
        );
        assert_eq!(
            checked_combined_length(usize::MAX, 1, usize::MAX),
            Err(FirstSliceError::Limits)
        );
    }

    #[test]
    fn source_retention_is_byte_bounded_deduplicated_and_cleanup_aware() {
        const FIRST: &str = "pub fn answer() -> u32 {\n    42\n}\n";
        const SECOND: &str = "pub fn answer() -> u32 {\n    43\n}\n";
        const THIRD: &str = "pub fn answer() -> u32 {\n    44\n}\n";
        const STABLE: &str = "pub fn stable() -> bool {\n    true\n}\n";

        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let answer_path = fixture.path().join("src/lib.rs");
        fs::write(&answer_path, FIRST).expect("first source writes");
        fs::write(fixture.path().join("src/stable.rs"), STABLE).expect("stable source writes");
        let first_generation_bytes = FIRST.len() + STABLE.len();
        let exact_retention_bytes = first_generation_bytes + SECOND.len();
        let mut service = FirstSliceService::new_with_source_limit(3, exact_retention_bytes)
            .expect("bounded first-slice service initializes");

        let cancelled = deadline();
        let prepared = service
            .prepare_rust_fixture(fixture.path(), &cancelled)
            .expect("first generation prepares");
        let staged = service
            .stage_prepared(prepared, &cancelled)
            .expect("first generation stages");
        assert_eq!(
            service.source_snapshots.retained_bytes(),
            first_generation_bytes
        );
        assert_eq!(service.source_snapshots.staged_generations(), 1);
        assert!(cancelled.cancel(CancellationReason::ClientRequest));
        service
            .discard_staged(staged)
            .expect("cancelled staging releases source retention");
        assert_eq!(service.source_snapshots.retained_bytes(), 0);
        assert_eq!(service.source_snapshots.staged_generations(), 0);

        let first = service
            .index_rust_fixture(fixture.path(), &deadline())
            .expect("first generation publishes after cleanup");
        let first_locate = service
            .code_locate(
                first.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                1,
                &deadline(),
            )
            .expect("first answer locates");
        let first_reference = first_locate.data.hits[0]
            .source
            .clone()
            .expect("first answer has exact source evidence");

        fs::write(&answer_path, SECOND).expect("second source writes");
        let second = service
            .index_rust_fixture(fixture.path(), &deadline())
            .expect("exact source retention cap admits the successor");
        assert_eq!(
            service.source_snapshots.retained_bytes(),
            exact_retention_bytes
        );
        let second_locate = service
            .code_locate(
                second.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                1,
                &deadline(),
            )
            .expect("second answer locates");
        let second_reference = second_locate.data.hits[0]
            .source
            .clone()
            .expect("second answer has exact source evidence");

        fs::write(&answer_path, THIRD).expect("third source writes");
        let prepared = service
            .prepare_rust_fixture(fixture.path(), &deadline())
            .expect("over-cap successor prepares before retention admission");
        let third = prepared.receipt();
        assert!(matches!(
            service.stage_prepared(prepared, &deadline()),
            Err(FirstSliceError::Retention)
        ));
        assert_eq!(
            service.source_snapshots.retained_bytes(),
            exact_retention_bytes
        );
        assert_eq!(service.source_snapshots.staged_generations(), 0);
        assert_eq!(
            service.active_generation_for(first.repository),
            Some(second.generation)
        );
        assert_eq!(
            service.resolve_generation(first.repository, Some(third.generation)),
            Err(FirstSliceError::GenerationNotFound)
        );
        assert!(matches!(
            service.source_read(third.generation, vec![first_reference.clone()], &deadline(),),
            Err(FirstSliceError::Query)
        ));

        let first_source = service
            .source_read(first.generation, vec![first_reference.clone()], &deadline())
            .expect("published first snapshot remains readable");
        assert_eq!(first_source.data.chunks[0].text, FIRST);
        assert_eq!(
            first_source.data.chunks[0].content_hash,
            first_reference.content_hash()
        );
        let second_source = service
            .source_read(
                second.generation,
                vec![second_reference.clone()],
                &deadline(),
            )
            .expect("published second snapshot remains readable");
        assert_eq!(second_source.data.chunks[0].text, SECOND);
        assert_eq!(
            second_source.data.chunks[0].content_hash,
            second_reference.content_hash()
        );
    }

    #[test]
    fn gate_fixture_preserves_nested_policy_recovery_and_generation_lineage() {
        let fixture = materialize_gate_fixture();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("Gate-1 v1 indexes");
        assert_eq!(first.discovered_inputs, 5);
        assert_eq!(first.indexed_files, 3);
        assert_indexed_gate_paths(&service, first.generation);
        assert_malformed_recovery(&service, first.generation);

        let answer = service
            .code_locate(
                first.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                8,
                &cancellation,
            )
            .expect("v1 answer locate succeeds");
        assert_eq!(answer.data.hits.len(), 1);
        assert_eq!(answer.data.hits[0].path, "src/lib.rs");
        let first_symbol = answer.data.hits[0].symbol;
        let first_answer = answer.data.hits[0]
            .source
            .clone()
            .expect("v1 answer retains exact source evidence");
        let cached_v1_source = service
            .source_read(first.generation, vec![first_answer.clone()], &cancellation)
            .expect("v1 answer source reads");
        assert_eq!(cached_v1_source.data.chunks.len(), 1);
        let cached_v1_text = &cached_v1_source.data.chunks[0].text;
        assert!(cached_v1_text.contains("ROOTLIGHT_PROMPT_SENTINEL"));
        assert!(cached_v1_text.contains("42"));
        assert!(!cached_v1_text.contains("43"));
        assert!(!cached_v1_text.contains(IGNORED_SENTINEL));

        let kept = service
            .code_locate(
                first.generation,
                "kept_after_negation".to_owned(),
                LocateMode::Exact,
                8,
                &cancellation,
            )
            .expect("negated nested source locate succeeds");
        assert_eq!(kept.data.hits.len(), 1);
        assert_eq!(kept.data.hits[0].path, "nested/ignored/kept.rs");
        let kept_source = service
            .source_read(
                first.generation,
                vec![
                    kept.data.hits[0]
                        .source
                        .clone()
                        .expect("kept source retains exact evidence"),
                ],
                &cancellation,
            )
            .expect("kept source reads");
        assert!(
            kept_source.data.chunks[0]
                .text
                .contains("kept_after_negation")
        );
        assert!(!kept_source.data.chunks[0].text.contains(IGNORED_SENTINEL));

        assert_no_exact_hits(
            &service,
            first.generation,
            &["ignored_by_nested_rule", IGNORED_SENTINEL, "broken"],
            &cancellation,
        );
        let repeated = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("unchanged Gate-1 v1 is idempotent");
        assert_eq!(repeated, first);

        apply_gate_v2_patch(fixture.path());
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("Gate-1 v2 indexes");
        assert_eq!(second.parent, Some(first.generation));
        assert_ne!(second.generation, first.generation);
        assert_eq!(
            service.active_generation_for(second.repository),
            Some(second.generation)
        );
        assert_indexed_gate_paths(&service, second.generation);
        assert_malformed_recovery(&service, second.generation);

        let active_answer = service
            .code_locate(
                second.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                8,
                &cancellation,
            )
            .expect("v2 answer locate succeeds");
        assert_eq!(active_answer.data.hits.len(), 1);
        assert_eq!(active_answer.data.hits[0].path, "src/lib.rs");
        assert_eq!(active_answer.data.hits[0].symbol, first_symbol);
        let active_source = service
            .source_read(
                second.generation,
                vec![
                    active_answer.data.hits[0]
                        .source
                        .clone()
                        .expect("v2 answer retains exact source evidence"),
                ],
                &cancellation,
            )
            .expect("v2 answer source reads");
        assert_eq!(active_source.data.chunks.len(), 1);
        let active_text = &active_source.data.chunks[0].text;
        assert!(active_text.contains("43"));
        assert!(!active_text.contains("42"));
        assert!(!active_text.contains(IGNORED_SENTINEL));

        let prior_answer = service
            .code_locate(
                first.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                8,
                &cancellation,
            )
            .expect("prior generation remains queryable");
        assert_eq!(prior_answer.data.hits.len(), 1);
        assert_eq!(prior_answer.data.hits[0].symbol, first_symbol);
        assert_eq!(prior_answer.data.hits[0].path, "src/lib.rs");
        let prior_reference = prior_answer.data.hits[0]
            .source
            .clone()
            .expect("prior answer retains exact source evidence");
        assert_eq!(prior_reference, first_answer);
        let prior_source = service
            .source_read(first.generation, vec![prior_reference], &cancellation)
            .expect("prior source snapshot remains readable");
        assert_eq!(prior_source.data.chunks.len(), 1);
        assert_eq!(prior_source.data.chunks[0].text, *cached_v1_text);
        assert_eq!(
            prior_source.data.chunks[0].content_hash,
            first_answer.content_hash()
        );
        assert!(
            !service
                .resolve_generation(first.repository, Some(first.generation))
                .expect("prior generation remains retained")
                .active
        );
        assert_no_exact_hits(
            &service,
            second.generation,
            &["ignored_by_nested_rule", IGNORED_SENTINEL, "broken"],
            &cancellation,
        );
    }

    fn materialize_gate_fixture() -> TempDir {
        let fixture = TempDir::new().expect("materialized fixture root exists");
        copy_fixture_tree(Path::new(GATE_FIXTURE_ROOT), fixture.path());
        fixture
    }

    fn copy_fixture_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("fixture directory materializes");
        for entry in fs::read_dir(source).expect("fixture directory reads") {
            let entry = entry.expect("fixture entry reads");
            let file_type = entry.file_type().expect("fixture entry type reads");
            let target = destination.join(entry.file_name());
            if file_type.is_dir() {
                copy_fixture_tree(&entry.path(), &target);
            } else {
                assert!(file_type.is_file(), "fixture entries must be regular files");
                fs::copy(entry.path(), target).expect("fixture file materializes");
            }
        }
    }

    fn apply_gate_v2_patch(root: &Path) {
        let target = GATE_V2_PATCH
            .lines()
            .find_map(|line| line.strip_prefix("+++ b/"))
            .expect("Gate-1 patch names a target");
        let removed = GATE_V2_PATCH
            .lines()
            .find(|line| line.starts_with('-') && !line.starts_with("---"))
            .and_then(|line| line.strip_prefix('-'))
            .expect("Gate-1 patch removes one line");
        let added = GATE_V2_PATCH
            .lines()
            .find(|line| line.starts_with('+') && !line.starts_with("+++"))
            .and_then(|line| line.strip_prefix('+'))
            .expect("Gate-1 patch adds one line");
        let path = root.join(target);
        let source = fs::read_to_string(&path).expect("materialized v1 source reads");
        let removed = format!("{removed}\n");
        let added = format!("{added}\n");
        assert_eq!(
            source.matches(&removed).count(),
            1,
            "Gate-1 patch context must match exactly once"
        );
        fs::write(path, source.replacen(&removed, &added, 1))
            .expect("Gate-1 v2 source materializes");
    }

    fn deadline() -> Cancellation {
        Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        )
    }

    fn assert_indexed_gate_paths(service: &FirstSliceService, generation: GenerationId) {
        let snapshot = service
            .generations
            .generation(generation)
            .expect("Gate-1 generation remains retained");
        let mut paths = snapshot
            .document()
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        assert_eq!(
            paths,
            ["nested/ignored/kept.rs", "src/lib.rs", "src/malformed.rs"]
        );
    }

    fn assert_malformed_recovery(service: &FirstSliceService, generation: GenerationId) {
        let snapshot = service
            .generations
            .generation(generation)
            .expect("Gate-1 generation remains retained");
        let document = snapshot.document();
        let malformed = document
            .files
            .iter()
            .find(|file| file.path == "src/malformed.rs")
            .expect("malformed Gate-1 file remains represented")
            .id;
        assert!(document.coverage_records.iter().any(|coverage| {
            coverage.scope == CoverageScope::File(malformed)
                && coverage.status == CoverageStatus::Unknown
        }));
        assert!(document.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "syntax-error-recovery"
                && diagnostic.coverage_effect == CoverageStatus::Unknown
                && diagnostic
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == malformed)
        }));
    }

    fn assert_no_exact_hits(
        service: &FirstSliceService,
        generation: GenerationId,
        queries: &[&str],
        cancellation: &Cancellation,
    ) {
        for query in queries {
            let located = service
                .code_locate(
                    generation,
                    (*query).to_owned(),
                    LocateMode::Exact,
                    8,
                    cancellation,
                )
                .expect("excluded Gate-1 query succeeds");
            assert!(located.data.hits.is_empty(), "{query} must not be exposed");
        }
    }
}
