//! Deterministic cross-repository links from bounded declarative evidence.
//!
//! Link construction performs no discovery, source reads, DNS, service calls,
//! or repository access. Ambiguous matches remain explicit candidate sets.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId};
use rootlight_ir::Confidence;
use serde::{Deserialize, Serialize};

use crate::{WorkspaceSnapshot, WorkspaceSnapshotId, identity::identity_hash};

const LINK_SCHEMA_VERSION: u16 = 1;
const HARD_MAX_DECLARATIONS: usize = 200_000;
const HARD_MAX_LINKS: usize = 100_000;
const HARD_MAX_CANDIDATES_PER_LINK: usize = 64;
const HARD_MAX_KEY_BYTES: usize = 1_024;
const HARD_MAX_CAVEATS_PER_DECLARATION: usize = 32;

/// Identity of one immutable link overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkOverlayId(ContentHash);

impl LinkOverlayId {
    /// Creates an overlay identity from its canonical content hash.
    #[must_use]
    pub const fn from_hash(hash: ContentHash) -> Self {
        Self(hash)
    }

    /// Returns the canonical overlay content hash.
    #[must_use]
    pub const fn as_hash(self) -> ContentHash {
        self.0
    }
}

/// Exact fact endpoint pinned to repository and generation identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFactRef {
    repository: RepositoryId,
    generation: GenerationId,
    fact: FactId,
}

impl WorkspaceFactRef {
    /// Creates an exact cross-repository endpoint.
    #[must_use]
    pub const fn new(repository: RepositoryId, generation: GenerationId, fact: FactId) -> Self {
        Self {
            repository,
            generation,
            fact,
        }
    }

    /// Returns the endpoint repository.
    #[must_use]
    pub const fn repository(self) -> RepositoryId {
        self.repository
    }

    /// Returns the endpoint generation.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the evidence fact identity.
    #[must_use]
    pub const fn fact(self) -> FactId {
        self.fact
    }
}

/// Closed cross-repository declaration family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkKind {
    /// Declared package dependency.
    Package,
    /// HTTP method and route template.
    Http,
    /// RPC service and method identity.
    Rpc,
    /// Messaging channel or topic identity.
    Messaging,
    /// Database object or operation identity.
    Database,
}

/// Provider or consumer side of one declarative key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LinkDirection {
    /// Declares a target that can satisfy consumers.
    Provider,
    /// Declares a dependency or service use.
    Consumer,
}

/// Explicit caveat that lowers certainty without discarding a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkCaveat {
    /// Base URL or authority depends on deployment environment.
    EnvironmentDependentBase,
    /// A route contains a dynamic segment.
    DynamicRoute,
    /// Schema or generated configuration was unavailable.
    MissingSchema,
    /// Link depends on generated configuration.
    GeneratedConfiguration,
    /// More than one provider satisfies the normalized key.
    MultipleCandidates,
    /// Candidate set was truncated by an explicit limit.
    CandidateLimit,
}

/// Canonical bounded package or service key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ServiceKey(String);

