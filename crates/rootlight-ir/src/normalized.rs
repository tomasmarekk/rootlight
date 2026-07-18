//! Version 1.1 of Rootlight's common normalized fact document.
//!
//! This module owns the language-neutral wire model and strict version dispatch.
//! Cross-record validation and deterministic canonicalization live separately.

use std::io::{self, Cursor, Read};

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};
use serde::{Deserialize, Serialize, de, de::DeserializeOwned};

use crate::validation::{
    StandaloneExtensionValidationError, validate_standalone_extension_envelope,
};
use crate::{
    AnalysisTier, BuildContextIdentity, Confidence, CoverageStatus, EvidenceKind, ExtensionSupport,
    IR_VERSION, IrDocumentSchema, IrLimits, IrVersion, LineRange, ProducerIdentity, SourceRef,
    SourceSpan, canonicalize_ir_document,
};

/// The exact normalized fact-document version.
pub const NORMALIZED_IR_VERSION: IrVersion = IrVersion::new(1, 1);

/// The singleton wire marker for normalized fact-document version 1.1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedIrVersion;

impl NormalizedIrVersion {
    /// Creates the version 1.1 wire marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Returns the numeric normalized IR version.
    #[must_use]
    pub const fn value(self) -> IrVersion {
        NORMALIZED_IR_VERSION
    }
}

impl Serialize for NormalizedIrVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        NORMALIZED_IR_VERSION.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NormalizedIrVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = IrVersion::deserialize(deserializer)?;
        if version == NORMALIZED_IR_VERSION {
            Ok(Self)
        } else {
            Err(de::Error::custom(format_args!(
                "expected normalized IR version 1.1, got {}.{}",
                version.major(),
                version.minor()
            )))
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for NormalizedIrVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NormalizedIrVersion".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "major": { "type": "integer", "const": 1 },
                "minor": { "type": "integer", "const": 1 }
            },
            "required": ["major", "minor"],
            "additionalProperties": false
        })
    }
}

/// A closed common entity kind understood by the Rootlight core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    /// A repository root.
    Repository,
    /// A checked-out worktree.
    Worktree,
    /// A package or distribution unit.
    Package,
    /// A configuration-specific build target.
    BuildTarget,
    /// A physical directory.
    Directory,
    /// An immutable generation file.
    File,
    /// A language module.
    Module,
    /// A language namespace.
    Namespace,
    /// A class-like type.
    Class,
    /// A struct-like type.
    Struct,
    /// An enumeration.
    Enum,
    /// A union type.
    Union,
    /// A type alias.
    TypeAlias,
    /// A trait.
    Trait,
    /// An interface.
    Interface,
    /// A protocol.
    Protocol,
    /// A free function.
    Function,
    /// A method.
    Method,
    /// A constructor.
    Constructor,
    /// A closure.
    Closure,
    /// A field.
    Field,
    /// A property.
    Property,
    /// A constant.
    Constant,
    /// A variable.
    Variable,
    /// A callable parameter.
    Parameter,
    /// A type parameter.
    TypeParameter,
    /// An import clause.
    Import,
    /// An export clause.
    Export,
    /// A service route or endpoint.
    Route,
    /// A service boundary.
    Service,
    /// A message topic or queue.
    MessageTopic,
    /// A database object.
    DatabaseObject,
    /// A test declaration.
    Test,
    /// A configuration key.
    ConfigurationKey,
    /// A source-control commit.
    Commit,
    /// A source-control change.
    Change,
    /// A derived architecture community.
    CommunityView,
    /// A symbol outside the indexed repository.
    ExternalSymbol,
}

/// A closed common predicate for normalized relations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RelationPredicate {
    /// Structural containment.
    Contains,
    /// A container declares a symbol.
    Declares,
    /// A symbol has a definition occurrence.
    DefinesAt,
    /// An occurrence or symbol refers to a target.
    RefersTo,
    /// A site or callable invokes a callable.
    Calls,
    /// A site may dispatch to a runtime target.
    DispatchCandidate,
    /// A file or module imports another target.
    Imports,
    /// A module or package exports a symbol.
    Exports,
    /// A fact uses a type.
    UsesType,
    /// A callable returns a type.
    ReturnsType,
    /// A parameter has a type.
    ParameterType,
    /// A type extends another type.
    Extends,
    /// A type implements another type.
    Implements,
    /// A type satisfies a protocol or constraint.
    Satisfies,
    /// A type embeds another type.
    Embeds,
    /// A type mixes in another type.
    MixesIn,
    /// A method overrides another declaration.
    Overrides,
    /// A callable or occurrence reads a value.
    Reads,
    /// A callable or occurrence writes a value.
    Writes,
    /// A callable throws an error type.
    Throws,
    /// A callable handles an error type.
    HandlesError,
    /// A test exercises a target.
    Tests,
    /// A target depends on another target.
    DependsOn,
    /// A client calls a route.
    CallsRoute,
    /// A handler serves a route.
    ServesRoute,
    /// A callable or service publishes to a topic.
    Publishes,
    /// A callable or service consumes from a topic.
    Consumes,
    /// A callable or service reads a database object.
    ReadsTable,
    /// A callable or service writes a database object.
    WritesTable,
    /// A fact binds to a foreign declaration.
    BindsTo,
    /// A callable invokes a foreign declaration.
    CallsForeign,
    /// Generated content originates from another fact.
    GeneratedFrom,
    /// A fact changed in a source-control change.
    ChangedIn,
    /// A fact was renamed from another fact.
    LineageRenamedFrom,
    /// A fact was moved from another fact.
    LineageMovedFrom,
    /// A fact was split from another fact.
    LineageSplitFrom,
    /// A fact was merged from another fact.
    LineageMergedFrom,
    /// Two facts co-changed within a bounded history window.
    CoChangedWith,
    /// A fact has ownership evidence.
    OwnedBy,
    /// A fact belongs to a derived architecture view.
    MemberOfView,
}

/// A closed role for a source occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceRole {
    /// A defining occurrence.
    Definition,
    /// A non-defining declaration.
    Declaration,
    /// A general reference.
    Reference,
    /// A call site.
    CallSite,
    /// A type use.
    TypeUse,
    /// An import use.
    ImportUse,
    /// A write.
    Write,
    /// A read.
    Read,
    /// An inheritance use.
    InheritanceUse,
    /// An implementation use.
    ImplementationUse,
    /// A decorator or annotation use.
    DecoratorUse,
    /// A macro use.
    MacroUse,
    /// A route use.
    RouteUse,
    /// A test use.
    TestUse,
    /// Documentation evidence.
    Documentation,
    /// String-literal evidence.
    StringEvidence,
}

/// A producer class retained in deduplicated provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProducerKind {
    /// A resilient parser.
    Parser,
    /// A compiler or semantic frontend.
    Compiler,
    /// A SCIP producer.
    Scip,
    /// A build manifest.
    BuildManifest,
    /// Source-control evidence.
    Git,
    /// A runtime trace.
    RuntimeTrace,
    /// A deterministic rule.
    Rule,
    /// A bounded heuristic.
    Heuristic,
    /// User configuration.
    UserConfiguration,
    /// A derivation from base facts.
    Derivation,
}

/// A normalized fact domain used for coverage reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FactDomain {
    /// File facts.
    Files,
    /// Entity facts.
    Entities,
    /// Occurrence facts.
    Occurrences,
    /// Relation facts.
    Relations,
    /// Provenance facts.
    Provenance,
    /// Source-mapping facts.
    SourceMappings,
    /// Diagnostics and skipped regions.
    Diagnostics,
    /// Extension facts.
    Extensions,
}

/// A common entity visibility projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntityVisibility {
    /// Visible outside its owning package or module.
    Public,
    /// Visible to a restricted set of consumers.
    Restricted,
    /// Visible only within its owning scope.
    Private,
    /// Visibility is not expressible in the common projection.
    Unknown,
}

/// A common boolean entity characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntityFlag {
    /// Generated from another source.
    Generated,
    /// External to the indexed repository.
    External,
    /// Test-only or test-related.
    Test,
    /// Exported from its module or package.
    Exported,
    /// Deprecated by its producer.
    Deprecated,
    /// Synthesized without a direct source declaration.
    Synthetic,
}

/// A typed semantic container reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ContainerRef {
    /// The owning repository.
    Repository(RepositoryId),
    /// A containing file.
    File(FileId),
    /// A containing semantic entity.
    Entity(SymbolId),
}

