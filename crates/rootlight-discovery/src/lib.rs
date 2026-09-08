//! Deterministic repository discovery over the capability-confined VFS.
//!
//! The engine applies bounded policy and classification without reading the
//! ambient filesystem, then emits a canonical versioned manifest.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};
use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_config::{CONFIG_VERSION_1_0, ConfigSnapshot};
use rootlight_ids::{ContentHash, FileId, RepositoryId, content_hash};
use rootlight_incremental::{FileDescriptor, IncrementalError, ScannedFile};
use rootlight_vfs::{
    BoundedDirectoryEntries, DirectoryEntry, EntryKind, MAX_SNAPSHOT_BATCH_BYTES,
    MAX_SNAPSHOT_BATCH_FILES, RelativePath, RepositoryRoot, SourceSnapshot, VfsError,
};
use serde::{Deserialize, Serialize};

mod incremental;

pub use incremental::{
    IncrementalDiscovery, IncrementalDiscoveryBaseline, IncrementalDiscoveryContext,
    IncrementalDiscoveryOptions, IncrementalDiscoveryProgress, ManifestReconcileInputs,
    correlate_incremental_manifest, discover_incremental, discover_incremental_with_progress,
};

/// Current deterministic discovery-manifest version.
pub const DISCOVERY_MANIFEST_VERSION: &str = "1.3";
/// Stable source-free diagnostic emitted when the entry budget truncates discovery.
pub const DISCOVERY_ENTRY_LIMIT_DIAGNOSTIC_CODE: &str = "DISCOVERY_ENTRY_LIMIT";
/// Stable source-free diagnostic emitted when retained source bytes truncate discovery.
pub const DISCOVERY_SOURCE_BYTE_LIMIT_DIAGNOSTIC_CODE: &str = "DISCOVERY_SOURCE_BYTE_LIMIT";
/// Hard entry ceiling independent of caller configuration.
pub const MAX_DISCOVERY_ENTRIES: usize = 1_000_000;
/// Hard traversal-depth ceiling independent of caller configuration.
pub const MAX_DISCOVERY_DEPTH: usize = 256;
/// Hard diagnostic ceiling independent of caller configuration.
pub const MAX_DISCOVERY_DIAGNOSTICS: usize = 10_000;
/// Maximum bytes read for bounded content classification.
pub const MAX_CLASSIFICATION_BYTES: usize = 8 * 1024;
/// Hard aggregate ceiling for source snapshots retained by one discovery.
pub const MAX_RETAINED_SOURCE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetainedSnapshotBudget {
    observed: u64,
    maximum: u64,
}

impl RetainedSnapshotBudget {
    pub(crate) const fn new(maximum: u64) -> Self {
        Self {
            observed: 0,
            maximum,
        }
    }

    pub(crate) fn preflight<'a>(
        maximum: u64,
        snapshots: impl IntoIterator<Item = &'a SourceSnapshot>,
    ) -> Result<Self, DiscoveryError> {
        let mut budget = Self::new(maximum);
        for snapshot in snapshots {
            budget.reserve(snapshot_byte_length(snapshot, maximum)?)?;
        }
        Ok(budget)
    }

    pub(crate) fn reserve(&mut self, bytes: u64) -> Result<(), DiscoveryError> {
        let observed =
            self.observed
                .checked_add(bytes)
                .ok_or(DiscoveryError::RetainedSnapshotByteLimit {
                    observed: u64::MAX,
                    maximum: self.maximum,
                })?;
        if observed > self.maximum {
            return Err(DiscoveryError::RetainedSnapshotByteLimit {
                observed,
                maximum: self.maximum,
            });
        }
        self.observed = observed;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) const fn observed(self) -> u64 {
        self.observed
    }
}

fn snapshot_byte_length(snapshot: &SourceSnapshot, maximum: u64) -> Result<u64, DiscoveryError> {
    let bytes = u64::try_from(snapshot.content().len()).map_err(|_| {
        DiscoveryError::RetainedSnapshotByteLimit {
            observed: u64::MAX,
            maximum,
        }
    })?;
    if bytes != snapshot.metadata().length {
        return Err(DiscoveryError::IncrementalDrift);
    }
    Ok(bytes)
}

/// Per-scan resource limits below hard safety ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryLimits {
    /// Maximum entries visited, including excluded entries.
    pub max_entries: usize,
    /// Maximum directory depth beneath the repository root.
    pub max_depth: usize,
    /// Maximum regular-file bytes included in one manifest entry.
    pub max_file_bytes: u64,
    /// Maximum retained diagnostics.
    pub max_diagnostics: usize,
}

impl DiscoveryLimits {
    /// Creates checked discovery limits.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::InvalidLimits`] for zero or hard-ceiling values.
    pub fn new(
        max_entries: usize,
        max_depth: usize,
        max_file_bytes: u64,
        max_diagnostics: usize,
    ) -> Result<Self, DiscoveryError> {
        if max_entries == 0
            || max_entries > MAX_DISCOVERY_ENTRIES
            || max_depth == 0
            || max_depth > MAX_DISCOVERY_DEPTH
            || max_file_bytes == 0
            || max_file_bytes > rootlight_vfs::MAX_SNAPSHOT_BYTES
            || max_diagnostics > MAX_DISCOVERY_DIAGNOSTICS
        {
            return Err(DiscoveryError::InvalidLimits);
        }
        Ok(Self {
            max_entries,
            max_depth,
            max_file_bytes,
            max_diagnostics,
        })
    }

    /// Derives conservative discovery bounds from the immutable core config.
    #[must_use]
    pub fn from_config(config: &ConfigSnapshot) -> Self {
        let analysis = config.analysis();
        let max_file_bytes = if config.version() == CONFIG_VERSION_1_0 {
            config.resources().max_source_bytes
        } else {
            analysis.max_source_file_bytes
        };
        Self {
            max_entries: usize::try_from(config.max_discovery_entries())
                .unwrap_or(usize::MAX)
                .min(MAX_DISCOVERY_ENTRIES),
            max_depth: MAX_DISCOVERY_DEPTH.min(128),
            max_file_bytes: max_file_bytes.min(rootlight_vfs::MAX_SNAPSHOT_BYTES),
            max_diagnostics: MAX_DISCOVERY_DIAGNOSTICS.min(1_000),
        }
    }
}

/// One ordered pattern layer in discovery policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyLayer {
    /// Safe compiled exclusions.
    Default,
    /// Rules read from repository VCS ignore files.
    VcsIgnore,
    /// Explicit repository configuration.
    Repository,
    /// Explicit operation-specific include and exclude rules.
    Operation,
}

/// A validated policy rule associated with one precedence layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRule {
    /// Precedence layer; higher enum order is evaluated later.
    pub layer: PolicyLayer,
    /// Gitignore-compatible pattern; leading `!` includes a prior exclusion.
    pub pattern: String,
    /// Source-free stable label used by audit output.
    pub source: String,
}

/// Immutable discovery policy built from caller-supplied rules.
#[derive(Debug)]
pub struct DiscoveryPolicy {
    rules: Vec<PolicyRule>,
    matchers: PolicyMatchers,
    audit: bool,
    tracked_files: BTreeSet<String>,
}

#[derive(Debug)]
struct PolicyMatchers {
    default: Gitignore,
    vcs_ignore: Gitignore,
    repository: Gitignore,
    operation: Gitignore,
}

impl DiscoveryPolicy {
    /// Builds a layered matcher in deterministic precedence order.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::InvalidPolicy`] for unordered layers or unsafe
    /// source labels, and [`DiscoveryError::InvalidPattern`] for malformed
    /// configured patterns.
    pub fn build(mut rules: Vec<PolicyRule>, audit: bool) -> Result<Self, DiscoveryError> {
        rules.splice(0..0, default_rules());
        if rules.windows(2).any(|pair| pair[0].layer > pair[1].layer) {
            return Err(DiscoveryError::InvalidPolicy);
        }
        for rule in &rules {
            if !valid_source_label(&rule.source) {
                return Err(DiscoveryError::InvalidPolicy);
            }
        }
        let matchers = PolicyMatchers {
            default: build_policy_matcher(&rules, PolicyLayer::Default)?,
            vcs_ignore: build_policy_matcher(&rules, PolicyLayer::VcsIgnore)?,
            repository: build_policy_matcher(&rules, PolicyLayer::Repository)?,
            operation: build_policy_matcher(&rules, PolicyLayer::Operation)?,
        };
        Ok(Self {
            rules,
            matchers,
            audit,
            tracked_files: BTreeSet::new(),
        })
    }

    /// Retains tracked regular files through VCS ignores, without overriding
    /// default, repository, operation, or VFS safety policy.
    ///
    /// Ancestors are traversal routes only; their untracked children do not
    /// inherit the tracked-file admission.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError`] for excessive inventory size, invalid relative
    /// paths, or cancellation. No partially validated policy is returned.
    pub fn with_tracked_files(
        mut self,
        paths: BTreeSet<String>,
        cancellation: &Cancellation,
    ) -> Result<Self, DiscoveryError> {
        if paths.len() > MAX_DISCOVERY_ENTRIES {
            return Err(DiscoveryError::EntryLimit {
                maximum: MAX_DISCOVERY_ENTRIES,
            });
        }
        for path in paths {
            cancellation.check()?;
            let path = RelativePath::parse(Path::new(&path))?;
            self.tracked_files.insert(path.as_str().to_owned());
        }
        cancellation.check()?;
        Ok(self)
    }

    fn admits_tracked_path(&self, path: &RelativePath, is_directory: bool) -> bool {
        if !is_directory {
            return self.tracked_files.contains(path.as_str());
        }
        let prefix = format!("{}/", path.as_str());
        self.tracked_files
            .range::<str, _>((
                std::ops::Bound::Included(prefix.as_str()),
                std::ops::Bound::Unbounded,
            ))
            .next()
            .is_some_and(|file| file.starts_with(&prefix))
    }

    /// Returns the ordered policy rules used to build this matcher.
    #[must_use]
    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    #[cfg(test)]
    fn decision(&self, path: &RelativePath, is_directory: bool) -> PolicyDecision {
        self.layered_decision(path, is_directory, None)
    }

    fn decision_with_scoped_ignores(
        &self,
        path: &RelativePath,
        is_directory: bool,
        scoped_ignores: &ScopedIgnores,
    ) -> PolicyDecision {
        self.layered_decision(path, is_directory, Some(scoped_ignores))
    }