impl ServiceKey {
    /// Canonicalizes a named package, RPC, messaging, or database key.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::InvalidKey`] for HTTP use, empty or oversized
    /// input, control bytes, whitespace, or unsupported punctuation.
    pub fn named(kind: LinkKind, value: &str) -> Result<Self, LinkError> {
        if kind == LinkKind::Http {
            return Err(LinkError::InvalidKey);
        }
        let value = value.trim();
        if value.is_empty()
            || value.len() > HARD_MAX_KEY_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
            })
        {
            return Err(LinkError::InvalidKey);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// Creates a package or schema key from an immutable identity.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::InvalidKey`] for HTTP use.
    pub fn immutable(kind: LinkKind, identity: ContentHash) -> Result<Self, LinkError> {
        if kind == LinkKind::Http {
            return Err(LinkError::InvalidKey);
        }
        Ok(Self(identity.to_string()))
    }

    /// Canonicalizes an HTTP method and route without resolving an authority.
    ///
    /// Dynamic `{name}` and `:name` segments normalize to `{}` so candidate
    /// ambiguity is preserved independently of framework parameter spelling.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::InvalidKey`] for an invalid method, absolute URL,
    /// query, fragment, parent segment, control byte, or oversized route.
    pub fn http(method: &str, path: &str) -> Result<Self, LinkError> {
        if method.is_empty()
            || method.len() > 16
            || !method.bytes().all(|byte| byte.is_ascii_alphabetic())
            || !path.starts_with('/')
            || path.len() > HARD_MAX_KEY_BYTES.saturating_sub(17)
            || path
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'?' | b'#' | b'\\'))
        {
            return Err(LinkError::InvalidKey);
        }
        let mut normalized = String::with_capacity(path.len());
        normalized.push('/');
        let mut first = true;
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            if matches!(segment, "." | "..") {
                return Err(LinkError::InvalidKey);
            }
            if !first {
                normalized.push('/');
            }
            first = false;
            if (segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2)
                || (segment.starts_with(':') && segment.len() > 1)
            {
                normalized.push_str("{}");
            } else if segment.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'_' | b'-' | b'~' | b'%' | b'+' | b'@')
            }) {
                normalized.push_str(segment);
            } else {
                return Err(LinkError::InvalidKey);
            }
        }
        let key = format!("{} {normalized}", method.to_ascii_uppercase());
        if key.len() > HARD_MAX_KEY_BYTES {
            return Err(LinkError::InvalidKey);
        }
        Ok(Self(key))
    }

    /// Returns the canonical key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One provider or consumer declaration from exact evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDeclaration {
    endpoint: WorkspaceFactRef,
    kind: LinkKind,
    direction: LinkDirection,
    key: ServiceKey,
    confidence: Confidence,
    caveats: Vec<LinkCaveat>,
}

impl LinkDeclaration {
    /// Creates a declaration with no caveat.
    #[must_use]
    pub const fn new(
        endpoint: WorkspaceFactRef,
        kind: LinkKind,
        direction: LinkDirection,
        key: ServiceKey,
        confidence: Confidence,
    ) -> Self {
        Self {
            endpoint,
            kind,
            direction,
            key,
            confidence,
            caveats: Vec::new(),
        }
    }

    /// Adds one explicit caveat for canonicalization during linking.
    #[must_use]
    pub fn with_caveat(mut self, caveat: LinkCaveat) -> Self {
        self.caveats.push(caveat);
        self
    }
}

/// Linker capacities no broader than hard process limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkLimits {
    max_declarations: usize,
    max_links: usize,
    max_candidates_per_link: usize,
    max_key_bytes: usize,
    max_caveats_per_declaration: usize,
}

impl LinkLimits {
    /// Creates a linker limit policy.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::InvalidLimits`] when a value is zero or exceeds a
    /// hard process ceiling.
    pub fn new(
        max_declarations: usize,
        max_links: usize,
        max_candidates_per_link: usize,
        max_key_bytes: usize,
        max_caveats_per_declaration: usize,
    ) -> Result<Self, LinkError> {
        if max_declarations == 0
            || max_declarations > HARD_MAX_DECLARATIONS
            || max_links == 0
            || max_links > HARD_MAX_LINKS
            || max_candidates_per_link == 0
            || max_candidates_per_link > HARD_MAX_CANDIDATES_PER_LINK
            || max_key_bytes == 0
            || max_key_bytes > HARD_MAX_KEY_BYTES
            || max_caveats_per_declaration == 0
            || max_caveats_per_declaration > HARD_MAX_CAVEATS_PER_DECLARATION
        {
            return Err(LinkError::InvalidLimits);
        }
        Ok(Self {
            max_declarations,
            max_links,
            max_candidates_per_link,
            max_key_bytes,
            max_caveats_per_declaration,
        })
    }
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            max_declarations: 20_000,
            max_links: 10_000,
            max_candidates_per_link: 16,
            max_key_bytes: 512,
            max_caveats_per_declaration: 16,
        }
    }
}