/// A typed endpoint for a normalized relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum RelationEndpoint {
    /// A repository endpoint.
    Repository(RepositoryId),
    /// A file endpoint.
    File(FileId),
    /// A semantic entity endpoint.
    Entity(SymbolId),
    /// An occurrence endpoint.
    Occurrence(FactId),
}

/// A typed reference to a base fact used by a derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum FactRef {
    /// A file fact.
    File(FileId),
    /// A semantic entity fact.
    Entity(SymbolId),
    /// A non-entity fact identified by [`FactId`].
    Fact(FactId),
}

/// Source or derivation evidence required for a normalized fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FactEvidence {
    /// Direct immutable source evidence, when present.
    pub source: Option<SourceRef>,
    /// Base facts supporting a derived fact.
    pub derivation: Vec<FactRef>,
}

/// One immutable file owned by the document repository and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    /// Stable repository-scoped file identity.
    pub id: FileId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Canonical repository-relative path.
    pub path: String,
    /// Immutable content hash used by every source reference.
    pub content_hash: ContentHash,
    /// Authoritative file length in bytes.
    pub byte_length: u64,
    /// Normalized language or data-format identity.
    pub language: String,
    /// Declared source encoding.
    pub encoding: String,
    /// Whether the file is generated.
    pub generated: bool,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the file fact.
    pub evidence: FactEvidence,
}

/// One common semantic entity owned by the document repository and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EntityRecord {
    /// Stable semantic entity identity.
    pub id: SymbolId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Closed common entity kind.
    pub kind: EntityKind,
    /// Normalized language or data-format identity.
    pub language: String,
    /// Analysis tier supporting this entity.
    pub tier: AnalysisTier,
    /// Canonical name used by language identity rules.
    pub canonical_name: String,
    /// Original source spelling for presentation.
    pub display_name: String,
    /// Display-oriented qualified name.
    pub qualified_name: String,
    /// Optional semantic container.
    pub container: Option<ContainerRef>,
    /// Common visibility projection.
    pub visibility: EntityVisibility,
    /// Common entity characteristics.
    pub flags: Vec<EntityFlag>,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the entity fact.
    pub evidence: FactEvidence,
}

/// A resolved, bounded-candidate, or unresolved occurrence target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OccurrenceTarget {
    /// One resolved semantic target.
    Resolved {
        /// Resolved symbol.
        symbol: SymbolId,
    },
    /// A bounded set of possible semantic targets.
    Candidates {
        /// Materialized candidate symbols.
        symbols: Vec<SymbolId>,
        /// Candidate count before any declared truncation.
        total_count: u64,
        /// Completeness of the materialized set.
        completeness: CoverageStatus,
    },
    /// An unresolved site retained without inventing a target.
    Unresolved {
        /// Hash of the source spelling or other untrusted target text.
        text_hash: ContentHash,
    },
}

/// One source occurrence owned by the document repository and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OccurrenceRecord {
    /// Stable occurrence fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// File containing the occurrence.
    pub file: FileId,
    /// Authoritative occurrence source span.
    pub source: SourceRef,
    /// Closed common occurrence role.
    pub role: OccurrenceRole,
    /// Optional enclosing semantic entity.
    pub enclosing: Option<SymbolId>,
    /// Resolved, ambiguous, or unresolved target.
    pub target: OccurrenceTarget,
    /// Hash of the occurrence spelling.
    pub syntactic_text_hash: ContentHash,
    /// Producer-defined syntax kind label.
    pub syntax_kind: String,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Calibrated fixed-point confidence.
    pub confidence: Confidence,
    /// Source or derivation evidence for the occurrence fact.
    pub evidence: FactEvidence,
}

/// One typed relation owned by the document repository and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RelationRecord {
    /// Stable relation fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Typed relation subject.
    pub subject: RelationEndpoint,
    /// Closed common predicate.
    pub predicate: RelationPredicate,
    /// Typed relation object.
    pub object: RelationEndpoint,
    /// Calibrated fixed-point confidence.
    pub confidence: Confidence,
    /// Evidence class supporting the relation.
    pub evidence_kind: EvidenceKind,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the relation fact.
    pub evidence: FactEvidence,
}

/// One deduplicated producer and derivation provenance record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProvenanceRecord {
    /// Stable provenance fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Producer class.
    pub producer_kind: ProducerKind,
    /// Stable producer identity and configuration.
    pub producer: ProducerIdentity,
    /// Digest of the producing binary or immutable package.
    pub binary_digest: ContentHash,
    /// Grammar, compiler, or frontend version label.
    pub frontend_version: Option<String>,
    /// Normalized language or data-format identity.
    pub language: String,
    /// Analysis tier supplied by the producer.
    pub tier: AnalysisTier,
    /// Build-context interpretation used by the producer.
    pub build_context: BuildContextIdentity,
    /// Source inputs consumed by the producer.
    pub input_sources: Vec<SourceRef>,
    /// Source evidence directly supporting produced facts.
    pub evidence_sources: Vec<SourceRef>,
    /// Base facts or provenance records used by a derivation.
    pub derivation_parents: Vec<FactRef>,
    /// Optional deterministic rule or resolver identifier.
    pub rule: Option<String>,
}

/// The semantic direction of a generated or expanded source mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceMappingKind {
    /// Generated source maps to its handwritten origin.
    GeneratedToOrigin,
    /// Expanded source maps to its invocation.
    ExpansionToInvocation,
    /// An invocation maps to its original declaration or schema.
    InvocationToOrigin,
}

/// One source-to-source origin mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceMappingRecord {
    /// Stable source-mapping fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Generated or expanded source reference.
    pub from: SourceRef,
    /// Invocation or handwritten origin reference.
    pub to: SourceRef,
    /// Mapping direction.
    pub kind: SourceMappingKind,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the mapping fact.
    pub evidence: FactEvidence,
}

/// A typed scope for one coverage record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum CoverageScope {
    /// Repository-wide coverage.
    Repository(RepositoryId),
    /// File-specific coverage.
    File(FileId),
    /// Entity-specific coverage.
    Entity(SymbolId),
}

/// One bounded coverage result for a fact domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CoverageRecord {
    /// Stable coverage fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Covered repository, file, or entity.
    pub scope: CoverageScope,
    /// Covered fact domain.
    pub domain: FactDomain,
    /// Analysis tier supporting the coverage result.
    pub tier: AnalysisTier,
    /// Declared completeness.
    pub status: CoverageStatus,
    /// Discovered units in the scope and domain.
    pub discovered: u64,
    /// Successfully indexed units.
    pub indexed: u64,
    /// Explicitly skipped or failed units.
    pub skipped: u64,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the coverage fact.
    pub evidence: FactEvidence,
}

/// A closed reason for a skipped source region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SkippedRegionReason {
    /// A configured byte or node bound was reached.
    ResourceLimit,
    /// Source could not be parsed completely.
    ParseError,
    /// Required build context was unavailable.
    MissingBuildContext,
    /// The adapter failed without invalidating other facts.
    AdapterFailure,
    /// The source encoding was unsupported.
    UnsupportedEncoding,
    /// The source construct was unsupported.
    UnsupportedConstruct,
}

/// One explicitly skipped source region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SkippedRegion {
    /// Stable skipped-region fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Skipped source region.
    pub source: SourceRef,
    /// Affected fact domain.
    pub domain: FactDomain,
    /// Closed skip reason.
    pub reason: SkippedRegionReason,
    /// Bounded source-free detail.
    pub detail: String,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the skipped-region fact.
    pub evidence: FactEvidence,
}

/// A bounded diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Informational diagnostic.
    Info,
    /// Recoverable warning.
    Warning,
    /// Error that caused partial analysis.
    Error,
}

/// One bounded analysis diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRecord {
    /// Stable diagnostic fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Stable producer-defined diagnostic code.
    pub code: String,
    /// Bounded source-free diagnostic message.
    pub message: String,
    /// Diagnostic severity.
    pub severity: DiagnosticSeverity,
    /// Optional source region affected by the diagnostic.
    pub source: Option<SourceRef>,
    /// Coverage status after applying the diagnostic.
    pub coverage_effect: CoverageStatus,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the diagnostic fact.
    pub evidence: FactEvidence,
}

/// Extension criticality understood by compatibility policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCriticality {
    /// A decoder must understand the extension before accepting the document.
    Critical,
    /// A decoder may preserve or skip the extension without changing common facts.
    Noncritical,
}

