//! Evidence-keyed linking for foreign, service, test, and configuration facts.
//!
//! Names alone never create a relation: every group carries a typed namespace,
//! normalized protocol-key hash, immutable source, provenance, and confidence.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    BuildContextIdentity, Confidence, CoverageStatus, EvidenceKind, ExtensionSupport, FactEvidence,
    FactRef, IrLimits, NormalizedIrDocument, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, canonicalize_ir_document,
    derive_provenance_record_id, derive_relation_record_id, validate_ir_document,
};

use crate::{DEFAULT_CANDIDATE_LIMIT, MAX_CANDIDATE_LIMIT, ResolverFactContext};

/// Minimum confidence required to persist a unique foreign link as exact.
pub const MIN_EXACT_FOREIGN_LINK_CONFIDENCE: u16 = 900;
/// Absolute number of typed link inputs accepted by one operation.
pub const MAX_FOREIGN_LINK_INPUTS: usize = 65_536;

/// Checked resource limits for evidence-keyed link groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignLinkLimits {
    candidate_limit: usize,
    input_limit: usize,
}

impl ForeignLinkLimits {
    /// Creates checked foreign-link limits.
    ///
    /// # Errors
    ///
    /// Returns [`ForeignLinkLimitError`] for zero limits, a candidate limit
    /// above 4,096, or an input limit above 65,536.
    pub const fn new(
        candidate_limit: usize,
        input_limit: usize,
    ) -> Result<Self, ForeignLinkLimitError> {
        if candidate_limit == 0
            || candidate_limit > MAX_CANDIDATE_LIMIT
            || input_limit == 0
            || input_limit > MAX_FOREIGN_LINK_INPUTS
        {
            Err(ForeignLinkLimitError)
        } else {
            Ok(Self {
                candidate_limit,
                input_limit,
            })
        }
    }

    /// Returns the materialized candidate ceiling for one protocol identity.
    #[must_use]
    pub const fn candidate_limit(self) -> usize {
        self.candidate_limit
    }

    /// Returns the operation-wide typed evidence ceiling.
    #[must_use]
    pub const fn input_limit(self) -> usize {
        self.input_limit
    }
}

impl Default for ForeignLinkLimits {
    fn default() -> Self {
        Self {
            candidate_limit: DEFAULT_CANDIDATE_LIMIT,
            input_limit: MAX_FOREIGN_LINK_INPUTS,
        }
    }
}

/// Invalid foreign-link resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("foreign-link limits exceed the supported bounded range")]
pub struct ForeignLinkLimitError;

/// Typed evidence namespace preventing name-only cross-language matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ForeignLinkNamespace {
    /// Native ABI, JNI, P/Invoke, cgo, or wasm import/export evidence.
    ForeignFunction,
    /// Normalized HTTP method, authority scope, and route-template evidence.
    Http,
    /// RPC schema, service, and method identity evidence.
    Rpc,
    /// Message topic, queue, producer, or consumer identity evidence.
    Messaging,
    /// Database schema, table, query, or migration evidence.
    Database,
    /// Test framework and explicit target evidence.
    Test,
    /// Configuration or generated-binding evidence.
    Configuration,
}

impl ForeignLinkNamespace {
    const fn label(self) -> &'static str {
        match self {
            Self::ForeignFunction => "foreign_function",
            Self::Http => "http",
            Self::Rpc => "rpc",
            Self::Messaging => "messaging",
            Self::Database => "database",
            Self::Test => "test",
            Self::Configuration => "configuration",
        }
    }

    fn supports(self, predicate: RelationPredicate) -> bool {
        match self {
            Self::ForeignFunction => matches!(
                predicate,
                RelationPredicate::BindsTo | RelationPredicate::CallsForeign
            ),
            Self::Http => matches!(
                predicate,
                RelationPredicate::CallsRoute | RelationPredicate::ServesRoute
            ),
            Self::Rpc => matches!(
                predicate,
                RelationPredicate::BindsTo | RelationPredicate::CallsForeign
            ),
            Self::Messaging => matches!(
                predicate,
                RelationPredicate::Publishes | RelationPredicate::Consumes
            ),
            Self::Database => matches!(
                predicate,
                RelationPredicate::ReadsTable | RelationPredicate::WritesTable
            ),
            Self::Test => predicate == RelationPredicate::Tests,
            Self::Configuration => matches!(
                predicate,
                RelationPredicate::DependsOn | RelationPredicate::GeneratedFrom
            ),
        }
    }
}