/// One explicit candidate target with provenance and confidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrossRepositoryCandidate {
    endpoint: WorkspaceFactRef,
    confidence: Confidence,
    caveats: Vec<LinkCaveat>,
}

impl CrossRepositoryCandidate {
    /// Returns the exact provider endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> WorkspaceFactRef {
        self.endpoint
    }

    /// Returns calibrated fixed-point confidence.
    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    /// Returns explicit caveats in canonical order.
    #[must_use]
    pub fn caveats(&self) -> &[LinkCaveat] {
        &self.caveats
    }
}

/// One consumer and its bounded cross-repository candidate set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrossRepositoryLink {
    id: ContentHash,
    kind: LinkKind,
    key: ServiceKey,
    consumer: WorkspaceFactRef,
    candidates: Vec<CrossRepositoryCandidate>,
    candidate_count: usize,
    truncated: bool,
}

impl CrossRepositoryLink {
    /// Returns the deterministic link identity.
    #[must_use]
    pub const fn id(&self) -> ContentHash {
        self.id
    }

    /// Returns the relation family.
    #[must_use]
    pub const fn kind(&self) -> LinkKind {
        self.kind
    }

    /// Returns the normalized declarative key.
    #[must_use]
    pub const fn key(&self) -> &ServiceKey {
        &self.key
    }

    /// Returns the exact consumer endpoint.
    #[must_use]
    pub const fn consumer(&self) -> WorkspaceFactRef {
        self.consumer
    }

    /// Returns retained candidates in canonical order.
    #[must_use]
    pub fn candidates(&self) -> &[CrossRepositoryCandidate] {
        &self.candidates
    }

    /// Returns the complete candidate count before truncation.
    #[must_use]
    pub const fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    /// Reports whether an explicit candidate bound truncated the set.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Immutable links built against one exact workspace snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkOverlay {
    schema_version: u16,
    id: LinkOverlayId,
    snapshot: WorkspaceSnapshotId,
    declarations: usize,
    unresolved_consumers: usize,
    links: Vec<CrossRepositoryLink>,
}

impl LinkOverlay {
    /// Returns the link schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Returns the overlay identity.
    #[must_use]
    pub const fn id(&self) -> LinkOverlayId {
        self.id
    }

    /// Returns the exact owning snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> WorkspaceSnapshotId {
        self.snapshot
    }

    /// Returns the validated declaration count.
    #[must_use]
    pub const fn declarations(&self) -> usize {
        self.declarations
    }

    /// Returns consumers with no cross-repository provider.
    #[must_use]
    pub const fn unresolved_consumers(&self) -> usize {
        self.unresolved_consumers
    }

    /// Returns links in deterministic identity order.
    #[must_use]
    pub fn links(&self) -> &[CrossRepositoryLink] {
        &self.links
    }
}

