//! Canonical identity for one generation-pinned `context.pack` request.

use std::collections::BTreeSet;

use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use rootlight_mcp_contract::{
    RepositorySelector,
    context::{
        ContextPackId, ContextPackInput, ContextSection, Diversity, OBJECTIVE_ROLE_POLICY_VERSION,
        PLANNER_VERSION, SourcePolicy,
    },
    vertical::{ContinuationCursor, GenerationSelector, ResponseProfile},
};
use unicode_normalization::UnicodeNormalization;

use crate::context_pack::EvidenceRole;

/// Maximum distinct typed seeds admitted by one context-pack request.
pub const MAX_CONTEXT_PACK_SEEDS: usize = 16;

const DEFAULT_MIN_CONFIDENCE: u16 = 700;
const DEFAULT_SOURCE_POLICY: SourcePolicy = SourcePolicy::ReferencesOnly;
const DEFAULT_DIVERSITY: Diversity = Diversity::Balanced;
const DEFAULT_RESPONSE_PROFILE: ResponseProfile = ResponseProfile::Compact;
const ALL_CONTEXT_SECTIONS: [ContextSection; 9] = [
    ContextSection::Architecture,
    ContextSection::Definitions,
    ContextSection::Callers,
    ContextSection::Callees,
    ContextSection::Types,
    ContextSection::Tests,
    ContextSection::History,
    ContextSection::Source,
    ContextSection::Risks,
];

/// Normalized task objective used by context evidence planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPackObjective {
    /// Fix a defect in target behavior.
    BugFix,
    /// Restructure existing behavior without changing its contract.
    Refactor,
    /// Explain existing behavior.
    Explanation,
    /// Move behavior to a new API, platform, or representation.
    Migration,
    /// Review behavior, risk, or security.
    Review,
}

impl ContextPackObjective {
    pub(crate) fn from_normalized_task(task: &str) -> Self {
        if task.contains("fix")
            || task.contains("bug")
            || task.contains("error")
            || task.contains("crash")
            || task.contains("broken")
        {
            Self::BugFix
        } else if task.contains("refactor")
            || task.contains("restructure")
            || task.contains("simplify")
            || task.contains("clean")
        {
            Self::Refactor
        } else if task.contains("migrat")
            || task.contains("upgrade")
            || task.contains("port to")
            || task.contains("move to")
        {
            Self::Migration
        } else if task.contains("review") || task.contains("audit") || task.contains("security") {
            Self::Review
        } else {
            Self::Explanation
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::BugFix => 0,
            Self::Refactor => 1,
            Self::Explanation => 2,
            Self::Migration => 3,
            Self::Review => 4,
        }
    }
}

/// Category-preserving normalized context-pack seeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalContextSeeds {
    symbols: Vec<SymbolId>,
    paths: Vec<String>,
    routes: Vec<String>,
    tests: Vec<SymbolId>,
    located: Option<ContinuationCursor>,
    change: Option<String>,
    plan: Option<String>,
}

impl CanonicalContextSeeds {
    /// Returns canonical symbol anchors.
    #[must_use]
    pub fn symbols(&self) -> &[SymbolId] {
        &self.symbols
    }

    /// Returns canonical repository-relative path anchors.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Returns canonical route or service-name anchors.
    #[must_use]
    pub fn routes(&self) -> &[String] {
        &self.routes
    }

    /// Returns canonical test anchors.
    #[must_use]
    pub fn tests(&self) -> &[SymbolId] {
        &self.tests
    }

    /// Returns the exact bounded located-result handle.
    #[must_use]
    pub const fn located(&self) -> Option<&ContinuationCursor> {
        self.located.as_ref()
    }

    /// Returns the exact bounded change descriptor.
    #[must_use]
    pub fn change(&self) -> Option<&str> {
        self.change.as_deref()
    }

    /// Returns the exact bounded plan handle.
    #[must_use]
    pub fn plan(&self) -> Option<&str> {
        self.plan.as_deref()
    }