/// One namespaced, length-bounded extension envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExtensionEnvelope {
    /// Stable extension fact identity.
    pub id: FactId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Owning immutable generation.
    pub generation: GenerationId,
    /// Reverse-DNS or similarly collision-resistant namespace.
    pub namespace: String,
    /// Namespace-specific payload version.
    pub version: String,
    /// Compatibility behavior for unknown payloads.
    pub criticality: ExtensionCriticality,
    /// Opaque canonical UTF-8 payload owned by the extension namespace.
    pub payload: String,
    /// Deduplicated provenance record.
    pub provenance: FactId,
    /// Source or derivation evidence for the extension fact.
    pub evidence: FactEvidence,
}

/// A strict version 1.1 normalized fact document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NormalizedIrDocument {
    /// Exact version 1.1 wire marker.
    pub version: NormalizedIrVersion,
    /// Single repository owning every record and reference.
    pub repository: RepositoryId,
    /// Single immutable generation owning every record and reference.
    pub generation: GenerationId,
    /// Owned immutable files.
    pub files: Vec<FileRecord>,
    /// Owned common semantic entities.
    pub entities: Vec<EntityRecord>,
    /// Owned source occurrences.
    pub occurrences: Vec<OccurrenceRecord>,
    /// Owned typed relations.
    pub relations: Vec<RelationRecord>,
    /// Deduplicated producer and derivation provenance.
    pub provenance: Vec<ProvenanceRecord>,
    /// Generated and expanded source mappings.
    pub source_mappings: Vec<SourceMappingRecord>,
    /// Coverage records by scope and fact domain.
    pub coverage_records: Vec<CoverageRecord>,
    /// Explicitly skipped source regions.
    pub skipped_regions: Vec<SkippedRegion>,
    /// Bounded diagnostics.
    pub diagnostics: Vec<DiagnosticRecord>,
    /// Namespaced critical and noncritical extension envelopes.
    pub extensions: Vec<ExtensionEnvelope>,
}

impl NormalizedIrDocument {
    /// Creates an empty version 1.1 document for one repository generation.
    #[must_use]
    pub const fn empty(repository: RepositoryId, generation: GenerationId) -> Self {
        Self {
            version: NormalizedIrVersion::new(),
            repository,
            generation,
            files: Vec::new(),
            entities: Vec::new(),
            occurrences: Vec::new(),
            relations: Vec::new(),
            provenance: Vec::new(),
            source_mappings: Vec::new(),
            coverage_records: Vec::new(),
            skipped_regions: Vec::new(),
            diagnostics: Vec::new(),
            extensions: Vec::new(),
        }
    }
}

/// A strictly dispatched legacy 1.0 or normalized 1.1 IR document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrDocument {
    /// The frozen legacy 1.0 envelope.
    LegacyV1_0(IrDocumentSchema),
    /// The current normalized 1.1 fact document.
    NormalizedV1_1(NormalizedIrDocument),
}

impl IrDocument {
    /// Returns the exact version selected by the bounded dispatch decoder.
    #[must_use]
    pub const fn version(&self) -> IrVersion {
        match self {
            Self::LegacyV1_0(_) => IR_VERSION,
            Self::NormalizedV1_1(_) => NORMALIZED_IR_VERSION,
        }
    }
}

impl Serialize for IrDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::LegacyV1_0(document) => document.serialize(serializer),
            Self::NormalizedV1_1(document) => document.serialize(serializer),
        }
    }
}

/// Bounded legacy 1.x envelope decoding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LegacyIrDocumentDecodeError {
    /// The encoded legacy envelope exceeded its configured byte ceiling.
    #[error("encoded legacy IR document contains {observed} bytes, limit is {limit}")]
    EncodedDocumentTooLarge {
        /// Observed encoded byte length.
        observed: usize,
        /// Configured encoded byte ceiling.
        limit: usize,
    },
    /// The input was not syntactically valid JSON.
    #[error("encoded legacy IR document is malformed")]
    MalformedDocument,
    /// The decoded JSON did not match the frozen legacy envelope shape.
    #[error("encoded legacy IR document is invalid")]
    InvalidDocument,
    /// The envelope declared an unsupported major version.
    #[error("unsupported legacy IR major version {major}")]
    UnsupportedMajor {
        /// Unsupported major component.
        major: u16,
    },
}

/// Bounded standalone extension-envelope decoding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExtensionEnvelopeDecodeError {
    /// The encoded envelope exceeded its configured byte ceiling.
    #[error("encoded extension envelope contains {observed} bytes, limit is {limit}")]
    EncodedEnvelopeTooLarge {
        /// Observed encoded byte length.
        observed: usize,
        /// Configured encoded byte ceiling.
        limit: usize,
    },
    /// The input was not syntactically valid JSON.
    #[error("encoded extension envelope is malformed")]
    MalformedEnvelope,
    /// The decoded JSON did not match the strict extension-envelope shape.
    #[error("encoded extension envelope has an invalid shape")]
    InvalidEnvelopeShape,
    /// The envelope exceeded a nested, string, or payload quota.
    #[error("encoded extension envelope exceeds configured limits")]
    EnvelopeLimitExceeded,
    /// The envelope namespace or version was not syntactically valid.
    #[error("encoded extension envelope has an invalid identity")]
    InvalidExtensionIdentity,
    /// A caller-supplied cooperative checkpoint stopped decoding.
    #[error("extension envelope decoding was interrupted")]
    Interrupted,
}

/// Bounded standalone normalized-record decoding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NormalizedRecordDecodeError {
    /// The encoded record exceeded the document byte ceiling.
    #[error("encoded normalized record contains {observed} bytes, limit is {limit}")]
    EncodedRecordTooLarge {
        /// Observed encoded byte length.
        observed: usize,
        /// Configured encoded byte ceiling.
        limit: usize,
    },
    /// The input was not syntactically valid JSON.
    #[error("encoded normalized record is malformed")]
    MalformedRecord,
    /// The decoded JSON did not match the strict record shape.
    #[error("encoded normalized record has an invalid shape")]
    InvalidRecordShape,
    /// A caller-supplied cooperative checkpoint stopped decoding.
    #[error("normalized record decoding was interrupted")]
    Interrupted,
}

/// Bounded IR document decoding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IrDocumentDecodeError {
    /// The encoded document exceeded its configured byte ceiling.
    #[error("encoded IR document contains {observed} bytes, limit is {limit}")]
    EncodedDocumentTooLarge {
        /// Observed encoded byte length.
        observed: usize,
        /// Configured encoded byte ceiling.
        limit: usize,
    },
    /// The input was not a strict supported IR JSON document.
    #[error("encoded IR document is malformed")]
    MalformedDocument,
    /// The document declared a version other than exactly 1.0 or 1.1.
    #[error("unsupported IR version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major component.
        major: u16,
        /// Unsupported minor component.
        minor: u16,
    },
    /// The fields did not match the shape selected by the declared version.
    #[error("encoded IR document does not match its declared version")]
    InvalidDocumentShape,
    /// A normalized 1.1 document failed quota or semantic validation.
    #[error("normalized IR document is invalid")]
    InvalidNormalizedDocument,
}

/// Decodes a byte-bounded frozen legacy IR envelope.
///
/// This compatibility entry point accepts every additive minor under major 1.
/// Unified dispatch through [`decode_ir_document`] remains exact and accepts
/// only versions 1.0 and 1.1.
///
/// Direct Serde decoding is intentionally unavailable:
///
/// ```compile_fail
/// use rootlight_ir::IrDocumentSchema;
///
/// fn decode_without_limits(encoded: &[u8]) {
///     let _: IrDocumentSchema = serde_json::from_slice(encoded).unwrap();
/// }
/// ```
///
/// # Errors
///
/// Returns [`LegacyIrDocumentDecodeError::EncodedDocumentTooLarge`] before
/// JSON decoding when `encoded` exceeds [`IrLimits::max_document_bytes`].
/// Malformed JSON, an invalid strict envelope shape, an invalid producer
/// identity, or a major other than 1 is rejected without exposing input text.
pub fn decode_legacy_ir_document(
    encoded: &[u8],
    limits: &IrLimits,
) -> Result<IrDocumentSchema, LegacyIrDocumentDecodeError> {
    let observed = encoded.len();
    if observed > limits.max_document_bytes {
        return Err(LegacyIrDocumentDecodeError::EncodedDocumentTooLarge {
            observed,
            limit: limits.max_document_bytes,
        });
    }

    // Probe before strict decoding so unsupported majors cannot materialize
    // attacker-controlled producer strings from an otherwise valid envelope.
    let version = serde_json::from_slice::<VersionProbe>(encoded)
        .map_err(classify_legacy_wire_error)?
        .version;
    if version.major() != IR_VERSION.major() {
        return Err(LegacyIrDocumentDecodeError::UnsupportedMajor {
            major: version.major(),
        });
    }

    let wire = serde_json::from_slice::<LegacyWireDocument>(encoded)
        .map_err(classify_legacy_wire_error)?;
    wire.into_domain()
        .map_err(|()| LegacyIrDocumentDecodeError::InvalidDocument)
}