    fn layered_decision(
        &self,
        path: &RelativePath,
        is_directory: bool,
        scoped_ignores: Option<&ScopedIgnores>,
    ) -> PolicyDecision {
        let candidate = Path::new(path.as_str());
        let default_decision = decision_from_match(
            self.matchers.default.matched(candidate, is_directory),
            self.audit,
        );
        let default_excluded = default_decision
            .as_ref()
            .is_some_and(|decision| decision.excluded);
        let mut decision = default_decision.unwrap_or_default();

        // Repository-controlled ignore files must not reopen host-owned default exclusions.
        if !default_excluded {
            if let Some(layer_decision) = decision_from_match(
                self.matchers.vcs_ignore.matched(candidate, is_directory),
                self.audit,
            ) {
                decision = layer_decision;
            }
            if let Some(scoped_decision) =
                scoped_ignores.and_then(|ignores| ignores.decision(path, is_directory, self.audit))
            {
                decision = scoped_decision;
            }
            if !self.tracked_files.is_empty() {
                // Reopened ignored directories still exclude untracked children,
                // even when a child ignore file attempts to negate the parent.
                let mut parent = parent_scope(path.as_str());
                while !parent.is_empty() {
                    let parent_path = Path::new(parent);
                    let inherited = scoped_ignores
                        .and_then(|ignores| ignores.decision_at(parent, true, self.audit))
                        .or_else(|| {
                            decision_from_match(
                                self.matchers.vcs_ignore.matched(parent_path, true),
                                self.audit,
                            )
                        });
                    if let Some(inherited) = inherited.filter(|value| value.excluded) {
                        decision = inherited;
                        break;
                    }
                    parent = parent_scope(parent);
                }
                if self.admits_tracked_path(path, is_directory) {
                    decision = PolicyDecision {
                        included: true,
                        ..PolicyDecision::default()
                    };
                }
            }
        }
        for matcher in [&self.matchers.repository, &self.matchers.operation] {
            if let Some(layer_decision) =
                decision_from_match(matcher.matched(candidate, is_directory), self.audit)
            {
                decision = layer_decision;
            }
        }
        decision
    }
}

fn build_policy_matcher(
    rules: &[PolicyRule],
    layer: PolicyLayer,
) -> Result<Gitignore, DiscoveryError> {
    let mut builder = GitignoreBuilder::new("");
    for rule in rules.iter().filter(|rule| rule.layer == layer) {
        builder
            .add_line(Some(PathBuf::from(&rule.source)), &rule.pattern)
            .map_err(|source| DiscoveryError::InvalidPattern { source })?;
    }
    builder
        .build()
        .map_err(|source| DiscoveryError::InvalidPattern { source })
}

/// Why one path was included or excluded by layered policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisiveRule {
    /// Repository-relative ignore file or source-free stable rule label.
    pub source: String,
    /// Original Gitignore-compatible pattern.
    pub pattern: String,
}

#[derive(Debug, Default)]
struct PolicyDecision {
    included: bool,
    excluded: bool,
    decisive_rule: Option<DecisiveRule>,
}

fn decision_from_match(
    matched: Match<&ignore::gitignore::Glob>,
    audit: bool,
) -> Option<PolicyDecision> {
    match matched {
        Match::Ignore(glob) | Match::Whitelist(glob) => Some(PolicyDecision {
            included: glob.is_whitelist(),
            excluded: !glob.is_whitelist(),
            decisive_rule: audit.then(|| DecisiveRule {
                source: glob.from().map_or_else(
                    || "default".to_owned(),
                    |source| source.to_string_lossy().into_owned(),
                ),
                pattern: glob.original().to_owned(),
            }),
        }),
        Match::None => None,
    }
}

#[derive(Debug, Default)]
struct ScopedIgnores {
    matchers: BTreeMap<String, Gitignore>,
}

impl ScopedIgnores {
    fn insert(&mut self, scope: &str, matcher: Gitignore) {
        self.matchers.insert(scope.to_owned(), matcher);
    }

    fn decision(
        &self,
        path: &RelativePath,
        is_directory: bool,
        audit: bool,
    ) -> Option<PolicyDecision> {
        self.decision_at(path.as_str(), is_directory, audit)
    }

    fn decision_at(&self, path: &str, is_directory: bool, audit: bool) -> Option<PolicyDecision> {
        let candidate = Path::new(path);
        let mut scope = parent_scope(path);
        loop {
            if let Some(matcher) = self.matchers.get(scope)
                && let Some(decision) =
                    decision_from_match(matcher.matched(candidate, is_directory), audit)
            {
                return Some(decision);
            }
            if scope.is_empty() {
                return None;
            }
            scope = parent_scope(scope);
        }
    }
}

fn parent_scope(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Included-file classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputClass {
    /// Ordinary source or documentation input.
    Source,
    /// Generated input recognized from stable path or content evidence.
    Generated,
    /// Third-party or vendored input.
    Vendored,
}

/// Reason an entry was excluded from source parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// Layered include/exclude policy denied the path.
    Policy,
    /// A symbolic link, junction, mount point, or reparse point was rejected.
    Link,
    /// A non-regular filesystem object was rejected.
    Special,
    /// The file exceeded configured source bytes.
    Oversized,
    /// Bounded content sniffing identified binary content.
    Binary,
    /// The entry could not be read safely.
    Unreadable,
    /// The traversal-depth limit excluded the subtree.
    DepthLimit,
}

/// Evidence used to classify one file's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageEvidence {
    /// Compound or simple file extension.
    Extension,
    /// Interpreter shebang.
    Shebang,
    /// Project/toolchain context from a manifest name, not the file's source syntax.
    Manifest,
    /// Bounded deterministic content signal.
    Content,
}

/// One language signal without a semantic-tier claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageSignal {
    /// Canonical language label.
    pub language: String,
    /// Evidence that produced this signal.
    pub evidence: LanguageEvidence,
}

/// Included immutable input in a discovery manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestInput {
    /// Stable repository-scoped file identity.
    pub file: FileId,
    /// Canonical repository-relative path.
    pub path: String,
    /// Actual-byte content hash.
    pub content_hash: ContentHash,
    /// Source byte length.
    pub bytes: u64,
    /// Input classification.
    pub class: InputClass,
    /// Ordered language signals; no signal implies unknown language.
    pub language_signals: Vec<LanguageSignal>,
    /// Optional decisive include rule in audit mode.
    pub decisive_rule: Option<DecisiveRule>,
}

/// One excluded path in audit mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestExclusion {
    /// Canonical repository-relative path.
    pub path: String,
    /// Stable exclusion class.
    pub reason: ExclusionReason,
    /// Optional decisive policy rule.
    pub decisive_rule: Option<DecisiveRule>,
}

/// Source-free bounded diagnostic retained by discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryDiagnostic {
    /// Canonical repository-relative path, when safely known.
    pub path: Option<String>,
    /// Stable source-free diagnostic code.
    pub code: String,
}

/// Coverage counts for one bounded discovery run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryCoverage {
    /// Total filesystem entries observed.
    pub visited: u64,
    /// Included regular inputs.
    pub included: u64,
    /// Exclusions grouped by stable reason name.
    pub excluded: BTreeMap<String, u64>,
    /// Whether traversal reached the end of the repository namespace.
    #[serde(default = "complete_discovery_coverage")]
    pub complete: bool,
    /// Exact bound that selected a deterministic prefix, when traversal was truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<DiscoveryTruncation>,
}

/// Closed resource labels for deterministic discovery truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryTruncationResource {
    /// Filesystem entry traversal.
    Entries,
    /// Aggregate source snapshots retained for one generation.
    RetainedSourceBytes,
}

/// Exact bounded condition that selected an incomplete discovery prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryTruncation {
    /// Resource that selected the prefix.
    pub resource: DiscoveryTruncationResource,
    /// Safe observed or lower-bound demand.
    pub observed: u64,
    /// Effective resource limit.
    pub limit: u64,
}

impl Default for DiscoveryCoverage {
    fn default() -> Self {
        Self {
            visited: 0,
            included: 0,
            excluded: BTreeMap::new(),
            complete: true,
            truncation: None,
        }
    }
}

const fn complete_discovery_coverage() -> bool {
    true
}

/// Canonical, versioned result of deterministic repository discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryManifest {
    /// Manifest contract version.
    pub version: String,
    /// Stable repository identity.
    pub repository: RepositoryId,
    /// Immutable configuration identity.
    pub configuration_hash: ContentHash,
    /// Included inputs ordered by path identity.
    pub inputs: Vec<ManifestInput>,
    /// Audit exclusions ordered by path and reason.
    pub exclusions: Vec<ManifestExclusion>,
    /// Source-free diagnostics ordered by path and code.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    /// Bounded discovery coverage.
    pub coverage: DiscoveryCoverage,
}

impl DiscoveryManifest {
    /// Serializes deterministic canonical JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::SerializeManifest`] on unexpected serialization
    /// failure.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, DiscoveryError> {
        serde_json::to_vec(self).map_err(DiscoveryError::SerializeManifest)
    }

    /// Returns the content hash of canonical manifest bytes.
    ///
    /// # Errors
    ///
    /// Propagates canonical serialization failure.
    pub fn hash(&self) -> Result<ContentHash, DiscoveryError> {
        self.canonical_bytes().map(|bytes| content_hash(&bytes))
    }
}

/// Clean discovery output paired with the immutable bytes it classified.
#[derive(Clone, PartialEq, Eq)]
pub struct DiscoveryResult {
    manifest: DiscoveryManifest,
    snapshots: BTreeMap<FileId, SourceSnapshot>,
}

impl DiscoveryResult {
    /// Splits the canonical manifest from its matching source snapshots.
    #[must_use]
    pub fn into_parts(self) -> (DiscoveryManifest, BTreeMap<FileId, SourceSnapshot>) {
        (self.manifest, self.snapshots)
    }
}

/// Runs deterministic bounded discovery through the approved VFS root.
///
/// # Errors
///
/// Returns a typed error for cancellation, resource limits, unsafe VFS state,
/// invalid policy, or manifest serialization.
pub fn discover(
    root: &RepositoryRoot,
    config: &ConfigSnapshot,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    cancellation: &Cancellation,
) -> Result<DiscoveryManifest, DiscoveryError> {
    discover_manifest_streaming(root, config, policy, limits, cancellation)
}

/// Runs deterministic discovery while retaining only manifest metadata.
///
/// Source contents are captured, classified, and hashed one file at a time.
/// The per-file limit still bounds resident bytes, while repositories whose
/// aggregate text exceeds [`MAX_RETAINED_SOURCE_BYTES`] can produce a complete
/// manifest for a later staged or streaming consumer.
///
/// # Errors
///
/// Returns the same typed discovery failures as [`discover`].
pub fn discover_manifest_streaming(
    root: &RepositoryRoot,
    config: &ConfigSnapshot,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    cancellation: &Cancellation,
) -> Result<DiscoveryManifest, DiscoveryError> {
    let mut state = DiscoveryState::new(
        root,
        config,
        policy,
        limits,
        SnapshotRetentionOptions::manifest_only(),
        None,
        cancellation,
    )?;
    state.run()?;
    Ok(state.finish().manifest)
}