/// Builds exact package and service candidate sets for one immutable snapshot.
///
/// # Errors
///
/// Returns [`LinkError`] for invalid endpoints, duplicates, resource limits,
/// identity accounting, or cancellation.
pub fn build_link_overlay(
    snapshot: &WorkspaceSnapshot,
    mut declarations: Vec<LinkDeclaration>,
    limits: LinkLimits,
    cancellation: &Cancellation,
) -> Result<LinkOverlay, LinkError> {
    cancellation.check()?;
    if declarations.len() > limits.max_declarations {
        return Err(LinkError::DeclarationLimit);
    }
    for declaration in &mut declarations {
        validate_endpoint(snapshot, declaration.endpoint)?;
        if declaration.key.as_str().len() > limits.max_key_bytes {
            return Err(LinkError::KeyLimit);
        }
        declaration.caveats.sort();
        declaration.caveats.dedup();
        if declaration.caveats.len() > limits.max_caveats_per_declaration {
            return Err(LinkError::CaveatLimit);
        }
    }
    declarations.sort_by(|left, right| declaration_key(left).cmp(&declaration_key(right)));
    if declarations
        .windows(2)
        .any(|pair| declaration_key(&pair[0]) == declaration_key(&pair[1]))
    {
        return Err(LinkError::DuplicateDeclaration);
    }

    let mut providers: BTreeMap<(LinkKind, ServiceKey), Vec<&LinkDeclaration>> = BTreeMap::new();
    for declaration in &declarations {
        if declaration.direction == LinkDirection::Provider {
            providers
                .entry((declaration.kind, declaration.key.clone()))
                .or_default()
                .push(declaration);
        }
    }
    let mut unresolved_consumers = 0_usize;
    let mut links = Vec::new();
    for (index, consumer) in declarations
        .iter()
        .filter(|declaration| declaration.direction == LinkDirection::Consumer)
        .enumerate()
    {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        let matching = providers
            .get(&(consumer.kind, consumer.key.clone()))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let cross_repository = matching
            .iter()
            .copied()
            .filter(|provider| provider.endpoint.repository != consumer.endpoint.repository)
            .collect::<Vec<_>>();
        if cross_repository.is_empty() {
            unresolved_consumers = unresolved_consumers
                .checked_add(1)
                .ok_or(LinkError::Accounting)?;
            continue;
        }
        if links.len() >= limits.max_links {
            return Err(LinkError::LinkLimit);
        }
        let candidate_count = cross_repository.len();
        let truncated = candidate_count > limits.max_candidates_per_link;
        let mut candidates =
            Vec::with_capacity(candidate_count.min(limits.max_candidates_per_link));
        for provider in cross_repository
            .into_iter()
            .take(limits.max_candidates_per_link)
        {
            let mut caveats = consumer
                .caveats
                .iter()
                .chain(&provider.caveats)
                .copied()
                .collect::<BTreeSet<_>>();
            if candidate_count > 1 {
                caveats.insert(LinkCaveat::MultipleCandidates);
            }
            if truncated {
                caveats.insert(LinkCaveat::CandidateLimit);
            }
            let confidence =
                Confidence::new(consumer.confidence.get().min(provider.confidence.get()))
                    .map_err(|_| LinkError::InvalidConfidence)?;
            candidates.push(CrossRepositoryCandidate {
                endpoint: provider.endpoint,
                confidence,
                caveats: caveats.into_iter().collect(),
            });
        }
        let id = derive_link_id(consumer, &candidates, candidate_count, truncated);
        links.push(CrossRepositoryLink {
            id,
            kind: consumer.kind,
            key: consumer.key.clone(),
            consumer: consumer.endpoint,
            candidates,
            candidate_count,
            truncated,
        });
    }
    links.sort_by_key(|link| link.id);
    cancellation.check()?;
    let overlay_id = derive_overlay_id(
        snapshot.id(),
        declarations.len(),
        unresolved_consumers,
        &links,
    );
    Ok(LinkOverlay {
        schema_version: LINK_SCHEMA_VERSION,
        id: overlay_id,
        snapshot: snapshot.id(),
        declarations: declarations.len(),
        unresolved_consumers,
        links,
    })
}

fn validate_endpoint(
    snapshot: &WorkspaceSnapshot,
    endpoint: WorkspaceFactRef,
) -> Result<(), LinkError> {
    match snapshot.generation_for(endpoint.repository) {
        Some(generation) if generation == endpoint.generation => Ok(()),
        _ => Err(LinkError::EndpointOutsideSnapshot),
    }
}

fn declaration_key(
    declaration: &LinkDeclaration,
) -> (LinkKind, &ServiceKey, LinkDirection, WorkspaceFactRef) {
    (
        declaration.kind,
        &declaration.key,
        declaration.direction,
        declaration.endpoint,
    )
}