/// Decodes and validates one byte-bounded standalone extension envelope.
///
/// The encoded ceiling is checked before Serde can materialize payload,
/// metadata, or nested evidence. The decoded value is then checked against the
/// configured per-envelope nested, string, and payload quotas and against the
/// extension identity grammar.
///
/// Direct Serde decoding is intentionally unavailable:
///
/// ```compile_fail
/// use rootlight_ir::ExtensionEnvelope;
///
/// fn decode_without_limits(encoded: &[u8]) {
///     let _: ExtensionEnvelope = serde_json::from_slice(encoded).unwrap();
/// }
/// ```
///
/// # Errors
///
/// Returns [`ExtensionEnvelopeDecodeError::EncodedEnvelopeTooLarge`] before
/// JSON decoding when `encoded` exceeds
/// [`IrLimits::max_extension_envelope_bytes`]. Strict shape, quota, and
/// identity failures are source-free.
pub fn decode_extension_envelope(
    encoded: &[u8],
    limits: &IrLimits,
) -> Result<ExtensionEnvelope, ExtensionEnvelopeDecodeError> {
    decode_extension_envelope_with_checkpoint(encoded, limits, || true)
}

/// Decodes one bounded extension envelope with cooperative byte checkpoints.
///
/// The callback runs before decoding and after each 4 KiB of JSON input. It
/// must return `false` to stop before additional input is parsed.
///
/// # Errors
///
/// Returns the same failures as [`decode_extension_envelope`], plus
/// [`ExtensionEnvelopeDecodeError::Interrupted`] when `checkpoint` stops work.
pub fn decode_extension_envelope_with_checkpoint(
    encoded: &[u8],
    limits: &IrLimits,
    checkpoint: impl FnMut() -> bool,
) -> Result<ExtensionEnvelope, ExtensionEnvelopeDecodeError> {
    let observed = encoded.len();
    if observed > limits.max_extension_envelope_bytes {
        return Err(ExtensionEnvelopeDecodeError::EncodedEnvelopeTooLarge {
            observed,
            limit: limits.max_extension_envelope_bytes,
        });
    }

    let wire =
        decode_checkpointed::<WireExtensionEnvelope>(encoded, checkpoint).map_err(|error| {
            match error {
                CheckpointDecodeError::Malformed => ExtensionEnvelopeDecodeError::MalformedEnvelope,
                CheckpointDecodeError::InvalidShape => {
                    ExtensionEnvelopeDecodeError::InvalidEnvelopeShape
                }
                CheckpointDecodeError::Interrupted => ExtensionEnvelopeDecodeError::Interrupted,
            }
        })?;
    let envelope = wire
        .into_domain()
        .map_err(|()| ExtensionEnvelopeDecodeError::InvalidEnvelopeShape)?;
    match validate_standalone_extension_envelope(&envelope, limits) {
        Ok(()) => Ok(envelope),
        Err(StandaloneExtensionValidationError::Limit) => {
            Err(ExtensionEnvelopeDecodeError::EnvelopeLimitExceeded)
        }
        Err(StandaloneExtensionValidationError::Identity) => {
            Err(ExtensionEnvelopeDecodeError::InvalidExtensionIdentity)
        }
    }
}

/// Decodes one bounded source-mapping record with cooperative byte checkpoints.
///
/// Cross-record ownership and reference invariants remain the responsibility
/// of [`canonicalize_ir_document`].
///
/// # Errors
///
/// Returns [`NormalizedRecordDecodeError`] for size, JSON, strict shape, or
/// checkpoint failures.
pub fn decode_source_mapping_record_with_checkpoint(
    encoded: &[u8],
    limits: &IrLimits,
    checkpoint: impl FnMut() -> bool,
) -> Result<SourceMappingRecord, NormalizedRecordDecodeError> {
    check_standalone_record_size(encoded, limits)?;
    decode_checkpointed::<WireSourceMappingRecord>(encoded, checkpoint)
        .map_err(NormalizedRecordDecodeError::from)?
        .into_domain()
        .map_err(|()| NormalizedRecordDecodeError::InvalidRecordShape)
}

/// Decodes one bounded skipped-region record with cooperative byte checkpoints.
///
/// Cross-record ownership and reference invariants remain the responsibility
/// of [`canonicalize_ir_document`].
///
/// # Errors
///
/// Returns [`NormalizedRecordDecodeError`] for size, JSON, strict shape, or
/// checkpoint failures.
pub fn decode_skipped_region_with_checkpoint(
    encoded: &[u8],
    limits: &IrLimits,
    checkpoint: impl FnMut() -> bool,
) -> Result<SkippedRegion, NormalizedRecordDecodeError> {
    check_standalone_record_size(encoded, limits)?;
    decode_checkpointed::<WireSkippedRegion>(encoded, checkpoint)
        .map_err(NormalizedRecordDecodeError::from)?
        .into_domain()
        .map_err(|()| NormalizedRecordDecodeError::InvalidRecordShape)
}

/// Decodes one bounded diagnostic record with cooperative byte checkpoints.
///
/// Cross-record ownership and reference invariants remain the responsibility
/// of [`canonicalize_ir_document`].
///
/// # Errors
///
/// Returns [`NormalizedRecordDecodeError`] for size, JSON, strict shape, or
/// checkpoint failures.
pub fn decode_diagnostic_record_with_checkpoint(
    encoded: &[u8],
    limits: &IrLimits,
    checkpoint: impl FnMut() -> bool,
) -> Result<DiagnosticRecord, NormalizedRecordDecodeError> {
    check_standalone_record_size(encoded, limits)?;
    decode_checkpointed::<WireDiagnosticRecord>(encoded, checkpoint)
        .map_err(NormalizedRecordDecodeError::from)?
        .into_domain()
        .map_err(|()| NormalizedRecordDecodeError::InvalidRecordShape)
}

fn check_standalone_record_size(
    encoded: &[u8],
    limits: &IrLimits,
) -> Result<(), NormalizedRecordDecodeError> {
    let observed = encoded.len();
    if observed > limits.max_document_bytes {
        Err(NormalizedRecordDecodeError::EncodedRecordTooLarge {
            observed,
            limit: limits.max_document_bytes,
        })
    } else {
        Ok(())
    }
}

const JSON_CHECKPOINT_BYTES: usize = 4 * 1024;

enum CheckpointDecodeError {
    Malformed,
    InvalidShape,
    Interrupted,
}

impl From<CheckpointDecodeError> for NormalizedRecordDecodeError {
    fn from(error: CheckpointDecodeError) -> Self {
        match error {
            CheckpointDecodeError::Malformed => Self::MalformedRecord,
            CheckpointDecodeError::InvalidShape => Self::InvalidRecordShape,
            CheckpointDecodeError::Interrupted => Self::Interrupted,
        }
    }
}

fn decode_checkpointed<T: DeserializeOwned>(
    encoded: &[u8],
    mut checkpoint: impl FnMut() -> bool,
) -> Result<T, CheckpointDecodeError> {
    if !checkpoint() {
        return Err(CheckpointDecodeError::Interrupted);
    }
    let mut reader = CheckpointReader {
        inner: Cursor::new(encoded),
        checkpoint,
        bytes_since_checkpoint: 0,
        interrupted: false,
    };
    let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
    let result = T::deserialize(&mut deserializer).and_then(|value| {
        deserializer.end()?;
        Ok(value)
    });
    drop(deserializer);
    if reader.interrupted {
        return Err(CheckpointDecodeError::Interrupted);
    }
    result.map_err(|error| {
        if error.is_data() {
            CheckpointDecodeError::InvalidShape
        } else {
            CheckpointDecodeError::Malformed
        }
    })
}

struct CheckpointReader<'a, F> {
    inner: Cursor<&'a [u8]>,
    checkpoint: F,
    bytes_since_checkpoint: usize,
    interrupted: bool,
}

