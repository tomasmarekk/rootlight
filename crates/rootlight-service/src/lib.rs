//! Transport-independent first-slice indexing and query use cases.
//!
//! This crate composes existing bounded domain contracts. It does not parse
//! CLI, IPC, or MCP requests and does not own durable generation publication.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Instant};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, ParseProvider, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
pub use rootlight_cancel::Cancellation;
use rootlight_catalog::EphemeralOracleWriter;
use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};
use rootlight_discovery::{DiscoveryLimits, DiscoveryPolicy, InputClass, discover};
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
use rootlight_query::{GenerationSet, QueryBudget, project_lexical_documents};
use rootlight_search::{BuildBudget, LexicalIndex, SearchBudget};
use rootlight_source::{SourceBudget, SourceReadOptions, SourceService};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationManifestRecipe,
    GenerationMetadata, IdentityVerifiedGeneration,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use serde::Serialize;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SYNTAX_NODES: usize = 16_384;
const MAX_SYNTAX_DEPTH: usize = 128;
const MAX_REPOSITORY_PATH_IDENTITY_BYTES: usize = 64 * 1024;
const PROVIDER_SET_SEED: &[u8] = b"rootlight.first-slice.providers/1";
const BUILD_CONTEXT_SEED: &[u8] = b"rootlight.first-slice.build-context/1";
const ANALYZER_BINARY_SEED: &[u8] = b"rootlight.first-slice.treesitter-rust/1";