fn derive_link_id(
    consumer: &LinkDeclaration,
    candidates: &[CrossRepositoryCandidate],
    candidate_count: usize,
    truncated: bool,
) -> ContentHash {
    let mut endpoints = Vec::with_capacity(candidates.len().saturating_mul(62).saturating_add(32));
    endpoints.extend_from_slice(consumer.endpoint.repository.as_bytes());
    endpoints.extend_from_slice(consumer.endpoint.generation.as_bytes());
    endpoints.extend_from_slice(consumer.endpoint.fact.as_bytes());
    endpoints.extend_from_slice(
        &u64::try_from(candidate_count)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    endpoints.push(u8::from(truncated));
    for candidate in candidates {
        endpoints.extend_from_slice(candidate.endpoint.repository.as_bytes());
        endpoints.extend_from_slice(candidate.endpoint.generation.as_bytes());
        endpoints.extend_from_slice(candidate.endpoint.fact.as_bytes());
        endpoints.extend_from_slice(&candidate.confidence.get().to_be_bytes());
        for caveat in &candidate.caveats {
            endpoints.push(caveat_code(*caveat));
        }
    }
    identity_hash(
        b"rootlight/cross-repository-link/v1",
        &[
            &[kind_code(consumer.kind)],
            consumer.key.as_str().as_bytes(),
            &endpoints,
        ],
    )
}

fn derive_overlay_id(
    snapshot: WorkspaceSnapshotId,
    declarations: usize,
    unresolved_consumers: usize,
    links: &[CrossRepositoryLink],
) -> LinkOverlayId {
    let mut content = Vec::with_capacity(links.len().saturating_mul(32).saturating_add(16));
    content.extend_from_slice(
        &u64::try_from(declarations)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    content.extend_from_slice(
        &u64::try_from(unresolved_consumers)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for link in links {
        content.extend_from_slice(link.id.as_bytes());
    }
    LinkOverlayId::from_hash(identity_hash(
        b"rootlight/cross-link-overlay/v1",
        &[snapshot.as_hash().as_bytes(), &content],
    ))
}

const fn kind_code(kind: LinkKind) -> u8 {
    match kind {
        LinkKind::Package => 0,
        LinkKind::Http => 1,
        LinkKind::Rpc => 2,
        LinkKind::Messaging => 3,
        LinkKind::Database => 4,
    }
}

const fn caveat_code(caveat: LinkCaveat) -> u8 {
    match caveat {
        LinkCaveat::EnvironmentDependentBase => 0,
        LinkCaveat::DynamicRoute => 1,
        LinkCaveat::MissingSchema => 2,
        LinkCaveat::GeneratedConfiguration => 3,
        LinkCaveat::MultipleCandidates => 4,
        LinkCaveat::CandidateLimit => 5,
    }
}

/// Invalid, unbounded, inconsistent, or cancelled link construction.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LinkError {
    /// Limit policy is zero or broader than a hard ceiling.
    #[error("cross-repository link limits are invalid")]
    InvalidLimits,
    /// Service key is malformed or noncanonical.
    #[error("cross-repository service key is invalid")]
    InvalidKey,
    /// Declaration bound is exhausted.
    #[error("cross-repository declaration limit exceeded")]
    DeclarationLimit,
    /// Link bound is exhausted.
    #[error("cross-repository link limit exceeded")]
    LinkLimit,
    /// Per-link key bound is exhausted.
    #[error("cross-repository key limit exceeded")]
    KeyLimit,
    /// Per-declaration caveat bound is exhausted.
    #[error("cross-repository caveat limit exceeded")]
    CaveatLimit,
    /// Declaration endpoint is absent from the exact snapshot.
    #[error("cross-repository endpoint is outside the workspace snapshot")]
    EndpointOutsideSnapshot,
    /// Exact declaration was repeated.
    #[error("cross-repository declaration is duplicated")]
    DuplicateDeclaration,
    /// Fixed-point confidence could not be represented.
    #[error("cross-repository confidence is invalid")]
    InvalidConfidence,
    /// Bounded integer accounting overflowed.
    #[error("cross-repository resource accounting overflowed")]
    Accounting,
    /// Cooperative cancellation won.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}
