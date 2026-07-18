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
use rootlight_discovery::{DiscoveryError, DiscoveryLimits, DiscoveryPolicy, InputClass, discover};
use rootlight_ids::{
    ContentHash, GenerationId, GenerationIdentity, RepositoryId, SymbolId, content_hash,
    derive_generation, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, FileIdentityClaim, IrLimits,
    ProducerIdentity, SourceRef, SourceSpan,
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
use rootlight_vfs::{RelativePath, RepositoryRoot, VfsError};
use serde::Serialize;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
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
    root: RepositoryRoot,
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
        root: RepositoryRoot,
        root_identity: ContentHash,
        register_repository: bool,
    },
}

/// Transport-independent owner of bounded ephemeral fixture generations.
///
/// The service intentionally retains at most the caller-selected hard-bounded
/// generation count. SQLite and lexical state are in memory because ADR-026
/// has not authorized durable private-file creation. Full crash recovery,
/// leases, and filesystem publication remain M12 work.
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
    roots: BTreeMap<GenerationId, RepositoryRoot>,
    receipts: BTreeMap<GenerationId, FirstSliceIndexReceipt>,
}

impl FirstSliceService {
    /// Creates the bounded Rust first-slice service.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] when a required bounded parser, analyzer,
    /// configuration, or generation-retention contract cannot initialize.
    pub fn new(maximum_generations: usize) -> Result<Self, FirstSliceError> {
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
        Ok(Self {
            config,
            analysis_limits,
            extensions: ExtensionSupport::default(),
            analyzer,
            repositories: BTreeMap::new(),
            active_by_repository: BTreeMap::new(),
            generations,
            roots: BTreeMap::new(),
            receipts: BTreeMap::new(),
        })
    }

    /// Discovers, parses, validates, round-trips, indexes, and publishes one
    /// single-file Rust fixture repository.
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
        let [input] = manifest.inputs.as_slice() else {
            return Err(FirstSliceError::FixtureShape);
        };
        if !input
            .language_signals
            .iter()
            .any(|signal| signal.language == "rust")
        {
            return Err(FirstSliceError::FixtureShape);
        }
        let relative =
            RelativePath::parse(Path::new(&input.path)).map_err(|_| FirstSliceError::Repository)?;
        let snapshot = root
            .snapshot_with_cancellation(
                &relative,
                u64::try_from(self.analysis_limits.max_source_bytes())
                    .map_err(|_| FirstSliceError::Limits)?,
                cancellation,
            )
            .map_err(|error| map_vfs_error(error, cancellation))?;
        if snapshot.file() != input.file
            || snapshot.content_hash() != input.content_hash
            || u64::try_from(snapshot.content().len()).ok() != Some(input.bytes)
        {
            return Err(FirstSliceError::DiscoveryDrift);
        }
        let file_claim = FileIdentityClaim {
            file: input.file,
            repository,
            path: fallible_copy_string(&input.path)?,
            path_identity: fallible_copy_bytes(relative.identity_bytes())?,
            content_hash: input.content_hash,
            byte_length: input.bytes,
        };
        let mut file_claims = Vec::new();
        file_claims
            .try_reserve_exact(1)
            .map_err(|_| FirstSliceError::Limits)?;
        file_claims.push(file_claim);
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
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(input.file, 0, input.bytes).map_err(|_| FirstSliceError::Identity)?,
            input.content_hash,
            None,
        );
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source)
                .map_err(|_| FirstSliceError::Adapter)?,
            LanguageId::new("rust").map_err(|_| FirstSliceError::Adapter)?,
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(BUILD_CONTEXT_SEED)),
            &self.analysis_limits,
        )
        .map_err(|_| FirstSliceError::Adapter)?
        .with_generated_status(matches!(input.class, InputClass::Generated));
        let output = execute_analysis(
            &self.analyzer,
            &request,
            self.extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            cancellation,
        )
        .map_err(|error| map_adapter_error(error, cancellation))?;
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
            output.document().clone(),
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
                root,
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
                let receipt = prepared.receipt;
                self.generations
                    .stage(prepared.verified, prepared.search)
                    .map_err(|_| FirstSliceError::Retention)?;
                Ok(FirstSliceStagedIndex {
                    receipt,
                    publication: FirstSlicePublication::Pending {
                        root: prepared.root,
                        root_identity: prepared.root_identity,
                        register_repository: prepared.register_repository,
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
                root,
                root_identity,
                register_repository,
            } => {
                self.generations
                    .commit_staged(receipt.generation, true)
                    .map_err(|_| FirstSliceError::Retention)?;
                self.roots.insert(receipt.generation, root);
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
            self.generations
                .discard_staged(staged.receipt.generation)
                .map_err(|_| FirstSliceError::Retention)?;
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
        let root = self.roots.get(&generation).ok_or(FirstSliceError::Query)?;
        let source = SourceService::new(root, snapshot)
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
    /// The retained generation set cannot admit another generation.
    #[error("first-slice generation retention is exhausted")]
    Retention,
    /// A configured integer or duration is not representable.
    #[error("first-slice limits are invalid")]
    Limits,
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