/// Discovers one clean manifest and reconciles its exact observation.
///
/// Unlike a separate incremental scan followed by clean discovery, this path
/// derives the metadata baseline and content changes from the stable snapshots
/// that already produced the manifest. Callers with a parent may still prefer
/// [`discover_incremental_with_progress`] first when a metadata-only no-op can
/// avoid clean content discovery altogether.
///
/// # Errors
///
/// Returns a typed discovery, VFS, incremental-contract, resource-limit,
/// cancellation, or scan/snapshot drift error.
pub fn discover_manifest_and_reconcile_with_progress(
    root: &RepositoryRoot,
    config: &ConfigSnapshot,
    reconcile: ManifestReconcileInputs<'_>,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    cancellation: &Cancellation,
    mut observe_progress: impl FnMut(IncrementalDiscoveryProgress),
) -> Result<(DiscoveryManifest, IncrementalDiscovery), DiscoveryError> {
    let mut state = DiscoveryState::new(
        root,
        config,
        policy,
        limits,
        SnapshotRetentionOptions::manifest_only(),
        Some(&mut observe_progress),
        cancellation,
    )?;
    state.run()?;
    let (result, scanned) = state.finish_with_scan();
    let empty_hashed_snapshots = BTreeMap::new();
    let incremental = incremental::reconcile_manifest_scan(
        incremental::ManifestScanReconcile {
            repository: root.repository(),
            observed_complete: result.manifest.coverage.complete,
            scanned,
            reconcile,
            observed_hashed_files: None,
            observed_hashed_snapshots: &empty_hashed_snapshots,
        },
        &result.manifest,
        limits,
        cancellation,
    )?;
    Ok((result.manifest, incremental))
}

/// Runs deterministic discovery while retaining the exact classified snapshots.
///
/// `cached_snapshots` must come from an immediately preceding authoritative
/// metadata reconcile for the same repository. Every reused snapshot is checked
/// against the clean traversal's current metadata before it becomes an input.
///
/// # Errors
///
/// Returns the same errors as [`discover`], plus
/// [`DiscoveryError::IncrementalDrift`] when a cached snapshot no longer
/// matches the clean traversal.
pub fn discover_with_snapshots(
    root: &RepositoryRoot,
    config: &ConfigSnapshot,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    cached_snapshots: BTreeMap<FileId, SourceSnapshot>,
    cancellation: &Cancellation,
) -> Result<DiscoveryResult, DiscoveryError> {
    discover_with_snapshots_at_limit(
        root,
        config,
        policy,
        limits,
        cached_snapshots,
        MAX_RETAINED_SOURCE_BYTES,
        cancellation,
    )
}

/// Discovers inputs while tightening the process-wide retained-source ceiling.
///
/// A caller-provided ceiling above [`MAX_RETAINED_SOURCE_BYTES`] is clamped to
/// that hard bound. This variant lets an orchestrator apply a stricter local
/// budget without creating a path that can relax the process limit.
///
/// # Errors
///
/// Returns the same errors as [`discover_with_snapshots`].
pub fn discover_with_snapshots_at_limit(
    root: &RepositoryRoot,
    config: &ConfigSnapshot,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    cached_snapshots: BTreeMap<FileId, SourceSnapshot>,
    maximum_retained_source_bytes: u64,
    cancellation: &Cancellation,
) -> Result<DiscoveryResult, DiscoveryError> {
    let maximum_retained_source_bytes =
        maximum_retained_source_bytes.min(MAX_RETAINED_SOURCE_BYTES);
    let mut state = DiscoveryState::new(
        root,
        config,
        policy,
        limits,
        SnapshotRetentionOptions::retained(cached_snapshots, maximum_retained_source_bytes),
        None,
        cancellation,
    )?;
    state.run()?;
    Ok(state.finish())
}

struct DiscoveryState<'a> {
    root: &'a RepositoryRoot,
    config: &'a ConfigSnapshot,
    policy: &'a DiscoveryPolicy,
    limits: DiscoveryLimits,
    cancellation: &'a Cancellation,
    queue: VecDeque<(Option<RelativePath>, usize)>,
    inputs: Vec<ManifestInput>,
    exclusions: Vec<ManifestExclusion>,
    diagnostics: Vec<DiscoveryDiagnostic>,
    coverage: DiscoveryCoverage,
    scoped_ignores: ScopedIgnores,
    cached_snapshots: BTreeMap<FileId, SourceSnapshot>,
    snapshots: BTreeMap<FileId, SourceSnapshot>,
    scanned: Vec<ScannedFile>,
    snapshot_budget: RetainedSnapshotBudget,
    retain_snapshots: bool,
    observe_progress: Option<&'a mut dyn FnMut(IncrementalDiscoveryProgress)>,
    files_examined: u64,
    bytes_examined: u64,
    pending_files: Vec<PendingDiscoveryFile>,
    pending_source_bytes: u64,
}

struct PendingDiscoveryFile {
    path: RelativePath,
    metadata: rootlight_vfs::SnapshotMetadata,
    decisive_rule: Option<DecisiveRule>,
}

struct SnapshotRetentionOptions {
    cached_snapshots: BTreeMap<FileId, SourceSnapshot>,
    maximum_bytes: u64,
    retain_snapshots: bool,
}

impl SnapshotRetentionOptions {
    fn manifest_only() -> Self {
        Self {
            cached_snapshots: BTreeMap::new(),
            maximum_bytes: MAX_RETAINED_SOURCE_BYTES,
            retain_snapshots: false,
        }
    }

    fn retained(cached_snapshots: BTreeMap<FileId, SourceSnapshot>, maximum_bytes: u64) -> Self {
        Self {
            cached_snapshots,
            maximum_bytes,
            retain_snapshots: true,
        }
    }
}

impl<'a> DiscoveryState<'a> {
    fn new(
        root: &'a RepositoryRoot,
        config: &'a ConfigSnapshot,
        policy: &'a DiscoveryPolicy,
        limits: DiscoveryLimits,
        snapshot_options: SnapshotRetentionOptions,
        observe_progress: Option<&'a mut dyn FnMut(IncrementalDiscoveryProgress)>,
        cancellation: &'a Cancellation,
    ) -> Result<Self, DiscoveryError> {
        let SnapshotRetentionOptions {
            cached_snapshots,
            maximum_bytes,
            retain_snapshots,
        } = snapshot_options;
        let snapshot_budget =
            RetainedSnapshotBudget::preflight(maximum_bytes, cached_snapshots.values())?;
        let mut queue = VecDeque::new();
        queue.push_back((None, 0));
        Ok(Self {
            root,
            config,
            policy,
            limits,
            cancellation,
            queue,
            inputs: Vec::new(),
            exclusions: Vec::new(),
            diagnostics: Vec::new(),
            coverage: DiscoveryCoverage::default(),
            scoped_ignores: ScopedIgnores::default(),
            cached_snapshots,
            snapshots: BTreeMap::new(),
            scanned: Vec::new(),
            snapshot_budget,
            retain_snapshots,
            observe_progress,
            files_examined: 0,
            bytes_examined: 0,
            pending_files: Vec::new(),
            pending_source_bytes: 0,
        })
    }