/// Bounded receipt for one ephemeral first-slice generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FirstSliceIndexReceipt {
    /// Stable repository identity derived from the local root identity.
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
        let started = Instant::now();
        require_deadline(cancellation)?;
        cancellation
            .check()
            .map_err(|_| FirstSliceError::Cancelled)?;
        let absolute = std::path::absolute(path).map_err(|_| FirstSliceError::Repository)?;
        let repository = derive_repository(repository_path_hash(&absolute)?.as_bytes()).id();
        let root =
            RepositoryRoot::open(repository, &absolute).map_err(|_| FirstSliceError::Repository)?;
        let policy =
            DiscoveryPolicy::build(Vec::new(), false).map_err(|_| FirstSliceError::Discovery)?;
        let manifest = discover(
            &root,
            &self.config,
            &policy,
            DiscoveryLimits::from_config(&self.config),
            cancellation,
        )
        .map_err(|_| FirstSliceError::Discovery)?;
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
            .snapshot(
                &relative,
                u64::try_from(self.analysis_limits.max_source_bytes())
                    .map_err(|_| FirstSliceError::Limits)?,
            )
            .map_err(|_| FirstSliceError::Repository)?;
        if snapshot.file() != input.file
            || snapshot.content_hash() != input.content_hash
            || u64::try_from(snapshot.content().len()).ok() != Some(input.bytes)
        {
            return Err(FirstSliceError::DiscoveryDrift);
        }
        let file_claim = FileIdentityClaim {
            file: input.file,
            repository,
            path: input.path.clone(),
            path_identity: relative.identity_bytes().to_vec(),
            content_hash: input.content_hash,
            byte_length: input.bytes,
        };
        let manifest_hash =
            GenerationManifestRecipe::new(repository, self.config.hash(), vec![file_claim])
                .map_err(|_| FirstSliceError::Identity)?
                .canonical_hash()
                .map_err(|_| FirstSliceError::Identity)?;
        let provider_set_hash = content_hash(PROVIDER_SET_SEED);
        let active = self.generations.active_generation();
        if let Some(active) = active
            && let Ok(snapshot) = self.generations.generation(active)
        {
            let metadata = snapshot.metadata();
            if metadata.repository() == repository
                && metadata.manifest_hash() == manifest_hash
                && metadata.configuration_hash() == self.config.hash()
                && metadata.provider_set_hash() == provider_set_hash
                && let Some(receipt) = self.receipts.get(&active)
            {
                return Ok(*receipt);
            }
        }
        let parent = active.and_then(|generation| {
            self.generations
                .generation(generation)
                .ok()
                .filter(|snapshot| snapshot.metadata().repository() == repository)
                .map(|_| generation)
        });
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent,
            manifest_hash,
            config_hash: self.config.hash(),
            provider_set_hash,
            format_version: generation_format_version(),
        })
        .id();
        if let Some(receipt) = self.receipts.get(&generation) {
            return Ok(*receipt);
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
        .map_err(|_| FirstSliceError::Adapter)?;
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
        .map_err(|_| FirstSliceError::Identity)?;
        let oracle = EphemeralOracleWriter::create()
            .map_err(|_| FirstSliceError::Catalog)?
            .seal(verified, &context)
            .map_err(|_| FirstSliceError::Catalog)?;
        let oracle_allocated_bytes = oracle
            .allocated_bytes()
            .map_err(|_| FirstSliceError::Catalog)?;
        let persisted = oracle
            .read(&context)
            .map_err(|_| FirstSliceError::Catalog)?;
        let documents = project_lexical_documents(&persisted, BuildBudget::default(), cancellation)
            .map_err(|_| FirstSliceError::Search)?;
        let lexical_documents =
            u64::try_from(documents.len()).map_err(|_| FirstSliceError::Limits)?;
        let search = LexicalIndex::build_ephemeral(
            generation,
            documents,
            BuildBudget::default(),
            cancellation,
        )
        .map_err(|_| FirstSliceError::Search)?;
        let indexed_files =
            u64::try_from(persisted.document().files.len()).map_err(|_| FirstSliceError::Limits)?;
        let entities = u64::try_from(persisted.document().entities.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let verified = oracle
            .read_verified(&context)
            .map_err(|_| FirstSliceError::Catalog)?;
        self.generations
            .publish(verified, search, true)
            .map_err(|_| FirstSliceError::Retention)?;
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
        self.roots.insert(generation, root);
        self.receipts.insert(generation, receipt);
        Ok(receipt)
    }

    /// Returns the active generation selected by the last successful index.
    #[must_use]
    pub const fn active_generation(&self) -> Option<GenerationId> {
        self.generations.active_generation()
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
            .map_err(|_| FirstSliceError::Query)?;
        service
            .execute_code_locate(&plan, cancellation)
            .map_err(|_| FirstSliceError::Query)
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
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_symbol_explain(symbol, QueryBudget::new())
            .map_err(|_| FirstSliceError::Query)?;
        service
            .execute_symbol_explain(&plan, cancellation)
            .map_err(|_| FirstSliceError::Query)
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
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let snapshot = self
            .generations
            .generation(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let root = self.roots.get(&generation).ok_or(FirstSliceError::Query)?;
        let source = SourceService::new(root, snapshot).map_err(|_| FirstSliceError::Source)?;
        let plan = service
            .plan_source_read(
                references,
                SourceReadOptions::new(),
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .map_err(|_| FirstSliceError::Query)?;
        service
            .execute_source_read(&plan, &source, cancellation)
            .map_err(|_| FirstSliceError::Query)
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
    /// The caller omitted the required monotonic deadline.
    #[error("first-slice indexing requires a monotonic deadline")]
    DeadlineRequired,
    /// Cooperative cancellation or deadline stopped the operation.
    #[error("first-slice operation was cancelled")]
    Cancelled,
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

fn repository_path_hash(path: &Path) -> Result<ContentHash, FirstSliceError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let bytes = path.as_os_str().as_bytes();
        if bytes.len() > MAX_REPOSITORY_PATH_IDENTITY_BYTES {
            return Err(FirstSliceError::Repository);
        }
        Ok(content_hash(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let byte_length = encoded
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or(FirstSliceError::Repository)?;
        if byte_length > MAX_REPOSITORY_PATH_IDENTITY_BYTES {
            return Err(FirstSliceError::Repository);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_length)
            .map_err(|_| FirstSliceError::Limits)?;
        for unit in encoded {
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
