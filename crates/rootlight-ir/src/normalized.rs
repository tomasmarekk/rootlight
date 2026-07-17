//! Version 1.1 of Rootlight's common normalized fact document.
//!
//! This module owns the language-neutral wire model and strict version dispatch.
//! Cross-record validation and deterministic canonicalization live separately.

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};
use serde::{Deserialize, Serialize, de};

use crate::{
    AnalysisTier, BuildContextIdentity, Confidence, CoverageStatus, EvidenceKind, ExtensionSupport,
    IR_VERSION, IrDocumentSchema, IrLimits, IrVersion, ProducerIdentity, SourceRef,
    canonicalize_ir_document,
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

/// Decodes, validates, and dispatches one byte-bounded IR JSON document.
///
/// The encoded byte ceiling is checked before Serde can materialize any
/// document string or collection. Version 1.0 returns the frozen legacy
/// envelope. Version 1.1 is validated against raw quotas and returned in
/// deterministic canonical order under the supplied extension policy.
///
/// The frozen [`IrDocumentSchema`] and individual record types retain
/// `Deserialize` for schema and fixture composition. Those trait
/// implementations do not apply document-wide [`IrLimits`] and are not
/// substitutes for this untrusted-input boundary.
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
            IrDocumentSchema::new(
                wire.version,
                wire.generation,
                wire.producer,
                wire.build_context,
                wire.coverage,
                wire.evidence,
            )
            .map(IrDocument::LegacyV1_0)
            .map_err(|_| IrDocumentDecodeError::InvalidDocumentShape)
        }
        NORMALIZED_IR_VERSION => {
            let wire = serde_json::from_slice::<NormalizedWireDocument>(encoded)
                .map_err(classify_wire_error)?;
            let document = wire.into();
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
    producer: ProducerIdentity,
    build_context: BuildContextIdentity,
    coverage: CoverageStatus,
    evidence: EvidenceKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizedWireDocument {
    version: NormalizedIrVersion,
    repository: RepositoryId,
    generation: GenerationId,
    files: Vec<FileRecord>,
    entities: Vec<EntityRecord>,
    occurrences: Vec<OccurrenceRecord>,
    relations: Vec<RelationRecord>,
    provenance: Vec<ProvenanceRecord>,
    source_mappings: Vec<SourceMappingRecord>,
    coverage_records: Vec<CoverageRecord>,
    skipped_regions: Vec<SkippedRegion>,
    diagnostics: Vec<DiagnosticRecord>,
    extensions: Vec<ExtensionEnvelope>,
}

impl From<NormalizedWireDocument> for NormalizedIrDocument {
    fn from(wire: NormalizedWireDocument) -> Self {
        Self {
            version: wire.version,
            repository: wire.repository,
            generation: wire.generation,
            files: wire.files,
            entities: wire.entities,
            occurrences: wire.occurrences,
            relations: wire.relations,
            provenance: wire.provenance,
            source_mappings: wire.source_mappings,
            coverage_records: wire.coverage_records,
            skipped_regions: wire.skipped_regions,
            diagnostics: wire.diagnostics,
            extensions: wire.extensions,
        }
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

    fn legacy_value() -> serde_json::Value {
        serde_json::from_slice(LEGACY_FIXTURE).expect("legacy fixture JSON parses")
    }

    fn normalized_value() -> serde_json::Value {
        serde_json::from_slice(NORMALIZED_FIXTURE).expect("normalized fixture JSON parses")
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
