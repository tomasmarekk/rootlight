//! Version 1.1 of Rootlight's common normalized fact document.
//!
//! This module owns the language-neutral wire model and strict version dispatch.
//! Cross-record validation and deterministic canonicalization live separately.

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};
use serde::{Deserialize, Serialize, de};

use crate::{
    AnalysisTier, BuildContextIdentity, Confidence, CoverageStatus, EvidenceKind, IR_VERSION,
    IrDocumentSchema, IrVersion, ProducerIdentity, SourceRef,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FactEvidence {
    /// Direct immutable source evidence, when present.
    pub source: Option<SourceRef>,
    /// Base facts supporting a derived fact.
    pub derivation: Vec<FactRef>,
}

/// One immutable file owned by the document repository and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Returns the exact version selected by the dispatch decoder.
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

impl<'de> Deserialize<'de> for IrDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = DispatchDocument::deserialize(deserializer)?;
        match wire.version {
            IR_VERSION => wire.into_legacy().map(Self::LegacyV1_0),
            NORMALIZED_IR_VERSION => wire.into_normalized().map(Self::NormalizedV1_1),
            version => Err(de::Error::custom(format_args!(
                "unsupported IR version {}.{}",
                version.major(),
                version.minor()
            ))),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchDocument {
    version: IrVersion,
    generation: GenerationId,
    repository: Option<RepositoryId>,
    producer: Option<ProducerIdentity>,
    build_context: Option<BuildContextIdentity>,
    coverage: Option<CoverageStatus>,
    evidence: Option<EvidenceKind>,
    files: Option<Vec<FileRecord>>,
    entities: Option<Vec<EntityRecord>>,
    occurrences: Option<Vec<OccurrenceRecord>>,
    relations: Option<Vec<RelationRecord>>,
    provenance: Option<Vec<ProvenanceRecord>>,
    source_mappings: Option<Vec<SourceMappingRecord>>,
    coverage_records: Option<Vec<CoverageRecord>>,
    skipped_regions: Option<Vec<SkippedRegion>>,
    diagnostics: Option<Vec<DiagnosticRecord>>,
    extensions: Option<Vec<ExtensionEnvelope>>,
}

impl DispatchDocument {
    fn into_legacy<E>(self) -> Result<IrDocumentSchema, E>
    where
        E: de::Error,
    {
        if self.repository.is_some()
            || self.files.is_some()
            || self.entities.is_some()
            || self.occurrences.is_some()
            || self.relations.is_some()
            || self.provenance.is_some()
            || self.source_mappings.is_some()
            || self.coverage_records.is_some()
            || self.skipped_regions.is_some()
            || self.diagnostics.is_some()
            || self.extensions.is_some()
        {
            return Err(E::custom("IR version 1.0 contains version 1.1 fields"));
        }
        let producer = self.producer.ok_or_else(|| E::missing_field("producer"))?;
        let build_context = self
            .build_context
            .ok_or_else(|| E::missing_field("build_context"))?;
        let coverage = self.coverage.ok_or_else(|| E::missing_field("coverage"))?;
        let evidence = self.evidence.ok_or_else(|| E::missing_field("evidence"))?;
        IrDocumentSchema::new(
            self.version,
            self.generation,
            producer,
            build_context,
            coverage,
            evidence,
        )
        .map_err(E::custom)
    }

    fn into_normalized<E>(self) -> Result<NormalizedIrDocument, E>
    where
        E: de::Error,
    {
        if self.producer.is_some()
            || self.build_context.is_some()
            || self.coverage.is_some()
            || self.evidence.is_some()
        {
            return Err(E::custom(
                "IR version 1.1 contains legacy version 1.0 fields",
            ));
        }
        Ok(NormalizedIrDocument {
            version: NormalizedIrVersion::new(),
            repository: self
                .repository
                .ok_or_else(|| E::missing_field("repository"))?,
            generation: self.generation,
            files: self.files.ok_or_else(|| E::missing_field("files"))?,
            entities: self.entities.ok_or_else(|| E::missing_field("entities"))?,
            occurrences: self
                .occurrences
                .ok_or_else(|| E::missing_field("occurrences"))?,
            relations: self
                .relations
                .ok_or_else(|| E::missing_field("relations"))?,
            provenance: self
                .provenance
                .ok_or_else(|| E::missing_field("provenance"))?,
            source_mappings: self
                .source_mappings
                .ok_or_else(|| E::missing_field("source_mappings"))?,
            coverage_records: self
                .coverage_records
                .ok_or_else(|| E::missing_field("coverage_records"))?,
            skipped_regions: self
                .skipped_regions
                .ok_or_else(|| E::missing_field("skipped_regions"))?,
            diagnostics: self
                .diagnostics
                .ok_or_else(|| E::missing_field("diagnostics"))?,
            extensions: self
                .extensions
                .ok_or_else(|| E::missing_field("extensions"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{GenerationId, derive_repository};

    use super::*;

    #[test]
    fn normalized_document_requires_exact_version() {
        let repository = derive_repository(b"repository").id();
        let generation = GenerationId::from_bytes([2; 20]);
        let document = NormalizedIrDocument::empty(repository, generation);
        let mut value = serde_json::to_value(document).expect("normalized document serializes");

        value["version"]["minor"] = serde_json::json!(2);
        assert!(serde_json::from_value::<NormalizedIrDocument>(value).is_err());
    }

    #[test]
    fn dispatch_rejects_unsupported_minor_instead_of_reinterpreting_shape() {
        let repository = derive_repository(b"repository").id();
        let generation = GenerationId::from_bytes([2; 20]);
        let mut value = serde_json::to_value(NormalizedIrDocument::empty(repository, generation))
            .expect("normalized document serializes");

        value["version"]["minor"] = serde_json::json!(2);
        let error = serde_json::from_value::<IrDocument>(value)
            .expect_err("unsupported minor must be rejected");
        assert!(error.to_string().contains("unsupported IR version 1.2"));
    }

    #[test]
    fn dispatch_selects_normalized_document() {
        let repository = derive_repository(b"repository").id();
        let generation = GenerationId::from_bytes([2; 20]);
        let value = serde_json::to_value(NormalizedIrDocument::empty(repository, generation))
            .expect("normalized document serializes");

        let decoded = serde_json::from_value::<IrDocument>(value).expect("version 1.1 dispatches");
        assert!(matches!(decoded, IrDocument::NormalizedV1_1(_)));
    }
}
