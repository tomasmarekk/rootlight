//! Transport-independent first-slice indexing and query use cases.
//!
//! This crate composes existing bounded domain contracts. It does not parse
//! CLI, IPC, or MCP requests; durable mode also owns crash-safe generation
//! publication beneath a caller-prepared private state root.

#![forbid(unsafe_code)]

pub mod catalog;
mod durable;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use catalog::{
    CATALOG_MAX_LABEL_BYTES, CATALOG_MAX_ROOT_PATH_BYTES, CatalogFreshness, CatalogInstant,
    CatalogLanguageCoverage, CatalogPage, CatalogPageRequest, CatalogRepositoryRecord,
    CatalogRepositoryState, CatalogSnapshotStore,
};
use durable::{
    DURABLE_PUBLICATION_RESIDUAL_BYTES, DurableCatalog, DurablePublishedGeneration,
    DurableRepositoryMetadata, DurableStorageAdmissionFailure, DurableStorageAdmissionPolicy,
    DurableStorageAdmissionScope, DurableStorageAdmittedGeneration, DurableStorageReservation,
    REPOSITORY_METADATA_VERSION, RestoredGeneration, recovery_snapshot_output_reservation,
};
use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GeneratedOriginMapping, GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy,
    ParseProvider, ReportError, RequestError, ResourceKind, SinkError, StreamLimits,
    TransformationId,
};
use rootlight_adapter_treesitter::{
    ADAPTER_VERSION as TREE_SITTER_ADAPTER_VERSION, GrammarDescriptor, GrammarRegistry,
    ParserSettings, RuntimeConfig, TREE_SITTER_RUNTIME_VERSION, TreeSitterAnalyzer,
    TreeSitterProvider, TreeSitterStructuralArtifact,
};
pub use rootlight_adapters::{
    PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC, RUNTIME_TRACE_SCHEMA_VERSION, RuntimeTraceImportError,
    RuntimeTraceLimits, RuntimeTraceOverlay, RuntimeTraceProvenance, RuntimeTraceRelation,
    RuntimeTraceRelationKind, RuntimeTraceResource,
};
use rootlight_adapters::{
    RuntimeTraceImportRequest, SemanticProjectLanguage, import_runtime_trace,
};
pub use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::{CatalogError, CatalogErrorKind, EphemeralOracleWriter, OracleWriter};
use rootlight_config::{
    CONFIG_VERSION_1_2, ConfigLayer, ConfigSnapshot, ConfigSource, MAXIMUM_REPOSITORIES,
};
use rootlight_discovery::{
    DiscoveryError, DiscoveryLimits, DiscoveryPolicy, IncrementalDiscovery,
    IncrementalDiscoveryBaseline, IncrementalDiscoveryContext, IncrementalDiscoveryOptions,
    InputClass, LanguageEvidence, ManifestInput, canonical_language,
    correlate_incremental_manifest, discover_incremental_with_progress, discover_with_snapshots,
    extension_language, language_capabilities,
};
use rootlight_git::{
    ChangeSet as GitChangeSet, GitCollectErrorCode, GitCollectLimits, GitLimits,
    RevisionSelector as GitRevisionSelector, collect_repository, collect_revision_range,
    collect_worktree_status, revision_resolves_to_head,
};
use rootlight_ids::{
    ContentHash, FactId, FileId, GenerationId, GenerationIdentity, OperationId, RepositoryId,
    SymbolId, content_hash, derive_fact, derive_generation, derive_repository,
};
use rootlight_incremental::{
    AnalysisUnitId, ArtifactDecisionKind, ArtifactId, ArtifactSummary, DependencyEdge,
    DependencyGraph, DependencyRegistry, DependencySource, EquivalenceSnapshot, FactDomainSet,
    FactNode, GenerationSummary, GraphLimits, INCREMENTAL_SCHEMA_VERSION, IncrementalError,
    InputFingerprint, InputKey, InputKind, InputSnapshot, InvalidationPlan,
    LOGICAL_SNAPSHOT_SCHEMA_VERSION, LogicalComponent, LogicalDomain, PassDeclaration, PassId,
    PassObservation, PlanningLimits, ReconcileMode, TraceEntry, plan_invalidation,
};
pub use rootlight_incremental::{ChangeClass, FactDomain, FallbackReason, FileChangeKind};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CanonicalGenerationNeutralDigests,
    CanonicalNormalizedFileChunk, CoverageRecord, CoverageScope, CoverageStatus, DiagnosticRecord,
    DiagnosticSeverity, EntityKind, EntityRecord, ExtensionEnvelope, ExtensionSupport,
    FILE_IDENTITY_CLAIM_NAMESPACE, FactDomain as IrFactDomain, FactEvidence, FactRef,
    FileIdentityClaim, FileRecord, IrDocumentValidationError, IrLimits,
    LEXICAL_EXTENSION_NAMESPACE, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    ProducerIdentity, ProducerKind, ProvenanceRecord, RelationEndpoint, RelationPredicate,
    RelationRecord, SYMBOL_IDENTITY_CLAIM_NAMESPACE, SkippedRegion, SkippedRegionReason,
    SourceMappingKind, SourceMappingRecord, SourceRef, SourceSpan, derive_coverage_record_id,
    derive_diagnostic_record_id, derive_provenance_record_id, derive_skipped_region_id,
    generation_neutral_workspace_bytes, new_file_identity_claim_envelope,
};
pub use rootlight_query::{
    ADVANCED_DEFAULT_MAX_DEPTH, ADVANCED_DEFAULT_MAX_RESULTS, ADVANCED_MAX_TRAVERSAL,
    AdvancedAstNode, AdvancedColumnSchema, AdvancedColumnType, AdvancedCompleteness,
    AdvancedPlanExplanation, AdvancedQueryResult, AnalysisScope, ArchitectureCommunity,
    ArchitectureComponent, ArchitectureConnection, ArchitectureCyclesResult, ArchitectureHotspot,
    ArchitectureOverviewDerivedView, ArchitectureOverviewDetail, ArchitectureOverviewResult,
    ArchitectureOverviewView, BreakingCandidateRecord, ChangeImpactClassification,
    ChangeImpactRelationPolicy, ChangeImpactResult, ChangeImpactRiskLevel, ChangeImpactRiskSummary,
    ChangeImpactTestCandidate, CodeDeadEntryPointPolicy, CodeDeadResult, CodeLocateResult,
    CycleProjectionLevel, CycleRankBy, FlowTraceResult, HistoryArchitectureDelta,
    HistoryChangeKind, HistoryCompareResult, HistorySemanticChangeKind, ImpactEntryRecord,
    ImpactGroupRecord, LineageMatchRecord, LocateMode, PlanChangeContextPack, PlanChangeDecision,
    PlanChangeImpactSummary, PlanChangeObjective, PlanChangeResult, PlanChangeStepRecord,
    QueryResponse, RankedTestSelection, RelationDirection, RelationFamily, ResolvedChangeRecord,
    SemanticChangeRecord, SourceReadQueryResult, SymbolExplainResult, SymbolRelationshipsResult,
    TestsSelectCoverage, TestsSelectGap, TestsSelectKind, TestsSelectResult,
};
use rootlight_query::{
    GenerationSet, QueryBudget, QueryError, QueryService, SOURCE_FALLBACK_TEXT_BYTES,
    project_lexical_documents_with_sources,
};
use rootlight_resolve::{
    DEFAULT_CANDIDATE_LIMIT, MAX_RESOLUTION_WORK_LIMIT, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionEngine, ResolutionError, ResolutionLimits,
    ResolverFactContext,
};
use rootlight_search::{BuildBudget, LexicalDocument, LexicalIndex, SearchBudget, SearchError};
use rootlight_source::{SourceBudget, SourceError, SourceService};
pub use rootlight_source::{SourceEncoding, SourceReadOptions};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationControlError,
    GenerationManifestRecipe, GenerationMetadata, GenerationNeutralDigestError, GenerationResource,
    IdentityMismatchComponent, IdentityVerificationError, IdentityVerifiedGeneration,
    SharedGenerationError, export_shared_generation as encode_shared_generation,
    import_shared_generation as decode_shared_generation, shared_generation_source_set_hash,
};
pub use rootlight_storage::{
    SharedGenerationExpectation, SharedGenerationImport, SharedGenerationLimits,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot, VfsError};
use serde::{Deserialize, Serialize, ser::SerializeSeq};

const MAX_RETAINED_SOURCE_BYTES: usize = 512 * 1024 * 1024;
const DISCOVERY_PROGRESS_INTERVAL_FILES: u64 = 64;
const MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RETAINED_OPTIONAL_EXTENSIONS: usize = 10_000;
const MAX_RETAINED_OPTIONAL_EXTENSION_BYTES: usize = 16 * 1024 * 1024;
const RECOVERY_SMOKE_QUERY: &str = "__rootlight_startup_recovery_smoke__";
// Divide the generation-wide syntax-fact allowance across every admitted
// source so large repositories degrade per file instead of exhausting memory.
const MAX_FIRST_SLICE_STRUCTURAL_FACTS: usize = 1_048_576;
const MAX_TOTAL_MATERIALIZED_RESOLUTION_CANDIDATES: usize = 1_000_000;
const DURABLE_STAGING_FIXED_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;
const DURABLE_DISK_SAFETY_MARGIN_BYTES: u64 = 64 * 1024 * 1024;
// The measured large-repository profile reached 24.51 durable bytes per
// source byte. Rounding to 25 plus fixed and disk-safety margins keeps
// admission ahead of staging without pretending the estimate is exact.
#[cfg(test)]
const DURABLE_SOURCE_WRITE_AMPLIFICATION_FACTOR: u64 = 25;
// SQLite stores normalized fields across tables and indexes, so the streaming
// JSON size is multiplied before any durable file is created.
// The measured clean deep-analysis peak reached 45.97 bytes per source byte.
// A 48x preflight plus fixed small-repository overhead leaves bounded headroom
// before any parser or lowerer runs.
const GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR: u64 = 48;
const GENERATION_MEMORY_FIXED_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_FIRST_SLICE_GIT_CHANGE_PATHS: usize = 1_000;
const MAX_FIRST_SLICE_GIT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const HARD_MAX_FIRST_SLICE_GENERATIONS: usize = 8_193;
const MAX_SYNTAX_NODES: usize = 1_048_576;
const MAX_SYNTAX_DEPTH: usize = 128;
const MAX_STREAM_RECORDS_PER_FILE: usize = 1_048_576;
const MAX_STREAM_OUTPUT_BYTES_PER_FILE: usize = 512 * 1024 * 1024;
const MAX_STREAM_DIAGNOSTICS_PER_FILE: usize = 16_384;
const MAX_STREAM_DIAGNOSTIC_BYTES_PER_FILE: usize = 16 * 1024 * 1024;
const MAX_STREAM_STRING_BYTES_PER_FILE: usize = 128 * 1024 * 1024;
const MAX_REPORTED_MEMORY_BYTES_PER_FILE: usize = 512 * 1024 * 1024;
const MAX_LOGICAL_SNAPSHOT_COMPONENT_BYTES: usize = 16 * 1024 * 1024 * 1024;
const MAX_REPOSITORY_PATH_IDENTITY_BYTES: usize = 64 * 1024;
const MAX_RANDOM_ID_ATTEMPTS: usize = 8;
const GENERATED_HEADER_MAX_BYTES: usize = 8 * 1024;
const GENERATED_HEADER_MAX_LINES: usize = 64;
const PROVIDER_SET_SEED: &[u8] = b"rootlight.first-slice.providers/3";
const PROJECT_PROVIDER_SET_SEED: &[u8] = b"rootlight.first-slice.project-provider/7";
const PARSER_PROVIDER_SET_SEED: &[u8] = b"rootlight.first-slice.parser-providers/1";
const BUILD_CONTEXT_SEED: &[u8] = b"rootlight.first-slice.build-context/1";
const PROJECT_CONTEXT_SEED: &[u8] = b"rootlight.first-slice.project-context/1";
const PROJECT_PARTITION_DIAGNOSTIC_RESERVE: usize = 2;
const PROJECT_DIAGNOSTICS_TRUNCATED_CODE: &str = "project-adapter-diagnostics-truncated";
const PROJECT_DIAGNOSTICS_TRUNCATED_MESSAGE: &str =
    "additional project adapter diagnostics were omitted by the aggregate diagnostic limit";
const PROJECT_FACTS_TRUNCATED_CODE: &str = "project-adapter-facts-truncated";
const PROJECT_FACTS_TRUNCATED_MESSAGE: &str =
    "additional project semantic facts were omitted by aggregate resource limits";
const AGGREGATE_DIAGNOSTICS_TRUNCATED_CODE: &str = "aggregate-diagnostics-truncated";
const ANALYZER_BINARY_SEED: &[u8] = b"rootlight.first-slice.treesitter-structural/5";
const RESOLVER_BINARY_SEED: &[u8] = b"rootlight.first-slice.resolve/1";
const INCREMENTAL_PROVIDER_SEED: &[u8] = b"rootlight.first-slice.incremental-provider/1";
const LANGUAGE_DISPOSITION_PROVIDER_SEED: &[u8] = b"rootlight.first-slice.language-disposition/1";
const INCREMENTAL_UNIT_SEED: &str = "rootlight.first-slice.repository-unit";
const INCREMENTAL_FILE_UNIT_SEED: &str = "rootlight.first-slice.file-unit";
const PARSER_ARTIFACT_SEED: &str = "rootlight.first-slice.parser-artifact";
const PARSER_PASS_ID: &str = "first-slice.parser";
const LOWERING_PASS_ID: &str = "first-slice.lowering";
const RESOLVER_PASS_ID: &str = "first-slice.resolver";
const DERIVED_PASS_ID: &str = "first-slice.derived";
const SEARCH_PASS_ID: &str = "first-slice.search";
const GRAMMAR_REVISION_SEED: &[u8] = b"rootlight.first-slice.grammar-registry/2";
const COMPILER_CONTEXT_INPUT_SEED: &[u8] = b"rootlight.first-slice.compiler-context/1";
const SEARCH_REVISION_SEED: &[u8] = b"rootlight.first-slice.search-schema/1";
const DERIVED_PLAN_REVISION_SEED: &[u8] =
    b"rootlight.first-slice.incremental-plan/schema-1.0/graph-1";
// The isolated project host accepts only this semantic set. Tree-sitter has
// additional fallback grammars that must not be advertised for the host.
const PROJECT_ADAPTER_SUPPORT_LANGUAGES: [SemanticProjectLanguage; 10] = [
    SemanticProjectLanguage::Rust,
    SemanticProjectLanguage::TypeScript,
    SemanticProjectLanguage::JavaScript,
    SemanticProjectLanguage::Python,
    SemanticProjectLanguage::Go,
    SemanticProjectLanguage::Java,
    SemanticProjectLanguage::Cpp,
    SemanticProjectLanguage::CSharp,
    SemanticProjectLanguage::Php,
    SemanticProjectLanguage::C,
];

fn project_adapter_supports_language(language: &str) -> bool {
    PROJECT_ADAPTER_SUPPORT_LANGUAGES
        .into_iter()
        .any(|supported| supported.as_str() == language)
}

/// Maximum number of distinct source-free diagnostics retained in an index receipt.
pub const MAX_FIRST_SLICE_INDEX_DIAGNOSTICS: usize = 100;

/// One source-free diagnostic retained with a published generation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceIndexDiagnostic {
    /// Stable producer-defined diagnostic code.
    pub code: String,
    /// Bounded source-free diagnostic message.
    pub message: String,
}

/// Immutable generation-neutral identity of one complete logical snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceLogicalSnapshotIdentity {
    schema_version: String,
    hash: ContentHash,
}

impl FirstSliceLogicalSnapshotIdentity {
    fn new(hash: ContentHash) -> Self {
        Self {
            schema_version: LOGICAL_SNAPSHOT_SCHEMA_VERSION.to_owned(),
            hash,
        }
    }

    /// Returns the version of the canonical logical projection.
    #[must_use]
    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }

    /// Returns the checked numeric version accepted by the current service.
    #[must_use]
    pub fn schema_version_parts(&self) -> Option<(u16, u16)> {
        match self.schema_version.as_str() {
            LOGICAL_SNAPSHOT_SCHEMA_VERSION => Some((1, 0)),
            _ => None,
        }
    }

    /// Returns the generation-neutral logical snapshot digest.
    #[must_use]
    pub const fn hash(&self) -> ContentHash {
        self.hash
    }
}

impl<'de> Deserialize<'de> for FirstSliceLogicalSnapshotIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireIdentity {
            schema_version: String,
            hash: ContentHash,
        }

        let wire = WireIdentity::deserialize(deserializer)?;
        if wire.schema_version != LOGICAL_SNAPSHOT_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(
                "unsupported logical snapshot schema version",
            ));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            hash: wire.hash,
        })
    }
}

/// Bounded receipt for one first-slice generation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceIndexReceipt {
    /// Random local-UUID identity stable for aliases of this repository.
    ///
    /// The canonical-root digest is only an internal lookup key, not this
    /// public identity. Durable services persist the UUID with each generation.
    pub repository: RepositoryId,
    /// Immutable generation published into this service instance.
    pub generation: GenerationId,
    /// Prior generation in the same repository lineage, when present.
    pub parent: Option<GenerationId>,
    /// Regular inputs admitted by deterministic discovery.
    pub discovered_inputs: u64,
    /// Files and directories visited by bounded discovery.
    #[serde(default)]
    pub visited_entries: u64,
    /// Inputs omitted by stable discovery policy or resource classification.
    #[serde(default)]
    pub excluded_inputs: u64,
    /// Oversized regular inputs omitted by the configured per-file bound.
    #[serde(default)]
    pub oversized_inputs: u64,
    /// Inputs classified as binary and omitted from text analysis.
    #[serde(default)]
    pub binary_inputs: u64,
    /// Other inputs omitted by explicit discovery policy or safe-read rules.
    #[serde(default)]
    pub policy_excluded_inputs: u64,
    /// Files committed into normalized IR.
    pub indexed_files: u64,
    /// Semantic entities committed into normalized IR.
    pub entities: u64,
    /// Lexical documents committed into the generation-pinned reader.
    pub lexical_documents: u64,
    /// SQLite bytes allocated by the normalized generation oracle.
    pub oracle_allocated_bytes: u64,
    /// Conservative staging and publication reservation, including safety margin.
    #[serde(default)]
    pub estimated_disk_bytes: u64,
    /// Durable bytes retained for this generation after atomic publication.
    #[serde(default)]
    pub retained_durable_bytes: u64,
    /// Distinct source-free diagnostics retained in deterministic order.
    #[serde(default)]
    pub diagnostics: Vec<FirstSliceIndexDiagnostic>,
    /// Complete generation-neutral identity, absent when no supported sidecar is available.
    #[serde(default, skip_serializing)]
    pub logical_snapshot: Option<FirstSliceLogicalSnapshotIdentity>,
    /// End-to-end indexing time rounded up to microseconds.
    pub elapsed_micros: u64,
}

/// One committed generation plus confirmed durable bytes written by its operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceIndexCommit {
    receipt: FirstSliceIndexReceipt,
    written_bytes: u64,
    evidence: FirstSliceIndexOperationEvidence,
}

impl FirstSliceIndexCommit {
    /// Returns the published generation receipt.
    #[must_use]
    pub const fn receipt(&self) -> &FirstSliceIndexReceipt {
        &self.receipt
    }

    /// Returns cumulative bytes confirmed by durable writer boundaries.
    #[must_use]
    pub const fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    /// Returns final source-free reuse, rebuild, I/O, and memory evidence.
    #[must_use]
    pub const fn evidence(&self) -> &FirstSliceIndexOperationEvidence {
        &self.evidence
    }

    /// Separates the published receipt from its operation-local write metric.
    #[must_use]
    pub fn into_parts(self) -> (FirstSliceIndexReceipt, u64) {
        (self.receipt, self.written_bytes)
    }
}

/// Construction strategy used by one committed index operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceIndexOperationStrategy {
    /// No committed parent generation was available.
    Initial,
    /// Declared dependencies selected bounded parser-artifact reuse.
    DependencyDirected,
    /// Missing dependency evidence required a repository-wide rebuild.
    ConservativeRepositoryRebuild,
    /// The caller requested a complete rebuild without parent reuse.
    CleanRebuild,
    /// An identical retained generation was reactivated without rebuilding it.
    RetainedGeneration,
}

/// Final source-free reuse, rebuild, I/O, and memory evidence for one index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceIndexOperationEvidence {
    /// Construction strategy used by this operation.
    pub strategy: FirstSliceIndexOperationStrategy,
    /// Explicit reason fine-grained invalidation was abandoned, when applicable.
    pub fallback_reason: Option<FallbackReason>,
    /// Analysis units selected by the invalidation closure.
    pub invalidated_units: u64,
    /// Typed generation inputs changed from the parent snapshot.
    pub changed_inputs: u64,
    /// Authoritative file transitions observed by reconciliation.
    pub changed_files: u64,
    /// Source files whose immutable parser artifacts were reused.
    pub reused_files: u64,
    /// Source files parsed into fresh immutable parser artifacts.
    pub rebuilt_files: u64,
    /// Final normalized records retained through verified identity rebind.
    ///
    /// Rebind reconstructs generation-owned identities and references while
    /// avoiding analyzer and lowering recomputation.
    pub reused_facts: u64,
    /// Normalized facts rebuilt for the published generation.
    pub rebuilt_facts: u64,
    /// Existing artifact or generation bytes referenced by this operation.
    pub referenced_bytes: u64,
    /// Bytes confirmed by operation-owned durable writer boundaries.
    pub newly_written_bytes: u64,
    /// Conservative generation-memory reservation admitted for this operation.
    pub reserved_memory_bytes: u64,
    /// Retained generation-memory charge owned after successful publication.
    pub owned_memory_bytes: u64,
    /// Durable bytes retained for the resulting immutable generation.
    pub retained_durable_bytes: u64,
    /// Logical pre-work closure grouped by domain, pass, and cause.
    pub planned_fact_work: Vec<FirstSlicePlannedFactWork>,
    /// Actual normalized records retained or rebuilt by domain and pass.
    pub normalized_fact_work: Vec<FirstSliceNormalizedFactWork>,
}

/// Opaque durable recovery work split between startup-active and retained-history phases.
pub struct FirstSliceDeferredRestore {
    durable: Arc<DurableCatalog>,
    active_targets: Vec<FirstSliceRecoveryTarget>,
}

/// One repository whose last activated generation requires background recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceRecoveryTarget {
    repository: RepositoryId,
    generation: GenerationId,
    root_identity: ContentHash,
    activation_sequence: u64,
    global_activation_sequence: u64,
    published_generation_count: u64,
}

impl FirstSliceRecoveryTarget {
    /// Returns the durable repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the generation named by the newest durable activation marker.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }
}

/// Fully verified durable state awaiting one atomic in-memory installation.
pub struct FirstSliceRestoredState {
    generations: Vec<RestoredGeneration>,
}

impl FirstSliceDeferredRestore {
    /// Reports whether the deferred-open snapshot contains active restore work.
    ///
    /// Deferred open has already validated the bounded activation and generation
    /// manifests. Generation payloads remain unopened until
    /// [`Self::restore_active`] performs their integrity-checked recovery.
    ///
    /// # Errors
    ///
    /// This cached lookup does not currently fail.
    pub fn has_active_restore_work(&self) -> Result<bool, FirstSliceError> {
        Ok(!self.active_targets.is_empty())
    }

    /// Lists active repositories in most-recently-activated order.
    ///
    /// Deferred open has already collected this bounded metadata. Generation
    /// payloads remain unopened so the daemon can publish recovery work before
    /// reconstruction.
    ///
    /// # Errors
    ///
    /// This cached lookup does not currently fail.
    pub fn active_targets(&self) -> Result<Vec<FirstSliceRecoveryTarget>, FirstSliceError> {
        Ok(self.active_targets.clone())
    }

    /// Restores the newest valid generation for one exact repository.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::restore_active`].
    pub fn restore_active_repository(
        &self,
        repository: RepositoryId,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceRestoredState, FirstSliceError> {
        self.durable
            .restore_active_repository(repository, cancellation)
            .map(|generations| FirstSliceRestoredState { generations })
    }

    /// Verifies retained rollback generations for one repository without repair writes.
    ///
    /// The read-only boundary permits this nonessential validation to run after
    /// active generations and write admission become available. Newly published
    /// immutable generations may be absent from this snapshot and will be
    /// recovered normally after a later restart.
    ///
    /// # Errors
    ///
    /// Returns the same bounded integrity, cancellation, and retention failures
    /// as [`Self::restore_active_repository`].
    pub fn restore_retained_repository(
        &self,
        repository: RepositoryId,
        excluded: &BTreeSet<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceRestoredState, FirstSliceError> {
        self.durable
            .restore_retained_repository(repository, excluded, cancellation)
            .map(|generations| FirstSliceRestoredState { generations })
    }

    /// Loads and verifies every retained activation-marked generation.
    ///
    /// The work is intentionally separate from service construction so daemon
    /// readiness and lifecycle control do not depend on catalog size.
    ///
    /// # Errors
    ///
    /// Returns a typed durable catalog, integrity, cancellation, or retention
    /// failure.
    pub fn restore(
        &self,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceRestoredState, FirstSliceError> {
        self.durable
            .restore(cancellation)
            .map(|generations| FirstSliceRestoredState { generations })
    }

    /// Loads only the newest valid generation for each retained repository.
    ///
    /// This is the bounded startup path. Older rollback generations remain
    /// immutable on disk and can be added with
    /// [`Self::restore_excluding`] after interactive last-good reads resume.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::restore`].
    pub fn restore_active(
        &self,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceRestoredState, FirstSliceError> {
        self.durable
            .restore_active(cancellation)
            .map(|generations| FirstSliceRestoredState { generations })
    }

    /// Loads retained rollback generations not already installed.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::restore`].
    pub fn restore_excluding(
        &self,
        excluded: &BTreeSet<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceRestoredState, FirstSliceError> {
        self.durable
            .restore_excluding(excluded, cancellation)
            .map(|generations| FirstSliceRestoredState { generations })
    }
}

impl FirstSliceRestoredState {
    /// Returns the exact immutable generations carried by this restore batch.
    #[must_use]
    pub fn generation_ids(&self) -> BTreeSet<GenerationId> {
        self.generations
            .iter()
            .map(|generation| generation.receipt.generation)
            .collect()
    }
}

/// Source-free adapter facts for production support evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSupportAdapter {
    /// Stable provider label.
    pub name: String,
    /// Closed language labels handled by the provider.
    pub languages: Vec<String>,
    /// Whether the provider executes outside the daemon process.
    pub isolated: bool,
}

/// Installed source-language detection and analysis capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSupportLanguageCapability {
    /// Canonical normalized language label.
    pub language: String,
    /// Audited filename suffixes, including the leading dot.
    pub suffixes: Vec<String>,
    /// Accepted source-language aliases.
    pub aliases: Vec<String>,
    /// Installed detector families.
    pub detectors: Vec<String>,
    /// Highest installed analysis tier.
    pub maximum_tier: String,
    /// Installed analyzer or bounded fallback labels.
    pub analyzers: Vec<String>,
}

/// Source-free repository facts for production support evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSupportRepository {
    /// Opaque repository identity.
    pub repository: RepositoryId,
    /// Closed indexed language labels.
    pub languages: Vec<String>,
    /// Closed completed analysis-tier labels.
    pub tiers: Vec<String>,
    /// Files retained by the active generation.
    pub files: u64,
    /// Symbols retained by the active generation.
    pub symbols: u64,
    /// Relationships retained by the active generation.
    pub relationships: u64,
    /// Immutable generations currently retained for this repository.
    pub generation_count: u32,
    /// Reconciled physical bytes attributed to this repository.
    pub storage_bytes: Option<u64>,
    /// Active immutable generation bytes attributed to this repository.
    pub active_generation_bytes: Option<u64>,
    /// Direct active-predecessor bytes attributed to this repository.
    pub predecessor_generation_bytes: Option<u64>,
    /// Other retained immutable generation bytes attributed to this repository.
    pub other_retained_generation_bytes: Option<u64>,
    /// Source-pool bytes referenced by retained generations.
    pub source_pool_bytes: Option<u64>,
    /// Source-pool bytes referenced by multiple retained generations.
    pub shared_source_bytes: Option<u64>,
    /// Unpublished temporary bytes attributed to this repository.
    pub temporary_bytes: Option<u64>,
    /// Reclaimable bytes attributed to this repository.
    pub reclaimable_bytes: Option<u64>,
    /// Live durable metadata bytes attributed to this repository.
    pub repository_overhead_bytes: Option<u64>,
    /// Repository-budget bytes reserved by in-flight publications.
    pub inflight_reservation_bytes: Option<u64>,
    /// Current repository-budget headroom after in-flight reservations.
    pub repository_headroom_bytes: Option<u64>,
}

/// Source-free immutable generation facts for production support evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSupportGeneration {
    /// Repository owning the generation.
    pub repository: RepositoryId,
    /// Immutable generation identity.
    pub generation: GenerationId,
    /// Physical bytes retained by this generation's durable state.
    pub disk_bytes: u64,
    /// Prior generation in the same repository lineage, when retained.
    pub parent: Option<GenerationId>,
    /// Whether this generation is currently active.
    pub active: bool,
}

/// Authoritative repository-registration capacity for production support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceRepositoryCapacityInventory {
    /// Registrations currently charged to repository admission.
    pub registered_repository_count: u32,
    /// Admitted registrations that are not yet active.
    pub pending_repository_count: u32,
    /// Committed registrations with one queryable active generation.
    pub active_repository_count: u32,
    /// Failed registrations retained by the service.
    pub failed_repository_count: u32,
    /// Registrations protected by an explicit durable pin.
    pub pinned_repository_count: u32,
    /// Committed registrations removable through explicit repository deletion.
    pub reclaimable_repository_count: u32,
    /// Whether explicit durable repository pinning is implemented.
    pub pinning_supported: bool,
    /// User-configured repository registration ceiling.
    pub configured_maximum_repositories: u32,
    /// Effective ceiling after generation-retention capacity is applied.
    pub effective_maximum_repositories: u32,
    /// Registration slots available through the same arithmetic as admission.
    pub repository_headroom: u32,
}

/// Bounded source-free snapshot consumed by the daemon support bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSupportInventory {
    /// Available parser and project-analysis providers.
    pub adapters: Vec<FirstSliceSupportAdapter>,
    /// Authoritative installed language detection and analysis capabilities.
    pub languages: Vec<FirstSliceSupportLanguageCapability>,
    /// Active repository summaries.
    pub repositories: Vec<FirstSliceSupportRepository>,
    /// Authoritative repository-registration counts and admission headroom.
    pub repository_capacity: FirstSliceRepositoryCapacityInventory,
    /// Retained immutable generation summaries.
    pub generations: Vec<FirstSliceSupportGeneration>,
    /// Stable normalized generation format.
    pub generation_format: String,
    /// Total allocated bytes reported by retained generation receipts.
    pub generation_disk_bytes: u64,
    /// Total physical bytes retained by immutable generation trees, when measured.
    pub total_storage_bytes: Option<u64>,
    /// Physical bytes stored once and referenced by multiple generation trees.
    pub shared_bytes: Option<u64>,
    /// Physical bytes eligible for safe reclamation.
    pub reclaimable_bytes: Option<u64>,
    /// Physical bytes protected by an explicit durable pin.
    pub pinned_bytes: Option<u64>,
    /// Immutable bytes owned by active generations.
    pub active_generation_bytes: Option<u64>,
    /// Immutable bytes owned by direct active predecessors.
    pub predecessor_generation_bytes: Option<u64>,
    /// Immutable bytes owned by other retained generations.
    pub other_retained_generation_bytes: Option<u64>,
    /// Physical source-pool bytes referenced by retained generations.
    pub source_pool_bytes: Option<u64>,
    /// Bytes currently owned by unpublished durable staging trees.
    pub unreclaimed_temporary_bytes: u64,
    /// Live durable marker and metadata bytes.
    pub repository_overhead_bytes: Option<u64>,
    /// Isolated corrupt or untrusted durable bytes.
    pub quarantine_bytes: Option<u64>,
    /// Whether durable pinning is implemented by this installation.
    pub pinning_supported: bool,
    /// Catalog-budget bytes reserved by in-flight publications.
    pub inflight_catalog_reservation_bytes: Option<u64>,
    /// Repository-budget bytes reserved by in-flight publications.
    pub inflight_repository_reservation_bytes: Option<u64>,
    /// Free bytes on the durable repository volume when persistence is enabled.
    pub disk_margin_bytes: Option<u64>,
    /// Bytes remaining after configured catalog and filesystem admission margins.
    pub admission_margin_bytes: Option<u64>,
    /// Minimum current repository-budget headroom.
    pub repository_headroom_bytes: Option<u64>,
    /// Current durable-catalog headroom after in-flight reservations.
    pub catalog_headroom_bytes: Option<u64>,
    /// Current filesystem headroom after minimum-free and reservations.
    pub filesystem_headroom_bytes: Option<u64>,
    /// Provenance of the physical storage accounting.
    pub storage_accounting_state: FirstSliceStorageAccountingState,
    /// Effective maximum physical bytes per repository.
    pub maximum_repository_storage_bytes: u64,
    /// Effective maximum physical bytes in the durable catalog.
    pub maximum_catalog_storage_bytes: u64,
    /// Effective minimum free bytes preserved on the durable volume.
    pub minimum_free_disk_bytes: u64,
    /// Effective maximum retained generations per repository.
    pub effective_retention_generations: u32,
    /// Effective source reservation multiplier.
    pub source_reservation_factor: u64,
    /// Effective semantic-oracle reservation multiplier.
    pub oracle_reservation_factor: u64,
}

/// Provenance of storage measurements in a support snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceStorageAccountingState {
    /// Physical accounting is unavailable without a durable storage read.
    Unavailable,
    /// Physical accounting was reconciled from Rootlight-owned mutations.
    Reconciled,
    /// Physical accounting was rebuilt by a content-verifying durable scan.
    VerifiedScan,
}

/// Immutable storage settings used by durable recovery, admission, and support evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceStoragePolicy {
    config: ConfigSnapshot,
    maximum_repository_bytes: u64,
    maximum_catalog_bytes: u64,
    maximum_repositories: u32,
    retained_generations: usize,
    minimum_free_disk_bytes: u64,
    source_reservation_factor: u64,
    oracle_reservation_factor: u64,
}

impl FirstSliceStoragePolicy {
    /// Builds a durable policy from an already resolved configuration snapshot.
    #[must_use]
    pub fn from_config(config: ConfigSnapshot, legacy_retained_generations: usize) -> Self {
        if config.version() < CONFIG_VERSION_1_2 {
            return Self::legacy_with_config(legacy_retained_generations, config);
        }
        let storage = config.storage();
        Self {
            maximum_repositories: config.maximum_repositories(),
            config,
            maximum_repository_bytes: storage.maximum_repository_bytes,
            maximum_catalog_bytes: storage.maximum_catalog_bytes,
            retained_generations: usize::from(storage.retained_generations),
            minimum_free_disk_bytes: storage.minimum_free_disk_bytes,
            source_reservation_factor: u64::from(storage.source_reservation_factor),
            oracle_reservation_factor: u64::from(storage.oracle_reservation_factor),
        }
    }

    /// Resolves an optional bounded user layer into the effective storage policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Configuration`] when the layer is invalid.
    pub fn resolve_user_config(
        contents: Option<&str>,
        legacy_retained_generations: usize,
    ) -> Result<Self, FirstSliceError> {
        let layer = contents.map(|contents| ConfigLayer {
            source: ConfigSource::User,
            contents,
        });
        let config = ConfigSnapshot::resolve(layer.as_slice())
            .map_err(|_| FirstSliceError::Configuration)?;
        Ok(Self::from_config(config, legacy_retained_generations))
    }

    fn legacy(retained_generations: usize) -> Self {
        let config = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::Defaults,
            contents: "version = \"1.1\"",
        }])
        .expect("the embedded compatibility configuration is valid");
        Self::legacy_with_config(retained_generations, config)
    }

    fn legacy_with_config(retained_generations: usize, config: ConfigSnapshot) -> Self {
        Self {
            config,
            maximum_repository_bytes: u64::MAX,
            maximum_catalog_bytes: u64::MAX,
            maximum_repositories: MAXIMUM_REPOSITORIES,
            retained_generations,
            minimum_free_disk_bytes: 0,
            source_reservation_factor: u64::from(
                rootlight_config::DEFAULT_SOURCE_RESERVATION_FACTOR,
            ),
            oracle_reservation_factor: u64::from(
                rootlight_config::DEFAULT_ORACLE_RESERVATION_FACTOR,
            ),
        }
    }

    /// Returns the configured complete-generation retention count.
    #[must_use]
    pub const fn retained_generations(&self) -> usize {
        self.retained_generations
    }

    fn effective_maximum_repositories(
        &self,
        actual_retained_generations: usize,
    ) -> Result<usize, FirstSliceError> {
        let configured =
            usize::try_from(self.maximum_repositories).map_err(|_| FirstSliceError::Limits)?;
        maximum_repositories_for_retention(actual_retained_generations, configured)
    }

    #[cfg(test)]
    const fn minimum_free_disk_bytes(&self) -> u64 {
        self.minimum_free_disk_bytes
    }

    #[cfg(test)]
    const fn maximum_repository_bytes(&self) -> u64 {
        self.maximum_repository_bytes
    }

    #[cfg(test)]
    const fn source_reservation_factor(&self) -> u64 {
        self.source_reservation_factor
    }
}

/// Coarse source-free stage reported while preparing an index generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceIndexStage {
    /// Deterministic repository discovery and incremental correlation completed.
    Discovery,
    /// Stable source snapshots were captured.
    Snapshot,
    /// Bounded parser and adapter analysis completed.
    Analysis,
    /// Per-file normalized documents were merged.
    Merge,
    /// Verified durable or ephemeral generation state was constructed.
    Persistence,
    /// The generation-pinned lexical index was built.
    Search,
}

/// Monotonic coarse progress for one generation preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceIndexProgress {
    /// Completed coarse stage.
    pub stage: FirstSliceIndexStage,
    /// Completed coarse units.
    pub completed: u64,
    /// Fixed total coarse units for this preparation.
    pub total: u64,
    /// Source files examined so far.
    pub files_examined: u64,
    /// Source bytes examined so far.
    pub bytes_examined: u64,
    /// Durable bytes written so far.
    pub written_bytes: u64,
}

impl FirstSliceIndexProgress {
    const TOTAL: u64 = 6;

    const fn observed(
        stage: FirstSliceIndexStage,
        completed: u64,
        files_examined: u64,
        bytes_examined: u64,
        written_bytes: u64,
    ) -> Self {
        Self {
            stage,
            completed,
            total: Self::TOTAL,
            files_examined,
            bytes_examined,
            written_bytes,
        }
    }
}

/// Receipts for one completed structural-first semantic refinement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceTwoStageIndexReceipt {
    structural: FirstSliceIndexReceipt,
    semantic: FirstSliceIndexReceipt,
}

impl FirstSliceTwoStageIndexReceipt {
    /// Returns the first queryable structural generation.
    #[must_use]
    pub const fn structural(&self) -> &FirstSliceIndexReceipt {
        &self.structural
    }

    /// Returns the atomic semantic refinement generation.
    #[must_use]
    pub const fn semantic(&self) -> &FirstSliceIndexReceipt {
        &self.semantic
    }
}

/// Portable source-free bundle exported from one retained immutable generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSharedGenerationExport {
    repository: RepositoryId,
    generation: GenerationId,
    source_set_hash: ContentHash,
    bundle: Vec<u8>,
}

impl FirstSliceSharedGenerationExport {
    /// Returns the repository bound into the bundle.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation bound into the bundle.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the generation-independent source-set identity.
    #[must_use]
    pub const fn source_set_hash(&self) -> ContentHash {
        self.source_set_hash
    }

    /// Returns the canonical portable bundle bytes.
    #[must_use]
    pub fn bundle(&self) -> &[u8] {
        &self.bundle
    }
}

/// Bounded repository identity and durable-capacity admission for one index.
#[derive(Debug)]
pub struct FirstSliceIndexAdmission {
    /// Stable identity reserved for the canonical repository root.
    pub repository: RepositoryId,
    /// Canonical root identity retained for durable registration recovery.
    pub root_identity: ContentHash,
    /// Active generation from which the admitted operation will derive.
    pub parent: Option<GenerationId>,
    /// Conservative upper bound reserved for durable staging and publication.
    pub estimated_disk_bytes: u64,
    reservation_inserted: bool,
    storage_admission: DurableStorageAdmissionState,
}

#[derive(Debug)]
enum DurableStorageAdmissionState {
    Deferred,
    Reserved(Option<DurableStorageReservation>),
}

#[derive(Clone, Copy)]
enum DurableStorageAdmissionTiming {
    Immediate,
    BeforePreparation,
}

impl FirstSliceIndexAdmission {
    /// Returns whether this admission created a new catalog reservation.
    #[must_use]
    pub const fn created_repository_reservation(&self) -> bool {
        self.reservation_inserted
    }

    /// Returns a source-free copy that omits the process-local storage token.
    #[must_use]
    pub const fn metadata(&self) -> FirstSliceIndexAdmissionMetadata {
        FirstSliceIndexAdmissionMetadata {
            repository: self.repository,
            root_identity: self.root_identity,
            parent: self.parent,
            estimated_disk_bytes: self.estimated_disk_bytes,
        }
    }
}

/// Source-free admission facts safe to copy into daemon lifecycle metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceIndexAdmissionMetadata {
    /// Stable reserved repository identity.
    pub repository: RepositoryId,
    /// Canonical root identity.
    pub root_identity: ContentHash,
    /// Active parent observed at admission.
    pub parent: Option<GenerationId>,
    /// Conservative initial durable reservation.
    pub estimated_disk_bytes: u64,
}

struct StorageReservationGuard {
    reservation: Option<DurableStorageReservation>,
}

impl StorageReservationGuard {
    fn transfer(mut self) -> Option<DurableStorageReservation> {
        self.reservation.take()
    }
}

impl Drop for StorageReservationGuard {
    fn drop(&mut self) {}
}

/// Evidence-backed semantic stitch between two active repository generations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceCrossRepositoryLink {
    /// Repository that owns the requested target symbol.
    pub target_repository: RepositoryId,
    /// Active generation that owns the requested target symbol.
    pub target_generation: GenerationId,
    /// Relation family established by the source occurrence.
    pub family: RelationFamily,
    /// Calibrated confidence retained from the source occurrence.
    pub confidence: u16,
    /// Direct immutable source evidence for the cross-repository hop.
    pub source_refs: Vec<SourceRef>,
}

/// Provider family that produced one durable generation activation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstSliceIndexProvider {
    /// A legacy activation did not persist its provider family.
    #[default]
    Unknown,
    /// Built-in structural parsing and lowering.
    TreeSitter,
    /// Installed project-level semantic analyzer.
    ProjectAnalyzer,
}

/// Durable operation identity bound to one generation activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceOperationContext {
    /// Durable journal operation that authorized publication.
    pub operation: OperationId,
    /// Source-redacted wall-clock start time captured by the daemon.
    pub started_unix_ms: u64,
    /// Source-free provider family retained for terminal diagnostics.
    #[serde(default)]
    pub provider: FirstSliceIndexProvider,
}

/// Restored durable operation-to-generation publication mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceDurableOperation {
    /// Durable journal operation.
    pub operation: OperationId,
    /// Source-redacted wall-clock start time captured by the daemon.
    pub started_unix_ms: u64,
    /// Source-free provider family that produced the generation.
    pub provider: FirstSliceIndexProvider,
    /// Immutable generation receipt published by the operation.
    pub receipt: FirstSliceIndexReceipt,
}

/// Construction strategy used for one process-local first-slice generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FirstSliceBuildStrategy {
    /// No committed parent baseline existed.
    Initial,
    /// Declared dependencies selected bounded parser-artifact reuse.
    ///
    /// Generation-bound lowering, resolution, storage, and search still rebuild.
    DependencyDirected,
    /// Missing fine-grained declarations required a complete repository rebuild.
    ConservativeRepositoryRebuild,
    /// The caller requested a complete rebuild without parent reuse.
    CleanRebuild,
}

/// Count of changed typed inputs in one conservative semantic class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceInputChangeCount {
    class: ChangeClass,
    inputs: u64,
}

impl FirstSliceInputChangeCount {
    /// Returns the conservative semantic class.
    #[must_use]
    pub const fn class(self) -> ChangeClass {
        self.class
    }

    /// Returns the number of changed typed inputs in this class.
    #[must_use]
    pub const fn inputs(self) -> u64 {
        self.inputs
    }
}

/// Count of authoritative file transitions in one canonical class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceFileChangeCount {
    kind: FileChangeKind,
    files: u64,
}

impl FirstSliceFileChangeCount {
    /// Returns the authoritative file-transition class.
    #[must_use]
    pub const fn kind(self) -> FileChangeKind {
        self.kind
    }

    /// Returns the number of files in this class.
    #[must_use]
    pub const fn files(self) -> u64 {
        self.files
    }
}

/// Source-free incremental planning evidence retained with one generation.
///
/// The evidence records exact parser-artifact actions separately from fresh
/// lowering and preserves the bounded source-free invalidation trace.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceIncrementalEvidence {
    strategy: FirstSliceBuildStrategy,
    input_changes: Vec<FirstSliceInputChangeCount>,
    file_changes: Vec<FirstSliceFileChangeCount>,
    hashed_files: u64,
    invalidated_domains: Vec<FactDomain>,
    invalidated_units: u64,
    fallback_reason: Option<FallbackReason>,
    trace_entries: u64,
    #[serde(default)]
    invalidation_trace: Vec<TraceEntry>,
    parsed_files: u64,
    reused_parser_artifacts: u64,
    #[serde(default)]
    reused_parser_artifact_bytes: u64,
    #[serde(default)]
    reused_durable_artifact_bytes: u64,
    lowered_files: u64,
    #[serde(default)]
    reused_normalized_facts: u64,
    #[serde(default)]
    rebuilt_normalized_facts: u64,
    #[serde(default)]
    planned_fact_work: Vec<FirstSlicePlannedFactWork>,
    #[serde(default)]
    normalized_fact_work: Vec<FirstSliceNormalizedFactWork>,
    structural_cache_retained: bool,
}

/// Work disposition selected before generation-bound fact construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstSliceFactWorkDisposition {
    /// The dependency closure requires this logical scope to run.
    Rebuild,
    /// The dependency closure excludes this logical scope from rebuilding.
    Reuse,
}

/// Source-free cause attributed to planned or completed incremental work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstSliceFactWorkCause {
    /// No committed parent existed, so every fact is initially constructed.
    InitialGeneration,
    /// A declared dependency edge selected the scope.
    DependencyClosure,
    /// The planner excluded the scope, or verified normalized records survived rebind.
    CompleteDependencyMatch,
    /// A conservative fallback selected a repository-wide rebuild.
    ConservativeFallback,
    /// The caller explicitly requested a complete rebuild.
    UserRequestedCleanRebuild,
    /// Generation ownership requires fresh normalized records from parser facts.
    GenerationBoundLowering,
    /// Repository-wide semantic resolution produced or retained the records.
    Resolution,
}

/// Pre-work logical scope and cost selected by incremental planning.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSlicePlannedFactWork {
    disposition: FirstSliceFactWorkDisposition,
    domain: FactDomain,
    provider_pass: String,
    cause: FirstSliceFactWorkCause,
    files: u64,
    analysis_units: u64,
    #[serde(skip)]
    file_ids: BTreeSet<FileId>,
    #[serde(skip)]
    analysis_unit_ids: BTreeSet<AnalysisUnitId>,
}

impl FirstSlicePlannedFactWork {
    /// Returns whether this logical scope was selected to rebuild or reuse.
    #[must_use]
    pub const fn disposition(&self) -> FirstSliceFactWorkDisposition {
        self.disposition
    }

    /// Returns the logical incremental fact domain.
    #[must_use]
    pub const fn domain(&self) -> FactDomain {
        self.domain
    }

    /// Returns the source-free provider pass selected for this work.
    #[must_use]
    pub fn provider_pass(&self) -> &str {
        &self.provider_pass
    }

    /// Returns the dependency or construction cause.
    #[must_use]
    pub const fn cause(&self) -> FirstSliceFactWorkCause {
        self.cause
    }

    /// Returns distinct files represented by this scope.
    #[must_use]
    pub const fn files(&self) -> u64 {
        self.files
    }

    /// Returns distinct analysis units represented by this scope.
    #[must_use]
    pub const fn analysis_units(&self) -> u64 {
        self.analysis_units
    }

    /// Iterates canonical file identities represented by this in-process result.
    pub fn file_ids(&self) -> impl Iterator<Item = FileId> + '_ {
        self.file_ids.iter().copied()
    }

    /// Iterates canonical fact identities for represented analysis units.
    pub fn analysis_unit_ids(&self) -> impl Iterator<Item = FactId> + '_ {
        self.analysis_unit_ids
            .iter()
            .copied()
            .map(AnalysisUnitId::as_fact_id)
    }
}

/// Normalized records retained by verified rebind or rebuilt during construction.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceNormalizedFactWork {
    domain: IrFactDomain,
    provider_pass: String,
    cause: FirstSliceFactWorkCause,
    files: u64,
    facts: u64,
    #[serde(skip)]
    file_ids: BTreeSet<FileId>,
}

impl FirstSliceNormalizedFactWork {
    /// Returns the normalized IR record domain.
    #[must_use]
    pub const fn domain(&self) -> IrFactDomain {
        self.domain
    }

    /// Returns the source-free provider pass that produced the records.
    #[must_use]
    pub fn provider_pass(&self) -> &str {
        &self.provider_pass
    }

    /// Returns why the records were rebound or rebuilt.
    #[must_use]
    pub const fn cause(&self) -> FirstSliceFactWorkCause {
        self.cause
    }

    /// Returns distinct files directly evidenced by the records.
    #[must_use]
    pub const fn files(&self) -> u64 {
        self.files
    }

    /// Returns records actually constructed in this domain.
    #[must_use]
    pub const fn facts(&self) -> u64 {
        self.facts
    }

    /// Iterates canonical file identities evidenced by this in-process result.
    pub fn file_ids(&self) -> impl Iterator<Item = FileId> + '_ {
        self.file_ids.iter().copied()
    }

    /// Iterates canonical fact identities for evidenced file analysis units.
    pub fn analysis_unit_ids(&self) -> impl Iterator<Item = FactId> + '_ {
        self.file_ids
            .iter()
            .copied()
            .map(file_analysis_unit)
            .map(AnalysisUnitId::as_fact_id)
    }
}

/// Response-bounded view of one durable source-free invalidation trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstSliceInvalidationTraceView {
    version: String,
    entries: Vec<TraceEntry>,
    total_entries: u64,
    complete: bool,
}

impl FirstSliceInvalidationTraceView {
    /// Returns the incremental trace schema version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the response-bounded canonical trace prefix.
    #[must_use]
    pub fn entries(&self) -> &[TraceEntry] {
        &self.entries
    }

    /// Returns the complete durable entry count before response bounding.
    #[must_use]
    pub const fn total_entries(&self) -> u64 {
        self.total_entries
    }

    /// Reports whether the response contains every durable trace entry.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Serializes the deterministic trace view for the local protocol.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Incremental`] if canonical serialization
    /// unexpectedly fails.
    pub fn canonical_json(&self) -> Result<Vec<u8>, FirstSliceError> {
        serde_json::to_vec(self).map_err(|_| FirstSliceError::Incremental)
    }
}

impl FirstSliceIncrementalEvidence {
    /// Returns the actual build strategy used for this generation.
    #[must_use]
    pub const fn strategy(&self) -> FirstSliceBuildStrategy {
        self.strategy
    }

    /// Returns changed typed-input counts in canonical class order.
    #[must_use]
    pub fn input_changes(&self) -> &[FirstSliceInputChangeCount] {
        &self.input_changes
    }

    /// Returns authoritative file-transition counts in canonical class order.
    #[must_use]
    pub fn file_changes(&self) -> &[FirstSliceFileChangeCount] {
        &self.file_changes
    }

    /// Returns files whose bytes were hashed by the authoritative reconcile.
    #[must_use]
    pub const fn hashed_files(&self) -> u64 {
        self.hashed_files
    }

    /// Returns invalidated fact domains in canonical order.
    #[must_use]
    pub fn invalidated_domains(&self) -> &[FactDomain] {
        &self.invalidated_domains
    }

    /// Returns analysis units selected for rebuilding.
    #[must_use]
    pub const fn invalidated_units(&self) -> u64 {
        self.invalidated_units
    }

    /// Returns why fine-grained planning fell back, when it did.
    #[must_use]
    pub const fn fallback_reason(&self) -> Option<FallbackReason> {
        self.fallback_reason
    }

    /// Returns the number of bounded source-free trace entries produced.
    #[must_use]
    pub const fn trace_entries(&self) -> u64 {
        self.trace_entries
    }

    /// Returns the complete bounded source-free invalidation decisions.
    #[must_use]
    pub fn invalidation_trace(&self) -> &[TraceEntry] {
        &self.invalidation_trace
    }

    /// Returns files whose concrete syntax was parsed in this generation.
    #[must_use]
    pub const fn parsed_files(&self) -> u64 {
        self.parsed_files
    }

    /// Returns exact-match parser artifacts reused from the parent generation.
    #[must_use]
    pub const fn reused_parser_artifacts(&self) -> u64 {
        self.reused_parser_artifacts
    }

    /// Returns logical parser-artifact bytes referenced from the parent generation.
    #[must_use]
    pub const fn reused_parser_artifact_bytes(&self) -> u64 {
        self.reused_parser_artifact_bytes
    }

    /// Returns immutable recovery-artifact bytes referenced from durable storage.
    #[must_use]
    pub const fn reused_durable_artifact_bytes(&self) -> u64 {
        self.reused_durable_artifact_bytes
    }

    /// Returns files lowered into fresh generation-bound normalized IR.
    ///
    /// Parser artifact reuse does not imply normalized-IR or resolver reuse.
    #[must_use]
    pub const fn lowered_files(&self) -> u64 {
        self.lowered_files
    }

    /// Returns exact final normalized records retained from verified chunk rebind.
    ///
    /// Rebind reconstructs generation-owned identities and references but
    /// avoids analyzer and lowering recomputation. Records later replaced or
    /// removed by project composition or resolution do not increase this count.
    #[must_use]
    pub const fn reused_normalized_facts(&self) -> u64 {
        self.reused_normalized_facts
    }

    /// Returns fresh normalized records built for this generation.
    #[must_use]
    pub const fn rebuilt_normalized_facts(&self) -> u64 {
        self.rebuilt_normalized_facts
    }

    /// Returns the pre-work logical closure grouped by domain, pass, and cause.
    #[must_use]
    pub fn planned_fact_work(&self) -> &[FirstSlicePlannedFactWork] {
        &self.planned_fact_work
    }

    /// Returns rebound and rebuilt records grouped by IR domain, pass, and cause.
    #[must_use]
    pub fn normalized_fact_work(&self) -> &[FirstSliceNormalizedFactWork] {
        &self.normalized_fact_work
    }

    /// Reports whether this generation retained parser artifacts for a successor.
    #[must_use]
    pub const fn structural_cache_retained(&self) -> bool {
        self.structural_cache_retained
    }
}

/// Freshness observed by the last committed index operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FirstSliceObservedFreshness {
    /// The generation completed the latest committed authoritative scan.
    CurrentAtLastAuthoritativeScan,
    /// Structural facts are current while semantic refinement is pending.
    PendingSemanticRefinement,
    /// A later committed generation superseded this generation.
    Superseded,
}

/// Publication shape available to the current first-slice service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FirstSlicePublicationMode {
    /// Structural and semantic facts activate together inside this process.
    ProcessLocalSingleStage,
    /// Structural and semantic facts share one immutable durable activation.
    DurableSingleStage,
    /// A durable structural generation is queryable before semantic refinement.
    DurableStructuralStage,
    /// A durable semantic generation atomically refines its structural parent.
    DurableSemanticRefinement,
}

/// Availability of structural-first semantic refinement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FirstSliceTwoStageAvailability {
    /// Durable atomic generation publication is not yet authorized.
    UnavailableWithoutDurablePublication,
    /// Durable publication exists, but semantic refinement remains single-stage.
    UnavailableWithoutSemanticRefinement,
    /// The structural stage is published and semantic refinement is pending.
    StructuralPublished,
    /// The semantic refinement generation is published.
    SemanticRefinementPublished,
}

/// Honest structural and semantic freshness for one retained generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FirstSliceFreshnessStatus {
    /// Structural facts relative to the latest committed scan.
    pub structural: FirstSliceObservedFreshness,
    /// Semantic facts relative to the latest committed scan.
    pub semantic: FirstSliceObservedFreshness,
    /// Activation shape implemented by the service.
    pub publication: FirstSlicePublicationMode,
    /// Explicit two-stage capability state.
    pub two_stage: FirstSliceTwoStageAvailability,
}

/// Checked repository and generation correlation for one first-slice query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceGenerationContext {
    /// Repository owning the immutable generation.
    pub repository: RepositoryId,
    /// Selected immutable generation.
    pub generation: GenerationId,
    /// Optional predecessor generation.
    pub parent: Option<GenerationId>,
    /// Repository generation active when selection was resolved.
    pub active_generation: GenerationId,
    /// Optional predecessor of the resolved active generation.
    pub active_parent: Option<GenerationId>,
    /// Whether this generation is currently active for its repository.
    pub active: bool,
    /// Publication receipt retained with the generation.
    pub receipt: FirstSliceIndexReceipt,
}

/// Repository state label reported while a generation is active and queryable.
const REPOSITORY_STATE_READY: &str = "ready";

/// One language-scoped coverage entry for a repository generation.
///
/// The tier and status labels mirror the daemon's coverage aggregation but are
/// carried as stable strings so the wire protocol does not need new enums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryCoverageEntryDto {
    /// Stable normalized language label.
    pub language: String,
    /// Aggregate analysis tier label, such as `tier_a`.
    pub tier: String,
    /// Aggregate completeness label, such as `complete` or `bounded`.
    pub status: String,
    /// Inputs admitted by deterministic discovery.
    pub discovered_files: u64,
    /// Files committed into normalized IR.
    pub indexed_files: u64,
}

/// One repository entry in the bounded repository list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryListEntryDto {
    /// Process-local repository identity.
    pub repository: RepositoryId,
    /// Active immutable generation for the repository.
    pub active_generation: GenerationId,
    /// Indexed languages.
    pub languages: Vec<String>,
    /// Structural freshness label, such as `current`.
    pub structural_freshness: String,
    /// Semantic freshness label, such as `current`.
    pub semantic_freshness: String,
    /// Repository state label, such as `ready`.
    pub state: String,
}

/// One retained canonical root owned by a registered repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceRepositoryRoot {
    repository: RepositoryId,
    root: PathBuf,
}

impl FirstSliceRepositoryRoot {
    /// Returns the stable repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the retained canonical repository root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Working-tree portion selected for bounded Git change evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceWorkingTreeSelection {
    /// Changes recorded in the index relative to `HEAD`.
    Staged,
    /// Changes in the working tree relative to the index.
    Unstaged,
    /// Both staged and unstaged changes.
    All,
}

/// Stable source-free failures from service-owned Git evidence collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FirstSliceGitEvidenceError {
    /// A revision selector violates the bounded Git evidence contract.
    #[error("first-slice Git selector is invalid")]
    InvalidSelector,
    /// A caller-supplied resource bound is invalid.
    #[error("first-slice Git evidence limits are invalid")]
    InvalidLimits,
    /// Cooperative cancellation stopped collection.
    #[error("first-slice Git evidence collection was cancelled")]
    Cancelled,
    /// The requested evidence cannot be established safely.
    #[error("first-slice Git evidence is unavailable")]
    Unavailable,
}

/// One repository's resolved and active generation status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryStatusDto {
    /// Process-local repository identity.
    pub repository: RepositoryId,
    /// Sanitized Rootlight-owned display label.
    pub display_name: String,
    /// Optional sanitized registered alias.
    pub alias: Option<String>,
    /// Immutable generation selected by the request.
    pub resolved_generation: GenerationId,
    /// Active immutable generation for the repository.
    pub active_generation: GenerationId,
    /// Optional predecessor of the resolved generation.
    pub parent_generation: Option<GenerationId>,
    /// Optional predecessor of the active generation.
    pub active_parent_generation: Option<GenerationId>,
    /// Structural freshness of the active generation.
    pub active_structural_freshness: String,
    /// Semantic freshness of the active generation.
    pub active_semantic_freshness: String,
    /// Structural freshness label, such as `current`.
    pub structural_freshness: String,
    /// Semantic freshness label, such as `current`.
    pub semantic_freshness: String,
    /// Repository state label, such as `ready`.
    pub state: String,
    /// Publication relationship of the selected generation.
    pub publication_state: String,
    /// Durable bytes retained for the selected immutable generation.
    pub retained_durable_bytes: u64,
    /// Complete logical identity for the selected generation, if persisted.
    pub logical_snapshot: Option<FirstSliceLogicalSnapshotIdentity>,
    /// Language-scoped coverage entries.
    pub coverage: Vec<RepositoryCoverageEntryDto>,
}

/// Maps an observed freshness value to its stable wire label.
const fn freshness_label(freshness: FirstSliceObservedFreshness) -> &'static str {
    match freshness {
        FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan => "current",
        FirstSliceObservedFreshness::PendingSemanticRefinement => "pending_refinement",
        FirstSliceObservedFreshness::Superseded => "superseded",
    }
}

const fn catalog_freshness(freshness: FirstSliceObservedFreshness) -> CatalogFreshness {
    match freshness {
        FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan => CatalogFreshness::Current,
        FirstSliceObservedFreshness::PendingSemanticRefinement => CatalogFreshness::Stale,
        FirstSliceObservedFreshness::Superseded => CatalogFreshness::Superseded,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LanguageCoverageSummary {
    language: String,
    tier: AnalysisTier,
    status: rootlight_ir::CoverageStatus,
    discovered_files: u64,
    indexed_files: u64,
}

fn language_coverage(document: &NormalizedIrDocument) -> Vec<LanguageCoverageSummary> {
    let provenance_tiers: BTreeMap<_, _> = document
        .provenance
        .iter()
        .map(|provenance| (provenance.id, provenance.tier))
        .collect();
    let mut coverage_by_file = BTreeMap::new();
    for coverage in &document.coverage_records {
        let rootlight_ir::CoverageScope::File(file) = coverage.scope else {
            continue;
        };
        coverage_by_file
            .entry(file)
            .and_modify(|status| {
                *status = lower_coverage_status(*status, coverage.status);
            })
            .or_insert(coverage.status);
    }
    let unsupported_files = document
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "unsupported-language")
        .filter_map(|diagnostic| {
            diagnostic
                .source
                .as_ref()
                .map(|source| source.span().file())
        })
        .collect::<BTreeSet<_>>();
    let mut by_language = BTreeMap::<String, LanguageCoverageSummary>::new();
    for file in &document.files {
        let tier = provenance_tiers
            .get(&file.provenance)
            .copied()
            .unwrap_or(AnalysisTier::TierD);
        let status = coverage_by_file
            .get(&file.id)
            .copied()
            .unwrap_or(rootlight_ir::CoverageStatus::Unknown);
        let entry =
            by_language
                .entry(file.language.clone())
                .or_insert_with(|| LanguageCoverageSummary {
                    language: file.language.clone(),
                    tier,
                    status,
                    discovered_files: 0,
                    indexed_files: 0,
                });
        entry.tier = lower_analysis_tier(entry.tier, tier);
        entry.status = lower_coverage_status(entry.status, status);
        entry.discovered_files = entry.discovered_files.saturating_add(1);
        if !unsupported_files.contains(&file.id) {
            entry.indexed_files = entry.indexed_files.saturating_add(1);
        }
    }
    by_language.into_values().collect()
}

fn coverage_from_summaries(
    summaries: &[LanguageCoverageSummary],
) -> Vec<RepositoryCoverageEntryDto> {
    summaries
        .iter()
        .map(|coverage| RepositoryCoverageEntryDto {
            language: coverage.language.clone(),
            tier: analysis_tier_label(coverage.tier).to_owned(),
            status: coverage_status_label(coverage.status).to_owned(),
            discovered_files: coverage.discovered_files,
            indexed_files: coverage.indexed_files,
        })
        .collect()
}

const fn lower_analysis_tier(left: AnalysisTier, right: AnalysisTier) -> AnalysisTier {
    if analysis_tier_rank(left) <= analysis_tier_rank(right) {
        left
    } else {
        right
    }
}

const fn analysis_tier_rank(tier: AnalysisTier) -> u8 {
    match tier {
        AnalysisTier::TierA => 4,
        AnalysisTier::TierB => 3,
        AnalysisTier::TierC => 2,
        AnalysisTier::TierD => 1,
        _ => 1,
    }
}

const fn analysis_tier_label(tier: AnalysisTier) -> &'static str {
    match tier {
        AnalysisTier::TierA => "tier_a",
        AnalysisTier::TierB => "tier_b",
        AnalysisTier::TierC => "tier_c",
        AnalysisTier::TierD => "tier_d",
        _ => "tier_d",
    }
}

const fn lower_coverage_status(
    left: rootlight_ir::CoverageStatus,
    right: rootlight_ir::CoverageStatus,
) -> rootlight_ir::CoverageStatus {
    if coverage_status_rank(left) <= coverage_status_rank(right) {
        left
    } else {
        right
    }
}

const fn coverage_status_rank(status: rootlight_ir::CoverageStatus) -> u8 {
    match status {
        rootlight_ir::CoverageStatus::Complete => 4,
        rootlight_ir::CoverageStatus::Bounded => 3,
        rootlight_ir::CoverageStatus::Sampled => 2,
        rootlight_ir::CoverageStatus::Unknown => 1,
        _ => 1,
    }
}

const fn coverage_status_label(status: rootlight_ir::CoverageStatus) -> &'static str {
    match status {
        rootlight_ir::CoverageStatus::Complete => "complete",
        rootlight_ir::CoverageStatus::Bounded => "bounded",
        rootlight_ir::CoverageStatus::Sampled => "sampled",
        rootlight_ir::CoverageStatus::Unknown => "unknown",
        _ => "unknown",
    }
}

/// Two-phase index result awaiting an explicit publication decision.
///
/// The prepared variant remains inline because adding a `Box` here would
/// introduce an infallible allocation after the pipeline's fallible admission
/// checks.
#[allow(clippy::large_enum_variant)]
pub enum FirstSliceIndexPreparation {
    /// An identical retained generation only needs reactivation.
    Retained {
        /// Existing immutable generation that will become active.
        receipt: FirstSliceIndexReceipt,
        /// Current display-only canonical repository root.
        root_path: String,
    },
    /// Newly built immutable state that has not entered the queryable set.
    Pending(PreparedFirstSliceIndex),
}

impl FirstSliceIndexPreparation {
    /// Returns the receipt that publication would make active.
    #[must_use]
    pub fn receipt(&self) -> FirstSliceIndexReceipt {
        match self {
            Self::Retained { receipt, .. } => receipt.clone(),
            Self::Pending(prepared) => prepared.receipt.clone(),
        }
    }
}

/// Fully verified first-slice state that is not yet queryable.
pub struct PreparedFirstSliceIndex {
    verified: IdentityVerifiedGeneration,
    search: LexicalIndex,
    sources: Vec<RustSourceInput>,
    structural_artifacts: StructuralGenerationArtifacts,
    incremental: PreparedIncrementalState,
    receipt: FirstSliceIndexReceipt,
    root_identity: ContentHash,
    display_name: String,
    root_path: String,
    register_repository: bool,
    durable: Option<DurableStorageAdmittedGeneration>,
    written_bytes: u64,
    reserved_memory_bytes: u64,
    memory_bytes: u64,
    storage_reservation: Option<DurableStorageReservation>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DurableGenerationRepresentation {
    Oracle,
    // Semantic refinements are already identity-verified and keep their
    // structural oracle predecessor, so one checksummed snapshot is sufficient.
    RecoverySnapshot,
}

/// Retention-admitted generation awaiting durable lifecycle success.
///
/// Newly built state is reserved inside the bounded generation and source
/// retention sets but remains invisible to every query path until this token
/// is committed.
pub struct FirstSliceStagedIndex {
    receipt: FirstSliceIndexReceipt,
    publication: FirstSlicePublication,
    written_bytes: u64,
    storage_reservation: Option<DurableStorageReservation>,
}

impl FirstSliceStagedIndex {
    /// Returns the still-hidden generation receipt.
    #[must_use]
    pub fn receipt(&self) -> FirstSliceIndexReceipt {
        self.receipt.clone()
    }

    /// Returns bytes already confirmed by durable preparation writers.
    #[must_use]
    pub const fn written_bytes(&self) -> u64 {
        self.written_bytes
    }
}

// Keeping staged state inline avoids a new allocation after retention sets
// have already changed and would otherwise require transactional rollback.
#[allow(clippy::large_enum_variant)]
enum FirstSlicePublication {
    Retained {
        root_path: String,
    },
    Pending {
        root_identity: ContentHash,
        display_name: String,
        root_path: String,
        register_repository: bool,
        language_coverage: Vec<LanguageCoverageSummary>,
        incremental: PreparedIncrementalState,
        reserved_memory_bytes: u64,
        memory_bytes: u64,
        durable: Option<DurablePublishedGeneration>,
    },
}

struct PreparedIncrementalState {
    baseline: IncrementalDiscoveryBaseline,
    inputs: InputSnapshot,
    evidence: FirstSliceIncrementalEvidence,
}

struct PreparedIncrementalPlan {
    state: PreparedIncrementalState,
    reusable_parser_artifacts: BTreeSet<ArtifactId>,
}

struct RustSourceInput {
    snapshot: SourceSnapshot,
    generated: bool,
    origins: Vec<GeneratedOriginMapping>,
}

struct UnsupportedSourceInput {
    claim: FileIdentityClaim,
    language: String,
    generated: bool,
}

/// A known reason why one retained source has partial analysis coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceSourceCoverageReason {
    /// The source language has no configured structural analyzer.
    UnsupportedLanguage,
}

/// Stable source-free reason that repository coverage is partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FirstSliceCoverageGapReason {
    /// The source language is recognized but has no structural analyzer.
    Unsupported,
    /// No installed detector recognized the source language.
    Unrecognized,
    /// Discovery policy excluded an input.
    Excluded,
    /// An input exceeded a configured source bound.
    Oversized,
    /// An input was classified as binary.
    Binary,
    /// Parsing could not cover the complete source.
    ParseError,
    /// An isolated or in-process analyzer failed and a lower tier was used.
    AdapterFailed,
    /// A bounded publication or retrieval limit omitted facts or source text.
    Truncated,
    /// Generated-source policy prevented a complete production claim.
    Generated,
    /// The selected immutable generation is not current.
    Stale,
}

/// One bounded source-free repository coverage gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceCoverageGap {
    /// Stable closed reason.
    pub reason: FirstSliceCoverageGapReason,
    /// Canonical language when the gap is language-scoped.
    pub language: Option<String>,
    /// Files affected by the gap.
    pub files: u64,
}

/// Analysis coverage attached to one retained source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSliceSourceCoverage {
    /// Normalized source language.
    pub language: String,
    /// Highest analysis tier that produced facts for the file.
    pub tier: AnalysisTier,
    /// Lowest coverage status across the file's normalized fact domains.
    pub status: CoverageStatus,
    /// Known reason for partial coverage, when one is available.
    pub reason: Option<FirstSliceSourceCoverageReason>,
}

struct StructuralArtifactEntry {
    id: ArtifactId,
    artifact: Arc<TreeSitterStructuralArtifact>,
    normalized: Option<Arc<CanonicalNormalizedFileChunk>>,
}

impl StructuralArtifactEntry {
    fn accounted_bytes(&self) -> Result<usize, FirstSliceError> {
        self.artifact
            .accounted_bytes()
            .checked_add(
                self.normalized
                    .as_ref()
                    .map_or(0, |chunk| chunk.encoded_bytes()),
            )
            .ok_or(FirstSliceError::Retention)
    }
}

#[derive(Default)]
struct ReusedNormalizedRecords {
    // Generation-bound fact IDs content-bind their complete records. File and
    // symbol IDs are stable across generations, so those records require exact
    // equality before final accounting can call them retained.
    files: BTreeMap<FileId, FileRecord>,
    entities: BTreeMap<SymbolId, EntityRecord>,
    occurrences: BTreeSet<FactId>,
    relations: BTreeSet<FactId>,
    provenance: BTreeSet<FactId>,
    source_mappings: BTreeSet<FactId>,
    coverage_records: BTreeSet<FactId>,
    skipped_regions: BTreeSet<FactId>,
    diagnostics: BTreeSet<FactId>,
    extensions: BTreeSet<FactId>,
}

impl ReusedNormalizedRecords {
    fn insert_document(&mut self, document: &NormalizedIrDocument) -> Result<(), FirstSliceError> {
        for record in &document.files {
            if self.files.insert(record.id, record.clone()).is_some() {
                return Err(FirstSliceError::Identity);
            }
        }
        for record in &document.entities {
            if self.entities.insert(record.id, record.clone()).is_some() {
                return Err(FirstSliceError::Identity);
            }
        }
        self.occurrences
            .extend(document.occurrences.iter().map(|record| record.id));
        self.relations
            .extend(document.relations.iter().map(|record| record.id));
        self.provenance
            .extend(document.provenance.iter().map(|record| record.id));
        self.source_mappings
            .extend(document.source_mappings.iter().map(|record| record.id));
        self.coverage_records
            .extend(document.coverage_records.iter().map(|record| record.id));
        self.skipped_regions
            .extend(document.skipped_regions.iter().map(|record| record.id));
        self.diagnostics
            .extend(document.diagnostics.iter().map(|record| record.id));
        self.extensions
            .extend(document.extensions.iter().map(|record| record.id));
        Ok(())
    }

    fn retained_count(&self, document: &NormalizedIrDocument) -> Result<usize, FirstSliceError> {
        [
            document
                .files
                .iter()
                .filter(|record| self.contains_file(record))
                .count(),
            document
                .entities
                .iter()
                .filter(|record| self.contains_entity(record))
                .count(),
            document
                .occurrences
                .iter()
                .filter(|record| self.contains_occurrence(record))
                .count(),
            document
                .relations
                .iter()
                .filter(|record| self.contains_relation(record))
                .count(),
            document
                .provenance
                .iter()
                .filter(|record| self.contains_provenance(record))
                .count(),
            document
                .source_mappings
                .iter()
                .filter(|record| self.contains_source_mapping(record))
                .count(),
            document
                .coverage_records
                .iter()
                .filter(|record| self.contains_coverage(record))
                .count(),
            document
                .skipped_regions
                .iter()
                .filter(|record| self.contains_skipped(record))
                .count(),
            document
                .diagnostics
                .iter()
                .filter(|record| self.contains_diagnostic(record))
                .count(),
            document
                .extensions
                .iter()
                .filter(|record| self.contains_extension(record))
                .count(),
        ]
        .into_iter()
        .try_fold(0_usize, |total, count| {
            total.checked_add(count).ok_or(FirstSliceError::Limits)
        })
    }

    fn contains_file(&self, record: &FileRecord) -> bool {
        self.files.get(&record.id) == Some(record)
    }

    fn contains_entity(&self, record: &EntityRecord) -> bool {
        self.entities.get(&record.id) == Some(record)
    }

    fn contains_occurrence(&self, record: &OccurrenceRecord) -> bool {
        self.occurrences.contains(&record.id)
    }

    fn contains_relation(&self, record: &RelationRecord) -> bool {
        self.relations.contains(&record.id)
    }

    fn contains_provenance(&self, record: &ProvenanceRecord) -> bool {
        self.provenance.contains(&record.id)
    }

    fn contains_source_mapping(&self, record: &SourceMappingRecord) -> bool {
        self.source_mappings.contains(&record.id)
    }

    fn contains_coverage(&self, record: &CoverageRecord) -> bool {
        self.coverage_records.contains(&record.id)
    }

    fn contains_skipped(&self, record: &SkippedRegion) -> bool {
        self.skipped_regions.contains(&record.id)
    }

    fn contains_diagnostic(&self, record: &DiagnosticRecord) -> bool {
        self.diagnostics.contains(&record.id)
    }

    fn contains_extension(&self, record: &ExtensionEnvelope) -> bool {
        self.extensions.contains(&record.id)
    }
}

struct StructuralGenerationArtifacts {
    by_file: BTreeMap<FileId, StructuralArtifactEntry>,
    accounted_bytes: usize,
    retention_complete: bool,
}

impl StructuralGenerationArtifacts {
    fn empty() -> Self {
        Self {
            by_file: BTreeMap::new(),
            accounted_bytes: 0,
            retention_complete: false,
        }
    }

    fn new(
        entries: impl IntoIterator<Item = StructuralArtifactEntry>,
        maximum_bytes: usize,
        retention_complete: bool,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        let mut by_file = BTreeMap::new();
        let mut accounted_bytes = 0usize;
        for entry in entries {
            check_cancellation(cancellation)?;
            let file = entry.artifact.file();
            if entry.id != parser_artifact_id(file) || by_file.contains_key(&file) {
                return Err(FirstSliceError::Incremental);
            }
            accounted_bytes = accounted_bytes
                .checked_add(entry.accounted_bytes()?)
                .ok_or(FirstSliceError::Retention)?;
            if accounted_bytes > maximum_bytes {
                return Err(FirstSliceError::Retention);
            }
            by_file.insert(file, entry);
        }
        check_cancellation(cancellation)?;
        Ok(Self {
            by_file,
            accounted_bytes,
            retention_complete,
        })
    }

    fn get(&self, file: FileId) -> Option<&StructuralArtifactEntry> {
        self.by_file.get(&file)
    }

    fn iter(&self) -> impl Iterator<Item = (&FileId, &StructuralArtifactEntry)> {
        self.by_file.iter()
    }

    fn len(&self) -> usize {
        self.by_file.len()
    }
}

struct StructuralArtifactRelease {
    generation: GenerationId,
    artifacts: StructuralGenerationArtifacts,
    retained_bytes_after: usize,
}

/// Bounded process-local parser-artifact retention aligned with publication.
///
/// Charges are deterministic logical bytes. Exact artifacts can share their
/// allocation through `Arc`, while every generation is conservatively charged
/// in full so logical retention never exceeds the configured ceiling.
struct StructuralArtifactRetention {
    maximum_generations: usize,
    maximum_bytes: usize,
    retained_bytes: usize,
    committed: BTreeMap<GenerationId, StructuralGenerationArtifacts>,
    staged: BTreeMap<GenerationId, StructuralGenerationArtifacts>,
}

impl StructuralArtifactRetention {
    fn new(maximum_generations: usize, maximum_bytes: usize) -> Result<Self, FirstSliceError> {
        if maximum_generations == 0 || maximum_bytes == 0 {
            return Err(FirstSliceError::Retention);
        }
        Ok(Self {
            maximum_generations,
            maximum_bytes,
            retained_bytes: 0,
            committed: BTreeMap::new(),
            staged: BTreeMap::new(),
        })
    }

    fn generation(&self, generation: GenerationId) -> Option<&StructuralGenerationArtifacts> {
        self.committed.get(&generation)
    }

    const fn available_bytes(&self) -> usize {
        self.maximum_bytes.saturating_sub(self.retained_bytes)
    }

    fn stage(
        &mut self,
        generation: GenerationId,
        artifacts: StructuralGenerationArtifacts,
        cancellation: &Cancellation,
    ) -> Result<bool, FirstSliceError> {
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
        let admitted_bytes = self
            .retained_bytes
            .checked_add(artifacts.accounted_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let (artifacts, retained, admitted_bytes) =
            if artifacts.retention_complete && admitted_bytes <= self.maximum_bytes {
                (artifacts, true, admitted_bytes)
            } else {
                (
                    StructuralGenerationArtifacts::empty(),
                    false,
                    self.retained_bytes,
                )
            };
        check_cancellation(cancellation)?;
        self.staged.insert(generation, artifacts);
        self.retained_bytes = admitted_bytes;
        Ok(retained)
    }

    fn commit_staged(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let std::collections::btree_map::Entry::Vacant(committed) =
            self.committed.entry(generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        let artifacts = self
            .staged
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        committed.insert(artifacts);
        Ok(())
    }

    fn rollback_commit(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let std::collections::btree_map::Entry::Vacant(staged) = self.staged.entry(generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        let artifacts = self
            .committed
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        staged.insert(artifacts);
        Ok(())
    }

    fn begin_discard(
        &mut self,
        generation: GenerationId,
    ) -> Result<StructuralArtifactRelease, FirstSliceError> {
        let accounted_bytes = self
            .staged
            .get(&generation)
            .map(|artifacts| artifacts.accounted_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let retained_bytes_after = self
            .retained_bytes
            .checked_sub(accounted_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let artifacts = self
            .staged
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        Ok(StructuralArtifactRelease {
            generation,
            artifacts,
            retained_bytes_after,
        })
    }

    fn finish_discard(&mut self, release: StructuralArtifactRelease) {
        self.retained_bytes = release.retained_bytes_after;
    }

    fn rollback_discard(
        &mut self,
        release: StructuralArtifactRelease,
    ) -> Result<(), FirstSliceError> {
        let std::collections::btree_map::Entry::Vacant(staged) =
            self.staged.entry(release.generation)
        else {
            return Err(FirstSliceError::Retention);
        };
        staged.insert(release.artifacts);
        Ok(())
    }

    fn contains_committed(&self, generation: GenerationId) -> bool {
        self.committed.contains_key(&generation)
    }

    fn remove_committed(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let accounted_bytes = self
            .committed
            .get(&generation)
            .map(|artifacts| artifacts.accounted_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let retained_bytes = self
            .retained_bytes
            .checked_sub(accounted_bytes)
            .ok_or(FirstSliceError::Retention)?;
        self.committed
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        self.retained_bytes = retained_bytes;
        Ok(())
    }
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

const SOURCE_RETENTION_BTREE_ENTRY_OVERHEAD: usize = 4 * size_of::<usize>();
const SOURCE_RETENTION_ARC_OVERHEAD: usize = 2 * size_of::<usize>();
const SOURCE_RETENTION_ALLOCATION_OVERHEAD: usize = 2 * size_of::<usize>();

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

fn unique_source_snapshot_bytes(snapshot: &SourceSnapshot) -> Result<usize, FirstSliceError> {
    snapshot
        .retained_memory_bytes()
        .ok_or(FirstSliceError::Retention)?
        .checked_add(SOURCE_RETENTION_ARC_OVERHEAD)
        .and_then(|bytes| bytes.checked_add(size_of::<SourceSnapshotIdentity>()))
        .and_then(|bytes| bytes.checked_add(size_of::<SharedSourceSnapshot>()))
        .and_then(|bytes| bytes.checked_add(SOURCE_RETENTION_BTREE_ENTRY_OVERHEAD))
        .ok_or(FirstSliceError::Retention)
}

fn source_generation_reference_bytes(snapshot_count: usize) -> Result<usize, FirstSliceError> {
    let references = snapshot_count
        .checked_mul(size_of::<Arc<SourceSnapshot>>())
        .ok_or(FirstSliceError::Retention)?;
    let allocation_overhead = usize::from(snapshot_count != 0)
        .checked_mul(SOURCE_RETENTION_ALLOCATION_OVERHEAD)
        .ok_or(FirstSliceError::Retention)?;
    size_of::<GenerationId>()
        .checked_add(size_of::<Vec<Arc<SourceSnapshot>>>())
        .and_then(|bytes| bytes.checked_add(SOURCE_RETENTION_BTREE_ENTRY_OVERHEAD))
        .and_then(|bytes| bytes.checked_add(references))
        .and_then(|bytes| bytes.checked_add(allocation_overhead))
        .ok_or(FirstSliceError::Retention)
}

/// Bounded runtime source retention mirroring generation publication.
///
/// Source bodies remain outside normalized IR. Durable services persist
/// identity-verified bytes beside each immutable oracle and reconstruct this
/// deduplicated, memory-bounded runtime view during startup.
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
        let additional_bytes =
            self.preflight_admission(generation, sources.as_slice(), cancellation)?;
        check_cancellation(cancellation)?;
        sources.sort_unstable_by_key(|source| SourceSnapshotIdentity::from(&source.snapshot));
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(sources.len())
            .map_err(|_| FirstSliceError::Retention)?;
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

    fn preflight_admission(
        &self,
        generation: GenerationId,
        sources: &[RustSourceInput],
        cancellation: &Cancellation,
    ) -> Result<usize, FirstSliceError> {
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

        let mut files = BTreeSet::new();
        let mut additional_bytes = source_generation_reference_bytes(sources.len())?;
        for source in sources {
            check_cancellation(cancellation)?;
            let identity = SourceSnapshotIdentity::from(&source.snapshot);
            if !files.insert(identity.file) {
                return Err(FirstSliceError::Retention);
            }
            if !self.shared.contains_key(&identity) {
                additional_bytes = additional_bytes
                    .checked_add(unique_source_snapshot_bytes(&source.snapshot)?)
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
        Ok(additional_bytes)
    }

    fn preflight_admission_after_reclaim(
        &self,
        generation: GenerationId,
        sources: &[RustSourceInput],
        reclaimable: &BTreeSet<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        check_cancellation(cancellation)?;
        if self.committed.contains_key(&generation) || self.staged.contains_key(&generation) {
            return Err(FirstSliceError::Retention);
        }
        let retained_generations = self
            .committed
            .keys()
            .filter(|candidate| !reclaimable.contains(candidate))
            .count()
            .checked_add(self.staged.len())
            .ok_or(FirstSliceError::Retention)?;
        if retained_generations >= self.maximum_generations {
            return Err(FirstSliceError::Retention);
        }

        let mut retained = BTreeMap::<SourceSnapshotIdentity, usize>::new();
        let mut retained_bytes = 0_usize;
        for snapshots in self
            .committed
            .iter()
            .filter(|(candidate, _)| !reclaimable.contains(candidate))
            .map(|(_, snapshots)| snapshots)
            .chain(self.staged.values())
        {
            retained_bytes = retained_bytes
                .checked_add(source_generation_reference_bytes(snapshots.len())?)
                .ok_or(FirstSliceError::Retention)?;
            for snapshot in snapshots {
                check_cancellation(cancellation)?;
                let identity = SourceSnapshotIdentity::from(snapshot.as_ref());
                if let std::collections::btree_map::Entry::Vacant(entry) = retained.entry(identity)
                {
                    let snapshot_bytes = unique_source_snapshot_bytes(snapshot.as_ref())?;
                    retained_bytes = retained_bytes
                        .checked_add(snapshot_bytes)
                        .ok_or(FirstSliceError::Retention)?;
                    entry.insert(snapshot_bytes);
                }
            }
        }
        let mut files = BTreeSet::new();
        let mut additional_bytes = source_generation_reference_bytes(sources.len())?;
        for source in sources {
            check_cancellation(cancellation)?;
            let identity = SourceSnapshotIdentity::from(&source.snapshot);
            if !files.insert(identity.file) {
                return Err(FirstSliceError::Retention);
            }
            if !retained.contains_key(&identity) {
                additional_bytes = additional_bytes
                    .checked_add(unique_source_snapshot_bytes(&source.snapshot)?)
                    .ok_or(FirstSliceError::Retention)?;
            }
        }
        let observed = retained_bytes
            .checked_add(additional_bytes)
            .ok_or(FirstSliceError::Retention)?;
        if observed > self.maximum_bytes {
            return Err(FirstSliceError::Retention);
        }
        check_cancellation(cancellation)?;
        Ok(())
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
        let mut released_bytes = source_generation_reference_bytes(snapshots.len())?;
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
                    .checked_add(unique_source_snapshot_bytes(shared.snapshot.as_ref())?)
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

    fn contains_committed(&self, generation: GenerationId) -> bool {
        self.committed.contains_key(&generation)
    }

    fn remove_committed(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let snapshots = self
            .committed
            .get(&generation)
            .ok_or(FirstSliceError::Retention)?;
        let mut updates = Vec::new();
        updates
            .try_reserve_exact(snapshots.len())
            .map_err(|_| FirstSliceError::Retention)?;
        let mut released_bytes = source_generation_reference_bytes(snapshots.len())?;
        for snapshot in snapshots {
            let identity = SourceSnapshotIdentity::from(snapshot.as_ref());
            let shared = self
                .shared
                .get(&identity)
                .ok_or(FirstSliceError::Retention)?;
            let remaining = shared
                .generation_references
                .checked_sub(1)
                .ok_or(FirstSliceError::Retention)?;
            if remaining == 0 {
                released_bytes = released_bytes
                    .checked_add(unique_source_snapshot_bytes(shared.snapshot.as_ref())?)
                    .ok_or(FirstSliceError::Retention)?;
            }
            updates.push((identity, remaining));
        }
        let retained_bytes = self
            .retained_bytes
            .checked_sub(released_bytes)
            .ok_or(FirstSliceError::Retention)?;
        self.committed
            .remove(&generation)
            .ok_or(FirstSliceError::Retention)?;
        for (identity, remaining) in updates {
            if remaining == 0 {
                self.shared.remove(&identity);
            } else {
                self.shared
                    .get_mut(&identity)
                    .ok_or(FirstSliceError::Retention)?
                    .generation_references = remaining;
            }
        }
        self.retained_bytes = retained_bytes;
        Ok(())
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

/// Complete lower-layer limits for one first-slice analytical request.
///
/// The daemon constructs this policy only after intersecting its own ceilings
/// with any authenticated transport reduction. Keeping query, lexical-search,
/// and source-read limits together prevents a caller reduction from reaching
/// one engine while another engine silently recreates broader defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceBudget {
    query: QueryBudget,
    search: SearchBudget,
    source: SourceBudget,
}

impl FirstSliceBudget {
    /// Creates the bounded interactive service policy.
    #[must_use]
    pub fn new() -> Self {
        let query = QueryBudget::new();
        let mut search = SearchBudget::default();
        if let Ok(query_max_results) = usize::try_from(query.max_results()) {
            // The service historically admitted direct locate caps up to the
            // query ceiling, while the search crate's standalone default is lower.
            search.max_results = query_max_results;
        }
        Self {
            query,
            search,
            source: SourceBudget::new(),
        }
    }

    /// Returns the query-engine policy.
    #[must_use]
    pub const fn query(self) -> QueryBudget {
        self.query
    }

    /// Returns the lexical-search policy.
    #[must_use]
    pub const fn search(self) -> SearchBudget {
        self.search
    }

    /// Returns the source-read policy.
    #[must_use]
    pub const fn source(self) -> SourceBudget {
        self.source
    }

    /// Replaces the source context-line ceiling for a bounded presentation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Query`] when the requested source policy
    /// exceeds the source service's hard bounds.
    pub fn with_source_max_context_lines(mut self, maximum: u16) -> Result<Self, FirstSliceError> {
        self.source.max_context_lines = maximum;
        self.source.validate().map_err(|_| FirstSliceError::Query)?;
        Ok(self)
    }

    /// Reduces the logical-row ceiling.
    #[must_use]
    pub const fn reduce_max_rows(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_rows(if self.query.max_rows() < maximum {
                self.query.max_rows()
            } else {
                maximum
            });
        self
    }

    /// Reduces every result-bearing lower-layer policy.
    #[must_use]
    pub fn reduce_max_results(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_results(self.query.max_results().min(maximum));
        if let Ok(maximum) = usize::try_from(maximum) {
            self.search.max_results = self.search.max_results.min(maximum);
        }
        self
    }

    /// Reduces the traversed-edge ceiling.
    #[must_use]
    pub const fn reduce_max_edges(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_edges(if self.query.max_edges() < maximum {
                self.query.max_edges()
            } else {
                maximum
            });
        self
    }

    /// Reduces the raw source-byte ceiling in both relevant engines.
    #[must_use]
    pub fn reduce_max_source_bytes(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_source_bytes(self.query.max_source_bytes().min(maximum));
        if let Ok(maximum) = usize::try_from(maximum) {
            self.source.max_source_bytes = self.source.max_source_bytes.min(maximum);
        }
        self
    }

    /// Reduces the conservative token ceiling.
    #[must_use]
    pub const fn reduce_max_tokens(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_tokens(if self.query.max_tokens() < maximum {
                self.query.max_tokens()
            } else {
                maximum
            });
        self
    }

    /// Reduces the exact lower query-response JSON ceiling.
    #[must_use]
    pub const fn reduce_max_json_bytes(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_json_bytes(if self.query.max_json_bytes() < maximum {
                self.query.max_json_bytes()
            } else {
                maximum
            });
        self
    }

    /// Reduces owned response memory in both relevant engines.
    #[must_use]
    pub fn reduce_max_memory_bytes(mut self, maximum: u64) -> Self {
        self.query = self
            .query
            .with_max_memory_bytes(self.query.max_memory_bytes().min(maximum));
        if let Ok(maximum) = usize::try_from(maximum) {
            self.source.max_response_memory_bytes =
                self.source.max_response_memory_bytes.min(maximum);
        }
        self
    }

    /// Reduces the cooperative duration in every lower-layer engine.
    #[must_use]
    pub fn reduce_max_duration(mut self, maximum: Duration) -> Self {
        self.query = self
            .query
            .with_max_duration(self.query.max_duration().min(maximum));
        self.search.max_duration = self.search.max_duration.min(maximum);
        self.source.max_duration = self.source.max_duration.min(maximum);
        self
    }

    /// Reduces the lexical query-text ceiling.
    #[must_use]
    pub const fn reduce_search_max_query_bytes(mut self, maximum: usize) -> Self {
        self.search.max_query_bytes = if self.search.max_query_bytes < maximum {
            self.search.max_query_bytes
        } else {
            maximum
        };
        self
    }

    /// Reduces the lexical candidate ceiling.
    #[must_use]
    pub const fn reduce_search_max_candidates(mut self, maximum: usize) -> Self {
        self.search.max_candidates = if self.search.max_candidates < maximum {
            self.search.max_candidates
        } else {
            maximum
        };
        self
    }

    /// Reduces the lexical term ceiling.
    #[must_use]
    pub const fn reduce_search_max_terms(mut self, maximum: usize) -> Self {
        self.search.max_terms = if self.search.max_terms < maximum {
            self.search.max_terms
        } else {
            maximum
        };
        self
    }

    /// Reduces the expanded lexical-term ceiling.
    #[must_use]
    pub const fn reduce_search_max_expanded_terms(mut self, maximum: usize) -> Self {
        self.search.max_expanded_terms = if self.search.max_expanded_terms < maximum {
            self.search.max_expanded_terms
        } else {
            maximum
        };
        self
    }

    /// Reduces the examined lexical-term ceiling.
    #[must_use]
    pub const fn reduce_search_max_examined_terms(mut self, maximum: usize) -> Self {
        self.search.max_examined_terms = if self.search.max_examined_terms < maximum {
            self.search.max_examined_terms
        } else {
            maximum
        };
        self
    }

    /// Reduces the lexical posting ceiling.
    #[must_use]
    pub const fn reduce_search_max_postings(mut self, maximum: u64) -> Self {
        self.search.max_postings = if self.search.max_postings < maximum {
            self.search.max_postings
        } else {
            maximum
        };
        self
    }

    /// Reduces the returned lexical-text ceiling.
    #[must_use]
    pub const fn reduce_search_max_returned_text_bytes(mut self, maximum: usize) -> Self {
        self.search.max_returned_text_bytes = if self.search.max_returned_text_bytes < maximum {
            self.search.max_returned_text_bytes
        } else {
            maximum
        };
        self
    }

    /// Reduces the source-selector ceiling.
    #[must_use]
    pub const fn reduce_source_max_selectors(mut self, maximum: usize) -> Self {
        self.source.max_selectors = if self.source.max_selectors < maximum {
            self.source.max_selectors
        } else {
            maximum
        };
        self
    }

    /// Reduces the source context-line ceiling.
    #[must_use]
    pub const fn reduce_source_max_context_lines(mut self, maximum: u16) -> Self {
        self.source.max_context_lines = if self.source.max_context_lines < maximum {
            self.source.max_context_lines
        } else {
            maximum
        };
        self
    }

    /// Reduces the copied source-metadata ceiling.
    #[must_use]
    pub const fn reduce_source_max_metadata_bytes(mut self, maximum: usize) -> Self {
        self.source.max_metadata_bytes = if self.source.max_metadata_bytes < maximum {
            self.source.max_metadata_bytes
        } else {
            maximum
        };
        self
    }

    /// Reduces the source snapshot-read ceiling.
    #[must_use]
    pub const fn reduce_source_max_snapshot_bytes(mut self, maximum: u64) -> Self {
        self.source.max_snapshot_bytes = if self.source.max_snapshot_bytes < maximum {
            self.source.max_snapshot_bytes
        } else {
            maximum
        };
        self
    }
}

impl Default for FirstSliceBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Analysis strength requested for one repository generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceIndexMode {
    /// Use only in-process audited structural analyzers.
    Structural,
    /// Attempt isolated whole-project Tier B analysis with explicit fallback.
    Deep,
}

/// Controls whether one repository generation may reuse its active parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceReusePolicy {
    /// Use verified parent baselines and artifacts when they remain compatible.
    Incremental,
    /// Rebuild every source and fact while retaining the active lineage parent.
    CleanRebuild,
}

/// Orthogonal analysis-strength and reuse policy for one repository index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceIndexOptions {
    /// Analysis strength used to construct the new generation.
    pub mode: FirstSliceIndexMode,
    /// Parent-reuse policy used to construct the new generation.
    pub reuse_policy: FirstSliceReusePolicy,
}

impl FirstSliceIndexOptions {
    /// Constructs an incremental index using the requested analysis strength.
    #[must_use]
    pub const fn incremental(mode: FirstSliceIndexMode) -> Self {
        Self {
            mode,
            reuse_policy: FirstSliceReusePolicy::Incremental,
        }
    }

    /// Constructs a caller-requested clean rebuild using the requested strength.
    #[must_use]
    pub const fn clean_rebuild(mode: FirstSliceIndexMode) -> Self {
        Self {
            mode,
            reuse_policy: FirstSliceReusePolicy::CleanRebuild,
        }
    }
}

/// One immutable source made available to a whole-project analysis provider.
#[derive(Debug, Clone, Copy)]
pub struct FirstSliceProjectInput<'a> {
    file: FileId,
    path: &'a str,
    content_hash: ContentHash,
    source: &'a [u8],
    generated: bool,
    origins: &'a [GeneratedOriginMapping],
}

impl FirstSliceProjectInput<'_> {
    /// Returns the repository-stable file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the canonical repository-relative display path.
    #[must_use]
    pub const fn path(&self) -> &str {
        self.path
    }

    /// Returns the digest of the immutable source bytes.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns the immutable source bytes.
    #[must_use]
    pub const fn source(&self) -> &[u8] {
        self.source
    }

    /// Reports whether discovery classified the source as generated.
    #[must_use]
    pub const fn generated(&self) -> bool {
        self.generated
    }

    /// Returns reliable, disjoint generated-to-origin mappings.
    #[must_use]
    pub const fn origins(&self) -> &[GeneratedOriginMapping] {
        self.origins
    }
}

/// One generation-bound whole-project analysis transaction.
#[derive(Debug, Clone, Copy)]
pub struct FirstSliceProjectAnalysisRequest<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    language: &'a str,
    build_context: ContentHash,
    context_manifest: &'a [u8],
    inputs: &'a [FirstSliceProjectInput<'a>],
}

impl FirstSliceProjectAnalysisRequest<'_> {
    /// Returns the owning repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable target generation.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the exact language shared by every input.
    #[must_use]
    pub const fn language(&self) -> &str {
        self.language
    }

    /// Returns the build-context identity selected by the service.
    #[must_use]
    pub const fn build_context(&self) -> ContentHash {
        self.build_context
    }

    /// Returns the canonical opaque build-context manifest.
    #[must_use]
    pub const fn context_manifest(&self) -> &[u8] {
        self.context_manifest
    }

    /// Returns display-path-sorted immutable inputs.
    #[must_use]
    pub const fn inputs(&self) -> &[FirstSliceProjectInput<'_>] {
        self.inputs
    }
}

/// Validated project output plus evidence from its exact producing process.
#[derive(Debug)]
pub struct FirstSliceProjectAnalysis {
    documents: Vec<NormalizedIrDocument>,
    external_symbols: BTreeSet<SymbolId>,
    isolation_permits_deep_adapter: bool,
    partitioned: bool,
    diagnostics_truncated: bool,
    facts_truncated: bool,
}

impl FirstSliceProjectAnalysis {
    /// Creates one project output at the daemon-owned adapter boundary.
    #[must_use]
    pub fn new(document: NormalizedIrDocument, isolation_permits_deep_adapter: bool) -> Self {
        let external_symbols = project_external_symbols(&document);
        Self {
            documents: vec![document],
            external_symbols,
            isolation_permits_deep_adapter,
            partitioned: false,
            diagnostics_truncated: false,
            facts_truncated: false,
        }
    }

    /// Creates deterministic partition outputs from one daemon-owned analysis.
    #[must_use]
    pub fn new_partitioned(
        documents: Vec<NormalizedIrDocument>,
        isolation_permits_deep_adapter: bool,
    ) -> Self {
        let partitioned = documents.len() > 1;
        let external_symbols = documents
            .iter()
            .flat_map(project_external_symbols)
            .collect();
        Self {
            documents,
            external_symbols,
            isolation_permits_deep_adapter,
            partitioned,
            diagnostics_truncated: false,
            facts_truncated: false,
        }
    }

    /// Merges one validated adapter partition into the retained project output.
    ///
    /// Consuming each partition as it arrives bounds peak memory to the merged
    /// output plus one adapter response instead of retaining every response.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceProjectAnalysisError::Analysis`] when the partition
    /// names a different document identity or bounded allocation fails.
    pub fn append_partition(
        &mut self,
        document: NormalizedIrDocument,
        isolation_permits_deep_adapter: bool,
    ) -> Result<(), FirstSliceProjectAnalysisError> {
        let merged = self
            .documents
            .first_mut()
            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
        // Partition output still needs one aggregate truncation record and one
        // cross-partition coverage record after the streaming merge completes.
        let diagnostic_capacity = IrLimits::default()
            .max_diagnostics
            .saturating_sub(PROJECT_PARTITION_DIAGNOSTIC_RESERVE);
        let truncation = merge_project_document(
            merged,
            document,
            &mut self.external_symbols,
            diagnostic_capacity,
            &IrLimits::default(),
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        self.diagnostics_truncated |= truncation.diagnostics;
        self.facts_truncated |= truncation.facts;
        self.isolation_permits_deep_adapter &= isolation_permits_deep_adapter;
        self.partitioned = true;
        Ok(())
    }

    fn into_parts(self) -> (Vec<NormalizedIrDocument>, bool, bool, bool, bool) {
        (
            self.documents,
            self.isolation_permits_deep_adapter,
            self.partitioned,
            self.diagnostics_truncated,
            self.facts_truncated,
        )
    }
}

/// Source-free progress from one whole-project analysis transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceProjectAnalysisProgress {
    /// Inputs completed by durable provider partitions.
    pub completed_files: u64,
    /// Total immutable inputs admitted to the provider transaction.
    pub total_files: u64,
    /// Source bytes completed by durable provider partitions.
    pub completed_bytes: u64,
    /// Total immutable source bytes admitted to the provider transaction.
    pub total_bytes: u64,
}

impl FirstSliceProjectAnalysisProgress {
    fn complete(
        request: &FirstSliceProjectAnalysisRequest<'_>,
    ) -> Result<Self, FirstSliceProjectAnalysisError> {
        let total_files = u64::try_from(request.inputs().len())
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let total_bytes = request.inputs().iter().try_fold(0_u64, |total, input| {
            total
                .checked_add(
                    u64::try_from(input.source().len())
                        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
                )
                .ok_or(FirstSliceProjectAnalysisError::Analysis)
        })?;
        Ok(Self {
            completed_files: total_files,
            total_files,
            completed_bytes: total_bytes,
            total_bytes,
        })
    }
}

/// Source-free failures from the optional whole-project analysis boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FirstSliceProjectAnalysisError {
    /// Cooperative cancellation or a monotonic deadline won.
    #[error("project analysis was cancelled: {0:?}")]
    Cancelled(CancellationReason),
    /// The configured adapter identity could not be authenticated.
    #[error("project adapter identity is unavailable")]
    Identity,
    /// The adapter protocol or request correlation failed.
    #[error("project adapter protocol failed")]
    Protocol,
    /// The native isolation boundary could not be established.
    #[error("project adapter isolation failed")]
    Isolation,
    /// The operating system could not reserve resources for the isolated process.
    #[error("project adapter process resources are temporarily unavailable")]
    ResourceUnavailable,
    /// The isolated adapter crossed its configured wall-time ceiling.
    #[error("project adapter wall-time limit was reached")]
    WallTimeLimit,
    /// The isolated adapter crossed its configured input-volume ceiling.
    #[error("project adapter input limit was reached")]
    InputLimit,
    /// The isolated adapter crossed its configured output-volume ceiling.
    #[error("project adapter output limit was reached")]
    OutputLimit,
    /// The isolated adapter crossed its configured memory ceiling.
    #[error("project adapter memory limit was reached")]
    MemoryLimit,
    /// The isolated adapter process exited without a valid result.
    #[error("project adapter process failed")]
    ProcessFailure,
    /// The bounded project analysis could not complete.
    #[error("project adapter analysis failed")]
    Analysis,
    /// Project facts exceeded the generation's normalized-IR capacity.
    #[error("project adapter output exceeded normalized fact capacity")]
    Capacity,
    /// Aggregate project facts were truncated before semantic composition.
    #[error("project adapter facts were truncated")]
    FactsTruncated,
    /// Project output omitted structural declarations required for safe replacement.
    #[error("project adapter omitted {missing_declarations} structural declarations")]
    StructuralDeclarationLoss {
        /// Exact source-free count of missing structural declaration claims.
        missing_declarations: u64,
    },
}

impl FirstSliceProjectAnalysisError {
    const fn fallback_code(self) -> &'static str {
        match self {
            Self::Cancelled(_) => "project-adapter-cancelled",
            Self::Identity => "project-adapter-identity-fallback",
            Self::Protocol => "project-adapter-protocol-fallback",
            Self::Isolation => "project-adapter-isolation-fallback",
            Self::ResourceUnavailable => "project-adapter-resource-unavailable-fallback",
            Self::WallTimeLimit => "project-adapter-wall-time-fallback",
            Self::InputLimit => "project-adapter-input-limit-fallback",
            Self::OutputLimit => "project-adapter-output-limit-fallback",
            Self::MemoryLimit => "project-adapter-memory-limit-fallback",
            Self::ProcessFailure => "project-adapter-process-fallback",
            Self::Analysis => "project-adapter-analysis-fallback",
            Self::Capacity => "project-adapter-capacity-fallback",
            Self::FactsTruncated => "project-adapter-facts-truncated-fallback",
            Self::StructuralDeclarationLoss { .. } => "project-adapter-declaration-loss-fallback",
        }
    }
}

/// Daemon-supplied whole-project analyzer used only through native isolation.
pub trait FirstSliceProjectAnalyzer: Send + Sync {
    /// Returns the exact adapter-binary identity included in generation identity.
    fn provider_identity(&self) -> ContentHash;

    /// Produces one atomic language-project document.
    ///
    /// Implementations must not execute repository-owned code or expose local
    /// repository paths to the child process.
    ///
    /// # Errors
    ///
    /// Returns a source-free boundary failure. Cancellation must be reported as
    /// [`FirstSliceProjectAnalysisError::Cancelled`] instead of selecting a
    /// structural fallback.
    fn analyze(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError>;

    /// Produces one project document while reporting completed provider work.
    ///
    /// Implementations with multiple isolated partitions should override this
    /// method and emit after each completed partition. The default preserves
    /// compatibility for single-transaction providers.
    ///
    /// # Errors
    ///
    /// Returns the same source-free provider or cancellation failures as
    /// [`Self::analyze`].
    fn analyze_with_progress(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
        observe_progress: &mut dyn FnMut(FirstSliceProjectAnalysisProgress),
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
        let completed = FirstSliceProjectAnalysisProgress::complete(&request)?;
        let analysis = self.analyze(request, cancellation)?;
        observe_progress(completed);
        Ok(analysis)
    }
}

type PendingRepositoryRegistration = (RepositoryId, String, Option<String>);

fn smoke_query_restored_generation(
    restored: &RestoredGeneration,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    check_cancellation(cancellation)?;
    let service = QueryService::new(restored.verified.snapshot(), &restored.search)
        .map_err(|error| map_query_error(error, cancellation))?;
    let plan = service
        .plan_code_locate(
            RECOVERY_SMOKE_QUERY.to_owned(),
            LocateMode::Exact,
            1,
            0,
            SearchBudget::default(),
            QueryBudget::default(),
        )
        .map_err(|error| map_query_error(error, cancellation))?;
    service
        .execute_code_locate(&plan, cancellation)
        .map_err(|error| map_query_error(error, cancellation))?;
    Ok(())
}

/// Transport-independent owner of bounded repository generations.
///
/// The service retains at most the caller-selected hard-bounded generation
/// count, 64 MiB of deduplicated source content, and 64 MiB of logically
/// accounted parser artifacts. The default constructor is process-local;
/// [`Self::new_durable`] publishes normalized SQLite, source, and activation
/// state beneath an already prepared account-private state root.
pub struct FirstSliceService {
    config: ConfigSnapshot,
    analysis_limits: AnalysisLimits,
    extensions: ExtensionSupport,
    analyzers: BTreeMap<String, TreeSitterAnalyzer>,
    project_analyzer: Option<Arc<dyn FirstSliceProjectAnalyzer>>,
    // The canonical-root digest remains an internal lookup key. Durable mode
    // persists the random repository UUID instead of deriving public identity
    // from a local path.
    repositories: BTreeMap<ContentHash, RepositoryId>,
    pending_repository_registrations: Mutex<BTreeMap<ContentHash, PendingRepositoryRegistration>>,
    repository_display_names: BTreeMap<RepositoryId, String>,
    repository_root_paths: BTreeMap<RepositoryId, String>,
    repository_aliases: BTreeMap<RepositoryId, String>,
    repository_metadata_sequences: BTreeMap<RepositoryId, u64>,
    published_generation_counts: BTreeMap<RepositoryId, u64>,
    active_by_repository: BTreeMap<RepositoryId, GenerationId>,
    generations: GenerationSet<LexicalIndex>,
    // Language coverage is immutable generation metadata. Precomputing it at
    // publication keeps catalog reads independent of normalized IR size.
    language_coverage_by_generation: BTreeMap<GenerationId, Vec<LanguageCoverageSummary>>,
    source_snapshots: SourceSnapshotRetention,
    structural_artifacts: StructuralArtifactRetention,
    receipts: BTreeMap<GenerationId, FirstSliceIndexReceipt>,
    incremental_baselines: BTreeMap<GenerationId, IncrementalDiscoveryBaseline>,
    incremental_inputs: BTreeMap<GenerationId, InputSnapshot>,
    incremental_evidence: BTreeMap<GenerationId, FirstSliceIncrementalEvidence>,
    generation_memory_bytes: BTreeMap<GenerationId, u64>,
    // Catalog pagination mutates only bounded cursor state. Its independent
    // lock keeps catalog reads concurrent with long immutable analysis reads.
    catalog_snapshots: Mutex<CatalogSnapshotStore>,
    durable: Option<Arc<DurableCatalog>>,
    storage_policy: FirstSliceStoragePolicy,
    maximum_generations_per_repository: usize,
    maximum_repositories: usize,
    activation_sequences: BTreeMap<RepositoryId, u64>,
    global_activation_sequence: u64,
    activation_order_by_generation: BTreeMap<GenerationId, u64>,
    most_recent_activation: Option<(u64, GenerationId)>,
    durable_operations: BTreeMap<OperationId, FirstSliceDurableOperation>,
    pending_durable_compactions: BTreeSet<RepositoryId>,
    #[cfg(test)]
    available_disk_bytes_override: Option<u64>,
}

impl FirstSliceService {
    /// Creates the bounded structural indexing service.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] when required local identity randomness,
    /// parser, analyzer, configuration, generation-retention, or
    /// source-retention state cannot initialize.
    pub fn new(maximum_generations: usize) -> Result<Self, FirstSliceError> {
        Self::new_with_source_limit(maximum_generations, MAX_RETAINED_SOURCE_BYTES)
    }

    /// Opens and restores the bounded durable structural indexing service.
    ///
    /// The caller must prepare `state_root` as an account-private directory.
    /// Only activation-marked immutable generations become queryable; abandoned
    /// staging trees and unactivated generation trees are removed during
    /// recovery.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unsupported or insecure platform
    /// boundary, corrupt durable state, cancellation, or bounded initialization
    /// and retention failures.
    pub fn new_durable(
        maximum_generations: usize,
        state_root: &Path,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        Self::new_durable_with_policy(
            FirstSliceStoragePolicy::legacy(maximum_generations),
            state_root,
            cancellation,
        )
    }

    /// Opens and restores durable state under an effective storage policy.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::new_durable`].
    pub fn new_durable_with_policy(
        policy: FirstSliceStoragePolicy,
        state_root: &Path,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        let (mut service, deferred) =
            Self::open_durable_deferred_with_optional_project_analyzer(policy, state_root, None)?;
        let restored = deferred.restore(cancellation)?;
        service.install_deferred_restore(restored, cancellation)?;
        Ok(service)
    }

    /// Opens the durable service with one authenticated project analyzer.
    ///
    /// The exact provider identity participates in generation derivation.
    /// Project output is accepted only when the provider also reports native
    /// isolation evidence; all other non-cancellation failures select an
    /// explicit structural fallback.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::new_durable`].
    pub fn new_durable_with_project_analyzer(
        maximum_generations: usize,
        state_root: &Path,
        project_analyzer: Arc<dyn FirstSliceProjectAnalyzer>,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        let policy = FirstSliceStoragePolicy::legacy(maximum_generations);
        let (mut service, deferred) = Self::open_durable_deferred_with_optional_project_analyzer(
            policy,
            state_root,
            Some(project_analyzer),
        )?;
        let restored = deferred.restore(cancellation)?;
        service.install_deferred_restore(restored, cancellation)?;
        Ok(service)
    }

    /// Opens durable publication state without scanning retained generations.
    ///
    /// The returned recovery token can be evaluated on a background worker and
    /// then installed through [`Self::install_deferred_restore`].
    ///
    /// # Errors
    ///
    /// Returns an error when the durable boundary or bounded in-memory service
    /// cannot initialize.
    pub fn open_durable_deferred(
        maximum_generations: usize,
        state_root: &Path,
    ) -> Result<(Self, FirstSliceDeferredRestore), FirstSliceError> {
        Self::open_durable_deferred_with_policy(
            FirstSliceStoragePolicy::legacy(maximum_generations),
            state_root,
        )
    }

    /// Opens deferred durable state under an effective storage policy.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::open_durable_deferred`].
    pub fn open_durable_deferred_with_policy(
        policy: FirstSliceStoragePolicy,
        state_root: &Path,
    ) -> Result<(Self, FirstSliceDeferredRestore), FirstSliceError> {
        Self::open_durable_deferred_with_optional_project_analyzer(policy, state_root, None)
    }

    /// Opens deferred durable state with one authenticated project analyzer.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::open_durable_deferred`].
    pub fn open_durable_deferred_with_project_analyzer(
        maximum_generations: usize,
        state_root: &Path,
        project_analyzer: Arc<dyn FirstSliceProjectAnalyzer>,
    ) -> Result<(Self, FirstSliceDeferredRestore), FirstSliceError> {
        Self::open_durable_deferred_with_project_analyzer_and_policy(
            FirstSliceStoragePolicy::legacy(maximum_generations),
            state_root,
            project_analyzer,
        )
    }

    /// Opens deferred durable state with an analyzer and effective storage policy.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::open_durable_deferred`].
    pub fn open_durable_deferred_with_project_analyzer_and_policy(
        policy: FirstSliceStoragePolicy,
        state_root: &Path,
        project_analyzer: Arc<dyn FirstSliceProjectAnalyzer>,
    ) -> Result<(Self, FirstSliceDeferredRestore), FirstSliceError> {
        Self::open_durable_deferred_with_optional_project_analyzer(
            policy,
            state_root,
            Some(project_analyzer),
        )
    }

    fn open_durable_deferred_with_optional_project_analyzer(
        policy: FirstSliceStoragePolicy,
        state_root: &Path,
        project_analyzer: Option<Arc<dyn FirstSliceProjectAnalyzer>>,
    ) -> Result<(Self, FirstSliceDeferredRestore), FirstSliceError> {
        let maximum_generations = policy.retained_generations();
        let maximum_repositories = policy.effective_maximum_repositories(maximum_generations)?;
        let durable = Arc::new(DurableCatalog::open(
            state_root,
            maximum_generations,
            maximum_repositories,
        )?);
        let active_targets = durable.active_restore_targets()?;
        let mut service = Self::new_with_storage_policy(
            maximum_generations,
            MAX_RETAINED_SOURCE_BYTES,
            Some(Arc::clone(&durable)),
            project_analyzer,
            policy,
        )?;
        service.bootstrap_deferred_restore(&active_targets)?;
        Ok((
            service,
            FirstSliceDeferredRestore {
                durable,
                active_targets,
            },
        ))
    }

    fn bootstrap_deferred_restore(
        &mut self,
        targets: &[FirstSliceRecoveryTarget],
    ) -> Result<(), FirstSliceError> {
        for target in targets {
            self.restore_repository_registration(target.root_identity, target.repository)?;
            self.activation_sequences
                .insert(target.repository, target.activation_sequence);
            self.published_generation_counts
                .insert(target.repository, target.published_generation_count);
            self.global_activation_sequence = self
                .global_activation_sequence
                .max(target.global_activation_sequence);
        }
        Ok(())
    }

    /// Installs fully verified durable state into an otherwise empty service.
    ///
    /// # Errors
    ///
    /// Returns a typed cancellation, integrity, or bounded-retention failure.
    pub fn install_deferred_restore(
        &mut self,
        restored: FirstSliceRestoredState,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        if !self.repositories.is_empty()
            || !self.receipts.is_empty()
            || !self.generations.is_empty()
        {
            return Err(FirstSliceError::Retention);
        }
        self.install_restored(restored.generations, true, cancellation)
    }

    /// Installs one independently verified active-recovery batch.
    ///
    /// Unlike [`Self::install_deferred_restore`], this permits prior recovered
    /// repositories and activates the newest generation in the supplied batch.
    ///
    /// # Errors
    ///
    /// Returns a typed cancellation, integrity, or bounded-retention failure.
    pub fn install_progressive_deferred_restore(
        &mut self,
        restored: FirstSliceRestoredState,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        self.install_restored(restored.generations, true, cancellation)
    }

    /// Adds verified retained predecessors without changing active pointers.
    ///
    /// # Errors
    ///
    /// Returns a typed integrity, cancellation, or retention failure.
    pub fn install_additional_deferred_restore(
        &mut self,
        restored: FirstSliceRestoredState,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        if self.repositories.is_empty() || self.receipts.is_empty() || self.generations.is_empty() {
            return Err(FirstSliceError::Retention);
        }
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(restored.generations.len())
            .map_err(|_| FirstSliceError::Retention)?;
        for generation in restored.generations {
            let receipt = &generation.receipt;
            let Some(installed) = self.receipts.get(&receipt.generation) else {
                pending.push(generation);
                continue;
            };
            if installed != receipt
                || self.repositories.get(&generation.root_identity) != Some(&receipt.repository)
                || self
                    .repository_display_names
                    .get(&receipt.repository)
                    .is_none_or(|display_name| display_name != &generation.display_name)
                || self.repository_root_paths.get(&receipt.repository)
                    != generation.root_path.as_ref()
                || self.repository_aliases.get(&receipt.repository) != generation.alias.as_ref()
                || self
                    .repository_metadata_sequences
                    .get(&receipt.repository)
                    .copied()
                    .unwrap_or(0)
                    != generation.metadata_sequence
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        self.install_restored(pending, false, cancellation)
    }

    /// Resolves an already registered repository from its canonical root.
    ///
    /// This lookup never registers a new repository. It allows operation
    /// metadata to retain the identity of an update that fails before staging.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] when canonicalization, cancellation, or
    /// root-identity hashing fails.
    pub fn registered_repository_for_root(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<Option<RepositoryId>, FirstSliceError> {
        let canonical = canonical_repository_root(path, cancellation)?;
        let root_identity = repository_path_hash(&canonical)?;
        if let Some(repository) = self.repositories.get(&root_identity).copied() {
            return Ok(Some(repository));
        }
        self.pending_repository_registrations
            .lock()
            .map_err(|_| FirstSliceError::Retention)
            .map(|pending| {
                pending
                    .get(&root_identity)
                    .map(|(repository, _, _)| *repository)
            })
    }

    /// Returns every committed repository with its retained canonical root.
    ///
    /// The result is repository-ID ordered and bounded by the service's fixed
    /// repository retention ceiling. Pending admissions are excluded.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Retention`] when the bounded result allocation fails.
    pub fn registered_repository_roots(
        &self,
    ) -> Result<Vec<FirstSliceRepositoryRoot>, FirstSliceError> {
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(self.repository_root_paths.len())
            .map_err(|_| FirstSliceError::Retention)?;
        for (repository, root) in &self.repository_root_paths {
            if !self.active_by_repository.contains_key(repository) {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            roots.push(FirstSliceRepositoryRoot {
                repository: *repository,
                root: PathBuf::from(root),
            });
        }
        Ok(roots)
    }

    /// Collects bounded changed paths from the selected working-tree states and
    /// optional two-dot revision range.
    ///
    /// Collection is read-only and uses the canonical root retained by the
    /// service. Repository hooks, filters, prompts, lazy fetches, and writes
    /// remain disabled by the Git evidence boundary.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceGitEvidenceError`] for an unknown repository,
    /// invalid selector or limits, unavailable Git evidence, or cancellation.
    pub fn collect_git_change_paths(
        &self,
        repository: RepositoryId,
        working_tree: Option<FirstSliceWorkingTreeSelection>,
        revision_range: Option<(&str, &str)>,
        maximum_output_bytes: usize,
        command_timeout: Duration,
        cancellation: &Cancellation,
    ) -> Result<Vec<String>, FirstSliceGitEvidenceError> {
        if working_tree.is_none() && revision_range.is_none() {
            return Ok(Vec::new());
        }
        let root = self.git_repository_root(repository)?;
        let (limits, collect_limits) =
            first_slice_git_limits(maximum_output_bytes, command_timeout)?;
        let mut paths = BTreeSet::new();
        if let Some(selection) = working_tree {
            let snapshot =
                collect_repository(&root, repository, &limits, collect_limits, cancellation)
                    .map_err(|error| first_slice_git_error(error.code()))?;
            for change_set in &snapshot.as_input().change_sets {
                if first_slice_working_tree_change_matches(selection, change_set) {
                    collect_first_slice_git_change_paths(change_set, &mut paths);
                }
            }
        }
        if let Some((base, head)) = revision_range {
            validate_first_slice_git_revision(base)?;
            validate_first_slice_git_revision(head)?;
            let change_set =
                collect_revision_range(&root, base, head, &limits, collect_limits, cancellation)
                    .map_err(|error| first_slice_git_error(error.code()))?;
            collect_first_slice_git_change_paths(&change_set, &mut paths);
        }
        if paths.len() > MAX_FIRST_SLICE_GIT_CHANGE_PATHS {
            return Err(FirstSliceGitEvidenceError::Unavailable);
        }
        Ok(paths.into_iter().collect())
    }

    /// Verifies that up to two Git revisions denote the clean checked-out
    /// state represented by the active generation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceGitEvidenceError`] for an unknown repository,
    /// invalid selector or limits, unavailable Git evidence, or cancellation.
    pub fn git_revisions_match_clean_head(
        &self,
        repository: RepositoryId,
        revisions: &[&str],
        maximum_output_bytes: usize,
        command_timeout: Duration,
        cancellation: &Cancellation,
    ) -> Result<bool, FirstSliceGitEvidenceError> {
        if revisions.len() > 2 {
            return Err(FirstSliceGitEvidenceError::InvalidSelector);
        }
        let root = self.git_repository_root(repository)?;
        let (limits, collect_limits) =
            first_slice_git_limits(maximum_output_bytes, command_timeout)?;
        for revision in revisions {
            validate_first_slice_git_revision(revision)?;
            if *revision != "HEAD"
                && !revision_resolves_to_head(&root, revision, collect_limits, cancellation)
                    .map_err(|error| first_slice_git_error(error.code()))?
            {
                return Ok(false);
            }
        }
        let status = collect_worktree_status(&root, &limits, collect_limits, cancellation)
            .map_err(|error| first_slice_git_error(error.code()))?;
        Ok(status.tracked_changes == 0 && status.untracked_paths == 0 && status.conflicts == 0)
    }

    fn git_repository_root(
        &self,
        repository: RepositoryId,
    ) -> Result<PathBuf, FirstSliceGitEvidenceError> {
        if !self.active_by_repository.contains_key(&repository) {
            return Err(FirstSliceGitEvidenceError::Unavailable);
        }
        self.repository_root_paths
            .get(&repository)
            .map(PathBuf::from)
            .ok_or(FirstSliceGitEvidenceError::Unavailable)
    }

    /// Reserves a repository identity and worst-case durable capacity.
    ///
    /// This admission intentionally precedes source discovery and parsing. A
    /// detached caller can therefore receive a stable operation handle without
    /// allowing expensive work to begin before its bounded disk reservation is
    /// known to fit.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] when the root is invalid, repository
    /// retention is full, random identity generation fails, cancellation wins,
    /// or the durable state root lacks the conservative staging capacity.
    pub fn admit_repository(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexAdmission, FirstSliceError> {
        self.admit_repository_with_storage_timing(
            path,
            cancellation,
            DurableStorageAdmissionTiming::Immediate,
        )
    }

    /// Reserves a repository identity for a detached durable operation.
    ///
    /// Durable capacity is still admitted atomically before discovery begins.
    /// Deferring only the physical catalog scan lets the caller publish the
    /// durable operation handle before a cold, bounded inventory walk.
    ///
    /// # Errors
    ///
    /// Returns the root, repository-retention, random-identity, and
    /// cancellation failures from [`Self::admit_repository`]. Storage failures
    /// are returned by the subsequent preparation boundary.
    pub fn admit_repository_for_detached_operation(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexAdmission, FirstSliceError> {
        self.admit_repository_with_storage_timing(
            path,
            cancellation,
            DurableStorageAdmissionTiming::BeforePreparation,
        )
    }

    fn admit_repository_with_storage_timing(
        &self,
        path: &Path,
        cancellation: &Cancellation,
        storage_timing: DurableStorageAdmissionTiming,
    ) -> Result<FirstSliceIndexAdmission, FirstSliceError> {
        require_deadline(cancellation)?;
        check_cancellation(cancellation)?;
        let canonical = canonical_repository_root(path, cancellation)?;
        let root_identity = repository_path_hash(&canonical)?;
        let root_path = sanitized_repository_root_path(&canonical)?;
        let mut pending = self
            .pending_repository_registrations
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let (repository, reservation_inserted) = if let Some(repository) =
            self.repositories.get(&root_identity).copied()
        {
            (repository, false)
        } else if let Some((repository, display_name, pending_root_path)) =
            pending.get_mut(&root_identity)
        {
            *display_name = sanitized_repository_display_name(&canonical, *repository)?;
            *pending_root_path = Some(root_path.clone());
            (*repository, false)
        } else {
            let retained_repositories = self
                .repository_display_names
                .len()
                .checked_add(pending.len())
                .ok_or(FirstSliceError::Retention)?;
            if retained_repositories >= self.maximum_repositories {
                return Err(self.repository_capacity_limit(retained_repositories, pending.len()));
            }
            let repository = random_repository_id_with_pending(&self.repositories, &pending)?;
            let display_name = sanitized_repository_display_name(&canonical, repository)?;
            // Opening the root now proves the reserved identity names a valid
            // directory before the operation is acknowledged to the caller.
            let _root =
                RepositoryRoot::open(repository, path).map_err(|_| FirstSliceError::Repository)?;
            pending.insert(root_identity, (repository, display_name, Some(root_path)));
            (repository, true)
        };
        drop(pending);
        let maximum_source_bytes =
            u64::try_from(MAX_RETAINED_SOURCE_BYTES).map_err(|_| FirstSliceError::Limits)?;
        let estimated_disk_bytes =
            self.durable_initial_admission_reservation(maximum_source_bytes)?;
        let storage_admission = match storage_timing {
            DurableStorageAdmissionTiming::Immediate => {
                match self.begin_durable_storage_reservation(repository, estimated_disk_bytes) {
                    Ok(reservation) => DurableStorageAdmissionState::Reserved(reservation),
                    Err(error) => {
                        self.release_index_admission(FirstSliceIndexAdmission {
                            repository,
                            root_identity,
                            parent: self.active_by_repository.get(&repository).copied(),
                            estimated_disk_bytes,
                            reservation_inserted,
                            storage_admission: DurableStorageAdmissionState::Reserved(None),
                        });
                        return Err(error);
                    }
                }
            }
            DurableStorageAdmissionTiming::BeforePreparation => {
                DurableStorageAdmissionState::Deferred
            }
        };
        if let Err(error) = check_cancellation(cancellation) {
            self.release_index_admission(FirstSliceIndexAdmission {
                repository,
                root_identity,
                parent: self.active_by_repository.get(&repository).copied(),
                estimated_disk_bytes,
                reservation_inserted,
                storage_admission,
            });
            return Err(error);
        }
        Ok(FirstSliceIndexAdmission {
            repository,
            root_identity,
            parent: self.active_by_repository.get(&repository).copied(),
            estimated_disk_bytes,
            reservation_inserted,
            storage_admission,
        })
    }

    fn repository_capacity_limit(
        &self,
        observed: usize,
        pending_repositories: usize,
    ) -> FirstSliceError {
        let configured =
            usize::try_from(self.storage_policy.maximum_repositories).unwrap_or(usize::MAX);
        let configuration = if self.maximum_repositories < configured {
            FirstSliceRepositoryCapacityConfiguration::RetainedGenerations
        } else {
            FirstSliceRepositoryCapacityConfiguration::MaximumRepositories
        };
        repository_capacity_limit(
            observed,
            self.maximum_repositories,
            configuration,
            pending_repositories,
            self.repository_display_names.len(),
        )
    }

    /// Compatibility wrapper for callers using the original Rust-first API name.
    ///
    /// Admission is language-independent and has the same behavior as
    /// [`Self::admit_repository`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::admit_repository`].
    pub fn admit_rust_fixture(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexAdmission, FirstSliceError> {
        self.admit_repository(path, cancellation)
    }

    /// Releases an uncommitted repository identity reservation.
    pub fn release_index_admission(&self, admission: FirstSliceIndexAdmission) {
        if let (Some(durable), DurableStorageAdmissionState::Reserved(Some(reservation))) =
            (&self.durable, admission.storage_admission)
        {
            let _ = durable.release_staging_reservation(reservation);
        }
        if !admission.reservation_inserted {
            return;
        }
        if let Ok(mut pending) = self.pending_repository_registrations.lock() {
            pending.retain(|_, (candidate, _, _)| *candidate != admission.repository);
        }
    }

    /// Restores one durable root-to-repository reservation without source paths.
    ///
    /// The next admission for the same canonical root replaces the source-free
    /// fallback display name before a generation can be published.
    ///
    /// # Errors
    ///
    /// Returns a catalog-integrity or bounded-retention failure for conflicting
    /// root identities, repository identities, or capacity.
    pub fn restore_repository_registration(
        &self,
        root_identity: ContentHash,
        repository: RepositoryId,
    ) -> Result<(), FirstSliceError> {
        if let Some(existing) = self.repositories.get(&root_identity) {
            return (*existing == repository)
                .then_some(())
                .ok_or(FirstSliceError::CatalogCorrupt);
        }
        if self.repositories.iter().any(|(candidate_root, candidate)| {
            *candidate == repository && *candidate_root != root_identity
        }) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let mut pending = self
            .pending_repository_registrations
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        if let Some((existing, _, _)) = pending.get(&root_identity) {
            return (*existing == repository)
                .then_some(())
                .ok_or(FirstSliceError::CatalogCorrupt);
        }
        if pending.iter().any(|(candidate_root, (candidate, _, _))| {
            *candidate == repository && *candidate_root != root_identity
        }) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        if self
            .repository_display_names
            .len()
            .checked_add(pending.len())
            .ok_or(FirstSliceError::Retention)?
            >= self.maximum_repositories
        {
            return Err(self.repository_capacity_limit(
                self.repository_display_names.len() + pending.len(),
                pending.len(),
            ));
        }
        pending.insert(root_identity, (repository, repository.to_string(), None));
        Ok(())
    }

    fn new_with_source_limit(
        maximum_generations: usize,
        maximum_source_bytes: usize,
    ) -> Result<Self, FirstSliceError> {
        let storage_policy =
            FirstSliceStoragePolicy::resolve_user_config(None, maximum_generations)?;
        Self::new_with_storage_policy(
            maximum_generations,
            maximum_source_bytes,
            None,
            None,
            storage_policy,
        )
    }

    #[cfg(test)]
    fn new_with_storage(
        maximum_generations: usize,
        maximum_retained_source_bytes: usize,
        durable: Option<Arc<DurableCatalog>>,
        project_analyzer: Option<Arc<dyn FirstSliceProjectAnalyzer>>,
    ) -> Result<Self, FirstSliceError> {
        Self::new_with_storage_policy(
            maximum_generations,
            maximum_retained_source_bytes,
            durable,
            project_analyzer,
            FirstSliceStoragePolicy::legacy(maximum_generations),
        )
    }

    fn new_with_storage_policy(
        maximum_generations: usize,
        maximum_retained_source_bytes: usize,
        durable: Option<Arc<DurableCatalog>>,
        project_analyzer: Option<Arc<dyn FirstSliceProjectAnalyzer>>,
        storage_policy: FirstSliceStoragePolicy,
    ) -> Result<Self, FirstSliceError> {
        let maximum_repositories =
            storage_policy.effective_maximum_repositories(maximum_generations)?;
        let total_generation_capacity = maximum_generations
            .checked_mul(maximum_repositories)
            .and_then(|capacity| capacity.checked_add(1))
            .ok_or(FirstSliceError::Retention)?;
        let config = storage_policy.config.clone();
        // Discovery, the capability snapshot, and parser admission must share
        // one effective source-file ceiling. Divergent limits make a file pass
        // discovery only to fail as an unrelated repository error later.
        let maximum_source_bytes = usize::try_from(config.analysis().max_source_file_bytes)
            .map_err(|_| FirstSliceError::Limits)?;
        let analysis_limits = analysis_limits(maximum_source_bytes)?;
        let parser = Arc::new(
            TreeSitterProvider::new(parser_config(maximum_source_bytes)?)
                .map_err(|_| FirstSliceError::Adapter)?,
        );
        let registry = GrammarRegistry::audited().map_err(|_| FirstSliceError::Adapter)?;
        let producer =
            ProducerIdentity::new("rootlight-first-slice-treesitter", "1.0", config.hash())
                .map_err(|_| FirstSliceError::Adapter)?;
        let mut analyzers = BTreeMap::new();
        for descriptor in registry.descriptors() {
            let language = descriptor.language().clone();
            let frontend_version = format!(
                "tree-sitter-{}-{}",
                language.as_str(),
                descriptor.grammar_version()
            );
            let parse_provider: Arc<dyn ParseProvider> =
                Arc::clone(&parser) as Arc<dyn ParseProvider>;
            let analyzer = if language.as_str() == "rust" {
                TreeSitterAnalyzer::new_rust_structural(
                    parse_provider,
                    producer.clone(),
                    language,
                    &frontend_version,
                    grammar_binary_digest(descriptor)?,
                )
            } else {
                TreeSitterAnalyzer::new(
                    parse_provider,
                    producer.clone(),
                    language,
                    &frontend_version,
                    grammar_binary_digest(descriptor)?,
                )
            }
            .map_err(|_| FirstSliceError::Adapter)?;
            if analyzers
                .insert(descriptor.language().as_str().to_owned(), analyzer)
                .is_some()
            {
                return Err(FirstSliceError::Adapter);
            }
        }
        if analyzers.len() != registry.descriptors().len() {
            return Err(FirstSliceError::Adapter);
        }
        let generations = GenerationSet::new(total_generation_capacity)
            .map_err(|_| FirstSliceError::Retention)?;
        let source_snapshots =
            SourceSnapshotRetention::new(total_generation_capacity, maximum_retained_source_bytes)?;
        let structural_artifacts = StructuralArtifactRetention::new(
            total_generation_capacity,
            MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES,
        )?;
        let mut catalog_instance_nonce = [0_u8; 32];
        getrandom::fill(&mut catalog_instance_nonce)
            .map_err(|_| FirstSliceError::RandomUnavailable)?;
        Ok(Self {
            config,
            analysis_limits,
            extensions: ExtensionSupport::default(),
            analyzers,
            project_analyzer,
            repositories: BTreeMap::new(),
            pending_repository_registrations: Mutex::new(BTreeMap::new()),
            repository_display_names: BTreeMap::new(),
            repository_root_paths: BTreeMap::new(),
            repository_aliases: BTreeMap::new(),
            repository_metadata_sequences: BTreeMap::new(),
            published_generation_counts: BTreeMap::new(),
            active_by_repository: BTreeMap::new(),
            generations,
            language_coverage_by_generation: BTreeMap::new(),
            source_snapshots,
            structural_artifacts,
            receipts: BTreeMap::new(),
            incremental_baselines: BTreeMap::new(),
            incremental_inputs: BTreeMap::new(),
            incremental_evidence: BTreeMap::new(),
            generation_memory_bytes: BTreeMap::new(),
            catalog_snapshots: Mutex::new(CatalogSnapshotStore::new(
                catalog::CatalogSnapshotLimits::default(),
                catalog_instance_nonce,
            )),
            durable,
            storage_policy,
            maximum_generations_per_repository: maximum_generations,
            maximum_repositories,
            activation_sequences: BTreeMap::new(),
            global_activation_sequence: 0,
            activation_order_by_generation: BTreeMap::new(),
            most_recent_activation: None,
            durable_operations: BTreeMap::new(),
            pending_durable_compactions: BTreeSet::new(),
            #[cfg(test)]
            available_disk_bytes_override: None,
        })
    }

    fn begin_durable_storage_reservation(
        &self,
        repository: RepositoryId,
        required_catalog_bytes: u64,
    ) -> Result<Option<DurableStorageReservation>, FirstSliceError> {
        let Some(durable) = &self.durable else {
            return Ok(None);
        };
        #[cfg(test)]
        if let Some(available_bytes) = self.available_disk_bytes_override {
            if available_bytes < required_catalog_bytes {
                return Err(FirstSliceError::InsufficientDiskSpace {
                    required_bytes: required_catalog_bytes,
                    available_bytes,
                });
            }
            return Ok(None);
        }
        let policy = self.durable_storage_admission_policy(required_catalog_bytes, 0);
        durable
            .ensure_staging_capacity(repository, policy)?
            .map(|(_, reservation)| Some(reservation))
            .map_err(storage_admission_error)
    }

    fn complete_durable_storage_admission(
        &self,
        admission: &mut FirstSliceIndexAdmission,
    ) -> Result<(), FirstSliceError> {
        if matches!(
            admission.storage_admission,
            DurableStorageAdmissionState::Reserved(_)
        ) {
            return Ok(());
        }
        let reservation = self.begin_durable_storage_reservation(
            admission.repository,
            admission.estimated_disk_bytes,
        )?;
        admission.storage_admission = DurableStorageAdmissionState::Reserved(reservation);
        Ok(())
    }

    fn resize_durable_storage_reservation(
        &self,
        reservation: Option<&DurableStorageReservation>,
        required_catalog_bytes: u64,
        required_repository_bytes: u64,
    ) -> Result<(), FirstSliceError> {
        let (Some(durable), Some(reservation)) = (&self.durable, reservation) else {
            return Ok(());
        };
        #[cfg(test)]
        if let Some(available_bytes) = self.available_disk_bytes_override {
            if available_bytes < required_catalog_bytes {
                return Err(FirstSliceError::InsufficientDiskSpace {
                    required_bytes: required_catalog_bytes,
                    available_bytes,
                });
            }
            return Ok(());
        }
        durable
            .resize_staging_reservation(
                reservation,
                self.durable_storage_admission_policy(
                    required_catalog_bytes,
                    required_repository_bytes,
                ),
            )?
            .map(|_| ())
            .map_err(storage_admission_error)
    }

    const fn durable_storage_admission_policy(
        &self,
        required_catalog_bytes: u64,
        required_repository_bytes: u64,
    ) -> DurableStorageAdmissionPolicy {
        DurableStorageAdmissionPolicy {
            required_catalog_bytes,
            required_repository_bytes,
            maximum_repository_bytes: self.storage_policy.maximum_repository_bytes,
            maximum_storage_bytes: self.storage_policy.maximum_catalog_bytes,
            minimum_free_bytes: self.storage_policy.minimum_free_disk_bytes,
        }
    }

    fn durable_staging_reservation(&self, source_bytes: u64) -> Result<u64, FirstSliceError> {
        source_bytes
            .checked_mul(self.storage_policy.source_reservation_factor)
            .and_then(|bytes| bytes.checked_add(DURABLE_STAGING_FIXED_OVERHEAD_BYTES))
            .ok_or(FirstSliceError::Limits)
    }

    fn durable_initial_admission_reservation(
        &self,
        source_bytes: u64,
    ) -> Result<u64, FirstSliceError> {
        let required_bytes = source_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(DURABLE_STAGING_FIXED_OVERHEAD_BYTES))
            .ok_or(FirstSliceError::Limits)?;
        if self.storage_policy.minimum_free_disk_bytes == 0 {
            required_bytes
                .checked_add(DURABLE_DISK_SAFETY_MARGIN_BYTES)
                .ok_or(FirstSliceError::Limits)
        } else {
            Ok(required_bytes)
        }
    }

    fn durable_output_reservation(
        &self,
        source_bytes: u64,
        serialized_document_bytes: u64,
        representation: DurableGenerationRepresentation,
    ) -> Result<u64, FirstSliceError> {
        let generation_bytes = match representation {
            DurableGenerationRepresentation::Oracle => serialized_document_bytes
                .checked_mul(self.storage_policy.oracle_reservation_factor)
                .ok_or(FirstSliceError::Limits)?,
            DurableGenerationRepresentation::RecoverySnapshot => {
                recovery_snapshot_output_reservation(serialized_document_bytes)?
            }
        };
        source_bytes
            .checked_add(generation_bytes)
            .and_then(|bytes| bytes.checked_add(DURABLE_STAGING_FIXED_OVERHEAD_BYTES))
            .ok_or(FirstSliceError::Limits)
    }

    /// Reports whether an authenticated whole-project analyzer is configured.
    #[must_use]
    pub fn deep_analysis_available(&self) -> bool {
        self.project_analyzer.is_some()
    }

    fn provider_set_hash(&self, mode: FirstSliceIndexMode) -> Result<ContentHash, FirstSliceError> {
        let structural = first_slice_provider_set_hash()?;
        if mode == FirstSliceIndexMode::Structural {
            return Ok(structural);
        }
        let Some(project_analyzer) = &self.project_analyzer else {
            return hash_static_components(&[
                PROJECT_PROVIDER_SET_SEED,
                structural.as_bytes(),
                b"unavailable",
            ]);
        };
        let project = project_analyzer.provider_identity();
        hash_static_components(&[
            PROJECT_PROVIDER_SET_SEED,
            structural.as_bytes(),
            project.as_bytes(),
        ])
    }

    #[cfg(test)]
    fn set_available_disk_bytes_override(&mut self, available_bytes: u64) {
        self.available_disk_bytes_override = Some(available_bytes);
    }

    fn install_restored(
        &mut self,
        restored: Vec<RestoredGeneration>,
        activate_latest: bool,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        if activate_latest {
            for generation in &restored {
                smoke_query_restored_generation(generation, cancellation)?;
            }
        }
        let mut active = BTreeMap::<RepositoryId, (u64, GenerationId)>::new();
        let mut legacy_generation_counts = BTreeMap::<RepositoryId, u64>::new();
        let mut global_activation_order = self
            .activation_order_by_generation
            .iter()
            .filter_map(|(generation, sequence)| {
                (*sequence > 0).then_some((*sequence, *generation))
            })
            .collect::<BTreeMap<_, _>>();
        let mut legacy_most_recent = None::<(u64, RepositoryId, GenerationId)>;
        for restored in restored {
            check_cancellation(cancellation)?;
            let receipt = restored.receipt;
            let language_coverage = language_coverage(restored.verified.document());
            let serialized_document_bytes =
                normalized_document_serialized_bytes(restored.verified.document())?;
            let memory_bytes = ensure_generation_memory_admission(serialized_document_bytes)?;
            if self.receipts.contains_key(&receipt.generation)
                || self
                    .repositories
                    .get(&restored.root_identity)
                    .is_some_and(|repository| *repository != receipt.repository)
                || self
                    .repository_display_names
                    .get(&receipt.repository)
                    .is_some_and(|display_name| *display_name != restored.display_name)
                || self
                    .repository_root_paths
                    .get(&receipt.repository)
                    .is_some_and(|root_path| Some(root_path) != restored.root_path.as_ref())
                || self
                    .repository_aliases
                    .get(&receipt.repository)
                    .is_some_and(|alias| Some(alias) != restored.alias.as_ref())
                || self
                    .repository_metadata_sequences
                    .get(&receipt.repository)
                    .is_some_and(|sequence| *sequence != restored.metadata_sequence)
                || self.repositories.iter().any(|(root_identity, repository)| {
                    *repository == receipt.repository && *root_identity != restored.root_identity
                })
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            self.make_room_for_generation(receipt.repository, memory_bytes)?;
            let source_admission =
                self.source_snapshots
                    .admit(receipt.generation, restored.sources, cancellation)?;
            self.generations
                .publish(restored.verified, restored.search, false)
                .map_err(|_| FirstSliceError::Retention)?;
            self.source_snapshots.stage(source_admission)?;
            self.source_snapshots.commit_staged(receipt.generation)?;
            self.repositories
                .insert(restored.root_identity, receipt.repository);
            self.repository_display_names
                .insert(receipt.repository, restored.display_name);
            if let Some(root_path) = restored.root_path {
                self.repository_root_paths
                    .insert(receipt.repository, root_path);
            }
            if let Some(alias) = restored.alias {
                self.repository_aliases.insert(receipt.repository, alias);
            }
            if restored.metadata_sequence > 0 {
                self.repository_metadata_sequences
                    .insert(receipt.repository, restored.metadata_sequence);
            }
            if self
                .pending_repository_registrations
                .get_mut()
                .map_err(|_| FirstSliceError::Retention)?
                .remove(&restored.root_identity)
                .is_some_and(|(repository, _, _)| repository != receipt.repository)
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            self.receipts.insert(receipt.generation, receipt.clone());
            let previous_memory = self
                .generation_memory_bytes
                .insert(receipt.generation, memory_bytes);
            debug_assert!(previous_memory.is_none());
            if self
                .language_coverage_by_generation
                .insert(receipt.generation, language_coverage)
                .is_some()
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            if let Some(incremental) = restored.incremental
                && (self
                    .incremental_baselines
                    .insert(receipt.generation, incremental.baseline)
                    .is_some()
                    || self
                        .incremental_inputs
                        .insert(receipt.generation, incremental.inputs)
                        .is_some()
                    || self
                        .incremental_evidence
                        .insert(receipt.generation, incremental.evidence)
                        .is_some())
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            if let Some(generation_count) = restored.published_generation_count {
                self.published_generation_counts
                    .entry(receipt.repository)
                    .and_modify(|current| *current = (*current).max(generation_count))
                    .or_insert(generation_count);
            } else {
                let legacy_count = legacy_generation_counts
                    .entry(receipt.repository)
                    .or_insert(0);
                *legacy_count = legacy_count
                    .checked_add(1)
                    .ok_or(FirstSliceError::Retention)?;
            }
            self.activation_sequences
                .entry(receipt.repository)
                .and_modify(|sequence| {
                    *sequence = (*sequence).max(restored.activation_sequence);
                })
                .or_insert(restored.activation_sequence);
            let activation_order = match restored.global_activation_sequence {
                Some(sequence) => {
                    if sequence == 0
                        || global_activation_order
                            .insert(sequence, receipt.generation)
                            .is_some()
                    {
                        return Err(FirstSliceError::CatalogCorrupt);
                    }
                    self.global_activation_sequence = self.global_activation_sequence.max(sequence);
                    if self
                        .most_recent_activation
                        .is_none_or(|current| sequence > current.0)
                    {
                        self.most_recent_activation = Some((sequence, receipt.generation));
                    }
                    sequence
                }
                None => {
                    let candidate = (
                        restored.activation_sequence,
                        receipt.repository,
                        receipt.generation,
                    );
                    if legacy_most_recent.is_none_or(|current| candidate > current) {
                        legacy_most_recent = Some(candidate);
                    }
                    restored.activation_sequence
                }
            };
            self.activation_order_by_generation
                .insert(receipt.generation, activation_order);
            for operation in restored.operations {
                let publication = FirstSliceDurableOperation {
                    operation: operation.operation,
                    started_unix_ms: operation.started_unix_ms,
                    provider: operation.provider,
                    receipt: receipt.clone(),
                };
                if self
                    .durable_operations
                    .insert(operation.operation, publication)
                    .is_some()
                {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
            }
            active
                .entry(receipt.repository)
                .and_modify(|current| {
                    if restored.activation_sequence > current.0 {
                        *current = (restored.activation_sequence, receipt.generation);
                    }
                })
                .or_insert((restored.activation_sequence, receipt.generation));
        }
        for (repository, legacy_count) in legacy_generation_counts {
            self.published_generation_counts
                .entry(repository)
                .and_modify(|current| *current = (*current).max(legacy_count))
                .or_insert(legacy_count);
        }
        if self.most_recent_activation.is_none() {
            self.most_recent_activation =
                legacy_most_recent.map(|(_, _, generation)| (0, generation));
        }
        if activate_latest {
            for (repository, (_, generation)) in active {
                self.generations
                    .activate(generation)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                self.active_by_repository.insert(repository, generation);
            }
            if let Some((_, generation)) = self.most_recent_activation {
                self.generations
                    .activate(generation)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            }
        }
        Ok(())
    }

    /// Discovers, parses, validates, round-trips, indexes, and publishes one
    /// repository.
    ///
    /// Every source admitted by the audited grammar registry is lowered through
    /// the common normalized IR. Rust uses the reviewed Tier B structural
    /// profile; other registered grammars use the bounded Tier D fallback.
    /// Repeating an unchanged active repository is idempotent. The caller must
    /// supply a monotonic deadline so every synchronous stage stays bounded.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an invalid fixture shape, missing
    /// deadline, cancellation, resource limit, identity drift, persistence,
    /// search, or retention failure.
    pub fn index_repository(
        &mut self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        self.index_repository_with_mode(path, FirstSliceIndexMode::Structural, cancellation)
    }

    /// Indexes and publishes one repository using the requested analysis strength.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::index_repository`].
    pub fn index_repository_with_mode(
        &mut self,
        path: &Path,
        mode: FirstSliceIndexMode,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        let prepared = self.prepare_repository_with_options(
            path,
            FirstSliceIndexOptions::incremental(mode),
            cancellation,
        )?;
        self.publish_prepared(prepared, cancellation)
    }

    /// Indexes and publishes one repository using explicit analysis and reuse policy.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::index_repository_with_mode`].
    pub fn index_repository_with_options(
        &mut self,
        path: &Path,
        options: FirstSliceIndexOptions,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        let prepared = self.prepare_repository_with_options(path, options, cancellation)?;
        self.publish_prepared(prepared, cancellation)
    }

    /// Publishes a durable structural generation followed by an atomic deep
    /// semantic refinement.
    ///
    /// The structural receipt becomes active before deep analysis starts. The
    /// refinement is published only when every language accepted isolated
    /// project output without a structural-fallback diagnostic. Cancellation,
    /// adapter failure, or incomplete deep coverage leaves the structural
    /// generation active and queryable.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Catalog`] when durable publication is
    /// unavailable, [`FirstSliceError::Adapter`] when no deep analyzer is
    /// configured or the adapter fails,
    /// [`FirstSliceError::ProjectCoverageIncomplete`] when bounded project
    /// output cannot safely replace structural facts, and the normal bounded
    /// indexing failures from
    /// [`Self::index_repository_with_mode`]. At least two retained generations
    /// are required so the structural parent remains queryable after
    /// refinement.
    pub fn index_repository_two_stage(
        &mut self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceTwoStageIndexReceipt, FirstSliceError> {
        if self.durable.is_none() {
            return Err(FirstSliceError::Catalog);
        }
        if self.project_analyzer.is_none() {
            return Err(FirstSliceError::Adapter);
        }
        if self.maximum_generations_per_repository < 2 {
            return Err(FirstSliceError::Retention);
        }

        let structural =
            self.index_repository_with_mode(path, FirstSliceIndexMode::Structural, cancellation)?;
        let semantic_preparation =
            self.prepare_semantic_refinement(path, structural.generation, cancellation)?;
        let semantic = self.publish_prepared(semantic_preparation, cancellation)?;
        Ok(FirstSliceTwoStageIndexReceipt {
            structural,
            semantic,
        })
    }

    /// Compatibility wrapper for callers using the original Rust-first API name.
    ///
    /// The production implementation now indexes every audited grammar, so this
    /// method has the same behavior as [`Self::index_repository`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::index_repository`].
    pub fn index_rust_fixture(
        &mut self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        self.index_repository(path, cancellation)
    }

    /// Builds and verifies one repository generation without making it queryable.
    ///
    /// This phase may perform all bounded discovery, parsing, normalization,
    /// oracle, and lexical work. Publication remains an explicit second step so
    /// the daemon can durably linearize lifecycle completion before activation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] under the same bounded validation,
    /// cancellation, identity, storage, and retention conditions as
    /// [`Self::index_repository`].
    pub fn prepare_repository(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository_with_mode(path, FirstSliceIndexMode::Structural, cancellation)
    }

    /// Builds one hidden generation using the requested analysis strength.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_repository`].
    pub fn prepare_repository_with_mode(
        &self,
        path: &Path,
        mode: FirstSliceIndexMode,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository_with_options(
            path,
            FirstSliceIndexOptions::incremental(mode),
            cancellation,
        )
    }

    /// Builds one hidden generation using explicit analysis and reuse policy.
    ///
    /// Clean rebuild requires an active generation for the repository. It
    /// retains that lineage parent while excluding all parent reuse inputs.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::RepositoryNotFound`] when clean rebuild has no
    /// active repository, or the same failures as [`Self::prepare_repository_with_mode`].
    pub fn prepare_repository_with_options(
        &self,
        path: &Path,
        options: FirstSliceIndexOptions,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository_with_options_and_progress(path, options, cancellation, |_| {})
    }

    /// Builds one hidden generation and reports monotonic coarse progress.
    ///
    /// Progress callbacks contain only a closed stage and bounded counters.
    /// They must remain lightweight because they run synchronously between
    /// preparation stages.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_repository_with_mode`].
    pub fn prepare_repository_with_mode_and_progress(
        &self,
        path: &Path,
        mode: FirstSliceIndexMode,
        cancellation: &Cancellation,
        observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository_with_options_and_progress(
            path,
            FirstSliceIndexOptions::incremental(mode),
            cancellation,
            observe_progress,
        )
    }

    /// Builds one hidden generation with explicit policy and reports progress.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_repository_with_options`].
    pub fn prepare_repository_with_options_and_progress(
        &self,
        path: &Path,
        options: FirstSliceIndexOptions,
        cancellation: &Cancellation,
        observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        let mut admission = self.admit_repository(path, cancellation)?;
        let result = self.prepare_repository_after_admission_with_options_and_progress(
            path,
            options,
            &mut admission,
            cancellation,
            observe_progress,
        );
        if result.as_ref().is_err()
            || result.as_ref().is_ok_and(|prepared| {
                matches!(prepared, FirstSliceIndexPreparation::Retained { .. })
            })
        {
            self.release_index_admission(admission);
        }
        result
    }

    /// Builds one hidden generation using an existing atomic admission.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_repository_with_mode_and_progress`].
    pub fn prepare_repository_after_admission_with_progress(
        &self,
        path: &Path,
        mode: FirstSliceIndexMode,
        admission: &mut FirstSliceIndexAdmission,
        cancellation: &Cancellation,
        observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository_after_admission_with_options_and_progress(
            path,
            FirstSliceIndexOptions::incremental(mode),
            admission,
            cancellation,
            observe_progress,
        )
    }

    /// Builds one hidden generation with explicit policy under an admission.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_repository_with_options_and_progress`].
    pub fn prepare_repository_after_admission_with_options_and_progress(
        &self,
        path: &Path,
        options: FirstSliceIndexOptions,
        admission: &mut FirstSliceIndexAdmission,
        cancellation: &Cancellation,
        observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository_with_representation_and_progress(
            path,
            options,
            DurableGenerationRepresentation::Oracle,
            admission,
            cancellation,
            observe_progress,
        )
    }

    fn prepare_repository_with_representation_and_progress(
        &self,
        path: &Path,
        options: FirstSliceIndexOptions,
        representation: DurableGenerationRepresentation,
        admission: &mut FirstSliceIndexAdmission,
        cancellation: &Cancellation,
        mut observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        let FirstSliceIndexOptions { mode, reuse_policy } = options;
        let started = Instant::now();
        require_deadline(cancellation)?;
        cancellation
            .check()
            .map_err(|cancelled| FirstSliceError::Cancelled(cancelled.reason()))?;
        self.complete_durable_storage_admission(admission)?;
        let canonical = canonical_repository_root(path, cancellation)?;
        let root_identity = repository_path_hash(&canonical)?;
        if root_identity != admission.root_identity {
            return Err(FirstSliceError::Identity);
        }
        let storage_reservation = StorageReservationGuard {
            reservation: match &mut admission.storage_admission {
                DurableStorageAdmissionState::Reserved(reservation) => reservation.take(),
                DurableStorageAdmissionState::Deferred => return Err(FirstSliceError::Retention),
            },
        };
        let root_path = sanitized_repository_root_path(&canonical)?;
        let existing_repository = self.repositories.get(&root_identity).copied();
        let pending = self
            .pending_repository_registrations
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let reserved_repository = pending.get(&root_identity);
        if existing_repository.is_none()
            && reserved_repository.is_none()
            && self
                .repository_display_names
                .len()
                .checked_add(pending.len())
                .ok_or(FirstSliceError::Retention)?
                >= self.maximum_repositories
        {
            return Err(self.repository_capacity_limit(
                self.repository_display_names.len() + pending.len(),
                pending.len(),
            ));
        }
        let repository_result = match existing_repository {
            Some(repository) => repository,
            None => reserved_repository.map_or_else(
                || random_repository_id_with_pending(&self.repositories, &pending),
                |(repository, _, _)| Ok(*repository),
            )?,
        };
        check_cancellation(cancellation)?;
        let repository = repository_result;
        if repository != admission.repository {
            return Err(FirstSliceError::Identity);
        }
        let display_name = match reserved_repository {
            Some((_, display_name, _)) => fallible_copy_string(display_name)?,
            None => sanitized_repository_display_name(&canonical, repository)?,
        };
        drop(pending);
        let root_result = RepositoryRoot::open(repository, path);
        check_cancellation(cancellation)?;
        let root = root_result.map_err(|_| FirstSliceError::Repository)?;
        let policy =
            DiscoveryPolicy::build(Vec::new(), false).map_err(|_| FirstSliceError::Discovery)?;
        let discovery_limits = DiscoveryLimits::from_config(&self.config);
        let parser_provider_hash = first_slice_parser_provider_hash()?;
        let provider_set_hash = self.provider_set_hash(mode)?;
        let active = self.active_by_repository.get(&repository).copied();
        if reuse_policy == FirstSliceReusePolicy::CleanRebuild && active.is_none() {
            return Err(FirstSliceError::RepositoryNotFound);
        }
        let parent_baseline = if reuse_policy == FirstSliceReusePolicy::Incremental {
            active.and_then(|generation| self.incremental_baselines.get(&generation))
        } else {
            None
        };
        let incremental_context = IncrementalDiscoveryContext::new(
            self.config.hash(),
            derive_fact("incremental-provider", INCREMENTAL_PROVIDER_SEED).id(),
            parser_provider_hash,
        );
        let mut discovery_files_examined = 0_u64;
        let mut discovery_bytes_examined = 0_u64;
        let mut last_reported_files = 0_u64;
        let mut incremental = discover_incremental_with_progress(
            &root,
            parent_baseline,
            incremental_context,
            &policy,
            IncrementalDiscoveryOptions::new(ReconcileMode::Normal, discovery_limits),
            cancellation,
            |progress| {
                discovery_files_examined = progress.files_examined;
                discovery_bytes_examined = progress.bytes_examined;
                if progress.files_examined == 1
                    || progress.files_examined.saturating_sub(last_reported_files)
                        >= DISCOVERY_PROGRESS_INTERVAL_FILES
                {
                    last_reported_files = progress.files_examined;
                    observe_progress(FirstSliceIndexProgress::observed(
                        FirstSliceIndexStage::Discovery,
                        0,
                        progress.files_examined,
                        progress.bytes_examined,
                        0,
                    ));
                }
            },
        )
        .map_err(|error| map_discovery_error(error, cancellation))?;
        // A no-op authoritative reconcile already proves that every tracked
        // path and content fingerprint still matches the active baseline.
        // Reuse is valid only while the complete product configuration and
        // provider-set identities also match the published generation.
        if reuse_policy == FirstSliceReusePolicy::Incremental
            && incremental.changes().is_empty()
            && let Some(active) = active
            && let Ok(snapshot) = self.generations.generation(active)
        {
            let metadata = snapshot.metadata();
            if metadata.repository() == repository
                && metadata.configuration_hash() == self.config.hash()
                && metadata.provider_set_hash() == provider_set_hash
                && let Some(receipt) = self.receipts.get(&active).cloned()
            {
                check_cancellation(cancellation)?;
                return Ok(FirstSliceIndexPreparation::Retained { receipt, root_path });
            }
        }
        let cached_snapshots = if reuse_policy == FirstSliceReusePolicy::Incremental {
            incremental.take_hashed_snapshots()
        } else {
            Default::default()
        };
        let (manifest, mut discovered_snapshots) = discover_with_snapshots(
            &root,
            &self.config,
            &policy,
            discovery_limits,
            cached_snapshots,
            cancellation,
        )
        .map_err(|error| map_discovery_error(error, cancellation))?
        .into_parts();
        let incremental = correlate_incremental_manifest(
            &incremental,
            parent_baseline,
            incremental_context,
            &manifest,
            discovery_limits,
            cancellation,
        )
        .map_err(|error| map_discovery_error(error, cancellation))?;
        let manifest_bytes_examined = manifest.inputs.iter().try_fold(0_u64, |total, input| {
            total
                .checked_add(input.bytes)
                .ok_or(FirstSliceError::Limits)
        })?;
        let files_examined = discovery_files_examined.max(manifest.coverage.included);
        let bytes_examined = discovery_bytes_examined.max(manifest_bytes_examined);
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Discovery,
            1,
            files_examined,
            bytes_examined,
            0,
        ));
        let source_preflight = preflight_source_inputs(
            &manifest.inputs,
            &self.analyzers,
            self.analysis_limits.ir().max_files,
            self.source_snapshots.maximum_bytes,
            cancellation,
        )?;
        let mut estimated_disk_bytes =
            self.durable_staging_reservation(source_preflight.source_bytes)?;
        self.resize_durable_storage_reservation(
            storage_reservation.reservation.as_ref(),
            estimated_disk_bytes,
            0,
        )?;
        let source_count = source_preflight.supported_file_count;
        let mut file_claims = Vec::new();
        file_claims
            .try_reserve_exact(manifest.inputs.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(source_count)
            .map_err(|_| FirstSliceError::Limits)?;
        let mut retained_sources = Vec::new();
        retained_sources
            .try_reserve_exact(manifest.inputs.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let mut unsupported_sources = Vec::new();
        unsupported_sources
            .try_reserve_exact(manifest.inputs.len().saturating_sub(source_count))
            .map_err(|_| FirstSliceError::Limits)?;
        let mut source_languages = BTreeMap::new();
        let mut source_analysis_limits = BTreeMap::new();
        for input in &manifest.inputs {
            check_cancellation(cancellation)?;
            let relative = RelativePath::parse(Path::new(&input.path))
                .map_err(|_| FirstSliceError::Repository)?;
            let claim = FileIdentityClaim {
                file: input.file,
                repository,
                path: fallible_copy_string(&input.path)?,
                path_identity: fallible_copy_bytes(relative.identity_bytes())?,
                content_hash: input.content_hash,
                byte_length: input.bytes,
            };
            file_claims.push(claim.clone());
            let snapshot = discovered_snapshots
                .remove(&input.file)
                .ok_or(FirstSliceError::DiscoveryDrift)?;
            if snapshot.file() != input.file
                || snapshot.content_hash() != input.content_hash
                || u64::try_from(snapshot.content().len()).ok() != Some(input.bytes)
            {
                return Err(FirstSliceError::DiscoveryDrift);
            }
            let Some(language) = supported_source_language(input, &self.analyzers) else {
                retained_sources.push(RustSourceInput {
                    snapshot,
                    generated: matches!(input.class, InputClass::Generated),
                    origins: Vec::new(),
                });
                unsupported_sources.push(UnsupportedSourceInput {
                    claim,
                    language: detected_source_language(input)
                        .unwrap_or("unknown")
                        .to_owned(),
                    generated: matches!(input.class, InputClass::Generated),
                });
                continue;
            };
            retained_sources.push(RustSourceInput {
                snapshot: snapshot.clone(),
                generated: matches!(input.class, InputClass::Generated),
                origins: Vec::new(),
            });
            if source_languages
                .insert(input.file, language.to_owned())
                .is_some()
            {
                return Err(FirstSliceError::Identity);
            }
            sources.push(RustSourceInput {
                snapshot,
                generated: matches!(input.class, InputClass::Generated),
                origins: Vec::new(),
            });
        }
        let total_analysis_weight = sources.iter().try_fold(0_usize, |total, source| {
            analysis_partition_weight(source.snapshot.content().len())
                .and_then(|weight| total.checked_add(weight).ok_or(FirstSliceError::Limits))
        })?;
        for source in &sources {
            let analysis_limits = partitioned_analysis_limits(
                &self.analysis_limits,
                source_count,
                total_analysis_weight,
                analysis_partition_weight(source.snapshot.content().len())?,
            )?;
            if source_analysis_limits
                .insert(source.snapshot.file(), analysis_limits)
                .is_some()
            {
                return Err(FirstSliceError::Identity);
            }
        }
        attach_generated_origin_mappings(&mut sources, &source_languages, cancellation)?;
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Snapshot,
            2,
            files_examined,
            bytes_examined,
            0,
        ));
        let source_files = file_claims
            .iter()
            .map(|claim| claim.file)
            .collect::<BTreeSet<_>>();
        let manifest_hash =
            GenerationManifestRecipe::new(repository, self.config.hash(), file_claims)
                .map_err(|_| FirstSliceError::Identity)?
                .canonical_hash()
                .map_err(|_| FirstSliceError::Identity)?;
        if reuse_policy == FirstSliceReusePolicy::Incremental
            && let Some(active) = active
            && let Ok(snapshot) = self.generations.generation(active)
        {
            let metadata = snapshot.metadata();
            if metadata.repository() == repository
                && metadata.manifest_hash() == manifest_hash
                && metadata.configuration_hash() == self.config.hash()
                && metadata.provider_set_hash() == provider_set_hash
                && let Some(receipt) = self.receipts.get(&active).cloned()
            {
                check_cancellation(cancellation)?;
                return Ok(FirstSliceIndexPreparation::Retained { receipt, root_path });
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
        if reuse_policy == FirstSliceReusePolicy::Incremental
            && let Some(receipt) = self.receipts.get(&generation).cloned()
        {
            check_cancellation(cancellation)?;
            return Ok(FirstSliceIndexPreparation::Retained { receipt, root_path });
        }
        let reserved_memory_bytes =
            ensure_generation_memory_preflight(source_preflight.source_bytes)?;
        self.preflight_generation_memory_capacity(
            reserved_memory_bytes,
            PendingGenerationMemory::Reserved,
        )?;
        let reclaimable_generations = self.inactive_generation_ids();
        self.source_snapshots.preflight_admission_after_reclaim(
            generation,
            &retained_sources,
            &reclaimable_generations,
            cancellation,
        )?;
        let parent_structural_artifacts = if reuse_policy == FirstSliceReusePolicy::Incremental {
            active
                .and_then(|generation| self.structural_artifacts.generation(generation))
                .filter(|artifacts| {
                    artifacts.iter().all(|(_, entry)| {
                        source_analysis_limits
                            .get(&entry.artifact.file())
                            .is_some_and(|limits| entry.artifact.is_compatible_with_limits(limits))
                    })
                })
        } else {
            None
        };
        let parent_incremental_inputs = if reuse_policy == FirstSliceReusePolicy::Incremental {
            active.and_then(|generation| self.incremental_inputs.get(&generation))
        } else {
            None
        };
        let mut incremental_plan = prepare_incremental_state(
            FirstSliceIncrementalPlanningContext {
                repository,
                has_parent: active.is_some(),
                reuse_policy,
                parent: parent_incremental_inputs,
                parent_artifacts: parent_structural_artifacts,
                discovery: &incremental,
                source_files: &source_files,
                semantic_inputs: &[],
            },
            cancellation,
        )?;
        let mut document = NormalizedIrDocument::empty(repository, generation);
        let mut disposition_append_state = DocumentAppendState::from_document(&document)?;
        for input in &unsupported_sources {
            let disposition = unsupported_language_document(repository, generation, input)?;
            append_normalized_document(
                &mut document,
                disposition,
                self.analysis_limits.ir(),
                &mut disposition_append_state,
            )?;
        }
        let mut structural_documents = BTreeMap::<String, Vec<NormalizedIrDocument>>::new();
        let mut structural_entries = Vec::new();
        let structural_artifact_budget = self.structural_artifacts.available_bytes();
        let mut structural_artifact_bytes = 0usize;
        let mut structural_retention_complete = true;
        let mut parsed_files = 0usize;
        let mut reused_parser_artifacts = 0usize;
        let mut reused_parser_artifact_bytes = 0usize;
        let mut reused_normalized_records = ReusedNormalizedRecords::default();
        let mut lowered_files = 0usize;
        let mut analyzed_files = 0_u64;
        let mut analyzed_bytes = 0_u64;
        let mut last_reported_analyzed_files = 0_u64;
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Analysis,
            2,
            files_examined,
            bytes_examined,
            0,
        ));
        for input in &sources {
            check_cancellation(cancellation)?;
            let snapshot = &input.snapshot;
            let language = source_languages
                .get(&snapshot.file())
                .ok_or(FirstSliceError::Adapter)?;
            let analyzer = self
                .analyzers
                .get(language)
                .ok_or(FirstSliceError::Adapter)?;
            let analysis_limits = source_analysis_limits
                .get(&snapshot.file())
                .ok_or(FirstSliceError::Adapter)?;
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
                LanguageId::new(language).map_err(|_| FirstSliceError::Adapter)?,
                EncodingId::utf8(),
                Vec::new(),
                analysis_tier_for_language(language),
                BuildContextIdentity::new(first_slice_build_context()),
                analysis_limits,
            )
            .map_err(|_| FirstSliceError::Adapter)?
            .with_generated_status(input.generated);
            let artifact_id = parser_artifact_id(snapshot.file());
            let (file_document, artifact, normalized, normalized_reused) = if incremental_plan
                .reusable_parser_artifacts
                .contains(&artifact_id)
            {
                let entry = parent_structural_artifacts
                    .and_then(|artifacts| artifacts.get(snapshot.file()))
                    .filter(|entry| entry.id == artifact_id)
                    .ok_or(FirstSliceError::Incremental)?;
                // Normalized reuse is optional. Any policy, limit, extension,
                // or identity rejection returns to the verified lowering path.
                let rebound = entry.normalized.as_ref().and_then(|chunk| {
                    chunk
                        .rebind(generation, self.analysis_limits.ir(), &self.extensions)
                        .ok()
                        .map(|document| (document, Arc::clone(chunk)))
                });
                let (document, artifact, normalized, normalized_reused, parser_reused) =
                    if let Some((document, normalized)) = rebound {
                        (
                            document,
                            Some(Arc::clone(&entry.artifact)),
                            Some(normalized),
                            true,
                            true,
                        )
                    } else {
                        match analyzer.analyze_from_artifact(
                            &request,
                            &entry.artifact,
                            self.extensions.clone(),
                            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                            cancellation,
                        ) {
                            Ok(output) => (
                                output.document().clone(),
                                Some(Arc::clone(&entry.artifact)),
                                None,
                                false,
                                true,
                            ),
                            Err(error) if is_invalid_utf8_adapter_failure(&error) => (
                                analyzer
                                    .analyze_unsupported_encoding(
                                        &request,
                                        self.extensions.clone(),
                                        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                                        cancellation,
                                    )
                                    .map_err(|error| map_adapter_error(error, cancellation))?
                                    .document()
                                    .clone(),
                                None,
                                None,
                                false,
                                false,
                            ),
                            Err(error) => return Err(map_adapter_error(error, cancellation)),
                        }
                    };
                if parser_reused {
                    reused_parser_artifacts = reused_parser_artifacts
                        .checked_add(1)
                        .ok_or(FirstSliceError::Limits)?;
                    reused_parser_artifact_bytes = reused_parser_artifact_bytes
                        .checked_add(entry.artifact.accounted_bytes())
                        .ok_or(FirstSliceError::Limits)?;
                }
                (document, artifact, normalized, normalized_reused)
            } else {
                match analyzer.analyze_and_capture(
                    &request,
                    self.extensions.clone(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    cancellation,
                ) {
                    Ok((output, artifact)) => {
                        parsed_files =
                            parsed_files.checked_add(1).ok_or(FirstSliceError::Limits)?;
                        (
                            output.document().clone(),
                            Some(Arc::new(artifact)),
                            None,
                            false,
                        )
                    }
                    Err(error) if is_invalid_utf8_adapter_failure(&error) => (
                        analyzer
                            .analyze_unsupported_encoding(
                                &request,
                                self.extensions.clone(),
                                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                                cancellation,
                            )
                            .map_err(|error| map_adapter_error(error, cancellation))?
                            .document()
                            .clone(),
                        None,
                        None,
                        false,
                    ),
                    Err(error) => return Err(map_adapter_error(error, cancellation)),
                }
            };
            if !normalized_reused {
                lowered_files = lowered_files
                    .checked_add(1)
                    .ok_or(FirstSliceError::Limits)?;
            } else {
                reused_normalized_records.insert_document(&file_document)?;
            }
            let normalized = normalized.or_else(|| {
                CanonicalNormalizedFileChunk::new(
                    &file_document,
                    self.analysis_limits.ir(),
                    &self.extensions,
                )
                .ok()
                .map(Arc::new)
            });
            let language_documents = structural_documents.entry(language.clone()).or_default();
            language_documents
                .try_reserve(1)
                .map_err(|_| FirstSliceError::Limits)?;
            language_documents.push(file_document);
            if let Some(artifact) = artifact
                && structural_retention_complete
            {
                let entry = StructuralArtifactEntry {
                    id: artifact_id,
                    artifact,
                    normalized,
                };
                let admitted_bytes = structural_artifact_bytes
                    .checked_add(entry.accounted_bytes()?)
                    .ok_or(FirstSliceError::Retention)?;
                if admitted_bytes <= structural_artifact_budget {
                    structural_entries
                        .try_reserve(1)
                        .map_err(|_| FirstSliceError::Retention)?;
                    structural_entries.push(entry);
                    structural_artifact_bytes = admitted_bytes;
                } else {
                    structural_entries.clear();
                    structural_retention_complete = false;
                }
            }
            analyzed_files = analyzed_files
                .checked_add(1)
                .ok_or(FirstSliceError::Limits)?;
            analyzed_bytes = analyzed_bytes
                .checked_add(
                    u64::try_from(snapshot.content().len()).map_err(|_| FirstSliceError::Limits)?,
                )
                .ok_or(FirstSliceError::Limits)?;
            if analyzed_files == 1
                || analyzed_files.saturating_sub(last_reported_analyzed_files)
                    >= DISCOVERY_PROGRESS_INTERVAL_FILES
            {
                last_reported_analyzed_files = analyzed_files;
                observe_progress(FirstSliceIndexProgress::observed(
                    FirstSliceIndexStage::Analysis,
                    2,
                    files_examined
                        .checked_add(analyzed_files)
                        .ok_or(FirstSliceError::Limits)?,
                    bytes_examined
                        .checked_add(analyzed_bytes)
                        .ok_or(FirstSliceError::Limits)?,
                    0,
                ));
            }
        }
        let structurally_examined_files = files_examined
            .checked_add(analyzed_files)
            .ok_or(FirstSliceError::Limits)?;
        let structurally_examined_bytes = bytes_examined
            .checked_add(analyzed_bytes)
            .ok_or(FirstSliceError::Limits)?;
        let (provider_files, provider_bytes) = self.append_best_available_documents(
            &mut document,
            structural_documents,
            &sources,
            &source_languages,
            mode,
            representation == DurableGenerationRepresentation::RecoverySnapshot,
            cancellation,
            structurally_examined_files,
            structurally_examined_bytes,
            &mut observe_progress,
        )?;
        if representation == DurableGenerationRepresentation::RecoverySnapshot
            && let Some(error) = document
                .diagnostics
                .iter()
                .find_map(|diagnostic| project_fallback_error(&diagnostic.code))
        {
            // A semantic refinement must decide eligibility before opening
            // durable staging; otherwise an honest fallback rewrites a complete
            // structural candidate that can never become active.
            return Err(error);
        }
        let semantic_inputs = first_slice_semantic_inputs(&document, &source_files, cancellation)?;
        let final_incremental_plan = prepare_incremental_state(
            FirstSliceIncrementalPlanningContext {
                repository,
                has_parent: active.is_some(),
                reuse_policy,
                parent: parent_incremental_inputs,
                parent_artifacts: parent_structural_artifacts,
                discovery: &incremental,
                source_files: &source_files,
                semantic_inputs: &semantic_inputs,
            },
            cancellation,
        )?;
        if final_incremental_plan.reusable_parser_artifacts
            != incremental_plan.reusable_parser_artifacts
        {
            return Err(FirstSliceError::Incremental);
        }
        incremental_plan = final_incremental_plan;
        let fully_examined_files = structurally_examined_files
            .checked_add(provider_files)
            .ok_or(FirstSliceError::Limits)?;
        let fully_examined_bytes = structurally_examined_bytes
            .checked_add(provider_bytes)
            .ok_or(FirstSliceError::Limits)?;
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Analysis,
            3,
            fully_examined_files,
            fully_examined_bytes,
            0,
        ));
        let oversized_inputs = manifest
            .coverage
            .excluded
            .get("oversized")
            .copied()
            .unwrap_or(0);
        if oversized_inputs > 0 {
            append_oversized_input_diagnostic(
                &mut document,
                oversized_inputs,
                self.analysis_limits.ir(),
            )?;
        }
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Merge,
            4,
            fully_examined_files,
            fully_examined_bytes,
            0,
        ));
        incremental_plan.state.evidence.parsed_files =
            u64::try_from(parsed_files).map_err(|_| FirstSliceError::Limits)?;
        incremental_plan.state.evidence.reused_parser_artifacts =
            u64::try_from(reused_parser_artifacts).map_err(|_| FirstSliceError::Limits)?;
        incremental_plan.state.evidence.reused_parser_artifact_bytes =
            u64::try_from(reused_parser_artifact_bytes).map_err(|_| FirstSliceError::Limits)?;
        incremental_plan.state.evidence.lowered_files =
            u64::try_from(lowered_files).map_err(|_| FirstSliceError::Limits)?;
        let structural_artifacts = StructuralGenerationArtifacts::new(
            structural_entries,
            structural_artifact_budget,
            structural_retention_complete,
            cancellation,
        )?;
        let resolution_limits = resolution_limits_for_occurrences(document.occurrences.len())?;
        let document = apply_bounded_resolution(
            document,
            resolution_limits,
            ResolverFactContext::new(content_hash(RESOLVER_BINARY_SEED)),
            cancellation,
        )?;
        let normalized_facts = normalized_record_count(&document)?;
        // Count only rebound records that remain exact after project
        // composition and resolution; replaced candidates are rebuilt work.
        let reused_normalized_facts = reused_normalized_records.retained_count(&document)?;
        let rebuilt_normalized_facts = normalized_facts
            .checked_sub(reused_normalized_facts)
            .ok_or(FirstSliceError::Identity)?;
        incremental_plan.state.evidence.reused_normalized_facts =
            u64::try_from(reused_normalized_facts).map_err(|_| FirstSliceError::Limits)?;
        incremental_plan.state.evidence.rebuilt_normalized_facts =
            u64::try_from(rebuilt_normalized_facts).map_err(|_| FirstSliceError::Limits)?;
        incremental_plan.state.evidence.normalized_fact_work =
            normalized_fact_work(&document, &reused_normalized_records, cancellation)?;
        let mut incremental = incremental_plan.state;
        let serialized_document_bytes = normalized_document_serialized_bytes(&document)?;
        let memory_bytes = ensure_generation_memory_admission(serialized_document_bytes)?;
        let logical_workspace_bytes =
            generation_neutral_workspace_bytes(&document, || cancellation.check().is_ok())
                .map_err(|_| {
                    current_cancellation(cancellation).unwrap_or(FirstSliceError::Limits)
                })?;
        let logical_staged_memory_bytes =
            logical_projection_staged_memory_bytes(memory_bytes, logical_workspace_bytes)?;
        // Projection can retain the canonical document while one complete
        // source record and its rebound buffers coexist with compact graph
        // workspace. Admit that conservative peak before either allocation.
        self.preflight_generation_memory_capacity(
            logical_staged_memory_bytes,
            PendingGenerationMemory::Staged,
        )?;
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
        let normalized_ir_logical = verified
            .generation_neutral_digests(&context)
            .map_err(|error| map_generation_neutral_digest_error(error, cancellation))?;
        if self.durable.is_some() {
            estimated_disk_bytes = self
                .durable_output_reservation(
                    source_preflight.source_bytes,
                    serialized_document_bytes,
                    representation,
                )?
                .max(estimated_disk_bytes);
            self.resize_durable_storage_reservation(
                storage_reservation.reservation.as_ref(),
                estimated_disk_bytes,
                0,
            )?;
        }
        let (oracle_allocated_bytes, verified, durable, mut written_bytes) = if let Some(durable) =
            &self.durable
        {
            let prepared = durable.begin_generation(repository, generation)?;
            let source_write = prepared.write_sources(&retained_sources)?;
            incremental.evidence.reused_durable_artifact_bytes = source_write.referenced_bytes;
            let source_written_bytes = source_write.newly_written_bytes;
            observe_progress(FirstSliceIndexProgress::observed(
                FirstSliceIndexStage::Persistence,
                4,
                fully_examined_files,
                fully_examined_bytes,
                source_written_bytes,
            ));
            let (allocated_bytes, verified, generation_written_bytes) = match representation {
                DurableGenerationRepresentation::Oracle => {
                    let (oracle, verified) = OracleWriter::create_in(prepared.path())
                        .map_err(|error| map_catalog_error(&error, cancellation))?
                        .seal_preserving_verified(verified, &context)
                        .map_err(|error| map_catalog_error(&error, cancellation))?;
                    let allocated_bytes = oracle
                        .allocated_bytes(&context)
                        .map_err(|error| map_catalog_error(&error, cancellation))?;
                    prepared.account_external_staging_bytes(allocated_bytes)?;
                    (allocated_bytes, verified, allocated_bytes)
                }
                DurableGenerationRepresentation::RecoverySnapshot => {
                    let recovery_bytes = prepared
                        .write_recovery_snapshot(verified.snapshot(), serialized_document_bytes)?;
                    (0, verified, recovery_bytes)
                }
            };
            let written_bytes = source_written_bytes
                .checked_add(generation_written_bytes)
                .ok_or(FirstSliceError::Limits)?;
            observe_progress(FirstSliceIndexProgress::observed(
                FirstSliceIndexStage::Persistence,
                4,
                fully_examined_files,
                fully_examined_bytes,
                written_bytes,
            ));
            (allocated_bytes, verified, Some(prepared), written_bytes)
        } else {
            let (oracle, verified) = EphemeralOracleWriter::create()
                .map_err(|error| map_catalog_error(&error, cancellation))?
                .seal_and_retain(verified, &context)
                .map_err(|error| map_catalog_error(&error, cancellation))?;
            let allocated_bytes = oracle
                .allocated_bytes()
                .map_err(|error| map_catalog_error(&error, cancellation))?;
            (allocated_bytes, verified, None, 0)
        };
        if let Some(durable) = durable.as_ref() {
            let incremental_bytes = durable.write_incremental_state(&incremental)?;
            written_bytes = written_bytes
                .checked_add(incremental_bytes)
                .ok_or(FirstSliceError::Limits)?;
            observe_progress(FirstSliceIndexProgress::observed(
                FirstSliceIndexStage::Persistence,
                4,
                fully_examined_files,
                fully_examined_bytes,
                written_bytes,
            ));
        }
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Persistence,
            5,
            fully_examined_files,
            fully_examined_bytes,
            written_bytes,
        ));
        let source_snapshots = retained_sources
            .iter()
            .map(|source| &source.snapshot)
            .collect::<Vec<_>>();
        let documents = project_lexical_documents_with_sources(
            verified.snapshot(),
            &source_snapshots,
            BuildBudget::default(),
            cancellation,
        )
        .map_err(|error| map_query_error(error, cancellation))?;
        let lexical_documents =
            u64::try_from(documents.len()).map_err(|_| FirstSliceError::Limits)?;
        let logical_snapshot = logical_snapshot_identity(
            normalized_ir_logical,
            &documents,
            &incremental.inputs,
            cancellation,
        )?;
        if let Some(durable) = durable.as_ref() {
            let logical_snapshot_bytes =
                durable.write_logical_snapshot_identity(verified.snapshot(), &logical_snapshot)?;
            written_bytes = written_bytes
                .checked_add(logical_snapshot_bytes)
                .ok_or(FirstSliceError::Limits)?;
        }
        let search = LexicalIndex::build_ephemeral(
            generation,
            documents,
            BuildBudget::default(),
            cancellation,
        )
        .map_err(|error| map_search_error(error, cancellation))?;
        observe_progress(FirstSliceIndexProgress::observed(
            FirstSliceIndexStage::Search,
            6,
            fully_examined_files,
            fully_examined_bytes,
            written_bytes,
        ));
        let indexed_files =
            u64::try_from(verified.document().files.len()).map_err(|_| FirstSliceError::Limits)?;
        let entities = u64::try_from(verified.document().entities.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let mut receipt = FirstSliceIndexReceipt {
            repository,
            generation,
            parent,
            discovered_inputs: manifest.coverage.included,
            visited_entries: manifest.coverage.visited,
            excluded_inputs: manifest
                .coverage
                .excluded
                .values()
                .try_fold(0_u64, |total, count| {
                    total.checked_add(*count).ok_or(FirstSliceError::Limits)
                })?,
            oversized_inputs,
            binary_inputs: manifest
                .coverage
                .excluded
                .get("binary")
                .copied()
                .unwrap_or(0),
            policy_excluded_inputs: manifest
                .coverage
                .excluded
                .iter()
                .filter(|(reason, _)| !matches!(reason.as_str(), "binary" | "oversized"))
                .try_fold(0_u64, |total, (_, count)| {
                    total.checked_add(*count).ok_or(FirstSliceError::Limits)
                })?,
            indexed_files,
            entities,
            lexical_documents,
            oracle_allocated_bytes,
            estimated_disk_bytes,
            retained_durable_bytes: 0,
            diagnostics: index_diagnostic_summaries(verified.document())?,
            logical_snapshot: Some(logical_snapshot),
            elapsed_micros: elapsed_micros(started),
        };
        let durable = if let Some(durable) = durable {
            let sealed = durable.finish(
                repository,
                root_identity,
                &display_name,
                &root_path,
                &mut receipt,
            )?;
            let manifest_written_bytes = sealed.manifest_written_bytes();
            written_bytes = written_bytes
                .checked_add(manifest_written_bytes)
                .ok_or(FirstSliceError::Limits)?;
            if let (Some(catalog), Some(reservation)) = (
                self.durable.as_ref(),
                storage_reservation.reservation.as_ref(),
            ) {
                let (_, admitted) = catalog
                    .finalize_repository_capacity(
                        reservation,
                        sealed,
                        self.durable_storage_admission_policy(
                            estimated_disk_bytes,
                            DURABLE_PUBLICATION_RESIDUAL_BYTES,
                        ),
                    )?
                    .map_err(storage_admission_error)?;
                Some(admitted)
            } else {
                return Err(FirstSliceError::Retention);
            }
        } else {
            None
        };
        check_cancellation(cancellation)?;
        Ok(FirstSliceIndexPreparation::Pending(
            PreparedFirstSliceIndex {
                verified,
                search,
                sources: retained_sources,
                structural_artifacts,
                incremental,
                receipt,
                root_identity,
                display_name,
                root_path,
                register_repository: existing_repository.is_none(),
                durable,
                written_bytes,
                reserved_memory_bytes,
                memory_bytes,
                storage_reservation: storage_reservation.transfer(),
            },
        ))
    }

    /// Builds a deep generation that is eligible to refine one exact structural
    /// parent.
    ///
    /// Unlike a caller-requested deep index, a semantic refinement must not
    /// publish structural fallback output under a deep provider identity. The
    /// returned preparation is therefore guaranteed to contain accepted
    /// project-adapter output and to name `structural_generation` as its parent.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Catalog`] when durable publication is
    /// unavailable, [`FirstSliceError::Adapter`] when deep analysis is
    /// unavailable or fails, [`FirstSliceError::ProjectCoverageIncomplete`]
    /// when bounded project output cannot safely replace structural facts,
    /// [`FirstSliceError::Identity`] when the active lineage changed during
    /// preparation, and the normal bounded preparation failures from
    /// [`Self::prepare_repository_with_mode`].
    pub fn prepare_semantic_refinement(
        &self,
        path: &Path,
        structural_generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_semantic_refinement_with_progress(
            path,
            structural_generation,
            cancellation,
            |_| {},
        )
    }

    /// Builds an eligible semantic refinement and reports monotonic progress.
    ///
    /// The callback observes the same bounded preparation stages and resource
    /// counters as [`Self::prepare_repository_with_mode_and_progress`].
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_semantic_refinement`].
    pub fn prepare_semantic_refinement_with_progress(
        &self,
        path: &Path,
        structural_generation: GenerationId,
        cancellation: &Cancellation,
        observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        let mut admission = self.admit_repository(path, cancellation)?;
        let result = self.prepare_semantic_refinement_after_admission_with_progress(
            path,
            structural_generation,
            &mut admission,
            cancellation,
            observe_progress,
        );
        if result.as_ref().is_err()
            || result.as_ref().is_ok_and(|prepared| {
                matches!(prepared, FirstSliceIndexPreparation::Retained { .. })
            })
        {
            self.release_index_admission(admission);
        }
        result
    }

    /// Builds a semantic refinement using an existing atomic admission.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::prepare_semantic_refinement_with_progress`].
    pub fn prepare_semantic_refinement_after_admission_with_progress(
        &self,
        path: &Path,
        structural_generation: GenerationId,
        admission: &mut FirstSliceIndexAdmission,
        cancellation: &Cancellation,
        observe_progress: impl FnMut(FirstSliceIndexProgress),
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        if self.durable.is_none() {
            return Err(FirstSliceError::Catalog);
        }
        if self.project_analyzer.is_none() {
            return Err(FirstSliceError::Adapter);
        }
        if self.maximum_generations_per_repository < 2 {
            return Err(FirstSliceError::Retention);
        }

        let preparation = self.prepare_repository_with_representation_and_progress(
            path,
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Deep),
            DurableGenerationRepresentation::RecoverySnapshot,
            admission,
            cancellation,
            observe_progress,
        )?;
        let receipt = preparation.receipt();
        if let Some(error) = receipt
            .diagnostics
            .iter()
            .find_map(|diagnostic| project_fallback_error(&diagnostic.code))
        {
            return Err(error);
        }
        if receipt.parent != Some(structural_generation) {
            return Err(FirstSliceError::Identity);
        }
        Ok(preparation)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "analysis publication keeps independent generation, source-work, and progress invariants explicit"
    )]
    fn append_best_available_documents(
        &self,
        target: &mut NormalizedIrDocument,
        structural_documents: BTreeMap<String, Vec<NormalizedIrDocument>>,
        sources: &[RustSourceInput],
        source_languages: &BTreeMap<FileId, String>,
        mode: FirstSliceIndexMode,
        require_complete_project_analysis: bool,
        cancellation: &Cancellation,
        baseline_files_examined: u64,
        baseline_bytes_examined: u64,
        observe_progress: &mut impl FnMut(FirstSliceIndexProgress),
    ) -> Result<(u64, u64), FirstSliceError> {
        let mut append_state = DocumentAppendState::from_document(target)?;
        let mut completed_provider_files = 0_u64;
        let mut completed_provider_bytes = 0_u64;
        for (language, fallback_documents) in structural_documents {
            check_cancellation(cancellation)?;
            let mut inputs = Vec::new();
            inputs
                .try_reserve_exact(fallback_documents.len())
                .map_err(|_| FirstSliceError::Limits)?;
            for source in sources {
                if source_languages.get(&source.snapshot.file()) != Some(&language) {
                    continue;
                }
                inputs.push(FirstSliceProjectInput {
                    file: source.snapshot.file(),
                    path: source.snapshot.path().as_str(),
                    content_hash: source.snapshot.content_hash(),
                    source: source.snapshot.content(),
                    generated: source.generated,
                    origins: &source.origins,
                });
            }
            if inputs.len() != fallback_documents.len() {
                return Err(FirstSliceError::Identity);
            }

            let mut fallback_error = (mode == FirstSliceIndexMode::Deep
                && self.project_analyzer.is_none())
            .then_some(FirstSliceProjectAnalysisError::Identity);
            if mode == FirstSliceIndexMode::Deep
                && project_adapter_supports_language(&language)
                && let Some(project_analyzer) = &self.project_analyzer
            {
                let context_manifest = project_context_manifest(&language, self.config.hash())?;
                // Symbol identity includes the build-context digest. Structural
                // and project providers therefore share this exact value; the
                // language remains independently encoded in both the symbol
                // recipe and the project context manifest.
                let build_context = first_slice_build_context();
                let request = FirstSliceProjectAnalysisRequest {
                    repository: target.repository,
                    generation: target.generation,
                    language: &language,
                    build_context,
                    context_manifest: &context_manifest,
                    inputs: &inputs,
                };
                let language_base_files = completed_provider_files;
                let language_base_bytes = completed_provider_bytes;
                let expected_language_files =
                    u64::try_from(inputs.len()).map_err(|_| FirstSliceError::Limits)?;
                let expected_language_bytes = inputs.iter().try_fold(0_u64, |total, input| {
                    total
                        .checked_add(
                            u64::try_from(input.source().len())
                                .map_err(|_| FirstSliceError::Limits)?,
                        )
                        .ok_or(FirstSliceError::Limits)
                })?;
                let mut observed_language_files = 0_u64;
                let mut observed_language_bytes = 0_u64;
                let mut invalid_progress = false;
                let analysis = project_analyzer.analyze_with_progress(
                    request,
                    cancellation,
                    &mut |progress| {
                        if progress.total_files != expected_language_files
                            || progress.total_bytes != expected_language_bytes
                            || progress.completed_files < observed_language_files
                            || progress.completed_bytes < observed_language_bytes
                            || progress.completed_files > progress.total_files
                            || progress.completed_bytes > progress.total_bytes
                        {
                            invalid_progress = true;
                            return;
                        }
                        observed_language_files =
                            observed_language_files.max(progress.completed_files);
                        observed_language_bytes =
                            observed_language_bytes.max(progress.completed_bytes);
                        let files = language_base_files.saturating_add(progress.completed_files);
                        let bytes = language_base_bytes.saturating_add(progress.completed_bytes);
                        observe_progress(FirstSliceIndexProgress::observed(
                            FirstSliceIndexStage::Analysis,
                            2,
                            baseline_files_examined.saturating_add(files),
                            baseline_bytes_examined.saturating_add(bytes),
                            0,
                        ));
                    },
                );
                if invalid_progress {
                    return Err(FirstSliceError::Adapter);
                }
                completed_provider_files = language_base_files
                    .checked_add(observed_language_files)
                    .ok_or(FirstSliceError::Limits)?;
                completed_provider_bytes = language_base_bytes
                    .checked_add(observed_language_bytes)
                    .ok_or(FirstSliceError::Limits)?;
                match analysis {
                    Ok(output) => {
                        let (
                            documents,
                            isolation_permits_deep_adapter,
                            partitioned,
                            diagnostics_truncated,
                            facts_truncated,
                        ) = output.into_parts();
                        if !isolation_permits_deep_adapter {
                            fallback_error = Some(FirstSliceProjectAnalysisError::Isolation);
                        } else if !project_documents_match_inputs(
                            &documents,
                            target.repository,
                            target.generation,
                            project_analyzer.provider_identity(),
                            &inputs,
                        ) {
                            fallback_error = Some(FirstSliceProjectAnalysisError::Protocol);
                        } else {
                            match prepare_project_analysis_document(
                                documents,
                                &language,
                                partitioned,
                                diagnostics_truncated,
                                facts_truncated,
                                target.diagnostics.len(),
                                self.analysis_limits.ir(),
                            ) {
                                Ok(document) => {
                                    let syntax_facts_bounded =
                                        document.diagnostics.iter().any(|diagnostic| {
                                            diagnostic.code == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC
                                        });
                                    let aggregate_facts_truncated =
                                        document.diagnostics.iter().any(|diagnostic| {
                                            diagnostic.code == PROJECT_FACTS_TRUNCATED_CODE
                                        });
                                    let missing_declarations =
                                        missing_structural_declaration_count(
                                            &fallback_documents,
                                            &document,
                                            syntax_facts_bounded,
                                        )?;
                                    if aggregate_facts_truncated || missing_declarations > 0 {
                                        if require_complete_project_analysis {
                                            let language =
                                                FirstSliceProjectCoverageLanguage::from_label(
                                                    &language,
                                                )
                                                .ok_or(FirstSliceError::Adapter)?;
                                            let reason = if aggregate_facts_truncated {
                                                FirstSliceProjectCoverageReason::FactsTruncated
                                            } else {
                                                FirstSliceProjectCoverageReason::StructuralDeclarationLoss
                                            };
                                            return Err(
                                                FirstSliceError::ProjectCoverageIncomplete {
                                                    language,
                                                    reason,
                                                    missing_declarations,
                                                },
                                            );
                                        }
                                        fallback_error = Some(if aggregate_facts_truncated {
                                            FirstSliceProjectAnalysisError::FactsTruncated
                                        } else {
                                            FirstSliceProjectAnalysisError::StructuralDeclarationLoss {
                                                missing_declarations,
                                            }
                                        });
                                    } else {
                                        match append_project_document_with_capacity(
                                            target,
                                            document,
                                            self.analysis_limits.ir(),
                                            &mut append_state,
                                        ) {
                                            Ok(()) => continue,
                                            Err(error @ FirstSliceError::ResourceLimit { .. })
                                                if require_complete_project_analysis =>
                                            {
                                                return Err(error);
                                            }
                                            Err(FirstSliceError::ResourceLimit { .. }) => {
                                                fallback_error =
                                                    Some(FirstSliceProjectAnalysisError::Capacity);
                                            }
                                            Err(error @ FirstSliceError::Limits) => {
                                                return Err(error);
                                            }
                                            Err(_) => {
                                                fallback_error =
                                                    Some(FirstSliceProjectAnalysisError::Analysis);
                                            }
                                        }
                                    }
                                }
                                Err(
                                    error @ (FirstSliceError::Limits
                                    | FirstSliceError::ResourceLimit { .. }),
                                ) => return Err(error),
                                Err(_) => {
                                    fallback_error = Some(FirstSliceProjectAnalysisError::Analysis);
                                }
                            }
                        }
                    }
                    Err(FirstSliceProjectAnalysisError::Cancelled(reason)) => {
                        return Err(FirstSliceError::Cancelled(reason));
                    }
                    Err(FirstSliceProjectAnalysisError::ResourceUnavailable) => {
                        return Err(FirstSliceError::ResourceUnavailable {
                            resource: FirstSliceResource::ProcessResources,
                        });
                    }
                    Err(error) => fallback_error = Some(error),
                }
            }

            let fallback_file = fallback_documents
                .first()
                .and_then(|document| document.files.first())
                .map(|file| file.id)
                .ok_or(FirstSliceError::Identity)?;
            let fallback_provenance = fallback_documents
                .first()
                .and_then(|document| document.provenance.first())
                .map(|provenance| provenance.id)
                .ok_or(FirstSliceError::Identity)?;
            for document in fallback_documents {
                append_normalized_document(
                    target,
                    document,
                    self.analysis_limits.ir(),
                    &mut append_state,
                )?;
            }
            if let Some(error) = fallback_error {
                append_project_fallback_diagnostic(
                    target,
                    &language,
                    error,
                    fallback_file,
                    fallback_provenance,
                    self.analysis_limits.ir(),
                )?;
            }
        }
        if append_state.truncated_extensions > 0 {
            append_extension_truncation_diagnostic(
                target,
                append_state.truncated_extensions,
                self.analysis_limits.ir(),
            )?;
        }
        if append_state.truncated_diagnostics > 0 {
            append_diagnostic_truncation_diagnostic(
                target,
                append_state.truncated_diagnostics,
                self.analysis_limits.ir(),
            )?;
        }
        if append_state.truncated_skipped_regions > 0 {
            append_skipped_region_truncation_diagnostic(
                target,
                append_state.truncated_skipped_regions,
                self.analysis_limits.ir(),
            )?;
        }
        Ok((completed_provider_files, completed_provider_bytes))
    }

    /// Compatibility wrapper for callers using the original Rust-first API name.
    ///
    /// The production implementation now indexes every audited grammar, so this
    /// method has the same behavior as [`Self::prepare_repository`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::prepare_repository`].
    pub fn prepare_rust_fixture(
        &self,
        path: &Path,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexPreparation, FirstSliceError> {
        self.prepare_repository(path, cancellation)
    }

    /// Publishes or reactivates one prepared generation for standalone use.
    ///
    /// Its final token check is the standalone cancellation linearization
    /// point. The daemon instead stages first, closes journal cancellation
    /// admission, and then invokes [`Self::commit_staged_for_operation`].
    /// Daemon status remains nonterminal until the generation is queryable.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Cancelled`] when cancellation was already
    /// established, or [`FirstSliceError::Retention`] when bounded generation
    /// or source retention cannot publish the prepared state.
    pub fn publish_prepared(
        &mut self,
        prepared: FirstSliceIndexPreparation,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        self.publish_prepared_with_metrics(prepared, cancellation)
            .map(|committed| committed.receipt)
    }

    /// Publishes standalone prepared work with durable reuse and write evidence.
    ///
    /// This follows the same cancellation linearization point as
    /// [`Self::publish_prepared`] while retaining the final operation-local
    /// metrics needed by controlled resource gates.
    ///
    /// # Errors
    ///
    /// Returns the same typed cancellation, publication, integrity, and
    /// retention failures as [`Self::publish_prepared`].
    pub fn publish_prepared_with_metrics(
        &mut self,
        prepared: FirstSliceIndexPreparation,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceIndexCommit, FirstSliceError> {
        let staged = self.stage_prepared(prepared, cancellation)?;
        if let Err(error) = check_cancellation(cancellation) {
            self.discard_staged(staged)?;
            return Err(error);
        }
        self.commit_staged_with_operation(staged, None)
    }

    fn oldest_inactive_generation(&self, repository: RepositoryId) -> Option<GenerationId> {
        let active = self.active_by_repository.get(&repository).copied();
        self.receipts
            .values()
            .filter(|receipt| {
                receipt.repository == repository && Some(receipt.generation) != active
            })
            .min_by_key(|receipt| {
                (
                    self.activation_order_by_generation
                        .get(&receipt.generation)
                        .copied()
                        .unwrap_or(0),
                    receipt.generation,
                )
            })
            .map(|receipt| receipt.generation)
    }

    fn retained_generation_count(&self, repository: RepositoryId) -> usize {
        self.receipts
            .values()
            .filter(|receipt| receipt.repository == repository)
            .count()
    }

    fn inactive_generation_ids(&self) -> BTreeSet<GenerationId> {
        self.receipts
            .values()
            .filter_map(|receipt| {
                (self.active_by_repository.get(&receipt.repository) != Some(&receipt.generation)
                    && self.generations.active_generation() != Some(receipt.generation))
                .then_some(receipt.generation)
            })
            .collect()
    }

    fn preflight_generation_memory_capacity(
        &self,
        required_memory_bytes: u64,
        pending: PendingGenerationMemory,
    ) -> Result<(), FirstSliceError> {
        let reclaimable = self.inactive_generation_ids();
        let retained_after_reclaim =
            self.generation_memory_bytes
                .iter()
                .try_fold(0_u64, |total, (generation, bytes)| {
                    if reclaimable.contains(generation) {
                        Ok(total)
                    } else {
                        total.checked_add(*bytes).ok_or(FirstSliceError::Limits)
                    }
                })?;
        let observed = retained_after_reclaim
            .checked_add(required_memory_bytes)
            .ok_or(FirstSliceError::Limits)?;
        if observed > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES {
            return Err(generation_memory_limit(
                retained_after_reclaim,
                required_memory_bytes,
                pending,
            ));
        }
        Ok(())
    }

    fn make_room_for_generation(
        &mut self,
        repository: RepositoryId,
        required_memory_bytes: u64,
    ) -> Result<(), FirstSliceError> {
        while self.retained_generation_count(repository) >= self.maximum_generations_per_repository
        {
            let Some(generation) = self.oldest_inactive_generation(repository) else {
                // A retention of one temporarily needs the dedicated staging
                // slot. The prior active generation becomes evictable only
                // after the new generation is atomically selected.
                break;
            };
            self.evict_generation(generation)?;
        }
        while self
            .retained_generation_memory_bytes()?
            .checked_add(required_memory_bytes)
            .is_none_or(|total| total > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES)
        {
            let Some(generation) = self.oldest_inactive_generation_global() else {
                let retained_memory_bytes = self.retained_generation_memory_bytes()?;
                return Err(generation_memory_limit(
                    retained_memory_bytes,
                    required_memory_bytes,
                    PendingGenerationMemory::Staged,
                ));
            };
            self.evict_generation(generation)?;
        }
        Ok(())
    }

    fn make_room_for_source_admission(
        &mut self,
        generation: GenerationId,
        sources: &[RustSourceInput],
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        loop {
            match self
                .source_snapshots
                .preflight_admission(generation, sources, cancellation)
            {
                Ok(_) => return Ok(()),
                Err(FirstSliceError::Retention) => {
                    let Some(reclaimable) = self.oldest_inactive_generation_global() else {
                        return Err(FirstSliceError::Retention);
                    };
                    self.evict_generation(reclaimable)?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn retained_generation_memory_bytes(&self) -> Result<u64, FirstSliceError> {
        self.generation_memory_bytes
            .values()
            .try_fold(0_u64, |total, bytes| {
                total.checked_add(*bytes).ok_or(FirstSliceError::Limits)
            })
    }

    fn oldest_inactive_generation_global(&self) -> Option<GenerationId> {
        self.receipts
            .values()
            .filter(|receipt| {
                self.active_by_repository.get(&receipt.repository) != Some(&receipt.generation)
                    && self.generations.active_generation() != Some(receipt.generation)
            })
            .min_by_key(|receipt| {
                (
                    self.activation_order_by_generation
                        .get(&receipt.generation)
                        .copied()
                        .unwrap_or(0),
                    receipt.generation,
                )
            })
            .map(|receipt| receipt.generation)
    }

    fn trim_repository_retention(
        &mut self,
        repository: RepositoryId,
    ) -> Result<(), FirstSliceError> {
        while self.retained_generation_count(repository) > self.maximum_generations_per_repository {
            let generation = self
                .oldest_inactive_generation(repository)
                .ok_or(FirstSliceError::Retention)?;
            self.evict_generation(generation)?;
        }
        Ok(())
    }

    fn evict_generation(&mut self, generation: GenerationId) -> Result<(), FirstSliceError> {
        let receipt = self
            .receipts
            .get(&generation)
            .cloned()
            .ok_or(FirstSliceError::Retention)?;
        if self.active_by_repository.get(&receipt.repository) == Some(&generation)
            || self.generations.active_generation() == Some(generation)
            || !self.generations.contains(generation)
            || !self.source_snapshots.contains_committed(generation)
            || !self
                .language_coverage_by_generation
                .contains_key(&generation)
        {
            return Err(FirstSliceError::Retention);
        }
        self.source_snapshots.remove_committed(generation)?;
        if self.structural_artifacts.contains_committed(generation) {
            self.structural_artifacts.remove_committed(generation)?;
        }
        self.generations
            .remove(generation)
            .map_err(|_| FirstSliceError::Retention)?;
        self.receipts.remove(&generation);
        self.language_coverage_by_generation.remove(&generation);
        self.incremental_baselines.remove(&generation);
        self.incremental_inputs.remove(&generation);
        self.incremental_evidence.remove(&generation);
        self.generation_memory_bytes.remove(&generation);
        self.activation_order_by_generation.remove(&generation);
        self.durable_operations
            .retain(|_, publication| publication.receipt.generation != generation);
        Ok(())
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
            FirstSliceIndexPreparation::Retained { receipt, root_path } => {
                Ok(FirstSliceStagedIndex {
                    receipt,
                    publication: FirstSlicePublication::Retained { root_path },
                    written_bytes: 0,
                    storage_reservation: None,
                })
            }
            FirstSliceIndexPreparation::Pending(prepared) => {
                let PreparedFirstSliceIndex {
                    verified,
                    search,
                    sources,
                    structural_artifacts,
                    mut incremental,
                    receipt,
                    root_identity,
                    display_name,
                    root_path,
                    register_repository,
                    durable,
                    written_bytes,
                    reserved_memory_bytes,
                    memory_bytes,
                    storage_reservation,
                } = prepared;
                self.make_room_for_generation(receipt.repository, memory_bytes)?;
                self.make_room_for_source_admission(receipt.generation, &sources, cancellation)?;
                let language_coverage = language_coverage(verified.document());
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
                let structural_retained = match self.structural_artifacts.stage(
                    receipt.generation,
                    structural_artifacts,
                    cancellation,
                ) {
                    Ok(retained) => retained,
                    Err(error) => {
                        let source_release =
                            self.source_snapshots.begin_discard(receipt.generation)?;
                        self.generations
                            .discard_staged(receipt.generation)
                            .map_err(|_| FirstSliceError::Retention)?;
                        self.source_snapshots.finish_discard(source_release);
                        return Err(error);
                    }
                };
                incremental.evidence.structural_cache_retained = structural_retained;
                let durable = match durable {
                    Some(prepared) => match prepared.publish() {
                        Ok(published) => Some(published),
                        Err(error) => {
                            let source_release =
                                self.source_snapshots.begin_discard(receipt.generation)?;
                            let structural_release = self
                                .structural_artifacts
                                .begin_discard(receipt.generation)?;
                            self.generations
                                .discard_staged(receipt.generation)
                                .map_err(|_| FirstSliceError::Retention)?;
                            self.source_snapshots.finish_discard(source_release);
                            self.structural_artifacts.finish_discard(structural_release);
                            return Err(error);
                        }
                    },
                    None => None,
                };
                Ok(FirstSliceStagedIndex {
                    receipt,
                    publication: FirstSlicePublication::Pending {
                        root_identity,
                        display_name,
                        root_path,
                        register_repository,
                        language_coverage,
                        incremental,
                        reserved_memory_bytes,
                        memory_bytes,
                        durable,
                    },
                    written_bytes,
                    storage_reservation,
                })
            }
        }
    }

    /// Commits one correctly linearized staged generation.
    ///
    /// Daemon callers first close cancellation admission in the journal and
    /// suppress that internal success from public status until this commit
    /// returns. Standalone [`Self::publish_prepared`] callers instead use its
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
        self.commit_staged_with_operation(staged, None)
            .map(|committed| committed.receipt)
    }

    /// Commits a staged generation and durably binds its journal operation.
    ///
    /// Durable services append the operation identity and source-redacted start
    /// time to the immutable activation marker. Process-local services preserve
    /// the ordinary commit semantics without persisting this projection.
    ///
    /// # Errors
    ///
    /// Returns a typed durable activation, integrity, or retention failure.
    pub fn commit_staged_for_operation(
        &mut self,
        staged: FirstSliceStagedIndex,
        operation: FirstSliceOperationContext,
    ) -> Result<FirstSliceIndexReceipt, FirstSliceError> {
        self.commit_staged_with_operation(staged, Some(operation))
            .map(|committed| committed.receipt)
    }

    /// Commits a staged operation and reports confirmed durable write volume.
    ///
    /// # Errors
    ///
    /// Returns the same typed durable activation, integrity, or retention
    /// failures as [`Self::commit_staged_for_operation`].
    pub fn commit_staged_for_operation_with_metrics(
        &mut self,
        staged: FirstSliceStagedIndex,
        operation: FirstSliceOperationContext,
    ) -> Result<FirstSliceIndexCommit, FirstSliceError> {
        self.commit_staged_with_operation(staged, Some(operation))
    }

    fn commit_staged_with_operation(
        &mut self,
        staged: FirstSliceStagedIndex,
        operation: Option<FirstSliceOperationContext>,
    ) -> Result<FirstSliceIndexCommit, FirstSliceError> {
        self.retry_pending_durable_compactions()?;
        let _storage_reservation = StorageReservationGuard {
            reservation: staged.storage_reservation,
        };
        let receipt = staged.receipt;
        let mut written_bytes = staged.written_bytes;
        let mut operation_evidence = match &staged.publication {
            FirstSlicePublication::Retained { .. } => {
                let document = self
                    .generations
                    .generation(receipt.generation)
                    .map_err(|_| FirstSliceError::Retention)?
                    .document();
                FirstSliceIndexOperationEvidence {
                    strategy: FirstSliceIndexOperationStrategy::RetainedGeneration,
                    fallback_reason: None,
                    invalidated_units: 0,
                    changed_inputs: 0,
                    changed_files: 0,
                    reused_files: receipt.indexed_files,
                    rebuilt_files: 0,
                    reused_facts: u64::try_from(normalized_record_count(document)?)
                        .map_err(|_| FirstSliceError::Limits)?,
                    rebuilt_facts: 0,
                    referenced_bytes: self
                        .generation_memory_bytes
                        .get(&receipt.generation)
                        .copied()
                        .ok_or(FirstSliceError::Retention)?,
                    newly_written_bytes: 0,
                    reserved_memory_bytes: 0,
                    owned_memory_bytes: 0,
                    retained_durable_bytes: receipt.retained_durable_bytes,
                    planned_fact_work: Vec::new(),
                    normalized_fact_work: Vec::new(),
                }
            }
            FirstSlicePublication::Pending {
                incremental,
                reserved_memory_bytes,
                memory_bytes,
                ..
            } => {
                let changed_inputs = incremental
                    .evidence
                    .input_changes
                    .iter()
                    .filter(|change| change.class != ChangeClass::NoChange)
                    .try_fold(0_u64, |total, change| {
                        total
                            .checked_add(change.inputs)
                            .ok_or(FirstSliceError::Limits)
                    })?;
                let changed_files = incremental
                    .evidence
                    .file_changes
                    .iter()
                    .filter(|change| change.kind != FileChangeKind::NoChange)
                    .try_fold(0_u64, |total, change| {
                        total
                            .checked_add(change.files)
                            .ok_or(FirstSliceError::Limits)
                    })?;
                FirstSliceIndexOperationEvidence {
                    strategy: match incremental.evidence.strategy {
                        FirstSliceBuildStrategy::Initial => {
                            FirstSliceIndexOperationStrategy::Initial
                        }
                        FirstSliceBuildStrategy::DependencyDirected => {
                            FirstSliceIndexOperationStrategy::DependencyDirected
                        }
                        FirstSliceBuildStrategy::ConservativeRepositoryRebuild => {
                            FirstSliceIndexOperationStrategy::ConservativeRepositoryRebuild
                        }
                        FirstSliceBuildStrategy::CleanRebuild => {
                            FirstSliceIndexOperationStrategy::CleanRebuild
                        }
                    },
                    fallback_reason: incremental.evidence.fallback_reason,
                    invalidated_units: incremental.evidence.invalidated_units,
                    changed_inputs,
                    changed_files,
                    reused_files: incremental.evidence.reused_parser_artifacts,
                    rebuilt_files: incremental.evidence.parsed_files,
                    reused_facts: incremental.evidence.reused_normalized_facts,
                    rebuilt_facts: incremental.evidence.rebuilt_normalized_facts,
                    referenced_bytes: incremental
                        .evidence
                        .reused_parser_artifact_bytes
                        .checked_add(incremental.evidence.reused_durable_artifact_bytes)
                        .ok_or(FirstSliceError::Limits)?,
                    newly_written_bytes: 0,
                    reserved_memory_bytes: *reserved_memory_bytes,
                    owned_memory_bytes: *memory_bytes,
                    retained_durable_bytes: receipt.retained_durable_bytes,
                    planned_fact_work: incremental.evidence.planned_fact_work.clone(),
                    normalized_fact_work: incremental.evidence.normalized_fact_work.clone(),
                }
            }
        };
        let repository_activation_sequence = self
            .activation_sequences
            .get(&receipt.repository)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(FirstSliceError::Retention)?;
        let global_activation_sequence = self
            .global_activation_sequence
            .checked_add(1)
            .ok_or(FirstSliceError::Retention)?;
        if self.durable.is_some()
            && let Some(operation) = operation
        {
            self.validate_durable_operation(operation, &receipt)?;
        }
        match staged.publication {
            FirstSlicePublication::Retained { root_path } => {
                if let Some(durable) = &self.durable {
                    let published_generation_count = self
                        .published_generation_counts
                        .get(&receipt.repository)
                        .copied()
                        .ok_or(FirstSliceError::CatalogCorrupt)?;
                    let activation_written_bytes = durable.activate_existing(
                        receipt.repository,
                        receipt.generation,
                        repository_activation_sequence,
                        global_activation_sequence,
                        published_generation_count,
                        operation,
                    )?;
                    written_bytes = written_bytes
                        .checked_add(activation_written_bytes)
                        .ok_or(FirstSliceError::Limits)?;
                    if let Some(operation) = operation {
                        self.record_durable_operation(operation, &receipt);
                    }
                }
                self.generations
                    .activate(receipt.generation)
                    .map_err(|_| FirstSliceError::Retention)?;
                self.repository_root_paths
                    .insert(receipt.repository, root_path);
            }
            FirstSlicePublication::Pending {
                root_identity,
                display_name,
                root_path,
                register_repository,
                language_coverage,
                incremental,
                reserved_memory_bytes: _,
                memory_bytes,
                mut durable,
            } => {
                let published_generation_count = self
                    .published_generation_counts
                    .get(&receipt.repository)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(FirstSliceError::Retention)?;
                if self.durable.is_some() != durable.is_some() {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                self.source_snapshots
                    .commit_staged(receipt.generation)
                    .map_err(|_| FirstSliceError::Retention)?;
                if self
                    .structural_artifacts
                    .commit_staged(receipt.generation)
                    .is_err()
                {
                    self.source_snapshots
                        .rollback_commit(receipt.generation)
                        .map_err(|_| FirstSliceError::Retention)?;
                    return Err(FirstSliceError::Retention);
                }
                let prior_active = self.generations.active_generation();
                if self
                    .generations
                    .commit_staged(receipt.generation, true)
                    .is_err()
                {
                    self.structural_artifacts
                        .rollback_commit(receipt.generation)
                        .map_err(|_| FirstSliceError::Retention)?;
                    self.source_snapshots
                        .rollback_commit(receipt.generation)
                        .map_err(|_| FirstSliceError::Retention)?;
                    return Err(FirstSliceError::Retention);
                }
                if let Some(durable) = &mut durable {
                    match durable.activate(
                        repository_activation_sequence,
                        global_activation_sequence,
                        published_generation_count,
                        operation,
                    ) {
                        Ok(activation_written_bytes) => {
                            written_bytes = written_bytes
                                .checked_add(activation_written_bytes)
                                .ok_or(FirstSliceError::Limits)?;
                        }
                        Err(error) => {
                            self.generations
                                .rollback_commit(receipt.generation, prior_active)
                                .map_err(|_| FirstSliceError::Retention)?;
                            self.structural_artifacts
                                .rollback_commit(receipt.generation)
                                .map_err(|_| FirstSliceError::Retention)?;
                            self.source_snapshots
                                .rollback_commit(receipt.generation)
                                .map_err(|_| FirstSliceError::Retention)?;
                            let source_release =
                                self.source_snapshots.begin_discard(receipt.generation)?;
                            let structural_release = self
                                .structural_artifacts
                                .begin_discard(receipt.generation)?;
                            self.generations
                                .discard_staged(receipt.generation)
                                .map_err(|_| FirstSliceError::Retention)?;
                            self.source_snapshots.finish_discard(source_release);
                            self.structural_artifacts.finish_discard(structural_release);
                            return Err(error);
                        }
                    }
                }
                self.receipts.insert(receipt.generation, receipt.clone());
                let previous_coverage = self
                    .language_coverage_by_generation
                    .insert(receipt.generation, language_coverage);
                debug_assert!(previous_coverage.is_none());
                self.incremental_baselines
                    .insert(receipt.generation, incremental.baseline);
                self.incremental_inputs
                    .insert(receipt.generation, incremental.inputs);
                self.incremental_evidence
                    .insert(receipt.generation, incremental.evidence);
                let previous_memory = self
                    .generation_memory_bytes
                    .insert(receipt.generation, memory_bytes);
                debug_assert!(previous_memory.is_none());
                self.published_generation_counts
                    .insert(receipt.repository, published_generation_count);
                if self.durable.is_some()
                    && let Some(operation) = operation
                {
                    self.record_durable_operation(operation, &receipt);
                }
                if register_repository {
                    self.repositories.insert(root_identity, receipt.repository);
                    self.repository_display_names
                        .insert(receipt.repository, display_name);
                    self.pending_repository_registrations
                        .lock()
                        .map_err(|_| FirstSliceError::Retention)?
                        .remove(&root_identity);
                }
                self.repository_root_paths
                    .insert(receipt.repository, root_path);
                if let Some(durable) = durable {
                    durable.disarm();
                }
            }
        }
        self.activation_sequences
            .insert(receipt.repository, repository_activation_sequence);
        self.global_activation_sequence = global_activation_sequence;
        self.activation_order_by_generation
            .insert(receipt.generation, global_activation_sequence);
        self.most_recent_activation = Some((global_activation_sequence, receipt.generation));
        self.active_by_repository
            .insert(receipt.repository, receipt.generation);
        self.trim_repository_retention(receipt.repository)?;
        if let Some(durable) = &self.durable {
            let retained = self
                .receipts
                .values()
                .filter_map(|candidate| {
                    (candidate.repository == receipt.repository).then_some(candidate.generation)
                })
                .collect::<BTreeSet<_>>();
            if durable
                .compact_repository(receipt.repository, &retained)
                .is_err()
            {
                // Activation is already durable at this point. Report this
                // publication as committed and fail the next mutation unless
                // bounded cleanup can be retried successfully.
                self.pending_durable_compactions.insert(receipt.repository);
            }
        }
        operation_evidence.newly_written_bytes = written_bytes;
        Ok(FirstSliceIndexCommit {
            receipt,
            written_bytes,
            evidence: operation_evidence,
        })
    }

    fn retry_pending_durable_compactions(&mut self) -> Result<(), FirstSliceError> {
        let Some(durable) = &self.durable else {
            self.pending_durable_compactions.clear();
            return Ok(());
        };
        let repositories = std::mem::take(&mut self.pending_durable_compactions);
        for repository in repositories {
            let retained = self
                .receipts
                .values()
                .filter_map(|candidate| {
                    (candidate.repository == repository).then_some(candidate.generation)
                })
                .collect::<BTreeSet<_>>();
            if let Err(error) = durable.compact_repository(repository, &retained) {
                self.pending_durable_compactions.insert(repository);
                return Err(error);
            }
        }
        Ok(())
    }

    fn validate_durable_operation(
        &self,
        operation: FirstSliceOperationContext,
        receipt: &FirstSliceIndexReceipt,
    ) -> Result<(), FirstSliceError> {
        let publication = Self::durable_operation(operation, receipt);
        match self.durable_operations.get(&operation.operation) {
            None => Ok(()),
            Some(existing) if *existing == publication => Ok(()),
            Some(_) => Err(FirstSliceError::CatalogCorrupt),
        }
    }

    fn record_durable_operation(
        &mut self,
        operation: FirstSliceOperationContext,
        receipt: &FirstSliceIndexReceipt,
    ) {
        self.durable_operations.insert(
            operation.operation,
            Self::durable_operation(operation, receipt),
        );
    }

    fn durable_operation(
        operation: FirstSliceOperationContext,
        receipt: &FirstSliceIndexReceipt,
    ) -> FirstSliceDurableOperation {
        FirstSliceDurableOperation {
            operation: operation.operation,
            started_unix_ms: operation.started_unix_ms,
            provider: operation.provider,
            receipt: receipt.clone(),
        }
    }

    /// Releases one pre-terminal staging reservation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Retention`] when a newly built reservation
    /// was already consumed or does not belong to this service.
    pub fn discard_staged(&mut self, staged: FirstSliceStagedIndex) -> Result<(), FirstSliceError> {
        let FirstSliceStagedIndex {
            receipt,
            publication,
            storage_reservation,
            ..
        } = staged;
        if let FirstSlicePublication::Pending {
            root_identity,
            register_repository,
            durable,
            ..
        } = publication
        {
            let source_release = self.source_snapshots.begin_discard(receipt.generation)?;
            let structural_release =
                match self.structural_artifacts.begin_discard(receipt.generation) {
                    Ok(release) => release,
                    Err(error) => {
                        self.source_snapshots
                            .rollback_discard(source_release)
                            .map_err(|_| FirstSliceError::Retention)?;
                        return Err(error);
                    }
                };
            if self.generations.discard_staged(receipt.generation).is_err() {
                self.structural_artifacts
                    .rollback_discard(structural_release)
                    .map_err(|_| FirstSliceError::Retention)?;
                self.source_snapshots
                    .rollback_discard(source_release)
                    .map_err(|_| FirstSliceError::Retention)?;
                return Err(FirstSliceError::Retention);
            }
            self.source_snapshots.finish_discard(source_release);
            self.structural_artifacts.finish_discard(structural_release);
            if let Some(durable) = durable {
                durable.discard()?;
            }
            if register_repository {
                self.pending_repository_registrations
                    .lock()
                    .map_err(|_| FirstSliceError::Retention)?
                    .remove(&root_identity);
            }
        }
        let _storage_reservation = StorageReservationGuard {
            reservation: storage_reservation,
        };
        Ok(())
    }

    /// Returns source-free incremental evidence retained with one generation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::GenerationNotFound`] when the generation is
    /// not retained by this service process.
    pub fn incremental_evidence(
        &self,
        generation: GenerationId,
    ) -> Result<&FirstSliceIncrementalEvidence, FirstSliceError> {
        self.incremental_evidence
            .get(&generation)
            .ok_or(FirstSliceError::GenerationNotFound)
    }

    /// Returns a response-bounded source-free invalidation trace.
    ///
    /// The complete trace remains retained with the generation. This view
    /// exposes a deterministic prefix so operation diagnostics cannot exceed
    /// their public response budget.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::GenerationNotFound`] when the generation is
    /// not retained, or [`FirstSliceError::Limits`] if its entry count cannot
    /// be represented by the public counter.
    pub fn incremental_trace_view(
        &self,
        generation: GenerationId,
        max_entries: usize,
    ) -> Result<FirstSliceInvalidationTraceView, FirstSliceError> {
        let evidence = self.incremental_evidence(generation)?;
        let total_entries = u64::try_from(evidence.invalidation_trace.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let visible_entries = evidence
            .invalidation_trace
            .iter()
            .take(max_entries)
            .cloned()
            .collect::<Vec<_>>();
        let complete = visible_entries.len() == evidence.invalidation_trace.len();
        Ok(FirstSliceInvalidationTraceView {
            version: INCREMENTAL_SCHEMA_VERSION.to_owned(),
            entries: visible_entries,
            total_entries,
            complete,
        })
    }

    /// Returns separately named structural and semantic freshness.
    ///
    /// This call does not touch the filesystem. `CurrentAtLastAuthoritativeScan`
    /// therefore means current relative to the latest successfully committed
    /// reconcile, not a live watcher observation.
    ///
    /// # Errors
    ///
    /// Returns the same repository, generation, and ownership errors as
    /// [`Self::resolve_generation`].
    pub fn generation_freshness(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<FirstSliceFreshnessStatus, FirstSliceError> {
        let generation = self.resolve_generation(repository, Some(generation))?;
        let observed = if generation.active {
            FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan
        } else {
            FirstSliceObservedFreshness::Superseded
        };
        let snapshot = self
            .generations
            .generation(generation.generation)
            .map_err(|_| FirstSliceError::GenerationNotFound)?;
        let provider_set_hash = snapshot.metadata().provider_set_hash();
        let structural_provider_set_hash =
            self.provider_set_hash(FirstSliceIndexMode::Structural)?;
        let deep_provider_set_hash = self
            .project_analyzer
            .as_ref()
            .map(|_| self.provider_set_hash(FirstSliceIndexMode::Deep))
            .transpose()?;
        let used_project_fallback =
            self.receipts
                .get(&generation.generation)
                .is_some_and(|receipt| {
                    receipt
                        .diagnostics
                        .iter()
                        .any(|diagnostic| is_project_fallback_code(&diagnostic.code))
                });
        let (semantic, publication, two_stage) = if used_project_fallback {
            (
                if generation.active {
                    FirstSliceObservedFreshness::PendingSemanticRefinement
                } else {
                    FirstSliceObservedFreshness::Superseded
                },
                if self.durable.is_some() {
                    FirstSlicePublicationMode::DurableSingleStage
                } else {
                    FirstSlicePublicationMode::ProcessLocalSingleStage
                },
                if self.durable.is_some() {
                    FirstSliceTwoStageAvailability::UnavailableWithoutSemanticRefinement
                } else {
                    FirstSliceTwoStageAvailability::UnavailableWithoutDurablePublication
                },
            )
        } else if self.durable.is_some() && deep_provider_set_hash == Some(provider_set_hash) {
            (
                observed,
                FirstSlicePublicationMode::DurableSemanticRefinement,
                FirstSliceTwoStageAvailability::SemanticRefinementPublished,
            )
        } else if self.durable.is_some()
            && self.project_analyzer.is_some()
            && provider_set_hash == structural_provider_set_hash
        {
            (
                if generation.active {
                    FirstSliceObservedFreshness::PendingSemanticRefinement
                } else {
                    FirstSliceObservedFreshness::Superseded
                },
                FirstSlicePublicationMode::DurableStructuralStage,
                FirstSliceTwoStageAvailability::StructuralPublished,
            )
        } else if self.durable.is_some() {
            // A durable structural snapshot cannot claim semantic freshness
            // when no semantic refinement provider is installed.
            (
                if generation.active {
                    FirstSliceObservedFreshness::PendingSemanticRefinement
                } else {
                    FirstSliceObservedFreshness::Superseded
                },
                FirstSlicePublicationMode::DurableSingleStage,
                FirstSliceTwoStageAvailability::UnavailableWithoutSemanticRefinement,
            )
        } else {
            (
                observed,
                FirstSlicePublicationMode::ProcessLocalSingleStage,
                FirstSliceTwoStageAvailability::UnavailableWithoutDurablePublication,
            )
        };
        Ok(FirstSliceFreshnessStatus {
            structural: observed,
            semantic,
            publication,
            two_stage,
        })
    }

    /// Returns the most recently activated generation across all repositories.
    ///
    /// Callers that already know a repository should use
    /// [`Self::active_generation_for`] to avoid cross-repository ambiguity.
    #[must_use]
    pub const fn active_generation(&self) -> Option<GenerationId> {
        match self.most_recent_activation {
            Some((_, generation)) => Some(generation),
            None => None,
        }
    }

    /// Returns the active immutable generation for one repository.
    #[must_use]
    pub fn active_generation_for(&self, repository: RepositoryId) -> Option<GenerationId> {
        self.active_by_repository.get(&repository).copied()
    }

    /// Reports whether the active generation already carries deep project facts.
    ///
    /// # Errors
    ///
    /// Returns an error if the active generation cannot be opened or the
    /// configured provider identity cannot be derived.
    pub fn active_generation_is_deep(
        &self,
        repository: RepositoryId,
    ) -> Result<bool, FirstSliceError> {
        let Some(generation) = self.active_generation_for(repository) else {
            return Ok(false);
        };
        let snapshot = self
            .generations
            .generation(generation)
            .map_err(|_| FirstSliceError::Retention)?;
        Ok(snapshot.metadata().provider_set_hash()
            == self.provider_set_hash(FirstSliceIndexMode::Deep)?)
    }

    /// Exports one retained immutable generation as a portable source-free
    /// bundle.
    ///
    /// # Errors
    ///
    /// Returns repository and generation selection failures from
    /// [`Self::resolve_generation`], or [`FirstSliceError::Sharing`] when
    /// canonical encoding, cancellation, or transfer limits reject export.
    pub fn export_shared_generation(
        &self,
        repository: RepositoryId,
        generation: Option<GenerationId>,
        limits: SharedGenerationLimits,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceSharedGenerationExport, FirstSliceError> {
        let generation = self.resolve_generation(repository, generation)?.generation;
        let snapshot = self
            .generations
            .generation(generation)
            .map_err(|error| map_query_error(error, cancellation))?;
        let source_set_hash = shared_generation_source_set_hash(snapshot.document())
            .map_err(|error| map_sharing_error(error, cancellation))?;
        let bundle = encode_shared_generation(snapshot, limits, cancellation)
            .map_err(|error| map_sharing_error(error, cancellation))?;
        Ok(FirstSliceSharedGenerationExport {
            repository,
            generation,
            source_set_hash,
            bundle,
        })
    }

    /// Imports and identity-verifies one portable generation without
    /// activating it or mutating this service's retained generations.
    ///
    /// The returned object owns the verified immutable generation. Callers may
    /// inspect it or pass it into a separately authorized publication flow;
    /// this method intentionally has no implicit catalog write.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Sharing`] for framing, integrity,
    /// repository, generation, source-set, cancellation, or resource-limit
    /// failures.
    pub fn import_shared_generation(
        &self,
        encoded: &[u8],
        expectation: SharedGenerationExpectation,
        limits: SharedGenerationLimits,
        cancellation: &Cancellation,
    ) -> Result<SharedGenerationImport, FirstSliceError> {
        let context = GenerationContext::new(cancellation, GenerationBudget::default());
        decode_shared_generation(
            encoded,
            expectation,
            limits,
            &IrLimits::default(),
            &self.extensions,
            &context,
        )
        .map_err(|error| map_sharing_error(error, cancellation))
    }

    /// Imports explicit local runtime observations for one exact retained
    /// generation without mutating its canonical static facts.
    ///
    /// The returned overlay remains caller-owned and read-only. This service
    /// does not persist, activate, or merge it into normalized IR.
    ///
    /// # Errors
    ///
    /// Returns repository or generation selection failures from
    /// [`Self::resolve_generation`], [`FirstSliceError::RuntimeTrace`] when the
    /// bounded trace contract rejects the input, or [`FirstSliceError::Cancelled`]
    /// when cooperative cancellation wins.
    pub fn import_runtime_trace_overlay(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        trace: &[u8],
        limits: RuntimeTraceLimits,
        cancellation: &Cancellation,
    ) -> Result<RuntimeTraceOverlay, FirstSliceError> {
        let context = self.resolve_generation(repository, Some(generation))?;
        let snapshot = self
            .generations
            .generation(context.generation)
            .map_err(|error| map_query_error(error, cancellation))?;
        import_runtime_trace(
            trace,
            RuntimeTraceImportRequest::new(
                repository,
                context.generation,
                snapshot.document(),
                cancellation,
            )
            .with_limits(limits),
        )
        .map_err(map_runtime_trace_error)
    }

    /// Reports whether commits cross the private durable catalog boundary.
    #[must_use]
    pub const fn uses_durable_publication(&self) -> bool {
        self.durable.is_some()
    }

    /// Iterates restored durable operation-to-generation publications.
    ///
    /// The ordered, source-redacted records let the daemon rebuild its
    /// process-local status projection while the operation journal remains the
    /// authority for lifecycle state.
    pub fn durable_operation_publications(
        &self,
    ) -> impl ExactSizeIterator<Item = FirstSliceDurableOperation> + '_ {
        self.durable_operations.values().cloned()
    }

    /// Returns bounded source-free indexing facts after a verified durable scan.
    ///
    /// Durable callers should reserve this catalog-wide content revalidation for
    /// recovery and explicit integrity work. Automatic post-mutation refreshes
    /// should use [`Self::support_inventory_snapshot_from_accounting`].
    ///
    /// # Errors
    ///
    /// Returns a durable catalog, retention, or corruption failure when physical
    /// storage cannot be read and verified, or [`FirstSliceError::Limits`] when
    /// a retained count cannot be represented by the support contract.
    pub fn support_inventory_snapshot(
        &self,
    ) -> Result<FirstSliceSupportInventory, FirstSliceError> {
        let storage = self
            .durable
            .as_ref()
            .map(|durable| durable.storage_inventory())
            .transpose()?;
        let accounting_state = if storage.is_some() {
            FirstSliceStorageAccountingState::VerifiedScan
        } else {
            FirstSliceStorageAccountingState::Unavailable
        };
        self.support_inventory_snapshot_inner(storage, accounting_state)
    }

    /// Returns bounded source-free indexing facts from reconciled storage accounting.
    ///
    /// This projection is intended for automatic refreshes after Rootlight-owned
    /// mutations. Explicit integrity checks should use
    /// [`Self::support_inventory_snapshot`] so they revalidate durable contents.
    /// A `None` snapshot leaves the last published support state unchanged while
    /// accounting is cold, dirty, or covered by an in-flight reservation.
    ///
    /// # Errors
    ///
    /// Returns a durable catalog failure when accounted state is internally
    /// inconsistent, or [`FirstSliceError::Limits`] when a retained count cannot
    /// be represented by the support contract. Physical fields remain unknown
    /// while accounting awaits explicit reconciliation.
    pub fn support_inventory_snapshot_from_accounting(
        &self,
    ) -> Result<Option<FirstSliceSupportInventory>, FirstSliceError> {
        let storage = match self.durable.as_ref() {
            Some(durable) => match durable.storage_inventory_cached()? {
                Some(storage) => Some(storage),
                None => return Ok(None),
            },
            None => {
                return self
                    .support_inventory_snapshot_inner(
                        None,
                        FirstSliceStorageAccountingState::Unavailable,
                    )
                    .map(Some);
            }
        };
        self.support_inventory_snapshot_inner(storage, FirstSliceStorageAccountingState::Reconciled)
            .map(Some)
    }

    /// Returns bounded source-free indexing facts without scanning durable storage.
    ///
    /// This startup projection deliberately leaves physical storage measurements
    /// unknown so daemon readiness cannot depend on catalog-wide blob verification.
    ///
    /// # Errors
    ///
    /// Returns the same in-memory consistency and limit failures as
    /// [`Self::support_inventory_snapshot`].
    pub fn support_inventory_snapshot_without_storage_scan(
        &self,
    ) -> Result<FirstSliceSupportInventory, FirstSliceError> {
        self.support_inventory_snapshot_inner(None, FirstSliceStorageAccountingState::Unavailable)
    }

    fn support_inventory_snapshot_inner(
        &self,
        storage: Option<durable::DurableStorageInventory>,
        storage_accounting_state: FirstSliceStorageAccountingState,
    ) -> Result<FirstSliceSupportInventory, FirstSliceError> {
        let pending = self
            .pending_repository_registrations
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let committed_repository_count = self.repository_display_names.len();
        let active_repository_count = self.active_by_repository.len();
        if committed_repository_count != active_repository_count
            || self.repositories.len() != committed_repository_count
            || !self
                .repository_display_names
                .keys()
                .eq(self.active_by_repository.keys())
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let mut committed_repositories = BTreeSet::new();
        for repository in self.repositories.values() {
            if !self.active_by_repository.contains_key(repository)
                || !committed_repositories.insert(*repository)
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        let mut pending_repositories = BTreeSet::new();
        for (root_identity, (repository, _, _)) in pending.iter() {
            if self.repositories.contains_key(root_identity)
                || self.active_by_repository.contains_key(repository)
                || !pending_repositories.insert(*repository)
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        let registered_repository_count = committed_repository_count
            .checked_add(pending.len())
            .ok_or(FirstSliceError::Limits)?;
        let configured_maximum_repositories =
            usize::try_from(self.storage_policy.maximum_repositories)
                .map_err(|_| FirstSliceError::Limits)?;
        let effective_maximum_repositories = self
            .storage_policy
            .effective_maximum_repositories(self.maximum_generations_per_repository)?;
        if effective_maximum_repositories != self.maximum_repositories
            || effective_maximum_repositories > configured_maximum_repositories
            || registered_repository_count > effective_maximum_repositories
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let repository_headroom = effective_maximum_repositories
            .checked_sub(registered_repository_count)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let repository_capacity = FirstSliceRepositoryCapacityInventory {
            registered_repository_count: u32::try_from(registered_repository_count)
                .map_err(|_| FirstSliceError::Limits)?,
            pending_repository_count: u32::try_from(pending.len())
                .map_err(|_| FirstSliceError::Limits)?,
            active_repository_count: u32::try_from(active_repository_count)
                .map_err(|_| FirstSliceError::Limits)?,
            failed_repository_count: 0,
            pinned_repository_count: 0,
            reclaimable_repository_count: u32::try_from(committed_repository_count)
                .map_err(|_| FirstSliceError::Limits)?,
            pinning_supported: false,
            configured_maximum_repositories: self.storage_policy.maximum_repositories,
            effective_maximum_repositories: u32::try_from(effective_maximum_repositories)
                .map_err(|_| FirstSliceError::Limits)?,
            repository_headroom: u32::try_from(repository_headroom)
                .map_err(|_| FirstSliceError::Limits)?,
        };
        drop(pending);

        let languages: Vec<_> = self.analyzers.keys().cloned().collect();
        let mut adapters = vec![FirstSliceSupportAdapter {
            name: "tree-sitter".to_owned(),
            languages,
            isolated: false,
        }];
        if self.project_analyzer.is_some() {
            adapters.push(FirstSliceSupportAdapter {
                name: "project-adapter".to_owned(),
                languages: PROJECT_ADAPTER_SUPPORT_LANGUAGES
                    .into_iter()
                    .map(|language| language.as_str().to_owned())
                    .collect(),
                isolated: true,
            });
        }

        let mut repositories = Vec::new();
        repositories
            .try_reserve_exact(self.active_by_repository.len())
            .map_err(|_| FirstSliceError::Limits)?;
        for (repository, active_generation) in &self.active_by_repository {
            let snapshot = self
                .generations
                .generation(*active_generation)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let coverage = self
                .language_coverage_by_generation
                .get(active_generation)
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            let languages = coverage
                .iter()
                .map(|entry| entry.language.clone())
                .collect();
            let tiers = coverage
                .iter()
                .map(|entry| analysis_tier_label(entry.tier).to_owned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let generation_count = u32::try_from(self.retained_generation_count(*repository))
                .map_err(|_| FirstSliceError::Limits)?;
            let repository_storage = storage.as_ref().and_then(|inventory| {
                inventory
                    .repositories
                    .iter()
                    .find(|storage| storage.repository == *repository)
            });
            let repository_headroom_bytes = storage.as_ref().map(|inventory| {
                durable::storage_headroom(
                    inventory,
                    *repository,
                    durable::DurableStorageAdmissionPolicy {
                        required_catalog_bytes: 0,
                        required_repository_bytes: 0,
                        maximum_repository_bytes: self.storage_policy.maximum_repository_bytes,
                        maximum_storage_bytes: self.storage_policy.maximum_catalog_bytes,
                        minimum_free_bytes: self.storage_policy.minimum_free_disk_bytes,
                    },
                )
                .repository_bytes
            });
            repositories.push(FirstSliceSupportRepository {
                repository: *repository,
                languages,
                tiers,
                files: u64::try_from(snapshot.document().files.len())
                    .map_err(|_| FirstSliceError::Limits)?,
                symbols: u64::try_from(snapshot.document().entities.len())
                    .map_err(|_| FirstSliceError::Limits)?,
                relationships: u64::try_from(snapshot.document().relations.len())
                    .map_err(|_| FirstSliceError::Limits)?,
                generation_count,
                storage_bytes: repository_storage.map(|storage| storage.physical_bytes),
                active_generation_bytes: repository_storage
                    .map(|storage| storage.active_generation_bytes),
                predecessor_generation_bytes: repository_storage
                    .map(|storage| storage.predecessor_generation_bytes),
                other_retained_generation_bytes: repository_storage
                    .map(|storage| storage.other_retained_generation_bytes),
                source_pool_bytes: repository_storage.map(|storage| storage.source_pool_bytes),
                shared_source_bytes: repository_storage.map(|storage| storage.shared_source_bytes),
                temporary_bytes: repository_storage.map(|storage| storage.temporary_bytes),
                reclaimable_bytes: repository_storage.map(|storage| storage.reclaimable_bytes),
                repository_overhead_bytes: repository_storage
                    .map(|storage| storage.repository_overhead_bytes),
                inflight_reservation_bytes: repository_storage
                    .map(|storage| storage.inflight_reservation_bytes),
                repository_headroom_bytes,
            });
        }

        let mut generation_disk_bytes = 0_u64;
        let mut generations = Vec::new();
        generations
            .try_reserve_exact(self.receipts.len())
            .map_err(|_| FirstSliceError::Limits)?;
        for receipt in self.receipts.values() {
            let disk_bytes = if self.durable.is_some() {
                receipt.retained_durable_bytes
            } else {
                receipt.oracle_allocated_bytes
            };
            generation_disk_bytes = generation_disk_bytes
                .checked_add(disk_bytes)
                .ok_or(FirstSliceError::Limits)?;
            generations.push(FirstSliceSupportGeneration {
                repository: receipt.repository,
                generation: receipt.generation,
                disk_bytes,
                parent: receipt.parent,
                active: self.active_by_repository.get(&receipt.repository)
                    == Some(&receipt.generation),
            });
        }

        let admission_policy = durable::DurableStorageAdmissionPolicy {
            required_catalog_bytes: 0,
            required_repository_bytes: 0,
            maximum_repository_bytes: self.storage_policy.maximum_repository_bytes,
            maximum_storage_bytes: self.storage_policy.maximum_catalog_bytes,
            minimum_free_bytes: self.storage_policy.minimum_free_disk_bytes,
        };
        let storage_headroom = storage.as_ref().map(|inventory| {
            inventory
                .repositories
                .iter()
                .map(|repository| {
                    durable::storage_headroom(inventory, repository.repository, admission_policy)
                })
                .reduce(|left, right| durable::DurableStorageHeadroom {
                    repository_bytes: left.repository_bytes.min(right.repository_bytes),
                    catalog_bytes: left.catalog_bytes.min(right.catalog_bytes),
                    filesystem_bytes: left.filesystem_bytes.min(right.filesystem_bytes),
                    admission_bytes: left.admission_bytes.min(right.admission_bytes),
                })
                .unwrap_or_else(|| {
                    durable::storage_headroom(
                        inventory,
                        RepositoryId::from_bytes([0; 16]),
                        admission_policy,
                    )
                })
        });
        let total_storage_bytes = storage
            .as_ref()
            .map(|inventory| inventory.total_physical_bytes);
        let shared_bytes = storage
            .as_ref()
            .map(|inventory| inventory.shared_source_bytes);
        let reclaimable_bytes = storage
            .as_ref()
            .map(|inventory| inventory.reclaimable_bytes);
        let pinned_bytes = Some(
            storage
                .as_ref()
                .map_or(0, |inventory| inventory.pinned_bytes),
        );
        let active_generation_bytes = storage
            .as_ref()
            .map(|inventory| inventory.active_generation_bytes);
        let predecessor_generation_bytes = storage
            .as_ref()
            .map(|inventory| inventory.predecessor_generation_bytes);
        let other_retained_generation_bytes = storage
            .as_ref()
            .map(|inventory| inventory.other_retained_generation_bytes);
        let source_pool_bytes = storage
            .as_ref()
            .map(|inventory| inventory.source_pool_bytes);
        let unreclaimed_temporary_bytes = storage
            .as_ref()
            .map_or(0, |inventory| inventory.temporary_bytes);
        let repository_overhead_bytes = storage
            .as_ref()
            .map(|inventory| inventory.repository_overhead_bytes);
        let quarantine_bytes = storage.as_ref().map(|inventory| inventory.quarantine_bytes);
        let inflight_catalog_reservation_bytes = storage
            .as_ref()
            .map(|inventory| inventory.inflight_catalog_reservation_bytes);
        let inflight_repository_reservation_bytes = storage
            .as_ref()
            .map(|inventory| inventory.inflight_repository_reservation_bytes);
        let disk_margin_bytes = storage.as_ref().map(|inventory| inventory.available_bytes);
        let admission_margin_bytes = storage_headroom.map(|headroom| headroom.admission_bytes);
        Ok(FirstSliceSupportInventory {
            adapters,
            languages: language_capabilities()
                .iter()
                .map(|capability| FirstSliceSupportLanguageCapability {
                    language: capability.language.to_owned(),
                    suffixes: capability
                        .suffixes
                        .iter()
                        .map(|suffix| (*suffix).to_owned())
                        .collect(),
                    aliases: capability
                        .aliases
                        .iter()
                        .map(|alias| (*alias).to_owned())
                        .collect(),
                    detectors: capability
                        .detectors
                        .iter()
                        .map(|detector| (*detector).to_owned())
                        .collect(),
                    maximum_tier: capability.maximum_tier.to_owned(),
                    analyzers: capability
                        .analyzers
                        .iter()
                        .map(|analyzer| (*analyzer).to_owned())
                        .collect(),
                })
                .collect(),
            repositories,
            repository_capacity,
            generations,
            generation_format: format!(
                "{}.{}",
                GENERATION_CONTRACT_VERSION.major(),
                GENERATION_CONTRACT_VERSION.minor()
            ),
            generation_disk_bytes,
            total_storage_bytes,
            shared_bytes,
            reclaimable_bytes,
            pinned_bytes,
            active_generation_bytes,
            predecessor_generation_bytes,
            other_retained_generation_bytes,
            source_pool_bytes,
            unreclaimed_temporary_bytes,
            repository_overhead_bytes,
            quarantine_bytes,
            pinning_supported: false,
            inflight_catalog_reservation_bytes,
            inflight_repository_reservation_bytes,
            disk_margin_bytes,
            admission_margin_bytes,
            repository_headroom_bytes: storage_headroom.map(|headroom| headroom.repository_bytes),
            catalog_headroom_bytes: storage_headroom.map(|headroom| headroom.catalog_bytes),
            filesystem_headroom_bytes: storage_headroom.map(|headroom| headroom.filesystem_bytes),
            storage_accounting_state,
            maximum_repository_storage_bytes: self.storage_policy.maximum_repository_bytes,
            maximum_catalog_storage_bytes: self.storage_policy.maximum_catalog_bytes,
            minimum_free_disk_bytes: self.storage_policy.minimum_free_disk_bytes,
            effective_retention_generations: u32::try_from(self.maximum_generations_per_repository)
                .map_err(|_| FirstSliceError::Limits)?,
            source_reservation_factor: self.storage_policy.source_reservation_factor,
            oracle_reservation_factor: self.storage_policy.oracle_reservation_factor,
        })
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
        let active_receipt = self
            .receipts
            .get(&active)
            .cloned()
            .ok_or(FirstSliceError::GenerationNotFound)?;
        let active_parent = active_receipt.parent;
        let generation = generation.unwrap_or(active);
        let receipt = if generation == active {
            active_receipt
        } else {
            self.receipts
                .get(&generation)
                .cloned()
                .ok_or(FirstSliceError::GenerationNotFound)?
        };
        if receipt.repository != repository {
            return Err(FirstSliceError::GenerationMismatch);
        }
        Ok(FirstSliceGenerationContext {
            repository,
            generation,
            parent: receipt.parent,
            active_generation: active,
            active_parent,
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
        page_offset: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeLocateResult>, FirstSliceError> {
        self.code_locate_with_budget(
            generation,
            query,
            mode,
            maximum_results,
            page_offset,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes a generation-pinned `code.locate` query under a reduced policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies one bounded locate request"
    )]
    pub fn code_locate_with_budget(
        &self,
        generation: GenerationId,
        query: String,
        mode: LocateMode,
        maximum_results: usize,
        page_offset: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeLocateResult>, FirstSliceError> {
        self.code_locate_with_languages_and_budget(
            generation,
            query,
            mode,
            Vec::new(),
            maximum_results,
            page_offset,
            budget,
            cancellation,
        )
    }

    /// Executes a generation-pinned `code.locate` query over a language union.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid language
    /// filter or plan, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy and language union accompany one bounded locate request"
    )]
    pub fn code_locate_with_languages_and_budget(
        &self,
        generation: GenerationId,
        query: String,
        mode: LocateMode,
        languages: Vec<String>,
        maximum_results: usize,
        page_offset: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeLocateResult>, FirstSliceError> {
        self.code_locate_with_filters_and_budget(
            generation,
            query,
            mode,
            languages,
            Vec::new(),
            maximum_results,
            page_offset,
            budget,
            cancellation,
        )
    }

    /// Executes a generation-pinned `code.locate` query over language and path unions.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid filter or
    /// plan, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy, language union, and path union accompany one bounded locate request"
    )]
    pub fn code_locate_with_filters_and_budget(
        &self,
        generation: GenerationId,
        query: String,
        mode: LocateMode,
        mut languages: Vec<String>,
        path_prefixes: Vec<String>,
        maximum_results: usize,
        page_offset: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeLocateResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let query_budget = budget.query();
        let mut search_budget = budget.search();
        let mut effective_maximum_results = maximum_results.min(search_budget.max_results);
        if let Ok(query_maximum_results) = usize::try_from(query_budget.max_results()) {
            effective_maximum_results = effective_maximum_results.min(query_maximum_results);
        }
        let result_rows = u64::try_from(effective_maximum_results)
            .map_err(|_| FirstSliceError::BudgetExceeded)?;
        let candidate_rows = query_budget.max_rows().saturating_sub(result_rows);
        if let Ok(candidate_rows) = usize::try_from(candidate_rows) {
            search_budget.max_candidates = search_budget.max_candidates.min(candidate_rows);
        }
        for language in &mut languages {
            let Some(canonical) = canonical_language(language) else {
                continue;
            };
            language
                .try_reserve(canonical.len().saturating_sub(language.len()))
                .map_err(|_| FirstSliceError::BudgetExceeded)?;
            language.clear();
            language.push_str(canonical);
        }
        let plan = service
            .plan_code_locate_with_filters(
                query,
                mode,
                languages,
                path_prefixes,
                effective_maximum_results,
                page_offset,
                search_budget,
                query_budget,
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
        self.symbol_explain_with_budget(
            generation,
            symbol,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes a generation-pinned `symbol.explain` query under a reduced policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, symbol, invalid
    /// plan, or bounded execution failure.
    pub fn symbol_explain_with_budget(
        &self,
        generation: GenerationId,
        symbol: SymbolId,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SymbolExplainResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_symbol_explain(symbol, budget.query())
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_symbol_explain(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `symbol.relationships` query.
    ///
    /// The seed symbols, relation families, optional direction override,
    /// confidence floor, and result bound are validated by the query plan. The
    /// result carries deterministic seed-relation groups plus aggregate edge
    /// counts and truncation evidence.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded relationships query dimension"
    )]
    pub fn symbol_relationships(
        &self,
        generation: GenerationId,
        seeds: BTreeSet<SymbolId>,
        families: Vec<RelationFamily>,
        direction: Option<RelationDirection>,
        min_confidence: u16,
        max_results: usize,
        page_offset: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SymbolRelationshipsResult>, FirstSliceError> {
        self.symbol_relationships_with_budget(
            generation,
            seeds,
            families,
            direction,
            min_confidence,
            max_results,
            page_offset,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `symbol.relationships` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded relationships dimensions"
    )]
    pub fn symbol_relationships_with_budget(
        &self,
        generation: GenerationId,
        seeds: BTreeSet<SymbolId>,
        families: Vec<RelationFamily>,
        direction: Option<RelationDirection>,
        min_confidence: u16,
        max_results: usize,
        page_offset: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SymbolRelationshipsResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_symbol_relationships(
                seeds,
                families,
                direction,
                min_confidence,
                max_results,
                page_offset,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_symbol_relationships(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `flow.trace` query.
    ///
    /// The source and optional target symbols, relation families, direction,
    /// confidence floor, depth, and path cap are validated by the query plan.
    /// The result carries deterministic bounded paths plus a traversal frontier
    /// and the relation projection actually used.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded flow trace dimension"
    )]
    pub fn flow_trace(
        &self,
        generation: GenerationId,
        from: SymbolId,
        to: Option<SymbolId>,
        families: Vec<RelationFamily>,
        direction: Option<RelationDirection>,
        min_confidence: u16,
        max_depth: u8,
        max_paths: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<FlowTraceResult>, FirstSliceError> {
        self.flow_trace_with_budget(
            generation,
            from,
            to,
            families,
            direction,
            min_confidence,
            max_depth,
            max_paths,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `flow.trace` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded flow dimensions"
    )]
    pub fn flow_trace_with_budget(
        &self,
        generation: GenerationId,
        from: SymbolId,
        to: Option<SymbolId>,
        families: Vec<RelationFamily>,
        direction: Option<RelationDirection>,
        min_confidence: u16,
        max_depth: u8,
        max_paths: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<FlowTraceResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_flow_trace(
                from,
                to,
                direction,
                families,
                min_confidence,
                max_depth,
                max_paths,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_flow_trace(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Resolves one evidence-backed hop into another active repository.
    ///
    /// The stitch is admitted only when the source generation contains an
    /// occurrence enclosed by `from` whose exact spelling hash matches the
    /// requested target entity. This preserves honest unresolved boundaries:
    /// merely supplying two valid symbol identifiers never fabricates an edge.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Query`] when the source generation or symbol
    /// is unavailable and propagates cooperative cancellation.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded cross-repository flow dimension"
    )]
    pub fn cross_repository_flow_link(
        &self,
        generation: GenerationId,
        from: SymbolId,
        to: SymbolId,
        families: &[RelationFamily],
        direction: RelationDirection,
        min_confidence: u16,
        cancellation: &Cancellation,
    ) -> Result<Option<FirstSliceCrossRepositoryLink>, FirstSliceError> {
        check_cancellation(cancellation)?;
        if direction == RelationDirection::Inbound {
            return Ok(None);
        }
        let source = self
            .generations
            .generation(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let source_repository = source.metadata().repository();
        if !source
            .document()
            .entities
            .iter()
            .any(|entity| entity.id == from)
        {
            return Err(FirstSliceError::Query);
        }
        let target = self
            .active_by_repository
            .iter()
            .filter(|(repository, _)| **repository != source_repository)
            .find_map(|(repository, target_generation)| {
                let snapshot = self.generations.generation(*target_generation).ok()?;
                let entity = snapshot
                    .document()
                    .entities
                    .iter()
                    .find(|entity| entity.id == to)?;
                Some((*repository, *target_generation, entity))
            });
        let Some((target_repository, target_generation, target)) = target else {
            return Ok(None);
        };
        let mut target_hashes = BTreeSet::new();
        for name in [
            target.canonical_name.as_str(),
            target.display_name.as_str(),
            target.qualified_name.as_str(),
            target
                .qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(target.qualified_name.as_str()),
        ] {
            if !name.is_empty() {
                target_hashes.insert(content_hash(name.as_bytes()));
            }
        }
        for occurrence in &source.document().occurrences {
            check_cancellation(cancellation)?;
            if occurrence.enclosing != Some(from)
                || occurrence.confidence.get() < min_confidence
                || !target_hashes.contains(&occurrence.syntactic_text_hash)
            {
                continue;
            }
            let family = match occurrence.role {
                OccurrenceRole::CallSite => RelationFamily::Calls,
                OccurrenceRole::ImportUse => RelationFamily::Imports,
                OccurrenceRole::Reference | OccurrenceRole::Read | OccurrenceRole::Write => {
                    RelationFamily::References
                }
                OccurrenceRole::TypeUse => RelationFamily::Types,
                OccurrenceRole::RouteUse => RelationFamily::ServiceCall,
                _ => continue,
            };
            if !families.is_empty() && !families.contains(&family) {
                continue;
            }
            return Ok(Some(FirstSliceCrossRepositoryLink {
                target_repository,
                target_generation,
                family,
                confidence: occurrence.confidence.get(),
                source_refs: vec![occurrence.source.clone()],
            }));
        }
        Ok(None)
    }

    /// Executes a generation-pinned bounded `architecture.cycles` query.
    ///
    /// The relation families, component-size floor, cycle cap, and self-cycle
    /// opt-in are validated by the query plan. The result carries deterministic
    /// strongly connected components, bounded representative minimal cycles,
    /// ranked break candidates, and the relation projection actually used.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    pub fn architecture_cycles(
        &self,
        generation: GenerationId,
        families: Vec<RelationFamily>,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureCyclesResult>, FirstSliceError> {
        self.architecture_cycles_with_budget(
            generation,
            families,
            min_size,
            max_cycles,
            include_self_cycles,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `architecture.cycles` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded cycle dimensions"
    )]
    pub fn architecture_cycles_with_budget(
        &self,
        generation: GenerationId,
        families: Vec<RelationFamily>,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureCyclesResult>, FirstSliceError> {
        self.architecture_cycles_with_options_and_budget(
            generation,
            families,
            None,
            CycleProjectionLevel::Symbol,
            min_size,
            max_cycles,
            include_self_cycles,
            CycleRankBy::Size,
            budget,
            cancellation,
        )
    }

    /// Executes `architecture.cycles` with the complete scoped projection contract.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid scope or
    /// projection, invalid plan, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded architecture-cycle dimension"
    )]
    pub fn architecture_cycles_with_options_and_budget(
        &self,
        generation: GenerationId,
        families: Vec<RelationFamily>,
        scope: Option<AnalysisScope>,
        level: CycleProjectionLevel,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
        rank_by: CycleRankBy,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureCyclesResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_architecture_cycles_with_options(
                families,
                scope,
                level,
                0,
                min_size,
                max_cycles,
                include_self_cycles,
                rank_by,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_architecture_cycles(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `code.dead` query.
    ///
    /// The entry-point policy, exported and test inclusion flags, confidence
    /// floor, and candidate cap are validated by the query plan. The result
    /// carries deterministic ranked dead-code candidates, the partial
    /// entry-point model summary, blind spots, and applied suppression rules.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded code dead dimension"
    )]
    pub fn code_dead(
        &self,
        generation: GenerationId,
        entry_point_policy: CodeDeadEntryPointPolicy,
        include_exported: bool,
        include_tests: bool,
        min_confidence: u16,
        max_candidates: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeDeadResult>, FirstSliceError> {
        self.code_dead_with_budget(
            generation,
            entry_point_policy,
            include_exported,
            include_tests,
            min_confidence,
            max_candidates,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `code.dead` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded dead-code dimensions"
    )]
    pub fn code_dead_with_budget(
        &self,
        generation: GenerationId,
        entry_point_policy: CodeDeadEntryPointPolicy,
        include_exported: bool,
        include_tests: bool,
        min_confidence: u16,
        max_candidates: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeDeadResult>, FirstSliceError> {
        self.code_dead_with_options_and_budget(
            generation,
            entry_point_policy,
            BTreeSet::new(),
            None,
            include_exported,
            include_tests,
            min_confidence,
            max_candidates,
            budget,
            cancellation,
        )
    }

    /// Executes `code.dead` with a complete typed entry model and scope.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid scope or
    /// entry model, invalid plan, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded dead-code dimension"
    )]
    pub fn code_dead_with_options_and_budget(
        &self,
        generation: GenerationId,
        entry_point_policy: CodeDeadEntryPointPolicy,
        explicit_entry_points: BTreeSet<SymbolId>,
        scope: Option<AnalysisScope>,
        include_exported: bool,
        include_tests: bool,
        min_confidence: u16,
        max_candidates: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeDeadResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_code_dead_with_options(
                entry_point_policy,
                explicit_entry_points,
                scope,
                include_exported,
                include_tests,
                min_confidence,
                max_candidates,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_code_dead(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `architecture.overview` query.
    ///
    /// The requested derived views, confidence floor, component cap, and edge
    /// inclusion are validated by the query plan. The result carries
    /// deterministic file-granularity components, aggregated typed connections,
    /// hotspot rankings, and derived-view metadata.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    pub fn architecture_overview(
        &self,
        generation: GenerationId,
        views: Vec<ArchitectureOverviewView>,
        min_confidence: u16,
        max_components: usize,
        include_edges: bool,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureOverviewResult>, FirstSliceError> {
        self.architecture_overview_with_budget(
            generation,
            views,
            min_confidence,
            max_components,
            include_edges,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `architecture.overview` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded overview dimensions"
    )]
    pub fn architecture_overview_with_budget(
        &self,
        generation: GenerationId,
        views: Vec<ArchitectureOverviewView>,
        min_confidence: u16,
        max_components: usize,
        include_edges: bool,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureOverviewResult>, FirstSliceError> {
        self.architecture_overview_with_options_and_budget(
            generation,
            views,
            None,
            ArchitectureOverviewDetail::Standard,
            min_confidence,
            max_components,
            include_edges,
            budget,
            cancellation,
        )
    }

    /// Executes `architecture.overview` with complete typed scope and detail.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid scope or
    /// view, invalid plan, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded architecture-overview dimension"
    )]
    pub fn architecture_overview_with_options_and_budget(
        &self,
        generation: GenerationId,
        views: Vec<ArchitectureOverviewView>,
        scope: Option<AnalysisScope>,
        detail: ArchitectureOverviewDetail,
        min_confidence: u16,
        max_components: usize,
        include_edges: bool,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureOverviewResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_architecture_overview_with_options(
                views,
                scope,
                detail,
                min_confidence,
                max_components,
                include_edges,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_architecture_overview(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `tests.select` query.
    ///
    /// The seed set, requested test kinds, test cap, and command inclusion are
    /// validated by the query plan. The result carries deterministic ranked
    /// tests, the coverage strategy actually used, and honest gaps for seeds
    /// with no related test.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    pub fn tests_select(
        &self,
        generation: GenerationId,
        seeds: BTreeSet<SymbolId>,
        test_kinds: Vec<TestsSelectKind>,
        max_tests: usize,
        include_commands: bool,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<TestsSelectResult>, FirstSliceError> {
        self.tests_select_with_budget(
            generation,
            seeds,
            test_kinds,
            max_tests,
            include_commands,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `tests.select` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded test-selection dimensions"
    )]
    pub fn tests_select_with_budget(
        &self,
        generation: GenerationId,
        seeds: BTreeSet<SymbolId>,
        test_kinds: Vec<TestsSelectKind>,
        max_tests: usize,
        include_commands: bool,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<TestsSelectResult>, FirstSliceError> {
        self.tests_select_with_filters_and_budget(
            generation,
            seeds,
            Vec::new(),
            Vec::new(),
            test_kinds,
            Vec::new(),
            max_tests,
            None,
            None,
            include_commands,
            budget,
            cancellation,
        )
    }

    /// Executes `tests.select` with bounded seed, framework, and cost filters.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid filter,
    /// invalid plan, cancellation, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded public test-selection dimension"
    )]
    pub fn tests_select_with_filters_and_budget(
        &self,
        generation: GenerationId,
        seeds: BTreeSet<SymbolId>,
        seed_paths: Vec<String>,
        seed_build_targets: Vec<String>,
        test_kinds: Vec<TestsSelectKind>,
        frameworks: Vec<String>,
        max_tests: usize,
        max_total_ms: Option<u32>,
        max_slow_tests: Option<u16>,
        include_commands: bool,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<TestsSelectResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_tests_select_with_filters(
                seeds,
                seed_paths,
                seed_build_targets,
                test_kinds,
                frameworks,
                max_tests,
                max_total_ms,
                max_slow_tests,
                include_commands,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_tests_select(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `change.impact` query.
    ///
    /// The explicit changed symbols and paths, depth and confidence bounds, test
    /// inclusion, and dependent cap are validated by the query plan. The result
    /// carries deterministic resolved changes, ranked impact groups, optional
    /// test candidates, and an honest risk summary.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the facade forwards the explicit change set plus its bounded propagation options"
    )]
    pub fn change_impact(
        &self,
        generation: GenerationId,
        changed_symbols: BTreeSet<SymbolId>,
        changed_paths: Vec<String>,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        max_dependents: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ChangeImpactResult>, FirstSliceError> {
        self.change_impact_with_budget(
            generation,
            changed_symbols,
            changed_paths,
            max_depth,
            min_confidence,
            include_tests,
            max_dependents,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `change.impact` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded impact dimensions"
    )]
    pub fn change_impact_with_budget(
        &self,
        generation: GenerationId,
        changed_symbols: BTreeSet<SymbolId>,
        changed_paths: Vec<String>,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        max_dependents: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ChangeImpactResult>, FirstSliceError> {
        self.change_impact_with_policy_and_budget(
            generation,
            changed_symbols,
            changed_paths,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ChangeImpactRelationPolicy::Standard,
            max_depth,
            min_confidence,
            include_tests,
            false,
            max_dependents,
            budget,
            cancellation,
        )
    }

    /// Executes `change.impact` with bounded scope and relation policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid scope or
    /// plan, cancellation, or bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded public impact-analysis dimension"
    )]
    pub fn change_impact_with_policy_and_budget(
        &self,
        generation: GenerationId,
        changed_symbols: BTreeSet<SymbolId>,
        changed_paths: Vec<String>,
        scope_paths: Vec<String>,
        scope_packages: Vec<String>,
        scope_services: Vec<String>,
        relation_policy: ChangeImpactRelationPolicy,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        include_history: bool,
        max_dependents: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ChangeImpactResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_change_impact_with_policy(
                changed_symbols,
                changed_paths,
                scope_paths,
                scope_packages,
                scope_services,
                relation_policy,
                max_depth,
                min_confidence,
                include_tests,
                include_history,
                max_dependents,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_change_impact(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `plan.change` query.
    ///
    /// The objective class, explicit symbol and file targets, and step cap are
    /// validated by the query plan. The result carries deterministic source-free
    /// ordered steps, a compact impact summary, a verification test plan, open
    /// decisions, and a ready context-pack request.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    pub fn plan_change(
        &self,
        generation: GenerationId,
        objective: PlanChangeObjective,
        target_symbols: BTreeSet<SymbolId>,
        target_files: BTreeSet<FileId>,
        max_steps: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<PlanChangeResult>, FirstSliceError> {
        self.plan_change_with_budget(
            generation,
            objective,
            objective.as_str().to_owned(),
            target_symbols,
            target_files,
            max_steps,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `plan.change` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded change-plan dimensions"
    )]
    pub fn plan_change_with_budget(
        &self,
        generation: GenerationId,
        objective: PlanChangeObjective,
        objective_text: String,
        target_symbols: BTreeSet<SymbolId>,
        target_files: BTreeSet<FileId>,
        max_steps: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<PlanChangeResult>, FirstSliceError> {
        self.plan_change_with_context_and_budget(
            generation,
            objective,
            objective_text,
            target_symbols,
            target_files,
            BTreeSet::new(),
            Vec::new(),
            max_steps,
            budget,
            cancellation,
        )
    }

    /// Executes `plan.change` with bounded path context and caller constraints.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "change context and constraints remain explicit across the daemon boundary"
    )]
    pub fn plan_change_with_context_and_budget(
        &self,
        generation: GenerationId,
        objective: PlanChangeObjective,
        objective_text: String,
        target_symbols: BTreeSet<SymbolId>,
        target_files: BTreeSet<FileId>,
        target_paths: BTreeSet<String>,
        constraints: Vec<String>,
        max_steps: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<PlanChangeResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_plan_change_with_context(
                objective,
                objective_text,
                target_symbols,
                target_files,
                target_paths,
                constraints,
                max_steps,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_plan_change(&plan, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a bounded `history.compare` query between two retained
    /// generations.
    ///
    /// The head generation pins the query service while the base generation
    /// document is loaded from the generation set and supplied to the executor.
    /// The change-kind filter and result cap are validated by the query plan.
    /// The result carries deterministic semantic changes, an honest zero
    /// architecture delta, breaking candidates, and lineage matches.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown base or head generation,
    /// invalid plan, or bounded execution failure.
    pub fn history_compare(
        &self,
        base: GenerationId,
        head: GenerationId,
        change_kinds: BTreeSet<HistoryChangeKind>,
        max_results: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<HistoryCompareResult>, FirstSliceError> {
        self.history_compare_with_budget(
            base,
            head,
            change_kinds,
            max_results,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `history.compare` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    pub fn history_compare_with_budget(
        &self,
        base: GenerationId,
        head: GenerationId,
        change_kinds: BTreeSet<HistoryChangeKind>,
        max_results: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<HistoryCompareResult>, FirstSliceError> {
        self.history_compare_with_scope_and_budget(
            base,
            head,
            rootlight_query::HistoryCompareScope::default(),
            change_kinds,
            false,
            max_results,
            budget,
            cancellation,
        )
    }

    /// Executes `history.compare` with combined structural scope and context projection.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "scope and unchanged-context projection are independent comparison dimensions"
    )]
    pub fn history_compare_with_scope_and_budget(
        &self,
        base: GenerationId,
        head: GenerationId,
        scope: rootlight_query::HistoryCompareScope,
        change_kinds: BTreeSet<HistoryChangeKind>,
        include_unchanged_context: bool,
        max_results: usize,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<HistoryCompareResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(head)
            .map_err(|_| FirstSliceError::Query)?;
        let base_document = self
            .generations
            .generation(base)
            .map_err(|_| FirstSliceError::Query)?
            .document();
        let plan = service
            .plan_history_compare_with_scope(
                base,
                scope,
                change_kinds,
                include_unchanged_context,
                max_results,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_history_compare(&plan, base_document, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Executes a generation-pinned bounded `query.advanced` query.
    ///
    /// The safe typed AST is planned and validated against the resource
    /// ceilings and the optional client cost limit, then executed against the
    /// pinned generation. Execution serves an honest supported operator subset;
    /// unsupported patterns return non-empty columns with empty rows rather than
    /// fabricated data.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded advanced query dimension"
    )]
    pub fn advanced_query(
        &self,
        generation: GenerationId,
        ast: AdvancedAstNode,
        explain: bool,
        max_results: usize,
        page_offset: usize,
        max_depth: usize,
        max_traversal: usize,
        cost_limit: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<AdvancedQueryResult>, FirstSliceError> {
        self.advanced_query_with_budget(
            generation,
            ast,
            explain,
            max_results,
            page_offset,
            max_depth,
            max_traversal,
            cost_limit,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `query.advanced` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, invalid plan, or
    /// bounded execution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit policy accompanies the bounded advanced-query dimensions"
    )]
    pub fn advanced_query_with_budget(
        &self,
        generation: GenerationId,
        ast: AdvancedAstNode,
        explain: bool,
        max_results: usize,
        page_offset: usize,
        max_depth: usize,
        max_traversal: usize,
        cost_limit: Option<u64>,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<AdvancedQueryResult>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let service = self
            .generations
            .query(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let plan = service
            .plan_advanced_query(
                ast,
                explain,
                max_results,
                page_offset,
                max_depth,
                max_traversal,
                cost_limit,
                budget.query(),
            )
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_advanced_query(&plan, cancellation)
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
        self.source_read_with_budget(
            generation,
            references,
            FirstSliceBudget::default(),
            cancellation,
        )
    }

    /// Executes `source.read` under a reduced lower-layer policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, stale source,
    /// invalid plan, or bounded execution failure.
    pub fn source_read_with_budget(
        &self,
        generation: GenerationId,
        references: Vec<SourceRef>,
        budget: FirstSliceBudget,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SourceReadQueryResult>, FirstSliceError> {
        self.source_read_with_options_and_budget(
            generation,
            references,
            SourceReadOptions::new(),
            budget,
            cancellation,
        )
    }

    /// Executes `source.read` with explicit presentation controls and policy.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError`] for an unknown generation, stale source,
    /// invalid options, or bounded execution failure.
    pub fn source_read_with_options_and_budget(
        &self,
        generation: GenerationId,
        references: Vec<SourceRef>,
        options: SourceReadOptions,
        budget: FirstSliceBudget,
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
        let retained_snapshots = self
            .source_snapshots
            .snapshots(generation)
            .ok_or(FirstSliceError::Query)?;
        let requested_files = references
            .iter()
            .map(|source| source.span().file())
            .collect::<BTreeSet<_>>();
        let retained_files = retained_snapshots
            .iter()
            .map(|snapshot| snapshot.file())
            .collect::<BTreeSet<_>>();
        let missing_files = requested_files
            .difference(&retained_files)
            .copied()
            .collect::<Vec<_>>();
        let mut restored_snapshots = Vec::new();
        let source_snapshots = if missing_files.is_empty() {
            retained_snapshots
        } else {
            let durable = self.durable.as_ref().ok_or(FirstSliceError::Query)?;
            restored_snapshots
                .try_reserve_exact(
                    retained_snapshots
                        .len()
                        .checked_add(missing_files.len())
                        .ok_or(FirstSliceError::Retention)?,
                )
                .map_err(|_| FirstSliceError::Retention)?;
            restored_snapshots.extend(retained_snapshots.iter().cloned());
            for file in missing_files {
                check_cancellation(cancellation)?;
                let record = snapshot
                    .document()
                    .files
                    .iter()
                    .find(|candidate| candidate.id == file)
                    .ok_or(FirstSliceError::Query)?;
                restored_snapshots.push(Arc::new(durable.read_source(
                    record.repository,
                    generation,
                    record,
                    cancellation,
                )?));
            }
            restored_snapshots.sort_unstable_by_key(|snapshot| snapshot.file());
            restored_snapshots.as_slice()
        };
        debug_assert!(
            source_snapshots
                .windows(2)
                .all(|pair| pair[0].file() < pair[1].file()),
            "retained and lazily hydrated source snapshots remain canonical"
        );
        let source = SourceService::from_snapshots(source_snapshots, snapshot)
            .map_err(|error| map_source_error(error, cancellation))?;
        let plan = service
            .plan_source_read(references, options, budget.source(), budget.query())
            .map_err(|error| map_query_error(error, cancellation))?;
        service
            .execute_source_read(&plan, &source, cancellation)
            .map_err(|error| map_query_error(error, cancellation))
    }

    /// Returns the normalized language label for one retained source file.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Query`] when the generation or file is not
    /// retained, or [`FirstSliceError::CatalogCorrupt`] when its normalized
    /// provenance record is missing.
    pub fn source_language(
        &self,
        generation: GenerationId,
        file: FileId,
    ) -> Result<String, FirstSliceError> {
        self.source_file_coverage(generation, file)
            .map(|coverage| coverage.language)
    }

    /// Returns authoritative language and analysis tier for one retained file.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Query`] when the generation or file is not
    /// retained, or [`FirstSliceError::CatalogCorrupt`] when its normalized
    /// provenance record is missing.
    pub fn source_language_coverage(
        &self,
        generation: GenerationId,
        file: FileId,
    ) -> Result<(String, AnalysisTier), FirstSliceError> {
        self.source_file_coverage(generation, file)
            .map(|coverage| (coverage.language, coverage.tier))
    }

    /// Returns authoritative analysis coverage for one retained source file.
    ///
    /// The reason distinguishes an intentionally metadata-only unsupported
    /// language from an unexplained missing fact domain.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::Query`] when the generation or file is not
    /// retained, or [`FirstSliceError::CatalogCorrupt`] when its normalized
    /// provenance or coverage records are missing.
    pub fn source_file_coverage(
        &self,
        generation: GenerationId,
        file: FileId,
    ) -> Result<FirstSliceSourceCoverage, FirstSliceError> {
        let snapshot = self
            .generations
            .generation(generation)
            .map_err(|_| FirstSliceError::Query)?;
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|candidate| candidate.id == file)
            .ok_or(FirstSliceError::Query)?;
        let tier = document
            .provenance
            .iter()
            .find_map(|provenance| (provenance.id == file.provenance).then_some(provenance.tier))
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let status = document
            .coverage_records
            .iter()
            .filter_map(|coverage| {
                (coverage.scope == CoverageScope::File(file.id)).then_some(coverage.status)
            })
            .reduce(lower_coverage_status)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let reason = document
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.code == "unsupported-language"
                    && diagnostic
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .then_some(FirstSliceSourceCoverageReason::UnsupportedLanguage);
        Ok(FirstSliceSourceCoverage {
            language: file.language.clone(),
            tier,
            status,
            reason,
        })
    }

    /// Returns bounded source-free reasons that a generation is only partially covered.
    ///
    /// # Errors
    ///
    /// Returns the same generation-selection and retained-catalog failures as
    /// [`Self::resolve_generation`].
    pub fn coverage_gaps(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<Vec<FirstSliceCoverageGap>, FirstSliceError> {
        let context = self.resolve_generation(repository, Some(generation))?;
        let snapshot = self
            .generations
            .generation(context.generation)
            .map_err(|_| FirstSliceError::GenerationNotFound)?;
        let document = snapshot.document();
        let mut grouped =
            BTreeMap::<(FirstSliceCoverageGapReason, Option<String>), BTreeSet<FileId>>::new();
        let mut observe = |reason, file: Option<FileId>| {
            let language = file.and_then(|file| {
                document
                    .files
                    .binary_search_by_key(&file, |candidate| candidate.id)
                    .ok()
                    .and_then(|index| document.files.get(index))
                    .map(|file| file.language.clone())
            });
            if let Some(file) = file {
                grouped.entry((reason, language)).or_default().insert(file);
            }
        };
        for diagnostic in &document.diagnostics {
            let file = diagnostic
                .source
                .as_ref()
                .map(|source| source.span().file());
            let reason = match diagnostic.code.as_str() {
                "unsupported-language" => Some(
                    if file
                        .and_then(|file| {
                            document
                                .files
                                .binary_search_by_key(&file, |candidate| candidate.id)
                                .ok()
                                .and_then(|index| document.files.get(index))
                        })
                        .is_some_and(|file| file.language == "unknown")
                    {
                        FirstSliceCoverageGapReason::Unrecognized
                    } else {
                        FirstSliceCoverageGapReason::Unsupported
                    },
                ),
                code if code.starts_with("project-adapter-") && code.ends_with("-fallback") => {
                    Some(FirstSliceCoverageGapReason::AdapterFailed)
                }
                code if code.contains("truncated") || code == "resolution-work-bounded" => {
                    Some(FirstSliceCoverageGapReason::Truncated)
                }
                _ => None,
            };
            if let Some(reason) = reason {
                observe(reason, file);
            }
        }
        for skipped in &document.skipped_regions {
            let reason = match skipped.reason {
                SkippedRegionReason::ParseError => FirstSliceCoverageGapReason::ParseError,
                SkippedRegionReason::AdapterFailure => FirstSliceCoverageGapReason::AdapterFailed,
                SkippedRegionReason::ResourceLimit => FirstSliceCoverageGapReason::Truncated,
                SkippedRegionReason::UnsupportedConstruct
                    if skipped.detail != "unsupported-language" =>
                {
                    FirstSliceCoverageGapReason::Unsupported
                }
                _ => continue,
            };
            observe(reason, Some(skipped.source.span().file()));
        }
        for file in &document.files {
            if file.byte_length > SOURCE_FALLBACK_TEXT_BYTES as u64
                && document.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "unsupported-language"
                        && diagnostic
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
            {
                observe(FirstSliceCoverageGapReason::Truncated, Some(file.id));
            }
        }
        let receipt = &context.receipt;
        let excluded = receipt.policy_excluded_inputs;
        let mut gaps = grouped
            .into_iter()
            .map(|((reason, language), files)| FirstSliceCoverageGap {
                reason,
                language,
                files: u64::try_from(files.len()).unwrap_or(u64::MAX),
            })
            .collect::<Vec<_>>();
        for (reason, files) in [
            (FirstSliceCoverageGapReason::Excluded, excluded),
            (
                FirstSliceCoverageGapReason::Oversized,
                receipt.oversized_inputs,
            ),
            (FirstSliceCoverageGapReason::Binary, receipt.binary_inputs),
        ] {
            if files > 0 {
                gaps.push(FirstSliceCoverageGap {
                    reason,
                    language: None,
                    files,
                });
            }
        }
        let freshness = self.generation_freshness(repository, generation)?;
        if !matches!(
            freshness.structural,
            FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan
        ) || !matches!(
            freshness.semantic,
            FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan
        ) {
            gaps.push(FirstSliceCoverageGap {
                reason: FirstSliceCoverageGapReason::Stale,
                language: None,
                files: receipt.indexed_files.max(1),
            });
        }
        gaps.sort_by(|left, right| {
            (left.reason, left.language.as_deref()).cmp(&(right.reason, right.language.as_deref()))
        });
        gaps.truncate(64);
        Ok(gaps)
    }

    /// Lists every repository known to this daemon process.
    ///
    /// The result is deterministic because the underlying repository map is an
    /// ordered map keyed by repository identity. Each entry joins the active
    /// generation document for languages and freshness. A structural stage is
    /// ready and queryable while explicitly reporting pending semantic work.
    #[must_use]
    pub fn list_repositories(&self) -> Vec<RepositoryListEntryDto> {
        self.active_by_repository
            .iter()
            // A missing immutable snapshot indicates internal state drift; the
            // infallible compatibility API omits that invalid entry.
            .filter_map(|(repository, active_generation)| {
                self.generations.generation(*active_generation).ok()?;
                let freshness = self
                    .generation_freshness(*repository, *active_generation)
                    .ok()?;
                let languages = self
                    .language_coverage_by_generation
                    .get(active_generation)?
                    .iter()
                    .map(|coverage| coverage.language.clone())
                    .collect();
                Some(RepositoryListEntryDto {
                    repository: *repository,
                    active_generation: *active_generation,
                    languages,
                    structural_freshness: freshness_label(freshness.structural).to_owned(),
                    semantic_freshness: freshness_label(freshness.semantic).to_owned(),
                    state: REPOSITORY_STATE_READY.to_owned(),
                })
            })
            .collect()
    }

    /// Returns one page from an immutable, service-owned repository snapshot.
    ///
    /// A first-page request freezes authoritative filtered records. A
    /// continuation request reads only that retained snapshot, even if later
    /// registrations or publications mutate the live catalog.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for invalid bounds or continuation metadata,
    /// unsupported filters, unavailable snapshots, or inconsistent internal
    /// repository metadata.
    pub fn repository_catalog_page(
        &self,
        request: CatalogPageRequest,
        now: CatalogInstant,
    ) -> Result<CatalogPage, catalog::CatalogError> {
        let records = if request.snapshot_id().is_none() {
            self.catalog_records()?
        } else {
            Vec::new()
        };
        self.catalog_snapshots
            .lock()
            .map_err(|_| catalog::CatalogError::CatalogInvariant)?
            .page(request, records, now)
    }

    fn catalog_records(&self) -> Result<Vec<CatalogRepositoryRecord>, catalog::CatalogError> {
        let pending = self
            .pending_repository_registrations
            .lock()
            .map_err(|_| catalog::CatalogError::CatalogInvariant)?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(
                self.active_by_repository
                    .len()
                    .checked_add(pending.len())
                    .ok_or(catalog::CatalogError::SnapshotEntryBound)?,
            )
            .map_err(|_| catalog::CatalogError::SnapshotEntryBound)?;
        for (repository, active_generation) in &self.active_by_repository {
            let receipt = self
                .receipts
                .get(active_generation)
                .ok_or(catalog::CatalogError::CatalogInvariant)?;
            if receipt.repository != *repository {
                return Err(catalog::CatalogError::CatalogInvariant);
            }
            let display_name = self
                .repository_display_names
                .get(repository)
                .ok_or(catalog::CatalogError::CatalogInvariant)?
                .clone();
            let generation_count = self
                .published_generation_counts
                .get(repository)
                .copied()
                .ok_or(catalog::CatalogError::CatalogInvariant)?;
            self.generations
                .generation(*active_generation)
                .map_err(|_| catalog::CatalogError::CatalogInvariant)?;
            let freshness = self
                .generation_freshness(*repository, *active_generation)
                .map_err(|_| catalog::CatalogError::CatalogInvariant)?;
            let mut coverage = Vec::new();
            let summaries = self
                .language_coverage_by_generation
                .get(active_generation)
                .ok_or(catalog::CatalogError::CatalogInvariant)?;
            for summary in summaries {
                coverage.push(CatalogLanguageCoverage::new(
                    summary.language.clone(),
                    summary.tier,
                    summary.status,
                    summary.discovered_files,
                    summary.indexed_files,
                )?);
            }
            let record = CatalogRepositoryRecord::new(
                *repository,
                display_name,
                Some(*active_generation),
                generation_count,
                CatalogRepositoryState::Ready,
            )?
            .with_alias(self.repository_aliases.get(repository).cloned())?
            .with_root_path(self.repository_root_paths.get(repository).cloned())?
            .with_freshness(
                catalog_freshness(freshness.structural),
                catalog_freshness(freshness.semantic),
            )
            .with_coverage(coverage)?;
            records.push(record);
        }
        for (repository, display_name, root_path) in pending.values() {
            if self.active_by_repository.contains_key(repository) {
                continue;
            }
            records.push(
                CatalogRepositoryRecord::new(
                    *repository,
                    display_name.clone(),
                    None,
                    0,
                    CatalogRepositoryState::Indexing,
                )?
                .with_root_path(root_path.clone())?,
            );
        }
        Ok(records)
    }

    /// Changes the authoritative user-facing repository name.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::RepositoryNotFound`] for an unknown
    /// repository, [`FirstSliceError::Catalog`] for an invalid alias, or a
    /// durable catalog failure when the mutation cannot be persisted.
    pub fn rename_repository(
        &mut self,
        repository: RepositoryId,
        alias: String,
    ) -> Result<(), FirstSliceError> {
        catalog::validate_label(&alias).map_err(|_| FirstSliceError::Catalog)?;
        if !self.active_by_repository.contains_key(&repository) {
            return Err(FirstSliceError::RepositoryNotFound);
        }
        let sequence = self
            .repository_metadata_sequences
            .get(&repository)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(FirstSliceError::Retention)?;
        if let Some(durable) = &self.durable {
            durable.write_repository_metadata(DurableRepositoryMetadata {
                version: REPOSITORY_METADATA_VERSION,
                sequence,
                repository,
                root_path: self.repository_root_paths.get(&repository).cloned(),
                alias: Some(alias.clone()),
            })?;
        }
        self.repository_aliases.insert(repository, alias);
        self.repository_metadata_sequences
            .insert(repository, sequence);
        Ok(())
    }

    /// Deletes one repository's Rootlight-owned generations and catalog state.
    ///
    /// The source repository is never opened or mutated by this operation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::RepositoryNotFound`] for an unknown
    /// repository, or a durable/in-memory integrity failure when complete
    /// removal cannot be established.
    pub fn delete_repository(&mut self, repository: RepositoryId) -> Result<(), FirstSliceError> {
        if !self.active_by_repository.contains_key(&repository) {
            return Err(FirstSliceError::RepositoryNotFound);
        }
        let generations = self
            .receipts
            .values()
            .filter_map(|receipt| (receipt.repository == repository).then_some(receipt.generation))
            .collect::<BTreeSet<_>>();
        if generations.is_empty()
            || generations.iter().any(|generation| {
                !self.generations.contains(*generation)
                    || !self.source_snapshots.contains_committed(*generation)
                    || !self
                        .language_coverage_by_generation
                        .contains_key(generation)
            })
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let replacement_active = self
            .activation_order_by_generation
            .iter()
            .filter(|(generation, _)| !generations.contains(generation))
            .max_by_key(|(_, sequence)| **sequence)
            .map(|(generation, _)| *generation);
        if let Some(durable) = &self.durable {
            durable.remove_repository(repository)?;
        }
        for generation in &generations {
            self.source_snapshots.remove_committed(*generation)?;
            if self.structural_artifacts.contains_committed(*generation) {
                self.structural_artifacts.remove_committed(*generation)?;
            }
        }
        self.generations
            .remove_many(&generations, replacement_active)
            .map_err(|_| FirstSliceError::Retention)?;
        self.receipts
            .retain(|generation, _| !generations.contains(generation));
        self.language_coverage_by_generation
            .retain(|generation, _| !generations.contains(generation));
        self.incremental_baselines
            .retain(|generation, _| !generations.contains(generation));
        self.incremental_inputs
            .retain(|generation, _| !generations.contains(generation));
        self.incremental_evidence
            .retain(|generation, _| !generations.contains(generation));
        self.generation_memory_bytes
            .retain(|generation, _| !generations.contains(generation));
        self.activation_order_by_generation
            .retain(|generation, _| !generations.contains(generation));
        self.durable_operations
            .retain(|_, operation| !generations.contains(&operation.receipt.generation));
        self.active_by_repository.remove(&repository);
        self.repository_display_names.remove(&repository);
        self.repository_root_paths.remove(&repository);
        self.repository_aliases.remove(&repository);
        self.repository_metadata_sequences.remove(&repository);
        self.published_generation_counts.remove(&repository);
        self.activation_sequences.remove(&repository);
        self.pending_durable_compactions.remove(&repository);
        self.repositories
            .retain(|_, candidate| *candidate != repository);
        self.pending_repository_registrations
            .lock()
            .map_err(|_| FirstSliceError::Retention)?
            .retain(|_, (candidate, _, _)| *candidate != repository);
        self.most_recent_activation = replacement_active.and_then(|generation| {
            self.activation_order_by_generation
                .get(&generation)
                .copied()
                .map(|sequence| (sequence, generation))
        });
        Ok(())
    }

    /// Returns one repository's active or exact generation status.
    ///
    /// Selection, active identity, freshness, and coverage come from one
    /// resolution context, so an exact generation cannot be replaced by a
    /// later active generation.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceError::RepositoryNotFound`] when the repository is
    /// unknown, or the same generation errors as [`Self::resolve_generation`].
    pub fn repository_status(
        &self,
        repository: RepositoryId,
        generation: Option<GenerationId>,
    ) -> Result<RepositoryStatusDto, FirstSliceError> {
        if generation.is_none()
            && !self.active_by_repository.contains_key(&repository)
            && self
                .pending_repository_registrations
                .lock()
                .map_err(|_| FirstSliceError::Retention)?
                .values()
                .any(|(candidate, _, _)| *candidate == repository)
        {
            return Err(FirstSliceError::GenerationNotFound);
        }
        let context = self.resolve_generation(repository, generation)?;
        self.generations
            .generation(context.generation)
            .map_err(|_| FirstSliceError::GenerationNotFound)?;
        let coverage = self
            .language_coverage_by_generation
            .get(&context.generation)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let receipt = self
            .receipts
            .get(&context.generation)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let display_name = self
            .repository_display_names
            .get(&repository)
            .ok_or(FirstSliceError::RepositoryNotFound)?
            .clone();
        let active_freshness = self.generation_freshness(repository, context.active_generation)?;
        let freshness = self.generation_freshness(repository, context.generation)?;
        Ok(RepositoryStatusDto {
            repository: context.repository,
            display_name,
            alias: self.repository_aliases.get(&repository).cloned(),
            resolved_generation: context.generation,
            active_generation: context.active_generation,
            parent_generation: context.parent,
            active_parent_generation: context.active_parent,
            active_structural_freshness: freshness_label(active_freshness.structural).to_owned(),
            active_semantic_freshness: freshness_label(active_freshness.semantic).to_owned(),
            structural_freshness: freshness_label(freshness.structural).to_owned(),
            semantic_freshness: freshness_label(freshness.semantic).to_owned(),
            state: REPOSITORY_STATE_READY.to_owned(),
            publication_state: if context.active {
                "published".to_owned()
            } else {
                "retained".to_owned()
            },
            retained_durable_bytes: receipt.retained_durable_bytes,
            logical_snapshot: receipt.logical_snapshot.clone(),
            coverage: coverage_from_summaries(coverage),
        })
    }
}

impl std::fmt::Debug for FirstSliceService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FirstSliceService")
            .field("active_generation", &self.active_generation())
            .field("retained_generations", &self.receipts.len())
            .finish()
    }
}

/// Closed resource labels safe to expose in first-slice diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceResource {
    /// Repository registrations retained in the local catalog.
    Repositories,
    /// Supported source files selected for one generation.
    SourceFiles,
    /// Filesystem entries visited during deterministic discovery.
    DiscoveryEntries,
    /// Aggregate retained source bytes selected for one generation.
    SourceBytes,
    /// Bytes in one repository-relative path.
    PathBytes,
    /// Components in one repository-relative path.
    PathComponents,
    /// Process memory needed by one bounded repository operation.
    MemoryBytes,
    /// Operating-system resources needed to create an isolated process.
    ProcessResources,
    /// Files in one incremental metadata baseline or authoritative scan.
    IncrementalFiles,
    /// Typed fingerprints in one incremental generation snapshot.
    IncrementalInputs,
    /// Reusable artifacts in one incremental generation snapshot.
    IncrementalArtifacts,
    /// Fact nodes produced by one reusable incremental artifact.
    IncrementalArtifactOutputs,
    /// Declared incremental analysis passes.
    IncrementalPasses,
    /// Scoped fact nodes in the incremental dependency graph.
    IncrementalDependencyNodes,
    /// Typed edges in the incremental dependency graph.
    IncrementalDependencyEdges,
    /// Edge visits during incremental fixed-point closure.
    IncrementalClosureWork,
    /// Source-free incremental invalidation trace entries.
    IncrementalTraceEntries,
    /// Canonical bytes in one incremental logical-equivalence component.
    IncrementalLogicalBytes,
    /// Top-level normalized IR records.
    Records,
    /// Normalized file records.
    Files,
    /// Normalized entity records.
    Entities,
    /// Normalized occurrence records.
    Occurrences,
    /// Normalized relation records.
    Relations,
    /// Normalized provenance records.
    Provenance,
    /// Normalized source-mapping records.
    SourceMappings,
    /// Normalized coverage records.
    Coverage,
    /// Normalized skipped-region records.
    SkippedRegions,
    /// Normalized diagnostic records.
    Diagnostics,
    /// Normalized extension envelopes.
    Extensions,
    /// Aggregate extension payload bytes.
    ExtensionBytes,
    /// Aggregate nested fact references and bounded nested values.
    NestedItems,
    /// Logical rows visited during generation identity verification.
    GenerationRows,
    /// Distinct source references visited during generation verification.
    SourceReferences,
    /// Aggregate non-source text visited during generation verification.
    TextBytes,
    /// Aggregate UTF-8 bytes in the persisted catalog representation.
    EncodedTextBytes,
    /// Conservative aggregate memory required by generation finalization.
    GenerationMemoryBytes,
    /// Transactional adapter stream batches.
    Batches,
    /// Deterministically accounted adapter output bytes.
    OutputBytes,
    /// UTF-8 bytes in adapter diagnostics.
    DiagnosticBytes,
    /// UTF-8 bytes in adapter labels and other non-payload strings.
    StringBytes,
    /// Files admitted to one project-analysis transaction.
    ProjectFiles,
    /// Aggregate source bytes admitted to one project-analysis transaction.
    ProjectSourceBytes,
    /// Canonical project build-context bytes.
    ProjectContextBytes,
    /// Generated-origin mappings admitted to one project-analysis transaction.
    GeneratedMappings,
    /// Deterministically accounted generated-origin mapping bytes.
    GeneratedMappingBytes,
    /// Analysis-unit identity bytes.
    AnalysisUnitBytes,
    /// Build-target identity bytes.
    BuildTargetBytes,
    /// Embedded included source ranges.
    IncludedRanges,
    /// Concrete-syntax nodes processed for one source file.
    SyntaxNodes,
    /// Concrete-syntax nesting depth.
    SyntaxDepth,
    /// Adapter-reported in-process memory bytes.
    ReportedMemoryBytes,
    /// Resolver occurrence and same-name candidate inspections.
    ResolutionWork,
}

impl FirstSliceResource {
    /// Returns the stable source-free diagnostic label for this resource.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repositories => "repositories",
            Self::SourceFiles => "source_files",
            Self::DiscoveryEntries => "discovery_entries",
            Self::SourceBytes => "source_bytes",
            Self::PathBytes => "path_bytes",
            Self::PathComponents => "path_components",
            Self::MemoryBytes => "memory_bytes",
            Self::ProcessResources => "process_resources",
            Self::IncrementalFiles => "incremental_files",
            Self::IncrementalInputs => "incremental_inputs",
            Self::IncrementalArtifacts => "incremental_artifacts",
            Self::IncrementalArtifactOutputs => "incremental_artifact_outputs",
            Self::IncrementalPasses => "incremental_passes",
            Self::IncrementalDependencyNodes => "incremental_dependency_nodes",
            Self::IncrementalDependencyEdges => "incremental_dependency_edges",
            Self::IncrementalClosureWork => "incremental_closure_work",
            Self::IncrementalTraceEntries => "incremental_trace_entries",
            Self::IncrementalLogicalBytes => "incremental_logical_bytes",
            Self::Records => "records",
            Self::Files => "files",
            Self::Entities => "entities",
            Self::Occurrences => "occurrences",
            Self::Relations => "relations",
            Self::Provenance => "provenance",
            Self::SourceMappings => "source_mappings",
            Self::Coverage => "coverage",
            Self::SkippedRegions => "skipped_regions",
            Self::Diagnostics => "diagnostics",
            Self::Extensions => "extensions",
            Self::ExtensionBytes => "extension_bytes",
            Self::NestedItems => "nested_items",
            Self::GenerationRows => "generation_rows",
            Self::SourceReferences => "source_references",
            Self::TextBytes => "text_bytes",
            Self::EncodedTextBytes => "encoded_text_bytes",
            Self::GenerationMemoryBytes => "generation_memory_bytes",
            Self::Batches => "batches",
            Self::OutputBytes => "output_bytes",
            Self::DiagnosticBytes => "diagnostic_bytes",
            Self::StringBytes => "string_bytes",
            Self::ProjectFiles => "project_files",
            Self::ProjectSourceBytes => "project_source_bytes",
            Self::ProjectContextBytes => "project_context_bytes",
            Self::GeneratedMappings => "generated_mappings",
            Self::GeneratedMappingBytes => "generated_mapping_bytes",
            Self::AnalysisUnitBytes => "analysis_unit_bytes",
            Self::BuildTargetBytes => "build_target_bytes",
            Self::IncludedRanges => "included_ranges",
            Self::SyntaxNodes => "syntax_nodes",
            Self::SyntaxDepth => "syntax_depth",
            Self::ReportedMemoryBytes => "reported_memory_bytes",
            Self::ResolutionWork => "resolution_work",
        }
    }
}

/// Closed project languages safe to expose in semantic coverage diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceProjectCoverageLanguage {
    /// Rust project analysis.
    Rust,
    /// TypeScript project analysis.
    TypeScript,
    /// JavaScript project analysis.
    JavaScript,
    /// Python project analysis.
    Python,
    /// Go project analysis.
    Go,
    /// Java project analysis.
    Java,
    /// C++ project analysis.
    Cpp,
    /// C# project analysis.
    CSharp,
    /// PHP project analysis.
    Php,
    /// C project analysis.
    C,
}

impl FirstSliceProjectCoverageLanguage {
    const fn from_label(label: &str) -> Option<Self> {
        match label.as_bytes() {
            b"rust" => Some(Self::Rust),
            b"typescript" => Some(Self::TypeScript),
            b"javascript" => Some(Self::JavaScript),
            b"python" => Some(Self::Python),
            b"go" => Some(Self::Go),
            b"java" => Some(Self::Java),
            b"cpp" => Some(Self::Cpp),
            b"csharp" => Some(Self::CSharp),
            b"php" => Some(Self::Php),
            b"c" => Some(Self::C),
            _ => None,
        }
    }

    /// Returns the canonical source-free language label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Php => "php",
            Self::C => "c",
        }
    }
}

/// Closed reason why bounded project evidence cannot replace structural facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceProjectCoverageReason {
    /// Aggregate project facts were truncated before composition.
    FactsTruncated,
    /// Project output omitted structural declaration claims.
    StructuralDeclarationLoss,
}

impl FirstSliceProjectCoverageReason {
    /// Returns the stable source-free diagnostic label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FactsTruncated => "project_facts_truncated",
            Self::StructuralDeclarationLoss => "structural_declaration_loss",
        }
    }
}

/// Supported configuration that governs repository-registration capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceRepositoryCapacityConfiguration {
    /// Explicit repository-registration ceiling.
    MaximumRepositories,
    /// Per-repository generation retention determines the effective ceiling.
    RetainedGenerations,
}

impl FirstSliceRepositoryCapacityConfiguration {
    /// Returns the canonical source-free user configuration key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaximumRepositories => "storage.maximum_repositories",
            Self::RetainedGenerations => "storage.retained_generations",
        }
    }
}

/// Closed source-redacted component of an identity-verification failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceIdentityFailure {
    /// Snapshot construction rejected the normalized document.
    InvalidGeneration,
    /// The generation uses a contract without complete identity claims.
    LegacyContract,
    /// A required claim was absent or an unexpected claim was present.
    MissingClaim,
    /// More than one claim described the same stable identity.
    DuplicateClaim,
    /// A stable record ID differed from its canonical recipe.
    IdentityMismatch(IdentityMismatchComponent),
    /// Canonical manifest inputs differed from generation metadata.
    ManifestMismatch,
    /// An extension did not expose a verifiable shared identity recipe.
    UnsupportedExtension,
    /// A fixed typed identity recipe could not be encoded.
    RecipeEncoding,
}

impl FirstSliceIdentityFailure {
    /// Returns the stable public label used by diagnostics and support evidence.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidGeneration => "invalid_generation",
            Self::LegacyContract => "legacy_contract",
            Self::MissingClaim => "missing_claim",
            Self::DuplicateClaim => "duplicate_claim",
            Self::IdentityMismatch(component) => component.as_str(),
            Self::ManifestMismatch => "manifest_mismatch",
            Self::UnsupportedExtension => "unsupported_extension",
            Self::RecipeEncoding => "recipe_encoding",
        }
    }
}

/// Source-free component accounting for one failed generation-memory admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationMemoryBreakdown {
    /// Total memory retained by already admitted generations.
    pub retained_bytes: u64,
    /// Conservative capacity reserved before generation construction.
    pub reserved_bytes: u64,
    /// Retained memory backed by generation-owned allocations.
    pub owned_bytes: u64,
    /// Retained memory referenced without transferring ownership.
    pub referenced_bytes: u64,
    /// Retained memory backed by file mappings.
    pub mapped_bytes: u64,
    /// Memory held by the candidate generation awaiting admission.
    pub staged_bytes: u64,
    /// Retained memory charged to shared allocations.
    pub shared_bytes: u64,
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
    /// Incremental baseline or invalidation planning failed.
    #[error("first-slice incremental planning failed")]
    Incremental,
    /// Source changed between discovery and capability snapshot.
    #[error("first-slice discovery snapshot changed")]
    DiscoveryDrift,
    /// Parser or normalized adapter output failed.
    #[error("first-slice analysis failed")]
    Adapter,
    /// Bounded semantic output could not safely replace structural evidence.
    #[error("first-slice semantic refinement coverage is incomplete")]
    IncompleteCoverage,
    /// Bounded project evidence could not safely replace structural facts.
    #[error(
        "first-slice project coverage for {language:?} is incomplete: {reason:?}, missing {missing_declarations} declarations"
    )]
    ProjectCoverageIncomplete {
        /// Canonical project language.
        language: FirstSliceProjectCoverageLanguage,
        /// Closed coverage failure reason.
        reason: FirstSliceProjectCoverageReason,
        /// Exact source-free number of omitted structural declarations.
        missing_declarations: u64,
    },
    /// The isolated project adapter crossed its configured wall-time ceiling.
    #[error("first-slice project adapter wall-time limit was reached")]
    AdapterWallTimeLimit,
    /// The isolated project adapter crossed its configured input-volume ceiling.
    #[error("first-slice project adapter input limit was reached")]
    AdapterInputLimit,
    /// The isolated project adapter crossed its configured output-volume ceiling.
    #[error("first-slice project adapter output limit was reached")]
    AdapterOutputLimit,
    /// The isolated project adapter crossed its configured memory ceiling.
    #[error("first-slice project adapter memory limit was reached")]
    AdapterMemoryLimit,
    /// The isolated project adapter process exited without a valid result.
    #[error("first-slice project adapter process failed")]
    AdapterProcessFailure,
    /// Bounded semantic resolution failed.
    #[error("first-slice semantic resolution failed")]
    Resolution,
    /// Stable identity verification failed.
    #[error("first-slice identity verification failed")]
    Identity,
    /// Stable identity verification failed at a known canonical component.
    #[error("first-slice identity verification failed: {0:?}")]
    IdentityVerification(FirstSliceIdentityFailure),
    /// Normalized SQLite persistence or verification failed.
    #[error("first-slice oracle failed")]
    Catalog,
    /// A retained generation failed integrity validation.
    #[error("first-slice oracle is corrupt")]
    CatalogCorrupt,
    /// A retained generation uses an unsupported storage schema.
    #[error("first-slice oracle requires migration")]
    CatalogMigrationRequired,
    /// Lexical projection, construction, or validation failed.
    #[error("first-slice search failed")]
    Search,
    /// A bounded source read failed.
    #[error("first-slice source read failed")]
    Source,
    /// A query plan or execution failed.
    #[error("first-slice query failed")]
    Query,
    /// Portable generation export or verified read-only import failed.
    #[error("first-slice shared generation transfer failed")]
    Sharing,
    /// A caller-supplied runtime trace violated its bounded import contract.
    #[error(transparent)]
    RuntimeTrace(#[from] RuntimeTraceImportError),
    /// The requested stable symbol is absent from the pinned generation.
    #[error("first-slice symbol was not found")]
    SymbolNotFound,
    /// A query or source-read budget rejected planning or execution.
    #[error("first-slice execution budget was exceeded")]
    BudgetExceeded,
    /// The requested repository registration is unavailable.
    #[error("first-slice repository was not found")]
    RepositoryNotFound,
    /// The immutable generation is not retained by this daemon process.
    #[error("first-slice generation was not found")]
    GenerationNotFound,
    /// The immutable generation belongs to another repository.
    #[error("first-slice generation does not belong to the repository")]
    GenerationMismatch,
    /// Generation or source retention cannot admit more state.
    #[error("first-slice retention is exhausted")]
    Retention,
    /// Repository filesystem access failed in a way that may be transient.
    #[error("first-slice repository access is temporarily unavailable")]
    RepositoryIo,
    /// A bounded resource could not be reserved, but no numeric bound was available.
    #[error("first-slice resource {resource:?} is temporarily unavailable")]
    ResourceUnavailable {
        /// Closed source-free resource label.
        resource: FirstSliceResource,
    },
    /// A configured integer or duration is not representable.
    #[error("first-slice limits are invalid")]
    Limits,
    /// One explicit bounded resource crossed its configured ceiling.
    #[error("first-slice resource {resource:?} observed {observed} above limit {limit}")]
    ResourceLimit {
        /// Closed source-free resource label.
        resource: FirstSliceResource,
        /// Safe observed count or byte total.
        observed: u64,
        /// Configured count or byte ceiling.
        limit: u64,
    },
    /// Repository registration crossed its effective configured capacity.
    #[error(
        "repository registration observed {observed} above limit {limit} governed by {configuration:?}"
    )]
    RepositoryCapacityLimit {
        /// Safe attempted registration count.
        observed: u64,
        /// Effective repository-registration ceiling.
        limit: u64,
        /// Configuration key that currently governs the effective ceiling.
        configuration: FirstSliceRepositoryCapacityConfiguration,
        /// Registrations that have reserved capacity but are not yet committed.
        pending_repositories: u64,
        /// Committed registrations that the supported delete lifecycle can reclaim.
        reclaimable_repositories: u64,
    },
    /// One explicit bounded resource crossed a ceiling whose exact observation was discarded.
    #[error("first-slice resource {resource:?} estimated at least {estimated} above limit {limit}")]
    EstimatedResourceLimit {
        /// Closed source-free resource label.
        resource: FirstSliceResource,
        /// Safe lower-bound estimate for the rejected observation.
        estimated: u64,
        /// Configured count or byte ceiling.
        limit: u64,
    },
    /// Aggregate generation memory cannot admit the pending generation.
    #[error("generation memory observed {observed} bytes above limit {limit} bytes")]
    GenerationMemoryLimit {
        /// Source-free component accounting for the failed admission.
        breakdown: GenerationMemoryBreakdown,
        /// Retained plus pending bytes counted once for admission.
        observed: u64,
        /// Configured generation-memory ceiling.
        limit: u64,
    },
    /// Durable staging cannot preserve its required free-space margin.
    #[error(
        "insufficient disk space for first-slice publication: required {required_bytes} bytes, available {available_bytes} bytes"
    )]
    InsufficientDiskSpace {
        /// Staging reservation including its safety margin.
        required_bytes: u64,
        /// Free bytes observed on the durable state filesystem.
        available_bytes: u64,
    },
    /// Durable publication crossed one configured physical-storage boundary.
    #[error(
        "durable storage {scope:?} requires {required_bytes} bytes with {observed_bytes} observed against {limit_bytes}, preserving {minimum_free_bytes} free bytes"
    )]
    StorageResourceExhausted {
        /// Closed physical-storage boundary that rejected admission.
        scope: FirstSliceStorageScope,
        /// Additional physical bytes required by the candidate publication.
        required_bytes: u64,
        /// Physical or available bytes observed at admission.
        observed_bytes: u64,
        /// Effective ceiling for the selected scope.
        limit_bytes: u64,
        /// Configured filesystem free-space floor.
        minimum_free_bytes: u64,
    },
}

/// Source-free configured boundary used by durable storage admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirstSliceStorageScope {
    /// Per-repository physical-byte ceiling.
    Repository,
    /// Whole-catalog physical-byte ceiling.
    Catalog,
    /// Filesystem free-space floor.
    Filesystem,
}

struct FirstSliceIncrementalPlanningContext<'a> {
    repository: RepositoryId,
    has_parent: bool,
    reuse_policy: FirstSliceReusePolicy,
    parent: Option<&'a InputSnapshot>,
    parent_artifacts: Option<&'a StructuralGenerationArtifacts>,
    discovery: &'a IncrementalDiscovery,
    source_files: &'a BTreeSet<FileId>,
    semantic_inputs: &'a [InputFingerprint],
}

fn prepare_incremental_state(
    context: FirstSliceIncrementalPlanningContext<'_>,
    cancellation: &Cancellation,
) -> Result<PreparedIncrementalPlan, FirstSliceError> {
    let FirstSliceIncrementalPlanningContext {
        repository,
        has_parent,
        reuse_policy,
        parent,
        parent_artifacts,
        discovery,
        source_files,
        semantic_inputs,
    } = context;
    check_cancellation(cancellation)?;
    let parent_inputs = match parent {
        Some(parent) => parent.clone(),
        None => InputSnapshot::new([], PlanningLimits::default(), cancellation)
            .map_err(|error| map_incremental_error(error, cancellation))?,
    };
    let current_inputs =
        first_slice_input_snapshot(discovery, source_files, semantic_inputs, cancellation)?;
    let input_keys = incremental_input_keys(&parent_inputs, &current_inputs, cancellation)?;
    let files = incremental_file_ids(&input_keys, cancellation)?;
    let passes = first_slice_passes()?;
    let (nodes, edges) =
        first_slice_dependency_parts(repository, &files, &input_keys, &passes, cancellation)?;
    let graph_limits = GraphLimits::new(5, nodes.len().max(1), edges.len().max(1))
        .map_err(|error| map_incremental_error(error, cancellation))?;
    let registry = DependencyRegistry::new(passes.declarations()?, graph_limits, cancellation)
        .map_err(|error| map_incremental_error(error, cancellation))?;
    verify_first_slice_observations(&registry, &passes)
        .map_err(|error| map_incremental_error(error, cancellation))?;
    let graph = DependencyGraph::new(nodes, edges, &registry, graph_limits, cancellation)
        .map_err(|error| map_incremental_error(error, cancellation))?;
    let artifact_count = parent_artifacts.map_or(0, StructuralGenerationArtifacts::len);
    let planning_limits = incremental_planning_limits(
        input_keys.len(),
        artifact_count,
        graph.nodes().count(),
        graph.edges().count(),
    )?;
    let artifact_summaries = parent_artifact_summaries(
        &parent_inputs,
        parent_artifacts,
        planning_limits,
        cancellation,
    )?;
    let parent_summary = GenerationSummary::new(
        parent_inputs,
        artifact_summaries,
        planning_limits,
        cancellation,
    )
    .map_err(|error| map_incremental_error(error, cancellation))?;
    let plan = plan_invalidation(
        &parent_summary,
        &current_inputs,
        &graph,
        planning_limits,
        cancellation,
    )
    .map_err(|error| map_incremental_error(error, cancellation))?;
    let evidence =
        summarize_incremental_evidence(has_parent, reuse_policy, discovery, source_files, &plan)?;
    let reusable_parser_artifacts = if reuse_policy == FirstSliceReusePolicy::CleanRebuild {
        BTreeSet::new()
    } else {
        plan.artifact_decisions()
            .iter()
            .filter(|decision| decision.kind() == ArtifactDecisionKind::Reuse)
            .map(|decision| decision.artifact())
            .collect()
    };
    Ok(PreparedIncrementalPlan {
        state: PreparedIncrementalState {
            baseline: discovery.baseline().clone(),
            inputs: current_inputs,
            evidence,
        },
        reusable_parser_artifacts,
    })
}

fn incremental_planning_limits(
    input_count: usize,
    artifact_count: usize,
    node_count: usize,
    edge_count: usize,
) -> Result<PlanningLimits, FirstSliceError> {
    let max_trace_entries = input_count
        .checked_add(node_count)
        .and_then(|entries| entries.checked_add(artifact_count))
        .and_then(|entries| entries.checked_add(1))
        .ok_or(FirstSliceError::Limits)?;
    PlanningLimits::new(
        input_count.max(1),
        artifact_count.max(1),
        node_count.max(1),
        edge_count.max(1),
        max_trace_entries.max(1),
    )
    .map_err(|_| FirstSliceError::Limits)
}

struct FirstSlicePasses {
    parser: PassId,
    lowering: PassId,
    resolver: PassId,
    derived: PassId,
    search: PassId,
}

impl FirstSlicePasses {
    fn declarations(&self) -> Result<Vec<PassDeclaration>, FirstSliceError> {
        Ok(vec![
            PassDeclaration::new(
                self.parser.clone(),
                [
                    InputKind::FileContent,
                    InputKind::FilePath,
                    InputKind::GrammarVersion,
                    InputKind::AdapterVersion,
                    InputKind::ConfigurationRevision,
                ],
                FactDomainSet::default(),
                FactDomainSet::new([FactDomain::Syntax]),
            )
            .map_err(|_| FirstSliceError::Incremental)?,
            PassDeclaration::new(
                self.lowering.clone(),
                [
                    InputKind::FileContent,
                    InputKind::FilePath,
                    InputKind::AdapterVersion,
                    InputKind::CompilerOptions,
                    InputKind::ConfigurationRevision,
                ],
                FactDomainSet::new([FactDomain::Syntax]),
                local_semantic_domains(),
            )
            .map_err(|_| FirstSliceError::Incremental)?,
            PassDeclaration::new(
                self.resolver.clone(),
                [
                    InputKind::PublicSurface,
                    InputKind::BodySummary,
                    InputKind::ImportSet,
                    InputKind::BuildTarget,
                    InputKind::CompilerOptions,
                    InputKind::DependencyVersion,
                    InputKind::ResolverVersion,
                    InputKind::ConfigurationRevision,
                ],
                local_semantic_domains(),
                FactDomainSet::new([FactDomain::Resolution]),
            )
            .map_err(|_| FirstSliceError::Incremental)?,
            PassDeclaration::new(
                self.derived.clone(),
                [InputKind::DerivedPlan, InputKind::ConfigurationRevision],
                FactDomainSet::new([FactDomain::Resolution]),
                FactDomainSet::new([FactDomain::DerivedGraph]),
            )
            .map_err(|_| FirstSliceError::Incremental)?,
            PassDeclaration::new(
                self.search.clone(),
                [
                    InputKind::PublicSurface,
                    InputKind::BodySummary,
                    InputKind::SearchRevision,
                    InputKind::ConfigurationRevision,
                ],
                FactDomainSet::new([
                    FactDomain::PublicSurface,
                    FactDomain::Body,
                    FactDomain::Resolution,
                ]),
                FactDomainSet::new([FactDomain::Search]),
            )
            .map_err(|_| FirstSliceError::Incremental)?,
        ])
    }
}

fn first_slice_passes() -> Result<FirstSlicePasses, FirstSliceError> {
    Ok(FirstSlicePasses {
        parser: PassId::parse(PARSER_PASS_ID).map_err(|_| FirstSliceError::Incremental)?,
        lowering: PassId::parse(LOWERING_PASS_ID).map_err(|_| FirstSliceError::Incremental)?,
        resolver: PassId::parse(RESOLVER_PASS_ID).map_err(|_| FirstSliceError::Incremental)?,
        derived: PassId::parse(DERIVED_PASS_ID).map_err(|_| FirstSliceError::Incremental)?,
        search: PassId::parse(SEARCH_PASS_ID).map_err(|_| FirstSliceError::Incremental)?,
    })
}

fn local_semantic_domains() -> FactDomainSet {
    FactDomainSet::new([
        FactDomain::PublicSurface,
        FactDomain::Body,
        FactDomain::Tests,
        FactDomain::Services,
    ])
}

fn verify_first_slice_observations(
    registry: &DependencyRegistry,
    passes: &FirstSlicePasses,
) -> Result<(), IncrementalError> {
    let observations = [
        (
            &passes.parser,
            PassObservation::new(
                [
                    InputKind::FileContent,
                    InputKind::FilePath,
                    InputKind::GrammarVersion,
                    InputKind::AdapterVersion,
                    InputKind::ConfigurationRevision,
                ],
                FactDomainSet::default(),
                FactDomainSet::new([FactDomain::Syntax]),
            ),
        ),
        (
            &passes.lowering,
            PassObservation::new(
                [
                    InputKind::FileContent,
                    InputKind::FilePath,
                    InputKind::AdapterVersion,
                    InputKind::CompilerOptions,
                    InputKind::ConfigurationRevision,
                ],
                FactDomainSet::new([FactDomain::Syntax]),
                local_semantic_domains(),
            ),
        ),
        (
            &passes.resolver,
            PassObservation::new(
                [
                    InputKind::PublicSurface,
                    InputKind::BodySummary,
                    InputKind::ImportSet,
                    InputKind::BuildTarget,
                    InputKind::CompilerOptions,
                    InputKind::DependencyVersion,
                    InputKind::ResolverVersion,
                    InputKind::ConfigurationRevision,
                ],
                local_semantic_domains(),
                FactDomainSet::new([FactDomain::Resolution]),
            ),
        ),
        (
            &passes.derived,
            PassObservation::new(
                [InputKind::DerivedPlan, InputKind::ConfigurationRevision],
                FactDomainSet::new([FactDomain::Resolution]),
                FactDomainSet::new([FactDomain::DerivedGraph]),
            ),
        ),
        (
            &passes.search,
            PassObservation::new(
                [
                    InputKind::PublicSurface,
                    InputKind::BodySummary,
                    InputKind::SearchRevision,
                    InputKind::ConfigurationRevision,
                ],
                FactDomainSet::new([
                    FactDomain::PublicSurface,
                    FactDomain::Body,
                    FactDomain::Resolution,
                ]),
                FactDomainSet::new([FactDomain::Search]),
            ),
        ),
    ];
    for (pass, observation) in observations {
        registry.verify_observation(pass, &observation)?;
    }
    Ok(())
}

fn first_slice_input_snapshot(
    discovery: &IncrementalDiscovery,
    source_files: &BTreeSet<FileId>,
    semantic_inputs: &[InputFingerprint],
    cancellation: &Cancellation,
) -> Result<InputSnapshot, FirstSliceError> {
    let mut inputs = Vec::new();
    let expected = source_files
        .len()
        .checked_mul(2)
        .and_then(|count| count.checked_add(semantic_inputs.len()))
        .and_then(|count| count.checked_add(7))
        .ok_or(FirstSliceError::Limits)?;
    inputs
        .try_reserve_exact(expected)
        .map_err(|_| FirstSliceError::Limits)?;
    let mut configuration_present = false;
    let mut adapter_present = false;
    for input in discovery.baseline().inputs().iter() {
        check_cancellation(cancellation)?;
        let include = match input.key() {
            InputKey::FileContent(file) | InputKey::FilePath(file) => source_files.contains(&file),
            InputKey::ConfigurationRevision => {
                configuration_present = true;
                true
            }
            InputKey::AdapterVersion(_) => {
                adapter_present = true;
                true
            }
            _ => false,
        };
        if include {
            inputs.push(input);
        }
    }
    if !configuration_present || !adapter_present {
        return Err(FirstSliceError::Incremental);
    }

    inputs.extend([
        InputFingerprint::new(
            InputKey::GrammarVersion(
                derive_fact("rootlight.first-slice.grammar-input", GRAMMAR_REVISION_SEED).id(),
            ),
            first_slice_grammar_revision_hash()?,
        ),
        InputFingerprint::new(
            InputKey::CompilerOptions(
                derive_fact(
                    "rootlight.first-slice.compiler-input",
                    COMPILER_CONTEXT_INPUT_SEED,
                )
                .id(),
            ),
            first_slice_build_context(),
        ),
        InputFingerprint::new(
            InputKey::ResolverVersion,
            content_hash(RESOLVER_BINARY_SEED),
        ),
        InputFingerprint::new(InputKey::SearchRevision, content_hash(SEARCH_REVISION_SEED)),
        InputFingerprint::new(
            InputKey::DerivedPlan(
                derive_fact(
                    "rootlight.first-slice.derived-plan-input",
                    DERIVED_PLAN_REVISION_SEED,
                )
                .id(),
            ),
            first_slice_incremental_plan_hash()?,
        ),
    ]);
    inputs.extend_from_slice(semantic_inputs);
    let snapshot = InputSnapshot::new(inputs, PlanningLimits::default(), cancellation)
        .map_err(|error| map_incremental_error(error, cancellation))?;
    for file in source_files {
        check_cancellation(cancellation)?;
        if snapshot.value(InputKey::FileContent(*file)).is_none()
            || snapshot.value(InputKey::FilePath(*file)).is_none()
        {
            return Err(FirstSliceError::Incremental);
        }
    }
    Ok(snapshot)
}

struct SemanticFingerprintState {
    public_surface: Vec<ContentHash>,
    body: Vec<ContentHash>,
    imports: Vec<ContentHash>,
}

impl SemanticFingerprintState {
    fn new() -> Self {
        Self {
            public_surface: Vec::new(),
            body: Vec::new(),
            imports: Vec::new(),
        }
    }
}

struct SemanticHashWriter<'a>(&'a mut blake3::Hasher);

impl std::io::Write for SemanticHashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn semantic_fingerprint_hasher(domain: &[u8]) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.first-slice.semantic-fingerprint/1\0");
    hasher.update(domain);
    hasher
}

fn semantic_value_fingerprint(
    label: &[u8],
    value: &impl Serialize,
) -> Result<ContentHash, FirstSliceError> {
    let mut hasher = semantic_fingerprint_hasher(b"value");
    let length = u64::try_from(label.len()).map_err(|_| FirstSliceError::Limits)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(label);
    serde_json::to_writer(SemanticHashWriter(&mut hasher), value)
        .map_err(|_| FirstSliceError::Limits)?;
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn aggregate_semantic_fingerprint(
    domain: &[u8],
    mut values: Vec<ContentHash>,
) -> Result<ContentHash, FirstSliceError> {
    values.sort_unstable();
    let mut hasher = semantic_fingerprint_hasher(domain);
    let count = u64::try_from(values.len()).map_err(|_| FirstSliceError::Limits)?;
    hasher.update(&count.to_be_bytes());
    for value in values {
        hasher.update(value.as_bytes());
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn evidence_file(evidence: &FactEvidence) -> Option<FileId> {
    evidence
        .source
        .as_ref()
        .map(|source| source.span().file())
        .or_else(|| {
            evidence.derivation.iter().find_map(|fact| match fact {
                FactRef::File(file) => Some(*file),
                FactRef::Entity(_) | FactRef::Fact(_) => None,
            })
        })
}

fn relation_has_stable_endpoints(subject: RelationEndpoint, object: RelationEndpoint) -> bool {
    !matches!(subject, RelationEndpoint::Occurrence(_))
        && !matches!(object, RelationEndpoint::Occurrence(_))
}

fn first_slice_semantic_inputs(
    document: &NormalizedIrDocument,
    source_files: &BTreeSet<FileId>,
    cancellation: &Cancellation,
) -> Result<Vec<InputFingerprint>, FirstSliceError> {
    let mut by_file = source_files
        .iter()
        .copied()
        .map(|file| (file, SemanticFingerprintState::new()))
        .collect::<BTreeMap<_, _>>();

    for entity in &document.entities {
        check_cancellation(cancellation)?;
        let Some(file) = evidence_file(&entity.evidence) else {
            continue;
        };
        let Some(state) = by_file.get_mut(&file) else {
            continue;
        };
        state.public_surface.push(semantic_value_fingerprint(
            b"entity",
            &(
                entity.id,
                entity.kind,
                &entity.language,
                entity.tier,
                &entity.canonical_name,
                &entity.display_name,
                &entity.qualified_name,
                entity.container,
                entity.visibility,
                &entity.flags,
            ),
        )?);
    }

    for occurrence in &document.occurrences {
        check_cancellation(cancellation)?;
        let Some(state) = by_file.get_mut(&occurrence.file) else {
            continue;
        };
        let value = (
            occurrence.role,
            occurrence.enclosing,
            &occurrence.target,
            occurrence.syntactic_text_hash,
            &occurrence.syntax_kind,
            occurrence.confidence,
        );
        if occurrence.role == OccurrenceRole::ImportUse {
            state
                .imports
                .push(semantic_value_fingerprint(b"occurrence", &value)?);
        } else if occurrence.role == OccurrenceRole::Documentation {
            continue;
        } else {
            state
                .body
                .push(semantic_value_fingerprint(b"occurrence", &value)?);
        }
    }

    for relation in &document.relations {
        check_cancellation(cancellation)?;
        if !relation_has_stable_endpoints(relation.subject, relation.object) {
            continue;
        }
        let Some(file) = evidence_file(&relation.evidence) else {
            continue;
        };
        let Some(state) = by_file.get_mut(&file) else {
            continue;
        };
        let value = (
            relation.subject,
            relation.predicate,
            relation.object,
            relation.confidence,
            relation.evidence_kind,
        );
        match relation.predicate {
            RelationPredicate::Imports => {
                state
                    .imports
                    .push(semantic_value_fingerprint(b"relation", &value)?);
            }
            RelationPredicate::Exports => {
                state
                    .public_surface
                    .push(semantic_value_fingerprint(b"relation", &value)?);
            }
            RelationPredicate::Contains
            | RelationPredicate::Declares
            | RelationPredicate::DefinesAt => {}
            _ => {
                state
                    .body
                    .push(semantic_value_fingerprint(b"relation", &value)?);
            }
        }
    }

    let expected = by_file
        .len()
        .checked_mul(3)
        .ok_or(FirstSliceError::Limits)?;
    let mut inputs = Vec::new();
    inputs
        .try_reserve_exact(expected)
        .map_err(|_| FirstSliceError::Limits)?;
    for (file, state) in by_file {
        check_cancellation(cancellation)?;
        let unit = file_analysis_unit(file);
        inputs.extend([
            InputFingerprint::new(
                InputKey::PublicSurface(unit),
                aggregate_semantic_fingerprint(b"public-surface", state.public_surface)?,
            ),
            InputFingerprint::new(
                InputKey::BodySummary(unit),
                aggregate_semantic_fingerprint(b"body", state.body)?,
            ),
            InputFingerprint::new(
                InputKey::ImportSet(unit),
                aggregate_semantic_fingerprint(b"imports", state.imports)?,
            ),
        ]);
    }
    Ok(inputs)
}

fn incremental_input_keys(
    parent: &InputSnapshot,
    current: &InputSnapshot,
    cancellation: &Cancellation,
) -> Result<BTreeSet<InputKey>, FirstSliceError> {
    let mut keys = BTreeSet::new();
    for input in parent.iter().chain(current.iter()) {
        check_cancellation(cancellation)?;
        keys.insert(input.key());
    }
    Ok(keys)
}

fn incremental_file_ids(
    inputs: &BTreeSet<InputKey>,
    cancellation: &Cancellation,
) -> Result<BTreeSet<FileId>, FirstSliceError> {
    let mut files = BTreeSet::new();
    for input in inputs {
        check_cancellation(cancellation)?;
        match input {
            InputKey::FileContent(file) | InputKey::FilePath(file) => {
                files.insert(*file);
            }
            _ => {}
        }
    }
    Ok(files)
}

fn first_slice_dependency_parts(
    repository: RepositoryId,
    files: &BTreeSet<FileId>,
    inputs: &BTreeSet<InputKey>,
    passes: &FirstSlicePasses,
    cancellation: &Cancellation,
) -> Result<(Vec<FactNode>, Vec<DependencyEdge>), FirstSliceError> {
    let repository_unit =
        AnalysisUnitId::new(derive_fact(INCREMENTAL_UNIT_SEED, repository.as_bytes()).id());
    let resolution = FactNode::new(repository_unit, FactDomain::Resolution);
    let derived = FactNode::new(repository_unit, FactDomain::DerivedGraph);
    let search = FactNode::new(repository_unit, FactDomain::Search);
    let mut nodes = vec![resolution, derived, search];
    let mut edges = Vec::new();
    for file in files {
        check_cancellation(cancellation)?;
        let unit = file_analysis_unit(*file);
        let syntax = FactNode::new(unit, FactDomain::Syntax);
        nodes.push(syntax);
        for domain in local_semantic_domains().iter() {
            let target = FactNode::new(unit, domain);
            nodes.push(target);
            edges.push(DependencyEdge::new(
                DependencySource::Fact(syntax),
                target,
                passes.lowering.clone(),
            ));
        }
    }
    edges.push(DependencyEdge::new(
        DependencySource::Fact(resolution),
        derived,
        passes.derived.clone(),
    ));
    edges.push(DependencyEdge::new(
        DependencySource::Fact(resolution),
        search,
        passes.search.clone(),
    ));

    for input in inputs {
        check_cancellation(cancellation)?;
        match *input {
            InputKey::FileContent(file) | InputKey::FilePath(file) => {
                add_file_input_edges(*input, file, passes, &mut edges);
            }
            InputKey::GrammarVersion(_) | InputKey::AdapterVersion(_) => {
                add_all_file_parser_edges(*input, files, passes, &mut edges);
                if matches!(input, InputKey::AdapterVersion(_)) {
                    add_all_file_lowering_edges(*input, files, passes, &mut edges);
                }
            }
            InputKey::CompilerOptions(_) => {
                add_all_file_lowering_edges(*input, files, passes, &mut edges);
                edges.push(DependencyEdge::new(
                    DependencySource::Input(*input),
                    resolution,
                    passes.resolver.clone(),
                ));
            }
            InputKey::ConfigurationRevision => {
                add_all_file_parser_edges(*input, files, passes, &mut edges);
                add_all_file_lowering_edges(*input, files, passes, &mut edges);
                for (target, pass) in [
                    (resolution, passes.resolver.clone()),
                    (derived, passes.derived.clone()),
                    (search, passes.search.clone()),
                ] {
                    edges.push(DependencyEdge::new(
                        DependencySource::Input(*input),
                        target,
                        pass,
                    ));
                }
            }
            InputKey::PublicSurface(_) | InputKey::BodySummary(_) => {
                edges.push(DependencyEdge::new(
                    DependencySource::Input(*input),
                    resolution,
                    passes.resolver.clone(),
                ));
                edges.push(DependencyEdge::new(
                    DependencySource::Input(*input),
                    search,
                    passes.search.clone(),
                ));
            }
            InputKey::ImportSet(_)
            | InputKey::BuildTarget(_)
            | InputKey::DependencyVersion(_)
            | InputKey::ResolverVersion => {
                edges.push(DependencyEdge::new(
                    DependencySource::Input(*input),
                    resolution,
                    passes.resolver.clone(),
                ));
            }
            InputKey::SearchRevision => {
                edges.push(DependencyEdge::new(
                    DependencySource::Input(*input),
                    search,
                    passes.search.clone(),
                ));
            }
            InputKey::DerivedPlan(_) => {
                edges.push(DependencyEdge::new(
                    DependencySource::Input(*input),
                    derived,
                    passes.derived.clone(),
                ));
            }
        }
    }
    Ok((nodes, edges))
}

fn add_file_input_edges(
    input: InputKey,
    file: FileId,
    passes: &FirstSlicePasses,
    edges: &mut Vec<DependencyEdge>,
) {
    let unit = file_analysis_unit(file);
    edges.push(DependencyEdge::new(
        DependencySource::Input(input),
        FactNode::new(unit, FactDomain::Syntax),
        passes.parser.clone(),
    ));
    for domain in local_semantic_domains().iter() {
        edges.push(DependencyEdge::new(
            DependencySource::Input(input),
            FactNode::new(unit, domain),
            passes.lowering.clone(),
        ));
    }
}

fn add_all_file_parser_edges(
    input: InputKey,
    files: &BTreeSet<FileId>,
    passes: &FirstSlicePasses,
    edges: &mut Vec<DependencyEdge>,
) {
    for file in files {
        edges.push(DependencyEdge::new(
            DependencySource::Input(input),
            FactNode::new(file_analysis_unit(*file), FactDomain::Syntax),
            passes.parser.clone(),
        ));
    }
}

fn add_all_file_lowering_edges(
    input: InputKey,
    files: &BTreeSet<FileId>,
    passes: &FirstSlicePasses,
    edges: &mut Vec<DependencyEdge>,
) {
    for file in files {
        let unit = file_analysis_unit(*file);
        for domain in local_semantic_domains().iter() {
            edges.push(DependencyEdge::new(
                DependencySource::Input(input),
                FactNode::new(unit, domain),
                passes.lowering.clone(),
            ));
        }
    }
}

fn parent_artifact_summaries(
    inputs: &InputSnapshot,
    artifacts: Option<&StructuralGenerationArtifacts>,
    limits: PlanningLimits,
    cancellation: &Cancellation,
) -> Result<Vec<ArtifactSummary>, FirstSliceError> {
    let Some(artifacts) = artifacts else {
        return Ok(Vec::new());
    };
    let mut summaries = Vec::new();
    summaries
        .try_reserve_exact(artifacts.len())
        .map_err(|_| FirstSliceError::Retention)?;
    for (file, entry) in artifacts.iter() {
        check_cancellation(cancellation)?;
        let dependencies: Vec<_> = inputs
            .iter()
            .filter(|input| parser_artifact_depends_on(input.key(), *file))
            .collect();
        if !dependencies
            .iter()
            .any(|input| input.key() == InputKey::FileContent(*file))
            || !dependencies
                .iter()
                .any(|input| input.key() == InputKey::FilePath(*file))
        {
            return Err(FirstSliceError::Incremental);
        }
        summaries.push(
            ArtifactSummary::new(
                entry.id,
                [FactNode::new(file_analysis_unit(*file), FactDomain::Syntax)],
                dependencies,
                limits,
                cancellation,
            )
            .map_err(|error| map_incremental_error(error, cancellation))?,
        );
    }
    Ok(summaries)
}

fn parser_artifact_depends_on(key: InputKey, file: FileId) -> bool {
    match key {
        InputKey::FileContent(candidate) | InputKey::FilePath(candidate) => candidate == file,
        InputKey::GrammarVersion(_)
        | InputKey::AdapterVersion(_)
        | InputKey::ConfigurationRevision => true,
        _ => false,
    }
}

fn file_analysis_unit(file: FileId) -> AnalysisUnitId {
    AnalysisUnitId::new(derive_fact(INCREMENTAL_FILE_UNIT_SEED, file.as_bytes()).id())
}

fn parser_artifact_id(file: FileId) -> ArtifactId {
    ArtifactId::new(derive_fact(PARSER_ARTIFACT_SEED, file.as_bytes()).id())
}

fn first_slice_parser_provider_hash() -> Result<ContentHash, FirstSliceError> {
    let grammar_revision = first_slice_grammar_revision_hash()?;
    hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        ANALYZER_BINARY_SEED,
        grammar_revision.as_bytes(),
    ])
}

fn first_slice_grammar_revision_hash() -> Result<ContentHash, FirstSliceError> {
    let registry = GrammarRegistry::audited().map_err(|_| FirstSliceError::Adapter)?;
    let mut hasher = blake3::Hasher::new();
    hash_component(&mut hasher, GRAMMAR_REVISION_SEED)?;
    hash_component(&mut hasher, TREE_SITTER_RUNTIME_VERSION.as_bytes())?;
    for descriptor in registry.descriptors() {
        hash_component(&mut hasher, descriptor.language().as_str().as_bytes())?;
        hash_component(&mut hasher, descriptor.grammar_version().as_bytes())?;
        hash_component(&mut hasher, descriptor.grammar_source_sha256().as_bytes())?;
        hash_component(&mut hasher, descriptor.parser_sha256().as_bytes())?;
        match descriptor.scanner_sha256() {
            Some(scanner) => {
                hash_component(&mut hasher, b"scanner")?;
                hash_component(&mut hasher, scanner.as_bytes())?;
            }
            None => hash_component(&mut hasher, b"no-scanner")?,
        }
        hash_component(
            &mut hasher,
            &u64::try_from(descriptor.abi_version())
                .map_err(|_| FirstSliceError::Limits)?
                .to_be_bytes(),
        )?;
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn grammar_binary_digest(descriptor: &GrammarDescriptor) -> Result<ContentHash, FirstSliceError> {
    let scanner = descriptor.scanner_sha256().unwrap_or("no-scanner");
    hash_static_components(&[
        ANALYZER_BINARY_SEED,
        descriptor.language().as_str().as_bytes(),
        descriptor.grammar_version().as_bytes(),
        descriptor.grammar_source_sha256().as_bytes(),
        descriptor.parser_sha256().as_bytes(),
        scanner.as_bytes(),
    ])
}

fn first_slice_provider_set_hash() -> Result<ContentHash, FirstSliceError> {
    let parser = first_slice_parser_provider_hash()?;
    hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
    ])
}

fn first_slice_incremental_plan_hash() -> Result<ContentHash, FirstSliceError> {
    hash_static_components(&[
        DERIVED_PLAN_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
    ])
}

fn first_slice_build_context() -> ContentHash {
    content_hash(BUILD_CONTEXT_SEED)
}

fn hash_static_components(components: &[&[u8]]) -> Result<ContentHash, FirstSliceError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.first-slice.static-components/1\0");
    for component in components {
        hash_component(&mut hasher, component)?;
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn hash_component(hasher: &mut blake3::Hasher, component: &[u8]) -> Result<(), FirstSliceError> {
    let length = u64::try_from(component.len()).map_err(|_| FirstSliceError::Limits)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(component);
    Ok(())
}

fn summarize_incremental_evidence(
    has_parent: bool,
    reuse_policy: FirstSliceReusePolicy,
    discovery: &IncrementalDiscovery,
    source_files: &BTreeSet<FileId>,
    plan: &InvalidationPlan,
) -> Result<FirstSliceIncrementalEvidence, FirstSliceError> {
    let mut input_counts = BTreeMap::new();
    for change in plan.changes().changes() {
        increment_evidence_count(&mut input_counts, change.class())?;
    }
    let input_changes = input_counts
        .into_iter()
        .map(|(class, inputs)| FirstSliceInputChangeCount { class, inputs })
        .collect();

    let mut file_counts = BTreeMap::new();
    for change in discovery.file_changes() {
        increment_evidence_count(&mut file_counts, change.kind())?;
    }
    let file_changes = file_counts
        .into_iter()
        .map(|(kind, files)| FirstSliceFileChangeCount { kind, files })
        .collect();

    let fallback_reason = if reuse_policy == FirstSliceReusePolicy::Incremental && has_parent {
        plan.fallback().map(|fallback| fallback.reason())
    } else {
        None
    };
    let strategy = if reuse_policy == FirstSliceReusePolicy::CleanRebuild {
        FirstSliceBuildStrategy::CleanRebuild
    } else if !has_parent {
        FirstSliceBuildStrategy::Initial
    } else if fallback_reason.is_some() {
        FirstSliceBuildStrategy::ConservativeRepositoryRebuild
    } else {
        FirstSliceBuildStrategy::DependencyDirected
    };
    Ok(FirstSliceIncrementalEvidence {
        strategy,
        input_changes,
        file_changes,
        hashed_files: u64::try_from(discovery.hashed_files().len())
            .map_err(|_| FirstSliceError::Limits)?,
        invalidated_domains: plan.rerun_domains().iter().collect(),
        invalidated_units: u64::try_from(plan.reanalyze().count())
            .map_err(|_| FirstSliceError::Limits)?,
        fallback_reason,
        trace_entries: u64::try_from(plan.trace().entries().len())
            .map_err(|_| FirstSliceError::Limits)?,
        invalidation_trace: plan.trace().entries().to_vec(),
        parsed_files: 0,
        reused_parser_artifacts: 0,
        reused_parser_artifact_bytes: 0,
        reused_durable_artifact_bytes: 0,
        lowered_files: 0,
        reused_normalized_facts: 0,
        rebuilt_normalized_facts: 0,
        planned_fact_work: planned_fact_work(has_parent, reuse_policy, source_files, plan)?,
        normalized_fact_work: Vec::new(),
        structural_cache_retained: false,
    })
}

fn planned_fact_work(
    has_parent: bool,
    reuse_policy: FirstSliceReusePolicy,
    source_files: &BTreeSet<FileId>,
    plan: &InvalidationPlan,
) -> Result<Vec<FirstSlicePlannedFactWork>, FirstSliceError> {
    let units_to_files = source_files
        .iter()
        .copied()
        .map(|file| (file_analysis_unit(file), file))
        .collect::<BTreeMap<_, _>>();
    let rebuild_cause = if reuse_policy == FirstSliceReusePolicy::CleanRebuild {
        FirstSliceFactWorkCause::UserRequestedCleanRebuild
    } else if !has_parent {
        FirstSliceFactWorkCause::InitialGeneration
    } else if plan.fallback().is_some() {
        FirstSliceFactWorkCause::ConservativeFallback
    } else {
        FirstSliceFactWorkCause::DependencyClosure
    };
    let mut groups = BTreeMap::<
        (
            FirstSliceFactWorkDisposition,
            FactDomain,
            String,
            FirstSliceFactWorkCause,
        ),
        (BTreeSet<AnalysisUnitId>, BTreeSet<FileId>),
    >::new();
    let reusable_disposition = if reuse_policy == FirstSliceReusePolicy::CleanRebuild {
        FirstSliceFactWorkDisposition::Rebuild
    } else {
        FirstSliceFactWorkDisposition::Reuse
    };
    let reusable_cause = if reuse_policy == FirstSliceReusePolicy::CleanRebuild {
        FirstSliceFactWorkCause::UserRequestedCleanRebuild
    } else {
        FirstSliceFactWorkCause::CompleteDependencyMatch
    };
    for (disposition, cause, nodes) in [
        (
            FirstSliceFactWorkDisposition::Rebuild,
            rebuild_cause,
            plan.invalidated_nodes().collect::<Vec<_>>(),
        ),
        (
            reusable_disposition,
            reusable_cause,
            plan.reusable_nodes().collect::<Vec<_>>(),
        ),
    ] {
        for node in nodes {
            let group = groups
                .entry((
                    disposition,
                    node.domain(),
                    incremental_pass_for_domain(node.domain()).to_owned(),
                    cause,
                ))
                .or_default();
            group.0.insert(node.unit());
            if let Some(file) = units_to_files.get(&node.unit()) {
                group.1.insert(*file);
            }
        }
    }
    groups
        .into_iter()
        .map(
            |((disposition, domain, provider_pass, cause), (units, files))| {
                Ok(FirstSlicePlannedFactWork {
                    disposition,
                    domain,
                    provider_pass,
                    cause,
                    files: u64::try_from(files.len()).map_err(|_| FirstSliceError::Limits)?,
                    analysis_units: u64::try_from(units.len())
                        .map_err(|_| FirstSliceError::Limits)?,
                    file_ids: files,
                    analysis_unit_ids: units,
                })
            },
        )
        .collect()
}

fn incremental_pass_for_domain(domain: FactDomain) -> &'static str {
    match domain {
        FactDomain::Syntax => PARSER_PASS_ID,
        FactDomain::PublicSurface | FactDomain::Body | FactDomain::Tests | FactDomain::Services => {
            LOWERING_PASS_ID
        }
        FactDomain::Resolution => RESOLVER_PASS_ID,
        FactDomain::Search => SEARCH_PASS_ID,
        FactDomain::DerivedGraph | FactDomain::History => DERIVED_PASS_ID,
    }
}

fn increment_evidence_count<Key: Ord>(
    counts: &mut BTreeMap<Key, u64>,
    key: Key,
) -> Result<(), FirstSliceError> {
    let count = counts.entry(key).or_insert(0);
    *count = count.checked_add(1).ok_or(FirstSliceError::Limits)?;
    Ok(())
}

fn supported_source_language<'a>(
    input: &'a ManifestInput,
    analyzers: &BTreeMap<String, TreeSitterAnalyzer>,
) -> Option<&'a str> {
    if let Some(language) = source_language_from_path(&input.path)
        && analyzers.contains_key(language)
    {
        return Some(language);
    }
    for evidence in [LanguageEvidence::Extension, LanguageEvidence::Shebang] {
        let mut matched = input
            .language_signals
            .iter()
            .filter(|signal| signal.evidence == evidence)
            .filter(|signal| analyzers.contains_key(signal.language.as_str()));
        if let Some(language) = matched.next() {
            if matched.any(|candidate| candidate.language != language.language) {
                return None;
            }
            return Some(language.language.as_str());
        }
    }
    None
}

fn detected_source_language(input: &ManifestInput) -> Option<&str> {
    if let Some(language) = source_language_from_path(&input.path) {
        return Some(language);
    }
    for evidence in [
        LanguageEvidence::Extension,
        LanguageEvidence::Shebang,
        LanguageEvidence::Manifest,
        LanguageEvidence::Content,
    ] {
        let mut matched = input
            .language_signals
            .iter()
            .filter(|signal| signal.evidence == evidence);
        if let Some(language) = matched.next() {
            if matched.any(|candidate| candidate.language != language.language) {
                return None;
            }
            return Some(language.language.as_str());
        }
    }
    None
}

fn source_language_from_path(path: &str) -> Option<&'static str> {
    extension_language(path)
}

fn analysis_tier_for_language(language: &str) -> AnalysisTier {
    if language == "rust" {
        AnalysisTier::TierB
    } else {
        AnalysisTier::TierD
    }
}

fn unsupported_language_document(
    repository: RepositoryId,
    generation: GenerationId,
    input: &UnsupportedSourceInput,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    let relative = RelativePath::parse(Path::new(&input.claim.path))
        .map_err(|_| FirstSliceError::Repository)?;
    let span = SourceSpan::new(input.claim.file, 0, input.claim.byte_length)
        .map_err(|_| FirstSliceError::Identity)?;
    let source = SourceRef::new(repository, generation, span, input.claim.content_hash, None);
    let producer = ProducerIdentity::new(
        "rootlight-language-disposition",
        "1.0.0",
        first_slice_build_context(),
    )
    .map_err(|_| FirstSliceError::Identity)?;
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Rule,
        producer,
        binary_digest: content_hash(LANGUAGE_DISPOSITION_PROVIDER_SEED),
        frontend_version: Some("disposition-1".to_owned()),
        language: input.language.clone(),
        tier: AnalysisTier::TierD,
        build_context: BuildContextIdentity::new(first_slice_build_context()),
        input_sources: vec![source.clone()],
        evidence_sources: vec![source.clone()],
        derivation_parents: Vec::new(),
        rule: Some("unsupported-language".to_owned()),
    };
    provenance.id =
        derive_provenance_record_id(&provenance).map_err(|_| FirstSliceError::Identity)?;
    let provenance_id = provenance.id;
    let evidence = || FactEvidence {
        source: Some(source.clone()),
        derivation: Vec::new(),
    };
    let file = FileRecord {
        id: input.claim.file,
        repository,
        generation,
        path: input.claim.path.clone(),
        path_locator: Some(relative.to_locator()),
        content_hash: input.claim.content_hash,
        byte_length: input.claim.byte_length,
        language: input.language.clone(),
        encoding: "utf-8".to_owned(),
        generated: input.generated,
        provenance: provenance_id,
        evidence: evidence(),
    };
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.files.push(file);
    document.provenance.push(provenance);
    for (domain, status, discovered, indexed, skipped) in [
        (IrFactDomain::Files, CoverageStatus::Complete, 1, 1, 0),
        (IrFactDomain::Entities, CoverageStatus::Unknown, 1, 0, 1),
        (IrFactDomain::Occurrences, CoverageStatus::Unknown, 1, 0, 1),
        (IrFactDomain::Relations, CoverageStatus::Unknown, 1, 0, 1),
        (IrFactDomain::Provenance, CoverageStatus::Complete, 1, 1, 0),
        (
            IrFactDomain::SourceMappings,
            CoverageStatus::Complete,
            0,
            0,
            0,
        ),
        (IrFactDomain::Diagnostics, CoverageStatus::Complete, 1, 1, 0),
        (IrFactDomain::Extensions, CoverageStatus::Complete, 1, 1, 0),
    ] {
        let mut coverage = CoverageRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            scope: CoverageScope::File(input.claim.file),
            domain,
            tier: AnalysisTier::TierD,
            status,
            discovered,
            indexed,
            skipped,
            provenance: provenance_id,
            evidence: evidence(),
        };
        coverage.id =
            derive_coverage_record_id(&coverage).map_err(|_| FirstSliceError::Identity)?;
        document.coverage_records.push(coverage);
    }
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let mut skipped = SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            source: source.clone(),
            domain,
            reason: SkippedRegionReason::UnsupportedConstruct,
            detail: "unsupported-language".to_owned(),
            provenance: provenance_id,
            evidence: evidence(),
        };
        skipped.id = derive_skipped_region_id(&skipped).map_err(|_| FirstSliceError::Identity)?;
        document.skipped_regions.push(skipped);
    }
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        code: "unsupported-language".to_owned(),
        message: "source language has no configured analyzer".to_owned(),
        severity: DiagnosticSeverity::Warning,
        source: Some(source.clone()),
        coverage_effect: CoverageStatus::Unknown,
        provenance: provenance_id,
        evidence: evidence(),
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    document.diagnostics.push(diagnostic);
    document.extensions.push(
        new_file_identity_claim_envelope(&input.claim, generation, provenance_id, source)
            .map_err(|_| FirstSliceError::Identity)?,
    );
    Ok(document)
}

fn attach_generated_origin_mappings(
    sources: &mut [RustSourceInput],
    source_languages: &BTreeMap<FileId, String>,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    let mut indexed_paths = BTreeMap::new();
    for (index, source) in sources.iter().enumerate() {
        if indexed_paths
            .insert(source.snapshot.path().identity_bytes(), index)
            .is_some()
        {
            return Err(FirstSliceError::Identity);
        }
    }

    let mut detected = Vec::new();
    detected
        .try_reserve_exact(sources.len())
        .map_err(|_| FirstSliceError::Limits)?;
    for (index, source) in sources.iter().enumerate() {
        check_cancellation(cancellation)?;
        detected.push(detect_generated_origin_mapping(
            index,
            source,
            sources,
            &indexed_paths,
            source_languages,
        )?);
    }
    drop(indexed_paths);

    for (source, mapping) in sources.iter_mut().zip(detected) {
        if let Some(mapping) = mapping {
            source
                .origins
                .try_reserve_exact(1)
                .map_err(|_| FirstSliceError::Limits)?;
            source.origins.push(mapping);
        }
    }
    Ok(())
}

fn detect_generated_origin_mapping(
    generated_index: usize,
    generated: &RustSourceInput,
    sources: &[RustSourceInput],
    indexed_paths: &BTreeMap<&[u8], usize>,
    source_languages: &BTreeMap<FileId, String>,
) -> Result<Option<GeneratedOriginMapping>, FirstSliceError> {
    if !generated.generated {
        return Ok(None);
    }
    let Some(evidence) = generated_header_evidence(generated.snapshot.content()) else {
        return Ok(None);
    };
    let Ok(origin_path) = RelativePath::parse(Path::new(evidence.origin_path)) else {
        return Ok(None);
    };
    if origin_path.as_str() != evidence.origin_path {
        return Ok(None);
    }
    let Some(origin_index) = indexed_paths
        .get(origin_path.identity_bytes())
        .copied()
        .filter(|origin_index| *origin_index != generated_index)
    else {
        return Ok(None);
    };
    let Some(origin) = sources.get(origin_index) else {
        return Err(FirstSliceError::Identity);
    };
    if source_languages.get(&generated.snapshot.file())
        != source_languages.get(&origin.snapshot.file())
    {
        return Ok(None);
    }
    let Ok(transformation) = TransformationId::new(evidence.generator) else {
        return Ok(None);
    };
    let generated_length =
        u64::try_from(generated.snapshot.content().len()).map_err(|_| FirstSliceError::Limits)?;
    let origin_length =
        u64::try_from(origin.snapshot.content().len()).map_err(|_| FirstSliceError::Limits)?;
    let (Ok(generated_span), Ok(origin_span)) = (
        SourceSpan::new(generated.snapshot.file(), 0, generated_length),
        SourceSpan::new(origin.snapshot.file(), 0, origin_length),
    ) else {
        return Ok(None);
    };
    if generated_span.start_byte() == generated_span.end_byte()
        || origin_span.start_byte() == origin_span.end_byte()
    {
        return Ok(None);
    }
    Ok(Some(GeneratedOriginMapping::new(
        generated_span,
        origin_path,
        origin_span,
        transformation,
        None,
    )))
}

struct GeneratedHeaderEvidence<'a> {
    generator: &'a str,
    origin_path: &'a str,
}

fn generated_header_evidence(source: &[u8]) -> Option<GeneratedHeaderEvidence<'_>> {
    let scan_length = source.len().min(GENERATED_HEADER_MAX_BYTES);
    let bounded = source.get(..scan_length)?;
    let scan = if source.len() > scan_length && !bounded.ends_with(b"\n") {
        let complete_end = bounded
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position.saturating_add(1));
        bounded.get(..complete_end)?
    } else {
        bounded
    };

    let mut generator = None;
    let mut origin_path = None;
    for line in scan
        .split(|byte| *byte == b'\n')
        .take(GENERATED_HEADER_MAX_LINES)
        .map(trim_ascii_whitespace)
    {
        if let Some(value) = generated_by_marker(line)
            && generator.replace(value).is_some()
        {
            return None;
        }
        if let Some(value) = generated_source_marker(line)
            && origin_path.replace(value).is_some()
        {
            return None;
        }
    }

    let generator = std::str::from_utf8(generator?).ok()?;
    let origin_path = std::str::from_utf8(origin_path?).ok()?;
    (!generator.is_empty() && !origin_path.is_empty()).then_some(GeneratedHeaderEvidence {
        generator,
        origin_path,
    })
}

fn generated_by_marker(line: &[u8]) -> Option<&[u8]> {
    line.strip_prefix(b"// Code generated by ")
        .and_then(|value| value.strip_suffix(b". DO NOT EDIT."))
        .or_else(|| {
            line.strip_prefix(b"# Code generated by ")
                .and_then(|value| value.strip_suffix(b". DO NOT EDIT."))
        })
        .or_else(|| {
            line.strip_prefix(b"/* Code generated by ")
                .and_then(|value| value.strip_suffix(b". DO NOT EDIT. */"))
        })
}

fn generated_source_marker(line: &[u8]) -> Option<&[u8]> {
    line.strip_prefix(b"// source: ")
        .or_else(|| line.strip_prefix(b"# source: "))
        .or_else(|| {
            line.strip_prefix(b"/* source: ")
                .and_then(|value| value.strip_suffix(b" */"))
        })
}

fn trim_ascii_whitespace(mut value: &[u8]) -> &[u8] {
    while let Some((first, rest)) = value.split_first()
        && first.is_ascii_whitespace()
    {
        value = rest;
    }
    while let Some((last, rest)) = value.split_last()
        && last.is_ascii_whitespace()
    {
        value = rest;
    }
    value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceInputPreflight {
    supported_file_count: usize,
    source_bytes: u64,
}

fn preflight_source_inputs(
    inputs: &[ManifestInput],
    analyzers: &BTreeMap<String, TreeSitterAnalyzer>,
    maximum_files: usize,
    maximum_source_bytes: usize,
    cancellation: &Cancellation,
) -> Result<SourceInputPreflight, FirstSliceError> {
    check_cancellation(cancellation)?;
    let mut discovered_file_count = 0usize;
    let mut supported_file_count = 0usize;
    let mut source_bytes = 0usize;
    for input in inputs {
        check_cancellation(cancellation)?;
        discovered_file_count = checked_resource_length(
            discovered_file_count,
            1,
            maximum_files,
            FirstSliceResource::SourceFiles,
        )?;
        if supported_source_language(input, analyzers).is_some() {
            supported_file_count = supported_file_count
                .checked_add(1)
                .ok_or(FirstSliceError::Limits)?;
        }
        let input_bytes = usize::try_from(input.bytes).map_err(|_| FirstSliceError::Limits)?;
        source_bytes = source_bytes
            .checked_add(input_bytes)
            .ok_or(FirstSliceError::Limits)?;
        if source_bytes > maximum_source_bytes {
            return Err(resource_limit(
                FirstSliceResource::SourceBytes,
                source_bytes,
                maximum_source_bytes,
            ));
        }
    }
    check_cancellation(cancellation)?;
    if discovered_file_count == 0 {
        return Err(FirstSliceError::FixtureShape);
    }
    Ok(SourceInputPreflight {
        supported_file_count,
        source_bytes: u64::try_from(source_bytes).map_err(|_| FirstSliceError::Limits)?,
    })
}

#[cfg(test)]
fn durable_initial_admission_reservation(source_bytes: u64) -> Result<u64, FirstSliceError> {
    source_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(DURABLE_STAGING_FIXED_OVERHEAD_BYTES))
        .and_then(|bytes| bytes.checked_add(DURABLE_DISK_SAFETY_MARGIN_BYTES))
        .ok_or(FirstSliceError::Limits)
}

fn storage_admission_error(failure: DurableStorageAdmissionFailure) -> FirstSliceError {
    let scope = match failure.scope {
        DurableStorageAdmissionScope::RepositoryBudget => FirstSliceStorageScope::Repository,
        DurableStorageAdmissionScope::CatalogBudget => FirstSliceStorageScope::Catalog,
        DurableStorageAdmissionScope::FilesystemFreeSpace => FirstSliceStorageScope::Filesystem,
    };
    FirstSliceError::StorageResourceExhausted {
        scope,
        required_bytes: failure.required_bytes,
        observed_bytes: failure.observed_bytes,
        limit_bytes: failure.limit_bytes,
        minimum_free_bytes: failure.minimum_free_bytes,
    }
}

#[cfg(test)]
fn durable_staging_reservation(source_bytes: u64) -> Result<u64, FirstSliceError> {
    source_bytes
        .checked_mul(DURABLE_SOURCE_WRITE_AMPLIFICATION_FACTOR)
        .and_then(|bytes| bytes.checked_add(DURABLE_STAGING_FIXED_OVERHEAD_BYTES))
        .and_then(|bytes| bytes.checked_add(DURABLE_DISK_SAFETY_MARGIN_BYTES))
        .ok_or(FirstSliceError::Limits)
}

#[derive(Default)]
struct SerializedSizeCounter {
    bytes: u64,
}

impl std::io::Write for SerializedSizeCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let length =
            u64::try_from(buffer.len()).map_err(|_| std::io::Error::other("size overflow"))?;
        self.bytes = self
            .bytes
            .checked_add(length)
            .ok_or_else(|| std::io::Error::other("size overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn normalized_document_serialized_bytes(
    document: &NormalizedIrDocument,
) -> Result<u64, FirstSliceError> {
    let mut serialized = SerializedSizeCounter::default();
    serde_json::to_writer(&mut serialized, document).map_err(|_| FirstSliceError::Limits)?;
    Ok(serialized.bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingGenerationMemory {
    Reserved,
    Staged,
}

fn generation_memory_limit(
    retained_bytes: u64,
    pending_bytes: u64,
    pending: PendingGenerationMemory,
) -> FirstSliceError {
    let (reserved_bytes, staged_bytes) = match pending {
        PendingGenerationMemory::Reserved => (pending_bytes, 0),
        PendingGenerationMemory::Staged => (0, pending_bytes),
    };
    FirstSliceError::GenerationMemoryLimit {
        breakdown: GenerationMemoryBreakdown {
            retained_bytes,
            reserved_bytes,
            owned_bytes: retained_bytes,
            referenced_bytes: 0,
            mapped_bytes: 0,
            staged_bytes,
            shared_bytes: 0,
        },
        observed: retained_bytes.saturating_add(pending_bytes),
        limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
    }
}

fn ensure_generation_memory_preflight(source_bytes: u64) -> Result<u64, FirstSliceError> {
    let observed = source_bytes
        .checked_mul(GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR)
        .and_then(|bytes| bytes.checked_add(GENERATION_MEMORY_FIXED_OVERHEAD_BYTES))
        .ok_or(FirstSliceError::Limits)?;
    if observed > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES {
        return Err(generation_memory_limit(
            0,
            observed,
            PendingGenerationMemory::Reserved,
        ));
    }
    Ok(observed)
}

fn ensure_generation_memory_admission(
    serialized_document_bytes: u64,
) -> Result<u64, FirstSliceError> {
    let observed = serialized_document_bytes;
    if observed > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES {
        return Err(generation_memory_limit(
            0,
            observed,
            PendingGenerationMemory::Staged,
        ));
    }
    Ok(observed)
}

fn logical_projection_staged_memory_bytes(
    serialized_document_bytes: u64,
    compact_graph_workspace_bytes: u64,
) -> Result<u64, FirstSliceError> {
    serialized_document_bytes
        .checked_mul(3)
        .and_then(|documents| documents.checked_add(compact_graph_workspace_bytes))
        .ok_or(FirstSliceError::Limits)
}

fn project_context_manifest(
    language: &str,
    configuration: ContentHash,
) -> Result<Vec<u8>, FirstSliceError> {
    let capacity = PROJECT_CONTEXT_SEED
        .len()
        .checked_add(configuration.as_bytes().len())
        .and_then(|length| length.checked_add(language.len()))
        .and_then(|length| length.checked_add(2))
        .ok_or(FirstSliceError::Limits)?;
    let mut manifest = Vec::new();
    manifest
        .try_reserve_exact(capacity)
        .map_err(|_| FirstSliceError::Limits)?;
    manifest.extend_from_slice(PROJECT_CONTEXT_SEED);
    manifest.push(0);
    manifest.extend_from_slice(configuration.as_bytes());
    manifest.push(0);
    manifest.extend_from_slice(language.as_bytes());
    Ok(manifest)
}

#[cfg(test)]
fn project_document_matches_inputs(
    document: &NormalizedIrDocument,
    repository: RepositoryId,
    generation: GenerationId,
    provider_identity: ContentHash,
    inputs: &[FirstSliceProjectInput<'_>],
) -> bool {
    project_documents_match_inputs(
        std::slice::from_ref(document),
        repository,
        generation,
        provider_identity,
        inputs,
    )
}

fn project_documents_match_inputs(
    documents: &[NormalizedIrDocument],
    repository: RepositoryId,
    generation: GenerationId,
    provider_identity: ContentHash,
    inputs: &[FirstSliceProjectInput<'_>],
) -> bool {
    if documents.is_empty()
        || documents.iter().any(|document| {
            document.repository != repository
                || document.generation != generation
                || document.provenance.is_empty()
                || document.provenance.iter().any(|provenance| {
                    provenance.tier != AnalysisTier::TierB
                        || provenance.binary_digest != provider_identity
                })
        })
    {
        return false;
    }
    let expected_files = inputs
        .iter()
        .map(|input| (input.file(), input))
        .collect::<BTreeMap<_, _>>();
    if expected_files.len() != inputs.len() {
        return false;
    }
    let mut observed_files = BTreeMap::new();
    for file in documents.iter().flat_map(|document| &document.files) {
        if observed_files.insert(file.id, file).is_some() {
            return false;
        }
        let Some(input) = expected_files.get(&file.id) else {
            return false;
        };
        if file.repository != repository
            || file.generation != generation
            || file.path != input.path()
            || file.content_hash != input.content_hash()
            || usize::try_from(file.byte_length) != Ok(input.source().len())
            || file.generated != input.generated()
        {
            return false;
        }
    }
    if observed_files.len() != inputs.len() {
        return false;
    }

    let expected_mappings = inputs
        .iter()
        .flat_map(FirstSliceProjectInput::origins)
        .map(|mapping| ((mapping.generated(), mapping.origin()), mapping))
        .collect::<BTreeMap<_, _>>();
    let expected_mapping_count = inputs
        .iter()
        .map(|input| input.origins().len())
        .sum::<usize>();
    if expected_mappings.len() != expected_mapping_count {
        return false;
    }
    let mut provenance = BTreeMap::new();
    for record in documents.iter().flat_map(|document| &document.provenance) {
        if provenance.insert(record.id, record).is_some() {
            return false;
        }
    }
    let mut observed_mapping_keys = BTreeSet::new();
    let mut observed_mapping_count = 0_usize;
    for observed in documents
        .iter()
        .flat_map(|document| &document.source_mappings)
        .filter(|mapping| mapping.kind == SourceMappingKind::GeneratedToOrigin)
    {
        observed_mapping_count = match observed_mapping_count.checked_add(1) {
            Some(count) => count,
            None => return false,
        };
        let key = (observed.from.span(), observed.to.span());
        if !observed_mapping_keys.insert(key) {
            return false;
        }
        let Some(expected) = expected_mappings.get(&key) else {
            return false;
        };
        let Some(mapping_provenance) = provenance.get(&observed.provenance) else {
            return false;
        };
        if mapping_provenance.producer_kind != ProducerKind::Derivation
            || mapping_provenance.rule.as_deref() != Some(expected.provenance_rule().as_str())
            || mapping_provenance.evidence_sources != [observed.from.clone(), observed.to.clone()]
            || observed.evidence.source.as_ref() != Some(&observed.from)
            || observed.evidence.derivation != [FactRef::File(observed.to.span().file())]
        {
            return false;
        }
    }
    observed_mapping_count == expected_mapping_count
}

fn missing_structural_declaration_count(
    structural_documents: &[NormalizedIrDocument],
    project_document: &NormalizedIrDocument,
    require_explicit_check: bool,
) -> Result<u64, FirstSliceError> {
    let entity_coverage_is_bounded = project_document.coverage_records.iter().any(|coverage| {
        coverage.domain == IrFactDomain::Entities
            && coverage.status != CoverageStatus::Complete
            && coverage.skipped > 0
    });
    if !require_explicit_check && !entity_coverage_is_bounded {
        return Ok(0);
    }
    let structural = structural_declaration_keys(
        structural_documents
            .iter()
            .flat_map(|document| document.entities.iter()),
    );
    let project = project_declaration_index(project_document);
    u64::try_from(
        structural
            .iter()
            .filter(|declaration| !project.preserves(declaration))
            .count(),
    )
    .map_err(|_| FirstSliceError::Limits)
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectDeclarationKey {
    source: SourceSpan,
    kind: EntityKind,
    canonical_name: String,
}

fn structural_declaration_keys<'a>(
    entities: impl Iterator<Item = &'a rootlight_ir::EntityRecord>,
) -> BTreeSet<ProjectDeclarationKey> {
    let mut declarations = BTreeSet::new();
    for entity in entities {
        let Some(key) = project_declaration_key(entity) else {
            continue;
        };
        declarations.insert(key);
    }
    declarations
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectDeclarationIdentity {
    file: FileId,
    kind: EntityKind,
    canonical_name: String,
}

#[derive(Debug, Default)]
struct ProjectDeclarationIndex {
    exact: BTreeSet<ProjectDeclarationKey>,
    definition_sites: BTreeMap<ProjectDeclarationIdentity, BTreeSet<(u64, u64)>>,
}

impl ProjectDeclarationIndex {
    fn preserves(&self, declaration: &ProjectDeclarationKey) -> bool {
        if self.exact.contains(declaration) {
            return true;
        }
        let identity = ProjectDeclarationIdentity {
            file: declaration.source.file(),
            kind: declaration.kind,
            canonical_name: declaration.canonical_name.clone(),
        };
        let Some(sites) = self.definition_sites.get(&identity) else {
            return false;
        };
        sites
            .range((declaration.source.start_byte(), 0)..=(declaration.source.end_byte(), u64::MAX))
            .any(|(_, end)| *end <= declaration.source.end_byte())
    }
}

fn project_declaration_index(document: &NormalizedIrDocument) -> ProjectDeclarationIndex {
    let mut declarations = ProjectDeclarationIndex::default();
    let mut identity = BTreeMap::new();
    for entity in &document.entities {
        let Some(key) = project_declaration_key(entity) else {
            continue;
        };
        identity.insert(entity.id, (entity.kind, entity.canonical_name.clone()));
        declarations.exact.insert(key);
    }
    for occurrence in &document.occurrences {
        if occurrence.role != OccurrenceRole::Definition {
            continue;
        }
        let rootlight_ir::OccurrenceTarget::Resolved { symbol } = &occurrence.target else {
            continue;
        };
        let Some((kind, canonical_name)) = identity.get(symbol) else {
            continue;
        };
        // Project analysis intentionally unifies repeated declarations under
        // one stable entity. Its definition occurrence identifies the name
        // token inside every declaration span that structural output may keep.
        let source = occurrence.source.span();
        declarations
            .definition_sites
            .entry(ProjectDeclarationIdentity {
                file: source.file(),
                kind: *kind,
                canonical_name: canonical_name.clone(),
            })
            .or_default()
            .insert((source.start_byte(), source.end_byte()));
    }
    declarations
}

fn project_declaration_key(entity: &rootlight_ir::EntityRecord) -> Option<ProjectDeclarationKey> {
    if entity.kind == EntityKind::ExternalSymbol
        || entity.flags.contains(&rootlight_ir::EntityFlag::Synthetic)
    {
        return None;
    }
    let source = entity.evidence.source.as_ref()?;
    Some(ProjectDeclarationKey {
        source: source.span(),
        kind: entity.kind,
        canonical_name: entity.canonical_name.clone(),
    })
}

fn merge_project_documents(
    documents: Vec<NormalizedIrDocument>,
    diagnostic_capacity: usize,
) -> Result<(NormalizedIrDocument, ProjectMergeTruncation), FirstSliceError> {
    let mut documents = documents.into_iter();
    let mut merged = documents.next().ok_or(FirstSliceError::Identity)?;
    let mut external_symbols = project_external_symbols(&merged);
    let mut truncation = ProjectMergeTruncation {
        diagnostics: merged.diagnostics.len() > diagnostic_capacity,
        facts: false,
    };
    merged.diagnostics.truncate(diagnostic_capacity);
    for document in documents {
        truncation.merge(merge_project_document(
            &mut merged,
            document,
            &mut external_symbols,
            diagnostic_capacity,
            &IrLimits::default(),
        )?);
    }
    Ok((merged, truncation))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ProjectMergeTruncation {
    diagnostics: bool,
    facts: bool,
}

impl ProjectMergeTruncation {
    fn merge(&mut self, other: Self) {
        self.diagnostics |= other.diagnostics;
        self.facts |= other.facts;
    }
}

fn project_external_symbols(document: &NormalizedIrDocument) -> BTreeSet<SymbolId> {
    document
        .entities
        .iter()
        .filter_map(|entity| (entity.kind == EntityKind::ExternalSymbol).then_some(entity.id))
        .collect()
}

fn merge_project_document(
    merged: &mut NormalizedIrDocument,
    mut document: NormalizedIrDocument,
    external_symbols: &mut BTreeSet<SymbolId>,
    diagnostic_capacity: usize,
    limits: &IrLimits,
) -> Result<ProjectMergeTruncation, FirstSliceError> {
    if document.version != merged.version
        || document.repository != merged.repository
        || document.generation != merged.generation
    {
        return Err(FirstSliceError::Identity);
    }
    let mut duplicate_external_symbols = BTreeSet::new();
    let mut new_external_symbols = BTreeSet::new();
    document.entities.retain(|entity| {
        entity.kind != EntityKind::ExternalSymbol
            || if !external_symbols.contains(&entity.id) && new_external_symbols.insert(entity.id) {
                true
            } else {
                duplicate_external_symbols.insert(entity.id);
                false
            }
    });
    if !duplicate_external_symbols.is_empty() {
        document.extensions.retain(|extension| {
            if extension.namespace != SYMBOL_IDENTITY_CLAIM_NAMESPACE {
                return true;
            }
            let [FactRef::Entity(symbol)] = extension.evidence.derivation.as_slice() else {
                return true;
            };
            !duplicate_external_symbols.contains(symbol)
        });
    }
    drop(new_external_symbols);
    let mut diagnostics_truncated = merged.diagnostics.len() > diagnostic_capacity;
    merged.diagnostics.truncate(diagnostic_capacity);
    let remaining_diagnostics = diagnostic_capacity.saturating_sub(merged.diagnostics.len());
    diagnostics_truncated |= document.diagnostics.len() > remaining_diagnostics;
    document.diagnostics.truncate(remaining_diagnostics);
    let mut facts_truncated = false;
    match preflight_normalized_document_append(merged, &document, limits) {
        Ok(()) => {}
        Err(FirstSliceError::ResourceLimit { .. }) => {
            retain_project_capacity_summary(&mut document)?;
            facts_truncated = true;
            preflight_normalized_document_append(merged, &document, limits)?;
            validate_project_capacity_summary(&document, limits)?;
        }
        Err(error) => return Err(error),
    }
    if let Err(error) = reserve_project_merge(merged, &document) {
        if error != FirstSliceError::Limits || facts_truncated {
            return Err(error);
        }
        retain_project_capacity_summary(&mut document)?;
        facts_truncated = true;
        preflight_normalized_document_append(merged, &document, limits)?;
        validate_project_capacity_summary(&document, limits)?;
        reserve_project_merge(merged, &document)?;
    }
    external_symbols.extend(
        document
            .entities
            .iter()
            .filter_map(|entity| (entity.kind == EntityKind::ExternalSymbol).then_some(entity.id)),
    );
    merged.files.append(&mut document.files);
    merged.entities.append(&mut document.entities);
    merged.occurrences.append(&mut document.occurrences);
    merged.relations.append(&mut document.relations);
    merged.provenance.append(&mut document.provenance);
    merged.source_mappings.append(&mut document.source_mappings);
    merged
        .coverage_records
        .append(&mut document.coverage_records);
    merged.skipped_regions.append(&mut document.skipped_regions);
    merged.diagnostics.append(&mut document.diagnostics);
    merged.extensions.append(&mut document.extensions);
    Ok(ProjectMergeTruncation {
        diagnostics: diagnostics_truncated,
        facts: facts_truncated,
    })
}

fn reserve_project_merge(
    merged: &mut NormalizedIrDocument,
    document: &NormalizedIrDocument,
) -> Result<(), FirstSliceError> {
    merged
        .files
        .try_reserve_exact(document.files.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .entities
        .try_reserve_exact(document.entities.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .occurrences
        .try_reserve_exact(document.occurrences.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .relations
        .try_reserve_exact(document.relations.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .provenance
        .try_reserve_exact(document.provenance.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .source_mappings
        .try_reserve_exact(document.source_mappings.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .coverage_records
        .try_reserve_exact(document.coverage_records.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .skipped_regions
        .try_reserve_exact(document.skipped_regions.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .diagnostics
        .try_reserve_exact(document.diagnostics.len())
        .map_err(|_| FirstSliceError::Limits)?;
    merged
        .extensions
        .try_reserve_exact(document.extensions.len())
        .map_err(|_| FirstSliceError::Limits)?;
    Ok(())
}

fn prepare_project_analysis_document(
    documents: Vec<NormalizedIrDocument>,
    language: &str,
    partitioned: bool,
    diagnostics_truncated: bool,
    facts_truncated: bool,
    existing_target_diagnostics: usize,
    limits: &IrLimits,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    // The project document is appended to the structural target, so its local
    // quota must account for diagnostics already owned by earlier providers.
    let available_diagnostics = limits
        .max_diagnostics
        .checked_sub(existing_target_diagnostics)
        .ok_or(FirstSliceError::Limits)?;
    let requested_reserve = if partitioned {
        PROJECT_PARTITION_DIAGNOSTIC_RESERVE
    } else {
        1
    };
    let diagnostic_reserve = available_diagnostics.min(requested_reserve);
    let diagnostic_capacity = available_diagnostics - diagnostic_reserve;
    let (mut document, merge_truncation) = merge_project_documents(documents, diagnostic_capacity)?;
    let diagnostics_truncated = diagnostics_truncated || merge_truncation.diagnostics;
    let facts_truncated = facts_truncated || merge_truncation.facts;
    if partitioned || diagnostics_truncated || facts_truncated {
        bound_project_coverage(&mut document)?;
    }
    let mut remaining_reserve = diagnostic_reserve;
    if facts_truncated && remaining_reserve > 0 {
        append_project_facts_truncated(&mut document, limits)?;
        remaining_reserve -= 1;
    }
    if diagnostics_truncated && remaining_reserve > 0 {
        append_project_diagnostics_truncated(&mut document, limits)?;
        remaining_reserve -= 1;
    }
    if partitioned && remaining_reserve > 0 {
        append_project_partition_diagnostic(&mut document, language, limits)?;
    }
    let combined_diagnostics = existing_target_diagnostics
        .checked_add(document.diagnostics.len())
        .ok_or(FirstSliceError::Limits)?;
    if combined_diagnostics > limits.max_diagnostics {
        return Err(FirstSliceError::Limits);
    }
    Ok(document)
}

fn bound_project_coverage(document: &mut NormalizedIrDocument) -> Result<(), FirstSliceError> {
    for coverage in &mut document.coverage_records {
        if coverage.tier == AnalysisTier::TierB && coverage.status == CoverageStatus::Complete {
            coverage.status = CoverageStatus::Bounded;
            coverage.id =
                derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
        }
    }
    Ok(())
}

fn append_project_diagnostics_truncated(
    document: &mut NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    append_project_truncation_diagnostic(
        document,
        PROJECT_DIAGNOSTICS_TRUNCATED_CODE,
        PROJECT_DIAGNOSTICS_TRUNCATED_MESSAGE,
        limits,
    )
}

fn append_project_facts_truncated(
    document: &mut NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    append_project_truncation_diagnostic(
        document,
        PROJECT_FACTS_TRUNCATED_CODE,
        PROJECT_FACTS_TRUNCATED_MESSAGE,
        limits,
    )
}

fn append_project_truncation_diagnostic(
    document: &mut NormalizedIrDocument,
    code: &str,
    message: &str,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let total = normalized_record_count(document)?;
    checked_combined_length(total, 1, limits.max_total_records)?;
    reserve_records(&mut document.diagnostics, 1, limits.max_diagnostics)?;
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: code.to_owned(),
        message: message.to_owned(),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    document.diagnostics.push(diagnostic);
    Ok(())
}

fn append_project_partition_diagnostic(
    document: &mut NormalizedIrDocument,
    language: &str,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let total = normalized_record_count(document)?;
    checked_combined_length(total, 1, limits.max_total_records)?;
    reserve_records(&mut document.diagnostics, 1, limits.max_diagnostics)?;
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: "project-adapter-partitioned-coverage".to_owned(),
        message: format!(
            "project analysis for {language} was partitioned and cross-partition relationships are bounded"
        ),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    document.diagnostics.push(diagnostic);
    Ok(())
}

fn is_project_fallback_code(code: &str) -> bool {
    code.starts_with("project-adapter-") && code.ends_with("-fallback")
}

fn is_priority_aggregate_diagnostic(code: &str) -> bool {
    code.starts_with("project-adapter-")
        || matches!(
            code,
            AGGREGATE_DIAGNOSTICS_TRUNCATED_CODE
                | "extension-coverage-bounded"
                | "skipped-region-details-bounded"
                | "oversized-inputs-bounded"
        )
}

fn retain_priority_aggregate_diagnostic(
    document: &mut NormalizedIrDocument,
    diagnostic: DiagnosticRecord,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let total = normalized_record_count(document)?;
    if total > limits.max_total_records || document.diagnostics.len() > limits.max_diagnostics {
        return Err(FirstSliceError::Limits);
    }
    if total < limits.max_total_records && document.diagnostics.len() < limits.max_diagnostics {
        reserve_records(&mut document.diagnostics, 1, limits.max_diagnostics)?;
        document.diagnostics.push(diagnostic);
        return Ok(());
    }
    // Source-bearing bounded details are already lossy, so evict one before
    // diagnostics that may be the only disposition evidence for a source.
    let replacement = document
        .diagnostics
        .iter()
        .rposition(|retained| {
            !is_priority_aggregate_diagnostic(&retained.code)
                && retained.source.is_some()
                && retained.coverage_effect == CoverageStatus::Bounded
        })
        .or_else(|| {
            document
                .diagnostics
                .iter()
                .rposition(|retained| !is_priority_aggregate_diagnostic(&retained.code))
        })
        .ok_or(FirstSliceError::Limits)?;
    let retained = document
        .diagnostics
        .get_mut(replacement)
        .ok_or(FirstSliceError::Limits)?;
    *retained = diagnostic;
    Ok(())
}

fn project_fallback_error(code: &str) -> Option<FirstSliceError> {
    match code {
        "project-adapter-wall-time-fallback" => Some(FirstSliceError::AdapterWallTimeLimit),
        "project-adapter-input-limit-fallback" => Some(FirstSliceError::AdapterInputLimit),
        "project-adapter-output-limit-fallback" => Some(FirstSliceError::AdapterOutputLimit),
        "project-adapter-memory-limit-fallback" => Some(FirstSliceError::AdapterMemoryLimit),
        "project-adapter-process-fallback" => Some(FirstSliceError::AdapterProcessFailure),
        "project-adapter-resource-unavailable-fallback" => {
            Some(FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::ProcessResources,
            })
        }
        "project-adapter-capacity-fallback"
        | "project-adapter-facts-truncated-fallback"
        | "project-adapter-declaration-loss-fallback" => Some(FirstSliceError::IncompleteCoverage),
        _ if is_project_fallback_code(code) => Some(FirstSliceError::Adapter),
        _ => None,
    }
}

fn append_project_fallback_diagnostic(
    document: &mut NormalizedIrDocument,
    language: &str,
    error: FirstSliceProjectAnalysisError,
    fallback_file: FileId,
    fallback_provenance: FactId,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: error.fallback_code().to_owned(),
        message: format!("project analysis for {language} used structural fallback"),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Unknown,
        provenance: fallback_provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(fallback_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    retain_priority_aggregate_diagnostic(document, diagnostic, limits)
}

fn append_extension_truncation_diagnostic(
    document: &mut NormalizedIrDocument,
    truncated_extensions: u64,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: "extension-coverage-bounded".to_owned(),
        message: format!(
            "{truncated_extensions} optional lexical extensions omitted by aggregate resource limit"
        ),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    retain_priority_aggregate_diagnostic(document, diagnostic, limits)
}

fn append_diagnostic_truncation_diagnostic(
    document: &mut NormalizedIrDocument,
    truncated_diagnostics: u64,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: AGGREGATE_DIAGNOSTICS_TRUNCATED_CODE.to_owned(),
        message: format!(
            "{truncated_diagnostics} source diagnostics omitted by aggregate resource limit"
        ),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    retain_priority_aggregate_diagnostic(document, diagnostic, limits)
}

fn append_skipped_region_truncation_diagnostic(
    document: &mut NormalizedIrDocument,
    truncated_regions: u64,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: "skipped-region-details-bounded".to_owned(),
        message: format!(
            "{truncated_regions} skipped-region detail records omitted by aggregate resource limit"
        ),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    retain_priority_aggregate_diagnostic(document, diagnostic, limits)
}

fn append_oversized_input_diagnostic(
    document: &mut NormalizedIrDocument,
    oversized_inputs: u64,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: "oversized-inputs-bounded".to_owned(),
        message: format!(
            "oversized repository input count {oversized_inputs} omitted by configured source file limit"
        ),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    retain_priority_aggregate_diagnostic(document, diagnostic, limits)
}

#[derive(Debug, Default)]
struct DocumentAppendState {
    extension_payload_bytes: usize,
    truncated_diagnostics: u64,
    truncated_extensions: u64,
    truncated_skipped_regions: u64,
}

impl DocumentAppendState {
    fn from_document(document: &NormalizedIrDocument) -> Result<Self, FirstSliceError> {
        let extension_payload_bytes =
            document
                .extensions
                .iter()
                .try_fold(0_usize, |total, extension| {
                    total
                        .checked_add(extension.payload.len())
                        .ok_or(FirstSliceError::Limits)
                })?;
        Ok(Self {
            extension_payload_bytes,
            truncated_diagnostics: 0,
            truncated_extensions: 0,
            truncated_skipped_regions: 0,
        })
    }
}

fn append_normalized_document(
    target: &mut NormalizedIrDocument,
    mut source: NormalizedIrDocument,
    limits: &IrLimits,
    append_state: &mut DocumentAppendState,
) -> Result<(), FirstSliceError> {
    if source.version != target.version
        || source.repository != target.repository
        || source.generation != target.generation
    {
        return Err(FirstSliceError::Identity);
    }
    reserve_resource_records(
        &mut target.files,
        source.files.len(),
        limits.max_files,
        FirstSliceResource::Files,
    )?;
    reserve_resource_records(
        &mut target.entities,
        source.entities.len(),
        limits.max_entities,
        FirstSliceResource::Entities,
    )?;
    reserve_resource_records(
        &mut target.occurrences,
        source.occurrences.len(),
        limits.max_occurrences,
        FirstSliceResource::Occurrences,
    )?;
    reserve_resource_records(
        &mut target.relations,
        source.relations.len(),
        limits.max_relations,
        FirstSliceResource::Relations,
    )?;
    reserve_resource_records(
        &mut target.provenance,
        source.provenance.len(),
        limits.max_provenance_records,
        FirstSliceResource::Provenance,
    )?;
    reserve_resource_records(
        &mut target.source_mappings,
        source.source_mappings.len(),
        limits.max_source_mappings,
        FirstSliceResource::SourceMappings,
    )?;
    reserve_resource_records(
        &mut target.coverage_records,
        source.coverage_records.len(),
        limits.max_coverage_records,
        FirstSliceResource::Coverage,
    )?;
    let truncated_skipped_regions = truncate_skipped_regions(target, &mut source, limits)?;
    reserve_resource_records(
        &mut target.skipped_regions,
        source.skipped_regions.len(),
        limits.max_skipped_regions,
        FirstSliceResource::SkippedRegions,
    )?;
    let truncated_diagnostics =
        truncate_aggregate_diagnostics(target, &mut source, append_state, limits)?;
    reserve_resource_records(
        &mut target.diagnostics,
        source.diagnostics.len(),
        limits.max_diagnostics,
        FirstSliceResource::Diagnostics,
    )?;
    let target_non_extensions = normalized_record_count(target)?
        .checked_sub(target.extensions.len())
        .ok_or(FirstSliceError::Limits)?;
    let source_non_extensions = normalized_record_count(&source)?
        .checked_sub(source.extensions.len())
        .ok_or(FirstSliceError::Limits)?;
    let maximum_merged_extensions = target
        .extensions
        .len()
        .checked_add(source.extensions.len())
        .ok_or(FirstSliceError::Limits)?
        .min(limits.max_extensions);
    let maximum_total = target_non_extensions
        .checked_add(source_non_extensions)
        .and_then(|total| total.checked_add(maximum_merged_extensions))
        .ok_or(FirstSliceError::Limits)?;
    if maximum_total > limits.max_total_records {
        return Err(resource_limit(
            FirstSliceResource::Records,
            maximum_total,
            limits.max_total_records,
        ));
    }
    target
        .extensions
        .try_reserve(
            source.extensions.len().min(
                limits
                    .max_extensions
                    .saturating_sub(target.extensions.len()),
            ),
        )
        .map_err(|_| FirstSliceError::Limits)?;
    let next_append_state = truncate_optional_extensions(
        target,
        &mut source,
        append_state,
        truncated_diagnostics,
        truncated_skipped_regions,
        limits,
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
    *append_state = next_append_state;
    Ok(())
}

fn append_project_document_with_capacity(
    target: &mut NormalizedIrDocument,
    source: NormalizedIrDocument,
    limits: &IrLimits,
    append_state: &mut DocumentAppendState,
) -> Result<(), FirstSliceError> {
    // Decide whether to retain full project facts before the shared append path
    // can reserve or truncate any target-owned collection.
    preflight_normalized_document_append(target, &source, limits)?;
    append_normalized_document(target, source, limits, append_state)?;
    Ok(())
}

fn preflight_normalized_document_append(
    target: &NormalizedIrDocument,
    source: &NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    if source.version != target.version
        || source.repository != target.repository
        || source.generation != target.generation
    {
        return Err(FirstSliceError::Identity);
    }
    for (current, additional, maximum, resource) in [
        (
            target.files.len(),
            source.files.len(),
            limits.max_files,
            FirstSliceResource::Files,
        ),
        (
            target.entities.len(),
            source.entities.len(),
            limits.max_entities,
            FirstSliceResource::Entities,
        ),
        (
            target.occurrences.len(),
            source.occurrences.len(),
            limits.max_occurrences,
            FirstSliceResource::Occurrences,
        ),
        (
            target.relations.len(),
            source.relations.len(),
            limits.max_relations,
            FirstSliceResource::Relations,
        ),
        (
            target.provenance.len(),
            source.provenance.len(),
            limits.max_provenance_records,
            FirstSliceResource::Provenance,
        ),
        (
            target.source_mappings.len(),
            source.source_mappings.len(),
            limits.max_source_mappings,
            FirstSliceResource::SourceMappings,
        ),
        (
            target.coverage_records.len(),
            source.coverage_records.len(),
            limits.max_coverage_records,
            FirstSliceResource::Coverage,
        ),
        (
            target.diagnostics.len(),
            source.diagnostics.len(),
            limits.max_diagnostics,
            FirstSliceResource::Diagnostics,
        ),
    ] {
        checked_resource_length(current, additional, maximum, resource)?;
    }
    checked_resource_length(
        target.skipped_regions.len(),
        0,
        limits.max_skipped_regions,
        FirstSliceResource::SkippedRegions,
    )?;

    let target_required_extensions = target
        .extensions
        .iter()
        .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE)
        .count();
    let source_required_extensions = source
        .extensions
        .iter()
        .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE)
        .count();
    checked_resource_length(
        target_required_extensions,
        source_required_extensions,
        limits.max_extensions,
        FirstSliceResource::Extensions,
    )?;
    let target_required_extension_bytes = extension_payload_bytes(
        target
            .extensions
            .iter()
            .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE),
    )?;
    let source_required_extension_bytes = extension_payload_bytes(
        source
            .extensions
            .iter()
            .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE),
    )?;
    checked_resource_length(
        target_required_extension_bytes,
        source_required_extension_bytes,
        limits.max_total_extension_bytes,
        FirstSliceResource::ExtensionBytes,
    )?;

    let available_skipped_regions = limits
        .max_skipped_regions
        .checked_sub(target.skipped_regions.len())
        .ok_or_else(|| {
            resource_limit(
                FirstSliceResource::SkippedRegions,
                target.skipped_regions.len(),
                limits.max_skipped_regions,
            )
        })?;
    let retained_source_skipped_regions =
        source.skipped_regions.len().min(available_skipped_regions);
    let target_non_extensions = normalized_record_count(target)?
        .checked_sub(target.extensions.len())
        .ok_or(FirstSliceError::Limits)?;
    let source_non_extensions = normalized_record_count(source)?
        .checked_sub(source.extensions.len())
        .and_then(|total| total.checked_sub(source.skipped_regions.len()))
        .and_then(|total| total.checked_add(retained_source_skipped_regions))
        .ok_or(FirstSliceError::Limits)?;
    let maximum_merged_extensions = target
        .extensions
        .len()
        .checked_add(source.extensions.len())
        .ok_or(FirstSliceError::Limits)?
        .min(limits.max_extensions);
    let maximum_total = target_non_extensions
        .checked_add(source_non_extensions)
        .and_then(|total| total.checked_add(maximum_merged_extensions))
        .ok_or(FirstSliceError::Limits)?;
    if maximum_total > limits.max_total_records {
        return Err(resource_limit(
            FirstSliceResource::Records,
            maximum_total,
            limits.max_total_records,
        ));
    }
    Ok(())
}

fn retain_project_capacity_summary(
    document: &mut NormalizedIrDocument,
) -> Result<(), FirstSliceError> {
    document.entities = Vec::new();
    document.occurrences = Vec::new();
    document.relations = Vec::new();
    document.source_mappings = Vec::new();
    document.skipped_regions = Vec::new();
    document.diagnostics = Vec::new();
    // File claims remain required for generation identity verification. Other
    // extensions can name entities or mappings omitted by this summary.
    document.extensions = std::mem::take(&mut document.extensions)
        .into_iter()
        .filter(|extension| extension.namespace == FILE_IDENTITY_CLAIM_NAMESPACE)
        .collect();
    for coverage in &mut document.coverage_records {
        coverage.status = CoverageStatus::Bounded;
        if !matches!(
            coverage.domain,
            IrFactDomain::Files | IrFactDomain::Provenance
        ) {
            coverage.indexed = 0;
            coverage.skipped = coverage.discovered;
        }
        coverage.evidence.derivation.clear();
        coverage.id = derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
    }
    Ok(())
}

fn validate_project_capacity_summary(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    // Callers preflight first so aggregate collection exhaustion keeps its
    // typed capacity error; a broken retained reference instead triggers fallback.
    rootlight_ir::validate_ir_document(document, limits, &ExtensionSupport::default())
        .map_err(|_| FirstSliceError::Identity)
}

fn truncate_skipped_regions(
    target: &NormalizedIrDocument,
    source: &mut NormalizedIrDocument,
    limits: &IrLimits,
) -> Result<u64, FirstSliceError> {
    let available = limits
        .max_skipped_regions
        .checked_sub(target.skipped_regions.len())
        .ok_or_else(|| {
            resource_limit(
                FirstSliceResource::SkippedRegions,
                target.skipped_regions.len(),
                limits.max_skipped_regions,
            )
        })?;
    let retained = source.skipped_regions.len().min(available);
    let truncated = source
        .skipped_regions
        .len()
        .checked_sub(retained)
        .ok_or(FirstSliceError::Limits)?;
    source.skipped_regions.truncate(retained);
    u64::try_from(truncated).map_err(|_| FirstSliceError::Limits)
}

fn truncate_aggregate_diagnostics(
    target: &mut NormalizedIrDocument,
    source: &mut NormalizedIrDocument,
    append_state: &DocumentAppendState,
    limits: &IrLimits,
) -> Result<u64, FirstSliceError> {
    let mut available = limits
        .max_diagnostics
        .checked_sub(target.diagnostics.len())
        .ok_or_else(|| {
            resource_limit(
                FirstSliceResource::Diagnostics,
                target.diagnostics.len(),
                limits.max_diagnostics,
            )
        })?;
    let requested_total = source
        .diagnostics
        .len()
        .checked_add(usize::from(append_state.truncated_diagnostics > 0))
        .ok_or(FirstSliceError::Limits)?;
    if requested_total <= available {
        return Ok(0);
    }

    let mut truncated_by_file = BTreeMap::<FileId, u64>::new();
    let mut source_free = 0_u64;
    let mut truncated = 0_usize;
    if available == 0 {
        let replacement = target
            .diagnostics
            .iter()
            .rposition(|diagnostic| !is_priority_aggregate_diagnostic(&diagnostic.code))
            .ok_or(FirstSliceError::Limits)?;
        let diagnostic = target.diagnostics.remove(replacement);
        record_truncated_diagnostic(&diagnostic, &mut truncated_by_file, &mut source_free)?;
        truncated = 1;
        available = 1;
        target.coverage_records = adjusted_diagnostic_coverage(
            &target.coverage_records,
            &truncated_by_file,
            source_free,
        )?;
        truncated_by_file.clear();
        source_free = 0;
    }

    let retained = available.saturating_sub(1).min(source.diagnostics.len());
    truncated = truncated
        .checked_add(
            source
                .diagnostics
                .len()
                .checked_sub(retained)
                .ok_or(FirstSliceError::Limits)?,
        )
        .ok_or(FirstSliceError::Limits)?;
    for diagnostic in source.diagnostics.drain(retained..) {
        record_truncated_diagnostic(&diagnostic, &mut truncated_by_file, &mut source_free)?;
    }
    source.coverage_records =
        adjusted_diagnostic_coverage(&source.coverage_records, &truncated_by_file, source_free)?;
    u64::try_from(truncated).map_err(|_| FirstSliceError::Limits)
}

fn record_truncated_diagnostic(
    diagnostic: &DiagnosticRecord,
    truncated_by_file: &mut BTreeMap<FileId, u64>,
    source_free: &mut u64,
) -> Result<(), FirstSliceError> {
    if let Some(source) = diagnostic.source.as_ref() {
        let truncated = truncated_by_file.entry(source.span().file()).or_default();
        *truncated = truncated.checked_add(1).ok_or(FirstSliceError::Limits)?;
    } else {
        *source_free = source_free.checked_add(1).ok_or(FirstSliceError::Limits)?;
    }
    Ok(())
}

fn truncate_optional_extensions(
    target: &mut NormalizedIrDocument,
    source: &mut NormalizedIrDocument,
    append_state: &DocumentAppendState,
    truncated_diagnostics: u64,
    truncated_skipped_regions: u64,
    limits: &IrLimits,
) -> Result<DocumentAppendState, FirstSliceError> {
    let required_count = source
        .extensions
        .iter()
        .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE)
        .count();
    let required_bytes = extension_payload_bytes(
        source
            .extensions
            .iter()
            .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE),
    )?;
    let retained_required_count = target
        .extensions
        .iter()
        .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE)
        .count();
    let retained_required_bytes = extension_payload_bytes(
        target
            .extensions
            .iter()
            .filter(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE),
    )?;
    let observed_required_count = retained_required_count
        .checked_add(required_count)
        .ok_or(FirstSliceError::Limits)?;
    if observed_required_count > limits.max_extensions {
        return Err(resource_limit(
            FirstSliceResource::Extensions,
            observed_required_count,
            limits.max_extensions,
        ));
    }
    let observed_required_bytes = retained_required_bytes
        .checked_add(required_bytes)
        .ok_or(FirstSliceError::Limits)?;
    if observed_required_bytes > limits.max_total_extension_bytes {
        return Err(resource_limit(
            FirstSliceResource::ExtensionBytes,
            observed_required_bytes,
            limits.max_total_extension_bytes,
        ));
    }

    let mut dropped_from_target = BTreeMap::<FileId, u64>::new();
    let mut dropped_target_indices = Vec::new();
    dropped_target_indices
        .try_reserve_exact(
            target
                .extensions
                .len()
                .checked_sub(retained_required_count)
                .ok_or(FirstSliceError::Limits)?,
        )
        .map_err(|_| FirstSliceError::Limits)?;
    let mut target_extension_count = target.extensions.len();
    let mut target_extension_bytes = append_state.extension_payload_bytes;
    let mut target_optional_count = target
        .extensions
        .len()
        .checked_sub(retained_required_count)
        .ok_or(FirstSliceError::Limits)?;
    let mut target_optional_bytes = append_state
        .extension_payload_bytes
        .checked_sub(retained_required_bytes)
        .ok_or(FirstSliceError::Limits)?;
    let mut search_before = target.extensions.len();
    while target_extension_count
        .checked_add(required_count)
        .is_none_or(|observed| observed > limits.max_extensions)
        || target_extension_bytes
            .checked_add(required_bytes)
            .is_none_or(|observed| observed > limits.max_total_extension_bytes)
        || target_optional_count > MAX_RETAINED_OPTIONAL_EXTENSIONS
        || target_optional_bytes > MAX_RETAINED_OPTIONAL_EXTENSION_BYTES
    {
        let index = target
            .extensions
            .get(..search_before)
            .ok_or(FirstSliceError::Limits)?
            .iter()
            .rposition(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
            .ok_or(FirstSliceError::Limits)?;
        search_before = index;
        let extension = &target.extensions[index];
        target_extension_count = target_extension_count
            .checked_sub(1)
            .ok_or(FirstSliceError::Limits)?;
        target_extension_bytes = target_extension_bytes
            .checked_sub(extension.payload.len())
            .ok_or(FirstSliceError::Limits)?;
        target_optional_count = target_optional_count
            .checked_sub(1)
            .ok_or(FirstSliceError::Limits)?;
        target_optional_bytes = target_optional_bytes
            .checked_sub(extension.payload.len())
            .ok_or(FirstSliceError::Limits)?;
        let file = extension_file(extension)?;
        let dropped = dropped_from_target.entry(file).or_default();
        *dropped = dropped.checked_add(1).ok_or(FirstSliceError::Limits)?;
        dropped_target_indices.push(index);
    }
    let adjusted_target_coverage = (!dropped_from_target.is_empty())
        .then(|| adjusted_extension_coverage(&target.coverage_records, &dropped_from_target))
        .transpose()?;

    let available_count = limits
        .max_extensions
        .checked_sub(target_extension_count)
        .ok_or(FirstSliceError::Limits)?;
    let available_bytes = limits
        .max_total_extension_bytes
        .checked_sub(target_extension_bytes)
        .ok_or(FirstSliceError::Limits)?;
    let available_optional_count = available_count
        .checked_sub(required_count)
        .ok_or(FirstSliceError::Limits)?
        .min(
            MAX_RETAINED_OPTIONAL_EXTENSIONS
                .checked_sub(target_optional_count)
                .ok_or(FirstSliceError::Limits)?,
        );
    let available_optional_bytes = available_bytes
        .checked_sub(required_bytes)
        .ok_or(FirstSliceError::Limits)?
        .min(
            MAX_RETAINED_OPTIONAL_EXTENSION_BYTES
                .checked_sub(target_optional_bytes)
                .ok_or(FirstSliceError::Limits)?,
        );
    let mut retained = Vec::new();
    retained
        .try_reserve_exact(source.extensions.len().min(available_count))
        .map_err(|_| FirstSliceError::Limits)?;
    let mut retained_optional_count = 0_usize;
    let mut retained_optional_bytes = 0_usize;
    let mut retained_bytes = 0_usize;
    let mut dropped_by_file = BTreeMap::<FileId, u64>::new();

    for extension in source.extensions.drain(..) {
        let payload_bytes = extension.payload.len();
        let optional = extension.namespace == LEXICAL_EXTENSION_NAMESPACE;
        let next_optional_bytes = retained_optional_bytes
            .checked_add(payload_bytes)
            .ok_or(FirstSliceError::Limits)?;
        let keep = !optional
            || (retained_optional_count < available_optional_count
                && next_optional_bytes <= available_optional_bytes);
        if keep {
            if optional {
                retained_optional_count = retained_optional_count
                    .checked_add(1)
                    .ok_or(FirstSliceError::Limits)?;
                retained_optional_bytes = next_optional_bytes;
            }
            retained_bytes = retained_bytes
                .checked_add(payload_bytes)
                .ok_or(FirstSliceError::Limits)?;
            retained.push(extension);
            continue;
        }

        let file = extension_file(&extension)?;
        let dropped = dropped_by_file.entry(file).or_default();
        *dropped = dropped.checked_add(1).ok_or(FirstSliceError::Limits)?;
    }
    let adjusted_source_coverage = (!dropped_by_file.is_empty())
        .then(|| adjusted_extension_coverage(&source.coverage_records, &dropped_by_file))
        .transpose()?;
    let truncated_extensions = dropped_from_target
        .values()
        .chain(dropped_by_file.values())
        .try_fold(0_u64, |total, dropped| {
            total.checked_add(*dropped).ok_or(FirstSliceError::Limits)
        })?;
    let next_state = DocumentAppendState {
        extension_payload_bytes: target_extension_bytes
            .checked_add(retained_bytes)
            .ok_or(FirstSliceError::Limits)?,
        truncated_diagnostics: append_state
            .truncated_diagnostics
            .checked_add(truncated_diagnostics)
            .ok_or(FirstSliceError::Limits)?,
        truncated_extensions: append_state
            .truncated_extensions
            .checked_add(truncated_extensions)
            .ok_or(FirstSliceError::Limits)?,
        truncated_skipped_regions: append_state
            .truncated_skipped_regions
            .checked_add(truncated_skipped_regions)
            .ok_or(FirstSliceError::Limits)?,
    };

    for index in dropped_target_indices {
        target.extensions.remove(index);
    }
    if let Some(coverage_records) = adjusted_target_coverage {
        target.coverage_records = coverage_records;
    }
    source.extensions = retained;
    if let Some(coverage_records) = adjusted_source_coverage {
        source.coverage_records = coverage_records;
    }
    Ok(next_state)
}

fn extension_payload_bytes<'a>(
    mut extensions: impl Iterator<Item = &'a rootlight_ir::ExtensionEnvelope>,
) -> Result<usize, FirstSliceError> {
    extensions.try_fold(0_usize, |total, extension| {
        total
            .checked_add(extension.payload.len())
            .ok_or(FirstSliceError::Limits)
    })
}

fn extension_file(extension: &rootlight_ir::ExtensionEnvelope) -> Result<FileId, FirstSliceError> {
    extension
        .evidence
        .source
        .as_ref()
        .map(|source| source.span().file())
        .ok_or(FirstSliceError::Identity)
}

fn adjusted_extension_coverage(
    existing: &[rootlight_ir::CoverageRecord],
    dropped_by_file: &BTreeMap<FileId, u64>,
) -> Result<Vec<rootlight_ir::CoverageRecord>, FirstSliceError> {
    let mut coverage_records = existing.to_vec();
    for (file, dropped) in dropped_by_file {
        let mut matching_records = coverage_records.iter_mut().filter(|coverage| {
            coverage.domain == rootlight_ir::FactDomain::Extensions
                && coverage.scope == rootlight_ir::CoverageScope::File(*file)
        });
        let coverage = matching_records.next().ok_or(FirstSliceError::Identity)?;
        if matching_records.next().is_some() {
            return Err(FirstSliceError::Identity);
        }
        coverage.indexed = coverage
            .indexed
            .checked_sub(*dropped)
            .ok_or(FirstSliceError::Identity)?;
        coverage.skipped = coverage
            .skipped
            .checked_add(*dropped)
            .ok_or(FirstSliceError::Limits)?;
        if coverage.status != CoverageStatus::Unknown {
            coverage.status = CoverageStatus::Bounded;
        }
        coverage.id = derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
    }
    Ok(coverage_records)
}

fn adjusted_diagnostic_coverage(
    existing: &[CoverageRecord],
    dropped_by_file: &BTreeMap<FileId, u64>,
    source_free_dropped: u64,
) -> Result<Vec<CoverageRecord>, FirstSliceError> {
    let mut coverage_records = existing.to_vec();
    for (file, dropped) in dropped_by_file {
        let mut matching_records = coverage_records.iter_mut().filter(|coverage| {
            coverage.domain == IrFactDomain::Diagnostics
                && coverage.scope == CoverageScope::File(*file)
        });
        let coverage = matching_records.next().ok_or(FirstSliceError::Identity)?;
        if matching_records.next().is_some() {
            return Err(FirstSliceError::Identity);
        }
        coverage.indexed = coverage
            .indexed
            .checked_sub(*dropped)
            .ok_or(FirstSliceError::Identity)?;
        coverage.skipped = coverage
            .skipped
            .checked_add(*dropped)
            .ok_or(FirstSliceError::Limits)?;
        if coverage.status != CoverageStatus::Unknown {
            coverage.status = CoverageStatus::Bounded;
        }
        coverage.id = derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
    }
    if source_free_dropped > 0 {
        for coverage in &mut coverage_records {
            if coverage.domain == IrFactDomain::Diagnostics
                && coverage.status != CoverageStatus::Unknown
            {
                coverage.status = CoverageStatus::Bounded;
                coverage.id =
                    derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
            }
        }
    }
    Ok(coverage_records)
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

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct LogicalLexicalDocument<'document> {
    symbol_id: Option<SymbolId>,
    file_id: FileId,
    identifier: &'document str,
    qualified_name: &'document str,
    path: &'document str,
    kind: &'document str,
    language: &'document str,
    tier: &'document str,
    package: &'document Option<String>,
    build_target: &'document Option<String>,
    signature: &'document Option<String>,
    type_names: &'document [String],
    documentation: &'document Option<String>,
    source_identifiers: &'document [String],
    source_text: &'document Option<String>,
    generated: bool,
    test: bool,
    declaration_only: bool,
}

impl<'document> From<&'document LexicalDocument> for LogicalLexicalDocument<'document> {
    fn from(document: &'document LexicalDocument) -> Self {
        Self {
            symbol_id: document.symbol_id,
            file_id: document.file_id,
            identifier: &document.identifier,
            qualified_name: &document.qualified_name,
            path: &document.path,
            kind: &document.kind,
            language: &document.language,
            tier: &document.tier,
            package: &document.package,
            build_target: &document.build_target,
            signature: &document.signature,
            type_names: &document.type_names,
            documentation: &document.documentation,
            source_identifiers: &document.source_identifiers,
            source_text: &document.source_text,
            generated: document.generated,
            test: document.test,
            declaration_only: document.declaration_only,
        }
    }
}

struct LogicalLexicalProjection<'document>(&'document [LexicalDocument]);

impl Serialize for LogicalLexicalProjection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for document in self.0 {
            sequence.serialize_element(&LogicalLexicalDocument::from(document))?;
        }
        sequence.end()
    }
}

struct LogicalDiscoveryProjection<'inputs>(&'inputs InputSnapshot);

impl Serialize for LogicalDiscoveryProjection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for input in self.0.iter() {
            sequence.serialize_element(&input)?;
        }
        sequence.end()
    }
}

fn logical_component(
    domain: LogicalDomain,
    value: &impl Serialize,
    records: u64,
    cancellation: &Cancellation,
) -> Result<LogicalComponent, FirstSliceError> {
    LogicalComponent::from_canonical_value(
        domain,
        value,
        records,
        MAX_LOGICAL_SNAPSHOT_COMPONENT_BYTES,
        cancellation,
    )
    .map_err(|error| map_incremental_error(error, cancellation))
}

fn logical_snapshot_identity(
    normalized_ir: CanonicalGenerationNeutralDigests,
    lexical_documents: &[LexicalDocument],
    inputs: &InputSnapshot,
    cancellation: &Cancellation,
) -> Result<FirstSliceLogicalSnapshotIdentity, FirstSliceError> {
    check_cancellation(cancellation)?;
    let query_projection = LogicalLexicalProjection(lexical_documents);
    let discovery_inputs = LogicalDiscoveryProjection(inputs);
    let normalized_ir_component = logical_component(
        LogicalDomain::NormalizedIr,
        &normalized_ir.complete(),
        normalized_ir.records(),
        cancellation,
    )?;
    // LogicalStore is the backend-neutral intended store projection. Catalog
    // and recovery round-trip correctness is tested separately, so ordinary
    // publication does not traverse the same generation twice.
    let logical_store_component = logical_component(
        LogicalDomain::LogicalStore,
        &normalized_ir.complete(),
        normalized_ir.records(),
        cancellation,
    )?;
    let components = [
        logical_component(
            LogicalDomain::Discovery,
            &discovery_inputs,
            u64::try_from(inputs.len()).map_err(|_| FirstSliceError::Limits)?,
            cancellation,
        )?,
        normalized_ir_component,
        logical_store_component,
        logical_component(
            LogicalDomain::QueryOutputs,
            &query_projection,
            u64::try_from(lexical_documents.len()).map_err(|_| FirstSliceError::Limits)?,
            cancellation,
        )?,
        logical_component(
            LogicalDomain::Coverage,
            &normalized_ir.coverage(),
            normalized_ir.coverage_records(),
            cancellation,
        )?,
        logical_component(
            LogicalDomain::Provenance,
            &normalized_ir.provenance(),
            normalized_ir.provenance_records(),
            cancellation,
        )?,
        logical_component(
            LogicalDomain::StableIds,
            &normalized_ir.stable_identities(),
            normalized_ir.stable_identities_records(),
            cancellation,
        )?,
    ];
    let snapshot = EquivalenceSnapshot::new(components, cancellation)
        .map_err(|error| map_incremental_error(error, cancellation))?;
    Ok(FirstSliceLogicalSnapshotIdentity::new(
        snapshot.logical_snapshot_hash(),
    ))
}

fn normalized_fact_work(
    document: &NormalizedIrDocument,
    reused_records: &ReusedNormalizedRecords,
    cancellation: &Cancellation,
) -> Result<Vec<FirstSliceNormalizedFactWork>, FirstSliceError> {
    let provenance = document
        .provenance
        .iter()
        .map(|record| (record.id, record))
        .collect::<BTreeMap<_, _>>();
    let mut groups =
        BTreeMap::<(IrFactDomain, String, FirstSliceFactWorkCause), (u64, BTreeSet<FileId>)>::new();
    let mut record = |domain: IrFactDomain,
                      provenance_id: FactId,
                      files: BTreeSet<FileId>,
                      reused: bool|
     -> Result<(), FirstSliceError> {
        check_cancellation(cancellation)?;
        let producer = provenance
            .get(&provenance_id)
            .ok_or(FirstSliceError::Identity)?
            .producer
            .name();
        let (provider_pass, rebuilt_cause) = normalized_provider_work(producer);
        let cause = if reused {
            FirstSliceFactWorkCause::CompleteDependencyMatch
        } else {
            rebuilt_cause
        };
        let group = groups
            .entry((domain, provider_pass.to_owned(), cause))
            .or_default();
        group.0 = group.0.checked_add(1).ok_or(FirstSliceError::Limits)?;
        group.1.extend(files);
        Ok(())
    };
    let evidence_files =
        |evidence: &FactEvidence| evidence_file(evidence).into_iter().collect::<BTreeSet<_>>();

    for file in &document.files {
        record(
            IrFactDomain::Files,
            file.provenance,
            BTreeSet::from([file.id]),
            reused_records.contains_file(file),
        )?;
    }
    for entity in &document.entities {
        record(
            IrFactDomain::Entities,
            entity.provenance,
            evidence_files(&entity.evidence),
            reused_records.contains_entity(entity),
        )?;
    }
    for occurrence in &document.occurrences {
        record(
            IrFactDomain::Occurrences,
            occurrence.provenance,
            BTreeSet::from([occurrence.file]),
            reused_records.contains_occurrence(occurrence),
        )?;
    }
    for relation in &document.relations {
        record(
            IrFactDomain::Relations,
            relation.provenance,
            evidence_files(&relation.evidence),
            reused_records.contains_relation(relation),
        )?;
    }
    for provenance_record in &document.provenance {
        let files = provenance_record
            .input_sources
            .iter()
            .chain(&provenance_record.evidence_sources)
            .map(|source| source.span().file())
            .collect();
        record(
            IrFactDomain::Provenance,
            provenance_record.id,
            files,
            reused_records.contains_provenance(provenance_record),
        )?;
    }
    for mapping in &document.source_mappings {
        record(
            IrFactDomain::SourceMappings,
            mapping.provenance,
            BTreeSet::from([mapping.from.span().file(), mapping.to.span().file()]),
            reused_records.contains_source_mapping(mapping),
        )?;
    }
    for coverage in &document.coverage_records {
        let mut files = evidence_files(&coverage.evidence);
        if let CoverageScope::File(file) = coverage.scope {
            files.insert(file);
        }
        record(
            coverage.domain,
            coverage.provenance,
            files,
            reused_records.contains_coverage(coverage),
        )?;
    }
    for skipped in &document.skipped_regions {
        record(
            IrFactDomain::Diagnostics,
            skipped.provenance,
            BTreeSet::from([skipped.source.span().file()]),
            reused_records.contains_skipped(skipped),
        )?;
    }
    for diagnostic in &document.diagnostics {
        record(
            IrFactDomain::Diagnostics,
            diagnostic.provenance,
            evidence_files(&diagnostic.evidence),
            reused_records.contains_diagnostic(diagnostic),
        )?;
    }
    for extension in &document.extensions {
        record(
            IrFactDomain::Extensions,
            extension.provenance,
            evidence_files(&extension.evidence),
            reused_records.contains_extension(extension),
        )?;
    }

    groups
        .into_iter()
        .map(|((domain, provider_pass, cause), (facts, files))| {
            Ok(FirstSliceNormalizedFactWork {
                domain,
                provider_pass,
                cause,
                files: u64::try_from(files.len()).map_err(|_| FirstSliceError::Limits)?,
                facts,
                file_ids: files,
            })
        })
        .collect()
}

fn normalized_provider_work(producer: &str) -> (&str, FirstSliceFactWorkCause) {
    if producer == RESOLVER_PROVIDER_NAME {
        (RESOLVER_PASS_ID, FirstSliceFactWorkCause::Resolution)
    } else if producer == "rootlight-first-slice-treesitter" {
        (
            LOWERING_PASS_ID,
            FirstSliceFactWorkCause::GenerationBoundLowering,
        )
    } else {
        (producer, FirstSliceFactWorkCause::GenerationBoundLowering)
    }
}

fn index_diagnostic_summaries(
    document: &NormalizedIrDocument,
) -> Result<Vec<FirstSliceIndexDiagnostic>, FirstSliceError> {
    const REDACTED_CODE: &str = "diagnostic-redacted";
    const REDACTED_MESSAGE: &str = "an index diagnostic was omitted because it was not source free";
    const TRUNCATED_CODE: &str = "diagnostics-truncated";
    const TRUNCATED_MESSAGE: &str =
        "additional index diagnostics were omitted by the response limit";

    let mut summaries = BTreeSet::new();
    for diagnostic in &document.diagnostics {
        let summary = if public_diagnostic_code(&diagnostic.code)
            && public_diagnostic_message(&diagnostic.message)
        {
            FirstSliceIndexDiagnostic {
                code: diagnostic.code.clone(),
                message: diagnostic.message.clone(),
            }
        } else {
            FirstSliceIndexDiagnostic {
                code: REDACTED_CODE.to_owned(),
                message: REDACTED_MESSAGE.to_owned(),
            }
        };
        summaries.insert(summary);
    }

    let truncated = summaries.len() > MAX_FIRST_SLICE_INDEX_DIAGNOSTICS;
    let retained = if truncated {
        MAX_FIRST_SLICE_INDEX_DIAGNOSTICS.saturating_sub(1)
    } else {
        summaries.len()
    };
    let mut result = Vec::new();
    result
        .try_reserve_exact(retained.saturating_add(usize::from(truncated)))
        .map_err(|_| FirstSliceError::Limits)?;
    result.extend(summaries.into_iter().take(retained));
    if truncated {
        result.push(FirstSliceIndexDiagnostic {
            code: TRUNCATED_CODE.to_owned(),
            message: TRUNCATED_MESSAGE.to_owned(),
        });
        result.sort();
    }
    Ok(result)
}

fn public_diagnostic_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

fn public_diagnostic_message(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b' ' | b'-')
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

fn reserve_resource_records<T>(
    target: &mut Vec<T>,
    additional: usize,
    maximum: usize,
    resource: FirstSliceResource,
) -> Result<(), FirstSliceError> {
    checked_resource_length(target.len(), additional, maximum, resource)?;
    target
        .try_reserve(additional)
        .map_err(|_| FirstSliceError::Limits)
}

fn checked_resource_length(
    current: usize,
    additional: usize,
    maximum: usize,
    resource: FirstSliceResource,
) -> Result<usize, FirstSliceError> {
    let observed = current
        .checked_add(additional)
        .ok_or(FirstSliceError::Limits)?;
    if observed > maximum {
        return Err(resource_limit(resource, observed, maximum));
    }
    Ok(observed)
}

fn resource_limit(resource: FirstSliceResource, observed: usize, limit: usize) -> FirstSliceError {
    FirstSliceError::ResourceLimit {
        resource,
        observed: u64::try_from(observed).unwrap_or(u64::MAX),
        limit: u64::try_from(limit).unwrap_or(u64::MAX),
    }
}

fn estimated_resource_limit(
    resource: FirstSliceResource,
    estimated: u64,
    limit: u64,
) -> FirstSliceError {
    FirstSliceError::EstimatedResourceLimit {
        resource,
        estimated,
        limit,
    }
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
        DiscoveryError::InvalidLimits => FirstSliceError::Limits,
        DiscoveryError::InvalidPolicy | DiscoveryError::InvalidPattern { .. } => {
            FirstSliceError::Configuration
        }
        DiscoveryError::EntryLimit { maximum } => resource_limit(
            FirstSliceResource::DiscoveryEntries,
            maximum.saturating_add(1),
            maximum,
        ),
        DiscoveryError::RetainedSnapshotByteLimit { observed, maximum } => {
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::SourceBytes,
                observed,
                limit: maximum,
            }
        }
        DiscoveryError::Vfs(error) => map_vfs_error(error, cancellation),
        DiscoveryError::Cancelled(cancelled) => FirstSliceError::Cancelled(cancelled.reason()),
        DiscoveryError::Incremental(error) => map_incremental_error(error, cancellation),
        DiscoveryError::IncrementalDrift => FirstSliceError::DiscoveryDrift,
        DiscoveryError::SerializeManifest(_) => FirstSliceError::Discovery,
    }
}

fn repository_capacity_limit(
    observed: usize,
    limit: usize,
    configuration: FirstSliceRepositoryCapacityConfiguration,
    pending_repositories: usize,
    reclaimable_repositories: usize,
) -> FirstSliceError {
    FirstSliceError::RepositoryCapacityLimit {
        observed: u64::try_from(observed)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        limit: u64::try_from(limit).unwrap_or(u64::MAX),
        configuration,
        pending_repositories: u64::try_from(pending_repositories).unwrap_or(u64::MAX),
        reclaimable_repositories: u64::try_from(reclaimable_repositories).unwrap_or(u64::MAX),
    }
}

fn maximum_repositories_for_retention(
    maximum_generations: usize,
    configured_maximum_repositories: usize,
) -> Result<usize, FirstSliceError> {
    // One slot remains available for transactional publication while every
    // configured repository retains its complete recovery generations.
    HARD_MAX_FIRST_SLICE_GENERATIONS
        .checked_sub(1)
        .and_then(|capacity| capacity.checked_div(maximum_generations))
        .map(|capacity| capacity.min(configured_maximum_repositories))
        .filter(|capacity| *capacity > 0)
        .ok_or(FirstSliceError::Retention)
}

fn map_incremental_error(error: IncrementalError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        IncrementalError::Cancelled(cancelled) => FirstSliceError::Cancelled(cancelled.reason()),
        IncrementalError::InvalidLimit { .. } => FirstSliceError::Limits,
        IncrementalError::ResourceLimit {
            resource,
            observed,
            limit,
        } => resource_limit(incremental_resource(resource), observed, limit),
        IncrementalError::InvalidPassId
        | IncrementalError::DuplicateInput { .. }
        | IncrementalError::DuplicateArtifact { .. }
        | IncrementalError::DuplicateArtifactOutput { .. }
        | IncrementalError::DuplicateArtifactDependency { .. }
        | IncrementalError::EmptyArtifactPart { .. }
        | IncrementalError::ArtifactDependencyMismatch { .. }
        | IncrementalError::ArtifactUnknownOutput { .. }
        | IncrementalError::DuplicatePass { .. }
        | IncrementalError::EmptyPassOutputs { .. }
        | IncrementalError::UndeclaredInputKind { .. }
        | IncrementalError::UndeclaredInputDomain { .. }
        | IncrementalError::UndeclaredOutputDomain { .. }
        | IncrementalError::UnknownPass { .. }
        | IncrementalError::UnknownFactNode { .. }
        | IncrementalError::DuplicateFile { .. }
        | IncrementalError::PathIdentityCollision { .. }
        | IncrementalError::MissingHash { .. }
        | IncrementalError::UnexpectedHash { .. }
        | IncrementalError::DuplicateLogicalDomain { .. }
        | IncrementalError::MissingLogicalDomain { .. }
        | IncrementalError::LogicalInequality
        | IncrementalError::SerializeTrace(_)
        | IncrementalError::DuplicateIdentity { .. } => FirstSliceError::Incremental,
        _ => FirstSliceError::Incremental,
    }
}

fn map_vfs_error(error: VfsError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        VfsError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        VfsError::PathTooLong { maximum } => estimated_resource_limit(
            FirstSliceResource::PathBytes,
            u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1),
            u64::try_from(maximum).unwrap_or(u64::MAX),
        ),
        VfsError::TooManyPathComponents { maximum } => estimated_resource_limit(
            FirstSliceResource::PathComponents,
            u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1),
            u64::try_from(maximum).unwrap_or(u64::MAX),
        ),
        VfsError::DirectoryEntryLimit { maximum } => resource_limit(
            FirstSliceResource::Files,
            maximum.saturating_add(1),
            maximum,
        ),
        VfsError::FileTooLarge { maximum } => estimated_resource_limit(
            FirstSliceResource::SourceBytes,
            maximum.saturating_add(1),
            maximum,
        ),
        VfsError::InvalidByteLimit => FirstSliceError::Limits,
        VfsError::MemoryUnavailable => FirstSliceError::ResourceUnavailable {
            resource: FirstSliceResource::MemoryBytes,
        },
        VfsError::OpenRoot { .. }
        | VfsError::OpenDirectory { .. }
        | VfsError::ReadDirectory { .. }
        | VfsError::OpenFile { .. }
        | VfsError::ReadFile { .. } => FirstSliceError::RepositoryIo,
        VfsError::RootNotDirectory => FirstSliceError::Repository,
        VfsError::LinkedPath
        | VfsError::NotRegularFile
        | VfsError::UnstableFile
        | VfsError::StaleContentHash => FirstSliceError::DiscoveryDrift,
        VfsError::InvalidRelativePath
        | VfsError::InvalidRootPath
        | VfsError::SourceReferenceMismatch
        | VfsError::InvalidSourceSpan
        | VfsError::PersistedFileIdentityMismatch
        | VfsError::PersistedContentHashMismatch => FirstSliceError::Discovery,
    }
}

const fn incremental_resource(resource: rootlight_incremental::ResourceKind) -> FirstSliceResource {
    match resource {
        rootlight_incremental::ResourceKind::Files => FirstSliceResource::IncrementalFiles,
        rootlight_incremental::ResourceKind::Inputs => FirstSliceResource::IncrementalInputs,
        rootlight_incremental::ResourceKind::Artifacts => FirstSliceResource::IncrementalArtifacts,
        rootlight_incremental::ResourceKind::ArtifactOutputs => {
            FirstSliceResource::IncrementalArtifactOutputs
        }
        rootlight_incremental::ResourceKind::Passes => FirstSliceResource::IncrementalPasses,
        rootlight_incremental::ResourceKind::DependencyNodes => {
            FirstSliceResource::IncrementalDependencyNodes
        }
        rootlight_incremental::ResourceKind::DependencyEdges => {
            FirstSliceResource::IncrementalDependencyEdges
        }
        rootlight_incremental::ResourceKind::ClosureWork => {
            FirstSliceResource::IncrementalClosureWork
        }
        rootlight_incremental::ResourceKind::TraceEntries => {
            FirstSliceResource::IncrementalTraceEntries
        }
        rootlight_incremental::ResourceKind::LogicalBytes => {
            FirstSliceResource::IncrementalLogicalBytes
        }
    }
}

fn is_invalid_utf8_adapter_failure(error: &AdapterError) -> bool {
    matches!(
        error,
        AdapterError::ProviderFailed { code }
            if matches!(
                code.as_str(),
                "invalid-utf8" | "treesitter-lowering-invalid-utf8"
            )
    )
}

fn map_adapter_error(error: AdapterError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        AdapterError::Cancelled { reason } => FirstSliceError::Cancelled(reason),
        AdapterError::RejectedRequest(RequestError::SourceTooLarge { observed, limit }) => {
            resource_limit(FirstSliceResource::SourceBytes, observed, limit)
        }
        AdapterError::RejectedRequest(RequestError::TooManyIncludedRanges { observed, limit }) => {
            resource_limit(FirstSliceResource::IncludedRanges, observed, limit)
        }
        AdapterError::RejectedRequest(RequestError::ProjectLimit {
            resource,
            observed,
            limit,
        }) => adapter_resource(resource).map_or(FirstSliceError::Adapter, |resource| {
            resource_limit(resource, observed, limit)
        }),
        AdapterError::RejectedRequest(RequestError::ProviderLimit {
            resource,
            observed,
            limit,
        })
        | AdapterError::Sink(SinkError::BatchLimit {
            resource,
            observed,
            limit,
        })
        | AdapterError::Sink(SinkError::StreamLimit {
            resource,
            observed,
            limit,
        })
        | AdapterError::InvalidReport(ReportError::ResourceLimit {
            resource,
            observed,
            limit,
        }) => adapter_resource(resource).map_or(FirstSliceError::Adapter, |resource| {
            resource_limit(resource, observed, limit)
        }),
        _ => FirstSliceError::Adapter,
    }
}

const fn adapter_resource(resource: ResourceKind) -> Option<FirstSliceResource> {
    match resource {
        ResourceKind::Batches => Some(FirstSliceResource::Batches),
        ResourceKind::Records => Some(FirstSliceResource::Records),
        ResourceKind::OutputBytes => Some(FirstSliceResource::OutputBytes),
        ResourceKind::Diagnostics => Some(FirstSliceResource::Diagnostics),
        ResourceKind::DiagnosticBytes => Some(FirstSliceResource::DiagnosticBytes),
        ResourceKind::StringBytes => Some(FirstSliceResource::StringBytes),
        ResourceKind::NestedItems => Some(FirstSliceResource::NestedItems),
        ResourceKind::ExtensionBytes => Some(FirstSliceResource::ExtensionBytes),
        ResourceKind::SourceBytes => Some(FirstSliceResource::SourceBytes),
        ResourceKind::ProjectFiles => Some(FirstSliceResource::ProjectFiles),
        ResourceKind::ProjectSourceBytes => Some(FirstSliceResource::ProjectSourceBytes),
        ResourceKind::ProjectContextBytes => Some(FirstSliceResource::ProjectContextBytes),
        ResourceKind::GeneratedMappings => Some(FirstSliceResource::GeneratedMappings),
        ResourceKind::GeneratedMappingBytes => Some(FirstSliceResource::GeneratedMappingBytes),
        ResourceKind::AnalysisUnitBytes => Some(FirstSliceResource::AnalysisUnitBytes),
        ResourceKind::BuildTargetBytes => Some(FirstSliceResource::BuildTargetBytes),
        ResourceKind::IncludedRanges => Some(FirstSliceResource::IncludedRanges),
        ResourceKind::SyntaxNodes => Some(FirstSliceResource::SyntaxNodes),
        ResourceKind::SyntaxDepth => Some(FirstSliceResource::SyntaxDepth),
        ResourceKind::ReportedMemoryBytes => Some(FirstSliceResource::ReportedMemoryBytes),
        _ => None,
    }
}

fn map_resolution_error(error: ResolutionError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        ResolutionError::Cancelled(cancelled) => FirstSliceError::Cancelled(cancelled.reason()),
        ResolutionError::WorkLimit { maximum } => resource_limit(
            FirstSliceResource::ResolutionWork,
            maximum.saturating_add(1),
            maximum,
        ),
        ResolutionError::InvalidDocument(IrDocumentValidationError::CollectionLimit {
            collection,
            observed,
            limit,
        }) => ir_collection_resource(collection).map_or(FirstSliceError::Resolution, |resource| {
            resource_limit(resource, observed, limit)
        }),
        ResolutionError::InvalidDocument(IrDocumentValidationError::TotalRecordLimit {
            observed,
            limit,
        }) => resource_limit(FirstSliceResource::Records, observed, limit),
        ResolutionError::InvalidDocument(IrDocumentValidationError::TotalExtensionBytesLimit {
            observed,
            limit,
        }) => resource_limit(FirstSliceResource::ExtensionBytes, observed, limit),
        ResolutionError::InvalidDocument(IrDocumentValidationError::TotalNestedItemLimit {
            observed,
            limit,
        }) => resource_limit(FirstSliceResource::NestedItems, observed, limit),
        _ => FirstSliceError::Resolution,
    }
}

fn apply_bounded_resolution(
    mut document: NormalizedIrDocument,
    limits: ResolutionLimits,
    context: ResolverFactContext,
    cancellation: &Cancellation,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    let engine = ResolutionEngine::new(limits);
    let estimate = engine
        .estimate_work(&document, cancellation)
        .map_err(|error| map_resolution_error(error, cancellation))?;
    if !estimate.fits() {
        append_resolution_work_bounded_diagnostic(
            &mut document,
            estimate.required,
            estimate.limit,
            &IrLimits::default(),
        )?;
        return Ok(document);
    }
    engine
        .apply_document(document, context, cancellation)
        .map_err(|error| map_resolution_error(error, cancellation))
}

fn append_resolution_work_bounded_diagnostic(
    document: &mut NormalizedIrDocument,
    required: usize,
    limit: usize,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    let provenance = document
        .provenance
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    let evidence_file = document
        .files
        .first()
        .map(|record| record.id)
        .ok_or(FirstSliceError::Identity)?;
    for coverage in &mut document.coverage_records {
        if matches!(
            coverage.domain,
            IrFactDomain::Occurrences | IrFactDomain::Relations
        ) && coverage.status != CoverageStatus::Unknown
        {
            coverage.status = CoverageStatus::Bounded;
            coverage.id =
                derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
        }
    }
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        code: "resolution-work-bounded".to_owned(),
        message: format!(
            "resolution required {required} work units above the configured limit {limit}"
        ),
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(evidence_file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    retain_priority_aggregate_diagnostic(document, diagnostic, limits)
}

fn resolution_limits_for_occurrences(
    occurrence_count: usize,
) -> Result<ResolutionLimits, FirstSliceError> {
    // Preserve the documented per-site default for ordinary repositories while
    // constraining aggregate materialization and resolver work independently.
    let aggregate_limit = MAX_TOTAL_MATERIALIZED_RESOLUTION_CANDIDATES
        .checked_div(occurrence_count.max(1))
        .unwrap_or(1);
    ResolutionLimits::with_work_limit(
        aggregate_limit.clamp(1, DEFAULT_CANDIDATE_LIMIT),
        MAX_RESOLUTION_WORK_LIMIT,
    )
    .map_err(|_| FirstSliceError::Limits)
}

const fn ir_collection_resource(collection: &str) -> Option<FirstSliceResource> {
    match collection.as_bytes() {
        b"files" => Some(FirstSliceResource::Files),
        b"entities" => Some(FirstSliceResource::Entities),
        b"occurrences" => Some(FirstSliceResource::Occurrences),
        b"relations" => Some(FirstSliceResource::Relations),
        b"provenance" => Some(FirstSliceResource::Provenance),
        b"source_mappings" => Some(FirstSliceResource::SourceMappings),
        b"coverage_records" => Some(FirstSliceResource::Coverage),
        b"skipped_regions" => Some(FirstSliceResource::SkippedRegions),
        b"diagnostics" => Some(FirstSliceResource::Diagnostics),
        b"extensions" => Some(FirstSliceResource::Extensions),
        _ => None,
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
        IdentityVerificationError::Control(GenerationControlError::BudgetExceeded {
            resource,
            observed,
            limit,
        }) => generation_resource(resource).map_or(FirstSliceError::Identity, |resource| {
            FirstSliceError::ResourceLimit {
                resource,
                observed,
                limit,
            }
        }),
        IdentityVerificationError::InvalidGeneration => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::InvalidGeneration)
        }
        IdentityVerificationError::LegacyContract => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::LegacyContract)
        }
        IdentityVerificationError::MissingClaim => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::MissingClaim)
        }
        IdentityVerificationError::DuplicateClaim => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::DuplicateClaim)
        }
        IdentityVerificationError::IdentityMismatch(component) => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::IdentityMismatch(
                component,
            ))
        }
        IdentityVerificationError::ManifestMismatch => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::ManifestMismatch)
        }
        IdentityVerificationError::UnsupportedExtension => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::UnsupportedExtension)
        }
        IdentityVerificationError::RecipeEncoding => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::RecipeEncoding)
        }
    }
}

fn map_generation_neutral_digest_error(
    error: GenerationNeutralDigestError,
    cancellation: &Cancellation,
) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        GenerationNeutralDigestError::Control(GenerationControlError::Cancelled { reason }) => {
            FirstSliceError::Cancelled(reason)
        }
        GenerationNeutralDigestError::Control(GenerationControlError::BudgetExceeded {
            resource,
            observed,
            limit,
        }) => generation_resource(resource).map_or(FirstSliceError::Identity, |resource| {
            FirstSliceError::ResourceLimit {
                resource,
                observed,
                limit,
            }
        }),
        GenerationNeutralDigestError::InvalidGeneration => {
            FirstSliceError::IdentityVerification(FirstSliceIdentityFailure::InvalidGeneration)
        }
        GenerationNeutralDigestError::ResourceUnavailable => FirstSliceError::ResourceUnavailable {
            resource: FirstSliceResource::MemoryBytes,
        },
    }
}

fn map_catalog_error(error: &CatalogError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    map_catalog_error_kind(error.kind(), cancellation)
}

fn map_catalog_error_kind(kind: CatalogErrorKind, cancellation: &Cancellation) -> FirstSliceError {
    match kind {
        CatalogErrorKind::Cancelled => FirstSliceError::Cancelled(
            cancellation
                .reason()
                .unwrap_or(CancellationReason::ParentCancelled),
        ),
        CatalogErrorKind::BudgetExceeded {
            resource,
            observed,
            limit,
        } => generation_resource(resource).map_or(FirstSliceError::Catalog, |resource| {
            FirstSliceError::ResourceLimit {
                resource,
                observed,
                limit,
            }
        }),
        CatalogErrorKind::Corrupt
        | CatalogErrorKind::MigrationChecksumMismatch
        | CatalogErrorKind::InvalidGeneration
        | CatalogErrorKind::IdentityProofRequired => FirstSliceError::CatalogCorrupt,
        CatalogErrorKind::IncompatibleSchema => FirstSliceError::CatalogMigrationRequired,
        _ => FirstSliceError::Catalog,
    }
}

const fn generation_resource(resource: GenerationResource) -> Option<FirstSliceResource> {
    match resource {
        GenerationResource::Rows => Some(FirstSliceResource::GenerationRows),
        GenerationResource::SourceReferences => Some(FirstSliceResource::SourceReferences),
        GenerationResource::TextBytes => Some(FirstSliceResource::TextBytes),
        GenerationResource::EncodedTextBytes => Some(FirstSliceResource::EncodedTextBytes),
        _ => None,
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
        SourceError::InvalidBudget
        | SourceError::SelectorLimit
        | SourceError::ContextLimit
        | SourceError::SnapshotBudgetExceeded
        | SourceError::MetadataStringLimitExceeded
        | SourceError::MetadataBudgetExceeded
        | SourceError::SourceBudgetExceeded
        | SourceError::ResponseMemoryBudgetExceeded
        | SourceError::MemoryUnavailable => FirstSliceError::BudgetExceeded,
        _ => FirstSliceError::Source,
    }
}

fn map_sharing_error(error: SharedGenerationError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        SharedGenerationError::Cancelled => FirstSliceError::Cancelled(
            cancellation
                .reason()
                .unwrap_or(CancellationReason::ClientRequest),
        ),
        _ => FirstSliceError::Sharing,
    }
}

fn map_runtime_trace_error(error: RuntimeTraceImportError) -> FirstSliceError {
    match error {
        RuntimeTraceImportError::Cancelled(cancelled) => {
            FirstSliceError::Cancelled(cancelled.reason())
        }
        error => FirstSliceError::RuntimeTrace(error),
    }
}

fn map_query_error(error: QueryError, cancellation: &Cancellation) -> FirstSliceError {
    if let Some(cancelled) = current_cancellation(cancellation) {
        return cancelled;
    }
    match error {
        QueryError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        QueryError::SymbolNotFound => FirstSliceError::SymbolNotFound,
        QueryError::InvalidBudget { .. }
        | QueryError::InvalidDurationBudget { .. }
        | QueryError::PlanRejected { .. }
        | QueryError::BudgetExceeded { .. }
        | QueryError::MemoryUnavailable => FirstSliceError::BudgetExceeded,
        QueryError::Source(source) => map_source_error(source, cancellation),
        _ => FirstSliceError::Query,
    }
}

fn analysis_partition_weight(source_bytes: usize) -> Result<usize, FirstSliceError> {
    source_bytes
        .max(1)
        .checked_next_power_of_two()
        .ok_or(FirstSliceError::Limits)
}

fn partitioned_analysis_limits(
    limits: &AnalysisLimits,
    source_files: usize,
    total_source_weight: usize,
    file_source_weight: usize,
) -> Result<AnalysisLimits, FirstSliceError> {
    if source_files == 0 {
        return Ok(limits.clone());
    }
    let syntax = limits.syntax_stream();
    let record_budget = MAX_FIRST_SLICE_STRUCTURAL_FACTS.min(syntax.max_records());
    let maximum_records = if total_source_weight == 0 {
        record_budget
            .checked_div(source_files)
            .ok_or(FirstSliceError::Limits)?
    } else {
        if file_source_weight > total_source_weight {
            return Err(FirstSliceError::Limits);
        }
        // Keep small files queryable while giving large source units enough facts to
        // retain symbols that occur late in the parse stream.
        let proportional_budget = record_budget / 2;
        let even_budget = record_budget
            .checked_sub(proportional_budget)
            .ok_or(FirstSliceError::Limits)?;
        let even_records = even_budget
            .checked_div(source_files)
            .ok_or(FirstSliceError::Limits)?;
        let proportional_records = proportional_budget
            .checked_mul(file_source_weight)
            .and_then(|value| value.checked_div(total_source_weight))
            .ok_or(FirstSliceError::Limits)?;
        even_records
            .checked_add(proportional_records)
            .ok_or(FirstSliceError::Limits)?
    }
    .max(1)
    .min(syntax.max_records());
    if maximum_records == syntax.max_records() {
        return Ok(limits.clone());
    }
    let current_batch = syntax.batch();
    let batch = BatchThresholds::new(
        current_batch.max_records().min(maximum_records),
        current_batch.max_output_bytes(),
        current_batch.max_diagnostics(),
        current_batch.max_diagnostic_bytes(),
    )
    .map_err(|_| FirstSliceError::Limits)?;
    let syntax = StreamLimits::new(
        syntax.max_batches(),
        maximum_records,
        syntax.max_output_bytes(),
        syntax.max_diagnostics(),
        syntax.max_diagnostic_bytes(),
        syntax.max_string_bytes(),
        batch,
    )
    .map_err(|_| FirstSliceError::Limits)?;
    let partitioned = AnalysisLimits::new(
        limits.max_source_bytes(),
        limits.max_syntax_nodes(),
        limits.max_syntax_depth(),
        limits.max_embedded_ranges(),
        limits.max_reported_memory_bytes(),
        syntax,
        limits.ir_stream().clone(),
        limits.ir().clone(),
    )
    .map_err(|_| FirstSliceError::Limits)?;
    Ok(match limits.project() {
        Some(project) => partitioned.with_project_limits(project),
        None => partitioned,
    })
}

fn analysis_limits(maximum_source_bytes: usize) -> Result<AnalysisLimits, FirstSliceError> {
    let batch = BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024)
        .map_err(|_| FirstSliceError::Limits)?;
    // The public source ceiling admits multi-megabyte generated declarations.
    // Keep their normalized output bounded, but do not apply tiny fixture-sized
    // stream quotas that reject otherwise valid production repository files.
    let stream = StreamLimits::new(
        8_192,
        MAX_STREAM_RECORDS_PER_FILE,
        MAX_STREAM_OUTPUT_BYTES_PER_FILE,
        MAX_STREAM_DIAGNOSTICS_PER_FILE,
        MAX_STREAM_DIAGNOSTIC_BYTES_PER_FILE,
        MAX_STREAM_STRING_BYTES_PER_FILE,
        batch,
    )
    .map_err(|_| FirstSliceError::Limits)?;
    AnalysisLimits::new(
        maximum_source_bytes,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        MAX_REPORTED_MEMORY_BYTES_PER_FILE,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .map_err(|_| FirstSliceError::Limits)
}

fn parser_config(maximum_source_bytes: usize) -> Result<RuntimeConfig, FirstSliceError> {
    let settings = ParserSettings::new(4096).map_err(|_| FirstSliceError::Limits)?;
    RuntimeConfig::new(
        maximum_source_bytes,
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

fn sanitized_repository_root_path(canonical_root: &Path) -> Result<String, FirstSliceError> {
    let lossy = canonical_root.to_string_lossy();
    #[cfg(windows)]
    let visible = lossy
        .strip_prefix(r"\\?\UNC\")
        .map(|path| format!(r"\\{path}"))
        .unwrap_or_else(|| {
            lossy
                .strip_prefix(r"\\?\")
                .map_or_else(|| lossy.to_string(), str::to_owned)
        });
    #[cfg(not(windows))]
    let visible = lossy.into_owned();
    if visible.is_empty() || visible.len() > CATALOG_MAX_ROOT_PATH_BYTES {
        return Err(FirstSliceError::Limits);
    }
    let mut sanitized = String::new();
    sanitized
        .try_reserve_exact(visible.len())
        .map_err(|_| FirstSliceError::Limits)?;
    sanitized.extend(visible.chars().map(|character| {
        if character.is_control() {
            '\u{fffd}'
        } else {
            character
        }
    }));
    if sanitized.len() > CATALOG_MAX_ROOT_PATH_BYTES {
        return Err(FirstSliceError::Limits);
    }
    Ok(sanitized)
}

fn random_repository_id_with_pending(
    repositories: &BTreeMap<ContentHash, RepositoryId>,
    pending: &BTreeMap<ContentHash, PendingRepositoryRegistration>,
) -> Result<RepositoryId, FirstSliceError> {
    random_repository_id_where(|candidate| {
        repositories
            .values()
            .any(|repository| *repository == candidate)
            || pending
                .values()
                .any(|(repository, _, _)| *repository == candidate)
    })
}

fn random_repository_id_where(
    mut is_used: impl FnMut(RepositoryId) -> bool,
) -> Result<RepositoryId, FirstSliceError> {
    for _ in 0..MAX_RANDOM_ID_ATTEMPTS {
        let mut local_uuid = [0_u8; 16];
        getrandom::fill(&mut local_uuid).map_err(|_| FirstSliceError::RandomUnavailable)?;
        local_uuid[6] = (local_uuid[6] & 0x0f) | 0x40;
        local_uuid[8] = (local_uuid[8] & 0x3f) | 0x80;
        let candidate = derive_repository(&local_uuid).id();
        if !is_used(candidate) {
            return Ok(candidate);
        }
    }
    Err(FirstSliceError::RandomUnavailable)
}

fn sanitized_repository_display_name(
    canonical_root: &Path,
    repository: RepositoryId,
) -> Result<String, FirstSliceError> {
    let component = canonical_root
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let mut display_name = String::new();
    display_name
        .try_reserve_exact(CATALOG_MAX_LABEL_BYTES)
        .map_err(|_| FirstSliceError::Limits)?;
    for character in component.trim().chars() {
        let character = if character.is_control() || matches!(character, '/' | '\\') {
            '\u{fffd}'
        } else {
            character
        };
        if display_name.len().saturating_add(character.len_utf8()) > CATALOG_MAX_LABEL_BYTES {
            break;
        }
        display_name.push(character);
    }
    if display_name.is_empty() {
        display_name.push_str(&repository.to_string());
    }
    Ok(display_name)
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

fn first_slice_git_limits(
    maximum_output_bytes: usize,
    command_timeout: Duration,
) -> Result<(GitLimits, GitCollectLimits), FirstSliceGitEvidenceError> {
    if maximum_output_bytes == 0
        || maximum_output_bytes > MAX_FIRST_SLICE_GIT_OUTPUT_BYTES
        || command_timeout.is_zero()
        || command_timeout > Duration::from_secs(30)
    {
        return Err(FirstSliceGitEvidenceError::InvalidLimits);
    }
    let limits = GitLimits::new(
        1,
        MAX_FIRST_SLICE_GIT_CHANGE_PATHS,
        4_000,
        1,
        4_096,
        MAX_FIRST_SLICE_GIT_OUTPUT_BYTES,
    )
    .map_err(|_| FirstSliceGitEvidenceError::InvalidLimits)?;
    let collect_limits = GitCollectLimits::new(1, maximum_output_bytes, command_timeout)
        .map_err(|_| FirstSliceGitEvidenceError::InvalidLimits)?;
    Ok((limits, collect_limits))
}

fn validate_first_slice_git_revision(revision: &str) -> Result<(), FirstSliceGitEvidenceError> {
    if revision.is_empty()
        || revision.len() > 512
        || revision.starts_with('-')
        || revision.contains("..")
        || revision
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(FirstSliceGitEvidenceError::InvalidSelector);
    }
    Ok(())
}

fn first_slice_working_tree_change_matches(
    selection: FirstSliceWorkingTreeSelection,
    change_set: &GitChangeSet,
) -> bool {
    let base_is_index = matches!(change_set.base, GitRevisionSelector::Index { .. });
    let head_is_index = matches!(change_set.head, GitRevisionSelector::Index { .. });
    let head_is_working = matches!(change_set.head, GitRevisionSelector::WorkingTree { .. });
    match selection {
        FirstSliceWorkingTreeSelection::Staged => head_is_index,
        FirstSliceWorkingTreeSelection::Unstaged => base_is_index && head_is_working,
        FirstSliceWorkingTreeSelection::All => head_is_working || head_is_index,
    }
}

fn collect_first_slice_git_change_paths(change_set: &GitChangeSet, paths: &mut BTreeSet<String>) {
    for change in &change_set.changes {
        if let Some(path) = &change.before_path {
            paths.insert(path.clone());
        }
        if let Some(path) = &change.after_path {
            paths.insert(path.clone());
        }
    }
}

const fn first_slice_git_error(code: GitCollectErrorCode) -> FirstSliceGitEvidenceError {
    match code {
        GitCollectErrorCode::InvalidLimits => FirstSliceGitEvidenceError::InvalidLimits,
        GitCollectErrorCode::Cancelled => FirstSliceGitEvidenceError::Cancelled,
        GitCollectErrorCode::CommandIo
        | GitCollectErrorCode::CommandTimedOut
        | GitCollectErrorCode::CommandOutputLimit
        | GitCollectErrorCode::CommandFailed
        | GitCollectErrorCode::InvalidOutput
        | GitCollectErrorCode::Contract => FirstSliceGitEvidenceError::Unavailable,
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
        collections::BTreeSet,
        ffi::OsStr,
        fs,
        path::Path,
        process::Command,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use cap_std::{ambient_authority, fs::Dir};
    use rootlight_ids::{FileIdentity, GenerationId, derive_file};
    use rootlight_incremental::{EquivalenceSnapshot, LogicalComponent, LogicalDomain};
    use rootlight_ir::{
        ContainerRef, CoverageScope, CoverageStatus, EntityRecord, EntityVisibility, FileRecord,
        OccurrenceRole, OccurrenceTarget, ProducerKind, ProvenanceRecord, RelationPredicate,
        SourceMappingRecord, SymbolIdentityClaim, derive_provenance_record_id,
        new_file_identity_claim_envelope, new_symbol_identity_claim_envelope,
    };
    use rootlight_runtime::RuntimePaths;
    use rootlight_vfs::platform::PrivateDirectory;
    use serde::Serialize;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn storage_policy_distinguishes_current_defaults_user_overrides_and_legacy_config() {
        let current = FirstSliceStoragePolicy::resolve_user_config(None, 8)
            .expect("absent config resolves current defaults");
        assert_eq!(current.retained_generations(), 2);
        assert_eq!(current.minimum_free_disk_bytes(), 64 * 1024 * 1024);
        assert_eq!(
            current
                .effective_maximum_repositories(current.retained_generations())
                .expect("current repository capacity resolves"),
            4_096
        );

        let configured = FirstSliceStoragePolicy::resolve_user_config(
            Some(
                "version = \"1.2\"\n[storage]\nmaximum_repository_bytes = 1073741824\nmaximum_catalog_bytes = 2147483648\nretained_generations = 3\nminimum_free_disk_bytes = 134217728\nsource_reservation_factor = 32\noracle_reservation_factor = 12\n",
            ),
            8,
        )
        .expect("valid user storage policy resolves");
        assert_eq!(configured.retained_generations(), 3);
        assert_eq!(configured.maximum_repository_bytes(), 1024 * 1024 * 1024);
        assert_eq!(configured.minimum_free_disk_bytes(), 128 * 1024 * 1024);
        assert_eq!(configured.source_reservation_factor(), 32);
        assert_eq!(
            configured
                .effective_maximum_repositories(configured.retained_generations())
                .expect("configuration 1.2 repository capacity resolves"),
            2_730
        );

        let legacy = FirstSliceStoragePolicy::resolve_user_config(Some("version = \"1.1\""), 8)
            .expect("legacy config keeps compatibility policy");
        assert_eq!(legacy.retained_generations(), 8);
        assert_eq!(legacy.minimum_free_disk_bytes(), 0);
        assert_eq!(legacy.maximum_repository_bytes(), u64::MAX);
        assert_eq!(
            legacy
                .effective_maximum_repositories(legacy.retained_generations())
                .expect("legacy repository capacity resolves"),
            1_024
        );
    }

    #[test]
    fn storage_policy_clamps_configured_repository_capacity_to_generation_capacity() {
        let configured = FirstSliceStoragePolicy::resolve_user_config(
            Some(
                "version = \"1.3\"\n[storage]\nmaximum_repositories = 176\nretained_generations = 2\n",
            ),
            8,
        )
        .expect("configured repository capacity resolves");
        assert_eq!(
            configured
                .effective_maximum_repositories(configured.retained_generations())
                .expect("configured repository capacity is representable"),
            176
        );

        let generation_bound = FirstSliceStoragePolicy::resolve_user_config(
            Some(
                "version = \"1.3\"\n[storage]\nmaximum_repositories = 4096\nretained_generations = 8\n",
            ),
            2,
        )
        .expect("generation-bound repository capacity resolves");
        assert_eq!(
            generation_bound
                .effective_maximum_repositories(generation_bound.retained_generations())
                .expect("generation-bound repository capacity is representable"),
            1_024
        );
    }

    const VERTICAL_SLICE_FIXTURE_ROOT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/vertical-slice/first-slice/v1"
    );
    const VERTICAL_SLICE_V2_PATCH: &str =
        include_str!("../../../tests/fixtures/vertical-slice/first-slice/v1-to-v2.patch");
    const IGNORED_SENTINEL: &str = "ROOTLIGHT_IGNORED_SENTINEL";
    const EQUIVALENCE_COMPONENT_BYTES: usize = 4 * 1024 * 1024;

    #[test]
    fn registered_repository_roots_are_bounded_and_canonical() {
        let fixture = TempDir::new().expect("repository root exists");
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn registered() -> bool { true }\n",
        )
        .expect("fixture writes");
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &deadline())
            .expect("repository indexes");

        let roots = service
            .registered_repository_roots()
            .expect("registered roots enumerate");
        let [root] = roots.as_slice() else {
            panic!("one committed repository root is expected");
        };
        assert_eq!(root.repository(), receipt.repository);
        assert!(root.root().is_absolute());
        assert_eq!(
            service
                .registered_repository_for_root(root.root(), &deadline())
                .expect("retained root resolves"),
            Some(receipt.repository)
        );
    }

    #[test]
    fn service_owned_git_evidence_uses_the_registered_repository_root() {
        let fixture = TempDir::new().expect("repository root exists");
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn value() -> usize { 1 }\n",
        )
        .expect("fixture source writes");
        for arguments in [
            &["init", "--quiet"][..],
            &["config", "user.email", "rootlight@example.invalid"][..],
            &["config", "user.name", "Rootlight Test"][..],
            &["add", "."][..],
            &["commit", "--quiet", "-m", "base"][..],
        ] {
            let output = Command::new("git")
                .args(arguments)
                .current_dir(fixture.path())
                .output()
                .expect("Git fixture command starts");
            assert!(
                output.status.success(),
                "Git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let mut service = FirstSliceService::new(4).expect("service starts");
        let cancellation = deadline();
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("fixture indexes");
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn value() -> usize { 2 }\n",
        )
        .expect("working-tree change writes");

        let paths = service
            .collect_git_change_paths(
                receipt.repository,
                Some(FirstSliceWorkingTreeSelection::All),
                None,
                1024 * 1024,
                Duration::from_secs(5),
                &cancellation,
            )
            .expect("working-tree evidence collects");
        assert_eq!(paths, vec!["lib.rs"]);
        assert!(
            !service
                .git_revisions_match_clean_head(
                    receipt.repository,
                    &["HEAD"],
                    1024 * 1024,
                    Duration::from_secs(5),
                    &cancellation,
                )
                .expect("dirty status collects")
        );
        assert_eq!(
            service.collect_git_change_paths(
                receipt.repository,
                None,
                Some(("HEAD..invalid", "HEAD")),
                1024 * 1024,
                Duration::from_secs(5),
                &cancellation,
            ),
            Err(FirstSliceGitEvidenceError::InvalidSelector)
        );
    }

    #[test]
    fn substantial_indexes_bound_aggregate_resolution_candidates() {
        assert_eq!(
            resolution_limits_for_occurrences(1)
                .expect("small indexes use valid resolver limits")
                .candidate_limit(),
            DEFAULT_CANDIDATE_LIMIT
        );
        assert_eq!(
            resolution_limits_for_occurrences(125_000)
                .expect("substantial indexes use valid resolver limits")
                .candidate_limit(),
            8
        );
        assert_eq!(
            resolution_limits_for_occurrences(usize::MAX)
                .expect("maximum indexes use valid resolver limits")
                .candidate_limit(),
            1
        );
    }

    #[test]
    fn resolution_work_overflow_publishes_unresolved_bounded_facts() {
        let repository = derive_repository(b"resolution-work-bounded-repository").id();
        let generation = GenerationId::from_bytes([73; 20]);
        let file = derive_file(FileIdentity {
            repository,
            path_identity: b"src/lib.rs",
        })
        .id();
        let source_hash = content_hash(b"resolution-work-bounded-source");
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 64).expect("fixture source span is valid"),
            source_hash,
            None,
        );
        let producer = ProducerIdentity::new(
            "rootlight-resolution-work-fixture",
            "1.0",
            content_hash(b"resolution-work-fixture-producer"),
        )
        .expect("fixture producer is valid");
        let mut provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            producer_kind: ProducerKind::Parser,
            producer,
            binary_digest: content_hash(b"resolution-work-fixture-binary"),
            frontend_version: Some("fixture-1".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(content_hash(
                b"resolution-work-fixture-context",
            )),
            input_sources: vec![source.clone()],
            evidence_sources: vec![source.clone()],
            derivation_parents: Vec::new(),
            rule: None,
        };
        provenance.id =
            derive_provenance_record_id(&provenance).expect("fixture provenance identity derives");
        let provenance_id = provenance.id;
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.provenance.push(provenance);
        document.files.push(FileRecord {
            id: file,
            repository,
            generation,
            path: "src/lib.rs".to_owned(),
            path_locator: None,
            content_hash: source_hash,
            byte_length: 64,
            language: "rust".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: provenance_id,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
        let file_claim = FileIdentityClaim {
            file,
            repository,
            path: "src/lib.rs".to_owned(),
            path_identity: b"src/lib.rs".to_vec(),
            content_hash: source_hash,
            byte_length: 64,
        };
        document.extensions.push(
            new_file_identity_claim_envelope(
                &file_claim,
                generation,
                provenance_id,
                source.clone(),
            )
            .expect("fixture file identity claim encodes"),
        );
        for identity in [75_u8, 76, 77] {
            let declared_identity = format!("shared-{identity}");
            let mut container_identity = vec![0];
            container_identity.extend_from_slice(file.as_bytes());
            let claim = SymbolIdentityClaim {
                symbol: SymbolId::from_bytes([0; 20]),
                repository,
                language: "rust".to_owned(),
                kind: EntityKind::Function,
                container: Some(ContainerRef::File(file)),
                container_identity,
                declared_identity,
                signature_discriminator: Vec::new(),
                build_context_discriminator: content_hash(b"resolution-work-fixture-context")
                    .as_bytes()
                    .to_vec(),
            };
            let claim = SymbolIdentityClaim {
                symbol: claim.derived_symbol(),
                ..claim
            };
            document.entities.push(EntityRecord {
                id: claim.symbol,
                repository,
                generation,
                kind: EntityKind::Function,
                language: "rust".to_owned(),
                tier: AnalysisTier::TierB,
                canonical_name: "shared".to_owned(),
                display_name: "shared".to_owned(),
                qualified_name: format!("fixture::shared::{identity}"),
                container: Some(ContainerRef::File(file)),
                visibility: rootlight_ir::EntityVisibility::Private,
                flags: Vec::new(),
                provenance: provenance_id,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            });
            document.extensions.push(
                new_symbol_identity_claim_envelope(
                    &claim,
                    generation,
                    provenance_id,
                    source.clone(),
                )
                .expect("fixture symbol identity claim encodes"),
            );
        }
        let text_hash = content_hash(b"shared");
        let mut occurrence = rootlight_ir::OccurrenceRecord {
            id: FactId::from_bytes([78; 20]),
            repository,
            generation,
            file,
            source: source.clone(),
            role: rootlight_ir::OccurrenceRole::Reference,
            enclosing: None,
            target: rootlight_ir::OccurrenceTarget::Unresolved { text_hash },
            syntactic_text_hash: text_hash,
            syntax_kind: "identifier".to_owned(),
            provenance: provenance_id,
            confidence: rootlight_ir::Confidence::new(0).expect("zero fixture confidence is valid"),
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        };
        occurrence.id = rootlight_ir::derive_occurrence_record_id(&occurrence)
            .expect("fixture occurrence identity derives");
        document.occurrences.push(occurrence);
        for domain in [IrFactDomain::Occurrences, IrFactDomain::Relations] {
            let mut coverage = CoverageRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                scope: CoverageScope::File(file),
                domain,
                tier: AnalysisTier::TierB,
                status: CoverageStatus::Complete,
                discovered: 1,
                indexed: 1,
                skipped: 0,
                provenance: provenance_id,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            coverage.id =
                derive_coverage_record_id(&coverage).expect("fixture coverage identity derives");
            document.coverage_records.push(coverage);
        }
        let limits =
            ResolutionLimits::with_work_limit(1, 3).expect("fixture resolver limits are valid");
        rootlight_ir::validate_ir_document(
            &document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("fixture normalized document is valid");

        let bounded = apply_bounded_resolution(
            document,
            limits,
            ResolverFactContext::new(content_hash(b"resolution-work-fixture-context")),
            &Cancellation::new(),
        )
        .expect("over-limit resolution publishes bounded structural facts");

        assert!(matches!(
            bounded.occurrences[0].target,
            rootlight_ir::OccurrenceTarget::Unresolved { .. }
        ));
        assert!(bounded.relations.is_empty());
        assert!(bounded.coverage_records.iter().all(|coverage| {
            !matches!(
                coverage.domain,
                IrFactDomain::Occurrences | IrFactDomain::Relations
            ) || coverage.status == CoverageStatus::Bounded
        }));
        assert!(bounded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "resolution-work-bounded"
                && diagnostic.message
                    == "resolution required 4 work units above the configured limit 3"
        }));
    }

    #[test]
    fn project_partition_merge_unifies_external_symbol_claims() {
        fn document(
            repository: RepositoryId,
            generation: GenerationId,
            file: FileId,
        ) -> NormalizedIrDocument {
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, 1).expect("external symbol source span is valid"),
                content_hash(file.as_bytes()),
                None,
            );
            let build_context =
                BuildContextIdentity::new(content_hash(b"partition-merge-build-context"));
            let producer = ProducerIdentity::new(
                "rootlight-test-project",
                "1.0",
                content_hash(b"partition-merge-producer"),
            )
            .expect("project producer is valid");
            let mut provenance = ProvenanceRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                producer_kind: ProducerKind::Derivation,
                producer,
                binary_digest: content_hash(b"partition-merge-binary"),
                frontend_version: Some("test-project-1".to_owned()),
                language: "rust".to_owned(),
                tier: AnalysisTier::TierB,
                build_context,
                input_sources: vec![source.clone()],
                evidence_sources: vec![source.clone()],
                derivation_parents: Vec::new(),
                rule: None,
            };
            provenance.id = derive_provenance_record_id(&provenance)
                .expect("project provenance identity derives");
            let mut container_identity = vec![0];
            container_identity.extend_from_slice(repository.as_bytes());
            let mut claim = SymbolIdentityClaim {
                symbol: SymbolId::from_bytes([0; 20]),
                repository,
                language: "rust".to_owned(),
                kind: EntityKind::ExternalSymbol,
                container: Some(ContainerRef::Repository(repository)),
                container_identity,
                declared_identity: "external_api".to_owned(),
                signature_discriminator: content_hash(b"external_api").as_bytes().to_vec(),
                build_context_discriminator: build_context.digest().as_bytes().to_vec(),
            };
            claim.symbol = claim.derived_symbol();
            let entity = EntityRecord {
                id: claim.symbol,
                repository,
                generation,
                kind: EntityKind::ExternalSymbol,
                language: "rust".to_owned(),
                tier: AnalysisTier::TierB,
                canonical_name: "external_api".to_owned(),
                display_name: "external_api".to_owned(),
                qualified_name: "external_api".to_owned(),
                container: Some(ContainerRef::Repository(repository)),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            let identity_claim =
                new_symbol_identity_claim_envelope(&claim, generation, provenance.id, source)
                    .expect("external symbol identity claim encodes");
            let mut document = NormalizedIrDocument::empty(repository, generation);
            document.provenance.push(provenance);
            document.entities.push(entity);
            document.extensions.push(identity_claim);
            document
        }

        let repository = derive_repository(b"partition-merge-repository").id();
        let generation = GenerationId::from_bytes([21; 20]);
        let mut analysis = FirstSliceProjectAnalysis::new(
            document(repository, generation, FileId::from_bytes([1; 20])),
            true,
        );
        analysis
            .append_partition(
                document(repository, generation, FileId::from_bytes([2; 20])),
                false,
            )
            .expect("the next project partition merges immediately");
        let (
            documents,
            isolation_permits_deep_adapter,
            partitioned,
            diagnostics_truncated,
            facts_truncated,
        ) = analysis.into_parts();
        assert!(!isolation_permits_deep_adapter);
        assert!(partitioned);
        assert!(!diagnostics_truncated);
        assert!(!facts_truncated);
        assert_eq!(documents.len(), 1);
        let merged = &documents[0];

        assert_eq!(merged.entities.len(), 1);
        assert_eq!(
            merged
                .extensions
                .iter()
                .filter(|extension| extension.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
                .count(),
            1
        );
        assert_eq!(merged.provenance.len(), 2);
    }

    #[test]
    fn project_partition_merge_bounds_aggregate_output() {
        fn document(
            repository: RepositoryId,
            generation: GenerationId,
            file: FileId,
            partition: usize,
            diagnostic_count: usize,
        ) -> NormalizedIrDocument {
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, 1).expect("diagnostic source span is valid"),
                content_hash(file.as_bytes()),
                None,
            );
            let producer = ProducerIdentity::new(
                "rootlight-test-project",
                "1.0",
                content_hash(b"partition-diagnostic-producer"),
            )
            .expect("project producer is valid");
            let mut provenance = ProvenanceRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                producer_kind: ProducerKind::Derivation,
                producer,
                binary_digest: content_hash(b"partition-diagnostic-binary"),
                frontend_version: Some("test-project-1".to_owned()),
                language: "rust".to_owned(),
                tier: AnalysisTier::TierB,
                build_context: BuildContextIdentity::new(content_hash(
                    b"partition-diagnostic-context",
                )),
                input_sources: vec![source.clone()],
                evidence_sources: vec![source.clone()],
                derivation_parents: Vec::new(),
                rule: None,
            };
            provenance.id = derive_provenance_record_id(&provenance)
                .expect("project provenance identity derives");
            let file_record = FileRecord {
                id: file,
                repository,
                generation,
                path: format!("src/partition-{partition}.rs"),
                path_locator: None,
                content_hash: source.content_hash(),
                byte_length: 1,
                language: "rust".to_owned(),
                encoding: "utf-8".to_owned(),
                generated: false,
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            let diagnostics = (0..diagnostic_count)
                .map(|index| {
                    let mut diagnostic = DiagnosticRecord {
                        id: FactId::from_bytes([0; 20]),
                        repository,
                        generation,
                        code: format!("partition-{partition}-diagnostic-{index}"),
                        message: "bounded parser recovery".to_owned(),
                        severity: DiagnosticSeverity::Warning,
                        source: Some(source.clone()),
                        coverage_effect: CoverageStatus::Bounded,
                        provenance: provenance.id,
                        evidence: FactEvidence {
                            source: Some(source.clone()),
                            derivation: Vec::new(),
                        },
                    };
                    diagnostic.id = derive_diagnostic_record_id(&diagnostic)
                        .expect("diagnostic identity derives");
                    diagnostic
                })
                .collect();
            let mut coverage = CoverageRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                scope: CoverageScope::File(file),
                domain: IrFactDomain::Relations,
                tier: AnalysisTier::TierB,
                status: CoverageStatus::Complete,
                discovered: 1,
                indexed: 1,
                skipped: 0,
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            coverage.id =
                derive_coverage_record_id(&coverage).expect("project coverage identity derives");
            let mut document = NormalizedIrDocument::empty(repository, generation);
            document.files.push(file_record);
            document.provenance.push(provenance);
            document.coverage_records.push(coverage);
            document.diagnostics = diagnostics;
            document
        }

        let repository = derive_repository(b"partition-diagnostic-repository").id();
        let generation = GenerationId::from_bytes([22; 20]);
        let limits = IrLimits::default();
        let existing_target_diagnostics = 1;
        let diagnostics_per_partition = limits.max_diagnostics / 2 + 1;
        let mut analysis = FirstSliceProjectAnalysis::new(
            document(
                repository,
                generation,
                FileId::from_bytes([1; 20]),
                1,
                diagnostics_per_partition,
            ),
            true,
        );
        analysis
            .append_partition(
                document(
                    repository,
                    generation,
                    FileId::from_bytes([2; 20]),
                    2,
                    diagnostics_per_partition,
                ),
                true,
            )
            .expect("the bounded diagnostic partition merges");
        let (documents, isolated, partitioned, diagnostics_truncated, facts_truncated) =
            analysis.into_parts();
        assert!(isolated);
        assert!(partitioned);
        assert!(diagnostics_truncated);
        assert!(!facts_truncated);

        let merged = prepare_project_analysis_document(
            documents,
            "rust",
            partitioned,
            diagnostics_truncated,
            facts_truncated,
            existing_target_diagnostics,
            &limits,
        )
        .expect("bounded aggregate diagnostics remain publishable");
        assert_eq!(
            existing_target_diagnostics + merged.diagnostics.len(),
            limits.max_diagnostics
        );
        assert!(
            merged
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == PROJECT_DIAGNOSTICS_TRUNCATED_CODE)
        );
        assert!(merged.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "project-adapter-partitioned-coverage"
                && diagnostic.coverage_effect == CoverageStatus::Bounded
        }));
        assert!(
            merged
                .coverage_records
                .iter()
                .all(|coverage| coverage.status == CoverageStatus::Bounded)
        );

        let saturated = prepare_project_analysis_document(
            vec![
                document(
                    repository,
                    generation,
                    FileId::from_bytes([3; 20]),
                    3,
                    diagnostics_per_partition,
                ),
                document(
                    repository,
                    generation,
                    FileId::from_bytes([4; 20]),
                    4,
                    diagnostics_per_partition,
                ),
            ],
            "rust",
            true,
            false,
            false,
            limits.max_diagnostics,
            &limits,
        )
        .expect("saturated structural diagnostics retain bounded project coverage");
        assert!(saturated.diagnostics.is_empty());
        assert!(
            saturated
                .coverage_records
                .iter()
                .all(|coverage| coverage.status == CoverageStatus::Bounded)
        );

        let mut saturated_fallback = document(
            repository,
            generation,
            FileId::from_bytes([6; 20]),
            6,
            limits.max_diagnostics,
        );
        let fallback_file = saturated_fallback.files[0].id;
        let fallback_provenance = saturated_fallback.provenance[0].id;
        append_project_fallback_diagnostic(
            &mut saturated_fallback,
            "typescript",
            FirstSliceProjectAnalysisError::Protocol,
            fallback_file,
            fallback_provenance,
            &limits,
        )
        .expect("a saturated target retains the project fallback reason");
        assert_eq!(saturated_fallback.diagnostics.len(), limits.max_diagnostics);
        assert_eq!(
            saturated_fallback
                .diagnostics
                .last()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("project-adapter-protocol-fallback")
        );
        append_extension_truncation_diagnostic(&mut saturated_fallback, 17, &limits)
            .expect("a saturated target retains the extension coverage summary");
        append_skipped_region_truncation_diagnostic(&mut saturated_fallback, 19, &limits)
            .expect("a saturated target retains the skipped-region coverage summary");
        assert_eq!(saturated_fallback.diagnostics.len(), limits.max_diagnostics);
        for code in [
            "project-adapter-protocol-fallback",
            "extension-coverage-bounded",
            "skipped-region-details-bounded",
        ] {
            assert!(
                saturated_fallback
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "priority diagnostic {code} remains observable"
            );
        }
        rootlight_ir::validate_ir_document(
            &saturated_fallback,
            &limits,
            &ExtensionSupport::default(),
        )
        .expect("the saturated fallback target remains valid normalized IR");

        let mut capacity_document =
            document(repository, generation, FileId::from_bytes([5; 20]), 5, 0);
        let provenance = capacity_document.provenance[0].id;
        let source = capacity_document.files[0]
            .evidence
            .source
            .clone()
            .expect("project file has direct evidence");
        capacity_document.entities.push(EntityRecord {
            id: SymbolId::from_bytes([6; 20]),
            repository,
            generation,
            kind: EntityKind::Function,
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            canonical_name: "capacity_bound".to_owned(),
            display_name: "capacity_bound".to_owned(),
            qualified_name: "capacity_bound".to_owned(),
            container: None,
            visibility: EntityVisibility::Private,
            flags: Vec::new(),
            provenance,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
        let mut invalid_summary_document = capacity_document.clone();
        let entity = invalid_summary_document.entities[0].id;
        let mut entity_coverage = invalid_summary_document.coverage_records[0].clone();
        entity_coverage.scope = CoverageScope::Entity(entity);
        entity_coverage.evidence.source = Some(source);
        entity_coverage.id = derive_coverage_record_id(&entity_coverage)
            .expect("entity-scoped coverage identity derives");
        invalid_summary_document
            .coverage_records
            .push(entity_coverage);
        let mut dependent_provenance = invalid_summary_document.provenance[0].clone();
        dependent_provenance.derivation_parents = vec![FactRef::Entity(entity)];
        dependent_provenance.id = derive_provenance_record_id(&dependent_provenance)
            .expect("entity-derived provenance identity derives");
        invalid_summary_document
            .provenance
            .push(dependent_provenance);
        rootlight_ir::validate_ir_document(
            &invalid_summary_document,
            &limits,
            &ExtensionSupport::default(),
        )
        .expect("entity-scoped project output is valid before capacity summarization");
        let mut capacity_limits = limits;
        capacity_limits.max_entities = 0;
        let partition_document = capacity_document.clone();
        let mut target = NormalizedIrDocument::empty(repository, generation);
        let mut append_state =
            DocumentAppendState::from_document(&target).expect("empty append state initializes");
        assert_eq!(
            append_project_document_with_capacity(
                &mut target,
                capacity_document,
                &capacity_limits,
                &mut append_state,
            ),
            Err(FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::Entities,
                observed: 1,
                limit: 0,
            })
        );
        assert!(target.entities.is_empty());
        assert!(target.files.is_empty());
        assert!(target.coverage_records.is_empty());
        rootlight_ir::validate_ir_document(&target, &capacity_limits, &ExtensionSupport::default())
            .expect("rejected project output leaves valid normalized IR");

        let mut merged_partition = NormalizedIrDocument::empty(repository, generation);
        let mut external_symbols = BTreeSet::new();
        let truncation = merge_project_document(
            &mut merged_partition,
            partition_document,
            &mut external_symbols,
            capacity_limits.max_diagnostics,
            &capacity_limits,
        )
        .expect("streaming partition capacity exhaustion commits a bounded summary");
        assert!(truncation.facts);
        assert!(merged_partition.entities.is_empty());
        rootlight_ir::validate_ir_document(
            &merged_partition,
            &capacity_limits,
            &ExtensionSupport::default(),
        )
        .expect("the bounded streaming partition remains valid normalized IR");

        let mut rejected_target = NormalizedIrDocument::empty(repository, generation);
        let expected_target = rejected_target.clone();
        let mut rejected_state = DocumentAppendState::from_document(&rejected_target)
            .expect("rejected append state initializes");
        assert_eq!(
            append_project_document_with_capacity(
                &mut rejected_target,
                invalid_summary_document.clone(),
                &capacity_limits,
                &mut rejected_state,
            ),
            Err(FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::Entities,
                observed: 1,
                limit: 0,
            })
        );
        assert_eq!(rejected_target, expected_target);

        let mut rejected_partition = NormalizedIrDocument::empty(repository, generation);
        assert_eq!(
            merge_project_document(
                &mut rejected_partition,
                invalid_summary_document,
                &mut BTreeSet::new(),
                capacity_limits.max_diagnostics,
                &capacity_limits,
            ),
            Err(FirstSliceError::Identity)
        );
        assert_eq!(
            rejected_partition,
            NormalizedIrDocument::empty(repository, generation)
        );
    }

    #[test]
    fn structural_merge_bounds_aggregate_diagnostics() {
        fn document(
            repository: RepositoryId,
            generation: GenerationId,
            file: FileId,
            diagnostic_count: usize,
        ) -> NormalizedIrDocument {
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, 1).expect("diagnostic source span is valid"),
                content_hash(file.as_bytes()),
                None,
            );
            let producer = ProducerIdentity::new(
                "rootlight-test-structural",
                "1.0",
                content_hash(b"structural-diagnostic-producer"),
            )
            .expect("structural producer is valid");
            let mut provenance = ProvenanceRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                producer_kind: ProducerKind::Parser,
                producer,
                binary_digest: content_hash(b"structural-diagnostic-binary"),
                frontend_version: Some("test-structural-1".to_owned()),
                language: "rust".to_owned(),
                tier: AnalysisTier::TierB,
                build_context: BuildContextIdentity::new(content_hash(
                    b"structural-diagnostic-context",
                )),
                input_sources: vec![source.clone()],
                evidence_sources: vec![source.clone()],
                derivation_parents: Vec::new(),
                rule: None,
            };
            provenance.id = derive_provenance_record_id(&provenance)
                .expect("structural provenance identity derives");
            let file_record = FileRecord {
                id: file,
                repository,
                generation,
                path: format!("src/{file}.rs"),
                path_locator: None,
                content_hash: source.content_hash(),
                byte_length: 1,
                language: "rust".to_owned(),
                encoding: "utf-8".to_owned(),
                generated: false,
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            let diagnostics = (0..diagnostic_count)
                .map(|index| {
                    let mut diagnostic = DiagnosticRecord {
                        id: FactId::from_bytes([0; 20]),
                        repository,
                        generation,
                        code: format!("structural-diagnostic-{index}"),
                        message: "bounded parser recovery".to_owned(),
                        severity: DiagnosticSeverity::Warning,
                        source: Some(source.clone()),
                        coverage_effect: CoverageStatus::Bounded,
                        provenance: provenance.id,
                        evidence: FactEvidence {
                            source: Some(source.clone()),
                            derivation: Vec::new(),
                        },
                    };
                    diagnostic.id = derive_diagnostic_record_id(&diagnostic)
                        .expect("diagnostic identity derives");
                    diagnostic
                })
                .collect();
            let count = u64::try_from(diagnostic_count).expect("fixture count is representable");
            let mut coverage = CoverageRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                scope: CoverageScope::File(file),
                domain: IrFactDomain::Diagnostics,
                tier: AnalysisTier::TierB,
                status: CoverageStatus::Complete,
                discovered: count,
                indexed: count,
                skipped: 0,
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            coverage.id =
                derive_coverage_record_id(&coverage).expect("diagnostic coverage identity derives");
            let mut document = NormalizedIrDocument::empty(repository, generation);
            document.files.push(file_record);
            document.provenance.push(provenance);
            document.coverage_records.push(coverage);
            document.diagnostics = diagnostics;
            document
        }

        let repository = derive_repository(b"structural-diagnostic-repository").id();
        let generation = GenerationId::from_bytes([26; 20]);
        let mut limits = IrLimits::default();
        limits.max_diagnostics = 3;
        let mut target = NormalizedIrDocument::empty(repository, generation);
        let mut append_state =
            DocumentAppendState::from_document(&target).expect("append state initializes");
        for (file, diagnostics) in [
            (FileId::from_bytes([27; 20]), 2),
            (FileId::from_bytes([28; 20]), 2),
        ] {
            append_normalized_document(
                &mut target,
                document(repository, generation, file, diagnostics),
                &limits,
                &mut append_state,
            )
            .expect("structural diagnostics append within the aggregate bound");
        }
        append_diagnostic_truncation_diagnostic(
            &mut target,
            append_state.truncated_diagnostics,
            &limits,
        )
        .expect("aggregate diagnostic disposition remains visible");

        assert_eq!(append_state.truncated_diagnostics, 2);
        assert_eq!(target.diagnostics.len(), limits.max_diagnostics);
        assert_eq!(
            target
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == AGGREGATE_DIAGNOSTICS_TRUNCATED_CODE)
                .count(),
            1
        );
        assert!(target.coverage_records.iter().any(|coverage| {
            coverage.scope == CoverageScope::File(FileId::from_bytes([28; 20]))
                && coverage.status == CoverageStatus::Bounded
                && coverage.discovered == 2
                && coverage.indexed == 0
                && coverage.skipped == 2
        }));
        rootlight_ir::validate_ir_document(&target, &limits, &ExtensionSupport::default())
            .expect("bounded aggregate diagnostics remain valid normalized IR");
    }

    #[test]
    fn priority_diagnostics_replace_details_or_fail_closed() {
        fn diagnostic(
            repository: RepositoryId,
            generation: GenerationId,
            source: &SourceRef,
            provenance: FactId,
            code: &str,
            coverage_effect: CoverageStatus,
        ) -> DiagnosticRecord {
            let mut diagnostic = DiagnosticRecord {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                code: code.to_owned(),
                message: format!("{code} detail"),
                severity: DiagnosticSeverity::Warning,
                source: Some(source.clone()),
                coverage_effect,
                provenance,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            diagnostic.id =
                derive_diagnostic_record_id(&diagnostic).expect("diagnostic identity derives");
            diagnostic
        }

        let repository = derive_repository(b"priority-diagnostic-repository").id();
        let generation = GenerationId::from_bytes([23; 20]);
        let file = FileId::from_bytes([24; 20]);
        let provenance = FactId::from_bytes([25; 20]);
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 1).expect("diagnostic source span is valid"),
            content_hash(b"priority-diagnostic-source"),
            None,
        );
        let mut limits = IrLimits::default();
        limits.max_diagnostics = 3;

        let mut mixed = NormalizedIrDocument::empty(repository, generation);
        mixed.diagnostics = vec![
            diagnostic(
                repository,
                generation,
                &source,
                provenance,
                "unsupported-language-first",
                CoverageStatus::Unknown,
            ),
            diagnostic(
                repository,
                generation,
                &source,
                provenance,
                "bounded-parser-detail",
                CoverageStatus::Bounded,
            ),
            diagnostic(
                repository,
                generation,
                &source,
                provenance,
                "unsupported-language-last",
                CoverageStatus::Unknown,
            ),
        ];
        append_project_fallback_diagnostic(
            &mut mixed,
            "rust",
            FirstSliceProjectAnalysisError::Protocol,
            file,
            provenance,
            &limits,
        )
        .expect("a bounded detail yields to the project fallback");
        assert_eq!(mixed.diagnostics[0].code, "unsupported-language-first");
        assert_eq!(
            mixed.diagnostics[1].code,
            "project-adapter-protocol-fallback"
        );
        assert_eq!(mixed.diagnostics[2].code, "unsupported-language-last");

        let mut unknown_only = NormalizedIrDocument::empty(repository, generation);
        unknown_only.diagnostics = ["first", "middle", "last"]
            .into_iter()
            .map(|suffix| {
                diagnostic(
                    repository,
                    generation,
                    &source,
                    provenance,
                    &format!("unsupported-language-{suffix}"),
                    CoverageStatus::Unknown,
                )
            })
            .collect();
        append_project_fallback_diagnostic(
            &mut unknown_only,
            "rust",
            FirstSliceProjectAnalysisError::Protocol,
            file,
            provenance,
            &limits,
        )
        .expect("the last non-priority detail yields to the project fallback");
        assert_eq!(
            unknown_only
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            vec![
                "unsupported-language-first",
                "unsupported-language-middle",
                "project-adapter-protocol-fallback",
            ]
        );

        let mut priority_only = NormalizedIrDocument::empty(repository, generation);
        priority_only.diagnostics = ["first", "middle", "last"]
            .into_iter()
            .map(|suffix| {
                diagnostic(
                    repository,
                    generation,
                    &source,
                    provenance,
                    &format!("project-adapter-{suffix}"),
                    CoverageStatus::Bounded,
                )
            })
            .collect();
        let retained = priority_only.diagnostics.clone();
        assert_eq!(
            append_project_fallback_diagnostic(
                &mut priority_only,
                "rust",
                FirstSliceProjectAnalysisError::Protocol,
                file,
                provenance,
                &limits,
            ),
            Err(FirstSliceError::Limits)
        );
        assert_eq!(priority_only.diagnostics, retained);
    }

    struct FailingProjectAnalyzer {
        identity: ContentHash,
        error: FirstSliceProjectAnalysisError,
        calls: Arc<AtomicUsize>,
    }

    impl FirstSliceProjectAnalyzer for FailingProjectAnalyzer {
        fn provider_identity(&self) -> ContentHash {
            self.identity
        }

        fn analyze(
            &self,
            _request: FirstSliceProjectAnalysisRequest<'_>,
            _cancellation: &Cancellation,
        ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(self.error)
        }
    }

    struct SuccessfulProjectAnalyzer {
        identity: ContentHash,
        calls: Arc<AtomicUsize>,
        partitioned: bool,
        syntax_facts_bounded: bool,
    }

    struct CapacityProjectAnalyzer {
        identity: ContentHash,
        calls: Arc<AtomicUsize>,
    }

    impl FirstSliceProjectAnalyzer for CapacityProjectAnalyzer {
        fn provider_identity(&self) -> ContentHash {
            self.identity
        }

        fn analyze(
            &self,
            request: FirstSliceProjectAnalysisRequest<'_>,
            _cancellation: &Cancellation,
        ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let input = request
                .inputs()
                .first()
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            let relative = RelativePath::parse(Path::new(input.path()))
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let length = u64::try_from(input.source().len())
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let source = SourceRef::new(
                request.repository(),
                request.generation(),
                SourceSpan::new(input.file(), 0, length)
                    .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
                input.content_hash(),
                None,
            );
            let producer =
                ProducerIdentity::new("rootlight-test-project", "1.0", request.build_context())
                    .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let mut provenance = ProvenanceRecord {
                id: FactId::from_bytes([0; 20]),
                repository: request.repository(),
                generation: request.generation(),
                producer_kind: ProducerKind::Derivation,
                producer,
                binary_digest: self.identity,
                frontend_version: Some("test-project-1".to_owned()),
                language: request.language().to_owned(),
                tier: AnalysisTier::TierB,
                build_context: BuildContextIdentity::new(request.build_context()),
                input_sources: vec![source.clone()],
                evidence_sources: vec![source.clone()],
                derivation_parents: Vec::new(),
                rule: None,
            };
            provenance.id = derive_provenance_record_id(&provenance)
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let provenance_id = provenance.id;
            let mut document =
                NormalizedIrDocument::empty(request.repository(), request.generation());
            document.files.push(FileRecord {
                id: input.file(),
                repository: request.repository(),
                generation: request.generation(),
                path: input.path().to_owned(),
                path_locator: Some(relative.to_locator()),
                content_hash: input.content_hash(),
                byte_length: length,
                language: request.language().to_owned(),
                encoding: "utf-8".to_owned(),
                generated: input.generated(),
                provenance: provenance_id,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            });
            document.provenance.push(provenance);
            let mut container_identity = vec![0];
            container_identity.extend_from_slice(request.repository().as_bytes());
            for index in 0..3 {
                let name = format!("project_only_symbol_{index}");
                let mut symbol_claim = SymbolIdentityClaim {
                    symbol: SymbolId::from_bytes([0; 20]),
                    repository: request.repository(),
                    language: request.language().to_owned(),
                    kind: EntityKind::ExternalSymbol,
                    container: Some(ContainerRef::Repository(request.repository())),
                    container_identity: container_identity.clone(),
                    declared_identity: name.clone(),
                    signature_discriminator: content_hash(name.as_bytes()).as_bytes().to_vec(),
                    build_context_discriminator: request.build_context().as_bytes().to_vec(),
                };
                symbol_claim.symbol = symbol_claim.derived_symbol();
                document.entities.push(EntityRecord {
                    id: symbol_claim.symbol,
                    repository: request.repository(),
                    generation: request.generation(),
                    kind: EntityKind::ExternalSymbol,
                    language: request.language().to_owned(),
                    tier: AnalysisTier::TierB,
                    canonical_name: name.clone(),
                    display_name: name.clone(),
                    qualified_name: name,
                    container: Some(ContainerRef::Repository(request.repository())),
                    visibility: EntityVisibility::Unknown,
                    flags: Vec::new(),
                    provenance: provenance_id,
                    evidence: FactEvidence {
                        source: Some(source.clone()),
                        derivation: Vec::new(),
                    },
                });
                document.extensions.push(
                    new_symbol_identity_claim_envelope(
                        &symbol_claim,
                        request.generation(),
                        provenance_id,
                        source.clone(),
                    )
                    .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
                );
            }
            for index in 0..2 {
                let mut coverage = CoverageRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: request.repository(),
                    generation: request.generation(),
                    scope: CoverageScope::File(input.file()),
                    domain: if index == 0 {
                        IrFactDomain::Files
                    } else {
                        IrFactDomain::Relations
                    },
                    tier: AnalysisTier::TierB,
                    status: CoverageStatus::Complete,
                    discovered: 1,
                    indexed: 1,
                    skipped: 0,
                    provenance: provenance_id,
                    evidence: FactEvidence {
                        source: Some(source.clone()),
                        derivation: Vec::new(),
                    },
                };
                coverage.id = derive_coverage_record_id(&coverage)
                    .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
                document.coverage_records.push(coverage);
            }
            let claim = FileIdentityClaim {
                file: input.file(),
                repository: request.repository(),
                path: input.path().to_owned(),
                path_identity: relative.identity_bytes().to_vec(),
                content_hash: input.content_hash(),
                byte_length: length,
            };
            document.extensions.push(
                new_file_identity_claim_envelope(
                    &claim,
                    request.generation(),
                    provenance_id,
                    source,
                )
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            );
            Ok(FirstSliceProjectAnalysis::new(document, true))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CapturedGeneratedOrigin {
        generated: SourceSpan,
        origin_path: String,
        origin: SourceSpan,
        transformation: String,
        generator_digest: Option<ContentHash>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CapturedProjectInput {
        path: String,
        generated: bool,
        origins: Vec<CapturedGeneratedOrigin>,
    }

    struct CapturingProjectAnalyzer {
        identity: ContentHash,
        observations: Arc<Mutex<Vec<CapturedProjectInput>>>,
    }

    impl FirstSliceProjectAnalyzer for CapturingProjectAnalyzer {
        fn provider_identity(&self) -> ContentHash {
            self.identity
        }

        fn analyze(
            &self,
            request: FirstSliceProjectAnalysisRequest<'_>,
            _cancellation: &Cancellation,
        ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
            let captured = request
                .inputs()
                .iter()
                .map(|input| CapturedProjectInput {
                    path: input.path().to_owned(),
                    generated: input.generated(),
                    origins: input
                        .origins()
                        .iter()
                        .map(|mapping| CapturedGeneratedOrigin {
                            generated: mapping.generated(),
                            origin_path: mapping.origin_path().as_str().to_owned(),
                            origin: mapping.origin(),
                            transformation: mapping.transformation().as_str().to_owned(),
                            generator_digest: mapping.generator_digest(),
                        })
                        .collect(),
                })
                .collect();
            *self
                .observations
                .lock()
                .expect("capture mutex is not poisoned") = captured;
            Err(FirstSliceProjectAnalysisError::Analysis)
        }
    }

    impl FirstSliceProjectAnalyzer for SuccessfulProjectAnalyzer {
        fn provider_identity(&self) -> ContentHash {
            self.identity
        }

        fn analyze(
            &self,
            request: FirstSliceProjectAnalysisRequest<'_>,
            _cancellation: &Cancellation,
        ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let mut documents = Vec::new();
            if !self.partitioned {
                documents.push(NormalizedIrDocument::empty(
                    request.repository(),
                    request.generation(),
                ));
            }
            for input in request.inputs() {
                if self.partitioned {
                    documents.push(NormalizedIrDocument::empty(
                        request.repository(),
                        request.generation(),
                    ));
                }
                let document = documents
                    .last_mut()
                    .expect("test analyzer has an output document");
                let relative = RelativePath::parse(Path::new(input.path()))
                    .expect("test project path is canonical");
                let length = u64::try_from(input.source().len())
                    .expect("test project source length is bounded");
                let source = SourceRef::new(
                    request.repository(),
                    request.generation(),
                    SourceSpan::new(input.file(), 0, length)
                        .expect("test project source span is valid"),
                    input.content_hash(),
                    None,
                );
                let producer =
                    ProducerIdentity::new("rootlight-test-project", "1.0", request.build_context())
                        .expect("test project producer is valid");
                let mut provenance = ProvenanceRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: request.repository(),
                    generation: request.generation(),
                    producer_kind: ProducerKind::Derivation,
                    producer,
                    binary_digest: self.identity,
                    frontend_version: Some("test-project-1".to_owned()),
                    language: request.language().to_owned(),
                    tier: AnalysisTier::TierB,
                    build_context: BuildContextIdentity::new(request.build_context()),
                    input_sources: vec![source.clone()],
                    evidence_sources: vec![source.clone()],
                    derivation_parents: Vec::new(),
                    rule: None,
                };
                provenance.id = derive_provenance_record_id(&provenance)
                    .expect("test project provenance identity derives");
                let provenance_id = provenance.id;
                document.provenance.push(provenance);
                document.files.push(FileRecord {
                    id: input.file(),
                    repository: request.repository(),
                    generation: request.generation(),
                    path: input.path().to_owned(),
                    path_locator: Some(relative.to_locator()),
                    content_hash: input.content_hash(),
                    byte_length: length,
                    language: request.language().to_owned(),
                    encoding: "utf-8".to_owned(),
                    generated: input.generated(),
                    provenance: provenance_id,
                    evidence: FactEvidence {
                        source: Some(source.clone()),
                        derivation: Vec::new(),
                    },
                });
                let mut coverage = rootlight_ir::CoverageRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: request.repository(),
                    generation: request.generation(),
                    scope: CoverageScope::File(input.file()),
                    domain: rootlight_ir::FactDomain::Relations,
                    tier: AnalysisTier::TierB,
                    status: if self.syntax_facts_bounded {
                        CoverageStatus::Bounded
                    } else {
                        CoverageStatus::Complete
                    },
                    discovered: if self.syntax_facts_bounded { 2 } else { 1 },
                    indexed: 1,
                    skipped: if self.syntax_facts_bounded { 1 } else { 0 },
                    provenance: provenance_id,
                    evidence: FactEvidence {
                        source: Some(source.clone()),
                        derivation: Vec::new(),
                    },
                };
                coverage.id = derive_coverage_record_id(&coverage)
                    .expect("test project coverage identity derives");
                document.coverage_records.push(coverage);
                if self.syntax_facts_bounded {
                    let mut diagnostic = DiagnosticRecord {
                        id: FactId::from_bytes([0; 20]),
                        repository: request.repository(),
                        generation: request.generation(),
                        code: PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC.to_owned(),
                        message: "project syntax facts exceeded the bounded semantic limit"
                            .to_owned(),
                        severity: DiagnosticSeverity::Warning,
                        source: Some(source.clone()),
                        coverage_effect: CoverageStatus::Bounded,
                        provenance: provenance_id,
                        evidence: FactEvidence {
                            source: Some(source.clone()),
                            derivation: Vec::new(),
                        },
                    };
                    diagnostic.id = derive_diagnostic_record_id(&diagnostic)
                        .expect("test project diagnostic identity derives");
                    document.diagnostics.push(diagnostic);
                }
                let claim = FileIdentityClaim {
                    file: input.file(),
                    repository: request.repository(),
                    path: input.path().to_owned(),
                    path_identity: relative.identity_bytes().to_vec(),
                    content_hash: input.content_hash(),
                    byte_length: length,
                };
                document.extensions.push(
                    new_file_identity_claim_envelope(
                        &claim,
                        request.generation(),
                        provenance_id,
                        source,
                    )
                    .expect("test file identity claim encodes"),
                );
            }
            if self.partitioned {
                Ok(FirstSliceProjectAnalysis::new_partitioned(documents, true))
            } else {
                Ok(FirstSliceProjectAnalysis::new(
                    documents
                        .pop()
                        .expect("test analyzer has one output document"),
                    true,
                ))
            }
        }
    }

    #[test]
    fn project_document_validation_binds_generated_mapping_provenance() {
        let repository = derive_repository(b"mapping-validation").id();
        let generation = GenerationId::from_bytes([9; 20]);
        let provider_identity = content_hash(b"mapping-provider");
        let origin_file = FileId::from_bytes([1; 20]);
        let generated_file = FileId::from_bytes([2; 20]);
        let origin_bytes = b"schema";
        let generated_bytes = b"generated";
        let origin_length =
            u64::try_from(origin_bytes.len()).expect("origin fixture length is representable");
        let generated_length = u64::try_from(generated_bytes.len())
            .expect("generated fixture length is representable");
        let origin_span =
            SourceSpan::new(origin_file, 0, origin_length).expect("origin span is valid");
        let generated_span =
            SourceSpan::new(generated_file, 0, generated_length).expect("generated span is valid");
        let mapping = GeneratedOriginMapping::new(
            generated_span,
            RelativePath::parse(Path::new("schema/source.rs")).expect("origin path is canonical"),
            origin_span,
            TransformationId::new("fixture.codegen").expect("transformation is canonical"),
            Some(content_hash(b"fixture-generator")),
        );
        let mappings = [mapping.clone()];
        let inputs = [
            FirstSliceProjectInput {
                file: origin_file,
                path: "schema/source.rs",
                content_hash: content_hash(origin_bytes),
                source: origin_bytes,
                generated: false,
                origins: &[],
            },
            FirstSliceProjectInput {
                file: generated_file,
                path: "src/generated.rs",
                content_hash: content_hash(generated_bytes),
                source: generated_bytes,
                generated: true,
                origins: &mappings,
            },
        ];
        let origin_source = SourceRef::new(
            repository,
            generation,
            origin_span,
            content_hash(origin_bytes),
            None,
        );
        let generated_source = SourceRef::new(
            repository,
            generation,
            generated_span,
            content_hash(generated_bytes),
            None,
        );
        let producer = ProducerIdentity::new(
            "rootlight-mapping-fixture",
            "1.0.0",
            content_hash(b"fixture-config"),
        )
        .expect("producer identity is valid");
        let mut file_provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            producer_kind: ProducerKind::Compiler,
            producer: producer.clone(),
            binary_digest: provider_identity,
            frontend_version: Some("fixture-frontend".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(content_hash(b"build-context")),
            input_sources: vec![generated_source.clone()],
            evidence_sources: vec![generated_source.clone()],
            derivation_parents: Vec::new(),
            rule: Some("fixture.structural".to_owned()),
        };
        file_provenance.id =
            derive_provenance_record_id(&file_provenance).expect("file provenance derives");
        let mut mapping_provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            producer_kind: ProducerKind::Derivation,
            producer,
            binary_digest: provider_identity,
            frontend_version: Some("fixture-frontend".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(content_hash(b"build-context")),
            input_sources: vec![generated_source.clone(), origin_source.clone()],
            evidence_sources: vec![generated_source.clone(), origin_source.clone()],
            derivation_parents: vec![FactRef::Fact(file_provenance.id)],
            rule: Some(mapping.provenance_rule()),
        };
        mapping_provenance.id =
            derive_provenance_record_id(&mapping_provenance).expect("mapping provenance derives");
        let mut mapping_record = SourceMappingRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            from: generated_source.clone(),
            to: origin_source.clone(),
            kind: SourceMappingKind::GeneratedToOrigin,
            provenance: mapping_provenance.id,
            evidence: FactEvidence {
                source: Some(generated_source.clone()),
                derivation: vec![FactRef::File(origin_file)],
            },
        };
        mapping_record.id = rootlight_ir::derive_source_mapping_record_id(&mapping_record)
            .expect("mapping identity derives");
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.files = vec![
            FileRecord {
                id: origin_file,
                repository,
                generation,
                path: "schema/source.rs".to_owned(),
                path_locator: None,
                content_hash: content_hash(origin_bytes),
                byte_length: origin_length,
                language: "rust".to_owned(),
                encoding: "utf-8".to_owned(),
                generated: false,
                provenance: file_provenance.id,
                evidence: FactEvidence {
                    source: Some(origin_source),
                    derivation: Vec::new(),
                },
            },
            FileRecord {
                id: generated_file,
                repository,
                generation,
                path: "src/generated.rs".to_owned(),
                path_locator: None,
                content_hash: content_hash(generated_bytes),
                byte_length: generated_length,
                language: "rust".to_owned(),
                encoding: "utf-8".to_owned(),
                generated: true,
                provenance: file_provenance.id,
                evidence: FactEvidence {
                    source: Some(generated_source),
                    derivation: Vec::new(),
                },
            },
        ];
        document.provenance = vec![file_provenance, mapping_provenance];
        document.source_mappings = vec![mapping_record.clone()];

        assert!(project_document_matches_inputs(
            &document,
            repository,
            generation,
            provider_identity,
            &inputs,
        ));

        document.provenance[1].rule = Some("rootlight.generated-origin.v1:other:none".to_owned());
        assert!(!project_document_matches_inputs(
            &document,
            repository,
            generation,
            provider_identity,
            &inputs,
        ));
        document.provenance[1].rule = Some(mapping.provenance_rule());

        let mut duplicate = mapping_record;
        duplicate.id = FactId::from_bytes([7; 20]);
        document.source_mappings.push(duplicate);
        assert!(!project_document_matches_inputs(
            &document,
            repository,
            generation,
            provider_identity,
            &inputs,
        ));
    }

    fn durable_test_tempdir() -> TempDir {
        #[cfg(target_os = "macos")]
        {
            // Avoid the default `/var` alias rejected by repository-root VFS checks.
            tempfile::Builder::new()
                .prefix("rl-durable-")
                .tempdir_in("/private/tmp")
                .expect("durable test directory is available")
        }
        #[cfg(not(target_os = "macos"))]
        {
            TempDir::new().expect("durable test directory is available")
        }
    }

    #[test]
    fn catalog_integrity_failures_retain_stable_service_classes() {
        let cancellation = Cancellation::new();

        for kind in [
            CatalogErrorKind::Corrupt,
            CatalogErrorKind::MigrationChecksumMismatch,
            CatalogErrorKind::InvalidGeneration,
            CatalogErrorKind::IdentityProofRequired,
        ] {
            assert_eq!(
                map_catalog_error_kind(kind, &cancellation),
                FirstSliceError::CatalogCorrupt
            );
        }
        assert_eq!(
            map_catalog_error_kind(CatalogErrorKind::IncompatibleSchema, &cancellation),
            FirstSliceError::CatalogMigrationRequired
        );
    }

    #[test]
    fn public_indexing_makes_typescript_and_javascript_queryable_at_tier_d() {
        assert_public_language_repository(
            &[
                (
                    "src/value.ts",
                    "export function typescriptValue(): number { return 1; }\n",
                ),
                (
                    "src/value.js",
                    "export function javascriptValue() { return 2; }\n",
                ),
            ],
            &[
                ("typescript", "typescriptValue"),
                ("javascript", "javascriptValue"),
            ],
        );
    }

    #[test]
    fn public_indexing_makes_python_queryable_at_tier_d() {
        assert_public_language_repository(
            &[("src/value.py", "def python_value():\n    return 1\n")],
            &[("python", "python_value")],
        );
    }

    #[test]
    fn javascript_implementation_ranks_ahead_of_typescript_declaration() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[
                (
                    "packages/react/index.d.ts",
                    "export declare function createElement(type: unknown): unknown;\n",
                ),
                (
                    "packages/react/src/jsx/ReactJSXElement.js",
                    "export function createElement(type) { return { type }; }\n",
                ),
            ],
        );
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .expect("mixed declaration and implementation repository publishes");

        let located = service
            .code_locate(
                receipt.generation,
                "createElement".to_owned(),
                LocateMode::Exact,
                10,
                0,
                &cancellation,
            )
            .expect("duplicate declaration and implementation are queryable");
        let matching_paths = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.identifier == "createElement")
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            matching_paths.first().copied(),
            Some("packages/react/src/jsx/ReactJSXElement.js")
        );
        assert!(
            matching_paths.contains(&"packages/react/index.d.ts"),
            "the declaration remains available alongside the implementation"
        );
    }

    #[test]
    fn public_indexing_makes_go_queryable_at_tier_d() {
        assert_public_language_repository(
            &[(
                "value.go",
                "package sample\n\nfunc GoValue() int { return 1 }\n",
            )],
            &[("go", "GoValue")],
        );
    }

    #[test]
    fn project_analysis_failure_uses_an_explicit_structural_fallback() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"failing-project-adapter"),
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::clone(&calls),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("structural fallback remains publishable");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("fallback generation status resolves");

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "project-adapter-analysis-fallback"
                && diagnostic.message == "project analysis for python used structural fallback"
        }));
        assert!(status.coverage.iter().any(|coverage| {
            coverage.language == "python"
                && coverage.tier == "tier_d"
                && coverage.status == "complete"
        }));
        assert_eq!(status.semantic_freshness, "pending_refinement");
    }

    #[test]
    fn saturated_unsupported_diagnostics_keep_fallback_noncurrent() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[
                ("src/first.asm", "first: nop\n"),
                ("src/second.asm", "second: nop\n"),
                ("src/third.asm", "third: nop\n"),
                ("src/value.rs", "pub fn value() -> usize { 1 }\n"),
            ],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"saturated-fallback-project-adapter"),
            error: FirstSliceProjectAnalysisError::Protocol,
            calls: Arc::clone(&calls),
        });
        let cancellation = deadline();
        let mut direct = FirstSliceService::new_with_storage(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            Some(Arc::clone(&analyzer)),
        )
        .expect("project service initializes");
        limit_service_diagnostics(&mut direct, 3);

        let deep = direct
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &cancellation)
            .expect("direct deep indexing publishes an explicit fallback");
        let deep_document = direct
            .generations
            .generation(deep.generation)
            .expect("deep fallback generation remains retained")
            .document();
        assert_eq!(deep_document.diagnostics.len(), 3);
        assert_eq!(
            deep_document
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "unsupported-language")
                .count(),
            2
        );
        assert!(
            deep.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "project-adapter-protocol-fallback")
        );
        assert_eq!(
            direct
                .generation_freshness(deep.repository, deep.generation)
                .expect("fallback freshness resolves"),
            FirstSliceFreshnessStatus {
                structural: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
                semantic: FirstSliceObservedFreshness::PendingSemanticRefinement,
                publication: FirstSlicePublicationMode::ProcessLocalSingleStage,
                two_stage: FirstSliceTwoStageAvailability::UnavailableWithoutDurablePublication,
            }
        );

        let temporary = durable_test_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let mut service = FirstSliceService::new_durable_with_project_analyzer(
            3,
            paths.state_dir(),
            analyzer,
            &cancellation,
        )
        .expect("durable project service initializes");
        limit_service_diagnostics(&mut service, 3);

        let structural = service
            .index_repository_with_mode(
                fixture.path(),
                FirstSliceIndexMode::Structural,
                &cancellation,
            )
            .expect("structural stage publishes");
        let structural_document = service
            .generations
            .generation(structural.generation)
            .expect("structural generation remains retained")
            .document();
        assert_eq!(structural_document.diagnostics.len(), 3);
        assert!(
            structural_document
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code == "unsupported-language")
        );
        let error = match service.prepare_semantic_refinement(
            fixture.path(),
            structural.generation,
            &cancellation,
        ) {
            Ok(_) => panic!("saturated fallback cannot become a semantic refinement"),
            Err(error) => error,
        };
        let freshness = service
            .generation_freshness(structural.repository, structural.generation)
            .expect("structural freshness resolves");

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(error, FirstSliceError::Adapter);
        assert_eq!(
            freshness,
            FirstSliceFreshnessStatus {
                structural: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
                semantic: FirstSliceObservedFreshness::PendingSemanticRefinement,
                publication: FirstSlicePublicationMode::DurableStructuralStage,
                two_stage: FirstSliceTwoStageAvailability::StructuralPublished,
            }
        );
        assert_eq!(
            service.active_generation_for(structural.repository),
            Some(structural.generation)
        );
    }

    #[test]
    fn unsupported_project_language_keeps_structural_coverage() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/Value.kt", "fun kotlinValue(): Int = 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"unsupported-language-project-adapter"),
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::clone(&calls),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("unsupported project languages retain structural output");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("structural generation status resolves");

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(
            !receipt
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.starts_with("project-adapter-"))
        );
        assert!(status.coverage.iter().any(|coverage| {
            coverage.language == "kotlin"
                && coverage.tier == "tier_d"
                && coverage.status == "complete"
        }));
    }

    #[test]
    fn project_fallback_errors_preserve_bounded_adapter_failures() {
        assert_eq!(
            project_fallback_error("project-adapter-wall-time-fallback"),
            Some(FirstSliceError::AdapterWallTimeLimit)
        );
        assert_eq!(
            project_fallback_error("project-adapter-input-limit-fallback"),
            Some(FirstSliceError::AdapterInputLimit)
        );
        assert_eq!(
            project_fallback_error("project-adapter-output-limit-fallback"),
            Some(FirstSliceError::AdapterOutputLimit)
        );
        assert_eq!(
            project_fallback_error("project-adapter-memory-limit-fallback"),
            Some(FirstSliceError::AdapterMemoryLimit)
        );
        assert_eq!(
            project_fallback_error("project-adapter-process-fallback"),
            Some(FirstSliceError::AdapterProcessFailure)
        );
        assert_eq!(
            project_fallback_error("project-adapter-resource-unavailable-fallback"),
            Some(FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::ProcessResources,
            })
        );
        assert_eq!(
            project_fallback_error("project-adapter-analysis-fallback"),
            Some(FirstSliceError::Adapter)
        );
        assert_eq!(
            project_fallback_error("project-adapter-capacity-fallback"),
            Some(FirstSliceError::IncompleteCoverage)
        );
        assert_eq!(
            project_fallback_error("project-adapter-facts-truncated-fallback"),
            Some(FirstSliceError::IncompleteCoverage)
        );
        assert_eq!(
            project_fallback_error("project-adapter-declaration-loss-fallback"),
            Some(FirstSliceError::IncompleteCoverage)
        );
        assert_eq!(project_fallback_error("unrelated-diagnostic"), None);
    }

    #[test]
    fn partitioned_project_analysis_marks_cross_partition_coverage_as_bounded() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[
                ("src/first.py", "def first_value():\n    return 1\n"),
                ("src/second.py", "def second_value():\n    return 2\n"),
            ],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"partitioned-project-adapter"),
            calls: Arc::clone(&calls),
            partitioned: true,
            syntax_facts_bounded: false,
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("partitioned deep analysis remains publishable");

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "project-adapter-partitioned-coverage"
                && diagnostic.message
                    == "project analysis for python was partitioned and cross-partition relationships are bounded"
        }));
        let document = service
            .generations
            .generation(receipt.generation)
            .expect("partitioned generation resolves")
            .document();
        assert!(document.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "project-adapter-partitioned-coverage"
                && diagnostic.coverage_effect == CoverageStatus::Bounded
        }));
        assert_eq!(
            document
                .coverage_records
                .iter()
                .filter(|coverage| coverage.tier == AnalysisTier::TierB)
                .count(),
            2
        );
        assert!(
            document
                .coverage_records
                .iter()
                .filter(|coverage| coverage.tier == AnalysisTier::TierB)
                .all(|coverage| coverage.status == CoverageStatus::Bounded)
        );
        assert!(
            receipt
                .diagnostics
                .iter()
                .all(|diagnostic| !is_project_fallback_code(&diagnostic.code))
        );
    }

    #[test]
    fn project_capacity_overflow_keeps_structural_symbols_queryable() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(CapacityProjectAnalyzer {
            identity: content_hash(b"capacity-project-adapter"),
            calls: Arc::clone(&calls),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");
        limit_service_ir_entities(&mut service, 2);

        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("structural fallback publishes after project capacity overflow");
        let located = service
            .code_locate(
                receipt.generation,
                "python_value".to_owned(),
                LocateMode::Exact,
                4,
                0,
                &deadline(),
            )
            .expect("structural fallback remains queryable");

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(located.data.hits.len(), 1);
        assert_eq!(located.data.hits[0].language, "python");
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "project-adapter-capacity-fallback"
                && diagnostic.message == "project analysis for python used structural fallback"
        }));
    }

    #[test]
    fn bounded_project_syntax_accepts_an_exact_unified_definition_site() {
        let repository = derive_repository(b"unified-declaration-repository").id();
        let generation = GenerationId::from_bytes([71; 20]);
        let file = FileId::from_bytes([72; 20]);
        let symbol = SymbolId::from_bytes([73; 20]);
        let provenance = FactId::from_bytes([74; 20]);
        let entity = |evidence_start: u64, evidence_end: u64, symbol| EntityRecord {
            id: symbol,
            repository,
            generation,
            kind: EntityKind::Function,
            language: "javascript".to_owned(),
            tier: AnalysisTier::TierB,
            canonical_name: "App".to_owned(),
            display_name: "App".to_owned(),
            qualified_name: "App".to_owned(),
            container: Some(ContainerRef::Repository(repository)),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance,
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(file, evidence_start, evidence_end)
                        .expect("fixture declaration span is valid"),
                    content_hash(b"function App() {}"),
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        let mut structural = NormalizedIrDocument::empty(repository, generation);
        structural.entities.push(entity(100, 120, symbol));
        let mut project = NormalizedIrDocument::empty(repository, generation);
        project.entities.push(entity(10, 30, symbol));

        assert_eq!(
            missing_structural_declaration_count(&[structural.clone()], &project, true)
                .expect("different declaration representative compares"),
            1
        );

        let definition_source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 109, 112).expect("fixture definition span is valid"),
            content_hash(b"function App() {}"),
            None,
        );
        project.occurrences.push(OccurrenceRecord {
            id: FactId::from_bytes([75; 20]),
            repository,
            generation,
            file,
            source: definition_source.clone(),
            role: OccurrenceRole::Definition,
            enclosing: None,
            target: OccurrenceTarget::Resolved { symbol },
            syntactic_text_hash: content_hash(b"App"),
            syntax_kind: "identifier.definition".to_owned(),
            provenance,
            confidence: rootlight_ir::Confidence::new(1000).expect("fixture confidence is valid"),
            evidence: FactEvidence {
                source: Some(definition_source),
                derivation: Vec::new(),
            },
        });
        assert_eq!(
            missing_structural_declaration_count(&[structural.clone()], &project, true)
                .expect("unified definition comparison succeeds"),
            0
        );

        project.occurrences[0].source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 121, 124).expect("fixture outside span is valid"),
            content_hash(b"function App() {}"),
            None,
        );
        project.occurrences[0].evidence.source = Some(project.occurrences[0].source.clone());
        assert_eq!(
            missing_structural_declaration_count(&[structural.clone()], &project, true)
                .expect("outside definition comparison succeeds"),
            1
        );

        project.occurrences[0].source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 109, 112).expect("fixture definition span is valid"),
            content_hash(b"function App() {}"),
            None,
        );
        project.occurrences[0].evidence.source = Some(project.occurrences[0].source.clone());
        project.occurrences[0].target = OccurrenceTarget::Resolved {
            symbol: SymbolId::from_bytes([76; 20]),
        };
        assert_eq!(
            missing_structural_declaration_count(&[structural], &project, true)
                .expect("unresolved declaration identity compares"),
            1
        );
    }

    #[test]
    fn bounded_project_syntax_publishes_when_no_declarations_are_lost() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "print('bounded project syntax')\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"bounded-syntax-project-adapter"),
            calls: Arc::clone(&calls),
            partitioned: false,
            syntax_facts_bounded: true,
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("bounded project output publishes when it preserves declarations");
        let status = service
            .repository_status(receipt.repository, Some(receipt.generation))
            .expect("bounded project status resolves");
        assert!(
            service
                .generations
                .generation(receipt.generation)
                .expect("bounded project generation remains retained")
                .document()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC)
        );

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(status.coverage.iter().any(|coverage| {
            coverage.language == "python"
                && coverage.tier == "tier_b"
                && coverage.status == "bounded"
        }));
        assert!(
            receipt
                .diagnostics
                .iter()
                .all(|diagnostic| { diagnostic.code != "project-adapter-capacity-fallback" })
        );
    }

    #[test]
    fn bounded_project_syntax_falls_back_when_a_declaration_is_lost() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"bounded-declaration-project-adapter"),
            calls: Arc::clone(&calls),
            partitioned: false,
            syntax_facts_bounded: true,
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("direct deep indexing publishes an honest structural fallback");
        let located = service
            .code_locate(
                receipt.generation,
                "python_value".to_owned(),
                LocateMode::Exact,
                4,
                0,
                &deadline(),
            )
            .expect("structural declaration remains queryable");

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(located.data.hits.len(), 1);
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "project-adapter-declaration-loss-fallback"
                && diagnostic.message == "project analysis for python used structural fallback"
        }));
    }

    #[test]
    fn unsafe_bounded_semantic_output_stops_before_durable_persistence() {
        let temporary = durable_test_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::write(
            repository_root.join("value.py"),
            "def structural_survivor():\n    return 1\n",
        )
        .expect("bounded semantic fixture writes");
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(CapacityProjectAnalyzer {
            identity: content_hash(b"unsafe-bounded-project-adapter"),
            calls: Arc::clone(&calls),
        });
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable_with_project_analyzer(
            3,
            paths.state_dir(),
            analyzer,
            &cancellation,
        )
        .expect("durable project service initializes");
        limit_service_ir_entities(&mut service, 2);
        let structural = service
            .index_repository_with_mode(
                &repository_root,
                FirstSliceIndexMode::Structural,
                &cancellation,
            )
            .expect("structural generation publishes");
        let before = service
            .support_inventory_snapshot()
            .expect("pre-refinement storage inventory verifies");
        let mut progress = Vec::new();

        let error = match service.prepare_semantic_refinement_with_progress(
            &repository_root,
            structural.generation,
            &cancellation,
            |observed| progress.push(observed),
        ) {
            Ok(_) => panic!("unsafe bounded output cannot refine the structural generation"),
            Err(error) => error,
        };
        let after = service
            .support_inventory_snapshot()
            .expect("post-refinement storage inventory verifies");

        assert!(matches!(
            error,
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::Entities,
                observed: 3,
                limit: 2,
            }
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(progress.iter().all(|observed| observed.stage
            != FirstSliceIndexStage::Persistence
            && observed.written_bytes == 0));
        assert_eq!(after.generations, before.generations);
        assert_eq!(after.total_storage_bytes, before.total_storage_bytes);
        assert_eq!(after.unreclaimed_temporary_bytes, 0);
        assert_eq!(
            service.active_generation_for(structural.repository),
            Some(structural.generation)
        );
    }

    #[test]
    fn deep_project_path_forwards_only_reliable_generated_origin_headers() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[
                ("src/generator.rs", "pub fn generate() {}\n"),
                (
                    "src/mapped.generated.rs",
                    "// Code generated by fixture-gen. DO NOT EDIT.\n// source: src/generator.rs\npub fn mapped() {}\n",
                ),
                (
                    "src/missing-source.generated.rs",
                    "// Code generated by fixture-gen. DO NOT EDIT.\npub fn missing_source() {}\n",
                ),
                (
                    "src/traversal.generated.rs",
                    "// Code generated by fixture-gen. DO NOT EDIT.\n// source: ../generator.rs\npub fn traversal() {}\n",
                ),
                (
                    "src/duplicate.generated.rs",
                    "// Code generated by fixture-gen. DO NOT EDIT.\n// source: src/generator.rs\n// source: src/generator.rs\npub fn duplicate() {}\n",
                ),
                (
                    "src/unknown.generated.rs",
                    "// Code generated by fixture-gen. DO NOT EDIT.\n// source: src/not-indexed.rs\npub fn unknown() {}\n",
                ),
            ],
        );
        let observations = Arc::new(Mutex::new(Vec::new()));
        let analyzer = Arc::new(CapturingProjectAnalyzer {
            identity: content_hash(b"generated-origin-capture"),
            observations: Arc::clone(&observations),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .expect("structural fallback remains publishable");

        let observed = observations.lock().expect("capture mutex is not poisoned");
        let mapped = observed
            .iter()
            .find(|input| input.path == "src/mapped.generated.rs")
            .expect("mapped generated input is captured");
        assert!(mapped.generated);
        assert_eq!(mapped.origins.len(), 1);
        assert_eq!(mapped.origins[0].origin_path, "src/generator.rs");
        assert_eq!(mapped.origins[0].transformation, "fixture-gen");
        assert_eq!(mapped.origins[0].generator_digest, None);
        assert_eq!(mapped.origins[0].generated.start_byte(), 0);
        assert_eq!(
            mapped.origins[0].generated.end_byte(),
            u64::try_from(
                "// Code generated by fixture-gen. DO NOT EDIT.\n// source: src/generator.rs\npub fn mapped() {}\n"
                    .len()
            )
            .expect("fixture length is representable")
        );
        assert_eq!(mapped.origins[0].origin.start_byte(), 0);
        assert_eq!(
            mapped.origins[0].origin.end_byte(),
            u64::try_from("pub fn generate() {}\n".len()).expect("fixture length is representable")
        );
        assert!(
            observed
                .iter()
                .filter(|input| input.path != "src/mapped.generated.rs")
                .all(|input| input.origins.is_empty())
        );
    }

    #[test]
    fn structural_mode_never_invokes_the_project_analyzer() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"unused-project-adapter"),
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::clone(&calls),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let receipt = service
            .index_repository_with_mode(
                fixture.path(),
                FirstSliceIndexMode::Structural,
                &deadline(),
            )
            .expect("structural analysis publishes");

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(receipt.diagnostics.is_empty());
    }

    #[test]
    fn project_adapter_cancellation_never_degrades_to_fallback() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"cancelled-project-adapter"),
            error: FirstSliceProjectAnalysisError::Cancelled(CancellationReason::ClientRequest),
            calls: Arc::clone(&calls),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        assert_eq!(
            service.index_repository_with_mode(
                fixture.path(),
                FirstSliceIndexMode::Deep,
                &deadline(),
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(service.active_generation().is_none());
    }

    #[test]
    fn project_adapter_resource_pressure_never_degrades_to_fallback() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"resource-limited-project-adapter"),
            error: FirstSliceProjectAnalysisError::ResourceUnavailable,
            calls: Arc::clone(&calls),
        });
        let mut service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        assert_eq!(
            service.index_repository_with_mode(
                fixture.path(),
                FirstSliceIndexMode::Deep,
                &deadline(),
            ),
            Err(FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::ProcessResources,
            })
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(service.active_generation().is_none());
    }

    #[test]
    fn durable_two_stage_publication_refines_and_restores_the_structural_parent() {
        let temporary = durable_test_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::create_dir(repository_root.join("src")).expect("source directory creates");
        fs::write(
            repository_root.join("src/lib.rs"),
            "pub fn two_stage_value() -> u32 { 2 }\n",
        )
        .expect("two-stage fixture writes");
        let calls = Arc::new(AtomicUsize::new(0));
        let identity = content_hash(b"successful-project-analyzer");
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(SuccessfulProjectAnalyzer {
            identity,
            calls: Arc::clone(&calls),
            partitioned: false,
            syntax_facts_bounded: false,
        });
        let cancellation = deadline();

        let (receipt, structural_written_bytes, semantic_written_bytes) = {
            let mut service = FirstSliceService::new_durable_with_project_analyzer(
                3,
                paths.state_dir(),
                Arc::clone(&analyzer),
                &cancellation,
            )
            .expect("durable project service initializes");
            let structural = service
                .index_repository_with_mode(
                    &repository_root,
                    FirstSliceIndexMode::Structural,
                    &cancellation,
                )
                .expect("structural stage publishes");
            let mut progress = Vec::new();
            let semantic_preparation = service
                .prepare_semantic_refinement_with_progress(
                    &repository_root,
                    structural.generation,
                    &cancellation,
                    |observed| progress.push(observed),
                )
                .expect("semantic refinement prepares");
            let semantic_preparation_written_bytes = match &semantic_preparation {
                FirstSliceIndexPreparation::Pending(prepared) => prepared.written_bytes,
                FirstSliceIndexPreparation::Retained { .. } => {
                    panic!("semantic refinement must build a new generation")
                }
            };
            let first = progress.first().expect("initial progress exists");
            assert_eq!(first.stage, FirstSliceIndexStage::Discovery);
            assert_eq!(first.total, 6);
            assert!(first.completed <= 1);
            assert_eq!(
                progress
                    .last()
                    .map(|observed| (observed.completed, observed.total)),
                Some((6, 6))
            );
            assert!(progress.windows(2).all(|pair| {
                pair[0].completed <= pair[1].completed
                    && pair[0].files_examined <= pair[1].files_examined
                    && pair[0].bytes_examined <= pair[1].bytes_examined
                    && pair[0].written_bytes <= pair[1].written_bytes
            }));
            assert!(progress.iter().any(|observed| {
                observed.completed > 0 && observed.files_examined > 0 && observed.bytes_examined > 0
            }));
            assert!(
                progress
                    .last()
                    .is_some_and(|observed| observed.written_bytes > 0)
            );
            let semantic = service
                .publish_prepared(semantic_preparation, &cancellation)
                .expect("semantic refinement publishes");
            let receipt = FirstSliceTwoStageIndexReceipt {
                structural,
                semantic,
            };
            assert_eq!(
                receipt.semantic().parent,
                Some(receipt.structural().generation)
            );
            assert_ne!(
                receipt.structural().generation,
                receipt.semantic().generation
            );
            assert_eq!(
                service.active_generation_for(receipt.semantic().repository),
                Some(receipt.semantic().generation)
            );
            assert_eq!(
                service
                    .generation_freshness(
                        receipt.structural().repository,
                        receipt.structural().generation,
                    )
                    .expect("structural freshness resolves"),
                FirstSliceFreshnessStatus {
                    structural: FirstSliceObservedFreshness::Superseded,
                    semantic: FirstSliceObservedFreshness::Superseded,
                    publication: FirstSlicePublicationMode::DurableStructuralStage,
                    two_stage: FirstSliceTwoStageAvailability::StructuralPublished,
                }
            );
            assert_eq!(
                service
                    .generation_freshness(
                        receipt.semantic().repository,
                        receipt.semantic().generation,
                    )
                    .expect("semantic freshness resolves"),
                FirstSliceFreshnessStatus {
                    structural: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
                    semantic: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
                    publication: FirstSlicePublicationMode::DurableSemanticRefinement,
                    two_stage: FirstSliceTwoStageAvailability::SemanticRefinementPublished,
                }
            );
            let structural_written_bytes = receipt.structural().retained_durable_bytes;
            (
                receipt,
                structural_written_bytes,
                semantic_preparation_written_bytes,
            )
        };
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(receipt.structural().oracle_allocated_bytes > 0);
        assert_eq!(receipt.semantic().oracle_allocated_bytes, 0);
        assert!(semantic_written_bytes < structural_written_bytes);
        assert!(receipt.semantic().retained_durable_bytes < structural_written_bytes);
        let repository_directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.semantic().repository.to_string());
        let structural_directory =
            repository_directory.join(receipt.structural().generation.to_string());
        assert!(structural_directory.join("oracle.sqlite3").is_file());
        assert!(!structural_directory.join("recovery-manifest.json").exists());
        assert!(!structural_directory.join("recovery.json.gz").exists());
        let semantic_directory =
            repository_directory.join(receipt.semantic().generation.to_string());
        assert!(!semantic_directory.join("oracle.sqlite3").exists());
        assert!(semantic_directory.join("recovery-manifest.json").is_file());
        assert!(semantic_directory.join("recovery.json.gz").is_file());

        let restored = FirstSliceService::new_durable_with_project_analyzer(
            3,
            paths.state_dir(),
            analyzer,
            &cancellation,
        )
        .expect("two-stage state restores");
        assert_eq!(
            restored.active_generation_for(receipt.semantic().repository),
            Some(receipt.semantic().generation)
        );
        assert_eq!(
            restored
                .generation_freshness(receipt.semantic().repository, receipt.semantic().generation,)
                .expect("restored semantic freshness resolves")
                .publication,
            FirstSlicePublicationMode::DurableSemanticRefinement
        );
        restored
            .resolve_generation(
                receipt.structural().repository,
                Some(receipt.structural().generation),
            )
            .expect("restored structural parent remains queryable");
        let file = restored
            .generations
            .generation(receipt.semantic().generation)
            .expect("restored semantic generation resolves")
            .document()
            .files
            .first()
            .expect("restored semantic generation retains its source record");
        let source = SourceRef::new(
            receipt.semantic().repository,
            receipt.semantic().generation,
            SourceSpan::new(file.id, 0, file.byte_length).expect("source span is valid"),
            file.content_hash,
            None,
        );
        let read = restored
            .source_read(receipt.semantic().generation, vec![source], &cancellation)
            .expect("restored semantic source hydrates lazily");
        assert_eq!(
            read.data.chunks[0].bytes,
            b"pub fn two_stage_value() -> u32 { 2 }\n"
        );
    }

    #[test]
    fn caller_requested_deep_generation_remains_oracle_authoritative() {
        let temporary = durable_test_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::write(
            repository_root.join("lib.rs"),
            "pub fn caller_deep() -> bool { true }\n",
        )
        .expect("deep fixture writes");
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"caller-deep-project-analyzer"),
            calls: Arc::new(AtomicUsize::new(0)),
            partitioned: false,
            syntax_facts_bounded: false,
        });
        let cancellation = deadline();
        let receipt = {
            let mut service = FirstSliceService::new_durable_with_project_analyzer(
                2,
                paths.state_dir(),
                analyzer,
                &cancellation,
            )
            .expect("durable project service initializes");
            service
                .index_repository_with_mode(
                    &repository_root,
                    FirstSliceIndexMode::Deep,
                    &cancellation,
                )
                .expect("caller-requested deep generation publishes")
        };
        assert!(receipt.oracle_allocated_bytes > 0);
        let generation_directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        assert!(generation_directory.join("oracle.sqlite3").is_file());
        assert!(!generation_directory.join("recovery-manifest.json").exists());
        assert!(!generation_directory.join("recovery.json.gz").exists());
    }

    #[test]
    fn corrupt_semantic_snapshot_restores_the_structural_predecessor() {
        let temporary = durable_test_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::write(
            repository_root.join("lib.rs"),
            "pub fn structural_fallback() -> bool { true }\n",
        )
        .expect("two-stage fixture writes");
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"corrupt-semantic-project-analyzer"),
            calls: Arc::new(AtomicUsize::new(0)),
            partitioned: false,
            syntax_facts_bounded: false,
        });
        let cancellation = deadline();

        let receipt = {
            let mut service = FirstSliceService::new_durable_with_project_analyzer(
                3,
                paths.state_dir(),
                Arc::clone(&analyzer),
                &cancellation,
            )
            .expect("durable project service initializes");
            service
                .index_repository_two_stage(&repository_root, &cancellation)
                .expect("two-stage generation publishes")
        };
        let semantic_directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.semantic().repository.to_string())
            .join(receipt.semantic().generation.to_string());
        fs::write(semantic_directory.join("recovery.json.gz"), b"corrupt")
            .expect("semantic snapshot corrupts");

        let restored = FirstSliceService::new_durable_with_project_analyzer(
            3,
            paths.state_dir(),
            analyzer,
            &cancellation,
        )
        .expect("structural predecessor restores");
        assert_eq!(
            restored.active_generation_for(receipt.structural().repository),
            Some(receipt.structural().generation)
        );
        assert!(!semantic_directory.exists());
        assert!(
            fs::read_dir(paths.state_dir().join("first-slice/quarantine"))
                .expect("quarantine enumerates")
                .any(|entry| {
                    entry
                        .expect("quarantine entry reads")
                        .file_name()
                        .to_string_lossy()
                        .contains(&receipt.semantic().generation.to_string())
                })
        );
    }

    #[test]
    fn two_stage_refinement_failure_preserves_the_structural_generation() {
        let temporary = durable_test_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::write(
            repository_root.join("lib.rs"),
            "pub fn structural_survivor() -> bool { true }\n",
        )
        .expect("fallback fixture writes");
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"failing-two-stage-analyzer"),
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::clone(&calls),
        });
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable_with_project_analyzer(
            3,
            paths.state_dir(),
            analyzer,
            &cancellation,
        )
        .expect("durable project service initializes");

        assert_eq!(
            service.index_repository_two_stage(&repository_root, &cancellation),
            Err(FirstSliceError::Adapter)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let repositories = service.list_repositories();
        let [repository] = repositories.as_slice() else {
            panic!("only the structural repository is active");
        };
        assert_eq!(repository.semantic_freshness, "pending_refinement");
        assert_eq!(
            service
                .generation_freshness(repository.repository, repository.active_generation)
                .expect("structural freshness resolves")
                .publication,
            FirstSlicePublicationMode::DurableStructuralStage
        );
    }

    #[test]
    fn exact_project_adapter_identity_changes_generation_provider_identity() {
        let structural = FirstSliceService::new(2).expect("structural service initializes");
        let first = FirstSliceService::new_with_storage(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            Some(Arc::new(FailingProjectAnalyzer {
                identity: content_hash(b"project-adapter-one"),
                error: FirstSliceProjectAnalysisError::Analysis,
                calls: Arc::new(AtomicUsize::new(0)),
            })),
        )
        .expect("first project service initializes");
        let second = FirstSliceService::new_with_storage(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            Some(Arc::new(FailingProjectAnalyzer {
                identity: content_hash(b"project-adapter-two"),
                error: FirstSliceProjectAnalysisError::Analysis,
                calls: Arc::new(AtomicUsize::new(0)),
            })),
        )
        .expect("second project service initializes");

        assert_ne!(
            structural
                .provider_set_hash(FirstSliceIndexMode::Structural)
                .expect("hash derives"),
            first
                .provider_set_hash(FirstSliceIndexMode::Deep)
                .expect("hash derives")
        );
        assert_eq!(
            structural
                .provider_set_hash(FirstSliceIndexMode::Structural)
                .expect("hash derives"),
            first
                .provider_set_hash(FirstSliceIndexMode::Structural)
                .expect("hash derives")
        );
        assert_ne!(
            first
                .provider_set_hash(FirstSliceIndexMode::Deep)
                .expect("hash derives"),
            second
                .provider_set_hash(FirstSliceIndexMode::Deep)
                .expect("hash derives")
        );
    }

    #[test]
    fn mixed_repository_reports_honest_per_language_tiers() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[
                ("src/lib.rs", "pub fn rust_value() -> u32 { 1 }\n"),
                (
                    "src/value.ts",
                    "export function typescriptValue(): number { return 2; }\n",
                ),
                ("src/value.py", "def python_value():\n    return 3\n"),
                (
                    "value.go",
                    "package sample\n\nfunc GoValue() int { return 4 }\n",
                ),
            ],
        );
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("service initializes");

        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .expect("mixed repository publishes");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("mixed repository status resolves");
        let tiers = status
            .coverage
            .iter()
            .map(|coverage| (coverage.language.as_str(), coverage.tier.as_str()))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(receipt.indexed_files, 4);
        assert_eq!(tiers["rust"], "tier_b");
        assert_eq!(tiers["typescript"], "tier_d");
        assert_eq!(tiers["python"], "tier_d");
        assert_eq!(tiers["go"], "tier_d");
        assert_eq!(
            service.list_repositories()[0].languages,
            vec!["go", "python", "rust", "typescript"]
        );
    }

    #[test]
    fn repository_reads_use_precomputed_language_coverage() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/lib.rs", "pub fn rust_value() -> u32 { 1 }\n")],
        );
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .expect("repository publishes");

        assert_eq!(
            service.list_repositories()[0].languages,
            vec!["rust".to_owned()]
        );
        assert!(
            service
                .language_coverage_by_generation
                .remove(&receipt.generation)
                .is_some()
        );
        assert!(service.list_repositories().is_empty());
        assert!(matches!(
            service.repository_status(receipt.repository, None),
            Err(FirstSliceError::CatalogCorrupt)
        ));
        assert!(matches!(
            service.support_inventory_snapshot(),
            Err(FirstSliceError::CatalogCorrupt)
        ));
    }

    #[test]
    fn every_audited_grammar_has_a_fail_closed_source_suffix() {
        let registry = GrammarRegistry::audited().expect("audited grammar registry initializes");
        let mapped = [
            "sample.c",
            "sample.cpp",
            "sample.cs",
            "sample.go",
            "sample.java",
            "sample.js",
            "sample.kt",
            "sample.php",
            "sample.py",
            "sample.rs",
            "sample.ts",
        ]
        .into_iter()
        .filter_map(source_language_from_path)
        .collect::<BTreeSet<_>>();
        let registered = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.language().as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(mapped, registered);
    }

    #[test]
    fn unsupported_text_repository_publishes_an_explicit_disposition() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::write(
            fixture.path().join("looks-like-rust.txt"),
            "pub struct UntrustedContentSignal;\n",
        )
        .expect("unsupported text writes");
        let mut service = FirstSliceService::new(2).expect("service initializes");

        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .expect("unsupported input disposition publishes");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("unsupported repository status resolves");

        assert_eq!(status.coverage.len(), 1);
        assert_eq!(status.coverage[0].language, "rust");
        assert_eq!(status.coverage[0].tier, "tier_d");
        assert_eq!(status.coverage[0].status, "unknown");
        assert_eq!(status.coverage[0].discovered_files, 1);
        assert_eq!(status.coverage[0].indexed_files, 0);
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "unsupported-language"
                && diagnostic.message == "source language has no configured analyzer"
        }));
    }

    #[test]
    fn unsupported_primary_languages_remain_visible_in_coverage() {
        let fixture = TempDir::new().expect("fixture root exists");
        let languages = [
            ("normalize.css", "css"),
            ("plugin.lua", "lua"),
            ("analysis.mlx", "matlab"),
            ("script.pl", "perl"),
            ("plot.R", "r"),
            ("schema.sql", "sql"),
            ("script.sh", "bash"),
            ("page.html", "html"),
            ("client.swift", "swift"),
            ("model.rb", "ruby"),
            ("request.dart", "dart"),
            ("setup.ps1", "powershell"),
            ("build.scala", "scala"),
            ("pipeline.groovy", "groovy"),
            ("boot.asm", "assembly"),
            ("token.sol", "solidity"),
        ];
        for (path, _) in languages {
            fs::write(fixture.path().join(path), "unsupported fixture\n")
                .expect("unsupported source writes");
        }
        let mut service = FirstSliceService::new(2).expect("service initializes");

        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .expect("unsupported language dispositions publish");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("unsupported repository status resolves");
        let coverage = status
            .coverage
            .iter()
            .map(|entry| {
                (
                    entry.language.as_str(),
                    (entry.discovered_files, entry.indexed_files),
                )
            })
            .collect::<BTreeMap<_, _>>();

        for (_, language) in languages {
            assert_eq!(coverage[language], (1, 0));
        }
    }

    #[test]
    fn unsupported_primary_sources_retain_exact_evidence_and_reason() {
        let fixture = TempDir::new().expect("fixture root exists");
        let sources = [
            ("normalize.css", "html { line-height: 1.15; }\n", "css"),
            (
                "Alamofire.swift",
                "public final class SessionDelegate { let requestAdapter = 1 }\n",
                "swift",
            ),
            (
                "Session.m",
                "@interface Session : NSObject\n@end\n",
                "objective-c",
            ),
            (
                "classify.m",
                "function result = classify(value)\nresult = value;\nend\n",
                "matlab",
            ),
            (
                "telescope.lua",
                "local telescopePicker = require('telescope.pickers')\n",
                "lua",
            ),
            (
                "rails_application.rb",
                "class RailsApplication\nend\n",
                "ruby",
            ),
            ("script.pl", "sub render { return 1; }\n", "perl"),
            (
                "mystery.sourceblob",
                "opaqueWidget configures lexicalFallback\n",
                "unknown",
            ),
        ];
        for (path, content, _) in sources {
            fs::write(fixture.path().join(path), content).expect("source fixture writes");
        }
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .expect("unsupported sources publish");
        let generation = service
            .generations
            .generation(receipt.generation)
            .expect("published generation remains retained");

        for (path, content, language) in sources {
            let file = generation
                .document()
                .files
                .iter()
                .find(|file| file.path == path)
                .expect("unsupported file remains represented");
            let coverage = service
                .source_file_coverage(receipt.generation, file.id)
                .expect("source coverage resolves");
            assert_eq!(coverage.language, language);
            assert_eq!(coverage.tier, AnalysisTier::TierD);
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
            let reference = file
                .evidence
                .source
                .clone()
                .expect("unsupported file carries source evidence");
            let read = service
                .source_read(receipt.generation, vec![reference.clone()], &deadline())
                .expect("unsupported source remains readable");
            assert_eq!(read.data.chunks[0].bytes, content.as_bytes());
            assert_eq!(read.data.chunks[0].language, language);
            let exact_identifier = content
                .split(|character: char| !(character == '_' || character.is_alphanumeric()))
                .find(|candidate| {
                    candidate.len() > 4
                        && candidate
                            .chars()
                            .next()
                            .is_some_and(|character| character.is_alphabetic())
                })
                .expect("fixture contains a searchable identifier");
            for (query, mode) in [
                (exact_identifier.to_owned(), LocateMode::Exact),
                (exact_identifier.to_owned(), LocateMode::Text),
                (path.to_owned(), LocateMode::Prefix),
            ] {
                let located = service
                    .code_locate(receipt.generation, query, mode, 10, 0, &deadline())
                    .expect("unsupported source locates");
                let hit = located
                    .data
                    .hits
                    .iter()
                    .find(|hit| hit.file == file.id)
                    .expect("file-only source hit is returned");
                assert_eq!(hit.symbol, None);
                assert_eq!(hit.kind, "file");
                assert_eq!(hit.source.as_ref(), Some(&reference));
            }
        }
        let gaps = service
            .coverage_gaps(receipt.repository, receipt.generation)
            .expect("coverage gaps resolve");
        assert!(gaps.iter().any(|gap| {
            gap.reason == FirstSliceCoverageGapReason::Unrecognized
                && gap.language.as_deref() == Some("unknown")
                && gap.files == 1
        }));
        for language in [
            "css",
            "swift",
            "objective-c",
            "matlab",
            "lua",
            "ruby",
            "perl",
        ] {
            assert!(gaps.iter().any(|gap| {
                gap.reason == FirstSliceCoverageGapReason::Unsupported
                    && gap.language.as_deref() == Some(language)
                    && gap.files == 1
            }));
        }
    }

    #[test]
    fn locate_language_aliases_select_the_canonical_installed_language() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn alias_target() -> u32 { 1 }\n",
        )
        .expect("Rust fixture writes");
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .expect("Rust fixture publishes");

        let located = service
            .code_locate_with_languages_and_budget(
                receipt.generation,
                "alias_target".to_owned(),
                LocateMode::Exact,
                vec!["rs".to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .expect("canonical language filter executes");

        assert_eq!(located.data.hits.len(), 1);
        assert_eq!(located.data.hits[0].language, "rust");
        assert!(located.data.hits[0].symbol.is_some());
    }

    #[test]
    fn bounded_source_fallback_reports_truncated_coverage() {
        let fixture = TempDir::new().expect("fixture root exists");
        let source = format!(
            "{} searchableNeedle {{ color: green; }}\n",
            "a".repeat(SOURCE_FALLBACK_TEXT_BYTES + 256)
        );
        fs::write(fixture.path().join("normalize.css"), source).expect("large CSS source writes");
        let mut service = FirstSliceService::new(2).expect("service initializes");

        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .expect("bounded source fallback publishes");
        let gaps = service
            .coverage_gaps(receipt.repository, receipt.generation)
            .expect("coverage gaps resolve");

        assert!(gaps.iter().any(|gap| {
            gap.reason == FirstSliceCoverageGapReason::Truncated
                && gap.language.as_deref() == Some("css")
                && gap.files == 1
        }));
    }

    #[test]
    fn kotlin_remains_structurally_supported() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::write(
            fixture.path().join("Session.kt"),
            "class Session { fun execute(): Int = 1 }\n",
        )
        .expect("Kotlin fixture writes");
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .expect("Kotlin source publishes");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("Kotlin repository status resolves");
        assert!(status.coverage.iter().any(|coverage| {
            coverage.language == "kotlin"
                && coverage.discovered_files == 1
                && coverage.indexed_files == 1
                && coverage.status == "bounded"
        }));
    }

    #[test]
    fn restored_repository_registration_reuses_reserved_identity() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::write(fixture.path().join("lib.rs"), "pub fn answer() {}\n")
            .expect("fixture source writes");
        let cancellation = deadline();
        let original = FirstSliceService::new(2)
            .expect("service initializes")
            .admit_repository(fixture.path(), &cancellation)
            .expect("repository admission succeeds");

        let restored = FirstSliceService::new(2).expect("replacement service initializes");
        restored
            .restore_repository_registration(original.root_identity, original.repository)
            .expect("durable registration restores");
        let repeated = restored
            .admit_repository(fixture.path(), &cancellation)
            .expect("restored repository is admitted");

        assert_eq!(repeated.repository, original.repository);
        assert_eq!(repeated.root_identity, original.root_identity);
        restored.release_index_admission(repeated);
        assert_eq!(
            restored
                .registered_repository_for_root(fixture.path(), &cancellation)
                .expect("restored registration is queryable"),
            Some(original.repository)
        );
        let catalog = restored
            .repository_catalog_page(
                CatalogPageRequest::new(
                    None,
                    None,
                    catalog::CatalogListFilter::new(None, None, None)
                        .expect("catalog filter is valid"),
                    catalog::CatalogPageSize::new(20).expect("page size is valid"),
                )
                .expect("catalog request is valid"),
                CatalogInstant::from_millis(0),
            )
            .expect("restored registration is publicly cataloged");
        let [entry] = catalog.items() else {
            panic!("one restored repository registration is expected");
        };
        assert_eq!(entry.repository(), original.repository);
        assert_eq!(entry.state(), CatalogRepositoryState::Indexing);
        assert!(entry.active_generation().is_none());
        assert_eq!(
            restored.repository_status(original.repository, None),
            Err(FirstSliceError::GenerationNotFound)
        );
        assert_eq!(
            restored
                .admit_repository(fixture.path(), &cancellation)
                .expect("failed reuse keeps durable reservation")
                .repository,
            original.repository
        );
    }

    #[test]
    fn repository_catalog_admits_more_than_128_roots() {
        const ROOT_COUNT: usize = 176;

        let fixture = TempDir::new().expect("fixture root exists");
        let service = FirstSliceService::new(8).expect("service initializes");
        let cancellation = deadline();
        let mut admissions = Vec::new();
        for ordinal in 0..ROOT_COUNT {
            let root = fixture.path().join(format!("repository-{ordinal}"));
            fs::create_dir(&root).expect("repository root exists");
            admissions.push(
                service
                    .admit_repository(&root, &cancellation)
                    .expect("repository is admitted"),
            );
        }

        assert_eq!(admissions.len(), ROOT_COUNT);
        assert!(
            admissions
                .iter()
                .all(FirstSliceIndexAdmission::created_repository_reservation)
        );
        let capacity = service
            .support_inventory_snapshot()
            .expect("support inventory preserves aggregate capacity")
            .repository_capacity;
        assert_eq!(capacity.registered_repository_count, 176);
        assert_eq!(capacity.pending_repository_count, 176);
        assert_eq!(capacity.active_repository_count, 0);
        assert_eq!(capacity.configured_maximum_repositories, 4_096);
        assert_eq!(capacity.effective_maximum_repositories, 1_024);
        assert_eq!(capacity.repository_headroom, 1_024 - 176);
    }

    #[test]
    fn repository_catalog_capacity_reports_observed_and_limit() {
        let fixture = TempDir::new().expect("fixture root exists");
        let service = FirstSliceService::new(4_096).expect("service initializes");
        let cancellation = deadline();
        for ordinal in 0..2 {
            let root = fixture.path().join(format!("repository-{ordinal}"));
            fs::create_dir(&root).expect("repository root exists");
            service
                .admit_repository(&root, &cancellation)
                .expect("repository is admitted");
        }
        let rejected = fixture.path().join("repository-rejected");
        fs::create_dir(&rejected).expect("rejected repository root exists");

        assert!(matches!(
            service.admit_repository(&rejected, &cancellation),
            Err(FirstSliceError::RepositoryCapacityLimit {
                observed: 3,
                limit: 2,
                configuration: FirstSliceRepositoryCapacityConfiguration::RetainedGenerations,
                pending_repositories: 2,
                reclaimable_repositories: 0,
            })
        ));
    }

    #[test]
    fn configured_repository_capacity_rejects_only_new_registrations() {
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some(
                "version = \"1.3\"\n[storage]\nmaximum_repositories = 2\nretained_generations = 2\n",
            ),
            2,
        )
        .expect("configured repository capacity resolves");
        let service = FirstSliceService::new_with_storage_policy(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            None,
            policy,
        )
        .expect("configured service initializes");
        let fixture = TempDir::new().expect("fixture root exists");
        let cancellation = deadline();
        for ordinal in 0..2 {
            let root = fixture.path().join(format!("repository-{ordinal}"));
            fs::create_dir(&root).expect("repository root exists");
            service
                .admit_repository(&root, &cancellation)
                .expect("configured repository is admitted");
        }
        let rejected = fixture.path().join("repository-rejected");
        fs::create_dir(&rejected).expect("rejected repository root exists");

        assert!(matches!(
            service.admit_repository(&rejected, &cancellation),
            Err(FirstSliceError::RepositoryCapacityLimit {
                observed: 3,
                limit: 2,
                configuration: FirstSliceRepositoryCapacityConfiguration::MaximumRepositories,
                pending_repositories: 2,
                reclaimable_repositories: 0,
            })
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn configured_repository_capacity_is_shared_by_admission_and_cold_reopen() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let indexed_root = durable_test_tempdir();
        fs::write(
            indexed_root.path().join("lib.rs"),
            "pub fn indexed() -> bool { true }\n",
        )
        .expect("indexed fixture writes");
        let cancellation = deadline();
        let policy = || {
            FirstSliceStoragePolicy::resolve_user_config(
                Some(
                    "version = \"1.3\"\n[storage]\nmaximum_repositories = 1\nretained_generations = 2\n",
                ),
                2,
            )
            .expect("configured repository capacity resolves")
        };

        let indexed = {
            let mut service = FirstSliceService::new_durable_with_policy(
                policy(),
                paths.state_dir(),
                &cancellation,
            )
            .expect("configured durable service initializes");
            service
                .index_rust_fixture(indexed_root.path(), &cancellation)
                .expect("first configured repository indexes")
        };

        let reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &cancellation)
                .expect("configured durable service reopens");
        assert_eq!(
            reopened.active_generation_for(indexed.repository),
            Some(indexed.generation)
        );
        assert_eq!(
            reopened
                .support_inventory_snapshot_without_storage_scan()
                .expect("reopened support capacity builds")
                .repository_capacity,
            FirstSliceRepositoryCapacityInventory {
                registered_repository_count: 1,
                pending_repository_count: 0,
                active_repository_count: 1,
                failed_repository_count: 0,
                pinned_repository_count: 0,
                reclaimable_repository_count: 1,
                pinning_supported: false,
                configured_maximum_repositories: 1,
                effective_maximum_repositories: 1,
                repository_headroom: 0,
            }
        );
        let rejected_root = durable_test_tempdir();
        fs::write(
            rejected_root.path().join("lib.rs"),
            "pub fn rejected() -> bool { true }\n",
        )
        .expect("rejected fixture writes");
        assert!(matches!(
            reopened.admit_repository(rejected_root.path(), &cancellation),
            Err(FirstSliceError::RepositoryCapacityLimit {
                observed: 2,
                limit: 1,
                configuration: FirstSliceRepositoryCapacityConfiguration::MaximumRepositories,
                pending_repositories: 0,
                reclaimable_repositories: 1,
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn repository_operations_reject_symbolic_link_roots() {
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().expect("fixture root exists");
        let target = fixture.path().join("target");
        fs::create_dir(&target).expect("repository target exists");
        fs::write(target.join("lib.rs"), "pub fn answer() {}\n").expect("fixture source writes");
        let linked_root = fixture.path().join("linked");
        symlink(&target, &linked_root).expect("repository root symlink is created");

        assert_linked_repository_root_is_rejected(&linked_root);
    }

    #[cfg(windows)]
    #[test]
    fn repository_operations_reject_windows_reparse_roots() {
        use std::os::windows::fs::symlink_dir;

        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1_314;

        let fixture = TempDir::new().expect("fixture root exists");
        let target = fixture.path().join("target");
        fs::create_dir(&target).expect("repository target exists");
        fs::write(target.join("lib.rs"), "pub fn answer() {}\n").expect("fixture source writes");
        let linked_root = fixture.path().join("linked");
        match symlink_dir(&target, &linked_root) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => return,
            Err(error) => panic!("repository root reparse point is created: {error}"),
        }

        assert_linked_repository_root_is_rejected(&linked_root);
    }

    #[cfg(any(unix, windows))]
    fn assert_linked_repository_root_is_rejected(root: &Path) {
        let service = FirstSliceService::new(2).expect("service initializes");
        let cancellation = deadline();

        assert!(matches!(
            service.admit_repository(root, &cancellation),
            Err(FirstSliceError::Repository)
        ));
        assert!(matches!(
            service.prepare_repository(root, &cancellation),
            Err(FirstSliceError::Repository)
        ));
        assert!(service.list_repositories().is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_service_restores_repository_query_and_source_state() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn durable_answer() -> u32 {\n    42\n}\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let (_, empty_restore) = FirstSliceService::open_durable_deferred(2, paths.state_dir())
            .expect("empty durable boundary opens");
        assert!(
            !empty_restore
                .has_active_restore_work()
                .expect("empty durable metadata is readable")
        );
        drop(empty_restore);

        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes")
        };

        let (mut restored, deferred) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir())
                .expect("durable boundary opens without scanning generations");
        assert!(restored.list_repositories().is_empty());
        assert!(
            deferred
                .has_active_restore_work()
                .expect("activation metadata is readable")
        );
        let restored_state = deferred
            .restore(&cancellation)
            .expect("durable generation verifies");
        restored
            .install_deferred_restore(restored_state, &cancellation)
            .expect("verified durable generation installs");
        assert_eq!(
            restored.source_snapshots.retained_bytes(),
            source_generation_reference_bytes(0).expect("empty generation charge fits")
        );
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        assert_eq!(
            restored
                .resolve_generation(receipt.repository, None)
                .expect("restored generation resolves")
                .receipt,
            receipt
        );
        let located = restored
            .code_locate(
                receipt.generation,
                "durable_answer".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("restored lexical query succeeds");
        let source = located
            .data
            .hits
            .first()
            .and_then(|hit| hit.source.clone())
            .expect("restored query carries source evidence");
        let read = restored
            .source_read(receipt.generation, vec![source], &cancellation)
            .expect("restored source bytes remain readable");
        assert_eq!(read.data.generation, receipt.generation);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn deferred_bootstrap_preserves_identity_across_concurrent_publication() {
        fn global_activation_sequence(
            paths: &RuntimePaths,
            receipt: &FirstSliceIndexReceipt,
        ) -> u64 {
            let generation = receipt.generation.to_string();
            let repository = paths
                .state_dir()
                .join("first-slice/repositories")
                .join(receipt.repository.to_string());
            fs::read_dir(repository)
                .expect("repository directory reads")
                .map(|entry| entry.expect("repository entry reads"))
                .find_map(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("activation-"))
                        .then(|| {
                            serde_json::from_slice::<serde_json::Value>(
                                &fs::read(entry.path().join("activation.json"))
                                    .expect("activation manifest reads"),
                            )
                            .expect("activation manifest parses")
                        })
                        .filter(|manifest| {
                            manifest["generation"].as_str() == Some(generation.as_str())
                        })
                })
                .and_then(|manifest| manifest["global_activation_sequence"].as_u64())
                .expect("global activation sequence is present")
        }

        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let old_fixtures = [durable_test_tempdir(), durable_test_tempdir()];
        for (ordinal, fixture) in old_fixtures.iter().enumerate() {
            fs::write(
                fixture.path().join("lib.rs"),
                format!("pub fn old_{ordinal}() -> u32 {{ {ordinal} }}\n"),
            )
            .expect("old fixture source writes");
        }
        let new_fixture = durable_test_tempdir();
        fs::write(
            new_fixture.path().join("lib.rs"),
            "pub fn newly_published() -> u32 { 3 }\n",
        )
        .expect("new fixture source writes");
        let cancellation = deadline();

        let old_receipts = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            old_fixtures
                .iter()
                .map(|fixture| {
                    service
                        .index_rust_fixture(fixture.path(), &cancellation)
                        .expect("old durable generation publishes")
                })
                .collect::<Vec<_>>()
        };

        let (mut service, deferred) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir())
                .expect("deferred durable service opens");
        assert!(service.list_repositories().is_empty());
        for (fixture, receipt) in old_fixtures.iter().zip(&old_receipts) {
            assert_eq!(
                service
                    .registered_repository_for_root(fixture.path(), &cancellation)
                    .expect("bootstrapped root registration resolves"),
                Some(receipt.repository)
            );
        }

        let new_receipt = service
            .index_rust_fixture(new_fixture.path(), &cancellation)
            .expect("new repository publishes before payload restore");
        assert!(
            old_receipts
                .iter()
                .all(|receipt| receipt.repository != new_receipt.repository)
        );
        for target in deferred
            .active_targets()
            .expect("bootstrapped recovery targets remain available")
        {
            let restored = deferred
                .restore_active_repository(target.repository(), &cancellation)
                .expect("old active generation verifies");
            service
                .install_progressive_deferred_restore(restored, &cancellation)
                .expect("old active generation installs progressively");
        }
        for receipt in &old_receipts {
            assert_eq!(
                service.active_generation_for(receipt.repository),
                Some(receipt.generation)
            );
        }
        assert_eq!(
            service.active_generation_for(new_receipt.repository),
            Some(new_receipt.generation)
        );
        fs::write(
            old_fixtures[0].path().join("lib.rs"),
            "pub fn old_0_updated() -> u32 { 10 }\n",
        )
        .expect("old fixture update writes");
        let updated_old_receipt = service
            .index_rust_fixture(old_fixtures[0].path(), &cancellation)
            .expect("restored repository publishes with its bootstrapped frontier");
        assert_eq!(updated_old_receipt.repository, old_receipts[0].repository);
        drop(service);

        let mut activation_sequences = old_receipts
            .iter()
            .map(|receipt| global_activation_sequence(&paths, receipt))
            .collect::<Vec<_>>();
        let new_activation_sequence = global_activation_sequence(&paths, &new_receipt);
        let updated_activation_sequence = global_activation_sequence(&paths, &updated_old_receipt);
        assert!(
            activation_sequences
                .iter()
                .all(|sequence| *sequence < new_activation_sequence)
        );
        assert!(updated_activation_sequence > new_activation_sequence);
        activation_sequences.push(new_activation_sequence);
        activation_sequences.push(updated_activation_sequence);
        activation_sequences.sort_unstable();
        activation_sequences.dedup();
        assert_eq!(activation_sequences.len(), 4);

        let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("fully populated catalog reopens cleanly");
        for receipt in [&updated_old_receipt, &old_receipts[1], &new_receipt] {
            assert_eq!(
                reopened.active_generation_for(receipt.repository),
                Some(receipt.generation)
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn healthy_active_recovery_preserves_inflight_publication_accounting() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let retained_fixture = durable_test_tempdir();
        fs::write(
            retained_fixture.path().join("lib.rs"),
            "pub fn retained_during_publication() -> u32 { 1 }\n",
        )
        .expect("retained fixture source writes");
        let new_fixture = durable_test_tempdir();
        fs::write(
            new_fixture.path().join("lib.rs"),
            "pub fn published_during_recovery() -> u32 { 2 }\n",
        )
        .expect("new fixture source writes");
        let cancellation = deadline();
        let retained = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(retained_fixture.path(), &cancellation)
                .expect("retained generation publishes")
        };

        let (mut service, deferred) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir())
                .expect("deferred durable service opens");
        let prepared = service
            .prepare_repository(new_fixture.path(), &cancellation)
            .expect("independent generation prepares while recovery is pending");
        let restored = deferred
            .restore_active_repository(retained.repository, &cancellation)
            .expect("healthy active generation verifies without repair");
        let published = service
            .publish_prepared(prepared, &cancellation)
            .expect("healthy recovery does not invalidate publication accounting");
        service
            .install_progressive_deferred_restore(restored, &cancellation)
            .expect("retained generation installs after independent publication");

        assert_ne!(published.repository, retained.repository);
        assert_eq!(
            service.active_generation_for(published.repository),
            Some(published.generation)
        );
        assert_eq!(
            service.active_generation_for(retained.repository),
            Some(retained.generation)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn deferred_bootstrap_leaves_generation_payloads_unopened() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn payload_probe() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes")
        };
        let oracle = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string())
            .join("oracle.sqlite3");
        fs::write(oracle, b"not a sqlite database").expect("oracle payload corrupts");

        let (service, deferred) = FirstSliceService::open_durable_deferred(2, paths.state_dir())
            .expect("bounded bootstrap ignores generation payload bytes");
        assert_eq!(
            service
                .registered_repository_for_root(fixture.path(), &cancellation)
                .expect("bootstrapped root registration resolves"),
            Some(receipt.repository)
        );
        assert_eq!(
            deferred
                .active_targets()
                .expect("bootstrap target metadata is available")
                .iter()
                .map(FirstSliceRecoveryTarget::repository)
                .collect::<Vec<_>>(),
            [receipt.repository]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn deferred_bootstrap_rejects_conflicting_retained_root_bindings() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        let source = fixture.path().join("lib.rs");
        fs::write(&source, "pub fn binding_probe() -> u32 { 1 }\n").expect("initial source writes");
        let cancellation = deadline();
        let first = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let first = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("first generation publishes");
            fs::write(&source, "pub fn binding_probe() -> u32 { 2 }\n")
                .expect("successor source writes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("successor generation publishes");
            first
        };
        let other_root = durable_test_tempdir();
        let other_canonical = canonical_repository_root(other_root.path(), &cancellation)
            .expect("other root canonicalizes");
        let other_identity =
            repository_path_hash(&other_canonical).expect("other root identity hashes");
        let other_path = sanitized_repository_root_path(&other_canonical)
            .expect("other root path is persistable");
        let manifest_path = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(first.repository.to_string())
            .join(first.generation.to_string())
            .join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("retained manifest reads"))
                .expect("retained manifest parses");
        manifest["root_identity"] =
            serde_json::to_value(other_identity).expect("other identity serializes");
        manifest["root_path"] = json!(other_path);
        fs::write(
            manifest_path,
            serde_json::to_vec(&manifest).expect("conflicting manifest serializes"),
        )
        .expect("retained manifest binding changes");

        assert!(matches!(
            FirstSliceService::open_durable_deferred(2, paths.state_dir()),
            Err(FirstSliceError::CatalogCorrupt)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_service_restores_file_only_source_fallback() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("normalize.css"),
            "html { line-height: 1.15; }\n",
        )
        .expect("CSS fixture writes");
        let cancellation = deadline();
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_repository(fixture.path(), &cancellation)
                .expect("file-only generation publishes")
        };

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service restores");
        let located = restored
            .code_locate(
                receipt.generation,
                "line".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("file-only lexical query succeeds after restart");
        let hit = located
            .data
            .hits
            .first()
            .expect("file-only hit remains queryable");
        assert_eq!(hit.symbol, None);
        assert_eq!(hit.kind, "file");
        let source = hit
            .source
            .clone()
            .expect("file-only hit retains exact source evidence");
        let read = restored
            .source_read(receipt.generation, vec![source], &cancellation)
            .expect("file-only source remains readable after restart");
        assert_eq!(read.data.chunks[0].bytes, b"html { line-height: 1.15; }\n");
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_service_lazily_hydrates_sources_absent_from_fallback_retention() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn retained_answer() -> u32 { 42 }\n",
        )
        .expect("Rust fixture writes");
        fs::write(
            fixture.path().join("normalize.css"),
            "html { line-height: 1.15; }\n",
        )
        .expect("CSS fixture writes");
        let cancellation = deadline();
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_repository(fixture.path(), &cancellation)
                .expect("mixed-language generation publishes")
        };

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service restores");
        let retained = restored
            .source_snapshots
            .snapshots(receipt.generation)
            .expect("restored source generation exists");
        assert_eq!(
            retained.len(),
            1,
            "only the file-only fallback source is retained eagerly"
        );

        let located = restored
            .code_locate(
                receipt.generation,
                "retained_answer".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("structural source locates after restart");
        let source = located.data.hits[0]
            .source
            .clone()
            .expect("structural hit retains exact source evidence");
        let read = restored
            .source_read(receipt.generation, vec![source], &cancellation)
            .expect("non-fallback source hydrates beside retained fallback bytes");
        assert!(
            std::str::from_utf8(&read.data.chunks[0].bytes)
                .expect("Rust fixture is UTF-8")
                .contains("retained_answer")
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn failed_semantic_preparation_keeps_structural_sources_across_restart() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("value.py"),
            "def retained_answer():\n    return 42\n",
        )
        .expect("Python fixture writes");
        let cancellation = deadline();
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"bounded-restart-project-analyzer"),
            calls,
            partitioned: false,
            syntax_facts_bounded: true,
        });
        let structural = {
            let mut service = FirstSliceService::new_durable_with_project_analyzer(
                2,
                paths.state_dir(),
                analyzer,
                &cancellation,
            )
            .expect("durable project service initializes");
            let structural = service
                .index_repository_with_mode(
                    fixture.path(),
                    FirstSliceIndexMode::Structural,
                    &cancellation,
                )
                .expect("structural generation publishes");
            let error = match service.prepare_semantic_refinement(
                fixture.path(),
                structural.generation,
                &cancellation,
            ) {
                Ok(_) => panic!("incomplete project coverage cannot publish"),
                Err(error) => error,
            };
            assert_eq!(
                error,
                FirstSliceError::ProjectCoverageIncomplete {
                    language: FirstSliceProjectCoverageLanguage::Python,
                    reason: FirstSliceProjectCoverageReason::StructuralDeclarationLoss,
                    missing_declarations: 1,
                }
            );
            structural
        };

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("structural generation restores after semantic fallback");
        let located = restored
            .code_locate(
                structural.generation,
                "retained_answer".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("structural declaration remains queryable after restart");
        let source = located.data.hits[0]
            .source
            .clone()
            .expect("structural declaration retains source evidence");
        let read = restored
            .source_read(structural.generation, vec![source], &cancellation)
            .expect("structural source remains readable after semantic fallback");
        assert!(
            std::str::from_utf8(&read.data.chunks[0].bytes)
                .expect("Python fixture is UTF-8")
                .contains("retained_answer")
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_restore_accepts_legacy_inline_source_and_recovery_storage() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn legacy_inline_source() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let receipt = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes");
            let generation_directory = paths
                .state_dir()
                .join("first-slice/repositories")
                .join(receipt.repository.to_string())
                .join(receipt.generation.to_string());
            durable::write_legacy_recovery_snapshot(
                &generation_directory,
                service
                    .generations
                    .generation(receipt.generation)
                    .expect("published generation resolves"),
            )
            .expect("legacy recovery snapshot writes");
            receipt
        };
        let repository_directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string());
        let generation_directory = repository_directory.join(receipt.generation.to_string());
        let sources_directory = generation_directory.join("sources");
        for source in fs::read_dir(&sources_directory).expect("source pointers enumerate") {
            let source = source.expect("source pointer entry reads").path();
            let pointer = fs::read_to_string(&source).expect("source pointer is UTF-8");
            let mut fields = pointer.lines();
            assert_eq!(fields.next(), Some("rootlight.source-pointer/1"));
            let digest = fields.next().expect("source pointer retains its digest");
            let expected_bytes = fields
                .next()
                .expect("source pointer retains its byte length")
                .parse::<u64>()
                .expect("source pointer byte length is valid");
            assert!(fields.next().is_none());
            let content = fs::read(
                repository_directory
                    .join("source-blobs")
                    .join(digest)
                    .join("content"),
            )
            .expect("content-addressed source blob reads");
            assert_eq!(
                u64::try_from(content.len()).expect("source length fits u64"),
                expected_bytes
            );
            fs::write(source, content).expect("legacy inline source replaces the pointer");
        }
        let manifest_path = generation_directory.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("generation manifest reads"))
                .expect("generation manifest is valid JSON");
        manifest["version"] = serde_json::json!(1);
        manifest
            .as_object_mut()
            .expect("generation manifest is an object")
            .remove("source_storage");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("legacy manifest serializes"),
        )
        .expect("legacy generation manifest writes");
        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("legacy generation restores");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        let located = restored
            .code_locate(
                receipt.generation,
                "legacy_inline_source".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("legacy generation remains queryable");
        let source = located
            .data
            .hits
            .first()
            .and_then(|hit| hit.source.clone())
            .expect("legacy query carries source evidence");
        let read = restored
            .source_read(receipt.generation, vec![source], &cancellation)
            .expect("legacy inline source bytes remain readable");
        assert_eq!(
            read.data.chunks[0].bytes,
            b"pub fn legacy_inline_source() -> u32 { 42 }\n"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_restore_accepts_legacy_gzip_sidecar_and_falls_back_to_oracle() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn legacy_gzip_source() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let receipt = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes");
            let generation_directory = paths
                .state_dir()
                .join("first-slice/repositories")
                .join(receipt.repository.to_string())
                .join(receipt.generation.to_string());
            durable::write_legacy_gzip_recovery_snapshot(
                &generation_directory,
                service
                    .generations
                    .generation(receipt.generation)
                    .expect("published generation resolves"),
            )
            .expect("legacy gzip recovery sidecar writes");
            receipt
        };
        let generation_directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        assert!(generation_directory.join("oracle.sqlite3").is_file());
        assert!(generation_directory.join("recovery.json.gz").is_file());

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("legacy gzip sidecar restores");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        drop(restored);

        fs::write(generation_directory.join("recovery.json.gz"), b"corrupt")
            .expect("legacy gzip sidecar corrupts");
        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("legacy corruption falls back to the authoritative oracle");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        assert!(generation_directory.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_durable_generation_publishes_and_restores_past_legacy_path_limit() {
        use std::os::windows::ffi::OsStrExt as _;

        fn utf16_len(path: &Path) -> usize {
            path.as_os_str().encode_wide().count()
        }

        let storage = durable_test_tempdir();
        let storage_root = storage.path().to_path_buf();
        let padding_length = 107_usize
            .checked_sub(utf16_len(&storage_root) + 1)
            .expect("temporary root leaves room for the long state path");
        assert!(
            padding_length <= 255,
            "fixture padding must fit one Windows path component"
        );
        let state_root = storage_root.join("s".repeat(padding_length));
        assert_eq!(utf16_len(&state_root), 107);
        let paths = RuntimePaths::new(state_root, storage_root.join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");

        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn long_path_answer() -> u32 {\n    42\n}\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );

        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes across the legacy path limit")
        };
        let projected_journal = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(format!("stage-{}-0000000000000000", receipt.generation))
            .join("oracle.sqlite3-journal");
        assert!(utf16_len(&projected_journal) > 260);

        let (mut restored, deferred) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir())
                .expect("durable boundary reopens");
        let restored_state = deferred
            .restore(&cancellation)
            .expect("long-path generation verifies");
        restored
            .install_deferred_restore(restored_state, &cancellation)
            .expect("long-path generation installs");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        assert!(
            !restored
                .code_locate(
                    receipt.generation,
                    "long_path_answer".to_owned(),
                    LocateMode::Exact,
                    8,
                    0,
                    &cancellation,
                )
                .expect("restored long-path query succeeds")
                .data
                .hits
                .is_empty()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_restore_makes_active_generation_ready_before_retained_history() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn retained_answer() -> u32 { 1 }\n")
            .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );

        let (retained, active) = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let retained = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("retained generation publishes");
            fs::write(&source, "pub fn active_answer() -> u32 { 2 }\n")
                .expect("fixture source changes");
            let active = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("active generation publishes");
            (retained, active)
        };
        let active_directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(active.repository.to_string())
            .join(active.generation.to_string());
        let generation_manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(active_directory.join("manifest.json"))
                .expect("generation manifest remains readable"),
        )
        .expect("generation manifest is valid JSON");
        assert_eq!(generation_manifest["version"], 2);
        assert!(generation_manifest.get("recovery").is_none());
        assert_eq!(generation_manifest["source_storage"]["version"], 1);
        assert!(active_directory.join("oracle.sqlite3").is_file());
        assert!(!active_directory.join("recovery-manifest.json").exists());
        assert!(!active_directory.join("recovery.json.gz").exists());
        assert!(active_directory.join("incremental.json").is_file());

        let (mut restored, deferred) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir())
                .expect("durable boundary opens without scanning generations");
        let bootstrap_capacity = restored
            .support_inventory_snapshot_without_storage_scan()
            .expect("deferred bootstrap capacity builds")
            .repository_capacity;
        assert_eq!(bootstrap_capacity.registered_repository_count, 1);
        assert_eq!(bootstrap_capacity.pending_repository_count, 1);
        assert_eq!(bootstrap_capacity.active_repository_count, 0);
        assert_eq!(bootstrap_capacity.reclaimable_repository_count, 0);
        let active_state = deferred
            .restore_active(&cancellation)
            .expect("active generation verifies");
        assert_eq!(
            active_state.generation_ids(),
            BTreeSet::from([active.generation])
        );
        restored
            .install_deferred_restore(active_state, &cancellation)
            .expect("active generation installs");
        assert_eq!(
            restored.active_generation_for(active.repository),
            Some(active.generation)
        );
        let active_capacity = restored
            .support_inventory_snapshot_without_storage_scan()
            .expect("progressively restored capacity builds")
            .repository_capacity;
        assert_eq!(active_capacity.registered_repository_count, 1);
        assert_eq!(active_capacity.pending_repository_count, 0);
        assert_eq!(active_capacity.active_repository_count, 1);
        assert_eq!(active_capacity.reclaimable_repository_count, 1);
        assert_eq!(
            restored
                .incremental_evidence(active.generation)
                .expect("active incremental evidence survives restart")
                .strategy(),
            FirstSliceBuildStrategy::DependencyDirected
        );
        assert!(
            restored
                .incremental_evidence(active.generation)
                .expect("active incremental evidence survives restart")
                .rebuilt_normalized_facts()
                > 0
        );
        assert!(
            restored
                .code_locate(
                    active.generation,
                    "active_answer".to_owned(),
                    LocateMode::Exact,
                    8,
                    0,
                    &cancellation,
                )
                .expect("active generation is queryable")
                .data
                .hits
                .iter()
                .any(|hit| hit.identifier == "active_answer")
        );

        let remaining_state = deferred
            .restore_retained_repository(
                active.repository,
                &BTreeSet::from([active.generation]),
                &cancellation,
            )
            .expect("retained history verifies through the read-only recovery path");
        assert_eq!(
            remaining_state.generation_ids(),
            BTreeSet::from([retained.generation])
        );
        restored
            .install_additional_deferred_restore(remaining_state, &cancellation)
            .expect("retained history installs without changing the active generation");
        assert_eq!(
            restored.active_generation_for(active.repository),
            Some(active.generation)
        );
        assert!(
            restored
                .code_locate(
                    retained.generation,
                    "retained_answer".to_owned(),
                    LocateMode::Exact,
                    8,
                    0,
                    &cancellation,
                )
                .expect("retained generation is queryable")
                .data
                .hits
                .iter()
                .any(|hit| hit.identifier == "retained_answer")
        );
        assert_eq!(
            restored
                .incremental_evidence(retained.generation)
                .expect("retained incremental evidence survives restart")
                .strategy(),
            FirstSliceBuildStrategy::Initial
        );
        assert!(
            restored
                .incremental_evidence(retained.generation)
                .expect("retained incremental evidence survives restart")
                .rebuilt_normalized_facts()
                > 0
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_structural_generation_without_semantic_provider_is_semantically_stale() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn durable_answer() -> u32 { 1 }\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let generation = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("durable structural generation publishes");

        assert_eq!(
            service
                .generation_freshness(generation.repository, generation.generation)
                .expect("generation freshness resolves"),
            FirstSliceFreshnessStatus {
                structural: FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan,
                semantic: FirstSliceObservedFreshness::PendingSemanticRefinement,
                publication: FirstSlicePublicationMode::DurableSingleStage,
                two_stage: FirstSliceTwoStageAvailability::UnavailableWithoutSemanticRefinement,
            }
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_disk_admission_preserves_the_active_generation() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn durable_answer() -> u32 { 1 }\n")
            .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let active = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial durable generation publishes");
        let retained_receipts = service.receipts.len();

        fs::write(&source, "pub fn durable_answer() -> u32 { 2 }\n")
            .expect("fixture source changes");
        service.set_available_disk_bytes_override(0);
        assert!(matches!(
            service.prepare_rust_fixture(fixture.path(), &cancellation),
            Err(FirstSliceError::InsufficientDiskSpace {
                available_bytes: 0,
                ..
            })
        ));
        assert_eq!(
            service.active_generation_for(active.repository),
            Some(active.generation)
        );
        assert_eq!(service.receipts.len(), retained_receipts);
        assert_eq!(
            service
                .resolve_generation(active.repository, None)
                .expect("active generation remains queryable")
                .receipt,
            active
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn detached_admission_defers_disk_failure_until_preparation() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn deferred_admission() -> u32 { 1 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        service.set_available_disk_bytes_override(0);

        let mut admission = service
            .admit_repository_for_detached_operation(fixture.path(), &cancellation)
            .expect("detached identity admission does not scan durable storage");
        let error = match service.prepare_repository_after_admission_with_progress(
            fixture.path(),
            FirstSliceIndexMode::Structural,
            &mut admission,
            &cancellation,
            |_| {},
        ) {
            Ok(_) => panic!("preparation must enforce deferred storage admission"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            FirstSliceError::InsufficientDiskSpace {
                available_bytes: 0,
                ..
            }
        ));
        assert_eq!(
            service
                .support_inventory_snapshot_without_storage_scan()
                .expect("live failed admission remains accounted")
                .repository_capacity
                .pending_repository_count,
            1
        );
        service.release_index_admission(admission);
        assert!(service.list_repositories().is_empty());
        let released = service
            .support_inventory_snapshot_without_storage_scan()
            .expect("failed admission cleanup restores capacity")
            .repository_capacity;
        assert_eq!(released.registered_repository_count, 0);
        assert_eq!(released.pending_repository_count, 0);
        assert_eq!(released.failed_repository_count, 0);
        assert_eq!(
            released.repository_headroom,
            released.effective_maximum_repositories
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_output_admission_rejects_expansion_before_staging() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = b"pub fn expanded_output() -> u32 { 42 }\n";
        fs::write(fixture.path().join("src/lib.rs"), source).expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let source_bytes = u64::try_from(source.len()).expect("fixture length is representable");
        let source_only_reservation =
            durable_staging_reservation(source_bytes).expect("source reservation is valid");
        service.set_available_disk_bytes_override(source_only_reservation);

        let error = match service.prepare_rust_fixture(fixture.path(), &cancellation) {
            Ok(_) => panic!("normalized output expansion exceeds the source-only reservation"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            FirstSliceError::InsufficientDiskSpace {
                required_bytes,
                available_bytes,
            } if required_bytes > available_bytes
                && available_bytes == source_only_reservation
        ));
        assert!(service.receipts.is_empty());
    }

    #[test]
    fn generation_memory_admission_rejects_oversized_finalization() {
        let serialized_bytes = MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
            .checked_add(1)
            .expect("test observation is representable");

        assert!(matches!(
            ensure_generation_memory_admission(serialized_bytes),
            Err(FirstSliceError::GenerationMemoryLimit {
                breakdown: GenerationMemoryBreakdown {
                    retained_bytes: 0,
                    reserved_bytes: 0,
                    owned_bytes: 0,
                    referenced_bytes: 0,
                    mapped_bytes: 0,
                    staged_bytes,
                    shared_bytes: 0,
                },
                observed,
                limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            }) if observed == serialized_bytes && staged_bytes == serialized_bytes
        ));
    }

    #[test]
    fn logical_projection_preflight_counts_three_document_charges_and_graph_workspace() {
        assert_eq!(
            logical_projection_staged_memory_bytes(1_024, 512)
                .expect("logical projection peak is representable"),
            3_584
        );
        assert_eq!(
            logical_projection_staged_memory_bytes(u64::MAX / 3 + 1, 0),
            Err(FirstSliceError::Limits)
        );
        assert_eq!(
            logical_projection_staged_memory_bytes(u64::MAX / 3, 2),
            Err(FirstSliceError::Limits)
        );

        let serialized_bytes = MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES / 3 + 1;
        assert!(
            serialized_bytes * 2 < MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            "the former two-charge model would have admitted this projection"
        );
        let required = logical_projection_staged_memory_bytes(serialized_bytes, 0)
            .expect("three-charge observation is representable");
        let service = FirstSliceService::new(2).expect("service initializes");
        assert!(matches!(
            service.preflight_generation_memory_capacity(
                required,
                PendingGenerationMemory::Staged,
            ),
            Err(FirstSliceError::GenerationMemoryLimit {
                breakdown: GenerationMemoryBreakdown {
                    retained_bytes: 0,
                    reserved_bytes: 0,
                    owned_bytes: 0,
                    referenced_bytes: 0,
                    mapped_bytes: 0,
                    staged_bytes,
                    shared_bytes: 0,
                },
                observed,
                limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            }) if staged_bytes == required && observed == required
        ));
    }

    #[test]
    fn generation_memory_breakdown_distinguishes_reservation_from_retained_ownership() {
        let retained = 9 * 1024_u64.pow(3);
        let reserved = 8 * 1024_u64.pow(3);

        assert_eq!(
            generation_memory_limit(retained, reserved, PendingGenerationMemory::Reserved),
            FirstSliceError::GenerationMemoryLimit {
                breakdown: GenerationMemoryBreakdown {
                    retained_bytes: retained,
                    reserved_bytes: reserved,
                    owned_bytes: retained,
                    referenced_bytes: 0,
                    mapped_bytes: 0,
                    staged_bytes: 0,
                    shared_bytes: 0,
                },
                observed: retained + reserved,
                limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            }
        );
    }

    #[test]
    fn retained_generation_charge_does_not_multiply_serialized_storage() {
        let serialized_bytes = 6 * 1024_u64.pow(3);

        assert_eq!(
            ensure_generation_memory_admission(serialized_bytes)
                .expect("large normalized generation remains admissible"),
            serialized_bytes
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_status_reports_bytes_within_cold_index_verifier_ratios() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let mut source = String::with_capacity(64 * 1024);
        source.push_str("/*");
        source.push_str(&"retained-durable-state-ratio ".repeat(2_048));
        source.push_str("*/\n");
        source.push_str("pub fn durable_ratio_fixture() -> bool { true }\n");
        fs::write(fixture.path().join("src/lib.rs"), &source).expect("fixture source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");

        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("durable generation publishes");
        let status = service
            .repository_status(receipt.repository, Some(receipt.generation))
            .expect("published generation status resolves");
        let source_bytes = u64::try_from(source.len()).expect("fixture length fits u64");
        let byte_ratio_limit = source_bytes
            .checked_mul(512)
            .expect("source-byte ratio limit is representable");
        let file_ratio_limit = receipt
            .indexed_files
            .checked_mul(4 * 1024 * 1024)
            .expect("file ratio limit is representable");

        assert_eq!(
            status.retained_durable_bytes,
            receipt.retained_durable_bytes
        );
        assert!(status.retained_durable_bytes > 0);
        assert!(
            status.retained_durable_bytes <= byte_ratio_limit,
            "retained={} byte_limit={byte_ratio_limit}",
            status.retained_durable_bytes
        );
        assert!(
            status.retained_durable_bytes <= file_ratio_limit,
            "retained={} file_limit={file_ratio_limit}",
            status.retained_durable_bytes
        );
    }

    #[test]
    fn generation_memory_budget_evicts_the_globally_oldest_inactive_generation() {
        let first_repository = TempDir::new().expect("first repository exists");
        let second_repository = TempDir::new().expect("second repository exists");
        fs::create_dir(first_repository.path().join("src")).expect("first source directory exists");
        fs::create_dir(second_repository.path().join("src"))
            .expect("second source directory exists");
        let first_source = first_repository.path().join("src/lib.rs");
        fs::write(&first_source, "pub fn first() -> u32 { 1 }\n").expect("first source writes");
        fs::write(
            second_repository.path().join("src/lib.rs"),
            "pub fn second() -> u32 { 2 }\n",
        )
        .expect("second source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(3).expect("service initializes");
        let inactive = service
            .index_rust_fixture(first_repository.path(), &cancellation)
            .expect("first generation publishes");
        fs::write(&first_source, "pub fn first() -> u32 { 3 }\n").expect("first source changes");
        let first_active = service
            .index_rust_fixture(first_repository.path(), &cancellation)
            .expect("first successor publishes");
        let second_active = service
            .index_rust_fixture(second_repository.path(), &cancellation)
            .expect("second repository publishes");
        let gibibyte = 1024_u64 * 1024 * 1024;
        service
            .generation_memory_bytes
            .insert(inactive.generation, 8 * gibibyte);
        service
            .generation_memory_bytes
            .insert(first_active.generation, 3 * gibibyte);
        service
            .generation_memory_bytes
            .insert(second_active.generation, 3 * gibibyte);

        service
            .make_room_for_generation(second_active.repository, 3 * gibibyte)
            .expect("global budget evicts inactive history");

        assert!(!service.receipts.contains_key(&inactive.generation));
        assert!(service.receipts.contains_key(&first_active.generation));
        assert!(service.receipts.contains_key(&second_active.generation));
    }

    #[test]
    fn generation_memory_budget_never_evicts_active_repositories() {
        let first_repository = TempDir::new().expect("first repository exists");
        let second_repository = TempDir::new().expect("second repository exists");
        fs::create_dir(first_repository.path().join("src")).expect("first source directory exists");
        fs::create_dir(second_repository.path().join("src"))
            .expect("second source directory exists");
        fs::write(
            first_repository.path().join("src/lib.rs"),
            "pub fn first() -> u32 { 1 }\n",
        )
        .expect("first source writes");
        fs::write(
            second_repository.path().join("src/lib.rs"),
            "pub fn second() -> u32 { 2 }\n",
        )
        .expect("second source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let first = service
            .index_rust_fixture(first_repository.path(), &cancellation)
            .expect("first repository publishes");
        let second = service
            .index_rust_fixture(second_repository.path(), &cancellation)
            .expect("second repository publishes");
        let gibibyte = 1024_u64 * 1024 * 1024;
        service
            .generation_memory_bytes
            .insert(first.generation, 8 * gibibyte);
        service
            .generation_memory_bytes
            .insert(second.generation, 8 * gibibyte);

        assert!(matches!(
            service.make_room_for_generation(first.repository, 1),
            Err(FirstSliceError::GenerationMemoryLimit {
                breakdown: GenerationMemoryBreakdown {
                    retained_bytes,
                    reserved_bytes: 0,
                    owned_bytes,
                    referenced_bytes: 0,
                    mapped_bytes: 0,
                    staged_bytes: 1,
                    shared_bytes: 0,
                },
                observed,
                limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            }) if retained_bytes == 16 * gibibyte
                && owned_bytes == retained_bytes
                && observed == retained_bytes + 1
        ));
        assert!(service.receipts.contains_key(&first.generation));
        assert!(service.receipts.contains_key(&second.generation));
    }

    #[test]
    fn durable_preflight_reserves_the_large_repository_write_profile() {
        let source_bytes = 1_000;
        let fixed_bytes = DURABLE_STAGING_FIXED_OVERHEAD_BYTES
            .checked_add(DURABLE_DISK_SAFETY_MARGIN_BYTES)
            .expect("fixed reservation is representable");
        let initial = durable_initial_admission_reservation(source_bytes)
            .expect("initial reservation is representable");
        let preflight =
            durable_staging_reservation(source_bytes).expect("preflight is representable");

        assert_eq!(initial - fixed_bytes, 2_000);
        assert_eq!(
            preflight - fixed_bytes,
            source_bytes * DURABLE_SOURCE_WRITE_AMPLIFICATION_FACTOR
        );
        assert!(preflight > initial);
    }

    #[test]
    fn generation_memory_preflight_uses_the_measured_large_repository_ceiling() {
        let source_bytes = 62_980_516;
        let reservation =
            ensure_generation_memory_preflight(source_bytes).expect("profile is admitted");

        assert_eq!(
            reservation,
            source_bytes * GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR
                + GENERATION_MEMORY_FIXED_OVERHEAD_BYTES
        );
        assert_eq!(GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR, 48);
        assert!(reservation > 2_895_064_417);
        let over_limit_source = MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
            .checked_sub(GENERATION_MEMORY_FIXED_OVERHEAD_BYTES)
            .and_then(|bytes| bytes.checked_div(GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR))
            .and_then(|bytes| bytes.checked_add(1))
            .expect("over-limit source size is representable");
        assert!(matches!(
            ensure_generation_memory_preflight(over_limit_source),
            Err(FirstSliceError::GenerationMemoryLimit {
                breakdown: GenerationMemoryBreakdown {
                    retained_bytes: 0,
                    reserved_bytes,
                    owned_bytes: 0,
                    referenced_bytes: 0,
                    mapped_bytes: 0,
                    staged_bytes: 0,
                    shared_bytes: 0,
                },
                observed,
                limit: MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
            }) if observed > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
                && reserved_bytes == observed
        ));
        assert_eq!(
            ensure_generation_memory_preflight(u64::MAX),
            Err(FirstSliceError::Limits)
        );
    }

    #[test]
    fn publication_reports_the_early_memory_reservation_separately() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = "pub fn reservation_probe() -> u32 { 42 }\n";
        fs::write(fixture.path().join("src/lib.rs"), source).expect("fixture source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("service initializes");

        let prepared = service
            .prepare_rust_fixture(fixture.path(), &cancellation)
            .expect("generation prepares");
        let commit = service
            .publish_prepared_with_metrics(prepared, &cancellation)
            .expect("generation publishes");

        assert_eq!(
            commit.evidence().reserved_memory_bytes,
            ensure_generation_memory_preflight(
                u64::try_from(source.len()).expect("source length fits u64")
            )
            .expect("source is admitted")
        );
        assert!(
            commit.evidence().reserved_memory_bytes >= commit.evidence().owned_memory_bytes,
            "the pre-parse reservation must cover the retained generation charge"
        );
    }

    #[test]
    fn repository_analysis_partitions_structural_facts_across_count_and_size() {
        let limits = analysis_limits(16 * 1024 * 1024).expect("analysis limits are valid");
        let one_file_weight = analysis_partition_weight(1024).expect("weight is valid");
        let unpartitioned =
            partitioned_analysis_limits(&limits, 1, one_file_weight, one_file_weight)
                .expect("one source retains its full budget");
        assert_eq!(
            unpartitioned.syntax_stream().max_records(),
            limits.syntax_stream().max_records()
        );

        let small_weight = analysis_partition_weight(1).expect("small weight is valid");
        let large_weight = analysis_partition_weight(9).expect("large weight is valid");
        let total_weight = small_weight
            .checked_add(large_weight)
            .expect("combined weight is valid");
        let small = partitioned_analysis_limits(&limits, 2, total_weight, small_weight)
            .expect("small sources retain an even base partition");
        let large = partitioned_analysis_limits(&limits, 2, total_weight, large_weight)
            .expect("large sources receive a proportional partition");
        assert!(large.syntax_stream().max_records() > small.syntax_stream().max_records());
        assert!(
            large
                .syntax_stream()
                .max_records()
                .checked_add(small.syntax_stream().max_records())
                .is_some_and(|total| total <= MAX_FIRST_SLICE_STRUCTURAL_FACTS)
        );
        assert_eq!(
            small.syntax_stream().batch().max_records(),
            limits.syntax_stream().batch().max_records()
        );

        let source_files = MAX_FIRST_SLICE_STRUCTURAL_FACTS
            .checked_add(1)
            .expect("test source count is representable");
        let minimum = partitioned_analysis_limits(&limits, source_files, 0, 0)
            .expect("extreme source counts retain a nonzero bounded partition");
        assert_eq!(minimum.syntax_stream().max_records(), 1);
        assert_eq!(minimum.syntax_stream().batch().max_records(), 1);
        assert!(matches!(
            partitioned_analysis_limits(&limits, 2, 1, 2),
            Err(FirstSliceError::Limits)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_service_removes_unactivated_crash_artifacts_before_restore() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn retained_answer() -> u32 {\n    42\n}\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );

        let receipt = {
            let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes")
        };
        let repository_directory = paths
            .state_dir()
            .join("first-slice")
            .join("repositories")
            .join(receipt.repository.to_string());
        let staging_directory = repository_directory.join("stage-crash-fixture");
        let repository_capability =
            Dir::open_ambient_dir(&repository_directory, ambient_authority())
                .expect("repository capability opens");
        PrivateDirectory::verify_parent(&repository_capability)
            .expect("repository capability remains private");
        drop(
            PrivateDirectory::create(&repository_capability, OsStr::new("stage-crash-fixture"))
                .expect("orphan staging directory creates"),
        );
        let orphan_generation = GenerationId::from_bytes([99; 20]);
        let orphan_directory = repository_directory.join(orphan_generation.to_string());
        drop(
            PrivateDirectory::create(
                &repository_capability,
                OsStr::new(&orphan_generation.to_string()),
            )
            .expect("unactivated generation directory creates"),
        );

        let restored = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation)
            .expect("durable service restores");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        assert!(!staging_directory.exists());
        assert!(!orphan_directory.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_service_rejects_a_tampered_activation_manifest() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn tamper_target() -> u32 {\n    42\n}\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );

        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("durable generation publishes")
        };
        let repository_directory = paths
            .state_dir()
            .join("first-slice")
            .join("repositories")
            .join(receipt.repository.to_string());
        let activation_directory = fs::read_dir(&repository_directory)
            .expect("repository directory reads")
            .map(|entry| entry.expect("repository entry reads"))
            .find(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("activation-"))
            })
            .expect("activation marker exists")
            .path();
        let activation_manifest = activation_directory.join("activation.json");
        let mut document: serde_json::Value = serde_json::from_slice(
            &fs::read(&activation_manifest).expect("activation manifest reads"),
        )
        .expect("activation manifest parses");
        document["version"] = json!(99);
        fs::write(
            &activation_manifest,
            serde_json::to_vec(&document).expect("tampered manifest serializes"),
        )
        .expect("activation manifest tampers");

        assert!(matches!(
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation),
            Err(FirstSliceError::CatalogCorrupt)
        ));
    }

    #[test]
    fn one_generation_retention_uses_the_staging_slot_for_successors() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, EQUIVALENCE_INITIAL).expect("initial source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(1).expect("first-slice service initializes");

        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");
        fs::write(&source, EQUIVALENCE_BODY_EDIT).expect("successor source writes");
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("successor uses the dedicated staging slot");

        assert_ne!(first.generation, second.generation);
        assert_eq!(
            service.active_generation_for(first.repository),
            Some(second.generation)
        );
        assert!(matches!(
            service.resolve_generation(first.repository, Some(first.generation)),
            Err(FirstSliceError::GenerationNotFound)
        ));
        assert!(
            !service
                .language_coverage_by_generation
                .contains_key(&first.generation)
        );
        assert!(
            service
                .language_coverage_by_generation
                .contains_key(&second.generation)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_retention_reclaims_old_state_and_preserves_publication_count() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, EQUIVALENCE_INITIAL).expect("initial source writes");
        let cancellation = deadline();

        let (first, second, third) = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let first = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("initial generation publishes");
            fs::write(&source, EQUIVALENCE_BODY_EDIT).expect("body edit writes");
            let second = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("second generation publishes");
            fs::write(&source, EQUIVALENCE_SURFACE_EDIT).expect("surface edit writes");
            let third = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("third generation publishes");
            assert!(second.logical_snapshot.is_some());
            assert!(third.logical_snapshot.is_some());
            assert!(matches!(
                service.resolve_generation(first.repository, Some(first.generation)),
                Err(FirstSliceError::GenerationNotFound)
            ));
            service
                .resolve_generation(second.repository, Some(second.generation))
                .expect("previous retained generation resolves");
            (first, second, third)
        };

        let repository_directory = paths
            .state_dir()
            .join("first-slice")
            .join("repositories")
            .join(first.repository.to_string());
        let names: Vec<_> = fs::read_dir(&repository_directory)
            .expect("repository directory reads")
            .map(|entry| entry.expect("repository entry reads").file_name())
            .collect();
        assert_eq!(
            names
                .iter()
                .filter(|name| {
                    name.to_str()
                        .is_some_and(|value| value.starts_with("gen1_"))
                })
                .count(),
            2
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| {
                    name.to_str()
                        .is_some_and(|value| value.starts_with("activation-"))
                })
                .count(),
            2
        );

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable state restores");
        assert_eq!(
            restored.active_generation_for(first.repository),
            Some(third.generation)
        );
        assert_eq!(restored.active_generation(), Some(third.generation));
        assert!(matches!(
            restored.resolve_generation(first.repository, Some(first.generation)),
            Err(FirstSliceError::GenerationNotFound)
        ));
        restored
            .resolve_generation(second.repository, Some(second.generation))
            .expect("previous generation survives restart");
        assert_eq!(
            restored
                .repository_status(second.repository, Some(second.generation))
                .expect("retained logical identity survives restart")
                .logical_snapshot,
            second.logical_snapshot
        );
        assert_eq!(
            restored
                .repository_status(third.repository, None)
                .expect("active logical identity survives restart")
                .logical_snapshot,
            third.logical_snapshot
        );
        let page = restored
            .repository_catalog_page(
                CatalogPageRequest::new(
                    None,
                    None,
                    catalog::CatalogListFilter::new(None, None, None).expect("filter is valid"),
                    catalog::CatalogPageSize::new(20).expect("page size is valid"),
                )
                .expect("request is valid"),
                CatalogInstant::from_millis(1_000),
            )
            .expect("catalog page succeeds");
        assert_eq!(page.items()[0].generation_count(), 3);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_repository_metadata_rename_and_delete_survive_restart() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn durable_metadata_probe() -> u32 { 42 }\n")
            .expect("fixture source writes");
        let cancellation = deadline();

        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let receipt = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("fixture publishes");
            service
                .rename_repository(receipt.repository, "Renamed project".to_owned())
                .expect("repository alias persists");
            receipt
        };

        let mut restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("renamed repository restores");
        let page = restored
            .repository_catalog_page(
                CatalogPageRequest::new(
                    None,
                    None,
                    catalog::CatalogListFilter::new(None, None, None).expect("filter is valid"),
                    catalog::CatalogPageSize::new(20).expect("page size is valid"),
                )
                .expect("request is valid"),
                CatalogInstant::from_millis(1_000),
            )
            .expect("catalog page succeeds");
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].alias(), Some("Renamed project"));
        assert!(page.items()[0].root_path().is_some_and(|root| {
            fixture
                .path()
                .file_name()
                .is_some_and(|name| root.ends_with(&name.to_string_lossy().to_string()))
        }));

        restored
            .delete_repository(receipt.repository)
            .expect("Rootlight-owned repository state deletes");
        assert!(source.exists(), "source repository must remain untouched");
        assert!(
            !paths
                .state_dir()
                .join("first-slice")
                .join("repositories")
                .join(receipt.repository.to_string())
                .exists()
        );
        drop(restored);

        let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("deleted repository state stays absent");
        let page = reopened
            .repository_catalog_page(
                CatalogPageRequest::new(
                    None,
                    None,
                    catalog::CatalogListFilter::new(None, None, None).expect("filter is valid"),
                    catalog::CatalogPageSize::new(20).expect("page size is valid"),
                )
                .expect("request is valid"),
                CatalogInstant::from_millis(1_000),
            )
            .expect("empty catalog page succeeds");
        assert!(page.items().is_empty());
        assert!(source.exists(), "source repository must survive restart");
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_reactivation_compacts_markers_and_restores_global_chronology() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let first_fixture = durable_test_tempdir();
        let second_fixture = durable_test_tempdir();
        for (fixture, function) in [
            (&first_fixture, "first_repository"),
            (&second_fixture, "second_repository"),
        ] {
            fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
            fs::write(
                fixture.path().join("src/lib.rs"),
                format!("pub fn {function}() -> u32 {{ 42 }}\n"),
            )
            .expect("fixture source writes");
        }
        let cancellation = deadline();

        let most_recent = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let first = service
                .index_rust_fixture(first_fixture.path(), &cancellation)
                .expect("first repository publishes");
            let second = service
                .index_rust_fixture(second_fixture.path(), &cancellation)
                .expect("second repository publishes");
            let (fixture, expected) = if first.repository < second.repository {
                (&first_fixture, first)
            } else {
                (&second_fixture, second)
            };
            for _ in 0..32 {
                let repeated = service
                    .index_rust_fixture(fixture.path(), &cancellation)
                    .expect("retained generation reactivates");
                assert_eq!(repeated, expected);
            }
            assert_eq!(service.active_generation(), Some(expected.generation));
            expected
        };

        let repository_directory = paths
            .state_dir()
            .join("first-slice")
            .join("repositories")
            .join(most_recent.repository.to_string());
        let activation_markers = fs::read_dir(repository_directory)
            .expect("repository directory reads")
            .filter_map(|entry| {
                entry
                    .ok()
                    .and_then(|entry| entry.file_name().into_string().ok())
            })
            .filter(|name| name.starts_with("activation-"))
            .count();
        assert_eq!(activation_markers, 1);

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable state restores");
        assert_eq!(restored.active_generation(), Some(most_recent.generation));
        assert_eq!(
            restored.active_generation_for(most_recent.repository),
            Some(most_recent.generation)
        );
    }

    const EQUIVALENCE_INITIAL: &str =
        "pub fn answer() -> u32 {\n    42\n}\n\npub fn helper() -> u32 {\n    7\n}\n";
    const EQUIVALENCE_BODY_EDIT: &str =
        "pub fn answer() -> u32 {\n    43\n}\n\npub fn helper() -> u32 {\n    7\n}\n";

    #[test]
    fn query_and_source_budget_failures_remain_distinct() {
        let cancellation = Cancellation::new();
        assert_eq!(
            map_discovery_error(
                DiscoveryError::RetainedSnapshotByteLimit {
                    observed: 513,
                    maximum: 512,
                },
                &cancellation,
            ),
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::SourceBytes,
                observed: 513,
                limit: 512,
            }
        );
        assert_eq!(
            map_discovery_error(DiscoveryError::EntryLimit { maximum: 90 }, &cancellation,),
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::DiscoveryEntries,
                observed: 91,
                limit: 90,
            }
        );
        assert_eq!(
            map_discovery_error(
                DiscoveryError::Vfs(VfsError::DirectoryEntryLimit { maximum: 90 }),
                &cancellation,
            ),
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::Files,
                observed: 91,
                limit: 90,
            }
        );
        assert_eq!(
            map_vfs_error(
                VfsError::FileTooLarge {
                    maximum: rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES,
                },
                &cancellation,
            ),
            FirstSliceError::EstimatedResourceLimit {
                resource: FirstSliceResource::SourceBytes,
                estimated: rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES.saturating_add(1),
                limit: rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES,
            }
        );
        assert_eq!(
            map_query_error(
                QueryError::PlanRejected {
                    resource: rootlight_query::QueryResource::Results,
                },
                &cancellation,
            ),
            FirstSliceError::BudgetExceeded
        );
        assert_eq!(
            map_query_error(
                QueryError::Source(SourceError::SourceBudgetExceeded),
                &cancellation,
            ),
            FirstSliceError::BudgetExceeded
        );
        assert_eq!(
            map_query_error(QueryError::SymbolNotFound, &cancellation),
            FirstSliceError::SymbolNotFound
        );
    }

    #[test]
    fn incremental_vfs_and_project_limits_keep_typed_admission_evidence() {
        let cancellation = Cancellation::new();
        assert_eq!(
            map_incremental_error(
                IncrementalError::ResourceLimit {
                    resource: rootlight_incremental::ResourceKind::DependencyEdges,
                    observed: 101,
                    limit: 100,
                },
                &cancellation,
            ),
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::IncrementalDependencyEdges,
                observed: 101,
                limit: 100,
            }
        );
        assert_eq!(
            map_incremental_error(
                IncrementalError::InvalidLimit {
                    resource: rootlight_incremental::ResourceKind::Files,
                    value: 0,
                    hard_maximum: 1_000,
                },
                &cancellation,
            ),
            FirstSliceError::Limits
        );
        assert_eq!(
            map_discovery_error(
                DiscoveryError::Vfs(VfsError::MemoryUnavailable),
                &cancellation,
            ),
            FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::MemoryBytes,
            }
        );
        assert_eq!(
            map_discovery_error(
                DiscoveryError::Vfs(VfsError::OpenFile {
                    source: std::io::Error::other("private path"),
                }),
                &cancellation,
            ),
            FirstSliceError::RepositoryIo
        );
        assert_eq!(
            map_adapter_error(
                AdapterError::RejectedRequest(RequestError::ProjectLimit {
                    resource: ResourceKind::ProjectFiles,
                    observed: 17,
                    limit: 16,
                }),
                &cancellation,
            ),
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::ProjectFiles,
                observed: 17,
                limit: 16,
            }
        );
    }
    const EQUIVALENCE_SURFACE_EDIT: &str =
        "pub fn answer() -> u32 {\n    43\n}\n\npub fn renamed() -> u32 {\n    7\n}\n";

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
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "syntax-error-recovery"
                && diagnostic.message == "parser reported syntax-error-recovery"
        }));
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
    fn configured_source_file_limit_is_shared_by_discovery_and_analysis() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let minimum_regression_bytes = 1024 * 1024 + 1;
        let mut source = vec![b' '; minimum_regression_bytes];
        source.extend_from_slice(b"\npub fn beyond_one_mebibyte() -> u32 { 42 }\n");
        fs::write(fixture.path().join("src/lib.rs"), source).expect("large source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        assert_eq!(service.config.version(), rootlight_config::CONFIG_VERSION);
        assert_eq!(
            service.analysis_limits.max_source_bytes(),
            usize::try_from(rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES)
                .expect("default source-file bound fits usize")
        );
        let mut progress = Vec::new();
        let preparation = service
            .prepare_repository_with_mode_and_progress(
                fixture.path(),
                FirstSliceIndexMode::Structural,
                &cancellation,
                |observed| progress.push(observed),
            )
            .expect("a source between one MiB and the configured bound prepares");
        assert_eq!(
            progress
                .first()
                .map(|observed| (observed.completed, observed.total)),
            Some((0, 6))
        );
        assert_eq!(
            progress
                .last()
                .map(|observed| (observed.completed, observed.total)),
            Some((6, 6))
        );
        assert!(
            progress
                .windows(2)
                .all(|pair| pair[0].completed <= pair[1].completed
                    && pair[0].files_examined <= pair[1].files_examined
                    && pair[0].bytes_examined <= pair[1].bytes_examined)
        );
        let analysis = progress
            .iter()
            .filter(|observed| observed.stage == FirstSliceIndexStage::Analysis)
            .collect::<Vec<_>>();
        assert!(analysis.len() >= 2);
        assert!(
            analysis
                .windows(2)
                .any(|pair| pair[0].files_examined < pair[1].files_examined
                    && pair[0].bytes_examined < pair[1].bytes_examined)
        );
        let receipt = service
            .publish_prepared(preparation, &cancellation)
            .expect("a source between one MiB and the configured bound publishes");
        assert_eq!(receipt.indexed_files, 1);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_preparation_reports_written_bytes_before_publication() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn progress_probe() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let mut progress = Vec::new();

        let preparation = service
            .prepare_repository_with_mode_and_progress(
                fixture.path(),
                FirstSliceIndexMode::Structural,
                &cancellation,
                |observed| progress.push(observed),
            )
            .expect("durable preparation succeeds");

        let written = progress
            .iter()
            .filter_map(|observed| (observed.written_bytes > 0).then_some(observed.written_bytes))
            .collect::<Vec<_>>();
        assert!(!written.is_empty());
        assert!(written.windows(2).all(|pair| pair[0] <= pair[1]));
        let terminal = progress.last().expect("terminal progress exists");
        assert_eq!((terminal.completed, terminal.total), (6, 6));
        assert!(terminal.written_bytes > 0);
        let receipt = service
            .publish_prepared(preparation, &cancellation)
            .expect("durable preparation publishes");
        assert_eq!(receipt.indexed_files, 1);
    }

    #[test]
    fn support_repository_capacity_tracks_active_pending_release_and_delete() {
        let fixture = TempDir::new().expect("fixture root exists");
        let active_root = fixture.path().join("active");
        let pending_root = fixture.path().join("pending");
        for (root, function) in [
            (&active_root, "active_capacity"),
            (&pending_root, "pending_capacity"),
        ] {
            fs::create_dir(root).expect("repository root exists");
            fs::write(
                root.join("lib.rs"),
                format!("pub fn {function}() -> bool {{ true }}\n"),
            )
            .expect("repository fixture writes");
        }
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
        let active = service
            .index_rust_fixture(&active_root, &cancellation)
            .expect("active repository publishes");
        let pending = service
            .admit_repository(&pending_root, &cancellation)
            .expect("pending repository is admitted");

        assert_eq!(
            service
                .support_inventory_snapshot_without_storage_scan()
                .expect("active and pending capacity builds")
                .repository_capacity,
            FirstSliceRepositoryCapacityInventory {
                registered_repository_count: 2,
                pending_repository_count: 1,
                active_repository_count: 1,
                failed_repository_count: 0,
                pinned_repository_count: 0,
                reclaimable_repository_count: 1,
                pinning_supported: false,
                configured_maximum_repositories: 4_096,
                effective_maximum_repositories: 4_096,
                repository_headroom: 4_094,
            }
        );

        service.release_index_admission(pending);
        assert_eq!(
            service
                .support_inventory_snapshot_without_storage_scan()
                .expect("released capacity builds")
                .repository_capacity
                .repository_headroom,
            4_095
        );
        service
            .delete_repository(active.repository)
            .expect("committed repository deletes");
        assert_eq!(
            service
                .support_inventory_snapshot_without_storage_scan()
                .expect("deleted capacity builds")
                .repository_capacity,
            FirstSliceRepositoryCapacityInventory {
                registered_repository_count: 0,
                pending_repository_count: 0,
                active_repository_count: 0,
                failed_repository_count: 0,
                pinned_repository_count: 0,
                reclaimable_repository_count: 0,
                pinning_supported: false,
                configured_maximum_repositories: 4_096,
                effective_maximum_repositories: 4_096,
                repository_headroom: 4_096,
            }
        );
    }

    #[test]
    fn support_repository_capacity_rejects_lifecycle_drift() {
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
        service.repository_display_names.insert(
            RepositoryId::from_bytes([91; 16]),
            "orphaned-registration".to_owned(),
        );

        assert!(matches!(
            service.support_inventory_snapshot_without_storage_scan(),
            Err(FirstSliceError::CatalogCorrupt)
        ));
    }

    #[test]
    fn support_inventory_is_source_free_and_tracks_active_generation() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn inventory_probe() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("fixture publishes");

        let inventory = service
            .support_inventory_snapshot()
            .expect("support inventory builds");
        assert!(inventory.languages.iter().any(|capability| {
            capability.language == "rust"
                && capability.suffixes.iter().any(|suffix| suffix == ".rs")
                && capability
                    .detectors
                    .iter()
                    .any(|detector| detector == "extension")
                && capability.maximum_tier == "tier_b"
                && capability
                    .analyzers
                    .iter()
                    .any(|analyzer| analyzer == "treesitter")
        }));
        assert_eq!(inventory.repositories.len(), 1);
        assert_eq!(inventory.generations.len(), 1);
        assert_eq!(inventory.generation_format, "1.2");
        assert_eq!(
            inventory.generation_disk_bytes,
            receipt.oracle_allocated_bytes
        );
        assert_eq!(inventory.total_storage_bytes, None);
        assert_eq!(
            inventory.storage_accounting_state,
            FirstSliceStorageAccountingState::Unavailable
        );
        assert!(!inventory.pinning_supported);
        assert_eq!(inventory.pinned_bytes, Some(0));
        assert_eq!(inventory.effective_retention_generations, 2);
        let repository = &inventory.repositories[0];
        assert_eq!(repository.repository, receipt.repository);
        assert_eq!(repository.languages, ["rust"]);
        assert_eq!(repository.tiers, ["tier_b"]);
        assert_eq!(repository.files, 1);
        assert!(repository.symbols > 0);
        assert_eq!(repository.generation_count, 1);
        let generation = &inventory.generations[0];
        assert_eq!(generation.repository, receipt.repository);
        assert_eq!(generation.generation, receipt.generation);
        assert_eq!(generation.disk_bytes, receipt.oracle_allocated_bytes);
        assert_eq!(generation.parent, None);
        assert!(generation.active);
        assert!(
            inventory
                .adapters
                .iter()
                .any(|adapter| adapter.name == "tree-sitter"
                    && adapter.languages.iter().any(|language| language == "rust")
                    && !adapter.isolated)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_support_inventory_tracks_and_reclaims_staging_bytes() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn staging_inventory_probe() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");

        let prepared = service
            .prepare_rust_fixture(fixture.path(), &cancellation)
            .expect("durable generation prepares");
        let active = service
            .support_inventory_snapshot()
            .expect("active staging inventory builds");
        assert!(active.unreclaimed_temporary_bytes > 0);
        assert!(active.disk_margin_bytes.is_some());

        drop(prepared);
        let reclaimed = service
            .support_inventory_snapshot()
            .expect("reclaimed staging inventory builds");
        assert_eq!(reclaimed.unreclaimed_temporary_bytes, 0);
        assert!(reclaimed.disk_margin_bytes.is_some());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_support_inventory_reports_physical_generation_bytes_and_parent() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn first() -> u32 { 1 }\n").expect("first source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("first generation publishes");
        fs::write(&source, "pub fn second() -> u32 { 2 }\n").expect("second source writes");
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("second generation publishes");

        let inventory = service
            .support_inventory_snapshot()
            .expect("support inventory builds");
        assert_eq!(inventory.effective_retention_generations, 3);
        assert_eq!(
            inventory.generation_disk_bytes,
            first
                .retained_durable_bytes
                .checked_add(second.retained_durable_bytes)
                .expect("fixture retained-byte total fits")
        );
        assert!(
            inventory
                .total_storage_bytes
                .is_some_and(|bytes| bytes >= inventory.generation_disk_bytes)
        );
        assert!(inventory.shared_bytes.is_some());
        assert!(inventory.reclaimable_bytes.is_some());
        let active = inventory
            .generations
            .iter()
            .find(|generation| generation.active)
            .expect("active generation is reported");
        assert_eq!(active.generation, second.generation);
        assert_eq!(active.parent, Some(first.generation));
        assert_eq!(active.disk_bytes, second.retained_durable_bytes);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn accounted_support_inventory_matches_verified_after_publish_and_delete() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn accounted_inventory_probe() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");

        let accounted_after_publish = service
            .support_inventory_snapshot_from_accounting()
            .expect("accounted inventory builds after publication")
            .expect("published accounting is reconciled");
        let verified_after_publish = service
            .support_inventory_snapshot()
            .expect("verified inventory builds after publication");
        assert_support_storage_inventory_eq(&accounted_after_publish, &verified_after_publish);
        assert_eq!(accounted_after_publish.repositories.len(), 1);
        assert_eq!(accounted_after_publish.generations.len(), 1);
        assert_eq!(
            accounted_after_publish
                .repository_capacity
                .active_repository_count,
            1
        );
        assert_eq!(
            accounted_after_publish
                .repository_capacity
                .reclaimable_repository_count,
            1
        );

        service
            .delete_repository(receipt.repository)
            .expect("repository history deletes");
        let accounted_after_delete = service
            .support_inventory_snapshot_from_accounting()
            .expect("accounted inventory builds after deletion")
            .expect("deleted accounting remains reconciled");
        let verified_after_delete = service
            .support_inventory_snapshot()
            .expect("verified inventory builds after deletion");
        assert_support_storage_inventory_eq(&accounted_after_delete, &verified_after_delete);
        assert!(accounted_after_delete.repositories.is_empty());
        assert!(accounted_after_delete.generations.is_empty());
        assert_eq!(
            accounted_after_delete
                .repository_capacity
                .repository_headroom,
            accounted_after_delete
                .repository_capacity
                .effective_maximum_repositories
        );
    }

    fn assert_support_storage_inventory_eq(
        accounted: &FirstSliceSupportInventory,
        verified: &FirstSliceSupportInventory,
    ) {
        assert_eq!(
            accounted.storage_accounting_state,
            FirstSliceStorageAccountingState::Reconciled
        );
        assert_eq!(
            verified.storage_accounting_state,
            FirstSliceStorageAccountingState::VerifiedScan
        );
        assert_eq!(accounted.repositories.len(), verified.repositories.len());
        assert_eq!(accounted.generations.len(), verified.generations.len());
        assert_eq!(accounted.repository_capacity, verified.repository_capacity);
        assert_eq!(accounted.total_storage_bytes, verified.total_storage_bytes);
        assert_eq!(accounted.shared_bytes, verified.shared_bytes);
        assert_eq!(accounted.reclaimable_bytes, verified.reclaimable_bytes);
        assert_eq!(
            accounted.other_retained_generation_bytes,
            verified.other_retained_generation_bytes
        );
        assert_eq!(accounted.source_pool_bytes, verified.source_pool_bytes);
        assert_eq!(
            accounted.repository_overhead_bytes,
            verified.repository_overhead_bytes
        );
        assert_eq!(accounted.quarantine_bytes, verified.quarantine_bytes);
        assert_eq!(
            accounted.inflight_catalog_reservation_bytes,
            verified.inflight_catalog_reservation_bytes
        );
        assert_eq!(
            accounted.unreclaimed_temporary_bytes,
            verified.unreclaimed_temporary_bytes
        );
        let accounted_disk_margin = accounted
            .disk_margin_bytes
            .expect("accounted inventory reports the durable disk margin");
        let verified_disk_margin = verified
            .disk_margin_bytes
            .expect("verified inventory reports the durable disk margin");
        let accounted_admission_margin = accounted
            .admission_margin_bytes
            .expect("accounted inventory reports the admission margin");
        let verified_admission_margin = verified
            .admission_margin_bytes
            .expect("verified inventory reports the admission margin");
        assert!(accounted_admission_margin <= accounted_disk_margin);
        assert!(verified_admission_margin <= verified_disk_margin);
        for inventory in [accounted, verified] {
            let reconciled = inventory.active_generation_bytes.unwrap_or(0)
                + inventory.predecessor_generation_bytes.unwrap_or(0)
                + inventory.other_retained_generation_bytes.unwrap_or(0)
                + inventory.source_pool_bytes.unwrap_or(0)
                + inventory.unreclaimed_temporary_bytes
                + inventory.reclaimable_bytes.unwrap_or(0)
                + inventory.repository_overhead_bytes.unwrap_or(0)
                + inventory.quarantine_bytes.unwrap_or(0);
            assert_eq!(inventory.total_storage_bytes, Some(reconciled));
            assert_eq!(inventory.pinned_bytes, Some(0));
            assert!(!inventory.pinning_supported);
            assert_eq!(
                inventory.admission_margin_bytes,
                Some(
                    inventory
                        .repository_headroom_bytes
                        .expect("repository headroom is measured")
                        .min(
                            inventory
                                .catalog_headroom_bytes
                                .expect("catalog headroom is measured"),
                        )
                        .min(
                            inventory
                                .filesystem_headroom_bytes
                                .expect("filesystem headroom is measured"),
                        ),
                )
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn lightweight_support_inventory_skips_durable_storage_scan() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let (service, _) = FirstSliceService::open_durable_deferred(2, paths.state_dir())
            .expect("deferred durable service opens");
        fs::write(
            paths
                .state_dir()
                .join("first-slice")
                .join("repositories")
                .join("not-a-repository"),
            b"invalid inventory entry",
        )
        .expect("invalid durable entry writes");

        let lightweight = service
            .support_inventory_snapshot_without_storage_scan()
            .expect("lightweight support inventory avoids durable storage");
        assert!(!lightweight.adapters.is_empty());
        assert!(!lightweight.languages.is_empty());
        assert_eq!(lightweight.effective_retention_generations, 2);
        assert_eq!(lightweight.total_storage_bytes, None);
        assert_eq!(lightweight.disk_margin_bytes, None);
        assert_eq!(lightweight.admission_margin_bytes, None);
        assert!(matches!(
            service.support_inventory_snapshot(),
            Err(FirstSliceError::CatalogCorrupt)
        ));
    }

    #[test]
    fn project_support_inventory_excludes_unsupported_parser_grammars() {
        let analyzer = Arc::new(FailingProjectAnalyzer {
            identity: content_hash(b"support-project-adapter"),
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let service =
            FirstSliceService::new_with_storage(2, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("service initializes with a project adapter");

        let inventory = service
            .support_inventory_snapshot()
            .expect("support inventory builds");
        let project = inventory
            .adapters
            .iter()
            .find(|adapter| adapter.name == "project-adapter")
            .expect("project adapter is advertised");
        assert_eq!(
            project.languages,
            [
                "rust",
                "typescript",
                "javascript",
                "python",
                "go",
                "java",
                "cpp",
                "csharp",
                "php",
                "c",
            ]
        );
        assert!(
            !project
                .languages
                .iter()
                .any(|language| language == "kotlin")
        );
        assert!(
            inventory
                .adapters
                .iter()
                .find(|adapter| adapter.name == "tree-sitter")
                .is_some_and(|adapter| adapter.languages.iter().any(|language| language == "java"))
        );
    }

    #[test]
    fn oversized_source_is_reported_while_safe_inputs_publish() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn retained() -> u32 { 42 }\n",
        )
        .expect("safe source writes");
        let oversized = fs::File::create(fixture.path().join("src/oversized.rs"))
            .expect("oversized fixture creates");
        oversized
            .set_len(rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES + 1)
            .expect("oversized fixture length sets");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("safe source publishes despite an oversized peer");
        assert_eq!(receipt.indexed_files, 1);
        assert_eq!(receipt.oversized_inputs, 1);
        assert!(receipt.excluded_inputs >= receipt.oversized_inputs);
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "oversized-inputs-bounded"
                && diagnostic.message
                    == "oversized repository input count 1 omitted by configured source file limit"
        }));
    }

    #[test]
    fn invalid_utf8_source_publishes_bounded_file_coverage() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn retained() -> u32 { 42 }\n",
        )
        .expect("valid source writes");
        fs::write(
            fixture.path().join("src/invalid.rs"),
            b"pub fn unavailable() {}\n\xff",
        )
        .expect("invalid UTF-8 source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("invalid UTF-8 bounds one file without rejecting the repository");
        assert_eq!(receipt.indexed_files, 2);
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "invalid-utf8" && diagnostic.message == "source is not valid utf-8"
        }));

        let generation = service
            .generations
            .generation(receipt.generation)
            .expect("published generation is retained");
        let invalid_file = generation
            .document()
            .files
            .iter()
            .find(|file| file.path == "src/invalid.rs")
            .expect("invalid source retains its file identity");
        assert!(generation.document().skipped_regions.iter().any(|region| {
            region.source.span().file() == invalid_file.id
                && region.reason == rootlight_ir::SkippedRegionReason::UnsupportedEncoding
                && region.detail == "invalid-utf8"
        }));
        assert!(
            generation
                .document()
                .coverage_records
                .iter()
                .any(|coverage| {
                    coverage.scope == rootlight_ir::CoverageScope::File(invalid_file.id)
                        && coverage.domain == rootlight_ir::FactDomain::Entities
                        && coverage.status == CoverageStatus::Bounded
                        && coverage.skipped == 1
                })
        );
        service
            .code_locate(
                receipt.generation,
                "retained".to_owned(),
                LocateMode::Exact,
                8,
                0,
                &cancellation,
            )
            .expect("valid peer remains queryable");
    }

    #[test]
    fn aggregate_lexical_extension_limit_publishes_bounded_coverage() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        for file_index in 0..16 {
            let mut source = String::new();
            for comment_index in 0..800 {
                source.push_str(&format!(
                    "// lexical evidence {file_index:02} {comment_index:04}\n"
                ));
            }
            source.push_str(&format!(
                "pub fn indexed_{file_index:02}() -> usize {{ {file_index} }}\n"
            ));
            fs::write(
                fixture.path().join(format!("src/file_{file_index:02}.rs")),
                source,
            )
            .expect("extension-heavy source writes");
        }
        // macOS and Windows debug builds need extra headroom to materialize
        // and merge the full 10,000-extension boundary. This deadline guards
        // against hangs; the test does not define a performance SLO.
        let deadline_seconds = if cfg!(any(target_os = "macos", target_os = "windows")) {
            300
        } else {
            120
        };
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(deadline_seconds))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("optional extension overflow preserves a generation");
        let repeated = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("unchanged bounded generation remains reusable");
        assert_eq!(repeated.generation, first.generation);
        let document = service
            .generations
            .generation(first.generation)
            .expect("bounded generation remains retained")
            .document();
        assert_eq!(
            document
                .extensions
                .iter()
                .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
                .count(),
            MAX_RETAINED_OPTIONAL_EXTENSIONS
        );
        assert!(
            document.extensions.len() <= service.analysis_limits.ir().max_extensions,
            "required identity extensions remain inside the hard IR capacity"
        );
        assert!(document.coverage_records.iter().any(|coverage| {
            coverage.domain == rootlight_ir::FactDomain::Extensions
                && coverage.status == CoverageStatus::Bounded
                && coverage.skipped > 0
        }));
        assert!(first.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "extension-coverage-bounded"
                && diagnostic
                    .message
                    .ends_with("optional lexical extensions omitted by aggregate resource limit")
        }));
    }

    #[test]
    fn rejected_project_append_preserves_structural_target_and_state() {
        let repository = derive_repository(b"transactional-project-append").id();
        let generation = GenerationId::from_bytes([31; 20]);
        let provenance = FactId::from_bytes([32; 20]);
        let extension = |id: u8, namespace: &str| rootlight_ir::ExtensionEnvelope {
            id: FactId::from_bytes([id; 20]),
            repository,
            generation,
            namespace: namespace.to_owned(),
            version: "1.0".to_owned(),
            criticality: rootlight_ir::ExtensionCriticality::Noncritical,
            payload: "{}".to_owned(),
            provenance,
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        };
        let mut target = NormalizedIrDocument::empty(repository, generation);
        target
            .extensions
            .push(extension(33, LEXICAL_EXTENSION_NAMESPACE));
        let expected_target = target.clone();
        let mut state =
            DocumentAppendState::from_document(&target).expect("append state initializes");
        let expected_extension_bytes = state.extension_payload_bytes;
        let mut source = NormalizedIrDocument::empty(repository, generation);
        source.extensions.push(extension(34, "test.required"));
        let mut limits = IrLimits::default();
        limits.max_extensions = 1;

        assert!(matches!(
            append_normalized_document(&mut target, source, &limits, &mut state),
            Err(FirstSliceError::Identity)
        ));
        assert_eq!(target, expected_target);
        assert_eq!(state.extension_payload_bytes, expected_extension_bytes);
        assert_eq!(state.truncated_extensions, 0);
        assert_eq!(state.truncated_skipped_regions, 0);
    }

    #[test]
    fn aggregate_skipped_region_limit_retains_bounded_detail() {
        let repository = derive_repository(b"bounded-skipped-region-details").id();
        let generation = GenerationId::from_bytes([11; 20]);
        let file = FileId::from_bytes([12; 20]);
        let provenance = FactId::from_bytes([13; 20]);
        let source_hash = content_hash(b"abcd");
        let region = |start: u64| {
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, start, start + 1).expect("source span is valid"),
                source_hash,
                None,
            );
            let mut region = rootlight_ir::SkippedRegion {
                id: FactId::from_bytes([0; 20]),
                repository,
                generation,
                source: source.clone(),
                domain: rootlight_ir::FactDomain::Entities,
                reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
                detail: "unsupported test construct".to_owned(),
                provenance,
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            region.id =
                rootlight_ir::derive_skipped_region_id(&region).expect("region identity derives");
            region
        };
        let mut target = NormalizedIrDocument::empty(repository, generation);
        let mut limits = IrLimits::default();
        limits.max_skipped_regions = 2;
        let mut state =
            DocumentAppendState::from_document(&target).expect("append state initializes");

        let mut first = NormalizedIrDocument::empty(repository, generation);
        first.skipped_regions = vec![region(0), region(1), region(2)];
        let expected = first.skipped_regions[..2]
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        append_normalized_document(&mut target, first, &limits, &mut state)
            .expect("first document retains bounded detail");

        let mut second = NormalizedIrDocument::empty(repository, generation);
        second.skipped_regions = vec![region(1), region(2), region(3)];
        append_normalized_document(&mut target, second, &limits, &mut state)
            .expect("later detail can be omitted without failing the generation");

        assert_eq!(
            target
                .skipped_regions
                .iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(state.truncated_skipped_regions, 4);
    }

    #[test]
    fn published_generation_contains_resolver_owned_call_facts() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn target() -> u32 { 42 }\npub fn caller() -> u32 { target() }\n",
        )
        .expect("call fixture writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("call fixture publishes");
        let snapshot = service
            .generations
            .generation(receipt.generation)
            .expect("published generation remains retained");
        let document = snapshot.document();
        let call = document
            .occurrences
            .iter()
            .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .expect("adapter emits the explicit call occurrence");
        let target = match call.target {
            OccurrenceTarget::Resolved { symbol } => symbol,
            _ => panic!("the reviewed Rust Tier B profile resolves a unique local call"),
        };

        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target)
                && relation.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
        }));
        assert!(document.provenance.iter().any(|provenance| {
            provenance.id == call.provenance
                && provenance.producer.name() == rootlight_resolve::RESOLVER_PROVIDER_NAME
        }));
    }

    #[test]
    fn dependency_directed_successors_match_fresh_logical_rebuilds() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let primary = fixture.path().join("src/lib.rs");
        let added = fixture.path().join("src/added.rs");
        let moved = fixture.path().join("src/moved.rs");
        fs::write(&primary, EQUIVALENCE_INITIAL).expect("initial source writes");
        let cancellation = deadline();
        let mut incremental = FirstSliceService::new(8).expect("incremental service initializes");
        let initial = incremental
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");

        fs::write(&primary, EQUIVALENCE_BODY_EDIT).expect("body edit writes");
        let body = incremental
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("body successor publishes");
        assert_ne!(
            initial.logical_snapshot, body.logical_snapshot,
            "a source-body edit must change the public logical identity"
        );
        assert_fresh_equivalent(
            &incremental,
            fixture.path(),
            initial.generation,
            &body,
            &cancellation,
        );

        fs::write(&primary, EQUIVALENCE_SURFACE_EDIT).expect("surface edit writes");
        let surface = incremental
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("surface successor publishes");
        assert_fresh_equivalent(
            &incremental,
            fixture.path(),
            body.generation,
            &surface,
            &cancellation,
        );

        fs::write(&added, "pub fn added() -> u32 { 11 }\n").expect("added source writes");
        let addition = incremental
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("addition successor publishes");
        assert_fresh_equivalent(
            &incremental,
            fixture.path(),
            surface.generation,
            &addition,
            &cancellation,
        );

        fs::rename(&added, &moved).expect("source move writes");
        let movement = incremental
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("move successor publishes");
        assert_fresh_equivalent(
            &incremental,
            fixture.path(),
            addition.generation,
            &movement,
            &cancellation,
        );

        fs::remove_file(&moved).expect("moved source deletes");
        let deletion = incremental
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("deletion successor publishes");
        assert_fresh_equivalent(
            &incremental,
            fixture.path(),
            movement.generation,
            &deletion,
            &cancellation,
        );
    }

    #[test]
    fn clean_rebuild_preserves_logical_identity_and_rebuilds_every_fact() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, EQUIVALENCE_INITIAL).expect("initial source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(4).expect("service initializes");
        assert!(matches!(
            service.index_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &cancellation,
            ),
            Err(FirstSliceError::RepositoryNotFound)
        ));
        service
            .index_repository(fixture.path(), &cancellation)
            .expect("initial generation publishes");

        fs::write(&source, EQUIVALENCE_BODY_EDIT).expect("body edit writes");
        let incremental = service
            .index_repository(fixture.path(), &cancellation)
            .expect("incremental successor publishes");
        let prepared = service
            .prepare_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &cancellation,
            )
            .expect("clean rebuild prepares");
        let committed = service
            .publish_prepared_with_metrics(prepared, &cancellation)
            .expect("clean rebuild publishes");
        let clean = committed.receipt();
        let evidence = committed.evidence();

        assert_eq!(clean.repository, incremental.repository);
        assert_eq!(clean.parent, Some(incremental.generation));
        assert_ne!(clean.generation, incremental.generation);
        assert_eq!(clean.logical_snapshot, incremental.logical_snapshot);
        assert_eq!(
            evidence.strategy,
            FirstSliceIndexOperationStrategy::CleanRebuild
        );
        assert_eq!(evidence.fallback_reason, None);
        assert_eq!(evidence.reused_files, 0);
        assert_eq!(evidence.reused_facts, 0);
        assert_eq!(evidence.rebuilt_files, clean.indexed_files);
        assert!(evidence.rebuilt_facts > 0);
        assert!(!evidence.planned_fact_work.is_empty());
        assert!(evidence.planned_fact_work.iter().all(|work| {
            work.disposition() == FirstSliceFactWorkDisposition::Rebuild
                && work.cause() == FirstSliceFactWorkCause::UserRequestedCleanRebuild
        }));
        assert!(evidence.normalized_fact_work.iter().all(|work| {
            work.cause() != FirstSliceFactWorkCause::CompleteDependencyMatch
                && work.cause() != FirstSliceFactWorkCause::UserRequestedCleanRebuild
        }));
    }

    #[test]
    fn clean_rebuild_preserves_deep_analysis_strength() {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(
            fixture.path(),
            &[("src/value.py", "def python_value():\n    return 1\n")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer = Arc::new(SuccessfulProjectAnalyzer {
            identity: content_hash(b"clean-rebuild-project-adapter"),
            calls: Arc::clone(&calls),
            partitioned: false,
            syntax_facts_bounded: false,
        });
        let cancellation = deadline();
        let mut service =
            FirstSliceService::new_with_storage(3, MAX_RETAINED_SOURCE_BYTES, None, Some(analyzer))
                .expect("deep service initializes");
        let deep = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &cancellation)
            .expect("deep generation publishes");
        let clean = service
            .index_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Deep),
                &cancellation,
            )
            .expect("deep clean rebuild publishes");

        assert_eq!(clean.parent, Some(deep.generation));
        assert_eq!(clean.logical_snapshot, deep.logical_snapshot);
        assert!(
            service
                .active_generation_is_deep(clean.repository)
                .expect("active provider identity resolves")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_clean_rebuild_evidence_survives_restart() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn retained() -> u32 { 1 }\n",
        )
        .expect("fixture source writes");
        let cancellation = deadline();

        let clean = {
            let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_repository(fixture.path(), &cancellation)
                .expect("initial durable generation publishes");
            service
                .index_repository_with_options(
                    fixture.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &cancellation,
                )
                .expect("durable clean rebuild publishes")
        };

        let restored = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation)
            .expect("durable service restores");
        let evidence = restored
            .incremental_evidence(clean.generation)
            .expect("clean rebuild evidence restores");
        assert_eq!(evidence.strategy(), FirstSliceBuildStrategy::CleanRebuild);
        assert_eq!(evidence.reused_parser_artifacts(), 0);
        assert_eq!(evidence.reused_normalized_facts(), 0);
        assert!(evidence.planned_fact_work().iter().all(|work| {
            work.disposition() == FirstSliceFactWorkDisposition::Rebuild
                && work.cause() == FirstSliceFactWorkCause::UserRequestedCleanRebuild
        }));
    }

    #[test]
    fn semantic_fingerprints_distinguish_comment_body_and_surface_closures() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let changed = fixture.path().join("src/changed.rs");
        fs::write(
            &changed,
            "pub fn first() {}\npub fn second() {}\npub fn selected() {\n    first();\n}\n",
        )
        .expect("initial source writes");
        fs::write(
            fixture.path().join("src/stable.rs"),
            "pub fn stable() -> bool { true }\n",
        )
        .expect("stable source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(4).expect("service initializes");
        service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");

        fs::write(
            &changed,
            "pub fn first() {}\npub fn second() {}\npub fn selected() {\n    // keep the first target\n    first();\n}\n",
        )
        .expect("comment-only edit writes");
        let comment = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("comment-only successor publishes");
        let comment_evidence = service
            .incremental_evidence(comment.generation)
            .expect("comment-only evidence remains retained");
        assert_eq!(comment_evidence.parsed_files(), 1);
        assert_eq!(comment_evidence.reused_parser_artifacts(), 1);
        assert!(
            comment_evidence
                .input_changes()
                .iter()
                .any(|change| change.class == ChangeClass::BodyOnly)
        );
        assert!(
            !comment_evidence
                .input_changes()
                .iter()
                .any(|change| change.class == ChangeClass::Surface),
            "comment-only changes were classified as {:?}",
            comment_evidence.invalidation_trace()
        );
        assert!(
            !comment_evidence
                .invalidated_domains()
                .contains(&FactDomain::Resolution)
        );
        assert!(
            !comment_evidence
                .invalidated_domains()
                .contains(&FactDomain::Search)
        );
        assert_eq!(
            comment_evidence.trace_entries(),
            u64::try_from(comment_evidence.invalidation_trace().len())
                .expect("bounded trace length fits u64")
        );
        assert!(comment_evidence.invalidation_trace().iter().any(|entry| {
            entry.action() == rootlight_incremental::TraceAction::Changed
                && matches!(
                    entry.reason(),
                    rootlight_incremental::TraceReason::InputTransition(ChangeClass::BodyOnly)
                )
        }));
        let trace_view = service
            .incremental_trace_view(comment.generation, 1)
            .expect("response-bounded trace remains available");
        assert_eq!(trace_view.entries().len(), 1);
        assert_eq!(trace_view.total_entries(), comment_evidence.trace_entries());
        assert_eq!(
            trace_view.is_complete(),
            comment_evidence.trace_entries() == 1
        );
        let trace_json: serde_json::Value = serde_json::from_slice(
            &trace_view
                .canonical_json()
                .expect("response-bounded trace serializes"),
        )
        .expect("response-bounded trace is valid JSON");
        assert_eq!(trace_json["version"], INCREMENTAL_SCHEMA_VERSION);
        assert_eq!(trace_json["entries"].as_array().map(Vec::len), Some(1));

        fs::write(
            &changed,
            "pub fn first() {}\npub fn second() {}\npub fn selected() { second(); }\n",
        )
        .expect("body edit writes");
        let body = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("body successor publishes");
        let body_evidence = service
            .incremental_evidence(body.generation)
            .expect("body evidence remains retained");
        assert!(
            body_evidence
                .input_changes()
                .iter()
                .all(|change| change.class != ChangeClass::Surface)
        );
        assert!(
            body_evidence
                .invalidated_domains()
                .contains(&FactDomain::Resolution)
        );
        assert!(
            body_evidence
                .invalidated_domains()
                .contains(&FactDomain::Search)
        );

        fs::write(
            &changed,
            "pub fn first() {}\npub fn second() {}\npub fn renamed() { second(); }\n",
        )
        .expect("surface edit writes");
        let surface = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("surface successor publishes");
        let surface_evidence = service
            .incremental_evidence(surface.generation)
            .expect("surface evidence remains retained");
        assert!(
            surface_evidence
                .input_changes()
                .iter()
                .any(|change| change.class == ChangeClass::Surface)
        );
        assert!(
            surface_evidence
                .invalidated_domains()
                .contains(&FactDomain::Resolution)
        );
    }

    #[test]
    fn unchanged_file_reuses_parser_artifact_with_fresh_diagnostics() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let changed = fixture.path().join("src/changed.rs");
        fs::write(&changed, "pub fn changed() -> u32 { 1 }\n").expect("changed source writes");
        fs::write(
            fixture.path().join("src/malformed.rs"),
            "pub fn malformed( {\n",
        )
        .expect("malformed source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(3).expect("service initializes");
        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");
        let first_artifacts = service
            .structural_artifacts
            .generation(first.generation)
            .expect("initial structural cache remains retained");
        let unchanged_file = service
            .generations
            .generation(first.generation)
            .expect("initial generation remains retained")
            .document()
            .files
            .iter()
            .find(|file| file.path == "src/malformed.rs")
            .map(|file| file.id)
            .expect("unchanged malformed file is indexed");
        let cached = first_artifacts
            .get(unchanged_file)
            .expect("unchanged parser artifact remains retained");
        assert!(cached.normalized.is_some());
        assert!(
            cached
                .accounted_bytes()
                .expect("combined cache charge fits")
                > cached.artifact.accounted_bytes()
        );
        let unchanged_chunk = Arc::clone(
            cached
                .normalized
                .as_ref()
                .expect("initial normalized chunk remains retained"),
        );

        fs::write(&changed, "pub fn changed() -> u32 { 2 }\n").expect("body edit writes");
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("successor generation publishes");
        let evidence = service
            .incremental_evidence(second.generation)
            .expect("successor evidence remains retained");

        assert_eq!(
            evidence.strategy(),
            FirstSliceBuildStrategy::DependencyDirected
        );
        assert_eq!(evidence.fallback_reason(), None);
        assert_eq!(evidence.parsed_files(), 1);
        assert_eq!(evidence.reused_parser_artifacts(), 1);
        assert!(evidence.reused_parser_artifact_bytes() > 0);
        assert_eq!(evidence.lowered_files(), 1);
        assert!(evidence.reused_normalized_facts() > 0);
        assert!(evidence.rebuilt_normalized_facts() > 0);
        assert_eq!(
            evidence
                .normalized_fact_work()
                .iter()
                .map(FirstSliceNormalizedFactWork::facts)
                .sum::<u64>(),
            evidence
                .reused_normalized_facts()
                .checked_add(evidence.rebuilt_normalized_facts())
                .expect("normalized evidence total fits")
        );
        assert!(evidence.normalized_fact_work().iter().any(|work| {
            work.provider_pass() == LOWERING_PASS_ID
                && work.cause() == FirstSliceFactWorkCause::GenerationBoundLowering
                && work.files() > 0
                && work.facts() > 0
        }));
        assert!(evidence.normalized_fact_work().iter().any(|work| {
            work.provider_pass() == LOWERING_PASS_ID
                && work.cause() == FirstSliceFactWorkCause::CompleteDependencyMatch
                && work.files() > 0
                && work.facts() > 0
        }));
        assert_eq!(
            evidence
                .normalized_fact_work()
                .iter()
                .filter(|work| work.cause() == FirstSliceFactWorkCause::CompleteDependencyMatch)
                .map(FirstSliceNormalizedFactWork::facts)
                .sum::<u64>(),
            evidence.reused_normalized_facts()
        );
        assert_eq!(
            evidence
                .normalized_fact_work()
                .iter()
                .filter(|work| work.cause() != FirstSliceFactWorkCause::CompleteDependencyMatch)
                .map(FirstSliceNormalizedFactWork::facts)
                .sum::<u64>(),
            evidence.rebuilt_normalized_facts()
        );
        assert!(evidence.planned_fact_work().iter().any(|work| {
            work.disposition() == FirstSliceFactWorkDisposition::Rebuild
                && work.domain() == FactDomain::Body
                && work.cause() == FirstSliceFactWorkCause::DependencyClosure
                && work.files() == 1
                && work.analysis_units() == 1
        }));
        assert!(evidence.planned_fact_work().iter().any(|work| {
            work.disposition() == FirstSliceFactWorkDisposition::Reuse
                && work.domain() == FactDomain::Body
                && work.cause() == FirstSliceFactWorkCause::CompleteDependencyMatch
                && work.files() == 1
                && work.analysis_units() == 1
        }));
        assert!(evidence.structural_cache_retained());

        let mut legacy_evidence =
            serde_json::to_value(evidence).expect("incremental evidence serializes");
        let legacy_object = legacy_evidence
            .as_object_mut()
            .expect("incremental evidence serializes as an object");
        legacy_object.remove("planned_fact_work");
        legacy_object.remove("normalized_fact_work");
        let restored: FirstSliceIncrementalEvidence =
            serde_json::from_value(legacy_evidence).expect("legacy evidence remains readable");
        assert!(restored.planned_fact_work().is_empty());
        assert!(restored.normalized_fact_work().is_empty());

        let snapshot = service
            .generations
            .generation(second.generation)
            .expect("successor remains retained");
        let document = snapshot.document();
        let mut expected_reuse = ReusedNormalizedRecords::default();
        expected_reuse
            .insert_document(
                &unchanged_chunk
                    .rebind(
                        second.generation,
                        service.analysis_limits.ir(),
                        &service.extensions,
                    )
                    .expect("unchanged chunk rebinds for the successor"),
            )
            .expect("rebound chunk has unique normalized records");
        assert_eq!(
            usize::try_from(evidence.reused_normalized_facts())
                .expect("reuse evidence fits the platform"),
            expected_reuse
                .retained_count(document)
                .expect("final retained reuse count fits")
        );
        assert_eq!(
            normalized_record_count(document).expect("final normalized count fits"),
            usize::try_from(
                evidence
                    .reused_normalized_facts()
                    .checked_add(evidence.rebuilt_normalized_facts())
                    .expect("final evidence total fits")
            )
            .expect("final evidence total fits the platform")
        );
        let malformed = document
            .files
            .iter()
            .find(|file| file.path == "src/malformed.rs")
            .expect("malformed file remains represented")
            .id;
        assert!(document.diagnostics.iter().any(|diagnostic| {
            diagnostic.generation == second.generation
                && diagnostic.source.as_ref().is_some_and(|source| {
                    source.generation() == second.generation && source.span().file() == malformed
                })
        }));
        assert_ne!(first.generation, second.generation);
        assert_fresh_equivalent(
            &service,
            fixture.path(),
            first.generation,
            &second,
            &cancellation,
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_one_file_edit_reuses_content_addressed_source_artifacts() {
        fn source(prefix: &str, changed_value: u32) -> String {
            let mut source = String::new();
            for ordinal in 0..320 {
                let value = if ordinal == 0 { changed_value } else { ordinal };
                source.push_str(&format!(
                    "pub fn {prefix}_{ordinal:04}() -> u32 {{ {value} }}\n"
                ));
            }
            source
        }

        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let changed = fixture.path().join("src/changed.rs");
        fs::write(&changed, source("changed", 1)).expect("changed source writes");
        fs::write(
            fixture.path().join("src/unchanged.rs"),
            source("unchanged", 1),
        )
        .expect("unchanged source writes");
        let indexing_cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .expect("test deadline is representable"),
        );
        let (first, second, second_evidence) = {
            let mut service =
                FirstSliceService::new_durable(2, paths.state_dir(), &indexing_cancellation)
                    .expect("durable service initializes");
            let prepared = service
                .prepare_rust_fixture(fixture.path(), &indexing_cancellation)
                .expect("initial generation prepares");
            let staged = service
                .stage_prepared(prepared, &indexing_cancellation)
                .expect("initial generation stages");
            let first = service
                .commit_staged_with_operation(staged, None)
                .expect("initial generation commits");

            fs::write(&changed, source("changed", 2)).expect("one-file edit writes");
            let prepared = service
                .prepare_rust_fixture(fixture.path(), &indexing_cancellation)
                .expect("successor generation prepares");
            let staged = service
                .stage_prepared(prepared, &indexing_cancellation)
                .expect("successor generation stages");
            let second = service
                .commit_staged_with_operation(staged, None)
                .expect("successor generation commits");
            assert!(second.evidence().referenced_bytes > 0);
            let evidence = service
                .incremental_evidence(second.receipt().generation)
                .expect("successor evidence remains retained")
                .clone();
            assert!(evidence.reused_durable_artifact_bytes() > 0);
            assert!(
                second.evidence().referenced_bytes > evidence.reused_parser_artifact_bytes(),
                "operation evidence must include persisted source-artifact reuse"
            );
            (first, second, evidence)
        };

        // Restore is a separate bounded operation. A slow CI runner must not
        // inherit the nearly spent indexing deadline from the prior service.
        let restore_cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .expect("test deadline is representable"),
        );
        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &restore_cancellation)
            .expect("durable service restores");
        assert_eq!(
            restored.active_generation_for(second.receipt().repository),
            Some(second.receipt().generation)
        );
        let restored_evidence = restored
            .incremental_evidence(second.receipt().generation)
            .expect("reused artifact evidence survives restart");
        assert_eq!(
            restored_evidence.reused_durable_artifact_bytes(),
            second_evidence.reused_durable_artifact_bytes()
        );
        assert_eq!(
            restored_evidence.reused_parser_artifact_bytes(),
            second_evidence.reused_parser_artifact_bytes()
        );
        assert_eq!(
            restored_evidence.rebuilt_normalized_facts(),
            second_evidence.rebuilt_normalized_facts()
        );
        assert_eq!(
            restored_evidence.invalidation_trace(),
            second_evidence.invalidation_trace()
        );
        assert_eq!(
            restored_evidence.trace_entries(),
            u64::try_from(restored_evidence.invalidation_trace().len())
                .expect("bounded trace length fits u64")
        );
        assert_ne!(
            first.receipt().generation,
            second.receipt().generation,
            "the edit must publish a successor"
        );
    }

    #[test]
    fn unchanged_repository_reuses_the_active_generation() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn stable() -> u32 { 1 }\n",
        )
        .expect("Rust source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(3).expect("service initializes");
        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");

        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("unchanged generation is retained");

        assert_eq!(second, first);
        assert_eq!(service.receipts.len(), 1);
        assert_eq!(service.incremental_inputs.len(), 1);
        assert_eq!(service.published_generation_counts[&first.repository], 1);
    }

    #[test]
    fn unsupported_source_change_publishes_a_disposition_generation() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn stable() -> u32 { 1 }\n",
        )
        .expect("Rust source writes");
        let readme = fixture.path().join("README.md");
        fs::write(&readme, "first\n").expect("non-Rust input writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(3).expect("service initializes");
        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");

        fs::write(&readme, "second\n").expect("non-Rust input changes");
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("unsupported-source disposition successor publishes");
        let evidence = service
            .incremental_evidence(second.generation)
            .expect("successor evidence remains retained");

        assert_ne!(second.generation, first.generation);
        assert_eq!(second.parent, Some(first.generation));
        assert_eq!(
            evidence.strategy(),
            FirstSliceBuildStrategy::DependencyDirected
        );
        assert_eq!(evidence.parsed_files(), 0);
        assert_eq!(evidence.reused_parser_artifacts(), 1);
        assert_eq!(evidence.lowered_files(), 0);
        assert!(evidence.reused_normalized_facts() > 0);
        assert!(evidence.structural_cache_retained());
        assert_eq!(service.receipts.len(), 2);
        assert_eq!(service.incremental_inputs.len(), 2);
    }

    #[test]
    fn structural_cache_exhaustion_falls_back_to_fresh_parsing() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn value() -> u32 { 1 }\n").expect("Rust source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(3).expect("service initializes");
        service.structural_artifacts.maximum_bytes = 1;

        let prepared = service
            .prepare_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation prepares within the structural cap");
        let FirstSliceIndexPreparation::Pending(pending) = &prepared else {
            panic!("changed fixture requires a pending generation");
        };
        assert!(pending.structural_artifacts.by_file.is_empty());
        assert_eq!(pending.structural_artifacts.accounted_bytes, 0);
        assert!(!pending.structural_artifacts.retention_complete);
        let first = service
            .publish_prepared(prepared, &cancellation)
            .expect("initial generation publishes without structural retention");
        let initial = service
            .incremental_evidence(first.generation)
            .expect("initial evidence remains retained");
        assert!(!initial.structural_cache_retained());
        assert_eq!(service.structural_artifacts.retained_bytes, 0);

        fs::write(&source, "pub fn value() -> u32 { 2 }\n").expect("Rust source changes");
        let prepared = service
            .prepare_rust_fixture(fixture.path(), &cancellation)
            .expect("successor prepares with immediate artifact eviction");
        let FirstSliceIndexPreparation::Pending(pending) = &prepared else {
            panic!("changed fixture requires a pending generation");
        };
        assert!(pending.structural_artifacts.by_file.is_empty());
        assert_eq!(pending.structural_artifacts.accounted_bytes, 0);
        assert!(!pending.structural_artifacts.retention_complete);
        let second = service
            .publish_prepared(prepared, &cancellation)
            .expect("successor generation performs a fresh parse");
        let successor = service
            .incremental_evidence(second.generation)
            .expect("successor evidence remains retained");

        assert_eq!(successor.parsed_files(), 1);
        assert_eq!(successor.reused_parser_artifacts(), 0);
        assert_eq!(successor.lowered_files(), 1);
        assert!(!successor.structural_cache_retained());
        assert_eq!(service.structural_artifacts.retained_bytes, 0);
    }

    #[test]
    fn unavailable_normalized_chunk_falls_back_to_parser_artifact_lowering() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn stable() -> u32 { 1 }\n",
        )
        .expect("Rust source writes");
        let readme = fixture.path().join("README.md");
        fs::write(&readme, "first\n").expect("unsupported source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(3).expect("service initializes");
        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");

        let artifacts = service
            .structural_artifacts
            .committed
            .get_mut(&first.generation)
            .expect("initial structural cache remains retained");
        let entry = artifacts
            .by_file
            .values_mut()
            .next()
            .expect("initial parser artifact remains retained");
        let normalized_bytes = entry
            .normalized
            .take()
            .expect("initial normalized chunk remains retained")
            .encoded_bytes();
        artifacts.accounted_bytes = artifacts
            .accounted_bytes
            .checked_sub(normalized_bytes)
            .expect("generation cache charge includes the chunk");
        service.structural_artifacts.retained_bytes = service
            .structural_artifacts
            .retained_bytes
            .checked_sub(normalized_bytes)
            .expect("aggregate cache charge includes the chunk");

        fs::write(&readme, "second\n").expect("unsupported source changes");
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("successor publishes through the lowering fallback");
        let evidence = service
            .incremental_evidence(second.generation)
            .expect("successor evidence remains retained");

        assert_eq!(evidence.parsed_files(), 0);
        assert_eq!(evidence.reused_parser_artifacts(), 1);
        assert_eq!(evidence.lowered_files(), 1);
        assert_eq!(evidence.reused_normalized_facts(), 0);
        assert!(evidence.rebuilt_normalized_facts() > 0);
        assert_fresh_equivalent(
            &service,
            fixture.path(),
            first.generation,
            &second,
            &cancellation,
        );
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
        assert_eq!(
            checked_resource_length(10_000, 1, 10_000, FirstSliceResource::Extensions),
            Err(FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::Extensions,
                observed: 10_001,
                limit: 10_000,
            })
        );
    }

    #[test]
    fn first_slice_budget_reductions_are_monotonic_for_every_lower_layer() {
        let reduced = FirstSliceBudget::default()
            .reduce_max_rows(20_000)
            .reduce_max_edges(9_000)
            .reduce_max_results(40)
            .reduce_max_source_bytes(8_000)
            .reduce_max_json_bytes(32_000)
            .reduce_max_tokens(16_000)
            .reduce_max_memory_bytes(64_000)
            .reduce_max_duration(Duration::from_millis(500))
            .reduce_search_max_query_bytes(128)
            .reduce_search_max_candidates(2_000)
            .reduce_search_max_terms(8)
            .reduce_search_max_expanded_terms(256)
            .reduce_search_max_examined_terms(1_024)
            .reduce_search_max_postings(20_000)
            .reduce_search_max_returned_text_bytes(32_000)
            .reduce_source_max_selectors(8)
            .reduce_source_max_context_lines(4)
            .reduce_source_max_metadata_bytes(32_000)
            .reduce_source_max_snapshot_bytes(1_000_000);
        let query = reduced.query();
        let search = reduced.search();
        let source = reduced.source();

        assert_eq!(query.max_rows(), 20_000);
        assert_eq!(query.max_edges(), 9_000);
        assert_eq!(query.max_results(), 40);
        assert_eq!(query.max_source_bytes(), 8_000);
        assert_eq!(query.max_json_bytes(), 32_000);
        assert_eq!(query.max_tokens(), 16_000);
        assert_eq!(query.max_memory_bytes(), 64_000);
        assert_eq!(query.max_duration(), Duration::from_millis(500));
        assert_eq!(search.max_results, 40);
        assert_eq!(search.max_query_bytes, 128);
        assert_eq!(search.max_candidates, 2_000);
        assert_eq!(search.max_terms, 8);
        assert_eq!(search.max_expanded_terms, 256);
        assert_eq!(search.max_examined_terms, 1_024);
        assert_eq!(search.max_postings, 20_000);
        assert_eq!(search.max_returned_text_bytes, 32_000);
        assert_eq!(search.max_duration, Duration::from_millis(500));
        assert_eq!(source.max_selectors, 8);
        assert_eq!(source.max_context_lines, 4);
        assert_eq!(source.max_source_bytes, 8_000);
        assert_eq!(source.max_metadata_bytes, 32_000);
        assert_eq!(source.max_response_memory_bytes, 64_000);
        assert_eq!(source.max_snapshot_bytes, 1_000_000);
        assert_eq!(source.max_duration, Duration::from_millis(500));

        let attempted_raise = reduced
            .reduce_max_rows(u64::MAX)
            .reduce_max_edges(u64::MAX)
            .reduce_max_results(u64::MAX)
            .reduce_max_source_bytes(u64::MAX)
            .reduce_max_json_bytes(u64::MAX)
            .reduce_max_tokens(u64::MAX)
            .reduce_max_memory_bytes(u64::MAX)
            .reduce_max_duration(Duration::MAX)
            .reduce_search_max_query_bytes(usize::MAX)
            .reduce_search_max_candidates(usize::MAX)
            .reduce_search_max_terms(usize::MAX)
            .reduce_search_max_expanded_terms(usize::MAX)
            .reduce_search_max_examined_terms(usize::MAX)
            .reduce_search_max_postings(u64::MAX)
            .reduce_search_max_returned_text_bytes(usize::MAX)
            .reduce_source_max_selectors(usize::MAX)
            .reduce_source_max_context_lines(u16::MAX)
            .reduce_source_max_metadata_bytes(usize::MAX)
            .reduce_source_max_snapshot_bytes(u64::MAX);
        assert_eq!(attempted_raise, reduced);
    }

    #[test]
    fn reduced_policy_reaches_query_search_and_source_plans() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn item_one() -> u32 { 1 }\npub fn item_two() -> u32 { 2 }\n",
        )
        .expect("budget fixture writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("budget fixture indexes");

        let first = service
            .code_locate_with_budget(
                receipt.generation,
                "item".to_owned(),
                LocateMode::Prefix,
                10,
                0,
                FirstSliceBudget::default()
                    .reduce_max_rows(10_001)
                    .reduce_max_results(1),
                &cancellation,
            )
            .expect("reduced query policy admits its exact conservative plan");
        assert_eq!(first.plan.estimate.rows, 10_001);
        assert_eq!(first.plan.estimate.results, 1);
        assert_eq!(first.data.hits.len(), 1);
        assert!(first.data.execution.is_truncated());

        let tightly_bounded = service
            .code_locate_with_budget(
                receipt.generation,
                "item".to_owned(),
                LocateMode::Prefix,
                1,
                0,
                FirstSliceBudget::default()
                    .reduce_max_rows(224)
                    .reduce_max_results(4),
                &cancellation,
            )
            .expect("row reduction also narrows the search candidate plan");
        assert_eq!(tightly_bounded.plan.estimate.rows, 224);
        assert_eq!(tightly_bounded.data.hits.len(), 1);

        assert!(matches!(
            service.code_locate_with_budget(
                receipt.generation,
                "item_one".to_owned(),
                LocateMode::Exact,
                1,
                0,
                FirstSliceBudget::default().reduce_search_max_query_bytes(4),
                &cancellation,
            ),
            Err(FirstSliceError::Query)
        ));

        let second = service
            .code_locate(
                receipt.generation,
                "item_two".to_owned(),
                LocateMode::Exact,
                1,
                0,
                &cancellation,
            )
            .expect("second symbol locates");
        let references = vec![
            first.data.hits[0]
                .source
                .clone()
                .expect("first symbol has source evidence"),
            second.data.hits[0]
                .source
                .clone()
                .expect("second symbol has source evidence"),
        ];
        assert!(matches!(
            service.source_read_with_budget(
                receipt.generation,
                references,
                FirstSliceBudget::default().reduce_source_max_selectors(1),
                &cancellation,
            ),
            Err(FirstSliceError::BudgetExceeded)
        ));
    }

    #[test]
    fn default_wrapper_matches_explicit_default_budget() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )
        .expect("default-policy fixture writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("default-policy fixture indexes");

        let wrapped = service
            .code_locate(
                receipt.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                128,
                0,
                &cancellation,
            )
            .expect("compatibility wrapper succeeds");
        let explicit = service
            .code_locate_with_budget(
                receipt.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                128,
                0,
                FirstSliceBudget::default(),
                &cancellation,
            )
            .expect("explicit default policy succeeds");

        assert_eq!(wrapped.plan, explicit.plan);
        assert_eq!(wrapped.data, explicit.data);
        assert_eq!(wrapped.usage.rows, explicit.usage.rows);
        assert_eq!(wrapped.usage.edges, explicit.usage.edges);
        assert_eq!(wrapped.usage.results, explicit.usage.results);
        assert_eq!(wrapped.usage.source_bytes, explicit.usage.source_bytes);
        assert_eq!(wrapped.usage.memory_bytes, explicit.usage.memory_bytes);
    }

    #[test]
    fn preparation_rejects_aggregate_source_content_before_retaining_state() {
        const FIRST: &str = "pub fn first() {}\n";
        const SECOND: &str = "pub fn second() {}\n";

        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(fixture.path().join("src/first.rs"), FIRST).expect("first source writes");
        fs::write(fixture.path().join("src/second.rs"), SECOND).expect("second source writes");
        let per_file_limit = FIRST.len().max(SECOND.len());
        let service = FirstSliceService::new_with_source_limit(2, per_file_limit)
            .expect("bounded first-slice service initializes");

        assert!(matches!(
            service.prepare_rust_fixture(fixture.path(), &deadline()),
            Err(FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::SourceBytes,
                ..
            })
        ));
        assert!(service.generations.is_empty());
        assert!(service.repositories.is_empty());
        assert!(service.active_by_repository.is_empty());
        assert!(service.receipts.is_empty());
        assert!(service.source_snapshots.shared.is_empty());
        assert!(service.source_snapshots.committed.is_empty());
        assert_eq!(service.source_snapshots.retained_bytes(), 0);
        assert_eq!(service.source_snapshots.staged_generations(), 0);
        assert!(service.structural_artifacts.committed.is_empty());
        assert!(service.structural_artifacts.staged.is_empty());
        assert_eq!(service.structural_artifacts.retained_bytes, 0);
    }

    #[test]
    fn empty_snapshot_path_and_container_overhead_is_retention_bounded() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let file_name = format!("{}.rs", "x".repeat(200));
        fs::write(fixture.path().join("src").join(&file_name), [])
            .expect("empty long-path source writes");
        let root = RepositoryRoot::open(
            derive_repository(b"empty-source-retention").id(),
            fixture.path(),
        )
        .expect("retention fixture root opens");
        let relative = RelativePath::parse(Path::new(&format!("src/{file_name}")))
            .expect("long fixture path is valid");
        let snapshot = root
            .snapshot(&relative, 1)
            .expect("empty source snapshot captures");
        assert!(snapshot.content().is_empty());
        let exact_charge = unique_source_snapshot_bytes(&snapshot)
            .and_then(|bytes| {
                bytes
                    .checked_add(source_generation_reference_bytes(1)?)
                    .ok_or(FirstSliceError::Retention)
            })
            .expect("empty snapshot retained charge fits");
        assert!(exact_charge > relative.as_str().len());
        let generation = GenerationId::from_bytes([0x39; 20]);
        let source = || RustSourceInput {
            snapshot: snapshot.clone(),
            generated: false,
            origins: Vec::new(),
        };

        let below = SourceSnapshotRetention::new(1, exact_charge - 1)
            .expect("below-limit retention initializes");
        assert!(matches!(
            below.admit(generation, vec![source()], &Cancellation::new()),
            Err(FirstSliceError::Retention)
        ));

        let mut exact = SourceSnapshotRetention::new(1, exact_charge)
            .expect("exact-limit retention initializes");
        let admission = exact
            .admit(generation, vec![source()], &Cancellation::new())
            .expect("exact metadata-aware charge is admitted");
        exact.stage(admission).expect("exact admission stages");
        assert_eq!(exact.retained_bytes(), exact_charge);
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
        assert_eq!(FIRST.len(), SECOND.len());
        assert_eq!(SECOND.len(), THIRD.len());
        let root = RepositoryRoot::open(
            derive_repository(b"source-retention-accounting").id(),
            fixture.path(),
        )
        .expect("retention accounting root opens");
        let answer_snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("src/lib.rs"))
                    .expect("answer relative path is valid"),
                u64::try_from(FIRST.len()).expect("answer length is representable"),
            )
            .expect("answer snapshot captures");
        let stable_snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("src/stable.rs"))
                    .expect("stable relative path is valid"),
                u64::try_from(STABLE.len()).expect("stable length is representable"),
            )
            .expect("stable snapshot captures");
        let generation_reference_bytes =
            source_generation_reference_bytes(2).expect("generation reference charge fits");
        let answer_snapshot_bytes =
            unique_source_snapshot_bytes(&answer_snapshot).expect("answer snapshot charge fits");
        let stable_snapshot_bytes =
            unique_source_snapshot_bytes(&stable_snapshot).expect("stable snapshot charge fits");
        let first_generation_bytes =
            generation_reference_bytes + answer_snapshot_bytes + stable_snapshot_bytes;
        let exact_retention_bytes =
            first_generation_bytes + generation_reference_bytes + answer_snapshot_bytes;
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
        assert_eq!(service.structural_artifacts.staged.len(), 1);
        assert!(service.structural_artifacts.retained_bytes > 0);
        assert!(service.structural_artifacts.committed.is_empty());
        assert!(cancelled.cancel(CancellationReason::ClientRequest));
        service
            .discard_staged(staged)
            .expect("cancelled staging releases source retention");
        assert_eq!(service.source_snapshots.retained_bytes(), 0);
        assert_eq!(service.source_snapshots.staged_generations(), 0);
        assert!(service.structural_artifacts.staged.is_empty());
        assert_eq!(service.structural_artifacts.retained_bytes, 0);

        let first = service
            .index_rust_fixture(fixture.path(), &deadline())
            .expect("first generation publishes after cleanup");
        assert!(
            service
                .structural_artifacts
                .committed
                .contains_key(&first.generation)
        );
        assert!(service.structural_artifacts.staged.is_empty());
        let first_locate = service
            .code_locate(
                first.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                1,
                0,
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
        assert!(
            service
                .structural_artifacts
                .committed
                .contains_key(&second.generation)
        );
        let second_locate = service
            .code_locate(
                second.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                1,
                0,
                &deadline(),
            )
            .expect("second answer locates");
        let second_reference = second_locate.data.hits[0]
            .source
            .clone()
            .expect("second answer has exact source evidence");

        fs::write(&answer_path, THIRD).expect("third source writes");
        let third = service
            .index_rust_fixture(fixture.path(), &deadline())
            .expect("successor reclaims an inactive source snapshot before admission");
        assert_eq!(
            service.source_snapshots.retained_bytes(),
            exact_retention_bytes
        );
        assert_eq!(service.source_snapshots.staged_generations(), 0);
        assert!(service.structural_artifacts.staged.is_empty());
        assert_eq!(
            service.active_generation_for(first.repository),
            Some(third.generation)
        );
        assert_eq!(service.receipts.len(), 2);

        assert!(matches!(
            service.source_read(first.generation, vec![first_reference], &deadline()),
            Err(FirstSliceError::Query)
        ));
        let second_source = service
            .source_read(
                second.generation,
                vec![second_reference.clone()],
                &deadline(),
            )
            .expect("published second snapshot remains readable");
        assert_eq!(second_source.data.chunks[0].bytes, SECOND.as_bytes());
        assert_eq!(
            second_source.data.chunks[0].content_hash,
            second_reference.content_hash()
        );
    }

    #[test]
    fn vertical_slice_fixture_preserves_nested_policy_recovery_and_generation_lineage() {
        let fixture = materialize_vertical_slice_fixture();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("first-slice service initializes");

        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("Vertical slice v1 indexes");
        assert_eq!(first.discovered_inputs, 5);
        assert_eq!(first.indexed_files, 5);
        assert_indexed_gate_paths(&service, first.generation);
        assert_malformed_recovery(&service, first.generation);

        let answer = service
            .code_locate(
                first.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                8,
                0,
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
        let cached_v1_text = String::from_utf8_lossy(&cached_v1_source.data.chunks[0].bytes);
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
                0,
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
            String::from_utf8_lossy(&kept_source.data.chunks[0].bytes)
                .contains("kept_after_negation")
        );
        assert!(
            !String::from_utf8_lossy(&kept_source.data.chunks[0].bytes).contains(IGNORED_SENTINEL)
        );

        assert_no_exact_hits(
            &service,
            first.generation,
            &["ignored_by_nested_rule", IGNORED_SENTINEL, "broken"],
            &cancellation,
        );
        let repeated = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("unchanged Vertical slice v1 is idempotent");
        assert_eq!(repeated, first);

        apply_vertical_slice_v2_patch(fixture.path());
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("Vertical slice v2 indexes");
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
                0,
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
        let active_text = String::from_utf8_lossy(&active_source.data.chunks[0].bytes);
        assert!(active_text.contains("43"));
        assert!(!active_text.contains("42"));
        assert!(!active_text.contains(IGNORED_SENTINEL));

        let prior_answer = service
            .code_locate(
                first.generation,
                "answer".to_owned(),
                LocateMode::Exact,
                8,
                0,
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
        assert_eq!(prior_source.data.chunks[0].bytes, cached_v1_text.as_bytes());
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

    fn materialize_vertical_slice_fixture() -> TempDir {
        let fixture = TempDir::new().expect("materialized fixture root exists");
        copy_fixture_tree(Path::new(VERTICAL_SLICE_FIXTURE_ROOT), fixture.path());
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

    fn apply_vertical_slice_v2_patch(root: &Path) {
        let target = VERTICAL_SLICE_V2_PATCH
            .lines()
            .find_map(|line| line.strip_prefix("+++ b/"))
            .expect("Vertical slice patch names a target");
        let removed = VERTICAL_SLICE_V2_PATCH
            .lines()
            .find(|line| line.starts_with('-') && !line.starts_with("---"))
            .and_then(|line| line.strip_prefix('-'))
            .expect("Vertical slice patch removes one line");
        let added = VERTICAL_SLICE_V2_PATCH
            .lines()
            .find(|line| line.starts_with('+') && !line.starts_with("+++"))
            .and_then(|line| line.strip_prefix('+'))
            .expect("Vertical slice patch adds one line");
        let path = root.join(target);
        let source = fs::read_to_string(&path).expect("materialized v1 source reads");
        let removed = format!("{removed}\n");
        let added = format!("{added}\n");
        assert_eq!(
            source.matches(&removed).count(),
            1,
            "Vertical slice patch context must match exactly once"
        );
        fs::write(path, source.replacen(&removed, &added, 1))
            .expect("Vertical slice v2 source materializes");
    }

    fn assert_public_language_repository(
        files: &[(&str, &str)],
        expected_symbols: &[(&str, &str)],
    ) {
        let fixture = TempDir::new().expect("fixture root exists");
        write_language_fixture(fixture.path(), files);
        let cancellation = deadline();
        let mut service = FirstSliceService::new(2).expect("service initializes");

        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .expect("supported language repository publishes");
        let repeated = service
            .index_repository(fixture.path(), &cancellation)
            .expect("unchanged supported language repository is idempotent");
        let status = service
            .repository_status(receipt.repository, None)
            .expect("repository status resolves");

        assert_eq!(repeated, receipt);
        assert_eq!(
            receipt.indexed_files,
            u64::try_from(files.len()).expect("test file count is bounded")
        );
        for (language, symbol) in expected_symbols {
            let coverage = status
                .coverage
                .iter()
                .find(|coverage| coverage.language == *language)
                .expect("expected language coverage is reported");
            assert_eq!(coverage.tier, "tier_d");
            assert_eq!(coverage.status, "complete");
            assert_eq!(coverage.discovered_files, 1);
            assert_eq!(coverage.indexed_files, 1);

            let located = service
                .code_locate(
                    receipt.generation,
                    (*symbol).to_owned(),
                    LocateMode::Exact,
                    10,
                    0,
                    &cancellation,
                )
                .expect("indexed language symbol is queryable");
            assert!(located.data.hits.iter().any(|hit| {
                hit.language == *language && hit.identifier.eq_ignore_ascii_case(symbol)
            }));
        }
    }

    fn write_language_fixture(root: &Path, files: &[(&str, &str)]) {
        for (relative, source) in files {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixture source directory exists");
            }
            fs::write(path, source).expect("fixture source writes");
        }
    }

    fn limit_service_diagnostics(service: &mut FirstSliceService, max_diagnostics: usize) {
        let current = &service.analysis_limits;
        let mut ir = current.ir().clone();
        ir.max_diagnostics = max_diagnostics;
        replace_service_ir_limits(service, ir);
    }

    fn limit_service_ir_entities(service: &mut FirstSliceService, max_entities: usize) {
        let mut ir = service.analysis_limits.ir().clone();
        ir.max_entities = max_entities;
        replace_service_ir_limits(service, ir);
    }

    fn replace_service_ir_limits(service: &mut FirstSliceService, ir: IrLimits) {
        let current = &service.analysis_limits;
        let mut limited = AnalysisLimits::new(
            current.max_source_bytes(),
            current.max_syntax_nodes(),
            current.max_syntax_depth(),
            current.max_embedded_ranges(),
            current.max_reported_memory_bytes(),
            current.syntax_stream().clone(),
            current.ir_stream().clone(),
            ir,
        )
        .expect("test analysis limits are valid");
        if let Some(project) = current.project() {
            limited = limited.with_project_limits(project);
        }
        service.analysis_limits = limited;
    }

    fn assert_fresh_equivalent(
        incremental: &FirstSliceService,
        root: &Path,
        parent: GenerationId,
        successor: &FirstSliceIndexReceipt,
        cancellation: &Cancellation,
    ) {
        let evidence = incremental
            .incremental_evidence(successor.generation)
            .expect("successor evidence remains retained");
        assert_eq!(
            evidence.strategy(),
            FirstSliceBuildStrategy::DependencyDirected
        );
        assert_eq!(evidence.fallback_reason(), None);
        let structural_inputs = evidence
            .parsed_files()
            .checked_add(evidence.reused_parser_artifacts())
            .expect("structural input count fits");
        assert!(evidence.lowered_files() <= structural_inputs);
        assert!(evidence.structural_cache_retained());

        let mut fresh = FirstSliceService::new(2).expect("fresh comparison service initializes");
        fresh.repositories = incremental.repositories.clone();
        fresh
            .active_by_repository
            .insert(successor.repository, parent);
        fresh.incremental_baselines.insert(
            parent,
            incremental
                .incremental_baselines
                .get(&parent)
                .expect("parent baseline remains retained")
                .clone(),
        );
        let rebuilt = fresh
            .index_rust_fixture(root, cancellation)
            .expect("fresh logical rebuild publishes");
        assert_eq!(rebuilt.repository, successor.repository);
        assert_eq!(rebuilt.parent, successor.parent);
        assert_eq!(rebuilt.generation, successor.generation);
        assert_eq!(
            rebuilt.logical_snapshot, successor.logical_snapshot,
            "generation-neutral public identity must match the fresh logical projection"
        );

        let incremental_snapshot =
            equivalence_snapshot(incremental, successor.generation, cancellation);
        let clean_snapshot = equivalence_snapshot(&fresh, rebuilt.generation, cancellation);
        incremental_snapshot
            .compare_clean(&clean_snapshot, cancellation)
            .expect("equivalence comparison completes")
            .require_equivalent()
            .expect("incremental successor equals the fresh logical rebuild");
    }

    fn equivalence_snapshot(
        service: &FirstSliceService,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> EquivalenceSnapshot {
        let snapshot = service
            .generations
            .generation(generation)
            .expect("generation remains retained");
        let document = snapshot.document();
        let discovery_inputs = service
            .incremental_baselines
            .get(&generation)
            .expect("generation baseline remains retained")
            .inputs()
            .iter()
            .collect::<Vec<_>>();
        let mut query_names = document
            .entities
            .iter()
            .map(|entity| entity.canonical_name.clone())
            .collect::<BTreeSet<_>>();
        query_names.remove("");
        let query_outputs = query_names
            .into_iter()
            .map(|query| {
                let response = service
                    .code_locate(
                        generation,
                        query.clone(),
                        LocateMode::Exact,
                        64,
                        0,
                        cancellation,
                    )
                    .expect("equivalence locate query succeeds");
                json!({"query": query, "response": response.data})
            })
            .collect::<Vec<_>>();
        let coverage = json!({
            "coverage": document.coverage_records,
            "skipped_regions": document.skipped_regions,
            "diagnostics": document.diagnostics,
        });
        let stable_ids = json!({
            "files": document.files.iter().map(|record| record.id).collect::<Vec<_>>(),
            "entities": document.entities.iter().map(|record| record.id).collect::<Vec<_>>(),
            "occurrences": document.occurrences.iter().map(|record| record.id).collect::<Vec<_>>(),
            "relations": document.relations.iter().map(|record| record.id).collect::<Vec<_>>(),
            "provenance": document.provenance.iter().map(|record| record.id).collect::<Vec<_>>(),
            "source_mappings": document.source_mappings.iter().map(|record| record.id).collect::<Vec<_>>(),
            "coverage": document.coverage_records.iter().map(|record| record.id).collect::<Vec<_>>(),
            "skipped_regions": document.skipped_regions.iter().map(|record| record.id).collect::<Vec<_>>(),
            "diagnostics": document.diagnostics.iter().map(|record| record.id).collect::<Vec<_>>(),
            "extensions": document.extensions.iter().map(|record| record.id).collect::<Vec<_>>(),
        });
        let normalized_records =
            u64::try_from(normalized_record_count(document).expect("record count is bounded"))
                .expect("record count fits u64");
        let coverage_records = document
            .coverage_records
            .len()
            .checked_add(document.skipped_regions.len())
            .and_then(|count| count.checked_add(document.diagnostics.len()))
            .and_then(|count| u64::try_from(count).ok())
            .expect("coverage record count is bounded");
        let stable_records = document
            .files
            .len()
            .checked_add(document.entities.len())
            .and_then(|count| count.checked_add(document.occurrences.len()))
            .and_then(|count| count.checked_add(document.relations.len()))
            .and_then(|count| count.checked_add(document.provenance.len()))
            .and_then(|count| count.checked_add(document.source_mappings.len()))
            .and_then(|count| count.checked_add(document.coverage_records.len()))
            .and_then(|count| count.checked_add(document.skipped_regions.len()))
            .and_then(|count| count.checked_add(document.diagnostics.len()))
            .and_then(|count| count.checked_add(document.extensions.len()))
            .and_then(|count| u64::try_from(count).ok())
            .expect("stable identity count is bounded");
        let components = [
            logical_component(
                LogicalDomain::Discovery,
                &discovery_inputs,
                u64::try_from(discovery_inputs.len()).expect("input count fits u64"),
                cancellation,
            ),
            logical_component(
                LogicalDomain::NormalizedIr,
                document,
                normalized_records,
                cancellation,
            ),
            logical_component(
                LogicalDomain::LogicalStore,
                document,
                normalized_records,
                cancellation,
            ),
            logical_component(
                LogicalDomain::QueryOutputs,
                &query_outputs,
                u64::try_from(query_outputs.len()).expect("query count fits u64"),
                cancellation,
            ),
            logical_component(
                LogicalDomain::Coverage,
                &coverage,
                coverage_records,
                cancellation,
            ),
            logical_component(
                LogicalDomain::Provenance,
                &document.provenance,
                u64::try_from(document.provenance.len()).expect("provenance count fits u64"),
                cancellation,
            ),
            logical_component(
                LogicalDomain::StableIds,
                &stable_ids,
                stable_records,
                cancellation,
            ),
        ];
        EquivalenceSnapshot::new(components, cancellation)
            .expect("complete equivalence snapshot builds")
    }

    #[test]
    fn catalog_history_and_support_retention_counts_diverge_after_reclamation() {
        let fixture = TempDir::new().expect("fixture root exists");
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, EQUIVALENCE_INITIAL).expect("initial source writes");
        let cancellation = deadline();
        let mut service = FirstSliceService::new(1).expect("first-slice service initializes");

        let first = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("initial generation publishes");
        let repeated = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("unchanged publication is idempotent");
        assert_eq!(repeated, first);

        fs::write(&source, EQUIVALENCE_BODY_EDIT).expect("successor source writes");
        let second = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("successor generation publishes");
        assert_ne!(second.generation, first.generation);

        assert!(matches!(
            service.resolve_generation(first.repository, Some(first.generation)),
            Err(FirstSliceError::GenerationNotFound)
        ));
        let request = CatalogPageRequest::new(
            None,
            None,
            catalog::CatalogListFilter::new(None, None, None).expect("filter is valid"),
            catalog::CatalogPageSize::new(20).expect("page size is valid"),
        )
        .expect("request is valid");
        let page = service
            .repository_catalog_page(request, CatalogInstant::from_millis(1_000))
            .expect("catalog page succeeds after receipt reclamation");
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].active_generation(), Some(second.generation));
        assert_eq!(page.items()[0].generation_count(), 2);

        let inventory = service
            .support_inventory_snapshot()
            .expect("support inventory builds after reclamation");
        assert_eq!(inventory.repositories.len(), 1);
        assert_eq!(inventory.repositories[0].generation_count, 1);
        assert_eq!(inventory.generations.len(), 1);
        assert_eq!(inventory.generations[0].generation, second.generation);
    }

    fn logical_component(
        domain: LogicalDomain,
        value: &impl Serialize,
        records: u64,
        cancellation: &Cancellation,
    ) -> LogicalComponent {
        let bytes = serde_json::to_vec(value).expect("logical projection encodes");
        LogicalComponent::from_canonical_bytes(
            domain,
            &bytes,
            records,
            EQUIVALENCE_COMPONENT_BYTES,
            cancellation,
        )
        .expect("bounded logical component hashes")
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
            .expect("Vertical slice generation remains retained");
        let mut paths = snapshot
            .document()
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        assert_eq!(
            paths,
            [
                "Cargo.toml",
                "nested/.gitignore",
                "nested/ignored/kept.rs",
                "src/lib.rs",
                "src/malformed.rs",
            ]
        );
    }

    fn assert_malformed_recovery(service: &FirstSliceService, generation: GenerationId) {
        let snapshot = service
            .generations
            .generation(generation)
            .expect("Vertical slice generation remains retained");
        let document = snapshot.document();
        let malformed = document
            .files
            .iter()
            .find(|file| file.path == "src/malformed.rs")
            .expect("malformed Vertical slice file remains represented")
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
                    0,
                    cancellation,
                )
                .expect("excluded Vertical slice query succeeds");
            assert!(located.data.hits.is_empty(), "{query} must not be exposed");
        }
    }
}