    fn run(&mut self) -> Result<(), DiscoveryError> {
        while let Some((directory, depth)) = self.queue.pop_front() {
            self.cancellation.check()?;
            let visited = usize::try_from(self.coverage.visited).unwrap_or(usize::MAX);
            let bounded = read_directory_with_entry_budget(
                self.root,
                directory.as_ref(),
                self.limits.max_entries.saturating_sub(visited),
                self.cancellation,
            )?;
            let complete = bounded.is_complete();
            let entries = bounded.into_entries();
            self.cancellation.check()?;
            self.ensure_entry_capacity(entries.len())?;
            // Ignore files become readable only after ancestor policy admits this
            // directory, so descendant negations cannot force traversal into it.
            let mut ignore_snapshot = match self.load_scoped_ignore(directory.as_ref(), &entries) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.finish_at_source_byte_limit(error)?;
                    return Ok(());
                }
            };
            for entry in entries {
                self.cancellation.check()?;
                let cached_snapshot = if entry.name == OsStr::new(".gitignore") {
                    ignore_snapshot.take()
                } else {
                    None
                };
                if let Err(error) =
                    self.visit_entry(directory.as_ref(), depth, entry, cached_snapshot)
                {
                    self.finish_at_source_byte_limit(error)?;
                    return Ok(());
                }
            }
            if !complete {
                self.coverage.complete = false;
                let limit = u64::try_from(self.limits.max_entries).unwrap_or(u64::MAX);
                self.coverage.truncation = Some(DiscoveryTruncation {
                    resource: DiscoveryTruncationResource::Entries,
                    observed: limit.saturating_add(1),
                    limit,
                });
                self.diagnostic(None, DISCOVERY_ENTRY_LIMIT_DIAGNOSTIC_CODE);
                self.queue.clear();
            }
        }
        if let Err(error) = self.flush_pending_files() {
            self.finish_at_source_byte_limit(error)?;
        }
        Ok(())
    }

    fn finish_at_source_byte_limit(&mut self, error: DiscoveryError) -> Result<(), DiscoveryError> {
        match error {
            DiscoveryError::RetainedSnapshotByteLimit { observed, maximum } => {
                // Later paths must not displace this deterministic traversal
                // prefix merely because their sources happen to be smaller.
                self.coverage.complete = false;
                self.coverage.truncation = Some(DiscoveryTruncation {
                    resource: DiscoveryTruncationResource::RetainedSourceBytes,
                    observed,
                    limit: maximum,
                });
                self.diagnostic(None, DISCOVERY_SOURCE_BYTE_LIMIT_DIAGNOSTIC_CODE);
                self.queue.clear();
                self.pending_files.clear();
                self.pending_source_bytes = 0;
                Ok(())
            }
            error => Err(error),
        }
    }

    fn ensure_entry_capacity(&self, entry_count: usize) -> Result<(), DiscoveryError> {
        let visited = usize::try_from(self.coverage.visited).unwrap_or(usize::MAX);
        if entry_count > self.limits.max_entries.saturating_sub(visited) {
            return Err(DiscoveryError::EntryLimit {
                maximum: self.limits.max_entries,
            });
        }
        Ok(())
    }

    fn load_scoped_ignore(
        &mut self,
        directory: Option<&RelativePath>,
        entries: &[DirectoryEntry],
    ) -> Result<Option<SourceSnapshot>, DiscoveryError> {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.name == OsStr::new(".gitignore") && entry.kind == EntryKind::File)
        else {
            return Ok(None);
        };
        let path = child_path(directory, &entry.name)?;
        let snapshot = self.snapshot_for_path(&path, entry.metadata)?;
        let contents =
            std::str::from_utf8(snapshot.content()).map_err(|_| DiscoveryError::InvalidPolicy)?;
        let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
        let scope = directory.map_or("", RelativePath::as_str);
        let source = PathBuf::from(path.as_str());
        let mut builder = GitignoreBuilder::new(Path::new(scope));
        for line in contents.lines() {
            self.cancellation.check()?;
            builder
                .add_line(Some(source.clone()), line)
                .map_err(|_| DiscoveryError::InvalidPolicy)?;
        }
        self.cancellation.check()?;
        let matcher = builder.build().map_err(|_| DiscoveryError::InvalidPolicy)?;
        self.cancellation.check()?;
        self.scoped_ignores.insert(scope, matcher);
        Ok(Some(snapshot))
    }

    fn visit_entry(
        &mut self,
        directory: Option<&RelativePath>,
        depth: usize,
        entry: DirectoryEntry,
        cached_snapshot: Option<SourceSnapshot>,
    ) -> Result<(), DiscoveryError> {
        if usize::try_from(self.coverage.visited).unwrap_or(usize::MAX) >= self.limits.max_entries {
            return Err(DiscoveryError::EntryLimit {
                maximum: self.limits.max_entries,
            });
        }
        self.coverage.visited = self.coverage.visited.saturating_add(1);
        let path = child_path(directory, &entry.name)?;
        let is_directory = entry.kind == EntryKind::Directory;
        let decision =
            self.policy
                .decision_with_scoped_ignores(&path, is_directory, &self.scoped_ignores);
        if decision.excluded && !decision.included {
            self.exclude(&path, ExclusionReason::Policy, decision.decisive_rule);
            return Ok(());
        }

        match entry.kind {
            EntryKind::Directory => {
                if depth >= self.limits.max_depth {
                    self.exclude(&path, ExclusionReason::DepthLimit, decision.decisive_rule);
                } else {
                    self.queue.push_back((Some(path), depth + 1));
                }
            }
            EntryKind::Link => self.exclude(&path, ExclusionReason::Link, decision.decisive_rule),
            EntryKind::Special => {
                self.exclude(&path, ExclusionReason::Special, decision.decisive_rule);
            }
            EntryKind::File => self.visit_or_queue_file(
                path,
                entry.metadata,
                decision.decisive_rule,
                cached_snapshot,
            )?,
        }
        Ok(())
    }

    fn visit_or_queue_file(
        &mut self,
        path: RelativePath,
        observed_metadata: rootlight_vfs::SnapshotMetadata,
        decisive_rule: Option<DecisiveRule>,
        cached_snapshot: Option<SourceSnapshot>,
    ) -> Result<(), DiscoveryError> {
        if self.retain_snapshots
            || cached_snapshot.is_some()
            || observed_metadata.length > self.limits.max_file_bytes
        {
            return self.visit_file(path, observed_metadata, decisive_rule, cached_snapshot);
        }
        let capture_bytes = observed_metadata.length.max(1);
        let next_bytes = self.pending_source_bytes.checked_add(capture_bytes);
        if !self.pending_files.is_empty()
            && (self.pending_files.len() >= MAX_SNAPSHOT_BATCH_FILES
                || next_bytes.is_none_or(|bytes| bytes > MAX_SNAPSHOT_BATCH_BYTES))
        {
            self.flush_pending_files()?;
        }
        self.pending_source_bytes = self.pending_source_bytes.checked_add(capture_bytes).ok_or(
            DiscoveryError::RetainedSnapshotByteLimit {
                observed: u64::MAX,
                maximum: MAX_SNAPSHOT_BATCH_BYTES,
            },
        )?;
        self.pending_files
            .try_reserve(1)
            .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
        self.pending_files.push(PendingDiscoveryFile {
            path,
            metadata: observed_metadata,
            decisive_rule,
        });
        if self.pending_files.len() >= MAX_SNAPSHOT_BATCH_FILES
            || self.pending_source_bytes >= MAX_SNAPSHOT_BATCH_BYTES
        {
            self.flush_pending_files()?;
        }
        Ok(())
    }

    fn flush_pending_files(&mut self) -> Result<(), DiscoveryError> {
        if self.pending_files.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending_files);
        self.pending_source_bytes = 0;
        let mut requests = Vec::new();
        requests
            .try_reserve_exact(pending.len())
            .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
        requests.extend(
            pending
                .iter()
                .map(|file| (file.path.clone(), file.metadata.length.max(1))),
        );
        let snapshots = self
            .root
            .snapshot_batch_with_cancellation(&requests, self.cancellation)?;
        if snapshots.len() != pending.len() {
            return Err(DiscoveryError::IncrementalDrift);
        }
        for (file, snapshot) in pending.into_iter().zip(snapshots) {
            self.record_file_snapshot(
                file.path,
                file.decisive_rule,
                snapshot.map_err(DiscoveryError::from),
            )?;
        }
        Ok(())
    }

    fn visit_file(
        &mut self,
        path: RelativePath,
        observed_metadata: rootlight_vfs::SnapshotMetadata,
        decisive_rule: Option<DecisiveRule>,
        cached_snapshot: Option<SourceSnapshot>,
    ) -> Result<(), DiscoveryError> {
        if observed_metadata.length > self.limits.max_file_bytes {
            self.exclude(&path, ExclusionReason::Oversized, decisive_rule);
            return Ok(());
        }
        let snapshot_result = match cached_snapshot {
            Some(snapshot) => revalidate_cached_snapshot(
                self.root,
                &path,
                observed_metadata,
                snapshot,
                self.cancellation,
            ),
            None => self.snapshot_for_path(&path, observed_metadata),
        };
        self.record_file_snapshot(path, decisive_rule, snapshot_result)
    }

    fn record_file_snapshot(
        &mut self,
        path: RelativePath,
        decisive_rule: Option<DecisiveRule>,
        snapshot_result: Result<SourceSnapshot, DiscoveryError>,
    ) -> Result<(), DiscoveryError> {
        let snapshot = match snapshot_result {
            Ok(snapshot) => snapshot,
            Err(DiscoveryError::Vfs(VfsError::FileTooLarge { .. })) => {
                self.exclude(&path, ExclusionReason::Oversized, decisive_rule);
                return Ok(());
            }
            Err(DiscoveryError::Vfs(VfsError::LinkedPath | VfsError::OpenFile { .. })) => {
                self.exclude(&path, ExclusionReason::Unreadable, decisive_rule);
                self.diagnostic(Some(&path), "DISCOVERY_UNREADABLE");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.files_examined = self
            .files_examined
            .checked_add(1)
            .ok_or(DiscoveryError::IncrementalDrift)?;
        self.bytes_examined = self
            .bytes_examined
            .checked_add(snapshot.metadata().length)
            .ok_or(DiscoveryError::IncrementalDrift)?;
        if let Some(observe_progress) = self.observe_progress.as_mut() {
            observe_progress(IncrementalDiscoveryProgress {
                files_examined: self.files_examined,
                bytes_examined: self.bytes_examined,
            });
        }
        if looks_binary(snapshot.content()) {
            self.exclude(&path, ExclusionReason::Binary, decisive_rule);
            return Ok(());
        }
        let (class, language_signals) = classify(&path, snapshot.content());
        let file = snapshot.file();
        let descriptor = FileDescriptor::new(
            file,
            content_hash(path.identity_bytes()),
            incremental::incremental_metadata(snapshot.metadata()),
        );
        self.scanned
            .try_reserve(1)
            .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
        self.scanned.push(ScannedFile::new(descriptor));
        self.inputs.push(ManifestInput {
            file,
            path: path.as_str().to_owned(),
            content_hash: snapshot.content_hash(),
            bytes: snapshot.metadata().length,
            class,
            language_signals,
            decisive_rule,
        });
        if self.retain_snapshots && self.snapshots.insert(file, snapshot).is_some() {
            return Err(DiscoveryError::IncrementalDrift);
        }
        self.coverage.included = self.coverage.included.saturating_add(1);
        Ok(())
    }

    fn snapshot_for_path(
        &mut self,
        path: &RelativePath,
        observed_metadata: rootlight_vfs::SnapshotMetadata,
    ) -> Result<SourceSnapshot, DiscoveryError> {
        let file = self.root.file_id(path);
        if let Some(snapshot) = self.cached_snapshots.remove(&file) {
            return revalidate_cached_snapshot(
                self.root,
                path,
                observed_metadata,
                snapshot,
                self.cancellation,
            );
        }
        if observed_metadata.length > self.limits.max_file_bytes {
            return Err(DiscoveryError::Vfs(VfsError::FileTooLarge {
                maximum: self.limits.max_file_bytes,
            }));
        }
        if self.retain_snapshots {
            self.snapshot_budget.reserve(observed_metadata.length)?;
        }
        // The traversal metadata is the aggregate reservation. Restricting the
        // capture to that size prevents a racing growth from bypassing it.
        let capture_limit = observed_metadata.length.max(1);
        self.root
            .snapshot_with_cancellation(path, capture_limit, self.cancellation)
            .map_err(DiscoveryError::from)
    }

    fn exclude(
        &mut self,
        path: &RelativePath,
        reason: ExclusionReason,
        decisive_rule: Option<DecisiveRule>,
    ) {
        let key = exclusion_key(reason).to_owned();
        *self.coverage.excluded.entry(key).or_default() += 1;
        if self.policy.audit {
            self.exclusions.push(ManifestExclusion {
                path: path.as_str().to_owned(),
                reason,
                decisive_rule,
            });
        }
    }

    fn diagnostic(&mut self, path: Option<&RelativePath>, code: &str) {
        if self.diagnostics.len() < self.limits.max_diagnostics {
            self.diagnostics.push(DiscoveryDiagnostic {
                path: path.map(|path| path.as_str().to_owned()),
                code: code.to_owned(),
            });
        }
    }

    fn finish(self) -> DiscoveryResult {
        self.finish_with_scan().0
    }

    fn finish_with_scan(mut self) -> (DiscoveryResult, Vec<ScannedFile>) {
        self.inputs.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.file.cmp(&right.file))
        });
        self.exclusions.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.reason.cmp(&right.reason))
        });
        self.diagnostics.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.code.cmp(&right.code))
        });
        let manifest = DiscoveryManifest {
            version: DISCOVERY_MANIFEST_VERSION.to_owned(),
            repository: self.root.repository(),
            configuration_hash: self.config.hash(),
            inputs: self.inputs,
            exclusions: self.exclusions,
            diagnostics: self.diagnostics,
            coverage: self.coverage,
        };
        (
            DiscoveryResult {
                manifest,
                snapshots: self.snapshots,
            },
            self.scanned,
        )
    }
}