/// One candidate backed by typed protocol evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignLinkInput {
    /// Link subject.
    subject: RelationEndpoint,
    /// Link target.
    target: SymbolId,
    /// Common typed relation.
    predicate: RelationPredicate,
    /// Evidence namespace controlling predicate compatibility.
    namespace: ForeignLinkNamespace,
    /// Hash of the namespace-specific normalized protocol identity.
    protocol_key: ContentHash,
    /// Calibrated support for this candidate.
    confidence: Confidence,
    /// Immutable source evidence for the candidate.
    source: SourceRef,
    /// Existing provenance that produced the typed evidence.
    provenance: FactId,
}

impl ForeignLinkInput {
    /// Creates a checked typed foreign-link candidate.
    ///
    /// # Errors
    ///
    /// Returns [`ForeignLinkInputError`] when the relation is incompatible with
    /// the evidence namespace or has zero confidence.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        subject: RelationEndpoint,
        target: SymbolId,
        predicate: RelationPredicate,
        namespace: ForeignLinkNamespace,
        protocol_key: ContentHash,
        confidence: Confidence,
        source: SourceRef,
        provenance: FactId,
    ) -> Result<Self, ForeignLinkInputError> {
        if !namespace.supports(predicate) {
            return Err(ForeignLinkInputError::PredicateMismatch);
        }
        if confidence.get() == 0 {
            return Err(ForeignLinkInputError::ZeroConfidence);
        }
        Ok(Self {
            subject,
            target,
            predicate,
            namespace,
            protocol_key,
            confidence,
            source,
            provenance,
        })
    }

    /// Returns the link subject.
    #[must_use]
    pub const fn subject(&self) -> RelationEndpoint {
        self.subject
    }

    /// Returns the candidate target.
    #[must_use]
    pub const fn target(&self) -> SymbolId {
        self.target
    }

    /// Returns the typed relation family.
    #[must_use]
    pub const fn predicate(&self) -> RelationPredicate {
        self.predicate
    }

    /// Returns the evidence namespace.
    #[must_use]
    pub const fn namespace(&self) -> ForeignLinkNamespace {
        self.namespace
    }

    /// Returns the normalized protocol-key hash.
    #[must_use]
    pub const fn protocol_key(&self) -> ContentHash {
        self.protocol_key
    }

    /// Returns calibrated candidate confidence.
    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    /// Returns immutable source evidence.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns the producing provenance identity.
    #[must_use]
    pub const fn provenance(&self) -> FactId {
        self.provenance
    }
}

/// Invalid typed foreign-link candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ForeignLinkInputError {
    /// Predicate and evidence namespace are incompatible.
    #[error("foreign-link predicate is incompatible with its evidence namespace")]
    PredicateMismatch,
    /// Zero-confidence evidence cannot justify a candidate.
    #[error("foreign-link confidence must be greater than zero")]
    ZeroConfidence,
}

/// Exact, candidate, or unresolved foreign-link result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignLinkOutcome {
    /// One high-confidence protocol-compatible target.
    Exact {
        /// Resolved target.
        target: SymbolId,
        /// Retained calibrated confidence.
        confidence: Confidence,
    },
    /// One or more lower-confidence or ambiguous targets.
    Candidates {
        /// Targets in deterministic score and identity order.
        targets: Vec<SymbolId>,
        /// Number of unique compatible targets before materialization bounds.
        total_count: u64,
        /// Completeness of the materialized target set.
        completeness: CoverageStatus,
        /// Support for the highest-ranked target.
        confidence: Confidence,
    },
}

/// One deterministic decision for a normalized protocol identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignLinkDecision {
    /// Link subject.
    pub subject: RelationEndpoint,
    /// Relation family.
    pub predicate: RelationPredicate,
    /// Typed protocol namespace.
    pub namespace: ForeignLinkNamespace,
    /// Hashed normalized protocol key.
    pub protocol_key: ContentHash,
    /// Exact or ambiguity-preserving result.
    pub outcome: ForeignLinkOutcome,
}