impl<F: FnMut() -> bool> Read for CheckpointReader<'_, F> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.interrupted {
            return Err(io::Error::other("controlled JSON decode stopped"));
        }
        if self.bytes_since_checkpoint >= JSON_CHECKPOINT_BYTES {
            if !(self.checkpoint)() {
                self.interrupted = true;
                return Err(io::Error::other("controlled JSON decode stopped"));
            }
            self.bytes_since_checkpoint = 0;
        }
        let read = self.inner.read(buffer)?;
        self.bytes_since_checkpoint = self.bytes_since_checkpoint.saturating_add(read);
        Ok(read)
    }
}

/// Decodes, validates, and dispatches one byte-bounded IR JSON document.
///
/// The encoded byte ceiling is checked before Serde can materialize any
/// document string or collection. Version 1.0 returns the frozen legacy
/// envelope. Version 1.1 is validated against raw quotas and returned in
/// deterministic canonical order under the supplied extension policy.
///
/// Direct Serde decoding is intentionally unavailable:
///
/// ```compile_fail
/// use rootlight_ir::IrDocument;
///
/// fn decode_without_limits(encoded: &[u8]) {
///     let _: IrDocument = serde_json::from_slice(encoded).unwrap();
/// }
/// ```
///
/// ```compile_fail
/// use rootlight_ir::NormalizedIrDocument;
///
/// fn decode_normalized_without_limits(encoded: &[u8]) {
///     let _: NormalizedIrDocument = serde_json::from_slice(encoded).unwrap();
/// }
/// ```
///
/// Dynamic record traits cannot bypass the document preflight:
///
/// ```compile_fail
/// use rootlight_ir::{
///     CoverageRecord, DiagnosticRecord, EntityRecord, ExtensionEnvelope,
///     FactEvidence, FileRecord, OccurrenceRecord, OccurrenceTarget,
///     ProducerIdentity, ProvenanceRecord, RelationRecord, SkippedRegion,
///     SourceMappingRecord,
/// };
/// use serde::de::DeserializeOwned;
///
/// fn require_deserialize<T: DeserializeOwned>() {}
///
/// fn dynamic_records_do_not_deserialize() {
///     require_deserialize::<ProducerIdentity>();
///     require_deserialize::<FactEvidence>();
///     require_deserialize::<FileRecord>();
///     require_deserialize::<EntityRecord>();
///     require_deserialize::<OccurrenceTarget>();
///     require_deserialize::<OccurrenceRecord>();
///     require_deserialize::<RelationRecord>();
///     require_deserialize::<ProvenanceRecord>();
///     require_deserialize::<SourceMappingRecord>();
///     require_deserialize::<CoverageRecord>();
///     require_deserialize::<SkippedRegion>();
///     require_deserialize::<DiagnosticRecord>();
///     require_deserialize::<ExtensionEnvelope>();
/// }
/// ```
///
/// # Errors
///
/// Returns [`IrDocumentDecodeError::EncodedDocumentTooLarge`] before JSON
/// decoding when `encoded` exceeds [`IrLimits::max_document_bytes`]. Malformed
/// JSON and unsupported versions are rejected without exposing input content.
/// Version-shape mismatches and invalid normalized quotas, references,
/// provenance, source mappings, coverage, or extensions are also rejected.
pub fn decode_ir_document(
    encoded: &[u8],
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<IrDocument, IrDocumentDecodeError> {
    let observed = encoded.len();
    if observed > limits.max_document_bytes {
        return Err(IrDocumentDecodeError::EncodedDocumentTooLarge {
            observed,
            limit: limits.max_document_bytes,
        });
    }

    // Serde skips all unrecognized fields during this pass, so an unsupported
    // version does not materialize attacker-controlled document collections.
    let version = serde_json::from_slice::<VersionProbe>(encoded)
        .map_err(|_| IrDocumentDecodeError::MalformedDocument)?
        .version;
    if version != IR_VERSION && version != NORMALIZED_IR_VERSION {
        return Err(IrDocumentDecodeError::UnsupportedVersion {
            major: version.major(),
            minor: version.minor(),
        });
    }

    match version {
        IR_VERSION => {
            let wire = serde_json::from_slice::<LegacyWireDocument>(encoded)
                .map_err(classify_wire_error)?;
            wire.into_domain()
                .map(IrDocument::LegacyV1_0)
                .map_err(|_| IrDocumentDecodeError::InvalidDocumentShape)
        }
        NORMALIZED_IR_VERSION => {
            let wire = serde_json::from_slice::<NormalizedWireDocument>(encoded)
                .map_err(classify_wire_error)?;
            let document = wire
                .into_domain()
                .map_err(|()| IrDocumentDecodeError::InvalidDocumentShape)?;
            canonicalize_ir_document(document, limits, extensions)
                .map(IrDocument::NormalizedV1_1)
                .map_err(|_| IrDocumentDecodeError::InvalidNormalizedDocument)
        }
        version => Err(IrDocumentDecodeError::UnsupportedVersion {
            major: version.major(),
            minor: version.minor(),
        }),
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    version: IrVersion,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWireDocument {
    version: IrVersion,
    generation: GenerationId,
    producer: WireProducerIdentity,
    build_context: BuildContextIdentity,
    coverage: CoverageStatus,
    evidence: EvidenceKind,
}

impl LegacyWireDocument {
    fn into_domain(self) -> Result<IrDocumentSchema, ()> {
        IrDocumentSchema::new(
            self.version,
            self.generation,
            self.producer.into_domain()?,
            self.build_context,
            self.coverage,
            self.evidence,
        )
        .map_err(|_| ())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizedWireDocument {
    version: NormalizedIrVersion,
    repository: RepositoryId,
    generation: GenerationId,
    files: Vec<WireFileRecord>,
    entities: Vec<WireEntityRecord>,
    occurrences: Vec<WireOccurrenceRecord>,
    relations: Vec<WireRelationRecord>,
    provenance: Vec<WireProvenanceRecord>,
    source_mappings: Vec<WireSourceMappingRecord>,
    coverage_records: Vec<WireCoverageRecord>,
    skipped_regions: Vec<WireSkippedRegion>,
    diagnostics: Vec<WireDiagnosticRecord>,
    extensions: Vec<WireExtensionEnvelope>,
}

impl NormalizedWireDocument {
    fn into_domain(self) -> Result<NormalizedIrDocument, ()> {
        Ok(NormalizedIrDocument {
            version: self.version,
            repository: self.repository,
            generation: self.generation,
            files: self
                .files
                .into_iter()
                .map(WireFileRecord::into_domain)
                .collect::<Result<_, _>>()?,
            entities: self
                .entities
                .into_iter()
                .map(WireEntityRecord::into_domain)
                .collect::<Result<_, _>>()?,
            occurrences: self
                .occurrences
                .into_iter()
                .map(WireOccurrenceRecord::into_domain)
                .collect::<Result<_, _>>()?,
            relations: self
                .relations
                .into_iter()
                .map(WireRelationRecord::into_domain)
                .collect::<Result<_, _>>()?,
            provenance: self
                .provenance
                .into_iter()
                .map(WireProvenanceRecord::into_domain)
                .collect::<Result<_, _>>()?,
            source_mappings: self
                .source_mappings
                .into_iter()
                .map(WireSourceMappingRecord::into_domain)
                .collect::<Result<_, _>>()?,
            coverage_records: self
                .coverage_records
                .into_iter()
                .map(WireCoverageRecord::into_domain)
                .collect::<Result<_, _>>()?,
            skipped_regions: self
                .skipped_regions
                .into_iter()
                .map(WireSkippedRegion::into_domain)
                .collect::<Result<_, _>>()?,
            diagnostics: self
                .diagnostics
                .into_iter()
                .map(WireDiagnosticRecord::into_domain)
                .collect::<Result<_, _>>()?,
            extensions: self
                .extensions
                .into_iter()
                .map(WireExtensionEnvelope::into_domain)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProducerIdentity {
    name: String,
    version: String,
    configuration_hash: ContentHash,
}

impl WireProducerIdentity {
    fn into_domain(self) -> Result<ProducerIdentity, ()> {
        ProducerIdentity::new(&self.name, &self.version, self.configuration_hash).map_err(|_| ())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourceSpan {
    file: FileId,
    start_byte: u64,
    end_byte: u64,
}

impl WireSourceSpan {
    fn into_domain(self) -> Result<SourceSpan, ()> {
        SourceSpan::new(self.file, self.start_byte, self.end_byte).map_err(|_| ())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLineRange {
    start_line: u64,
    end_line: u64,
}

impl WireLineRange {
    fn into_domain(self) -> Result<LineRange, ()> {
        LineRange::new(self.start_line, self.end_line).map_err(|_| ())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourceRef {
    repository: RepositoryId,
    generation: GenerationId,
    span: WireSourceSpan,
    content_hash: ContentHash,
    line_hint: Option<WireLineRange>,
}

impl WireSourceRef {
    fn into_domain(self) -> Result<SourceRef, ()> {
        Ok(SourceRef::new(
            self.repository,
            self.generation,
            self.span.into_domain()?,
            self.content_hash,
            self.line_hint.map(WireLineRange::into_domain).transpose()?,
        ))
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireFactRef {
    File(FileId),
    Entity(SymbolId),
    Fact(FactId),
}

impl From<WireFactRef> for FactRef {
    fn from(wire: WireFactRef) -> Self {
        match wire {
            WireFactRef::File(id) => Self::File(id),
            WireFactRef::Entity(id) => Self::Entity(id),
            WireFactRef::Fact(id) => Self::Fact(id),
        }
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireContainerRef {
    Repository(RepositoryId),
    File(FileId),
    Entity(SymbolId),
}

impl From<WireContainerRef> for ContainerRef {
    fn from(wire: WireContainerRef) -> Self {
        match wire {
            WireContainerRef::Repository(id) => Self::Repository(id),
            WireContainerRef::File(id) => Self::File(id),
            WireContainerRef::Entity(id) => Self::Entity(id),
        }
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireRelationEndpoint {
    Repository(RepositoryId),
    File(FileId),
    Entity(SymbolId),
    Occurrence(FactId),
}

impl From<WireRelationEndpoint> for RelationEndpoint {
    fn from(wire: WireRelationEndpoint) -> Self {
        match wire {
            WireRelationEndpoint::Repository(id) => Self::Repository(id),
            WireRelationEndpoint::File(id) => Self::File(id),
            WireRelationEndpoint::Entity(id) => Self::Entity(id),
            WireRelationEndpoint::Occurrence(id) => Self::Occurrence(id),
        }
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireCoverageScope {
    Repository(RepositoryId),
    File(FileId),
    Entity(SymbolId),
}

impl From<WireCoverageScope> for CoverageScope {
    fn from(wire: WireCoverageScope) -> Self {
        match wire {
            WireCoverageScope::Repository(id) => Self::Repository(id),
            WireCoverageScope::File(id) => Self::File(id),
            WireCoverageScope::Entity(id) => Self::Entity(id),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFactEvidence {
    source: Option<WireSourceRef>,
    derivation: Vec<WireFactRef>,
}

impl WireFactEvidence {
    fn into_domain(self) -> Result<FactEvidence, ()> {
        Ok(FactEvidence {
            source: self.source.map(WireSourceRef::into_domain).transpose()?,
            derivation: self.derivation.into_iter().map(Into::into).collect(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFileRecord {
    id: FileId,
    repository: RepositoryId,
    generation: GenerationId,
    path: String,
    content_hash: ContentHash,
    byte_length: u64,
    language: String,
    encoding: String,
    generated: bool,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireFileRecord {
    fn into_domain(self) -> Result<FileRecord, ()> {
        Ok(FileRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            path: self.path,
            content_hash: self.content_hash,
            byte_length: self.byte_length,
            language: self.language,
            encoding: self.encoding,
            generated: self.generated,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntityRecord {
    id: SymbolId,
    repository: RepositoryId,
    generation: GenerationId,
    kind: EntityKind,
    language: String,
    tier: AnalysisTier,
    canonical_name: String,
    display_name: String,
    qualified_name: String,
    container: Option<WireContainerRef>,
    visibility: EntityVisibility,
    flags: Vec<EntityFlag>,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireEntityRecord {
    fn into_domain(self) -> Result<EntityRecord, ()> {
        Ok(EntityRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            kind: self.kind,
            language: self.language,
            tier: self.tier,
            canonical_name: self.canonical_name,
            display_name: self.display_name,
            qualified_name: self.qualified_name,
            container: self.container.map(Into::into),
            visibility: self.visibility,
            flags: self.flags,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireOccurrenceTarget {
    Resolved {
        symbol: SymbolId,
    },
    Candidates {
        symbols: Vec<SymbolId>,
        total_count: u64,
        completeness: CoverageStatus,
    },
    Unresolved {
        text_hash: ContentHash,
    },
}

impl From<WireOccurrenceTarget> for OccurrenceTarget {
    fn from(wire: WireOccurrenceTarget) -> Self {
        match wire {
            WireOccurrenceTarget::Resolved { symbol } => Self::Resolved { symbol },
            WireOccurrenceTarget::Candidates {
                symbols,
                total_count,
                completeness,
            } => Self::Candidates {
                symbols,
                total_count,
                completeness,
            },
            WireOccurrenceTarget::Unresolved { text_hash } => Self::Unresolved { text_hash },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireOccurrenceRecord {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    source: WireSourceRef,
    role: OccurrenceRole,
    enclosing: Option<SymbolId>,
    target: WireOccurrenceTarget,
    syntactic_text_hash: ContentHash,
    syntax_kind: String,
    provenance: FactId,
    confidence: Confidence,
    evidence: WireFactEvidence,
}

impl WireOccurrenceRecord {
    fn into_domain(self) -> Result<OccurrenceRecord, ()> {
        Ok(OccurrenceRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            file: self.file,
            source: self.source.into_domain()?,
            role: self.role,
            enclosing: self.enclosing,
            target: self.target.into(),
            syntactic_text_hash: self.syntactic_text_hash,
            syntax_kind: self.syntax_kind,
            provenance: self.provenance,
            confidence: self.confidence,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRelationRecord {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    subject: WireRelationEndpoint,
    predicate: RelationPredicate,
    object: WireRelationEndpoint,
    confidence: Confidence,
    evidence_kind: EvidenceKind,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireRelationRecord {
    fn into_domain(self) -> Result<RelationRecord, ()> {
        Ok(RelationRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            subject: self.subject.into(),
            predicate: self.predicate,
            object: self.object.into(),
            confidence: self.confidence,
            evidence_kind: self.evidence_kind,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvenanceRecord {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    producer_kind: ProducerKind,
    producer: WireProducerIdentity,
    binary_digest: ContentHash,
    frontend_version: Option<String>,
    language: String,
    tier: AnalysisTier,
    build_context: BuildContextIdentity,
    input_sources: Vec<WireSourceRef>,
    evidence_sources: Vec<WireSourceRef>,
    derivation_parents: Vec<WireFactRef>,
    rule: Option<String>,
}

impl WireProvenanceRecord {
    fn into_domain(self) -> Result<ProvenanceRecord, ()> {
        Ok(ProvenanceRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            producer_kind: self.producer_kind,
            producer: self.producer.into_domain()?,
            binary_digest: self.binary_digest,
            frontend_version: self.frontend_version,
            language: self.language,
            tier: self.tier,
            build_context: self.build_context,
            input_sources: self
                .input_sources
                .into_iter()
                .map(WireSourceRef::into_domain)
                .collect::<Result<_, _>>()?,
            evidence_sources: self
                .evidence_sources
                .into_iter()
                .map(WireSourceRef::into_domain)
                .collect::<Result<_, _>>()?,
            derivation_parents: self
                .derivation_parents
                .into_iter()
                .map(Into::into)
                .collect(),
            rule: self.rule,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourceMappingRecord {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    from: WireSourceRef,
    to: WireSourceRef,
    kind: SourceMappingKind,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireSourceMappingRecord {
    fn into_domain(self) -> Result<SourceMappingRecord, ()> {
        Ok(SourceMappingRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            from: self.from.into_domain()?,
            to: self.to.into_domain()?,
            kind: self.kind,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCoverageRecord {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    scope: WireCoverageScope,
    domain: FactDomain,
    tier: AnalysisTier,
    status: CoverageStatus,
    discovered: u64,
    indexed: u64,
    skipped: u64,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireCoverageRecord {
    fn into_domain(self) -> Result<CoverageRecord, ()> {
        Ok(CoverageRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            scope: self.scope.into(),
            domain: self.domain,
            tier: self.tier,
            status: self.status,
            discovered: self.discovered,
            indexed: self.indexed,
            skipped: self.skipped,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSkippedRegion {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    source: WireSourceRef,
    domain: FactDomain,
    reason: SkippedRegionReason,
    detail: String,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireSkippedRegion {
    fn into_domain(self) -> Result<SkippedRegion, ()> {
        Ok(SkippedRegion {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            source: self.source.into_domain()?,
            domain: self.domain,
            reason: self.reason,
            detail: self.detail,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDiagnosticRecord {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    code: String,
    message: String,
    severity: DiagnosticSeverity,
    source: Option<WireSourceRef>,
    coverage_effect: CoverageStatus,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireDiagnosticRecord {
    fn into_domain(self) -> Result<DiagnosticRecord, ()> {
        Ok(DiagnosticRecord {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            code: self.code,
            message: self.message,
            severity: self.severity,
            source: self.source.map(WireSourceRef::into_domain).transpose()?,
            coverage_effect: self.coverage_effect,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExtensionEnvelope {
    id: FactId,
    repository: RepositoryId,
    generation: GenerationId,
    namespace: String,
    version: String,
    criticality: ExtensionCriticality,
    payload: String,
    provenance: FactId,
    evidence: WireFactEvidence,
}

impl WireExtensionEnvelope {
    fn into_domain(self) -> Result<ExtensionEnvelope, ()> {
        Ok(ExtensionEnvelope {
            id: self.id,
            repository: self.repository,
            generation: self.generation,
            namespace: self.namespace,
            version: self.version,
            criticality: self.criticality,
            payload: self.payload,
            provenance: self.provenance,
            evidence: self.evidence.into_domain()?,
        })
    }
}

fn classify_legacy_wire_error(error: serde_json::Error) -> LegacyIrDocumentDecodeError {
    if error.is_data() {
        LegacyIrDocumentDecodeError::InvalidDocument
    } else {
        LegacyIrDocumentDecodeError::MalformedDocument
    }
}

fn classify_wire_error(error: serde_json::Error) -> IrDocumentDecodeError {
    if error.is_data() {
        IrDocumentDecodeError::InvalidDocumentShape
    } else {
        IrDocumentDecodeError::MalformedDocument
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::FactId;

    use super::*;

    const LEGACY_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/compatibility/ir/1.0/document.json");
    const NORMALIZED_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json");
    const LEXICAL_ENVELOPE_FIXTURE: &[u8] = include_bytes!(
        "../../../tests/fixtures/compatibility/extensions/rootlight.lexical/1/envelope.json"
    );

    fn legacy_value() -> serde_json::Value {
        serde_json::from_slice(LEGACY_FIXTURE).expect("legacy fixture JSON parses")
    }

    fn normalized_value() -> serde_json::Value {
        serde_json::from_slice(NORMALIZED_FIXTURE).expect("normalized fixture JSON parses")
    }

    fn lexical_envelope_value() -> serde_json::Value {
        serde_json::from_slice(LEXICAL_ENVELOPE_FIXTURE)
            .expect("lexical envelope fixture JSON parses")
    }

    #[test]
    fn standalone_decoder_stops_at_a_mid_payload_checkpoint() {
        let mut value = lexical_envelope_value();
        value["payload"] = serde_json::json!("x".repeat(16 * 1024));
        let encoded = encode_test_value(&value);
        let mut checkpoints = 0_u8;

        let error =
            decode_extension_envelope_with_checkpoint(&encoded, &IrLimits::default(), || {
                checkpoints = checkpoints.saturating_add(1);
                checkpoints < 2
            })
            .expect_err("the second checkpoint stops decoding");

        assert_eq!(error, ExtensionEnvelopeDecodeError::Interrupted);
        assert_eq!(checkpoints, 2);
    }

    fn encode_test_value(value: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(value).expect("test value serializes")
    }

    fn decode(encoded: &[u8], limits: &IrLimits) -> Result<IrDocument, IrDocumentDecodeError> {
        decode_ir_document(encoded, limits, &ExtensionSupport::default())
    }

    #[test]
    fn bounded_dispatch_accepts_exact_legacy_and_normalized_versions() {
        assert!(matches!(
            decode(LEGACY_FIXTURE, &IrLimits::default()),
            Ok(IrDocument::LegacyV1_0(_))
        ));
        assert!(matches!(
            decode(NORMALIZED_FIXTURE, &IrLimits::default()),
            Ok(IrDocument::NormalizedV1_1(_))
        ));
    }

    #[test]
    fn bounded_dispatch_rejects_malformed_and_unsupported_versions() {
        assert_eq!(
            decode(b"{", &IrLimits::default()),
            Err(IrDocumentDecodeError::MalformedDocument)
        );

        let mut value = normalized_value();
        value["version"]["minor"] = serde_json::json!(2);
        assert_eq!(
            decode(&encode_test_value(&value), &IrLimits::default()),
            Err(IrDocumentDecodeError::UnsupportedVersion { major: 1, minor: 2 })
        );
    }

    #[test]
    fn exact_dispatch_rejects_cross_version_fields_even_when_null() {
        let mut legacy = legacy_value();
        legacy["repository"] = serde_json::Value::Null;
        assert_eq!(
            decode(&encode_test_value(&legacy), &IrLimits::default()),
            Err(IrDocumentDecodeError::InvalidDocumentShape)
        );

        let mut normalized = normalized_value();
        normalized["producer"] = serde_json::Value::Null;
        assert_eq!(
            decode(&encode_test_value(&normalized), &IrLimits::default()),
            Err(IrDocumentDecodeError::InvalidDocumentShape)
        );
    }

    #[test]
    fn oversized_malformed_input_rejects_before_json_decoding() {
        let limits = IrLimits {
            max_document_bytes: 4,
            ..IrLimits::default()
        };
        assert_eq!(
            decode(b"{{{{{", &limits),
            Err(IrDocumentDecodeError::EncodedDocumentTooLarge {
                observed: 5,
                limit: 4,
            })
        );
    }

    #[test]
    fn legacy_decoder_preserves_additive_minor_wire_compatibility() {
        let decoded = decode_legacy_ir_document(LEGACY_FIXTURE, &IrLimits::default())
            .expect("frozen legacy fixture decodes");
        assert_eq!(
            serde_json::to_value(decoded).expect("legacy document serializes"),
            legacy_value()
        );

        let mut additive = legacy_value();
        additive["version"]["minor"] = serde_json::json!(u16::MAX);
        let decoded =
            decode_legacy_ir_document(&encode_test_value(&additive), &IrLimits::default())
                .expect("additive legacy minor decodes");
        assert_eq!(decoded.version(), IrVersion::new(1, u16::MAX));

        additive["version"]["major"] = serde_json::json!(2);
        assert_eq!(
            decode_legacy_ir_document(&encode_test_value(&additive), &IrLimits::default()),
            Err(LegacyIrDocumentDecodeError::UnsupportedMajor { major: 2 })
        );
    }

    #[test]
    fn legacy_decoder_rejects_unsupported_major_before_dynamic_fields() {
        let attacker_text = "/private/attacker.rs";
        let mut unsupported = legacy_value();
        unsupported["version"]["major"] = serde_json::json!(2);
        unsupported["producer"]["name"] = serde_json::json!(attacker_text);

        let error =
            decode_legacy_ir_document(&encode_test_value(&unsupported), &IrLimits::default())
                .expect_err("unsupported major rejects before producer validation");
        assert_eq!(
            error,
            LegacyIrDocumentDecodeError::UnsupportedMajor { major: 2 }
        );
        assert!(!error.to_string().contains(attacker_text));
    }

    #[test]
    fn legacy_decoder_rejects_oversize_unknown_fields_and_non_numeric_versions() {
        let limits = IrLimits {
            max_document_bytes: 4,
            ..IrLimits::default()
        };
        assert_eq!(
            decode_legacy_ir_document(b"{{{{{", &limits),
            Err(LegacyIrDocumentDecodeError::EncodedDocumentTooLarge {
                observed: 5,
                limit: 4,
            })
        );
        assert_eq!(
            decode_legacy_ir_document(b"{", &IrLimits::default()),
            Err(LegacyIrDocumentDecodeError::MalformedDocument)
        );

        let mut unknown = legacy_value();
        unknown["producer"]["unknown"] = serde_json::json!(true);
        assert_eq!(
            decode_legacy_ir_document(&encode_test_value(&unknown), &IrLimits::default()),
            Err(LegacyIrDocumentDecodeError::InvalidDocument)
        );

        let mut string_version = legacy_value();
        string_version["version"]["minor"] = serde_json::json!("0");
        assert_eq!(
            decode_legacy_ir_document(&encode_test_value(&string_version), &IrLimits::default()),
            Err(LegacyIrDocumentDecodeError::InvalidDocument)
        );

        let attacker_text = "/private/attacker.rs";
        let mut invalid_producer = legacy_value();
        invalid_producer["producer"]["name"] = serde_json::json!(attacker_text);
        let error =
            decode_legacy_ir_document(&encode_test_value(&invalid_producer), &IrLimits::default())
                .expect_err("invalid producer label is rejected");
        assert_eq!(error, LegacyIrDocumentDecodeError::InvalidDocument);
        assert!(!error.to_string().contains(attacker_text));
    }

    #[test]
    fn standalone_extension_decoder_preserves_wire_compatibility() {
        let decoded = decode_extension_envelope(LEXICAL_ENVELOPE_FIXTURE, &IrLimits::default())
            .expect("frozen extension envelope decodes");
        assert_eq!(
            serde_json::to_value(decoded).expect("extension envelope serializes"),
            lexical_envelope_value()
        );
    }

    #[test]
    fn standalone_extension_preflights_and_rejects_strict_shape_failures() {
        let limits = IrLimits {
            max_extension_envelope_bytes: 4,
            ..IrLimits::default()
        };
        assert_eq!(
            decode_extension_envelope(b"{{{{{", &limits),
            Err(ExtensionEnvelopeDecodeError::EncodedEnvelopeTooLarge {
                observed: 5,
                limit: 4,
            })
        );
        assert_eq!(
            decode_extension_envelope(b"{", &IrLimits::default()),
            Err(ExtensionEnvelopeDecodeError::MalformedEnvelope)
        );

        let mut cases = Vec::new();
        let mut top_level = lexical_envelope_value();
        top_level["unknown"] = serde_json::json!(true);
        cases.push(top_level);
        let mut evidence = lexical_envelope_value();
        evidence["evidence"]["unknown"] = serde_json::json!(true);
        cases.push(evidence);
        let mut fact_reference = lexical_envelope_value();
        fact_reference["evidence"]["derivation"][0]["unknown"] = serde_json::json!(true);
        cases.push(fact_reference);
        let mut numeric_version = lexical_envelope_value();
        numeric_version["version"] = serde_json::json!(1);
        cases.push(numeric_version);

        for invalid in cases {
            assert_eq!(
                decode_extension_envelope(&encode_test_value(&invalid), &IrLimits::default()),
                Err(ExtensionEnvelopeDecodeError::InvalidEnvelopeShape)
            );
        }
    }

    #[test]
    fn standalone_extension_decoder_enforces_nested_string_and_payload_quotas() {
        let value = lexical_envelope_value();
        let encoded = encode_test_value(&value);
        let derivation_len = value["evidence"]["derivation"]
            .as_array()
            .expect("fixture derivation is an array")
            .len();
        let namespace_len = value["namespace"]
            .as_str()
            .expect("fixture namespace is text")
            .len();
        let version_len = value["version"]
            .as_str()
            .expect("fixture version is text")
            .len();
        let payload_len = value["payload"]
            .as_str()
            .expect("fixture payload is text")
            .len();
        let exact_limits = IrLimits {
            max_extension_envelope_bytes: encoded.len(),
            max_nested_items_per_record: derivation_len,
            max_total_nested_items: derivation_len,
            max_string_bytes: namespace_len.max(version_len),
            max_total_string_bytes: namespace_len
                .checked_add(version_len)
                .expect("fixture lengths fit"),
            max_extension_payload_bytes: payload_len,
            max_total_extension_bytes: payload_len,
            ..IrLimits::default()
        };
        assert!(
            decode_extension_envelope(&encoded, &exact_limits).is_ok(),
            "exact standalone extension caps are inclusive"
        );

        let limit_cases = [
            IrLimits {
                max_nested_items_per_record: derivation_len.saturating_sub(1),
                ..IrLimits::default()
            },
            IrLimits {
                max_total_nested_items: derivation_len.saturating_sub(1),
                ..IrLimits::default()
            },
            IrLimits {
                max_string_bytes: namespace_len.saturating_sub(1),
                ..IrLimits::default()
            },
            IrLimits {
                max_total_string_bytes: namespace_len
                    .checked_add(version_len)
                    .expect("fixture lengths fit")
                    .saturating_sub(1),
                ..IrLimits::default()
            },
            IrLimits {
                max_extension_payload_bytes: payload_len.saturating_sub(1),
                ..IrLimits::default()
            },
            IrLimits {
                max_total_extension_bytes: payload_len.saturating_sub(1),
                ..IrLimits::default()
            },
        ];
        for limits in limit_cases {
            assert_eq!(
                decode_extension_envelope(&encoded, &limits),
                Err(ExtensionEnvelopeDecodeError::EnvelopeLimitExceeded)
            );
        }
    }

    #[test]
    fn standalone_extension_errors_do_not_expose_attacker_identity_text() {
        let attacker_text = "/private/attacker.rs";
        let mut invalid = lexical_envelope_value();
        invalid["namespace"] = serde_json::json!(attacker_text);
        let error = decode_extension_envelope(&encode_test_value(&invalid), &IrLimits::default())
            .expect_err("invalid identity is rejected");
        assert_eq!(
            error,
            ExtensionEnvelopeDecodeError::InvalidExtensionIdentity
        );
        assert!(!error.to_string().contains(attacker_text));
    }

    #[test]
    fn normalized_private_wires_reject_nested_unknown_fields() {
        let mut cases = Vec::new();

        let mut evidence = normalized_value();
        evidence["entities"][0]["evidence"]["unknown"] = serde_json::json!(true);
        cases.push(evidence);

        let mut source_span = normalized_value();
        source_span["entities"][0]["evidence"]["source"]["span"]["unknown"] =
            serde_json::json!(true);
        cases.push(source_span);

        let mut container = normalized_value();
        container["entities"][0]["container"]["unknown"] = serde_json::json!(true);
        cases.push(container);

        let mut occurrence_target = normalized_value();
        occurrence_target["occurrences"][0]["target"]["unknown"] = serde_json::json!(true);
        cases.push(occurrence_target);

        let mut relation_endpoint = normalized_value();
        relation_endpoint["relations"][0]["subject"]["unknown"] = serde_json::json!(true);
        cases.push(relation_endpoint);

        let mut producer = normalized_value();
        producer["provenance"][0]["producer"]["unknown"] = serde_json::json!(true);
        cases.push(producer);

        for invalid in cases {
            assert_eq!(
                decode(&encode_test_value(&invalid), &IrLimits::default()),
                Err(IrDocumentDecodeError::InvalidDocumentShape)
            );
        }
    }

    #[test]
    fn normalized_decode_canonicalizes_equal_duplicates() {
        let mut value = normalized_value();
        let duplicate = value["entities"][0].clone();
        value["entities"]
            .as_array_mut()
            .expect("fixture entities are an array")
            .push(duplicate);

        let decoded = decode(&encode_test_value(&value), &IrLimits::default())
            .expect("equal duplicates canonicalize");
        let IrDocument::NormalizedV1_1(document) = decoded else {
            panic!("normalized fixture must dispatch to version 1.1");
        };
        assert_eq!(document.entities.len(), 1);
    }

    #[test]
    fn invalid_normalized_quotas_and_references_never_return_documents() {
        let limits = IrLimits {
            max_files: 0,
            ..IrLimits::default()
        };
        assert_eq!(
            decode(NORMALIZED_FIXTURE, &limits),
            Err(IrDocumentDecodeError::InvalidNormalizedDocument)
        );

        let mut invalid_reference = normalized_value();
        invalid_reference["entities"][0]["provenance"] =
            serde_json::to_value(FactId::from_bytes([99; 20])).expect("fact ID serializes");
        assert_eq!(
            decode(&encode_test_value(&invalid_reference), &IrLimits::default()),
            Err(IrDocumentDecodeError::InvalidNormalizedDocument)
        );
    }

    #[test]
    fn decode_errors_do_not_expose_attacker_controlled_namespaces() {
        let sensitive_namespace = "/private/attacker.rs";
        let mut value = normalized_value();
        value["extensions"][0]["namespace"] = serde_json::json!(sensitive_namespace);

        let error = decode(&encode_test_value(&value), &IrLimits::default())
            .expect_err("invalid namespace is rejected");
        assert_eq!(error, IrDocumentDecodeError::InvalidNormalizedDocument);
        assert!(!error.to_string().contains(sensitive_namespace));
    }
}