fn revalidate_cached_snapshot(
    root: &RepositoryRoot,
    path: &RelativePath,
    observed_metadata: rootlight_vfs::SnapshotMetadata,
    snapshot: SourceSnapshot,
    cancellation: &Cancellation,
) -> Result<SourceSnapshot, DiscoveryError> {
    let file = root.file_id(path);
    if snapshot.file() != file || snapshot.path() != path {
        return Err(DiscoveryError::IncrementalDrift);
    }
    if snapshot.metadata() == observed_metadata && observed_metadata.supports_hash_reuse() {
        return Ok(snapshot);
    }
    if snapshot.metadata().length != observed_metadata.length {
        return Err(DiscoveryError::IncrementalDrift);
    }
    let cached_hash = snapshot.content_hash();
    drop(snapshot);
    // Windows lacks a safe change token, and a directory-entry handle can
    // temporarily expose weaker metadata while another process shares it.
    // Reopen and hash before accepting any cached content, including ignore
    // files that were already opened to construct scoped policy.
    let capture_limit = observed_metadata.length.max(1);
    let refreshed = root.snapshot_with_cancellation(path, capture_limit, cancellation)?;
    if refreshed.file() != file
        || refreshed.path() != path
        || refreshed.content_hash() != cached_hash
    {
        return Err(DiscoveryError::IncrementalDrift);
    }
    Ok(refreshed)
}

fn child_path(
    parent: Option<&RelativePath>,
    name: &OsString,
) -> Result<RelativePath, DiscoveryError> {
    match parent {
        Some(parent) => parent.join_name(name).map_err(DiscoveryError::Vfs),
        None => RelativePath::parse(Path::new(name)).map_err(DiscoveryError::Vfs),
    }
}

fn read_directory_with_entry_budget(
    root: &RepositoryRoot,
    directory: Option<&RelativePath>,
    remaining: usize,
    cancellation: &Cancellation,
) -> Result<BoundedDirectoryEntries, DiscoveryError> {
    root.read_directory_prefix(directory, remaining, cancellation)
        .map_err(DiscoveryError::Vfs)
}

fn classify(path: &RelativePath, content: &[u8]) -> (InputClass, Vec<LanguageSignal>) {
    let normalized = path.as_str().to_ascii_lowercase();
    let class = if generated_path(&normalized) || generated_content(content) {
        InputClass::Generated
    } else if vendored_path(&normalized) {
        InputClass::Vendored
    } else {
        InputClass::Source
    };

    let mut signals = BTreeSet::new();
    if let Some(language) = extension_language(&normalized) {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Extension,
        });
    }
    if let Some(language) = manifest_language(&normalized) {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Manifest,
        });
    }
    if let Some(language) = shebang_language(content) {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Shebang,
        });
    }
    if let Some(language) = content_language(content) {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Content,
        });
    }
    (class, signals.into_iter().collect())
}

fn looks_binary(content: &[u8]) -> bool {
    let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES));
    sample.is_some_and(|sample| sample.contains(&0))
}

fn generated_path(path: &str) -> bool {
    path.contains("/generated/")
        || path.starts_with("generated/")
        || path.ends_with(".generated.rs")
        || path.ends_with(".g.cs")
        || path.ends_with(".designer.cs")
        || path.ends_with(".pb.go")
}

fn vendored_path(path: &str) -> bool {
    path.starts_with("vendor/")
        || path.contains("/vendor/")
        || path.starts_with("third_party/")
        || path.contains("/third_party/")
}

fn generated_content(content: &[u8]) -> bool {
    let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES));
    sample.is_some_and(|sample| {
        let text = String::from_utf8_lossy(sample).to_ascii_lowercase();
        text.contains("generated file") || text.contains("do not edit")
    })
}

/// One installed source-language detection capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageCapability {
    /// Canonical normalized language label.
    pub language: &'static str,
    /// Audited filename suffixes, including the leading dot.
    pub suffixes: &'static [&'static str],
    /// Accepted source-language aliases.
    pub aliases: &'static [&'static str],
    /// Installed detector families.
    pub detectors: &'static [&'static str],
    /// Highest analysis tier installed for the language.
    pub maximum_tier: &'static str,
    /// Installed analyzer labels, or `source-fallback` for file-only retrieval.
    pub analyzers: &'static [&'static str],
}

/// Returns the authoritative installed source-language capability matrix.
#[must_use]
pub const fn language_capabilities() -> &'static [LanguageCapability] {
    LANGUAGE_CAPABILITIES
}