/// Decisions produced for one immutable generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignLinkBatch {
    /// Deterministically sorted link decisions.
    pub decisions: Vec<ForeignLinkDecision>,
}

/// Canonical IR plus the typed link decisions applied to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedForeignLinks {
    /// Validated canonical normalized facts.
    pub document: NormalizedIrDocument,
    /// Exact and candidate outcomes.
    pub batch: ForeignLinkBatch,
}

/// Evidence-keyed cross-language and service linker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignLinkEngine {
    limits: ForeignLinkLimits,
}

impl ForeignLinkEngine {
    /// Creates a foreign linker with checked resource limits.
    #[must_use]
    pub const fn new(limits: ForeignLinkLimits) -> Self {
        Self { limits }
    }

    /// Applies unique high-confidence typed links and retains ambiguous decisions.
    ///
    /// Candidate groups remain in [`ForeignLinkBatch`] and never become exact
    /// relations. Exact relation provenance incorporates the protocol key,
    /// namespace, source evidence, parent provenance, and producing binary.
    ///
    /// # Errors
    ///
    /// Returns [`ForeignLinkError`] for invalid input IR, missing endpoints or
    /// provenance, inconsistent source identity, cancellation, or derived-fact
    /// identity and validation failures.
    pub fn apply(
        &self,
        mut document: NormalizedIrDocument,
        inputs: &[ForeignLinkInput],
        context: ResolverFactContext,
        cancellation: &Cancellation,
    ) -> Result<AppliedForeignLinks, ForeignLinkError> {
        cancellation.check()?;
        if inputs.len() > self.limits.input_limit() {
            return Err(ForeignLinkError::ResourceLimit);
        }
        validate_ir_document(
            &document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .map_err(ForeignLinkError::InvalidDocument)?;
        let mut groups = BTreeMap::<LinkGroupKey, Vec<&ForeignLinkInput>>::new();
        for input in inputs {
            cancellation.check()?;
            validate_input(&document, input)?;
            groups
                .entry(LinkGroupKey {
                    subject: input.subject,
                    predicate: input.predicate,
                    namespace: input.namespace,
                    protocol_key: input.protocol_key,
                })
                .or_default()
                .push(input);
        }

        let mut decisions = Vec::with_capacity(groups.len());
        for (key, mut candidates) in groups {
            cancellation.check()?;
            candidates.sort_unstable_by(|left, right| {
                right
                    .confidence
                    .cmp(&left.confidence)
                    .then_with(|| left.target.cmp(&right.target))
                    .then_with(|| left.provenance.cmp(&right.provenance))
                    .then_with(|| source_key(&left.source).cmp(&source_key(&right.source)))
            });
            let mut unique = BTreeMap::new();
            for candidate in candidates {
                unique.entry(candidate.target).or_insert(candidate);
            }
            let mut candidates = unique.into_values().collect::<Vec<_>>();
            candidates.sort_unstable_by(|left, right| {
                right
                    .confidence
                    .cmp(&left.confidence)
                    .then_with(|| left.target.cmp(&right.target))
            });
            let top = candidates
                .first()
                .copied()
                .ok_or(ForeignLinkError::EmptyGroup)?;
            let outcome = if candidates.len() == 1
                && top.confidence.get() >= MIN_EXACT_FOREIGN_LINK_CONFIDENCE
            {
                let provenance = build_link_provenance(&document, top, context)?;
                let provenance_id = provenance.id;
                if !document
                    .provenance
                    .iter()
                    .any(|existing| existing.id == provenance_id)
                {
                    document.provenance.push(provenance);
                }
                let relation = build_link_relation(&document, top, provenance_id)?;
                if !document
                    .relations
                    .iter()
                    .any(|existing| existing.id == relation.id)
                {
                    document.relations.push(relation);
                }
                ForeignLinkOutcome::Exact {
                    target: top.target,
                    confidence: top.confidence,
                }
            } else {
                let total_count =
                    u64::try_from(candidates.len()).map_err(|_| ForeignLinkError::CountOverflow)?;
                let materialized = candidates.len().min(self.limits.candidate_limit());
                ForeignLinkOutcome::Candidates {
                    targets: candidates
                        .iter()
                        .take(materialized)
                        .map(|candidate| candidate.target)
                        .collect(),
                    total_count,
                    completeness: if materialized < candidates.len() {
                        CoverageStatus::Bounded
                    } else {
                        CoverageStatus::Complete
                    },
                    confidence: top.confidence,
                }
            };
            decisions.push(ForeignLinkDecision {
                subject: key.subject,
                predicate: key.predicate,
                namespace: key.namespace,
                protocol_key: key.protocol_key,
                outcome,
            });
        }

        let document =
            canonicalize_ir_document(document, &IrLimits::default(), &ExtensionSupport::default())
                .map_err(ForeignLinkError::InvalidDocument)?;
        Ok(AppliedForeignLinks {
            document,
            batch: ForeignLinkBatch { decisions },
        })
    }
}

impl Default for ForeignLinkEngine {
    fn default() -> Self {
        Self::new(ForeignLinkLimits::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LinkGroupKey {
    subject: RelationEndpoint,
    predicate: RelationPredicate,
    namespace: ForeignLinkNamespace,
    protocol_key: ContentHash,
}

/// Foreign-link application failure.
#[derive(Debug, thiserror::Error)]
pub enum ForeignLinkError {
    /// The normalized input or output document failed validation.
    #[error("foreign-link document is invalid")]
    InvalidDocument(#[source] rootlight_ir::IrDocumentValidationError),
    /// A caller bypassed the checked constructor with an incompatible input.
    #[error("foreign-link input violates its typed evidence contract")]
    InvalidInput,
    /// Cooperative cancellation stopped linking.
    #[error(transparent)]
    Cancelled(#[from] rootlight_cancel::Cancelled),
    /// A subject, target, source file, or provenance record was absent.
    #[error("foreign-link evidence references an unknown fact")]
    UnknownEvidence,
    /// Source ownership, generation, or content identity differed.
    #[error("foreign-link source does not match the normalized generation")]
    SourceMismatch,
    /// A checked group unexpectedly contained no candidates.
    #[error("foreign-link candidate group is empty")]
    EmptyGroup,
    /// Typed evidence exceeded an operation or candidate materialization bound.
    #[error("foreign-link evidence exceeds its resource limit")]
    ResourceLimit,
    /// A candidate count was not representable in the normalized count domain.
    #[error("foreign-link candidate count is not representable")]
    CountOverflow,
    /// Derived producer metadata was invalid.
    #[error("foreign-link producer identity is invalid")]
    InvalidProducer(#[source] rootlight_ir::IrValidationError),
    /// A derived provenance or relation identity could not be created.
    #[error("foreign-link fact identity could not be derived")]
    FactIdentity(#[source] rootlight_ir::FactIdentityRecipeError),
}

fn validate_input(
    document: &NormalizedIrDocument,
    input: &ForeignLinkInput,
) -> Result<(), ForeignLinkError> {
    if !input.namespace.supports(input.predicate) || input.confidence.get() == 0 {
        return Err(ForeignLinkError::InvalidInput);
    }
    if !endpoint_exists(document, input.subject)
        || !document
            .entities
            .iter()
            .any(|entity| entity.id == input.target)
        || !document
            .provenance
            .iter()
            .any(|record| record.id == input.provenance)
    {
        return Err(ForeignLinkError::UnknownEvidence);
    }
    let Some(file) = document
        .files
        .iter()
        .find(|file| file.id == input.source.span().file())
    else {
        return Err(ForeignLinkError::UnknownEvidence);
    };
    if input.source.repository() != document.repository
        || input.source.generation() != document.generation
        || input.source.content_hash() != file.content_hash
        || input.source.span().end_byte() > file.byte_length
    {
        return Err(ForeignLinkError::SourceMismatch);
    }
    Ok(())
}

fn source_key(source: &SourceRef) -> (rootlight_ids::FileId, u64, u64, ContentHash) {
    (
        source.span().file(),
        source.span().start_byte(),
        source.span().end_byte(),
        source.content_hash(),
    )
}

fn endpoint_exists(document: &NormalizedIrDocument, endpoint: RelationEndpoint) -> bool {
    match endpoint {
        RelationEndpoint::Repository(repository) => repository == document.repository,
        RelationEndpoint::File(file) => document.files.iter().any(|record| record.id == file),
        RelationEndpoint::Entity(entity) => {
            document.entities.iter().any(|record| record.id == entity)
        }
        RelationEndpoint::Occurrence(occurrence) => document
            .occurrences
            .iter()
            .any(|record| record.id == occurrence),
    }
}

fn build_link_provenance(
    document: &NormalizedIrDocument,
    input: &ForeignLinkInput,
    context: ResolverFactContext,
) -> Result<ProvenanceRecord, ForeignLinkError> {
    let parent = document
        .provenance
        .iter()
        .find(|record| record.id == input.provenance)
        .ok_or(ForeignLinkError::UnknownEvidence)?;
    let rule = format!(
        "foreign-v1.{}.{}",
        input.namespace.label(),
        predicate_label(input.predicate)
    );
    let mut configuration = Vec::new();
    configuration.extend_from_slice(rule.as_bytes());
    configuration.extend_from_slice(input.protocol_key.as_bytes());
    let producer = ProducerIdentity::new(
        "rootlight-resolve-foreign",
        env!("CARGO_PKG_VERSION"),
        content_hash(&configuration),
    )
    .map_err(ForeignLinkError::InvalidProducer)?;
    let mut build_bytes = Vec::with_capacity(64);
    build_bytes.extend_from_slice(parent.build_context.digest().as_bytes());
    build_bytes.extend_from_slice(input.protocol_key.as_bytes());
    let mut record = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        producer_kind: ProducerKind::Rule,
        producer,
        binary_digest: context.binary_digest(),
        frontend_version: Some("foreign-v1".to_owned()),
        language: parent.language.clone(),
        tier: parent.tier,
        build_context: BuildContextIdentity::new(content_hash(&build_bytes)),
        input_sources: vec![input.source.clone()],
        evidence_sources: vec![input.source.clone()],
        derivation_parents: vec![FactRef::Fact(input.provenance)],
        rule: Some(rule),
    };
    record.id = derive_provenance_record_id(&record).map_err(ForeignLinkError::FactIdentity)?;
    Ok(record)
}

fn build_link_relation(
    document: &NormalizedIrDocument,
    input: &ForeignLinkInput,
    provenance: FactId,
) -> Result<RelationRecord, ForeignLinkError> {
    let mut derivation = vec![FactRef::Entity(input.target)];
    if let Some(subject) = endpoint_fact(input.subject) {
        derivation.push(subject);
    }
    derivation.sort_unstable();
    derivation.dedup();
    let mut record = RelationRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        subject: input.subject,
        predicate: input.predicate,
        object: RelationEndpoint::Entity(input.target),
        confidence: input.confidence,
        evidence_kind: EvidenceKind::Derived,
        provenance,
        evidence: FactEvidence {
            source: Some(input.source.clone()),
            derivation,
        },
    };
    record.id = derive_relation_record_id(&record).map_err(ForeignLinkError::FactIdentity)?;
    Ok(record)
}

const fn endpoint_fact(endpoint: RelationEndpoint) -> Option<FactRef> {
    match endpoint {
        RelationEndpoint::Repository(_) => None,
        RelationEndpoint::File(file) => Some(FactRef::File(file)),
        RelationEndpoint::Entity(entity) => Some(FactRef::Entity(entity)),
        RelationEndpoint::Occurrence(occurrence) => Some(FactRef::Fact(occurrence)),
    }
}

const fn predicate_label(predicate: RelationPredicate) -> &'static str {
    match predicate {
        RelationPredicate::BindsTo => "binds_to",
        RelationPredicate::CallsForeign => "calls_foreign",
        RelationPredicate::CallsRoute => "calls_route",
        RelationPredicate::ServesRoute => "serves_route",
        RelationPredicate::Publishes => "publishes",
        RelationPredicate::Consumes => "consumes",
        RelationPredicate::ReadsTable => "reads_table",
        RelationPredicate::WritesTable => "writes_table",
        RelationPredicate::Tests => "tests",
        RelationPredicate::DependsOn => "depends_on",
        RelationPredicate::GeneratedFrom => "generated_from",
        _ => "unsupported",
    }
}