    /// Returns the number of distinct typed anchors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.symbols
            .len()
            .saturating_add(self.paths.len())
            .saturating_add(self.routes.len())
            .saturating_add(self.tests.len())
            .saturating_add(usize::from(self.located.is_some()))
            .saturating_add(usize::from(self.change.is_some()))
            .saturating_add(usize::from(self.plan.is_some()))
    }

    /// Returns whether no supported anchor remains after normalization.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the deduplicated symbol identities needed by the current
    /// definition provider while retaining categories in canonical identity.
    #[must_use]
    pub fn retrieval_symbols(&self) -> Vec<SymbolId> {
        self.symbols
            .iter()
            .chain(&self.tests)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Failure to construct a canonical request after identity resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalContextPackRequestError {
    /// The exact repository does not match an explicit public selector.
    #[error("context-pack repository identity does not match the request")]
    RepositoryMismatch,
    /// The exact generation does not match an explicit public selector.
    #[error("context-pack generation identity does not match the request")]
    GenerationMismatch,
    /// Task normalization removed all content.
    #[error("context-pack task is empty after normalization")]
    EmptyTask,
    /// No supported seed remains after normalization.
    #[error("context-pack request has no supported seed")]
    EmptySeeds,
    /// The distinct typed-seed ceiling was exceeded.
    #[error("context-pack request exceeds the typed-seed ceiling")]
    TooManySeeds,
    /// A field is malformed or outside its canonical byte bounds.
    #[error("context-pack field does not have a bounded canonical representation")]
    InvalidField(&'static str),
    /// A field is intentionally outside the current canonical request family.
    #[error("context-pack field is not supported by canonical request identity")]
    UnsupportedField(&'static str),
}

/// Complete normalized identity for one exact context-pack request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalContextPackRequest {
    repository: RepositoryId,
    generation: GenerationId,
    task: String,
    objective: ContextPackObjective,
    seeds: CanonicalContextSeeds,
    token_budget: u16,
    source_policy: SourcePolicy,
    sections: Vec<ContextSection>,
    diversity: Diversity,
    min_confidence: u16,
    response_profile: ResponseProfile,
    digest: [u8; 32],
}

impl CanonicalContextPackRequest {
    /// Normalizes one public request after its repository and generation have
    /// been resolved exactly.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalContextPackRequestError`] for mismatched identity,
    /// empty normalized input, an oversized seed set, or a seed whose opaque
    /// identity cannot yet be resolved.
    pub fn new(
        input: &ContextPackInput,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<Self, CanonicalContextPackRequestError> {
        if matches!(
            &input.repository,
            RepositorySelector::ById(selector) if selector.repository_id != repository
        ) {
            return Err(CanonicalContextPackRequestError::RepositoryMismatch);
        }
        if matches!(
            input.generation.as_ref(),
            Some(GenerationSelector::Explicit(expected)) if *expected != generation
        ) {
            return Err(CanonicalContextPackRequestError::GenerationMismatch);
        }
        let task = normalize_task(&input.task);
        if task.is_empty() {
            return Err(CanonicalContextPackRequestError::EmptyTask);
        }
        let objective = ContextPackObjective::from_normalized_task(&task);
        let seeds = CanonicalContextSeeds {
            symbols: sorted_unique(input.seeds.symbols.as_deref().unwrap_or_default()),
            paths: normalized_paths(input.seeds.paths.as_deref().unwrap_or_default())?,
            routes: normalized_names(input.seeds.routes.as_deref().unwrap_or_default(), "routes")?,
            tests: sorted_unique(input.seeds.tests.as_deref().unwrap_or_default()),
            located: input.seeds.located.clone(),
            change: bounded_opaque(input.seeds.change.as_deref(), "change")?,
            plan: bounded_opaque(input.seeds.plan.as_deref(), "plan")?,
        };
        if seeds.is_empty() {
            return Err(CanonicalContextPackRequestError::EmptySeeds);
        }
        if seeds.len() > MAX_CONTEXT_PACK_SEEDS {
            return Err(CanonicalContextPackRequestError::TooManySeeds);
        }

        let source_policy = input.source_policy.unwrap_or(DEFAULT_SOURCE_POLICY);
        let mut sections = input
            .sections
            .clone()
            .unwrap_or_else(|| ALL_CONTEXT_SECTIONS.to_vec());
        sections.sort_unstable_by_key(|section| context_section_tag(*section));
        sections.dedup_by_key(|section| context_section_tag(*section));
        if sections.is_empty() {
            return Err(CanonicalContextPackRequestError::InvalidField("sections"));
        }
        let source_policy_is_compatible = match source_policy {
            SourcePolicy::ReferencesOnly => true,
            SourcePolicy::Signatures => sections.iter().any(|section| {
                matches!(section, ContextSection::Definitions | ContextSection::Types)
            }),
            SourcePolicy::FocusedSnippets | SourcePolicy::EvidenceHeavy => {
                sections.contains(&ContextSection::Source)
            }
        };
        if !source_policy_is_compatible {
            return Err(CanonicalContextPackRequestError::InvalidField(
                "source_policy",
            ));
        }
        let diversity = input.diversity.unwrap_or(DEFAULT_DIVERSITY);
        let min_confidence = input.min_confidence.unwrap_or(DEFAULT_MIN_CONFIDENCE);
        let response_profile = input.response_profile.unwrap_or(DEFAULT_RESPONSE_PROFILE);

        let mut request = Self {
            repository,
            generation,
            task,
            objective,
            seeds,
            token_budget: input.token_budget,
            source_policy,
            sections,
            diversity,
            min_confidence,
            response_profile,
            digest: [0; 32],
        };
        request.digest = request.compute_digest();
        Ok(request)
    }

    /// Returns the exact repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the exact generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the normalized source-free task.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Returns the normalized task objective.
    #[must_use]
    pub const fn objective(&self) -> ContextPackObjective {
        self.objective
    }

    /// Returns the normalized typed seeds.
    #[must_use]
    pub const fn seeds(&self) -> &CanonicalContextSeeds {
        &self.seeds
    }

    /// Returns the requested hard token budget.
    #[must_use]
    pub const fn token_budget(&self) -> u16 {
        self.token_budget
    }

    /// Returns the canonical source inclusion policy.
    #[must_use]
    pub const fn source_policy(&self) -> SourcePolicy {
        self.source_policy
    }

    /// Returns canonical evidence sections in stable order.
    #[must_use]
    pub fn sections(&self) -> &[ContextSection] {
        &self.sections
    }

    /// Returns the canonical diversity policy.
    #[must_use]
    pub const fn diversity(&self) -> Diversity {
        self.diversity
    }

    /// Returns the minimum admitted evidence confidence.
    #[must_use]
    pub const fn min_confidence(&self) -> u16 {
        self.min_confidence
    }

    /// Returns the representation-only response profile.
    #[must_use]
    pub const fn response_profile(&self) -> ResponseProfile {
        self.response_profile
    }

    /// Returns evidence roles selected by the canonical section set.
    #[must_use]
    pub fn requested_roles(&self) -> Vec<EvidenceRole> {
        roles_for_sections(&self.sections)
    }

    /// Returns the complete domain-separated request digest.
    #[must_use]
    pub const fn digest_bytes(&self) -> [u8; 32] {
        self.digest
    }

    /// Returns the public source-free request digest evidence.
    #[must_use]
    pub fn request_digest(&self) -> String {
        format!("ctxreq1_{}", blake3::Hash::from_bytes(self.digest).to_hex())
    }

    /// Returns the stable context-pack identifier derived from this request.
    #[must_use]
    pub fn pack_id(&self) -> ContextPackId {
        ContextPackId::new(format!("pack1_{}", short_digest(self.digest)))
    }

    /// Returns the explain fingerprint bound to the same canonical identity.
    #[must_use]
    pub fn plan_fingerprint(&self) -> String {
        format!("plan1_{}", short_digest(self.digest))
    }

    fn compute_digest(&self) -> [u8; 32] {
        let mut hasher =
            blake3::Hasher::new_derive_key("rootlight.context-pack.canonical-request.v1");
        hasher.update(&PLANNER_VERSION.to_le_bytes());
        hasher.update(&OBJECTIVE_ROLE_POLICY_VERSION.to_le_bytes());
        hash_bytes(&mut hasher, self.repository.as_bytes());
        hash_bytes(&mut hasher, self.generation.as_bytes());
        hasher.update(&[self.objective.tag()]);
        hash_bytes(&mut hasher, self.task.as_bytes());
        hash_symbols(&mut hasher, 0, &self.seeds.symbols);
        hash_strings(&mut hasher, 1, &self.seeds.paths);
        hash_strings(&mut hasher, 2, &self.seeds.routes);
        hash_symbols(&mut hasher, 3, &self.seeds.tests);
        hash_optional_bytes(
            &mut hasher,
            4,
            self.seeds.located.as_ref().map(ContinuationCursor::as_str),
        );
        hash_optional_bytes(&mut hasher, 5, self.seeds.change.as_deref());
        hash_optional_bytes(&mut hasher, 6, self.seeds.plan.as_deref());
        hasher.update(&self.token_budget.to_le_bytes());
        hasher.update(&[source_policy_tag(self.source_policy)]);
        hash_count(&mut hasher, self.sections.len());
        for section in &self.sections {
            hasher.update(&[context_section_tag(*section)]);
        }
        hasher.update(&[diversity_tag(self.diversity)]);
        hasher.update(&self.min_confidence.to_le_bytes());
        hasher.update(&[response_profile_tag(self.response_profile)]);
        *hasher.finalize().as_bytes()
    }
}

/// Normalizes canonical composition, case, and every Unicode whitespace run.
#[must_use]
pub fn normalize_task(value: &str) -> String {
    normalize_whitespace(value, true)
}

fn normalize_whitespace(value: &str, lowercase: bool) -> String {
    let normalized: String = if lowercase {
        value
            .nfc()
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .nfc()
            .collect()
    } else {
        value.nfc().collect()
    };
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sorted_unique(values: &[SymbolId]) -> Vec<SymbolId> {
    values
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalized_paths(values: &[String]) -> Result<Vec<String>, CanonicalContextPackRequestError> {
    values
        .iter()
        .map(|value| normalize_repository_path(value))
        .collect::<Result<BTreeSet<_>, _>>()
        .map(BTreeSet::into_iter)
        .map(Iterator::collect)
}

fn normalize_repository_path(value: &str) -> Result<String, CanonicalContextPackRequestError> {
    let normalized = value.nfc().collect::<String>().replace('\\', "/");
    if normalized.starts_with('/')
        || normalized
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
    {
        return Err(CanonicalContextPackRequestError::InvalidField("paths"));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(CanonicalContextPackRequestError::InvalidField("paths")),
            component if component.chars().any(char::is_control) => {
                return Err(CanonicalContextPackRequestError::InvalidField("paths"));
            }
            component => components.push(component),
        }
    }
    let canonical = components.join("/");
    if canonical.is_empty() || canonical.len() > 4_096 {
        Err(CanonicalContextPackRequestError::InvalidField("paths"))
    } else {
        Ok(canonical)
    }
}

fn normalized_names(
    values: &[String],
    field: &'static str,
) -> Result<Vec<String>, CanonicalContextPackRequestError> {
    values
        .iter()
        .map(|value| {
            let normalized = normalize_whitespace(value, false);
            if normalized.is_empty() || normalized.len() > 4_096 {
                Err(CanonicalContextPackRequestError::InvalidField(field))
            } else {
                Ok(normalized)
            }
        })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(BTreeSet::into_iter)
        .map(Iterator::collect)
}

fn bounded_opaque(
    value: Option<&str>,
    field: &'static str,
) -> Result<Option<String>, CanonicalContextPackRequestError> {
    value
        .map(|value| {
            if value.is_empty() || value.len() > 256 {
                Err(CanonicalContextPackRequestError::InvalidField(field))
            } else {
                Ok(value.to_owned())
            }
        })
        .transpose()
}

fn hash_symbols(hasher: &mut blake3::Hasher, category: u8, symbols: &[SymbolId]) {
    hasher.update(&[category]);
    hash_count(hasher, symbols.len());
    for symbol in symbols {
        hash_bytes(hasher, symbol.as_bytes());
    }
}

fn hash_strings(hasher: &mut blake3::Hasher, category: u8, values: &[String]) {
    hasher.update(&[category]);
    hash_count(hasher, values.len());
    for value in values {
        hash_bytes(hasher, value.as_bytes());
    }
}

fn hash_optional_bytes(hasher: &mut blake3::Hasher, category: u8, value: Option<&str>) {
    hasher.update(&[category, u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_bytes(hasher, value.as_bytes());
    }
}

fn hash_count(hasher: &mut blake3::Hasher, count: usize) {
    hasher.update(&u64::try_from(count).unwrap_or(u64::MAX).to_le_bytes());
}

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_count(hasher, value.len());
    hasher.update(value);
}

fn short_digest(digest: [u8; 32]) -> String {
    blake3::Hash::from_bytes(digest)
        .to_hex()
        .chars()
        .take(32)
        .collect()
}

const fn source_policy_tag(policy: SourcePolicy) -> u8 {
    match policy {
        SourcePolicy::ReferencesOnly => 0,
        SourcePolicy::Signatures => 1,
        SourcePolicy::FocusedSnippets => 2,
        SourcePolicy::EvidenceHeavy => 3,
    }
}

const fn context_section_tag(section: ContextSection) -> u8 {
    match section {
        ContextSection::Architecture => 0,
        ContextSection::Definitions => 1,
        ContextSection::Callers => 2,
        ContextSection::Callees => 3,
        ContextSection::Types => 4,
        ContextSection::Tests => 5,
        ContextSection::History => 6,
        ContextSection::Source => 7,
        ContextSection::Risks => 8,
    }
}

const fn diversity_tag(diversity: Diversity) -> u8 {
    match diversity {
        Diversity::Balanced => 0,
        Diversity::Implementation => 1,
        Diversity::Tests => 2,
        Diversity::Impact => 3,
        Diversity::Architecture => 4,
    }
}

const fn response_profile_tag(profile: ResponseProfile) -> u8 {
    match profile {
        ResponseProfile::Compact => 0,
        ResponseProfile::Standard => 1,
        ResponseProfile::Evidence => 2,
    }
}

fn roles_for_sections(sections: &[ContextSection]) -> Vec<EvidenceRole> {
    let mut roles = sections
        .iter()
        .map(|section| match section {
            ContextSection::Architecture => EvidenceRole::Architecture,
            ContextSection::Definitions | ContextSection::Types => EvidenceRole::Definition,
            ContextSection::Callers | ContextSection::Callees => EvidenceRole::Caller,
            ContextSection::Tests => EvidenceRole::Test,
            ContextSection::History => EvidenceRole::Change,
            ContextSection::Source => EvidenceRole::Implementation,
            ContextSection::Risks => EvidenceRole::Risk,
        })
        .collect::<Vec<_>>();
    roles.sort_unstable_by_key(|role| role.priority());
    roles.dedup();
    roles
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rootlight_mcp_contract::{
        RepositorySelector,
        context::{ContextPackInput, ContextSection, ContextSeedSelector, Diversity, SourcePolicy},
        vertical::{ContinuationCursor, RepositoryIdSelector},
    };

    use super::{
        ALL_CONTEXT_SECTIONS, CanonicalContextPackRequest, CanonicalContextPackRequestError,
        ContextPackObjective, MAX_CONTEXT_PACK_SEEDS, normalize_task,
    };
    use crate::context_pack::EvidenceRole;
    use rootlight_ids::{GenerationId, RepositoryId, SymbolId};

    const REPOSITORY: RepositoryId = RepositoryId::from_bytes([1; 16]);
    const GENERATION: GenerationId = GenerationId::from_bytes([2; 20]);

    fn symbol(byte: u8) -> SymbolId {
        SymbolId::from_bytes([byte; 20])
    }

    fn input() -> ContextPackInput {
        ContextPackInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: REPOSITORY,
            }),
            generation: None,
            task: "Fix parser crash".to_owned(),
            seeds: ContextSeedSelector {
                symbols: Some(vec![symbol(1), symbol(2)]),
                paths: None,
                routes: None,
                tests: Some(vec![symbol(3)]),
                located: None,
                change: None,
                plan: None,
            },
            token_budget: 4_500,
            source_policy: None,
            sections: None,
            diversity: None,
            min_confidence: None,
            response_profile: None,
            continuation: None,
            explain: None,
        }
    }

    fn canonical(input: &ContextPackInput) -> CanonicalContextPackRequest {
        CanonicalContextPackRequest::new(input, REPOSITORY, GENERATION)
            .expect("fixture request canonicalizes")
    }

    #[test]
    fn task_normalization_handles_nfc_case_and_unicode_whitespace() {
        assert_eq!(
            normalize_task("  CAFÉ\u{00a0}\tFix  "),
            normalize_task("cafe\u{0301} fix")
        );
        let request = canonical(&input());
        assert_eq!(request.task(), "fix parser crash");
        assert_eq!(request.objective(), ContextPackObjective::BugFix);
    }

    #[test]
    fn paths_and_routes_normalize_without_filesystem_access() {
        let mut request = input();
        request.seeds.paths = Some(vec![
            "src\\.\\cafe\u{0301}//mod.rs".to_owned(),
            "src/café/mod.rs".to_owned(),
        ]);
        request.seeds.routes = Some(vec![
            " Payment\u{00a0}API ".to_owned(),
            "Payment API".to_owned(),
        ]);
        let canonical = canonical(&request);
        assert_eq!(canonical.seeds().paths(), ["src/café/mod.rs"]);
        assert_eq!(canonical.seeds().routes(), ["Payment API"]);

        request.seeds.paths = Some(vec!["../outside.rs".to_owned()]);
        assert_eq!(
            CanonicalContextPackRequest::new(&request, REPOSITORY, GENERATION),
            Err(CanonicalContextPackRequestError::InvalidField("paths"))
        );
    }

    #[test]
    fn explicit_defaults_and_reordered_sets_share_one_digest() {
        let implicit = canonical(&input());
        let mut explicit_input = input();
        explicit_input.source_policy = Some(SourcePolicy::ReferencesOnly);
        explicit_input.sections = Some(
            ALL_CONTEXT_SECTIONS
                .iter()
                .rev()
                .copied()
                .chain([ContextSection::Definitions])
                .collect(),
        );
        explicit_input.diversity = Some(Diversity::Balanced);
        explicit_input.min_confidence = Some(700);
        explicit_input.seeds.symbols = Some(vec![symbol(2), symbol(1), symbol(2)]);
        explicit_input.seeds.tests = Some(vec![symbol(3), symbol(3)]);
        let explicit = canonical(&explicit_input);

        assert_eq!(implicit.digest_bytes(), explicit.digest_bytes());
        assert_eq!(implicit.pack_id(), explicit.pack_id());
        assert_eq!(implicit.plan_fingerprint(), explicit.plan_fingerprint());
    }

    #[test]
    fn every_supported_semantic_dimension_changes_identity() {
        let base_input = input();
        let base = canonical(&base_input).digest_bytes();
        let mut digests = Vec::new();

        let mut changed = input();
        changed.task = "review parser security".to_owned();
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.symbols = Some(vec![symbol(4)]);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.tests = Some(vec![symbol(4)]);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.paths = Some(vec!["src/other.rs".to_owned()]);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.routes = Some(vec!["payments api".to_owned()]);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.located = Some(
            ContinuationCursor::parse("located-result").expect("located-result fixture is bounded"),
        );
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.change = Some("change-v1".to_owned());
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.seeds.plan = Some("plan-v1".to_owned());
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.token_budget = 4_501;
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.source_policy = Some(SourcePolicy::Signatures);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.sections = Some(vec![
            ContextSection::Definitions,
            ContextSection::Source,
            ContextSection::Callers,
            ContextSection::Tests,
        ]);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.diversity = Some(Diversity::Tests);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.min_confidence = Some(701);
        digests.push(canonical(&changed).digest_bytes());

        let mut changed = input();
        changed.response_profile =
            Some(rootlight_mcp_contract::vertical::ResponseProfile::Standard);
        digests.push(canonical(&changed).digest_bytes());

        let other_repository = RepositoryId::from_bytes([8; 16]);
        let mut changed = input();
        changed.repository = RepositorySelector::ById(RepositoryIdSelector {
            repository_id: other_repository,
        });
        digests.push(
            CanonicalContextPackRequest::new(&changed, other_repository, GENERATION)
                .expect("alternate repository canonicalizes")
                .digest_bytes(),
        );

        digests.push(
            CanonicalContextPackRequest::new(
                &input(),
                REPOSITORY,
                GenerationId::from_bytes([9; 20]),
            )
            .expect("active generation canonicalizes")
            .digest_bytes(),
        );

        assert!(digests.iter().all(|digest| *digest != base));
        let unique = digests
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), digests.len());
    }

    #[test]
    fn seed_categories_are_preserved_in_identity() {
        let mut all = input();
        all.seeds.symbols = Some(vec![symbol(7)]);
        all.seeds.paths = Some(vec!["src/lib.rs".to_owned()]);
        all.seeds.routes = Some(vec!["Payments API".to_owned()]);
        all.seeds.tests = Some(vec![symbol(7)]);
        all.seeds.located =
            Some(ContinuationCursor::parse("located-v1").expect("fixture handle is bounded"));
        all.seeds.change = Some("change-v1".to_owned());
        all.seeds.plan = Some("plan-v1".to_owned());
        let request = canonical(&all);

        assert_eq!(request.seeds().symbols(), &[symbol(7)]);
        assert_eq!(request.seeds().paths(), ["src/lib.rs"]);
        assert_eq!(request.seeds().routes(), ["Payments API"]);
        assert_eq!(request.seeds().tests(), &[symbol(7)]);
        assert_eq!(
            request.seeds().located().map(ContinuationCursor::as_str),
            Some("located-v1")
        );
        assert_eq!(request.seeds().change(), Some("change-v1"));
        assert_eq!(request.seeds().plan(), Some("plan-v1"));
        assert_eq!(request.seeds().len(), 7);

        let mut symbol_only = input();
        symbol_only.seeds.symbols = Some(vec![symbol(7)]);
        symbol_only.seeds.tests = None;
        let mut test_only = symbol_only.clone();
        test_only.seeds.symbols = None;
        test_only.seeds.tests = Some(vec![symbol(7)]);
        assert_ne!(
            canonical(&symbol_only).digest_bytes(),
            canonical(&test_only).digest_bytes()
        );
    }

    #[test]
    fn more_than_sixteen_distinct_typed_seeds_are_rejected() {
        let mut oversized = input();
        oversized.seeds.tests = None;
        oversized.seeds.symbols = Some(
            (0..=u8::try_from(MAX_CONTEXT_PACK_SEEDS).expect("limit fits u8"))
                .map(symbol)
                .collect(),
        );
        assert_eq!(
            CanonicalContextPackRequest::new(&oversized, REPOSITORY, GENERATION),
            Err(CanonicalContextPackRequestError::TooManySeeds)
        );
    }

    #[test]
    fn continuation_does_not_change_canonical_request_identity() {
        let mut continuation = input();
        continuation.continuation = Some(
            rootlight_mcp_contract::vertical::ContinuationCursor::parse("opaque")
                .expect("fixture cursor is bounded"),
        );
        assert_eq!(
            canonical(&continuation).digest_bytes(),
            canonical(&input()).digest_bytes()
        );
    }

    #[test]
    fn explicit_section_subsets_are_admitted_but_source_policies_remain_compatible() {
        let mut definitions_only = input();
        definitions_only.sections = Some(vec![ContextSection::Definitions]);
        let canonical = CanonicalContextPackRequest::new(&definitions_only, REPOSITORY, GENERATION)
            .expect("an explicit bounded section subset is valid");
        assert_eq!(canonical.sections(), [ContextSection::Definitions]);
        assert_eq!(canonical.requested_roles(), [EvidenceRole::Definition]);

        let mut snippets_without_source = input();
        snippets_without_source.task = "explain parser".to_owned();
        snippets_without_source.source_policy = Some(SourcePolicy::FocusedSnippets);
        snippets_without_source.sections = Some(vec![
            ContextSection::Definitions,
            ContextSection::Architecture,
        ]);
        assert_eq!(
            CanonicalContextPackRequest::new(&snippets_without_source, REPOSITORY, GENERATION),
            Err(CanonicalContextPackRequestError::InvalidField(
                "source_policy"
            ))
        );
    }

    #[test]
    fn opaque_seed_values_are_exact_and_bounded() {
        let mut request = input();
        request.seeds.change = Some(" change:v1 ".to_owned());
        request.seeds.plan = Some("plan:v1".to_owned());
        let canonical = canonical(&request);
        assert_eq!(canonical.seeds().change(), Some(" change:v1 "));
        assert_eq!(canonical.seeds().plan(), Some("plan:v1"));

        request.seeds.change = Some("x".repeat(257));
        assert_eq!(
            CanonicalContextPackRequest::new(&request, REPOSITORY, GENERATION),
            Err(CanonicalContextPackRequestError::InvalidField("change"))
        );
    }

    #[test]
    fn request_digest_has_a_stable_golden_encoding() {
        let request = canonical(&input());
        assert_eq!(
            request.request_digest(),
            "ctxreq1_46580bf665df899a47c667ff2ebaa508810cb3bdded69a9d10d9cb0caf6d921c"
        );
        assert_eq!(
            request.pack_id().as_str(),
            &request.request_digest().replacen("ctxreq1_", "pack1_", 1)[.."pack1_".len() + 32]
        );
        assert_eq!(
            request.plan_fingerprint(),
            request.pack_id().as_str().replacen("pack1_", "plan1_", 1)
        );
    }

    proptest! {
        #[test]
        fn seed_order_and_duplicates_do_not_change_digest(
            symbol_bytes in proptest::collection::vec(any::<u8>(), 1..=6),
            test_bytes in proptest::collection::vec(any::<u8>(), 0..=6),
        ) {
            let mut first = input();
            first.seeds.symbols = Some(symbol_bytes.iter().copied().map(symbol).collect());
            first.seeds.tests = Some(test_bytes.iter().copied().map(symbol).collect());

            let mut second = first.clone();
            let mut symbols = first.seeds.symbols.clone().unwrap_or_default();
            symbols.reverse();
            symbols.extend(symbols.clone());
            second.seeds.symbols = Some(symbols);
            let mut tests = first.seeds.tests.clone().unwrap_or_default();
            tests.reverse();
            tests.extend(tests.clone());
            second.seeds.tests = Some(tests);

            let first = canonical(&first);
            let second = canonical(&second);
            prop_assert_eq!(first.digest_bytes(), second.digest_bytes());
        }

        #[test]
        fn path_and_route_order_and_duplicates_do_not_change_digest(
            path_bytes in proptest::collection::vec(0_u8..=9, 0..=5),
            route_bytes in proptest::collection::vec(0_u8..=9, 0..=5),
        ) {
            let mut first = input();
            first.seeds.paths = Some(
                path_bytes
                    .iter()
                    .map(|byte| format!("src\\module_{byte}//lib.rs"))
                    .collect(),
            );
            first.seeds.routes = Some(
                route_bytes
                    .iter()
                    .map(|byte| format!(" Route\u{00a0}{byte} "))
                    .collect(),
            );

            let mut second = first.clone();
            let mut paths = first.seeds.paths.clone().unwrap_or_default();
            paths.reverse();
            paths.extend(paths.clone());
            second.seeds.paths = Some(paths);
            let mut routes = first.seeds.routes.clone().unwrap_or_default();
            routes.reverse();
            routes.extend(routes.clone());
            second.seeds.routes = Some(routes);

            prop_assert_eq!(
                canonical(&first).digest_bytes(),
                canonical(&second).digest_bytes()
            );
        }
    }
}