const LANGUAGE_CAPABILITIES: &[LanguageCapability] = &[
    LanguageCapability {
        language: "assembly",
        suffixes: &[".asm", ".s"],
        aliases: &["asm"],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "bash",
        suffixes: &[".bash", ".sh"],
        aliases: &["shell", "sh"],
        detectors: &["extension", "shebang"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "c",
        suffixes: &[".c", ".h"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "cpp",
        suffixes: &[".cc", ".cpp", ".cxx", ".hh", ".hpp", ".hxx"],
        aliases: &["cplusplus"],
        detectors: &["extension", "content"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "csharp",
        suffixes: &[".cs"],
        aliases: &["cs"],
        detectors: &["extension"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "css",
        suffixes: &[".css"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "dart",
        suffixes: &[".dart"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "go",
        suffixes: &[".go", ".pb.go"],
        aliases: &["golang"],
        detectors: &["content", "extension", "manifest"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "groovy",
        suffixes: &[".gradle", ".groovy"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "html",
        suffixes: &[".htm", ".html"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "java",
        suffixes: &[".java"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "javascript",
        suffixes: &[".cjs", ".js", ".jsx", ".mjs"],
        aliases: &["js"],
        detectors: &["extension", "manifest", "shebang"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "json",
        suffixes: &[".json"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "kotlin",
        suffixes: &[".kt", ".kts"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "lua",
        suffixes: &[".lua"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "matlab",
        suffixes: &[".mlx"],
        aliases: &[],
        detectors: &["content", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "objective-c",
        suffixes: &[".m"],
        aliases: &["objc"],
        detectors: &["content", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "objective-cpp",
        suffixes: &[".mm"],
        aliases: &["objcxx"],
        detectors: &["content", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "perl",
        suffixes: &[".pl", ".pm", ".pod"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "php",
        suffixes: &[".blade.php", ".php"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "powershell",
        suffixes: &[".ps1", ".psd1", ".psm1"],
        aliases: &["pwsh"],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "python",
        suffixes: &[".py"],
        aliases: &["py"],
        detectors: &["content", "extension", "manifest", "shebang"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "r",
        suffixes: &[".r"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "ruby",
        suffixes: &[".rb", ".ruby"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "rust",
        suffixes: &[".rs"],
        aliases: &["rs"],
        detectors: &["content", "extension", "manifest"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "scala",
        suffixes: &[".sc", ".scala"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "solidity",
        suffixes: &[".sol"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "sql",
        suffixes: &[".sql"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "swift",
        suffixes: &[".swift"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "toml",
        suffixes: &[".toml"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "typescript",
        suffixes: &[".d.ts", ".cts", ".mts", ".ts", ".tsx"],
        aliases: &["ts"],
        detectors: &["extension", "manifest"],
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "yaml",
        suffixes: &[".yaml", ".yml"],
        aliases: &["yml"],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
];

/// Returns the canonical language for one audited filename suffix.
#[must_use]
pub fn extension_language(path: &str) -> Option<&'static str> {
    let normalized = path.to_ascii_lowercase();
    let mut best = None::<(&str, &'static str)>;
    for capability in LANGUAGE_CAPABILITIES {
        for &suffix in capability.suffixes {
            if normalized.ends_with(suffix)
                && best.is_none_or(|(matched, _)| suffix.len() > matched.len())
            {
                best = Some((suffix, capability.language));
            }
        }
    }
    best.map(|(_, language)| language)
}

/// Resolves one installed canonical language label or accepted alias.
#[must_use]
pub fn canonical_language(language: &str) -> Option<&'static str> {
    LANGUAGE_CAPABILITIES
        .iter()
        .find(|capability| {
            capability.language == language || capability.aliases.contains(&language)
        })
        .map(|capability| capability.language)
}

fn manifest_language(path: &str) -> Option<&'static str> {
    match path.rsplit('/').next().unwrap_or(path) {
        "cargo.toml" => Some("rust"),
        "package.json" | "tsconfig.json" => Some("typescript"),
        "pyproject.toml" | "requirements.txt" => Some("python"),
        "go.mod" => Some("go"),
        _ => None,
    }
}

fn shebang_language(content: &[u8]) -> Option<&'static str> {
    let first_line = content.split(|byte| *byte == b'\n').next()?;
    if !first_line.starts_with(b"#!") {
        return None;
    }
    let line = String::from_utf8_lossy(first_line).to_ascii_lowercase();
    if line.contains("python") {
        Some("python")
    } else if line.contains("node") || line.contains("deno") {
        Some("javascript")
    } else if line.contains("bash") || line.contains("sh") {
        Some("bash")
    } else {
        None
    }
}

fn content_language(content: &[u8]) -> Option<&'static str> {
    let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES))?;
    let text = String::from_utf8_lossy(sample);
    if text.contains("@interface")
        || text.contains("@implementation")
        || text.contains("#import <Foundation/")
        || text.contains("#import \"")
    {
        Some("objective-c")
    } else if text.contains("classdef ")
        || text
            .lines()
            .any(|line| line.trim_start().starts_with("function "))
    {
        Some("matlab")
    } else if text.contains("fn main(") || text.contains("pub struct ") {
        Some("rust")
    } else if text.contains("package main") && text.contains("func ") {
        Some("go")
    } else if text.lines().any(|line| {
        let statement = line.trim_start();
        // Substring matches also accept C header guards and typedefs.
        (statement.starts_with("def ") || statement.starts_with("async def "))
            && statement.contains('(')
            && statement.contains(':')
    }) {
        Some("python")
    } else if looks_like_cpp(&text) {
        Some("cpp")
    } else {
        None
    }
}

fn looks_like_cpp(text: &str) -> bool {
    const NAMESPACE: u8 = 1 << 0;
    const TEMPLATE: u8 = 1 << 1;
    const CLASS: u8 = 1 << 2;
    const DECLARATION_FEATURES: u8 = NAMESPACE | TEMPLATE | CLASS;

    // A `.h` suffix is shared by C and C++, so one incidental token cannot
    // promote an otherwise ambiguous header into the C++ parser.
    let mut features = 0u8;
    let mut inside_block_comment = false;
    for line in text.lines() {
        let mut remaining = line;
        loop {
            if inside_block_comment {
                let Some(end) = remaining.find("*/") else {
                    break;
                };
                remaining = &remaining[end + 2..];
                inside_block_comment = false;
            }

            let line_comment = remaining.find("//");
            let block_comment = remaining.find("/*");
            let code_end = match (line_comment, block_comment) {
                (Some(line_start), Some(block_start)) => line_start.min(block_start),
                (Some(line_start), None) => line_start,
                (None, Some(block_start)) => block_start,
                (None, None) => remaining.len(),
            };
            let code = remaining[..code_end].trim();
            features |= cpp_features(code);
            if features & DECLARATION_FEATURES != 0 && features.count_ones() >= 2 {
                return true;
            }

            match (line_comment, block_comment) {
                (Some(line_start), Some(block_start)) if line_start <= block_start => break,
                (Some(_), None) | (None, None) => break,
                (_, Some(block_start)) => {
                    remaining = &remaining[block_start + 2..];
                    inside_block_comment = true;
                }
            }
        }
    }
    false
}

fn cpp_features(code: &str) -> u8 {
    const NAMESPACE: u8 = 1 << 0;
    const TEMPLATE: u8 = 1 << 1;
    const CLASS: u8 = 1 << 2;
    const STANDARD_LIBRARY: u8 = 1 << 3;
    const NOEXCEPT: u8 = 1 << 4;
    const ACCESS_SPECIFIER: u8 = 1 << 5;
    const CONSTEXPR: u8 = 1 << 6;

    let mut features = 0u8;
    if code.starts_with("namespace ") || code.starts_with("inline namespace ") {
        features |= NAMESPACE;
    }
    if (code.starts_with("template ") || code.starts_with("template<")) && code.contains('<') {
        features |= TEMPLATE;
    }
    if code.starts_with("class ") {
        features |= CLASS;
    }
    if code.contains("std::") {
        features |= STANDARD_LIBRARY;
    }
    if code.contains("noexcept") {
        features |= NOEXCEPT;
    }
    if matches!(code, "public:" | "protected:" | "private:") {
        features |= ACCESS_SPECIFIER;
    }
    if code.contains("constexpr") {
        features |= CONSTEXPR;
    }
    features
}

fn default_rules() -> Vec<PolicyRule> {
    [
        ".git/",
        "target/",
        "node_modules/",
        ".venv/",
        "dist/",
        "build/",
    ]
    .into_iter()
    .map(|pattern| PolicyRule {
        layer: PolicyLayer::Default,
        pattern: pattern.to_owned(),
        source: "rootlight-default".to_owned(),
    })
    .collect()
}

fn valid_source_label(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 128
        && source
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn exclusion_key(reason: ExclusionReason) -> &'static str {
    match reason {
        ExclusionReason::Policy => "policy",
        ExclusionReason::Link => "link",
        ExclusionReason::Special => "special",
        ExclusionReason::Oversized => "oversized",
        ExclusionReason::Binary => "binary",
        ExclusionReason::Unreadable => "unreadable",
        ExclusionReason::DepthLimit => "depth_limit",
    }
}

/// Typed failures returned by deterministic discovery.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// One or more configured limits were outside supported ceilings.
    #[error("invalid discovery limits")]
    InvalidLimits,
    /// A configured or repository-scoped layered policy was malformed.
    #[error("invalid discovery policy")]
    InvalidPolicy,
    /// A Gitignore-compatible rule failed to parse.
    #[error("invalid discovery pattern")]
    InvalidPattern {
        /// Underlying ignore-pattern parser error.
        #[source]
        source: ignore::Error,
    },
    /// Discovery crossed the configured entry ceiling.
    #[error("discovery exceeds {maximum} entries")]
    EntryLimit {
        /// Maximum permitted visited entries.
        maximum: usize,
    },
    /// Source snapshots crossed the hard aggregate retained-byte ceiling.
    #[error("discovery retained snapshot bytes {observed} exceed {maximum}")]
    RetainedSnapshotByteLimit {
        /// Aggregate source bytes requested by the discovery.
        observed: u64,
        /// Maximum aggregate retained source bytes.
        maximum: u64,
    },
    /// The VFS rejected or failed one repository operation.
    #[error(transparent)]
    Vfs(#[from] VfsError),
    /// Cooperative cancellation stopped discovery.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// Incremental reconciliation rejected a malformed or over-limit state.
    #[error(transparent)]
    Incremental(IncrementalError),
    /// Metadata changed between the complete scan and stable VFS snapshot.
    #[error("repository changed during incremental discovery")]
    IncrementalDrift,
    /// Canonical manifest serialization failed unexpectedly.
    #[error("failed to serialize discovery manifest")]
    SerializeManifest(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rootlight_cancel::CancellationReason;
    use rootlight_config::{ConfigLayer, ConfigSource};
    use rootlight_ids::derive_repository;
    use std::fs;
    use tempfile::{TempDir, tempdir_in};

    fn local_tempdir() -> TempDir {
        let current = std::env::current_dir().expect("current directory is available");
        tempdir_in(current).expect("local temporary directory is available")
    }

    fn config() -> ConfigSnapshot {
        ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::Defaults,
            contents: "version = \"1.0\"",
        }])
        .expect("minimal configuration resolves")
    }

    fn limits() -> DiscoveryLimits {
        DiscoveryLimits::new(1_000, 16, 1024 * 1024, 100).expect("test limits are valid")
    }

    fn write_fixture(temporary: &TempDir, path: &str, content: &[u8]) {
        let path = temporary.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent directories are created");
        }
        fs::write(path, content).expect("fixture file is written");
    }

    fn fixture_root(temporary: &TempDir, identity: &[u8]) -> RepositoryRoot {
        let repository = derive_repository(identity).id();
        RepositoryRoot::open(repository, temporary.path()).expect("fixture root opens")
    }

    #[test]
    fn retained_snapshot_budget_accepts_exact_limit_and_rejects_next_byte() {
        let mut exact = RetainedSnapshotBudget::new(5);
        exact.reserve(2).expect("first reservation fits");
        exact.reserve(3).expect("exact aggregate reservation fits");
        assert_eq!(exact.observed(), 5);

        let mut exceeded = RetainedSnapshotBudget::new(5);
        exceeded.reserve(2).expect("first reservation fits");
        assert!(matches!(
            exceeded.reserve(4),
            Err(DiscoveryError::RetainedSnapshotByteLimit {
                observed: 6,
                maximum: 5
            })
        ));
    }

    #[test]
    fn cached_snapshot_revalidation_handles_weak_directory_metadata() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"target/\n");
        let root = fixture_root(&temporary, b"weak-directory-metadata");
        let path = RelativePath::parse(Path::new(".gitignore")).expect("fixture path is valid");
        let cancellation = Cancellation::new();
        let snapshot = root
            .snapshot_with_cancellation(&path, 8, &cancellation)
            .expect("fixture snapshot succeeds");
        let mut weak_metadata = snapshot.metadata();
        weak_metadata.volume = None;
        weak_metadata.file_index = None;

        let refreshed =
            revalidate_cached_snapshot(&root, &path, weak_metadata, snapshot, &cancellation)
                .expect("weak metadata revalidates from file content");
        assert_eq!(refreshed.content(), b"target/\n");

        fs::write(temporary.path().join(".gitignore"), b"vendor/\n")
            .expect("same-length rewrite succeeds");
        assert!(matches!(
            revalidate_cached_snapshot(&root, &path, weak_metadata, refreshed, &cancellation,),
            Err(DiscoveryError::IncrementalDrift)
        ));
    }

    #[test]
    fn clean_discovery_bounds_new_snapshots_at_the_aggregate_limit() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "first.rs", b"aa");
        write_fixture(&temporary, "second.rs", b"bbb");
        let root = fixture_root(&temporary, b"clean-snapshot-budget");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");

        let exact = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits(),
            BTreeMap::new(),
            5,
            &Cancellation::new(),
        )
        .expect("exact aggregate snapshot bytes are admitted");
        let (exact_manifest, snapshots) = exact.into_parts();
        assert_eq!(
            snapshots
                .values()
                .map(|snapshot| snapshot.content().len())
                .sum::<usize>(),
            5
        );

        let bounded = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits(),
            BTreeMap::new(),
            4,
            &Cancellation::new(),
        )
        .expect("source exhaustion publishes a partial prefix");
        let (manifest, snapshots) = bounded.into_parts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            manifest.coverage.truncation,
            Some(DiscoveryTruncation {
                resource: DiscoveryTruncationResource::RetainedSourceBytes,
                observed: 5,
                limit: 4,
            })
        );

        let streaming =
            discover_manifest_streaming(&root, &config(), &policy, limits(), &Cancellation::new())
                .expect("source-free discovery crosses the snapshot retention boundary");
        assert_eq!(streaming, exact_manifest);
        assert!(streaming.coverage.complete);
        assert_eq!(streaming.coverage.included, 2);
        assert_eq!(
            streaming
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["first.rs", "second.rs"]
        );
    }

    #[test]
    fn clean_discovery_preflights_all_cached_snapshot_bytes() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "first.rs", b"aa");
        write_fixture(&temporary, "second.rs", b"bbb");
        let root = fixture_root(&temporary, b"cached-snapshot-budget");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let mut cached = BTreeMap::new();
        for path in ["first.rs", "second.rs"] {
            let path = RelativePath::parse(Path::new(path)).expect("fixture path is valid");
            let snapshot = root
                .snapshot(&path, 3)
                .expect("fixture snapshot is captured");
            cached.insert(snapshot.file(), snapshot);
        }
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));

        assert!(matches!(
            discover_with_snapshots_at_limit(
                &root,
                &config(),
                &policy,
                limits(),
                cached,
                4,
                &cancellation,
            ),
            Err(DiscoveryError::RetainedSnapshotByteLimit {
                observed: 5,
                maximum: 4
            })
        ));
    }

    #[test]
    fn configured_analysis_bytes_drive_discovery_without_response_budget_coupling() {
        let current = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: r#"
version = "1.1"
[resources]
max_source_bytes = 1
[analysis]
max_source_file_bytes = 2097152
"#,
        }])
        .expect("configuration 1.1 resolves");
        let current_limits = DiscoveryLimits::from_config(&current);

        assert_eq!(current.resources().max_source_bytes, 1);
        assert_eq!(current_limits.max_file_bytes, 2 * 1024 * 1024);

        let legacy = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: "version = \"1.0\"\n[resources]\nmax_source_bytes = 1048576\n",
        }])
        .expect("configuration 1.0 resolves");
        let legacy_limits = DiscoveryLimits::from_config(&legacy);

        assert_eq!(legacy.resources().max_source_bytes, 1024 * 1024);
        assert_eq!(legacy_limits.max_file_bytes, 1024 * 1024);
    }

    #[test]
    fn configured_discovery_entries_preserve_legacy_default_and_accept_current_override() {
        let legacy = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: "version = \"1.2\"\n",
        }])
        .expect("configuration 1.2 resolves");
        assert_eq!(DiscoveryLimits::from_config(&legacy).max_entries, 100_000);

        let current = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: "version = \"1.3\"\n[analysis]\nmax_discovery_entries = 250000\n",
        }])
        .expect("configuration 1.3 resolves");
        assert_eq!(DiscoveryLimits::from_config(&current).max_entries, 250_000);

        let maximum = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::System,
            contents: "version = \"1.3\"\n[analysis]\nmax_discovery_entries = 1000000\n",
        }])
        .expect("maximum discovery capacity resolves");
        assert_eq!(
            DiscoveryLimits::from_config(&maximum).max_entries,
            MAX_DISCOVERY_ENTRIES
        );
    }

    #[test]
    fn entry_budget_publishes_a_deterministic_incomplete_prefix() {
        let temporary = local_tempdir();
        for name in ["zeta.rs", "alpha.rs", "middle.rs"] {
            write_fixture(&temporary, name, b"pub fn item() {}\n");
        }
        let root = fixture_root(&temporary, b"bounded-discovery-prefix");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let limits =
            DiscoveryLimits::new(2, 4, 1024, 10).expect("test limits are within hard ceilings");

        let manifest = discover(&root, &config(), &policy, limits, &Cancellation::new())
            .expect("entry exhaustion publishes a partial manifest");

        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["alpha.rs", "middle.rs"]
        );
        assert_eq!(manifest.coverage.visited, 2);
        assert_eq!(manifest.coverage.included, 2);
        assert!(!manifest.coverage.complete);
        assert_eq!(
            manifest.coverage.truncation,
            Some(DiscoveryTruncation {
                resource: DiscoveryTruncationResource::Entries,
                observed: 3,
                limit: 2,
            })
        );
        assert!(manifest.diagnostics.iter().any(|diagnostic| {
            diagnostic.path.is_none() && diagnostic.code == DISCOVERY_ENTRY_LIMIT_DIAGNOSTIC_CODE
        }));
    }

    #[test]
    fn retained_source_budget_publishes_a_deterministic_incomplete_prefix() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "zeta.rs", b"zz");
        write_fixture(&temporary, "alpha.rs", b"aa");
        write_fixture(&temporary, "middle.rs", b"mmm");
        let root = fixture_root(&temporary, b"bounded-source-prefix");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let limits =
            DiscoveryLimits::new(10, 4, 1024, 10).expect("test limits are within hard ceilings");

        let result = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits,
            BTreeMap::new(),
            4,
            &Cancellation::new(),
        )
        .expect("source exhaustion publishes a partial manifest");
        let (manifest, snapshots) = result.into_parts();

        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["alpha.rs"]
        );
        assert_eq!(snapshots.len(), 1);
        assert_eq!(manifest.coverage.visited, 2);
        assert_eq!(manifest.coverage.included, 1);
        assert!(!manifest.coverage.complete);
        assert_eq!(
            manifest.coverage.truncation,
            Some(DiscoveryTruncation {
                resource: DiscoveryTruncationResource::RetainedSourceBytes,
                observed: 5,
                limit: 4,
            })
        );
        assert!(manifest.diagnostics.iter().any(|diagnostic| {
            diagnostic.path.is_none()
                && diagnostic.code == DISCOVERY_SOURCE_BYTE_LIMIT_DIAGNOSTIC_CODE
        }));
    }

    #[test]
    fn legacy_source_limit_excludes_oversized_files_before_snapshotting() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "oversized.rs", &[b'x'; 17]);
        write_fixture(&temporary, "included.rs", &[b'x'; 16]);
        let root = fixture_root(&temporary, b"legacy-source-limit");
        let config = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::User,
            contents: "version = \"1.0\"\n[resources]\nmax_source_bytes = 16\n",
        }])
        .expect("legacy configuration resolves");
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");

        let manifest = discover(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            &Cancellation::new(),
        )
        .expect("legacy-bounded discovery succeeds");

        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["included.rs"]
        );
        assert!(manifest.exclusions.iter().any(|exclusion| {
            exclusion.path == "oversized.rs" && exclusion.reason == ExclusionReason::Oversized
        }));
    }

    #[test]
    fn repeated_discovery_emits_byte_identical_manifest() {
        let temporary = local_tempdir();
        fs::create_dir_all(temporary.path().join("src")).expect("fixture directory is created");
        fs::write(temporary.path().join("src/lib.rs"), "pub fn sample() {}")
            .expect("fixture source is written");
        fs::write(temporary.path().join("ignored.tmp"), "ignored")
            .expect("fixture excluded input is written");
        let repository = derive_repository(b"discovery-test").id();
        let root = RepositoryRoot::open(repository, temporary.path()).expect("root opens");
        let policy = DiscoveryPolicy::build(
            vec![PolicyRule {
                layer: PolicyLayer::Operation,
                pattern: "*.tmp".to_owned(),
                source: "operation".to_owned(),
            }],
            true,
        )
        .expect("policy builds");

        let first = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("first discovery succeeds");
        let second = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("second discovery succeeds");

        assert_eq!(
            first.canonical_bytes().expect("manifest serializes"),
            second.canonical_bytes().expect("manifest serializes")
        );
        assert_eq!(first.inputs.len(), 1);
        assert_eq!(first.exclusions.len(), 1);
    }

    #[test]
    fn header_guards_and_typedefs_are_not_python_definitions() {
        for source in [
            b"#ifdef CLIENT_API\n/* contract: stable */\nint read_item(void);\n#endif\n".as_slice(),
            b"typedef struct { int value; } Item;\n/* note: opaque handle */\n".as_slice(),
        ] {
            assert_eq!(content_language(source), None);
        }
        for source in [
            b"def inspect_item(value):\n    return value\n".as_slice(),
            b"async def load_item(value):\n    return value\n".as_slice(),
        ] {
            assert_eq!(content_language(source), Some("python"));
        }
        assert_eq!(extension_language("include/reader.h"), Some("c"));
    }

    #[test]
    fn configuration_syntax_remains_distinct_from_project_manifest_context() {
        for (path, syntax, project) in [
            ("Cargo.toml", "toml", Some("rust")),
            ("pyproject.toml", "toml", Some("python")),
            ("package.json", "json", Some("typescript")),
            ("settings.JSON", "json", None),
            ("pipeline.yaml", "yaml", None),
            ("pipeline.yml", "yaml", None),
        ] {
            let (_, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path is valid"),
                b"",
            );
            assert!(
                signals.iter().any(|signal| {
                    signal.language == syntax && signal.evidence == LanguageEvidence::Extension
                }),
                "{path}: {signals:?}"
            );
            let projects = signals
                .iter()
                .filter(|signal| signal.evidence == LanguageEvidence::Manifest)
                .map(|signal| signal.language.as_str())
                .collect::<Vec<_>>();
            assert_eq!(projects, project.into_iter().collect::<Vec<_>>(), "{path}");
            assert_eq!(canonical_language(syntax), Some(syntax));
            let capability = language_capabilities()
                .iter()
                .find(|capability| capability.language == syntax)
                .expect("source capability is declared");
            assert_eq!(
                capability.analyzers,
                if matches!(syntax, "json" | "toml" | "yaml") {
                    &["treesitter"][..]
                } else {
                    &["source-fallback"][..]
                }
            );
        }
        assert_eq!(canonical_language("yml"), Some("yaml"));
        for path in [
            "sample.json5",
            "sample.jsonc",
            "sample.yaml.bak",
            "sample.toml.rs",
        ] {
            assert!(!matches!(
                extension_language(path),
                Some("json" | "yaml" | "toml")
            ));
        }
    }

    #[test]
    fn language_and_input_classification_uses_multiple_evidence_kinds() {
        let (class, signals) = classify(
            &RelativePath::parse(Path::new("generated/api.d.ts")).expect("fixture path is valid"),
            b"// generated file; do not edit\nexport interface Api {}",
        );
        assert_eq!(class, InputClass::Generated);
        assert!(signals.iter().any(|signal| {
            signal.language == "typescript" && signal.evidence == LanguageEvidence::Extension
        }));

        for (path, expected) in [
            ("normalize.css", "css"),
            ("plugin.lua", "lua"),
            ("client.m", "objective-c"),
            ("client.mm", "objective-cpp"),
            ("analysis.mlx", "matlab"),
            ("script.pl", "perl"),
            ("module.pm", "perl"),
            ("plot.R", "r"),
            ("build.gradle.kts", "kotlin"),
            ("Main.kt", "kotlin"),
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
        ] {
            assert_eq!(extension_language(path), Some(expected));
        }

        for (content, expected) in [
            (
                b"@interface Session : NSObject\n@end".as_slice(),
                "objective-c",
            ),
            (
                b"function result = classify(value)\nresult = value;\nend".as_slice(),
                "matlab",
            ),
            (
                b"namespace sample {\nclass Parser final {\n public:\n  template <typename T>\n  void prepare() noexcept;\n};\n}".as_slice(),
                "cpp",
            ),
        ] {
            assert_eq!(content_language(content), Some(expected));
        }
        assert_eq!(
            content_language(
                b"/* namespace ignored {\nclass CommentOnly {};\n} */\nint ordinary_header(void);"
            ),
            None
        );
        for (language, expected) in [
            ("bash", "bash"),
            ("shell", "bash"),
            ("cplusplus", "cpp"),
            ("cs", "csharp"),
            ("js", "javascript"),
            ("objc", "objective-c"),
            ("objcxx", "objective-cpp"),
            ("py", "python"),
            ("rs", "rust"),
            ("ts", "typescript"),
        ] {
            assert_eq!(canonical_language(language), Some(expected));
        }
        assert_eq!(canonical_language("unknown"), None);
    }

    #[test]
    fn policy_negation_overrides_earlier_exclusion() {
        let policy = DiscoveryPolicy::build(
            vec![
                PolicyRule {
                    layer: PolicyLayer::Repository,
                    pattern: "src/**".to_owned(),
                    source: "repo".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Operation,
                    pattern: "!src/lib.rs".to_owned(),
                    source: "operation".to_owned(),
                },
            ],
            true,
        )
        .expect("policy builds");
        let included = RelativePath::parse(Path::new("src/lib.rs")).expect("path is valid");
        let excluded = RelativePath::parse(Path::new("src/main.rs")).expect("path is valid");

        assert!(policy.decision(&included, false).included);
        assert!(policy.decision(&excluded, false).excluded);
    }

    #[test]
    fn nested_gitignore_honors_gate_patterns_deterministically_with_audit_evidence() {
        let temporary = local_tempdir();
        write_fixture(
            &temporary,
            "nested/.gitignore",
            b"ignored/*\n!ignored/kept.rs\n",
        );
        write_fixture(
            &temporary,
            "nested/ignored/ignored.rs",
            b"fn ignored_by_nested_rule() {}\n",
        );
        write_fixture(
            &temporary,
            "nested/ignored/kept.rs",
            b"fn kept_after_negation() {}\n",
        );
        let root = fixture_root(&temporary, b"nested-gate-patterns");
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");

        let first = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("first discovery succeeds");
        let second = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("second discovery succeeds");

        assert_eq!(first, second);
        assert_eq!(
            first.canonical_bytes().expect("first manifest serializes"),
            second
                .canonical_bytes()
                .expect("second manifest serializes")
        );
        assert_eq!(
            first
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["nested/.gitignore", "nested/ignored/kept.rs"]
        );
        let ignored = first
            .exclusions
            .iter()
            .find(|exclusion| exclusion.path == "nested/ignored/ignored.rs")
            .expect("ignored Vertical slice input is audited");
        assert_eq!(
            ignored.decisive_rule,
            Some(DecisiveRule {
                source: "nested/.gitignore".to_owned(),
                pattern: "ignored/*".to_owned(),
            })
        );
        let kept = first
            .inputs
            .iter()
            .find(|input| input.path == "nested/ignored/kept.rs")
            .expect("negated Vertical slice input is included");
        assert_eq!(
            kept.decisive_rule,
            Some(DecisiveRule {
                source: "nested/.gitignore".to_owned(),
                pattern: "!ignored/kept.rs".to_owned(),
            })
        );
    }

    #[test]
    fn tracked_admission_opens_only_required_routes_and_preserves_policy() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"cache/\n*.rs\n");
        write_fixture(&temporary, "cache/.gitignore", b"!*.rs\n");
        for path in [
            "cache/kept.rs",
            "cache/untracked.rs",
            "cache/deep/kept.rs",
            "cache/deep/untracked.rs",
            "cache-other.rs",
            "cache/denied.rs",
            "target/kept.rs",
        ] {
            write_fixture(&temporary, path, b"fn value() {}\n");
        }
        let root = fixture_root(&temporary, b"tracked-admission");
        let policy = DiscoveryPolicy::build(
            vec![PolicyRule {
                layer: PolicyLayer::Operation,
                pattern: "cache/denied.rs".to_owned(),
                source: "operation".to_owned(),
            }],
            true,
        )
        .expect("policy builds")
        .with_tracked_files(
            [
                "cache/kept.rs",
                "cache/deep/kept.rs",
                "cache-other.rs",
                "cache/denied.rs",
                "target/kept.rs",
            ]
            .map(str::to_owned)
            .into(),
            &Cancellation::new(),
        )
        .expect("tracked paths validate");
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("tracked discovery succeeds");
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            [
                ".gitignore",
                "cache-other.rs",
                "cache/deep/kept.rs",
                "cache/kept.rs"
            ]
        );
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "cache/untracked.rs")
        );
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "cache/deep/untracked.rs")
        );
        for invalid in ["../escape.rs", "/absolute.rs", "alias\\file.rs", ""] {
            assert!(
                DiscoveryPolicy::build(Vec::new(), false)
                    .expect("policy builds")
                    .with_tracked_files([invalid.to_owned()].into(), &Cancellation::new())
                    .is_err()
            );
        }
        let file_policy = DiscoveryPolicy::build(Vec::new(), false)
            .expect("policy builds")
            .with_tracked_files(["cache".to_owned()].into(), &Cancellation::new())
            .expect("path validates");
        assert!(!file_policy.admits_tracked_path(
            &RelativePath::parse(Path::new("cache")).expect("path"),
            true
        ));
    }

    #[test]
    fn slashless_nested_rule_matches_descendants_without_leaking_to_siblings() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "scope/.gitignore", b"\xef\xbb\xbfcache.rs\n");
        write_fixture(&temporary, "scope/cache.rs", b"fn direct() {}\n");
        write_fixture(&temporary, "scope/deep/cache.rs", b"fn nested() {}\n");
        write_fixture(&temporary, "scope/deep/kept.rs", b"fn kept() {}\n");
        write_fixture(&temporary, "sibling/cache.rs", b"fn sibling() {}\n");
        let root = fixture_root(&temporary, b"slashless-scope");
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");

        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("discovery succeeds");

        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["scope/.gitignore", "scope/deep/kept.rs", "sibling/cache.rs"]
        );
        assert_eq!(
            manifest
                .exclusions
                .iter()
                .map(|exclusion| exclusion.path.as_str())
                .collect::<Vec<_>>(),
            ["scope/cache.rs", "scope/deep/cache.rs"]
        );
    }

    #[test]
    fn child_gitignore_decision_overrides_matching_ancestor() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"*.log\n");
        write_fixture(&temporary, "child/.gitignore", b"!keep.log\n");
        write_fixture(&temporary, "child/drop.log", b"drop\n");
        write_fixture(&temporary, "child/keep.log", b"keep\n");
        write_fixture(&temporary, "sibling/keep.log", b"ignored\n");
        let root = fixture_root(&temporary, b"child-override");
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");

        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("discovery succeeds");

        let kept = manifest
            .inputs
            .iter()
            .find(|input| input.path == "child/keep.log")
            .expect("child negation includes the matching file");
        assert_eq!(
            kept.decisive_rule,
            Some(DecisiveRule {
                source: "child/.gitignore".to_owned(),
                pattern: "!keep.log".to_owned(),
            })
        );
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "child/drop.log")
        );
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "sibling/keep.log")
        );
    }

    #[test]
    fn operation_negation_overrides_nested_vcs_exclusion() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"*.rs\n");
        write_fixture(&temporary, "drop.rs", b"fn drop() {}\n");
        write_fixture(&temporary, "keep.rs", b"fn keep() {}\n");
        let root = fixture_root(&temporary, b"operation-overrides-vcs");
        let policy = DiscoveryPolicy::build(
            vec![PolicyRule {
                layer: PolicyLayer::Operation,
                pattern: "!keep.rs".to_owned(),
                source: "operation".to_owned(),
            }],
            true,
        )
        .expect("policy builds");

        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("discovery succeeds");

        let kept = manifest
            .inputs
            .iter()
            .find(|input| input.path == "keep.rs")
            .expect("operation negation includes the matching file");
        assert_eq!(
            kept.decisive_rule,
            Some(DecisiveRule {
                source: "operation".to_owned(),
                pattern: "!keep.rs".to_owned(),
            })
        );
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "drop.rs")
        );
    }

    #[test]
    fn descendant_negation_does_not_traverse_an_excluded_directory() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"blocked/\n!blocked/kept.rs\n");
        write_fixture(&temporary, "blocked/extra.rs", b"fn extra() {}\n");
        write_fixture(&temporary, "blocked/kept.rs", b"fn kept() {}\n");
        write_fixture(&temporary, "visible.rs", b"fn visible() {}\n");
        let root = fixture_root(&temporary, b"excluded-directory");
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");
        let root_entry_limit =
            DiscoveryLimits::new(3, 16, 1024 * 1024, 100).expect("test limits are valid");

        let manifest = discover(
            &root,
            &config(),
            &policy,
            root_entry_limit,
            &Cancellation::new(),
        )
        .expect("excluded subtree is not traversed");

        assert_eq!(manifest.coverage.visited, 3);
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "blocked")
        );
        assert!(
            manifest
                .inputs
                .iter()
                .all(|input| !input.path.starts_with("blocked/"))
        );
    }

    #[test]
    fn nested_ignore_discovery_honors_cancellation() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"*.rs\n");
        write_fixture(&temporary, "sample.rs", b"fn sample() {}\n");
        let root = fixture_root(&temporary, b"nested-ignore-cancellation");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));

        assert!(matches!(
            discover(&root, &config(), &policy, limits(), &cancellation),
            Err(DiscoveryError::Cancelled(cancelled))
                if cancelled.reason() == CancellationReason::ClientRequest
        ));
    }

    #[test]
    fn malformed_utf8_gitignore_fails_with_typed_policy_error() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", &[0xff]);
        let root = fixture_root(&temporary, b"malformed-ignore");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");

        assert!(matches!(
            discover(&root, &config(), &policy, limits(), &Cancellation::new()),
            Err(DiscoveryError::InvalidPolicy)
        ));
    }

    #[test]
    fn malformed_gitignore_pattern_fails_without_exposing_repository_text() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"[z-a]\n");
        let root = fixture_root(&temporary, b"malformed-ignore-pattern");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");

        let error = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect_err("malformed ignore pattern is rejected");

        assert!(matches!(error, DiscoveryError::InvalidPolicy));
        assert!(!format!("{error:?}").contains("[z-a]"));
    }

    #[test]
    fn oversized_gitignore_fails_at_the_configured_snapshot_bound() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"*.rs\n");
        let root = fixture_root(&temporary, b"oversized-ignore");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let four_byte_limit = DiscoveryLimits::new(10, 4, 4, 10).expect("test limits are valid");

        assert!(matches!(
            discover(
                &root,
                &config(),
                &policy,
                four_byte_limit,
                &Cancellation::new()
            ),
            Err(DiscoveryError::Vfs(VfsError::FileTooLarge { maximum: 4 }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn linked_gitignore_is_not_read_or_allowed_to_escape_the_root() {
        use std::os::unix::fs::symlink;

        let temporary = local_tempdir();
        let outside = local_tempdir();
        write_fixture(&outside, "outside-ignore", b"*.rs\n");
        symlink(
            outside.path().join("outside-ignore"),
            temporary.path().join(".gitignore"),
        )
        .expect("ignore link is created");
        write_fixture(&temporary, "visible.rs", b"fn visible() {}\n");
        let root = fixture_root(&temporary, b"linked-ignore");
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");

        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("discovery does not follow ignore link");

        assert!(
            manifest
                .inputs
                .iter()
                .any(|input| input.path == "visible.rs")
        );
        assert!(manifest.exclusions.iter().any(|exclusion| {
            exclusion.path == ".gitignore" && exclusion.reason == ExclusionReason::Link
        }));
    }

    proptest! {
        #[test]
        fn canonical_manifest_round_trips_for_safe_names(names in prop::collection::btree_set("[a-z]{1,12}\\.rs", 1..30)) {
            let temporary = local_tempdir();
            for name in &names {
                fs::write(temporary.path().join(name), "pub fn item() {}")
                    .expect("fixture source is written");
            }
            let repository = derive_repository(b"property-discovery").id();
            let root = RepositoryRoot::open(repository, temporary.path()).expect("root opens");
            let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
            let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
                .expect("discovery succeeds");
            let bytes = manifest.canonical_bytes().expect("manifest serializes");
            let decoded: DiscoveryManifest = serde_json::from_slice(&bytes).expect("manifest decodes");
            prop_assert_eq!(manifest, decoded);
        }
    }
}
